// task/uebersicht.rs — Die Task-Übersicht: Namen, Zähler, CPU-Auslastung
//
// Der Executor besitzt seine Tasks exklusiv (BTreeMap in run()) —
// eine App kann ihn nicht befragen. Deshalb führt dieses Modul eine
// globale REGISTRY als Schatten-Buchhaltung: Der Executor trägt beim
// Spawnen jeden Task mit Name/Art/Startzeit ein und beim Ende wieder
// aus; die heißen Zähler (Polls, Wecken, wach/schläft) liegen als
// ATOMICS in einem Arc, das sich Executor, Waker und Registry teilen —
// der Waker feuert aus Interrupt-Handlern und darf NIE einen Lock
// nehmen.
//
// LOCK-REGEL: Der REGISTRY-Mutex ist ein BLATT-Lock (wie Ablage und
// Einstellungen) — die Task-Manager-App darf ihre Momentaufnahme
// unter dem MANAGER-Lock ziehen. Der Executor nimmt ihn nur in
// spawn/austragen (nie im Interrupt-Kontext).
//
// TASK BEENDEN (kooperativ — ehrlich dokumentiert): Wir können einen
// Task nicht "abschießen" wie ein präemptives OS. beenden_anfordern()
// setzt nur ein Flag; der Executor LÄSST den Task in seiner nächsten
// Runde FALLEN (Drop der Future). Ein suspendierter Task steht per
// Definition an einem await-Punkt — sein Drop räumt sauber auf
// (z. B. gibt NaechsterTick seinen Waker-Slot zurück). Nur als
// beendbar markierte Tasks (Demos) dürfen das; Kernel-Tasks sind
// geschützt.
//
// CPU-AUSLASTUNG: Der Executor misst per TSC, wie viel Zeit er in
// run_ready_tasks (Arbeit) vs. im hlt-Schlaf (Ruhe) verbringt, und
// verbucht beides in einem GLEITENDEN FENSTER aus 10 Eimern à 100 ms
// (~1 s) — Prozent = Arbeit/Gesamt über das Fenster. Die Fenster-
// Logik ist eine reine, unit-getestete Struktur ohne Globals.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

/// Grobe Einordnung eines Tasks — die Task-Manager-App zeigt dafür
/// Icons und schützt Nicht-Demos vor dem Beenden-Knopf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskArt {
    /// Kernel-/Treiber-Task (Maus, Cursor, ...) — geschützt.
    Kernel,
    /// Desktop-/Fenster-Task (Compositor, Terminal-Shell, ...) —
    /// geschützt, aber als Fenster-Welt erkennbar.
    Fenster,
    /// Wegwerf-Task (Demo-Zähler) — beendbar.
    Demo,
}

/// Die heißen Zähler eines Tasks — Atomics, weil der WAKER sie aus
/// Interrupt-Handlern erhöht (dort sind Locks verboten).
#[derive(Default)]
pub struct TaskZaehler {
    /// Wie oft wurde der Task gepollt? (grobes Aktivitätsmaß)
    pub polls: AtomicU64,
    /// Wie oft wurde er geweckt?
    pub wecken: AtomicU64,
    /// true = geweckt und wartet aufs Pollen ("wachend"),
    /// false = schläft, bis sein Waker feuert.
    pub wach: AtomicBool,
    /// Beenden angefordert (Task-Manager) — der Executor prüft es
    /// vor dem nächsten Poll und lässt den Task dann fallen.
    pub beenden: AtomicBool,
}

/// Der Registry-Eintrag eines lebenden Tasks.
struct TaskInfo {
    name: String,
    art: TaskArt,
    beendbar: bool,
    start_us: u64,
    zaehler: Arc<TaskZaehler>,
}

/// Eine Zeile der Momentaufnahme — reine Daten, keine Referenzen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMoment {
    pub id: u64,
    pub name: String,
    pub art: TaskArt,
    pub beendbar: bool,
    /// Zeit seit dem Spawn in µs.
    pub laufzeit_us: u64,
    /// true = wachend (geweckt, wartet aufs Pollen), false = schläft.
    pub wach: bool,
    pub polls: u64,
    pub wecken: u64,
}

static REGISTRY: Mutex<BTreeMap<u64, TaskInfo>> = Mutex::new(BTreeMap::new());
/// Schnellpfad: Der Executor durchsucht die Registry nur nach
/// Beenden-Kandidaten, wenn überhaupt eine Anforderung vorliegt.
static BEENDEN_ANGEFORDERT: AtomicBool = AtomicBool::new(false);

/// Blatt-Lock-Zugriff (Interrupts aus) — Closure nimmt keine Locks.
fn mit_registry<T>(f: impl FnOnce(&mut BTreeMap<u64, TaskInfo>) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut REGISTRY.lock()))
}

/// Trägt einen Task ein (ruft der Executor beim Spawnen).
pub(crate) fn registrieren(
    id: u64,
    name: String,
    art: TaskArt,
    beendbar: bool,
    zaehler: Arc<TaskZaehler>,
) {
    let info = TaskInfo { name, art, beendbar, start_us: crate::zeit::us_seit_boot(), zaehler };
    mit_registry(|registry| {
        registry.insert(id, info);
    });
}

/// Trägt einen Task aus (fertig oder beendet).
pub(crate) fn austragen(id: u64) {
    mit_registry(|registry| {
        registry.remove(&id);
    });
}

/// Anzahl der lebenden (registrierten) Tasks.
pub fn anzahl() -> usize {
    mit_registry(|registry| registry.len())
}

/// DIE Momentaufnahme für den Task-Manager: alle lebenden Tasks mit
/// Laufzeit und Zählerständen, nach Id sortiert (BTreeMap-Ordnung —
/// die Liste springt beim Aktualisieren nicht durcheinander).
pub fn momentaufnahme() -> Vec<TaskMoment> {
    let jetzt_us = crate::zeit::us_seit_boot();
    mit_registry(|registry| {
        registry
            .iter()
            .map(|(id, info)| TaskMoment {
                id: *id,
                name: info.name.clone(),
                art: info.art,
                beendbar: info.beendbar,
                laufzeit_us: jetzt_us.saturating_sub(info.start_us),
                wach: info.zaehler.wach.load(Ordering::Relaxed),
                polls: info.zaehler.polls.load(Ordering::Relaxed),
                wecken: info.zaehler.wecken.load(Ordering::Relaxed),
            })
            .collect()
    })
}

/// Fordert das Beenden eines Tasks an. true = angenommen (Task ist
/// beendbar), false = geschützt oder unbekannt. Der Task endet beim
/// nächsten Executor-Durchlauf (siehe Kopfkommentar — kooperativ!).
pub fn beenden_anfordern(id: u64) -> bool {
    let angenommen = mit_registry(|registry| match registry.get(&id) {
        Some(info) if info.beendbar => {
            info.zaehler.beenden.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    });
    if angenommen {
        BEENDEN_ANGEFORDERT.store(true, Ordering::Release);
    }
    angenommen
}

/// Holt die Ids aller Tasks mit Beenden-Anforderung ab (Executor,
/// einmal pro Runde — dank Flag fast immer der billige Leer-Pfad).
pub(crate) fn beenden_abholen() -> Vec<u64> {
    if !BEENDEN_ANGEFORDERT.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    mit_registry(|registry| {
        registry
            .iter()
            .filter(|(_, info)| info.beendbar && info.zaehler.beenden.load(Ordering::Relaxed))
            .map(|(id, _)| *id)
            .collect()
    })
}

// ---------------------------------------------------------------------------
// CPU-Auslastung: gleitendes Fenster aus Arbeit/Ruhe-Eimern
// ---------------------------------------------------------------------------

/// Eimer-Raster des Gleitfensters: 10 Eimer à 100 ms ≈ 1 Sekunde.
pub const EIMER_ANZAHL: usize = 10;
pub const EIMER_MS: u64 = 100;

/// Das gleitende Auslastungs-Fenster — reine Logik ohne Globals
/// (deshalb mit synthetischen Zahlen unit-testbar). Jeder Eimer
/// sammelt (arbeit_us, gesamt_us) einer 100-ms-Periode; prozent()
/// mittelt über alle Eimer des Fensters.
pub struct GleitFenster {
    eimer: [(u64, u64); EIMER_ANZAHL],
    /// Perioden-Nummer (jetzt_ms / EIMER_MS) des zuletzt befüllten
    /// Eimers — beim Fortschreiten werden übersprungene Eimer geleert.
    periode: u64,
}

impl GleitFenster {
    pub const fn neu() -> Self {
        GleitFenster { eimer: [(0, 0); EIMER_ANZAHL], periode: 0 }
    }

    /// Verbucht gemessene Arbeits- und Ruhe-Zeit zur Zeit `jetzt_ms`.
    pub fn verbuchen(&mut self, jetzt_ms: u64, arbeit_us: u64, ruhe_us: u64) {
        let periode = jetzt_ms / EIMER_MS;
        if periode != self.periode {
            // Alle Perioden zwischen damals und jetzt leeren — auch
            // bei großen Sprüngen (mehr als ein voller Umlauf =
            // komplett leeren, gedeckelt auf EIMER_ANZAHL Schritte).
            let schritte = periode.saturating_sub(self.periode).min(EIMER_ANZAHL as u64);
            for schritt in 1..=schritte {
                let index = ((self.periode + schritt) % EIMER_ANZAHL as u64) as usize;
                self.eimer[index] = (0, 0);
            }
            self.periode = periode;
        }
        let eimer = &mut self.eimer[(periode % EIMER_ANZAHL as u64) as usize];
        eimer.0 += arbeit_us;
        eimer.1 += arbeit_us + ruhe_us;
    }

    /// Auslastung über das ganze Fenster in Prozent (0..=100).
    pub fn prozent(&self) -> u32 {
        let arbeit: u64 = self.eimer.iter().map(|(a, _)| a).sum();
        let gesamt: u64 = self.eimer.iter().map(|(_, g)| g).sum();
        if gesamt == 0 {
            return 0;
        }
        ((arbeit * 100 / gesamt) as u32).min(100)
    }
}

static AUSLASTUNG: Mutex<GleitFenster> = Mutex::new(GleitFenster::neu());

/// Verbucht Executor-Messwerte (ruft NUR der Executor, pro Runde).
pub(crate) fn cpu_verbuchen(arbeit_us: u64, ruhe_us: u64) {
    let jetzt_ms = crate::zeit::ms_seit_boot();
    x86_64::instructions::interrupts::without_interrupts(|| {
        AUSLASTUNG.lock().verbuchen(jetzt_ms, arbeit_us, ruhe_us);
    });
}

/// Die aktuelle System-Auslastung in Prozent (Fenster ~1 s).
pub fn cpu_auslastung_prozent() -> u32 {
    x86_64::instructions::interrupts::without_interrupts(|| AUSLASTUNG.lock().prozent())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// Momentaufnahme-Konsistenz: Einträge erscheinen mit Namen und
    /// Zählern, die Anzahl stimmt, Austragen räumt auf — und die
    /// Beenden-Anforderung greift NUR bei beendbaren Tasks.
    #[test_case]
    fn test_momentaufnahme_konsistent() {
        // Hohe Test-Ids, um nie mit echten Tasks zu kollidieren
        // (die Lib-Tests laufen ohne Executor, aber sicher ist sicher).
        let basis = 1_000_000;
        let vorher = anzahl();

        let kernel = Arc::new(TaskZaehler::default());
        let demo = Arc::new(TaskZaehler::default());
        registrieren(basis, "Test-Kernel".to_string(), TaskArt::Kernel, false, kernel.clone());
        registrieren(basis + 1, "Test-Demo".to_string(), TaskArt::Demo, true, demo.clone());
        kernel.polls.store(7, Ordering::Relaxed);
        kernel.wecken.store(9, Ordering::Relaxed);
        demo.wach.store(true, Ordering::Relaxed);

        assert_eq!(anzahl(), vorher + 2);
        let moment = momentaufnahme();
        let k = moment.iter().find(|m| m.id == basis).expect("Kernel-Task fehlt");
        assert_eq!(k.name, "Test-Kernel");
        assert_eq!((k.polls, k.wecken, k.wach, k.beendbar), (7, 9, false, false));
        let d = moment.iter().find(|m| m.id == basis + 1).expect("Demo-Task fehlt");
        assert!(d.wach && d.beendbar && d.art == TaskArt::Demo);
        // Die Momentaufnahme ist nach Id sortiert (stabile Anzeige):
        assert!(moment.windows(2).all(|paar| paar[0].id < paar[1].id));

        // Beenden: geschützter Task lehnt ab, Demo nimmt an und
        // taucht in der Abhol-Liste auf (die sich danach leert):
        assert!(!beenden_anfordern(basis));
        assert!(!kernel.beenden.load(Ordering::Relaxed));
        assert!(beenden_anfordern(basis + 1));
        assert_eq!(beenden_abholen(), alloc::vec![basis + 1]);
        // Flag ist verbraucht — ohne neue Anforderung kommt nichts:
        assert!(beenden_abholen().is_empty());

        austragen(basis);
        austragen(basis + 1);
        assert_eq!(anzahl(), vorher);
    }

    /// Auslastungs-Rechnung mit synthetischen Zahlen: Anteile,
    /// Fenster-Rotation und das Leeren nach langen Pausen.
    #[test_case]
    fn test_gleitfenster_auslastung() {
        let mut fenster = GleitFenster::neu();
        assert_eq!(fenster.prozent(), 0); // leer -> 0, keine Division durch 0

        // Eine Periode mit 30 % Arbeit:
        fenster.verbuchen(0, 30_000, 70_000);
        assert_eq!(fenster.prozent(), 30);

        // Zweite Periode voll ausgelastet -> Mittel über beide = 65 %:
        fenster.verbuchen(100, 100_000, 0);
        assert_eq!(fenster.prozent(), 65);

        // Nach einem vollen Fenster-Umlauf ist die alte Last raus:
        // 10 leere Perioden später zählt nur noch der neue Eimer.
        fenster.verbuchen(100 + EIMER_ANZAHL as u64 * EIMER_MS, 10_000, 90_000);
        assert_eq!(fenster.prozent(), 10);

        // Riesiger Zeitsprung (>> Fenster): alles geleert, dann 100 %.
        fenster.verbuchen(1_000_000_000, 50_000, 0);
        assert_eq!(fenster.prozent(), 100);

        // Deckel: krumme Messwerte können nie über 100 % melden.
        let mut voll = GleitFenster::neu();
        voll.verbuchen(0, 100, 0);
        assert_eq!(voll.prozent(), 100);
    }
}
