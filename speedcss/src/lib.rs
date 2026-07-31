// speedcss — CSS zerlegen, kaskadieren, vererben. Sonst nichts.
//
// ===========================================================================
// WAS DIESE KISTE IST
//
// Der zweite Teil des Browser-Fundaments (docs/browser-v1.md, Schritt 2).
// `speedhtml` macht aus Bytes einen Baum; diese Kiste macht aus dem Baum
// und ein paar Stylesheets **berechnete Stile je Knoten** — die Form, in
// der das Layout sie will.
//
//   CSS-Text --[parser]--> Stylesheet (Regeln mit Selektoren)
//   Stylesheet + Dokument --[kaskade]--> StilBaum (ein Stil je Knoten)
//
// ===========================================================================
// DIE VIER TEILE
//
//   `werte`    Laengen (in Tausendsteln — es gibt kein Fliesskomma),
//              Farben, Zahlen.
//   `parser`   CSS-Text zu Regeln. Fehlertolerant wie der HTML-Parser.
//   `stil`     Der BERECHNETE Stil als Struct — und damit die
//              abschliessende Liste der unterstuetzten Eigenschaften.
//   `kaskade`  Passen, sortieren, anwenden, vererben.
//   `standard` Das eingebaute Stylesheet — der Grund, warum HTML ohne
//              CSS ueberhaupt aussieht.
//
// ===========================================================================
// DIE ZUSAGE, DIESELBE WIE BEI speedhtml
//
// **Jede Eingabe ergibt ein Stylesheet.** `parsen` hat kein `Result`, es
// gibt keinen Fehlerfall und keine Panik. Was uebersprungen wurde, steht
// im `Befund` — und `cssdump` zeigt es.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod kaskade;
pub mod parser;
pub mod standard;
pub mod stil;
pub mod werte;

pub use kaskade::{berechnen, erklaeren, Erklaerung, Herkunft, Quelle, StilBaum, Zustand};
pub use parser::{parsen, parsen_mit, Befund, Grenzen, Regel, Selektor, Spezifitaet, Stylesheet};
pub use standard::STANDARD_CSS;
pub use stil::{
    Ausrichtung, Dekoration, Display, Familie, Kanten, Listenzeichen, RahmenStil, Stil, Vertikal,
    Zeilenhoehe, ANFANG,
};
pub use werte::{Farbe, Laenge};

use alloc::string::String;
use alloc::vec::Vec;
use speedhtml::Dokument;

/// Das Standard-Stylesheet, geparst.
///
/// Es wird bei jedem Aufruf neu geparst — gemessen unter einer
/// Millisekunde fuer ~90 Regeln. Wer eine Seite mehrfach durchrechnet
/// (der Browser bei jeder Groessenaenderung), haelt das Ergebnis fest,
/// statt diese Funktion erneut zu rufen.
pub fn standard_stylesheet() -> Stylesheet {
    parser::parsen(standard::STANDARD_CSS)
}

/// Alle `<style>`-Bloecke eines Dokuments einsammeln und parsen.
///
/// ===================================================================
/// WAS HIER NICHT PASSIERT: `<link rel=stylesheet>` WIRD NICHT GEHOLT
///
/// Das ist eine Entscheidung des Zuschnitts (docs/browser-v1.md §2.3) und
/// keine Vergesslichkeit: Ein externes Stylesheet zu holen hiesse, aus dem
/// Parsen eine NETZ-Operation zu machen — mit Frist, Fehlerfall,
/// Groessenlimit und der Frage, was passiert, wenn es nicht ankommt. Diese
/// Kiste kennt kein Netz und soll keins kennen.
///
/// Die FOLGE ist ehrlich zu benennen: Auf Seiten, die ihr gesamtes Aussehen
/// aus externen Dateien beziehen — das ist heute die Regel —, wirkt nur
/// das Standard-Stylesheet. Der Browser kann die Dateien spaeter selbst
/// holen und hier hereinreichen; die Kaskade aendert sich dadurch nicht.
pub fn autor_stylesheet(dokument: &Dokument) -> Stylesheet {
    let mut css = String::new();
    for id in dokument.alle_mit_tag("style") {
        // NICHT `dokument.text_von(id)`: Das ueberspringt `script` und
        // `style` ABSICHTLICH — deren Inhalt gehoert nicht in den
        // SICHTBAREN Text (speedhtml, die Lehre aus `news` in Serie 7).
        // Hier wollen wir aber genau diesen Inhalt.
        //
        // Das ist die Sorte Wechselwirkung, die zwischen zwei Kisten
        // entsteht und die kein Test der einzelnen Kiste findet — dieser
        // hier heisst `test_style_bloecke_im_dokument`.
        let Some(knoten) = dokument.knoten(id) else { continue };
        for kind in &knoten.kinder {
            if let Some(text) = dokument.knoten(*kind).and_then(|k| k.text()) {
                css.push_str(text);
            }
        }
        css.push('\n');
    }
    parser::parsen(&css)
}

/// Der bequeme Weg: Dokument hinein, Stil-Baum heraus.
///
/// Nimmt das Standard-Stylesheet und die `<style>`-Bloecke des Dokuments.
/// Wer mehr Kontrolle will (eigene Blaetter, Hover-Zustand), ruft
/// `kaskade::berechnen` direkt.
pub fn stile_berechnen(dokument: &Dokument) -> StilBaum {
    let standard = standard_stylesheet();
    let autor = autor_stylesheet(dokument);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    kaskade::berechnen(dokument, &blaetter, Zustand::default())
}

#[cfg(test)]
mod tests;
