// speedtext.rs — SpeedText: der Texteditor von SpeedOS
//
// Die letzte App der Serie. Sie verheiratet drei neue Bausteine:
//
//   * das mehrzeilige Editor-Widget (ui/texteditor.rs) — der PUFFER
//     lebt als Arc<Mutex<TextPuffer>> GETEILT zwischen App und
//     Widget, damit der Text die Neu-Aufbauten des Widget-Baums
//     überlebt (Statuszeile + Titel-Stern erzwingen die ständig),
//   * den Datei-Dialog (ui/dialog.rs) für Strg+O (Öffnen) und
//     Speichern-unter (Strg+S ohne Pfad),
//   * den Bestätigungs-Dialog fürs Schließen mit ungespeicherten
//     Änderungen (Speichern / Verwerfen / Abbrechen) über den neuen
//     App::schliessen_abfragen-Hook + AppReaktion.schliessen.
//
// Dialoge ERSETZEN den Fenster-Inhalt (App-Zustand `dialog` steuert
// aufbau()) — der Editor-Inhalt bleibt dabei im Arc erhalten.
//
// Titelleiste: "name.txt - SpeedText" mit Stern (*) bei
// ungespeicherten Änderungen — über AppReaktion.titel, das Zeichnen
// übernimmt wie immer der Compositor.

use crate::fs;
use crate::grafik::Icon;
use crate::ui::dialog::{self, bestaetigung, DateiDialog, DateiDialogErgebnis};
use crate::ui::texteditor::{geteilter_puffer, GeteilterPuffer, StatusZeile, TextEditor, TextPuffer};
use crate::ui::widgets::Label;
use crate::ui::{vbox, App, AppReaktion, Widget};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use pc_keyboard::DecodedKey;

// ----- Nachricht-IDs -----
const N_EDITOR: u32 = 1; // jede Änderung/Cursorbewegung im Editor
const N_SCHL_SPEICHERN: u32 = 10; // Schließen-Dialog: Speichern
const N_SCHL_VERWERFEN: u32 = 11; // Schließen-Dialog: Verwerfen
const N_SCHL_ABBRECHEN: u32 = 12; // Schließen-Dialog: Abbrechen
const N_FEHLER_OK: u32 = 13; // Fehler-Dialog: OK (schließt ihn)
const N_DIALOG_BASIS: u32 = 50_000; // Id-Fenster des Datei-Dialogs

/// Welcher Dialog liegt gerade über dem Editor?
enum Dialog {
    /// Strg+O — Ergebnis: Datei laden.
    Oeffnen(DateiDialog),
    /// Speichern unter (Strg+S ohne Pfad). `danach_schliessen`:
    /// Der Speichern-Weg kam aus dem Schließen-Dialog — nach
    /// erfolgreichem Speichern schließt das Fenster.
    Speichern { dialog: DateiDialog, danach_schliessen: bool },
    /// Schließen mit ungespeicherten Änderungen (X-Knopf).
    SchliessenFrage,
    /// Laden/Speichern ist gescheitert (FsFehler bis IoFehler):
    /// Die Meldung kommt als DIALOG, nicht nur als Statuszeile —
    /// still scheitern gibt es nicht (Daten-Integritäts-Regel).
    Fehler(String),
}

pub struct SpeedTextApp {
    puffer: GeteilterPuffer,
    /// Der Datei-Pfad (None = neue, unbenannte Datei).
    pfad: Option<String>,
    dialog: Option<Dialog>,
    /// Statuszeilen-Meldung (Fehler beim Laden/Speichern).
    meldung: Option<String>,
    /// Zuletzt gesetzter Fenster-Titel — beim Tippen wird der Titel
    /// nur gemeldet, wenn er sich WIRKLICH ändert (der Stern kommt
    /// genau einmal; danach kostet eine Taste keinen Titel-Update).
    letzter_titel: String,
}

impl SpeedTextApp {
    pub fn neu() -> Self {
        let mut app = SpeedTextApp {
            puffer: geteilter_puffer(TextPuffer::leer()),
            pfad: None,
            dialog: None,
            meldung: None,
            letzter_titel: String::new(),
        };
        app.letzter_titel = app.titel();
        app
    }

    /// Öffnet SpeedText direkt mit einer Datei (Explorer-Doppelklick).
    pub fn mit_datei(pfad: &str) -> Self {
        let mut app = SpeedTextApp::neu();
        app.laden(pfad);
        app
    }

    /// Kurzer Blick in den geteilten Puffer (Blatt-Lock).
    fn mit_puffer<T>(&self, f: impl FnOnce(&mut TextPuffer) -> T) -> T {
        x86_64::instructions::interrupts::without_interrupts(|| f(&mut self.puffer.lock()))
    }

    fn laden(&mut self, pfad: &str) {
        match fs::mit_fs(|f| f.lesen(pfad)) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                self.mit_puffer(|p| *p = TextPuffer::aus_text(&text));
                self.pfad = Some(String::from(pfad));
                self.meldung = None;
            }
            Err(fehler) => {
                // Dialog UND Statuszeile: Der Dialog macht den Fehler
                // unübersehbar, die Statuszeile erinnert nach dem OK.
                let text = format!("Laden: {}", fehler.meldung());
                self.meldung = Some(text.clone());
                self.dialog = Some(Dialog::Fehler(text));
            }
        }
    }

    /// Speichert unter dem bekannten Pfad. true = erfolgreich.
    /// Nach dem Schreiben ein fs::sync(): "Gespeichert" heißt seit
    /// SpeedFS "auf dem Medium" — auch der ATA-Schreib-Cache wird
    /// geflusht. Ein sync-Fehler zählt wie ein Schreibfehler
    /// (Dialog), denn die Daten sind dann eben NICHT sicher.
    fn speichern_nach(&mut self, pfad: &str) -> bool {
        let text = self.mit_puffer(|p| p.als_text());
        match fs::mit_fs(|f| f.schreiben(pfad, text.as_bytes())).and_then(|()| fs::sync()) {
            Ok(()) => {
                self.mit_puffer(|p| p.geaendert = false);
                self.pfad = Some(String::from(pfad));
                self.meldung = None;
                true
            }
            Err(fehler) => {
                let text = format!("Speichern: {}", fehler.meldung());
                self.meldung = Some(text.clone());
                self.dialog = Some(Dialog::Fehler(text));
                false
            }
        }
    }

    /// Strg+S: mit Pfad direkt speichern, ohne Pfad den
    /// Speichern-Dialog öffnen.
    fn speichern(&mut self, danach_schliessen: bool) -> AppReaktion {
        if let Some(pfad) = self.pfad.clone() {
            let ok = self.speichern_nach(&pfad);
            if ok && danach_schliessen {
                return AppReaktion::schliessen();
            }
        } else {
            self.dialog = Some(Dialog::Speichern {
                dialog: DateiDialog::neu("Speichern unter", crate::explorer::start_ordner(), "neu.txt", N_DIALOG_BASIS),
                danach_schliessen,
            });
        }
        self.reaktion_mit_titel()
    }

    /// Der Fenster-Titel: "name.txt - SpeedText", Stern bei
    /// ungespeicherten Änderungen.
    fn titel(&self) -> String {
        let name = match &self.pfad {
            Some(pfad) => pfad.rsplit('/').next().unwrap_or(pfad),
            None => "Unbenannt",
        };
        let stern = if self.mit_puffer(|p| p.geaendert) { " *" } else { "" };
        format!("{}{} - SpeedText", name, stern)
    }

    /// Standard-Reaktion: neu aufbauen + Titel nachziehen.
    fn reaktion_mit_titel(&mut self) -> AppReaktion {
        self.letzter_titel = self.titel();
        AppReaktion::neu_aufbauen().mit_titel(self.letzter_titel.clone())
    }
}

impl App for SpeedTextApp {
    fn name(&self) -> &'static str {
        "SpeedText"
    }

    fn fenster_titel(&self) -> String {
        self.titel()
    }

    fn icon(&self) -> &'static Icon {
        &crate::grafik::ICON_DATEI
    }

    fn aufbau(&self) -> Box<dyn Widget> {
        // Ein offener Dialog ERSETZT den Editor-Inhalt (der Text
        // bleibt im geteilten Puffer erhalten):
        match &self.dialog {
            Some(Dialog::Oeffnen(dialog)) => return dialog.aufbau(),
            Some(Dialog::Speichern { dialog, .. }) => return dialog.aufbau(),
            Some(Dialog::SchliessenFrage) => {
                return bestaetigung(
                    "Ungespeicherte Aenderungen - was tun?",
                    &[
                        ("Speichern", N_SCHL_SPEICHERN),
                        ("Verwerfen", N_SCHL_VERWERFEN),
                        ("Abbrechen", N_SCHL_ABBRECHEN),
                    ],
                );
            }
            Some(Dialog::Fehler(meldung)) => return dialog::fehler(meldung, N_FEHLER_OK),
            None => {}
        }

        // Die Statuszeile (Zeile:Spalte, Zeichenzahl, Änderungs-
        // Status) liest LIVE aus dem geteilten Puffer — Tippen
        // braucht deshalb keinen Baum-Neuaufbau mehr (Performance-
        // Pass). Nur Fehlermeldungen ersetzen sie durch ein Label.
        let status: Box<dyn Widget> = match &self.meldung {
            Some(meldung) => Box::new(Label::sekundaer(&format!("Fehler - {}", meldung))),
            None => Box::new(StatusZeile::neu(self.puffer.clone())),
        };

        Box::new(vbox(vec![
            Box::new(TextEditor::neu(self.puffer.clone(), N_EDITOR)) as Box<dyn Widget>,
            status,
        ]))
    }

    fn nachricht(&mut self, id: u32) -> AppReaktion {
        // Datei-Dialog zuerst (sein Id-Fenster liegt weit oben):
        if let Some(Dialog::Oeffnen(dialog)) = &mut self.dialog {
            if let Some(ergebnis) = dialog.nachricht(id) {
                match ergebnis {
                    Some(DateiDialogErgebnis::Gewaehlt(pfad)) => {
                        self.dialog = None;
                        self.laden(&pfad);
                    }
                    Some(DateiDialogErgebnis::Abgebrochen) => self.dialog = None,
                    None => {}
                }
                return self.reaktion_mit_titel();
            }
        }
        if let Some(Dialog::Speichern { dialog, danach_schliessen }) = &mut self.dialog {
            let danach_schliessen = *danach_schliessen;
            if let Some(ergebnis) = dialog.nachricht(id) {
                match ergebnis {
                    Some(DateiDialogErgebnis::Gewaehlt(pfad)) => {
                        self.dialog = None;
                        if self.speichern_nach(&pfad) && danach_schliessen {
                            return AppReaktion::schliessen();
                        }
                    }
                    Some(DateiDialogErgebnis::Abgebrochen) => self.dialog = None,
                    None => {}
                }
                return self.reaktion_mit_titel();
            }
        }

        match id {
            // Editor-Interaktion (jede Taste!): Das Neuzeichnen hat
            // das Widget schon angestoßen, die Statuszeile liest
            // live — hier bleibt nur der Titel-Stern, und der auch
            // nur, wenn er sich WIRKLICH ändert (einmal pro
            // Geändert-Wechsel statt bei jedem Tastendruck).
            N_EDITOR => {
                // Die Statuszeile (Zeile/Spalte/Zeichen) ändert sich bei
                // JEDER Editor-Nachricht — sie liegt aber unten, außerhalb
                // des Cursor-Schadens, den der Editor gemeldet hat. Also
                // zusätzlich den Statusstreifen neu zeichnen lassen (kein
                // Baum-Neuaufbau — der Puffer ist geteilt).
                let titel = self.titel();
                if titel != self.letzter_titel {
                    self.letzter_titel = titel.clone();
                    AppReaktion::keine().mit_titel(titel).mit_status_neu()
                } else {
                    AppReaktion::keine().mit_status_neu()
                }
            }
            // Schließen-Dialog:
            N_SCHL_SPEICHERN => self.speichern(true),
            N_SCHL_VERWERFEN => AppReaktion::schliessen(),
            N_SCHL_ABBRECHEN => {
                self.dialog = None;
                self.reaktion_mit_titel()
            }
            // Fehler-Dialog: OK schließt ihn (die Statuszeile
            // erinnert weiter an den Fehler).
            N_FEHLER_OK => {
                if matches!(self.dialog, Some(Dialog::Fehler(_))) {
                    self.dialog = None;
                }
                self.reaktion_mit_titel()
            }
            _ => AppReaktion::keine(),
        }
    }

    /// App-Shortcuts + Dialog-Tasten (VOR dem Widget-Baum).
    fn taste(&mut self, taste: DecodedKey) -> Option<AppReaktion> {
        // Offene Dialoge bekommen die Tasten zuerst:
        if let Some(Dialog::Oeffnen(dialog)) = &mut self.dialog {
            match dialog.taste(taste) {
                Some(DateiDialogErgebnis::Gewaehlt(pfad)) => {
                    self.dialog = None;
                    self.laden(&pfad);
                }
                Some(DateiDialogErgebnis::Abgebrochen) => self.dialog = None,
                None => {}
            }
            return Some(self.reaktion_mit_titel());
        }
        if let Some(Dialog::Speichern { dialog, danach_schliessen }) = &mut self.dialog {
            let danach_schliessen = *danach_schliessen;
            match dialog.taste(taste) {
                Some(DateiDialogErgebnis::Gewaehlt(pfad)) => {
                    self.dialog = None;
                    if self.speichern_nach(&pfad) && danach_schliessen {
                        return Some(AppReaktion::schliessen());
                    }
                }
                Some(DateiDialogErgebnis::Abgebrochen) => self.dialog = None,
                None => {}
            }
            return Some(self.reaktion_mit_titel());
        }
        if let Some(Dialog::SchliessenFrage) = &self.dialog {
            // Esc = Abbrechen; alles andere gehört den Knöpfen.
            if taste == DecodedKey::Unicode('\u{1b}') {
                self.dialog = None;
                return Some(self.reaktion_mit_titel());
            }
            return Some(AppReaktion::keine());
        }
        if let Some(Dialog::Fehler(_)) = &self.dialog {
            // Enter/Esc wirken wie der OK-Knopf.
            if matches!(
                taste,
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') | DecodedKey::Unicode('\u{1b}')
            ) {
                self.dialog = None;
                return Some(self.reaktion_mit_titel());
            }
            return Some(AppReaktion::keine());
        }

        // Editor-Shortcuts (MapLettersToUnicode: Strg+S = U+0013,
        // Strg+O = U+000F):
        match taste {
            DecodedKey::Unicode('\u{13}') => Some(self.speichern(false)),
            DecodedKey::Unicode('\u{f}') => {
                let start = match &self.pfad {
                    Some(pfad) => crate::explorer::eltern_pfad(pfad),
                    None => String::from("/"),
                };
                self.dialog = Some(Dialog::Oeffnen(DateiDialog::neu(
                    "Datei oeffnen",
                    &start,
                    "",
                    N_DIALOG_BASIS,
                )));
                Some(self.reaktion_mit_titel())
            }
            _ => None, // alles andere an den Editor (Widget-Baum)
        }
    }

    /// X-Knopf: Bei ungespeicherten Änderungen NICHT schließen,
    /// sondern den Nachfrage-Dialog aufbauen (ehrliche Grenze des
    /// kooperativen Schließens — die App entscheidet später).
    fn schliessen_abfragen(&mut self) -> Option<AppReaktion> {
        if !self.mit_puffer(|p| p.geaendert) {
            return None; // nichts zu verlieren -> sofort schließen
        }
        self.dialog = Some(Dialog::SchliessenFrage);
        Some(self.reaktion_mit_titel())
    }
}

/// Start-Funktionen für Registry und Explorer.
pub fn starten() {
    crate::fenster::app_starten(Box::new(SpeedTextApp::neu()), 560, 420);
}

pub fn starten_mit(pfad: &str) {
    crate::fenster::app_starten(Box::new(SpeedTextApp::mit_datei(pfad)), 560, 420);
}

// ---------------------------------------------------------------------------
// Tests — Datei-Roundtrip und die Schließen-Dialog-Logik
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Datei-Roundtrip übers echte Test-VFS: laden, ändern, Strg+S,
    /// neu laden — der Text stimmt, der Stern kommt und geht.
    #[test_case]
    fn test_datei_roundtrip() {
        fs::mit_fs(|f| f.schreiben("/speedtext_test.txt", b"Zeile 1\nZeile 2")).unwrap();

        let mut app = SpeedTextApp::mit_datei("/speedtext_test.txt");
        assert_eq!(app.mit_puffer(|p| p.als_text()), "Zeile 1\nZeile 2");
        assert_eq!(app.titel(), "speedtext_test.txt - SpeedText");

        // Ändern -> Stern im Titel; Strg+S -> gespeichert, Stern weg.
        app.mit_puffer(|p| {
            p.ende();
            p.einfuegen('!');
        });
        assert_eq!(app.titel(), "speedtext_test.txt * - SpeedText");
        let _ = app.taste(DecodedKey::Unicode('\u{13}')); // Strg+S
        assert_eq!(app.titel(), "speedtext_test.txt - SpeedText");
        assert_eq!(
            fs::mit_fs(|f| f.lesen("/speedtext_test.txt")).unwrap(),
            b"Zeile 1!\nZeile 2"
        );

        // Frisch laden bestätigt den Roundtrip:
        let app2 = SpeedTextApp::mit_datei("/speedtext_test.txt");
        assert_eq!(app2.mit_puffer(|p| p.als_text()), "Zeile 1!\nZeile 2");

        fs::mit_fs(|f| f.loeschen("/speedtext_test.txt")).unwrap();
    }

    /// Die Schließen-Logik: ohne Änderungen schließt das X sofort;
    /// mit Änderungen kommt der Dialog — Verwerfen schließt,
    /// Abbrechen kehrt zum Editor zurück, Speichern (mit Pfad)
    /// speichert UND schließt.
    #[test_case]
    fn test_schliessen_dialog_logik() {
        let mut app = SpeedTextApp::neu();
        assert!(app.schliessen_abfragen().is_none()); // ungeändert -> zu

        app.mit_puffer(|p| p.einfuegen('x'));
        let reaktion = app.schliessen_abfragen().expect("Dialog muss abfangen");
        assert!(reaktion.neu_aufbauen && !reaktion.schliessen);
        assert!(matches!(app.dialog, Some(Dialog::SchliessenFrage)));

        // Abbrechen: Dialog weg, Fenster bleibt.
        assert!(!app.nachricht(N_SCHL_ABBRECHEN).schliessen);
        assert!(app.dialog.is_none());

        // Verwerfen: Fenster schließt (Reaktion sagt es dem Manager).
        app.dialog = Some(Dialog::SchliessenFrage);
        assert!(app.nachricht(N_SCHL_VERWERFEN).schliessen);

        // Speichern mit bekanntem Pfad: schreibt UND schließt.
        fs::mit_fs(|f| f.schreiben("/speedtext_zu.txt", b"alt")).unwrap();
        let mut app = SpeedTextApp::mit_datei("/speedtext_zu.txt");
        app.mit_puffer(|p| {
            p.ende();
            p.einfuegen('!');
        });
        app.dialog = Some(Dialog::SchliessenFrage);
        assert!(app.nachricht(N_SCHL_SPEICHERN).schliessen);
        assert_eq!(fs::mit_fs(|f| f.lesen("/speedtext_zu.txt")).unwrap(), b"alt!");
        fs::mit_fs(|f| f.loeschen("/speedtext_zu.txt")).unwrap();
    }

    /// Fehler werden ANGEZEIGT, nicht verschluckt: Ein Speicherfehler
    /// (fehlendes Eltern-Verzeichnis) öffnet den Fehler-Dialog; OK
    /// schließt ihn, die Statuszeilen-Meldung bleibt als Erinnerung.
    #[test_case]
    fn test_fehler_dialog_bei_speicherfehler() {
        let mut app = SpeedTextApp::neu();
        app.mit_puffer(|p| p.einfuegen('x'));
        app.pfad = Some(String::from("/gibtsnicht/kaputt.txt"));

        let _ = app.speichern(false);
        assert!(matches!(app.dialog, Some(Dialog::Fehler(_))));
        assert!(app.meldung.as_deref().unwrap_or("").starts_with("Speichern:"));

        let _ = app.nachricht(N_FEHLER_OK);
        assert!(app.dialog.is_none());
        assert!(app.meldung.is_some());

        // Auch das Laden einer fehlenden Datei meldet sich per Dialog:
        let app = SpeedTextApp::mit_datei("/gibtsnicht/fehlt.txt");
        assert!(matches!(app.dialog, Some(Dialog::Fehler(_))));
    }
}
