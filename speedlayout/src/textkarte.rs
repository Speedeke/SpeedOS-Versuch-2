// speedlayout::textkarte — wo welcher Text auf der Seite steht
//
// ===========================================================================
// WOFUER ES DIESE DATEI GIBT
//
// Die Anzeigeliste sagt, WAS gezeichnet wird. Zwei Dinge, die einen
// Browser vom Vorzeigestueck zum Werkzeug machen, brauchen die
// Umkehrung — von einer Bildschirmstelle oder einem gesuchten Wort
// ZURUECK zum Text:
//
//   * TEXT AUSWAEHLEN UND KOPIEREN (Serie 9, Teil 2, Aufgabe 3): Der
//     Benutzer zeigt auf zwei Punkte, wir muessen sagen, welche Zeichen
//     dazwischenliegen.
//   * IM DOKUMENT SUCHEN (Aufgabe 4): Der Benutzer tippt ein Wort, wir
//     muessen sagen, WO es steht.
//
// Beides ist dieselbe Frage in zwei Richtungen, deshalb steht es in
// EINER Datei mit EINER Datenstruktur. Zwei getrennte Loesungen haetten
// zweimal dieselbe Zeichen-Arithmetik — und die ist der Teil, den man
// falsch macht.
//
// ===========================================================================
// WARUM DAS HIER LIEGT UND NICHT IM BROWSER
//
// Es ist REINE GEOMETRIE auf der Anzeigeliste: keine Maus, keine
// Tastatur, keine Ablage, kein Fenster. Damit ist es auf dem HOST
// testbar — und zwar mit `attrappe::FesteMetrik` (10 px je Zeichen), wo
// jede erwartete Zahl eine Kopfrechnung ist statt aus dem Ergebnis
// abgeschrieben. Die gleiche Ueberlegung wie beim Layout selbst (Serie
// 8, Teil 6): Ein Ergebnis, das man nur fotografieren kann, wird nicht
// geprueft.
//
// Der Browser verdrahtet es danach nur noch mit Maus und Tastatur.
//
// ===========================================================================
// DIE ENTSCHEIDUNG, DIE ALLES ANDERE BESTIMMT: JEDES WORT IST EIN LAUF
//
// `speedlayout` gibt JEDES Wort als eigenen `Befehl::Text` aus — es hat
// seine eigene Position (Serie 8, Teil 6). Fuer die Suche heisst das:
// Ein gesuchter Ausdruck wie „Getting Started" steht NIE in einem
// einzelnen Befehl, sondern immer ueber zwei hinweg. Eine Suche, die
// Befehl fuer Befehl schaut, findet ihn deshalb nie.
//
// Also wird der ganze Text EINMAL aneinandergehaengt, und zu jeder
// Stelle im Gesamttext ist gemerkt, aus welchem Lauf sie kommt. Suchen
// ist dann eine gewoehnliche Textsuche, und erst der TREFFER wird
// zurueck in Rechtecke uebersetzt.
//
// ===========================================================================
// DIE TRENNZEICHEN-FRAGE — der Fall, den man uebersieht
//
// Beim Aneinanderhaengen braucht es zwischen zwei Laeufen ein
// Leerzeichen: Aus „Getting" und „Started" muss „Getting Started"
// werden und nicht „GettingStarted".
//
// ABER NICHT IMMER. `<b>Rust</b>aceans` ergibt zwei Laeufe, die
// unmittelbar aneinanderstossen — dort ein Leerzeichen einzufuegen
// hiesse, dass „Rustaceans" nicht gefunden wird, obwohl es genau so auf
// dem Schirm steht. Das ist der Fall „Treffer ueber Inline-Grenzen".
//
// Unterschieden wird an der GEOMETRIE, denn die Information haben wir:
// Beginnt der naechste Lauf GENAU dort, wo der vorige endet (gleiche
// Zeile, kein Abstand), gehoeren sie zusammen. Sonst kommt ein
// Leerzeichen dazwischen. Kein Sonderwissen ueber Tags noetig.

use crate::anzeige::{Anzeigeliste, Befehl};
use crate::kasten::Rechteck;
use crate::Metrik;
use alloc::string::String;
use alloc::vec::Vec;

/// Ein Textstueck der Anzeigeliste mit seiner Geometrie.
#[derive(Debug, Clone)]
pub struct Lauf {
    /// Index des Befehls in der Anzeigeliste.
    pub befehl: usize,
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub groesse: i32,
    pub fett: bool,
    pub kursiv: bool,
    /// Wo dieser Lauf im Gesamttext der Karte anfaengt (Zeichen, nicht
    /// Bytes — siehe Kopfkommentar von `Textkarte::text`).
    pub start: usize,
}

/// Eine Stelle im Text der Seite.
///
/// GEZAEHLT WIRD IN ZEICHEN, NICHT IN BYTES. Der Text kommt woertlich
/// aus fremden Seiten; „Grüße" hat fuenf Zeichen und sieben Bytes. Wer
/// in Bytes rechnet, schneidet frueher oder spaeter mitten in ein
/// Zeichen — im besten Fall sieht man Muell, im schlechtesten panickt
/// das Slicing. Dieselbe Regel wie bei `text_breite` (Serie 8, Teil 3).
pub type Stelle = usize;

/// Ein Suchtreffer: eine Spanne im Gesamttext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Treffer {
    pub von: Stelle,
    pub bis: Stelle,
}

/// Die Textkarte einer Seite.
pub struct Textkarte {
    laeufe: Vec<Lauf>,
    /// Der gesamte sichtbare Text, in Anzeige-Reihenfolge.
    text: Vec<char>,
}

impl Textkarte {
    /// Aus einer Anzeigeliste bauen.
    pub fn neu(liste: &Anzeigeliste, metrik: &dyn Metrik) -> Textkarte {
        let mut laeufe: Vec<Lauf> = Vec::new();
        let mut text: Vec<char> = Vec::new();

        for (index, befehl) in liste.befehle.iter().enumerate() {
            let Befehl::Text {
                x,
                y,
                text: stueck,
                groesse,
                fett,
                kursiv,
                ..
            } = befehl
            else {
                continue;
            };
            if stueck.is_empty() {
                continue;
            }

            // Braucht es ein Trennzeichen vor diesem Lauf? Siehe
            // Kopfkommentar: nur, wenn er NICHT unmittelbar an den
            // vorigen anschliesst.
            if let Some(vorig) = laeufe.last() {
                let vorig_breite =
                    metrik.text_breite(&vorig.text, vorig.groesse, vorig.fett, vorig.kursiv);
                let stossen_aneinander = vorig.y == *y && (vorig.x + vorig_breite) == *x;
                if !stossen_aneinander {
                    text.push(' ');
                }
            }

            let start = text.len();
            text.extend(stueck.chars());
            laeufe.push(Lauf {
                befehl: index,
                text: stueck.clone(),
                x: *x,
                y: *y,
                groesse: *groesse,
                fett: *fett,
                kursiv: *kursiv,
                start,
            });
        }

        Textkarte { laeufe, text }
    }

    pub fn laeufe(&self) -> &[Lauf] {
        &self.laeufe
    }

    pub fn ist_leer(&self) -> bool {
        self.laeufe.is_empty()
    }

    /// Die Laenge des Gesamttextes **in Zeichen**.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Der gesamte sichtbare Text.
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Der Text zwischen zwei Stellen.
    ///
    /// Die Reihenfolge der beiden Argumente ist EGAL — wer von unten
    /// nach oben auswaehlt, bekommt denselben Text wie von oben nach
    /// unten. Ohne diese Normalisierung liefert eine
    /// Rueckwaerts-Auswahl eine leere Zeichenkette, und das sieht wie
    /// ein Fehler beim Kopieren aus.
    pub fn text_zwischen(&self, a: Stelle, b: Stelle) -> String {
        let (von, bis) = spanne(a, b, self.text.len());
        self.text[von..bis].iter().collect()
    }
}

/// Zwei Stellen zu einer geordneten, geklemmten Spanne machen.
fn spanne(a: Stelle, b: Stelle, laenge: usize) -> (usize, usize) {
    let von = a.min(b).min(laenge);
    let bis = a.max(b).min(laenge);
    (von, bis)
}

// ===========================================================================
// VON EINEM PUNKT ZUM TEXT — die Auswahl (Aufgabe 3)
// ===========================================================================

impl Textkarte {
    /// Welche Textstelle liegt an diesem Punkt?
    ///
    /// ===================================================================
    /// ES GIBT IMMER EINE ANTWORT — und das ist Absicht
    ///
    /// Ein Klick trifft fast nie ein Zeichen genau: Er landet im Abstand
    /// zwischen zwei Woertern, im Rand, unter der letzten Zeile. Ein
    /// `Option` waere hier die falsche Bequemlichkeit — der Aufrufer
    /// muesste bei jedem zweiten Klick entscheiden, was er statt dessen
    /// tut, und jede Auswahl haette Loecher.
    ///
    /// Statt dessen wird die NAECHSTGELEGENE Stelle geliefert. Genau so
    /// verhaelt sich jeder Editor: Klick rechts neben das Zeilenende
    /// setzt an das Zeilenende, Klick unter den Text an sein Ende.
    ///
    /// Gesucht wird in zwei Stufen — erst die Zeile, dann die Spalte —,
    /// weil ein reiner Abstandsvergleich in der Ebene bei mehrspaltigem
    /// Text auf die falsche Spalte zeigt.
    pub fn stelle_bei(&self, x: i32, y: i32, metrik: &dyn Metrik) -> Stelle {
        if self.laeufe.is_empty() {
            return 0;
        }
        // (1) Den Lauf finden, dessen Zeile am besten passt. „Zeile"
        // heisst hier: das senkrechte Band [y, y+groesse) des Laufs.
        let mut bester = 0usize;
        let mut bester_abstand = i32::MAX;
        for (index, lauf) in self.laeufe.iter().enumerate() {
            let oben = lauf.y;
            let unten = lauf.y + lauf.groesse;
            let senkrecht = if y < oben {
                oben - y
            } else if y >= unten {
                y - unten + 1
            } else {
                0
            };
            let breite = metrik.text_breite(&lauf.text, lauf.groesse, lauf.fett, lauf.kursiv);
            let waagerecht = if x < lauf.x {
                lauf.x - x
            } else if x > lauf.x + breite {
                x - (lauf.x + breite)
            } else {
                0
            };
            // Die Zeile wiegt SCHWERER als die Spalte: Ein Punkt zwei
            // Zeilen tiefer ist weiter weg als einer am anderen Ende
            // derselben Zeile. Ohne diese Gewichtung springt die Auswahl
            // bei langen Zeilen in die Nachbarzeile.
            let abstand = senkrecht.saturating_mul(4096).saturating_add(waagerecht);
            if abstand < bester_abstand {
                bester_abstand = abstand;
                bester = index;
            }
        }
        // (2) Innerhalb des Laufs die Spalte suchen.
        let lauf = &self.laeufe[bester];
        lauf.start + zeichen_bei(lauf, x, metrik)
    }

    /// Die Rechtecke, die eine Auswahl auf dem Schirm bedeckt.
    ///
    /// Je beruehrtem Lauf EIN Rechteck — nicht eines je Zeichen. Eine
    /// Auswahl ueber drei Zeilen ergibt also mehrere Rechtecke, und
    /// genau das will der Renderer: Er malt sie einzeln, und an einem
    /// Zeilenumbruch entsteht von selbst die richtige Treppe.
    pub fn rechtecke(&self, a: Stelle, b: Stelle, metrik: &dyn Metrik) -> Vec<Rechteck> {
        let (von, bis) = spanne(a, b, self.text.len());
        let mut aus = Vec::new();
        if von == bis {
            return aus;
        }
        for lauf in &self.laeufe {
            let lauf_laenge = lauf.text.chars().count();
            let lauf_ende = lauf.start + lauf_laenge;
            // Beruehrt die Auswahl diesen Lauf ueberhaupt?
            if lauf_ende <= von || lauf.start >= bis {
                continue;
            }
            let erstes = von.saturating_sub(lauf.start).min(lauf_laenge);
            let letztes = (bis - lauf.start).min(lauf_laenge);
            let vor: String = lauf.text.chars().take(erstes).collect();
            let mitte: String = lauf.text.chars().skip(erstes).take(letztes - erstes).collect();
            let x0 = lauf.x + metrik.text_breite(&vor, lauf.groesse, lauf.fett, lauf.kursiv);
            let breite = metrik.text_breite(&mitte, lauf.groesse, lauf.fett, lauf.kursiv);
            if breite <= 0 {
                continue;
            }
            aus.push(Rechteck::neu(x0, lauf.y, breite, lauf.groesse));
        }
        aus
    }
}

/// Welches Zeichen eines Laufs liegt bei `x`?
///
/// Liefert einen Index von 0 bis `laenge` (einschliesslich!) — die
/// Position HINTER dem letzten Zeichen ist eine gueltige Cursor-Stelle,
/// sonst koennte man das letzte Wort einer Auswahl nie ganz einschliessen.
///
/// Gerundet wird zur naechsten ZEICHENGRENZE: Wer auf die linke Haelfte
/// eines Zeichens klickt, meint davor, wer auf die rechte klickt,
/// dahinter. Ohne das Runden fuehlt sich Auswahl immer um ein Zeichen
/// daneben an.
fn zeichen_bei(lauf: &Lauf, x: i32, metrik: &dyn Metrik) -> usize {
    let zeichen: Vec<char> = lauf.text.chars().collect();
    if x <= lauf.x {
        return 0;
    }
    let mut vorher = 0i32;
    for (index, _) in zeichen.iter().enumerate() {
        let bis_hier: String = zeichen.iter().take(index + 1).collect();
        let breite = metrik.text_breite(&bis_hier, lauf.groesse, lauf.fett, lauf.kursiv);
        let mitte = lauf.x + (vorher + breite) / 2;
        if x < mitte {
            return index;
        }
        vorher = breite;
    }
    zeichen.len()
}

// ===========================================================================
// SUCHEN (Aufgabe 4)
// ===========================================================================

/// Ein Zeichen fuer den Vergleich vereinheitlichen.
///
/// ===================================================================
/// GROSS/KLEIN — UND WARUM `to_lowercase` UND NICHT `to_ascii_lowercase`
///
/// Wer ASCII kleinschreibt, findet „STRASSE" mit „strasse", aber nicht
/// „ÜBER" mit „über" — das grosse Ü bleibt, wie es ist. Auf einer
/// deutschen Wikipedia-Seite ist das kein Randfall, sondern der
/// Normalfall. `char::to_lowercase` kennt Unicode und kostet uns nichts,
/// weil es in `core` steht.
///
/// EHRLICHE GRENZE: Es ist eine ZEICHENWEISE Abbildung. „ß" und „ss"
/// gelten damit als verschieden, und das deutsche „İ" der tuerkischen
/// Sonderregel bleibt unbehandelt. Eine echte Unicode-Faltung braucht
/// eine Tabelle, die groesser ist als unser halber Browser.
fn falten(c: char) -> char {
    // `to_lowercase` liefert einen Iterator (aus „İ" werden zwei
    // Zeichen). Wir nehmen das erste — fuer eine Suche reicht das, und
    // es haelt die Zeichenzaehlung stabil, an der die Trefferpositionen
    // haengen.
    c.to_lowercase().next().unwrap_or(c)
}

impl Textkarte {
    /// Alle Treffer eines Suchbegriffs — **ohne Ruecksicht auf
    /// Gross/Klein**.
    ///
    /// ===================================================================
    /// GESUCHT WIRD IM GESAMTTEXT, NICHT JE ANZEIGE-BEFEHL
    ///
    /// Das ist der Grund, warum es die Textkarte gibt. `speedlayout` gibt
    /// jedes Wort als eigenen Befehl aus; „Getting Started" steht also
    /// nie in einem Befehl. Eine Suche je Befehl faende zweiteilige
    /// Begriffe grundsaetzlich nicht — und niemand haette einen Verdacht,
    /// weil einzelne Woerter ja gefunden werden.
    ///
    /// UEBERLAPPENDE TREFFER GIBT ES NICHT: Nach einem Treffer geht die
    /// Suche hinter seinem ENDE weiter. „aa" in „aaa" ist damit EIN
    /// Treffer, nicht zwei. Das ist die Erwartung an ein Suchfeld —
    /// „weiter" soll vorankommen und nicht um ein Zeichen ruecken.
    pub fn suchen(&self, nadel: &str) -> Vec<Treffer> {
        let mut aus = Vec::new();
        let muster: Vec<char> = nadel.chars().map(falten).collect();
        if muster.is_empty() || muster.len() > self.text.len() {
            return aus;
        }
        let heu: Vec<char> = self.text.iter().copied().map(falten).collect();
        let mut i = 0usize;
        while i + muster.len() <= heu.len() {
            if heu[i..i + muster.len()] == muster[..] {
                aus.push(Treffer {
                    von: i,
                    bis: i + muster.len(),
                });
                i += muster.len();
            } else {
                i += 1;
            }
        }
        aus
    }
}
