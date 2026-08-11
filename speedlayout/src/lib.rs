// speedlayout — aus Stilen Kaesten, aus Kaesten Anzeige-Befehle
//
// ===========================================================================
// DER WEG DURCH DIESE KISTE
//
//   Dokument + StilBaum --[kasten]--> Kastenbaum (wer ist Block, wer Inline)
//   Kastenbaum + Breite --[layout]--> Kastenbaum MIT Koordinaten
//   Kastenbaum          --[anzeige]-> Vec<Befehl>  (absolute Koordinaten)
//
// Die letzte Stufe ist die entscheidende Entscheidung dieses Teils: **Das
// Ergebnis ist eine Liste von BEFEHLEN, kein Bild.** Warum, steht in
// `anzeige.rs`; kurz: Ein Layout, das man nur fotografieren kann, wird
// nicht getestet.
//
// ===========================================================================
// WAS HIER NICHT PASSIERT
//
// Kein Zeichnen, kein Netz, keine Bilddaten. Ein `<img>` wird zu einem
// Befehl mit seiner QUELLE und seinem Rechteck — wer die Bytes holt und
// dekodiert, ist der Browser (`libspeed::netz`, `libspeed::bild`).
//
// ===========================================================================
// DER ZUSCHNITT
//
// docs/browser-v1.md, Schritt 3 und 4. Was fehlt, fehlt mit Ansage:
// keine Floats, kein `position: absolute`, kein Flexbox, kein Blocksatz.
// `display: flex` wird zu `block` — der Inhalt steht dann untereinander
// statt zu verschwinden, und das ist das ehrlichere Scheitern.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod anzeige;
pub mod attrappe;
pub mod inline;
pub mod kasten;
pub mod layout;

pub use anzeige::{Befehl, Anzeigeliste};
pub use kasten::{Kasten, KastenArt, Kanten, Masse, Rechteck};

use alloc::string::String;
use alloc::vec::Vec;
use speedcss::Stil;

// ---------------------------------------------------------------------------
// DIE METRIK-NAHT
// ---------------------------------------------------------------------------

/// Was das Layout ueber Text und Bilder wissen muss — und mehr nicht.
///
/// ===================================================================
/// WARUM EIN EIGENES TRAIT UND NICHT `speedui::Schrift`
///
/// Es waere naheliegend, das Schrift-Trait aus Serie 8, Teil 3 zu
/// benutzen. Zwei Gruende dagegen:
///
///  1. **Die Tests.** Sie brauchen eine Metrik mit FESTER Zeichenbreite,
///     damit Umbruchstellen exakt vorhersagbar sind — „bei 10 Zeichen
///     Breite bricht `aaa bbb ccc` nach `aaa bbb` um" ist eine Aussage,
///     die man hinschreiben kann. Mit einer echten Schrift waeren alle
///     Zahlen in den Tests aus dem Ergebnis abgeschrieben, und ein Test,
///     dessen Erwartung aus dem Ergebnis stammt, prueft nichts.
///  2. **Der Zuschnitt.** Das Layout braucht drei Zahlen. `speedui`
///     bringt Knoepfe, Listen, Themen und einen Datei-Dialog mit.
///
/// Der Browser verdrahtet beides — das ist eine Zeile.
pub trait Metrik {
    /// Breite eines Textstuecks in ganzen Pixeln.
    ///
    /// `groesse` ist eine Pixelhoehe. `fett`/`kursiv` duerfen die Breite
    /// aendern (bei einer Proportionalschrift tun sie das).
    fn text_breite(&self, text: &str, groesse: i32, fett: bool, kursiv: bool) -> i32;

    /// Hoehe einer Zeile in dieser Schriftgroesse, wenn `line-height`
    /// nichts anderes sagt.
    fn zeilen_hoehe(&self, groesse: i32) -> i32;

    /// Wie weit die Grundlinie unter der Oberkante liegt.
    ///
    /// Wird fuer die Ausrichtung verschieden grosser Schriften in EINER
    /// Zeile gebraucht. Voreinstellung: 80 % der Schriftgroesse — das ist
    /// bei fast jeder Schrift nah genug, und unsere vorgerasterten
    /// Bitmaps geben nichts Genaueres her (docs/schrift-groessen.md).
    fn grundlinie(&self, groesse: i32) -> i32 {
        (groesse * 8) / 10
    }

    /// Die Groesse, die dieser Wirt aus einer Wunschgroesse macht.
    ///
    /// Voreinstellung: unveraendert. Ein Wirt mit vorgerasterten Schriften
    /// rundet hier auf das naechste Raster — und das Layout rechnet dann
    /// mit der Groesse, die WIRKLICH gezeichnet wird, statt mit der
    /// gewuenschten. Ohne das laufen Zeilenhoehe und Textbreite
    /// auseinander.
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        wunsch
    }

    /// Die EIGENGROESSE eines Bildes, falls der Wirt sie schon kennt
    /// (Serie 8, Teil 8).
    ///
    /// ===================================================================
    /// WARUM DAS HIERHER GEHOERT UND NICHT IN EIN NEUES TRAIT
    ///
    /// Der Kopf dieses Traits sagt seit Teil 6 „was das Layout ueber Text
    /// UND BILDER wissen muss" — das war die Absicht, es fehlte nur die
    /// Methode. `Metrik` ist bereits durch das ganze Layout gefaedelt;
    /// ein zweites Trait daneben waere dieselbe Frage („wie gross ist
    /// dieser Inhalt?") an einer zweiten Adresse.
    ///
    /// VOREINSTELLUNG `None` = „weiss ich nicht". Dann gilt, was bisher
    /// galt: `width`/`height` aus Stil oder Attribut, sonst der feste
    /// Platzhalter. Bestehende Wirte laufen unveraendert weiter.
    ///
    /// DIE FOLGE FUER DEN BROWSER ist der eigentliche Punkt: Sobald ein
    /// Bild geladen ist, liefert der Wirt hier seine Masse — und ein
    /// erneutes `setzen` ergibt ein ANDERES Layout. Genau das ist der
    /// Reflow, den ein Bild ohne Massangabe ausloesen MUSS, wenn die
    /// Seite hinterher richtig aussehen soll.
    fn bild_masse(&self, _quelle: &str) -> Option<(i32, i32)> {
        None
    }
}

// ---------------------------------------------------------------------------
// GRENZEN
// ---------------------------------------------------------------------------

/// Was ein Layout hoechstens kosten darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grenzen {
    /// Wie tief der Kastenbaum verschachtelt sein darf.
    ///
    /// ===================================================================
    /// DIE ZAHL, DIE DEN STACK RETTET
    ///
    /// Das Layout ist REKURSIV — anders als der HTML-Parser und die
    /// Kaskade, die beide einen eigenen Stapel benutzen. Das ist hier
    /// Absicht: Block-Layout ist ein Nachbestell-Durchlauf mit
    /// Rueckgabewert (die Hoehe des Kindes bestimmt die Position des
    /// naechsten), und den iterativ zu schreiben kostet mehr
    /// Verstaendlichkeit, als er einbringt.
    ///
    /// Der User-Stack ist 64 KiB. Eine Layout-Ebene braucht bei uns
    /// gemessen unter 400 Byte, also waeren ~160 Ebenen die harte Grenze.
    /// 64 laesst Luft nach unten und liegt weit ueber dem, was echte
    /// Seiten brauchen (Wikipedia: 25).
    ///
    /// Wird sie erreicht, wird der Teilbaum ABGESCHNITTEN und gezaehlt —
    /// nicht abgestuerzt.
    pub max_tiefe: usize,
    /// Hoechstzahl der Kaesten. Schuetzt vor Dokumenten, die den Heap
    /// fuellen wuerden.
    pub max_kaesten: usize,
    /// Hoechstzahl der Zeilen in EINEM Inline-Kontext.
    ///
    /// Ein einzelnes Wort, das schmaler ist als ein Zeichen, koennte
    /// sonst unendlich viele Zeilen erzeugen. Der Umbruch verhindert das
    /// schon (er nimmt immer mindestens ein Zeichen je Zeile), aber eine
    /// zweite Sicherung kostet nichts.
    pub max_zeilen: usize,
}

impl Grenzen {
    pub const fn standard() -> Grenzen {
        Grenzen {
            max_tiefe: 64,
            max_kaesten: 100_000,
            max_zeilen: 100_000,
        }
    }
}

impl Default for Grenzen {
    fn default() -> Self {
        Grenzen::standard()
    }
}

/// Was beim Layout aufgefallen ist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Befund {
    /// Teilbaeume, die wegen `max_tiefe` nicht gesetzt wurden.
    pub zu_tief: u32,
    /// Inhalte, die breiter sind als ihr Kasten (sie laufen ueber).
    pub ueberlaeufe: u32,
    /// Eine Grenze hat gegriffen.
    pub abgeschnitten: bool,
    pub kaesten: usize,
    pub zeilen: usize,
}

impl Befund {
    pub fn sauber(&self) -> bool {
        self.zu_tief == 0 && !self.abgeschnitten
    }
}

// ---------------------------------------------------------------------------
// DER BEQUEME WEG
// ---------------------------------------------------------------------------

/// Ein fertiges Layout: Kastenbaum mit Koordinaten plus Befund.
pub struct Ergebnis {
    pub wurzel: Kasten,
    pub befund: Befund,
    /// Die Gesamthoehe des Dokuments — was der Browser zum Scrollen
    /// braucht.
    pub hoehe: i32,
}

/// Ein Dokument setzen: HTML + Stile + Breite -> Kastenbaum.
pub fn setzen(
    dokument: &speedhtml::Dokument,
    stile: &speedcss::StilBaum,
    breite: i32,
    metrik: &dyn Metrik,
) -> Ergebnis {
    setzen_mit(dokument, stile, breite, metrik, Grenzen::standard())
}

/// Wie `setzen`, mit ausdruecklichen Grenzen.
///
/// **PANICKT NIE.** Wie ueberall in diesem Fundament gibt es kein
/// `Result`: Jede Eingabe ergibt ein Layout, notfalls ein leeres. Was
/// abgeschnitten wurde, steht im `Befund`.
pub fn setzen_mit(
    dokument: &speedhtml::Dokument,
    stile: &speedcss::StilBaum,
    breite: i32,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
) -> Ergebnis {
    let mut befund = Befund::default();
    let mut wurzel = kasten::baum_bauen(dokument, stile, grenzen, &mut befund);
    let hoehe = layout::block_setzen(&mut wurzel, 0, 0, breite.max(0), metrik, grenzen, &mut befund, 0);
    Ergebnis {
        wurzel,
        befund,
        hoehe,
    }
}

/// Die Anzeige-Befehle eines fertigen Layouts.
pub fn anzeigeliste(ergebnis: &Ergebnis) -> Anzeigeliste {
    anzeige::erzeugen(&ergebnis.wurzel)
}

/// Der ganze Weg in einem Aufruf — fuer Tests und einfache Aufrufer.
pub fn seite_setzen(
    html: &str,
    css: &str,
    breite: i32,
    metrik: &dyn Metrik,
) -> (Ergebnis, Anzeigeliste) {
    let dokument = speedhtml::parsen(html);
    let standard = speedcss::standard_stylesheet();
    let autor_text = {
        let mut s = String::from(css);
        s.push('\n');
        s
    };
    let autor = speedcss::parsen(&autor_text);
    let dokument_css = speedcss::autor_stylesheet(&dokument);
    let blaetter: Vec<(&speedcss::Stylesheet, speedcss::Herkunft)> = alloc::vec![
        (&standard, speedcss::Herkunft::Standard),
        (&dokument_css, speedcss::Herkunft::Autor),
        (&autor, speedcss::Herkunft::Autor),
    ];
    let stile = speedcss::kaskade::berechnen(&dokument, &blaetter, speedcss::Zustand::default());
    let ergebnis = setzen(&dokument, &stile, breite, metrik);
    let liste = anzeigeliste(&ergebnis);
    (ergebnis, liste)
}

/// Hilfsfunktion: eine Laenge gegen einen Bezug in ganze Pixel.
///
/// `auto` und Unlesbares werden zu `ersatz`. An EINER Stelle, damit nicht
/// jeder Aufrufer sein eigenes `unwrap_or(0)` schreibt — genau so wird
/// aus `width: auto` still `width: 0`.
pub(crate) fn px(laenge: speedcss::Laenge, bezug: i32, ersatz: i32) -> i32 {
    laenge.auf_bezug(bezug).unwrap_or(ersatz)
}

/// Die Schriftgroesse eines Stils in ganzen Pixeln, wie der Wirt sie
/// wirklich zeichnet.
pub(crate) fn schrift_px(stil: &Stil, metrik: &dyn Metrik) -> i32 {
    metrik.groesse_waehlen(speedcss::werte::runden(stil.schrift_px).max(1))
}

#[cfg(test)]
mod tests;
