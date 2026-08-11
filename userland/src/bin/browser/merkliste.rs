// browser::merkliste — Lesezeichen und Startseite, dauerhaft
//
// ===========================================================================
// EINE DATEI, ZEILENWEISE, LESBAR
//
//     start\tspeedos:info
//     https://example.com/\tExample Domain
//     /platte/seiten/cern.html\tWorld Wide Web
//
// Tabulator als Trenner, weil er in URLs nicht vorkommt und in Titeln
// nichts zu suchen hat (er wird beim Speichern entfernt). Kein JSON, kein
// eigenes Binaerformat: Wer die Datei mit `cat` ansieht, versteht sie —
// und wer sie mit SpeedText bearbeitet, kaputt sie nicht.
//
// KAPUTTE ZEILEN WERDEN UEBERSPRUNGEN, nicht abgelehnt. Dieselbe Haltung
// wie beim PEM-Parser (Serie 7, Teil 2): Ein Lesezeichen weniger ist
// brauchbar, eine Datei, die bei einem Zeilenumbruch auf null faellt,
// ist eine Ausfallquelle.

use alloc::string::String;
use alloc::vec::Vec;

/// Wo die Datei liegt. `/platte` ueberlebt den Neustart.
pub const PFAD: &str = "/platte/system/lesezeichen.txt";
/// Ersatzort, wenn keine Platte gemountet ist (RAM, weg beim Neustart).
pub const PFAD_RAM: &str = "/system/lesezeichen.txt";

/// Hoechstzahl der Lesezeichen — eine Datei, die man von Hand pflegt,
/// wird nicht laenger.
pub const MAX: usize = 200;

pub struct Merkliste {
    pub eintraege: Vec<(String, String)>,
    pub startseite: String,
    /// Wo wirklich gespeichert wurde (fuer die Anzeige).
    pub pfad: String,
}

impl Merkliste {
    /// Laedt die Liste — oder liefert eine leere, wenn es sie nicht gibt.
    ///
    /// **Kein Fehler beim ersten Start.** Eine fehlende Datei ist der
    /// Normalfall, kein Problem.
    pub fn laden() -> Merkliste {
        let mut liste = Merkliste {
            eintraege: Vec::new(),
            startseite: String::from("speedos:info"),
            pfad: String::from(PFAD),
        };
        let (bytes, pfad) = match libspeed::netz::datei_lesen(PFAD) {
            Ok(b) => (b, String::from(PFAD)),
            Err(_) => match libspeed::netz::datei_lesen(PFAD_RAM) {
                Ok(b) => (b, String::from(PFAD_RAM)),
                Err(_) => return liste,
            },
        };
        liste.pfad = pfad;
        let text = String::from_utf8_lossy(&bytes);
        for zeile in text.lines() {
            let zeile = zeile.trim();
            if zeile.is_empty() || zeile.starts_with('#') {
                continue;
            }
            let (schluessel, wert) = match zeile.split_once('\t') {
                Some(paar) => paar,
                // Eine Zeile ohne Trenner ist eine Adresse ohne Titel —
                // brauchbar, also wird sie genommen.
                None => (zeile, ""),
            };
            if schluessel == "start" {
                if !wert.is_empty() {
                    liste.startseite = String::from(wert);
                }
                continue;
            }
            if liste.eintraege.len() < MAX {
                liste
                    .eintraege
                    .push((String::from(schluessel), String::from(wert)));
            }
        }
        liste
    }

    /// Schreibt die Liste zurueck. Liefert `false`, wenn es nicht ging.
    pub fn speichern(&mut self) -> bool {
        let mut text = String::from(
            "# SpeedOS-Browser: Lesezeichen. Eine Zeile je Eintrag:\n\
             # <adresse>\\t<titel>. Die Zeile 'start' setzt die Startseite.\n",
        );
        text.push_str("start\t");
        text.push_str(&self.startseite);
        text.push('\n');
        for (adresse, titel) in &self.eintraege {
            text.push_str(adresse);
            text.push('\t');
            text.push_str(titel);
            text.push('\n');
        }
        // Erst auf die Platte, sonst in den RAM — dasselbe Muster wie
        // `fs::persistenter_pfad` im Kernel.
        for pfad in [PFAD, PFAD_RAM] {
            if libspeed::netz::speichern(pfad, text.as_bytes()).is_ok() {
                self.pfad = String::from(pfad);
                return true;
            }
        }
        false
    }

    /// Ist diese Adresse schon gemerkt?
    pub fn kennt(&self, adresse: &str) -> bool {
        self.eintraege.iter().any(|(a, _)| a == adresse)
    }

    /// Setzt oder entfernt ein Lesezeichen. Liefert `true`, wenn es
    /// danach gesetzt ist.
    ///
    /// UMSCHALTEN UND NICHT NUR HINZUFUEGEN: Derselbe Handgriff, der ein
    /// Lesezeichen setzt, nimmt es auch wieder weg — sonst braeuchte es
    /// einen zweiten Weg zum Loeschen, den niemand findet.
    pub fn umschalten(&mut self, adresse: &str, titel: &str) -> bool {
        if let Some(i) = self.eintraege.iter().position(|(a, _)| a == adresse) {
            self.eintraege.remove(i);
            self.speichern();
            return false;
        }
        if self.eintraege.len() >= MAX {
            self.eintraege.remove(0);
        }
        // Tabulatoren aus dem Titel — sie waeren der Trenner.
        let sauber: String = titel.chars().map(|z| if z == '\t' { ' ' } else { z }).collect();
        self.eintraege.push((String::from(adresse), sauber));
        self.speichern();
        true
    }
}
