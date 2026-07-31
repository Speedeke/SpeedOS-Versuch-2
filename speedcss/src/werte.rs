// speedcss::werte — Laengen, Farben, Schluesselwoerter
//
// ===========================================================================
// WARUM HIER KEIN `f32` STEHT
//
// Unser Target hat `-sse,+soft-float` (userland/.cargo/config.toml) —
// Fliesskomma gibt es in SpeedOS nicht, weil der Kontext-Wechsel keine
// XMM-Register sichert. CSS ist aber voller Bruchzahlen: `0.5em`,
// `line-height: 1.5`, `font-size: 62.5%`.
//
// Geloest wie die UI-Skalierung im Kernel (die in HALBEN rechnet) und wie
// der Zoom im Bildbetrachter (ein Bruch aus zwei ganzen Zahlen): **alles
// in TAUSENDSTELN**. `1.5em` ist 1500, `62.5%` ist 62500, `0.5px` ist 500.
// Drei Nachkommastellen sind mehr, als jede Seite je braucht, und der
// groesste darstellbare Wert (i32) sind gut zwei Millionen Pixel.
//
// ===========================================================================
// COMPUTED VALUE vs. USED VALUE — die Unterscheidung, die alles traegt
//
// CSS kennt zwei Zeitpunkte, an denen Werte konkret werden, und sie hier
// zu verwechseln ist der teuerste Fehler:
//
//   * **Computed value** (Kaskadenzeit, hier): `em` ist aufgeloest, weil
//     die Schriftgroesse des Elternteils bekannt ist. `font-size` wird zu
//     einer festen Pixelzahl.
//   * **Used value** (Layoutzeit, spaeter): `%` und `auto` sind aufgeloest,
//     weil erst dann feststeht, wie breit der umgebende Kasten ist.
//
// Deshalb bleibt `Laenge::Prozent` und `Laenge::Auto` im berechneten Stil
// STEHEN. Ein Parser, der `width: 50%` schon in der Kaskade zu einer Zahl
// macht, muesste die Breite des Elternteils raten — und raet falsch,
// sobald das Fenster seine Groesse aendert.

use alloc::string::String;
use alloc::vec::Vec;

/// Der Nenner aller Bruchzahlen. Alles in dieser Datei ist ein
/// Tausendstel seiner Einheit.
pub const TAUSEND: i32 = 1000;

// ---------------------------------------------------------------------------
// LAENGEN
// ---------------------------------------------------------------------------

/// Eine Laengenangabe, wie sie im Stylesheet steht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Laenge {
    /// Absolut, in 1/1000 Pixel.
    Px(i32),
    /// Relativ zur Schriftgroesse DIESES Elements, in 1/1000 em.
    Em(i32),
    /// Relativ zu etwas, das erst das Layout kennt, in 1/1000 Prozent.
    Prozent(i32),
    /// `auto` — das Layout entscheidet.
    Auto,
}

impl Laenge {
    /// Null Pixel.
    pub const NULL: Laenge = Laenge::Px(0);

    /// Eine ganze Pixelzahl.
    pub const fn px(ganz: i32) -> Laenge {
        Laenge::Px(ganz.saturating_mul(TAUSEND))
    }

    /// Ist das eine absolute Laenge, die schon jetzt feststeht?
    pub fn ist_absolut(&self) -> bool {
        matches!(self, Laenge::Px(_))
    }

    /// `em` in `px` umrechnen — der Schritt, der zur KASKADENZEIT gehoert.
    ///
    /// `schrift_px` ist die berechnete Schriftgroesse DIESES Elements (in
    /// 1/1000 px). Prozent und `auto` bleiben stehen: Sie brauchen das
    /// Layout (siehe Kopfkommentar).
    pub fn em_aufloesen(self, schrift_px: i32) -> Laenge {
        match self {
            Laenge::Em(tausendstel) => {
                // (em * schriftgroesse) / 1000 — beides in Tausendsteln,
                // also einmal durch TAUSEND teilen. `i64` als
                // Zwischenschritt, weil 1000 em * 100 000 px sonst
                // ueberliefe.
                let produkt = (tausendstel as i64) * (schrift_px as i64) / TAUSEND as i64;
                Laenge::Px(produkt.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
            }
            andere => andere,
        }
    }

    /// Der Wert in GANZEN Pixeln, wenn er absolut ist.
    ///
    /// `None` bei Prozent und `auto` — der Aufrufer (das Layout) muss dann
    /// selbst entscheiden. Ein `unwrap_or(0)` an dieser Stelle waere der
    /// Weg, auf dem `width: auto` still zu `width: 0` wird.
    pub fn px_ganz(self) -> Option<i32> {
        match self {
            Laenge::Px(tausendstel) => Some(runden(tausendstel)),
            _ => None,
        }
    }

    /// Prozent auf eine Bezugsgroesse anwenden (Layoutzeit).
    ///
    /// `bezug_px` ist eine GANZE Pixelzahl (die Breite des umgebenden
    /// Kastens zum Beispiel).
    pub fn auf_bezug(self, bezug_px: i32) -> Option<i32> {
        match self {
            Laenge::Px(t) => Some(runden(t)),
            Laenge::Prozent(t) => {
                let produkt = (t as i64) * (bezug_px as i64) / (TAUSEND as i64 * 100);
                Some(produkt.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
            }
            // `em` haette zur Kaskadenzeit aufgeloest sein muessen. Dass es
            // hier noch auftaucht, ist ein Programmierfehler — er wird
            // aber nicht zur Panik, sondern zu „das Layout entscheidet".
            Laenge::Em(_) | Laenge::Auto => None,
        }
    }
}

/// Tausendstel zu ganzen Pixeln, kaufmaennisch gerundet.
///
/// GERUNDET UND NICHT ABGESCHNITTEN: Bei `margin: 0.5em` mit 16 px
/// Schrift kommen 8000 heraus — kein Problem. Bei `0.05em` waeren es 800,
/// abgeschnitten 0 und gerundet 1. Ueber eine ganze Seite summieren sich
/// abgeschnittene Werte sichtbar nach oben (alles rutscht zusammen).
pub fn runden(tausendstel: i32) -> i32 {
    if tausendstel >= 0 {
        (tausendstel + TAUSEND / 2) / TAUSEND
    } else {
        (tausendstel - TAUSEND / 2) / TAUSEND
    }
}

// ---------------------------------------------------------------------------
// FARBEN
// ---------------------------------------------------------------------------

/// Eine Farbe mit Alpha.
///
/// Eigener Typ und nicht `speedui::Farbe`: Diese Kiste kennt kein Toolkit
/// (sie kennt nur den Dokumentbaum). Die Umrechnung passiert im Browser,
/// an einer Stelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Farbe {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Farbe {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Farbe {
        Farbe { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Farbe {
        Farbe { r, g, b, a }
    }
    pub const SCHWARZ: Farbe = Farbe::rgb(0, 0, 0);
    pub const WEISS: Farbe = Farbe::rgb(255, 255, 255);
    /// Vollstaendig durchsichtig — der Anfangswert von `background-color`.
    pub const DURCHSICHTIG: Farbe = Farbe::rgba(0, 0, 0, 0);

    pub fn ist_durchsichtig(&self) -> bool {
        self.a == 0
    }
}

/// Die benannten Farben, die wir kennen — **aufsteigend sortiert**
/// (binaere Suche, wie bei den HTML-Entitaeten).
///
/// CSS kennt 148 benannte Farben. Hier stehen die, die auf echten Seiten
/// vorkommen, plus die 16 aus HTML 4. Unbekannte Namen werden ABGELEHNT
/// (die Deklaration faellt weg und der geerbte oder Anfangswert gilt) —
/// anders als bei HTML-Entitaeten gibt es hier nichts „durchzulassen":
/// Eine Farbe ist eine Farbe oder nicht.
static BENANNTE_FARBEN: &[(&str, Farbe)] = &[
    ("aqua", Farbe::rgb(0, 255, 255)),
    ("beige", Farbe::rgb(245, 245, 220)),
    ("black", Farbe::rgb(0, 0, 0)),
    ("blue", Farbe::rgb(0, 0, 255)),
    ("brown", Farbe::rgb(165, 42, 42)),
    ("crimson", Farbe::rgb(220, 20, 60)),
    ("cyan", Farbe::rgb(0, 255, 255)),
    ("darkblue", Farbe::rgb(0, 0, 139)),
    ("darkgray", Farbe::rgb(169, 169, 169)),
    ("darkgreen", Farbe::rgb(0, 100, 0)),
    ("darkgrey", Farbe::rgb(169, 169, 169)),
    ("darkred", Farbe::rgb(139, 0, 0)),
    ("fuchsia", Farbe::rgb(255, 0, 255)),
    ("gold", Farbe::rgb(255, 215, 0)),
    ("gray", Farbe::rgb(128, 128, 128)),
    ("green", Farbe::rgb(0, 128, 0)),
    ("grey", Farbe::rgb(128, 128, 128)),
    ("indigo", Farbe::rgb(75, 0, 130)),
    ("khaki", Farbe::rgb(240, 230, 140)),
    ("lightblue", Farbe::rgb(173, 216, 230)),
    ("lightgray", Farbe::rgb(211, 211, 211)),
    ("lightgreen", Farbe::rgb(144, 238, 144)),
    ("lightgrey", Farbe::rgb(211, 211, 211)),
    ("lime", Farbe::rgb(0, 255, 0)),
    ("magenta", Farbe::rgb(255, 0, 255)),
    ("maroon", Farbe::rgb(128, 0, 0)),
    ("navy", Farbe::rgb(0, 0, 128)),
    ("olive", Farbe::rgb(128, 128, 0)),
    ("orange", Farbe::rgb(255, 165, 0)),
    ("pink", Farbe::rgb(255, 192, 203)),
    ("purple", Farbe::rgb(128, 0, 128)),
    ("red", Farbe::rgb(255, 0, 0)),
    ("salmon", Farbe::rgb(250, 128, 114)),
    ("silver", Farbe::rgb(192, 192, 192)),
    ("skyblue", Farbe::rgb(135, 206, 235)),
    ("steelblue", Farbe::rgb(70, 130, 180)),
    ("tan", Farbe::rgb(210, 180, 140)),
    ("teal", Farbe::rgb(0, 128, 128)),
    ("tomato", Farbe::rgb(255, 99, 71)),
    ("transparent", Farbe::DURCHSICHTIG),
    ("violet", Farbe::rgb(238, 130, 238)),
    ("white", Farbe::rgb(255, 255, 255)),
    ("whitesmoke", Farbe::rgb(245, 245, 245)),
    ("yellow", Farbe::rgb(255, 255, 0)),
];

/// Eine Farbangabe zerlegen: `#rgb`, `#rrggbb`, `rgb(...)`, `rgba(...)`
/// oder ein Name.
pub fn farbe_parsen(text: &str) -> Option<Farbe> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return hex_farbe(hex);
    }
    if let Some(rest) = text.strip_prefix("rgb(").or_else(|| text.strip_prefix("rgba(")) {
        return funktions_farbe(rest.trim_end_matches(')'));
    }
    // Namen sind ASCII und case-insensitiv.
    let klein = kleinschreiben(text);
    BENANNTE_FARBEN
        .binary_search_by(|(n, _)| (*n).cmp(klein.as_str()))
        .ok()
        .map(|i| BENANNTE_FARBEN[i].1)
}

fn hex_farbe(hex: &str) -> Option<Farbe> {
    let ziffer = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let b = hex.as_bytes();
    match b.len() {
        // #rgb -> jede Ziffer verdoppeln (#f00 == #ff0000)
        3 => {
            let r = ziffer(b[0])?;
            let g = ziffer(b[1])?;
            let bl = ziffer(b[2])?;
            Some(Farbe::rgb(r * 17, g * 17, bl * 17))
        }
        4 => {
            let r = ziffer(b[0])?;
            let g = ziffer(b[1])?;
            let bl = ziffer(b[2])?;
            let a = ziffer(b[3])?;
            Some(Farbe::rgba(r * 17, g * 17, bl * 17, a * 17))
        }
        6 => Some(Farbe::rgb(
            ziffer(b[0])? * 16 + ziffer(b[1])?,
            ziffer(b[2])? * 16 + ziffer(b[3])?,
            ziffer(b[4])? * 16 + ziffer(b[5])?,
        )),
        8 => Some(Farbe::rgba(
            ziffer(b[0])? * 16 + ziffer(b[1])?,
            ziffer(b[2])? * 16 + ziffer(b[3])?,
            ziffer(b[4])? * 16 + ziffer(b[5])?,
            ziffer(b[6])? * 16 + ziffer(b[7])?,
        )),
        _ => None,
    }
}

/// `rgb(1, 2, 3)` / `rgba(1, 2, 3, 0.5)` — auch mit Prozent-Kanaelen.
fn funktions_farbe(inneres: &str) -> Option<Farbe> {
    let teile: Vec<&str> = inneres
        .split([',', '/', ' '])
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if teile.len() < 3 {
        return None;
    }
    let kanal = |t: &str| -> Option<u8> {
        if let Some(p) = t.strip_suffix('%') {
            let prozent: i32 = zahl_tausendstel(p)?;
            return Some(((prozent as i64 * 255) / (100 * TAUSEND as i64)).clamp(0, 255) as u8);
        }
        let wert = zahl_tausendstel(t)?;
        Some(runden(wert).clamp(0, 255) as u8)
    };
    let r = kanal(teile[0])?;
    let g = kanal(teile[1])?;
    let b = kanal(teile[2])?;
    let a = match teile.get(3) {
        None => 255,
        Some(t) => {
            if let Some(p) = t.strip_suffix('%') {
                let prozent = zahl_tausendstel(p)?;
                ((prozent as i64 * 255) / (100 * TAUSEND as i64)).clamp(0, 255) as u8
            } else {
                // Alpha ist 0..1 — also Tausendstel mal 255 durch 1000.
                let wert = zahl_tausendstel(t)?;
                ((wert as i64 * 255) / TAUSEND as i64).clamp(0, 255) as u8
            }
        }
    };
    Some(Farbe::rgba(r, g, b, a))
}

// ---------------------------------------------------------------------------
// ZAHLEN
// ---------------------------------------------------------------------------

/// Eine Dezimalzahl in TAUSENDSTELN lesen: `1.5` -> 1500, `-.25` -> -250.
///
/// Von Hand und nicht ueber `f32::from_str`: Es gibt kein Fliesskomma
/// (siehe Kopfkommentar). Mehr als drei Nachkommastellen werden
/// ABGESCHNITTEN, nicht gerundet — bei `0.0005px` ist der Unterschied
/// bedeutungslos, und Abschneiden kann nicht ueberlaufen.
pub fn zahl_tausendstel(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (negativ, rest) = match text.as_bytes()[0] {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    if rest.is_empty() {
        return None;
    }

    let (ganz_text, bruch_text) = match rest.split_once('.') {
        Some((g, b)) => (g, b),
        None => (rest, ""),
    };
    // `.5` ist gueltig, `5.` auch — aber irgendeine Ziffer muss da sein.
    if ganz_text.is_empty() && bruch_text.is_empty() {
        return None;
    }

    let mut ganz: i64 = 0;
    for c in ganz_text.bytes() {
        if !c.is_ascii_digit() {
            return None;
        }
        ganz = ganz.saturating_mul(10).saturating_add((c - b'0') as i64);
        if ganz > i32::MAX as i64 {
            return None; // absurd gross — die Deklaration faellt weg
        }
    }

    let mut bruch: i64 = 0;
    let mut stellen = 0;
    for c in bruch_text.bytes() {
        if !c.is_ascii_digit() {
            return None;
        }
        if stellen < 3 {
            bruch = bruch * 10 + (c - b'0') as i64;
            stellen += 1;
        }
    }
    while stellen < 3 {
        bruch *= 10;
        stellen += 1;
    }

    let gesamt = ganz.saturating_mul(TAUSEND as i64).saturating_add(bruch);
    if gesamt > i32::MAX as i64 {
        return None;
    }
    Some(if negativ { -(gesamt as i32) } else { gesamt as i32 })
}

/// Eine Laengenangabe mit Einheit lesen.
///
/// UNTERSTUETZT: `px`, `em`, `rem`, `%`, `pt`, und die nackte `0`.
/// `rem` wird wie `em` behandelt — wir haben keine Wurzel-Schriftgroesse,
/// die von der Fliesstextgroesse abweicht, und das ist bei einem Browser
/// mit vier Rastergroessen (docs/schrift-groessen.md) auch kein Verlust.
/// `pt` wird mit 4/3 zu px (die uebliche 96-dpi-Annahme).
///
/// NICHT UNTERSTUETZT: `vw`/`vh` (brauchen die Fenstergroesse zur
/// Kaskadenzeit), `ex`/`ch` (brauchen Schriftmetrik), `cm`/`mm`/`in`
/// (brauchen eine echte Pixeldichte, die wir nicht kennen), `calc()`.
/// Alle werden ABGELEHNT — die Deklaration faellt dann weg, und der
/// geerbte oder Anfangswert gilt. Das ist sichtbar falsch (etwas ist zu
/// klein), nicht unsichtbar falsch.
pub fn laenge_parsen(text: &str) -> Option<Laenge> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("auto") {
        return Some(Laenge::Auto);
    }
    if let Some(zahl) = text.strip_suffix('%') {
        return zahl_tausendstel(zahl).map(Laenge::Prozent);
    }
    for (endung, bauen) in [
        ("px", 0u8),
        ("em", 1),
        ("rem", 1),
        ("pt", 2),
    ] {
        if text.len() > endung.len() && text.to_ascii_lowercase().ends_with(endung) {
            let zahl = zahl_tausendstel(&text[..text.len() - endung.len()])?;
            return Some(match bauen {
                0 => Laenge::Px(zahl),
                1 => Laenge::Em(zahl),
                // pt -> px: mal 4 durch 3.
                _ => Laenge::Px(((zahl as i64) * 4 / 3) as i32),
            });
        }
    }
    // Eine nackte Zahl ist nur als 0 gueltig (CSS erlaubt `margin: 0`).
    match zahl_tausendstel(text) {
        Some(0) => Some(Laenge::Px(0)),
        _ => None,
    }
}

/// ASCII kleinschreiben.
///
/// ASCII und nicht `to_lowercase()`: Letzteres kann aus EINEM Zeichen
/// MEHRERE machen (ß -> ss). CSS-Schluesselwoerter sind ASCII; ein
/// Klassenname darf Unicode enthalten und wird deshalb NICHT hier
/// durchgereicht (Klassen sind gross-/kleinschreibungsempfindlich).
pub fn kleinschreiben(text: &str) -> String {
    text.chars().map(|c| c.to_ascii_lowercase()).collect()
}
