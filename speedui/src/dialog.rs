// speedui::dialog — Wiederverwendbare Dialog-Bausteine
//
// Zwei Dialoge, die JEDE App nutzen kann:
//
//   * `bestaetigung()` — der generische Nachfrage-Dialog: eine Frage plus
//     Knopfzeile (Beschriftung -> Nachricht-Id). Er ist bewusst nur eine
//     BAUM-FUNKTION: Der Dialog ERSETZT den Fenster-Inhalt im `aufbau()`
//     der App (kein Overlay-Mechanismus noetig — die App merkt sich
//     „Dialog offen" als Zustand und baut entsprechend).
//   * `DateiDialog` — Oeffnen/Speichern: ScrollListe des aktuellen Ordners
//     + Pfad-Eingabezeile + OK/Abbrechen. Ein ZUSTANDS-Baustein (Struct),
//     denn Ordner, Eintraege und die getippte Eingabe muessen
//     Neu-Aufbauten ueberleben.
//
// ==========================================================================
// DIE KOPPLUNG, DIE HIER AUFGELOEST WURDE
//
// Der Datei-Dialog las bis Serie 8, Teil 2 direkt aus dem VFS
// (`fs::mit_fs(|f| f.liste(...))`) und rechnete Pfade mit
// `fs::pfad_anhaengen` / `fs::pfad_aufloesen` zusammen. Ein Toolkit, das
// ein Dateisystem kennt, ist kein Toolkit.
//
// Jetzt bekommt er eine `&dyn Dateiquelle` gereicht — DREI Methoden, davon
// zwei reine Stringarbeit. Dass auch die mitgehen, ist Absicht: Was ein
// Pfad IST (`/` als Trenner, `..`, Mount-Praefixe), ist eine Eigenschaft
// des Wirts. Der Kernel reicht sein VFS herein, ein Prozess seine
// Datei-Syscalls, ein Test eine feste Liste.
//
// Ebenfalls neu: Ordner und Datei werden mit einem `bool` unterschieden
// statt mit `fs::NodeTyp` — der TYP gehoert dem Kernel, das Wissen
// „Ordner oder nicht" dem Dialog.

use crate::typen::{Icon, Taste};
use crate::umgebung::Dateiquelle;
use crate::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::{hbox, vbox, w, Fueller, Widget};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Baut den generischen Bestätigungs-Dialog: Frage (mehrzeilig ok)
/// plus eine Knopfzeile. Die Klicks landen als ganz normale
/// Nachrichten in App::nachricht.
pub fn bestaetigung(frage: &str, knoepfe: &[(&str, u32)]) -> Box<dyn Widget> {
    let mut zeile: Vec<Box<dyn Widget>> = Vec::new();
    for (text, id) in knoepfe {
        zeile.push(w(Button::neu(text, *id)));
    }
    zeile.push(w(Fueller));
    w(vbox(vec![w(Label::neu(frage)), w(Trennlinie), w(hbox(zeile))]))
}

/// Der FEHLER-Dialog: eine Meldung + OK-Knopf — die Standard-Antwort
/// einer App auf einen FsFehler/IoFehler (Daten-Integritäts-Regel:
/// Fehler werden ANGEZEIGT, nie verschluckt). Nur ein dünner Mantel
/// um bestaetigung(); OK schickt `ok_id` an App::nachricht, die App
/// schließt damit ihren Dialog-Zustand.
pub fn fehler(meldung: &str, ok_id: u32) -> Box<dyn Widget> {
    bestaetigung(&format!("Fehler: {}", meldung), &[("OK", ok_id)])
}

// ---------------------------------------------------------------------------
// DateiDialog
// ---------------------------------------------------------------------------

/// Nachricht-Offsets RELATIV zur Basis, die die App dem Dialog gibt
/// (Basis weit weg von den eigenen Ids wählen!).
const D_OK: u32 = 0;
const D_ABBRECHEN: u32 = 1;
const D_LISTE: u32 = 100; // + Eintrag-Index (Klick = auswählen)
const D_OEFFNEN: u32 = 10_000; // + Eintrag-Index (Doppelklick/Enter)
/// Wie viele Ids der Dialog ab seiner Basis belegt.
pub const DIALOG_ID_BREITE: u32 = 20_000;

/// Was der Dialog der App meldet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateiDialogErgebnis {
    /// Der Nutzer hat einen ABSOLUTEN Pfad gewählt (OK/Enter/
    /// Doppelklick auf eine Datei).
    Gewaehlt(String),
    Abgebrochen,
}

pub struct DateiDialog {
    /// Überschrift ("Datei oeffnen" / "Speichern unter").
    pub titel: &'static str,
    /// Der angezeigte Ordner.
    ordner: String,
    /// Seine Einträge (Name, ist_ordner) — Ordner zuerst, alphabetisch.
    eintraege: Vec<(String, bool)>,
    /// Die Pfad-/Namens-Eingabe (der Dialog puffert selbst).
    pub eingabe: String,
    auswahl: Option<usize>,
    /// Nachricht-Basis der einbettenden App.
    basis: u32,
    /// Icons für Ordner und Dateien — der WIRT liefert sie, denn sie
    /// gehören zu seinem Erscheinungsbild (der Kernel hat andere als ein
    /// Prozess, und ein Prozess hat vielleicht gar keine).
    icon_ordner: Option<&'static Icon>,
    icon_datei: Option<&'static Icon>,
}

impl DateiDialog {
    pub fn neu(
        titel: &'static str,
        ordner: &str,
        vorbelegung: &str,
        basis: u32,
        quelle: &dyn Dateiquelle,
    ) -> Self {
        let mut dialog = DateiDialog {
            titel,
            ordner: String::from(ordner),
            eintraege: Vec::new(),
            eingabe: String::from(vorbelegung),
            auswahl: None,
            basis,
            icon_ordner: None,
            icon_datei: None,
        };
        dialog.neu_laden(quelle);
        dialog
    }

    /// Builder: die Icons des Wirts für Ordner und Dateien.
    pub fn mit_icons(mut self, ordner: &'static Icon, datei: &'static Icon) -> Self {
        self.icon_ordner = Some(ordner);
        self.icon_datei = Some(datei);
        self
    }

    /// Lädt die Ordner-Einträge über die DATEIQUELLE DES WIRTS.
    /// Ordner zuerst, alphabetisch.
    fn neu_laden(&mut self, quelle: &dyn Dateiquelle) {
        self.eintraege = quelle.liste(&self.ordner);
        self.eintraege.sort_by(|a, b| {
            let ordnung = |e: &(String, bool)| !e.1;
            ordnung(a)
                .cmp(&ordnung(b))
                .then_with(|| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()))
        });
        self.auswahl = None;
    }

    /// Der Widget-Baum des Dialogs (die App ruft das in aufbau()).
    pub fn aufbau(&self) -> Box<dyn Widget> {
        let eintraege = self
            .eintraege
            .iter()
            .map(|(name, ist_ordner)| ListenEintrag {
                icon: if *ist_ordner { self.icon_ordner } else { self.icon_datei },
                text: name.clone(),
            })
            .collect();
        let liste =
            ScrollListe::mit_index_nachrichten(eintraege, self.basis + D_LISTE, self.basis + D_OEFFNEN)
                .mit_auswahl(self.auswahl);

        w(vbox(vec![
            w(Label::neu(self.titel)),
            w(Label::sekundaer(&format!("Ordner: {}", self.ordner))),
            w(liste),
            w(Label::neu(&format!("> {}_", self.eingabe))),
            w(hbox(vec![
                w(Button::neu("OK", self.basis + D_OK)),
                w(Button::neu("Abbrechen", self.basis + D_ABBRECHEN)),
                w(Fueller),
            ])),
            w(Label::sekundaer(
                "Doppelklick: Ordner oeffnen / Datei waehlen - Enter = OK, Esc = Abbrechen",
            )),
        ]))
    }

    /// Der gewählte absolute Pfad aus der aktuellen Eingabe.
    fn eingabe_pfad(&self, quelle: &dyn Dateiquelle) -> Option<String> {
        if self.eingabe.trim().is_empty() {
            return None;
        }
        Some(quelle.aufloesen(&self.ordner, self.eingabe.trim()))
    }

    /// Verarbeitet eine App-Nachricht, sofern sie im Id-Fenster des
    /// Dialogs liegt. Some(None) = verarbeitet, Dialog läuft weiter;
    /// Some(Some(ergebnis)) = fertig. None = Nachricht gehört nicht
    /// dem Dialog.
    pub fn nachricht(
        &mut self,
        id: u32,
        quelle: &dyn Dateiquelle,
    ) -> Option<Option<DateiDialogErgebnis>> {
        if !(self.basis..self.basis + DIALOG_ID_BREITE).contains(&id) {
            return None;
        }
        let id = id - self.basis;
        Some(match id {
            D_OK => self.eingabe_pfad(quelle).map(DateiDialogErgebnis::Gewaehlt),
            D_ABBRECHEN => Some(DateiDialogErgebnis::Abgebrochen),
            id if id >= D_OEFFNEN => {
                // Doppelklick/Enter: Ordner -> hinein; Datei -> nehmen.
                let index = (id - D_OEFFNEN) as usize;
                match self.eintraege.get(index) {
                    Some((name, true)) => {
                        self.ordner = quelle.anhaengen(&self.ordner, name);
                        self.neu_laden(quelle);
                        None
                    }
                    Some((name, false)) => Some(DateiDialogErgebnis::Gewaehlt(
                        quelle.anhaengen(&self.ordner, name),
                    )),
                    None => None,
                }
            }
            id if id >= D_LISTE => {
                // Klick: auswählen und den Namen in die Eingabe legen.
                let index = (id - D_LISTE) as usize;
                if let Some((name, _)) = self.eintraege.get(index) {
                    self.auswahl = Some(index);
                    self.eingabe = name.clone();
                }
                None
            }
            _ => None,
        })
    }

    /// Tasten im Dialog-Modus (die App reicht sie aus ihrem
    /// taste-Hook durch): Tippen in die Eingabe, Enter = OK,
    /// Esc = Abbrechen, ".." per Backspace-auf-leerer-Eingabe wäre
    /// Übertreibung — der Doppelklick auf ".."-lose Ordner reicht.
    pub fn taste(
        &mut self,
        taste: Taste,
        quelle: &dyn Dateiquelle,
    ) -> Option<DateiDialogErgebnis> {
        match taste {
            Taste::Zeichen('\n') | Taste::Zeichen('\r') => {
                return self.eingabe_pfad(quelle).map(DateiDialogErgebnis::Gewaehlt);
            }
            Taste::Zeichen('\u{1b}') => return Some(DateiDialogErgebnis::Abgebrochen),
            Taste::Zeichen('\u{8}') | Taste::Zeichen('\u{7f}') => {
                self.eingabe.pop();
            }
            Taste::Zeichen(zeichen) if zeichen >= ' ' && self.eingabe.chars().count() < 60 => {
                self.eingabe.push(zeichen);
            }
            _ => {}
        }
        None
    }
}


// ---------------------------------------------------------------------------
// Tests — die Dialog-Zustandsmaschine, ohne Fenster UND ohne Dateisystem
//
// Die alten Tests brauchten ein gemountetes VFS und liefen deshalb in QEMU.
// Mit dem `Dateiquelle`-Trait reicht eine feste Liste — genau dafuer ist
// die Umkehr da.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrappe::TestDateien;

    fn quelle() -> TestDateien {
        TestDateien::neu()
            .mit("/dialogtest", "unterordner", true)
            .mit("/dialogtest", "notiz.txt", false)
            .mit("/dialogtest/unterordner", "tief.txt", false)
    }

    /// Klick uebernimmt den Namen in die Eingabe, Doppelklick navigiert in
    /// Ordner bzw. waehlt Dateien, OK loest relativ zum Ordner auf.
    #[test]
    fn test_datei_dialog_navigiert() {
        let q = quelle();
        let mut dialog = DateiDialog::neu("Test", "/dialogtest", "", 1000, &q);

        // Sortierung: Ordner zuerst, dann alphabetisch.
        assert_eq!(dialog.eintraege[0].0, "unterordner");
        assert_eq!(dialog.eintraege[1].0, "notiz.txt");

        // Klick auf die Datei (Index 1) legt ihren Namen in die Eingabe.
        assert_eq!(dialog.nachricht(1000 + 100 + 1, &q), Some(None));
        assert_eq!(dialog.eingabe, "notiz.txt");

        // OK loest gegen den Ordner auf.
        assert_eq!(
            dialog.nachricht(1000, &q),
            Some(Some(DateiDialogErgebnis::Gewaehlt(String::from(
                "/dialogtest/notiz.txt"
            ))))
        );

        // Doppelklick auf den ORDNER (Index 0) navigiert hinein.
        assert_eq!(dialog.nachricht(1000 + 10_000, &q), Some(None));
        assert_eq!(dialog.eintraege.len(), 1);
        assert_eq!(dialog.eintraege[0].0, "tief.txt");

        // Doppelklick auf eine DATEI waehlt sie.
        assert_eq!(
            dialog.nachricht(1000 + 10_000, &q),
            Some(Some(DateiDialogErgebnis::Gewaehlt(String::from(
                "/dialogtest/unterordner/tief.txt"
            ))))
        );
    }

    /// Tasten: Tippen fuellt die Eingabe, Enter waehlt, Esc bricht ab —
    /// und Nachrichten AUSSERHALB des Id-Fensters gehen den Dialog nichts an.
    #[test]
    fn test_datei_dialog_tasten_und_fremde_nachrichten() {
        let q = quelle();
        let mut dialog = DateiDialog::neu("Test", "/dialogtest", "", 1000, &q);

        // Eine Nachricht unterhalb und oberhalb der Basis: nicht meins.
        assert_eq!(dialog.nachricht(999, &q), None);
        assert_eq!(dialog.nachricht(1000 + DIALOG_ID_BREITE, &q), None);

        for zeichen in "neu.txt".chars() {
            assert_eq!(dialog.taste(Taste::Zeichen(zeichen), &q), None);
        }
        assert_eq!(dialog.eingabe, "neu.txt");
        // Rueckschritt loescht.
        dialog.taste(Taste::Zeichen('\u{8}'), &q);
        assert_eq!(dialog.eingabe, "neu.tx");

        // Enter waehlt den aufgeloesten Pfad.
        assert_eq!(
            dialog.taste(Taste::Zeichen('\n'), &q),
            Some(DateiDialogErgebnis::Gewaehlt(String::from("/dialogtest/neu.tx")))
        );
        // Esc bricht ab.
        assert_eq!(
            dialog.taste(Taste::Zeichen('\u{1b}'), &q),
            Some(DateiDialogErgebnis::Abgebrochen)
        );
    }

    /// Eine LEERE Eingabe ist kein gueltiger Pfad — OK darf dann nichts
    /// liefern statt den Ordner selbst zu waehlen.
    #[test]
    fn test_leere_eingabe_waehlt_nichts() {
        let q = quelle();
        let mut dialog = DateiDialog::neu("Test", "/dialogtest", "   ", 1000, &q);
        assert_eq!(dialog.nachricht(1000, &q), Some(None));
        assert_eq!(dialog.taste(Taste::Zeichen('\n'), &q), None);
    }
}
