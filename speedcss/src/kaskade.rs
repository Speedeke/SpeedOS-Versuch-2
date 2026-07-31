// speedcss::kaskade — aus Regeln und Baum berechnete Stile machen
//
// ===========================================================================
// DIE VIER SCHRITTE, IN DIESER REIHENFOLGE
//
//   1. PASSEN      Welche Regeln treffen auf dieses Element zu?
//   2. SORTIEREN   Nach Herkunft, !important, Spezifitaet, Reihenfolge.
//   3. ANWENDEN    In dieser Sortierung, schwaechste zuerst.
//   4. VERERBEN    Die geerbten Felder kommen VOR Schritt 3 vom Elternteil.
//
// Die Reihenfolge ist nicht beliebig: Vererbung ist der UNTERGRUND, auf den
// die eigenen Regeln geschrieben werden. Wer erst kaskadiert und dann erbt,
// ueberschreibt die eigenen Regeln mit denen des Elternteils.
//
// ===========================================================================
// DIE SORTIERREGEL, VOLLSTAENDIG
//
// Von schwach nach stark:
//
//   (1) Standard-Stylesheet, gewoehnlich
//   (2) Autor-Stylesheet, gewoehnlich
//   (3) Autor-Stylesheet, !important
//   (4) Standard-Stylesheet, !important
//
// **Punkt (4) ist der, den man falsch macht**: Bei `!important` dreht sich
// die Herkunfts-Reihenfolge UM. In der echten Spezifikation gilt das fuer
// Benutzer- und Browser-Stylesheets und dient der Barrierefreiheit (eine
// Nutzer-Einstellung „alles gross" soll sich nicht wegformatieren lassen).
// Unser Standard-Stylesheet benutzt kein `!important`, die Regel ist hier
// also folgenlos — sie steht trotzdem drin, weil sie richtig ist und weil
// ein spaeteres Benutzer-Stylesheet sie braucht.
//
// Innerhalb derselben Stufe: hoehere Spezifitaet gewinnt, bei Gleichstand
// die SPAETERE Regel.

use crate::parser::{Deklaration, Pseudo, Selektor, Spezifitaet, Stylesheet, Verbund};
use crate::stil::{self, Ergebnis, Stil};
use alloc::string::String;
use alloc::vec::Vec;
use speedhtml::dom::{Art, Knoten};
use speedhtml::{Dokument, KnotenId};

// ---------------------------------------------------------------------------
// HERKUNFT
// ---------------------------------------------------------------------------

/// Woher eine Regel stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Herkunft {
    /// Das eingebaute Stylesheet (`standard.rs`).
    Standard,
    /// Aus dem Dokument: `<style>`-Bloecke und `style`-Attribute.
    Autor,
}

/// Der Zustand eines Elements, soweit Selektoren ihn sehen koennen.
///
/// **`:hover` ist damit VORBEREITET** (docs/browser-v1.md): Die Kaskade
/// kann es schon, der Browser meldet es noch nicht. Wenn er es tut,
/// aendert sich hier nichts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Zustand {
    pub unter_maus: bool,
    pub besucht: bool,
}

// ---------------------------------------------------------------------------
// PASSEN
// ---------------------------------------------------------------------------

/// Die Klassen eines Elements (aus dem `class`-Attribut).
fn klassen_von(knoten: &Knoten) -> impl Iterator<Item = &str> {
    knoten
        .attribut("class")
        .unwrap_or("")
        .split_whitespace()
}

/// Passt EIN Verbund (`div.warn#x`) auf dieses Element?
fn verbund_passt(verbund: &Verbund, knoten: &Knoten, zustand: Zustand) -> bool {
    if let Some(tag) = &verbund.tag {
        if knoten.name() != Some(tag.as_str()) {
            return false;
        }
    }
    if let Some(id) = &verbund.id {
        if knoten.attribut("id") != Some(id.as_str()) {
            return false;
        }
    }
    for klasse in &verbund.klassen {
        // Klassennamen sind gross-/kleinschreibungsempfindlich (anders als
        // Tag-Namen) — deshalb ein exakter Vergleich.
        if !klassen_von(knoten).any(|k| k == klasse) {
            return false;
        }
    }
    for pseudo in &verbund.pseudo {
        let erfuellt = match pseudo {
            // `:link` heisst „Link MIT Ziel". Ein `<a name=x>` ohne href
            // ist ein Anker und kein Link — er soll nicht blau werden.
            Pseudo::Link => {
                knoten.name() == Some("a") && knoten.attribut("href").is_some() && !zustand.besucht
            }
            Pseudo::Besucht => {
                knoten.name() == Some("a") && knoten.attribut("href").is_some() && zustand.besucht
            }
            Pseudo::Hover => zustand.unter_maus,
            // Unbekannte Pseudoklassen passen NIE (siehe parser.rs).
            Pseudo::Unbekannt(_) => false,
        };
        if !erfuellt {
            return false;
        }
    }
    true
}

/// Passt der ganze Selektor auf `ziel`?
///
/// ===================================================================
/// VON HINTEN NACH VORN — und warum das nicht nur schneller ist
///
/// `div p span` besteht aus drei Verbuenden. Der LETZTE beschreibt das
/// Element selbst, die davor seine Vorfahren. Von vorn zu suchen hiesse,
/// im Baum abzusteigen und alle Wege auszuprobieren; von hinten geht man
/// die EINE Kette der Vorfahren hoch.
///
/// Der Nachkommen-Kombinator ist dabei „irgendwo darueber", nicht
/// „direkt darueber" — also wird bei einem Fehlschlag weiter nach oben
/// gesucht, bis die Wurzel erreicht ist. (Ein Kind-Kombinator `>` wuerde
/// genau hier abbrechen statt weiterzusuchen. Wir haben ihn nicht;
/// `parser::selektor_parsen` lehnt solche Selektoren ab, statt sie
/// faelschlich als Nachkommen zu deuten.)
pub fn passt(selektor: &Selektor, dokument: &Dokument, ziel: KnotenId, zustand: Zustand) -> bool {
    let Some(knoten) = dokument.knoten(ziel) else {
        return false;
    };
    let Some(letzter) = selektor.verbuende.last() else {
        return false;
    };
    if !verbund_passt(letzter, knoten, zustand) {
        return false;
    }

    // Die uebrigen Verbuende muessen auf Vorfahren passen, in dieser
    // Reihenfolge.
    let mut rest = &selektor.verbuende[..selektor.verbuende.len() - 1];
    let mut aktuell = knoten.eltern;

    while !rest.is_empty() {
        let Some(id) = aktuell else {
            return false; // Wurzel erreicht, noch Bedingungen offen
        };
        let Some(vorfahr) = dokument.knoten(id) else {
            return false;
        };
        // Nur Elemente zaehlen als Vorfahren.
        if vorfahr.ist_element() {
            let letzter_rest = &rest[rest.len() - 1];
            // Der Zustand gilt nur fuer das ZIEL-Element; ein Vorfahr ist
            // nicht „hover", nur weil das Kind es ist. (Echte Browser
            // machen es anders — dort erbt Hover nach unten. Fuer V1 ist
            // die einfachere Regel ehrlicher.)
            if verbund_passt(letzter_rest, vorfahr, Zustand::default()) {
                rest = &rest[..rest.len() - 1];
            }
        }
        aktuell = vorfahr.eltern;
    }
    true
}

// ---------------------------------------------------------------------------
// KASKADE
// ---------------------------------------------------------------------------

/// Eine Deklaration mit allem, was ueber ihren Rang entscheidet.
#[derive(Debug, Clone)]
struct Kandidat<'a> {
    deklaration: &'a Deklaration,
    herkunft: Herkunft,
    spezifitaet: Spezifitaet,
    reihenfolge: usize,
    /// Der Selektor-Text — nur fuer `erklaeren`.
    selektor: &'a str,
}

impl Kandidat<'_> {
    /// Der Sortierschluessel. Groesser = gewinnt.
    ///
    /// Die STUFE (`stufe`) buendelt Herkunft und `!important` zu der
    /// Rangfolge aus dem Kopfkommentar; danach entscheidet Spezifitaet,
    /// dann die Position im Stylesheet.
    fn schluessel(&self) -> (u8, Spezifitaet, usize) {
        let stufe = match (self.herkunft, self.deklaration.wichtig) {
            (Herkunft::Standard, false) => 0,
            (Herkunft::Autor, false) => 1,
            (Herkunft::Autor, true) => 2,
            (Herkunft::Standard, true) => 3,
        };
        (stufe, self.spezifitaet, self.reihenfolge)
    }
}

/// Alle passenden Deklarationen fuer ein Element einsammeln.
fn kandidaten<'a>(
    blaetter: &[(&'a Stylesheet, Herkunft)],
    dokument: &Dokument,
    ziel: KnotenId,
    zustand: Zustand,
) -> Vec<Kandidat<'a>> {
    let mut aus = Vec::new();
    // Die Reihenfolge zaehlt ueber Stylesheet-Grenzen hinweg weiter, damit
    // zwei Autor-Stylesheets sich deterministisch verhalten.
    let mut versatz = 0usize;
    for (blatt, herkunft) in blaetter {
        for regel in &blatt.regeln {
            // Die BESTE Spezifitaet unter den passenden Selektoren zaehlt.
            let mut beste: Option<Spezifitaet> = None;
            let mut bester_text = "";
            for selektor in &regel.selektoren {
                if passt(selektor, dokument, ziel, zustand) {
                    let s = selektor.spezifitaet();
                    if beste.is_none_or(|b| s > b) {
                        beste = Some(s);
                        bester_text = &selektor.text;
                    }
                }
            }
            let Some(spezifitaet) = beste else { continue };
            for deklaration in &regel.deklarationen {
                aus.push(Kandidat {
                    deklaration,
                    herkunft: *herkunft,
                    spezifitaet,
                    reihenfolge: versatz + regel.reihenfolge,
                    selektor: bester_text,
                });
            }
        }
        versatz += blatt.regeln.len() + 1;
    }
    aus
}

/// Das `style`-Attribut eines Elements als Deklarationsliste.
///
/// Ein Inline-Stil schlaegt JEDEN Selektor — in der Spezifikation hat er
/// eine eigene, hoehere Stufe. Wir bilden das mit einer kuenstlich hohen
/// Spezifitaet ab (mehr Ids, als ein Selektor je haben kann); das ist
/// dasselbe Ergebnis mit weniger Maschinerie.
fn inline_deklarationen(knoten: &Knoten) -> Vec<Deklaration> {
    let Some(text) = knoten.attribut("style") else {
        return Vec::new();
    };
    let mut befund = crate::parser::Befund::default();
    crate::parser::deklarationen_parsen(text, &mut befund, crate::parser::Grenzen::standard())
}

/// Die Spezifitaet, die ein `style`-Attribut bekommt.
const INLINE_SPEZIFITAET: Spezifitaet = Spezifitaet {
    ids: u32::MAX,
    klassen: 0,
    typen: 0,
};

/// Den berechneten Stil EINES Elements bestimmen.
///
/// `eltern` ist der schon berechnete Stil des Elternteils (fuer
/// Vererbung), oder `ANFANG` bei der Wurzel.
pub fn stil_fuer(
    dokument: &Dokument,
    ziel: KnotenId,
    blaetter: &[(&Stylesheet, Herkunft)],
    eltern: &Stil,
    zustand: Zustand,
) -> Stil {
    // (4) Vererbung ist der Untergrund.
    let mut stil = Stil::geerbt_von(eltern);

    let Some(knoten) = dokument.knoten(ziel) else {
        return stil;
    };
    if !knoten.ist_element() {
        return stil;
    }

    let mut liste = kandidaten(blaetter, dokument, ziel, zustand);

    // Der Inline-Stil kommt dazu — mit der hoechsten Spezifitaet.
    let inline = inline_deklarationen(knoten);
    let inline_reihenfolge = usize::MAX;
    for deklaration in &inline {
        liste.push(Kandidat {
            deklaration,
            herkunft: Herkunft::Autor,
            spezifitaet: INLINE_SPEZIFITAET,
            reihenfolge: inline_reihenfolge,
            selektor: "style=\"\"",
        });
    }

    // (2) Sortieren: schwaechste zuerst.
    liste.sort_by_key(|k| k.schluessel());

    // (3) Anwenden — `font-size` VORWEG.
    //
    // Der Grund steht in `stil::anwenden`: `em` in allen anderen Laengen
    // bezieht sich auf die Schriftgroesse DIESES Elements, und die muss
    // deshalb feststehen, bevor sie gelesen wird. Ein zweiter Durchlauf
    // nur fuer `font-size` ist der einfachste Weg, der immer stimmt.
    for kandidat in &liste {
        if kandidat.deklaration.name == "font-size" {
            stil::anwenden(
                &mut stil,
                &kandidat.deklaration.name,
                &kandidat.deklaration.wert,
                eltern,
            );
        }
    }
    for kandidat in &liste {
        if kandidat.deklaration.name != "font-size" {
            stil::anwenden(
                &mut stil,
                &kandidat.deklaration.name,
                &kandidat.deklaration.wert,
                eltern,
            );
        }
    }

    // `em` in den uebrigen Laengen aufloesen — jetzt, wo `schrift_px`
    // endgueltig ist. Damit ist der Stil ein echter „computed value":
    // Was danach noch relativ ist (`%`, `auto`), gehoert dem Layout.
    em_aufloesen(&mut stil);
    stil
}

/// Alle `em`-Laengen eines Stils in Pixel umrechnen.
///
/// UND `line-height` MIT: Eine prozentuale oder em-basierte Zeilenhoehe
/// wird laut Spezifikation DORT ausgerechnet, wo sie steht — vererbt wird
/// dann das ERGEBNIS. Nur die nackte Zahl (`line-height: 1.5`) vererbt
/// sich als FAKTOR und wird beim Kind neu angewandt. Wer das verwechselt,
/// bekommt bei `line-height: 150%` auf einem Elternteil mit kleinerer
/// Schrift zu grosse Kinderzeilen.
fn em_aufloesen(stil: &mut Stil) {
    let schrift = stil.schrift_px;
    if let crate::stil::Zeilenhoehe::Laenge(l) = stil.zeilenhoehe {
        let absolut = match l.em_aufloesen(schrift) {
            crate::werte::Laenge::Prozent(p) => crate::werte::Laenge::Px(
                ((schrift as i64 * p as i64) / (100 * crate::werte::TAUSEND as i64)) as i32,
            ),
            andere => andere,
        };
        stil.zeilenhoehe = crate::stil::Zeilenhoehe::Laenge(absolut);
    }
    let kante = |k: &mut crate::stil::Kanten<crate::werte::Laenge>| {
        k.oben = k.oben.em_aufloesen(schrift);
        k.rechts = k.rechts.em_aufloesen(schrift);
        k.unten = k.unten.em_aufloesen(schrift);
        k.links = k.links.em_aufloesen(schrift);
    };
    kante(&mut stil.margin);
    kante(&mut stil.padding);
    kante(&mut stil.rahmen_breite);
    stil.breite = stil.breite.em_aufloesen(schrift);
    stil.hoehe = stil.hoehe.em_aufloesen(schrift);
    stil.max_breite = stil.max_breite.em_aufloesen(schrift);
}

// ---------------------------------------------------------------------------
// DER STIL-BAUM
// ---------------------------------------------------------------------------

/// Ein berechneter Stil je Knoten — das Ergebnis, das das Layout
/// konsumiert.
///
/// Ein `Vec`, indiziert wie die Knoten-Arena von `speedhtml`. Dieselbe
/// Ueberlegung wie dort: Eine Zuordnung waere flexibler und teurer, und
/// die Indizes sind ohnehin dicht.
pub struct StilBaum {
    stile: Vec<Stil>,
}

impl StilBaum {
    /// Der Stil eines Knotens.
    ///
    /// Fuer Textknoten ist es der Stil ihres ELTERNTEILS — Text hat keine
    /// eigenen Regeln, erbt aber alles. Genau so will das Layout es haben.
    pub fn stil(&self, id: KnotenId) -> &Stil {
        self.stile
            .get(id.0 as usize)
            .unwrap_or(&crate::stil::ANFANG)
    }

    pub fn anzahl(&self) -> usize {
        self.stile.len()
    }
}

/// Den ganzen Baum durchrechnen.
///
/// ===================================================================
/// ITERATIV, NICHT REKURSIV
///
/// Ein `max_tiefe` von 100 (speedhtml) waere fuer Rekursion sicher — aber
/// diese Funktion soll auch einen Baum ueberstehen, den jemand mit anderen
/// Grenzen gebaut hat, und der User-Stack ist 64 KiB. Dieselbe
/// Entscheidung wie bei `Dokument::text_von` und `baum_text`.
///
/// Ein Vorbestell-Durchlauf (Eltern vor Kindern) ist Pflicht: Vererbung
/// braucht den fertigen Elternstil.
pub fn berechnen(
    dokument: &Dokument,
    blaetter: &[(&Stylesheet, Herkunft)],
    zustand: Zustand,
) -> StilBaum {
    let mut stile = alloc::vec![crate::stil::ANFANG; dokument.anzahl()];
    // (Knoten, Elternstil-Index) — die Wurzel erbt vom Anfangswert.
    let mut stapel = alloc::vec![(Dokument::WURZEL, crate::stil::ANFANG)];

    while let Some((id, eltern_stil)) = stapel.pop() {
        let Some(knoten) = dokument.knoten(id) else {
            continue;
        };

        let eigener = match &knoten.art {
            // Die Wurzel selbst hat keinen Stil — sie ist kein Element.
            Art::Wurzel => crate::stil::ANFANG,
            // Text, Kommentare und DOCTYPE erben nur.
            Art::Text(_) | Art::Kommentar(_) | Art::Doctype(_) => eltern_stil,
            Art::Element { .. } => stil_fuer(dokument, id, blaetter, &eltern_stil, zustand),
        };
        if let Some(platz) = stile.get_mut(id.0 as usize) {
            *platz = eigener;
        }

        for kind in knoten.kinder.iter().rev() {
            stapel.push((*kind, eigener));
        }
    }
    StilBaum { stile }
}

// ---------------------------------------------------------------------------
// ERKLAEREN — die Grundlage von cssdump
// ---------------------------------------------------------------------------

/// Woher ein berechneter Wert kommt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quelle {
    /// Eine Regel hat ihn gesetzt.
    Regel {
        herkunft: Herkunft,
        selektor: String,
        spezifitaet: Spezifitaet,
        wichtig: bool,
        wert: String,
    },
    /// Vom Elternteil geerbt.
    Vererbt,
    /// Der Anfangswert der Spezifikation — niemand hat etwas gesagt.
    Anfangswert,
}

/// Was `cssdump` je Eigenschaft zeigt.
#[derive(Debug, Clone)]
pub struct Erklaerung {
    pub eigenschaft: &'static str,
    /// Der BERECHNETE Wert, lesbar.
    pub wert: String,
    pub quelle: Quelle,
    /// Regeln, die dieselbe Eigenschaft setzen wollten und verloren haben
    /// — von stark nach schwach, ohne die Siegerin.
    pub ueberstimmt: Vec<Quelle>,
}

/// Fuer EIN Element aufschluesseln, welche Regel welchen Wert gesetzt hat.
///
/// ===================================================================
/// WARUM DAS EINE ZWEITE KASKADE IST UND KEINE BUCHHALTUNG
///
/// Man koennte die Herkunft je Eigenschaft waehrend `berechnen`
/// mitschreiben. Das kostete bei 20 000 Knoten dann 20 000 mal zwanzig
/// Strings — fuer eine Information, die man an EINEM Knoten braucht und
/// auch nur dann, wenn jemand fragt.
///
/// Also wird fuer den gefragten Knoten noch einmal kaskadiert, diesmal
/// mit Protokoll. Das Ergebnis MUSS mit dem aus `berechnen` uebereinstimmen
/// — `tests::test_erklaerung_stimmt_mit_berechnung` prueft genau das.
pub fn erklaeren(
    dokument: &Dokument,
    ziel: KnotenId,
    blaetter: &[(&Stylesheet, Herkunft)],
    eltern: &Stil,
    zustand: Zustand,
) -> Vec<Erklaerung> {
    let mut liste = kandidaten(blaetter, dokument, ziel, zustand);
    let inline = dokument
        .knoten(ziel)
        .map(inline_deklarationen)
        .unwrap_or_default();
    for deklaration in &inline {
        liste.push(Kandidat {
            deklaration,
            herkunft: Herkunft::Autor,
            spezifitaet: INLINE_SPEZIFITAET,
            reihenfolge: usize::MAX,
            selektor: "style=\"\"",
        });
    }
    // Von STARK nach SCHWACH — der erste Treffer je Eigenschaft ist der
    // Sieger.
    liste.sort_by_key(|k| core::cmp::Reverse(k.schluessel()));

    let endstil = stil_fuer(dokument, ziel, blaetter, eltern, zustand);
    let geerbt = Stil::geerbt_von(eltern);

    let mut aus = Vec::new();
    for &eigenschaft in stil::ANZEIGE_REIHENFOLGE {
        let wert = stil::wert_als_text(&endstil, eigenschaft);

        // Alle Kandidaten, die diese Eigenschaft setzen wollten.
        let mut treffer: Vec<&Kandidat> = liste
            .iter()
            .filter(|k| betrifft(&k.deklaration.name, eigenschaft))
            .collect();
        // Die, deren Wert unlesbar war, haben nichts gesetzt.
        treffer.retain(|k| {
            let mut probe = geerbt;
            stil::anwenden(
                &mut probe,
                &k.deklaration.name,
                &k.deklaration.wert,
                eltern,
            ) == Ergebnis::Gesetzt
        });

        let quelle = match treffer.first() {
            Some(k) => Quelle::Regel {
                herkunft: k.herkunft,
                selektor: String::from(k.selektor),
                spezifitaet: k.spezifitaet,
                wichtig: k.deklaration.wichtig,
                wert: k.deklaration.wert.clone(),
            },
            None => {
                // Niemand hat etwas gesagt: geerbt oder Anfangswert?
                if stil::erbt(eigenschaft)
                    && stil::wert_als_text(&geerbt, eigenschaft)
                        != stil::wert_als_text(&crate::stil::ANFANG, eigenschaft)
                {
                    Quelle::Vererbt
                } else {
                    Quelle::Anfangswert
                }
            }
        };

        let ueberstimmt = treffer
            .iter()
            .skip(1)
            .take(4)
            .map(|k| Quelle::Regel {
                herkunft: k.herkunft,
                selektor: String::from(k.selektor),
                spezifitaet: k.spezifitaet,
                wichtig: k.deklaration.wichtig,
                wert: k.deklaration.wert.clone(),
            })
            .collect();

        aus.push(Erklaerung {
            eigenschaft,
            wert,
            quelle,
            ueberstimmt,
        });
    }
    aus
}

/// Setzt die Deklaration `name` die Anzeige-Eigenschaft `anzeige`?
///
/// Kurzformen zaehlen mit: `margin: 0` setzt `margin`, `border: 1px solid`
/// setzt `border-width`, `border-style` und `border-color`. Ohne diese
/// Zuordnung zeigte `cssdump` bei jeder Kurzform „Anfangswert" an, obwohl
/// im Stylesheet etwas steht — und genau dann sucht man an der falschen
/// Stelle.
fn betrifft(name: &str, anzeige: &str) -> bool {
    if name == anzeige {
        return true;
    }
    match anzeige {
        "margin" => name.starts_with("margin-"),
        "padding" => name.starts_with("padding-"),
        "border-width" => {
            name == "border" || name.ends_with("-width") && name.starts_with("border")
        }
        "border-style" => name == "border" || name.starts_with("border") && name.ends_with("-style"),
        "border-color" => name == "border" || name.starts_with("border") && name.ends_with("-color"),
        "background-color" => name == "background",
        "list-style-type" => name == "list-style",
        "text-decoration" => name == "text-decoration-line",
        _ => false,
    }
}
