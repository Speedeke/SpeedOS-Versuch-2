// interrupts.rs — Die Interrupt Descriptor Table (IDT) und unsere
//                 Exception-Handler
//
// Die IDT ist eine Tabelle mit bis zu 256 Einträgen, in der die CPU
// nachschlägt, welche Funktion sie bei welchem Ereignis aufrufen soll.
// Die ersten 32 Einträge sind für CPU-Exceptions reserviert — Fehler,
// die die CPU selbst meldet, z. B.:
//   - Breakpoint (int3): absichtlicher Debug-Stopp, harmlos
//   - Page Fault: Zugriff auf nicht gemappten/verbotenen Speicher
//   - Double Fault: beim Behandeln einer Exception ging etwas schief
//
// Ohne IDT führt JEDE Exception zum Triple Fault und damit zum Reboot.
// Mit dieser Datei wird der Kernel absturzsicher: Er meldet Fehler
// sauber auf VGA + seriell, statt wortlos neu zu starten.

use crate::{gdt, println};
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::{PrivilegeLevel, VirtAddr};

// ---------------------------------------------------------------------------
// Hardware-Interrupts und der 8259 PIC
//
// Hardware wie Timer und Tastatur meldet sich nicht direkt bei der CPU,
// sondern beim PIC (Programmable Interrupt Controller) — zwei
// zusammengeschaltete Chips mit je 8 Leitungen. Der PIC übersetzt
// "Leitung 1 hat gezuckt" in eine Interrupt-Nummer für die IDT.
//
// Ab Werk benutzt der PIC die Nummern 0–15. Das kollidiert mit den
// CPU-Exceptions (0–31)! Deshalb "remappen" wir ihn auf 32–47:
// Timer = 32, Tastatur = 33, usw.
// ---------------------------------------------------------------------------

/// Neue Basis-Nummern der beiden PICs: direkt hinter den 32 Exceptions.
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Die beiden PIC-Chips, geschützt durch einen Spinlock.
/// `unsafe`: Falsche Offsets würden das Interrupt-System zerstören —
/// unsere kollidieren garantiert mit nichts.
pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Die Interrupt-Nummern unserer Hardware — lesbar benannt.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,      // 32: der PIT-Timer (tickt ~250x pro Sekunde)
    Keyboard,                  // 33: die PS/2-Tastatur
    Maus = PIC_2_OFFSET + 4,   // 44: die PS/2-Maus (IRQ 12, am zweiten PIC)
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
    fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

/// Schaltet den Local APIC ab, damit die klassischen 8259-PIC-
/// Interrupts die CPU direkt erreichen (Pre-APIC-Verdrahtung).
///
/// Lektion aus der bootloader-0.11-Migration: UEFI-Firmware (OVMF)
/// benutzt den modernen APIC und hinterlässt ihn AKTIV — dabei ist
/// die Leitung vom alten PIC (LINT0) maskiert. Folge: Timer und
/// Tastatur feuern am PIC, aber die CPU bekommt NIE etwas davon mit.
/// Bis SpeedOS echtes APIC lernt (nötig für SMP und den präzisen
/// APIC-Timer), ist der abgeschaltete LAPIC die ehrliche Konfiguration
/// zu unserem 8259-Design.
pub fn lapic_deaktivieren() {
    use x86_64::registers::model_specific::Msr;

    /// Das Model-Specific Register, das den LAPIC steuert.
    const IA32_APIC_BASE: u32 = 0x1B;
    let mut msr = Msr::new(IA32_APIC_BASE);
    // unsafe (MSR-Zugriff): Bit 11 = "APIC Global Enable". Wir löschen
    // nur dieses eine Bit und lassen den Rest (Basisadresse) unberührt.
    unsafe {
        let wert = msr.read();
        msr.write(wert & !(1 << 11));
    }
}

/// Programmiert den PIT (Programmable Interval Timer, Kanal 0) auf
/// ~250 Interrupts pro Sekunde (Teiler zeit::PIT_TEILER = 4773,
/// Rate-Generator-Modus).
///
/// Warum 250 Hz statt der klassischen ~18,2 Hz (Teiler 65536)?
/// zeit::warte_ms kann nur so fein warten, wie der Timer tickt —
/// mit 55-ms-Ticks lief der Compositor effektiv mit ~18 FPS, und
/// Fenster-Ziehen ruckelte spürbar. 4-ms-Ticks geben flüssige
/// ~33 FPS, ohne die CPU mit Interrupts zu fluten. Der Teiler lebt
/// in zeit.rs, damit Timer-Programmierung und ms-Umrechnung
/// GARANTIERT denselben Wert benutzen.
///
/// Lektion aus der bootloader-0.11-Migration: Unter klassischem BIOS
/// hatte die Firmware den PIT immer schon eingestellt — unter UEFI
/// tut das NIEMAND für uns. Ohne diese Initialisierung feuert IRQ 0
/// nie, und alles, was auf Timer-Ticks wartet (zeit.rs, hlt-Aufwachen,
/// Executor-Notfallpfad), steht still. Ein OS stellt seine Uhr selbst!
pub fn pit_initialisieren() {
    use x86_64::instructions::port::Port;

    let teiler = crate::zeit::PIT_TEILER as u16;
    let mut kommando: Port<u8> = Port::new(0x43);
    let mut kanal_0: Port<u8> = Port::new(0x40);
    // unsafe (Port-I/O): Standard-PIT-Ports; die Werte sind die seit
    // 1981 dokumentierte Konfiguration (Kanal 0, Low/High-Byte,
    // Modus 2 = Rate-Generator, binär).
    unsafe {
        kommando.write(0b0011_0100);
        kanal_0.write((teiler & 0xff) as u8); // Teiler Low-Byte
        kanal_0.write((teiler >> 8) as u8); // Teiler High-Byte
    }
}

lazy_static! {
    /// Die IDT muss so lange leben, wie der Kernel läuft, denn die CPU
    /// greift bei jeder Exception darauf zu — deshalb `static` über
    /// lazy_static (normale `static`s erlauben die nötige Initialisierung
    /// mit Funktionsaufrufen nicht).
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        // General Protection Fault: klassisch bei ungültigen Segment-/
        // Privileg-Operationen — und der Auffang für verbotene Instruktionen
        // aus Ring 3 (ein User-Programm könnte auch ein #GP statt #PF auslösen).
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);

        // ==================================================================
        // ALLE ÜBRIGEN CPU-EXCEPTIONS (Serie-6-Abschluss, Sicherheits-Pass)
        //
        // GEFUNDEN DURCH DEN ANGREIFER-TEST: Bis hierhin waren nur #PF und
        // #GP mit einem Handler versehen. Ein Ring-3-Programm, das `ud2`
        // ausführt oder durch NULL teilt, löst aber #UD bzw. #DE aus — und
        // für einen Vektor OHNE IDT-Eintrag liefert die CPU keinen Handler,
        // sondern eskaliert zum DOUBLE FAULT. Der hält SpeedOS an.
        //
        // Das heisst: Ein einziges `div rax, 0` in einem unprivilegierten
        // Programm hätte den ganzen Kernel angehalten. Genau die Sorte
        // Lücke, für die dieser Test da ist.
        //
        // Deshalb wird hier nicht ein einzelnes Loch gestopft, sondern die
        // KLASSE geschlossen: Jede CPU-Exception, die überhaupt aus Ring 3
        // eintreffen kann, bekommt einen Handler, und alle laufen durch
        // dieselbe `user_recovery`-Prüfung. Aus Ring 3 stirbt der Prozess,
        // aus Ring 0 ist es ein Kernel-Bug und wir halten an — wie bei #PF
        // und #GP auch.
        //
        // (Nicht dabei: #DF hat schon einen Handler mit eigenem Stack, und
        //  #BP ist absichtlich DPL 0 — ein `int3` aus Ring 3 wird dadurch
        //  zum #GP und ist damit ebenfalls abgedeckt.)
        // ==================================================================
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_handler);
        idt.x87_floating_point.set_handler_fn(x87_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.simd_floating_point.set_handler_fn(simd_handler);
        // `unsafe`: Wir versprechen, dass der IST-Index gültig ist und
        // nicht für mehrere Handler gleichzeitig verwendet wird.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                // Double Fault bekommt den Notfall-Stack aus der IST —
                // so funktioniert der Handler selbst bei Stack Overflow.
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        // Hardware-Interrupts (nach dem PIC-Remapping):
        // DER TIMER (Serie 6, Teil 3): kein `extern "x86-interrupt"`-Handler
        // mehr, sondern ein NACKTER Assembler-Einstieg (scheduler.rs). Grund:
        // Ein Kontext-Wechsel muss den STACK-POINTER umbiegen (auf den
        // gesicherten Trap-Rahmen eines anderen Prozesses) — das kann eine
        // gewöhnliche Rust-Funktion nicht, deren Epilog der Compiler schreibt.
        // unsafe: set_handler_addr traut uns eine GÜLTIGE Handler-Adresse zu;
        // sie stammt aus unserem eigenen global_asm-Symbol.
        unsafe {
            idt[InterruptIndex::Timer.as_usize()]
                .set_handler_addr(VirtAddr::new(crate::scheduler::timer_handler_adresse()));
        }
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Maus.as_usize()].set_handler_fn(maus_interrupt_handler);
        // PCI-Interrupts (virtio-net, Serie 5): Welche IRQ das Gerät
        // bekommt, steht erst nach der PCI-Enumeration fest. Wir
        // registrieren die typischen PCI-Vektoren (IRQ 9/10/11 am
        // zweiten PIC) statisch und schalten zur Laufzeit nur die
        // tatsächlich benutzte per irq_freischalten() frei.
        idt[(PIC_2_OFFSET + 1) as usize].set_handler_fn(virtio_pci_irq9);
        idt[(PIC_2_OFFSET + 2) as usize].set_handler_fn(virtio_pci_irq10);
        idt[(PIC_2_OFFSET + 3) as usize].set_handler_fn(virtio_pci_irq11);
        // SYSCALL-GATE (Serie 6): INT 0x80 aus Ring 3. Der Einstieg ist ein
        // nackter Assembler-Handler (syscall/mod.rs), der den vollen User-Kontext
        // sichert. DPL 3, damit Ring-3-Code diesen Trap AUSLÖSEN darf (die
        // anderen Gates haben DPL 0 — User-Mode könnte sie nicht auslösen).
        // unsafe: set_handler_addr traut uns zu, eine GÜLTIGE Handler-Adresse
        // zu liefern — sie stammt aus unserem global_asm-Symbol (ring3.rs).
        unsafe {
            idt[0x80]
                .set_handler_addr(VirtAddr::new(crate::syscall::handler_adresse()))
                .set_privilege_level(PrivilegeLevel::Ring3);
        }
        idt
    };
}

/// Lädt die IDT in die CPU (lidt-Befehl). Ab diesem Moment werden
/// Exceptions von unseren Handlern behandelt statt vom Nichts.
pub fn init_idt() {
    IDT.load();
}

/// Handler für die Breakpoint-Exception (Befehl `int3`).
///
/// Das ist die harmloseste Exception: Debugger benutzen sie, um
/// Programme anzuhalten. Wir geben nur eine Meldung aus und kehren
/// zurück — das Programm läuft danach normal weiter.
/// `extern "x86-interrupt"`: spezielle Aufrufkonvention, bei der der
/// Compiler ALLE Register sichert, weil eine Exception mitten in
/// beliebigem Code zuschlagen kann.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT (int3) bei {:?}", stack_frame.instruction_pointer);
}

/// Handler für Page Faults: Zugriff auf Speicher, der nicht gemappt
/// oder geschützt ist. Gibt alle Infos lesbar aus und hält dann an —
/// weiterlaufen wäre gefährlich, ohne den Fehler zu beheben.
/// Gemeinsame User-Mode-Recovery (Dauerregel II): Kam der Trap aus Ring 3,
/// biegen wir den Interrupt-Rahmen um, sodass der Epilog-`iretq` in den KERNEL
/// zurückkehrt statt nach Ring 3 (wo derselbe Fehler sofort wieder käme), und
/// liefern `true`. Sonst `false` — dann ist es ein Fehler im Kernel selbst,
/// also ein echter Bug, und der Aufrufer hält an.
///
/// ZWEI Rückwege, weil es zwei Arten von Ring-3-Code gibt:
///
///  (a) EINGEPLANTER PROZESS (Scheduler, Serie 6 Teil 3): Der Prozess wird
///      auf `Beendet` gesetzt, und der Rahmen zeigt auf den Ring-0-Sterbe-Stub
///      — der schaltet auf den nächsten lauffähigen Prozess um. Der Kernel
///      merkt nur, dass ein Prozess weniger da ist.
///
///  (b) EINZELSCHUSS-PFAD (`ring3test`, Serie 6 Teil 1): Der Rahmen zeigt auf
///      den setjmp-Landeplatz, der den gesicherten Kernel-Kontext
///      wiederherstellt.
///
/// Reihenfolge ist wichtig: (a) zuerst, denn ein eingeplanter Prozess hat
/// keinen setjmp-Puffer, auf den man zurückspringen könnte.
fn user_recovery(stack_frame: &mut InterruptStackFrame) -> bool {
    let aus_user_mode = (stack_frame.code_segment & 3) == 3;
    if !aus_user_mode {
        return false;
    }

    // (a) Läuft ein eingeplanter User-Prozess? Dann stirbt genau der.
    // Wir bauen dafür eine TrapFrame-Ansicht der fünf CPU-Felder, damit
    // scheduler.rs mit derselben Struktur arbeitet wie überall sonst.
    // unsafe: as_mut() gibt volatilen Zugriff auf den echten Rahmen auf dem
    // Stack — genau den benutzt der iretq gleich.
    let mut rahmen_zugriff = unsafe { stack_frame.as_mut() };
    let wert = rahmen_zugriff.read();
    let mut trap = crate::prozess::TrapFrame {
        rip: wert.instruction_pointer.as_u64(),
        cs: wert.code_segment,
        rflags: wert.cpu_flags,
        rsp: wert.stack_pointer.as_u64(),
        ss: wert.stack_segment,
        ..Default::default()
    };
    if crate::scheduler::user_prozess_toeten(&mut trap) {
        let mut neu = wert;
        neu.instruction_pointer = VirtAddr::new(trap.rip);
        neu.stack_pointer = VirtAddr::new(trap.rsp);
        neu.code_segment = trap.cs;
        neu.stack_segment = trap.ss;
        neu.cpu_flags = trap.rflags;
        rahmen_zugriff.write(neu);
        return true;
    }

    // (b) Der Einzelschuss-Pfad aus Serie 6, Teil 1.
    if !crate::ring3::ring3_aktiv() {
        return false;
    }
    let mut neu = wert;
    neu.instruction_pointer = VirtAddr::new(crate::ring3::recovery_rip());
    neu.stack_pointer = VirtAddr::new(crate::ring3::recovery_rsp());
    neu.code_segment = gdt::kernel_code_selektor();
    neu.stack_segment = gdt::kernel_data_selektor();
    neu.cpu_flags = 0x202; // IF gesetzt
    rahmen_zugriff.write(neu);
    true
}

// ---------------------------------------------------------------------------
// DIE ÜBRIGEN CPU-EXCEPTIONS (Serie-6-Abschluss)
// ---------------------------------------------------------------------------
//
// Alle nach demselben Muster: erst `user_recovery` fragen (kam es aus Ring 3,
// stirbt der Prozess und der Kernel läuft weiter — Dauerregel II), sonst ist
// es ein Kernel-Bug und wir halten mit klarer Meldung an.
//
// Das Makro erspart dreizehnmal denselben Rumpf — und, wichtiger: Es macht
// unmöglich, dass einer davon die `user_recovery`-Prüfung vergisst.

/// Baut einen Exception-Handler, der Ring-3-Fehler auffängt.
macro_rules! user_exception_handler {
    ($name:ident, $bezeichnung:expr) => {
        extern "x86-interrupt" fn $name(mut stack_frame: InterruptStackFrame) {
            if user_recovery(&mut stack_frame) {
                println!(
                    "EXCEPTION: {} — aus USER-MODE, der Prozess wird beendet.",
                    $bezeichnung
                );
                return;
            }
            println!("EXCEPTION: {} (im KERNEL — das ist ein Bug)", $bezeichnung);
            println!("{:#?}", stack_frame);
            crate::hlt_loop();
        }
    };
    // Variante für Exceptions MIT Fehlercode (die Segment-Fehler).
    ($name:ident, $bezeichnung:expr, mit_code) => {
        extern "x86-interrupt" fn $name(mut stack_frame: InterruptStackFrame, error_code: u64) {
            if user_recovery(&mut stack_frame) {
                println!(
                    "EXCEPTION: {} (Fehlercode {:#x}) — aus USER-MODE, der Prozess wird beendet.",
                    $bezeichnung, error_code
                );
                return;
            }
            println!(
                "EXCEPTION: {} (Fehlercode {:#x}) (im KERNEL — das ist ein Bug)",
                $bezeichnung, error_code
            );
            println!("{:#?}", stack_frame);
            crate::hlt_loop();
        }
    };
}

// Die zwei, die ein gewöhnliches Programm am ehesten auslöst:
user_exception_handler!(divide_error_handler, "DIVIDE ERROR (#DE, Division durch 0)");
user_exception_handler!(invalid_opcode_handler, "INVALID OPCODE (#UD)");
// Und der Rest, damit die Klasse geschlossen ist:
user_exception_handler!(debug_handler, "DEBUG (#DB)");
user_exception_handler!(nmi_handler, "NON-MASKABLE INTERRUPT (#NMI)");
user_exception_handler!(overflow_handler, "OVERFLOW (#OF)");
user_exception_handler!(bound_range_handler, "BOUND RANGE EXCEEDED (#BR)");
user_exception_handler!(device_not_available_handler, "DEVICE NOT AVAILABLE (#NM)");
user_exception_handler!(x87_handler, "x87 FLOATING POINT (#MF)");
user_exception_handler!(simd_handler, "SIMD FLOATING POINT (#XM)");
user_exception_handler!(invalid_tss_handler, "INVALID TSS (#TS)", mit_code);
user_exception_handler!(segment_not_present_handler, "SEGMENT NOT PRESENT (#NP)", mit_code);
user_exception_handler!(stack_segment_handler, "STACK SEGMENT FAULT (#SS)", mit_code);
user_exception_handler!(alignment_check_handler, "ALIGNMENT CHECK (#AC)", mit_code);

/// General Protection Fault: ungültige Segment-/Privileg-Operation. Aus Ring 3
/// (bei laufendem Ring-3-Code) wird er aufgefangen; sonst ist es ein
/// Kernel-Bug und wir halten an.
extern "x86-interrupt" fn general_protection_fault_handler(
    mut stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    if user_recovery(&mut stack_frame) {
        println!("EXCEPTION: GENERAL PROTECTION FAULT — aus USER-MODE, Kernel faengt es auf.");
        return;
    }
    println!("EXCEPTION: GENERAL PROTECTION FAULT (Fehlercode {:#x})", error_code);
    println!("{:#?}", stack_frame);
    crate::hlt_loop();
}

extern "x86-interrupt" fn page_fault_handler(
    mut stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    // Die CPU legt die fehlgeschlagene Adresse automatisch in CR2.
    let adresse = Cr2::read();
    let ursache = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        "Schutzverletzung (Seite vorhanden, Zugriff verboten)"
    } else {
        "Seite nicht vorhanden (nicht gemappt)"
    };
    let zugriff = if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        "Schreibzugriff"
    } else {
        "Lesezugriff"
    };

    // DAUERREGEL (II): Ein User-Mode-Fehler reisst den Kernel NICHT mit. Kam
    // der Fault aus Ring 3 (bei laufendem Ring-3-Code), fangen wir ihn auf und
    // kehren in den Kernel zurück (statt nach Ring 3, wo derselbe Fault käme).
    if user_recovery(&mut stack_frame) {
        println!("EXCEPTION: PAGE FAULT — aus USER-MODE (Ring 3)");
        println!("  Zugriff auf Adresse: {:?}", adresse);
        println!("  Ursache:  {}", ursache);
        println!("  Zugriff:  {}", zugriff);
        println!("  -> Der User-Code wird BEENDET, der Kernel laeuft weiter.");
        return; // -> Epilog macht iretq mit dem umgebogenen Rahmen
    }

    // Sonst: ein Fault im KERNEL selbst — ein echter Bug. Wie bisher anhalten
    // (weiterlaufen wäre gefährlich, ohne die Ursache zu beheben).
    println!("EXCEPTION: PAGE FAULT");
    println!("  Zugriff auf Adresse: {:?}", adresse);
    println!("  Ursache:  {}", ursache);
    println!("  Zugriff:  {}", zugriff);
    if error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
        println!("  Ausgeloest beim Laden eines Befehls (Instruction Fetch)");
    }
    println!("  Roher Fehlercode: {:?}", error_code);
    println!("{:#?}", stack_frame);
    crate::hlt_loop();
}

/// Handler für Double Faults: Beim Behandeln einer Exception ist eine
/// weitere passiert (klassisch: Stack Overflow). Läuft dank IST auf
/// unserem Notfall-Stack (siehe gdt.rs) und kann deshalb selbst dann
/// noch eine saubere Fehlermeldung ausgeben.
/// Rückgabetyp `!`: Von einem Double Fault gibt es auf x86_64 kein
/// Zurück — die Architektur erlaubt keine Rückkehr.
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64, // ist bei Double Faults immer 0
) -> ! {
    // panic! gibt die Meldung über unseren Panic-Handler
    // auf VGA UND seriell aus und hält dann an.
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// ---------------------------------------------------------------------------
// Hardware-Interrupt-Handler
// ---------------------------------------------------------------------------

/// Globaler Tick-Zähler des Timers. `AtomicU64` statt Mutex, weil der
/// Zugriff aus dem Interrupt-Kontext niemals blockieren darf.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Wie oft der Timer seit dem Boot getickt hat (~250 Ticks/Sekunde).
pub fn timer_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Die BASISARBEIT jedes Timer-Ticks: Zähler erhöhen, wartende Tick-Futures
/// wecken (beides lock-frei!) und dem PIC den Interrupt quittieren — KEINE
/// Ausgabe, das würde den Bildschirm 250x pro Sekunde vollspammen.
///
/// Das war bis Serie 6 Teil 3 der komplette Timer-Handler. Jetzt ruft
/// `scheduler::timer_dispatch` diese Funktion als ersten Schritt und macht
/// danach die Prozess-Planung. Herausgezogen bleibt sie, weil sie mit dem
/// Scheduler nichts zu tun hat — und weil so sichtbar bleibt, dass sich das
/// alte Verhalten NICHT geändert hat.
pub fn timer_basisarbeit() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    crate::zeit::tick_waker_wecken();

    // Dem PIC melden: "fertig behandelt" (End of Interrupt).
    // Ohne das schickt er nie wieder einen Timer-Interrupt!
    // unsafe: Die Interrupt-Nummer stammt aus unserem eigenen Enum und
    // ist garantiert die, die gerade behandelt wird — eine falsche
    // Nummer könnte fremde Interrupts verschlucken.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

/// Tastatur-Interrupt: NUR den Scancode lesen und an die async
/// Verarbeitung übergeben (task/keyboard.rs). Der Handler ist bewusst
/// winzig — solange er läuft, steht das restliche System still. Die
/// eigentliche Dekodierung (QWERTZ, Umlaute, Backspace) erledigt der
/// Tastatur-Task im Executor, wenn gerade Zeit dafür ist.
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Der PS/2-Controller legt den Scancode der Taste in Port 0x60.
    // Wir MÜSSEN ihn lesen, sonst sendet die Tastatur nichts mehr.
    // unsafe (Port-I/O): 0x60 ist der Standard-Datenport des PS/2-
    // Controllers; das Lesen ist genau die vorgesehene Bedienung.
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    // In die lock-freie Queue damit + Tastatur-Task wecken. Fertig!
    crate::task::keyboard::add_scancode(scancode);

    // unsafe: korrekte Interrupt-Nummer, siehe Timer-Handler.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

/// Maus-Interrupt (IRQ 12): exakt das Tastatur-Muster — Byte lesen,
/// in die lock-freie Queue, Task wecken, EOI. Nicht mehr!
extern "x86-interrupt" fn maus_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // unsafe (Port-I/O): 0x60 ist der Datenport des PS/2-Controllers.
    let mut port = Port::new(0x60);
    let byte: u8 = unsafe { port.read() };
    crate::maus::byte_hinzufuegen(byte);

    // unsafe: korrekte Interrupt-Nummer, siehe Timer-Handler.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Maus.as_u8());
    }
}

/// PCI-IRQ-Handler (virtio-net): drei Einstiegspunkte für die typischen
/// PCI-Leitungen IRQ 9/10/11 — jeder kennt seinen eigenen Vektor.
extern "x86-interrupt" fn virtio_pci_irq9(_stack_frame: InterruptStackFrame) {
    pci_virtio_irq(PIC_2_OFFSET + 1);
}
extern "x86-interrupt" fn virtio_pci_irq10(_stack_frame: InterruptStackFrame) {
    pci_virtio_irq(PIC_2_OFFSET + 2);
}
extern "x86-interrupt" fn virtio_pci_irq11(_stack_frame: InterruptStackFrame) {
    pci_virtio_irq(PIC_2_OFFSET + 3);
}

/// Gemeinsamer PCI-IRQ-Pfad (das erste ASYNCHRONE Hardware-Event
/// jenseits von Tastatur/Maus/Timer): das virtio-net-Gerät prüfen (hat
/// ES interruptet? — Shared Interrupts) und ggf. den RX-Task wecken,
/// dann EOI. Minimal wie alle Handler: KEIN Lock auf Treiber-Zustand,
/// KEINE Allokation.
fn pci_virtio_irq(vektor: u8) {
    crate::virtio::net::irq_pruefen_und_wecken();
    // unsafe: korrekte Vektor-Nummer — der Handler ist genau dort
    // registriert; notify_end_of_interrupt behandelt die Kaskade selbst.
    unsafe {
        PICS.lock().notify_end_of_interrupt(vektor);
    }
}

/// Schaltet eine EINZELNE IRQ am 8259-PIC frei (löscht ihr Maskenbit,
/// andere bleiben unberührt). Für Geräte, deren IRQ erst zur Laufzeit
/// feststeht (PCI-Enumeration) — lib::init() maskiert anfangs alles
/// außer Timer/Tastatur/Kaskade/Maus.
pub fn irq_freischalten(irq: u8) {
    use x86_64::instructions::port::Port;
    // IRQ 0-7 hängen am ersten PIC (Datenport 0x21), 8-15 am zweiten (0xA1).
    let (port_nr, bit) = if irq < 8 { (0x21u16, irq) } else { (0xA1u16, irq - 8) };
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut port: Port<u8> = Port::new(port_nr);
        // unsafe (Port-I/O): PIC-Datenregister — aktuelle Maske lesen,
        // genau das eine Bit löschen, zurückschreiben.
        unsafe {
            let maske = port.read();
            port.write(maske & !(1u8 << bit));
        }
    });
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::println;

    /// Löst absichtlich eine Breakpoint-Exception aus. Der Test gilt als
    /// bestanden, wenn die Ausführung danach normal weiterläuft — das
    /// beweist, dass unser Handler aufgerufen wird und sauber zurückkehrt.
    #[test_case]
    fn test_breakpoint_exception_laeuft_weiter() {
        x86_64::instructions::interrupts::int3();
        println!("Nach int3 geht es hier normal weiter.");
    }

    /// Prüft, dass der Timer wirklich tickt: Wir legen die CPU ein paar
    /// Mal mit hlt schlafen (sie wacht bei jedem Interrupt auf) — danach
    /// muss der Zähler gestiegen sein.
    #[test_case]
    fn test_timer_tickt() {
        let vorher = super::timer_ticks();
        for _ in 0..5 {
            x86_64::instructions::hlt();
        }
        let nachher = super::timer_ticks();
        assert!(nachher > vorher, "Timer-Zaehler ist nicht gestiegen");
    }
}
