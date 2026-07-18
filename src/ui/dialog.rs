// ui/dialog.rs — Wiederverwendbare Dialog-Bausteine
//
// Zwei Dialoge, die JEDE App nutzen kann (SpeedText ist der erste
// Kunde, der Explorer der nächste):
//
//   * `bestaetigung()` — der generische Nachfrage-Dialog: eine Frage
//     plus Knopfzeile (Beschriftung -> Nachricht-Id). Er ist bewusst
//     nur eine BAUM-FUNKTION: Der Dialog ERSETZT den Fenster-Inhalt
//     im aufbau() der App (kein Overlay-Mechanismus nötig — die App
//     merkt sich "Dialog offen" als Zustand und baut entsprechend).
//   * `DateiDialog` — Öffnen/Speichern-Dialog: ScrollListe des
//     aktuellen Ordners + Pfad-Eingabezeile + OK/Abbrechen. Er ist
//     ein ZUSTANDS-Baustein (Struct), denn Ordner, Einträge und die
//     getippte Eingabe müssen Neu-Aufbauten überleben — die App
//     bettet ihn ein und reicht Nachrichten/Tasten durch. Die
//     Eingabezeile puffert der Dialog selbst (App::taste-Hook,
//     dasselbe Muster wie die Explorer-Adresszeile — das Textfeld-
//     Widget würde seinen Text beim Neu-Aufbau verlieren).

use super::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use super::{hbox, vbox, w, Fueller, Widget};
use crate::fs::{self, NodeTyp};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use pc_keyboard::DecodedKey;

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
    /// Seine Einträge (Name, Typ) — Ordner zuerst, alphabetisch.
    eintraege: Vec<(String, NodeTyp)>,
    /// Die Pfad-/Namens-Eingabe (der Dialog puffert selbst).
    pub eingabe: String,
    auswahl: Option<usize>,
    /// Nachricht-Basis der einbettenden App.
    basis: u32,
}

impl DateiDialog {
    pub fn neu(titel: &'static str, ordner: &str, vorbelegung: &str, basis: u32) -> Self {
        let mut dialog = DateiDialog {
            titel,
            ordner: String::from(ordner),
            eintraege: Vec::new(),
            eingabe: String::from(vorbelegung),
            auswahl: None,
            basis,
        };
        dialog.neu_laden();
        dialog
    }

    /// Lädt die Ordner-Einträge aus dem VFS (mit_fs ist unter dem
    /// MANAGER-Lock erlaubt). Ordner zuerst, alphabetisch.
    fn neu_laden(&mut self) {
        self.eintraege = fs::mit_fs(|f| f.liste(&self.ordner))
            .map(|liste| liste.into_iter().map(|e| (e.name, e.typ)).collect())
            .unwrap_or_default();
        self.eintraege.sort_by(|a, b| {
            let ordnung = |e: &(String, NodeTyp)| e.1 != NodeTyp::Verzeichnis;
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
            .map(|(name, typ)| ListenEintrag {
                icon: Some(match typ {
                    NodeTyp::Verzeichnis => &crate::grafik::ICON_ORDNER,
                    NodeTyp::Datei => &crate::grafik::ICON_DATEI,
                }),
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
    fn eingabe_pfad(&self) -> Option<String> {
        if self.eingabe.trim().is_empty() {
            return None;
        }
        Some(fs::pfad_aufloesen(&self.ordner, self.eingabe.trim()))
    }

    /// Verarbeitet eine App-Nachricht, sofern sie im Id-Fenster des
    /// Dialogs liegt. Some(None) = verarbeitet, Dialog läuft weiter;
    /// Some(Some(ergebnis)) = fertig. None = Nachricht gehört nicht
    /// dem Dialog.
    pub fn nachricht(&mut self, id: u32) -> Option<Option<DateiDialogErgebnis>> {
        if !(self.basis..self.basis + DIALOG_ID_BREITE).contains(&id) {
            return None;
        }
        let id = id - self.basis;
        Some(match id {
            D_OK => self.eingabe_pfad().map(DateiDialogErgebnis::Gewaehlt),
            D_ABBRECHEN => Some(DateiDialogErgebnis::Abgebrochen),
            id if id >= D_OEFFNEN => {
                // Doppelklick/Enter: Ordner -> hinein; Datei -> nehmen.
                let index = (id - D_OEFFNEN) as usize;
                match self.eintraege.get(index) {
                    Some((name, NodeTyp::Verzeichnis)) => {
                        self.ordner = fs::pfad_anhaengen(&self.ordner, name);
                        self.neu_laden();
                        None
                    }
                    Some((name, NodeTyp::Datei)) => Some(DateiDialogErgebnis::Gewaehlt(
                        fs::pfad_anhaengen(&self.ordner, name),
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
    pub fn taste(&mut self, taste: DecodedKey) -> Option<DateiDialogErgebnis> {
        match taste {
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                return self.eingabe_pfad().map(DateiDialogErgebnis::Gewaehlt);
            }
            DecodedKey::Unicode('\u{1b}') => return Some(DateiDialogErgebnis::Abgebrochen),
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                self.eingabe.pop();
            }
            DecodedKey::Unicode(zeichen) if zeichen >= ' ' && self.eingabe.chars().count() < 60 => {
                self.eingabe.push(zeichen);
            }
            _ => {}
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Tests — die Dialog-Zustandsmaschine, ohne Fenster
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Datei-Dialog gegen das echte Test-VFS: Klick übernimmt den
    /// Namen, Doppelklick navigiert in Ordner bzw. wählt Dateien,
    /// OK löst relativ zum Ordner auf, Esc bricht ab.
    #[test_case]
    fn test_datei_dialog_logik() {
        let _ = fs::mit_fs(|f| f.mkdir("/dialogtest"));
        fs::mit_fs(|f| f.schreiben("/dialogtest/notiz.txt", b"x")).unwrap();

        let basis = 50_000;
        let mut dialog = DateiDialog::neu("Datei oeffnen", "/", "", basis);
        // Fremde Ids gehen den Dialog nichts an:
        assert_eq!(dialog.nachricht(basis - 1), None);

        // Der Testordner ist in der Liste (Ordner werden zuerst
        // sortiert); Index suchen und ANKLICKEN -> Eingabe = Name:
        let index = dialog
            .eintraege
            .iter()
            .position(|(name, _)| name == "dialogtest")
            .expect("Testordner fehlt in der Liste");
        assert_eq!(dialog.nachricht(basis + D_LISTE + index as u32), Some(None));
        assert_eq!(dialog.eingabe, "dialogtest");

        // DOPPELKLICK auf den Ordner navigiert hinein:
        assert_eq!(dialog.nachricht(basis + D_OEFFNEN + index as u32), Some(None));
        assert_eq!(dialog.ordner, "/dialogtest");
        let datei_index = dialog
            .eintraege
            .iter()
            .position(|(name, _)| name == "notiz.txt")
            .expect("Datei fehlt nach der Navigation");

        // Doppelklick auf die DATEI liefert den absoluten Pfad:
        assert_eq!(
            dialog.nachricht(basis + D_OEFFNEN + datei_index as u32),
            Some(Some(DateiDialogErgebnis::Gewaehlt(String::from(
                "/dialogtest/notiz.txt"
            ))))
        );

        // Tippen + OK: relative Eingabe wird zum Ordner aufgelöst.
        dialog.eingabe.clear();
        for zeichen in "neu.txt".chars() {
            assert_eq!(dialog.taste(DecodedKey::Unicode(zeichen)), None);
        }
        assert_eq!(
            dialog.nachricht(basis + D_OK),
            Some(Some(DateiDialogErgebnis::Gewaehlt(String::from(
                "/dialogtest/neu.txt"
            ))))
        );
        // Enter wirkt wie OK, Esc bricht ab, leere Eingabe tut nichts:
        assert_eq!(
            dialog.taste(DecodedKey::Unicode('\n')),
            Some(DateiDialogErgebnis::Gewaehlt(String::from("/dialogtest/neu.txt")))
        );
        assert_eq!(
            dialog.taste(DecodedKey::Unicode('\u{1b}')),
            Some(DateiDialogErgebnis::Abgebrochen)
        );
        dialog.eingabe.clear();
        assert_eq!(dialog.nachricht(basis + D_OK), Some(None));

        fs::loeschen_rekursiv("/dialogtest").unwrap();
    }
}
