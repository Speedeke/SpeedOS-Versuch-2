// speedui::text — TEXTROLLEN, GROESSENWAHL UND ZEILENUMBRUCH
//                 (Serie 8, Teil 3)
//
// ===========================================================================
// WOZU DAS HIER STEHT
//
// Ein Widget kommt mit EINER Schriftgroesse aus — `Mass::SchriftUi`, fertig.
// Ein HTML-Renderer nicht: `<h1>` bis `<h6>`, `<p>`, `<small>` und `<code>`
// stehen auf DERSELBEN Seite, und jedes will eine andere Groesse.
//
// Die Abbildung „welche Rolle bekommt welche Pixelhoehe" ist eine REINE
// FUNKTION, und deshalb steht sie hier und nicht im Renderer: Sie ist
// testbar, ohne dass irgendetwas gezeichnet wird, und sie ist an EINER
// Stelle, statt in jedem `<h3>`-Zweig noch einmal.
//
// ===========================================================================
// DIE EHRLICHE LAGE (ausfuehrlich in docs/schrift-groessen.md)
//
// SpeedOS hat VIER vorgerasterte Schriftgroessen — 16, 20, 24, 32 —, und
// mehr GIBT ES NICHT: `noto-sans-mono-bitmap` liefert genau diese vier
// (nachgesehen in der Cargo.toml der Kiste, nicht geraten). Daraus folgt
// zweierlei, und beides wird hier nicht schoengeredet:
//
//   (1) NACH OBEN reicht es. h1..h4 bekommen mit 32/24/20/16 vier
//       unterscheidbare Groessen — genau die Abstufung, die eine Seite
//       braucht, damit man Ueberschriften als Ueberschriften erkennt.
//
//   (2) NACH UNTEN reicht es NICHT. Die kleinste Groesse ist zugleich die
//       Fliesstextgroesse. `<small>`, `<h5>` und `<h6>` wollen KLEINER als
//       Fliesstext sein und koennen es nicht. Sie bekommen deshalb 16 —
//       und werden ueber das GEWICHT unterschieden (h5/h6 fett, small
//       normal). Das ist ein Ausweichmanoever und wird auch so benannt:
//       `Rolle::exakt_moeglich` sagt fuer sie `false`.
//
// Der Ausweg waere ein TrueType-Rasterizer (`ab_glyph`, `fontdue`). Das ist
// ein eigenes Vorhaben (Glyph-Cache, Hinting, Subpixel) und ausdruecklich
// NICHT dieser Teil — Serie 9.

use crate::umgebung::{Schrift, Stil};
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// ROLLEN
// ---------------------------------------------------------------------------

/// Wofuer ein Textstueck da ist — die Rollen, die HTML kennt.
///
/// EIN ENUM UND KEINE ZAHL, aus demselben Grund wie bei `Farbrolle`: Die
/// Liste dessen, was ein Renderer ausdruecken darf, ist abschliessend und
/// nachlesbar. Wer `<h7>` erfindet, tut es sichtbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rolle {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    /// Fliesstext — die Bezugsgroesse.
    P,
    /// `<small>`, Fussnoten, Bildunterschriften.
    Klein,
    /// `<code>`, `<pre>` — bei uns dieselbe Schrift wie alles andere
    /// (wir HABEN nur Monospace), aber eine eigene Rolle, damit ein
    /// spaeterer Proportional-Wirt sie unterscheiden kann.
    Code,
}

impl Rolle {
    /// Der Groessenfaktor in PROMILLE, bezogen auf die Fliesstextgroesse.
    ///
    /// PROMILLE UND KEIN `f32`: Unser Target hat `-sse,+soft-float` —
    /// Fliesskomma gibt es im Kernel nicht (`theme.rs` speichert die
    /// UI-Skalierung aus demselben Grund in HALBEN). Die Zahlen sind die
    /// Voreinstellungen jedes Browsers seit CSS 2.1: h1 2em, h2 1.5em,
    /// h3 1.17em, h4 1em, h5 0.83em, h6 0.67em, small 0.8em.
    pub const fn faktor_promille(self) -> i32 {
        match self {
            Rolle::H1 => 2000,
            Rolle::H2 => 1500,
            Rolle::H3 => 1170,
            Rolle::H4 => 1000,
            Rolle::H5 => 830,
            Rolle::H6 => 670,
            Rolle::P => 1000,
            Rolle::Klein => 800,
            Rolle::Code => 1000,
        }
    }

    /// Ist diese Rolle fett?
    ///
    /// h1..h6 sind es in jedem Browser. h5 und h6 sind es bei uns
    /// ZUSAETZLICH deshalb, weil ihre Groesse nicht unterscheidbar ist
    /// (siehe Kopfkommentar) — das Gewicht traegt dann die Unterscheidung
    /// allein.
    pub const fn fett(self) -> bool {
        matches!(
            self,
            Rolle::H1 | Rolle::H2 | Rolle::H3 | Rolle::H4 | Rolle::H5 | Rolle::H6
        )
    }

    /// Der Schnitt dieser Rolle.
    pub const fn stil(self) -> Stil {
        Stil::neu(self.fett(), false)
    }

    /// Der Name in HTML — fuer Fehlermeldungen und die Doku-Tabelle.
    pub const fn name(self) -> &'static str {
        match self {
            Rolle::H1 => "h1",
            Rolle::H2 => "h2",
            Rolle::H3 => "h3",
            Rolle::H4 => "h4",
            Rolle::H5 => "h5",
            Rolle::H6 => "h6",
            Rolle::P => "p",
            Rolle::Klein => "small",
            Rolle::Code => "code",
        }
    }

    /// Alle Rollen — fuer Tests und die Anzeige.
    pub const ALLE: [Rolle; 9] = [
        Rolle::H1,
        Rolle::H2,
        Rolle::H3,
        Rolle::H4,
        Rolle::H5,
        Rolle::H6,
        Rolle::P,
        Rolle::Klein,
        Rolle::Code,
    ];
}

/// Die WUNSCHGROESSE einer Rolle bei gegebener Fliesstextgroesse.
///
/// Reine Ganzzahl-Rechnung mit Aufrunden ab der halben Stufe. Das Ergebnis
/// ist die Groesse, die man HAETTE GERN — welche daraus wird, entscheidet
/// erst `groesse_fuer` mit dem Wirt.
pub fn wunschgroesse(rolle: Rolle, basis: i32) -> i32 {
    let basis = basis.max(1);
    ((basis * rolle.faktor_promille()) + 500) / 1000
}

/// Die TATSAECHLICHE Groesse einer Rolle bei diesem Wirt.
///
/// Wunsch ausrechnen, dann vom Wirt auf eine vorhandene runden lassen.
/// Die zwei Schritte bleiben getrennt, weil sie zwei verschiedene Fragen
/// beantworten — und weil man nur so PRUEFEN kann, wie viel beim Runden
/// verloren geht (`exakt_moeglich`).
pub fn groesse_fuer(rolle: Rolle, basis: i32, schrift: &dyn Schrift) -> i32 {
    schrift.groesse_waehlen(wunschgroesse(rolle, basis))
}

/// Bekommt diese Rolle bei diesem Wirt WIRKLICH ihre Wunschgroesse?
///
/// DIE FUNKTION, DIE DIE LUECKE SICHTBAR MACHT. Bei unseren vier Rastern
/// liefert sie fuer h5, h6 und `small` `false` — und genau das soll sie:
/// Eine Einschraenkung, die man abfragen kann, ist eine dokumentierte
/// Einschraenkung. Eine, die man nur bemerkt, wenn die Seite komisch
/// aussieht, ist ein Fehler.
pub fn exakt_moeglich(rolle: Rolle, basis: i32, schrift: &dyn Schrift) -> bool {
    wunschgroesse(rolle, basis) == groesse_fuer(rolle, basis, schrift)
}

// ---------------------------------------------------------------------------
// ZEILENUMBRUCH
// ---------------------------------------------------------------------------

/// Eine umbrochene Zeile: Anfang und Ende als BYTE-Offsets in den Text.
///
/// OFFSETS UND KEINE `String`s: Der Umbruch eines Absatzes soll nicht den
/// Absatz kopieren. Ein Renderer haelt den Quelltext ohnehin und schneidet
/// sich mit `&text[zeile.ab..zeile.bis]` heraus, was er braucht.
///
/// Die Offsets liegen IMMER auf Zeichengrenzen — sie stammen aus
/// `char_indices()`, nie aus einer Rechnung. Das ist der Grund, warum das
/// Schneiden nicht panickt, auch nicht bei „Grüße".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zeile {
    pub ab: usize,
    pub bis: usize,
    /// Breite dieser Zeile in Pixeln (schon gemessen — der Aufrufer soll
    /// nicht ein zweites Mal messen muessen, etwa zum Zentrieren).
    pub breite: i32,
}

/// Bricht `text` so um, dass keine Zeile breiter als `max_breite` wird.
///
/// ===================================================================
/// DAS IST DIE FUNKTION, FUER DIE ES DIE TEXTMETRIK GIBT
///
/// Ohne `text_breite` kann man nicht umbrechen — man kann nur RATEN
/// (Zeichen zaehlen und mit einer angenommenen Breite multiplizieren).
/// Bei Monospace geht das Raten sogar gut; bei jeder Proportionalschrift
/// bricht es sofort. Deshalb laeuft der Umbruch hier ausschliesslich ueber
/// `schrift.text_breite_stil` und nie ueber `chars().count()`.
///
/// ===================================================================
/// DIE REGELN
///
///  * Umbrochen wird an LEERZEICHEN. Die Leerzeichen am Zeilenende
///    gehoeren NICHT zur Zeile (sonst waere eine zentrierte Zeile
///    verschoben).
///  * Ein Wort, das allein schon zu breit ist, wird HART GETRENNT — sonst
///    liefe es aus dem Fenster hinaus. Eine sehr lange URL ist der
///    Normalfall dafuer, nicht die Ausnahme.
///  * `\n` im Text bricht IMMER um.
///  * `max_breite <= 0` liefert eine einzige Zeile mit dem ganzen Text:
///    Ein Fenster der Breite 0 soll keine Endlosschleife erzeugen.
///
/// Leerer Text liefert eine leere Liste — NICHT eine leere Zeile. Wer
/// Absaetze zaehlt, soll nicht ueber ein Phantom stolpern.
pub fn umbrechen(
    text: &str,
    max_breite: i32,
    groesse: i32,
    stil: Stil,
    schrift: &dyn Schrift,
) -> Vec<Zeile> {
    let mut zeilen = Vec::new();
    if text.is_empty() {
        return zeilen;
    }
    if max_breite <= 0 {
        zeilen.push(Zeile {
            ab: 0,
            bis: text.len(),
            breite: schrift.text_breite_stil(text, groesse, stil),
        });
        return zeilen;
    }

    // Anfang der laufenden Zeile, und das letzte gesehene Trennrecht
    // (Byte-Offset NACH dem Leerzeichen bzw. AUF dem Leerzeichen).
    let mut zeilen_ab = 0usize;
    let mut trenn_bei: Option<(usize, usize)> = None; // (bis_ohne_leer, weiter_ab)

    let mut i = 0usize;
    let zeichen: Vec<(usize, char)> = text.char_indices().collect();
    let mut k = 0usize;

    while k < zeichen.len() {
        let (offset, c) = zeichen[k];

        if c == '\n' {
            zeilen.push(fertige_zeile(text, zeilen_ab, offset, groesse, stil, schrift));
            zeilen_ab = offset + c.len_utf8();
            trenn_bei = None;
            k += 1;
            i = zeilen_ab;
            continue;
        }

        if c == ' ' {
            // Hier DUERFTE man umbrechen: bis `offset` (ohne das
            // Leerzeichen), weiter ab dahinter.
            trenn_bei = Some((offset, offset + c.len_utf8()));
        }

        // Breite bis EINSCHLIESSLICH dieses Zeichens.
        let bis = offset + c.len_utf8();
        let breite = schrift.text_breite_stil(&text[zeilen_ab..bis], groesse, stil);

        if breite > max_breite && bis > zeilen_ab {
            match trenn_bei {
                // Es gab ein Leerzeichen — dort trennen.
                Some((ende, weiter)) if ende > zeilen_ab => {
                    zeilen.push(fertige_zeile(text, zeilen_ab, ende, groesse, stil, schrift));
                    zeilen_ab = weiter;
                    trenn_bei = None;
                    // Zurueck an die Stelle hinter dem Trenner.
                    k = zeichen
                        .iter()
                        .position(|&(o, _)| o >= zeilen_ab)
                        .unwrap_or(zeichen.len());
                    i = zeilen_ab;
                    continue;
                }
                // Kein Leerzeichen in dieser Zeile: HART trennen, und
                // zwar VOR dem Zeichen, das zu viel war. Ist es das
                // erste Zeichen der Zeile, muss es trotzdem hinein —
                // sonst kaeme die Schleife nie voran.
                _ => {
                    let ende = if offset > zeilen_ab { offset } else { bis };
                    zeilen.push(fertige_zeile(text, zeilen_ab, ende, groesse, stil, schrift));
                    zeilen_ab = ende;
                    trenn_bei = None;
                    k = zeichen
                        .iter()
                        .position(|&(o, _)| o >= zeilen_ab)
                        .unwrap_or(zeichen.len());
                    i = zeilen_ab;
                    continue;
                }
            }
        }

        k += 1;
        i = bis;
    }

    if zeilen_ab < text.len() || zeilen.is_empty() {
        zeilen.push(fertige_zeile(text, zeilen_ab, text.len(), groesse, stil, schrift));
    }
    let _ = i;
    zeilen
}

/// Eine Zeile abschliessen — Leerzeichen am Ende abschneiden und messen.
fn fertige_zeile(
    text: &str,
    ab: usize,
    bis: usize,
    groesse: i32,
    stil: Stil,
    schrift: &dyn Schrift,
) -> Zeile {
    let roh = &text[ab..bis];
    let getrimmt = roh.trim_end_matches(' ');
    let bis = ab + getrimmt.len();
    Zeile {
        ab,
        bis,
        breite: schrift.text_breite_stil(getrimmt, groesse, stil),
    }
}

// ---------------------------------------------------------------------------
// TESTS
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrappe::{TestSchrift, VierRaster};

    // -- Textmetrik --------------------------------------------------------

    /// DER UMLAUT-TEST. `TestSchrift` ist monospace mit Breite
    /// `groesse / 2`, bei Groesse 16 also 8 Pixel je ZEICHEN.
    ///
    /// Der Fehler, den dieser Test faengt, ist `len()` statt
    /// `chars().count()`: „Grüße" hat 5 Zeichen, aber 7 Bytes (ü und ß
    /// brauchen je zwei). Wer Bytes zaehlt, bekommt 56 statt 40 Pixel —
    /// und bricht jede deutsche Zeile zu frueh um.
    #[test]
    fn test_textbreite_zaehlt_zeichen_nicht_bytes() {
        let s = TestSchrift;
        assert_eq!(s.zeichen_breite(16), 8);

        assert_eq!(s.text_breite("", 16), 0);
        assert_eq!(s.text_breite("abc", 16), 24);
        // 5 Zeichen, 7 Bytes.
        assert_eq!("Grüße".len(), 7);
        assert_eq!("Grüße".chars().count(), 5);
        assert_eq!(s.text_breite("Grüße", 16), 40);

        // Alle deutschen Sonderzeichen, einzeln und zusammen.
        for z in ["ä", "ö", "ü", "Ä", "Ö", "Ü", "ß"] {
            assert_eq!(s.text_breite(z, 16), 8, "{z} ist EIN Zeichen");
        }
        assert_eq!(s.text_breite("äöüÄÖÜß", 16), 56);

        // Und etwas jenseits von Latin-1, damit die Rechnung nicht nur
        // fuer Zwei-Byte-Folgen stimmt: € ist 3 Bytes, 😀 sind 4.
        assert_eq!("€".len(), 3);
        assert_eq!(s.text_breite("€", 16), 8);
        assert_eq!("😀".len(), 4);
        assert_eq!(s.text_breite("a😀b", 16), 24);
    }

    #[test]
    fn test_textbreite_skaliert_mit_der_groesse() {
        let s = TestSchrift;
        assert_eq!(s.text_breite("Grüße", 16), 40);
        assert_eq!(s.text_breite("Grüße", 32), 80);
    }

    // -- Groessenwahl ------------------------------------------------------

    #[test]
    fn test_wunschgroesse_folgt_den_css_faktoren() {
        // Basis 16 — die Zahlen aus jedem Browser.
        assert_eq!(wunschgroesse(Rolle::H1, 16), 32);
        assert_eq!(wunschgroesse(Rolle::H2, 16), 24);
        assert_eq!(wunschgroesse(Rolle::H3, 16), 19); // 18.72 aufgerundet
        assert_eq!(wunschgroesse(Rolle::H4, 16), 16);
        assert_eq!(wunschgroesse(Rolle::H5, 16), 13); // 13.28
        assert_eq!(wunschgroesse(Rolle::H6, 16), 11); // 10.72
        assert_eq!(wunschgroesse(Rolle::P, 16), 16);
        assert_eq!(wunschgroesse(Rolle::Klein, 16), 13); // 12.8
    }

    #[test]
    fn test_groesse_waehlen_rundet_auf_vorhandene() {
        let s = VierRaster;
        assert_eq!(s.groessen(), &[16, 20, 24, 32]);

        // Genau vorhandene bleiben.
        for g in [16, 20, 24, 32] {
            assert_eq!(s.groesse_waehlen(g), g);
        }
        // Dazwischen: das naechstliegende.
        assert_eq!(s.groesse_waehlen(17), 16);
        assert_eq!(s.groesse_waehlen(19), 20);
        assert_eq!(s.groesse_waehlen(21), 20);
        assert_eq!(s.groesse_waehlen(23), 24);
        assert_eq!(s.groesse_waehlen(29), 32);
        // Bei GLEICHSTAND die kleinere (18 liegt zwischen 16 und 20).
        assert_eq!(s.groesse_waehlen(18), 16);
        assert_eq!(s.groesse_waehlen(22), 20);
        assert_eq!(s.groesse_waehlen(28), 24);
        // Ausserhalb: geklemmt, nicht extrapoliert.
        assert_eq!(s.groesse_waehlen(1), 16);
        assert_eq!(s.groesse_waehlen(500), 32);
    }

    /// DER TEST, DER DIE LUECKE FESTNAGELT.
    ///
    /// Er behauptet NICHT, dass alles gut ist — er schreibt fest, WAS
    /// geht und was nicht. Faellt er, hat sich der Font-Bestand geaendert,
    /// und dann gehoert docs/schrift-groessen.md nachgezogen.
    #[test]
    fn test_die_vier_raster_reichen_nach_oben_und_nicht_nach_unten() {
        let s = VierRaster;

        // NACH OBEN: vier unterscheidbare Stufen.
        assert_eq!(groesse_fuer(Rolle::H1, 16, &s), 32);
        assert_eq!(groesse_fuer(Rolle::H2, 16, &s), 24);
        assert_eq!(groesse_fuer(Rolle::H3, 16, &s), 20);
        assert_eq!(groesse_fuer(Rolle::H4, 16, &s), 16);
        assert!(exakt_moeglich(Rolle::H1, 16, &s));
        assert!(exakt_moeglich(Rolle::H2, 16, &s));
        assert!(exakt_moeglich(Rolle::H4, 16, &s));
        // h3 will 19 und bekommt 20 — nah genug, aber nicht exakt.
        assert!(!exakt_moeglich(Rolle::H3, 16, &s));

        // NACH UNTEN: nichts zu holen. Alle drei landen auf der
        // Fliesstextgroesse.
        assert_eq!(groesse_fuer(Rolle::H5, 16, &s), 16);
        assert_eq!(groesse_fuer(Rolle::H6, 16, &s), 16);
        assert_eq!(groesse_fuer(Rolle::Klein, 16, &s), 16);
        assert_eq!(groesse_fuer(Rolle::P, 16, &s), 16);
        assert!(!exakt_moeglich(Rolle::H5, 16, &s));
        assert!(!exakt_moeglich(Rolle::H6, 16, &s));
        assert!(!exakt_moeglich(Rolle::Klein, 16, &s));

        // Deshalb traegt das GEWICHT die Unterscheidung von h5/h6.
        assert!(Rolle::H5.fett());
        assert!(Rolle::H6.fett());
        assert!(!Rolle::Klein.fett());
        assert!(!Rolle::P.fett());
    }

    /// Bei UI-Skalierung 2.0 (Basis 32) ist die Lage umgekehrt: Nach
    /// unten wird es besser, nach oben geht der Vorrat aus.
    #[test]
    fn test_bei_grosser_basis_geht_der_vorrat_nach_oben_aus() {
        let s = VierRaster;
        assert_eq!(groesse_fuer(Rolle::H1, 32, &s), 32); // will 64, bekommt 32
        assert_eq!(groesse_fuer(Rolle::H2, 32, &s), 32); // will 48
        assert_eq!(groesse_fuer(Rolle::P, 32, &s), 32);
        assert!(!exakt_moeglich(Rolle::H1, 32, &s));
        // Nach unten dafuer sauber: small will 26 und bekommt 24.
        assert_eq!(groesse_fuer(Rolle::Klein, 32, &s), 24);
        assert_eq!(groesse_fuer(Rolle::H6, 32, &s), 20);
    }

    #[test]
    fn test_fett_und_kursiv_werden_ehrlich_gemeldet() {
        let s = VierRaster;
        // Der Testwirt bildet den Kernel nach: echtes Fett, kein Kursiv.
        assert!(s.fett_echt());
        assert!(!s.kursiv_echt());
    }

    // -- Umbruch -----------------------------------------------------------

    #[test]
    fn test_umbruch_leer_und_kurz() {
        let s = TestSchrift;
        assert!(umbrechen("", 100, 16, Stil::NORMAL, &s).is_empty());

        let z = umbrechen("kurz", 100, 16, Stil::NORMAL, &s);
        assert_eq!(z.len(), 1);
        assert_eq!(z[0].ab, 0);
        assert_eq!(z[0].bis, 4);
        assert_eq!(z[0].breite, 32);
    }

    #[test]
    fn test_umbruch_an_leerzeichen() {
        let s = TestSchrift;
        // Breite 8 je Zeichen, max 80 => 10 Zeichen je Zeile.
        let text = "eins zwei drei vier";
        let zeilen = umbrechen(text, 80, 16, Stil::NORMAL, &s);
        let stuecke: Vec<&str> = zeilen.iter().map(|z| &text[z.ab..z.bis]).collect();
        assert_eq!(stuecke, ["eins zwei", "drei vier"]);
        // Das trennende Leerzeichen gehoert zu KEINER Zeile.
        for z in &zeilen {
            assert!(!text[z.ab..z.bis].ends_with(' '));
            assert!(z.breite <= 80);
        }
    }

    #[test]
    fn test_umbruch_bricht_an_zeilenumbruch() {
        let s = TestSchrift;
        let text = "a\nb\nc";
        let zeilen = umbrechen(text, 800, 16, Stil::NORMAL, &s);
        let stuecke: Vec<&str> = zeilen.iter().map(|z| &text[z.ab..z.bis]).collect();
        assert_eq!(stuecke, ["a", "b", "c"]);
    }

    /// Ein Wort, das nicht passt, wird HART getrennt — sonst liefe es aus
    /// dem Fenster. Eine lange URL ist genau dieser Fall.
    #[test]
    fn test_umbruch_trennt_zu_langes_wort_hart() {
        let s = TestSchrift;
        let text = "AAAAAAAAAAAAAAA"; // 15 Zeichen a 8 = 120 px
        let zeilen = umbrechen(text, 40, 16, Stil::NORMAL, &s); // 5 je Zeile
        assert_eq!(zeilen.len(), 3);
        for z in &zeilen {
            assert!(z.breite <= 40, "Zeile zu breit: {}", z.breite);
        }
        let zusammen: alloc::string::String =
            zeilen.iter().map(|z| &text[z.ab..z.bis]).collect();
        assert_eq!(zusammen, text, "es darf kein Zeichen verlorengehen");
    }

    /// DER UMLAUT-UMBRUCH: Die Schnittstellen muessen auf Zeichengrenzen
    /// liegen, sonst panickt das Schneiden. Der Text besteht absichtlich
    /// aus Mehrbyte-Zeichen, damit ein byte-basierter Umbruch mitten in
    /// einem Zeichen landen WUERDE.
    #[test]
    fn test_umbruch_schneidet_nie_in_ein_zeichen() {
        let s = TestSchrift;
        let text = "Grüße öffnen Türen für größere Läden";
        for max in [8, 16, 24, 40, 56, 80, 200] {
            let zeilen = umbrechen(text, max, 16, Stil::NORMAL, &s);
            let mut zusammen = alloc::string::String::new();
            for z in &zeilen {
                // Panickt, wenn ab/bis nicht auf Zeichengrenzen liegen.
                assert!(text.is_char_boundary(z.ab), "ab={} max={}", z.ab, max);
                assert!(text.is_char_boundary(z.bis), "bis={} max={}", z.bis, max);
                zusammen.push_str(&text[z.ab..z.bis]);
            }
            // Kein Zeichen darf verlorengehen (ausser den Trenn-Leerzeichen).
            let ohne_leer: alloc::string::String =
                text.chars().filter(|&c| c != ' ').collect();
            let zusammen_ohne: alloc::string::String =
                zusammen.chars().filter(|&c| c != ' ').collect();
            assert_eq!(zusammen_ohne, ohne_leer, "max={max}");
        }
    }

    #[test]
    fn test_umbruch_ohne_breite_liefert_eine_zeile() {
        let s = TestSchrift;
        let zeilen = umbrechen("egal was", 0, 16, Stil::NORMAL, &s);
        assert_eq!(zeilen.len(), 1);
    }

    /// Der Umbruch benutzt die STIL-abhaengige Metrik. Bei unserer
    /// Monospace-Schrift kommt dasselbe heraus — der Test haelt fest, DASS
    /// er sie benutzt, damit ein spaeterer Proportional-Wirt nicht
    /// stillschweigend falsch umbricht.
    #[test]
    fn test_umbruch_benutzt_die_stil_metrik() {
        let s = BreiteresFett;
        // 9 Zeichen a 8 Pixel = 72 normal, 144 fett. Die Schranke liegt
        // GENAU auf der normalen Breite: normal passt gerade noch.
        let text = "aaaa aaaa";
        let normal = umbrechen(text, 72, 16, Stil::NORMAL, &s);
        let fett = umbrechen(text, 72, 16, Stil::FETT, &s);
        // Fett ist bei diesem Wirt doppelt so breit — es passt weniger.
        assert_eq!(normal.len(), 1, "normal passt in eine Zeile");
        assert!(fett.len() > 1, "fett muss umbrechen");
    }

    /// Ein Wirt, bei dem fett WIRKLICH breiter ist — es gibt ihn nur hier,
    /// um zu beweisen, dass `umbrechen` die Stil-Metrik befragt.
    struct BreiteresFett;
    impl Schrift for BreiteresFett {
        fn zeichen_breite(&self, groesse: i32) -> i32 {
            groesse / 2
        }
        fn zeilen_hoehe(&self, groesse: i32) -> i32 {
            groesse
        }
        fn text_breite_stil(&self, text: &str, groesse: i32, stil: Stil) -> i32 {
            let breite = self.text_breite(text, groesse);
            if stil.fett {
                breite * 2
            } else {
                breite
            }
        }
    }
}
