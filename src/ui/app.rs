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

use super::{UiFenster, Widget};
use crate::grafik::Icon;
use alloc::boxed::Box;

/// Antwort einer App auf eine Widget-Nachricht oder einen Tick.
#[derive(Default)]
pub struct AppReaktion {
    /// Der Widget-Baum soll aus dem App-Zustand neu gebaut werden
    /// (aufbau() wird gerufen; Widget-Zustände gehen dabei verloren).
    pub neu_aufbauen: bool,
    /// Wird NACH dem Loslassen des MANAGER-Locks ausgeführt
    /// (Fenster öffnen, drucken, ... — siehe Lock-Regel oben).
    pub danach: Option<fn()>,
}

impl AppReaktion {
    pub const fn keine() -> Self {
        AppReaktion { neu_aufbauen: false, danach: None }
    }
    pub const fn neu_aufbauen() -> Self {
        AppReaktion { neu_aufbauen: true, danach: None }
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
