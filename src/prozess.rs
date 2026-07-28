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

/// WIE ein Prozess geendet hat (Serie 6, Teil 5).
///
/// Der Unterschied ist für den Aufrufer wesentlich: `exit(0)` heisst „hat
/// seine Arbeit getan", `exit(3)` heisst „hat einen Fehler gemeldet", und
/// `Abgestuerzt` heisst „hat gegen die Regeln verstossen und wurde vom Kernel
/// abgeräumt". Ein Exit-Code kann das nicht ausdrücken — deshalb ein Enum
/// und keine Zahl mit Sonderwerten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProzessEnde {
    /// Sauber per `exit(code)` beendet.
    Beendet(u64),
    /// Durch einen Fault (Page Fault, #GP) vom Kernel getötet — Dauerregel II.
    Abgestuerzt,
    /// Von aussen beendet (Shell `prozess-stop`, Task-Manager).
    Gestoppt,
}

impl ProzessEnde {
    /// Kurzer Anzeigetext.
    pub fn text(self) -> &'static str {
        match self {
            ProzessEnde::Beendet(0) => "erfolgreich beendet",
            ProzessEnde::Beendet(_) => "mit Fehlercode beendet",
            ProzessEnde::Abgestuerzt => "abgestuerzt (vom Kernel beendet)",
            ProzessEnde::Gestoppt => "von aussen gestoppt",
        }
    }

    /// Der Exit-Code, wie ihn eine Shell weitergeben würde (Absturz und
    /// Stopp bekommen die üblichen Ersatzwerte).
    pub fn code(self) -> u64 {
        match self {
            ProzessEnde::Beendet(code) => code,
            ProzessEnde::Abgestuerzt => 139, // wie POSIX bei SIGSEGV
            ProzessEnde::Gestoppt => 143,    // wie POSIX bei SIGTERM
        }
    }
}

// ---------------------------------------------------------------------------
// WORAUF EIN PROZESS WARTET (Serie 6, Teil 6)
// ---------------------------------------------------------------------------

/// Der Grund, aus dem ein Prozess im Zustand `Wartend` liegt.
///
/// Bis Teil 5 gab es genau EINEN Grund (`schlafe(ms)`), und der Timer prüfte
/// ihn mit einem Zeitvergleich. Sobald Prozesse aufeinander warten, reicht
/// das nicht mehr: Der Timer muss WISSEN, worauf jemand wartet, um
/// entscheiden zu können, ob er wieder lauffähig ist.
///
/// DIE ENTSCHEIDUNG: NACHSEHEN STATT ANSTOSSEN. Ein Weckruf könnte auch von
/// dem kommen, der die Bedingung erfüllt (der schreibende Prozess weckt den
/// lesenden). Das wäre schneller — und es wäre eine Kette von Locks quer
/// durch den Kernel, genommen aus einem Syscall heraus, in dem wir nicht
/// warten dürfen. Stattdessen fragt der TIMER bei jedem Tick nach: „Ist die
/// Bedingung jetzt erfüllt?" Er hält dabei nichts fest (überall `try_lock`),
/// und der Preis ist eine Weck-Verzögerung von höchstens einem Tick (4 ms).
/// Für ein System mit 20-ms-Zeitscheiben ist das nicht messbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warteauf {
    /// `schlafe(ms)` — der Weckzeitpunkt steht in `wach_ab_ms`.
    Zeit,
    /// `warte(pid)` — bis dieses Kind endet. `0` heisst „irgendein Kind".
    Kind(Pid),
    /// `lese` auf einer leeren Pipe.
    PipeLesen(crate::pipe::PipeId),
    /// `schreibe` auf einer vollen Pipe.
    PipeSchreiben(crate::pipe::PipeId),
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
    /// WIE der Prozess geendet hat (`None`, solange er lebt). Wird gesetzt,
    /// wenn `zustand` auf `Beendet` wechselt — an ALLEN drei Stellen: `exit`,
    /// Absturz und Beenden von aussen.
    pub ende: Option<ProzessEnde>,
    /// WORAUF er wartet (nur bei `Zustand::Wartend` gesetzt).
    pub wartet_auf: Option<Warteauf>,
    /// Wer ihn gestartet hat. `None` = direkt vom Kernel (Shell, Explorer).
    pub eltern: Option<Pid>,
    /// DIE EXIT-CODES DER EIGENEN KINDER, bis sie jemand abholt.
    ///
    /// Hier steckt die Zombie-Vermeidung, und zwar mit einer bewussten
    /// Umkehrung: Ein Unix hält den *Kind*-Prozesseintrag am Leben, bis der
    /// Elternteil `wait` ruft — das ist der Zombie. Wir tun das Gegenteil:
    /// Beim Ende eines Kindes wird sein Ergebnis in DIESES Feld des
    /// ELTERNTEILS kopiert, und der Kind-Eintrag verschwindet sofort
    /// vollständig (Adressraum, Kernel-Stack, Handles). Es gibt also gar
    /// keinen Zustand, in dem ein toter Prozess noch Ressourcen hält.
    ///
    /// Zwei Folgen, beide gewollt:
    ///  * Stirbt der Elternteil, verschwinden die ungelesenen Ergebnisse mit
    ///    ihm — niemand muss sie noch abholen. Kein Waisen-Aufsammler nötig.
    ///  * Der Platz ist ENDLICH (festes Feld, keine Allokation im
    ///    Syscall-Pfad). Läuft er über, geht das ÄLTESTE Ergebnis verloren,
    ///    nicht das neueste: Wer nicht abholt, verliert die alten Nachrichten.
    pub kinder_enden: [Option<(Pid, ProzessEnde)>; MAX_PROZESSE],
    /// Die PER-PROZESS-HANDLE-TABELLE (Serie 6, Teil 4): kleine Zahlen, die
    /// NUR in diesem Prozess gelten. Sie steckt bewusst IM Prozess-
    /// Kontrollblock — dadurch schließt ihr `Drop` beim Prozess-Ende
    /// automatisch alle offenen Sockets, ohne dass irgendein Pfad das
    /// vergessen könnte (siehe `syscall::handle::HandleTabelle`).
    pub handles: crate::syscall::handle::HandleTabelle,
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
            ende: None,
            wartet_auf: None,
            eltern: None,
            kinder_enden: [None; MAX_PROZESSE],
            handles: crate::syscall::handle::HandleTabelle::neu(),
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
        Prozess::neu_ring3_mit_start(name, raum, einsprung, user_stack_oben, 0, 0)
    }

    /// Wie `neu_ring3`, aber mit START-ARGUMENTEN in rdi und rsi.
    ///
    /// DAS IST DIE ARGUMENT-ÜBERGABE VON SPEEDOS (docs/syscalls.md §9): Ein
    /// frischer Prozess bekommt `argc` in **rdi** und einen Zeiger auf ein
    /// Feld von `(Zeiger, Länge)`-Paaren in **rsi** — also genau die
    /// Aufrufkonvention, die auch `extern "C" fn(u64, *const ArgEintrag)`
    /// erwartet. Zwei Entscheidungen stecken darin:
    ///
    ///  * ARGUMENTE STEHEN IN REGISTERN, nicht (nur) auf dem Stack. Der
    ///    System-V-Weg legt argc/argv auf den Stack und verlangt vom
    ///    Startcode, sie von dort zu klauben. Register sind schlicht
    ///    einfacher, und wir schreiben unser Start-Runtime selbst.
    ///  * ARGUMENTE SIND (ZEIGER, LÄNGE), NIE NULLTERMINIERT. Das ist
    ///    dieselbe Regel wie in der Syscall-ABI: Nirgendwo im System sucht
    ///    jemand ein Terminator-Byte in fremdem Speicher. Ein Argument darf
    ///    deshalb jedes Byte enthalten, auch die Null.
    pub fn neu_ring3_mit_start(
        name: impl Into<String>,
        raum: AdressRaum,
        einsprung: VirtAddr,
        user_stack_oben: VirtAddr,
        rdi: u64,
        rsi: u64,
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
            // Die Start-Argumente (siehe Doku oben) ...
            rdi,
            rsi,
            // ... und ALLE übrigen General-Register auf 0: ein frischer
            // Prozess darf keine Reste eines anderen sehen.
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
            ende: None,
            wartet_auf: None,
            eltern: None,
            kinder_enden: [None; MAX_PROZESSE],
            handles: crate::syscall::handle::HandleTabelle::neu(),
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

    // --- Die Eltern-Seite der Eltern-Kind-Beziehung ---

    /// Legt das Ergebnis eines beendeten Kindes ab.
    ///
    /// Ist kein Platz mehr, wird der ÄLTESTE Eintrag überschrieben: Ein
    /// Elternteil, der nie abholt, soll das Weiterlaufen seiner Kinder nicht
    /// verhindern — und die jüngste Nachricht ist die, auf die jemand
    /// vermutlich gerade wartet.
    pub fn kind_ende_ablegen(&mut self, pid: Pid, ende: ProzessEnde) {
        if let Some(platz) = self.kinder_enden.iter_mut().find(|p| p.is_none()) {
            *platz = Some((pid, ende));
            return;
        }
        // Voll: nach vorne rücken und hinten anhängen.
        self.kinder_enden.rotate_left(1);
        self.kinder_enden[MAX_PROZESSE - 1] = Some((pid, ende));
    }

    /// Ist ein Ergebnis abholbereit? `pid == 0` heisst „irgendeines".
    /// Reine Abfrage ohne Seiteneffekt — genau das, was der Timer für seine
    /// Weck-Entscheidung braucht.
    pub fn kind_ende_bereit(&self, pid: Pid) -> bool {
        self.kinder_enden.iter().flatten().any(|(kind, _)| pid == 0 || *kind == pid)
    }

    /// Holt ein Ergebnis AB (und entfernt es). `pid == 0` = das erste beste.
    pub fn kind_ende_abholen(&mut self, pid: Pid) -> Option<(Pid, ProzessEnde)> {
        let platz = self
            .kinder_enden
            .iter()
            .position(|eintrag| matches!(eintrag, Some((kind, _)) if pid == 0 || *kind == pid))?;
        self.kinder_enden[platz].take()
    }

    /// Wie viele Kind-Ergebnisse liegen noch ungeholt herum? (Leck-Tests.)
    pub fn kinder_enden_offen(&self) -> usize {
        self.kinder_enden.iter().flatten().count()
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
///   mov  rax, 3                ; SYS_SCHREIBE
///   mov  edi, 2                ;   Handle 2 = DIAGNOSE (nur seriell!)
///   movabs rsi, text_va
///   mov  edx, laenge
///   int  0x80                  ; DER EINZIGE Kernel-Kontakt — und er gibt
///   jmp  schleife              ;   die CPU NICHT ab!
/// ```
///
/// Beachte, was hier NICHT steht: kein `yield`, kein `exit`, kein Warten.
/// Dieses Programm gibt die CPU unter keinen Umständen freiwillig her.
///
/// Und beachte das HANDLE: Ausgegeben wird auf 2 (Diagnose = nur seriell),
/// nicht auf 1 (Bildschirm). Tausende Zeilen ins Terminal-Fenster würden den
/// Compositor überschwemmen — dafür gibt es den getrennten Kanal.
pub fn zaehler_programm(text_va: u64, laenge: usize) -> Vec<u8> {
    use crate::syscall::{HANDLE_DIAGNOSE, SYS_SCHREIBE};
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
    p.extend_from_slice(&[0x48, 0xC7, 0xC0]); // mov rax, SYS_SCHREIBE
    p.extend_from_slice(&(SYS_SCHREIBE as u32).to_le_bytes());
    p.push(0xBF); // mov edi, HANDLE_DIAGNOSE
    p.extend_from_slice(&(HANDLE_DIAGNOSE as u32).to_le_bytes());
    p.extend_from_slice(&[0x48, 0xBE]); // movabs rsi, text_va
    p.extend_from_slice(&text_va.to_le_bytes());
    p.push(0xBA); // mov edx, laenge
    p.extend_from_slice(&(laenge as u32).to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
                                        // jmp schleife (rel8, rückwärts — der Rumpf ist < 128 Byte)
    let rel = schleife as i64 - (p.len() as i64 + 2);
    assert!(rel >= -128, "Zaehler-Programm zu lang fuer einen rel8-Sprung");
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
///   mov  rax, 4          ; SYS_SCHLAFE
///   mov  edi, ms
///   int  0x80
///   jmp  schleife
/// ```
pub fn schlaefer_programm(ms: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xC7, 0xC0]); // mov rax, SYS_SCHLAFE
    p.extend_from_slice(&(crate::syscall::SYS_SCHLAFE as u32).to_le_bytes());
    p.push(0xBF); // mov edi, imm32
    p.extend_from_slice(&ms.to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    let rel = 0i64 - (p.len() as i64 + 2);
    p.push(0xEB);
    p.push(rel as i8 as u8);
    p
}

// ---------------------------------------------------------------------------
// DER SYSCALL-PRÜFSTAND: ein Ring-3-Programm als Fernbedienung
// ---------------------------------------------------------------------------
//
// PROBLEM: Jeden einzelnen Syscall mit gültigen UND böswilligen Argumenten aus
// Ring 3 zu testen, würde ohne ELF-Loader bedeuten, für jeden Testfall ein
// eigenes Programm von Hand zu assemblieren. Das wäre unlesbar und fehleranfäl-
// lig — und der Test würde eher das Assemblieren prüfen als den Kernel.
//
// LÖSUNG: EIN winziges Programm, das als FERNBEDIENUNG dient. Es liest
// Syscall-Nummer und Argumente aus einer Struktur in seinem eigenen Speicher,
// löst `int 0x80` aus und legt Fehlercode und Ergebnis wieder dort ab. Der
// Kernel (bzw. der Test) schreibt Aufträge hinein und liest Antworten heraus —
// über `AdressRaum::schreiben/lesen`, also ohne den Adressraum zu aktivieren.
//
// Damit läuft JEDER Testfall als gewöhnlicher Rust-Code, während der Aufruf
// ECHT aus Ring 3 kommt: eigener Adressraum, eigene Handle-Tabelle, echte
// Zeigerprüfung. Genau die Kombination, die wir zum Testen der ABI brauchen.
//
// Zwischen zwei Aufträgen SCHLÄFT das Programm (`schlafe(1)`) statt zu pollen —
// dadurch bekommt der Kernel-Prozess die CPU sofort zurück, und die Tests
// laufen schnell.

/// Offset der Auftrags-Struktur in der Code-Seite des Prüfstands.
pub const PRUEFSTAND_AUFTRAG_OFFSET: u64 = 0x200;
/// Offset des Schreib-/Lese-Puffers, den Tests für Pfade und Daten benutzen.
pub const PRUEFSTAND_PUFFER_OFFSET: u64 = 0x400;
/// Nutzbare Größe dieses Puffers (bis zum Seitenende).
pub const PRUEFSTAND_PUFFER_GROESSE: u64 = 4096 - PRUEFSTAND_PUFFER_OFFSET;

// Feldoffsets in der Auftrags-Struktur (alles u64):
/// 1 = Auftrag liegt an, 0 = Ergebnis liegt an.
pub const PRUEFSTAND_FLAGGE: u64 = 0x00;
pub const PRUEFSTAND_NUMMER: u64 = 0x08;
pub const PRUEFSTAND_ARG0: u64 = 0x10;
pub const PRUEFSTAND_ARG1: u64 = 0x18;
pub const PRUEFSTAND_ARG2: u64 = 0x20;
pub const PRUEFSTAND_ARG3: u64 = 0x28;
pub const PRUEFSTAND_FEHLER: u64 = 0x30;
pub const PRUEFSTAND_ERGEBNIS: u64 = 0x38;

/// Baut das Prüfstand-Programm.
///
/// ```text
///   movabs r15, auftrag_va       ; Basis der Auftrags-Struktur (bleibt stehen:
/// warten:                        ;  unser Syscall-Rückweg stellt alle Register
///   mov  rax, [r15+0x00]         ;  wieder her, r15 überlebt also)
///   test rax, rax
///   jnz  los
///   mov  rax, 4                  ; SYS_SCHLAFE
///   mov  edi, 1                  ;   1 ms — CPU zurück an den Kernel
///   int  0x80
///   jmp  warten
/// los:
///   mov  rax, [r15+0x08]         ; Nummer
///   mov  rdi, [r15+0x10]         ; Argumente 0..3
///   mov  rsi, [r15+0x18]
///   mov  rdx, [r15+0x20]
///   mov  r10, [r15+0x28]
///   int  0x80
///   mov  [r15+0x30], rax         ; Fehlercode
///   mov  [r15+0x38], rdx         ; Ergebnis
///   mov  qword [r15+0x00], 0     ; „erledigt" — ZULETZT, damit der Test nie
///   jmp  warten                  ;   eine halb gefüllte Antwort sieht
/// ```
pub fn pruefstand_programm(auftrag_va: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x49, 0xBF]); // movabs r15, auftrag_va
    p.extend_from_slice(&auftrag_va.to_le_bytes());
    let warten = p.len();
    p.extend_from_slice(&[0x49, 0x8B, 0x47, PRUEFSTAND_FLAGGE as u8]); // mov rax,[r15+0]
    p.extend_from_slice(&[0x48, 0x85, 0xC0]); // test rax, rax
    let jnz_stelle = p.len();
    p.extend_from_slice(&[0x75, 0x00]); // jnz los (Ziel gleich einsetzen)
    p.extend_from_slice(&[0x48, 0xC7, 0xC0]); // mov rax, SYS_SCHLAFE
    p.extend_from_slice(&(crate::syscall::SYS_SCHLAFE as u32).to_le_bytes());
    p.extend_from_slice(&[0xBF, 0x01, 0x00, 0x00, 0x00]); // mov edi, 1
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    let rel_warten = warten as i64 - (p.len() as i64 + 2);
    p.push(0xEB);
    p.push(rel_warten as i8 as u8); // jmp warten
    let los = p.len();
    // Das jnz-Ziel nachtragen (rel8 ab dem Byte NACH dem Sprung).
    p[jnz_stelle + 1] = (los as i64 - (jnz_stelle as i64 + 2)) as i8 as u8;

    p.extend_from_slice(&[0x49, 0x8B, 0x47, PRUEFSTAND_NUMMER as u8]); // rax = Nummer
    p.extend_from_slice(&[0x49, 0x8B, 0x7F, PRUEFSTAND_ARG0 as u8]); // rdi
    p.extend_from_slice(&[0x49, 0x8B, 0x77, PRUEFSTAND_ARG1 as u8]); // rsi
    p.extend_from_slice(&[0x49, 0x8B, 0x57, PRUEFSTAND_ARG2 as u8]); // rdx
    p.extend_from_slice(&[0x4D, 0x8B, 0x57, PRUEFSTAND_ARG3 as u8]); // r10
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    p.extend_from_slice(&[0x49, 0x89, 0x47, PRUEFSTAND_FEHLER as u8]); // [..]=rax
    p.extend_from_slice(&[0x49, 0x89, 0x57, PRUEFSTAND_ERGEBNIS as u8]); // [..]=rdx
    p.extend_from_slice(&[0x49, 0xC7, 0x47, PRUEFSTAND_FLAGGE as u8, 0, 0, 0, 0]); // =0
    let rel_zurueck = warten as i64 - (p.len() as i64 + 2);
    assert!(rel_zurueck >= -128, "Pruefstand zu lang fuer rel8");
    p.push(0xEB);
    p.push(rel_zurueck as i8 as u8);
    assert!(
        (p.len() as u64) < PRUEFSTAND_AUFTRAG_OFFSET,
        "Pruefstand-Code laeuft in die Auftrags-Struktur"
    );
    p
}

/// Baut einen Prüfstand-Prozess: Adressraum, Programm, Stack.
pub fn pruefstand_prozess() -> Option<Prozess> {
    let auftrag_va = ZAEHLER_CODE_VA + PRUEFSTAND_AUFTRAG_OFFSET;
    let code = pruefstand_programm(auftrag_va);
    let (raum, einsprung, stack) = programm_aufsetzen(&code, None)?;
    Prozess::neu_ring3("Syscall-Pruefstand", raum, einsprung, stack)
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

// ===========================================================================
// ECHTE PROGRAMME VON DER PLATTE (Serie 6, Teil 5, Aufgabe 2)
//
// Alles oberhalb dieser Linie war ÜBUNG: hand-assemblierter Maschinencode,
// den der Kernel in seinem eigenen Image mitbringt. Ab hier lädt SpeedOS
// Programme, die es nicht kennt — aus einer Datei, in einen eigenen
// Adressraum, mit eigenen Argumenten.
//
// DAS SPEICHER-LAYOUT EINES ELF-PROZESSES (alles im privaten P4-Slot 1):
//
//   0x80_0000_0000  elf::IMAGE_START   .text   R-X  \
//                                      .rodata R--   |  vom Linker-Skript
//                                      .data   RW-   |  festgelegt
//                                      .bss    RW-  /
//   +16 MiB         elf::IMAGE_ENDE    ---- Obergrenze des Images ----
//
//                   ... 16 MiB ungemappte Lücke: ein durchgehender Fehler-
//                   greifer, der Programm und Stack sauber trennt ...
//
//   +32 MiB - 68 KiB  ELF_STACK_GUARD   GUARD-PAGE (nicht gemappt)
//   +32 MiB - 64 KiB  ELF_STACK_UNTEN   16 Seiten User-Stack, NX
//   +32 MiB           ELF_STACK_OBEN    Stack-Spitze; hier liegen argv-Daten
//
// Warum die Lücke so gross ist: Sie kostet nichts (ungemappte Adressen
// verbrauchen keinen Speicher) und macht jeden Zeiger-Ausrutscher zwischen
// Programm und Stack zu einem sofortigen Page Fault statt zu stiller
// Datenzerstörung.
// ===========================================================================

/// Obere Kante des User-Stacks eines ELF-Prozesses (32 MiB über dem Image-
/// Anfang — 16 MiB Image, 16 MiB ungemappte Lücke).
pub const ELF_STACK_OBEN: u64 = crate::elf::IMAGE_ENDE + 16 * 1024 * 1024;
/// Seiten des User-Stacks (64 KiB). Reichlich für Programme ohne Rekursion;
/// darunter liegt eine Guard-Page.
pub const ELF_STACK_SEITEN: usize = 16;

/// Höchstzahl von Argumenten (inklusive des Programmnamens als `argv[0]`).
pub const MAX_ARGUMENTE: usize = 16;
/// Höchstlänge EINES Arguments in Bytes.
pub const MAX_ARGUMENT_BYTES: usize = 255;
/// Höchstplatz, den alle argv-Daten zusammen auf dem Stack belegen dürfen.
/// Weit unter einer Stack-Seite — der Stack gehört dem Programm, nicht uns.
pub const MAX_ARGV_BYTES: usize = 2048;

/// Ein Argument, so wie der Prozess es sieht: (Zeiger, Länge). Diese
/// Struktur liegt IM USER-SPEICHER; `rsi` zeigt beim Start auf das erste
/// Element. 16 Byte, `repr(C)` — das ist ABI (docs/syscalls.md §9).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArgEintrag {
    pub zeiger: u64,
    pub laenge: u64,
}

/// Warum ein Programm nicht gestartet werden konnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFehler {
    /// Die Datei liess sich nicht lesen.
    Fs(crate::fs::FsFehler),
    /// Die Datei ist kein ladbares Programm.
    Elf(crate::elf::ElfFehler),
    /// Zu viele Argumente, ein zu langes Argument, oder alle zusammen zu gross.
    Argumente,
    /// Adressraum, Stack oder Kernel-Stack liessen sich nicht anlegen.
    KeinSpeicher,
    /// Die Prozess-Tabelle ist voll (`MAX_PROZESSE`).
    TabelleVoll,
}

impl StartFehler {
    /// Deutsche Meldung für Shell und Explorer.
    pub fn meldung(self) -> alloc::string::String {
        use alloc::string::ToString;
        match self {
            // `FsFehler` bringt seine eigene deutsche Meldung mit — die
            // nehmen wir, statt einen Rust-Typnamen vor einen Menschen zu
            // stellen.
            StartFehler::Fs(fehler) => fehler.meldung().to_string(),
            StartFehler::Elf(fehler) => fehler.meldung().to_string(),
            StartFehler::Argumente => {
                alloc::format!(
                    "zu viele/zu lange Argumente (max {} Stueck, {} Byte je Argument)",
                    MAX_ARGUMENTE,
                    MAX_ARGUMENT_BYTES
                )
            }
            StartFehler::KeinSpeicher => "kein Speicher fuer den Prozess".to_string(),
            StartFehler::TabelleVoll => {
                alloc::format!("Prozess-Tabelle voll (max {})", MAX_PROZESSE - 1)
            }
        }
    }
}

/// Legt die Argumente auf den User-Stack und liefert
/// `(rsp, argc, argv_zeiger)`.
///
/// AUFBAU, von der Stack-Spitze nach unten (Stacks wachsen nach unten, also
/// liegt das zuerst Angelegte am höchsten):
///
/// ```text
///   ELF_STACK_OBEN  ----------------------------
///                   "netzhole" "http://..."       die Bytes der Argumente
///                   (Ausrichtung auf 16)
///   argv_zeiger --> [ (ptr,len), (ptr,len), ... ] das Feld, auf das rsi zeigt
///                   (Ausrichtung auf 16)
///   rsp         --> ................ frei ......  hier arbeitet das Programm
/// ```
///
/// Alles wird über `raum.schreiben()` hineingelegt — also über das
/// Physik-Komplettmapping, ohne den Adressraum zu aktivieren. Der Prozess
/// findet seine Argumente schon vor, bevor er das erste Mal läuft.
fn argv_schreiben(
    raum: &mut AdressRaum,
    argumente: &[&str],
) -> Result<(VirtAddr, u64, u64), StartFehler> {
    if argumente.len() > MAX_ARGUMENTE {
        return Err(StartFehler::Argumente);
    }
    if argumente.iter().any(|a| a.len() > MAX_ARGUMENT_BYTES) {
        return Err(StartFehler::Argumente);
    }
    let string_bytes: usize = argumente.iter().map(|a| a.len()).sum();
    let tabellen_bytes = argumente.len() * core::mem::size_of::<ArgEintrag>();
    if string_bytes + tabellen_bytes + 32 > MAX_ARGV_BYTES {
        return Err(StartFehler::Argumente);
    }

    // Die Zeichenketten-Fläche, 16-ausgerichtet nach unten.
    let strings_basis = (ELF_STACK_OBEN - string_bytes as u64) & !0xF;
    // Darunter das Zeiger-Feld, ebenfalls 16-ausgerichtet.
    let tabellen_basis = (strings_basis - tabellen_bytes as u64) & !0xF;
    // Und darunter beginnt der freie Stack. Die 16 Byte Abstand sind reine
    // Vorsicht: Der erste `push` des Programms soll die Tabelle nicht
    // streifen, egal wie der Startcode ausrichtet.
    let rsp = (tabellen_basis - 16) & !0xF;

    let mut eintraege: Vec<ArgEintrag> = Vec::with_capacity(argumente.len());
    let mut schreibstelle = strings_basis;
    for argument in argumente {
        raum.schreiben(VirtAddr::new(schreibstelle), argument.as_bytes())
            .map_err(|_| StartFehler::KeinSpeicher)?;
        eintraege.push(ArgEintrag {
            zeiger: schreibstelle,
            laenge: argument.len() as u64,
        });
        schreibstelle += argument.len() as u64;
    }

    // Das Zeiger-Feld als rohe Bytes hinlegen. `ArgEintrag` ist `repr(C)` aus
    // zwei u64 — es gibt keine Padding-Löcher mit undefiniertem Inhalt.
    let mut tabellen_bytes_puffer = Vec::with_capacity(tabellen_bytes);
    for eintrag in &eintraege {
        tabellen_bytes_puffer.extend_from_slice(&eintrag.zeiger.to_le_bytes());
        tabellen_bytes_puffer.extend_from_slice(&eintrag.laenge.to_le_bytes());
    }
    if !tabellen_bytes_puffer.is_empty() {
        raum.schreiben(VirtAddr::new(tabellen_basis), &tabellen_bytes_puffer)
            .map_err(|_| StartFehler::KeinSpeicher)?;
    }

    Ok((
        VirtAddr::new(rsp),
        argumente.len() as u64,
        tabellen_basis,
    ))
}

/// Baut aus ELF-Bytes einen startbereiten Prozess — OHNE Dateisystem.
///
/// Getrennt von `prozess_starten`, damit die Tests ein Programm laden können,
/// ohne es erst auf eine Platte zu schreiben.
pub fn prozess_aus_elf(
    name: impl Into<String>,
    bytes: &[u8],
    argumente: &[&str],
) -> Result<Prozess, StartFehler> {
    let mut raum = AdressRaum::neu().map_err(|_| StartFehler::KeinSpeicher)?;

    // 1. Das Programm laden (prüft ALLES, bevor es die erste Seite mappt).
    let programm = crate::elf::laden(&mut raum, bytes).map_err(StartFehler::Elf)?;

    // 2. Der Stack — mit Guard-Page darunter, nicht ausführbar.
    raum.stack_anlegen(VirtAddr::new(ELF_STACK_OBEN), ELF_STACK_SEITEN)
        .map_err(|_| StartFehler::KeinSpeicher)?;

    // 3. Die Argumente auf den Stack.
    let (rsp, argc, argv) = argv_schreiben(&mut raum, argumente)?;

    // 4. Der Prozess-Kontrollblock mit von Hand geschriebenem Start-Rahmen.
    Prozess::neu_ring3_mit_start(
        name,
        raum,
        VirtAddr::new(programm.einsprung),
        rsp,
        argc,
        argv,
    )
    .ok_or(StartFehler::KeinSpeicher)
}

/// Lädt ein Programm aus dem VFS und plant es ein. Liefert seine PID.
///
/// DER WEG EINES PROGRAMMS, in einer Funktion:
/// Datei lesen -> ELF prüfen -> Adressraum bauen -> Segmente mappen und
/// füllen -> Stack anlegen -> Argumente hinlegen -> PCB bauen -> einplanen.
/// Ab dem nächsten Timer-Tick läuft es (Invariante 1 des Schedulers: ein
/// Prozess wird eingeplant, nie „gestartet").
///
/// LÄUFT AUS KERNEL-KONTEXT (Shell-Task, Explorer-Nachwirkung) — hier ist
/// `fs::mit_fs` erlaubt. Aus einem SYSCALL dürfte man das nicht so aufrufen
/// (dort gälte die Lock-Disziplin aus syscall/mod.rs); ein `spawn`-Syscall
/// müsste `syscall::mit_vfs` benutzen.
pub fn prozess_starten(pfad: &str, argumente: &[&str]) -> Result<Pid, StartFehler> {
    prozess_starten_mit(pfad, argumente, None, None, None, false)
}

/// Die volle Form: mit Elternteil, geerbten Standard-Handles und der Wahl
/// des VFS-Weges.
///
/// `aus_syscall` entscheidet, WIE die Datei gelesen wird — und das ist keine
/// Kosmetik, sondern die Lock-Disziplin aus syscall/mod.rs: Aus einem
/// Syscall (Interrupts aus) darf man auf `fs::mit_fs` NICHT warten, weil ein
/// verdrängter Kernel-Task den Lock halten könnte. Dort führt der Weg über
/// `syscall::mit_vfs` (try_lock + Wartefenster). Aus Kernel-Kontext (Shell,
/// Explorer) ist `fs::mit_fs` dagegen richtig und billiger.
pub fn prozess_starten_mit(
    pfad: &str,
    argumente: &[&str],
    eltern: Option<Pid>,
    erbe_eingabe: crate::syscall::handle::Erbe,
    erbe_ausgabe: crate::syscall::handle::Erbe,
    aus_syscall: bool,
) -> Result<Pid, StartFehler> {
    let bytes = if aus_syscall {
        let pfad_kopie = String::from(pfad);
        crate::syscall::mit_vfs(move |fs| fs.lesen(&pfad_kopie))
            // `mit_vfs` liefert schon einen ABI-Fehler; für den Start-Fehler
            // brauchen wir einen FsFehler zurück — „nicht gefunden" ist die
            // ehrlichste Näherung, und der genaue Code geht ohnehin gleich
            // wieder in die ABI (start_fehler_abbilden).
            .map_err(|_| StartFehler::Fs(crate::fs::FsFehler::NichtGefunden))?
    } else {
        crate::fs::mit_fs(|fs| fs.lesen(pfad)).map_err(StartFehler::Fs)?
    };
    // Der Name im Task-Manager ist der Dateiname, nicht der ganze Pfad.
    let name = pfad.rsplit('/').next().unwrap_or(pfad);

    // argv[0] ist per Konvention der Programmname.
    let standard_argumente = [name];
    let argumente = if argumente.is_empty() {
        &standard_argumente[..]
    } else {
        argumente
    };

    let mut prozess = prozess_aus_elf(name, &bytes, argumente)?;
    prozess.eltern = eltern;
    // Die geerbten Standard-Handles einsetzen, SOLANGE das Kind noch nicht
    // läuft — ein laufender Prozess kann seine Kanäle nicht mehr umbiegen.
    crate::syscall::handle::standard_erben(&mut prozess.handles, erbe_eingabe, erbe_ausgabe);
    let pid = prozess.pid;
    let seiten = prozess
        .raum
        .as_ref()
        .map(|r| r.frames_besitz())
        .unwrap_or(0);
    crate::scheduler::einplanen(prozess).ok_or(StartFehler::TabelleVoll)?;
    crate::serial_println!(
        "[elf] '{}' geladen: PID {}, {} Frames, {} Argument(e).",
        pfad,
        pid,
        seiten,
        argumente.len()
    );
    Ok(pid)
}

/// Ist diese Datei (nach ihren ersten Bytes) ein SpeedOS-Programm?
/// Liest nur den Kopf — für den Explorer, der beim Doppelklick entscheiden
/// muss, ob er SpeedText öffnet oder das Programm startet.
pub fn ist_programm(pfad: &str) -> bool {
    let mut kopf = [0u8; 20];
    match crate::fs::mit_fs(|fs| fs.read_at(pfad, 0, &mut kopf)) {
        Ok(gelesen) => crate::elf::sieht_ausfuehrbar_aus(&kopf[..gelesen]),
        Err(_) => false,
    }
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
        // ... und zwar SYS_SCHREIBE. Kein einziger ABGABE-Syscall (exit,
        // yield, schlafe) darf vorkommen — sonst wäre der Präemptions-Beweis
        // wertlos. Die Nummern stammen aus der ABI (docs/syscalls.md), damit
        // dieser Test bei einer ABI-Änderung mitwandert.
        let schreibe_muster = [
            0x48,
            0xC7,
            0xC0,
            crate::syscall::SYS_SCHREIBE as u8,
            0x00,
            0x00,
            0x00,
        ];
        assert!(
            code.windows(schreibe_muster.len()).any(|f| f == &schreibe_muster[..]),
            "Der Zaehler muss SYS_SCHREIBE benutzen"
        );
        for nummer in [
            crate::syscall::SYS_EXIT,
            crate::syscall::SYS_YIELD,
            crate::syscall::SYS_SCHLAFE,
        ] {
            let muster = [0x48, 0xC7, 0xC0, nummer as u8, 0x00, 0x00, 0x00];
            assert!(
                !code.windows(muster.len()).any(|f| f == &muster[..]),
                "Der Zaehler enthaelt einen Abgabe-Syscall (Nummer {})",
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
