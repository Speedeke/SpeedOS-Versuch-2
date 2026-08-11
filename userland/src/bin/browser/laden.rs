// browser::laden — Bytes besorgen, egal woher
//
// ===========================================================================
// DER GANZE NETZ-CODE DES BROWSERS STEHT IN DIESER DATEI, UND ES SIND
// DREI ZEILEN
//
// `libspeed::netz::Klient` ist seit Serie 7, Teil 5 die Stelle, an der ein
// Programm eine URL holt — mit Frist, Groessengrenze, Weiterleitungen und
// Zertifikatspruefung. Wer in einem neuen Programm DNS, Socket, Handshake
// und Rumpf-Schleife noch einmal schreibt, macht es falsch; genau dafuer
// gibt es die Schicht.
//
// Was hier dazukommt, ist deshalb nicht Netz-Logik, sondern:
//   * die Uebersetzung `AbrufFehler` -> ANZEIGBARE SEITE,
//   * ein Sitzungs-Cache, damit ein Bild nicht bei jedem Neu-Layout
//     erneut aus dem Netz kommt,
//   * die Unterscheidung, die alles andere ueberlagert: **war es ein
//     Sicherheitsfehler?**

use crate::ort::Ort;
use crate::seiten;
use alloc::string::String;
use alloc::vec::Vec;
use libspeed::netz::{AbrufFehler, Klient};

/// Obergrenze fuer ein Dokument.
pub const MAX_SEITE: usize = 4 * 1024 * 1024;
/// Obergrenze fuer ein Bild.
pub const MAX_BILD: usize = 2 * 1024 * 1024;
/// Frist je Abruf.
pub const FRIST_MS: u64 = 15_000;

/// Was beim Laden herauskam.
pub struct Geladen {
    /// Der Inhalt (HTML-Quelltext oder erzeugte Fehlerseite).
    pub bytes: Vec<u8>,
    /// Wo die Seite WIRKLICH herkam — nach Weiterleitungen.
    pub ort: Ort,
    /// `Some`, wenn eine Fehlerseite erzeugt wurde. Der Text ist die
    /// Kurzform fuer die Statuszeile.
    pub fehler: Option<String>,
    /// War es ein SICHERHEITSfehler? Dann faerbt sich die Adressleiste,
    /// und es gibt (bewusst) keinen Weg weiter.
    pub unsicher: bool,
}

impl Geladen {
    fn seite(bytes: Vec<u8>, ort: Ort) -> Geladen {
        Geladen {
            bytes,
            ort,
            fehler: None,
            unsicher: false,
        }
    }
}

// ===========================================================================
// DER CACHE
// ===========================================================================

/// Ein Sitzungs-Cache: Adresse -> Bytes.
///
/// ===================================================================
/// BEWUSST EINFACH, UND BEWUSST NUR FUER DIE SITZUNG
///
/// Kein `Cache-Control`, kein `ETag`, kein Verfallsdatum, nichts auf der
/// Platte. Er beantwortet genau eine Frage: „Habe ich das in DIESER
/// Sitzung schon geholt?" — und die stellt sich dauernd, weil ein
/// Reflow dasselbe Bild erneut braucht und ein `Zurueck` dieselbe Seite.
///
/// EIN ECHTER HTTP-CACHE WAERE ETWAS ANDERES: Er muesste Frische
/// beurteilen, und eine falsche Antwort aus dem Cache ist schwerer zu
/// finden als eine langsame Verbindung. Was hier steht, kann nicht
/// veralten, weil es die Sitzung nicht ueberlebt.
///
/// VERDRAENGT WIRD DER AELTESTE EINTRAG (kein LRU): Der Unterschied
/// waere bei einer Handvoll Bildern je Seite nicht messbar, und ein
/// Ringpuffer ist eine Zeile.
pub struct Cache {
    eintraege: Vec<(String, Vec<u8>)>,
    bytes: usize,
    max_bytes: usize,
    pub treffer: u32,
    pub verfehlt: u32,
}

impl Cache {
    pub fn neu(max_bytes: usize) -> Cache {
        Cache {
            eintraege: Vec::new(),
            bytes: 0,
            max_bytes,
            treffer: 0,
            verfehlt: 0,
        }
    }

    pub fn holen(&mut self, schluessel: &str) -> Option<&[u8]> {
        // Zwei Durchlaeufe, weil der Zaehler `&mut self` braucht und die
        // Rueckgabe `&self` festhaelt.
        let da = self.eintraege.iter().any(|(k, _)| k == schluessel);
        if da {
            self.treffer += 1;
        } else {
            self.verfehlt += 1;
            return None;
        }
        self.eintraege
            .iter()
            .find(|(k, _)| k == schluessel)
            .map(|(_, v)| v.as_slice())
    }

    pub fn legen(&mut self, schluessel: &str, daten: &[u8]) {
        if daten.len() > self.max_bytes {
            return; // passt nie hinein — gar nicht erst verdraengen
        }
        if self.eintraege.iter().any(|(k, _)| k == schluessel) {
            return;
        }
        while self.bytes + daten.len() > self.max_bytes && !self.eintraege.is_empty() {
            let (_, weg) = self.eintraege.remove(0);
            self.bytes -= weg.len();
        }
        self.bytes += daten.len();
        self.eintraege.push((String::from(schluessel), daten.to_vec()));
    }
}

// ===========================================================================
// LADEN
// ===========================================================================

/// Eine Seite holen. **Liefert IMMER etwas Anzeigbares** — ein Fehler
/// wird zu einer Fehlerseite, nicht zu einem leeren Fenster.
pub fn seite_laden(ort: &Ort, klient: &mut Klient, cache: &mut Cache) -> Geladen {
    match ort {
        Ort::Intern(seite) => Geladen::seite(interne_seite(*seite), ort.clone()),

        Ort::Datei(pfad) => match libspeed::netz::datei_lesen(pfad) {
            Ok(bytes) => Geladen::seite(bytes, ort.clone()),
            Err(fehler) => Geladen {
                bytes: seiten::fehler(
                    "Datei nicht lesbar",
                    fehler.text(),
                    pfad,
                )
                .into_bytes(),
                ort: ort.clone(),
                fehler: Some(String::from(fehler.text())),
                unsicher: false,
            },
        },

        Ort::Netz(ziel) => {
            let adresse = ziel.als_text();
            // Aus dem Cache? Nur, wenn wir sie in dieser Sitzung schon
            // hatten — beim ZURUECKGEHEN ist das der Normalfall.
            if let Some(bytes) = cache.holen(&adresse) {
                return Geladen::seite(bytes.to_vec(), ort.clone());
            }
            klient.frist_ms = FRIST_MS;
            klient.max_bytes = MAX_SEITE;
            match klient.holen(&adresse) {
                Ok(abruf) => {
                    let rumpf = abruf.antwort.rumpf;
                    cache.legen(&adresse, &rumpf);
                    // NACH Weiterleitungen ist die Adresse eine andere —
                    // und die gehoert in die Adressleiste und in den
                    // Verlauf, nicht die eingetippte.
                    Geladen::seite(rumpf, Ort::Netz(abruf.ziel))
                }
                Err(fehler) => fehlerseite(&fehler, &adresse, ort),
            }
        }
    }
}

/// Ein Bild oder Stylesheet holen — mit Cache, ohne Fehlerseite.
///
/// ===================================================================
/// GRENZE UND FRIST SIND ARGUMENTE UND KEINE KONSTANTEN
///
/// Ein Bild und ein Stylesheet sind beides „Nebensachen" — sie duerfen
/// die Seite nicht aufhalten und nicht verhindern. Aber sie sind
/// verschieden teuer und verschieden wichtig: Ein Bild darf 2 MiB gross
/// sein und 15 s brauchen, ein Stylesheet soll nach 8 s aufgeben, weil
/// es NACH dem Dokument geholt wird und der Benutzer solange auf eine
/// halbfertige Seite sieht.
///
/// Dieselbe Ueberlegung wie bei `libspeed::bild::Grenzen` (Serie 8,
/// Teil 3): Wer die Zahl kennt, gehoert nicht in die Funktion, die sie
/// anwendet.
pub fn nebensache_laden(
    ort: &Ort,
    klient: &mut Klient,
    cache: &mut Cache,
    max_bytes: usize,
    frist_ms: u64,
) -> Option<Vec<u8>> {
    match ort {
        Ort::Intern(_) => None,
        Ort::Datei(pfad) => {
            if let Some(bytes) = cache.holen(pfad) {
                return Some(bytes.to_vec());
            }
            let bytes = libspeed::netz::datei_lesen(pfad).ok()?;
            if bytes.len() > max_bytes {
                return None;
            }
            cache.legen(pfad, &bytes);
            Some(bytes)
        }
        Ort::Netz(ziel) => {
            let adresse = ziel.als_text();
            if let Some(bytes) = cache.holen(&adresse) {
                return Some(bytes.to_vec());
            }
            klient.frist_ms = frist_ms;
            klient.max_bytes = max_bytes;
            let abruf = klient.holen(&adresse).ok()?;
            let rumpf = abruf.antwort.rumpf;
            cache.legen(&adresse, &rumpf);
            Some(rumpf)
        }
    }
}

/// Der Inhalt einer eingebauten Seite.
fn interne_seite(seite: crate::ort::Intern) -> Vec<u8> {
    match seite {
        crate::ort::Intern::Info => seiten::info().into_bytes(),
        // Verlauf und Lesezeichen brauchen Daten, die nur der Browser
        // hat — sie werden dort erzeugt und hier nie angefragt.
        _ => seiten::info().into_bytes(),
    }
}

/// Die Uebersetzung `AbrufFehler` -> Seite.
///
/// ===================================================================
/// DIE EINE UNTERSCHEIDUNG, DIE ZAEHLT
///
/// `AbrufFehler::ist_sicherheitsfehler()` beantwortet die Frage, die
/// alles andere ueberlagert (Serie 7, Teil 5). Ein abgerissenes Kabel
/// und ein falsches Zertifikat sind NICHT dasselbe Ereignis, auch wenn
/// beide „hat nicht geklappt" heissen:
///
///   * Verbindungsfehler -> es kann sich lohnen, es noch einmal zu
///     versuchen.
///   * Sicherheitsfehler  -> es lohnt sich NICHT, und wer es trotzdem
///     anbietet, hat die Pruefung entwertet.
///
/// Deshalb bekommt der zweite Fall eine ANDERE Seite, kein rotes Wort
/// auf derselben.
fn fehlerseite(fehler: &AbrufFehler, adresse: &str, ort: &Ort) -> Geladen {
    let unsicher = fehler.ist_sicherheitsfehler();
    let kurz = fehler.kurz();
    let text = fehler.text();
    let bytes = if unsicher {
        seiten::sicherheitsfehler(kurz, &text, adresse).into_bytes()
    } else {
        let ueberschrift = match fehler {
            AbrufFehler::Dns(_) => "Server nicht gefunden",
            AbrufFehler::Verbindung(_) => "Keine Verbindung",
            AbrufFehler::Frist(..) => "Zeitueberschreitung",
            AbrufFehler::ZuGross { .. } => "Seite zu gross",
            AbrufFehler::ZuVieleWeiterleitungen(..) | AbrufFehler::Schleife(..) => {
                "Weiterleitung im Kreis"
            }
            AbrufFehler::Url(_) => "Ungueltige Adresse",
            AbrufFehler::LeereAntwort => "Leere Antwort",
            _ => "Seite konnte nicht geladen werden",
        };
        seiten::fehler(ueberschrift, &text, adresse).into_bytes()
    };
    Geladen {
        bytes,
        ort: ort.clone(),
        fehler: Some(String::from(kurz)),
        unsicher,
    }
}
