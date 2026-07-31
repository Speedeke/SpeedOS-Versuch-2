// speedhtml::tests — die fiesen Faelle
//
// ===========================================================================
// WORAUF ES HIER ANKOMMT
//
// Ein HTML-Parser wird nicht daran gemessen, ob er gutes HTML parst — das
// tut jeder. Er wird daran gemessen, was er mit dem tut, was ihm die Welt
// wirklich vorlegt: nie geschlossene Tags, doppelte Endtags, Attribute ohne
// Anfuehrungszeichen, `<` mitten im Fliesstext, abgeschnittene Downloads
// und generierten Muell.
//
// **DIE ZUSAGE UEBER ALLEN: NICHTS DARF PANICKEN.** Ein Panic in Ring 3
// beendet den Prozess (Exit 101) — ein Browser, der bei einer kaputten
// Seite verschwindet statt sie schief anzuzeigen, ist unbrauchbar.
//
// Diese Tests laufen auf dem HOST in Millisekunden. Genau deshalb sind es
// viele: `cargo test` in speedhtml/ kostet keinen QEMU-Start.

use crate::dom::{ist_void, Art, Grenzen};
use crate::tokenizer::{Token, Tokenizer};
use crate::{baum_text, parsen, parsen_mit};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

fn token(html: &str) -> Vec<Token> {
    Tokenizer::neu(html).collect()
}

/// Alle Tag-Namen in Dokumentreihenfolge — die kompakte Form, in der sich
/// eine Baumstruktur in einer Zeile behaupten laesst.
fn tags(html: &str) -> Vec<String> {
    let d = parsen(html);
    d.alle()
        .filter_map(|(_, k)| k.name().map(|n| n.to_string()))
        .collect()
}

/// Der sichtbare Text des ganzen Dokuments.
fn text(html: &str) -> String {
    let d = parsen(html);
    d.text_von(crate::Dokument::WURZEL)
}

/// Die Kinder-Tagnamen eines Elements.
fn kinder_von(html: &str, tag: &str) -> Vec<String> {
    let d = parsen(html);
    let Some(id) = d.erstes(tag) else {
        return Vec::new();
    };
    d.knoten(id)
        .unwrap()
        .kinder
        .iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()).map(|n| n.to_string()))
        .collect()
}

// ===========================================================================
// 1. TOKENIZER — die Grundlagen
// ===========================================================================

#[test]
fn test_einfachster_fall() {
    let t = token("<p>Hallo</p>");
    assert_eq!(t.len(), 3);
    assert!(matches!(&t[0], Token::StartTag { name, .. } if name == "p"));
    assert_eq!(t[1], Token::Text("Hallo".to_string()));
    assert!(matches!(&t[2], Token::EndTag { name } if name == "p"));
}

/// Tag- und Attributnamen werden kleingeschrieben, Attributwerte NICHT.
///
/// Der Unterschied ist wichtig: `<A HREF="/Pfad">` verweist auf `/Pfad`
/// mit grossem P, aber das Element heisst `a` und das Attribut `href`.
#[test]
fn test_grossschreibung() {
    let t = token("<DIV CLASS=\"Gross\">x</DIV>");
    match &t[0] {
        Token::StartTag { name, attribute, .. } => {
            assert_eq!(name, "div");
            assert_eq!(attribute[0].0, "class");
            assert_eq!(attribute[0].1, "Gross", "der WERT bleibt, wie er ist");
        }
        _ => panic!("kein StartTag"),
    }
    assert!(matches!(&t[2], Token::EndTag { name } if name == "div"));
}

/// ATTRIBUTE OHNE ANFUEHRUNGSZEICHEN — einer der genannten fiesen Faelle.
#[test]
fn test_attribute_ohne_anfuehrungszeichen() {
    let t = token("<a href=/pfad/seite.html class=link target=_blank>x</a>");
    match &t[0] {
        Token::StartTag { attribute, .. } => {
            assert_eq!(attribute.len(), 3);
            // DER FALL, DEN MAN FALSCH MACHT: Der Schraegstrich gehoert
            // zum Wert, nicht zum Tag.
            assert_eq!(attribute[0], ("href".to_string(), "/pfad/seite.html".to_string()));
            assert_eq!(attribute[1], ("class".to_string(), "link".to_string()));
            assert_eq!(attribute[2], ("target".to_string(), "_blank".to_string()));
        }
        _ => panic!("kein StartTag"),
    }
}

/// Auch der Fall, in dem ein nackter Wert direkt vor `>` endet.
#[test]
fn test_nackter_wert_vor_spitzklammer() {
    let t = token("<a href=/pfad/>Text</a>");
    match &t[0] {
        Token::StartTag { attribute, selbst_schliessend, .. } => {
            assert_eq!(attribute[0].1, "/pfad/", "der / gehoert zum Wert");
            assert!(!selbst_schliessend, "das ist KEIN selbstschliessender Tag");
        }
        _ => panic!("kein StartTag"),
    }
    assert_eq!(t[1], Token::Text("Text".to_string()));
}

#[test]
fn test_gemischte_anfuehrungszeichen() {
    let t = token("<a x=\"doppelt\" y='einfach' z=nackt>");
    match &t[0] {
        Token::StartTag { attribute, .. } => {
            assert_eq!(attribute[0].1, "doppelt");
            assert_eq!(attribute[1].1, "einfach");
            assert_eq!(attribute[2].1, "nackt");
        }
        _ => panic!("kein StartTag"),
    }
}

/// Ein Attribut ohne Wert ist LEER, nicht abwesend.
#[test]
fn test_attribut_ohne_wert() {
    let t = token("<input disabled checked type=text>");
    match &t[0] {
        Token::StartTag { attribute, .. } => {
            assert_eq!(attribute.len(), 3);
            assert_eq!(attribute[0], ("disabled".to_string(), String::new()));
            assert_eq!(attribute[1], ("checked".to_string(), String::new()));
            assert_eq!(attribute[2].1, "text");
        }
        _ => panic!("kein StartTag"),
    }
}

/// Doppelte Attribute: das ERSTE gewinnt (so die Spezifikation).
#[test]
fn test_doppelte_attribute() {
    let t = token("<a href=gut href=boese>");
    match &t[0] {
        Token::StartTag { attribute, .. } => {
            assert_eq!(attribute.len(), 1);
            assert_eq!(attribute[0].1, "gut");
        }
        _ => panic!("kein StartTag"),
    }
}

/// `<` MITTEN IM TEXT — einer der genannten fiesen Faelle.
///
/// Ein `<`, dem kein Buchstabe, `/`, `!` oder `?` folgt, ist ein
/// Kleinerzeichen und kein Tag-Anfang. Ohne diese Regel verschwindet bei
/// jeder Mathe- oder Programmierseite der halbe Text.
#[test]
fn test_kleinerzeichen_im_text() {
    assert_eq!(text("<p>5 < 7 und 3 > 1</p>"), "5 < 7 und 3 > 1");
    assert_eq!(text("<p>a<b</p>"), "a");  // `<b` IST ein Tag-Anfang
    assert_eq!(text("<p>a < b</p>"), "a < b");
    assert_eq!(text("<p>x<1</p>"), "x<1");
    assert_eq!(text("<p>2<3<4</p>"), "2<3<4");
    // Ein `<` als letztes Zeichen der Datei.
    assert_eq!(text("<p>Ende <"), "Ende <");
}

/// KOMMENTARE, auch nie geschlossene.
#[test]
fn test_kommentare() {
    let t = token("<!-- versteckt -->sichtbar");
    assert_eq!(t[0], Token::Kommentar(" versteckt ".to_string()));
    assert_eq!(t[1], Token::Text("sichtbar".to_string()));

    // Ein Kommentar, der nie endet, frisst den Rest — und das ist richtig.
    let t = token("Text<!-- nie zu Ende");
    assert_eq!(t[0], Token::Text("Text".to_string()));
    assert!(matches!(&t[1], Token::Kommentar(_)));
    // Der Text im Kommentar darf NICHT sichtbar sein.
    assert_eq!(text("Text<!-- nie zu Ende"), "Text");
}

/// Ein Kommentar mit `<` und Tags darin ist trotzdem nur ein Kommentar.
#[test]
fn test_kommentar_mit_tags_darin() {
    let d = parsen("<p>a</p><!-- <p>b</p> --><p>c</p>");
    assert_eq!(tags("<p>a</p><!-- <p>b</p> --><p>c</p>"), ["p", "p"]);
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "ac");
}

#[test]
fn test_doctype() {
    let t = token("<!DOCTYPE html><p>x");
    assert_eq!(t[0], Token::Doctype("html".to_string()));
    let t = token("<!doctype HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\">");
    assert_eq!(t[0], Token::Doctype("html".to_string()));
}

/// `<?xml ... ?>` ist laut Spezifikation ein Bogus-Kommentar, kein Fehler.
#[test]
fn test_xml_deklaration_wird_geschluckt() {
    assert_eq!(text("<?xml version=\"1.0\"?><p>Inhalt</p>"), "Inhalt");
}

// ===========================================================================
// 2. ROHTEXT — script und style
// ===========================================================================

/// DER WICHTIGSTE TOKENIZER-TEST.
///
/// In `<script>` steht `if (a < b)`. Ein Tokenizer ohne Rohtext-Zustand
/// findet dort einen Tag-Anfang und verschluckt den Rest der Seite. Das
/// ist kein Randfall, sondern das erste, woran ein selbstgebauter Parser
/// stirbt.
#[test]
fn test_script_inhalt_ist_kein_markup() {
    let html = "<script>if (a < b && c > d) { x = '</p>'; }</script><p>danach</p>";
    let d = parsen(html);
    // Genau zwei Elemente: script und p.
    assert_eq!(tags(html), ["script", "p"]);
    // Und der Absatz DANACH ist noch da — er wurde nicht verschluckt.
    let p = d.erstes("p").expect("<p> fehlt");
    assert_eq!(d.text_von(p), "danach");
}

/// Der Skript-Inhalt gehoert NICHT in den sichtbaren Text. Das ist die
/// Entscheidung, die `news` in Serie 7 gerettet hat.
#[test]
fn test_script_inhalt_ist_nicht_sichtbar() {
    let html = "<p>echt</p><script>var geheim = 1;</script><style>p{color:red}</style>";
    assert_eq!(text(html), "echt");
}

#[test]
fn test_style_inhalt_ist_kein_markup() {
    let html = "<style>p::before { content: '<b>' }</style><p>x</p>";
    assert_eq!(tags(html), ["style", "p"]);
}

/// `</scriptfoo>` beendet `<script>` NICHT.
#[test]
fn test_aehnliches_endtag_beendet_rohtext_nicht() {
    let html = "<script>a</scriptfoo>b</script><p>x</p>";
    assert_eq!(tags(html), ["script", "p"]);
}

/// Gross-/Kleinschreibung beim Endtag eines Rohtext-Elements.
#[test]
fn test_rohtext_endtag_gross_geschrieben() {
    let html = "<script>x</SCRIPT><p>danach</p>";
    assert_eq!(tags(html), ["script", "p"]);
    assert_eq!(text(html), "danach");
}

/// `<title>` ist RCDATA: kein Markup, ABER Zeichenreferenzen.
#[test]
fn test_title_loest_referenzen_auf() {
    let d = parsen("<title>Tom &amp; Jerry &lt;3</title>");
    let t = d.erstes("title").expect("<title> fehlt");
    assert_eq!(d.text_von(t), "Tom & Jerry <3");
}

/// In `<script>` werden Referenzen NICHT aufgeloest — dort ist `&amp;`
/// Programmtext.
#[test]
fn test_script_loest_referenzen_nicht_auf() {
    let d = parsen("<script>if (a &amp;&amp; b) {}</script>");
    let s = d.erstes("script").expect("<script> fehlt");
    // Der Rohtext bleibt, wie er ist.
    let inhalt = d
        .knoten(s)
        .unwrap()
        .kinder
        .first()
        .and_then(|k| d.knoten(*k))
        .and_then(|n| n.text())
        .unwrap_or("");
    assert_eq!(inhalt, "if (a &amp;&amp; b) {}");
}

// ===========================================================================
// 3. VOID-ELEMENTE
// ===========================================================================

/// Void-Elemente bekommen NIE Kinder.
///
/// Ein `<br>` auf den Stapel offener Elemente zu legen ist der Fehler,
/// nach dem der ganze Rest des Dokuments im `<br>` landet.
#[test]
fn test_void_elemente_bekommen_keine_kinder() {
    let d = parsen("<p>a<br>b<br>c</p>");
    let p = d.erstes("p").unwrap();
    // Der Absatz hat fuenf Kinder: Text, br, Text, br, Text.
    assert_eq!(d.knoten(p).unwrap().kinder.len(), 5);
    for br in d.alle_mit_tag("br") {
        assert!(d.knoten(br).unwrap().kinder.is_empty(), "<br> hat Kinder");
    }
    assert_eq!(d.text_von(p), "abc");
}

#[test]
fn test_img_und_meta_sind_void() {
    let html = "<meta charset=utf-8><img src=a.png alt=A><p>Text</p>";
    let d = parsen(html);
    // Alle drei sind GESCHWISTER, nicht ineinander verschachtelt.
    let kinder: Vec<_> = d
        .knoten(crate::Dokument::WURZEL)
        .unwrap()
        .kinder
        .iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
        .collect();
    assert_eq!(kinder, ["meta", "img", "p"]);
}

#[test]
fn test_void_liste_stimmt() {
    for name in ["br", "img", "meta", "hr", "input", "link", "area", "col", "wbr"] {
        assert!(ist_void(name), "{name} muss void sein");
    }
    for name in ["div", "p", "span", "a", "script", "table"] {
        assert!(!ist_void(name), "{name} darf nicht void sein");
    }
}

/// `</br>` ist bedeutungslos und darf nichts kaputtmachen.
#[test]
fn test_endtag_fuer_void_element() {
    let d = parsen("<p>a</br>b</p>");
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "ab");
    assert_eq!(d.befund.unerwartete_endtags, 1);
}

/// Selbstschliessende Schreibweise bei einem NICHT-Void-Element.
#[test]
fn test_selbstschliessender_nicht_void_tag() {
    // `<div/>` ist in HTML kein selbstschliessender Tag — der Tokenizer
    // meldet das Flag, und der Baum-Aufbau befolgt es (freundlicher als
    // die Spezifikation, aber es verhindert, dass der Rest im div landet).
    let d = parsen("<div/><p>danach</p>");
    let kinder: Vec<_> = d
        .knoten(crate::Dokument::WURZEL)
        .unwrap()
        .kinder
        .iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
        .collect();
    assert_eq!(kinder, ["div", "p"]);
}

// ===========================================================================
// 4. FEHLERERHOLUNG — die genannten Faelle
// ===========================================================================

/// VERSCHACHTELTE `<p>` — einer der genannten fiesen Faelle.
///
/// `<p>` darf laut Spezifikation ohne `</p>` bleiben. Ohne implizites
/// Schliessen verschachteln sich alle folgenden Absaetze ineinander, und
/// das Layout rueckt jeden weiter ein.
#[test]
fn test_verschachtelte_absaetze() {
    let html = "<p>eins<p>zwei<p>drei";
    let d = parsen(html);
    let wurzel = d.knoten(crate::Dokument::WURZEL).unwrap();
    assert_eq!(wurzel.kinder.len(), 3, "drei GESCHWISTER, nicht verschachtelt");
    for kind in &wurzel.kinder {
        assert_eq!(d.knoten(*kind).unwrap().name(), Some("p"));
    }
    assert_eq!(d.befund.implizit_geschlossen, 2);
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "einszweidrei");
}

/// Ein Blockelement beendet einen offenen Absatz.
#[test]
fn test_block_beendet_absatz() {
    let d = parsen("<p>Absatz<div>Block</div>");
    let wurzel = d.knoten(crate::Dokument::WURZEL).unwrap();
    assert_eq!(wurzel.kinder.len(), 2);
    assert_eq!(d.knoten(wurzel.kinder[0]).unwrap().name(), Some("p"));
    assert_eq!(d.knoten(wurzel.kinder[1]).unwrap().name(), Some("div"));
}

/// Ein INLINE-Element beendet den Absatz NICHT.
#[test]
fn test_inline_beendet_absatz_nicht() {
    assert_eq!(kinder_von("<p>a<b>fett</b>c</p>", "p"), ["b"]);
}

/// Ein `<div>` IM Listenpunkt darf den Listenpunkt nicht schliessen.
///
/// Die Absatzliste enthaelt `div`, aber geschlossen wird nur, was
/// UNMITTELBAR offen ist — sonst zerfaellt jede Liste mit Kaesten darin.
#[test]
fn test_div_im_listenpunkt_schliesst_ihn_nicht() {
    let d = parsen("<ul><li><div>Kasten</div></li></ul>");
    let li = d.erstes("li").unwrap();
    let kinder: Vec<_> = d.knoten(li).unwrap().kinder.iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
        .collect();
    assert_eq!(kinder, ["div"], "der <div> gehoert IN den <li>");
}

/// Listenpunkte ohne `</li>`.
#[test]
fn test_listenpunkte_ohne_endtag() {
    let html = "<ul><li>eins<li>zwei<li>drei</ul>";
    let d = parsen(html);
    let ul = d.erstes("ul").unwrap();
    let kinder = &d.knoten(ul).unwrap().kinder;
    assert_eq!(kinder.len(), 3, "drei Geschwister im <ul>");
    for k in kinder {
        assert_eq!(d.knoten(*k).unwrap().name(), Some("li"));
    }
}

/// TABELLEN OHNE `<tbody>` — einer der genannten fiesen Faelle.
///
/// Wir ergaenzen KEIN `<tbody>`: Ein Baum soll zeigen, was im Dokument
/// steht, und das spaetere Layout behandelt `table > tr` und
/// `table > tbody > tr` gleich. Was hier zaehlt, ist, dass die Zeilen
/// GESCHWISTER sind und die Zellen in den Zeilen liegen.
#[test]
fn test_tabelle_ohne_tbody() {
    let html = "<table><tr><td>a<td>b<tr><td>c<td>d</table>";
    let d = parsen(html);
    let tabelle = d.erstes("table").unwrap();
    let zeilen: Vec<_> = d.knoten(tabelle).unwrap().kinder.iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
        .collect();
    assert_eq!(zeilen, ["tr", "tr"], "zwei Zeilen als Geschwister");

    for tr in d.alle_mit_tag("tr") {
        let zellen: Vec<_> = d.knoten(tr).unwrap().kinder.iter()
            .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
            .collect();
        assert_eq!(zellen, ["td", "td"], "zwei Zellen je Zeile");
    }
    assert_eq!(d.text_von(tabelle), "abcd");
}

#[test]
fn test_tabelle_mit_tbody_und_th() {
    let html = "<table><thead><tr><th>K1<th>K2<tbody><tr><td>a<td>b</table>";
    let d = parsen(html);
    let tabelle = d.erstes("table").unwrap();
    let abschnitte: Vec<_> = d.knoten(tabelle).unwrap().kinder.iter()
        .filter_map(|k| d.knoten(*k).and_then(|n| n.name()))
        .collect();
    assert_eq!(abschnitte, ["thead", "tbody"], "thead wird von tbody beendet");
}

/// NIE GESCHLOSSENE TAGS — einer der genannten fiesen Faelle.
#[test]
fn test_nie_geschlossene_tags() {
    let html = "<div><section><article><p>Text";
    let d = parsen(html);
    assert_eq!(tags(html), ["div", "section", "article", "p"]);
    // Alle vier waren am Ende offen.
    assert_eq!(d.befund.am_ende_geschlossen, 4);
    // Und der Text ist trotzdem da.
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "Text");
    // Die Verschachtelung stimmt: p liegt in article.
    let p = d.erstes("p").unwrap();
    let eltern = d.knoten(p).unwrap().eltern.unwrap();
    assert_eq!(d.knoten(eltern).unwrap().name(), Some("article"));
}

/// UNERWARTETE ENDTAGS werden ignoriert.
///
/// Ein doppeltes `</div>` zu befolgen wuerde ein Element schliessen, das
/// jemand anders geoeffnet hat — ab da waere der ganze Baum falsch.
#[test]
fn test_unerwartete_endtags_werden_ignoriert() {
    let d = parsen("</p></div><p>Inhalt</p></span></div>");
    assert_eq!(d.befund.unerwartete_endtags, 4);
    // Der Absatz ist unbeschadet.
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "Inhalt");
    let p = d.erstes("p").unwrap();
    assert_eq!(d.knoten(p).unwrap().eltern, Some(crate::Dokument::WURZEL));
}

/// UEBERKREUZTE Tags (`<b><i></b></i>`) — HTML erlaubt das nicht, das Web
/// ist voll davon.
#[test]
fn test_ueberkreuzte_tags() {
    let html = "<b>fett<i>beides</b>nur kursiv</i>";
    let d = parsen(html);
    // `</b>` schliesst b UND den dazwischen offenen i.
    assert_eq!(d.befund.uebersprungene_ebenen, 1);
    // Kein Absturz, und der Text ist vollstaendig.
    assert_eq!(d.text_von(crate::Dokument::WURZEL), "fettbeidesnur kursiv");
}

/// Verschachtelte `<a>` — der Klassiker fuer „der ganze Rest der Seite ist
/// ein Link".
#[test]
fn test_verschachtelte_links() {
    let d = parsen("<a href=1>eins<a href=2>zwei</a>");
    let wurzel = d.knoten(crate::Dokument::WURZEL).unwrap();
    assert_eq!(wurzel.kinder.len(), 2, "zwei Links als Geschwister");
}

/// ABGESCHNITTENES DOKUMENT — einer der genannten fiesen Faelle.
///
/// Jede Abschnittstelle einzeln: Der Parser muss an JEDER Stelle einen
/// Baum liefern, nicht nur an bequemen.
#[test]
fn test_abgeschnitten_an_jeder_stelle() {
    let voll = "<!DOCTYPE html><html><head><title>T &amp; T</title></head>\
                <body><h1 class=\"kopf\">Titel</h1><p>Text mit <a href=\"/x\">Link</a>.</p>\
                <ul><li>eins<li>zwei</ul><img src=\"b.png\"><!-- Ende --></body></html>";

    for bis in 0..voll.len() {
        // Nur auf Zeichengrenzen schneiden — sonst schneidet der TEST
        // falsch, nicht der Parser.
        if !voll.is_char_boundary(bis) {
            continue;
        }
        let stueck = &voll[..bis];
        // Die eigentliche Zusage: kein Panic. `parsen` hat keinen
        // Fehlerfall, also ist schon der Durchlauf der Test.
        let d = parsen(stueck);
        // Und der Baum ist benutzbar: Textabruf darf ebenfalls nicht
        // panicken.
        let _ = d.text_von(crate::Dokument::WURZEL);
        let _ = baum_text(&d);
    }
}

/// REGRESSIONSWAECHTER: `<` INNERHALB eines Tags.
///
/// ===================================================================
/// DER FEHLER, DEN DIESER TEST FESTNAGELT
///
/// `<p>a<b</p>` — ein Tag `<b`, der nie mit `>` geschlossen wurde, gefolgt
/// vom naechsten Tag. Der Tokenizer stand darauf in einer ENDLOSSCHLEIFE:
/// `name_lesen` bricht bei `<` ab und liefert einen leeren Namen, ohne die
/// Position zu bewegen; der Zustandswechsel fuehrte zurueck an dieselbe
/// Stelle, und das ging ewig so weiter.
///
/// Gefunden hat ihn NICHT das Nachdenken, sondern
/// `test_muellfolgen_panicken_nicht` — der Test mit den zufaelligen
/// Zeichenfolgen. Das ist der Grund, warum es ihn gibt.
///
/// Die Regel, die daraus wurde, steht im Tokenizer: **Jeder Durchlauf der
/// Tag-Schleife muss die Position bewegen oder den Tag beenden.**
#[test]
fn test_kleinerzeichen_im_tag_haengt_nicht() {
    // Genau der Fall, der haengen blieb.
    assert_eq!(text("<p>a<b</p>"), "a");

    // Und seine Verwandten — alle muessen ENDEN, egal was herauskommt.
    for html in [
        "<a <b>x",
        "<a b<c>x",
        "<a b=<c>x",
        "<a <<<<>x",
        "<a b c<d e<f>x",
        "<p><<<<<<<<<<p>Text",
        "</a <b>",
        "<a =<>x",
        "<a b='<'>x",
        "<ü<ö<ä>x",
    ] {
        let d = parsen(html);
        // Erreicht der Test diese Zeile, hat der Parser terminiert —
        // und genau das ist die Zusage.
        let _ = d.text_von(crate::Dokument::WURZEL);
        let _ = baum_text(&d);
    }
}

/// Ein Tag, der mitten im Attribut abbricht, wird verworfen — aber der
/// Text davor bleibt.
#[test]
fn test_abbruch_mitten_im_tag() {
    assert_eq!(text("<p>Text</p><div class=\"unvollstaendig"), "Text");
    assert_eq!(text("<p>Text</p><div class="), "Text");
    assert_eq!(text("<p>Text</p><di"), "Text");
    assert_eq!(text("<p>Text</p><"), "Text<");
}

// ===========================================================================
// 5. GRENZEN — 20 MB Muell
// ===========================================================================

/// 20 MB MUELL — der genannte Fall.
///
/// Fuenf Sorten Muell, jede mit einer anderen Angriffsrichtung:
/// Knotenzahl, Tiefe, Textlaenge, Tokenizer-Schleifen und Attributzahl.
/// **Keine darf panicken, und keine darf den Speicher sprengen** — die
/// Grenzen aus `dom::Grenzen` muessen greifen, und der Befund muss es
/// sagen.
#[test]
fn test_zwanzig_megabyte_muell() {
    const ZIEL: usize = 20 * 1024 * 1024;

    // (a) Millionen flacher Elemente -> Knotengrenze.
    let mut viele = String::with_capacity(ZIEL + 16);
    while viele.len() < ZIEL {
        viele.push_str("<div>x</div>");
    }
    let d = parsen(&viele);
    assert!(d.befund.abgeschnitten, "die Knotengrenze haette greifen muessen");
    assert!(d.anzahl() <= Grenzen::standard().max_knoten + 1);

    // (b) Millionen INEINANDER verschachtelter Elemente -> Tiefengrenze.
    let mut tief = String::with_capacity(ZIEL + 16);
    while tief.len() < ZIEL {
        tief.push_str("<div>");
    }
    let d = parsen(&tief);
    assert!(d.befund.tiefe <= Grenzen::standard().max_tiefe);
    assert!(d.befund.abgeschnitten);

    // (c) EIN riesiger Textknoten -> Textgrenze.
    let mut text = String::with_capacity(ZIEL + 16);
    text.push_str("<p>");
    while text.len() < ZIEL {
        text.push_str("Lorem ipsum dolor sit amet. ");
    }
    let d = parsen(&text);
    assert!(d.befund.abgeschnitten);

    // (d) Millionen einzelner `<` — der Tokenizer darf hier nicht in eine
    //     Schleife geraten (jedes `<` ist Text, kein Tag).
    let mut spitz = String::with_capacity(ZIEL + 16);
    while spitz.len() < ZIEL {
        spitz.push_str("< < < ");
    }
    let d = parsen(&spitz);
    let _ = d.anzahl();

    // (e) EIN Tag mit Millionen Attributen -> MAX_ATTRIBUTE.
    let mut attr = String::with_capacity(ZIEL + 16);
    attr.push_str("<div");
    while attr.len() < ZIEL {
        attr.push_str(" a=1");
    }
    attr.push('>');
    let d = parsen(&attr);
    let div = d.erstes("div").expect("<div> fehlt");
    match &d.knoten(div).unwrap().art {
        Art::Element { attribute, .. } => {
            assert!(
                attribute.len() <= crate::tokenizer::MAX_ATTRIBUTE,
                "Attributgrenze hat nicht gegriffen: {}",
                attribute.len()
            );
        }
        _ => panic!("kein Element"),
    }
}

/// Zufaelliger Bytemuell — die Faelle, an die man beim Nachdenken nicht
/// kommt.
///
/// Ein billiger deterministischer Generator (KEIN Zufall im Sinne der
/// RNG-Dauerregel — er heisst deshalb auch nicht so): Er soll
/// REPRODUZIERBAR dieselben Folgen liefern, damit ein Fehlschlag
/// nachstellbar ist.
#[test]
fn test_muellfolgen_panicken_nicht() {
    // Zeichen, die einen Parser aus dem Tritt bringen.
    let vorrat: &[u8] = b"<>/=\"' abc&;!-?\n\t\\{}[]#%";
    let mut zustand: u32 = 0x1234_5678;
    let mut folge = String::new();

    for durchgang in 0..300 {
        folge.clear();
        let laenge = 1 + (durchgang * 7) % 400;
        for _ in 0..laenge {
            // Ein LCG — absichtlich reproduzierbar, siehe Kommentar oben.
            zustand = zustand.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let i = (zustand >> 16) as usize % vorrat.len();
            folge.push(vorrat[i] as char);
        }
        let d = parsen(&folge);
        // Alles, was ein Aufrufer danach tut, darf ebenfalls nicht panicken.
        let _ = d.text_von(crate::Dokument::WURZEL);
        let _ = baum_text(&d);
        let _ = crate::befund_text(&d);
    }
}

/// Grenzen sind ein ARGUMENT — wer weniger Speicher hat, setzt sie enger.
#[test]
fn test_eigene_grenzen() {
    let eng = Grenzen {
        max_knoten: 10,
        max_tiefe: 3,
        max_text_bytes: 8,
    };
    let d = parsen_mit("<a><b><c><d><e>viel zu langer Text</e></d></c></b></a>", eng);
    assert!(d.anzahl() <= 11, "Wurzel + hoechstens 10");
    assert!(d.befund.tiefe <= 3);
    assert!(d.befund.abgeschnitten);
}

/// Leere und minimale Eingaben.
#[test]
fn test_leere_eingaben() {
    for eingabe in ["", " ", "<", ">", "<>", "</>", "<!", "<!-", "<!--", "&", "&#", "&#x"] {
        let d = parsen(eingabe);
        let _ = d.text_von(crate::Dokument::WURZEL);
        let _ = baum_text(&d);
    }
    assert_eq!(parsen("").anzahl(), 1, "nur die Wurzel");
}

/// Mehrbyte-Zeichen an jeder denkbaren Stelle — Tagnamen, Attributnamen,
/// Attributwerte, Text.
///
/// Der Test faengt einen `&str`-Index, der nicht auf einer Zeichengrenze
/// liegt: Das waere eine Panik, und zwar genau die Sorte, die erst bei
/// einer deutschen oder japanischen Seite auftritt.
#[test]
fn test_mehrbyte_zeichen_ueberall() {
    let faelle = [
        "<p>Grüße aus München</p>",
        "<p title=\"Schöße\">ÄÖÜ</p>",
        "<über>Inhalt</über>",
        "<p daß=nö>x</p>",
        "<p>😀🎉</p>",
        "<p title='😀'>x</p>",
        "<!-- Kömmentar mit Ümläuten -->",
        "<script>var s = 'Grüße';</script>",
        "<p>ü", // abgeschnitten direkt nach einem Mehrbyte-Zeichen
        "&#x1F600;",
    ];
    for html in faelle {
        let d = parsen(html);
        let _ = d.text_von(crate::Dokument::WURZEL);
        let _ = baum_text(&d);
    }
    assert_eq!(text("<p>Grüße aus München</p>"), "Grüße aus München");
    assert_eq!(text("<p>😀🎉</p>"), "😀🎉");
}

// ===========================================================================
// 6. DER BAUM ALS GANZES
// ===========================================================================

#[test]
fn test_attribute_am_knoten() {
    let d = parsen("<a href=\"/ziel\" class=\"link extern\" id=x>Text</a>");
    let a = d.erstes("a").unwrap();
    let k = d.knoten(a).unwrap();
    assert_eq!(k.attribut("href"), Some("/ziel"));
    assert_eq!(k.attribut("class"), Some("link extern"));
    assert_eq!(k.attribut("id"), Some("x"));
    assert_eq!(k.attribut("gibtsnicht"), None);
}

/// Zeichenreferenzen kommen im Baum aufgeloest an — im Text UND im
/// Attributwert.
#[test]
fn test_referenzen_im_baum() {
    let d = parsen("<a title=\"Tom &amp; Jerry\">5 &lt; 7 &#x2014; wirklich</a>");
    let a = d.erstes("a").unwrap();
    assert_eq!(d.knoten(a).unwrap().attribut("title"), Some("Tom & Jerry"));
    assert_eq!(d.text_von(a), "5 < 7 — wirklich");
}

#[test]
fn test_eltern_und_kinder_stimmen_ueberein() {
    let d = parsen("<div><p>a<b>c</b></p><span>d</span></div>");
    // Jedes Kind kennt seinen Elternteil, und der Elternteil das Kind.
    for (id, knoten) in d.alle() {
        for kind in &knoten.kinder {
            assert_eq!(
                d.knoten(*kind).unwrap().eltern,
                Some(id),
                "Kind {kind:?} zeigt nicht auf {id:?}"
            );
        }
        if let Some(eltern) = knoten.eltern {
            assert!(
                d.knoten(eltern).unwrap().kinder.contains(&id),
                "{id:?} steht nicht in den Kindern seines Elternteils"
            );
        }
    }
}

#[test]
fn test_befund_sauber_bei_sauberem_html() {
    let d = parsen("<div><p>Text</p><ul><li>a</li><li>b</li></ul></div>");
    assert!(
        d.befund.sauber(),
        "sauberes HTML darf keinen Befund erzeugen: {:?}",
        d.befund
    );
}

/// Die Baumausgabe zeigt Struktur, Attribute und Void-Markierung.
#[test]
fn test_baum_text() {
    let d = parsen("<div class=k><p>Hallo</p><br></div>");
    let t = baum_text(&d);
    assert!(t.contains("<div class=\"k\">"), "{t}");
    assert!(t.contains("<p>"), "{t}");
    assert!(t.contains("\"Hallo\""), "{t}");
    assert!(t.contains("(void)"), "{t}");
    // Einrueckung: <p> liegt tiefer als <div>.
    let zeile_div = t.lines().find(|l| l.contains("<div")).unwrap();
    let zeile_p = t.lines().find(|l| l.contains("<p>")).unwrap();
    let tiefe = |l: &str| l.len() - l.trim_start().len();
    assert!(tiefe(zeile_p) > tiefe(zeile_div), "<p> muss eingerueckt sein");
}

/// Die Reihenfolge in der Ausgabe ist die DOKUMENTREIHENFOLGE.
#[test]
fn test_baum_text_reihenfolge() {
    let d = parsen("<p>eins</p><p>zwei</p><p>drei</p>");
    let t = baum_text(&d);
    let e = t.find("eins").unwrap();
    let z = t.find("zwei").unwrap();
    let dr = t.find("drei").unwrap();
    assert!(e < z && z < dr, "Reihenfolge stimmt nicht:\n{t}");
}

// ===========================================================================
// 7. EINE ECHTE SEITE
// ===========================================================================

/// Eine echte, heruntergeladene Seite: die erste Webseite der Welt.
///
/// ===================================================================
/// WARUM GERADE DIESE
///
/// `info.cern.ch/hypertext/WWW/TheProject.html` ist reines HTML von 1991
/// — kein CSS, kein JavaScript, keine Tabellen. Und sie ist nach heutigen
/// Massstaeben KAPUTT: nicht geschlossene `<p>`, `<DT>`/`<DD>` ohne
/// Endtags, Grossschreibung, Attribute ohne Anfuehrungszeichen.
///
/// Damit ist sie zugleich die Pruefseite A aus `docs/browser-v1.md` und
/// ein echter Fehlererholungs-Test — und sie ist klein genug, um sie ins
/// Repository zu legen.
///
/// Geholt mit `tools/testseiten_holen.ps1`; Herkunft und Datum stehen in
/// `assets/testseiten/HERKUNFT.txt`. Von Hand geholt, nicht vom build.rs
/// — dasselbe Prinzip wie beim CA-Buendel.
#[test]
fn test_echte_seite_cern() {
    let html = include_str!("../../assets/testseiten/cern-theproject.html");
    let d = parsen(html);

    // (1) Kein Absturz — schon der Durchlauf ist der halbe Test.
    // (2) Der Baum ist nicht leer und nicht abgeschnitten.
    assert!(d.anzahl() > 50, "nur {} Knoten — da fehlt etwas", d.anzahl());
    assert!(!d.befund.abgeschnitten, "keine Grenze durfte greifen");

    // (3) Die Struktur ist erkannt.
    assert!(d.erstes("header").is_some() || d.erstes("body").is_some());
    assert!(d.erstes("title").is_some(), "<title> fehlt");
    assert!(d.erstes("h1").is_some(), "<h1> fehlt");

    // (4) Der Text ist da — Stichprobe auf den bekannten Inhalt.
    let text = d.text_von(crate::Dokument::WURZEL);
    assert!(
        text.contains("World Wide Web"),
        "der Haupttext fehlt"
    );
    assert!(text.contains("hypermedia"), "der Haupttext ist unvollstaendig");

    // (5) Die Links sind da und haben Ziele.
    let links: Vec<_> = d.alle_mit_tag("a").collect();
    assert!(links.len() > 10, "nur {} Links", links.len());
    let mit_ziel = links
        .iter()
        .filter(|id| d.knoten(**id).unwrap().attribut("href").is_some())
        .count();
    assert!(mit_ziel > 10, "nur {mit_ziel} Links mit href");

    // (6) Die Seite IST nach heutigen Massstaeben kaputt — und genau das
    //     soll der Parser aufgefangen haben.
    assert!(
        !d.befund.sauber(),
        "eine Seite von 1991 ohne Befund waere verdaechtig: {:?}",
        d.befund
    );
}

/// Dieselbe Seite durch die Baumausgabe — sie darf weder panicken noch
/// unbrauchbar lang werden.
#[test]
fn test_echte_seite_baumausgabe() {
    let html = include_str!("../../assets/testseiten/cern-theproject.html");
    let d = parsen(html);
    let t = baum_text(&d);
    assert!(t.lines().count() > 50);
    // Keine Zeile laeuft davon (Textknoten werden gekuerzt).
    for zeile in t.lines() {
        assert!(
            zeile.chars().count() < 200,
            "Zeile zu lang ({}): {}",
            zeile.chars().count(),
            &zeile[..zeile.len().min(80)]
        );
    }
}

/// DIE PRUEFSEITE B: ein echter Wikipedia-Artikel, 300 KiB.
///
/// ===================================================================
/// DER GEGENPOL ZU CERN
///
/// Alles, was 1991 fehlte: `<script>`, `<style>`, Tabellen, Bilder, eine
/// Infobox, ein Formular (die Suche), hunderte Links, tiefe
/// Verschachtelung. Das ist die Seite, an der `docs/browser-v1.md` §4 die
/// Abnahme festmacht — und schon der Parser muss sie aushalten.
///
/// **Die wichtigste Zusage hier: kein JavaScript- und kein CSS-Quelltext
/// im sichtbaren Text.** Das ist Kriterium 9 aus dem Zuschnitt und der
/// Fehler, der `news` in Serie 7 fast wertlos gemacht haette.
#[test]
fn test_echte_seite_wikipedia() {
    let html = include_str!("../../assets/testseiten/wikipedia-betriebssystem.html");
    let d = parsen(html);
    let text = d.text_von(crate::Dokument::WURZEL);

    // (1) Nicht abgeschnitten — die Standard-Grenzen muessen fuer eine
    //     echte Seite reichen. Waere das nicht so, waeren sie zu eng.
    assert!(
        !d.befund.abgeschnitten,
        "eine echte Seite darf keine Grenze reissen: {:?}",
        d.befund
    );
    assert!(d.anzahl() > 1000, "nur {} Knoten", d.anzahl());

    // (2) Der Haupttext ist da (Kriterium 2 aus browser-v1.md).
    assert!(text.contains("Betriebssystem"), "Haupttext fehlt");

    // (3) KEIN Skript- und KEIN Stil-Inhalt im sichtbaren Text.
    for verraeter in ["function(", "addEventListener", "RLQ.push", "mw.config"] {
        assert!(
            !text.contains(verraeter),
            "JavaScript im sichtbaren Text: {verraeter}"
        );
    }
    for verraeter in ["{background", "@media", "font-size:", "}.mw-"] {
        assert!(!text.contains(verraeter), "CSS im sichtbaren Text: {verraeter}");
    }

    // (4) Die Struktur ist erkannt.
    assert!(d.erstes("title").is_some(), "<title> fehlt");
    assert!(d.erstes("h1").is_some(), "<h1> fehlt");
    assert!(d.alle_mit_tag("h2").count() > 3, "zu wenige Zwischenueberschriften");
    assert!(d.alle_mit_tag("p").count() > 20, "zu wenige Absaetze");
    assert!(d.alle_mit_tag("li").count() > 20, "zu wenige Listenpunkte");

    // (5) Tabellen und Bilder — genau das, was CERN nicht hat.
    assert!(d.erstes("table").is_some(), "keine Tabelle gefunden");
    assert!(d.alle_mit_tag("tr").count() > 2, "zu wenige Tabellenzeilen");
    assert!(d.alle_mit_tag("img").count() > 0, "kein Bild gefunden");
    // Jedes <img> ist void — keins hat Kinder.
    for i in d.alle_mit_tag("img") {
        assert!(d.knoten(i).unwrap().kinder.is_empty(), "<img> hat Kinder");
    }

    // (6) Links mit Zielen (Kriterium 5).
    let mit_ziel = d
        .alle_mit_tag("a")
        .filter(|id| d.knoten(*id).unwrap().attribut("href").is_some())
        .count();
    assert!(mit_ziel > 100, "nur {mit_ziel} Links mit href");

    // (7) Die Tiefe bleibt im Rahmen — wichtig, weil das spaetere Layout
    //     den Baum durchlaeuft und der User-Stack 64 KiB hat.
    assert!(
        d.befund.tiefe < Grenzen::standard().max_tiefe,
        "Tiefe {} — zu nah an der Grenze",
        d.befund.tiefe
    );
}

/// Der Kontrollfall: die kleinste denkbare gueltige Seite.
///
/// Wenn selbst die schiefgeht, liegt es nicht an der Seite. Sie ist
/// sauber genug, dass der Befund fast leer sein muss.
#[test]
fn test_echte_seite_example_com() {
    let html = include_str!("../../assets/testseiten/example.com.html");
    let d = parsen(html);
    let text = d.text_von(crate::Dokument::WURZEL);
    assert!(text.contains("Example Domain"));
    assert!(d.erstes("h1").is_some());
    assert!(d.erstes("a").is_some());
    assert!(!d.befund.abgeschnitten);
    assert_eq!(d.befund.unerwartete_endtags, 0, "die Seite ist sauber");
}
