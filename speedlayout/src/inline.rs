// speedlayout::inline — Zeilen bauen. Das Herzstueck.
//
// ===========================================================================
// DAS PROBLEM
//
// Block-Layout ist Stapeln: ein Kasten unter den anderen, fertig.
// Inline-Layout ist etwas anderes — es muss einen STROM aus Woertern,
// Bildern und verschachtelten Auszeichnungen in ZEILEN fuellen, die alle
// eine gemeinsame Grundlinie haben und deren Hoehe erst feststeht, wenn
// man weiss, was alles darin gelandet ist.
//
//     <p>Ein <b>fetter</b> Text mit <img src=x> und mehr Text</p>
//
// Der `<b>` ist KEIN Kasten mit Geometrie: Er faerbt und fettet ein Stueck
// des Stroms, und wenn mitten in ihm umgebrochen wird, steht sein Anfang
// auf Zeile eins und sein Ende auf Zeile zwei.
//
// ===========================================================================
// DIE DREI SCHRITTE
//
//   1. EINSAMMELN  Den Inline-Teilbaum zu einem flachen Strom aus
//                  `Stueck`en machen. Jedes Stueck traegt seinen Stil mit.
//   2. FUELLEN     Stuecke in Zeilen legen, an Wortgrenzen umbrechen.
//   3. AUSRICHTEN  Je Zeile die Grundlinie bestimmen und alles daran
//                  aufhaengen.
//
// Schritt 1 ist der, der die Verschachtelung aufloest — danach ist das
// Problem eindimensional, und genau deshalb ist es ueberhaupt loesbar.
//
// ===========================================================================
// LEERRAUM
//
// Die Faltung (mehrere Leerzeichen -> eins, Zeilenumbruch -> Leerzeichen)
// ist schon passiert (`kasten::leerraum_falten`). Hier bleiben zwei
// Regeln:
//
//   * Ein Leerzeichen am ZEILENANFANG faellt weg. Sonst ruecken alle
//     Zeilen nach einem Umbruch um ein Zeichen ein.
//   * Ein Leerzeichen am ZEILENENDE zaehlt nicht zur Zeilenbreite. Sonst
//     bricht eine Zeile um, die genau passen wuerde.

use crate::kasten::{Kasten, KastenArt, Rechteck};
use crate::{px, schrift_px, Befund, Grenzen, Metrik};
use alloc::string::String;
use alloc::vec::Vec;
use speedcss::stil::{Ausrichtung, Vertikal};
use speedcss::{Laenge, Stil};
use speedhtml::KnotenId;

/// Ein Stueck des Inline-Stroms.
#[derive(Debug, Clone)]
enum Inhalt {
    /// Ein Wort ohne Leerzeichen.
    Wort(String),
    /// Genau EIN Leerzeichen (zusammengefaltet).
    Leerzeichen,
    /// Ein erzwungener Umbruch (`<br>` oder `\n` in `<pre>`).
    Umbruch,
    /// Ein Bild oder ein `inline-block` — etwas mit fester Groesse.
    ///
    /// IN EINER BOX, und das ist keine Kosmetik: Ohne sie waere JEDE
    /// Variante dieses Enums so gross wie ein ganzer `Kasten` (336 Byte),
    /// auch ein einzelnes Leerzeichen. Ein Absatz mit tausend Woertern
    /// belegte dann 336 KiB statt 24 — auf einem 12-MiB-Prozess-Heap ist
    /// das der Unterschied zwischen „laeuft" und „laeuft nicht".
    Kasten(alloc::boxed::Box<Kasten>),
}

#[derive(Debug, Clone)]
struct Stueck {
    inhalt: Inhalt,
    stil: Stil,
    knoten: Option<KnotenId>,
}

/// Setzt die Inline-Kinder eines Block-Containers in Zeilen.
///
/// `inhalt_x`/`inhalt_y` ist die linke obere Ecke der Inhalts-Box,
/// `breite` ihre Breite. Liefert die GESAMTHOEHE aller Zeilen und ersetzt
/// `kinder` durch `KastenArt::Zeile`-Kaesten.
pub(crate) fn zeilen_setzen(
    kinder: &mut Vec<Kasten>,
    inhalt_x: i32,
    inhalt_y: i32,
    breite: i32,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
    befund: &mut Befund,
) -> i32 {
    // --- 1. EINSAMMELN ---
    let mut stuecke = Vec::new();
    for kind in kinder.iter() {
        einsammeln(kind, &mut stuecke);
    }
    if stuecke.is_empty() {
        kinder.clear();
        return 0;
    }

    // --- 2. FUELLEN + 3. AUSRICHTEN ---
    let mut zeilen: Vec<Kasten> = Vec::new();
    let mut laufend: Vec<(Stueck, i32)> = Vec::new(); // Stueck + gemessene Breite
    let mut laufende_breite = 0i32;
    let mut y = inhalt_y;

    // Die Breite, in die umgebrochen wird. Bei 0 oder negativ wuerde
    // jedes Wort eine eigene Zeile bekommen — dann lieber gar nicht
    // umbrechen und ueberlaufen lassen (Aufgabe 4).
    let umbruch_breite = if breite > 0 { breite } else { i32::MAX };

    let mut i = 0usize;
    while i < stuecke.len() {
        if zeilen.len() >= grenzen.max_zeilen {
            befund.abgeschnitten = true;
            break;
        }
        let stueck = stuecke[i].clone();
        let groesse = schrift_px(&stueck.stil, metrik);

        match &stueck.inhalt {
            Inhalt::Umbruch => {
                laufend.push((stueck, 0));
                y += zeile_abschliessen(
                    &mut laufend,
                    &mut zeilen,
                    inhalt_x,
                    y,
                    breite,
                    metrik,
                    befund,
                );
                laufende_breite = 0;
                i += 1;
            }
            Inhalt::Leerzeichen => {
                // Am Zeilenanfang faellt es weg.
                if laufend.is_empty() {
                    i += 1;
                    continue;
                }
                let w = metrik.text_breite(" ", groesse, stueck.stil.fett, stueck.stil.kursiv);
                laufend.push((stueck, w));
                laufende_breite += w;
                i += 1;
            }
            Inhalt::Wort(wort) => {
                let w = metrik.text_breite(wort, groesse, stueck.stil.fett, stueck.stil.kursiv);
                if laufende_breite + w <= umbruch_breite || laufend.is_empty() {
                    // Passt — oder die Zeile ist leer, dann MUSS es hinein
                    // (sonst dreht sich die Schleife ewig).
                    if laufend.is_empty() && w > umbruch_breite {
                        // Ein einzelnes Wort, das breiter ist als die
                        // Zeile: HART TRENNEN. Eine lange URL ist der
                        // Normalfall dafuer, nicht die Ausnahme.
                        let (kopf, rest) = hart_trennen(wort, umbruch_breite, &stueck, metrik);
                        if !rest.is_empty() {
                            // Den Rest als neues Stueck einschieben.
                            let mut rest_stueck = stueck.clone();
                            rest_stueck.inhalt = Inhalt::Wort(rest);
                            stuecke.insert(i + 1, rest_stueck);
                            let kopf_breite = metrik.text_breite(
                                &kopf,
                                groesse,
                                stueck.stil.fett,
                                stueck.stil.kursiv,
                            );
                            let mut kopf_stueck = stueck.clone();
                            kopf_stueck.inhalt = Inhalt::Wort(kopf);
                            laufend.push((kopf_stueck, kopf_breite));
                            laufende_breite += kopf_breite;
                            befund.ueberlaeufe += 1;
                            i += 1;
                            continue;
                        }
                    }
                    laufend.push((stueck, w));
                    laufende_breite += w;
                    i += 1;
                } else {
                    // Passt nicht: Zeile abschliessen und dasselbe Stueck
                    // noch einmal versuchen (i wird NICHT erhoeht).
                    y += zeile_abschliessen(
                        &mut laufend,
                        &mut zeilen,
                        inhalt_x,
                        y,
                        breite,
                        metrik,
                        befund,
                    );
                    laufende_breite = 0;
                }
            }
            Inhalt::Kasten(k) => {
                let w = k.masse.margin_box().breite;
                if laufende_breite + w <= umbruch_breite || laufend.is_empty() {
                    laufend.push((stueck, w));
                    laufende_breite += w;
                    i += 1;
                } else {
                    y += zeile_abschliessen(
                        &mut laufend,
                        &mut zeilen,
                        inhalt_x,
                        y,
                        breite,
                        metrik,
                        befund,
                    );
                    laufende_breite = 0;
                }
            }
        }
    }
    // Die letzte, angefangene Zeile.
    if !laufend.is_empty() {
        y += zeile_abschliessen(
            &mut laufend,
            &mut zeilen,
            inhalt_x,
            y,
            breite,
            metrik,
            befund,
        );
    }

    befund.zeilen += zeilen.len();
    *kinder = zeilen;
    y - inhalt_y
}

/// Den Inline-Teilbaum zu einem flachen Strom machen.
///
/// DIE STELLE, AN DER DIE VERSCHACHTELUNG VERSCHWINDET. Ein `<b>` mit
/// drei Woertern darin wird zu drei Woertern MIT dem Stil des `<b>` —
/// danach ist das Problem eindimensional.
///
/// Der Preis: Ein Hintergrund oder Rahmen auf einem Inline-Element geht
/// verloren (er muesste je Zeilenstueck gemalt werden). Das steht so in
/// docs/browser-v1.md und ist bei Fliesstext selten sichtbar.
fn einsammeln(kasten: &Kasten, aus: &mut Vec<Stueck>) {
    match &kasten.art {
        KastenArt::Text(text) | KastenArt::Marke(text) => {
            // In WOERTER zerlegen. Leerzeichen werden eigene Stuecke,
            // damit der Umbruch sie am Zeilenende wegwerfen kann.
            let mut wort = String::new();
            for c in text.chars() {
                if c == '\n' {
                    // Nur in `<pre>` uebrig (sonst schon gefaltet).
                    if !wort.is_empty() {
                        aus.push(Stueck {
                            inhalt: Inhalt::Wort(core::mem::take(&mut wort)),
                            stil: kasten.stil,
                            knoten: kasten.knoten,
                        });
                    }
                    aus.push(Stueck {
                        inhalt: Inhalt::Umbruch,
                        stil: kasten.stil,
                        knoten: kasten.knoten,
                    });
                } else if c == ' ' || c == '\t' {
                    if !wort.is_empty() {
                        aus.push(Stueck {
                            inhalt: Inhalt::Wort(core::mem::take(&mut wort)),
                            stil: kasten.stil,
                            knoten: kasten.knoten,
                        });
                    }
                    aus.push(Stueck {
                        inhalt: Inhalt::Leerzeichen,
                        stil: kasten.stil,
                        knoten: kasten.knoten,
                    });
                } else {
                    wort.push(c);
                }
            }
            if !wort.is_empty() {
                aus.push(Stueck {
                    inhalt: Inhalt::Wort(wort),
                    stil: kasten.stil,
                    knoten: kasten.knoten,
                });
            }
        }
        KastenArt::Umbruch => aus.push(Stueck {
            inhalt: Inhalt::Umbruch,
            stil: kasten.stil,
            knoten: kasten.knoten,
        }),
        KastenArt::Inline => {
            // Die Auszeichnung selbst hat keine Geometrie — nur ihre
            // Kinder, mit IHREM (schon geerbten) Stil.
            for kind in &kasten.kinder {
                einsammeln(kind, aus);
            }
        }
        KastenArt::Bild { breite, hoehe, .. } => {
            // Ein Bild ohne Massangabe bekommt einen Platzhalter. NICHT
            // 0x0: Ein Bild, dessen Groesse man erst nach dem Laden
            // kennt, wuerde sonst unsichtbar bleiben und die Seite
            // spaeter umspringen lassen.
            let mut k = kasten.clone();
            let b = breite.unwrap_or(PLATZHALTER_BREITE).max(0);
            let h = hoehe.unwrap_or(PLATZHALTER_HOEHE).max(0);
            k.masse.inhalt = Rechteck::neu(0, 0, b, h);
            aus.push(Stueck {
                inhalt: Inhalt::Kasten(alloc::boxed::Box::new(k)),
                stil: kasten.stil,
                knoten: kasten.knoten,
            });
        }
        KastenArt::InlineBlock => {
            aus.push(Stueck {
                inhalt: Inhalt::Kasten(alloc::boxed::Box::new(kasten.clone())),
                stil: kasten.stil,
                knoten: kasten.knoten,
            });
        }
        // Ein Block mitten im Inline-Strom kann es nach dem Einziehen
        // anonymer Bloecke nicht mehr geben. Falls doch: ueberspringen
        // statt raten.
        _ => {}
    }
}

/// Masse eines Bildes ohne Angabe. Entspricht dem, was Browser fuer ein
/// noch nicht geladenes Bild ohne `width`/`height` reservieren.
const PLATZHALTER_BREITE: i32 = 32;
const PLATZHALTER_HOEHE: i32 = 32;

/// Ein zu langes Wort so weit abschneiden, wie es passt.
///
/// Die Schnittstelle liegt IMMER auf einer Zeichengrenze — sie kommt aus
/// `char_indices()`, nie aus einer Rechnung. Deshalb panickt das
/// Schneiden auch bei „Grüße" nicht.
fn hart_trennen(
    wort: &str,
    breite: i32,
    stueck: &Stueck,
    metrik: &dyn Metrik,
) -> (String, String) {
    let groesse = schrift_px(&stueck.stil, metrik);
    let mut letzte_gute = 0usize;
    for (i, c) in wort.char_indices() {
        let bis = i + c.len_utf8();
        let w = metrik.text_breite(&wort[..bis], groesse, stueck.stil.fett, stueck.stil.kursiv);
        if w > breite && letzte_gute > 0 {
            break;
        }
        letzte_gute = bis;
    }
    if letzte_gute == 0 || letzte_gute >= wort.len() {
        // Selbst ein Zeichen passt nicht (oder alles passt): nicht
        // trennen. Ohne diese Bremse dreht sich die Schleife ewig.
        return (String::from(wort), String::new());
    }
    (
        String::from(&wort[..letzte_gute]),
        String::from(&wort[letzte_gute..]),
    )
}

/// Eine Zeile fertigstellen: Grundlinie bestimmen, Stuecke aufhaengen,
/// ausrichten. Liefert die Hoehe der Zeile.
fn zeile_abschliessen(
    laufend: &mut Vec<(Stueck, i32)>,
    zeilen: &mut Vec<Kasten>,
    x: i32,
    y: i32,
    breite: i32,
    metrik: &dyn Metrik,
    befund: &mut Befund,
) -> i32 {
    if laufend.is_empty() {
        return 0;
    }
    // Leerzeichen am Zeilenende zaehlen nicht mit — sonst bricht eine
    // Zeile um, die genau passen wuerde.
    while matches!(laufend.last(), Some((s, _)) if matches!(s.inhalt, Inhalt::Leerzeichen)) {
        laufend.pop();
    }
    // Ein reiner Umbruch erzeugt trotzdem eine (leere) Zeile — sonst
    // faellt `<br><br>` in sich zusammen.
    let nur_umbruch = laufend
        .iter()
        .all(|(s, _)| matches!(s.inhalt, Inhalt::Umbruch));

    // --- Grundlinie: die groesste ueber alle Stuecke ---
    let mut ueber_grundlinie = 0i32; // Hoehe ueber der Grundlinie
    let mut unter_grundlinie = 0i32;
    for (stueck, _) in laufend.iter() {
        let groesse = schrift_px(&stueck.stil, metrik);
        let zeilen_hoehe = zeilenhoehe_von(&stueck.stil, groesse, metrik);
        let basis = metrik.grundlinie(groesse);
        // Der Durchschuss (line-height minus Schrifthoehe) wird HALB
        // oben und HALB unten verteilt — so macht es die Spezifikation,
        // und so sitzt Text mit grosser line-height mittig in seiner
        // Zeile statt oben zu kleben.
        let zusatz = (zeilen_hoehe - groesse).max(0) / 2;
        match &stueck.inhalt {
            Inhalt::Kasten(k) => {
                let h = k.masse.margin_box().hoehe;
                match stueck.stil.vertikal {
                    // Unterkante auf der Grundlinie (die Voreinstellung).
                    Vertikal::Grundlinie | Vertikal::Unten => {
                        ueber_grundlinie = ueber_grundlinie.max(h);
                    }
                    Vertikal::Oben => {
                        ueber_grundlinie = ueber_grundlinie.max(basis + zusatz);
                        unter_grundlinie = unter_grundlinie.max(h - basis);
                    }
                    Vertikal::Mitte => {
                        ueber_grundlinie = ueber_grundlinie.max(h / 2 + basis / 2);
                        unter_grundlinie = unter_grundlinie.max(h / 2);
                    }
                    Vertikal::Tiefgestellt => {
                        ueber_grundlinie = ueber_grundlinie.max(h - groesse / 4);
                        unter_grundlinie = unter_grundlinie.max(groesse / 4);
                    }
                    Vertikal::Hochgestellt => {
                        ueber_grundlinie = ueber_grundlinie.max(h + groesse / 4);
                    }
                }
            }
            _ => {
                ueber_grundlinie = ueber_grundlinie.max(basis + zusatz);
                unter_grundlinie = unter_grundlinie.max(zeilen_hoehe - basis - zusatz);
            }
        }
    }
    let hoehe = (ueber_grundlinie + unter_grundlinie).max(if nur_umbruch { 0 } else { 1 });
    let grundlinie_y = y + ueber_grundlinie;

    // --- Ausrichtung: wo faengt die Zeile an? ---
    let inhalt_breite: i32 = laufend.iter().map(|(_, w)| *w).sum();
    let ausrichtung = laufend
        .first()
        .map(|(s, _)| s.stil.ausrichtung)
        .unwrap_or(Ausrichtung::Links);
    let mut cx = match ausrichtung {
        // `justify` wird wie `left` gesetzt — Blocksatz braucht
        // Wortabstands-Verteilung und steht nicht im Zuschnitt.
        Ausrichtung::Links | Ausrichtung::Blocksatz => x,
        Ausrichtung::Mitte => x + ((breite - inhalt_breite).max(0)) / 2,
        Ausrichtung::Rechts => x + (breite - inhalt_breite).max(0),
    };
    if inhalt_breite > breite && breite > 0 {
        // ZU BREIT: ueberlaufen lassen, nicht abschneiden (Aufgabe 4).
        // Der Inhalt bleibt sichtbar und ragt nach rechts hinaus.
        befund.ueberlaeufe += 1;
        cx = x;
    }

    // --- Die Stuecke aufhaengen ---
    let mut zeile = Kasten::neu(
        KastenArt::Zeile,
        laufend.first().map(|(s, _)| s.stil).unwrap_or_default(),
        None,
    );
    zeile.masse.inhalt = Rechteck::neu(x, y, breite.max(inhalt_breite), hoehe);

    for (stueck, w) in laufend.drain(..) {
        let groesse = schrift_px(&stueck.stil, metrik);
        let basis = metrik.grundlinie(groesse);
        match stueck.inhalt {
            Inhalt::Umbruch => {}
            Inhalt::Leerzeichen => {
                cx += w;
            }
            Inhalt::Wort(wort) => {
                let mut k = Kasten::neu(KastenArt::Text(wort), stueck.stil, stueck.knoten);
                // Die OBERKANTE des Textes, damit der Renderer von dort
                // aus zeichnen kann (unser Zeichner setzt Glyphen von
                // links oben).
                k.masse.inhalt = Rechteck::neu(cx, grundlinie_y - basis, w, groesse);
                zeile.kinder.push(k);
                cx += w;
            }
            Inhalt::Kasten(k) => {
                let mut k = *k;
                let h = k.masse.margin_box().hoehe;
                let oben = match stueck.stil.vertikal {
                    Vertikal::Grundlinie | Vertikal::Unten => grundlinie_y - h,
                    Vertikal::Oben => y,
                    Vertikal::Mitte => grundlinie_y - h / 2 - basis / 2,
                    Vertikal::Tiefgestellt => grundlinie_y - h + groesse / 4,
                    Vertikal::Hochgestellt => grundlinie_y - h - groesse / 4,
                };
                // Die Verschiebung VORHER ausrechnen: `verschieben` leiht
                // sich `k` veraenderlich aus, und dann liesse sich seine
                // alte Position nicht mehr lesen.
                let dx = cx - k.masse.inhalt.x;
                let dy = oben - k.masse.inhalt.y;
                verschieben(&mut k, dx, dy);
                cx += w;
                zeile.kinder.push(k);
            }
        }
    }
    zeilen.push(zeile);
    hoehe
}

/// Die Zeilenhoehe eines Stils in ganzen Pixeln.
fn zeilenhoehe_von(stil: &Stil, groesse: i32, metrik: &dyn Metrik) -> i32 {
    match stil.zeilenhoehe {
        speedcss::Zeilenhoehe::Normal => metrik.zeilen_hoehe(groesse),
        _ => {
            let t = stil.zeilenhoehe_px();
            let px = speedcss::werte::runden(t);
            if px > 0 {
                px
            } else {
                metrik.zeilen_hoehe(groesse)
            }
        }
    }
}

/// Einen fertig gesetzten Kasten mitsamt Kindern verschieben.
///
/// Wird gebraucht, weil ein `inline-block` gesetzt wird, BEVOR seine
/// Position in der Zeile feststeht — erst dann weiss man, wie breit die
/// Stuecke davor sind.
///
/// REKURSIV, und das ist hier gefahrlos: Der Kastenbaum ist beim Bauen
/// schon auf `Grenzen::max_tiefe` (64) gedeckelt worden. Die
/// Sicherheitsschranke steht trotzdem dabei, weil diese Funktion auch
/// einen Baum bekommen koennte, den jemand anders gebaut hat — und ein
/// `unsafe` mit rohen Zeigern waere fuer eine Addition der falsche Preis
/// (`libspeed::bild`, `pem`, `netz` und die anderen wirtsfreien Kisten
/// haben zusammen null unsafe-Bloecke).
pub(crate) fn verschieben(kasten: &mut Kasten, dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    verschieben_tief(kasten, dx, dy, 0);
}

fn verschieben_tief(kasten: &mut Kasten, dx: i32, dy: i32, tiefe: usize) {
    if tiefe > 256 {
        return;
    }
    kasten.masse.inhalt.x += dx;
    kasten.masse.inhalt.y += dy;
    for kind in kasten.kinder.iter_mut() {
        verschieben_tief(kind, dx, dy, tiefe + 1);
    }
}

/// Die Breite, die ein Inline-Teilbaum ohne Umbruch braeuchte.
///
/// Fuer die Spaltenbreiten der Tabellen (`layout::tabelle`). Eine
/// Naeherung: Sie misst den ganzen Text als eine Zeile und ignoriert,
/// dass ein `inline-block` selbst umbrechen koennte.
pub(crate) fn wunschbreite(kasten: &Kasten, metrik: &dyn Metrik) -> i32 {
    let mut stuecke = Vec::new();
    // ÜBER DEN GANZEN TEILBAUM, nicht nur ueber den Kasten selbst:
    // Eine Tabellenzelle ist ein BLOCK, und `einsammeln` sammelt aus
    // Bloecken nichts ein (es ist fuer den Inline-Strom gebaut). Ohne
    // diese Schleife blieben alle Wunschbreiten 0, alle Spalten gleich
    // breit — und die inhaltsbasierte Verteilung waere eine Behauptung.
    // Gefunden von `test_tabellenspalten_richten_sich_nach_dem_inhalt`.
    for teil in kasten.alle() {
        if teil.art.ist_inline() {
            einsammeln(teil, &mut stuecke);
        }
    }
    let mut summe = 0i32;
    for stueck in &stuecke {
        let groesse = schrift_px(&stueck.stil, metrik);
        summe += match &stueck.inhalt {
            Inhalt::Wort(w) => {
                metrik.text_breite(w, groesse, stueck.stil.fett, stueck.stil.kursiv)
            }
            Inhalt::Leerzeichen => {
                metrik.text_breite(" ", groesse, stueck.stil.fett, stueck.stil.kursiv)
            }
            Inhalt::Umbruch => 0,
            Inhalt::Kasten(k) => k.masse.margin_box().breite.max(PLATZHALTER_BREITE),
        };
    }
    summe
}

/// Der waagerechte Platzbedarf eines Kastens (Rand, Rahmen, Padding) bei
/// bekannter Bezugsbreite.
pub(crate) fn drumherum(stil: &Stil, bezug: i32) -> i32 {
    let l = |v: Laenge| px(v, bezug, 0);
    l(stil.margin.links)
        + l(stil.margin.rechts)
        + l(stil.padding.links)
        + l(stil.padding.rechts)
        + l(stil.rahmen_breite.links)
        + l(stil.rahmen_breite.rechts)
}
