// prozess.rs — Der Prozess-Kontrollblock (PCB) und die Prozess-Tabelle
//              (Serie 6, Teil 3 — Aufgabe 2)
//
// Ein PROZESS ist bei SpeedOS ab jetzt das, was der Scheduler umschaltet:
// ein Registersatz, ein Adressraum, ein Kernel-Stack und ein Zustand.
// Der Entwurf (WARUM es so und nicht anders aussieht) steht vollständig in
// docs/scheduler-design.md — hier steht, WIE.
//
// ==========================================================================
// DIE ZENTRALE IDEE: DER GESICHERTE KONTEXT IST EINE EINZIGE ZAHL
//
// Jeder Prozess hat seinen EIGENEN Kernel-Stack. Löst er einen Trap aus
// (Timer-Interrupt, int 0x80, Page Fault), landet die CPU auf genau diesem
// Stack — bei einem Trap aus Ring 3 sorgt TSS.RSP0 dafür (gdt::rsp0_setzen).
// Die CPU pusht dort RIP/CS/RFLAGS/RSP/SS, unser Assembler-Einstieg pusht
// alle 15 General-Register dahinter. Zusammen: der VOLLSTÄNDIGE Zustand des
// unterbrochenen Prozesses — und er liegt auf SEINEM Stack, wo er beliebig
// lange liegen bleiben darf.
//
// Deshalb ist "Kontext sichern" nur: sich den RSP merken, an dem der Rahmen
// liegt (`Prozess::kontext`). Und "Kontext wiederherstellen" ist:
// RSP dorthin setzen, 15-mal poppen, iretq. Das macht `schalte_auf_rahmen`
// in scheduler.rs — die EINZIGE Stelle im Kernel, die einen Kontext lädt.
//
//   Kernel-Stack von Prozess A            PCB von A
//   +--------------------------+ <- oben  +-----------------+
//   | SS RSP RFLAGS CS RIP     |          | kontext ------> |--+
//   | rax rbx rcx rdx rsi rdi  |          | zustand         |  |
//   | rbp r8 r9 r10 r11 ... r15|          | raum (CR3)      |  |
//   +--------------------------+ <--------+ ...             |  |
//   | frei: hier arbeitet der  |          +-----------------+  |
//   | Rust-Dispatcher          |                               |
//   +--------------------------+ <- unten (Guard-Page darunter)
// ==========================================================================

use crate::adressraum::{self, AdressRaum};
use crate::memory;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;

/// Höchstzahl gleichzeitiger Prozesse (inklusive Kernel-Prozess).
///
/// Warum eine FESTE Obergrenze und kein `Vec`? Die Prozess-Tabelle wird im
/// TIMER-INTERRUPT gelesen, und dort darf nicht allokiert werden
/// (Deadlock-Regel 2). Ein festes Array kann das garantieren, ein Vec nicht.
pub const MAX_PROZESSE: usize = 8;

/// Prozess-Nummer. 0 ist per Konstruktion der Kernel-Prozess.
pub type Pid = u32;

/// Die PID des Kernel-Prozesses (der kooperative Executor — siehe
/// docs/scheduler-design.md §1).
pub const KERNEL_PID: Pid = 0;

/// Seiten des Kernel-Stacks je Prozess (16 KiB). Darauf laufen alle
/// Trap-Dispatcher dieses Prozesses; sie allozieren nicht und rekursieren
/// nicht, 16 KiB sind reichlich.
pub const KERN_STACK_SEITEN: usize = 4;

/// Vergibt die nächste PID (0 ist für den Kernel-Prozess reserviert).
static NAECHSTE_PID: AtomicU32 = AtomicU32::new(1);

/// Wie viele Prozesse insgesamt schon erzeugt wurden (Statistik).
static ERZEUGT_GESAMT: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Der Trap-Rahmen — das Register-Bild eines unterbrochenen Prozesses
// ---------------------------------------------------------------------------

/// Der vollständige gesicherte Registersatz auf dem Kernel-Stack.
///
/// Die REIHENFOLGE ist ein Vertrag mit dem Assembler: Unsere Einstiege pushen
/// `rax` ZUERST und `r15` ZULETZT, also liegt r15 an der niedrigsten Adresse
/// und steht deshalb hier oben. Danach folgt, was die CPU beim Trap selbst
/// gepusht hat. Ein Fehler in dieser Reihenfolge äussert sich als scheinbar
/// unerklärliche Register-Korruption — genau deshalb nagelt
/// `test_trapframe_layout` jeden einzelnen Offset fest.
///
/// `size_of::<TrapFrame>() == 160` ist ebenfalls Vertrag: 160 ist durch 16
/// teilbar, und nur dadurch stimmt die C-ABI-Stack-Ausrichtung am `call` im
/// Assembler-Einstieg (Rechnung in docs/scheduler-design.md §2).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    // ---- ab hier: von der CPU beim Trap gepusht ----
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    /// Kam dieser Trap aus Ring 3? (Die unteren 2 Bits von CS sind die
    /// Privilegstufe, in der der Code lief.)
    pub fn aus_ring3(&self) -> bool {
        (self.cs & 3) == 3
    }
}

// ---------------------------------------------------------------------------
// Der Kernel-Stack eines Prozesses
// ---------------------------------------------------------------------------

/// Der private Kernel-Stack eines Prozesses — mit GUARD-PAGE darunter.
///
/// Die Guard-Page ist derselbe Gedanke wie beim User-Stack
/// (`AdressRaum::stack_anlegen`): Läuft der Stack über, gibt es SOFORT einen
/// Page Fault statt still fremden Kernel-Speicher zu zerschreiben. Der
/// Unterschied: Ein Kernel-Stack-Überlauf ist ein KERNEL-Bug, wird also nicht
/// aufgefangen, sondern gemeldet und angehalten (Dauerregel II gilt nur für
/// Fehler AUS Ring 3).
pub struct KernStack {
    /// Erste Seite des NUTZBAREN Bereichs (die Guard-Page liegt darunter).
    unten: VirtAddr,
    seiten: usize,
}

impl KernStack {
    /// Legt einen Kernel-Stack mit `seiten` nutzbaren Seiten an.
    ///
    /// Trick für die Guard-Page: Wir lassen `seiten + 1` Seiten mappen und
    /// hängen die UNTERSTE sofort wieder aus (ihr Frame geht zurück). Übrig
    /// bleibt ein Loch genau unter dem Stack.
    pub fn neu(seiten: usize) -> Option<KernStack> {
        let start = memory::allocate_pages(seiten + 1).ok()?;
        let guard = Page::containing_address(start);
        match memory::unmap_page(guard) {
            // unsafe: Die Seite ist gerade ausgehängt, niemand hält sie —
            // der Frame darf zurück in den Allocator.
            Ok(frame) => unsafe { memory::frame_freigeben(frame) },
            Err(_) => return None,
        }
        Some(KernStack {
            unten: start + 4096u64,
            seiten,
        })
    }

    /// Das OBERE Ende (Stacks wachsen nach unten) — genau der Wert, der in
    /// TSS.RSP0 gehört.
    pub fn oben(&self) -> VirtAddr {
        self.unten + (self.seiten as u64) * 4096
    }
}

impl Drop for KernStack {
    /// Gibt die physischen Frames zurück. Der VIRTUELLE Bereich bleibt
    /// verbraucht (`allocate_pages` zählt nur vorwärts) — bewusst notiert in
    /// docs/scheduler-design.md §8.
    fn drop(&mut self) {
        for i in 0..self.seiten {
            let page = Page::containing_address(self.unten + (i as u64) * 4096);
            if let Ok(frame) = memory::unmap_page(page) {
                // unsafe: eben ausgehängt, kein weiterer Besitzer.
                unsafe { memory::frame_freigeben(frame) };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Zustand und Prozess
// ---------------------------------------------------------------------------

/// Der Zustand eines Prozesses aus Sicht des Schedulers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zustand {
    /// Läuft GERADE auf der CPU (genau einer im System).
    Laeuft,
    /// Bereit, wartet nur auf eine Zeitscheibe.
    Lauffaehig,
    /// Wartet auf ein Ereignis (heute: Weck-Zeitpunkt von `SYS_SCHLAFEN`;
    /// später blockierende Syscalls) — bekommt KEINE Zeitscheibe.
    Wartend,
    /// Fertig oder abgestürzt. Bekommt nie wieder CPU; der Aufräum-Task
    /// gibt Adressraum und Kernel-Stack zurück.
    Beendet,
}

impl Zustand {
    /// Darf der Scheduler diesem Prozess eine Zeitscheibe geben?
    /// (`Laeuft` zählt mit: der aktuelle Prozess bleibt sonst nie dran.)
    pub fn ist_lauffaehig(self) -> bool {
        matches!(self, Zustand::Laeuft | Zustand::Lauffaehig)
    }

    /// Kurzer Anzeigetext (Task-Manager, Shell).
    pub fn text(self) -> &'static str {
        match self {
            Zustand::Laeuft => "laeuft",
            Zustand::Lauffaehig => "lauffaehig",
            Zustand::Wartend => "wartend",
            Zustand::Beendet => "beendet",
        }
    }
}

/// Der Prozess-Kontrollblock.
pub struct Prozess {
    pub pid: Pid,
    pub name: String,
    pub zustand: Zustand,
    /// Der eigene Adressraum — `None` beim KERNEL-Prozess (er benutzt die
    /// Kernel-P4, siehe `adressraum::kernel_aktivieren`).
    pub raum: Option<AdressRaum>,
    /// Der eigene Kernel-Stack — `None` beim Kernel-Prozess (der läuft auf
    /// dem Boot-Stack, den der Bootloader eingerichtet hat).
    pub kern_stack: Option<KernStack>,
    /// DER gesicherte Kontext: RSP, an dem der Trap-Rahmen liegt.
    /// 0 = "läuft gerade" (dann steht der Rahmen nicht fest).
    pub kontext: u64,
    /// Erzeugungszeitpunkt (us seit Boot).
    pub start_us: u64,
    /// Aufsummierte CPU-Zeit.
    pub cpu_us: u64,
    /// Beginn der laufenden Zeitscheibe (für die CPU-Zeit-Abrechnung).
    pub scheibe_start_us: u64,
    /// Wie oft wurde dieser Prozess AUS RING 3 verdrängt? Der
    /// Präemptions-Beweis (Aufgabe 4) hängt an dieser Zahl.
    pub praemptionen: u64,
    /// Wie oft hat er FREIWILLIG abgegeben (yield/schlafen/exit)?
    pub abgaben: u64,
    /// Zahl der Syscalls (Aktivitätsmaß).
    pub syscalls: u64,
    /// Weck-Zeitpunkt in ms seit Boot (nur bei `Zustand::Wartend`; 0 = keiner).
    pub wach_ab_ms: u64,
}

impl Prozess {
    /// Der KERNEL-Prozess (PID 0): kein eigener Adressraum, kein eigener
    /// Kernel-Stack — er ist der Kontext, in dem SpeedOS gebootet ist und in
    /// dem der kooperative Executor läuft.
    pub fn kernel() -> Prozess {
        let jetzt = crate::zeit::us_seit_boot();
        Prozess {
            pid: KERNEL_PID,
            name: String::from("SpeedOS-Kernel (Executor)"),
            // Er LÄUFT — von ihm aus wird alles andere eingeplant.
            zustand: Zustand::Laeuft,
            raum: None,
            kern_stack: None,
            kontext: 0,
            start_us: jetzt,
            cpu_us: 0,
            scheibe_start_us: jetzt,
            praemptionen: 0,
            abgaben: 0,
            syscalls: 0,
            wach_ab_ms: 0,
        }
    }

    /// Baut einen RING-3-Prozess: eigener Adressraum, eigener Kernel-Stack und
    /// ein von Hand geschriebener Start-Trap-Rahmen.
    ///
    /// DER TRICK (docs/scheduler-design.md §2): Der Prozess wird nie
    /// "gestartet". Wir legen ihm einen Trap-Rahmen hin, der so aussieht, als
    /// wäre er schon einmal gelaufen und gerade verdrängt worden. Der
    /// Scheduler WECHSELT dann einfach zu ihm — ein Sonderfall weniger.
    pub fn neu_ring3(
        name: impl Into<String>,
        raum: AdressRaum,
        einsprung: VirtAddr,
        user_stack_oben: VirtAddr,
    ) -> Option<Prozess> {
        let kern_stack = KernStack::neu(KERN_STACK_SEITEN)?;
        let jetzt = crate::zeit::us_seit_boot();

        // Der Start-Rahmen liegt am oberen Ende des Kernel-Stacks — exakt da,
        // wo ihn ein echter Trap aus Ring 3 hingelegt hätte.
        let rahmen_adresse = kern_stack.oben().as_u64() - core::mem::size_of::<TrapFrame>() as u64;
        let rahmen = TrapFrame {
            rip: einsprung.as_u64(),
            cs: crate::gdt::user_code_selektor(),
            // RFLAGS mit IF: Der Prozess läuft MIT Interrupts — anders könnte
            // ihn der Timer nie verdrängen (und genau das ist der Zweck).
            rflags: 0x202,
            rsp: user_stack_oben.as_u64(),
            ss: crate::gdt::user_data_selektor(),
            // Alle General-Register auf 0: ein frischer Prozess darf keine
            // Reste eines anderen sehen.
            ..TrapFrame::default()
        };
        // unsafe: `rahmen_adresse` liegt in unserem eben allozierten, exklusiv
        // besessenen Kernel-Stack — gemappt, beschreibbar, 16-ausgerichtet.
        unsafe {
            core::ptr::write(rahmen_adresse as *mut TrapFrame, rahmen);
        }

        ERZEUGT_GESAMT.fetch_add(1, Ordering::Relaxed);
        Some(Prozess {
            pid: NAECHSTE_PID.fetch_add(1, Ordering::Relaxed),
            name: name.into(),
            // LAUFFÄHIG, nicht "läuft": Die CPU bekommt er erst beim nächsten
            // Timer-Tick (Invariante 1 des Entwurfs).
            zustand: Zustand::Lauffaehig,
            raum: Some(raum),
            kern_stack: Some(kern_stack),
            kontext: rahmen_adresse,
            start_us: jetzt,
            cpu_us: 0,
            scheibe_start_us: jetzt,
            praemptionen: 0,
            abgaben: 0,
            syscalls: 0,
            wach_ab_ms: 0,
        })
    }

    /// Das obere Ende des Kernel-Stacks (für TSS.RSP0). Beim Kernel-Prozess
    /// der statische Standard-Stack aus gdt.rs.
    pub fn kern_stack_oben(&self) -> VirtAddr {
        match &self.kern_stack {
            Some(stack) => stack.oben(),
            None => crate::gdt::rsp0_standard(),
        }
    }

    /// Läuft dieser Prozess in einem EIGENEN Adressraum (= ist ein
    /// User-Prozess)?
    pub fn ist_user(&self) -> bool {
        self.raum.is_some()
    }
}

/// Anzahl aller je erzeugten Ring-3-Prozesse.
pub fn erzeugt_gesamt() -> usize {
    ERZEUGT_GESAMT.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Momentaufnahme für Anzeige (Task-Manager, Shell)
// ---------------------------------------------------------------------------

/// Eine Zeile der Prozess-Momentaufnahme — reine Daten, keine Referenzen
/// (damit der Tabellen-Lock sofort wieder frei ist).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProzessMoment {
    pub pid: Pid,
    pub name: String,
    pub zustand: Zustand,
    pub cpu_us: u64,
    pub laufzeit_us: u64,
    pub praemptionen: u64,
    pub abgaben: u64,
    pub syscalls: u64,
    pub ist_user: bool,
}

// ---------------------------------------------------------------------------
// Die DEMO-PROZESSE: zwei Zähler, die niemals freiwillig abgeben
// (Aufgabe 4 — der Präemptions-Beweis)
// ---------------------------------------------------------------------------

/// Basis-Adresse des Programms im (privaten!) User-Bereich jedes Prozesses.
pub const ZAEHLER_CODE_VA: u64 = adressraum::USER_START;
/// Offset der Ausgabe-Nachricht in derselben Seite.
pub const ZAEHLER_TEXT_OFFSET: u64 = 0x100;
/// Spitze des User-Stacks (1 MiB über dem Code; dazwischen ungemappt).
pub const ZAEHLER_STACK_OBEN: u64 = ZAEHLER_CODE_VA + 0x10_0000;
/// Seiten des User-Stacks (mit Guard-Page darunter).
pub const ZAEHLER_STACK_SEITEN: usize = 4;

/// Die Nachricht-Vorlage. Position 1 trägt die Prozess-KENNUNG ('A'/'B'),
/// Position 12 die letzte Hex-Ziffer des Zählers.
const ZAEHLER_TEXT: &[u8] = b"[A] zaehlt: 0\n";
/// Index der Kennung in der Vorlage.
const KENNUNG_INDEX: usize = 1;
/// Index der Zählerziffer in der Vorlage.
const ZIFFER_INDEX: usize = 12;

/// Wie viele Leerlauf-Runden zwischen zwei Ausgaben gerechnet werden.
///
/// Der Sinn dieser Bremse ist der BEWEIS, nicht die Optik: Zwischen zwei
/// Syscalls muss so viel Rechenzeit liegen, dass die Zeitscheibe (20 ms)
/// MITTEN in der Schleife abläuft. Nur dann wird der Prozess nachweislich
/// aus Ring 3 verdrängt, statt an einer Syscall-Grenze zu wechseln.
const BREMSE: u32 = 5_000_000;

/// Baut den Maschinencode des Zähler-Programms — von Hand assembliert, weil
/// wir (noch) keinen ELF-Loader haben.
///
/// ```text
///   xor  ebx, ebx              ; Zähler = 0
/// schleife:
///   inc  rbx                   ; zählen
///   mov  ecx, BREMSE           ; die Bremse: eine lange Rechenschleife,
/// bremse:                      ;   damit die Zeitscheibe MITTEN hier abläuft
///   dec  ecx
///   jnz  bremse
///   mov  rax, rbx              ; letzte Hex-Ziffer des Zählers ...
///   and  al, 0x0f
///   add  al, '0'
///   cmp  al, '9'
///   jbe  keine_buchstabe
///   add  al, 0x27              ;   ... 10..15 werden 'a'..'f'
/// keine_buchstabe:
///   movabs rdx, ziffer_va      ; ... in die Nachricht schreiben
///   mov  [rdx], al
///   mov  rax, 0                ; SYS_DEBUG_PRINT
///   movabs rdi, text_va
///   mov  esi, laenge
///   int  0x80                  ; DER EINZIGE Kernel-Kontakt — und er gibt
///   jmp  schleife              ;   die CPU NICHT ab!
/// ```
///
/// Beachte, was hier NICHT steht: kein `yield`, kein `exit`, kein Warten.
/// Dieses Programm gibt die CPU unter keinen Umständen freiwillig her.
pub fn zaehler_programm(text_va: u64, laenge: usize) -> Vec<u8> {
    let ziffer_va = text_va + ZIFFER_INDEX as u64;
    let mut p = Vec::new();
    p.extend_from_slice(&[0x31, 0xDB]); // xor ebx, ebx
    let schleife = p.len(); // Sprungziel
    p.extend_from_slice(&[0x48, 0xFF, 0xC3]); // inc rbx
    p.push(0xB9); // mov ecx, imm32
    p.extend_from_slice(&BREMSE.to_le_bytes());
    p.extend_from_slice(&[0xFF, 0xC9]); // dec ecx
    p.extend_from_slice(&[0x75, 0xFC]); // jnz -4 (zurück zu dec ecx)
    p.extend_from_slice(&[0x48, 0x89, 0xD8]); // mov rax, rbx
    p.extend_from_slice(&[0x24, 0x0F]); // and al, 0x0f
    p.extend_from_slice(&[0x04, 0x30]); // add al, '0'
    p.extend_from_slice(&[0x3C, 0x39]); // cmp al, '9'
    p.extend_from_slice(&[0x76, 0x02]); // jbe +2 (Ziffer bleibt Ziffer)
    p.extend_from_slice(&[0x04, 0x27]); // add al, 0x27 ('a'..'f')
    p.extend_from_slice(&[0x48, 0xBA]); // movabs rdx, ziffer_va
    p.extend_from_slice(&ziffer_va.to_le_bytes());
    p.extend_from_slice(&[0x88, 0x02]); // mov [rdx], al
    p.extend_from_slice(&[0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x00]); // mov rax, 0
    p.extend_from_slice(&[0x48, 0xBF]); // movabs rdi, text_va
    p.extend_from_slice(&text_va.to_le_bytes());
    p.push(0xBE); // mov esi, imm32
    p.extend_from_slice(&(laenge as u32).to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
                                        // jmp schleife (rel8, rückwärts — der Rumpf ist < 128 Byte)
    let rel = schleife as i64 - (p.len() as i64 + 2);
    p.push(0xEB);
    p.push(rel as i8 as u8);
    p
}

/// Baut einen SCHLÄFER: ruft in einer Endlosschleife `SYS_SCHLAFEN(ms)` auf.
/// Er beweist den Zustand `Wartend` — und dass ein wartender Prozess fast
/// keine CPU-Zeit verbraucht, obwohl er lebt.
///
/// ```text
/// schleife:
///   mov  rax, 3          ; SYS_SCHLAFEN
///   mov  edi, ms
///   int  0x80
///   jmp  schleife
/// ```
pub fn schlaefer_programm(ms: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xC7, 0xC0, 0x03, 0x00, 0x00, 0x00]); // mov rax, 3
    p.push(0xBF); // mov edi, imm32
    p.extend_from_slice(&ms.to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    let rel = 0i64 - (p.len() as i64 + 2);
    p.push(0xEB);
    p.push(rel as i8 as u8);
    p
}

/// Baut ein Programm, das VERBOTEN auf eine Kernel-Adresse zugreift und damit
/// einen Page Fault auslöst. Es beweist Dauerregel II jetzt PROZESS-WEISE: Der
/// Prozess stirbt, der Kernel läuft weiter — und alle anderen Prozesse auch.
///
/// ```text
///   movabs rax, kernel_adresse
///   mov    al, [rax]      ; Ring 3 darf das nicht -> #PF
///   jmp    $              ; wird nie erreicht
/// ```
pub fn absturz_programm(kernel_adresse: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xB8]); // movabs rax, kernel_adresse
    p.extend_from_slice(&kernel_adresse.to_le_bytes());
    p.extend_from_slice(&[0x8A, 0x00]); // mov al, [rax]
    p.extend_from_slice(&[0xEB, 0xFE]); // jmp $
    p
}

/// Setzt einen Prozess-Adressraum für ein Programm auf: eine Code-/Daten-Seite
/// und einen User-Stack mit Guard-Page. Liefert (Adressraum, Einsprung,
/// Stack-Spitze).
///
/// Der Code wird über `raum.schreiben()` hineingelegt — also über das
/// Physik-Komplettmapping, OHNE den Adressraum zu aktivieren. Genau dieses
/// Muster wird der ELF-Loader benutzen.
pub fn programm_aufsetzen(
    code: &[u8],
    text: Option<&[u8]>,
) -> Option<(AdressRaum, VirtAddr, VirtAddr)> {
    let mut raum = AdressRaum::neu().ok()?;
    raum.bereich_mappen(VirtAddr::new(ZAEHLER_CODE_VA), 4096).ok()?;
    raum.stack_anlegen(VirtAddr::new(ZAEHLER_STACK_OBEN), ZAEHLER_STACK_SEITEN)
        .ok()?;
    raum.schreiben(VirtAddr::new(ZAEHLER_CODE_VA), code).ok()?;
    if let Some(t) = text {
        raum.schreiben(VirtAddr::new(ZAEHLER_CODE_VA + ZAEHLER_TEXT_OFFSET), t)
            .ok()?;
    }
    Some((
        raum,
        VirtAddr::new(ZAEHLER_CODE_VA),
        VirtAddr::new(ZAEHLER_STACK_OBEN),
    ))
}

/// Die Nachricht eines Zähler-Prozesses mit eingesetzter Kennung.
pub fn zaehler_text(kennung: u8) -> Vec<u8> {
    let mut text = ZAEHLER_TEXT.to_vec();
    text[KENNUNG_INDEX] = kennung;
    text
}

/// Baut einen kompletten Zähler-Prozess (Adressraum + Programm + PCB).
pub fn zaehler_prozess(kennung: u8) -> Option<Prozess> {
    let text = zaehler_text(kennung);
    let text_va = ZAEHLER_CODE_VA + ZAEHLER_TEXT_OFFSET;
    let code = zaehler_programm(text_va, text.len());
    let (raum, einsprung, stack) = programm_aufsetzen(&code, Some(&text))?;
    Prozess::neu_ring3(
        alloc::format!("Zaehler {}", kennung as char),
        raum,
        einsprung,
        stack,
    )
}

/// Baut einen Schläfer-Prozess (beweist `Zustand::Wartend`).
pub fn schlaefer_prozess(ms: u32) -> Option<Prozess> {
    let code = schlaefer_programm(ms);
    let (raum, einsprung, stack) = programm_aufsetzen(&code, None)?;
    Prozess::neu_ring3(alloc::format!("Schlaefer {} ms", ms), raum, einsprung, stack)
}

/// Baut einen Prozess, der sofort abstürzt (Zugriff auf den Kernel-Heap).
pub fn absturz_prozess() -> Option<Prozess> {
    let code = absturz_programm(crate::allocator::HEAP_START as u64);
    let (raum, einsprung, stack) = programm_aufsetzen(&code, None)?;
    Prozess::neu_ring3("Absturz-Kandidat", raum, einsprung, stack)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// DER Vertrag zwischen Rust und Assembler: Jedes Feld muss genau dort
    /// liegen, wo der Einstieg es hinpusht. Bricht dieser Test, würde der
    /// Kontext-Wechsel Register vertauschen — ein Fehlerbild, das man von
    /// aussen praktisch nicht diagnostizieren kann. Deshalb prüfen wir hier
    /// ALLE 20 Offsets von Hand.
    #[test_case]
    fn test_trapframe_layout() {
        use core::mem::{offset_of, size_of};

        // Push-Reihenfolge des Einstiegs: rax, rbx, rcx, rdx, rsi, rdi, rbp,
        // r8..r15. Der ZULETZT gepushte (r15) liegt am NIEDRIGSTEN Offset.
        assert_eq!(offset_of!(TrapFrame, r15), 0x00);
        assert_eq!(offset_of!(TrapFrame, r14), 0x08);
        assert_eq!(offset_of!(TrapFrame, r13), 0x10);
        assert_eq!(offset_of!(TrapFrame, r12), 0x18);
        assert_eq!(offset_of!(TrapFrame, r11), 0x20);
        assert_eq!(offset_of!(TrapFrame, r10), 0x28);
        assert_eq!(offset_of!(TrapFrame, r9), 0x30);
        assert_eq!(offset_of!(TrapFrame, r8), 0x38);
        assert_eq!(offset_of!(TrapFrame, rbp), 0x40);
        assert_eq!(offset_of!(TrapFrame, rdi), 0x48);
        assert_eq!(offset_of!(TrapFrame, rsi), 0x50);
        assert_eq!(offset_of!(TrapFrame, rdx), 0x58);
        assert_eq!(offset_of!(TrapFrame, rcx), 0x60);
        assert_eq!(offset_of!(TrapFrame, rbx), 0x68);
        assert_eq!(offset_of!(TrapFrame, rax), 0x70);
        // Der von der CPU gepushte Teil — die Reihenfolge schreibt die
        // Architektur vor, nicht wir.
        assert_eq!(offset_of!(TrapFrame, rip), 0x78);
        assert_eq!(offset_of!(TrapFrame, cs), 0x80);
        assert_eq!(offset_of!(TrapFrame, rflags), 0x88);
        assert_eq!(offset_of!(TrapFrame, rsp), 0x90);
        assert_eq!(offset_of!(TrapFrame, ss), 0x98);
        // 160 Byte, durch 16 teilbar -> die C-ABI-Ausrichtung am `call`
        // stimmt (Rechnung in docs/scheduler-design.md §2).
        assert_eq!(size_of::<TrapFrame>(), 160);
        assert_eq!(size_of::<TrapFrame>() % 16, 0);
    }

    /// Ring-3-Erkennung am gesicherten CS.
    #[test_case]
    fn test_trapframe_aus_ring3() {
        let kernel = TrapFrame {
            cs: crate::gdt::kernel_code_selektor(),
            ..TrapFrame::default()
        };
        assert!(!kernel.aus_ring3(), "Kernel-CS darf nicht als Ring 3 gelten");
        let user = TrapFrame {
            cs: crate::gdt::user_code_selektor(),
            ..TrapFrame::default()
        };
        assert!(user.aus_ring3(), "User-CS (RPL 3) muss als Ring 3 gelten");
    }

    /// Zustands-Logik: nur `Laeuft`/`Lauffaehig` bekommen Zeitscheiben.
    #[test_case]
    fn test_zustand_lauffaehig() {
        assert!(Zustand::Laeuft.ist_lauffaehig());
        assert!(Zustand::Lauffaehig.ist_lauffaehig());
        assert!(!Zustand::Wartend.ist_lauffaehig());
        assert!(!Zustand::Beendet.ist_lauffaehig());
    }

    /// Der Kernel-Stack hat wirklich eine Guard-Page darunter, und sein Abriss
    /// ist frame-neutral.
    #[test_case]
    fn test_kern_stack_guard_und_bilanz() {
        use x86_64::structures::paging::PageTableFlags;

        let (frei_vorher, _) = memory::frame_statistik();
        {
            let stack = KernStack::neu(4).expect("Kernel-Stack anlegen");
            let oben = stack.oben();
            // Alle vier Seiten sind da ...
            for i in 1..=4u64 {
                let flags = memory::seiten_flags(oben - i * 4096);
                assert!(flags.is_some(), "Kernel-Stack-Seite {} fehlt", i);
                assert!(flags
                    .unwrap()
                    .contains(PageTableFlags::WRITABLE | PageTableFlags::PRESENT));
                // Und sie sind KEIN User-Speicher (das ist ein KERNEL-Stack).
                assert!(!flags.unwrap().contains(PageTableFlags::USER_ACCESSIBLE));
            }
            // ... und direkt darunter liegt das Loch.
            assert!(
                memory::seiten_flags(oben - 5u64 * 4096).is_none(),
                "Guard-Page unter dem Kernel-Stack fehlt"
            );
        }
        let (frei_nachher, _) = memory::frame_statistik();
        assert_eq!(frei_vorher, frei_nachher, "Kernel-Stack hat Frames geleckt");
    }

    /// Der handgebaute Start-Rahmen sieht genau so aus wie ein echter Trap aus
    /// Ring 3 — inklusive Lage am oberen Stack-Ende und 16er-Ausrichtung.
    #[test_case]
    fn test_start_rahmen_ist_ring3_rahmen() {
        let text = zaehler_text(b'T');
        let code = zaehler_programm(ZAEHLER_CODE_VA + ZAEHLER_TEXT_OFFSET, text.len());
        let (raum, einsprung, stack) =
            programm_aufsetzen(&code, Some(&text)).expect("Programm aufsetzen");
        let prozess = Prozess::neu_ring3("Rahmen-Test", raum, einsprung, stack).expect("PCB");

        assert!(prozess.ist_user());
        assert_eq!(prozess.zustand, Zustand::Lauffaehig);
        assert_ne!(prozess.kontext, 0, "Ein neuer Prozess braucht einen Kontext");
        assert_eq!(prozess.kontext % 16, 0, "Rahmen muss 16-ausgerichtet liegen");
        assert_eq!(
            prozess.kontext + core::mem::size_of::<TrapFrame>() as u64,
            prozess.kern_stack_oben().as_u64(),
            "Der Start-Rahmen muss GENAU am oberen Stack-Ende liegen"
        );

        // unsafe: Der Rahmen liegt in unserem eigenen Kernel-Stack.
        let rahmen = unsafe { *(prozess.kontext as *const TrapFrame) };
        assert!(rahmen.aus_ring3(), "Start-Rahmen muss nach Ring 3 zeigen");
        assert_eq!(rahmen.rip, ZAEHLER_CODE_VA);
        assert_eq!(rahmen.rsp, ZAEHLER_STACK_OBEN);
        assert_eq!(rahmen.ss, crate::gdt::user_data_selektor());
        // IF MUSS gesetzt sein — sonst könnte der Timer den Prozess nie
        // verdrängen, und der ganze Scheduler wäre wirkungslos.
        assert_eq!(rahmen.rflags & (1 << 9), 1 << 9, "IF fehlt im Start-Rahmen");
        // Alle General-Register genullt (kein Datenleck aus dem Kernel).
        assert_eq!(rahmen.rax, 0);
        assert_eq!(rahmen.rbx, 0);
        assert_eq!(rahmen.r15, 0);
    }

    /// Das Zähler-Programm: die Sprünge müssen stimmen (rückwärts in die
    /// Schleife, vorwärts über die Buchstaben-Korrektur) und es darf KEINEN
    /// Abgabe-Syscall enthalten — sonst wäre der Präemptions-Beweis wertlos.
    #[test_case]
    fn test_zaehler_programm_gibt_nie_ab() {
        let text = zaehler_text(b'A');
        let text_va = ZAEHLER_CODE_VA + ZAEHLER_TEXT_OFFSET;
        let code = zaehler_programm(text_va, text.len());

        // Genau EIN int 0x80 im ganzen Programm ...
        let syscalls = code.windows(2).filter(|f| f[0] == 0xCD && f[1] == 0x80).count();
        assert_eq!(syscalls, 1, "Der Zaehler darf nur EINEN Syscall machen");
        // ... und zwar mit rax = 0 (SYS_DEBUG_PRINT). Ein `mov rax, 1/3/4`
        // (exit/schlafen/yield) darf nicht vorkommen.
        for nummer in [1u8, 3, 4] {
            let muster = [0x48, 0xC7, 0xC0, nummer, 0x00, 0x00, 0x00];
            assert!(
                !code.windows(muster.len()).any(|f| f == &muster[..]),
                "Der Zaehler enthaelt einen Abgabe-Syscall ({})",
                nummer
            );
        }
        // Der Rücksprung muss ins Programm zeigen (rel8, negativ).
        assert_eq!(code[code.len() - 2], 0xEB, "Endlosschleife fehlt");
        let rel = code[code.len() - 1] as i8 as i64;
        let ziel = code.len() as i64 + rel;
        assert_eq!(ziel, 2, "Der Ruecksprung muss auf 'schleife' zeigen");
        // Die Bremse steht im Programm (sonst wechselt es an Syscall-Grenzen).
        let bremse = BREMSE.to_le_bytes();
        assert!(code.windows(bremse.len()).any(|f| f == &bremse[..]));
    }
}
