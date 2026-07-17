// taskmanager.rs — Der Task-Manager: Fenster in die Task-Übersicht
//
// App Nummer drei auf dem UI-Toolkit. Sie zeigt, was der Executor
// über seine Tasks weiß (task/uebersicht.rs):
//   * Kopf: CPU-Auslastung in Prozent (TSC-Messung Arbeit vs. hlt,
//     ~1-s-Gleitfenster) mit Live-Graph der letzten 60 Sekunden,
//     daneben Heap-Belegung und Task-Zahl.
//   * Tabelle: Id, Name, Art (Kernel/Fenster/Demo), Laufzeit,
//     Polls/s (grobes Aktivitätsmaß) und Status wachend/schläft.
//   * "Task beenden" wirkt NUR auf beendbare Tasks (Demos) —
//     Kernel-Tasks sind geschützt, der Knopf dann gedimmt.
//     EHRLICHE GRENZE (steht auch in der App): Kooperative Tasks
//     kann niemand abschießen — der Executor lässt den Task beim
//     nächsten Durchlauf an seinem await-Punkt fallen (Drop).
//   * "Demo-Task starten" spawnt einen beendbaren Zähler-Task —
//     damit lässt sich das Beenden gefahrlos ausprobieren.
//
// Aktualisierung 1x pro Sekunde über App::tick (wie die
// Einstellungen-Info-Seite); die Auswahl überlebt das Neu-Laden,
// weil sie als Task-ID gemerkt wird, nicht als Zeilen-Index.

use crate::grafik::{Icon, Rechteck, Zeichner};
use crate::task::uebersicht::{self, TaskArt, TaskMoment};
use crate::theme::{self, metrik};
use crate::ui::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::ui::{hbox, vbox, App, AppReaktion, Fueller, UiEreignis, UiReaktion, Widget};
use crate::zeit;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

// ----- Nachricht-IDs -----
const N_BEENDEN: u32 = 1;
const N_DEMO_STARTEN: u32 = 2;
const N_LISTE: u32 = 1000; // + Zeilen-Index (Auswahl und Doppelklick)

/// Wie viele Sekunden CPU-Geschichte der Graph zeigt.
const GRAPH_SEKUNDEN: usize = 60;

// ---------------------------------------------------------------------------
// Reine, unit-getestete Bausteine
// ---------------------------------------------------------------------------

/// Rechnet eine Messreihe (0..=100 %) in Pixel-Punkte für den
/// Linien-Graphen um. Downsampling: Gibt es mehr Werte als Spalten,
/// bekommt jede Spalte das MAXIMUM ihres Wertebereichs (Spitzen
/// sollen sichtbar bleiben, nicht im Mittel verschwinden).
pub fn graph_punkte(werte: &[u8], breite: i32, hoehe: i32) -> Vec<(i32, i32)> {
    if werte.is_empty() || breite < 1 || hoehe < 2 {
        return Vec::new();
    }
    let spalten = (werte.len() as i32).min(breite) as usize;
    (0..spalten)
        .map(|spalte| {
            // Wertebereich [von, bis) dieser Spalte — kumulativ
            // gerechnet, damit die Ränder exakt aufgehen:
            let von = spalte * werte.len() / spalten;
            let bis = ((spalte + 1) * werte.len() / spalten).max(von + 1);
            let wert = werte[von..bis].iter().copied().max().unwrap_or(0).min(100);
            let x = if spalten == 1 {
                0
            } else {
                spalte as i32 * (breite - 1) / (spalten as i32 - 1)
            };
            let y = (hoehe - 1) - wert as i32 * (hoehe - 1) / 100;
            (x, y)
        })
        .collect()
}

/// Laufzeit lesbar: "mm:ss", ab einer Stunde "h:mm:ss".
pub fn laufzeit_text(us: u64) -> String {
    let sekunden = us / 1_000_000;
    if sekunden >= 3600 {
        format!("{}:{:02}:{:02}", sekunden / 3600, (sekunden / 60) % 60, sekunden % 60)
    } else {
        format!("{:02}:{:02}", sekunden / 60, sekunden % 60)
    }
}

// ---------------------------------------------------------------------------
// Der CPU-Graph als eigenes Widget
// ---------------------------------------------------------------------------

struct CpuGraph {
    /// Kopie des Verlaufs (die App baut den Baum pro Tick neu).
    werte: Vec<u8>,
}

impl Widget for CpuGraph {
    fn wunschgroesse(&self) -> (i32, i32) {
        // Breite skaliert mit (60 Sekunden sollen erkennbar bleiben).
        (GRAPH_SEKUNDEN as i32 * 3 * theme::skala_halbe() / 2, 4 * metrik().zeilen_hoehe)
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, crate::fenster::FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        z.rechteck_fuellen(bereich, thema.eingabefeld);
        z.rechteck_rahmen(bereich, thema.rahmen_passiv);
        // 50-%-Hilfslinie:
        let mitte = bereich.y + bereich.hoehe / 2;
        z.linie(bereich.x + 1, mitte, bereich.x + bereich.breite - 2, mitte, thema.leiste_knopf);

        // Der Verlauf als Linienzug (innen, 2 px Rand):
        let innen_breite = bereich.breite - 4;
        let innen_hoehe = bereich.hoehe - 4;
        let punkte = graph_punkte(&self.werte, innen_breite, innen_hoehe);
        for paar in punkte.windows(2) {
            let (x1, y1) = paar[0];
            let (x2, y2) = paar[1];
            z.linie(
                bereich.x + 2 + x1,
                bereich.y + 2 + y1,
                bereich.x + 2 + x2,
                bereich.y + 2 + y2,
                thema.akzent,
            );
        }
    }

    fn ereignis(&mut self, _e: &UiEreignis, _b: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Die App
// ---------------------------------------------------------------------------

/// Laufende Nummer für Demo-Tasks (eindeutige Namen).
static DEMO_NUMMER: AtomicU64 = AtomicU64::new(1);

/// Der beendbare Demo-Task: zählt sekündlich ins serielle Log —
/// harmlos, unendlich, und genau dafür da, beendet zu werden.
async fn demo_zaehler(nummer: u64) {
    let mut runde = 0u64;
    loop {
        zeit::warte_ms(1000).await;
        runde += 1;
        crate::serial_println!("[DEMO {}] Runde {}", nummer, runde);
    }
}

pub struct TaskManagerApp {
    /// Letzte Momentaufnahme (nach Id sortiert — siehe uebersicht).
    tasks: Vec<TaskMoment>,
    /// Poll-Stände der VORIGEN Aufnahme -> Polls/s als Differenz.
    letzte_polls: BTreeMap<u64, u64>,
    letzte_aufnahme_us: u64,
    polls_pro_s: BTreeMap<u64, u64>,
    /// CPU-Prozent-Verlauf der letzten GRAPH_SEKUNDEN Sekunden.
    cpu_verlauf: Vec<u8>,
    cpu_prozent: u32,
    heap: Option<(usize, usize)>,
    /// Auswahl als Task-ID (nicht Index — überlebt Neu-Aufnahmen).
    auswahl_id: Option<u64>,
    letzte_sekunde: u64,
}

impl TaskManagerApp {
    pub fn neu() -> Self {
        let mut app = TaskManagerApp {
            tasks: Vec::new(),
            letzte_polls: BTreeMap::new(),
            letzte_aufnahme_us: 0,
            polls_pro_s: BTreeMap::new(),
            cpu_verlauf: Vec::new(),
            cpu_prozent: 0,
            heap: None,
            auswahl_id: None,
            letzte_sekunde: zeit::ms_seit_boot() / 1000,
        };
        app.aktualisieren();
        app
    }

    /// Zieht eine frische Momentaufnahme und leitet die Anzeigedaten
    /// ab (Polls/s, CPU-Verlauf, Heap). Läuft unter dem MANAGER-Lock —
    /// alles Blatt-Locks bzw. Atomics, das ist erlaubt.
    fn aktualisieren(&mut self) {
        let jetzt_us = zeit::us_seit_boot();
        self.tasks = uebersicht::momentaufnahme();

        // Polls/s: Differenz zur letzten Aufnahme, auf Sekunden
        // normiert (der Tick kommt nur UNGEFÄHR sekündlich).
        let delta_us = jetzt_us.saturating_sub(self.letzte_aufnahme_us).max(1);
        self.polls_pro_s = self
            .tasks
            .iter()
            .map(|task| {
                let vorher = self.letzte_polls.get(&task.id).copied().unwrap_or(task.polls);
                let rate = task.polls.saturating_sub(vorher) * 1_000_000 / delta_us;
                (task.id, rate)
            })
            .collect();
        self.letzte_polls = self.tasks.iter().map(|t| (t.id, t.polls)).collect();
        self.letzte_aufnahme_us = jetzt_us;

        // CPU + Graph-Verlauf (auf GRAPH_SEKUNDEN gedeckelt):
        self.cpu_prozent = uebersicht::cpu_auslastung_prozent();
        self.cpu_verlauf.push(self.cpu_prozent.min(100) as u8);
        if self.cpu_verlauf.len() > GRAPH_SEKUNDEN {
            let ueberhang = self.cpu_verlauf.len() - GRAPH_SEKUNDEN;
            self.cpu_verlauf.drain(..ueberhang);
        }

        self.heap = crate::allocator::heap_statistik();

        // Auswahl bereinigen, falls der Task verschwunden ist:
        if let Some(id) = self.auswahl_id {
            if !self.tasks.iter().any(|task| task.id == id) {
                self.auswahl_id = None;
            }
        }
    }

    fn auswahl_task(&self) -> Option<&TaskMoment> {
        self.tasks.iter().find(|task| Some(task.id) == self.auswahl_id)
    }

    fn art_text(art: TaskArt) -> &'static str {
        match art {
            TaskArt::Kernel => "Kernel",
            TaskArt::Fenster => "Fenster",
            TaskArt::Demo => "Demo",
        }
    }
}

impl App for TaskManagerApp {
    fn name(&self) -> &'static str {
        "Task-Manager"
    }

    fn icon(&self) -> &'static Icon {
        &crate::grafik::ICON_TASKS
    }

    fn aufbau(&self) -> Box<dyn Widget> {
        // --- Kopf: CPU/Heap/Tasks links, Live-Graph rechts ---
        // heap_statistik liefert (belegt, frei) — gesamt ist die Summe.
        let (heap_belegt, heap_frei) = self.heap.unwrap_or((0, 0));
        let kopf_text = format!(
            "CPU:   {:>3} %\nHeap:  {} von {}\nTasks: {}",
            self.cpu_prozent,
            crate::explorer::groesse_formatieren(heap_belegt),
            crate::explorer::groesse_formatieren(heap_belegt + heap_frei),
            self.tasks.len(),
        );
        let kopf = hbox(vec![
            Box::new(Label::neu(&kopf_text)) as Box<dyn Widget>,
            Box::new(Fueller),
            Box::new(CpuGraph { werte: self.cpu_verlauf.clone() }),
        ]);

        // --- Tabelle: Kopfzeile + eine Zeile pro Task ---
        // (Monospace-Font: Spalten entstehen durch feste Breiten.)
        let kopfzeile = format!(
            " {:>3}  {:<22} {:<7} {:>8} {:>8}  {}",
            "Id", "Name", "Art", "Zeit", "Polls/s", "Status"
        );
        let auswahl_index = self.tasks.iter().position(|t| Some(t.id) == self.auswahl_id);
        let eintraege = self
            .tasks
            .iter()
            .map(|task| {
                let name: String = task.name.chars().take(22).collect();
                ListenEintrag {
                    icon: None,
                    text: format!(
                        "{:>3}  {:<22} {:<7} {:>8} {:>8}  {}",
                        task.id,
                        name,
                        Self::art_text(task.art),
                        laufzeit_text(task.laufzeit_us),
                        self.polls_pro_s.get(&task.id).copied().unwrap_or(0),
                        if task.wach { "wach" } else { "schlaeft" },
                    ),
                }
            })
            .collect();
        let liste = ScrollListe::mit_index_nachrichten(eintraege, N_LISTE, N_LISTE)
            .mit_auswahl(auswahl_index)
            .mit_fokus(true);

        // --- Fußzeile: Aktionen + die ehrliche Kooperativ-Zeile ---
        let beenden_erlaubt = self.auswahl_task().map(|t| t.beendbar).unwrap_or(false);
        let aktionen = hbox(vec![
            Box::new(Button::neu("Task beenden", N_BEENDEN).mit_deaktiviert(!beenden_erlaubt))
                as Box<dyn Widget>,
            Box::new(Button::neu("Demo-Task starten", N_DEMO_STARTEN)),
            Box::new(Fueller),
        ]);

        Box::new(vbox(vec![
            Box::new(kopf) as Box<dyn Widget>,
            Box::new(Trennlinie),
            Box::new(Label::sekundaer(&kopfzeile)),
            Box::new(liste),
            Box::new(aktionen),
            Box::new(Label::sekundaer(
                "Kooperativ: 'Beenden' laesst den Task beim naechsten Durchlauf\n\
                 an seinem await-Punkt fallen. Kernel-/Fenster-Tasks sind geschuetzt.",
            )),
        ]))
    }

    fn nachricht(&mut self, id: u32) -> AppReaktion {
        match id {
            N_BEENDEN => {
                if let Some(task) = self.auswahl_task() {
                    if uebersicht::beenden_anfordern(task.id) {
                        crate::serial_println!(
                            "[TASKMGR] Beenden angefordert: {} ({})",
                            task.name,
                            task.id
                        );
                    }
                }
                // Die Liste zieht beim nächsten Tick nach; sofort
                // aktualisieren zeigt den Zwischenstand.
                self.aktualisieren();
            }
            N_DEMO_STARTEN => {
                let nummer = DEMO_NUMMER.fetch_add(1, Ordering::Relaxed);
                // task::spawn ist unter dem MANAGER-Lock erlaubt
                // (lock-freie Spawn-Queue); der Executor übernimmt
                // den Task in seiner nächsten Runde.
                let _ = crate::task::spawn(
                    crate::task::Task::new(format!("Demo-Zaehler {}", nummer), demo_zaehler(nummer))
                        .mit_art(TaskArt::Demo)
                        .als_beendbar(),
                );
                self.aktualisieren();
            }
            id if id >= N_LISTE => {
                let index = (id - N_LISTE) as usize;
                self.auswahl_id = self.tasks.get(index).map(|task| task.id);
            }
            _ => return AppReaktion::keine(),
        }
        AppReaktion::neu_aufbauen()
    }

    /// Sekündliche Aktualisierung (Uhr-Task stößt tick an).
    fn tick(&mut self) -> bool {
        let sekunde = zeit::ms_seit_boot() / 1000;
        if sekunde == self.letzte_sekunde {
            return false;
        }
        self.letzte_sekunde = sekunde;
        self.aktualisieren();
        true
    }
}

/// Start-Funktion für die App-Registry.
pub fn starten() {
    crate::fenster::app_starten(Box::new(TaskManagerApp::neu()), 640, 460);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Graph-Downsampling: wenige Werte spannen die volle Breite auf,
    /// viele Werte werden per Spalten-MAXIMUM eingedampft, und die
    /// y-Achse bildet 0/100 % auf unten/oben ab.
    #[test_case]
    fn test_graph_punkte() {
        // Zwei Werte auf 10 Spalten: erster bei x=0, letzter bei x=9;
        // 0 % liegt UNTEN (y = hoehe-1), 100 % OBEN (y = 0).
        let einfach = graph_punkte(&[0, 100], 10, 101);
        assert_eq!(einfach, vec![(0, 100), (9, 0)]);

        // 120 Werte auf 60 Spalten: jede Spalte nimmt das MAX ihres
        // Paars — die Spitzen (99) bleiben sichtbar, obwohl jeder
        // zweite Wert 1 ist.
        let mut saege = Vec::new();
        for _ in 0..60 {
            saege.push(1u8);
            saege.push(99u8);
        }
        let punkte = graph_punkte(&saege, 60, 100);
        assert_eq!(punkte.len(), 60);
        assert!(punkte.iter().all(|(_, y)| *y == 99 - 99 * 99 / 100));

        // Grenzfälle: leer/zu klein liefert nichts (kein Panic).
        assert!(graph_punkte(&[], 60, 100).is_empty());
        assert!(graph_punkte(&[50], 0, 100).is_empty());
        // Ein einzelner Wert: genau ein Punkt bei x=0.
        assert_eq!(graph_punkte(&[50], 60, 101).len(), 1);
        // Werte über 100 werden geklemmt statt aus dem Kasten zu laufen.
        assert_eq!(graph_punkte(&[255], 60, 101)[0].1, 0);
    }

    /// Laufzeit-Formatierung: Sekunden, Minuten, Stundenübergang.
    #[test_case]
    fn test_laufzeit_text() {
        assert_eq!(laufzeit_text(0), "00:00");
        assert_eq!(laufzeit_text(59 * 1_000_000), "00:59");
        assert_eq!(laufzeit_text(61 * 1_000_000), "01:01");
        assert_eq!(laufzeit_text(3_600 * 1_000_000), "1:00:00");
        assert_eq!(laufzeit_text(3_661 * 1_000_000), "1:01:01");
    }
}
