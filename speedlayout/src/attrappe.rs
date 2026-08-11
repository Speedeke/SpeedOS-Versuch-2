// speedlayout::attrappe — eine Metrik, mit der man RECHNEN kann
//
// ===========================================================================
// DER GRUND, WARUM ES SIE GIBT
//
// Ein Layout-Test soll behaupten koennen:
//
//     „Bei 100 px Breite und 10 px je Zeichen bricht `aaa bbb ccc`
//      nach `aaa bbb` um."
//
// Das ist eine Aussage, die man VOR dem Lauf hinschreibt und die falsch
// sein kann. Mit einer echten Schrift ginge das nicht: Man wuesste nicht,
// wie breit `aaa bbb` ist, also schriebe man die Zahl aus dem Ergebnis ab
// — und ein Test, dessen Erwartung aus dem Ergebnis stammt, prueft
// nichts.
//
// Deshalb: **feste Zeichenbreite, feste Zeilenhoehe, feste Grundlinie.**
// Alle Zahlen in den Tests sind damit von Hand nachrechenbar.
//
// Sie ist bewusst KEIN `#[cfg(test)]` — dieselbe Entscheidung wie bei
// `speedui::attrappe`: Ein Wirt in Pappe ist auch ausserhalb von Tests
// nuetzlich (ein Layout durchrechnen, ohne zu zeichnen).

use crate::Metrik;

/// Eine Metrik mit fester Zeichenbreite.
///
/// `breite_je_zeichen` gilt bei `bezugsgroesse`; bei anderen Groessen
/// wird proportional gerechnet. Damit bleibt „doppelte Schrift, doppelte
/// Breite" wahr, ohne dass die Tests rechnen muessen.
pub struct FesteMetrik {
    pub bezugsgroesse: i32,
    pub breite_je_zeichen: i32,
    /// Zeilenhoehe bei `bezugsgroesse`.
    pub zeilenhoehe: i32,
    /// Grundlinie bei `bezugsgroesse`.
    pub grundlinie: i32,
    /// Macht fetten Text breiter (fuer den Test, DASS der Stil ankommt).
    pub fett_faktor_promille: i32,
}

impl FesteMetrik {
    /// Die uebliche Attrappe: 16 px Schrift, **10 px je Zeichen**,
    /// 20 px Zeilenhoehe, Grundlinie bei 12.
    ///
    /// Runde Zahlen mit Absicht — sie machen jede Erwartung in den Tests
    /// zu einer Kopfrechnung.
    pub const fn neu() -> FesteMetrik {
        FesteMetrik {
            bezugsgroesse: 16,
            breite_je_zeichen: 10,
            zeilenhoehe: 20,
            grundlinie: 12,
            fett_faktor_promille: 1000,
        }
    }

    /// Wie `neu`, aber fett ist 1,5x so breit.
    pub const fn mit_breitem_fett() -> FesteMetrik {
        FesteMetrik {
            fett_faktor_promille: 1500,
            ..FesteMetrik::neu()
        }
    }

    fn skalieren(&self, wert: i32, groesse: i32) -> i32 {
        if groesse == self.bezugsgroesse {
            return wert;
        }
        ((wert as i64 * groesse as i64) / self.bezugsgroesse.max(1) as i64) as i32
    }
}

impl Default for FesteMetrik {
    fn default() -> Self {
        FesteMetrik::neu()
    }
}

impl Metrik for FesteMetrik {
    /// **`chars().count()` und niemals `len()`.**
    ///
    /// „Grüße" hat 5 Zeichen und 7 Bytes. Wer Bytes zaehlt, bricht jede
    /// deutsche Zeile zu frueh um — derselbe Fehler, den schon
    /// `speedui::text` festnagelt, und hier faellt er noch einmal an,
    /// weil das Layout eine eigene Metrik-Naht hat.
    fn text_breite(&self, text: &str, groesse: i32, fett: bool, _kursiv: bool) -> i32 {
        let je_zeichen = self.skalieren(self.breite_je_zeichen, groesse);
        let roh = text.chars().count() as i32 * je_zeichen;
        if fett {
            ((roh as i64 * self.fett_faktor_promille as i64) / 1000) as i32
        } else {
            roh
        }
    }

    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        self.skalieren(self.zeilenhoehe, groesse)
    }

    fn grundlinie(&self, groesse: i32) -> i32 {
        self.skalieren(self.grundlinie, groesse)
    }
}

/// Eine Metrik, die nur VIER Groessen kann — wie der Kernel
/// (docs/schrift-groessen.md).
///
/// Sie ist nicht die bequeme Wahl fuer Layout-Tests (die Zahlen springen),
/// sondern die ehrliche fuer die Frage „was passiert, wenn die gewuenschte
/// Groesse gar nicht existiert?". Genau dafuer gibt es
/// `Metrik::groesse_waehlen`.
pub struct VierGroessen;

/// Die Raster, die der Kernel wirklich hat.
pub const RASTER: &[i32] = &[16, 20, 24, 32];

impl Metrik for VierGroessen {
    fn text_breite(&self, text: &str, groesse: i32, _fett: bool, _kursiv: bool) -> i32 {
        // Halbe Schriftgroesse je Zeichen — die Faustregel unserer
        // Monospace-Raster (16 -> 8).
        text.chars().count() as i32 * (self.groesse_waehlen(groesse) / 2).max(1)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        self.groesse_waehlen(groesse) + 4
    }
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        let mut beste = RASTER[0];
        let mut abstand = (beste - wunsch).abs();
        for &g in &RASTER[1..] {
            let d = (g - wunsch).abs();
            // `<` und nicht `<=`: Bei Gleichstand gewinnt die kleinere.
            if d < abstand {
                beste = g;
                abstand = d;
            }
        }
        beste
    }
}
