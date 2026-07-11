// apps.rs — Die App-Registry von SpeedOS
//
// DIE Architektur, über die ab jetzt jede neue App angemeldet wird:
// Ein Eintrag = Name + Icon + Start-Funktion. Das Startmenü liest
// diese Liste, filtert sie über das Suchfeld und ruft beim Auswählen
// die Start-Funktion auf. Eine neue App ergänzen heißt nur:
//   1. Start-Funktion schreiben (öffnet z. B. ein Fenster),
//   2. einen App-Eintrag in APPS ergänzen — fertig.
//
// WICHTIG (Deadlock-Regel): Die Start-Funktionen werden IMMER erst
// NACH dem Loslassen des MANAGER-Locks aufgerufen (das Startmenü gibt
// sie als Rückgabewert nach draußen). Deshalb dürfen sie selbst
// bedenkenlos fenster::-Funktionen benutzen, die den Lock nehmen.

use crate::fenster::{self, Inhalt};
use crate::grafik::{self, Icon};
use alloc::string::String;
use alloc::vec::Vec;

/// Ein Eintrag der App-Registry.
pub struct App {
    pub name: &'static str,
    pub icon: &'static Icon,
    /// Startet die App — läuft OHNE gehaltene Locks (siehe oben).
    pub start: fn(),
}

/// Alle registrierten Apps (die Reihenfolge ist die Menü-Reihenfolge).
pub fn alle_apps() -> &'static [App] {
    &APPS
}

static APPS: [App; 6] = [
    App { name: "Terminal", icon: &grafik::ICON_TERMINAL, start: terminal_starten },
    App { name: "Uhr", icon: &grafik::ICON_UHR, start: uhr_starten },
    App { name: "Tastatur-Echo", icon: &grafik::ICON_TASTATUR, start: tastatur_starten },
    App { name: "Malkasten", icon: &grafik::ICON_PINSEL, start: malkasten_starten },
    App { name: "Theme wechseln", icon: &grafik::ICON_THEME, start: fenster::theme_wechseln },
    App { name: "Neustart", icon: &grafik::ICON_NEUSTART, start: crate::neustart },
];

fn terminal_starten() {
    // Öffnet die SpeedShell als Fenster (oder holt sie nach vorn).
    // Bei einem FRISCHEN Fenster den Prompt nachholen — die Shell
    // wartet gerade auf Tasten und würde sonst keinen zeigen.
    if fenster::terminal_oeffnen() {
        crate::shell::prompt_nachholen();
    }
}

fn uhr_starten() {
    fenster::app_fenster_oeffnen("Uhr", 420, 150, Inhalt::Uhr);
}

fn tastatur_starten() {
    fenster::app_fenster_oeffnen(
        "Tastatur",
        560,
        140,
        Inhalt::TastaturEcho { text: String::new() },
    );
}

fn malkasten_starten() {
    fenster::app_fenster_oeffnen("Malkasten", 380, 220, Inhalt::Malflaeche { klicks: Vec::new() });
}

/// Filtert die Registry nach dem Suchtext (Groß/Klein egal) — die
/// Grundlage der späteren systemweiten Schnellsuche. Leerer Suchtext
/// liefert alle Apps.
pub fn filtern(suchtext: &str) -> Vec<&'static App> {
    let klein = suchtext.to_ascii_lowercase();
    alle_apps()
        .iter()
        .filter(|app| app.name.to_ascii_lowercase().contains(klein.as_str()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Suchfilter: leer = alles, Teilwort egal welcher Schreibung,
    /// Unsinn = leere Liste.
    #[test_case]
    fn test_apps_filtern() {
        assert_eq!(filtern("").len(), alle_apps().len());
        let treffer = filtern("UHR");
        assert_eq!(treffer.len(), 1);
        assert_eq!(treffer[0].name, "Uhr");
        assert!(filtern("gibtsnicht").is_empty());
    }

    /// Jeder Registry-Eintrag hat einen eindeutigen Namen (sonst wäre
    /// die Auswahl im Startmenü mehrdeutig).
    #[test_case]
    fn test_apps_namen_eindeutig() {
        let apps = alle_apps();
        for (i, a) in apps.iter().enumerate() {
            for b in apps.iter().skip(i + 1) {
                assert_ne!(a.name, b.name);
            }
        }
        // "Neustart" wird hier bewusst NICHT gestartet (würde QEMU
        // mitten im Test rebooten) — der Eintrag muss nur existieren.
        assert!(apps.iter().any(|a| a.name == "Neustart"));
    }

    /// Die harmlosen Fenster-Apps lassen sich wirklich starten
    /// (legen ein Fenster im globalen Manager an, sofern er läuft).
    #[test_case]
    fn test_fenster_apps_starten_ohne_panic() {
        for app in alle_apps() {
            if matches!(app.name, "Uhr" | "Tastatur-Echo" | "Malkasten") {
                (app.start)();
            }
        }
    }
}
