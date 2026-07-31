// speedui::typen — Die Typen, die in jeder Signatur stehen
//
// Rechteck, Farbe, Icon und Taste sind REINE DATEN ohne Wirts-Bezug. Sie
// muessen deshalb nicht hinter einem Trait verschwinden — sie ziehen mit,
// und der Kernel re-exportiert sie unter seinen alten Namen
// (`grafik::Rechteck`), damit sich in Fenster-Manager und Apps keine Zeile
// aendert.
//
// Die Alternative — je einen eigenen Typ auf beiden Seiten und Konvertierung
// an der Grenze — waere bei Typen, die in JEDER Signatur vorkommen, Laerm
// ohne Gewinn. Bei `Taste` ist es anders, siehe unten.

// ---------------------------------------------------------------------------
// Rechteck (Wort fuer Wort aus src/grafik.rs, nur der Ort ist neu)
// ---------------------------------------------------------------------------

/// Ein achsenparalleles Rechteck. Alle UI-Koordinaten sind
/// FENSTERINHALT-Koordinaten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rechteck {
    pub x: i32,
    pub y: i32,
    pub breite: i32,
    pub hoehe: i32,
}

impl Rechteck {
    pub const fn neu(x: i32, y: i32, breite: i32, hoehe: i32) -> Self {
        Rechteck { x, y, breite, hoehe }
    }

    /// Liegt der Punkt (x, y) innerhalb?
    pub fn enthaelt(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.breite && y < self.y + self.hoehe
    }

    /// Schnittmenge zweier Rechtecke (None = keine Ueberlappung).
    /// DAS Herzstueck des Clippings — reine Funktion, unit-getestet.
    pub fn schneiden(&self, anderes: &Rechteck) -> Option<Rechteck> {
        let x = self.x.max(anderes.x);
        let y = self.y.max(anderes.y);
        let rechts = (self.x + self.breite).min(anderes.x + anderes.breite);
        let unten = (self.y + self.hoehe).min(anderes.y + anderes.hoehe);
        if x < rechts && y < unten {
            Some(Rechteck::neu(x, y, rechts - x, unten - y))
        } else {
            None
        }
    }

    /// Kleinstes Rechteck, das BEIDE umschliesst (Bounding-Box-Union).
    /// Ueber-Deckung ist erlaubt (Korrektheit vor Optimum).
    pub fn umschliessen(&self, anderes: &Rechteck) -> Rechteck {
        let x = self.x.min(anderes.x);
        let y = self.y.min(anderes.y);
        let rechts = (self.x + self.breite).max(anderes.x + anderes.breite);
        let unten = (self.y + self.hoehe).max(anderes.y + anderes.hoehe);
        Rechteck::neu(x, y, rechts - x, unten - y)
    }
}

// ---------------------------------------------------------------------------
// Farbe
// ---------------------------------------------------------------------------

/// Eine Farbe mit Alpha-Kanal (255 = voll deckend).
///
/// EINE Farbe, nicht zwei: Der Kernel kennt `Farbe` (RGB, der
/// Framebuffer-Typ) UND `Rgba` (der Zeichner-Typ). speedui nimmt nur RGBA,
/// weil Alpha in der UI gebraucht wird (Hover-Flaechen). Der Wirt
/// konvertiert an seiner Leinwand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Farbe {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Farbe {
    pub const fn neu(r: u8, g: u8, b: u8) -> Self {
        Farbe { r, g, b, a: 255 }
    }
    pub const fn mit_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Farbe { r, g, b, a }
    }
    /// Dieselbe Farbe mit anderer Deckkraft.
    pub const fn alpha(self, a: u8) -> Self {
        Farbe { a, ..self }
    }
    /// Linear mischen (t: 0 = self, 255 = ziel). Rechnung in i32 —
    /// `delta * 255` sprengt i16.
    pub fn mischen(self, ziel: Farbe, t: u8) -> Farbe {
        let m = |a: u8, b: u8| -> u8 {
            let (a, b) = (a as i32, b as i32);
            (a + (b - a) * t as i32 / 255) as u8
        };
        Farbe {
            r: m(self.r, ziel.r),
            g: m(self.g, ziel.g),
            b: m(self.b, ziel.b),
            a: m(self.a, ziel.a),
        }
    }
}

// ---------------------------------------------------------------------------
// Icon
// ---------------------------------------------------------------------------

/// Ein 16x16-Icon als ASCII-Art. Reine Daten — der TYP zieht mit, die
/// konkreten Icons bleiben beim jeweiligen Wirt (sie gehoeren zu seinem
/// Erscheinungsbild).
pub struct Icon {
    pub zeilen: [&'static str; 16],
}

/// Die gemeinsame Icon-Palette. Reine Funktion, also zieht sie mit —
/// sonst saehe dasselbe Icon in zwei Wirten verschieden aus.
///
/// `None` = transparent. Ein UNBEKANNTES Zeichen wird MAGENTA und damit
/// zu einem sichtbaren Tippfehler (dasselbe Prinzip wie im Kernel).
pub fn icon_farbe(zeichen: char) -> Option<Farbe> {
    match zeichen {
        '.' => None,                              // transparent
        'w' => Some(Farbe::neu(0xf8, 0xfa, 0xfc)), // Weiss
        'h' => Some(Farbe::neu(0xc4, 0xca, 0xd6)), // Hellgrau
        'd' => Some(Farbe::neu(0x56, 0x5f, 0x73)), // Dunkelgrau
        'D' => Some(Farbe::neu(0x1a, 0x20, 0x29)), // Fast-Schwarz
        'g' => Some(Farbe::neu(0xb4, 0x53, 0x09)), // Gold dunkel
        'G' => Some(Farbe::neu(0xfb, 0xbf, 0x24)), // Gold hell
        'v' => Some(Farbe::neu(0x7c, 0x3a, 0xed)), // Aurora-Violett
        'b' => Some(Farbe::neu(0x3b, 0x82, 0xf6)), // Aurora-Blau
        'c' => Some(Farbe::neu(0x22, 0xd3, 0xee)), // Aurora-Cyan
        _ => Some(Farbe::neu(0xff, 0x00, 0xff)),   // Magenta = Tippfehler im Icon
    }
}

// ---------------------------------------------------------------------------
// Taste — DER Eingabe-Typ, und die einzige bewusste Doppelarbeit
// ---------------------------------------------------------------------------

/// Eine Taste, wie das Toolkit sie sieht.
///
/// WARUM NICHT `pc_keyboard::DecodedKey`: Der Kernel dekodiert dort
/// Scancodes; ein Ring-3-Prozess bekommt ueber `fenster_ereignis` (Serie 8,
/// Teil 1) schon fertige Unicode-Zeichen und Sondertasten-Codes und hat
/// `pc_keyboard` nie gesehen. Ein gemeinsamer Typ haette die Kiste an eine
/// Tastatur-Kiste gebunden, die nur eine Seite braucht.
///
/// Das ist die EINZIGE Stelle, an der beide Wirte dasselbe zweimal tun —
/// und es ist Absicht, denn sie tun es aus verschiedenen Quellen. Dasselbe
/// Argument wie bei der ABI in docs/syscalls.md: ein Vertrag, kein
/// geteilter Header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taste {
    /// Ein Zeichen — inklusive der Steuerzeichen, die eine Tastatur
    /// liefert: `\n` (Enter), `\t` (Tab), `\u{8}` (Rueck), `\u{3}` (Strg+C).
    Zeichen(char),
    Hoch,
    Runter,
    Links,
    Rechts,
    Pos1,
    Ende,
    BildHoch,
    BildRunter,
    Entf,
    /// Funktionstaste 1..12.
    F(u8),
}

impl Taste {
    /// Das Zeichen, falls es eines ist.
    pub fn zeichen(self) -> Option<char> {
        match self {
            Taste::Zeichen(c) => Some(c),
            _ => None,
        }
    }
    /// Ist es die Tabulator-Taste? (Sie schaltet die Fokus-Kette weiter.)
    pub fn ist_tab(self) -> bool {
        matches!(self, Taste::Zeichen('\t'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rechteck_enthaelt_und_schneiden() {
        let r = Rechteck::neu(10, 10, 100, 50);
        assert!(r.enthaelt(10, 10));
        assert!(r.enthaelt(109, 59));
        assert!(!r.enthaelt(9, 10));
        assert!(!r.enthaelt(110, 10));
        assert!(!r.enthaelt(10, 60));

        // Ueberlappung
        let a = Rechteck::neu(0, 0, 100, 100);
        let b = Rechteck::neu(50, 50, 100, 100);
        assert_eq!(a.schneiden(&b), Some(Rechteck::neu(50, 50, 50, 50)));
        // Beruehrung ist KEINE Ueberlappung (halboffene Intervalle):
        let c = Rechteck::neu(100, 0, 10, 10);
        assert_eq!(a.schneiden(&c), None);
    }

    #[test]
    fn test_rechteck_umschliessen() {
        let a = Rechteck::neu(10, 10, 10, 10);
        let b = Rechteck::neu(50, 60, 10, 10);
        assert_eq!(a.umschliessen(&b), Rechteck::neu(10, 10, 50, 60));
        // Mit sich selbst: unveraendert.
        assert_eq!(a.umschliessen(&a), a);
    }

    #[test]
    fn test_farbe_mischen() {
        let schwarz = Farbe::neu(0, 0, 0);
        let weiss = Farbe::neu(255, 255, 255);
        assert_eq!(schwarz.mischen(weiss, 0), schwarz);
        assert_eq!(schwarz.mischen(weiss, 255), weiss);
        let mitte = schwarz.mischen(weiss, 128);
        assert_eq!(mitte.r, 128);
    }

    /// Ein unbekanntes Icon-Zeichen wird MAGENTA — ein Tippfehler soll
    /// auffallen, nicht verschwinden.
    #[test]
    fn test_icon_palette_meldet_tippfehler() {
        assert_eq!(icon_farbe('.'), None);
        assert_eq!(icon_farbe('w'), Some(Farbe::neu(0xf8, 0xfa, 0xfc)));
        assert_eq!(icon_farbe('§'), Some(Farbe::neu(0xff, 0x00, 0xff)));
    }

    #[test]
    fn test_taste_hilfen() {
        assert_eq!(Taste::Zeichen('a').zeichen(), Some('a'));
        assert_eq!(Taste::Hoch.zeichen(), None);
        assert!(Taste::Zeichen('\t').ist_tab());
        assert!(!Taste::Zeichen(' ').ist_tab());
    }
}
