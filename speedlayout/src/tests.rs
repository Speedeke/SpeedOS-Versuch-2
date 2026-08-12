// speedlayout::tests — Layout ohne Grafik
//
// ===========================================================================
// WARUM DAS HIER VIELE TESTS SIND
//
// Weil sie GEHEN. Das Layout liefert Zahlen und eine Befehlsliste, keine
// Pixel — jede Behauptung ist ein `assert_eq!`, und der ganze Durchlauf
// kostet Millisekunden auf dem Host.
//
// Die Metrik ist eine Attrappe mit **10 px je Zeichen bei 16 px Schrift**
// (`attrappe::FesteMetrik`). Damit ist jede Zahl in dieser Datei von Hand
// nachgerechnet und nicht aus dem Ergebnis abgeschrieben.

use crate::anzeige::Befehl;
use crate::attrappe::{FesteMetrik, VierGroessen};
use crate::kasten::{Kasten, KastenArt};
use crate::{seite_setzen, Anzeigeliste, Ergebnis, Metrik, Rechteck};
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

/// Ein Layout mit der Standard-Attrappe.
fn setzen(html: &str, css: &str, breite: i32) -> (Ergebnis, Anzeigeliste) {
    let metrik = FesteMetrik::neu();
    seite_setzen(html, css, breite, &metrik)
}

/// Alle Textbefehle als (x, y, Text).
fn texte(liste: &Anzeigeliste) -> Vec<(i32, i32, String)> {
    liste
        .befehle
        .iter()
        .filter_map(|b| match b {
            Befehl::Text { x, y, text, .. } => Some((*x, *y, text.clone())),
            _ => None,
        })
        .collect()
}

/// Die Zeilen als Liste von Strings — Woerter mit gleichem `y` gehoeren
/// zusammen.
fn zeilen(liste: &Anzeigeliste) -> Vec<String> {
    let mut aus: Vec<(i32, String)> = Vec::new();
    for (_, y, text) in texte(liste) {
        match aus.iter_mut().find(|(zy, _)| *zy == y) {
            Some((_, s)) => {
                s.push(' ');
                s.push_str(&text);
            }
            None => aus.push((y, text)),
        }
    }
    aus.sort_by_key(|(y, _)| *y);
    aus.into_iter().map(|(_, s)| s).collect()
}

/// Den ersten Kasten mit diesem Tag finden.
fn kasten_von<'a>(ergebnis: &'a Ergebnis, html: &str, tag: &str) -> &'a Kasten {
    let dokument = speedhtml::parsen(html);
    ergebnis
        .wurzel
        .finde_tag(&dokument, tag)
        .unwrap_or_else(|| panic!("<{tag}> nicht im Kastenbaum"))
}

/// Alle Rechteck-Befehle mit dieser Farbe.
fn rechtecke_mit(liste: &Anzeigeliste, farbe: speedcss::Farbe) -> Vec<Rechteck> {
    liste
        .befehle
        .iter()
        .filter_map(|b| match b {
            Befehl::Rechteck { rechteck, farbe: f } if *f == farbe => Some(*rechteck),
            _ => None,
        })
        .collect()
}

// ===========================================================================
// 1. DAS BOX-MODELL
// ===========================================================================

/// Die vier Rechtecke bauen sich von innen nach aussen auf.
#[test]
fn test_box_modell() {
    let (ergebnis, _) = setzen(
        "<body><div>x</div></body>",
        "body { margin: 0 } \
         div { margin: 10px; border: 2px solid #000000; padding: 5px; height: 50px }",
        1000,
    );
    let div = kasten_von(&ergebnis, "<body><div>x</div></body>", "div");
    let m = &div.masse;

    assert_eq!(m.margin, crate::Kanten::alle(10));
    assert_eq!(m.rahmen, crate::Kanten::alle(2));
    assert_eq!(m.padding, crate::Kanten::alle(5));

    // Inhaltsbreite = 1000 - 2*(10+2+5) = 966
    assert_eq!(m.inhalt.breite, 966);
    assert_eq!(m.inhalt.hoehe, 50);
    // Und die Boxen wachsen nach aussen.
    assert_eq!(m.padding_box().breite, 966 + 10);
    assert_eq!(m.rahmen_box().breite, 966 + 10 + 4);
    assert_eq!(m.margin_box().breite, 1000);
    assert_eq!(m.margin_box().hoehe, 50 + 10 + 4 + 20);
}

/// `display: none` faellt weg — mitsamt Teilbaum.
#[test]
fn test_display_none_faellt_weg() {
    let (_, liste) = setzen(
        "<p>sichtbar</p><div style='display:none'><p>versteckt</p></div>",
        "",
        1000,
    );
    assert!(liste.text().contains("sichtbar"));
    assert!(
        !liste.text().contains("versteckt"),
        "der Teilbaum unter display:none muss verschwinden: {}",
        liste.text()
    );
}

/// `<head>` und sein Inhalt erscheinen nie — das kommt aus dem
/// Standard-Stylesheet, nicht aus einer Sonderregel im Layout.
#[test]
fn test_kopfbereich_erscheint_nicht() {
    let (_, liste) = setzen(
        "<html><head><title>Titel</title><script>var a=1</script></head>\
         <body><p>Inhalt</p></body></html>",
        "",
        1000,
    );
    let text = liste.text();
    assert!(text.contains("Inhalt"));
    assert!(!text.contains("Titel"), "der <title> darf nicht erscheinen");
    assert!(!text.contains("var"), "das Skript darf nicht erscheinen");
}

/// Ein gemischter Container bekommt anonyme Bloecke.
#[test]
fn test_anonyme_bloecke() {
    let (ergebnis, liste) = setzen(
        "<body><div>vorher<p>Absatz</p>nachher</div></body>",
        "body { margin: 0 } p { margin: 0 }",
        1000,
    );
    let html = "<body><div>vorher<p>Absatz</p>nachher</div></body>";
    let div = kasten_von(&ergebnis, html, "div");
    // Drei Kinder: anonym, <p>, anonym.
    assert_eq!(div.kinder.len(), 3, "{:?}", div.kinder.iter().map(|k| &k.art).collect::<Vec<_>>());
    assert_eq!(div.kinder[0].art, KastenArt::AnonymerBlock);
    assert_eq!(div.kinder[2].art, KastenArt::AnonymerBlock);

    // Und die Reihenfolge stimmt senkrecht.
    let z = zeilen(&liste);
    assert_eq!(z, ["vorher", "Absatz", "nachher"]);
}

/// Ein reiner Inline-Container bekommt KEINE zusaetzliche Ebene.
#[test]
fn test_kein_anonymer_block_ohne_not() {
    let html = "<body><p>nur <b>Text</b> hier</p></body>";
    let (ergebnis, _) = setzen(html, "body { margin: 0 }", 1000);
    let p = kasten_von(&ergebnis, html, "p");
    assert!(
        !p.kinder.iter().any(|k| k.art == KastenArt::AnonymerBlock),
        "ein reiner Inline-Absatz braucht keinen anonymen Block"
    );
}

// ===========================================================================
// 2. BLOCK-LAYOUT
// ===========================================================================

/// Bloecke stapeln sich senkrecht, jeder so breit wie der Elternteil.
#[test]
fn test_bloecke_stapeln_sich() {
    let html = "<body><div id=a>A</div><div id=b>B</div><div id=c>C</div></body>";
    let (_, liste) = setzen(
        html,
        "body { margin: 0 } div { margin: 0; height: 30px }",
        800,
    );
    let t = texte(&liste);
    assert_eq!(t.len(), 3);
    // Alle links buendig, jeweils 30 px tiefer.
    assert_eq!(t[0].0, 0);
    assert_eq!(t[1].0, 0);
    assert!(t[1].1 >= t[0].1 + 30, "B muss unter A liegen: {t:?}");
    assert!(t[2].1 >= t[1].1 + 30, "C muss unter B liegen: {t:?}");
}

/// Verschachtelung: Die Breite verringert sich mit jeder Ebene.
#[test]
fn test_verschachtelung_verengt() {
    let html = "<body><div id=a><div id=b><div id=c>x</div></div></div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0 } div { padding: 10px; margin: 0 }",
        1000,
    );
    let alle = ergebnis.wurzel.alle();
    let breiten: Vec<i32> = alle
        .iter()
        .filter(|k| k.art == KastenArt::Block)
        .map(|k| k.masse.inhalt.breite)
        .collect();
    // body(1000) -> a(980) -> b(960) -> c(940)
    assert!(breiten.contains(&980), "{breiten:?}");
    assert!(breiten.contains(&960), "{breiten:?}");
    assert!(breiten.contains(&940), "{breiten:?}");
}

/// DER MARGIN-KOLLAPS: zwei Absaetze mit je 20 px haben 20 px Abstand,
/// nicht 40.
#[test]
fn test_margin_kollaps_zwischen_geschwistern() {
    let html = "<body><div id=a>A</div><div id=b>B</div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0 } div { margin: 20px 0; height: 10px }",
        800,
    );
    let dokument = speedhtml::parsen(html);
    let bloecke: Vec<&Kasten> = ergebnis
        .wurzel
        .alle()
        .into_iter()
        .filter(|k| {
            k.knoten
                .and_then(|id| dokument.knoten(id))
                .and_then(|n| n.name())
                == Some("div")
        })
        .collect();
    assert_eq!(bloecke.len(), 2);

    let a_unten = bloecke[0].masse.rahmen_box().unten();
    let b_oben = bloecke[1].masse.rahmen_box().y;
    assert_eq!(
        b_oben - a_unten,
        20,
        "20px + 20px muessen zu 20px kollabieren, nicht zu 40"
    );
}

/// Unterschiedliche Raender: der GROESSERE gewinnt.
#[test]
fn test_margin_kollaps_nimmt_das_maximum() {
    let html = "<body><div id=a>A</div><div id=b>B</div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0 } #a { margin-bottom: 30px; height: 10px } \
         #b { margin-top: 10px; height: 10px }",
        800,
    );
    let dokument = speedhtml::parsen(html);
    let bloecke: Vec<&Kasten> = ergebnis
        .wurzel
        .alle()
        .into_iter()
        .filter(|k| {
            k.knoten
                .and_then(|id| dokument.knoten(id))
                .and_then(|n| n.name())
                == Some("div")
        })
        .collect();
    let abstand = bloecke[1].masse.rahmen_box().y - bloecke[0].masse.rahmen_box().unten();
    assert_eq!(abstand, 30, "max(30, 10) = 30");
}

/// Der obere Rand des ERSTEN Kindes zaehlt voll (er hat keinen
/// Vorgaenger, mit dem er kollabieren koennte).
#[test]
fn test_erster_rand_kollabiert_nicht() {
    let html = "<body><div>A</div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0; padding: 0 } div { margin-top: 25px; height: 10px }",
        800,
    );
    let div = kasten_von(&ergebnis, html, "div");
    assert_eq!(div.masse.rahmen_box().y, 25);
}

/// Prozentbreiten beziehen sich auf die Inhaltsbreite des Elternteils.
#[test]
fn test_prozentbreite() {
    let html = "<body><div id=a><div id=b>x</div></div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0 } #a { width: 400px; margin: 0 } #b { width: 50%; margin: 0 }",
        1000,
    );
    let dokument = speedhtml::parsen(html);
    let b = ergebnis
        .wurzel
        .alle()
        .into_iter()
        .find(|k| {
            k.knoten
                .and_then(|id| dokument.knoten(id))
                .and_then(|n| n.attribut("id"))
                == Some("b")
        })
        .expect("#b fehlt");
    assert_eq!(b.masse.inhalt.breite, 200, "50% von 400px");
}

/// `max-width` deckelt.
#[test]
fn test_max_width() {
    let html = "<body><div>x</div></body>";
    let (ergebnis, _) = setzen(html, "body { margin: 0 } div { max-width: 300px }", 1000);
    let div = kasten_von(&ergebnis, html, "div");
    assert_eq!(div.masse.inhalt.breite, 300);
}

/// `margin: 0 auto` zentriert.
#[test]
fn test_auto_margin_zentriert() {
    let html = "<body><div>x</div></body>";
    let (ergebnis, _) = setzen(
        html,
        "body { margin: 0 } div { width: 400px; margin-left: auto; margin-right: auto }",
        1000,
    );
    let div = kasten_von(&ergebnis, html, "div");
    assert_eq!(div.masse.margin.links, 300);
    assert_eq!(div.masse.inhalt.x, 300);
}

/// Ein leeres Element hat Hoehe 0 (aber seine Raender zaehlen).
#[test]
fn test_leeres_element() {
    let html = "<body><div></div></body>";
    let (ergebnis, _) = setzen(html, "body { margin: 0 } div { margin: 0 }", 800);
    let div = kasten_von(&ergebnis, html, "div");
    assert_eq!(div.masse.inhalt.hoehe, 0);
    assert_eq!(div.masse.inhalt.breite, 800);
}

/// Ein Element, das nur Leerraum enthaelt, erzeugt keine Zeile.
#[test]
fn test_nur_leerraum_erzeugt_keine_zeile() {
    let (ergebnis, liste) = setzen("<body>\n   \n  <div>\n  </div>\n</body>", "", 800);
    assert!(liste.texte().is_empty(), "{:?}", liste.texte());
    assert!(ergebnis.hoehe < 40, "Hoehe {}", ergebnis.hoehe);
}

// ===========================================================================
// 3. INLINE-LAYOUT — das Herzstueck
// ===========================================================================

/// DER GRUNDFALL, von Hand nachgerechnet.
///
/// 10 px je Zeichen. `aaa bbb ccc` sind 3+1+3+1+3 = 11 Zeichen = 110 px.
/// Bei 80 px Breite passt `aaa bbb` (70 px) und `ccc` muss umbrechen.
#[test]
fn test_zeilenumbruch_grundfall() {
    let (_, liste) = setzen(
        "<body><p>aaa bbb ccc</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        80,
    );
    assert_eq!(zeilen(&liste), ["aaa bbb", "ccc"]);
}

/// Genau passend: `aaa bbb` sind 70 px — bei 70 px Breite bleibt es EINE
/// Zeile. Das ist der Test fuer „Leerzeichen am Zeilenende zaehlt nicht".
#[test]
fn test_zeilenumbruch_genau_passend() {
    let (_, liste) = setzen(
        "<body><p>aaa bbb</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        70,
    );
    assert_eq!(zeilen(&liste), ["aaa bbb"]);

    // Ein Pixel weniger, und es bricht um.
    let (_, liste) = setzen(
        "<body><p>aaa bbb</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        69,
    );
    assert_eq!(zeilen(&liste), ["aaa", "bbb"]);
}

/// Mehrere Leerzeichen werden EINS, Zeilenumbrueche im Quelltext auch.
#[test]
fn test_leerraum_wird_gefaltet() {
    let (_, liste) = setzen(
        "<body><p>aa     bb\n\n\tcc</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        1000,
    );
    assert_eq!(zeilen(&liste), ["aa bb cc"]);
}

/// In `<pre>` bleibt der Leerraum — und die Zeilen brechen dort um, wo
/// sie im Quelltext umbrechen.
#[test]
fn test_pre_behaelt_leerraum() {
    let (_, liste) = setzen(
        "<body><pre>eins\nzwei</pre></body>",
        "body { margin: 0 } pre { margin: 0 }",
        1000,
    );
    let z = zeilen(&liste);
    assert_eq!(z.len(), 2, "{z:?}");
    assert!(z[0].contains("eins"));
    assert!(z[1].contains("zwei"));
}

/// `<br>` bricht immer um.
#[test]
fn test_br_bricht_um() {
    let (_, liste) = setzen(
        "<body><p>eins<br>zwei</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        1000,
    );
    assert_eq!(zeilen(&liste), ["eins", "zwei"]);
}

/// Ein zu langes Wort wird HART getrennt statt aus dem Fenster zu laufen.
/// Eine lange URL ist der Normalfall dafuer.
#[test]
fn test_zu_langes_wort_wird_hart_getrennt() {
    let (_, liste) = setzen(
        "<body><p>aaaaaaaaaa</p></body>", // 10 Zeichen = 100 px
        "body { margin: 0 } p { margin: 0 }",
        40, // 4 Zeichen je Zeile
        );
    let z = zeilen(&liste);
    assert!(z.len() >= 2, "haette getrennt werden muessen: {z:?}");
    let zusammen: String = z.join("");
    assert_eq!(zusammen, "aaaaaaaaaa", "kein Zeichen darf verlorengehen");
    for zeile in &z {
        assert!(zeile.chars().count() <= 4, "Zeile zu lang: {zeile}");
    }
}

/// Der Umbruch benutzt die STIL-Metrik: fetter Text ist bei diesem Wirt
/// breiter und bricht frueher um.
#[test]
fn test_umbruch_beachtet_den_stil() {
    let metrik = FesteMetrik::mit_breitem_fett();
    // `aaa bbb` normal = 70 px, fett = 105 px.
    let (_, normal) = seite_setzen(
        "<body><p>aaa bbb</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        80,
        &metrik,
    );
    let (_, fett) = seite_setzen(
        "<body><p><b>aaa bbb</b></p></body>",
        "body { margin: 0 } p { margin: 0 }",
        80,
        &metrik,
    );
    assert_eq!(zeilen(&normal).len(), 1);
    assert_eq!(zeilen(&fett).len(), 2, "fett muss frueher umbrechen");
}

/// Zeilen liegen um die Zeilenhoehe auseinander.
#[test]
fn test_zeilenhoehe() {
    let (_, liste) = setzen(
        "<body><p>aaa bbb ccc</p></body>",
        "body { margin: 0 } p { margin: 0; line-height: 40px }",
        80,
    );
    // Die y-Werte der ZEILEN, nicht der Woerter — mehrere Woerter einer
    // Zeile haben denselben y-Wert, und ihre Differenz waere 0.
    let mut ys: Vec<i32> = texte(&liste).into_iter().map(|(_, y, _)| y).collect();
    ys.sort_unstable();
    ys.dedup();
    assert!(ys.len() >= 2, "es haette umbrechen muessen: {ys:?}");
    assert_eq!(
        ys[1] - ys[0],
        40,
        "line-height: 40px muss 40px Zeilenabstand ergeben"
    );
}

/// Verschieden grosse Schriften in EINER Zeile stehen auf derselben
/// Grundlinie.
#[test]
fn test_grundlinie_bei_gemischten_groessen() {
    let metrik = FesteMetrik::neu();
    let (_, liste) = seite_setzen(
        "<body><p>klein <big>GROSS</big></p></body>",
        "body { margin: 0 } p { margin: 0; font-size: 16px } big { font-size: 32px }",
        1000,
        &metrik,
    );
    let t = texte(&liste);
    assert_eq!(t.len(), 2, "{t:?}");
    // Grundlinie = y + grundlinie(groesse). Bei 16 px ist sie 12, bei
    // 32 px 24 — die Oberkanten muessen sich also um 12 unterscheiden.
    let klein_basis = t[0].1 + metrik.grundlinie(16);
    let gross_basis = t[1].1 + metrik.grundlinie(32);
    assert_eq!(
        klein_basis, gross_basis,
        "beide muessen auf derselben Grundlinie sitzen: {t:?}"
    );
}

/// `text-align: center` und `right`.
#[test]
fn test_ausrichtung() {
    // `abc` = 30 px in 100 px Breite.
    let (_, mitte) = setzen(
        "<body><p>abc</p></body>",
        "body { margin: 0 } p { margin: 0; text-align: center }",
        100,
    );
    assert_eq!(texte(&mitte)[0].0, 35, "(100-30)/2");

    let (_, rechts) = setzen(
        "<body><p>abc</p></body>",
        "body { margin: 0 } p { margin: 0; text-align: right }",
        100,
    );
    assert_eq!(texte(&rechts)[0].0, 70);
}

/// Ein Bild ist eine Inline-Box mit seiner Groesse.
#[test]
fn test_bild_als_inline_box() {
    let (_, liste) = setzen(
        "<body><p>vor <img src=a.png width=50 height=40> nach</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        1000,
    );
    let bilder: Vec<&Befehl> = liste
        .befehle
        .iter()
        .filter(|b| matches!(b, Befehl::Bild { .. }))
        .collect();
    assert_eq!(bilder.len(), 1);
    match bilder[0] {
        Befehl::Bild { rechteck, quelle, .. } => {
            assert_eq!(quelle, "a.png");
            assert_eq!(rechteck.breite, 50);
            assert_eq!(rechteck.hoehe, 40);
            // `vor ` sind 4 Zeichen = 40 px.
            assert_eq!(rechteck.x, 40);
        }
        _ => unreachable!(),
    }
}

/// Ein Bild ohne Massangabe bekommt einen Platzhalter — nicht 0x0.
#[test]
fn test_bild_ohne_masse() {
    let (_, liste) = setzen("<body><p><img src=x.png></p></body>", "body{margin:0}", 1000);
    match liste.befehle.iter().find(|b| matches!(b, Befehl::Bild { .. })) {
        Some(Befehl::Bild { rechteck, .. }) => {
            assert!(rechteck.breite > 0 && rechteck.hoehe > 0, "{rechteck:?}");
        }
        _ => panic!("kein Bildbefehl"),
    }
}

/// Ein Bild schiebt die Zeile hoeher.
#[test]
fn test_bild_bestimmt_die_zeilenhoehe() {
    let (klein, _) = setzen("<body><p>x</p></body>", "body{margin:0} p{margin:0}", 1000);
    let (gross, _) = setzen(
        "<body><p>x<img src=a width=10 height=100></p></body>",
        "body{margin:0} p{margin:0}",
        1000,
    );
    assert!(
        gross.hoehe > klein.hoehe + 50,
        "ein 100px-Bild muss die Zeile hoeher machen: {} vs {}",
        gross.hoehe,
        klein.hoehe
    );
}

/// Umlaute: Der Umbruch zaehlt ZEICHEN, nicht Bytes.
///
/// „Grüße" hat 5 Zeichen (7 Bytes) = 50 px. Bei 50 px Breite passt es
/// genau; wer Bytes zaehlt, kaeme auf 70 px und wuerde umbrechen.
#[test]
fn test_umlaute_zaehlen_als_ein_zeichen() {
    let (_, liste) = setzen(
        "<body><p>Grüße</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        50,
    );
    assert_eq!(zeilen(&liste), ["Grüße"]);
}

/// Und der harte Trenner schneidet nie in ein Zeichen.
#[test]
fn test_harte_trennung_bei_umlauten() {
    for breite in [10, 20, 30, 40] {
        let (_, liste) = setzen(
            "<body><p>Grüßeöäüß</p></body>",
            "body { margin: 0 } p { margin: 0 }",
            breite,
        );
        let zusammen: String = zeilen(&liste).join("");
        assert_eq!(zusammen, "Grüßeöäüß", "bei Breite {breite}");
    }
}

// ===========================================================================
// 4. LISTEN, TABELLEN, UEBERLAUF
// ===========================================================================

/// Ein `<ul>` bekommt Aufzaehlungszeichen und Einrueckung.
#[test]
fn test_liste_mit_punkten() {
    let (_, liste) = setzen(
        "<body><ul><li>eins</li><li>zwei</li></ul></body>",
        "body { margin: 0 }",
        1000,
    );
    let text = liste.text();
    assert!(text.contains('\u{2022}'), "kein Aufzaehlungszeichen: {text}");
    assert!(text.contains("eins") && text.contains("zwei"));

    // Eingerueckt: Das Standard-Stylesheet gibt <ul> 40px padding-left.
    let t = texte(&liste);
    assert!(t.iter().all(|(x, _, _)| *x >= 40), "nicht eingerueckt: {t:?}");
}

/// Ein `<ol>` zaehlt.
#[test]
fn test_nummerierte_liste() {
    let (_, liste) = setzen(
        "<body><ol><li>a</li><li>b</li><li>c</li></ol></body>",
        "body { margin: 0 }",
        1000,
    );
    let text = liste.text();
    assert!(text.contains("1."), "{text}");
    assert!(text.contains("2."), "{text}");
    assert!(text.contains("3."), "{text}");
}

/// Roemische Zahlen und Buchstaben.
#[test]
fn test_andere_listenzeichen() {
    let (_, liste) = setzen(
        "<body><ol><li>a</li><li>b</li><li>c</li><li>d</li></ol></body>",
        "body { margin: 0 } ol { list-style-type: lower-roman }",
        1000,
    );
    let text = liste.text();
    assert!(text.contains("iv."), "roemisch 4 fehlt: {text}");

    let (_, liste) = setzen(
        "<body><ol><li>a</li><li>b</li></ol></body>",
        "body { margin: 0 } ol { list-style-type: upper-alpha }",
        1000,
    );
    assert!(liste.text().contains("B."), "{}", liste.text());
}

/// `list-style-type: none` — kein Zeichen.
#[test]
fn test_liste_ohne_zeichen() {
    let (_, liste) = setzen(
        "<body><ul><li>eins</li></ul></body>",
        "body { margin: 0 } ul { list-style-type: none }",
        1000,
    );
    assert!(!liste.text().contains('\u{2022}'), "{}", liste.text());
}

/// EINE TABELLE: Zellen stehen nebeneinander, Zeilen untereinander.
#[test]
fn test_tabelle_grundfall() {
    let html = "<body><table><tr><td>a</td><td>b</td></tr>\
                <tr><td>c</td><td>d</td></tr></table></body>";
    let (_, liste) = setzen(html, "body { margin: 0 } td { padding: 0 }", 1000);
    let t = texte(&liste);
    assert_eq!(t.len(), 4, "{t:?}");

    let finde = |s: &str| t.iter().find(|(_, _, txt)| txt == s).expect(s);
    let (ax, ay, _) = finde("a");
    let (bx, by, _) = finde("b");
    let (cx, cy, _) = finde("c");

    assert_eq!(ay, by, "a und b muessen in derselben Zeile stehen");
    assert!(bx > ax, "b muss rechts von a stehen");
    assert!(cy > ay, "c muss unter a stehen");
    assert_eq!(*cx, *ax, "c muss unter a buendig stehen");
}

/// Eine Tabelle OHNE `<tbody>` (der Normalfall in handgeschriebenem
/// HTML) funktioniert genauso — der Parser erfindet keines, das Layout
/// kommt mit beiden Formen zurecht.
#[test]
fn test_tabelle_mit_und_ohne_tbody() {
    let ohne = "<body><table><tr><td>a</td><td>b</td></tr></table></body>";
    let mit = "<body><table><tbody><tr><td>a</td><td>b</td></tr></tbody></table></body>";
    let (_, l1) = setzen(ohne, "body{margin:0} td{padding:0}", 1000);
    let (_, l2) = setzen(mit, "body{margin:0} td{padding:0}", 1000);
    assert_eq!(texte(&l1).len(), 2);
    assert_eq!(texte(&l2).len(), 2);
    // Dieselben Positionen.
    assert_eq!(texte(&l1)[0].0, texte(&l2)[0].0);
    assert_eq!(texte(&l1)[1].0, texte(&l2)[1].0);
}

/// INHALTSBASIERTE SPALTEN: Die Spalte mit dem laengeren Text bekommt
/// mehr Platz. Genau das kann die „gleich breite Spalten"-Variante nicht,
/// und genau deshalb ist sie verworfen (siehe layout::tabelle_setzen).
#[test]
fn test_tabellenspalten_richten_sich_nach_dem_inhalt() {
    let html = "<body><table><tr><td>x</td><td>viel laengerer Text hier</td></tr></table></body>";
    let (ergebnis, _) = setzen(html, "body{margin:0} td{padding:0}", 1000);
    let dokument = speedhtml::parsen(html);
    let zellen: Vec<&Kasten> = ergebnis
        .wurzel
        .alle()
        .into_iter()
        .filter(|k| k.art == KastenArt::TabellenZelle)
        .collect();
    assert_eq!(zellen.len(), 2);
    assert!(
        zellen[1].masse.inhalt.breite > zellen[0].masse.inhalt.breite,
        "die Spalte mit mehr Text muss breiter sein: {} vs {}",
        zellen[1].masse.inhalt.breite,
        zellen[0].masse.inhalt.breite
    );
    let _ = dokument;
}

/// Alle Zellen einer Zeile sind gleich hoch.
#[test]
fn test_tabellenzeile_gleich_hoch() {
    let html = "<body><table><tr><td>kurz</td><td>eins zwei drei vier fuenf sechs</td></tr></table></body>";
    let (ergebnis, _) = setzen(html, "body{margin:0} td{padding:0}", 200);
    let zellen: Vec<&Kasten> = ergebnis
        .wurzel
        .alle()
        .into_iter()
        .filter(|k| k.art == KastenArt::TabellenZelle)
        .collect();
    assert_eq!(zellen.len(), 2);
    assert_eq!(
        zellen[0].masse.margin_box().hoehe,
        zellen[1].masse.margin_box().hoehe,
        "Zellen einer Zeile muessen gleich hoch sein"
    );
}

/// ZU BREITER INHALT LAEUFT UEBER — er wird nicht abgeschnitten, und es
/// stuerzt nichts ab.
#[test]
fn test_ueberlauf_statt_absturz() {
    // Ein Wort, das nicht einmal in EIN Zeichen passt.
    let (ergebnis, liste) = setzen(
        "<body><p>unteilbarwort</p></body>",
        "body { margin: 0 } p { margin: 0 }",
        3, // schmaler als ein Zeichen (10 px)
    );
    // Es darf nicht haengen und nicht abstuerzen — und der Text ist da.
    assert!(!liste.texte().is_empty());
    let zusammen: String = zeilen(&liste).join("");
    assert_eq!(zusammen, "unteilbarwort");
    assert!(ergebnis.befund.ueberlaeufe > 0, "der Ueberlauf muss gezaehlt werden");
}

/// Eine feste Hoehe, die zu klein ist: Der Inhalt laeuft ueber, wird
/// aber nicht weggeworfen.
#[test]
fn test_zu_kleine_hoehe_schneidet_nicht_ab() {
    let (ergebnis, liste) = setzen(
        "<body><p>aaa bbb ccc ddd eee</p></body>",
        "body { margin: 0 } p { margin: 0; height: 5px }",
        50,
    );
    assert!(liste.text().contains("eee"), "Text darf nicht verschwinden");
    assert!(ergebnis.befund.ueberlaeufe > 0);
}

// ===========================================================================
// 5. DIE ANZEIGELISTE
// ===========================================================================

/// Hintergruende werden als Rechtecke ausgegeben — VOR dem Text.
#[test]
fn test_hintergrund_vor_text() {
    let (_, liste) = setzen(
        "<body><p>Text</p></body>",
        "body { margin: 0 } p { margin: 0; background-color: #ff0000 }",
        200,
    );
    let erstes_rechteck = liste
        .befehle
        .iter()
        .position(|b| matches!(b, Befehl::Rechteck { .. }));
    let erster_text = liste
        .befehle
        .iter()
        .position(|b| matches!(b, Befehl::Text { .. }));
    assert!(erstes_rechteck.is_some() && erster_text.is_some());
    assert!(
        erstes_rechteck < erster_text,
        "der Hintergrund muss VOR dem Text kommen, sonst uebermalt er ihn"
    );
}

/// Ein Rahmen wird als vier Rechtecke ausgegeben.
#[test]
fn test_rahmen_als_vier_rechtecke() {
    let (_, liste) = setzen(
        "<body><div>x</div></body>",
        "body { margin: 0 } div { margin: 0; border: 3px solid #00ff00; height: 20px }",
        100,
    );
    let gruen = rechtecke_mit(&liste, speedcss::Farbe::rgb(0, 255, 0));
    assert_eq!(gruen.len(), 4, "vier Kanten: {gruen:?}");
    // Die obere Kante ist so breit wie der Kasten und 3 px hoch.
    assert!(gruen.iter().any(|r| r.hoehe == 3 && r.breite == 100));
    assert!(gruen.iter().any(|r| r.breite == 3 && r.hoehe == 26));
}

/// Ein Rahmen ohne Stil belegt keinen Platz (so die Spezifikation).
#[test]
fn test_rahmen_ohne_stil_ist_unsichtbar() {
    let html = "<body><div>x</div></body>";
    let (ergebnis, liste) = setzen(
        html,
        "body { margin: 0 } div { margin: 0; border-width: 10px }",
        100,
    );
    let div = kasten_von(&ergebnis, html, "div");
    assert_eq!(div.masse.rahmen, crate::Kanten::alle(0));
    assert!(rechtecke_mit(&liste, speedcss::Farbe::SCHWARZ).is_empty());
}

/// `text-decoration: underline` wird zu einer Linie.
#[test]
fn test_unterstreichung_ist_eine_linie() {
    let (_, liste) = setzen(
        "<body><p><a href=x>Link</a></p></body>",
        "body { margin: 0 }",
        1000,
    );
    let linien = liste
        .befehle
        .iter()
        .filter(|b| matches!(b, Befehl::Linie { .. }))
        .count();
    assert!(linien > 0, "ein Link muss unterstrichen sein");
}

/// Die Koordinaten sind ABSOLUT — ein verschachtelter Text weiss, wo er
/// auf der Seite steht.
#[test]
fn test_koordinaten_sind_absolut() {
    let (_, liste) = setzen(
        "<body><div><div><p>tief</p></div></div></body>",
        "body { margin: 0 } div { padding: 25px; margin: 0 } p { margin: 0 }",
        1000,
    );
    let t = texte(&liste);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, 50, "zwei mal 25px Padding");
    // Der y-Wert liegt UNTER der Oberkante der Inhalts-Box: Die
    // Zeilenhoehe (1.4 vom Standard-Stylesheet) ist groesser als die
    // Schrift, und der Durchschuss wird halb oben, halb unten verteilt.
    // Das ist richtig so — Text mit grosser line-height sitzt mittig in
    // seiner Zeile statt oben zu kleben.
    assert!(t[0].1 >= 50 && t[0].1 <= 56, "y = {} erwartet 50..56", t[0].1);
}

/// `im_bereich` filtert fuer das Scrollen.
#[test]
fn test_bereichsfilter() {
    let (_, liste) = setzen(
        "<body><p>eins</p><p>zwei</p><p>drei</p></body>",
        "body { margin: 0 } p { margin: 0; height: 100px }",
        200,
    );
    let sichtbar = liste.im_bereich(Rechteck::neu(0, 0, 200, 50));
    let alle = liste.befehle.len();
    assert!(
        sichtbar.len() < alle,
        "der Filter muss etwas wegnehmen ({} von {})",
        sichtbar.len(),
        alle
    );
}

// ===========================================================================
// 6. ROBUSTHEIT
// ===========================================================================

/// SEHR TIEFE VERSCHACHTELUNG: begrenzt, nicht ueberlaufen.
#[test]
fn test_sehr_tiefe_verschachtelung() {
    let mut html = String::from("<body>");
    for _ in 0..500 {
        html.push_str("<div>");
    }
    html.push_str("tief");
    for _ in 0..500 {
        html.push_str("</div>");
    }
    html.push_str("</body>");

    let (ergebnis, _) = setzen(&html, "div { padding: 1px }", 1000);
    // Der Parser deckelt bei 100, das Layout bei 64 — irgendeiner greift.
    assert!(
        ergebnis.befund.abgeschnitten || ergebnis.befund.zu_tief > 0,
        "eine Grenze haette greifen muessen: {:?}",
        ergebnis.befund
    );
}

/// Ein Dokument, das nur aus Verschachtelung besteht, ohne Inhalt.
#[test]
fn test_tiefe_ohne_inhalt() {
    let mut html = String::new();
    for _ in 0..200 {
        html.push_str("<span>");
    }
    let (_, liste) = setzen(&html, "", 1000);
    let _ = liste.len();
}

/// Ein Dokument aus sehr vielen Geschwistern.
#[test]
fn test_viele_geschwister() {
    let mut html = String::from("<body>");
    for i in 0..2000 {
        html.push_str("<p>");
        html.push_str(if i % 2 == 0 { "a" } else { "b" });
        html.push_str("</p>");
    }
    html.push_str("</body>");
    let (ergebnis, liste) = setzen(&html, "body{margin:0} p{margin:0}", 1000);
    assert_eq!(liste.texte().len(), 2000);
    assert!(ergebnis.hoehe > 0);
}

/// Breite 0 und negative Breiten.
#[test]
fn test_entartete_breiten() {
    for breite in [-100, 0, 1] {
        let (_, liste) = setzen("<body><p>Text hier</p></body>", "", breite);
        // Kein Panic, kein Haenger — und der Text ist noch da.
        assert!(!liste.text().is_empty(), "bei Breite {breite}");
    }
}

/// Endlos-Konstruktionen: Was passiert bei absurden Werten?
#[test]
fn test_absurde_werte() {
    let faelle: &[&str] = &[
        "p { width: 999999999px }",
        "p { margin: 999999999px }",
        "p { padding: -50px }",
        "p { line-height: 0 }",
        "p { font-size: 0 }",
        "p { font-size: 99999px }",
        "p { width: -100% }",
        "p { height: 999999999px }",
        "p { border: 999999px solid red }",
    ];
    for css in faelle {
        let (ergebnis, liste) = setzen("<body><p>Text</p></body>", css, 500);
        // Die Zusage: kein Panic, kein Haenger.
        let _ = ergebnis.hoehe;
        let _ = liste.len();
    }
}

/// Leeres Dokument, leeres CSS.
#[test]
fn test_leere_eingaben() {
    let (ergebnis, liste) = setzen("", "", 800);
    assert!(liste.is_empty() || liste.texte().is_empty());
    assert_eq!(ergebnis.hoehe.max(0), ergebnis.hoehe);

    let (_, liste) = setzen("<body></body>", "", 800);
    assert!(liste.texte().is_empty());
}

/// Muell-HTML mit Muell-CSS.
#[test]
fn test_muell_panickt_nicht() {
    let vorrat: &[&str] = &[
        "<p><div><span></p></div>",
        "<table><td>x<tr><th>",
        "<ul><li><ul><li><ul><li>x",
        "<p>a<br><br><br>b",
        "<img><img><img>",
        "<pre>   \n\n\n   </pre>",
        "<b><i><u><s>x",
    ];
    let css_vorrat: &[&str] = &[
        "",
        "* { margin: 1px }",
        "p { display: table-cell }",
        "div { display: list-item }",
        "td { width: 1% }",
        "* { line-height: 1000px }",
    ];
    for html in vorrat {
        for css in css_vorrat {
            let (ergebnis, liste) = setzen(html, css, 300);
            let _ = ergebnis.hoehe;
            let _ = liste.text();
        }
    }
}

/// Die Vier-Groessen-Metrik: Das Layout rechnet mit der Groesse, die
/// WIRKLICH gezeichnet wird.
///
/// `font-size: 19px` gibt es nicht — der Wirt macht 20 daraus. Wuerde das
/// Layout mit 19 rechnen und der Renderer mit 20 zeichnen, liefen
/// Textbreite und Zeilenhoehe auseinander.
#[test]
fn test_groesse_waehlen_wird_beachtet() {
    let metrik = VierGroessen;
    let (_, liste) = seite_setzen(
        "<body><p>abcd</p></body>",
        "body { margin: 0 } p { margin: 0; font-size: 19px }",
        1000,
        &metrik,
    );
    let t = texte(&liste);
    assert_eq!(t.len(), 1);
    // 20px gewaehlt -> 10px je Zeichen -> die Texthoehe ist 20, nicht 19.
    match &liste.befehle.iter().find(|b| matches!(b, Befehl::Text { .. })) {
        Some(Befehl::Text { groesse, .. }) => assert_eq!(*groesse, 20),
        _ => panic!("kein Textbefehl"),
    }
}

/// Eine ganze kleine Seite — der Zusammenbau aller Teile.
#[test]
fn test_kleine_seite() {
    let html = "<html><body>\
        <h1>Titel</h1>\
        <p>Ein Absatz mit <b>fettem</b> Text und einem <a href=x>Link</a>.</p>\
        <ul><li>eins</li><li>zwei</li></ul>\
        <table><tr><td>A</td><td>B</td></tr></table>\
        </body></html>";
    let (ergebnis, liste) = setzen(html, "", 600);

    assert!(ergebnis.befund.sauber(), "{:?}", ergebnis.befund);
    assert!(ergebnis.hoehe > 100, "Hoehe {}", ergebnis.hoehe);

    let text = liste.text();
    for stueck in ["Titel", "Absatz", "fettem", "Link", "eins", "zwei", "A", "B"] {
        assert!(text.contains(stueck), "'{stueck}' fehlt in: {text}");
    }

    // Die Ueberschrift ist groesser als der Fliesstext.
    let groessen: Vec<i32> = liste
        .befehle
        .iter()
        .filter_map(|b| match b {
            Befehl::Text { text, groesse, .. } if text.contains("Titel") => Some(*groesse),
            _ => None,
        })
        .collect();
    assert!(!groessen.is_empty());
    assert!(groessen[0] > 16, "h1 muss groesser als 16px sein: {groessen:?}");

    // Und alles liegt untereinander.
    let t = texte(&liste);
    let titel_y = t.iter().find(|(_, _, s)| s.contains("Titel")).unwrap().1;
    let letzte_y = t.iter().map(|(_, y, _)| *y).max().unwrap();
    assert!(letzte_y > titel_y);
}

// ===========================================================================
// BILDGROESSEN UND REFLOW (Serie 8, Teil 8)
// ===========================================================================

use crate::inline::masse_waehlen;

/// Eine Metrik, die die Eigengroesse EINES Bildes kennt.
struct MitBild {
    innen: FesteMetrik,
    quelle: &'static str,
    masse: (i32, i32),
}

impl Metrik for MitBild {
    fn text_breite(&self, text: &str, groesse: i32, fett: bool, kursiv: bool) -> i32 {
        self.innen.text_breite(text, groesse, fett, kursiv)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        self.innen.zeilen_hoehe(groesse)
    }
    fn grundlinie(&self, groesse: i32) -> i32 {
        self.innen.grundlinie(groesse)
    }
    fn bild_masse(&self, quelle: &str) -> Option<(i32, i32)> {
        if quelle == self.quelle {
            Some(self.masse)
        } else {
            None
        }
    }
}

/// Die drei Stufen als reine Funktion.
#[test]
fn test_masse_waehlen_drei_stufen() {
    // 1. Beide Angaben schlagen alles.
    assert_eq!(masse_waehlen(Some(10), Some(20), Some((800, 600))), (10, 20));
    // 2. Keine Angabe, aber Eigengroesse bekannt.
    assert_eq!(masse_waehlen(None, None, Some((800, 600))), (800, 600));
    // 3. Nichts bekannt -> Platzhalter.
    assert_eq!(masse_waehlen(None, None, None), (32, 32));
}

/// DIE HALBE ANGABE: `<img width="200">` an einem 800x600-Bild muss
/// 200x150 ergeben, nicht 200x32. Seiten, die nur die Breite vorgeben,
/// sind haeufig — ohne das Seitenverhaeltnis wird jedes solche Bild
/// gequetscht.
#[test]
fn test_halbe_angabe_ergaenzt_aus_dem_seitenverhaeltnis() {
    assert_eq!(masse_waehlen(Some(200), None, Some((800, 600))), (200, 150));
    assert_eq!(masse_waehlen(None, Some(300), Some((800, 600))), (400, 300));
    // Ohne Eigengroesse bleibt der Platzhalter fuer die fehlende Seite.
    assert_eq!(masse_waehlen(Some(200), None, None), (200, 32));
    // Und eine Eigengroesse von 0 fuehrt nicht zur Division durch null.
    assert_eq!(masse_waehlen(Some(200), None, Some((0, 600))), (200, 32));
    assert_eq!(masse_waehlen(None, Some(200), Some((800, 0))), (32, 200));
}

/// Ohne bekannte Eigengroesse bleibt es beim Platzhalter — der Stand von
/// Teil 7, unveraendert.
#[test]
fn test_bild_ohne_wissen_bleibt_platzhalter() {
    let metrik = FesteMetrik::neu();
    let (_, liste) = crate::seite_setzen("<img src='a.png'>", "", 400, &metrik);
    let bild = liste
        .befehle
        .iter()
        .find_map(|b| match b {
            crate::Befehl::Bild { rechteck, .. } => Some(*rechteck),
            _ => None,
        })
        .expect("Bild-Befehl");
    assert_eq!((bild.breite, bild.hoehe), (32, 32));
}

/// **DER REFLOW.** Dasselbe Dokument, dieselbe Breite — nur der Wirt kennt
/// das Bild jetzt. Das Layout MUSS ein anderes sein.
///
/// Das ist die Zusage, auf der die Invalidierungs-Regel des Browsers
/// steht: Ein Bild ohne Massangabe aendert nach dem Laden die Geometrie,
/// also reicht ein Neu-MALEN nicht.
#[test]
fn test_geladenes_bild_aendert_das_layout() {
    let html = "<img src='a.png'><p>text darunter</p>";
    let ohne = FesteMetrik::neu();
    let (vorher, liste_vorher) = crate::seite_setzen(html, "", 400, &ohne);

    let mit = MitBild {
        innen: FesteMetrik::neu(),
        quelle: "a.png",
        masse: (120, 90),
    };
    let (nachher, liste_nachher) = crate::seite_setzen(html, "", 400, &mit);

    // Das Bild hat jetzt seine Eigengroesse.
    let bild = liste_nachher
        .befehle
        .iter()
        .find_map(|b| match b {
            crate::Befehl::Bild { rechteck, .. } => Some(*rechteck),
            _ => None,
        })
        .expect("Bild-Befehl");
    assert_eq!((bild.breite, bild.hoehe), (120, 90));

    // Und die Seite ist dadurch hoeher geworden — der Text darunter ist
    // nach unten gerueckt. GENAU DAS ist der Reflow.
    assert!(
        nachher.hoehe > vorher.hoehe,
        "90 px Bild statt 32 px Platzhalter muss die Seite hoeher machen ({} -> {})",
        vorher.hoehe,
        nachher.hoehe
    );
    let y_vorher = text_y_von(&liste_vorher, "text");
    let y_nachher = text_y_von(&liste_nachher, "text");
    assert!(
        y_nachher > y_vorher,
        "der Text unter dem Bild muss nach unten wandern ({} -> {})",
        y_vorher,
        y_nachher
    );
}

/// Ein Bild MIT Massangabe aendert das Layout NICHT, auch wenn seine
/// Eigengroesse ganz anders ist. Deshalb darf der Browser in diesem Fall
/// beim blossen Neu-Malen bleiben.
#[test]
fn test_bild_mit_massangabe_loest_keinen_reflow_aus() {
    let html = "<img src='a.png' width='40' height='30'><p>text</p>";
    let ohne = FesteMetrik::neu();
    let (vorher, _) = crate::seite_setzen(html, "", 400, &ohne);
    let mit = MitBild {
        innen: FesteMetrik::neu(),
        quelle: "a.png",
        masse: (800, 600),
    };
    let (nachher, _) = crate::seite_setzen(html, "", 400, &mit);
    assert_eq!(
        vorher.hoehe, nachher.hoehe,
        "die Angabe im Dokument schlaegt die Eigengroesse"
    );
}

fn text_y_von(liste: &crate::Anzeigeliste, gesucht: &str) -> i32 {
    liste
        .befehle
        .iter()
        .find_map(|b| match b {
            crate::Befehl::Text { y, text, .. } if text == gesucht => Some(*y),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Text '{}' nicht gefunden", gesucht))
}

// ===========================================================================
// TEXTKARTE: AUSWAHL UND SUCHE (Serie 9, Teil 2)
//
// Alle Zahlen hier sind KOPFRECHNUNG mit der Attrappe: 10 px je Zeichen
// bei 16 px Schrift. Ein Test, dessen Erwartung aus dem Ergebnis
// abgeschrieben ist, prueft nichts.
// ===========================================================================

use crate::textkarte::Textkarte;

fn karte(html: &str, css: &str, breite: i32) -> (Textkarte, FesteMetrik) {
    let metrik = FesteMetrik::neu();
    let (_, liste) = seite_setzen(html, css, breite, &metrik);
    (Textkarte::neu(&liste, &metrik), metrik)
}

/// Der Gesamttext haengt die Woerter mit Leerzeichen aneinander.
#[test]
fn test_textkarte_gesamttext() {
    let (k, _) = karte("<p>Hallo schoene Welt</p>", "", 800);
    assert_eq!(k.text(), "Hallo schoene Welt");
}

/// **Ein Treffer ueber die Wortgrenze hinweg.** Das ist der Fall, an dem
/// eine Suche je Anzeige-Befehl scheitern wuerde: „Hallo" und „Welt"
/// sind zwei Befehle.
#[test]
fn test_suche_ueber_wortgrenze() {
    let (k, _) = karte("<p>Hallo schoene Welt</p>", "", 800);
    let treffer = k.suchen("schoene Welt");
    assert_eq!(treffer.len(), 1, "der Ausdruck steht ueber zwei Befehle");
    assert_eq!(k.text_zwischen(treffer[0].von, treffer[0].bis), "schoene Welt");
}

/// **Ein Treffer ueber eine INLINE-Grenze hinweg**, also ohne
/// Leerzeichen. `<b>Rust</b>aceans` sind zwei Laeufe, die unmittelbar
/// aneinanderstossen — dort darf KEIN Trennzeichen eingefuegt werden,
/// sonst ist „Rustaceans" unauffindbar, obwohl es so auf dem Schirm
/// steht.
#[test]
fn test_suche_ueber_inline_grenze() {
    let (k, _) = karte("<p><b>Rust</b>aceans</p>", "", 800);
    assert_eq!(k.text(), "Rustaceans", "kein Leerzeichen an der Inline-Naht");
    assert_eq!(k.suchen("Rustaceans").len(), 1);
}

/// Gross/Klein wird ignoriert — auch bei Umlauten. Ein ASCII-Vergleich
/// wuerde „UEBER" finden und „ÜBER" nicht.
#[test]
fn test_suche_ignoriert_gross_klein_mit_umlauten() {
    let (k, _) = karte("<p>Über Öl und Äpfel</p>", "", 800);
    assert_eq!(k.suchen("über").len(), 1, "Ü gegen ü");
    assert_eq!(k.suchen("ÖL").len(), 1, "Ö gegen ö");
    assert_eq!(k.suchen("äPFEL").len(), 1);
}

/// Treffer ueberlappen nicht: „aa" in „aaaa" sind ZWEI Treffer, nicht
/// drei. Sonst rueckt „weiter suchen" um ein Zeichen statt
/// voranzukommen.
#[test]
fn test_suche_ueberlappt_nicht() {
    let (k, _) = karte("<p>aaaa</p>", "", 800);
    assert_eq!(k.suchen("aa").len(), 2);
}

/// Eine leere Nadel findet nichts (und haengt nicht).
#[test]
fn test_suche_leer() {
    let (k, _) = karte("<p>Text</p>", "", 800);
    assert!(k.suchen("").is_empty());
    assert!(k.suchen("Textttt").is_empty(), "laenger als der Text");
}

/// Umlaute werden in ZEICHEN gezaehlt, nicht in Bytes. „Grüße" hat fuenf
/// Zeichen und sieben Bytes — wer in Bytes rechnet, waehlt hier falsch
/// aus und schneidet im schlimmsten Fall mitten in ein Zeichen.
#[test]
fn test_auswahl_zaehlt_zeichen_nicht_bytes() {
    let (k, _) = karte("<p>Grüße Welt</p>", "", 800);
    assert_eq!(k.text_zwischen(0, 5), "Grüße");
    assert_eq!(k.text_zwischen(6, 10), "Welt");
}

/// Rueckwaerts ausgewaehlt ist dasselbe wie vorwaerts.
#[test]
fn test_auswahl_richtungsunabhaengig() {
    let (k, _) = karte("<p>Hallo Welt</p>", "", 800);
    assert_eq!(k.text_zwischen(6, 10), k.text_zwischen(10, 6));
}

/// Ein Klick trifft die Zeichengrenze, an der er am naechsten ist:
/// linke Haelfte -> davor, rechte Haelfte -> dahinter. Bei 10 px je
/// Zeichen liegt die Mitte des ersten Zeichens bei x=5.
#[test]
fn test_auswahl_rundet_zur_naechsten_zeichengrenze() {
    let (k, m) = karte("<p>Hallo</p>", "body,p{margin:0;padding:0}", 800);
    let y = 8; // irgendwo in der ersten Zeile
    assert_eq!(k.stelle_bei(0, y, &m), 0, "ganz links");
    assert_eq!(k.stelle_bei(4, y, &m), 0, "linke Haelfte des ersten Zeichens");
    assert_eq!(k.stelle_bei(6, y, &m), 1, "rechte Haelfte -> dahinter");
    assert_eq!(k.stelle_bei(14, y, &m), 1, "linke Haelfte des zweiten");
}

/// Rechts neben das Zeilenende geklickt heisst: ans Zeilenende. Es gibt
/// IMMER eine Antwort, sonst haette jede Auswahl Loecher.
#[test]
fn test_auswahl_hinter_dem_zeilenende() {
    let (k, m) = karte("<p>Hallo</p>", "body,p{margin:0;padding:0}", 800);
    assert_eq!(k.stelle_bei(9999, 8, &m), 5, "hinter das letzte Zeichen");
}

/// **Eine Auswahl ueber zwei Zeilen ergibt zwei Rechtecke.** Ein
/// einzelnes umschliessendes Rechteck waere falsch — es faerbte auch
/// den Rand rechts und links mit.
#[test]
fn test_auswahl_ueber_zwei_zeilen_gibt_zwei_rechtecke() {
    // 70 px Breite = 7 Zeichen je Zeile: „aaa" und „bbb" landen
    // untereinander (aaa + Leerzeichen + bbb = 7 Zeichen passt genau,
    // deshalb hier enger).
    let (k, m) = karte("<p>aaa bbb</p>", "body,p{margin:0;padding:0;width:40px}", 40);
    let rechtecke = k.rechtecke(0, k.len(), &m);
    assert_eq!(rechtecke.len(), 2, "je Zeile ein Rechteck: {:?}", rechtecke);
    assert_ne!(rechtecke[0].y, rechtecke[1].y, "sie liegen untereinander");
}

/// Eine leere Auswahl bedeckt nichts.
#[test]
fn test_auswahl_leer_ergibt_keine_rechtecke() {
    let (k, m) = karte("<p>Hallo</p>", "", 800);
    assert!(k.rechtecke(3, 3, &m).is_empty());
}
