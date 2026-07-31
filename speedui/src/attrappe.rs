// speedui::attrappe — Ein Wirt aus Pappe
//
// ==========================================================================
// WOZU
//
// Die Traits aus `umgebung.rs` sind die Grenze der Kiste. Eine Grenze, die
// nur EIN Wirt bedient, ist keine Grenze, sondern eine Umbenennung — also
// gibt es hier einen zweiten: einen, der nichts kann ausser rechnen.
//
// Er hat drei Aufgaben:
//   1. Die Toolkit-Tests laufen damit auf dem HOST, ohne QEMU, ohne
//      Framebuffer, in Millisekunden.
//   2. Er ist die AUFSTELLUNG dessen, was ein Wirt liefern muss — wer einen
//      neuen schreibt (`uidemo`), liest hier ab.
//   3. Die `MalProtokoll`-Leinwand ZEICHNET NICHT, sondern SCHREIBT MIT.
//      Damit lassen sich Zeichen-Entscheidungen pruefen, ohne Pixel zu
//      vergleichen: „hat der Button seinen Rahmen gemalt?" ist eine Frage
//      an eine Liste, keine an ein Bild.
//
// Er ist bewusst KEIN `#[cfg(test)]`: Ein Wirt in Pappe ist auch fuer
// Anwender nuetzlich (ein Layout durchrechnen, ohne zu zeichnen).

use crate::typen::{Farbe, Icon, Rechteck};
use crate::umgebung::{Farbrolle, Leinwand, Mass, Schrift, Stil, Thema, Uhr, UiKontext};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

/// Ein Icon fuer Tests (ein Kaesten, damit man es sieht).
pub static TEST_ICON: &Icon = &Icon {
    zeilen: [
        "wwwwwwwwwwwwwwww",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "w..............w",
        "wwwwwwwwwwwwwwww",
    ],
};

// ---------------------------------------------------------------------------
// Thema
// ---------------------------------------------------------------------------

/// Ein Thema mit unterscheidbaren Farben und den Massen, die das
/// Kernel-Thema bei Skalierung 1.0 hat — damit die Layout-Tests aus
/// Serie 3 dieselben Zahlen erwarten koennen wie vorher.
pub struct TestThema;

impl Thema for TestThema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe {
        // JEDE Rolle bekommt einen eigenen Wert: Ein Test, der prueft
        // „wurde die Akzentfarbe benutzt?", darf nicht daran scheitern,
        // dass zwei Rollen zufaellig gleich sind.
        let n = rolle as u8;
        Farbe::neu(n.wrapping_mul(11), n.wrapping_mul(23), n.wrapping_mul(37))
    }

    fn mass(&self, mass: Mass) -> i32 {
        match mass {
            Mass::Abstand => 8,
            Mass::UiRand => 12,
            Mass::ElementHoehe => 30,
            Mass::ListenEintragHoehe => 24,
            Mass::ScrollbalkenBreite => 10,
            Mass::RadiusKlein => 6,
            Mass::SchriftUi => 16,
            Mass::ZeilenHoehe => 20,
            Mass::CursorBlinkUs => 500_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Schrift
// ---------------------------------------------------------------------------

/// Eine Monospace-Schrift, deren Zeichenbreite die halbe Groesse ist —
/// dieselbe Faustregel wie beim vorgerasterten Kernel-Font (16 -> 8).
pub struct TestSchrift;

impl Schrift for TestSchrift {
    fn zeichen_breite(&self, groesse: i32) -> i32 {
        (groesse / 2).max(1)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        groesse + 4
    }
}

/// Eine Schrift mit GENAU DEN VIER RASTERN DES KERNELS (16/20/24/32),
/// echtem Fettschnitt und ohne Kursiv.
///
/// WARUM ES SIE ZUSAETZLICH ZU `TestSchrift` GIBT: `TestSchrift` kann
/// jede Groesse — sie ist der bequeme Wirt fuer Layout-Tests. Genau
/// deshalb taugt sie NICHT, um die Groessen-LUECKE zu pruefen: Bei ihr
/// gibt es keine. `VierRaster` bildet den echten Font-Bestand nach
/// (nachgesehen in `noto-sans-mono-bitmap`, nicht geraten), damit
/// `speedui::text` gegen die WIRKLICHE Einschraenkung getestet wird und
/// nicht gegen eine bequeme.
pub struct VierRaster;

/// Die Groessen, die es wirklich gibt. Aufsteigend — `groesse_waehlen`
/// verlaesst sich darauf (bei Gleichstand gewinnt die kleinere).
pub const RASTER: &[i32] = &[16, 20, 24, 32];

impl Schrift for VierRaster {
    fn zeichen_breite(&self, groesse: i32) -> i32 {
        (self.groesse_waehlen(groesse) / 2).max(1)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        self.groesse_waehlen(groesse) + 4
    }
    fn groessen(&self) -> &[i32] {
        RASTER
    }
    /// Der Kernel hat einen ECHTEN Fettschnitt (`FontWeight::Bold`).
    fn fett_echt(&self) -> bool {
        true
    }
    /// Und KEINEN Kursivschnitt — die Kiste liefert nur Light/Regular/Bold.
    fn kursiv_echt(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Uhr
// ---------------------------------------------------------------------------

/// Eine Uhr, die STEHT, bis der Test sie stellt.
///
/// Das ist der eigentliche Gewinn gegenueber der echten Uhr: Ein Blink-
/// oder Doppelklick-Test muss nicht warten, er STELLT die Zeit. Deshalb
/// eine `Cell` — `Uhr::us` nimmt `&self`.
pub struct TestUhr {
    jetzt: Cell<u64>,
}

impl TestUhr {
    pub fn neu() -> Self {
        TestUhr { jetzt: Cell::new(0) }
    }
    /// Stellt die Uhr auf einen festen Wert.
    pub fn setzen(&self, us: u64) {
        self.jetzt.set(us);
    }
    /// Laesst die Zeit um `us` vergehen.
    pub fn vorstellen(&self, us: u64) {
        self.jetzt.set(self.jetzt.get() + us);
    }
}

impl Default for TestUhr {
    fn default() -> Self {
        Self::neu()
    }
}

impl Uhr for TestUhr {
    fn us(&self) -> u64 {
        self.jetzt.get()
    }
}

// ---------------------------------------------------------------------------
// Leinwand, die mitschreibt
// ---------------------------------------------------------------------------

/// Eine gezeichnete Operation — so, wie das Protokoll sie festhaelt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strich {
    Fuellen(Rechteck, Farbe),
    Abgerundet(Rechteck, i32, Farbe),
    Rahmen(Rechteck, Farbe),
    Linie(i32, i32, i32, i32, Farbe),
    Text(i32, i32, String, i32, bool, Farbe),
    Icon(i32, i32, i32),
    /// Text mit vollem Schnitt (Serie 8, Teil 3).
    ///
    /// EIGENE VARIANTE UND KEIN FELD AN `Text`: Die bestehenden
    /// Widget-Tests vergleichen `Strich::Text(..)` und sollen dabei
    /// bleiben — sie sind aus Serie 3 und ihr Wert liegt darin, dass sie
    /// UNVERAENDERT durchlaufen. Wer `text_stil` prueft, prueft etwas
    /// anderes und darf dafuer hinsehen, wo es steht.
    TextStil(i32, i32, String, i32, Stil, Farbe),
}

/// Eine Leinwand, die NICHTS malt und ALLES aufschreibt.
///
/// Pixel zu vergleichen waere die schlechtere Pruefung: Sie bricht bei
/// jeder Farbanpassung, sagt aber nichts darueber, WARUM etwas anders
/// aussieht. Ein Protokoll beantwortet die Frage, die ein Widget-Test
/// wirklich stellt: „Hat es seinen Rahmen gezeichnet? In welcher Farbe?"
pub struct MalProtokoll {
    pub striche: Vec<Strich>,
    masse: (i32, i32),
    clip: Option<Rechteck>,
}

impl MalProtokoll {
    pub fn neu(breite: i32, hoehe: i32) -> Self {
        MalProtokoll { striche: Vec::new(), masse: (breite, hoehe), clip: None }
    }

    /// Wie viele Striche der Art X gibt es?
    pub fn anzahl(&self, passt: impl Fn(&Strich) -> bool) -> usize {
        self.striche.iter().filter(|s| passt(s)).count()
    }

    /// Wurde dieser Text irgendwo gezeichnet?
    pub fn hat_text(&self, gesucht: &str) -> bool {
        self.striche
            .iter()
            .any(|s| matches!(s, Strich::Text(_, _, t, _, _, _) if t == gesucht))
    }

    pub fn leeren(&mut self) {
        self.striche.clear();
    }
}

impl Leinwand for MalProtokoll {
    fn masse(&self) -> (i32, i32) {
        self.masse
    }
    fn clip(&self) -> Option<Rechteck> {
        self.clip
    }
    fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.clip = clip;
    }
    fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.striche.push(Strich::Fuellen(rechteck, farbe));
    }
    fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe) {
        self.striche.push(Strich::Abgerundet(rechteck, radius, farbe));
    }
    fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.striche.push(Strich::Rahmen(rechteck, farbe));
    }
    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe) {
        self.striche.push(Strich::Linie(x0, y0, x1, y1, farbe));
    }
    fn text(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe) {
        self.striche
            .push(Strich::Text(x, y, String::from(text), groesse, fett, farbe));
    }
    fn icon(&mut self, x: i32, y: i32, _icon: &Icon, skalierung: i32) {
        self.striche.push(Strich::Icon(x, y, skalierung));
    }
    /// MITSCHREIBEN STATT WEGWERFEN: Die Voreinstellung des Traits wuerde
    /// das Kursiv verlieren (sie leitet auf `text` um). Eine Attrappe, die
    /// dasselbe taete, koennte nie beweisen, dass ein Aufrufer Kursiv
    /// ANGEFORDERT hat — und genau das ist hier die interessante Frage.
    fn text_stil(&mut self, x: i32, y: i32, text: &str, groesse: i32, stil: Stil, farbe: Farbe) {
        self.striche
            .push(Strich::TextStil(x, y, String::from(text), groesse, stil, farbe));
    }
}

// ---------------------------------------------------------------------------
// Der Wirt als Ganzes
// ---------------------------------------------------------------------------

/// Thema, Schrift und Uhr in einem — spart in jedem Test drei Zeilen.
pub struct TestWirt {
    pub thema: TestThema,
    pub schrift: TestSchrift,
    pub uhr: TestUhr,
}

impl TestWirt {
    pub fn neu() -> Self {
        TestWirt { thema: TestThema, schrift: TestSchrift, uhr: TestUhr::neu() }
    }

    pub fn kontext(&self) -> UiKontext<'_> {
        UiKontext::neu(&self.thema, &self.schrift, &self.uhr)
    }
}

impl Default for TestWirt {
    fn default() -> Self {
        Self::neu()
    }
}

// ---------------------------------------------------------------------------
// Eine Dateiquelle aus Pappe
// ---------------------------------------------------------------------------

/// Ein Dateisystem, das aus einer festen Liste besteht.
///
/// Genau dafuer ist das `Dateiquelle`-Trait da: Der Datei-Dialog laesst
/// sich pruefen, ohne dass irgendwo ein Dateisystem gemountet ist.
pub struct TestDateien {
    /// (Ordnerpfad, Name, ist_ordner)
    pub eintraege: Vec<(String, String, bool)>,
}

impl TestDateien {
    pub fn neu() -> Self {
        TestDateien { eintraege: Vec::new() }
    }
    pub fn mit(mut self, ordner: &str, name: &str, ist_ordner: bool) -> Self {
        self.eintraege
            .push((String::from(ordner), String::from(name), ist_ordner));
        self
    }
}

impl Default for TestDateien {
    fn default() -> Self {
        Self::neu()
    }
}

impl crate::umgebung::Dateiquelle for TestDateien {
    fn liste(&self, ordner: &str) -> Vec<(String, bool)> {
        self.eintraege
            .iter()
            .filter(|(o, _, _)| o == ordner)
            .map(|(_, n, d)| (n.clone(), *d))
            .collect()
    }
    fn anhaengen(&self, basis: &str, name: &str) -> String {
        let mut aus = String::from(basis);
        if !aus.ends_with('/') {
            aus.push('/');
        }
        aus.push_str(name);
        aus
    }
    fn aufloesen(&self, basis: &str, eingabe: &str) -> String {
        if eingabe.starts_with('/') {
            String::from(eingabe)
        } else {
            self.anhaengen(basis, eingabe)
        }
    }
}
