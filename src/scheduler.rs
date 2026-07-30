// scheduler.rs — Der präemptive Round-Robin-Scheduler (Serie 6, Teil 3)
//
// Der Entwurf steht VOR diesem Code in docs/scheduler-design.md — dort ist
// begründet, WARUM es so aussieht. Hier die Kurzfassung der einen grossen
// Architektur-Entscheidung:
//
// ==========================================================================
// DER EXECUTOR IST SELBST EIN PROZESS (PID 0 — der "Kernel-Prozess").
//
// SpeedOS hat seit Serie 2 einen KOOPERATIVEN async-Executor mit einem
// Dutzend Kernel-Tasks (Compositor, netz_task, Shell-Sitzungen, Maus ...).
// Die sollen NICHT verschwinden: Es ist unser eigener Code, dem wir
// Kooperation zutrauen dürfen. Ring-3-Code dagegen kooperiert nicht — ihm
// muss die CPU WEGGENOMMEN werden.
//
// Statt zwei Welten mit zwei Wechsel-Mechanismen zu bauen, steht der Executor
// als ganz normaler Eintrag (Slot 0) in der Prozess-Tabelle und bekommt seine
// Zeitscheibe wie jeder User-Prozess. INNERHALB seiner Scheibe multiplext er
// weiter kooperativ.
//
//   praemptiv (PIT, 20 ms):  PID 0 -> PID 1 -> PID 2 -> PID 0 -> ...
//                              |
//                              +-- kooperativ (await): Compositor, Netz, Shell
//
// Drei Dinge fallen dadurch geschenkt an:
//   * EIN Wechsel-Mechanismus, keine Sonderfälle "Kernel vs. Prozess".
//   * DER LEERLAUF BLEIBT, WIE ER WAR: PID 0 mit leerer Task-Queue schläft
//     per hlt (Executor::sleep_if_idle) — er IST der Idle-Prozess. "Nichts
//     lauffähig" kann es nicht geben, denn PID 0 ist immer lauffähig.
//   * Die Oberfläche verhungert nicht: ein endlos rechnender User-Prozess
//     bekommt eine Scheibe von N+1, Compositor und Maus laufen weiter.
// ==========================================================================
//
// DER WECHSEL SELBST ist in prozess.rs erklärt (der gesicherte Kontext ist
// EINE Zahl: der RSP, an dem der Trap-Rahmen auf dem Kernel-Stack des
// Prozesses liegt). Hier unten stehen die drei Assembler-Einstiege und der
// EINE Ausstieg `schalte_auf_rahmen`.

use crate::adressraum;
use x86_64::VirtAddr;
use crate::prozess::{
    Pid, Prozess, ProzessEnde, ProzessMoment, TrapFrame, Warteauf, Zustand, KERNEL_PID,
    MAX_PROZESSE,
};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

/// Der Kernel-Prozess sitzt per Konstruktion in Slot 0.
pub const KERNEL_SLOT: usize = 0;

/// Länge der Zeitscheibe in PIT-Ticks. Der PIT läuft mit 250 Hz (4 ms pro
/// Tick), 5 Ticks sind also **20 ms** — fein genug, dass eine 33-FPS-
/// Oberfläche nicht ruckelt, und grob genug, dass der Wechsel-Aufwand
/// (ein CR3-Wechsel leert den TLB!) nicht ins Gewicht fällt.
pub const SCHEIBE_TICKS: u32 = 5;

/// Die Prozess-Tabelle. FESTES ARRAY, kein Vec: Der Timer-Interrupt liest sie,
/// und im Interrupt-Kontext darf nicht allokiert werden (Deadlock-Regel 2).
type Tabelle = [Option<Prozess>; MAX_PROZESSE];

static TABELLE: Mutex<Tabelle> = Mutex::new([None, None, None, None, None, None, None, None]);

/// Index des LAUFENDEN Prozesses. Lock-frei, weil der Page-Fault-Handler ihn
/// lesen muss, ohne den Tabellen-Lock zu nehmen.
static AKTUELL: AtomicUsize = AtomicUsize::new(KERNEL_SLOT);
/// Restliche Ticks der aktuellen Zeitscheibe.
static SCHEIBE_REST: AtomicU32 = AtomicU32::new(SCHEIBE_TICKS);
/// Plant der Timer überhaupt um? (false, solange scheduler::init() nicht lief —
/// dann verhält sich der Timer exakt wie vor Serie 6 Teil 3.)
static PLANUNG_AN: AtomicBool = AtomicBool::new(false);
/// Sperr-ZÄHLER: > 0 heisst "der Timer plant NICHT um". Genau eine Stelle
/// benutzt das — der alte Einzelschuss-Ring-3-Pfad (siehe `sperre_erhoehen`).
static SPERRE: AtomicU32 = AtomicU32::new(0);
/// Gesamtzahl der Kontext-Wechsel (Statistik).
static WECHSEL_GESAMT: AtomicU64 = AtomicU64::new(0);
/// CPU-Zeit, die NICHT dem Kernel-Prozess gehörte — der Executor rechnet sie
/// aus seiner hlt-Messung heraus, sonst würde gestohlene Zeit als "Ruhe"
/// gelten und die CPU-Anzeige lügen.
static FREMDZEIT_US: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// SOFORTIGES WECKEN (Serie 7, Teil 0 — der Weck-Latenz-Pass)
// ---------------------------------------------------------------------------
//
// ==========================================================================
// DIE ENTSCHEIDUNG: „LAUFFÄHIG MARKIEREN" **UND** EIN RESCHEDULE-PUNKT —
// ABER NIEMALS EINE DIREKTE ÜBERGABE.
//
// Drei Möglichkeiten standen zur Wahl:
//
//  (a) NUR MARKIEREN. Der Geweckte wird `Lauffaehig` und wartet, bis die
//      Zeitscheibe des Weckers abläuft. KLINGT sauber und LÖST DAS PROBLEM
//      NICHT: Genau das war der gemessene Fall. Der Leser (PID 0) hatte nach
//      dem Blockieren des Schreibers eine frische 20-ms-Scheibe; ob der
//      Schreiber nach 4 ms per Timer-Prüfung oder nach 0 ms per Anstoss
//      lauffähig wird, ändert nichts daran, dass er erst in 20 ms drankommt.
//      Aus 199 KiB/s wären ~240 KiB/s geworden.
//
//  (b) DIREKTE ÜBERGABE an den Geweckten (klassisches „handoff scheduling").
//      Am schnellsten — und die eine Variante, die WIRKLICH aushungert: Ein
//      Ping-Pong-Paar A<->B übergibt sich die CPU gegenseitig, und ein
//      dritter Prozess C kommt nie an die Reihe, weil er nie „der Geweckte"
//      ist. VERWORFEN.
//
//  (c) RESCHEDULE-PUNKT über die NORMALE Round-Robin-Wahl — gewählt.
//      Der Weckruf setzt nur ein Flag; am Rückweg des Syscalls (und in den
//      synchronen Warteschleifen des Kernels) wird daraufhin `wechsel_
//      entscheiden` mit `freiwillig = true` gefragt. Das Ziel ist damit
//      IMMER der nächste Lauffähige HINTER dem aktuellen, nie „der, den ich
//      gerade geweckt habe".
//
// WARUM (c) NICHT AUSHUNGERN KANN — das ist der Kern, und er steckt nicht in
// einer Bremse, sondern in der Struktur: `naechster_lauffaehig` sucht
// ZYKLISCH AB `aktuell + 1`. Im Ping-Pong A(Slot 1) <-> B(Slot 2) mit einem
// dritten Prozess C(Slot 3) ergibt das
//
//      A weckt B, gibt ab -> nächster hinter 1 = 2 = B
//      B weckt A, gibt ab -> nächster hinter 2 = 3 = **C**
//
// C kommt also schon in der zweiten Runde dran, ohne jedes Zutun. Der
// Fairness-Beweis von `test_fairness_ueber_n_runden` gilt unverändert
// weiter, denn es ist dieselbe Auswahlfunktion.
//
// WOGEGEN DENNOCH EINE BREMSE NÖTIG IST — nicht Verhungern, sondern
// VERSCHWENDUNG: Ein Paar, das sich einzelne Bytes zuwirft, würde bei jedem
// Byte einen Kontext-Wechsel (~450 ns, inklusive TLB-Leerung durch CR3)
// auslösen. Der Durchsatz bräche nicht ein, aber ein beliebiger Anteil der
// CPU ginge ins Umschalten. Deshalb ein BUDGET JE TIMER-TICK
// (`SOFORT_MAX_PRO_TICK`): Danach wirkt der Weckruf nur noch markierend, und
// der Geweckte wartet auf die normale Planung. Damit ist der Zusatzaufwand
// nach oben hart gedeckelt (Rechnung an der Konstanten), und es gibt keinen
// Eingabe-Datenstrom, mit dem ein Prozess den Scheduler beschäftigen kann.
//
// DER TIMER BLEIBT DAS SICHERHEITSNETZ. `warter_wecken` prüft weiterhin
// JEDEN Tick alle Weck-Bedingungen nach. Das sofortige Wecken ist die
// SCHNELLE SPUR, nicht die einzige — deshalb darf `wecken()` mit `try_lock`
// arbeiten und im Zweifel gar nichts tun, ohne dass ein Weckruf verloren
// geht. Genau das macht die Sache erst sicher: Ein Weckruf, der aussetzt,
// kostet 4 ms; ein Weckruf, der als EINZIGER Weg gedacht wäre, würde einen
// Prozess für immer schlafen lassen.
// ==========================================================================

/// Schaltet das sofortige Wecken samt Reschedule-Punkt ab/an. Der Standard
/// ist AN; ausgeschaltet verhält sich der Kernel exakt wie vor diesem Pass
/// (nur die Timer-Prüfung weckt) — das ist der ALT-Zweig der Messung.
static SOFORT_AN: AtomicBool = AtomicBool::new(true);
/// „Es wurde jemand geweckt — bei nächster Gelegenheit neu planen."
static UMPLANUNG: AtomicBool = AtomicBool::new(false);
/// Wie viele sofortige Umplanungen in DIESEM Tick schon stattfanden.
static SOFORT_BUDGET: AtomicU32 = AtomicU32::new(0);
/// Wie viele Prozesse insgesamt sofort geweckt wurden (Statistik).
static SOFORT_WECKUNGEN: AtomicU64 = AtomicU64::new(0);
/// Wie viele Kontext-Wechsel daraus entstanden (Statistik).
static SOFORT_WECHSEL: AtomicU64 = AtomicU64::new(0);
/// Wie oft das Budget gegriffen hat (Statistik — wird das je gross, redet
/// ein Programm in zu kleinen Häppchen).
static SOFORT_GEBREMST: AtomicU64 = AtomicU64::new(0);

/// FAIRNESS-BREMSE: höchstens so viele weck-getriebene Umplanungen je
/// Timer-Tick (4 ms).
///
/// DIE RECHNUNG, aus der die Zahl kommt: Ein Kontext-Wechsel kostet gemessen
/// ~450 ns. 16 Wechsel sind 7,2 µs — bei 4 ms Tick sind das **0,18 %** der
/// CPU, die ein noch so gesprächiges Prozess-Paar maximal ins Umschalten
/// stecken kann. Nach oben ist die Zahl also durch „vernachlässigbarer
/// Aufwand" begrenzt, nach unten dadurch, dass ein produktiver Datenstrom
/// sie nie erreichen soll: Eine 64-KiB-Pipe braucht EINEN Weckruf je
/// 64 KiB — 16 Weckrufe je 4 ms wären 256 MiB/s, mehr als der Ringpuffer
/// selbst schafft. Wer das Budget ausschöpft, tauscht also nicht Daten,
/// sondern schaltet um.
const SOFORT_MAX_PRO_TICK: u32 = 16;

// ---------------------------------------------------------------------------
// (A) DIE REINEN ENTSCHEIDUNGS-FUNKTIONEN
//
// Bewusst ohne Globals, ohne Hardware, ohne Locks — damit die eigentliche
// Scheduler-LOGIK als gewöhnlicher Unit-Test prüfbar ist (Fairness über N
// Runden, übersprungene Zustände, Zeitscheiben-Ablauf). Alles, was danach
// kommt, ist nur noch Ausführung.
// ---------------------------------------------------------------------------

/// Zeitscheiben-Buchhaltung: liefert `(neuer Rest, abgelaufen?)`.
/// Läuft sie ab, füllt sie sich sofort selbst wieder auf — der Aufrufer muss
/// nichts zurücksetzen.
pub fn scheibe_tick(rest: u32) -> (u32, bool) {
    if rest <= 1 {
        (SCHEIBE_TICKS, true)
    } else {
        (rest - 1, false)
    }
}

/// Ist der Weck-Zeitpunkt eines wartenden Prozesses erreicht?
/// (`wach_ab_ms == 0` heisst "kein Weckruf gesetzt" — der Prozess wartet auf
/// etwas anderes und bleibt liegen.)
pub fn weck_faellig(wach_ab_ms: u64, jetzt_ms: u64) -> bool {
    wach_ab_ms != 0 && jetzt_ms >= wach_ab_ms
}

/// Round-Robin: der nächste lauffähige Slot, zyklisch ab `aktuell + 1`.
///
/// Dass wir HINTER dem aktuellen anfangen, IST die Fairness: Jeder andere
/// lauffähige Prozess kommt dran, bevor der aktuelle ein zweites Mal
/// gewählt wird. Liefert `Some(aktuell)`, wenn er der einzige Lauffähige ist,
/// und `None`, wenn überhaupt keiner lauffähig ist.
pub fn naechster_lauffaehig(zustaende: &[Option<Zustand>], aktuell: usize) -> Option<usize> {
    let n = zustaende.len();
    if n == 0 {
        return None;
    }
    for schritt in 1..=n {
        let index = (aktuell + schritt) % n;
        if let Some(zustand) = zustaende[index] {
            if zustand.ist_lauffaehig() {
                return Some(index);
            }
        }
    }
    None
}

/// DIE Wechsel-Entscheidung — Herz des Schedulers, als reine Funktion.
///
/// Regeln, in dieser Reihenfolge:
///  1. Ist der AKTUELLE Prozess nicht mehr lauffähig (beendet, wartend, weg),
///     MUSS gewechselt werden — sofort, auch mitten in der Zeitscheibe.
///  2. Sonst nur bei ABGELAUFENER Zeitscheibe oder FREIWILLIGER Abgabe.
///  3. Ist der Nächste der Aktuelle selbst, wird nicht gewechselt (die
///     Zeitscheibe füllt sich neu). Das ist der Alltag: nur PID 0 lauffähig.
///
/// `None` = bleiben. Findet Regel 1 kein Ziel, ist das ebenfalls `None` — der
/// Aufrufer bleibt dann beim Kernel-Prozess, der per Konstruktion immer
/// lauffähig ist.
pub fn wechsel_entscheiden(
    zustaende: &[Option<Zustand>],
    aktuell: usize,
    scheibe_abgelaufen: bool,
    freiwillig: bool,
) -> Option<usize> {
    let aktuell_lauffaehig = zustaende
        .get(aktuell)
        .and_then(|z| *z)
        .map(|z| z.ist_lauffaehig())
        .unwrap_or(false);

    // Regel 1: Der Aktuelle kann nicht weiterlaufen -> Wechsel ist Pflicht.
    if !aktuell_lauffaehig {
        return naechster_lauffaehig(zustaende, aktuell).filter(|ziel| *ziel != aktuell);
    }
    // Regel 2: Ohne Anlass bleibt alles, wie es ist.
    if !scheibe_abgelaufen && !freiwillig {
        return None;
    }
    // Regel 3: Nur wechseln, wenn es einen ANDEREN Lauffähigen gibt.
    naechster_lauffaehig(zustaende, aktuell).filter(|ziel| *ziel != aktuell)
}

/// PASST EIN EREIGNIS AUF EINEN WARTEGRUND? Reine Funktion, damit die
/// Zuordnung „wer wird von was geweckt" ohne Hardware prüfbar ist.
///
/// Zwei Feinheiten stecken darin:
///  * `Kind(0)` heisst „irgendein Kind" und passt deshalb auf JEDES
///    Kind-Ereignis. Andersherum gilt das nicht: Wer auf Kind 7 wartet, wird
///    vom Ende von Kind 9 nicht geweckt.
///  * `Zeit` passt auf NICHTS. Ein Weckzeitpunkt ist keine Nachricht, die
///    jemand schickt — den prüft allein der Timer (`weck_faellig`). Stünde
///    hier `(Zeit, Zeit) => true`, könnte ein beliebiger Anstoss einen
///    Schläfer VOR seiner Zeit wecken.
pub fn weck_passt(wartet_auf: Warteauf, ereignis: Warteauf) -> bool {
    match (wartet_auf, ereignis) {
        (Warteauf::PipeLesen(a), Warteauf::PipeLesen(b)) => a == b,
        (Warteauf::PipeSchreiben(a), Warteauf::PipeSchreiben(b)) => a == b,
        (Warteauf::Kind(wartend), Warteauf::Kind(geendet)) => wartend == 0 || wartend == geendet,
        (Warteauf::Fenster(a), Warteauf::Fenster(b)) => a == b,
        _ => false,
    }
}

/// WECKT alle Prozesse, die auf `ereignis` warten — sofort, ohne auf den
/// Timer-Tick zu warten. Liefert, wie viele es waren.
///
/// Aufrufbar aus jedem Kontext (Kernel, Syscall), aber **nie unter einem
/// anderen Lock**, den der Timer-Pfad ebenfalls nimmt: Diese Funktion nimmt
/// TABELLE, und der Timer nimmt TABELLE und DANN die Blatt-Locks (PIPES).
/// Wer hier aus einem Blatt-Lock heraus aufruft, baut ein ABBA — deshalb
/// ermittelt `pipe.rs` seinen Weckruf unter dem Lock und löst ihn danach aus.
///
/// `try_lock`: Bekommen wir die Tabelle nicht, passiert schlicht nichts. Der
/// Weckruf ist dann nicht verloren, sondern nur langsam — der Timer prüft
/// die Bedingung im nächsten Tick ohnehin nach.
pub fn wecken(ereignis: Warteauf) -> usize {
    if !SOFORT_AN.load(Ordering::Relaxed) || !PLANUNG_AN.load(Ordering::Relaxed) {
        return 0;
    }
    let geweckt = {
        let mut tabelle = match TABELLE.try_lock() {
            Some(tabelle) => tabelle,
            None => return 0,
        };
        let mut anzahl = 0usize;
        for prozess in tabelle.iter_mut().flatten() {
            if prozess.zustand != Zustand::Wartend {
                continue;
            }
            let passt = matches!(prozess.wartet_auf, Some(grund) if weck_passt(grund, ereignis));
            if passt {
                prozess.wach_ab_ms = 0;
                prozess.wartet_auf = None;
                prozess.zustand = Zustand::Lauffaehig;
                anzahl += 1;
            }
        }
        anzahl
    };
    if geweckt > 0 {
        SOFORT_WECKUNGEN.fetch_add(geweckt as u64, Ordering::Relaxed);
        // NUR MERKEN, nicht hier umschalten: Der Aufrufer steckt womöglich
        // mitten in einer Kernel-Operation. Umgeplant wird an definierten
        // Punkten (Syscall-Rückweg, synchrone Warteschleife).
        UMPLANUNG.store(true, Ordering::Relaxed);
    }
    geweckt
}

/// Liegt eine Umplanungs-Anforderung an? (Für die synchronen Warteschleifen
/// des Kernels — `zeit::warte_auf_interrupt`.)
pub fn umplanung_offen() -> bool {
    SOFORT_AN.load(Ordering::Relaxed) && UMPLANUNG.load(Ordering::Relaxed)
}

/// Schaltet das sofortige Wecken um (Messung ALT/NEU) und liefert den alten
/// Zustand. AUS heisst: nur der Timer weckt, kein Reschedule-Punkt.
pub fn sofort_wecken_setzen(an: bool) -> bool {
    let vorher = SOFORT_AN.swap(an, Ordering::SeqCst);
    if !an {
        UMPLANUNG.store(false, Ordering::SeqCst);
    }
    vorher
}

/// Läuft das sofortige Wecken?
pub fn sofort_wecken_an() -> bool {
    SOFORT_AN.load(Ordering::Relaxed)
}

/// Statistik: `(sofort geweckte Prozesse, daraus entstandene Wechsel,
/// vom Fairness-Budget gebremste Umplanungen)`.
pub fn sofort_statistik() -> (u64, u64, u64) {
    (
        SOFORT_WECKUNGEN.load(Ordering::Relaxed),
        SOFORT_WECHSEL.load(Ordering::Relaxed),
        SOFORT_GEBREMST.load(Ordering::Relaxed),
    )
}

// ---------------------------------------------------------------------------
// (B) DIE AUSGABE-SPUR — Grundlage des Präemptions-Beweises
//
// Ein lock-freier Ring: Jede `debug_print`-Ausgabe eines Prozesses hinterlässt
// (PID, letztes druckbares Zeichen). Warum lock-frei und nicht einfach eine
// Log-Datei? Weil das aus einem SYSCALL heraus passiert, also im Kontext eines
// präemptiv geplanten Prozesses — dort ist jeder Lock, den der Kernel-Prozess
// halten könnte, ein Deadlock-Risiko (siehe docs/scheduler-design.md §8).
// ---------------------------------------------------------------------------

/// Länge der Spur (älteres wird nicht überschrieben, sondern verworfen —
/// wir wollen den ANFANG des Beweises sehen, nicht das Ende).
pub const SPUR_LAENGE: usize = 256;

static SPUR: [AtomicU32; SPUR_LAENGE] = [const { AtomicU32::new(0) }; SPUR_LAENGE];
static SPUR_ANZAHL: AtomicUsize = AtomicUsize::new(0);

/// Vermerkt eine Prozess-Ausgabe in der Spur (aus dem Syscall, lock-frei).
pub fn spur_melden(pid: Pid, zeichen: u8) {
    let index = SPUR_ANZAHL.fetch_add(1, Ordering::Relaxed);
    if index < SPUR_LAENGE {
        SPUR[index].store(pid | ((zeichen as u32) << 16), Ordering::Relaxed);
    }
}

/// Leert die Spur (vor einem Messlauf).
pub fn spur_loeschen() {
    SPUR_ANZAHL.store(0, Ordering::Relaxed);
}

/// Die Spur als `(PID, Zeichen)`-Liste.
pub fn spur_lesen() -> Vec<(Pid, u8)> {
    let anzahl = SPUR_ANZAHL.load(Ordering::Relaxed).min(SPUR_LAENGE);
    (0..anzahl)
        .map(|i| {
            let wert = SPUR[i].load(Ordering::Relaxed);
            ((wert & 0xffff) as Pid, (wert >> 16) as u8)
        })
        .collect()
}

/// Das Ergebnis der Spur-Auswertung — die Zahlen, an denen der
/// Präemptions-Beweis hängt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpurBefund {
    /// Ausgaben insgesamt.
    pub gesamt: usize,
    /// Wie oft folgte auf die Ausgabe eines Prozesses die eines ANDEREN?
    pub wechsel: usize,
    /// Wie viele verschiedene Prozesse haben ausgegeben?
    pub beteiligte: usize,
}

/// Wertet eine Ausgabe-Spur aus (reine Funktion). `wechsel >= 2` bei zwei
/// beteiligten Prozessen heisst: die Ausgabe ist wirklich VERSCHRÄNKT und
/// nicht nur "erst alle von A, dann alle von B".
pub fn spur_auswerten(spur: &[Pid]) -> SpurBefund {
    // Beteiligte zählen ohne Allokation. WICHTIG: Eine PID ist NICHT der
    // Tabellen-Index — PIDs wachsen monoton weiter (PID 13 in Slot 4). Also
    // wird die Menge der gesehenen PIDs gesammelt, nicht ein Flag pro Slot
    // indiziert. (Genau dieser Fehler hat den Beweis anfangs verschluckt:
    // Bei PID 12/13 war "beteiligte" 0, obwohl beide gedruckt hatten.)
    let mut gesehen: [Pid; MAX_PROZESSE] = [0; MAX_PROZESSE];
    let mut anzahl = 0usize;
    for &pid in spur {
        if anzahl < MAX_PROZESSE && !gesehen[..anzahl].contains(&pid) {
            gesehen[anzahl] = pid;
            anzahl += 1;
        }
    }
    SpurBefund {
        gesamt: spur.len(),
        wechsel: spur.windows(2).filter(|paar| paar[0] != paar[1]).count(),
        beteiligte: anzahl,
    }
}

// ---------------------------------------------------------------------------
// (C) DIE ASSEMBLER-EIN- UND AUSSTIEGE
// ---------------------------------------------------------------------------

// DREI EINSTIEGE, EIN AUSSTIEG. Jeder Einstieg sichert den vollen Register-
// satz auf dem Kernel-Stack des unterbrochenen Prozesses, ruft seinen
// Rust-Dispatcher mit einem Zeiger darauf und bekommt einen (möglicherweise
// ANDEREN) Rahmen zurück. `schalte_auf_rahmen` lädt ihn — und das ist der
// gesamte Kontext-Wechsel.
//
// Die Push-Reihenfolge (rax zuerst, r15 zuletzt) IST der Vertrag mit
// `prozess::TrapFrame`; `test_trapframe_layout` nagelt ihn fest.
core::arch::global_asm!(
    // ----- DER AUSSTIEG: ein gesicherter Kontext wird wieder die CPU -----
    ".global schalte_auf_rahmen",
    "schalte_auf_rahmen:",
    "mov rsp, rdi",              // <-- HIER passiert der Prozess-Wechsel:
                                 //     der Stack (und damit der Rahmen) eines
                                 //     ANDEREN Prozesses wird der unsere
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp", "pop rdi", "pop rsi",
    "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",                     // lädt RIP/CS/RFLAGS/RSP/SS — auch nach Ring 3

    // ----- EINSTIEG 1: der PIT-Timer (Präemption) -----
    ".global timer_entry",
    "timer_entry:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",              // Argument: Zeiger auf den TrapFrame
    "call timer_dispatch",       // -> rax = Rahmen, mit dem es weitergeht
    "mov rdi, rax",
    "jmp schalte_auf_rahmen",

    // (EINSTIEG 2 ist `syscall_entry` in ring3.rs — INT 0x80. Er sieht genauso
    //  aus und endet im selben `schalte_auf_rahmen`.)

    // ----- EINSTIEG 3: der Sterbe-Stub (Fault aus Ring 3) -----
    // Wird NICHT von der CPU angesprungen, sondern vom Page-Fault-/#GP-
    // Handler: Der biegt seinen Interrupt-Rahmen hierher um (Ring 0, Kernel-
    // Stack des sterbenden Prozesses). Es gibt hier keinen zu sichernden
    // Kontext — der Prozess ist tot.
    ".global prozess_sterben",
    "prozess_sterben:",
    "call scheduler_sterben",    // -> rax = Rahmen des NÄCHSTEN Prozesses
    "mov rdi, rax",
    "jmp schalte_auf_rahmen",
);

extern "C" {
    fn timer_entry();
    fn prozess_sterben();
}

/// Adresse des Timer-Einstiegs (für die IDT-Registrierung in interrupts.rs).
pub fn timer_handler_adresse() -> u64 {
    timer_entry as *const () as u64
}

/// Adresse des Sterbe-Stubs (der Page-Fault-Handler biegt dorthin um).
fn sterbe_stub_adresse() -> u64 {
    prozess_sterben as *const () as u64
}

// ---------------------------------------------------------------------------
// (D) DER WECHSEL — die vier Schritte, von denen der vierte gern vergessen wird
// ---------------------------------------------------------------------------

/// Warum wird gewechselt? Entscheidet über die Beweis-Zähler und darüber,
/// ob der alte Zustand angefasst wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grund {
    /// Die Zeitscheibe ist abgelaufen — die CPU wird WEGGENOMMEN.
    Verdraengt,
    /// Der Prozess hat selbst abgegeben (yield/schlafen/exit).
    Freiwillig,
    /// Ein WECKRUF hat umgeplant. Technisch wie `Freiwillig` (der Kontext
    /// wird ganz normal gesichert), aber es zählt NICHT als Abgabe des
    /// Prozesses: Er hat nicht abgegeben, ihm wurde im Interesse eines
    /// Geweckten dazwischengefunkt. Würde das als `abgaben` gebucht, wären
    /// die Präemptions-Beweise aus Teil 3 („0 Abgaben") plötzlich falsch,
    /// ohne dass sich am Prozess etwas geändert hätte.
    Umplanung,
    /// Der Prozess ist gestorben (Fault) — kein Kontext zu sichern.
    Tod,
}

/// Ein Zustands-Abbild der Tabelle für die reinen Entscheidungs-Funktionen.
/// Allokationsfrei (festes Array) — läuft im Interrupt.
fn zustands_bild(tabelle: &Tabelle) -> [Option<Zustand>; MAX_PROZESSE] {
    let mut bild = [None; MAX_PROZESSE];
    for (slot, eintrag) in tabelle.iter().enumerate() {
        bild[slot] = eintrag.as_ref().map(|p| p.zustand);
    }
    bild
}

/// Weckt alle wartenden Prozesse, deren Bedingung jetzt erfüllt ist.
///
/// SEIT DEM SOFORTIGEN WECKEN IST DAS DAS SICHERHEITSNETZ, nicht mehr der
/// Hauptweg: Im Normalfall hat `wecken()` den Prozess längst lauffähig
/// gemacht, bevor dieser Tick kommt. Diese Schleife bleibt trotzdem, und
/// zwar vollständig — denn `wecken()` darf aussetzen (`try_lock`), und die
/// Zeit-Bedingung (`schlafe(ms)`) hat gar keinen Anstosser. Ein Weckruf, der
/// hier durchfällt, kostet 4 ms; einer, der nirgends nachgeprüft würde,
/// wäre ein Prozess, der nie wieder aufwacht.
///
/// Läuft bei JEDEM Timer-Tick (4 ms) und ist deshalb streng an die
/// Interrupt-Handler-Regel gebunden: keine Allokation, kein blockierendes
/// Warten auf einen Lock. Die Pipe-Abfragen benutzen `try_lock` und melden
/// bei belegtem Lock schlicht „noch nicht" — der nächste Tick fragt erneut.
///
/// Die KIND-Bedingung braucht dabei keinen fremden Lock: Das Ergebnis eines
/// beendeten Kindes liegt schon IM WARTENDEN PROZESS selbst
/// (`kinder_enden`, siehe `prozess::Prozess`). Genau deshalb ist es dort
/// abgelegt und nicht im Kind — sonst müsste hier die halbe Tabelle
/// durchsucht werden, während wir sie schon veränderlich in der Hand halten.
fn warter_wecken(tabelle: &mut Tabelle, jetzt_ms: u64) {
    for prozess in tabelle.iter_mut().flatten() {
        if prozess.zustand != Zustand::Wartend {
            continue;
        }
        let bereit = match prozess.wartet_auf {
            Some(Warteauf::Zeit) => weck_faellig(prozess.wach_ab_ms, jetzt_ms),
            Some(Warteauf::Kind(pid)) => prozess.kind_ende_bereit(pid),
            Some(Warteauf::PipeLesen(id)) => crate::pipe::lesbar(id),
            Some(Warteauf::PipeSchreiben(id)) => crate::pipe::schreibbar(id),
            // DAS SICHERHEITSNETZ FÜR FENSTER-EREIGNISSE (Serie 8): Der
            // schnelle Weg ist `wecken` aus dem Eingabe-Pfad; hier wird
            // nachgesehen, falls der aussetzen musste. `try_lock` — wir
            // halten die Tabelle und dürfen auf den MANAGER nicht warten;
            // „konnte nicht nachsehen" heisst dann schlicht „nächster
            // Tick nochmal". Zusätzlich die FRIST: `wach_ab_ms` ist bei
            // diesem Grund gesetzt, wenn der Aufrufer eine mitgegeben hat.
            Some(Warteauf::Fenster(id)) => {
                use crate::fenster::FensterLage;
                match crate::fenster::prozess_fenster_lage(id) {
                    FensterLage::Ereignis => true,
                    // Fenster weg (geschlossen, Prozess-Ende): Der Warter
                    // MUSS geweckt werden, sonst wartet er auf ein
                    // Ereignis, das nie kommt. Sein Syscall läuft neu und
                    // meldet dann ein ungültiges Handle.
                    FensterLage::Weg => true,
                    FensterLage::Leer | FensterLage::Unbekannt => {
                        weck_faellig(prozess.wach_ab_ms, jetzt_ms)
                    }
                }
            }
            // Kein Grund vermerkt: Der alte Weg über `wach_ab_ms` allein
            // (warten_setzen von aussen) — bleibt liegen, bis ihn jemand
            // ausdrücklich weckt.
            None => weck_faellig(prozess.wach_ab_ms, jetzt_ms),
        };
        if bereit {
            prozess.wach_ab_ms = 0;
            prozess.wartet_auf = None;
            prozess.zustand = Zustand::Lauffaehig;
        }
    }
}

/// DIE EINE STELLE, an der ein Prozess beendet wird.
///
/// Vorher stand „zustand = Beendet; ende = ..." an drei Stellen (exit,
/// Absturz, Beenden von aussen), und mit der Eltern-Kind-Beziehung kam ein
/// vierter Schritt dazu: das Ergebnis beim Elternteil abzulegen. Drei Kopien
/// davon wären drei Gelegenheiten, genau das zu vergessen — deshalb hier
/// einmal, und alle drei rufen es.
///
/// Der Aufrufer hält den Tabellen-Lock bereits.
fn ende_vermerken(tabelle: &mut Tabelle, slot: usize, ende: ProzessEnde) {
    let (pid, eltern) = match tabelle[slot].as_mut() {
        Some(prozess) => {
            // Schon beendet? Dann NICHT überschreiben — der erste Grund
            // zählt (ein abgestürzter Prozess, der danach noch gestoppt
            // wird, ist abgestürzt und nicht gestoppt).
            if prozess.zustand == Zustand::Beendet {
                return;
            }
            prozess.zustand = Zustand::Beendet;
            prozess.wartet_auf = None;
            prozess.ende = Some(ende);
            (prozess.pid, prozess.eltern)
        }
        None => return,
    };

    // Das Ergebnis beim Elternteil hinterlegen — falls es ihn noch gibt.
    // Gibt es ihn nicht mehr, verfällt es hier und jetzt: Kein Eintrag
    // bleibt zurück, den irgendwer später aufräumen müsste.
    if let Some(eltern_pid) = eltern {
        if let Some(elternteil) = tabelle
            .iter_mut()
            .flatten()
            .find(|kandidat| kandidat.pid == eltern_pid && kandidat.zustand != Zustand::Beendet)
        {
            elternteil.kind_ende_ablegen(pid, ende);
            // SOFORT WECKEN, wenn der Elternteil genau darauf gewartet hat.
            // Nicht über `wecken()`: Wir HALTEN die Tabelle schon (der
            // Aufrufer hat sie), ein zweiter Zugriff wäre ein Selbst-Deadlock.
            // Das Ereignis liegt hier ohnehin direkt vor uns.
            let wartet = elternteil.zustand == Zustand::Wartend
                && matches!(elternteil.wartet_auf,
                    Some(grund) if weck_passt(grund, Warteauf::Kind(pid)));
            if wartet && SOFORT_AN.load(Ordering::Relaxed) {
                elternteil.wach_ab_ms = 0;
                elternteil.wartet_auf = None;
                elternteil.zustand = Zustand::Lauffaehig;
                SOFORT_WECKUNGEN.fetch_add(1, Ordering::Relaxed);
                UMPLANUNG.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Führt den Kontext-Wechsel von `von` nach `nach` durch und liefert den
/// Rahmen, mit dem es weitergeht. Liefert 0, wenn das Ziel keinen gültigen
/// gesicherten Kontext hat (dann bleibt der Aufrufer, wo er ist).
///
/// Hier stehen DIE VIER SCHRITTE eines Prozess-Wechsels beieinander:
///   1. Registersatz des Alten festhalten (nur den RSP — der Rahmen liegt
///      schon auf seinem Kernel-Stack).
///   2. CPU-Zeit abrechnen.
///   3. CR3 auf den Adressraum des Neuen umlegen.
///   4. TSS.RSP0 auf den Kernel-Stack des Neuen setzen.
///
/// Schritt 4 ist der, den man vergisst — und dann überschreibt der nächste
/// Trap aus Ring 3 den gesicherten Kontext eines FREMDEN Prozesses.
fn wechsel_ausfuehren(
    tabelle: &mut Tabelle,
    von: usize,
    nach: usize,
    kontext_alt: u64,
    grund: Grund,
    alt_war_ring3: bool,
) -> u64 {
    // Zuerst prüfen, ob das Ziel überhaupt fortsetzbar ist — VOR jeder
    // Änderung, damit ein Fehlschlag nichts halb Umgeschaltetes hinterlässt.
    let ziel_kontext = tabelle[nach].as_ref().map(|p| p.kontext).unwrap_or(0);
    if ziel_kontext == 0 {
        return 0;
    }
    let jetzt_us = crate::zeit::us_seit_boot();

    // --- Schritt 1 + 2: den Alten einfrieren ---
    if let Some(alt) = tabelle[von].as_mut() {
        let lief_us = jetzt_us.saturating_sub(alt.scheibe_start_us);
        alt.cpu_us += lief_us;
        if von != KERNEL_SLOT {
            FREMDZEIT_US.fetch_add(lief_us, Ordering::Relaxed);
        }
        match grund {
            Grund::Tod => {
                // Kein Kontext zu sichern — er wird nie wieder gebraucht.
                alt.kontext = 0;
            }
            Grund::Verdraengt => {
                alt.kontext = kontext_alt;
                // DER BEWEIS-ZÄHLER: Nur wenn der gesicherte CS auf Ring 3
                // zeigt, wurde einem UNPRIVILEGIERTEN Programm die CPU
                // weggenommen. Ein verdrängter Kernel-Prozess zählt nicht.
                if alt_war_ring3 {
                    alt.praemptionen += 1;
                }
                if alt.zustand == Zustand::Laeuft {
                    alt.zustand = Zustand::Lauffaehig;
                }
            }
            Grund::Freiwillig => {
                alt.kontext = kontext_alt;
                alt.abgaben += 1;
                if alt.zustand == Zustand::Laeuft {
                    alt.zustand = Zustand::Lauffaehig;
                }
            }
            Grund::Umplanung => {
                alt.kontext = kontext_alt;
                // KEIN `abgaben += 1` — siehe die Begründung am Enum.
                if alt.zustand == Zustand::Laeuft {
                    alt.zustand = Zustand::Lauffaehig;
                }
            }
        }
    }

    // --- Schritt 3 + 4: den Neuen aufsetzen ---
    if let Some(neu) = tabelle[nach].as_mut() {
        neu.zustand = Zustand::Laeuft;
        neu.scheibe_start_us = jetzt_us;
        neu.kontext = 0; // läuft jetzt — der gesicherte Rahmen ist verbraucht
        match neu.raum.as_mut() {
            // Eigener Adressraum (aktivieren frischt den Kernel-Spiegel auf).
            Some(raum) => raum.aktivieren(),
            // Kernel-Prozess: zurück auf die Kernel-P4.
            None => adressraum::kernel_aktivieren(),
        }
        crate::gdt::rsp0_setzen(neu.kern_stack_oben());
    }

    AKTUELL.store(nach, Ordering::Relaxed);
    SCHEIBE_REST.store(SCHEIBE_TICKS, Ordering::Relaxed);
    WECHSEL_GESAMT.fetch_add(1, Ordering::Relaxed);
    ziel_kontext
}

// ---------------------------------------------------------------------------
// (E) DIE DISPATCHER (aus dem Assembler gerufen)
// ---------------------------------------------------------------------------

/// Der Timer-Dispatcher: Basisarbeit (Tick-Zähler, Weckrufe, EOI) und dann die
/// Planung. Liefert den Rahmen, mit dem es weitergeht.
///
/// WICHTIG, was hier NICHT passiert: keine Allokation, kein blockierendes
/// Warten auf einen Lock (`try_lock`!), keine Ausgabe. Das ist die
/// Interrupt-Handler-Regel dieses Projekts, und sie gilt auch für den
/// Scheduler — bekommt er die Tabelle nicht, findet in diesem Tick eben kein
/// Wechsel statt. Der nächste kommt in 4 ms.
#[no_mangle]
extern "C" fn timer_dispatch(rahmen: *mut TrapFrame) -> *mut TrapFrame {
    // Exakt das, was der alte timer_interrupt_handler getan hat:
    crate::interrupts::timer_basisarbeit();

    // Das Fairness-Budget für weck-getriebene Umplanungen füllt sich JE TICK
    // wieder auf. Genau dadurch ist der Zusatzaufwand eine Rate (Wechsel je
    // 4 ms) und keine Gesamtzahl, die irgendwann erschöpft wäre.
    SOFORT_BUDGET.store(0, Ordering::Relaxed);

    if !PLANUNG_AN.load(Ordering::Relaxed) || SPERRE.load(Ordering::Relaxed) > 0 {
        return rahmen;
    }

    let (rest, abgelaufen) = scheibe_tick(SCHEIBE_REST.load(Ordering::Relaxed));
    SCHEIBE_REST.store(rest, Ordering::Relaxed);

    let mut tabelle = match TABELLE.try_lock() {
        Some(tabelle) => tabelle,
        None => return rahmen,
    };
    warter_wecken(&mut tabelle, crate::zeit::ms_seit_boot());

    let aktuell = AKTUELL.load(Ordering::Relaxed);
    let bild = zustands_bild(&tabelle);
    let ziel = match wechsel_entscheiden(&bild, aktuell, abgelaufen, false) {
        Some(ziel) => ziel,
        None => return rahmen,
    };
    // unsafe: `rahmen` zeigt auf den eben gesicherten Registersatz auf dem
    // Kernel-Stack des unterbrochenen Prozesses — gültig und ausgerichtet.
    let war_ring3 = unsafe { (*rahmen).aus_ring3() };
    let neuer = wechsel_ausfuehren(
        &mut tabelle,
        aktuell,
        ziel,
        rahmen as u64,
        Grund::Verdraengt,
        war_ring3,
    );
    if neuer == 0 {
        rahmen
    } else {
        neuer as *mut TrapFrame
    }
}

/// Der Sterbe-Dispatcher: Läuft in Ring 0 auf dem Kernel-Stack des gerade
/// gestorbenen Prozesses (der Fault-Handler hat hierher umgebogen) und liefert
/// den Rahmen des nächsten lauffähigen Prozesses.
///
/// Der Tabellen-Lock wird hier BLOCKIEREND genommen, und das ist begründet:
/// Aus Kernel-Kontext wird er nur mit ausgeschalteten Interrupts gehalten —
/// während einer solchen Sperre kann gar kein Ring-3-Code laufen, also auch
/// kein Ring-3-Fault auftreten. Wir können nicht auf uns selbst warten.
#[no_mangle]
extern "C" fn scheduler_sterben() -> *mut TrapFrame {
    let mut tabelle = TABELLE.lock();
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    let bild = zustands_bild(&tabelle);
    // Der Aktuelle ist Beendet, wird von naechster_lauffaehig also
    // übersprungen. PID 0 ist immer lauffähig -> es gibt immer ein Ziel.
    let ziel = naechster_lauffaehig(&bild, aktuell).unwrap_or(KERNEL_SLOT);
    let neuer = wechsel_ausfuehren(&mut tabelle, aktuell, ziel, 0, Grund::Tod, false);
    // Ab hier gibt es kein Zurück: Der Stack, auf dem wir stehen, gehört einem
    // toten Prozess. Ohne gültiges Ziel ist das ein echter Kernel-Bug
    // (Invariante 1 verletzt), und dann ist Anhalten die ehrliche Reaktion.
    assert!(
        neuer != 0,
        "Scheduler: kein fortsetzbarer Prozess beim Sterben (PID-0-Kontext fehlt)"
    );
    neuer as *mut TrapFrame
}

// ---------------------------------------------------------------------------
// (F) SYSCALL-SEITE (aus ring3::syscall_dispatch gerufen)
// ---------------------------------------------------------------------------

/// Gemeinsamer Weg aller FREIWILLIGEN Abgaben (yield, schlafen, exit).
/// Liefert den Rahmen, mit dem es weitergeht (evtl. derselbe).
fn freiwillig_wechseln(rahmen: *mut TrapFrame) -> *mut TrapFrame {
    wechseln_mit_grund(rahmen, Grund::Freiwillig)
}

/// Wie `freiwillig_wechseln`, aber mit ausdrücklichem Grund — der
/// Reschedule-Punkt benutzt `Grund::Umplanung`, damit er nicht als Abgabe
/// des verdrängten Prozesses gebucht wird.
fn wechseln_mit_grund(rahmen: *mut TrapFrame, grund: Grund) -> *mut TrapFrame {
    if !PLANUNG_AN.load(Ordering::Relaxed) || SPERRE.load(Ordering::Relaxed) > 0 {
        return rahmen;
    }
    let mut tabelle = match TABELLE.try_lock() {
        Some(tabelle) => tabelle,
        None => return rahmen,
    };
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    let bild = zustands_bild(&tabelle);
    // `freiwillig = true`: DIESELBE Round-Robin-Wahl wie bei yield — nie eine
    // direkte Übergabe an den Geweckten (siehe die Entscheidung oben).
    let ziel = match wechsel_entscheiden(&bild, aktuell, false, true) {
        Some(ziel) => ziel,
        None => return rahmen,
    };
    let neuer = wechsel_ausfuehren(&mut tabelle, aktuell, ziel, rahmen as u64, grund, false);
    if neuer == 0 {
        rahmen
    } else {
        neuer as *mut TrapFrame
    }
}

/// DER RESCHEDULE-PUNKT am Rückweg jedes Syscalls.
///
/// Hat der gerade gelaufene Syscall jemanden geweckt (z. B. `schreibe` in
/// eine leere Pipe, auf der ein anderer Prozess schlief), bekommt der
/// Scheduler hier die Gelegenheit, sofort umzuplanen — statt den Geweckten
/// bis zum Ablauf der laufenden Zeitscheibe warten zu lassen.
///
/// WARUM GENAU HIER: Das ist der einzige Punkt im Syscall-Pfad, an dem
/// GARANTIERT kein Kernel-Lock gehalten wird und der gesicherte Kontext
/// vollständig ist (Ergebnis steht schon in rax/rdx). Ein Wechsel mitten im
/// Syscall wäre beides nicht.
///
/// Der Aufrufer übergibt den Rahmen, den der Syscall ohnehin zurückgeben
/// würde — hat der Syscall selbst schon umgeschaltet (exit/yield/blockieren),
/// wird diese Funktion gar nicht erst gerufen.
pub fn umplanen_am_syscall_ende(rahmen: *mut TrapFrame) -> *mut TrapFrame {
    // `swap`: Die Anforderung gilt genau einmal. Wer sie sich holt, plant um.
    if !SOFORT_AN.load(Ordering::Relaxed) || !UMPLANUNG.swap(false, Ordering::Relaxed) {
        return rahmen;
    }
    // FAIRNESS-BREMSE: Ist das Budget dieses Ticks aufgebraucht, wirkt der
    // Weckruf nur markierend — der Geweckte ist lauffähig und kommt über die
    // normale Planung dran (spätestens beim Ablauf der Zeitscheibe).
    if SOFORT_BUDGET.fetch_add(1, Ordering::Relaxed) >= SOFORT_MAX_PRO_TICK {
        SOFORT_GEBREMST.fetch_add(1, Ordering::Relaxed);
        return rahmen;
    }
    let neuer = wechseln_mit_grund(rahmen, Grund::Umplanung);
    if neuer != rahmen {
        SOFORT_WECHSEL.fetch_add(1, Ordering::Relaxed);
    }
    neuer
}

/// Der Reschedule-Punkt für KERNEL-Kontext (synchrone Warteschleifen).
///
/// Ein Kernel-Task kann nicht einfach seinen Trap-Rahmen umbiegen — er hat
/// keinen. Also gibt er stattdessen die Zeitscheibe per `int 0x80` ab; der
/// Weg durch den Syscall-Dispatcher ist derselbe wie bei einem Ring-3-yield.
/// Liefert `true`, wenn abgegeben wurde.
///
/// DIE FAIRNESS-BREMSE GILT AUCH HIER — sonst könnte ein Kernel-Task, der
/// eine Pipe leert, dasselbe Umschalt-Karussell auslösen.
pub fn umplanen_im_kernel() -> bool {
    if !SOFORT_AN.load(Ordering::Relaxed) || !UMPLANUNG.swap(false, Ordering::Relaxed) {
        return false;
    }
    if SOFORT_BUDGET.fetch_add(1, Ordering::Relaxed) >= SOFORT_MAX_PRO_TICK {
        SOFORT_GEBREMST.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    SOFORT_WECHSEL.fetch_add(1, Ordering::Relaxed);
    abgeben();
    true
}

/// Syscall `yield`: Zeitscheibe sofort abgeben.
pub fn syscall_abgeben(rahmen: *mut TrapFrame) -> *mut TrapFrame {
    freiwillig_wechseln(rahmen)
}

/// Syscall `exit(code)`: den aufrufenden Prozess beenden und weiterschalten.
/// Liefert `None`, wenn gar kein eingeplanter User-Prozess läuft (dann ist
/// der alte Einzelschuss-Pfad in ring3.rs zuständig).
pub fn syscall_beenden(rahmen: *mut TrapFrame, code: u64) -> Option<*mut TrapFrame> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if !PLANUNG_AN.load(Ordering::Relaxed) || aktuell == KERNEL_SLOT {
        return None;
    }
    {
        let mut tabelle = TABELLE.try_lock()?;
        // Exit-Code festhalten UND beim Elternteil hinterlegen, BEVOR der
        // Prozess die CPU verliert — der Aufrufer (Shell, oder ein
        // wartender Elternprozess) holt ihn später ab.
        ende_vermerken(&mut tabelle, aktuell, ProzessEnde::Beendet(code));
    }
    // Der Aktuelle ist jetzt nicht mehr lauffähig -> Regel 1 erzwingt den
    // Wechsel, ganz ohne Sonderbehandlung.
    Some(freiwillig_wechseln(rahmen))
}

/// Syscall `schlafen(ms)`: Prozess auf `Wartend` setzen; der Timer weckt ihn.
pub fn syscall_schlafen(rahmen: *mut TrapFrame, ms: u64) -> Option<*mut TrapFrame> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if !PLANUNG_AN.load(Ordering::Relaxed) || aktuell == KERNEL_SLOT {
        return None;
    }
    {
        let mut tabelle = TABELLE.try_lock()?;
        let prozess = tabelle[aktuell].as_mut()?;
        // Mindestens 1 ms, damit `wach_ab_ms == 0` eindeutig "kein Weckruf"
        // bleibt und schlafen(0) nicht ewig wartet.
        prozess.wach_ab_ms = crate::zeit::ms_seit_boot() + ms.max(1);
        prozess.wartet_auf = Some(Warteauf::Zeit);
        prozess.zustand = Zustand::Wartend;
    }
    Some(freiwillig_wechseln(rahmen))
}

/// LEGT DEN LAUFENDEN PROZESS SCHLAFEN, bis `grund` erfüllt ist.
///
/// ==========================================================================
/// WIE EIN BLOCKIERENDER SYSCALL BEI UNS FUNKTIONIERT — DER NEUSTART
///
/// Die naheliegende Vorstellung ist: „Der Syscall hält mitten drin an und
/// macht später weiter." Das geht bei uns NICHT, und zwar aus einem sehr
/// konkreten Grund: Unser gesicherter Kontext ist der TRAP-RAHMEN am
/// Eingang des Syscalls (siehe prozess.rs). Schalten wir auf einen anderen
/// Prozess um, wird dieser Rahmen später per `iretq` geladen — die CPU
/// landet also hinter dem `int 0x80` in Ring 3, NICHT mitten im Rust-Code
/// des Dispatchers. Der Rust-Stack des halben Syscalls ist dann verloren.
///
/// Also andersherum, so wie es echte Kernel auch machen: Der Syscall wird
/// **von vorn wiederholt**. Der Aufrufer stellt `rip` um zwei Bytes zurück
/// — genau die Länge von `int 0x80` (CD 80) —, sodass der erste Befehl nach
/// dem Aufwachen wieder der Syscall ist, mit unveränderten Argumenten.
///
/// Zwei Bedingungen, die daraus folgen (und die jeder blockierende Syscall
/// erfüllen muss):
///  * Er darf bis zum Blockieren NICHTS verändert haben. Ein `lese`, das
///    schon Bytes aus der Pipe genommen hätte, würde sie beim Neustart
///    doppelt lesen. Deshalb melden die Pipe-Operationen `Blockiert`,
///    BEVOR sie irgendetwas anfassen.
///  * `rax` und `rdx` dürfen nicht beschrieben werden — sie tragen beim
///    Neustart noch die Syscall-Nummer und das dritte Argument.
///
/// Liefert den Rahmen, mit dem es weitergeht (der eines anderen Prozesses).
pub fn blockieren(rahmen: *mut TrapFrame, grund: Warteauf, wach_ab_ms: u64) -> *mut TrapFrame {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if !PLANUNG_AN.load(Ordering::Relaxed) || aktuell == KERNEL_SLOT {
        return rahmen;
    }
    {
        let mut tabelle = match TABELLE.try_lock() {
            Some(tabelle) => tabelle,
            None => return rahmen,
        };
        let prozess = match tabelle[aktuell].as_mut() {
            Some(prozess) => prozess,
            None => return rahmen,
        };
        prozess.wartet_auf = Some(grund);
        // 0 = keine Frist (Pipes, warte) — dann weckt nur das Ereignis.
        prozess.wach_ab_ms = wach_ab_ms;
        prozess.zustand = Zustand::Wartend;
    }
    // Nicht mehr lauffähig -> Regel 1 der Wechsel-Entscheidung greift.
    freiwillig_wechseln(rahmen)
}

/// Es gibt kein Kind mit dieser PID — auf sein Ende zu warten wäre ein
/// Hänger für immer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeinSolchesKind;

/// `warte(pid)`: Holt das Ergebnis eines beendeten Kindes ab.
///
/// Liefert `Some((pid, ende))`, wenn eines bereitliegt, sonst `None` — dann
/// blockiert der Aufrufer. `pid == 0` heisst „irgendein Kind".
///
/// `Err(KeinSolchesKind)` = es gibt gar kein solches Kind (weder lebend noch
/// mit Ergebnis). Darauf zu warten wäre ein Hänger für immer, also ist es ein
/// Fehler.
pub fn kind_ende_abholen(pid: Pid) -> Result<Option<(Pid, ProzessEnde)>, KeinSolchesKind> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    let mut tabelle = match TABELLE.try_lock() {
        Some(tabelle) => tabelle,
        None => return Ok(None), // gleich nochmal versuchen
    };
    let eigene_pid = match tabelle[aktuell].as_ref() {
        Some(prozess) => prozess.pid,
        None => return Err(KeinSolchesKind),
    };
    // Liegt schon ein Ergebnis bereit?
    if let Some(prozess) = tabelle[aktuell].as_mut() {
        if let Some(fund) = prozess.kind_ende_abholen(pid) {
            return Ok(Some(fund));
        }
    }
    // Nein — gibt es das Kind denn (noch)?
    let lebt = tabelle.iter().flatten().any(|kandidat| {
        kandidat.eltern == Some(eigene_pid)
            && (pid == 0 || kandidat.pid == pid)
            && kandidat.zustand != Zustand::Beendet
    });
    if lebt {
        Ok(None) // warten lohnt sich
    } else {
        Err(KeinSolchesKind) // warten wäre ein Hänger für immer
    }
}

/// Trägt den Elternteil eines noch nicht eingeplanten Prozesses ein.
pub fn eltern_setzen(prozess: &mut Prozess, eltern: Option<Pid>) {
    prozess.eltern = eltern;
}

/// Die PID des laufenden Prozesses — oder `None` im Kernel-Prozess.
/// Damit trägt `prozess_starten` die Eltern-Beziehung ein, wenn ein
/// PROZESS startet; startet die Shell (Kernel), gibt es keinen Elternteil.
pub fn aktuelle_user_pid() -> Option<Pid> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return None;
    }
    match TABELLE.try_lock() {
        Some(tabelle) => tabelle[aktuell].as_ref().map(|prozess| prozess.pid),
        None => None,
    }
}

/// Zählt einen Syscall des laufenden Prozesses (Aktivitätsmaß).
pub fn syscall_zaehlen() {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return;
    }
    if let Some(mut tabelle) = TABELLE.try_lock() {
        if let Some(prozess) = tabelle[aktuell].as_mut() {
            prozess.syscalls += 1;
        }
    }
}

/// TÖTET den laufenden User-Prozess nach einem Fault (Dauerregel II, jetzt
/// prozess-weise). Der Interrupt-Rahmen wird auf den Ring-0-Sterbe-Stub
/// umgebogen: Das `iretq` des Fault-Handlers landet damit im Kernel — auf dem
/// Kernel-Stack des Sterbenden —, und der Stub schaltet auf den nächsten
/// Prozess um. `false` = es lief kein eingeplanter User-Prozess.
pub fn user_prozess_toeten(rahmen: &mut TrapFrame) -> bool {
    if !PLANUNG_AN.load(Ordering::Relaxed) {
        return false;
    }
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return false;
    }
    let stack_oben = {
        let mut tabelle = match TABELLE.try_lock() {
            Some(tabelle) => tabelle,
            None => return false,
        };
        match tabelle[aktuell].as_ref() {
            Some(prozess) if prozess.ist_user() => {}
            _ => return false,
        }
        ende_vermerken(&mut tabelle, aktuell, ProzessEnde::Abgestuerzt);
        match tabelle[aktuell].as_ref() {
            Some(prozess) => prozess.kern_stack_oben().as_u64(),
            None => return false,
        }
    };
    rahmen.rip = sterbe_stub_adresse();
    rahmen.cs = crate::gdt::kernel_code_selektor();
    rahmen.ss = crate::gdt::kernel_data_selektor();
    rahmen.rsp = stack_oben;
    // IF AUS: Das Sterben selbst darf nicht mitten drin unterbrochen werden.
    // (Bit 1 ist ein reserviertes 1-Bit von RFLAGS.)
    rahmen.rflags = 0x002;
    true
}

// ---------------------------------------------------------------------------
// (G) ÖFFENTLICHE API für Kernel-Kontext (Shell, main.rs, Task-Manager)
// ---------------------------------------------------------------------------

/// Tabellen-Zugriff aus Kernel-Kontext — IMMER mit ausgeschalteten
/// Interrupts. Das ist nicht Kosmetik: Solange die Sperre steht, kann der
/// Timer nicht feuern, also auch kein Wechsel dazwischenkommen. Genau darum
/// darf der Interrupt-Handler mit `try_lock` arbeiten, ohne je zu warten.
fn mit_tabelle<T>(f: impl FnOnce(&mut Tabelle) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut TABELLE.lock()))
}

/// Richtet den Scheduler ein: Der LAUFENDE Kontext (der Executor bzw. der
/// Boot-Kontext) wird als Kernel-Prozess PID 0 eingetragen, und die Planung
/// wird eingeschaltet. Danach ist jeder Timer-Tick ein potenzieller
/// Kontext-Wechsel — solange nur PID 0 existiert, wechselt aber nichts
/// (Regel 3 der Entscheidung).
pub fn init() {
    mit_tabelle(|tabelle| {
        if tabelle[KERNEL_SLOT].is_none() {
            tabelle[KERNEL_SLOT] = Some(Prozess::kernel());
        }
    });
    AKTUELL.store(KERNEL_SLOT, Ordering::Relaxed);
    SCHEIBE_REST.store(SCHEIBE_TICKS, Ordering::Relaxed);
    crate::gdt::rsp0_setzen(crate::gdt::rsp0_standard());
    PLANUNG_AN.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[SCHED] Praemptiver Round-Robin aktiv: Zeitscheibe {} Ticks (~{} ms), \
         Kernel-Prozess ist PID 0.",
        SCHEIBE_TICKS,
        crate::zeit::ms_von_ticks(SCHEIBE_TICKS as u64)
    );
}

/// Läuft die Planung?
pub fn aktiv() -> bool {
    PLANUNG_AN.load(Ordering::Relaxed)
}

/// Sperrt die Umplanung (Zähler). NUR für den alten Einzelschuss-Ring-3-Pfad
/// (`ring3::nach_ring3`): Der führt Ring-3-Code IM Kontext des Kernel-
/// Prozesses aus, mit fremdem CR3 — käme dort ein Wechsel dazwischen, würde
/// PID 0 mit Kernel-CR3 mitten im User-Code fortgesetzt.
pub fn sperre_erhoehen() {
    SPERRE.fetch_add(1, Ordering::SeqCst);
}

/// Gegenstück zu `sperre_erhoehen`.
pub fn sperre_senken() {
    SPERRE.fetch_sub(1, Ordering::SeqCst);
}

/// PID des laufenden Prozesses.
pub fn aktuelle_pid() -> Pid {
    mit_tabelle(|tabelle| {
        tabelle[AKTUELL.load(Ordering::Relaxed)]
            .as_ref()
            .map(|p| p.pid)
            .unwrap_or(KERNEL_PID)
    })
}

/// PID des laufenden Prozesses OHNE Lock (für den Syscall-Pfad).
/// Liefert `None`, wenn der Kernel-Prozess läuft.
pub fn aktueller_user_slot() -> Option<usize> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        None
    } else {
        Some(aktuell)
    }
}

/// Die PID des laufenden User-Prozesses (lock-frei über die Spur-Meldung
/// hinaus nicht nötig — daher `try_lock`, im Zweifel 0).
pub fn laufende_user_pid() -> Pid {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return KERNEL_PID;
    }
    match TABELLE.try_lock() {
        Some(tabelle) => tabelle[aktuell].as_ref().map(|p| p.pid).unwrap_or(KERNEL_PID),
        None => KERNEL_PID,
    }
}

/// Führt eine Operation auf der HANDLE-TABELLE des laufenden Prozesses aus
/// (Serie 6, Teil 4). Das ist der einzige Weg, wie ein Syscall an Handles
/// kommt — und dadurch kann er per Konstruktion nur die des AUFRUFENDEN
/// Prozesses sehen.
///
/// `Err(Fehler::UngueltigerHandle)`, wenn gar kein User-Prozess läuft: Ein
/// Syscall aus dem Kernel-Prozess (z. B. der Executor über `yield`) hat keine
/// Handle-Tabelle — und soll auch keine bekommen.
pub fn mit_handles<T>(
    f: impl FnOnce(&mut crate::syscall::handle::HandleTabelle) -> T,
) -> Result<T, crate::syscall::Fehler> {
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return Err(crate::syscall::Fehler::UngueltigerHandle);
    }
    // try_lock statt lock: Im Syscall sind Interrupts aus, also hält die
    // Tabelle gerade niemand — aber warten würden wir hier nie (siehe die
    // Lock-Disziplin in syscall/mod.rs).
    let mut tabelle = TABELLE.try_lock().ok_or(crate::syscall::Fehler::Belegt)?;
    let prozess = tabelle[aktuell]
        .as_mut()
        .ok_or(crate::syscall::Fehler::UngueltigerHandle)?;
    Ok(f(&mut prozess.handles))
}

/// ERWEITERT DEN USER-HEAP des laufenden Prozesses um `bytes` (aufgerundet
/// auf Seiten) und liefert `(neue Basis, neue Gesamtgrösse)`.
///
/// Warum das hier steht und nicht in `adressraum`: Es braucht BEIDES — den
/// Adressraum (mappen) und den Prozess-Kontrollblock (wie viel ist schon
/// da?). Der Scheduler ist die einzige Stelle, die an beides herankommt.
///
/// Die neuen Seiten sind `PRESENT|WRITABLE|USER_ACCESSIBLE|NX` und **frisch
/// genullt** (`map_benutzer` nullt jeden Frame — sonst leckte der Inhalt des
/// Vorbesitzers nach Ring 3).
pub fn heap_erweitern(bytes: u64) -> Result<(u64, u64), crate::syscall::Fehler> {
    use crate::syscall::Fehler;
    let aktuell = AKTUELL.load(Ordering::Relaxed);
    if aktuell == KERNEL_SLOT {
        return Err(Fehler::NichtUnterstuetzt);
    }
    // Auf ganze Seiten aufrunden.
    let bytes = bytes
        .checked_add(4095)
        .ok_or(Fehler::ZuGross)?
        & !0xfff;
    if bytes == 0 {
        return Err(Fehler::UngueltigesArgument);
    }

    // try_lock statt lock: Im Syscall sind Interrupts aus, es hält sie
    // niemand — aber warten würden wir hier nie (Lock-Disziplin).
    let mut tabelle = TABELLE.try_lock().ok_or(Fehler::Belegt)?;
    let prozess = tabelle[aktuell].as_mut().ok_or(Fehler::NichtUnterstuetzt)?;

    let alt = prozess.heap_bytes;
    let neu = alt.checked_add(bytes).ok_or(Fehler::ZuGross)?;
    if neu > crate::prozess::HEAP_MAX_BYTES {
        // HARTE GRENZE: Der Heap darf die Lücke zum Stack nicht auffressen.
        return Err(Fehler::KeinPlatz);
    }
    let basis = crate::prozess::HEAP_START + alt;

    let raum = prozess.raum.as_mut().ok_or(Fehler::NichtUnterstuetzt)?;
    raum.bereich_mappen_mit_rechten(
        VirtAddr::new(basis),
        bytes as usize,
        // NX: Auf dem Heap liegen Daten. Ausführbarer Heap wäre die
        // Einladung, die W^X aus Serie 6 Teil 5 wieder aufzuheben.
        adressraum::Rechte::SCHREIBEN,
    )
    .map_err(|_| Fehler::KeinPlatz)?;

    prozess.heap_bytes = neu;
    Ok((basis, neu))
}

/// Wie viel User-Heap hält ein Prozess? (Diagnose und Leck-Tests.)
pub fn heap_bytes(pid: Pid) -> Option<u64> {
    mit_tabelle(|tabelle| {
        tabelle
            .iter()
            .flatten()
            .find(|prozess| prozess.pid == pid)
            .map(|prozess| prozess.heap_bytes)
    })
}

/// Zugriff auf den ADRESSRAUM eines Prozesses (Diagnose und Tests: So legt der
/// Kernel Daten in einen Prozess, ohne dessen Adressraum zu aktivieren —
/// dasselbe Muster wie beim künftigen ELF-Loader).
pub fn mit_prozess_raum<T>(
    pid: Pid,
    f: impl FnOnce(&mut adressraum::AdressRaum) -> T,
) -> Option<T> {
    mit_tabelle(|tabelle| {
        for prozess in tabelle.iter_mut().flatten() {
            if prozess.pid == pid {
                return prozess.raum.as_mut().map(f);
            }
        }
        None
    })
}

/// Wie viele Handles hält ein Prozess offen? (Leak-Test)
pub fn handle_anzahl(pid: Pid) -> Option<(usize, usize)> {
    mit_tabelle(|tabelle| {
        for prozess in tabelle.iter_mut().flatten() {
            if prozess.pid == pid {
                return Some((
                    prozess.handles.anzahl_offen(),
                    prozess.handles.anzahl_sockets(),
                ));
            }
        }
        None
    })
}

/// Nimmt einen fertig gebauten Prozess in die Tabelle auf ("einplanen", NICHT
/// "starten"): Die CPU bekommt er erst beim nächsten Timer-Tick. Genau deshalb
/// gibt es keinen Pfad, auf dem der Kernel-Prozess ohne gesicherten Kontext
/// verlassen wird (Invariante 1 in docs/scheduler-design.md).
pub fn einplanen(prozess: Prozess) -> Option<Pid> {
    let pid = prozess.pid;
    let name = prozess.name.clone();
    let slot = mit_tabelle(|tabelle| {
        let frei = tabelle.iter().position(|eintrag| eintrag.is_none())?;
        // Slot 0 gehört dem Kernel-Prozess und ist nach init() belegt.
        if frei == KERNEL_SLOT {
            return None;
        }
        tabelle[frei] = Some(prozess);
        Some(frei)
    })?;
    crate::serial_println!(
        "[SCHED] PID {} '{}' eingeplant (Slot {}) — laeuft ab dem naechsten Tick.",
        pid,
        name,
        slot
    );
    Some(pid)
}

/// Beendet einen Prozess von aussen (Shell, Task-Manager). Der Prozess
/// bekommt keine CPU mehr; abgeräumt wird er vom Aufräum-Task.
pub fn beenden(pid: Pid) -> bool {
    if pid == KERNEL_PID {
        return false; // Den Kernel-Prozess beendet niemand.
    }
    mit_tabelle(|tabelle| {
        let slot = tabelle.iter().position(|eintrag| {
            matches!(eintrag, Some(prozess)
                if prozess.pid == pid && prozess.zustand != Zustand::Beendet)
        });
        match slot {
            Some(slot) => {
                ende_vermerken(tabelle, slot, ProzessEnde::Gestoppt);
                true
            }
            None => false,
        }
    })
}

/// Setzt/löscht den Wartezustand von aussen (die Naht für blockierende
/// Syscalls, siehe docs/scheduler-design.md §8).
pub fn warten_setzen(pid: Pid, wartet: bool) -> bool {
    mit_tabelle(|tabelle| {
        for prozess in tabelle.iter_mut().flatten() {
            if prozess.pid == pid {
                prozess.zustand = if wartet {
                    Zustand::Wartend
                } else {
                    Zustand::Lauffaehig
                };
                prozess.wach_ab_ms = 0;
                return true;
            }
        }
        false
    })
}

/// Wartet aus KERNEL-KONTEXT darauf, dass ein Prozess endet — und liefert,
/// WIE er geendet hat. `None` = Frist abgelaufen (der Prozess läuft weiter)
/// oder es gibt ihn nicht (mehr).
///
/// WARUM `hlt` UND NICHT `await`: Der Shell-Befehl, der hier wartet, ist eine
/// gewöhnliche synchrone Funktion — der kooperative Executor bekommt in
/// dieser Zeit keine Gelegenheit zu laufen, also auch der Aufräum-Task nicht.
/// Deshalb erntet diese Funktion den Prozess SELBST (`aufraeumen()`), sobald
/// er beendet ist. Das ist erlaubt: Wir sind in Kernel-Kontext, dürfen also
/// Locks nehmen und Speicher freigeben.
///
/// Der PREIS steht ehrlich hier: Solange gewartet wird, laufen die anderen
/// KERNEL-Tasks (Compositor, Maus) nicht — der Bildschirm steht. Die
/// PROZESSE laufen sehr wohl weiter, denn die verdrängt der Timer. Genau
/// dasselbe tut schon `praemptionstest`. Eine wirklich nebenläufige Variante
/// bräuchte ein `async` Befehl-Trait; das ist eine eigene Aufgabe.
///
/// `frist_ms == 0` heisst „ohne Frist".
pub fn warten_auf(pid: Pid, frist_ms: u64) -> Option<ProzessEnde> {
    let frist = crate::zeit::ms_seit_boot() + frist_ms;
    loop {
        let stand = mit_tabelle(|tabelle| {
            tabelle
                .iter()
                .flatten()
                .find(|prozess| prozess.pid == pid)
                .map(|prozess| (prozess.zustand, prozess.ende))
        });
        match stand {
            // Weg — entweder nie da gewesen oder schon abgeräumt.
            None => return None,
            Some((Zustand::Beendet, ende)) => {
                // Selbst ernten, damit Adressraum und Kernel-Stack sofort
                // zurückfliessen (der Aufräum-Task kommt hier nicht dran).
                aufraeumen();
                // `ende` ist an allen drei Beendigungs-Stellen gesetzt; der
                // Ersatzwert ist reine Vorsicht.
                return Some(ende.unwrap_or(ProzessEnde::Gestoppt));
            }
            Some(_) => {}
        }
        if frist_ms != 0 && crate::zeit::ms_seit_boot() >= frist {
            return None;
        }
        // Bis zum nächsten Interrupt schlafen — in dieser Zeit bekommt der
        // gewartete Prozess seine Zeitscheiben.
        crate::zeit::warte_auf_interrupt();
    }
}

/// Wie viele Kind-Ergebnisse liegen bei diesem Prozess ungeholt herum?
/// `None` = den Prozess gibt es nicht (mehr). Für die Zombie-Tests.
pub fn kinder_enden_offen(pid: Pid) -> Option<usize> {
    mit_tabelle(|tabelle| {
        tabelle
            .iter()
            .flatten()
            .find(|prozess| prozess.pid == pid)
            .map(|prozess| prozess.kinder_enden_offen())
    })
}

/// WORAUF ein Prozess gerade wartet (`None` = er wartet nicht).
///
/// Diagnose und Tests: Es ist ein Unterschied, ob ein Prozess WIRKLICH
/// blockiert oder in einer Warteschleife dreht — und nur der vermerkte
/// Grund macht ihn sichtbar.
pub fn warte_grund(pid: Pid) -> Option<Warteauf> {
    mit_tabelle(|tabelle| {
        tabelle
            .iter()
            .flatten()
            .find(|prozess| prozess.pid == pid)
            .and_then(|prozess| prozess.wartet_auf)
    })
}

/// Wie ein Prozess geendet hat — OHNE ihn abzuräumen.
///
/// `None` heisst „lebt noch ODER ist schon abgeräumt". Wer den Unterschied
/// braucht, muss das Ergebnis rechtzeitig einsammeln — und genau dafür ist
/// diese Funktion da: Die Shell fragt sie in ihrer Pump-Schleife ab, BEVOR
/// sie `aufraeumen()` ruft. Sonst wäre der Exit-Code weg, denn Abräumen
/// löscht den Tabellen-Eintrag.
pub fn ende_abfragen(pid: Pid) -> Option<ProzessEnde> {
    mit_tabelle(|tabelle| {
        tabelle
            .iter()
            .flatten()
            .find(|prozess| prozess.pid == pid)
            .and_then(|prozess| prozess.ende)
    })
}

/// Räumt beendete Prozesse ab: Adressraum-Frames und Kernel-Stack gehen
/// zurück. Liefert die Anzahl.
///
/// WARUM NICHT IM TIMER: `Drop` gibt Frames frei (nimmt die memory-Locks) und
/// Heap-Speicher — beides im Interrupt-Kontext verboten. Der Timer MARKIERT
/// nur; abgeräumt wird hier, aus Kernel-Kontext. Und zwar bewusst in zwei
/// Schritten: erst aus der Tabelle nehmen, dann AUSSERHALB des Locks fallen
/// lassen.
pub fn aufraeumen() -> usize {
    let geerntet: Vec<Prozess> = mit_tabelle(|tabelle| {
        let laeuft = AKTUELL.load(Ordering::Relaxed);
        let mut raus = Vec::new();
        for (slot, eintrag) in tabelle.iter_mut().enumerate() {
            // Den Kernel-Prozess nie, und den GERADE LAUFENDEN nie — dessen
            // Kernel-Stack steht in TSS.RSP0 und ist gleich wieder in Benutzung.
            if slot == KERNEL_SLOT || slot == laeuft {
                continue;
            }
            let beendet = matches!(eintrag.as_ref().map(|p| p.zustand), Some(Zustand::Beendet));
            if beendet {
                if let Some(prozess) = eintrag.take() {
                    raus.push(prozess);
                }
            }
        }
        raus
    });
    let anzahl = geerntet.len();
    for prozess in geerntet {
        crate::serial_println!(
            "[SCHED] PID {} '{}' abgeraeumt: {} (Code {}), {} us CPU, \
             {} Praemptionen, {} Syscalls.",
            prozess.pid,
            prozess.name,
            prozess.ende.map(|e| e.text()).unwrap_or("Ende unbekannt"),
            prozess.ende.map(|e| e.code()).unwrap_or(0),
            prozess.cpu_us,
            prozess.praemptionen,
            prozess.syscalls
        );
        drop(prozess); // hier fliessen Frames und Kernel-Stack zurück
    }
    anzahl
}

/// Der Aufräum-Task: läuft im Kernel-Prozess und gibt die Ressourcen
/// beendeter Prozesse zurück.
pub async fn aufraeum_task() {
    loop {
        crate::zeit::warte_ms(250).await;
        aufraeumen();
    }
}

/// Gibt es ausser dem Kernel-Prozess noch lauffähige Prozesse? Der Executor
/// fragt das im Leerlauf: Dann gibt er die Scheibe SOFORT ab, statt bis zum
/// nächsten Tick zu schlafen.
pub fn andere_lauffaehig() -> bool {
    if !PLANUNG_AN.load(Ordering::Relaxed) {
        return false; // billiger Normalfall: ein Atomic-Laden
    }
    // `try_lock` statt `lock`, seit `zeit::warte_auf_interrupt` das hier
    // fragt: Diese Funktion ist ein HINWEIS, keine Wahrheit, und sie wird aus
    // Warteschleifen gerufen, deren Aufrufer die Tabelle theoretisch schon
    // halten könnte. Ein Fehlversuch heisst „im Zweifel schlafen" — die
    // nächste Runde fragt erneut, und der Timer plant ohnehin weiter.
    x86_64::instructions::interrupts::without_interrupts(|| match TABELLE.try_lock() {
        Some(tabelle) => tabelle
            .iter()
            .skip(1)
            .filter_map(|eintrag| eintrag.as_ref())
            .any(|prozess| prozess.zustand.ist_lauffaehig()),
        None => false,
    })
}

/// Gibt die aktuelle Zeitscheibe ab (freiwillige Abgabe per Syscall).
/// Funktioniert aus Ring 0 genauso wie aus Ring 3 — das INT-0x80-Gate hat
/// DPL 3, und eine Software-Interrupt-Auslösung verlangt nur CPL <= DPL.
pub fn abgeben() {
    // unsafe: `int 0x80` ist unser Syscall-Tor. Der Dispatcher wechselt ggf.
    // den Prozess und kehrt hierher zurück, sobald wir wieder dran sind;
    // alle Register werden aus dem Trap-Rahmen wiederhergestellt.
    unsafe {
        core::arch::asm!("int 0x80", inout("rax") crate::syscall::SYS_YIELD => _);
    }
}

/// CPU-Zeit, die nicht dem Kernel-Prozess gehörte (monoton, in µs).
pub fn fremdzeit_us() -> u64 {
    FREMDZEIT_US.load(Ordering::Relaxed)
}

/// Gesamtzahl der Kontext-Wechsel.
pub fn wechsel_gesamt() -> u64 {
    WECHSEL_GESAMT.load(Ordering::Relaxed)
}

/// DIE Momentaufnahme für Task-Manager und Shell (nach PID sortiert, damit
/// die Anzeige beim Aktualisieren nicht springt).
pub fn momentaufnahme() -> Vec<ProzessMoment> {
    let jetzt_us = crate::zeit::us_seit_boot();
    let mut zeilen: Vec<ProzessMoment> = mit_tabelle(|tabelle| {
        tabelle
            .iter()
            .filter_map(|eintrag| eintrag.as_ref())
            .map(|prozess| ProzessMoment {
                pid: prozess.pid,
                name: String::from(prozess.name.as_str()),
                zustand: prozess.zustand,
                // Der laufende Prozess sammelt seine Scheibe erst beim
                // Wechsel ein — für die Anzeige rechnen wir sie dazu.
                cpu_us: prozess.cpu_us
                    + if prozess.zustand == Zustand::Laeuft {
                        jetzt_us.saturating_sub(prozess.scheibe_start_us)
                    } else {
                        0
                    },
                laufzeit_us: jetzt_us.saturating_sub(prozess.start_us),
                praemptionen: prozess.praemptionen,
                abgaben: prozess.abgaben,
                syscalls: prozess.syscalls,
                ist_user: prozess.ist_user(),
            })
            .collect()
    });
    zeilen.sort_unstable_by_key(|zeile| zeile.pid);
    zeilen
}

// ---------------------------------------------------------------------------
// Tests der REINEN Logik (die Hardware-Beweise liegen in tests/scheduler.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Zeitscheibe: läuft genau beim letzten Tick ab und füllt sich selbst
    /// wieder auf.
    #[test_case]
    fn test_scheibe_tick() {
        // Eine volle Scheibe von 5 Ticks: 4x runterzählen, beim 5. abgelaufen.
        let mut rest = SCHEIBE_TICKS;
        for erwartet in (1..SCHEIBE_TICKS).rev() {
            let (neu, abgelaufen) = scheibe_tick(rest);
            assert!(!abgelaufen, "zu frueh abgelaufen bei Rest {}", rest);
            assert_eq!(neu, erwartet);
            rest = neu;
        }
        let (neu, abgelaufen) = scheibe_tick(rest);
        assert!(abgelaufen, "Scheibe muss beim letzten Tick ablaufen");
        assert_eq!(neu, SCHEIBE_TICKS, "Scheibe muss sich selbst nachfuellen");
        // Robust gegen 0 (z. B. nach einem Neustart der Planung):
        assert_eq!(scheibe_tick(0), (SCHEIBE_TICKS, true));
        // 20 ms bei 250 Hz — die Zahl aus dem Entwurf, hier festgenagelt.
        assert_eq!(crate::zeit::ms_von_ticks(SCHEIBE_TICKS as u64), 20);
    }

    /// Weck-Fälligkeit: 0 heisst "kein Weckruf" und darf nie fällig sein.
    #[test_case]
    fn test_weck_faellig() {
        assert!(!weck_faellig(0, 0));
        assert!(!weck_faellig(0, 1_000_000));
        assert!(!weck_faellig(100, 99));
        assert!(weck_faellig(100, 100));
        assert!(weck_faellig(100, 101));
    }

    /// Round-Robin-Auswahl: Wartende, Beendete und leere Slots werden
    /// übersprungen; gesucht wird zyklisch HINTER dem aktuellen.
    #[test_case]
    fn test_naechster_lauffaehig() {
        use Zustand::*;
        // Slot 0 Kernel (läuft), 1 beendet, 2 wartend, 3 lauffähig.
        let bild = [
            Some(Laeuft),
            Some(Beendet),
            Some(Wartend),
            Some(Lauffaehig),
            None,
            None,
            None,
            None,
        ];
        assert_eq!(naechster_lauffaehig(&bild, 0), Some(3));
        assert_eq!(naechster_lauffaehig(&bild, 3), Some(0)); // zyklisch
        // Nur der Kernel-Prozess: er wird sich selbst geliefert.
        let allein = [Some(Laeuft), None, None, None, None, None, None, None];
        assert_eq!(naechster_lauffaehig(&allein, 0), Some(0));
        // Keiner lauffähig (kann im echten System nicht passieren — PID 0 ist
        // immer lauffähig —, die Funktion muss es aber sauber melden).
        let keiner = [Some(Beendet), Some(Wartend), None, None, None, None, None, None];
        assert_eq!(naechster_lauffaehig(&keiner, 0), None);
        let leer: [Option<Zustand>; 0] = [];
        assert_eq!(naechster_lauffaehig(&leer, 0), None);
    }

    /// Die Wechsel-Entscheidung in allen vier Regeln.
    #[test_case]
    fn test_wechsel_entscheiden_regeln() {
        use Zustand::*;
        let zwei = [
            Some(Laeuft),
            Some(Lauffaehig),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        // Regel 2: ohne Anlass kein Wechsel — auch wenn andere warten.
        assert_eq!(wechsel_entscheiden(&zwei, 0, false, false), None);
        // Zeitscheibe abgelaufen -> wechseln.
        assert_eq!(wechsel_entscheiden(&zwei, 0, true, false), Some(1));
        // Freiwillige Abgabe -> wechseln, auch mit voller Scheibe.
        assert_eq!(wechsel_entscheiden(&zwei, 0, false, true), Some(1));

        // Regel 3: nur PID 0 lauffähig -> kein Wechsel, obwohl die Scheibe
        // abgelaufen ist (der Alltag ohne User-Prozesse).
        let allein = [Some(Laeuft), Some(Beendet), None, None, None, None, None, None];
        assert_eq!(wechsel_entscheiden(&allein, 0, true, false), None);

        // Regel 1: Der Aktuelle ist nicht mehr lauffähig -> Wechsel ist
        // Pflicht, auch OHNE abgelaufene Scheibe und ohne freiwillige Abgabe.
        let tot = [
            Some(Lauffaehig),
            Some(Beendet),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(wechsel_entscheiden(&tot, 1, false, false), Some(0));
        // Wartend zählt genauso (blockierender Syscall).
        let wartet = [
            Some(Lauffaehig),
            Some(Wartend),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(wechsel_entscheiden(&wartet, 1, false, false), Some(0));
        // Leerer Slot als "aktuell" (darf nie vorkommen, muss aber sauber sein).
        let leer = [Some(Lauffaehig), None, None, None, None, None, None, None];
        assert_eq!(wechsel_entscheiden(&leer, 1, false, false), Some(0));
        // Gar niemand da: bleiben (der Aufrufer bleibt beim Kernel-Prozess).
        let nichts = [None; MAX_PROZESSE];
        assert_eq!(wechsel_entscheiden(&nichts, 0, true, false), None);
    }

    /// FAIRNESS über N Runden: Bei k lauffähigen Prozessen bekommt jeder über
    /// k*N Zeitscheiben GENAU N — und zwar in strikter Reihum-Folge. Das ist
    /// die eigentliche Zusage von Round-Robin, deshalb wird sie hier
    /// nachgerechnet statt geglaubt.
    #[test_case]
    fn test_fairness_ueber_n_runden() {
        use Zustand::*;
        const RUNDEN: usize = 50;
        // Vier lauffähige (Slots 0..3), einer wartend, einer beendet.
        let mut bild = [
            Some(Laeuft),
            Some(Lauffaehig),
            Some(Lauffaehig),
            Some(Lauffaehig),
            Some(Wartend),
            Some(Beendet),
            None,
            None,
        ];
        let lauffaehig = [0usize, 1, 2, 3];
        let mut zaehler = [0usize; MAX_PROZESSE];
        let mut aktuell = 0usize;
        let mut folge = Vec::new();

        for _ in 0..(RUNDEN * lauffaehig.len()) {
            let ziel = wechsel_entscheiden(&bild, aktuell, true, false)
                .expect("bei 4 Lauffaehigen muss immer gewechselt werden");
            // Zustände nachziehen, wie es der echte Wechsel tut:
            bild[aktuell] = Some(Lauffaehig);
            bild[ziel] = Some(Laeuft);
            aktuell = ziel;
            zaehler[ziel] += 1;
            folge.push(ziel);
        }

        // Jeder Lauffähige kam genau RUNDEN-mal dran ...
        for slot in lauffaehig {
            assert_eq!(zaehler[slot], RUNDEN, "Slot {} wurde unfair bedient", slot);
        }
        // ... der Wartende und der Beendete NIE.
        assert_eq!(zaehler[4], 0, "ein wartender Prozess bekam CPU");
        assert_eq!(zaehler[5], 0, "ein beendeter Prozess bekam CPU");
        // Und die Reihenfolge ist strikt reihum (1,2,3,0,1,2,3,0,...):
        assert_eq!(folge[..8], [1usize, 2, 3, 0, 1, 2, 3, 0]);
    }

    /// Ein WARTENDER Prozess bekommt keine CPU, ein GEWECKTER sofort wieder.
    #[test_case]
    fn test_wecken_bringt_prozess_zurueck() {
        use Zustand::*;
        let mut bild = [Some(Laeuft), Some(Wartend), None, None, None, None, None, None];
        // Schläft: kein Wechsel, obwohl die Scheibe abgelaufen ist.
        assert_eq!(wechsel_entscheiden(&bild, 0, true, false), None);
        // Geweckt (das macht schlaefer_wecken im Timer): sofort wieder dabei.
        bild[1] = Some(Lauffaehig);
        assert_eq!(wechsel_entscheiden(&bild, 0, true, false), Some(1));
    }

    /// DIE WECK-ZUORDNUNG: Wer wird von welchem Ereignis geweckt — und
    /// vor allem: wer NICHT.
    #[test_case]
    fn test_weck_passt() {
        use Warteauf::*;
        // Dieselbe Pipe, dieselbe Richtung -> passt.
        assert!(weck_passt(PipeLesen(3), PipeLesen(3)));
        assert!(weck_passt(PipeSchreiben(3), PipeSchreiben(3)));
        // FREMDE Pipe -> nie. (Sonst würde ein Datenstrom auf Pipe 3 die
        // Warter aller anderen Pipes wecken; sie liefen alle in einen
        // Syscall-Neustart und blockierten sofort wieder.)
        assert!(!weck_passt(PipeLesen(3), PipeLesen(4)));
        assert!(!weck_passt(PipeSchreiben(0), PipeSchreiben(1)));
        // FALSCHE RICHTUNG -> nie: Wer auf Platz wartet, wird von neuen
        // Daten nicht lauffähig.
        assert!(!weck_passt(PipeLesen(3), PipeSchreiben(3)));
        assert!(!weck_passt(PipeSchreiben(3), PipeLesen(3)));

        // Kinder: `0` heisst "irgendeines" und passt auf alles ...
        assert!(weck_passt(Kind(0), Kind(7)));
        assert!(weck_passt(Kind(7), Kind(7)));
        // ... aber nicht andersherum, und nicht auf ein fremdes Kind.
        assert!(!weck_passt(Kind(7), Kind(9)));
        assert!(!weck_passt(Kind(7), Kind(0)));

        // ZEIT wird NIE angestossen — sie gehört allein dem Timer. Ein
        // Schläfer darf durch keinen Weckruf vor seiner Zeit hochkommen.
        assert!(!weck_passt(Zeit, Zeit));
        assert!(!weck_passt(Zeit, PipeLesen(0)));
        assert!(!weck_passt(PipeLesen(0), Zeit));
        assert!(!weck_passt(Zeit, Kind(0)));
    }

    /// Der Reschedule-Punkt benutzt DIESELBE Auswahl wie `yield` — daraus
    /// folgt, dass ein Ping-Pong-Paar niemanden aushungern KANN.
    ///
    /// Nachgerechnet an genau der Konstellation aus dem Entwurfs-Kommentar:
    /// A (Slot 1) und B (Slot 2) wecken sich abwechselnd, C (Slot 3) tut
    /// nichts als lauffähig zu sein. Bei einer direkten Übergabe an den
    /// Geweckten käme C nie dran — über die zyklische Suche kommt er in
    /// jeder Runde dran.
    #[test_case]
    fn test_pingpong_hungert_niemanden_aus() {
        use Zustand::*;
        let mut bild = [
            Some(Lauffaehig), // 0 Kernel
            Some(Laeuft),     // 1 = A
            Some(Lauffaehig), // 2 = B
            Some(Lauffaehig), // 3 = C
            None,
            None,
            None,
            None,
        ];
        let mut aktuell = 1usize;
        let mut zaehler = [0usize; MAX_PROZESSE];
        const RUNDEN: usize = 200;
        for _ in 0..RUNDEN {
            // Jeder Schritt ist ein weck-getriebener Reschedule: freiwillig,
            // Zeitscheibe NICHT abgelaufen — genau die Argumente, mit denen
            // `umplanen_am_syscall_ende` fragt.
            let ziel = wechsel_entscheiden(&bild, aktuell, false, true)
                .expect("bei mehreren Lauffaehigen muss ein Ziel existieren");
            bild[aktuell] = Some(Lauffaehig);
            bild[ziel] = Some(Laeuft);
            aktuell = ziel;
            zaehler[ziel] += 1;
        }
        // C (Slot 3) bekommt GENAU seinen Viertel-Anteil — nicht "wenigstens
        // etwas", sondern denselben wie A und B.
        let erwartet = RUNDEN / 4;
        for slot in [0usize, 1, 2, 3] {
            assert_eq!(
                zaehler[slot], erwartet,
                "Slot {} kam {}x dran statt {}x — das Ping-Pong-Paar hungert aus",
                slot, zaehler[slot], erwartet
            );
        }
    }

    /// Die Fairness-Bremse ist eine RATE, kein Vorrat: Sie deckelt den
    /// Umschalt-Aufwand je Tick auf einen vernachlässigbaren Anteil.
    #[test_case]
    fn test_fairness_budget_rechnung() {
        // 16 Wechsel à ~450 ns je 4-ms-Tick = 7,2 us = 0,18 % CPU.
        assert_eq!(SOFORT_MAX_PRO_TICK, 16);
        let ns_je_tick = SOFORT_MAX_PRO_TICK as u64 * 450;
        let tick_ns = crate::zeit::ms_von_ticks(1) * 1_000_000;
        assert!(
            ns_je_tick * 200 < tick_ns,
            "das Budget darf nie mehr als 0,5 % eines Ticks kosten ({} von {} ns)",
            ns_je_tick,
            tick_ns
        );
        // Und ein produktiver Datenstrom braucht nur EINEN Weckruf je
        // Pipe-Füllung — er kann das Budget gar nicht ausschöpfen. Rechnung:
        // Budget x Kapazität x Ticks/s wären hier > 200 MiB/s, mehr als der
        // Ringpuffer selbst schafft.
        let obergrenze = SOFORT_MAX_PRO_TICK as u64
            * crate::pipe::STANDARD_KAPAZITAET as u64
            * (1000 / crate::zeit::ms_von_ticks(1));
        assert!(
            obergrenze > 200 * 1024 * 1024,
            "das Budget bremst schon produktive Stroeme aus ({} B/s)",
            obergrenze
        );
    }

    /// Spur-Auswertung: die Zahlen, mit denen der Präemptions-Beweis
    /// argumentiert.
    #[test_case]
    fn test_spur_auswerten() {
        // Nicht verschränkt: erst alles von 1, dann alles von 2 -> 1 Wechsel.
        let getrennt = spur_auswerten(&[1, 1, 1, 2, 2, 2]);
        assert_eq!(getrennt.gesamt, 6);
        assert_eq!(getrennt.beteiligte, 2);
        assert_eq!(getrennt.wechsel, 1);
        // Verschränkt: mehrfach hin und her.
        let verschraenkt = spur_auswerten(&[1, 2, 1, 2, 1]);
        assert_eq!(verschraenkt.wechsel, 4);
        assert_eq!(verschraenkt.beteiligte, 2);
        // Blockweise, aber mehrfach abwechselnd (der realistische Fall):
        let bloecke = spur_auswerten(&[1, 1, 2, 2, 1, 1, 2, 2]);
        assert_eq!(bloecke.wechsel, 3);
        // Nur einer: kein Wechsel.
        assert_eq!(spur_auswerten(&[1, 1, 1]).wechsel, 0);
        assert_eq!(spur_auswerten(&[1, 1, 1]).beteiligte, 1);
        // Leer: keine Panik, alles 0.
        assert_eq!(spur_auswerten(&[]), SpurBefund::default());
        // GROSSE PIDs: Eine PID ist KEIN Tabellen-Index — sie wächst monoton
        // und liegt schnell über MAX_PROZESSE. Genau daran ist die erste
        // Fassung gescheitert (sie zählte 0 Beteiligte).
        let gross = spur_auswerten(&[12, 13, 12, 13]);
        assert_eq!(gross.beteiligte, 2, "grosse PIDs muessen mitgezaehlt werden");
        assert_eq!(gross.wechsel, 3);
        // Mehr verschiedene PIDs als Plätze: gedeckelt, keine Panik.
        let viele: Vec<Pid> = (100..130).collect();
        assert_eq!(spur_auswerten(&viele).beteiligte, MAX_PROZESSE);
    }

    /// Der Spur-Ring selbst: melden, lesen, löschen — und der Überlauf
    /// verwirft, statt zu panicken.
    #[test_case]
    fn test_spur_ring() {
        spur_loeschen();
        assert!(spur_lesen().is_empty());
        spur_melden(1, b'a');
        spur_melden(2, b'7');
        assert_eq!(spur_lesen(), alloc::vec![(1u32, b'a'), (2u32, b'7')]);
        // Überlauf: mehr Meldungen als Plätze -> gedeckelt, keine Panik.
        spur_loeschen();
        for i in 0..(SPUR_LAENGE + 20) {
            spur_melden((i % 3) as Pid, b'x');
        }
        assert_eq!(spur_lesen().len(), SPUR_LAENGE);
        spur_loeschen();
    }
}
