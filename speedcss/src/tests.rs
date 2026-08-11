// speedcss::tests — Spezifitaet, Vererbung, !important, Einheiten
//
// Wie bei `speedhtml`: Diese Tests laufen auf dem HOST in Millisekunden,
// und genau deshalb sind es viele.
//
// Die Faelle sind nicht ausgedacht, sondern die, an denen CSS-Umsetzungen
// wirklich scheitern: Spezifitaet lexikografisch statt verrechnet,
// `em` bezogen auf das falsche Element, `line-height` als Faktor statt als
// Laenge vererbt, `!important` ueber Herkunftsgrenzen, und ein
// `@media`-Block, dessen Regeln nach draussen durchschlagen.

use crate::kaskade::{self, Herkunft, Quelle, Zustand};
use crate::parser::{self, Spezifitaet};
use crate::stil::{Ausrichtung, Display, Familie, Listenzeichen, RahmenStil, Zeilenhoehe};
use crate::werte::{Farbe, Laenge};
use crate::{standard_stylesheet, stil, Stylesheet};
use alloc::string::String;
use alloc::vec::Vec;
use speedhtml::Dokument;

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

/// Ein Dokument parsen und mit den gegebenen Autor-Regeln durchrechnen.
/// Das Standard-Stylesheet ist dabei.
fn rechnen(html: &str, css: &str) -> (Dokument, kaskade::StilBaum) {
    let dokument = speedhtml::parsen(html);
    let standard = standard_stylesheet();
    let autor = parser::parsen(css);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());
    (dokument, baum)
}

/// Wie `rechnen`, aber OHNE das Standard-Stylesheet.
fn rechnen_ohne_standard(html: &str, css: &str) -> (Dokument, kaskade::StilBaum) {
    let dokument = speedhtml::parsen(html);
    let autor = parser::parsen(css);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![(&autor, Herkunft::Autor)];
    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());
    (dokument, baum)
}

/// Den Stil des ersten Elements mit diesem Tag.
fn stil_von<'a>(
    dokument: &Dokument,
    baum: &'a kaskade::StilBaum,
    tag: &str,
) -> &'a crate::Stil {
    let id = dokument
        .erstes(tag)
        .unwrap_or_else(|| panic!("<{tag}> nicht im Dokument"));
    baum.stil(id)
}

/// Die Spezifitaet eines Selektor-Textes.
fn spez(text: &str) -> Spezifitaet {
    parser::selektor_parsen(text)
        .unwrap_or_else(|| panic!("'{text}' liess sich nicht parsen"))
        .spezifitaet()
}

// ===========================================================================
// 1. PARSER
// ===========================================================================

#[test]
fn test_einfachste_regel() {
    let blatt = parser::parsen("p { color: red }");
    assert_eq!(blatt.regeln.len(), 1);
    assert_eq!(blatt.regeln[0].selektoren.len(), 1);
    assert_eq!(blatt.regeln[0].deklarationen.len(), 1);
    assert_eq!(blatt.regeln[0].deklarationen[0].name, "color");
    assert_eq!(blatt.regeln[0].deklarationen[0].wert, "red");
    assert!(!blatt.regeln[0].deklarationen[0].wichtig);
    assert!(blatt.befund.sauber(), "{:?}", blatt.befund);
}

#[test]
fn test_selektorliste_und_leerraum() {
    let blatt = parser::parsen("  h1 ,\n h2,h3   {\n\tfont-weight : bold ;\n }  ");
    assert_eq!(blatt.regeln.len(), 1);
    assert_eq!(blatt.regeln[0].selektoren.len(), 3);
    assert_eq!(blatt.regeln[0].deklarationen[0].name, "font-weight");
}

/// Kommentare duerfen ueberall stehen — auch mitten im Selektor.
#[test]
fn test_kommentare() {
    let blatt = parser::parsen("/* Anfang */ div/**/p { color: /* hier */ red } /* Ende */");
    assert_eq!(blatt.regeln.len(), 1);
    assert_eq!(blatt.regeln[0].deklarationen[0].wert, "red");

    // Ein nie geschlossener Kommentar frisst den Rest — und darf nicht
    // haengen.
    let blatt = parser::parsen("p { color: red } /* nie zu Ende");
    assert_eq!(blatt.regeln.len(), 1);
}

/// KAPUTTE REGELN WERDEN UEBERSPRUNGEN, NICHT ABGEBROCHEN.
#[test]
fn test_kaputte_regeln_ueberspringen() {
    let css = "
        p { color: red }
        }}}
        { kein-selektor: x }
        @unbekannt foo;
        div { }
        h1 { color: blue }
    ";
    let blatt = parser::parsen(css);
    // p, div (leer, aber gueltig) und h1 haben ueberlebt.
    let namen: Vec<&str> = blatt
        .regeln
        .iter()
        .flat_map(|r| r.selektoren.iter())
        .map(|s| s.text.as_str())
        .collect();
    assert!(namen.contains(&"p"), "{namen:?}");
    assert!(namen.contains(&"h1"), "{namen:?}");
    assert!(!blatt.befund.sauber());
}

/// KAPUTTE DEKLARATIONEN: nur sie fallen weg, der Rest der Regel bleibt.
#[test]
fn test_kaputte_deklarationen_ueberspringen() {
    let blatt = parser::parsen("p { color: red; das-ist-kein-css; ; margin: 0; :leer; x: }");
    let regel = &blatt.regeln[0];
    let namen: Vec<&str> = regel.deklarationen.iter().map(|d| d.name.as_str()).collect();
    assert!(namen.contains(&"color"), "{namen:?}");
    assert!(namen.contains(&"margin"), "{namen:?}");
    assert!(blatt.befund.deklarationen_uebersprungen >= 2);
}

/// DIE WICHTIGSTE PARSER-ZUSAGE: `@media` wird sauber uebersprungen.
///
/// Wer nur die Zeile ueberspringt, laesst den Block offen — dann werden
/// die Regeln DARIN als Regeln auf oberster Ebene gelesen, und eine
/// Druck- oder Handy-Formatierung schlaegt auf den Desktop durch. Das ist
/// schlimmer, als sie wegzulassen.
#[test]
fn test_media_block_wird_sauber_uebersprungen() {
    let css = "
        p { color: red }
        @media print {
            p { color: green }
            h1 { display: none }
        }
        @media screen and (max-width: 600px) {
            body { margin: 0 }
            .a { color: pink }
        }
        h2 { color: blue }
    ";
    let blatt = parser::parsen(css);
    assert_eq!(blatt.befund.at_regeln_uebersprungen, 2);

    // GENAU zwei Regeln haben ueberlebt: p und h2.
    assert_eq!(blatt.regeln.len(), 2, "{:?}", blatt.regeln);
    assert_eq!(blatt.regeln[0].selektoren[0].text, "p");
    assert_eq!(blatt.regeln[1].selektoren[0].text, "h2");
    // Und die Farbe aus dem Druck-Block ist NICHT durchgeschlagen.
    assert_eq!(blatt.regeln[0].deklarationen[0].wert, "red");
}

/// Verschachtelte At-Regeln (`@supports` um `@media`) — die balancierte
/// Klammerung muss auch das aushalten.
#[test]
fn test_verschachtelte_at_regeln() {
    let css = "
        @supports (display: grid) {
            @media screen {
                div { color: red }
            }
            span { color: blue }
        }
        p { color: green }
    ";
    let blatt = parser::parsen(css);
    assert_eq!(blatt.regeln.len(), 1);
    assert_eq!(blatt.regeln[0].selektoren[0].text, "p");
}

/// Eine geschweifte Klammer in einer Zeichenkette beendet den Block NICHT.
#[test]
fn test_klammer_in_zeichenkette() {
    let css = r#" a::before { content: "}" ; color: red } p { color: blue } "#;
    let blatt = parser::parsen(css);
    // `a::before` ist ein Pseudo-ELEMENT und damit unerfuellbar — aber
    // `p` muss trotzdem gefunden werden.
    let hat_p = blatt
        .regeln
        .iter()
        .any(|r| r.selektoren.iter().any(|s| s.text == "p"));
    assert!(hat_p, "die Klammer in der Zeichenkette hat den Parser verwirrt");
}

#[test]
fn test_abgeschnittenes_stylesheet() {
    for css in [
        "p { color: red",
        "p { color:",
        "p {",
        "p",
        "@media print {",
        "/*",
        "",
    ] {
        let blatt = parser::parsen(css);
        // Kein Panic, kein Haenger — das ist die Zusage.
        let _ = blatt.regeln.len();
    }
}

#[test]
fn test_doppelte_eigenschaft_spaetere_gewinnt() {
    let blatt = parser::parsen("p { color: red; color: blue }");
    assert_eq!(blatt.regeln[0].deklarationen.len(), 1);
    assert_eq!(blatt.regeln[0].deklarationen[0].wert, "blue");
}

/// Im selben Block schlaegt `!important` einen spaeteren gewoehnlichen
/// Wert — das ist das uebliche Rueckfall-Muster.
#[test]
fn test_important_im_selben_block() {
    let blatt = parser::parsen("p { color: red !important; color: blue }");
    assert_eq!(blatt.regeln[0].deklarationen[0].wert, "red");
    assert!(blatt.regeln[0].deklarationen[0].wichtig);
}

#[test]
fn test_important_schreibweisen() {
    for css in [
        "p { color: red !important }",
        "p { color: red!important }",
        "p { color: red ! important }",
        "p { color: red !IMPORTANT }",
    ] {
        let blatt = parser::parsen(css);
        assert!(
            blatt.regeln[0].deklarationen[0].wichtig,
            "nicht erkannt: {css}"
        );
        assert_eq!(blatt.regeln[0].deklarationen[0].wert, "red");
    }
}

// ===========================================================================
// 2. SELEKTOREN UND SPEZIFITAET
// ===========================================================================

/// DIE BEKANNTEN BEISPIELE aus der CSS-Spezifikation (§ Kaskade).
#[test]
fn test_spezifitaet_gegen_bekannte_beispiele() {
    assert_eq!(spez("*"), Spezifitaet { ids: 0, klassen: 0, typen: 0 });
    assert_eq!(spez("li"), Spezifitaet { ids: 0, klassen: 0, typen: 1 });
    assert_eq!(spez("ul li"), Spezifitaet { ids: 0, klassen: 0, typen: 2 });
    assert_eq!(
        spez("ul ol li"),
        Spezifitaet { ids: 0, klassen: 0, typen: 3 }
    );
    assert_eq!(
        spez("li.rot"),
        Spezifitaet { ids: 0, klassen: 1, typen: 1 }
    );
    assert_eq!(
        spez("ul ol li.rot"),
        Spezifitaet { ids: 0, klassen: 1, typen: 3 }
    );
    assert_eq!(
        spez("#x34y"),
        Spezifitaet { ids: 1, klassen: 0, typen: 0 }
    );
    assert_eq!(
        spez("div p.warn#haupt"),
        Spezifitaet { ids: 1, klassen: 1, typen: 2 }
    );
    // Pseudoklassen zaehlen wie Klassen.
    assert_eq!(
        spez("a:link"),
        Spezifitaet { ids: 0, klassen: 1, typen: 1 }
    );
}

/// DIE FALLE: Spezifitaet ist LEXIKOGRAFISCH, nicht ausgerechnet.
///
/// Wer `ids*100 + klassen*10 + typen` rechnet, laesst elf Klassen eine Id
/// schlagen. Das ist falsch — eine Id schlaegt BELIEBIG viele Klassen.
#[test]
fn test_spezifitaet_ist_lexikografisch() {
    let eine_id = spez("#a");
    let viele_klassen = spez(".a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t");
    assert!(
        eine_id > viele_klassen,
        "eine Id ({eine_id:?}) muss 20 Klassen ({viele_klassen:?}) schlagen"
    );

    let eine_klasse = spez(".a");
    let viele_typen = spez("a b c d e f g h i j k l m n o p q r s t");
    assert!(eine_klasse > viele_typen);
}

/// Konstrukte, die wir nicht koennen, machen den Selektor UNERFUELLBAR —
/// sie werden nicht als etwas Aehnliches gedeutet.
#[test]
fn test_unbekannte_selektoren_passen_nie() {
    for text in [
        "div > p",       // Kind-Kombinator
        "h1 + p",        // Nachbar
        "h1 ~ p",        // Geschwister
        "a[href]",       // Attribut
        "input[type=x]",
        "p:not(.a)",     // funktionale Pseudoklasse
        "li:nth-child(2)",
        "p::before",     // Pseudo-Element
    ] {
        assert!(
            parser::selektor_parsen(text).is_none(),
            "'{text}' haette abgelehnt werden muessen"
        );
    }
}

/// Ein abgelehnter Selektor darf die anderen der Liste nicht mitnehmen.
#[test]
fn test_abgelehnter_selektor_nimmt_die_liste_nicht_mit() {
    let (dokument, baum) = rechnen(
        "<h1>x</h1><p>y</p>",
        "h1, p:not(.a) { color: #ff0000 }",
    );
    assert_eq!(stil_von(&dokument, &baum, "h1").farbe, Farbe::rgb(255, 0, 0));
    // `p` wurde vom unerfuellbaren Selektor NICHT eingefaerbt.
    assert_ne!(stil_von(&dokument, &baum, "p").farbe, Farbe::rgb(255, 0, 0));
}

/// Nachkommen-Selektoren: `div p` passt auf jeden `p` UNTERHALB eines
/// `div`, nicht nur auf direkte Kinder.
#[test]
fn test_nachkomme_passt_ueber_ebenen() {
    let (dokument, baum) = rechnen(
        "<div><section><p id=tief>x</p></section></div><p id=aussen>y</p>",
        "div p { color: #00ff00 }",
    );
    let tief = dokument
        .alle()
        .find(|(_, k)| k.attribut("id") == Some("tief"))
        .map(|(id, _)| id)
        .unwrap();
    let aussen = dokument
        .alle()
        .find(|(_, k)| k.attribut("id") == Some("aussen"))
        .map(|(id, _)| id)
        .unwrap();
    assert_eq!(baum.stil(tief).farbe, Farbe::rgb(0, 255, 0));
    assert_ne!(baum.stil(aussen).farbe, Farbe::rgb(0, 255, 0));
}

/// Klassennamen sind gross-/kleinschreibungsempfindlich, Tag-Namen nicht.
#[test]
fn test_gross_und_kleinschreibung() {
    let (dokument, baum) = rechnen(
        "<DIV class='Warn'>x</DIV>",
        "div.Warn { color: #123456 }",
    );
    assert_eq!(stil_von(&dokument, &baum, "div").farbe, Farbe::rgb(0x12, 0x34, 0x56));

    let (dokument, baum) = rechnen("<div class='Warn'>x</div>", "div.warn { color: #123456 }");
    assert_ne!(
        stil_von(&dokument, &baum, "div").farbe,
        Farbe::rgb(0x12, 0x34, 0x56),
        "Klassennamen sind gross-/kleinschreibungsempfindlich"
    );
}

// ===========================================================================
// 3. KASKADE
// ===========================================================================

/// Bei gleicher Spezifitaet gewinnt die SPAETERE Regel.
#[test]
fn test_reihenfolge_entscheidet_bei_gleichstand() {
    let (dokument, baum) = rechnen(
        "<p>x</p>",
        "p { color: #ff0000 } p { color: #0000ff }",
    );
    assert_eq!(stil_von(&dokument, &baum, "p").farbe, Farbe::rgb(0, 0, 255));
}

/// Hoehere Spezifitaet schlaegt spaetere Position.
#[test]
fn test_spezifitaet_schlaegt_reihenfolge() {
    let (dokument, baum) = rechnen(
        "<p id=x class=y>t</p>",
        "#x { color: #ff0000 } p.y { color: #0000ff } p { color: #00ff00 }",
    );
    assert_eq!(
        stil_von(&dokument, &baum, "p").farbe,
        Farbe::rgb(255, 0, 0),
        "die Id-Regel muss gewinnen, obwohl sie zuerst steht"
    );
}

/// AUTOR SCHLAEGT STANDARD — auch bei niedrigerer Spezifitaet.
#[test]
fn test_autor_schlaegt_standard() {
    // Das Standard-Stylesheet sagt `h1 { font-weight: bold }`.
    let (dokument, baum) = rechnen("<h1>x</h1>", "h1 { font-weight: normal }");
    assert!(
        !stil_von(&dokument, &baum, "h1").fett,
        "die Autor-Regel muss den Standard schlagen"
    );

    // Und der Standard-Rand von <p> laesst sich wegnehmen.
    let (dokument, baum) = rechnen("<p>x</p>", "p { margin: 0 }");
    assert_eq!(stil_von(&dokument, &baum, "p").margin.oben, Laenge::Px(0));
}

/// `!important` schlaegt alles im Autor-Stylesheet — auch hoehere
/// Spezifitaet.
#[test]
fn test_important() {
    let (dokument, baum) = rechnen(
        "<p id=x class=y>t</p>",
        "#x { color: #ff0000 } p { color: #0000ff !important }",
    );
    assert_eq!(
        stil_von(&dokument, &baum, "p").farbe,
        Farbe::rgb(0, 0, 255),
        "!important schlaegt die Id-Regel"
    );
}

/// Zwei `!important` untereinander: Spezifitaet entscheidet wieder.
#[test]
fn test_important_gegen_important() {
    let (dokument, baum) = rechnen(
        "<p id=x>t</p>",
        "p { color: #0000ff !important } #x { color: #ff0000 !important }",
    );
    assert_eq!(stil_von(&dokument, &baum, "p").farbe, Farbe::rgb(255, 0, 0));
}

/// Ein Inline-Stil schlaegt jeden Selektor.
#[test]
fn test_inline_stil_schlaegt_alles() {
    let (dokument, baum) = rechnen(
        "<p id=x style='color: #00ff00'>t</p>",
        "#x { color: #ff0000 }",
    );
    assert_eq!(stil_von(&dokument, &baum, "p").farbe, Farbe::rgb(0, 255, 0));
}

// ===========================================================================
// 4. VERERBUNG
// ===========================================================================

/// DER KERNTEST: `color` erbt, `margin` nicht.
#[test]
fn test_color_erbt_margin_nicht() {
    let (dokument, baum) = rechnen(
        "<div><p><span>tief</span></p></div>",
        "div { color: #ff8800; margin: 50px }",
    );
    let orange = Farbe::rgb(0xff, 0x88, 0x00);

    assert_eq!(stil_von(&dokument, &baum, "div").farbe, orange);
    assert_eq!(
        stil_von(&dokument, &baum, "p").farbe,
        orange,
        "color muss erben"
    );
    assert_eq!(
        stil_von(&dokument, &baum, "span").farbe,
        orange,
        "color muss ueber zwei Ebenen erben"
    );

    assert_eq!(stil_von(&dokument, &baum, "div").margin.oben, Laenge::px(50));
    assert_ne!(
        stil_von(&dokument, &baum, "span").margin.oben,
        Laenge::px(50),
        "margin darf NICHT erben"
    );
}

/// Weitere erbende und nicht erbende Eigenschaften, in einem Durchgang.
#[test]
fn test_vererbungstabelle() {
    let (dokument, baum) = rechnen(
        "<div><span>x</span></div>",
        "div {
            color: #010203;
            font-weight: bold;
            font-style: italic;
            text-align: center;
            list-style-type: square;
            background-color: #ffeedd;
            padding: 7px;
            width: 300px;
            display: block;
         }",
    );
    let span = stil_von(&dokument, &baum, "span");

    // Geerbt:
    assert_eq!(span.farbe, Farbe::rgb(1, 2, 3));
    assert!(span.fett);
    assert!(span.kursiv);
    assert_eq!(span.ausrichtung, Ausrichtung::Mitte);
    assert_eq!(span.listenzeichen, Listenzeichen::Quadrat);

    // Nicht geerbt:
    assert!(span.hintergrund.ist_durchsichtig(), "background erbt nicht");
    assert_eq!(span.padding.oben, Laenge::Px(0), "padding erbt nicht");
    assert_eq!(span.breite, Laenge::Auto, "width erbt nicht");
    assert_eq!(
        span.display,
        Display::Inline,
        "display erbt nicht — <span> bleibt inline"
    );
}

/// `inherit` erzwingt Vererbung auch dort, wo sie nicht gilt.
#[test]
fn test_inherit_schluesselwort() {
    let (dokument, baum) = rechnen(
        "<div><span>x</span></div>",
        "div { background-color: #ff0000 } span { background-color: inherit }",
    );
    assert_eq!(
        stil_von(&dokument, &baum, "span").hintergrund,
        Farbe::rgb(255, 0, 0)
    );
}

/// `initial` setzt auf den Anfangswert zurueck — auch gegen den Standard.
#[test]
fn test_initial_schluesselwort() {
    let (dokument, baum) = rechnen("<h1>x</h1>", "h1 { font-weight: initial }");
    assert!(!stil_von(&dokument, &baum, "h1").fett);
}

/// Text erbt den Stil seines Elternteils — das Layout will es so.
#[test]
fn test_textknoten_erben() {
    let (dokument, baum) = rechnen("<p>Text</p>", "p { color: #abcdef }");
    let p = dokument.erstes("p").unwrap();
    let text = dokument.knoten(p).unwrap().kinder[0];
    assert_eq!(baum.stil(text).farbe, Farbe::rgb(0xab, 0xcd, 0xef));
}

// ===========================================================================
// 5. EINHEITEN
// ===========================================================================

/// `em` bezieht sich bei `font-size` auf den ELTERNTEIL, bei allen
/// anderen Eigenschaften auf DIESES Element.
///
/// DAS IST DIE FALLE. Bei
///     div { font-size: 20px }
///     p   { font-size: 2em; margin: 1em }
/// ist die Schrift 40 px (2 x 20, vom Elternteil) und der Rand 40 px
/// (1 x 40, vom eigenen Wert) — NICHT 20.
#[test]
fn test_em_bezugsgroesse() {
    let (dokument, baum) = rechnen(
        "<div><p>x</p></div>",
        "div { font-size: 20px } p { font-size: 2em; margin: 1em }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.schrift_px, 40_000, "2em von 20px sind 40px");
    assert_eq!(
        p.margin.oben,
        Laenge::px(40),
        "1em bezieht sich auf die EIGENE Schriftgroesse (40px), nicht auf 20px"
    );
}

/// Und das gilt auch, wenn `margin` VOR `font-size` im Block steht.
#[test]
fn test_em_unabhaengig_von_der_reihenfolge_im_block() {
    let (dokument, baum) = rechnen(
        "<div><p>x</p></div>",
        "div { font-size: 10px } p { margin: 1em; font-size: 3em }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.schrift_px, 30_000);
    assert_eq!(p.margin.oben, Laenge::px(30));
}

/// `em` kettet sich ueber mehrere Ebenen.
#[test]
fn test_em_kette() {
    let (dokument, baum) = rechnen(
        "<div><section><p>x</p></section></div>",
        "div { font-size: 10px } section { font-size: 2em } p { font-size: 2em }",
    );
    assert_eq!(stil_von(&dokument, &baum, "section").schrift_px, 20_000);
    assert_eq!(stil_von(&dokument, &baum, "p").schrift_px, 40_000);
}

/// `%` bei `font-size` bezieht sich auf den Elternteil und wird SOFORT
/// aufgeloest; `%` bei `width` bleibt STEHEN (das Layout entscheidet).
#[test]
fn test_prozent() {
    let (dokument, baum) = rechnen(
        "<div><p>x</p></div>",
        "div { font-size: 20px } p { font-size: 50%; width: 50% }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.schrift_px, 10_000, "50% von 20px");
    assert_eq!(
        p.breite,
        Laenge::Prozent(50_000),
        "width: 50% MUSS bis zum Layout stehen bleiben"
    );
}

#[test]
fn test_laengen_parsen() {
    use crate::werte::laenge_parsen;
    assert_eq!(laenge_parsen("0"), Some(Laenge::Px(0)));
    assert_eq!(laenge_parsen("10px"), Some(Laenge::Px(10_000)));
    assert_eq!(laenge_parsen("1.5px"), Some(Laenge::Px(1_500)));
    assert_eq!(laenge_parsen("-3px"), Some(Laenge::Px(-3_000)));
    assert_eq!(laenge_parsen(".5em"), Some(Laenge::Em(500)));
    assert_eq!(laenge_parsen("2EM"), Some(Laenge::Em(2_000)));
    assert_eq!(laenge_parsen("50%"), Some(Laenge::Prozent(50_000)));
    assert_eq!(laenge_parsen("auto"), Some(Laenge::Auto));
    // pt -> px mit 4/3.
    assert_eq!(laenge_parsen("12pt"), Some(Laenge::Px(16_000)));
    // Was wir nicht koennen, wird ABGELEHNT (nicht geraten).
    assert_eq!(laenge_parsen("5vw"), None);
    assert_eq!(laenge_parsen("2cm"), None);
    assert_eq!(laenge_parsen("calc(100% - 10px)"), None);
    assert_eq!(laenge_parsen("10"), None, "eine nackte Zahl ist nur als 0 gueltig");
    assert_eq!(laenge_parsen(""), None);
    assert_eq!(laenge_parsen("px"), None);
}

#[test]
fn test_farben_parsen() {
    use crate::werte::farbe_parsen;
    assert_eq!(farbe_parsen("#f00"), Some(Farbe::rgb(255, 0, 0)));
    assert_eq!(farbe_parsen("#ff0000"), Some(Farbe::rgb(255, 0, 0)));
    assert_eq!(farbe_parsen("#FF0000"), Some(Farbe::rgb(255, 0, 0)));
    assert_eq!(farbe_parsen("red"), Some(Farbe::rgb(255, 0, 0)));
    assert_eq!(farbe_parsen("RED"), Some(Farbe::rgb(255, 0, 0)));
    assert_eq!(farbe_parsen("rgb(1, 2, 3)"), Some(Farbe::rgb(1, 2, 3)));
    assert_eq!(farbe_parsen("rgb(1,2,3)"), Some(Farbe::rgb(1, 2, 3)));
    assert_eq!(
        farbe_parsen("rgba(1, 2, 3, 0.5)"),
        Some(Farbe::rgba(1, 2, 3, 127))
    );
    assert!(farbe_parsen("transparent").unwrap().ist_durchsichtig());
    assert_eq!(farbe_parsen("gibtsnicht"), None);
    assert_eq!(farbe_parsen("#12345"), None);
}

/// Die Kurzform mit ein bis vier Werten (die Uhrzeiger-Regel).
#[test]
fn test_kanten_kurzform() {
    let faelle: &[(&str, [i32; 4])] = &[
        ("margin: 1px", [1, 1, 1, 1]),
        ("margin: 1px 2px", [1, 2, 1, 2]),
        ("margin: 1px 2px 3px", [1, 2, 3, 2]),
        ("margin: 1px 2px 3px 4px", [1, 2, 3, 4]),
    ];
    for (css, [o, r, u, l] ) in faelle {
        let (dokument, baum) = rechnen("<p>x</p>", &alloc::format!("p {{ {css} }}"));
        let m = stil_von(&dokument, &baum, "p").margin;
        assert_eq!(m.oben, Laenge::px(*o), "{css}");
        assert_eq!(m.rechts, Laenge::px(*r), "{css}");
        assert_eq!(m.unten, Laenge::px(*u), "{css}");
        assert_eq!(m.links, Laenge::px(*l), "{css}");
    }
}

/// `border: 1px solid red` in beliebiger Reihenfolge.
#[test]
fn test_border_kurzform() {
    for css in [
        "border: 1px solid #ff0000",
        "border: solid 1px #ff0000",
        "border: #ff0000 1px solid",
    ] {
        let (dokument, baum) = rechnen("<p>x</p>", &alloc::format!("p {{ {css} }}"));
        let s = stil_von(&dokument, &baum, "p");
        assert_eq!(s.rahmen_breite.oben, Laenge::px(1), "{css}");
        assert_eq!(s.rahmen_stil.oben, RahmenStil::Durchgezogen, "{css}");
        assert_eq!(s.rahmen_farbe.oben, Farbe::rgb(255, 0, 0), "{css}");
    }
}

/// `border: solid red` ohne Breite bedeutet `medium` (3px) — NICHT 0.
/// Ein Rahmen, der stillschweigend unsichtbar ist, wird im Renderer
/// gesucht.
#[test]
fn test_border_ohne_breite_ist_medium() {
    let (dokument, baum) = rechnen("<p>x</p>", "p { border: solid #ff0000 }");
    assert_eq!(stil_von(&dokument, &baum, "p").rahmen_breite.oben, Laenge::px(3));
}

/// `line-height` als nackte Zahl ist ein FAKTOR und wird als solcher
/// vererbt — der Unterschied zu `150%` zeigt sich erst beim Kind.
#[test]
fn test_line_height_faktor_vs_prozent() {
    let (dokument, baum) = rechnen(
        "<div><p>x</p></div>",
        "div { font-size: 10px; line-height: 1.5 } p { font-size: 20px }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.zeilenhoehe, Zeilenhoehe::Faktor(1_500));
    assert_eq!(
        p.zeilenhoehe_px(),
        30_000,
        "der FAKTOR wird vererbt: 1.5 x 20px = 30px"
    );

    // Mit Prozent wird der BERECHNETE Wert vererbt.
    let (dokument, baum) = rechnen(
        "<div><p>x</p></div>",
        "div { font-size: 10px; line-height: 150% } p { font-size: 20px }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(
        p.zeilenhoehe_px(),
        15_000,
        "Prozent wird beim Elternteil ausgerechnet: 1.5 x 10px = 15px"
    );
}

#[test]
fn test_font_family_abbildung() {
    let (dokument, baum) = rechnen("<p>x</p>", "p { font-family: monospace }");
    assert_eq!(stil_von(&dokument, &baum, "p").familie, Familie::Monospace);

    let (dokument, baum) = rechnen("<p>x</p>", "p { font-family: Georgia, serif }");
    assert_eq!(stil_von(&dokument, &baum, "p").familie, Familie::Proportional);

    // Die ERSTE erkennbare Angabe gewinnt, nicht die generische am Ende.
    let (dokument, baum) = rechnen("<p>x</p>", "p { font-family: 'Courier New', sans-serif }");
    assert_eq!(stil_von(&dokument, &baum, "p").familie, Familie::Monospace);
}

/// Unbekannte Eigenschaften und unlesbare Werte fallen weg — der Rest
/// der Regel bleibt.
#[test]
fn test_unbekanntes_faellt_weg() {
    let (dokument, baum) = rechnen(
        "<p>x</p>",
        "p { -webkit-hyphens: auto; color: #ff0000; transform: rotate(3deg); width: 5vw }",
    );
    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.farbe, Farbe::rgb(255, 0, 0), "color muss durchkommen");
    assert_eq!(p.breite, Laenge::Auto, "5vw koennen wir nicht — bleibt auto");
}

// ===========================================================================
// 6. DAS STANDARD-STYLESHEET
// ===========================================================================

/// Es parst sauber — der Selbsttest des Parsers bei jedem Start.
#[test]
fn test_standard_stylesheet_parst_sauber() {
    let blatt = standard_stylesheet();
    assert!(blatt.regeln.len() > 40, "nur {} Regeln", blatt.regeln.len());
    assert_eq!(
        blatt.befund.regeln_uebersprungen, 0,
        "keine Regel darf uebersprungen werden"
    );
    assert_eq!(blatt.befund.deklarationen_uebersprungen, 0);
    assert_eq!(
        blatt.befund.selektoren_unerfuellbar, 0,
        "das Standard-Stylesheet darf nur Selektoren benutzen, die wir koennen"
    );
    assert!(!blatt.befund.abgeschnitten);
}

/// DER TEST, DER DIE ARCHITEKTUR FESTNAGELT: Ohne Standard-Stylesheet ist
/// ALLES inline und unformatiert.
///
/// Das ist der Beweis, dass der Renderer kein HTML-Wissen enthaelt: Ein
/// `<h1>` ist nur deshalb gross und fett, weil ein Stylesheet es sagt.
#[test]
fn test_ohne_standard_ist_alles_inline() {
    let (dokument, baum) = rechnen_ohne_standard("<h1>Titel</h1><p>Text</p>", "");
    let h1 = stil_von(&dokument, &baum, "h1");
    assert_eq!(h1.display, Display::Inline, "ohne Stylesheet ist alles inline");
    assert!(!h1.fett, "ohne Stylesheet ist nichts fett");
    assert_eq!(h1.schrift_px, 16_000, "ohne Stylesheet ist alles gleich gross");
    assert_eq!(stil_von(&dokument, &baum, "p").margin.oben, Laenge::Px(0));
}

/// MIT Standard-Stylesheet sieht es aus wie im Browser.
#[test]
fn test_standard_stylesheet_greift() {
    let (dokument, baum) = rechnen(
        "<body><h1>Titel</h1><p>Text</p><ul><li>Punkt</li></ul>\
         <pre>code</pre><a href=/x>Link</a><strong>fett</strong><em>kursiv</em></body>",
        "",
    );

    let h1 = stil_von(&dokument, &baum, "h1");
    assert_eq!(h1.display, Display::Block);
    assert!(h1.fett, "h1 muss fett sein");
    assert_eq!(h1.schrift_px, 32_000, "h1 ist 2em von 16px");

    let p = stil_von(&dokument, &baum, "p");
    assert_eq!(p.display, Display::Block);
    // 1em Rand bei 16px Schrift.
    assert_eq!(p.margin.oben, Laenge::px(16));

    assert_eq!(stil_von(&dokument, &baum, "li").display, Display::Listenpunkt);
    assert_eq!(stil_von(&dokument, &baum, "ul").listenzeichen, Listenzeichen::Punkt);
    assert_eq!(stil_von(&dokument, &baum, "pre").familie, Familie::Monospace);
    assert!(stil_von(&dokument, &baum, "strong").fett);
    assert!(stil_von(&dokument, &baum, "em").kursiv);

    // <body> hat seinen 8px-Rand.
    assert_eq!(stil_von(&dokument, &baum, "body").margin.oben, Laenge::px(8));

    // Ein Link ist blau und unterstrichen.
    let a = stil_von(&dokument, &baum, "a");
    assert_eq!(a.farbe, Farbe::rgb(0, 0, 0xee));
    assert!(a.dekoration.unterstrichen);
}

/// `<head>` und was darin steht, erscheint NICHT — sonst stuenden Titel
/// und Skripte als Text oben auf der Seite.
#[test]
fn test_kopfbereich_ist_unsichtbar() {
    let (dokument, baum) = rechnen(
        "<html><head><title>T</title><style>x{}</style><script>var a=1</script></head>\
         <body><p>sichtbar</p></body></html>",
        "",
    );
    for tag in ["head", "title", "style", "script"] {
        assert_eq!(
            stil_von(&dokument, &baum, tag).display,
            Display::Keine,
            "<{tag}> muss display:none haben"
        );
    }
    assert_eq!(stil_von(&dokument, &baum, "p").display, Display::Block);
}

/// `a:link` passt nur auf `<a>` MIT `href` — ein Anker ohne Ziel bleibt
/// schwarz.
#[test]
fn test_anker_ohne_href_ist_kein_link() {
    let (dokument, baum) = rechnen("<a name=oben>Anker</a>", "");
    let a = stil_von(&dokument, &baum, "a");
    assert_eq!(a.farbe, Farbe::SCHWARZ, "ein Anker ohne href ist kein Link");
    assert!(!a.dekoration.unterstrichen);
}

/// Verschachtelte Listen wechseln das Aufzaehlungszeichen und bekommen
/// keinen zusaetzlichen Aussenabstand.
#[test]
fn test_verschachtelte_listen() {
    let (dokument, baum) = rechnen("<ul><li>a<ul><li>b</li></ul></li></ul>", "");
    let innere = dokument.alle_mit_tag("ul").nth(1).expect("zweites <ul>");
    assert_eq!(baum.stil(innere).listenzeichen, Listenzeichen::Kreis);
    assert_eq!(baum.stil(innere).margin.oben, Laenge::Px(0));
}

// ===========================================================================
// 7. ERKLAEREN (die Grundlage von cssdump)
// ===========================================================================

/// Die Erklaerung muss zu dem passen, was `berechnen` liefert — sonst
/// zeigt `cssdump` etwas anderes an, als der Renderer benutzt.
#[test]
fn test_erklaerung_stimmt_mit_berechnung() {
    let html = "<div class=a><p id=b style='color: #00ff00'>x</p></div>";
    let css = "div { font-size: 20px } .a p { margin: 2em } #b { text-align: center }";
    let (dokument, baum) = rechnen(html, css);

    let p = dokument.erstes("p").unwrap();
    let eltern = dokument.knoten(p).unwrap().eltern.unwrap();

    let standard = standard_stylesheet();
    let autor = parser::parsen(css);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    let erklaerungen = kaskade::erklaeren(
        &dokument,
        p,
        &blaetter,
        baum.stil(eltern),
        Zustand::default(),
    );

    for erklaerung in &erklaerungen {
        let aus_baum = stil::wert_als_text(baum.stil(p), erklaerung.eigenschaft);
        assert_eq!(
            erklaerung.wert, aus_baum,
            "{} weicht ab",
            erklaerung.eigenschaft
        );
    }
}

/// Die Erklaerung nennt die richtige Quelle.
#[test]
fn test_erklaerung_nennt_die_quelle() {
    let html = "<div><p>x</p></div>";
    let css = "p { color: #ff0000 }";
    let (dokument, baum) = rechnen(html, css);
    let p = dokument.erstes("p").unwrap();
    let eltern = dokument.knoten(p).unwrap().eltern.unwrap();

    let standard = standard_stylesheet();
    let autor = parser::parsen(css);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    let erklaerungen =
        kaskade::erklaeren(&dokument, p, &blaetter, baum.stil(eltern), Zustand::default());

    let finde = |name: &str| {
        erklaerungen
            .iter()
            .find(|e| e.eigenschaft == name)
            .unwrap_or_else(|| panic!("{name} fehlt in der Erklaerung"))
    };

    // color kommt vom Autor.
    match &finde("color").quelle {
        Quelle::Regel { herkunft, selektor, .. } => {
            assert_eq!(*herkunft, Herkunft::Autor);
            assert_eq!(selektor, "p");
        }
        andere => panic!("color sollte von einer Regel kommen, ist aber {andere:?}"),
    }
    // display kommt aus dem Standard-Stylesheet.
    match &finde("display").quelle {
        Quelle::Regel { herkunft, .. } => assert_eq!(*herkunft, Herkunft::Standard),
        andere => panic!("display sollte aus dem Standard kommen, ist aber {andere:?}"),
    }
    // width hat niemand gesetzt.
    assert_eq!(finde("width").quelle, Quelle::Anfangswert);
}

/// Überstimmte Regeln werden mitgeliefert — das ist beim Debuggen die
/// eigentliche Frage („warum gilt DAS und nicht meins?").
#[test]
fn test_erklaerung_zeigt_ueberstimmte_regeln() {
    let html = "<p id=x class=y>t</p>";
    let css = "p { color: #111111 } .y { color: #222222 } #x { color: #333333 }";
    let (dokument, _baum) = rechnen(html, css);
    let p = dokument.erstes("p").unwrap();

    let standard = standard_stylesheet();
    let autor = parser::parsen(css);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    let erklaerungen =
        kaskade::erklaeren(&dokument, p, &blaetter, &crate::stil::ANFANG, Zustand::default());
    let farbe = erklaerungen.iter().find(|e| e.eigenschaft == "color").unwrap();

    assert_eq!(farbe.wert, "#333333");
    assert_eq!(farbe.ueberstimmt.len(), 2, "zwei Regeln haben verloren");
}

// ===========================================================================
// 8. ROBUSTHEIT
// ===========================================================================

/// Muell darf nicht panicken — dieselbe Zusage wie bei speedhtml.
#[test]
fn test_muell_panickt_nicht() {
    let vorrat: &[u8] = b"{}:;,.#*>+~ abc/*!@()[]\"'-0123%\n";
    let mut zustand: u32 = 0x2468_ACE0;
    let mut folge = String::new();

    for durchgang in 0..300 {
        folge.clear();
        let laenge = 1 + (durchgang * 11) % 500;
        for _ in 0..laenge {
            zustand = zustand.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let i = (zustand >> 16) as usize % vorrat.len();
            folge.push(vorrat[i] as char);
        }
        let blatt = parser::parsen(&folge);
        // Und die Kaskade damit ebenfalls.
        let dokument = speedhtml::parsen("<div><p class=a id=b>x</p></div>");
        let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![(&blatt, Herkunft::Autor)];
        let _ = kaskade::berechnen(&dokument, &blaetter, Zustand::default());
    }
}

/// Ein sehr grosses Stylesheet laeuft in eine Grenze, statt den Speicher
/// zu fuellen.
#[test]
fn test_grosses_stylesheet_laeuft_in_die_grenze() {
    let mut css = String::new();
    while css.len() < 4 * 1024 * 1024 {
        css.push_str(".a1.a2.a3 div p { color: red; margin: 1px 2px 3px 4px }\n");
    }
    let grenzen = parser::Grenzen {
        max_regeln: 1000,
        ..parser::Grenzen::standard()
    };
    let blatt = parser::parsen_mit(&css, grenzen);
    assert!(blatt.befund.abgeschnitten);
    assert!(blatt.regeln.len() <= 1000);
}

/// Ein tiefes Dokument mit vielen Regeln — die Kaskade darf weder
/// panicken noch rekursiv den Stack sprengen.
#[test]
fn test_tiefes_dokument() {
    let mut html = String::new();
    for _ in 0..90 {
        html.push_str("<div class=x>");
    }
    html.push_str("<p>tief</p>");
    let (dokument, baum) = rechnen(&html, ".x { color: #ff0000; font-size: 1.01em }");
    // Die Farbe ist bis nach unten geerbt.
    let p = dokument.erstes("p").unwrap();
    assert_eq!(baum.stil(p).farbe, Farbe::rgb(255, 0, 0));
    // Und die em-Kette hat die Schrift wachsen lassen.
    assert!(baum.stil(p).schrift_px > 16_000);
}

/// Ein leeres Dokument und ein leeres Stylesheet gehen auch.
#[test]
fn test_leere_eingaben() {
    let (_, baum) = rechnen("", "");
    assert!(baum.anzahl() >= 1);
    let (_, baum) = rechnen("<p>x</p>", "");
    assert!(baum.anzahl() > 1);
}

/// Der bequeme Weg (`stile_berechnen`) liest die `<style>`-Bloecke des
/// Dokuments.
#[test]
fn test_style_bloecke_im_dokument() {
    let dokument = speedhtml::parsen(
        "<html><head><style>p { color: #ff0000 }</style></head><body><p>x</p></body></html>",
    );
    let baum = crate::stile_berechnen(&dokument);
    let p = dokument.erstes("p").unwrap();
    assert_eq!(baum.stil(p).farbe, Farbe::rgb(255, 0, 0));
}

/// Die Zahl-zu-Text-Umrechnung fuer die Anzeige.
#[test]
fn test_tausendstel_text() {
    use crate::stil::tausendstel_text;
    assert_eq!(tausendstel_text(16_000), "16");
    assert_eq!(tausendstel_text(1_500), "1.5");
    assert_eq!(tausendstel_text(0), "0");
    assert_eq!(tausendstel_text(-3_000), "-3");
    assert_eq!(tausendstel_text(500), "0.5");
    assert_eq!(tausendstel_text(1_050), "1.05");
}

// ===========================================================================
// EXTERNE STYLESHEETS — WAS EIN DOKUMENT BRAUCHT (Serie 9, Teil 1)
// ===========================================================================
//
// Diese Kiste HOLT nichts; sie sagt nur, WAS zu holen waere. Getestet wird
// deshalb genau das: die Liste und — wichtiger — ihre REIHENFOLGE.

use crate::Blattbedarf;

/// Kurzform: die Bedarfsliste eines HTML-Schnipsels.
fn bedarf(html: &str) -> Vec<Blattbedarf> {
    crate::blaetter_einsammeln(&speedhtml::parsen(html))
}

/// Die externen Verweise daraus, als Text.
fn externe(html: &str) -> Vec<String> {
    bedarf(html)
        .into_iter()
        .filter_map(|b| match b {
            Blattbedarf::Extern(href) => Some(href),
            Blattbedarf::Inline(_) => None,
        })
        .collect()
}

/// **DIE REIHENFOLGE IST DAS EIGENTLICHE ERGEBNIS.**
///
/// Ein `<style>` VOR einem `<link>` ist schwaecher, ein `<style>` DANACH
/// staerker. Wer erst alle externen und dann alle inline einsortiert,
/// bekommt auf jeder Seite, die beides mischt, eine andere Darstellung
/// als jeder echte Browser.
#[test]
fn test_blaetter_in_dokumentreihenfolge() {
    let liste = bedarf(
        "<html><head>\
         <style>p{color:#ff0000}</style>\
         <link rel=stylesheet href=\"a.css\">\
         <style>p{color:#00ff00}</style>\
         <link rel=stylesheet href=\"b.css\">\
         </head><body><p>x</p></body></html>",
    );
    assert_eq!(liste.len(), 4);
    assert!(matches!(&liste[0], Blattbedarf::Inline(css) if css.contains("#ff0000")));
    assert_eq!(liste[1], Blattbedarf::Extern(String::from("a.css")));
    assert!(matches!(&liste[2], Blattbedarf::Inline(css) if css.contains("#00ff00")));
    assert_eq!(liste[3], Blattbedarf::Extern(String::from("b.css")));
}

/// Ein `<link>` im `<body>` zaehlt auch — echte Seiten tun das, und der
/// Arena-Index bildet die Dokumentreihenfolge ohnehin richtig ab.
#[test]
fn test_link_im_body_zaehlt_und_bleibt_hinten() {
    let liste = bedarf(
        "<html><head><link rel=stylesheet href=\"kopf.css\"></head>\
         <body><p>x</p><link rel=stylesheet href=\"rumpf.css\"></body></html>",
    );
    assert_eq!(
        liste,
        alloc::vec![
            Blattbedarf::Extern(String::from("kopf.css")),
            Blattbedarf::Extern(String::from("rumpf.css")),
        ]
    );
}

/// `rel` ist eine LISTE — und `alternate stylesheet` ist ein Angebot,
/// keine Anweisung.
#[test]
fn test_rel_wird_richtig_gefiltert() {
    assert_eq!(externe("<link rel=stylesheet href=a.css>"), alloc::vec!["a.css"]);
    // Gross-/Kleinschreibung ist egal.
    assert_eq!(externe("<link rel=STYLESHEET href=a.css>"), alloc::vec!["a.css"]);
    // Mehrere Woerter, `stylesheet` dabei.
    assert_eq!(
        externe("<link rel=\"stylesheet preload\" href=a.css>"),
        alloc::vec!["a.css"]
    );
    // Alternative Stylesheets: NICHT anwenden.
    let leer: Vec<String> = Vec::new();
    assert_eq!(externe("<link rel=\"alternate stylesheet\" href=a.css>"), leer);
    // Alles andere auch nicht.
    assert_eq!(externe("<link rel=icon href=f.ico>"), leer);
    assert_eq!(externe("<link rel=preload href=a.css>"), leer);
    assert_eq!(externe("<link rel=canonical href=/x>"), leer);
    // Ohne href und mit leerem href gibt es nichts zu holen.
    assert_eq!(externe("<link rel=stylesheet>"), leer);
    assert_eq!(externe("<link rel=stylesheet href=\"   \">"), leer);
}

/// `media` gilt fuer `<link>` und `<style>` gleich — und die Richtung ist
/// die vorsichtige: Was wir nicht auswerten koennen, wenden wir nicht an.
#[test]
fn test_media_wird_beachtet() {
    let leer: Vec<String> = Vec::new();
    assert_eq!(externe("<link rel=stylesheet media=screen href=a.css>"), alloc::vec!["a.css"]);
    assert_eq!(externe("<link rel=stylesheet media=all href=a.css>"), alloc::vec!["a.css"]);
    assert_eq!(
        externe("<link rel=stylesheet media=\"only screen\" href=a.css>"),
        alloc::vec!["a.css"]
    );
    assert_eq!(
        externe("<link rel=stylesheet media=\"print, screen\" href=a.css>"),
        alloc::vec!["a.css"]
    );
    // Eine Druckformatierung auf dem Schirm ist genau der Schaden, den das
    // Ueberspringen von `@media` verhindern soll.
    assert_eq!(externe("<link rel=stylesheet media=print href=a.css>"), leer);
    // Feature-Abfragen koennen wir nicht auswerten — also nicht anwenden.
    assert_eq!(
        externe("<link rel=stylesheet media=\"screen and (max-width: 600px)\" href=a.css>"),
        leer
    );
    // Dasselbe fuer `<style media=print>`.
    assert!(bedarf("<style media=print>p{color:#ff0000}</style>").is_empty());
    assert_eq!(bedarf("<style media=screen>p{color:#ff0000}</style>").len(), 1);
}

/// Ein leerer `<style>`-Block ist kein Blatt.
#[test]
fn test_leerer_style_block_faellt_weg() {
    assert!(bedarf("<style></style><style>   </style>").is_empty());
}

// ---------------------------------------------------------------------------
// `@import` — gemeldet, nicht geholt
// ---------------------------------------------------------------------------

/// Beide Schreibweisen, und die Adresse kommt sauber heraus.
#[test]
fn test_import_beide_schreibweisen() {
    let blatt = parser::parsen("@import url(\"reset.css\"); @import 'basis.css'; p{color:#ff0000}");
    assert_eq!(blatt.importe, alloc::vec!["reset.css", "basis.css"]);
    // Die Regel dahinter ist trotzdem da — Ueberspringen heisst nicht
    // Verschlucken.
    assert_eq!(blatt.regeln.len(), 1);
    // Ohne Anfuehrungszeichen in `url()` geht es auch.
    let blatt = parser::parsen("@import url(a.css);");
    assert_eq!(blatt.importe, alloc::vec!["a.css"]);
}

/// **NUR VOR DER ERSTEN REGEL.** Ein `@import` weiter unten ist laut
/// Spezifikation ungueltig und wirkt in keinem Browser — ihn zu holen
/// hiesse, eine Datei zu laden, die auf der Seite nichts tut.
#[test]
fn test_import_nach_der_ersten_regel_zaehlt_nicht() {
    let blatt = parser::parsen("p{color:#ff0000} @import \"spaet.css\";");
    assert!(blatt.importe.is_empty());
    // Uebersprungen wurde er trotzdem sauber.
    assert_eq!(blatt.regeln.len(), 1);
}

/// Die Medienliste hinter einem `@import` gilt genau wie bei `<link>`.
#[test]
fn test_import_mit_medienliste() {
    assert!(parser::parsen("@import \"d.css\" print;").importe.is_empty());
    assert_eq!(
        parser::parsen("@import \"s.css\" screen;").importe,
        alloc::vec!["s.css"]
    );
    assert_eq!(
        parser::parsen("@import url(a.css) all;").importe,
        alloc::vec!["a.css"]
    );
}

/// Kaputtes `@import` liefert nichts und haengt nicht.
#[test]
fn test_import_muell() {
    for muell in [
        "@import;",
        "@import ;",
        "@import url(;",
        "@import url();",
        "@import \"\";",
        "@import nichts;",
        "@import url(\"unbeendet",
        "@import",
    ] {
        let blatt = parser::parsen(muell);
        assert!(
            blatt.importe.is_empty(),
            "aus {muell:?} kam ein Import heraus: {:?}",
            blatt.importe
        );
    }
}

/// Die Speichergrenze fuer gemeldete Importe greift.
#[test]
fn test_import_grenze() {
    let mut css = String::new();
    for i in 0..200 {
        css.push_str(&alloc::format!("@import \"a{i}.css\";\n"));
    }
    let blatt = parser::parsen(&css);
    assert_eq!(blatt.importe.len(), parser::Grenzen::standard().max_importe);
}

// ---------------------------------------------------------------------------
// Die Kaskade ueber MEHRERE Autor-Blaetter
// ---------------------------------------------------------------------------

/// **Standard < externes Blatt < spaeteres Blatt < `style`-Attribut.**
///
/// Die Reihenfolge ist die halbe Aufgabe: Falsch einsortiert sieht es
/// anders falsch aus als vorher, nicht besser.
#[test]
fn test_kaskade_ueber_mehrere_blaetter() {
    let dokument = speedhtml::parsen("<p style=\"color:#0000ff\">x</p><p>y</p>");
    let standard = standard_stylesheet();
    // Zwei Autor-Blaetter mit DERSELBEN Spezifitaet — es entscheidet
    // allein die Position in der Liste.
    let erstes = parser::parsen("p { color: #ff0000; margin-top: 5px }");
    let zweites = parser::parsen("p { color: #00ff00 }");
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&erstes, Herkunft::Autor),
        (&zweites, Herkunft::Autor),
    ];
    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());

    let mut absaetze = dokument
        .alle()
        .filter(|(_, k)| k.name() == Some("p"))
        .map(|(id, _)| id);
    let erster = absaetze.next().expect("erster <p>");
    let zweiter = absaetze.next().expect("zweiter <p>");

    // Das spaetere Blatt gewinnt bei gleicher Spezifitaet.
    assert_eq!(baum.stil(zweiter).farbe, Farbe::rgb(0, 255, 0));
    // Was nur im ersten Blatt steht, bleibt stehen.
    assert_eq!(baum.stil(zweiter).margin.oben, Laenge::Px(5_000));
    // Das `style`-Attribut schlaegt BEIDE.
    assert_eq!(baum.stil(erster).farbe, Farbe::rgb(0, 0, 255));
    // Und der Standard wirkt weiter, wo niemand widerspricht.
    assert_eq!(baum.stil(zweiter).display, Display::Block);
}

/// **Das HTML-Attribut `hidden` versteckt** — und eine Autor-Regel darf
/// es trotzdem schlagen.
///
/// Gefunden hat das der zweite Realitaets-Bericht: Nach dem Holen der
/// externen Blaetter blieben githubs Screenreader-Meldungen stehen, weil
/// sie nicht ueber eine Klasse versteckt sind, sondern ueber genau
/// dieses Attribut.
#[test]
fn test_hidden_attribut() {
    let dokument = speedhtml::parsen(
        "<div hidden>weg</div><div hidden=\"until-found\">auch weg</div>\
         <div class=\"trotzdem\" hidden>doch da</div><div>normal</div>",
    );
    let standard = standard_stylesheet();
    // Eine AUTOR-Regel muss das Standard-Verhalten schlagen koennen —
    // so steht es in der Spezifikation.
    let autor = parser::parsen(".trotzdem { display: block }");
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];
    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());

    let divs: Vec<_> = dokument
        .alle()
        .filter(|(_, k)| k.name() == Some("div"))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(baum.stil(divs[0]).display, Display::Keine, "<div hidden>");
    assert_eq!(
        baum.stil(divs[1]).display,
        Display::Keine,
        "hidden=\"until-found\" ist ohne Suchfunktion dasselbe"
    );
    assert_eq!(
        baum.stil(divs[2]).display,
        Display::Block,
        "eine Autor-Regel schlaegt das Standard-Verhalten"
    );
    assert_eq!(baum.stil(divs[3]).display, Display::Block, "ohne Attribut");
}

/// **Der Inhalt von `<template>` wird nie gezeichnet.**
///
/// Er ist ein Bauplan fuer JavaScript, kein Seiteninhalt — wer ihn
/// zeichnet, zeigt Text, den auch ein echter Browser nie zeigt.
#[test]
fn test_template_inhalt_ist_unsichtbar() {
    let (dokument, baum) = rechnen(
        "<body><template><p>NIEMALS SICHTBAR</p></template><p>sichtbar</p></body>",
        "",
    );
    let template = dokument.erstes("template").expect("<template>");
    assert_eq!(baum.stil(template).display, Display::Keine);
}

/// **`display: none` aus einem externen Blatt muss wirken** — das ist der
/// sichtbarste Teil des Fixes (Screenreader-Text, Overlays).
///
/// Hier wird nur die Kaskade geprueft; dass der Kasten dann wirklich aus
/// dem Baum faellt, prueft `speedlayout`.
#[test]
fn test_display_none_aus_externem_blatt() {
    let dokument = speedhtml::parsen(
        "<div class=\"sr-only\">Nur fuer Screenreader</div><div>sichtbar</div>",
    );
    let standard = standard_stylesheet();
    // Das „externe" Blatt — fuer die Kaskade ist es ein Blatt wie jedes.
    let extern_blatt = parser::parsen(".sr-only { display: none }");
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&extern_blatt, Herkunft::Autor),
    ];
    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());

    let mut divs = dokument
        .alle()
        .filter(|(_, k)| k.name() == Some("div"))
        .map(|(id, _)| id);
    let versteckt = divs.next().unwrap();
    let sichtbar = divs.next().unwrap();
    assert_eq!(baum.stil(versteckt).display, Display::Keine);
    assert_eq!(baum.stil(sichtbar).display, Display::Block);
}
