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
pub use werte::kleinschreiben;
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

// ===========================================================================
// WAS EIN DOKUMENT AN STILEN BRAUCHT
// ===========================================================================

/// Ein Stylesheet, das ein Dokument haben will.
///
/// ===================================================================
/// GEMELDET, NICHT GEHOLT — DIE KISTE KENNT WEITER KEIN NETZ
///
/// Bis Serie 8, Teil 8 stand hier, `<link rel=stylesheet>` werde nicht
/// geholt, „weil das aus dem Parsen eine Netz-Operation machte". Der Satz
/// war richtig und ist es geblieben — was fehlte, war die Trennung:
///
///   * **WAS** ein Dokument braucht, steht IM Dokument. Das kann diese
///     Kiste beantworten, und sonst niemand so gut.
///   * **OB und WIE** es ankommt, ist eine Frage von Frist, Fehlerfall,
///     Groessengrenze und Cache. Das gehoert dem Wirt.
///
/// `blaetter_einsammeln` beantwortet die erste Frage. Der Browser holt,
/// parst und reicht die Ergebnisse an `kaskade::berechnen` — die sich
/// dadurch NICHT geaendert hat, sie nimmt seit Teil 5 eine Liste von
/// Blaettern entgegen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blattbedarf {
    /// `<link rel="stylesheet" href="…">` — muss geholt werden.
    Extern(String),
    /// `<style>…</style>` — der Text steht schon da.
    Inline(String),
}

/// Was ein Dokument an Stylesheets braucht — **in Dokumentreihenfolge**.
///
/// ===================================================================
/// DIE REIHENFOLGE IST DAS EIGENTLICHE ERGEBNIS
///
/// Nicht die Liste ist die Auskunft, sondern ihre Ordnung. Ein externes
/// Blatt und ein `<style>`-Block sind fuer die Kaskade DASSELBE (beide
/// Herkunft „Autor", beide gleich stark); wer gewinnt, entscheidet bei
/// gleicher Spezifitaet allein die Position im Dokument.
///
/// Deshalb wird hier NICHT nach Sorte getrennt, sondern der Baum von
/// vorn nach hinten durchlaufen. Ein `<style>` VOR einem `<link>` ist
/// schwaecher als das `<link>`; ein `<style>` DANACH ist staerker. Wer
/// erst alle externen und dann alle inline einsortiert, bekommt auf
/// jeder Seite, die beides mischt, eine andere Darstellung als jeder
/// echte Browser — und zwar eine, die genauso falsch aussieht wie vorher,
/// nur anders.
///
/// **Der Arena-Index IST die Dokumentreihenfolge** (speedhtml legt jeden
/// Knoten an, wenn er ihm begegnet), also genuegt ein Lauf ueber `alle()`.
///
/// AUSGELASSEN WIRD:
///   * `rel`, das nicht `stylesheet` ist (`icon`, `preload`, `canonical`);
///   * `rel="alternate stylesheet"` — ein Vorschlag, den ein Browser erst
///     auf Wunsch anwendet, und wir haben keinen Wunsch-Schalter;
///   * `media`, das nicht fuer den Bildschirm gilt (dieselbe vorsichtige
///     Regel wie bei `@import`, siehe `parser::medien_gelten`);
///   * ein leeres `href`.
pub fn blaetter_einsammeln(dokument: &Dokument) -> Vec<Blattbedarf> {
    let mut aus = Vec::new();
    for (id, knoten) in dokument.alle() {
        match knoten.name() {
            Some("link") => {
                if !ist_stylesheet_verweis(knoten.attribut("rel").unwrap_or("")) {
                    continue;
                }
                if !parser::medien_gelten_oeffentlich(knoten.attribut("media").unwrap_or("")) {
                    continue;
                }
                let Some(href) = knoten.attribut("href") else { continue };
                if href.trim().is_empty() {
                    continue;
                }
                aus.push(Blattbedarf::Extern(String::from(href.trim())));
            }
            Some("style") => {
                if !parser::medien_gelten_oeffentlich(knoten.attribut("media").unwrap_or("")) {
                    continue;
                }
                let mut css = String::new();
                // NICHT `text_von`: Das ueberspringt `style` absichtlich
                // (siehe `autor_stylesheet`).
                for kind in &knoten.kinder {
                    if let Some(text) = dokument.knoten(*kind).and_then(|k| k.text()) {
                        css.push_str(text);
                    }
                }
                let _ = id;
                if !css.trim().is_empty() {
                    aus.push(Blattbedarf::Inline(css));
                }
            }
            _ => {}
        }
    }
    aus
}

/// Ist das ein `rel`, das ein anzuwendendes Stylesheet meint?
///
/// `rel` ist eine LISTE (`rel="stylesheet"`, aber auch
/// `rel="alternate stylesheet"`). Das Wort `stylesheet` muss darin
/// vorkommen — und `alternate` darf es NICHT, denn ein alternatives
/// Stylesheet ist ein Angebot und keine Anweisung.
fn ist_stylesheet_verweis(rel: &str) -> bool {
    let mut hat_stylesheet = false;
    for wort in rel.split_whitespace() {
        let klein = werte::kleinschreiben(wort);
        if klein == "alternate" {
            return false;
        }
        if klein == "stylesheet" {
            hat_stylesheet = true;
        }
    }
    hat_stylesheet
}

/// Alle `<style>`-Bloecke eines Dokuments einsammeln und parsen.
///
/// ===================================================================
/// DAS IST DIE KLEINE FASSUNG — sie holt nichts
///
/// Sie sieht NUR die `<style>`-Bloecke; `<link rel=stylesheet>` bleibt
/// aussen vor, weil diese Kiste kein Netz kennt. Wer die externen Blaetter
/// will, benutzt `blaetter_einsammeln` und holt sie selbst.
///
/// Die Funktion bleibt, weil sie genau richtig ist, wenn es nichts zu
/// holen GIBT: `cssdump` auf einer lokalen Datei, die Tests, und jeder
/// Wirt ohne Netz.
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
