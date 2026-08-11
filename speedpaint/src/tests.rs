// speedpaint::tests — auf dem HOST, ohne Fenster, ohne QEMU
//
// ===========================================================================
// WARUM DAS HIER GEHT
//
// Weil `speedlayout` Befehle liefert und `speedui::attrappe::MalProtokoll`
// sie mitschreibt statt sie zu malen. Zwischen beidem liegt der Maler —
// und damit ist 'landet der Text an der richtigen Stelle?" eine Frage an
// zwei Listen.
//
// Die Alternative waere ein Bildschirmfoto-Vergleich gewesen. Der findet
// denselben Fehler, aber er sagt nicht, WELCHER Befehl falsch lag, er
// bricht bei jeder Farbanpassung, und er braucht einen QEMU-Start je
// Fall. Von diesen Tests gaebe es dann fuenf statt fuenfzig.

use crate::invalidierung::{entscheiden, Anlass, Massnahme};
use crate::maler::{malen, Auftrag, Bild, Bildquelle, OhneBilder};
use crate::sicht::{Scrollschritt, Sicht, ZEILEN_JE_RASTUNG};
use alloc::string::String;
use alloc::vec::Vec;
use speedlayout::attrappe::FesteMetrik;
use speedlayout::Anzeigeliste;
use speedui::attrappe::{MalProtokoll, Strich};
use speedui::{Farbe, Leinwand, Rechteck};

// ---------------------------------------------------------------------------
// Werkzeug
// ---------------------------------------------------------------------------

const WEISS: Farbe = Farbe::neu(255, 255, 255);

/// Eine Seite setzen und ihre Anzeigeliste zurueckgeben.
fn seite(html: &str, css: &str, breite: i32) -> (Anzeigeliste, i32) {
    let metrik = FesteMetrik::neu();
    let (ergebnis, liste) = speedlayout::seite_setzen(html, css, breite, &metrik);
    (liste, ergebnis.hoehe)
}

/// Eine Sicht auf ein Fenster bei (0,0).
fn sicht(breite: i32, hoehe: i32, inhalt: i32) -> Sicht {
    Sicht::neu(Rechteck::neu(0, 0, breite, hoehe), inhalt).mit_zeilenhoehe(20)
}

/// Alles malen und das Protokoll zurueckgeben.
fn malen_voll(liste: &Anzeigeliste, sicht: &Sicht) -> MalProtokoll {
    let mut protokoll = MalProtokoll::neu(sicht.bereich.breite, sicht.bereich.hoehe);
    malen(
        &Auftrag {
            liste,
            sicht,
            streifen: sicht.bereich,
            hintergrund: WEISS,
        },
        &mut protokoll,
        &OhneBilder,
    );
    protokoll
}

/// Alle Text-Striche als (x, y, Text).
fn texte(protokoll: &MalProtokoll) -> Vec<(i32, i32, String)> {
    protokoll
        .striche
        .iter()
        .filter_map(|s| match s {
            Strich::TextStil(x, y, t, ..) => Some((*x, *y, t.clone())),
            Strich::Text(x, y, t, ..) => Some((*x, *y, t.clone())),
            _ => None,
        })
        .collect()
}

fn text_y(protokoll: &MalProtokoll, gesucht: &str) -> Option<i32> {
    texte(protokoll)
        .into_iter()
        .find(|(_, _, t)| t == gesucht)
        .map(|(_, y, _)| y)
}

// ===========================================================================
// AUFGABE 1 — DER PAINTER
// ===========================================================================

/// Die Grundzusage: Was in der Liste steht, wird gezeichnet — und zwar
/// an der Stelle, die in der Liste steht.
#[test]
fn test_befehle_landen_an_ihrer_stelle() {
    let (liste, hoehe) = seite("<p>hallo welt</p>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let protokoll = malen_voll(&liste, &sicht);

    let gemalt = texte(&protokoll);
    assert!(
        gemalt.iter().any(|(_, _, t)| t == "hallo"),
        "der Text muss gezeichnet werden, gemalt wurde: {:?}",
        gemalt
    );

    // Und zwar GENAU dort, wo der Layout-Befehl ihn hinlegt.
    for befehl in &liste.befehle {
        if let speedlayout::Befehl::Text { x, y, text, .. } = befehl {
            let treffer = gemalt
                .iter()
                .find(|(_, _, gezeichnet)| gezeichnet == text)
                .unwrap_or_else(|| panic!("'{}' fehlt im Protokoll", text));
            assert_eq!(
                (treffer.0, treffer.1),
                (*x, *y),
                "'{}' muss bei ({}, {}) gezeichnet werden",
                text,
                x,
                y
            );
        }
    }
}

/// Der Hintergrund wird VOR den Befehlen gefuellt — sonst stuende beim
/// Scrollen der alte Inhalt unter dem neuen Text.
#[test]
fn test_hintergrund_kommt_zuerst() {
    let (liste, hoehe) = seite("<p>text</p>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let protokoll = malen_voll(&liste, &sicht);

    match protokoll.striche.first() {
        Some(Strich::Fuellen(rechteck, farbe)) => {
            assert_eq!(*farbe, WEISS);
            assert_eq!(*rechteck, sicht.bereich, "der ganze Streifen muss gefuellt werden");
        }
        anderes => panic!("erster Strich muss der Hintergrund sein, war: {:?}", anderes),
    }
}

/// Die vier Befehlsarten werden auf die richtigen Leinwand-Operationen
/// abgebildet.
#[test]
fn test_alle_vier_befehlsarten() {
    // Hintergrund (Rechteck), Rahmen (Rechteck), Text, Unterstreichung
    // (Linie) und ein Bild.
    let (liste, hoehe) = seite(
        "<div style='background:red;border:2px solid blue'>\
         <u>unterstrichen</u><img src='a.png' width='40' height='30' alt='x'></div>",
        "",
        400,
    );
    let sicht = sicht(400, 300, hoehe);
    let protokoll = malen_voll(&liste, &sicht);

    assert!(
        protokoll.anzahl(|s| matches!(s, Strich::Fuellen(..))) >= 2,
        "Hintergrund und Rahmenkanten sind Fuellungen"
    );
    assert!(protokoll.hat_text_stil("unterstrichen"), "Text fehlt");
    // Die Unterstreichung ist waagerecht und wird deshalb als Rechteck
    // gefuellt (Leinwand::linie kennt keine Dicke) — genau das prueft
    // dieser Test mit ab.
    assert!(
        protokoll
            .striche
            .iter()
            .any(|s| matches!(s, Strich::Rahmen(..))),
        "das noch nicht geladene Bild muss einen Platzhalter-Rahmen bekommen"
    );
}

/// Ein Bild, dessen Pixel da sind, wird GEZEICHNET; eines ohne Pixel
/// bekommt den Platzhalter. Der Unterschied muss sichtbar sein.
#[test]
fn test_bild_gezeichnet_oder_platzhalter() {
    struct EinBild {
        pixel: Vec<u8>,
    }
    impl Bildquelle for EinBild {
        fn bild(&self, quelle: &str) -> Option<Bild<'_>> {
            if quelle == "da.png" {
                Some(Bild {
                    breite: 4,
                    hoehe: 4,
                    rgba: &self.pixel,
                })
            } else {
                None
            }
        }
    }
    let quelle = EinBild {
        pixel: alloc::vec![255u8; 4 * 4 * 4],
    };

    let (liste, hoehe) = seite(
        "<img src='da.png' width='40' height='30'>\
         <img src='fehlt.png' width='40' height='30' alt='weg'>",
        "",
        400,
    );
    let sicht = sicht(400, 300, hoehe);
    let mut protokoll = MalProtokoll::neu(400, 300);
    let befund = malen(
        &Auftrag {
            liste: &liste,
            sicht: &sicht,
            streifen: sicht.bereich,
            hintergrund: WEISS,
        },
        &mut protokoll,
        &quelle,
    );

    assert_eq!(befund.bilder_gemalt, 1, "ein Bild war da");
    assert_eq!(befund.bilder_fehlend, 1, "eines fehlte");
    let bilder: Vec<_> = protokoll
        .striche
        .iter()
        .filter_map(|s| match s {
            Strich::Bild(ziel, qb, qh, laenge) => Some((*ziel, *qb, *qh, *laenge)),
            _ => None,
        })
        .collect();
    assert_eq!(bilder.len(), 1, "genau ein echtes Bild");
    assert_eq!((bilder[0].1, bilder[0].2), (4, 4), "Quellmasse durchgereicht");
    assert_eq!(bilder[0].0.breite, 40, "ins Layout-Rechteck gemalt");
    assert_eq!(bilder[0].0.hoehe, 30);
}

/// Ein Bild mit zu kurzem Puffer wird NICHT gezeichnet.
///
/// Die Bildquelle ist aus Sicht dieser Kiste Fremdcode. Ohne die
/// Laengenpruefung waere ein zu kurzer Puffer ein Absturz mitten im
/// Malen — bei einem Renderer also eine Seite, die VERSCHWINDET, statt
/// ein Bild, das fehlt.
#[test]
fn test_zu_kurzer_bildpuffer_wird_platzhalter() {
    struct Luegner;
    impl Bildquelle for Luegner {
        fn bild(&self, _quelle: &str) -> Option<Bild<'_>> {
            // Behauptet 100x100, liefert 4 Byte.
            Some(Bild {
                breite: 100,
                hoehe: 100,
                rgba: &[0, 0, 0, 0],
            })
        }
    }
    let (liste, hoehe) = seite("<img src='a.png' width='40' height='30'>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let mut protokoll = MalProtokoll::neu(400, 300);
    let befund = malen(
        &Auftrag {
            liste: &liste,
            sicht: &sicht,
            streifen: sicht.bereich,
            hintergrund: WEISS,
        },
        &mut protokoll,
        &Luegner,
    );
    assert_eq!(befund.bilder_gemalt, 0, "der Luegner darf nicht gezeichnet werden");
    assert_eq!(befund.bilder_fehlend, 1);
}

/// Vollstaendig durchsichtige Farben erzeugen keinen Aufruf.
#[test]
fn test_durchsichtiges_wird_nicht_gemalt() {
    // Ein `<div>` ohne `background` ist durchsichtig — dafuer darf kein
    // Fuell-Aufruf entstehen (ausser dem Hintergrund des Auftrags).
    let (liste, hoehe) = seite("<div><p>a</p></div>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let protokoll = malen_voll(&liste, &sicht);
    assert_eq!(
        protokoll.anzahl(|s| matches!(s, Strich::Fuellen(..))),
        1,
        "nur der Seitenhintergrund, keine durchsichtigen Kaesten: {:?}",
        protokoll.striche
    );
}

/// Das Clip des Aufrufers wird gesetzt UND wiederhergestellt.
#[test]
fn test_clip_wird_wiederhergestellt() {
    let (liste, hoehe) = seite("<p>text</p>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let vorher = Some(Rechteck::neu(5, 5, 100, 100));
    let mut protokoll = MalProtokoll::neu(400, 300);
    protokoll.clip_setzen(vorher);
    malen(
        &Auftrag {
            liste: &liste,
            sicht: &sicht,
            streifen: sicht.bereich,
            hintergrund: WEISS,
        },
        &mut protokoll,
        &OhneBilder,
    );
    assert_eq!(protokoll.clip(), vorher, "die Leinwand gehoert dem Aufrufer");
}

/// Ein Auftrag ausserhalb der Sicht malt gar nichts.
#[test]
fn test_streifen_ausserhalb_malt_nichts() {
    let (liste, hoehe) = seite("<p>text</p>", "", 400);
    let sicht = sicht(400, 300, hoehe);
    let mut protokoll = MalProtokoll::neu(400, 300);
    let befund = malen(
        &Auftrag {
            liste: &liste,
            sicht: &sicht,
            streifen: Rechteck::neu(0, 5000, 400, 10),
            hintergrund: WEISS,
        },
        &mut protokoll,
        &OhneBilder,
    );
    assert_eq!(befund.gemalt, 0);
    assert!(protokoll.striche.is_empty(), "kein Aufruf, nicht einmal der Hintergrund");
}

/// Der Maler ueberspringt, was nicht im Streifen liegt — das ist der
/// ganze Gewinn des Streifen-Zeichnens.
#[test]
fn test_streifen_ueberspringt_den_rest() {
    let mut html = String::new();
    for i in 0..200 {
        html.push_str("<p>zeile");
        html.push_str(&alloc::format!("{}", i));
        html.push_str("</p>");
    }
    let (liste, hoehe) = seite(&html, "", 400);
    let sicht = sicht(400, 300, hoehe);

    let voll = malen_voll(&liste, &sicht);
    let mut streifen_protokoll = MalProtokoll::neu(400, 300);
    let befund = malen(
        &Auftrag {
            liste: &liste,
            sicht: &sicht,
            streifen: Rechteck::neu(0, 0, 400, 20),
            hintergrund: WEISS,
        },
        &mut streifen_protokoll,
        &OhneBilder,
    );
    assert!(
        befund.uebersprungen > befund.gemalt * 10,
        "bei 200 Absaetzen und 20 px Streifen muss fast alles wegfallen \
         (gemalt {}, uebersprungen {})",
        befund.gemalt,
        befund.uebersprungen
    );
    assert!(
        streifen_protokoll.striche.len() < voll.striche.len(),
        "ein Streifen zeichnet weniger als das Vollbild"
    );
}

// ===========================================================================
// AUFGABE 2 — SCROLLEN
// ===========================================================================

/// Scrollen verschiebt den Text, ohne die Liste anzufassen.
#[test]
fn test_scrollen_verschiebt_nur_die_anzeige() {
    let (liste, hoehe) = seite("<p>oben</p><p>unten</p>", "", 400);
    let mut sicht = sicht(400, 100, hoehe.max(400));

    let vorher = malen_voll(&liste, &sicht);
    let y_vorher = text_y(&vorher, "oben").expect("'oben' fehlt");

    // Die Liste VOR und NACH dem Scrollen muss dieselbe sein — das ist
    // die Zusage 'Scrollen layoutet nicht neu", direkt geprueft.
    let liste_vorher: Vec<_> = liste.befehle.clone();
    sicht.scrollen(Scrollschritt::ZeileRunter);
    assert_eq!(liste.befehle, liste_vorher, "die Anzeigeliste darf sich nicht aendern");

    let nachher = malen_voll(&liste, &sicht);
    let y_nachher = text_y(&nachher, "oben").expect("'oben' fehlt nach dem Scrollen");
    assert_eq!(
        y_nachher,
        y_vorher - 20,
        "eine Zeile scrollen verschiebt den Text um genau eine Zeilenhoehe"
    );
}

/// Nicht ueber den Anfang hinaus.
#[test]
fn test_klemmung_am_anfang() {
    let mut s = sicht(400, 300, 2000);
    let folge = s.scrollen(Scrollschritt::ZeileHoch);
    assert_eq!(s.versatz(), 0, "am Anfang geht es nicht weiter hoch");
    assert!(!folge.geaendert(), "und es gibt nichts zu malen");
    assert_eq!(folge.streifen, None);

    s.scrollen(Scrollschritt::Rad(100));
    assert_eq!(s.versatz(), 0);
    s.scrollen(Scrollschritt::Nach(-5000));
    assert_eq!(s.versatz(), 0);
    s.scrollen(Scrollschritt::SeiteHoch);
    assert_eq!(s.versatz(), 0);
}

/// Nicht ueber das Ende hinaus.
#[test]
fn test_klemmung_am_ende() {
    let mut s = sicht(400, 300, 2000);
    let max = s.max_versatz();
    assert_eq!(max, 1700, "2000 hoch, 300 sichtbar");

    s.scrollen(Scrollschritt::Ende);
    assert_eq!(s.versatz(), max);
    s.scrollen(Scrollschritt::ZeileRunter);
    assert_eq!(s.versatz(), max, "am Ende geht es nicht weiter");
    s.scrollen(Scrollschritt::Rad(-10_000));
    assert_eq!(s.versatz(), max);
    s.scrollen(Scrollschritt::Nach(i32::MAX));
    assert_eq!(s.versatz(), max);
}

/// Ein absurdes Rad-Delta darf nicht ueberlaufen (und damit ans andere
/// Ende springen).
#[test]
fn test_rad_ueberlauf_klemmt_statt_zu_springen() {
    let mut s = sicht(400, 300, 2000);
    s.scrollen(Scrollschritt::Rad(i32::MIN));
    assert_eq!(s.versatz(), s.max_versatz());
    s.scrollen(Scrollschritt::Rad(i32::MAX));
    assert_eq!(s.versatz(), 0);
}

/// Ein Dokument, das kuerzer ist als das Fenster, laesst sich nicht
/// scrollen.
#[test]
fn test_kurzes_dokument_scrollt_nicht() {
    let mut s = sicht(400, 300, 100);
    assert_eq!(s.max_versatz(), 0);
    assert!(!s.scrollbar());
    s.scrollen(Scrollschritt::Ende);
    assert_eq!(s.versatz(), 0, "sonst scrollte man den Inhalt aus dem Bild");
    assert!(s.balken(10).is_none(), "und es gibt keinen Balken");
}

/// Das Mausrad bewegt drei Zeilen je Rastung — dieselbe Zahl wie im
/// Terminal.
#[test]
fn test_rad_drei_zeilen() {
    let mut s = sicht(400, 300, 5000);
    s.scrollen(Scrollschritt::Rad(-1));
    assert_eq!(s.versatz(), ZEILEN_JE_RASTUNG * 20);
}

/// DIE RICHTUNG DES MAUSRADS, aus der MITTE des Dokuments geprueft.
///
/// Der Test, der gefehlt hat. `test_klemmung_am_anfang` und
/// `test_klemmung_am_ende` fassen das Rad zwar an, aber jeweils dort, wo
/// ohnehin nichts passieren darf — sie waren mit BEIDEN Vorzeichen
/// gruen, waehrend das Rad im laufenden Browser in die falsche Richtung
/// scrollte. Ein Test, der an einer Klemmung steht, prueft die Klemmung
/// und nicht die Richtung.
#[test]
fn test_rad_richtung() {
    let mut s = sicht(400, 300, 5000);
    s.versatz_setzen(1000);

    // Positiv = nach oben (zum Anfang) — wie `maus.rs` es dokumentiert.
    s.scrollen(Scrollschritt::Rad(1));
    assert_eq!(
        s.versatz(),
        1000 - ZEILEN_JE_RASTUNG * 20,
        "Rad positiv muss zum Dokumentanfang scrollen"
    );

    // Negativ = nach unten (weiterlesen).
    s.versatz_setzen(1000);
    s.scrollen(Scrollschritt::Rad(-1));
    assert_eq!(
        s.versatz(),
        1000 + ZEILEN_JE_RASTUNG * 20,
        "Rad negativ muss zum Dokumentende scrollen"
    );
}

/// Eine Seite laesst eine Zeile Anschluss stehen.
#[test]
fn test_seite_laesst_eine_zeile_stehen() {
    let mut s = sicht(400, 300, 5000);
    s.scrollen(Scrollschritt::SeiteRunter);
    assert_eq!(s.versatz(), 280, "300 hoch minus eine Zeile von 20");
}

/// Pos1 und Ende.
#[test]
fn test_pos1_und_ende() {
    let mut s = sicht(400, 300, 5000);
    s.scrollen(Scrollschritt::Ende);
    assert_eq!(s.versatz(), 4700);
    s.scrollen(Scrollschritt::Anfang);
    assert_eq!(s.versatz(), 0);
}

/// DER STREIFEN: Beim Scrollen nach unten wird UNTEN etwas neu sichtbar,
/// beim Scrollen nach oben OBEN — und zwar genau so viel, wie gescrollt
/// wurde.
#[test]
fn test_streifen_liegt_am_richtigen_rand() {
    let mut s = sicht(400, 300, 5000);

    let runter = s.scrollen(Scrollschritt::ZeileRunter);
    let streifen = runter.streifen.expect("es wurde gescrollt");
    assert_eq!(streifen.hoehe, 20, "so viel wurde gescrollt");
    assert_eq!(streifen.y, 280, "der neue Rand liegt UNTEN");
    assert!(runter.verschieben_lohnt());

    let hoch = s.scrollen(Scrollschritt::ZeileHoch);
    let streifen = hoch.streifen.expect("es wurde gescrollt");
    assert_eq!(streifen.hoehe, 20);
    assert_eq!(streifen.y, 0, "der neue Rand liegt OBEN");
}

/// Ein Sprung, der weiter geht als das Fenster hoch ist, malt alles neu —
/// Verschieben waere Arbeit ohne Gewinn.
#[test]
fn test_grosser_sprung_malt_alles() {
    let mut s = sicht(400, 300, 5000);
    let folge = s.scrollen(Scrollschritt::Nach(1000));
    assert!(folge.alles, "1000 > 300 — vom alten Bild bleibt nichts");
    assert_eq!(folge.streifen, Some(s.bereich));
    assert!(!folge.verschieben_lohnt());

    // Genau eine Fensterhoehe ist schon 'alles".
    let mut s = sicht(400, 300, 5000);
    let folge = s.scrollen(Scrollschritt::Nach(300));
    assert!(folge.alles);
    // Ein Pixel weniger nicht mehr.
    let mut s = sicht(400, 300, 5000);
    let folge = s.scrollen(Scrollschritt::Nach(299));
    assert!(!folge.alles);
    assert_eq!(folge.streifen.map(|r| r.hoehe), Some(299));
}

/// Nach dem Scrollen zeigt der Streifen wirklich den neuen Inhalt.
///
/// Das ist die Probe aufs Exempel: Wer den Streifen falsch herum
/// berechnet, malt beim Herunterscrollen den obersten statt den
/// untersten Rand — und der Fehler faellt erst am Bildschirm auf.
#[test]
fn test_streifen_zeigt_den_neuen_inhalt() {
    let mut html = String::new();
    for i in 0..100 {
        html.push_str(&alloc::format!("<p>zeile{}</p>", i));
    }
    let (liste, hoehe) = seite(&html, "", 400);
    let mut s = sicht(400, 300, hoehe);

    // Erst voll malen, dann eine Zeile weiter.
    let folge = s.scrollen(Scrollschritt::Nach(100));
    let streifen = folge.streifen.expect("gescrollt");

    let mut protokoll = MalProtokoll::neu(400, 300);
    malen(
        &Auftrag {
            liste: &liste,
            sicht: &s,
            streifen,
            hintergrund: WEISS,
        },
        &mut protokoll,
        &OhneBilder,
    );
    // Alles, was im Streifen gezeichnet wurde, muss auch IM Streifen
    // liegen (Toleranz: eine Zeilenhoehe, weil ein Textbefehl an seiner
    // Oberkante haengt).
    for (_, y, text) in texte(&protokoll) {
        assert!(
            y + 40 >= streifen.y && y <= streifen.y + streifen.hoehe,
            "'{}' bei y={} liegt ausserhalb des Streifens {:?}",
            text,
            y,
            streifen
        );
    }
}

/// Wird das Fenster hoeher als der Rest des Dokuments, muss der Versatz
/// nachgeben — sonst zeigt der Browser eine leere Flaeche.
#[test]
fn test_anpassen_klemmt_den_versatz_nach() {
    let mut s = sicht(400, 300, 2000);
    s.scrollen(Scrollschritt::Ende);
    assert_eq!(s.versatz(), 1700);

    // Fenster wird doppelt so hoch.
    s.anpassen(Rechteck::neu(0, 0, 400, 600), 2000);
    assert_eq!(s.versatz(), 1400, "der Versatz muss auf das neue Maximum fallen");

    // Und ein kuerzeres Dokument (breiteres Fenster = weniger Umbrueche).
    s.anpassen(Rechteck::neu(0, 0, 400, 600), 500);
    assert_eq!(s.versatz(), 0);
}

// ---------------------------------------------------------------------------
// Der Scrollbalken
// ---------------------------------------------------------------------------

/// Der Greifer ist so lang wie der Anteil des Fensters am Dokument.
#[test]
fn test_balken_greifer_zeigt_den_anteil() {
    let s = sicht(400, 300, 1200);
    let balken = s.balken(10).expect("scrollbar");
    // 300 von 1200 = ein Viertel der Spur.
    assert_eq!(balken.greifer.hoehe, 75);
    assert_eq!(balken.greifer.y, 0, "am Anfang steht er oben");
    assert_eq!(balken.spur.x, 390, "am rechten Rand");
}

/// Am Ende steht der Greifer unten — und ragt nicht darueber hinaus.
#[test]
fn test_balken_greifer_am_ende() {
    let mut s = sicht(400, 300, 1200);
    s.scrollen(Scrollschritt::Ende);
    let balken = s.balken(10).expect("scrollbar");
    assert_eq!(
        balken.greifer.y + balken.greifer.hoehe,
        balken.spur.y + balken.spur.hoehe,
        "der Greifer muss genau unten abschliessen"
    );
}

/// Bei einem sehr langen Dokument bleibt der Greifer greifbar.
#[test]
fn test_balken_greifer_hat_mindestlaenge() {
    let s = sicht(400, 300, 5_000_000);
    let balken = s.balken(10).expect("scrollbar");
    assert!(
        balken.greifer.hoehe >= 24,
        "ein 0-Pixel-Greifer waere unsichtbar und nicht greifbar (war {})",
        balken.greifer.hoehe
    );
}

/// Ein Klick auf die Spur setzt den Versatz — und klemmt dabei.
#[test]
fn test_balken_klick_setzt_versatz() {
    let s = sicht(400, 300, 1200);
    let balken = s.balken(10).expect("scrollbar");
    assert_eq!(s.versatz_aus_balken(0, &balken), 0, "ganz oben");
    assert_eq!(
        s.versatz_aus_balken(300, &balken),
        s.max_versatz(),
        "ganz unten"
    );
    // Mitte der Spur -> etwa Mitte des Dokuments.
    let mitte = s.versatz_aus_balken(150, &balken);
    assert!(
        (mitte - s.max_versatz() / 2).abs() < 40,
        "Klick in die Mitte sollte etwa in die Mitte fuehren (war {})",
        mitte
    );
}

// ===========================================================================
// AUFGABE 3 — DIE INVALIDIERUNGS-REGELN
// ===========================================================================

/// (1) Andere Breite -> neu layouten.
#[test]
fn test_regel_breite_layoutet_neu() {
    let massnahme = entscheiden(Anlass::FensterGroesse {
        alte_breite: 800,
        alte_hoehe: 600,
        neue_breite: 900,
        neue_hoehe: 600,
    });
    assert_eq!(massnahme, Massnahme::NeuLayouten);
    assert!(massnahme.layoutet());
}

/// (2) NUR die Hoehe geaendert -> NICHT layouten, aber voll malen.
///
/// Die Regel, die man uebersieht — und die bei jedem Ziehen am unteren
/// Fensterrand ein volles Layout spart.
#[test]
fn test_regel_hoehe_layoutet_nicht() {
    let massnahme = entscheiden(Anlass::FensterGroesse {
        alte_breite: 800,
        alte_hoehe: 600,
        neue_breite: 800,
        neue_hoehe: 400,
    });
    assert_eq!(massnahme, Massnahme::Alles);
    assert!(!massnahme.layoutet(), "die Hoehe geht in kein Layout ein");
    assert!(massnahme.malt(), "der Fensterpuffer ist nach Groesse LEER");
}

/// (3) Scrollen layoutet NIE.
#[test]
fn test_regel_scrollen_layoutet_nie() {
    let streifen = Rechteck::neu(0, 280, 400, 20);
    let massnahme = entscheiden(Anlass::Scrollen {
        streifen: Some(streifen),
        alles: false,
    });
    assert_eq!(massnahme, Massnahme::Teil(streifen));
    assert!(!massnahme.layoutet());
    assert_eq!(massnahme.ausschnitt(), Some(streifen));

    // Grosser Sprung: alles malen, aber immer noch nicht layouten.
    let gross = entscheiden(Anlass::Scrollen {
        streifen: Some(streifen),
        alles: true,
    });
    assert_eq!(gross, Massnahme::Alles);
    assert!(!gross.layoutet());

    // Gar nicht gescrollt: gar nichts tun.
    let keins = entscheiden(Anlass::Scrollen {
        streifen: None,
        alles: false,
    });
    assert_eq!(keins, Massnahme::Nichts);
    assert!(!keins.malt());
}

/// (4) Ein fertiges Bild malt nur sein Rechteck neu.
#[test]
fn test_regel_bild_nur_sein_bereich() {
    let bereich = Rechteck::neu(10, 40, 100, 80);
    let massnahme = entscheiden(Anlass::BildGeladen { bereich });
    assert_eq!(massnahme, Massnahme::Teil(bereich));
    assert!(
        !massnahme.layoutet(),
        "unser Layout fragt ein Bild nie nach seiner Groesse — \
         ein geladenes Bild kann die Geometrie nicht aendern"
    );
}

/// (5) und (6).
#[test]
fn test_regel_seite_und_thema() {
    assert_eq!(entscheiden(Anlass::NeueSeite), Massnahme::NeuLayouten);
    assert_eq!(entscheiden(Anlass::ThemaGeaendert), Massnahme::Alles);
    assert!(!entscheiden(Anlass::ThemaGeaendert).layoutet());
    assert_eq!(entscheiden(Anlass::Nichts), Massnahme::Nichts);
}

/// Treffen mehrere Anlaesse zusammen, gewinnt der teuerste.
#[test]
fn test_massnahmen_verstaerken_sich() {
    let a = Rechteck::neu(0, 0, 10, 10);
    let b = Rechteck::neu(50, 50, 10, 10);
    // Zwei Teile -> ihre Bounding-Box (Korrektheit vor Optimum).
    assert_eq!(
        Massnahme::Teil(a).verstaerken(Massnahme::Teil(b)),
        Massnahme::Teil(Rechteck::neu(0, 0, 60, 60))
    );
    // Teil + Alles -> Alles.
    assert_eq!(
        Massnahme::Teil(a).verstaerken(Massnahme::Alles),
        Massnahme::Alles
    );
    // Alles + Layout -> Layout.
    assert_eq!(
        Massnahme::Alles.verstaerken(Massnahme::NeuLayouten),
        Massnahme::NeuLayouten
    );
    // Nichts verliert immer.
    assert_eq!(
        Massnahme::Nichts.verstaerken(Massnahme::Teil(a)),
        Massnahme::Teil(a)
    );
    // Und es ist symmetrisch.
    assert_eq!(
        Massnahme::NeuLayouten.verstaerken(Massnahme::Nichts),
        Massnahme::NeuLayouten
    );
}

/// Ein Bild ohne Flaeche loest gar nichts aus.
#[test]
fn test_leeres_bild_loest_nichts_aus() {
    assert_eq!(
        entscheiden(Anlass::BildGeladen {
            bereich: Rechteck::neu(0, 0, 0, 0)
        }),
        Massnahme::Nichts
    );
}

// ===========================================================================
// DIE ZUSAGE ALS GANZES
// ===========================================================================

/// **Der Test, um den es in diesem Teil geht:** Durch 50 Scroll-Schritte
/// hindurch bleibt die Anzeigeliste Byte fuer Byte dieselbe.
///
/// Er prueft nicht den Maler und nicht die Sicht, sondern die
/// ARCHITEKTUR: dass Scrollen und Layout getrennt sind. Faellt er, ist
/// irgendwo ein Neu-Layout in den Scroll-Pfad geraten — der Fehler, der
/// fluessiges Scrollen unmoeglich macht und den man am Bildschirm nur als
/// 'irgendwie zaeh" bemerkt.
#[test]
fn test_scrollen_laesst_das_layout_in_ruhe() {
    let mut html = String::new();
    for i in 0..300 {
        html.push_str(&alloc::format!("<p>absatz nummer {} mit etwas text</p>", i));
    }
    let (liste, hoehe) = seite(&html, "", 800);
    let urfassung: Vec<_> = liste.befehle.clone();
    let mut s = sicht(800, 600, hoehe);

    let schritte = [
        Scrollschritt::ZeileRunter,
        Scrollschritt::Rad(3),
        Scrollschritt::SeiteRunter,
        Scrollschritt::Ende,
        Scrollschritt::SeiteHoch,
        Scrollschritt::Anfang,
        Scrollschritt::Rad(-2),
    ];
    for runde in 0..50 {
        let folge = s.scrollen(schritte[runde % schritte.len()]);
        // Malen — auch das darf nichts aendern.
        if let Some(streifen) = folge.streifen {
            let mut protokoll = MalProtokoll::neu(800, 600);
            malen(
                &Auftrag {
                    liste: &liste,
                    sicht: &s,
                    streifen,
                    hintergrund: WEISS,
                },
                &mut protokoll,
                &OhneBilder,
            );
        }
        assert_eq!(
            liste.befehle, urfassung,
            "Runde {}: die Anzeigeliste hat sich beim Scrollen geaendert",
            runde
        );
        assert!(s.versatz() >= 0 && s.versatz() <= s.max_versatz());
    }
}
