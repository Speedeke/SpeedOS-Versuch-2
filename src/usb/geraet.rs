// usb::geraet — das Verzeichnis der erkannten USB-Geraete
//
// ===========================================================================
// DIE NAHT, UM DIE ES HIER GEHT
//
// Dieselbe Disziplin wie bei `BlockDevice` (Serie 4) und `NetzGeraet`
// (Serie 5): **Der Controller-Code kennt keine Geraeteklassen, und die
// Klassentreiber kennen keinen Controller.** Dazwischen steht diese
// Liste.
//
//   xhci  --[meldet an]-->  GERAETE  <--[sucht sich seins]--  Treiber
//
// Ein HID-Treiber (Teil 3) fragt nach Klasse 3, ein Massenspeicher-
// Treiber spaeter nach Klasse 8 — und `xhci/mod.rs` wird dafuer NICHT
// angefasst. Das ist der Zweck, und es ist derselbe Grund wie damals:
// Als virtio-net dazukam, musste der IP-Stack nicht geaendert werden.
//
// ===========================================================================
// WARUM DIE KLASSE AUS DEM INTERFACE KOMMEN KANN UND NICHT NUR AUS DEM
// GERAET
//
// Der Device-Deskriptor hat ein Klassenfeld — und bei fast jeder
// Tastatur steht dort **0**. Das ist kein Fehler, sondern die Ansage
// „schau in die Interfaces". Ein Verzeichnis, das nur das Geraetefeld
// liest, findet deshalb NIE eine Tastatur.
//
// `GeraeteEintrag::klasse_finden` loest das an EINER Stelle, damit es
// nicht jeder Treiber einzeln (und einer davon falsch) macht.

use crate::usb::deskriptor::{klasse_text, Endpunkt, Konfiguration, GeraeteDeskriptor};
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// Wie viele Geraete das Verzeichnis fuehrt.
///
/// Acht ist mehr als ein xHCI-Wurzelport-Satz in QEMU hergibt und weit
/// mehr, als ein Notebook direkt angeschlossen hat. Hubs koennen wir
/// ohnehin noch nicht (docs/grenzen.md).
pub const MAX_GERAETE: usize = 8;

/// Ein erkanntes USB-Geraet.
#[derive(Debug, Clone)]
pub struct GeraeteEintrag {
    /// Der xHCI-Slot, in dem es steckt. **Zugleich sein Ausweis** —
    /// beim Abziehen wird ueber ihn aufgeraeumt.
    pub slot: u8,
    /// Der Wurzelport (einsbasiert), an dem es haengt.
    pub port: u8,
    /// Die USB-Adresse, die der Controller vergeben hat.
    pub adresse: u8,
    pub geraet: GeraeteDeskriptor,
    pub konfiguration: Konfiguration,
    /// Hersteller und Produkt, schon aus UTF-16 uebersetzt. Leer, wenn
    /// das Geraet keine Strings hat (das ist erlaubt).
    pub hersteller: String,
    pub produkt: String,
    pub tempo: &'static str,
}

impl GeraeteEintrag {
    /// **Die tatsaechliche Klasse** — aus dem Geraet, sonst aus dem
    /// ersten Interface.
    ///
    /// Liefert (Klasse, Unterklasse, Protokoll). Siehe Kopfkommentar:
    /// Bei Klasse 0 im Device-Deskriptor steht die Wahrheit im
    /// Interface, und das ist bei Tastaturen der Normalfall.
    pub fn klasse_finden(&self) -> (u8, u8, u8) {
        if self.geraet.klasse != 0 {
            return (
                self.geraet.klasse,
                self.geraet.unterklasse,
                self.geraet.protokoll,
            );
        }
        match self.konfiguration.schnittstellen.first() {
            Some(s) => (s.klasse, s.unterklasse, s.protokoll),
            None => (0, 0, 0),
        }
    }

    pub fn klassen_text(&self) -> &'static str {
        let (k, u, p) = self.klasse_finden();
        klasse_text(k, u, p)
    }

    /// Alle Endpunkte ueber alle Interfaces.
    pub fn endpunkte(&self) -> Vec<Endpunkt> {
        let mut aus = Vec::new();
        for s in &self.konfiguration.schnittstellen {
            for e in &s.endpunkte {
                aus.push(*e);
            }
        }
        aus
    }

    /// Der erste Interrupt-IN-Endpunkt — **das, was Tastatur und Maus
    /// benutzen**, und damit die Frage, die ein HID-Treiber stellt.
    pub fn interrupt_eingang(&self) -> Option<Endpunkt> {
        use crate::usb::deskriptor::Uebertragung;
        self.endpunkte()
            .into_iter()
            .find(|e| e.art == Uebertragung::Interrupt && e.ist_eingang())
    }

    /// Ein Name fuer die Anzeige — Produkt, sonst die IDs.
    pub fn anzeigename(&self) -> String {
        if !self.produkt.is_empty() {
            return self.produkt.clone();
        }
        alloc::format!(
            "{:04x}:{:04x}",
            self.geraet.hersteller_id,
            self.geraet.produkt_id
        )
    }
}

/// Das Verzeichnis.
///
/// **Ein BLATT-LOCK**, wie die Ablage und die Laufwerks-Registry: Es
/// wird nur genommen, um einen Eintrag hinzuzufuegen, zu entfernen oder
/// zu lesen — und niemals waehrend eines Registerzugriffs oder mit dem
/// Controller-Lock in der Hand. Die Lock-Ordnung ist damit
/// CONTROLLER -> GERAETE und nie andersherum.
static GERAETE: Mutex<Vec<GeraeteEintrag>> = Mutex::new(Vec::new());

/// Ein Geraet anmelden. Ein Eintrag mit demselben Slot wird ERSETZT.
///
/// Das Ersetzen ist Absicht: Wenn ein Slot neu belegt wird, ohne dass
/// wir das Abziehen gesehen haben (verschluckte Events, schnelles
/// Umstecken), waere ein zweiter Eintrag fuer denselben Slot eine
/// Karteileiche, die nie verschwindet.
pub fn anmelden(eintrag: GeraeteEintrag) -> bool {
    let mut liste = GERAETE.lock();
    if let Some(vorhanden) = liste.iter_mut().find(|g| g.slot == eintrag.slot) {
        *vorhanden = eintrag;
        return true;
    }
    if liste.len() >= MAX_GERAETE {
        return false;
    }
    liste.push(eintrag);
    true
}

/// Ein Geraet abmelden (Slot). Liefert, ob eins entfernt wurde.
pub fn abmelden(slot: u8) -> bool {
    let mut liste = GERAETE.lock();
    let vorher = liste.len();
    liste.retain(|g| g.slot != slot);
    liste.len() != vorher
}

/// Alle Geraete an einem Port abmelden. Liefert die Slots, die
/// betroffen waren — der Aufrufer muss sie beim Controller freigeben.
///
/// **Die Trennung ist wichtig:** Diese Datei gibt keine
/// Controller-Ressourcen frei (sie kennt den Controller nicht). Sie
/// sagt nur, WELCHE es waeren. Wer beides vermischt, hat die Naht
/// wieder zugeklebt.
pub fn slots_an_port(port: u8) -> Vec<u8> {
    GERAETE
        .lock()
        .iter()
        .filter(|g| g.port == port)
        .map(|g| g.slot)
        .collect()
}

/// Wie viele Geraete bekannt sind.
pub fn anzahl() -> usize {
    GERAETE.lock().len()
}

/// Mit der Liste arbeiten (fuer `usb` und fuer Treiber).
pub fn mit_geraeten<R>(f: impl FnOnce(&[GeraeteEintrag]) -> R) -> R {
    f(&GERAETE.lock())
}

/// Alle Geraete einer Klasse — **die Frage, die ein Treiber stellt.**
///
/// Ein HID-Treiber ruft `finde_klasse(3, None, None)`, ein
/// Massenspeicher-Treiber spaeter `finde_klasse(8, None, Some(0x50))`.
/// `None` heisst „egal".
pub fn finde_klasse(
    klasse: u8,
    unterklasse: Option<u8>,
    protokoll: Option<u8>,
) -> Vec<GeraeteEintrag> {
    GERAETE
        .lock()
        .iter()
        .filter(|g| {
            let (k, u, p) = g.klasse_finden();
            k == klasse
                && unterklasse.map(|w| w == u).unwrap_or(true)
                && protokoll.map(|w| w == p).unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Alles vergessen (nur fuer Tests und den Controller-Neustart).
pub fn leeren() {
    GERAETE.lock().clear();
}
