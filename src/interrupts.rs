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
    Timer = PIC_1_OFFSET,      // 32: der PIT-Timer (tickt ~18,2x pro Sekunde)
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
/// ~18,2 Interrupts pro Sekunde (Teiler 65536, Rate-Generator-Modus).
///
/// Lektion aus der bootloader-0.11-Migration: Unter klassischem BIOS
/// hatte die Firmware den PIT immer schon so eingestellt — unter UEFI
/// tut das NIEMAND für uns. Ohne diese Initialisierung feuert IRQ 0
/// nie, und alles, was auf Timer-Ticks wartet (zeit.rs, hlt-Aufwachen,
/// Executor-Notfallpfad), steht still. Ein OS stellt seine Uhr selbst!
pub fn pit_initialisieren() {
    use x86_64::instructions::port::Port;

    let mut kommando: Port<u8> = Port::new(0x43);
    let mut kanal_0: Port<u8> = Port::new(0x40);
    // unsafe (Port-I/O): Standard-PIT-Ports; die Werte sind die seit
    // 1981 dokumentierte Konfiguration (Kanal 0, Low/High-Byte,
    // Modus 2 = Rate-Generator, binär).
    unsafe {
        kommando.write(0b0011_0100);
        kanal_0.write(0x00); // Teiler Low-Byte  (0x0000 = 65536)
        kanal_0.write(0x00); // Teiler High-Byte
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
        idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Maus.as_usize()].set_handler_fn(maus_interrupt_handler);
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
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    // Die CPU legt die Adresse, deren Zugriff fehlschlug,
    // automatisch ins Register CR2.
    println!("EXCEPTION: PAGE FAULT");
    println!("  Zugriff auf Adresse: {:?}", Cr2::read());
    // Den Fehlercode in verständliche Aussagen übersetzen:
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

/// Wie oft der Timer seit dem Boot getickt hat (~18,2 Ticks/Sekunde).
pub fn timer_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Timer-Interrupt: Zähler erhöhen und wartende Tick-Futures wecken
/// (beides lock-frei!) — KEINE Ausgabe, das würde den Bildschirm
/// ~18x pro Sekunde vollspammen.
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
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
