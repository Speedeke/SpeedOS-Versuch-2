// shell/sitzung.rs — Terminal-Sitzungen: eine Shell pro Fenster
//
// Bisher gab es EINE Shell und EIN Terminal-Fenster. Jetzt wird die
// _print-Umleitung zum SITZUNGS-Konzept:
//
//   * Jedes Terminal-Fenster trägt eine Sitzungs-Id; pro Sitzung
//     läuft ein eigener Shell-Task (shell::sitzung_laufen).
//   * TASTEN: Der zentrale Eingabe-Router (shell::eingabe_router,
//     der einzige KeyStream-Leser) wirft Tasten in die Queue der
//     FOKUSSIERTEN Sitzung; deren Shell-Task wacht per Waker auf.
//   * AUSGABE: print!/println! sind global — welcher Sitzung gehört
//     die Ausgabe? Der Shell-Task setzt VOR seiner synchronen
//     Verarbeitung AUSGABE_SITZUNG und danach zurück. Das ist beim
//     kooperativen Multitasking korrekt: Zwischen setzen und
//     zurücksetzen liegt KEIN await, also läuft kein anderer Task
//     dazwischen. (Der Preis eines präemptiven Systems wäre hier
//     Task-Local-Storage — brauchen wir noch nicht.)
//   * KERNEL-LOG: Ausgaben OHNE gesetzte Ausgabe-Sitzung (Boot-Rest,
//     Hintergrund-Tasks) gehen an das designierte HAUPT-Terminal.
//     Ist keins offen, werden sie GEPUFFERT und beim nächsten
//     Terminal-Öffnen nachgereicht (seriell laufen sie sowieso mit).
//   * SCHLIESSEN: Das Fenster-X trägt die Sitzung aus — beendet-Flag
//     plus Weckruf; naechste_taste liefert dann None und der
//     Shell-Task endet sauber (der Executor räumt ihn aus).
//
// LOCK-REGEL: Der SITZUNGEN-Mutex ist ein BLATT-Lock (wie Ablage/
// Einstellungen) — er wird auch unter dem MANAGER-Lock genommen
// (terminal_oeffnen/schliessen laufen im Manager). Die Tasten-Queue
// ist lock-frei, der Waker atomar.

use crate::framebuffer::Farbe;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crossbeam_queue::ArrayQueue;
use futures_util::task::AtomicWaker;
use pc_keyboard::DecodedKey;
use spin::Mutex;

/// Wie viele Tasten eine Sitzung puffern kann, bevor Tipp-Ereignisse
/// verloren gehen (großzügig — die Shell arbeitet schnell ab).
const TASTEN_KAPAZITAET: usize = 64;

/// Eine Terminal-Sitzung: die Brücke zwischen Eingabe-Router und
/// ihrem Shell-Task.
pub struct Sitzung {
    tasten: ArrayQueue<DecodedKey>,
    waker: AtomicWaker,
    beendet: AtomicBool,
    /// Strg+C wurde gedrückt (siehe `abbruch_anfordern`).
    abbruch: AtomicBool,
}

impl Sitzung {
    /// Wurde die Sitzung beendet (Fenster geschlossen)?
    pub fn ist_beendet(&self) -> bool {
        self.beendet.load(Ordering::Acquire)
    }

    /// Nächste Taste SYNCHRON abholen (None = Queue leer) — nur für
    /// Tests; der Shell-Task wartet stattdessen asynchron
    /// (naechste_taste).
    #[cfg(test)]
    pub(crate) fn taste_abholen(&self) -> Option<DecodedKey> {
        self.tasten.pop()
    }
}

static SITZUNGEN: Mutex<BTreeMap<u64, Arc<Sitzung>>> = Mutex::new(BTreeMap::new());
/// Nächste zu vergebende Sitzungs-Id (0 = "keine Sitzung").
static NAECHSTE_ID: AtomicU64 = AtomicU64::new(1);
/// Das designierte Haupt-Terminal (Kernel-Log-Ziel); 0 = keins.
static HAUPT: AtomicU64 = AtomicU64::new(0);
/// Die Sitzung, der die AKTUELLE print!-Ausgabe gehört (0 = Kernel).
static AUSGABE: AtomicU64 = AtomicU64::new(0);

fn mit_sitzungen<T>(f: impl FnOnce(&mut BTreeMap<u64, Arc<Sitzung>>) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut SITZUNGEN.lock()))
}

/// Legt eine neue Sitzung an und liefert ihre Id.
pub fn neu_registrieren() -> u64 {
    let id = NAECHSTE_ID.fetch_add(1, Ordering::Relaxed);
    let sitzung = Arc::new(Sitzung {
        tasten: ArrayQueue::new(TASTEN_KAPAZITAET),
        waker: AtomicWaker::new(),
        beendet: AtomicBool::new(false),
        abbruch: AtomicBool::new(false),
    });
    mit_sitzungen(|sitzungen| {
        sitzungen.insert(id, sitzung);
    });
    id
}

/// Wie viele Sitzungen sind OFFEN?
///
/// Die Zahl ist die Probe darauf, dass Terminal-Fenster wirklich
/// aufräumen: Wer N Fenster öffnet und wieder schliesst, muss hinterher
/// bei derselben Zahl landen. Sichtbar ist sie nirgends — sie ist ein
/// Messpunkt, kein Zustand (`tests` und der Task-Manager).
pub fn anzahl() -> usize {
    mit_sitzungen(|sitzungen| sitzungen.len())
}

/// Holt die Sitzung zu einer Id (für den Shell-Task).
pub fn holen(id: u64) -> Option<Arc<Sitzung>> {
    mit_sitzungen(|sitzungen| sitzungen.get(&id).cloned())
}

/// Trägt eine Sitzung aus und beendet ihren Shell-Task: beendet-Flag
/// setzen und wecken — er wacht auf, bekommt None und kehrt zurück.
pub fn austragen(id: u64) {
    let sitzung = mit_sitzungen(|sitzungen| sitzungen.remove(&id));
    if let Some(sitzung) = sitzung {
        sitzung.beendet.store(true, Ordering::Release);
        sitzung.waker.wake();
    }
    // War das das Haupt-Terminal, gibt es vorerst keins mehr — der
    // Fenster-Manager bestimmt beim Schließen einen Nachfolger.
    let _ = HAUPT.compare_exchange(id, 0, Ordering::AcqRel, Ordering::Relaxed);
}

/// Wirft eine Taste in die Sitzungs-Queue (der Eingabe-Router).
pub fn taste_einwerfen(id: u64, taste: DecodedKey) {
    if let Some(sitzung) = holen(id) {
        let _ = sitzung.tasten.push(taste); // voll -> Taste verfällt
        sitzung.waker.wake();
    }
}

// ---------------------------------------------------------------------------
// STRG+C — der Abbruch-Wunsch (Serie 6, Teil 6)
// ---------------------------------------------------------------------------
//
// WARUM EIN EIGENES FLAG UND NICHT EINFACH EINE TASTE IN DER QUEUE:
//
// Solange die Shell auf ein Programm wartet, steckt sie MITTEN in einem
// synchronen Shell-Befehl (`befehl_ausfuehren`). Sie kommt in dieser Zeit
// nicht an ihre Tasten-Queue — die liest sie erst wieder, wenn der Befehl
// fertig ist. Ein Strg+C in der Queue käme also frühestens an, wenn es
// nichts mehr abzubrechen gibt.
//
// Deshalb greift der EINGABE-ROUTER Strg+C ab, bevor er routet, und setzt
// hier ein Atomic. Die Warteschleife des Vordergrund-Prozesses fragt es bei
// jedem Durchgang ab. Das ist die schlichteste Form eines Signals, die es
// gibt — und sie reicht genau für den einen Zweck, für den wir sie brauchen.
//
// Das Flag lebt in der SITZUNG, nicht global: Zwei Terminal-Fenster mit je
// einem laufenden Programm sollen sich nicht gegenseitig abschiessen.

/// Setzt den Abbruch-Wunsch einer Sitzung (Strg+C).
pub fn abbruch_anfordern(id: u64) {
    if let Some(sitzung) = holen(id) {
        sitzung.abbruch.store(true, Ordering::Release);
        // Auch wecken: Wartet die Sitzung gerade auf eine Taste (kein
        // Programm läuft), soll sie die neue Eingabezeile zeigen können.
        sitzung.waker.wake();
    }
}

/// Fragt den Abbruch-Wunsch ab und LÖSCHT ihn (einmalige Wirkung).
pub fn abbruch_abholen(id: u64) -> bool {
    match holen(id) {
        Some(sitzung) => sitzung.abbruch.swap(false, Ordering::AcqRel),
        None => false,
    }
}

/// Löscht einen alten Abbruch-Wunsch — VOR jedem Programmstart, damit ein
/// verspätetes Strg+C nicht das nächste Programm trifft.
pub fn abbruch_loeschen(id: u64) {
    if let Some(sitzung) = holen(id) {
        sitzung.abbruch.store(false, Ordering::Release);
    }
}

/// Wartet asynchron auf die nächste Taste dieser Sitzung.
/// None = die Sitzung wurde beendet (Fenster geschlossen).
pub async fn naechste_taste(sitzung: &Arc<Sitzung>) -> Option<DecodedKey> {
    core::future::poll_fn(|cx| {
        use core::task::Poll;
        if let Some(taste) = sitzung.tasten.pop() {
            return Poll::Ready(Some(taste));
        }
        if sitzung.beendet.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }
        // Waker registrieren, dann NOCHMAL prüfen — schließt die
        // Race, falls Taste/Beenden genau dazwischen kam (dasselbe
        // Muster wie beim Scancode-Stream).
        sitzung.waker.register(cx.waker());
        if let Some(taste) = sitzung.tasten.pop() {
            return Poll::Ready(Some(taste));
        }
        if sitzung.beendet.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

// ----- Haupt-Terminal und Ausgabe-Kontext -----

pub fn haupt() -> u64 {
    HAUPT.load(Ordering::Relaxed)
}

pub fn haupt_setzen(id: u64) {
    HAUPT.store(id, Ordering::Relaxed);
}

/// Setzt die Ausgabe-Sitzung für die FOLGENDE synchrone Verarbeitung
/// (nur der Shell-Task selbst — und NIE über ein await hinweg!).
pub fn ausgabe_setzen(id: u64) {
    AUSGABE.store(id, Ordering::Relaxed);
}

pub fn ausgabe_zuruecksetzen() {
    AUSGABE.store(0, Ordering::Relaxed);
}

/// Wohin geht die aktuelle print!-Ausgabe? Die gesetzte Ausgabe-
/// Sitzung — oder das Haupt-Terminal (Kernel-Log). 0 = nirgendwohin
/// (dann puffern, siehe log_puffern).
pub fn ausgabe_ziel() -> u64 {
    let ausgabe = AUSGABE.load(Ordering::Relaxed);
    if ausgabe != 0 {
        ausgabe
    } else {
        haupt()
    }
}

/// Ist die aktuelle Ausgabe Kernel-Log (keine Shell-Ausgabe)?
pub fn ausgabe_ist_kernel_log() -> bool {
    AUSGABE.load(Ordering::Relaxed) == 0
}

// ----- Kernel-Log-Puffer (kein Terminal offen) -----

/// Gepufferte Log-Segmente (Text + Farben), gedeckelt.
static LOG_PUFFER: Mutex<Vec<(String, Farbe, Farbe)>> = Mutex::new(Vec::new());
const LOG_PUFFER_MAX: usize = 256;

/// Puffert ein Log-Segment, bis wieder ein Terminal offen ist.
/// Läuft der Puffer über, fällt das ÄLTESTE weg (seriell ist ohnehin
/// alles protokolliert — der Puffer ist nur Komfort).
pub fn log_puffern(text: String, vg: Farbe, hg: Farbe) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut puffer = LOG_PUFFER.lock();
        if puffer.len() >= LOG_PUFFER_MAX {
            puffer.remove(0);
        }
        puffer.push((text, vg, hg));
    });
}

/// Holt alle gepufferten Log-Segmente ab (beim Terminal-Öffnen).
pub fn log_abholen() -> Vec<(String, Farbe, Farbe)> {
    x86_64::instructions::interrupts::without_interrupts(|| core::mem::take(&mut *LOG_PUFFER.lock()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Sitzungs-Lebenszyklus: registrieren, Tasten einwerfen und
    /// abholen (FIFO), beenden setzt das Flag und leert die Registry.
    #[test_case]
    fn test_sitzung_lebenszyklus() {
        let id = neu_registrieren();
        let sitzung = holen(id).expect("Sitzung fehlt nach registrieren");

        taste_einwerfen(id, DecodedKey::Unicode('a'));
        taste_einwerfen(id, DecodedKey::Unicode('b'));
        assert_eq!(sitzung.tasten.pop(), Some(DecodedKey::Unicode('a')));
        assert_eq!(sitzung.tasten.pop(), Some(DecodedKey::Unicode('b')));
        assert_eq!(sitzung.tasten.pop(), None);

        assert!(!sitzung.beendet.load(Ordering::Acquire));
        austragen(id);
        assert!(sitzung.beendet.load(Ordering::Acquire));
        assert!(holen(id).is_none());
        // Tasten an eine tote Sitzung verpuffen (kein Panic):
        taste_einwerfen(id, DecodedKey::Unicode('x'));
    }

    /// Ausgabe-Ziel: gesetzte Sitzung schlägt das Haupt-Terminal;
    /// zurückgesetzt gilt wieder das Haupt (Kernel-Log).
    #[test_case]
    fn test_ausgabe_ziel_und_haupt() {
        let haupt_vorher = haupt();
        haupt_setzen(7);
        assert_eq!(ausgabe_ziel(), 7);
        assert!(ausgabe_ist_kernel_log());
        ausgabe_setzen(9);
        assert_eq!(ausgabe_ziel(), 9);
        assert!(!ausgabe_ist_kernel_log());
        ausgabe_zuruecksetzen();
        assert_eq!(ausgabe_ziel(), 7);
        // austragen des Haupts setzt es auf 0 zurück:
        haupt_setzen(7);
        let _ = HAUPT.compare_exchange(7, 7, Ordering::AcqRel, Ordering::Relaxed);
        austragen(7); // (nie registriert — nur der Haupt-Reset zählt)
        assert_eq!(haupt(), 0);
        haupt_setzen(haupt_vorher);
    }

    /// Der Log-Puffer sammelt Segmente und leert sich beim Abholen;
    /// der Deckel wirft das Älteste raus statt zu wachsen.
    #[test_case]
    fn test_log_puffer() {
        let _ = log_abholen(); // sauber starten
        let farbe = Farbe::neu(1, 2, 3);
        log_puffern(alloc::string::String::from("eins"), farbe, farbe);
        log_puffern(alloc::string::String::from("zwei"), farbe, farbe);
        let segmente = log_abholen();
        assert_eq!(segmente.len(), 2);
        assert_eq!(segmente[0].0, "eins");
        assert!(log_abholen().is_empty());

        for i in 0..LOG_PUFFER_MAX + 10 {
            log_puffern(alloc::format!("{}", i), farbe, farbe);
        }
        let voll = log_abholen();
        assert_eq!(voll.len(), LOG_PUFFER_MAX);
        // Das Älteste (0..9) ist rausgefallen:
        assert_eq!(voll[0].0, "10");
    }
}
