// ui/app.rs — Das App-Trait: die Brücke vom Inhalt-Enum zum Trait
//
// Das Inhalt-Enum im Fenster-Manager bleibt für Terminal und die
// alten Demos — jede NEUE App implementiert stattdessen dieses Trait
// und landet als `Inhalt::App(AppFenster)` im Fenster. Damit wächst
// bei neuen Apps kein match-Arm mehr im Manager.
//
// Arbeitsteilung:
//   * Die App hält ihren ZUSTAND und baut daraus in aufbau() den
//     Widget-Baum. Nach einer Zustandsänderung liefert sie
//     neu_aufbauen zurück — der Manager baut den Baum frisch.
//   * Das AppFenster (unten) verheiratet App + UiFenster und reicht
//     alle Fenster-Events an die Widget-Wurzel durch; Widget-
//     Nachrichten (u32) gehen an App::nachricht.
//
// LOCK-REGEL: nachricht()/tick() laufen UNTER dem MANAGER-Lock
// (Interrupts aus). Erlaubt: eigenen Zustand ändern, fs::mit_fs,
// serial_println!. VERBOTEN: print!/println! (Terminal-Umleitung
// braucht den MANAGER-Lock -> Deadlock) und fenster::-Aufrufe —
// dafür gibt es AppReaktion.danach: eine fn(), die der Manager NACH
// dem Loslassen des Locks ausführt (dasselbe NachLock-Muster wie
// bei App-Starts aus dem Startmenü).

use crate::grafik::Icon;
use speedui::{UiFenster, Widget};
use alloc::boxed::Box;

/// Antwort einer App auf eine Widget-Nachricht oder einen Tick.
#[derive(Default)]
pub struct AppReaktion {
    /// Der Widget-Baum soll aus dem App-Zustand neu gebaut werden
    /// (aufbau() wird gerufen; Widget-Zustände gehen dabei verloren).
    pub neu_aufbauen: bool,
    /// Wird NACH dem Loslassen des MANAGER-Locks ausgeführt
    /// (Fenster öffnen, drucken, ...). Box<dyn FnOnce> statt fn():
    /// Die Aktion darf Daten mitnehmen (move) — z. B. den Pfad der
    /// Datei, die der Betrachter öffnen soll.
    pub danach: Option<alloc::boxed::Box<dyn FnOnce() + Send>>,
    /// Kontextmenü am Maus-Cursor öffnen: (Beschriftung, Nachricht-ID).
    /// Der Klick auf einen Eintrag landet wieder in App::nachricht.
    pub kontextmenue: Option<alloc::vec::Vec<(alloc::string::String, u32)>>,
    /// Neuer Fenster-Titel (SpeedText: "name.txt *" bei Änderungen).
    pub titel: Option<alloc::string::String>,
    /// Das Fenster nach dieser Reaktion SCHLIESSEN (die App hat z. B.
    /// im Nachfrage-Dialog "Verwerfen" bestätigt bekommen).
    pub schliessen: bool,
    /// Auch die untere STATUSZEILE des Inhalts neu zeichnen — für
    /// Apps, deren Statusleiste sich bei jeder Eingabe ändert
    /// (SpeedText: Zeile/Spalte/Zeichen), ohne den ganzen Baum
    /// neu zu bauen. Der Manager übersetzt das mit den Fenstermaßen
    /// in einen Schadensstreifen am unteren Rand (Performance-Pass).
    pub status_neu: bool,
}

impl AppReaktion {
    pub fn keine() -> Self {
        AppReaktion::default()
    }
    pub fn neu_aufbauen() -> Self {
        AppReaktion { neu_aufbauen: true, ..AppReaktion::default() }
    }
    /// Aktion nach dem Lock (mit Daten — siehe Feld-Doku).
    pub fn danach(aktion: impl FnOnce() + Send + 'static) -> Self {
        AppReaktion {
            danach: Some(alloc::boxed::Box::new(aktion)),
            ..AppReaktion::default()
        }
    }
    /// Kontextmenü öffnen.
    pub fn menue(eintraege: alloc::vec::Vec<(alloc::string::String, u32)>) -> Self {
        AppReaktion { kontextmenue: Some(eintraege), ..AppReaktion::default() }
    }
    /// Fenster schließen (Baum wird vorher nicht mehr gebraucht).
    pub fn schliessen() -> Self {
        AppReaktion { schliessen: true, ..AppReaktion::default() }
    }
    /// Builder: zusätzlich eine danach-Aktion mitgeben.
    pub fn mit_danach(mut self, aktion: impl FnOnce() + Send + 'static) -> Self {
        self.danach = Some(alloc::boxed::Box::new(aktion));
        self
    }
    /// Builder: zusätzlich den Fenster-Titel aktualisieren.
    pub fn mit_titel(mut self, titel: alloc::string::String) -> Self {
        self.titel = Some(titel);
        self
    }
    /// Builder: auch die untere Statuszeile neu zeichnen lassen.
    pub fn mit_status_neu(mut self) -> Self {
        self.status_neu = true;
        self
    }
}

/// Eine SpeedOS-App: Zustand + Widget-Baum. `Send`, weil Fenster-
/// Inhalte durch den globalen Manager wandern.
pub trait App: Send {
    /// Fenster-Titel.
    fn name(&self) -> &'static str;
    /// Icon für Titelleiste, Taskleiste und Alt+Tab.
    fn icon(&self) -> &'static Icon;
    /// Baut den Widget-Baum aus dem aktuellen App-Zustand.
    fn aufbau(&self) -> Box<dyn Widget>;
    /// Eine Nachricht aus dem Widget-Baum (u32-IDs der Widgets).
    fn nachricht(&mut self, id: u32) -> AppReaktion {
        let _ = id;
        AppReaktion::keine()
    }
    /// Periodischer Anstoß (Uhr-Task, ~2x/Sekunde) für Live-Inhalte.
    /// true = Baum neu aufbauen und zeichnen.
    fn tick(&mut self) -> bool {
        false
    }

    /// Tasten-Hook VOR dem Widget-Baum: Some = die App hat die Taste
    /// selbst verarbeitet (App-Shortcuts wie Backspace = "hoch" im
    /// Explorer, Eingabemodi), None = normal an die Widgets.
    /// Läuft wie nachricht() unter dem MANAGER-Lock (gleiche Regeln).
    fn taste(&mut self, taste: pc_keyboard::DecodedKey) -> Option<AppReaktion> {
        let _ = taste;
        None
    }

    /// Der Fenster-Titel beim Start (Standard: der App-Name).
    /// Später ändern: AppReaktion.titel / mit_titel.
    fn fenster_titel(&self) -> alloc::string::String {
        alloc::string::String::from(self.name())
    }

    /// Der Nutzer will das Fenster schließen (X-Knopf). None = sofort
    /// schließen (Standard); Some(reaktion) = NICHT schließen, die
    /// Reaktion umsetzen — z. B. den Nachfrage-Dialog für
    /// ungespeicherte Änderungen aufbauen (SpeedText). Die App
    /// schließt dann später selbst über AppReaktion.schliessen.
    /// Läuft wie nachricht() unter dem MANAGER-Lock (gleiche Regeln).
    fn schliessen_abfragen(&mut self) -> Option<AppReaktion> {
        None
    }
}

/// TOOLKIT-REVIEW Serie 3: Einstellungen und Task-Manager haben
/// beide die "nur beim Sekundenwechsel neu bauen"-Logik dupliziert —
/// jetzt EIN Baustein für alle Live-Apps: in tick() einfach
/// `self.sekunden_tick.faellig()` fragen.
pub struct SekundenTick {
    letzte_sekunde: u64,
}

impl SekundenTick {
    pub fn neu() -> Self {
        SekundenTick { letzte_sekunde: crate::zeit::ms_seit_boot() / 1000 }
    }

    /// true GENAU einmal pro Sekundenwechsel (der Uhr-Task tickt
    /// öfter — ohne diese Bremse würde jede Live-App 2x/s neu bauen).
    pub fn faellig(&mut self) -> bool {
        let sekunde = crate::zeit::ms_seit_boot() / 1000;
        if sekunde == self.letzte_sekunde {
            return false;
        }
        self.letzte_sekunde = sekunde;
        true
    }
}

impl Default for SekundenTick {
    fn default() -> Self {
        Self::neu()
    }
}

/// App + Widget-Baum als Fenster-Inhalt (`Inhalt::App`).
pub struct AppFenster {
    pub app: Box<dyn App>,
    pub ui: UiFenster,
}

impl AppFenster {
    pub fn neu(app: Box<dyn App>) -> Self {
        // Nachricht-Handler des UiFensters bleibt ungenutzt (Apps
        // bekommen ihre Nachrichten über App::nachricht) — ein
        // No-op-fn erfüllt die Schnittstelle.
        let mut ui = UiFenster::neu(app.aufbau(), |_| {}, app.icon());
        // Erstes fokussierbares Widget fokussieren (Pfeiltasten
        // funktionieren dann sofort, ohne Tab).
        ui.fokus_initial();
        AppFenster { app, ui }
    }

    /// Baut den Widget-Baum aus dem App-Zustand neu (und stellt den
    /// Initial-Fokus wieder her — der alte Baum ist weg).
    pub fn neu_aufbauen(&mut self) {
        self.ui.wurzel_setzen(self.app.aufbau());
        self.ui.fokus_initial();
    }
}
