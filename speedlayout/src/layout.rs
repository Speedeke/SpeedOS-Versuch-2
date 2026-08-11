// speedlayout::layout — Bloecke stapeln, Tabellen ausmessen
//
// ===========================================================================
// BLOCK-LAYOUT IN EINEM SATZ
//
// Ein Block ist so BREIT wie sein Elternteil (minus dem eigenen
// Drumherum) und so HOCH wie sein Inhalt. Die Kinder stapeln sich
// senkrecht.
//
// Die Reihenfolge ist dabei nicht beliebig, und sie ist der Grund, warum
// Layout ein zweistufiger Vorgang ist:
//
//   1. BREITE von oben nach unten (ein Kind kennt sie, bevor es gesetzt
//      wird — sonst koennte es seinen Text nicht umbrechen).
//   2. HOEHE von unten nach oben (ein Elternteil kennt sie erst, wenn
//      alle Kinder fertig sind).
//
// Deshalb ist diese Funktion rekursiv mit Rueckgabewert: Sie bekommt die
// Breite und liefert die Hoehe.
//
// ===========================================================================
// MARGIN-KOLLAPS — die Regel, die man weglassen will
//
// Zwei senkrecht benachbarte Raender werden zu EINEM, und zwar dem
// GROESSEREN. Zwei Absaetze mit je 16 px Abstand haben also 16 px
// zwischen sich, nicht 32.
//
// Man will sie weglassen, weil sie kompliziert klingt. Danach sieht jede
// Seite falsch aus: Das Standard-Stylesheet gibt fast jedem Blockelement
// einen Rand, und ohne Kollaps addieren sich alle. Eine Liste von
// Absaetzen wird doppelt so hoch wie im Browser.
//
// **WIR MACHEN DIE EINFACHE VARIANTE: nur zwischen GESCHWISTERN.**
// Die volle Regel kollabiert auch zwischen Elternteil und erstem/letztem
// Kind und durch leere Kaesten hindurch — mit Ausnahmen fuer Padding,
// Rahmen und `overflow`. Das ist eine der verwickeltsten Ecken von CSS,
// und der sichtbare Gewinn ist klein: Der Fall, der auf jeder Seite
// hundertmal vorkommt, ist „Absatz unter Absatz".
//
// EHRLICHE FOLGE: Ein `<div>` um einen `<p>` bekommt bei uns den Rand des
// `<p>` INNEN statt aussen. Steht in docs/browser-v1.md.

use crate::inline;
use crate::kasten::{Kanten, Kasten, KastenArt};
use crate::{px, Befund, Grenzen, Metrik};
use alloc::vec::Vec;
use speedcss::{Laenge, Stil};

/// Einen Block-Kasten setzen: Position und Breite sind vorgegeben,
/// die Hoehe kommt heraus.
///
/// `x`/`y` ist die linke obere Ecke der **MARGIN-Box** — der Punkt, an
/// dem der Elternteil den Kasten hinstellt. Wo die Inhalts-Box dann
/// liegt, rechnet dieser Aufruf selbst aus.
///
/// ===================================================================
/// WARUM DIE POSITION HIER HINEINGEHT UND NICHT VORHER GESETZT WIRD
///
/// Der Elternteil kann sie nicht ausrechnen: `margin-left: auto` steht
/// erst fest, wenn die BREITE feststeht, und die berechnet dieser Aufruf.
/// Setzt der Elternteil `inhalt.x` vorher, benutzt er einen Rand, den es
/// noch gar nicht gibt — bei `margin: 0 auto` klebte der Kasten dann
/// links, obwohl er mittig gehoert. (Genau dieser Fehler stand hier,
/// bis `test_auto_margin_zentriert` ihn gefunden hat.)
///
/// `verfuegbar` ist die Breite der INHALTS-Box des Elternteils.
#[allow(clippy::too_many_arguments)]
pub(crate) fn block_setzen(
    kasten: &mut Kasten,
    x: i32,
    y: i32,
    verfuegbar: i32,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
    befund: &mut Befund,
    tiefe: usize,
) -> i32 {
    if tiefe >= grenzen.max_tiefe {
        befund.zu_tief += 1;
        befund.abgeschnitten = true;
        kasten.kinder.clear();
        kasten.masse.inhalt.breite = verfuegbar.max(0);
        kasten.masse.inhalt.hoehe = 0;
        return 0;
    }

    kanten_berechnen(kasten, verfuegbar);
    breite_berechnen(kasten, verfuegbar);

    // JETZT steht der Rand fest — erst danach die Position.
    kasten.masse.inhalt.x =
        x + kasten.masse.margin.links + kasten.masse.rahmen.links + kasten.masse.padding.links;
    kasten.masse.inhalt.y =
        y + kasten.masse.margin.oben + kasten.masse.rahmen.oben + kasten.masse.padding.oben;

    let inhalt_breite = kasten.masse.inhalt.breite;
    let start_x = kasten.masse.inhalt.x;
    let start_y = kasten.masse.inhalt.y;

    // --- Die Kinder ---
    let hat_bloecke = kasten.kinder.iter().any(|k| k.art.ist_block());
    let inhalt_hoehe = if matches!(kasten.art, KastenArt::Tabelle) {
        tabelle_setzen(kasten, metrik, grenzen, befund, tiefe)
    } else if hat_bloecke {
        bloecke_stapeln(
            kasten,
            start_x,
            start_y,
            inhalt_breite,
            metrik,
            grenzen,
            befund,
            tiefe,
        )
    } else {
        inline_kinder_setzen(
            kasten,
            start_x,
            start_y,
            inhalt_breite,
            metrik,
            grenzen,
            befund,
            tiefe,
        )
    };

    // --- Die Hoehe ---
    //
    // Eine ausdrueckliche Hoehe gewinnt, aber der Inhalt wird NICHT
    // abgeschnitten: Ist er hoeher, laeuft er ueber (Aufgabe 4). Ein
    // `overflow: hidden` gibt es in unserer Teilmenge nicht, und still
    // abzuschneiden waere die schlechtere Wahl — dann fehlt Text, und
    // niemand sieht, warum.
    let gesetzte_hoehe = kasten.stil.hoehe.px_ganz();
    kasten.masse.inhalt.hoehe = match gesetzte_hoehe {
        Some(h) if h >= 0 => {
            if inhalt_hoehe > h {
                befund.ueberlaeufe += 1;
            }
            h
        }
        _ => inhalt_hoehe,
    };

    kasten.masse.margin_box().hoehe
}

/// Die Kanten (margin/border/padding) in ganze Pixel umrechnen.
///
/// **Prozente beziehen sich IMMER auf die BREITE** des umgebenden
/// Blocks — auch bei `margin-top`. Das ist keine Marotte der
/// Spezifikation, sondern verhindert eine Rueckkopplung: Ein oberer Rand
/// in Prozent der HOEHE haenge von der Hoehe ab, die er selbst
/// mitbestimmt.
fn kanten_berechnen(kasten: &mut Kasten, bezug: i32) {
    let s = &kasten.stil;
    let k = |v: Laenge| px(v, bezug, 0).max(0);
    kasten.masse.padding = Kanten {
        oben: k(s.padding.oben),
        rechts: k(s.padding.rechts),
        unten: k(s.padding.unten),
        links: k(s.padding.links),
    };
    // Ein Rahmen ohne Stil ist unsichtbar und belegt keinen Platz — so
    // schreibt es die Spezifikation (`border-style` ist standardmaessig
    // `none`), und ohne diese Pruefung bekaeme jeder Kasten mit
    // `border-width` einen Rand, den niemand sieht.
    let rb = |breite: Laenge, stil: speedcss::RahmenStil| -> i32 {
        if stil == speedcss::RahmenStil::Keiner {
            0
        } else {
            px(breite, bezug, 0).max(0)
        }
    };
    kasten.masse.rahmen = Kanten {
        oben: rb(s.rahmen_breite.oben, s.rahmen_stil.oben),
        rechts: rb(s.rahmen_breite.rechts, s.rahmen_stil.rechts),
        unten: rb(s.rahmen_breite.unten, s.rahmen_stil.unten),
        links: rb(s.rahmen_breite.links, s.rahmen_stil.links),
    };
    // Raender duerfen NEGATIV sein (das ist ein gebraeuchlicher Kniff),
    // also hier kein `max(0)`.
    let m = |v: Laenge| px(v, bezug, 0);
    kasten.masse.margin = Kanten {
        oben: m(kasten.stil.margin.oben),
        rechts: m(kasten.stil.margin.rechts),
        unten: m(kasten.stil.margin.unten),
        links: m(kasten.stil.margin.links),
    };
}

/// Die Inhalts-Breite bestimmen.
fn breite_berechnen(kasten: &mut Kasten, verfuegbar: i32) {
    let drumherum = kasten.masse.padding.waagerecht()
        + kasten.masse.rahmen.waagerecht()
        + kasten.masse.margin.waagerecht();

    let mut breite = match kasten.stil.breite {
        Laenge::Auto => (verfuegbar - drumherum).max(0),
        andere => px(andere, verfuegbar, (verfuegbar - drumherum).max(0)).max(0),
    };

    // `max-width` deckelt.
    if let Some(max) = kasten.stil.max_breite.auf_bezug(verfuegbar) {
        if max >= 0 {
            breite = breite.min(max);
        }
    }
    kasten.masse.inhalt.breite = breite;

    // `margin: 0 auto` zentriert — der haeufigste Weg, eine Seite mittig
    // zu setzen, und billig zu haben.
    let ist_auto = |v: Laenge| matches!(v, Laenge::Auto);
    if ist_auto(kasten.stil.margin.links) && ist_auto(kasten.stil.margin.rechts) {
        let rest = (verfuegbar
            - breite
            - kasten.masse.padding.waagerecht()
            - kasten.masse.rahmen.waagerecht())
        .max(0);
        kasten.masse.margin.links = rest / 2;
        kasten.masse.margin.rechts = rest - rest / 2;
    }
}

/// Die Block-Kinder senkrecht stapeln — mit Margin-Kollaps.
#[allow(clippy::too_many_arguments)]
fn bloecke_stapeln(
    kasten: &mut Kasten,
    x: i32,
    y: i32,
    breite: i32,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
    befund: &mut Befund,
    tiefe: usize,
) -> i32 {
    let mut cy = y;
    // Der untere Rand des VORIGEN Geschwisters — er wartet darauf, mit
    // dem oberen des naechsten zu kollabieren.
    let mut offener_rand = 0i32;
    let mut erstes = true;

    let mut kinder = core::mem::take(&mut kasten.kinder);
    for kind in kinder.iter_mut() {
        // Inline-Kinder koennen hier nach dem Einziehen anonymer Bloecke
        // nicht mehr stehen; falls doch, werden sie uebersprungen statt
        // geraten.
        if !kind.art.ist_block() {
            continue;
        }
        // Die Kanten VORLAEUFIG berechnen — nur fuer den Kollaps
        // gebraucht (`block_setzen` rechnet sie gleich noch einmal, und
        // erst dann sind auch die `auto`-Raender aufgeloest).
        kanten_berechnen(kind, breite);

        // --- DER KOLLAPS ---
        //
        // Zwischen zwei Geschwistern gilt der GROESSERE der beiden
        // Raender, nicht die Summe. Beim ersten Kind gibt es keinen
        // Vorgaenger, sein oberer Rand zaehlt also voll.
        let oben = kind.masse.margin.oben;
        let abstand = if erstes {
            oben
        } else if oben >= 0 && offener_rand >= 0 {
            oben.max(offener_rand)
        } else {
            // Bei negativen Raendern ist die Regel eine andere (groesster
            // positiver plus kleinster negativer). Wir addieren — das ist
            // die einfache Variante und bei den seltenen negativen
            // Raendern nah genug.
            oben + offener_rand
        };
        cy += abstand;
        erstes = false;

        // Der obere Rand ist im `abstand` schon verrechnet, deshalb
        // bekommt `block_setzen` hier `cy - margin.oben`: Es addiert ihn
        // selbst wieder dazu, und doppelt gezaehlt waere er falsch.
        let oberer_rand = kind.masse.margin.oben;
        block_setzen(
            kind,
            x,
            cy - oberer_rand,
            breite,
            metrik,
            grenzen,
            befund,
            tiefe + 1,
        );

        // Die Hoehe OHNE die Raender — die werden gesondert verrechnet.
        let ohne_rand = kind.masse.rahmen_box().hoehe;
        cy += ohne_rand;
        offener_rand = kind.masse.margin.unten;
    }
    kasten.kinder = kinder;

    // Der untere Rand des letzten Kindes bleibt DRAUSSEN (er kollabiert
    // in der vollen Regel mit dem des Elternteils; in unserer einfachen
    // gehoert er zum Kind und wird von dessen `margin_box` mitgezaehlt).
    cy += offener_rand;
    (cy - y).max(0)
}

/// Inline-Kinder in Zeilen setzen — mit dem Aufzaehlungszeichen davor.
#[allow(clippy::too_many_arguments)]
fn inline_kinder_setzen(
    kasten: &mut Kasten,
    x: i32,
    y: i32,
    breite: i32,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
    befund: &mut Befund,
    tiefe: usize,
) -> i32 {
    // `inline-block`-Kinder muessen VOR dem Zeilenbau gesetzt werden —
    // ihre Masse bestimmen, wo umgebrochen wird.
    for kind in kasten.kinder.iter_mut() {
        if kind.art == KastenArt::InlineBlock {
            // Bei (0,0) setzen — die endgueltige Stelle kennt erst der
            // Zeilenbau, der den Kasten dann als Ganzes verschiebt
            // (`inline::verschieben`).
            block_setzen(kind, 0, 0, breite, metrik, grenzen, befund, tiefe + 1);
        }
    }
    inline::zeilen_setzen(&mut kasten.kinder, x, y, breite, metrik, grenzen, befund)
}

// ---------------------------------------------------------------------------
// TABELLEN
// ---------------------------------------------------------------------------

/// Eine Tabelle setzen.
///
/// ===========================================================================
/// DIE GEWAEHLTE VARIANTE: INHALTSBASIERT IN EINEM DURCHGANG
///
/// Es gab zwei einfache Moeglichkeiten:
///
///   (a) **Gleich breite Spalten.** Zwei Zeilen Code. Und unbrauchbar,
///       sobald eine Tabelle eine schmale und eine breite Spalte hat —
///       also bei jeder Infobox („Erschienen: 1969"), bei jeder
///       Datentabelle, bei praktisch allem auf Wikipedia. Die schmale
///       Spalte bekommt die Haelfte, die breite bricht auf zwanzig
///       Zeilen um.
///
///   (b) **Inhaltsbasiert, ein Durchgang.** Fuer jede Zelle die Breite
///       messen, die ihr Text OHNE Umbruch braeuchte; je Spalte das
///       Maximum nehmen; passt die Summe, ist man fertig, sonst
///       PROPORTIONAL herunterskalieren.
///
/// Gewaehlt ist (b), und die Begruendung ist der Zweck des Ganzen: V1
/// soll Seiten LESBAR machen (docs/browser-v1.md §4). Eine Tabelle mit
/// gleich breiten Spalten ist nicht lesbar, sie ist nur vorhanden.
///
/// Der Preis ist ein zusaetzlicher Messdurchgang ueber alle Zellen —
/// O(Zellen), keine Iteration bis zur Konvergenz. Was (b) NICHT kann und
/// die echte Tabellen-Spezifikation schon: Mindestbreiten beachten (das
/// laengste WORT einer Zelle), `colspan` sauber verteilen, und Spalten,
/// die mehr Platz haben als sie brauchen, an andere abgeben.
fn tabelle_setzen(
    kasten: &mut Kasten,
    metrik: &dyn Metrik,
    grenzen: Grenzen,
    befund: &mut Befund,
    tiefe: usize,
) -> i32 {
    let breite = kasten.masse.inhalt.breite;
    let x = kasten.masse.inhalt.x;
    let y = kasten.masse.inhalt.y;

    // --- Die Zeilen einsammeln ---
    //
    // `<tbody>` ist beim Kastenbau zu einem gewoehnlichen Block geworden,
    // die Zeilen liegen also eine Ebene tiefer. Beides wird hier
    // eingesammelt — das ist die Stelle, an der sich auszahlt, dass der
    // Parser KEIN `<tbody>` erfindet (speedhtml): Wir muessen ohnehin
    // beide Formen koennen.
    let mut zeilen_index: Vec<Vec<usize>> = Vec::new();
    sammle_zeilen(&kasten.kinder, &mut zeilen_index, &mut Vec::new());

    if zeilen_index.is_empty() {
        // Keine Zeilen: wie ein gewoehnlicher Block behandeln.
        return bloecke_stapeln(
            kasten, x, y, breite, metrik, grenzen, befund, tiefe,
        );
    }

    // --- Spaltenzahl und Wunschbreiten ---
    let mut spalten = 0usize;
    for pfad in &zeilen_index {
        let zeile = kasten_an(&kasten.kinder, pfad);
        if let Some(z) = zeile {
            spalten = spalten.max(z.kinder.iter().filter(|k| ist_zelle(k)).count());
        }
    }
    if spalten == 0 {
        return bloecke_stapeln(
            kasten, x, y, breite, metrik, grenzen, befund, tiefe,
        );
    }

    let mut wunsch = alloc::vec![0i32; spalten];
    for pfad in &zeilen_index {
        let Some(zeile) = kasten_an(&kasten.kinder, pfad) else {
            continue;
        };
        for (i, zelle) in zeile.kinder.iter().filter(|k| ist_zelle(k)).enumerate() {
            if i >= spalten {
                break;
            }
            let inhalt = inline::wunschbreite(zelle, metrik);
            let drumherum = inline::drumherum(&zelle.stil, breite);
            wunsch[i] = wunsch[i].max(inhalt + drumherum);
        }
    }

    // --- Verteilen ---
    let summe: i32 = wunsch.iter().sum();
    let spalten_breite: Vec<i32> = if summe <= breite || summe == 0 {
        // Es passt: jede Spalte bekommt ihren Wunsch, der Rest wird auf
        // alle verteilt (sonst klebt die Tabelle links).
        let rest = (breite - summe).max(0);
        let je = rest / spalten as i32;
        wunsch.iter().map(|w| w + je).collect()
    } else {
        // Es passt nicht: PROPORTIONAL herunterskalieren, aber jede
        // Spalte behaelt eine Mindestbreite. Ohne die faellt eine sehr
        // schmale Spalte auf 0 und ihr Inhalt verschwindet.
        const MINDEST: i32 = 8;
        wunsch
            .iter()
            .map(|w| {
                let skaliert = ((*w as i64 * breite as i64) / summe as i64) as i32;
                skaliert.max(MINDEST)
            })
            .collect()
    };

    // --- Die Zeilen setzen ---
    let mut cy = y;
    let mut kinder = core::mem::take(&mut kasten.kinder);
    for pfad in &zeilen_index {
        let Some(zeile) = kasten_an_mut(&mut kinder, pfad) else {
            continue;
        };
        zeile.masse.inhalt.x = x;
        zeile.masse.inhalt.y = cy;
        zeile.masse.inhalt.breite = breite;

        let mut cx = x;
        let mut zeilen_hoehe = 0i32;
        let mut spalte = 0usize;
        for zelle in zeile.kinder.iter_mut() {
            if !ist_zelle(zelle) {
                continue;
            }
            let zellen_breite = spalten_breite.get(spalte).copied().unwrap_or(0);
            block_setzen(zelle, cx, cy, zellen_breite, metrik, grenzen, befund, tiefe + 1);
            zeilen_hoehe = zeilen_hoehe.max(zelle.masse.margin_box().hoehe);
            cx += zellen_breite;
            spalte += 1;
        }
        // Alle Zellen einer Zeile sind gleich hoch — das ist der
        // sichtbare Unterschied zwischen einer Tabelle und untereinander
        // gestapelten Kaesten.
        for zelle in zeile.kinder.iter_mut() {
            if ist_zelle(zelle) {
                let drumherum = zelle.masse.margin.senkrecht()
                    + zelle.masse.rahmen.senkrecht()
                    + zelle.masse.padding.senkrecht();
                zelle.masse.inhalt.hoehe = (zeilen_hoehe - drumherum).max(0);
            }
        }
        zeile.masse.inhalt.hoehe = zeilen_hoehe;
        cy += zeilen_hoehe;
    }
    kasten.kinder = kinder;
    (cy - y).max(0)
}

fn ist_zelle(kasten: &Kasten) -> bool {
    kasten.art == KastenArt::TabellenZelle
}

/// Alle Tabellenzeilen finden — auch durch `<tbody>` hindurch.
///
/// Liefert PFADE (Kette von Kind-Indizes) statt Referenzen, weil die
/// Zeilen spaeter VERAENDERT werden muessen und Rust zwei gleichzeitige
/// Zugriffe nicht erlaubt.
fn sammle_zeilen(kinder: &[Kasten], aus: &mut Vec<Vec<usize>>, pfad: &mut Vec<usize>) {
    for (i, kind) in kinder.iter().enumerate() {
        pfad.push(i);
        if kind.art == KastenArt::TabellenZeile {
            aus.push(pfad.clone());
        } else if matches!(kind.art, KastenArt::Block | KastenArt::AnonymerBlock) {
            // Eine Gruppe (`<tbody>`) — eine Ebene tiefer schauen.
            sammle_zeilen(&kind.kinder, aus, pfad);
        }
        pfad.pop();
    }
}

fn kasten_an<'a>(kinder: &'a [Kasten], pfad: &[usize]) -> Option<&'a Kasten> {
    let mut aktuell = kinder.get(*pfad.first()?)?;
    for i in &pfad[1..] {
        aktuell = aktuell.kinder.get(*i)?;
    }
    Some(aktuell)
}

fn kasten_an_mut<'a>(kinder: &'a mut [Kasten], pfad: &[usize]) -> Option<&'a mut Kasten> {
    let mut aktuell = kinder.get_mut(*pfad.first()?)?;
    for i in &pfad[1..] {
        aktuell = aktuell.kinder.get_mut(*i)?;
    }
    Some(aktuell)
}

/// Die Wunschbreite eines Stils, falls gesetzt — fuer die Tabelle.
#[allow(dead_code)]
fn gesetzte_breite(stil: &Stil, bezug: i32) -> Option<i32> {
    stil.breite.auf_bezug(bezug)
}
