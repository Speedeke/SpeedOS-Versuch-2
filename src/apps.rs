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

/// Ein Eintrag der App-Registry. Trait-Apps (ui::App) tragen hier
/// eine Start-Funktion, die fenster::app_starten mit ihrer
/// App-Instanz aufruft — siehe galerie_starten.
pub struct AppEintrag {
    pub name: &'static str,
    pub icon: &'static Icon,
    /// Startet die App — läuft OHNE gehaltene Locks (siehe oben).
    pub start: fn(),
}

/// Alle registrierten Apps (die Reihenfolge ist die Menü-Reihenfolge).
pub fn alle_apps() -> &'static [AppEintrag] {
    &APPS
}

static APPS: [AppEintrag; 10] = [
    AppEintrag { name: "Terminal", icon: &grafik::ICON_TERMINAL, start: terminal_starten },
    AppEintrag { name: "Explorer", icon: &grafik::ICON_ORDNER, start: explorer_starten },
    AppEintrag { name: "Einstellungen", icon: &grafik::ICON_ZAHNRAD, start: crate::einstellungen::starten },
    AppEintrag { name: "Widget-Galerie", icon: &grafik::ICON_ZAHNRAD, start: galerie_starten },
    AppEintrag { name: "Uhr", icon: &grafik::ICON_UHR, start: uhr_starten },
    AppEintrag { name: "Tastatur-Echo", icon: &grafik::ICON_TASTATUR, start: tastatur_starten },
    AppEintrag { name: "Malkasten", icon: &grafik::ICON_PINSEL, start: malkasten_starten },
    AppEintrag { name: "Theme wechseln", icon: &grafik::ICON_THEME, start: fenster::theme_wechseln },
    AppEintrag { name: "Skalierung", icon: &grafik::ICON_ZAHNRAD, start: fenster::skalierung_wechseln },
    AppEintrag { name: "Neustart", icon: &grafik::ICON_NEUSTART, start: crate::neustart },
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

/// Der Explorer (src/explorer.rs) — die erste echte Trait-App.
/// Jeder Start = eine EIGENE Instanz mit eigenem Zustand, deshalb
/// funktionieren mehrere Explorer-Fenster unabhängig.
fn explorer_starten() {
    fenster::app_starten(
        alloc::boxed::Box::new(crate::explorer::ExplorerApp::neu()),
        560,
        400,
    );
}

// ---------------------------------------------------------------------------
// Widget-Galerie: die Demo-App des ui-Moduls — und die ERSTE App auf
// dem neuen App-Trait (Inhalt::App statt Inhalt-Enum-Variante).
// Sie zeigt alle Bausteine und loggt jede Interaktion seriell.
// ---------------------------------------------------------------------------

struct GalerieApp;

impl crate::ui::App for GalerieApp {
    fn name(&self) -> &'static str {
        "Widget-Galerie"
    }

    fn icon(&self) -> &'static Icon {
        &grafik::ICON_ZAHNRAD
    }

    fn aufbau(&self) -> alloc::boxed::Box<dyn crate::ui::Widget> {
        use crate::ui::widgets::{Button, Checkbox, Label, ListenEintrag, ScrollListe, Textfeld, Trennlinie};
        use crate::ui::{hbox, vbox, Fueller, Widget};
        use alloc::boxed::Box;
        use alloc::format;
        use alloc::vec;

        let eintraege = (1..=25)
            .map(|i| ListenEintrag { icon: Some(&grafik::ICON_DATEI), text: format!("Listeneintrag {i}") })
            .collect();

        Box::new(vbox(vec![
            Box::new(Label::neu("Widget-Galerie")) as Box<dyn Widget>,
            // Nur Latin-1-Zeichen — der vorgerasterte Font hat
            // keinen Gedankenstrich (wuerde als '?' erscheinen).
            Box::new(Label::sekundaer(
                "Hover, Klick, Tab-Fokus, Tippen, Scrollrad -\njede Interaktion landet im seriellen Log.",
            )),
            Box::new(hbox(vec![
                Box::new(Button::mit_icon("Start", &grafik::ICON_LOGO, 1)) as Box<dyn Widget>,
                Box::new(Button::neu("Zweiter", 2)),
                Box::new(Fueller),
                Box::new(Checkbox::neu("Haken", true, 3)),
            ])),
            Box::new(Textfeld::neu(4)),
            Box::new(Trennlinie),
            Box::new(ScrollListe::neu(eintraege, 5, 6)),
        ]))
    }

    /// Läuft unter dem MANAGER-Lock — serial_println! ist dort
    /// erlaubt (Blatt-Lock), print!/fenster:: wären es NICHT
    /// (siehe Lock-Regel in ui/app.rs).
    fn nachricht(&mut self, id: u32) -> crate::ui::AppReaktion {
        let quelle = match id {
            1 => "Button 'Start'",
            2 => "Button 'Zweiter'",
            3 => "Checkbox umgeschaltet",
            4 => "Textfeld: Enter",
            5 => "Liste: Auswahl",
            6 => "Liste: DOPPELKLICK",
            _ => "unbekannt",
        };
        crate::serial_println!("[GALERIE] Nachricht {}: {}", id, quelle);
        crate::ui::AppReaktion::keine()
    }
}

fn galerie_starten() {
    fenster::app_starten(alloc::boxed::Box::new(GalerieApp), 480, 460);
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
pub fn filtern(suchtext: &str) -> Vec<&'static AppEintrag> {
    filtern_indizes(suchtext)
        .into_iter()
        .map(|index| &alle_apps()[index])
        .collect()
}

/// Wie filtern, aber als Indizes in alle_apps() — das Startmenü
/// mappt damit Listenzeilen zurück auf Registry-Einträge.
pub fn filtern_indizes(suchtext: &str) -> Vec<usize> {
    let klein = suchtext.to_ascii_lowercase();
    alle_apps()
        .iter()
        .enumerate()
        .filter(|(_, app)| app.name.to_ascii_lowercase().contains(klein.as_str()))
        .map(|(index, _)| index)
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
            if matches!(app.name, "Uhr" | "Tastatur-Echo" | "Malkasten" | "Widget-Galerie") {
                (app.start)();
            }
        }
    }
}
