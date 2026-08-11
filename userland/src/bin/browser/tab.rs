// browser::tab — eine Seite mit allem, was zu ihr gehoert
//
// ===========================================================================
// EIN TAB IST EIN VOLLSTAENDIGER BROWSER-ZUSTAND
//
// Dokument, berechnete Stile, Kastenbaum-Ergebnis, Anzeigeliste,
// Scroll-Position, Verlauf, geladene Bilder — alles PRO TAB. Nichts davon
// ist geteilt, und das ist der Grund, warum Tabs ueberhaupt etwas taugen:
// Ein Wechsel ist eine Zeigeraenderung, kein Neuladen.
//
// Geteilt sind nur die Dinge, die zwischen Seiten SINNVOLL geteilt sind:
// der Klient (eine TLS-Konfiguration mit 119 Wurzeln zweimal aufzubauen
// waere Verschwendung) und der Cache.
//
// ===========================================================================
// WAS EIN TAB NICHT TUT
//
// Er laedt nicht selbst. `laden::seite_laden` besorgt die Bytes, der Tab
// bekommt sie vorgelegt. Damit bleibt die Frage „woher kommen die Bytes"
// an einer Stelle, und ein Tab laesst sich in einem Test mit einem
// String fuettern.

use crate::ort::Ort;
use crate::seiten;
use alloc::string::String;
use alloc::vec::Vec;
use libspeed::leinwand::RasterMetrik;
use speedcss::{Herkunft, StilBaum, Stylesheet};
use speedhtml::{Dokument, KnotenId};
use speedlayout::{Anzeigeliste, Metrik};
use speedpaint::Sicht;
use speedui::Rechteck;

/// Wie viele Verlaufs-Eintraege ein Tab behaelt.
pub const MAX_VERLAUF: usize = 50;

// ===========================================================================
// DIE BILDER EINES TABS
// ===========================================================================

/// Ein geladenes Bild.
pub struct Bild {
    pub quelle: String,
    /// `None` = versucht und gescheitert (wird nicht erneut versucht).
    pub daten: Option<BildDaten>,
}

pub struct BildDaten {
    pub breite: i32,
    pub hoehe: i32,
    pub rgba: Vec<u8>,
}

/// Alle Bilder einer Seite.
#[derive(Default)]
pub struct BildSammlung {
    pub bilder: Vec<Bild>,
}

impl BildSammlung {
    pub fn kennt(&self, quelle: &str) -> bool {
        self.bilder.iter().any(|b| b.quelle == quelle)
    }

    pub fn masse(&self, quelle: &str) -> Option<(i32, i32)> {
        self.bilder
            .iter()
            .find(|b| b.quelle == quelle)?
            .daten
            .as_ref()
            .map(|d| (d.breite, d.hoehe))
    }

    pub fn leeren(&mut self) {
        self.bilder.clear();
    }
}

impl speedpaint::maler::Bildquelle for BildSammlung {
    fn bild(&self, quelle: &str) -> Option<speedpaint::maler::Bild<'_>> {
        let daten = self
            .bilder
            .iter()
            .find(|b| b.quelle == quelle)?
            .daten
            .as_ref()?;
        Some(speedpaint::maler::Bild {
            breite: daten.breite,
            hoehe: daten.hoehe,
            rgba: &daten.rgba,
        })
    }
}

/// Die Metrik EINER Seite: Textmasse wie immer, Bildmasse aus dem, was
/// schon geladen ist.
///
/// ===================================================================
/// HIER ENTSTEHT DER REFLOW
///
/// `speedlayout` fragt ueber `Metrik::bild_masse`, wie gross ein Bild
/// ist. Solange es nicht geladen ist, lautet die Antwort `None` und es
/// gilt der Platzhalter. Sobald es da ist, kommt die Eigengroesse — und
/// dasselbe Dokument ergibt ein ANDERES Layout. Mehr ist der beruechtigte
/// Seitensprung nicht.
pub struct SeitenMetrik<'a> {
    pub bilder: &'a BildSammlung,
}

impl Metrik for SeitenMetrik<'_> {
    fn text_breite(&self, text: &str, groesse: i32, fett: bool, kursiv: bool) -> i32 {
        RasterMetrik.text_breite(text, groesse, fett, kursiv)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        RasterMetrik.zeilen_hoehe(groesse)
    }
    fn grundlinie(&self, groesse: i32) -> i32 {
        RasterMetrik.grundlinie(groesse)
    }
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        RasterMetrik.groesse_waehlen(wunsch)
    }
    fn bild_masse(&self, quelle: &str) -> Option<(i32, i32)> {
        self.bilder.masse(quelle)
    }
}

// ===========================================================================
// DER TAB
// ===========================================================================

/// Was gerade mit dem Tab los ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabZustand {
    Leer,
    Laedt,
    Fertig,
    Fehler,
}

pub struct Tab {
    pub ort: Ort,
    pub titel: String,
    pub dokument: Dokument,
    pub stile: StilBaum,
    pub liste: Anzeigeliste,
    pub hoehe: i32,
    pub sicht: Sicht,
    pub bilder: BildSammlung,
    pub zustand: TabZustand,
    /// Kurztext des letzten Fehlers (fuer die Statuszeile).
    pub fehler: Option<String>,
    pub unsicher: bool,
    /// Der Verlauf: (Ort, Titel). `stelle` zeigt auf den aktuellen.
    pub verlauf: Vec<(Ort, String)>,
    pub stelle: usize,
    /// Die Layout-Breite, mit der `liste` entstanden ist.
    pub layout_breite: i32,
    /// Bilder, die noch geholt werden muessen.
    pub offene_bilder: Vec<String>,
    /// Wurde fuer DIESE Seite der JavaScript-Hinweis gezeigt?
    ///
    /// GEMERKT UND NICHT NACHTRAEGLICH BERECHNET: Sobald der Hinweis
    /// steht, IST er der Seiteninhalt — und der hat reichlich Text.
    /// Eine erneute Pruefung wuerde also immer „nein" sagen und damit
    /// genau die Auskunft verlieren, um die es geht.
    pub js_hinweis: bool,
}

impl Tab {
    pub fn neu() -> Tab {
        Tab {
            ort: Ort::Intern(crate::ort::Intern::Info),
            titel: String::from("Neuer Tab"),
            dokument: speedhtml::parsen(""),
            stile: leerer_stilbaum(),
            liste: Anzeigeliste::default(),
            hoehe: 0,
            sicht: Sicht::neu(Rechteck::neu(0, 0, 100, 100), 0),
            bilder: BildSammlung::default(),
            zustand: TabZustand::Leer,
            fehler: None,
            unsicher: false,
            verlauf: Vec::new(),
            stelle: 0,
            layout_breite: 0,
            offene_bilder: Vec::new(),
            js_hinweis: false,
        }
    }

    // -----------------------------------------------------------------
    // INHALT
    // -----------------------------------------------------------------

    /// Neuen Inhalt uebernehmen: parsen, kaskadieren, Titel holen.
    ///
    /// Das Layout passiert NICHT hier — es braucht die Fensterbreite, und
    /// die kennt nur der Aufrufer.
    pub fn inhalt_setzen(&mut self, bytes: &[u8], ort: Ort, fehler: Option<String>, unsicher: bool) {
        let html = String::from_utf8_lossy(bytes);
        self.dokument = speedhtml::parsen(&html);
        self.stile = kaskadieren(&self.dokument);
        self.titel = titel_von(&self.dokument).unwrap_or_else(|| ort.kurzname());
        self.ort = ort;
        self.fehler = fehler;
        self.unsicher = unsicher;
        self.zustand = if self.fehler.is_some() {
            TabZustand::Fehler
        } else {
            TabZustand::Fertig
        };
        self.bilder.leeren();
        self.offene_bilder.clear();
        self.layout_breite = 0;
        self.js_hinweis = false;
        self.sicht.versatz_setzen(0);
    }

    /// Ein fertiges HTML-Dokument einsetzen, das der Browser selbst
    /// erzeugt hat (Verlauf, Lesezeichen, JS-Hinweis).
    pub fn erzeugte_seite_setzen(&mut self, html: &str, ort: Ort) {
        self.inhalt_setzen(html.as_bytes(), ort, None, false);
    }

    /// Das Dokument setzen. `breite` ist die verfuegbare Breite.
    pub fn setzen(&mut self, breite: i32, bereich: Rechteck) {
        let metrik = SeitenMetrik {
            bilder: &self.bilder,
        };
        let ergebnis = speedlayout::setzen(&self.dokument, &self.stile, breite, &metrik);
        self.liste = speedlayout::anzeigeliste(&ergebnis);
        self.hoehe = ergebnis.hoehe;
        self.layout_breite = breite;
        self.sicht.anpassen(bereich, self.hoehe);
        self.sicht.zeilen_hoehe = RasterMetrik.zeilen_hoehe(16).max(1);
    }

    // -----------------------------------------------------------------
    // DER JAVASCRIPT-BEFUND
    // -----------------------------------------------------------------

    /// Bleibt diese Seite ohne JavaScript leer?
    ///
    /// ZWEI BEDINGUNGEN, und beide muessen zutreffen:
    ///   1. Im gesetzten Dokument steht kein nennenswerter Text.
    ///   2. Es gibt `<script>`-Bloecke.
    ///
    /// Die zweite allein waere falsch (fast jede Seite hat Skripte), die
    /// erste allein auch (eine Seite darf leer sein). Zusammen sind sie
    /// die verlaessliche Diagnose fuer den haeufigsten Fall eines
    /// Browsers ohne JavaScript.
    pub fn braucht_javascript(&self) -> Option<usize> {
        let texte: Vec<&str> = self
            .liste
            .befehle
            .iter()
            .filter_map(|b| match b {
                speedlayout::Befehl::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if seiten::hat_sichtbaren_text(&texte) {
            return None;
        }
        let skripte = self.dokument.alle_mit_tag("script").count();
        if skripte > 0 {
            Some(skripte)
        } else {
            None
        }
    }

    // -----------------------------------------------------------------
    // VERLAUF
    // -----------------------------------------------------------------

    /// Einen Ort in den Verlauf eintragen (nach einem echten Ladevorgang).
    ///
    /// **Alles hinter der aktuellen Stelle faellt weg** — das ist die
    /// Regel jedes Browsers: Wer zurueckgeht und dann etwas Neues
    /// aufruft, hat den alten Vorwaerts-Zweig verlassen.
    pub fn verlauf_eintragen(&mut self, ort: Ort, titel: &str) {
        if !self.verlauf.is_empty() {
            self.verlauf.truncate(self.stelle + 1);
            // Dieselbe Seite zweimal hintereinander bringt nichts.
            if self.verlauf[self.stelle].0 == ort {
                self.verlauf[self.stelle].1 = String::from(titel);
                return;
            }
        }
        self.verlauf.push((ort, String::from(titel)));
        if self.verlauf.len() > MAX_VERLAUF {
            self.verlauf.remove(0);
        }
        self.stelle = self.verlauf.len() - 1;
    }

    pub fn kann_zurueck(&self) -> bool {
        self.stelle > 0
    }
    pub fn kann_vor(&self) -> bool {
        self.stelle + 1 < self.verlauf.len()
    }

    /// Einen Schritt zurueck — liefert den Ort, der zu laden ist.
    pub fn zurueck(&mut self) -> Option<Ort> {
        if !self.kann_zurueck() {
            return None;
        }
        self.stelle -= 1;
        Some(self.verlauf[self.stelle].0.clone())
    }

    pub fn vor(&mut self) -> Option<Ort> {
        if !self.kann_vor() {
            return None;
        }
        self.stelle += 1;
        Some(self.verlauf[self.stelle].0.clone())
    }

    /// Der Verlauf als (Adresse, Titel) — fuer `speedos:verlauf`.
    pub fn verlauf_liste(&self) -> Vec<(String, String)> {
        self.verlauf
            .iter()
            .map(|(o, t)| (o.als_text(), t.clone()))
            .collect()
    }

    // -----------------------------------------------------------------
    // KLICK-ZIELE
    // -----------------------------------------------------------------

    /// Welcher Verweis liegt an dieser Stelle? (Leinwand-Koordinaten.)
    ///
    /// ===================================================================
    /// VOM PIXEL ZUM `href` — in zwei Schritten
    ///
    /// 1. Welcher ANZEIGE-BEFEHL liegt unter dem Punkt? Jeder Textbefehl
    ///    traegt seine `KnotenId` (dafuer ist sie seit Teil 6 da).
    /// 2. Von diesem Knoten AUFWAERTS, bis ein `<a href>` kommt.
    ///
    /// Der zweite Schritt ist der noetige: Der getroffene Knoten ist ein
    /// TEXT-Knoten, und `href` steht am `<a>` darueber — bei
    /// `<a><b>fett</b></a>` sogar zwei Ebenen hoeher.
    pub fn verweis_bei(&self, x: i32, y: i32) -> Option<(String, Rechteck)> {
        let dok_y = self.sicht.nach_dokument_y(y);
        let dok_x = x - self.sicht.bereich.x;
        let mut treffer: Option<(KnotenId, Rechteck)> = None;
        for befehl in &self.liste.befehle {
            let (knoten, rechteck) = match befehl {
                speedlayout::Befehl::Text {
                    x: bx,
                    y: by,
                    text,
                    groesse,
                    fett,
                    kursiv,
                    knoten,
                    ..
                } => {
                    let breite = RasterMetrik.text_breite(text, *groesse, *fett, *kursiv);
                    (*knoten, Rechteck::neu(*bx, *by, breite, *groesse))
                }
                speedlayout::Befehl::Bild {
                    rechteck, knoten, ..
                } => (
                    *knoten,
                    Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe),
                ),
                _ => continue,
            };
            let Some(knoten) = knoten else { continue };
            if rechteck.enthaelt(dok_x, dok_y) {
                treffer = Some((knoten, rechteck));
                // NICHT abbrechen: Spaetere Befehle liegen weiter oben in
                // der Malreihenfolge, und der oberste gewinnt.
            }
        }
        let (knoten, rechteck) = treffer?;
        let href = self.href_ueber(knoten)?;
        // Das Rechteck in Leinwand-Koordinaten zurueckgeben.
        Some((
            href,
            Rechteck::neu(
                rechteck.x + self.sicht.bereich.x,
                self.sicht.nach_leinwand_y(rechteck.y),
                rechteck.breite,
                rechteck.hoehe,
            ),
        ))
    }

    /// Von einem Knoten aufwaerts nach `<a href>` suchen.
    fn href_ueber(&self, start: KnotenId) -> Option<String> {
        let mut aktuell = Some(start);
        // Feste Obergrenze: Der Baum ist hoechstens 100 tief (speedhtml),
        // aber eine Schleife im `eltern`-Zeiger waere ein Haenger — und
        // ein Baum, den ein Fremder geschickt hat, wird nicht geglaubt.
        for _ in 0..128 {
            let id = aktuell?;
            let knoten = self.dokument.knoten(id)?;
            if knoten.name() == Some("a") {
                if let Some(href) = knoten.attribut("href") {
                    if !href.trim().is_empty() {
                        return Some(String::from(href));
                    }
                }
            }
            aktuell = knoten.eltern;
        }
        None
    }

    // -----------------------------------------------------------------
    // BILDER
    // -----------------------------------------------------------------

    /// Die Bildquellen der Seite in Reihenfolge einsammeln.
    pub fn bilder_einsammeln(&mut self) {
        self.offene_bilder.clear();
        for befehl in &self.liste.befehle {
            if let speedlayout::Befehl::Bild { quelle, .. } = befehl {
                if quelle.is_empty() || self.bilder.kennt(quelle) {
                    continue;
                }
                if !self.offene_bilder.iter().any(|q| q == quelle) {
                    self.offene_bilder.push(quelle.clone());
                }
            }
        }
    }

    /// Stand ein `width`/`height` im Dokument? Wenn nicht, aendert das
    /// geladene Bild die Geometrie — und es MUSS neu gesetzt werden.
    pub fn bild_aendert_masse(&self, quelle: &str) -> bool {
        for (_, knoten) in self.dokument.alle() {
            if knoten.name() != Some("img") {
                continue;
            }
            if knoten.attribut("src").map(|s| s.trim()) != Some(quelle) {
                continue;
            }
            let hat_breite = knoten.attribut("width").is_some();
            let hat_hoehe = knoten.attribut("height").is_some();
            if !hat_breite || !hat_hoehe {
                return true;
            }
        }
        false
    }

    /// Das Rechteck eines Bildes auf der Leinwand (fuer den Teilschaden).
    pub fn bild_bereich(&self, quelle: &str) -> Option<Rechteck> {
        for befehl in &self.liste.befehle {
            if let speedlayout::Befehl::Bild {
                rechteck,
                quelle: q,
                ..
            } = befehl
            {
                if q == quelle {
                    return Some(Rechteck::neu(
                        rechteck.x + self.sicht.bereich.x,
                        self.sicht.nach_leinwand_y(rechteck.y),
                        rechteck.breite,
                        rechteck.hoehe,
                    ));
                }
            }
        }
        None
    }

    /// Zu welchem Anker gehoert dieses Fragment? Liefert die Y-Position
    /// im Dokument.
    pub fn fragment_y(&self, fragment: &str) -> Option<i32> {
        // Den Knoten mit dieser Id (oder `<a name=…>`) finden.
        let ziel = self.dokument.alle().find_map(|(id, knoten)| {
            let passt = knoten.attribut("id") == Some(fragment)
                || (knoten.name() == Some("a") && knoten.attribut("name") == Some(fragment));
            if passt {
                Some(id)
            } else {
                None
            }
        })?;
        // Alle Nachfahren einsammeln — der Anker selbst erzeugt oft
        // keinen eigenen Anzeige-Befehl, sein Text schon.
        let mut familie: Vec<u32> = Vec::new();
        sammeln(&self.dokument, ziel, &mut familie, 0);
        familie.sort_unstable();
        self.liste
            .befehle
            .iter()
            .filter_map(|b| {
                let (knoten, y) = match b {
                    speedlayout::Befehl::Text { knoten, y, .. } => (*knoten, *y),
                    speedlayout::Befehl::Bild {
                        knoten, rechteck, ..
                    } => (*knoten, rechteck.y),
                    _ => return None,
                };
                let id = knoten?;
                familie.binary_search(&id.0).ok().map(|_| y)
            })
            .next()
    }
}

/// Alle Knoten eines Teilbaums einsammeln (mit Tiefengrenze).
fn sammeln(dokument: &Dokument, id: KnotenId, aus: &mut Vec<u32>, tiefe: u32) {
    if tiefe > 100 || aus.len() > 5000 {
        return;
    }
    aus.push(id.0);
    if let Some(knoten) = dokument.knoten(id) {
        for kind in &knoten.kinder {
            sammeln(dokument, *kind, aus, tiefe + 1);
        }
    }
}

/// Standard-Stylesheet + die `<style>`-Bloecke des Dokuments.
pub fn kaskadieren(dokument: &Dokument) -> StilBaum {
    let standard = speedcss::standard_stylesheet();
    let autor = speedcss::autor_stylesheet(dokument);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    speedcss::kaskade::berechnen(dokument, &blaetter, speedcss::Zustand::default())
}

fn leerer_stilbaum() -> StilBaum {
    let leer = speedhtml::parsen("");
    kaskadieren(&leer)
}

/// Der `<title>` der Seite.
pub fn titel_von(dokument: &Dokument) -> Option<String> {
    let id = dokument.erstes("title")?;
    let text = dokument.text_von(id);
    let geputzt = text.trim();
    if geputzt.is_empty() {
        None
    } else {
        // Titel koennen absurd lang sein; die Tab-Leiste kuerzt ohnehin,
        // aber der Speicher muss es nicht mitmachen.
        Some(String::from(&geputzt[..geputzt.len().min(120)]))
    }
}
