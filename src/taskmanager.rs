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
//
// SEIT DEM PRÄEMPTIVEN SCHEDULER (Serie 6, Teil 3) zeigt die App ZWEI
// Tabellen — und macht damit die Architektur-Entscheidung sichtbar:
//   * OBEN die PROZESSE (PID, Name, Zustand, CPU-Zeit): präemptiv
//     umgeschaltet, jeder mit eigenem Adressraum. PID 0 ist der
//     Kernel-Prozess — also genau der Executor, dessen Tasks darunter
//     stehen.
//   * UNTEN die KERNEL-TASKS: kooperativ, alle INNERHALB von PID 0.
// Wer die beiden Listen nebeneinander sieht, versteht die Zwei-Ebenen-
// Architektur aus docs/scheduler-design.md auf einen Blick.

use crate::grafik::{Icon, Rechteck};
use crate::prozess::ProzessMoment;
use crate::task::uebersicht::{self, TaskArt, TaskMoment};
use crate::theme;
use crate::ui::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::ui::{
    hbox, vbox, App, AppReaktion, Farbrolle, Fueller, Maler, Mass, UiEreignis, UiKontext,
    UiReaktion, Widget,
};
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
// Serie 6, Teil 6: echte Prozesse beenden — mit Nachfrage.
const N_PROZESS_BEENDEN: u32 = 3;
const N_BEENDEN_JA: u32 = 4;
const N_BEENDEN_NEIN: u32 = 5;
const N_LISTE: u32 = 1000; // + Zeilen-Index (Auswahl und Doppelklick)
const N_PROZESS_LISTE: u32 = 500_000; // + Zeilen-Index (Prozess-Auswahl)

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

/// CPU-Zeit lesbar. Anders als `laufzeit_text` (mm:ss) braucht die
/// VERBRAUCHTE Zeit eines Prozesses feine Auflösung: Ein Prozess, der eine
/// halbe Sekunde gerechnet hat, darf nicht als "00:00" erscheinen — genau das
/// hat die erste Fassung der Prozess-Tabelle getan und den Unterschied
/// zwischen Schläfer (26 µs!) und Dauerrechner (600 ms) unsichtbar gemacht.
/// Unter 1 s in Millisekunden, darunter/darüber in Sekunden mit Komma, ab
/// 100 s wieder mm:ss.
pub fn cpu_zeit_text(us: u64) -> String {
    if us < 1_000 {
        format!("{} us", us)
    } else if us < 1_000_000 {
        format!("{} ms", us / 1_000)
    } else if us < 100_000_000 {
        format!("{},{:03} s", us / 1_000_000, (us / 1_000) % 1_000)
    } else {
        laufzeit_text(us)
    }
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
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        // Breite skaliert mit (60 Sekunden sollen erkennbar bleiben).
        (GRAPH_SEKUNDEN as i32 * 3 * theme::skala_halbe() / 2, 4 * k.mass(Mass::ZeilenHoehe))
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // Kopie statt Borrow: `&m.kontext` wuerde den Maler festhalten.
        let kontext = m.kontext;
        let k = &kontext;
        m.fuellen(bereich, k.farbe(Farbrolle::Eingabefeld));
        m.rahmen(bereich, k.farbe(Farbrolle::Rahmen));
        // 50-%-Hilfslinie:
        let mitte = bereich.y + bereich.hoehe / 2;
        m.linie(bereich.x + 1, mitte, bereich.x + bereich.breite - 2, mitte, k.farbe(Farbrolle::KnopfFlaeche));

        // Der Verlauf als Linienzug (innen, 2 px Rand):
        let innen_breite = bereich.breite - 4;
        let innen_hoehe = bereich.hoehe - 4;
        let punkte = graph_punkte(&self.werte, innen_breite, innen_hoehe);
        for paar in punkte.windows(2) {
            let (x1, y1) = paar[0];
            let (x2, y2) = paar[1];
            m.linie(
                bereich.x + 2 + x1,
                bereich.y + 2 + y1,
                bereich.x + 2 + x2,
                bereich.y + 2 + y2,
                k.farbe(Farbrolle::Akzent),
            );
        }
    }

    fn ereignis(&mut self, _e: &UiEreignis, _b: Rechteck, _k: &UiKontext) -> UiReaktion {
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
    /// Die PROZESSE (präemptiv, eigener Adressraum) — nach PID sortiert.
    prozesse: Vec<ProzessMoment>,
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
    /// Auswahl in der PROZESS-Tabelle, als PID (nicht Index).
    auswahl_pid: Option<crate::prozess::Pid>,
    /// Läuft gerade die Sicherheitsabfrage? Dann zeigt die App statt ihrer
    /// Tabellen den Bestätigungs-Dialog (dasselbe Muster wie SpeedText:
    /// Dialoge ERSETZEN den Inhalt, sie liegen nicht darüber).
    beenden_nachfrage: Option<(crate::prozess::Pid, String)>,
    /// Sekündliche Aktualisierung (geteilter Toolkit-Baustein).
    sekunden_tick: crate::ui::app::SekundenTick,
}

impl TaskManagerApp {
    pub fn neu() -> Self {
        let mut app = TaskManagerApp {
            tasks: Vec::new(),
            prozesse: Vec::new(),
            letzte_polls: BTreeMap::new(),
            letzte_aufnahme_us: 0,
            polls_pro_s: BTreeMap::new(),
            cpu_verlauf: Vec::new(),
            cpu_prozent: 0,
            heap: None,
            auswahl_id: None,
            auswahl_pid: None,
            beenden_nachfrage: None,
            sekunden_tick: crate::ui::app::SekundenTick::neu(),
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
        // Die Prozess-Tabelle ist ein BLATT-Lock (mit ausgeschalteten
        // Interrupts genommen) — unter dem MANAGER-Lock also erlaubt, genau
        // wie die Task-Übersicht.
        self.prozesse = crate::scheduler::momentaufnahme();

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
        // Dasselbe für die Prozess-Auswahl: Ein beendeter Prozess
        // verschwindet aus der Tabelle, sobald der Aufräum-Task ihn geholt
        // hat — dann darf die Auswahl nicht auf eine tote PID zeigen.
        if let Some(pid) = self.auswahl_pid {
            if !self.prozesse.iter().any(|prozess| prozess.pid == pid) {
                self.auswahl_pid = None;
            }
        }
    }

    /// Der ausgewählte Prozess (nach PID gesucht, nicht nach Index — die
    /// Auswahl überlebt so das sekündliche Neu-Einlesen).
    fn auswahl_prozess(&self) -> Option<&ProzessMoment> {
        let pid = self.auswahl_pid?;
        self.prozesse.iter().find(|prozess| prozess.pid == pid)
    }

    fn auswahl_task(&self) -> Option<&TaskMoment> {
        self.tasks.iter().find(|task| Some(task.id) == self.auswahl_id)
    }

    /// Füllt den CPU-Graphen mit 60 synthetischen Werten — für den
    /// Frame-Zeit-Berichts-Test (der Graph soll wie im Live-Betrieb
    /// eine volle Linie zeichnen müssen).
    pub fn cpu_verlauf_fuellen_fuer_messung(&mut self) {
        self.cpu_verlauf = (0..60).map(|i| ((i * 7) % 100) as u8).collect();
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
        // DIALOG STATT INHALT (das SpeedText-Muster): Läuft die Nachfrage,
        // ersetzt sie den ganzen Fensterinhalt. Kein Overlay, keine zweite
        // Ereignis-Ebene — der Dialog ist einfach der Baum, den `aufbau`
        // gerade liefert.
        if let Some((pid, name)) = &self.beenden_nachfrage {
            return crate::ui::dialog::bestaetigung(
                &format!(
                    "Prozess {} (PID {}) wirklich beenden?\n\n\
                     Er wird sofort gestoppt: sein Adressraum, sein\n\
                     Kernel-Stack und alle offenen Handles gehen zurueck.\n\
                     Nicht gespeicherte Daten des Programms sind verloren.",
                    name, pid
                ),
                &[("Beenden", N_BEENDEN_JA), ("Abbrechen", N_BEENDEN_NEIN)],
            );
        }

        // --- Kopf: CPU/Heap/Tasks links, Live-Graph rechts ---
        // heap_statistik liefert (belegt, frei) — gesamt ist die Summe.
        let (heap_belegt, heap_frei) = self.heap.unwrap_or((0, 0));
        let kopf_text = format!(
            "CPU:      {:>3} %\nHeap:     {} von {}\nTasks:    {}\nProzesse: {}  (Wechsel: {})",
            self.cpu_prozent,
            crate::explorer::groesse_formatieren(heap_belegt),
            crate::explorer::groesse_formatieren(heap_belegt + heap_frei),
            self.tasks.len(),
            self.prozesse.len(),
            crate::scheduler::wechsel_gesamt(),
        );
        let kopf = hbox(vec![
            Box::new(Label::neu(&kopf_text)) as Box<dyn Widget>,
            Box::new(Fueller),
            Box::new(CpuGraph { werte: self.cpu_verlauf.clone() }),
        ]);

        // --- PROZESS-Tabelle (präemptiv): PID, Name, Zustand, CPU-Zeit ---
        // Präemptionen stehen mit dabei, denn sie sind der sichtbare
        // Unterschied zur kooperativen Welt: Sie zählen, wie oft dem Prozess
        // die CPU WEGGENOMMEN wurde.
        let prozess_kopf = format!(
            " {:>3}  {:<24} {:<11} {:>9} {:>7}",
            "PID", "Name", "Zustand", "CPU-Zeit", "Praeem."
        );
        let prozess_eintraege = self
            .prozesse
            .iter()
            .map(|prozess| {
                let name: String = prozess.name.chars().take(24).collect();
                ListenEintrag {
                    icon: None,
                    text: format!(
                        "{:>3}  {:<24} {:<11} {:>9} {:>7}",
                        prozess.pid,
                        name,
                        prozess.zustand.text(),
                        cpu_zeit_text(prozess.cpu_us),
                        prozess.praemptionen,
                    ),
                }
            })
            .collect();
        // AUSWÄHLBAR seit Serie 6, Teil 6: Ein Prozess lässt sich hier
        // WIRKLICH beenden — anders als ein kooperativer Task, bei dem
        // „beenden" nur eine Bitte ist, die der Task am nächsten await-Punkt
        // erfüllt (oder eben nicht). Ein Prozess wird schlicht nicht mehr
        // eingeplant; sein Adressraum und alle seine Handles gehen zurück,
        // ob er will oder nicht. Genau das ist der Unterschied, den
        // Präemption ausmacht.
        let prozess_auswahl = self
            .prozesse
            .iter()
            .position(|prozess| Some(prozess.pid) == self.auswahl_pid);
        let prozess_liste =
            ScrollListe::mit_index_nachrichten(prozess_eintraege, N_PROZESS_LISTE, N_PROZESS_LISTE)
                .mit_auswahl(prozess_auswahl)
                .mit_layout(160, 1);

        // --- TASK-Tabelle (kooperativ): Kopfzeile + eine Zeile pro Task ---
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
            .mit_fokus(true)
            .mit_layout(160, 2);

        // --- Fußzeile: Aktionen + die ehrliche Kooperativ-Zeile ---
        let beenden_erlaubt = self.auswahl_task().map(|t| t.beendbar).unwrap_or(false);
        // Der Knopf, den es in Serie 3 für Prozesse noch gar nicht gab:
        // Erlaubt ist er für jeden ausgewählten USER-Prozess — PID 0 (der
        // Kernel-Prozess) ist geschützt, ihn zu beenden hiesse, das System
        // anzuhalten.
        let prozess_beenden_erlaubt = self
            .auswahl_prozess()
            .map(|prozess| prozess.ist_user && prozess.zustand != crate::prozess::Zustand::Beendet)
            .unwrap_or(false);
        let aktionen = hbox(vec![
            Box::new(
                Button::neu("Prozess beenden", N_PROZESS_BEENDEN)
                    .mit_deaktiviert(!prozess_beenden_erlaubt),
            ) as Box<dyn Widget>,
            Box::new(Button::neu("Task beenden", N_BEENDEN).mit_deaktiviert(!beenden_erlaubt)),
            Box::new(Button::neu("Demo-Task starten", N_DEMO_STARTEN)),
            Box::new(Fueller),
        ]);

        Box::new(vbox(vec![
            Box::new(kopf) as Box<dyn Widget>,
            Box::new(Trennlinie),
            // ZWEI EBENEN, sichtbar getrennt:
            Box::new(Label::neu("PROZESSE — praeemptiv, eigener Adressraum")),
            Box::new(Label::sekundaer(&prozess_kopf)),
            Box::new(prozess_liste),
            Box::new(Trennlinie),
            Box::new(Label::neu(
                "KERNEL-TASKS — kooperativ, alle innerhalb von PID 0",
            )),
            Box::new(Label::sekundaer(&kopfzeile)),
            Box::new(liste),
            Box::new(aktionen),
            Box::new(Label::sekundaer(
                "Kooperativ: 'Task beenden' ist eine BITTE - der Task faellt beim\n\
                 naechsten Durchlauf an seinem await-Punkt; Kernel-Tasks geschuetzt.\n\
                 Praeemptiv: 'Prozess beenden' ist eine TATSACHE - der Prozess wird\n\
                 nicht mehr eingeplant, Adressraum und Handles gehen zurueck.",
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
            // --- Einen Prozess beenden: erst FRAGEN ---
            N_PROZESS_BEENDEN => {
                // Der Name wird JETZT festgehalten: Bis der Benutzer
                // antwortet, kann der Prozess längst weg sein — dann soll
                // im Dialog trotzdem stehen, worum es ging.
                match self.auswahl_prozess() {
                    Some(prozess) if prozess.ist_user => {
                        self.beenden_nachfrage = Some((prozess.pid, prozess.name.clone()));
                    }
                    // Kein oder kein beendbarer Prozess ausgewählt: nichts
                    // tun (der Knopf ist dann ohnehin gedimmt).
                    _ => return AppReaktion::keine(),
                }
            }
            N_BEENDEN_JA => {
                if let Some((pid, name)) = self.beenden_nachfrage.take() {
                    // HIER passiert das, was bei einem kooperativen Task
                    // unmöglich ist: Der Prozess wird beendet, ob er will
                    // oder nicht. Er bekommt keine Zeitscheibe mehr, und der
                    // Aufräum-Task gibt Adressraum, Kernel-Stack und alle
                    // Handles zurück.
                    let erfolg = crate::scheduler::beenden(pid);
                    crate::serial_println!(
                        "[TASKMGR] Prozess {} (PID {}) beenden: {}",
                        name,
                        pid,
                        if erfolg { "ok" } else { "gibt es nicht mehr" }
                    );
                    self.auswahl_pid = None;
                }
                self.aktualisieren();
            }
            N_BEENDEN_NEIN => {
                self.beenden_nachfrage = None;
            }
            // Auswahl in der PROZESS-Tabelle (die höheren Ids zuerst
            // prüfen — N_LISTE ist die kleinere Basis!).
            id if id >= N_PROZESS_LISTE => {
                let index = (id - N_PROZESS_LISTE) as usize;
                self.auswahl_pid = self.prozesse.get(index).map(|prozess| prozess.pid);
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
        if !self.sekunden_tick.faellig() {
            return false;
        }
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

    /// CPU-Zeit-Formatierung: Genau die feine Auflösung, die der
    /// Prozess-Tabelle vorher gefehlt hat (ein Schläfer mit 26 µs und ein
    /// Dauerrechner mit 600 ms sahen beide wie "00:00" aus).
    #[test_case]
    fn test_cpu_zeit_text() {
        assert_eq!(cpu_zeit_text(0), "0 us");
        assert_eq!(cpu_zeit_text(26), "26 us");
        assert_eq!(cpu_zeit_text(999), "999 us");
        assert_eq!(cpu_zeit_text(1_000), "1 ms");
        assert_eq!(cpu_zeit_text(600_899), "600 ms");
        assert_eq!(cpu_zeit_text(999_999), "999 ms");
        assert_eq!(cpu_zeit_text(1_000_000), "1,000 s");
        assert_eq!(cpu_zeit_text(1_242_638), "1,242 s");
        assert_eq!(cpu_zeit_text(99_999_999), "99,999 s");
        // Ab 100 s wieder die kompakte mm:ss-Form.
        assert_eq!(cpu_zeit_text(100_000_000), "01:40");
        // Und: Ein Schlaefer und ein Rechner sind jetzt UNTERSCHEIDBAR.
        assert_ne!(cpu_zeit_text(26), cpu_zeit_text(600_899));
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
