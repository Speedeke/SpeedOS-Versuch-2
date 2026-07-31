// speedui::umgebung — WAS DAS TOOLKIT VOM WIRT VERLANGT
//
// ==========================================================================
// DIE UMKEHR
//
// Bis Serie 8, Teil 2 griff das Toolkit direkt in den Kernel: `metrik()`,
// `theme::aktuell()`, `zeit::us_seit_boot()`, `Zeichner<FensterPuffer>`. Das
// war bequem und machte die Kiste unverschiebbar.
//
// Hier stehen jetzt die Traits, die speedui VERLANGT und NICHT selbst
// mitbringt. Sie sind bewusst schmal: Was ein Widget nicht braucht, steht
// nicht drin — dann kann es auch niemand versehentlich benutzen.
//
// Begruendung fuer jede einzelne Entscheidung: docs/speedui-trennung.md.
// ==========================================================================

use crate::{Farbe, Icon, Rechteck};
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// (1) THEMA — Farben und Masse
// ---------------------------------------------------------------------------

/// Wofuer eine Farbe steht — NICHT welche sie ist.
///
/// Ein Rollen-Enum statt eines Farb-Structs, und das ist eine Entscheidung:
/// Ein Struct waere der bequeme Weg gewesen und haette die Kopplung nur
/// umbenannt (jede neue Kernel-Farbe muesste in die Kiste). So ist die
/// Liste dessen, was ein Widget ueberhaupt kennen darf, ABSCHLIESSEND und
/// lesbar. Wer eine Rolle ergaenzt, tut es sichtbar und muss beide Wirte
/// nachziehen.
/// DIE LISTE IST ABSICHTLICH GENAU SO LANG WIE NOETIG: Sie enthaelt exakt
/// die dreizehn Farben, die die Widgets heute benutzen — nachgezaehlt, nicht
/// geraten. Das Kernel-Thema hat rund vierzig Felder; zwei Drittel davon
/// sind Fenster-Dekoration (Titelleiste, Schatten, Taskleiste) und gehen ein
/// Widget nichts an.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Farbrolle {
    /// Hintergrund von Panels und Menues.
    Flaeche,
    /// Hintergrund des Fenster-Inhalts.
    InhaltHintergrund,
    /// Rahmen im Ruhezustand.
    Rahmen,
    /// Der Akzent — auch der Rahmen eines fokussierten Elements.
    Akzent,
    /// Hintergrund eines AUSGEWAEHLTEN Eintrags.
    Auswahl,
    /// Hintergrund von Eingabefeldern und ruhenden Knoepfen.
    Eingabefeld,
    /// Knopf-Flaeche (Leisten-Stil).
    KnopfFlaeche,
    /// Knopf-Flaeche unter dem Cursor.
    KnopfAktiv,
    /// Ueberschriften und Knopfbeschriftungen.
    TextStark,
    /// Fliesstext.
    TextNormal,
    /// Nebentexte.
    TextSekundaer,
    /// Deaktiviertes und Beilaeufiges.
    TextGedimmt,
    /// Text AUF einer Akzent-/Auswahlflaeche.
    TextAufAkzent,
}

/// Ein Mass in Pixeln (oder Mikrosekunden bei `CursorBlinkUs`).
///
/// Auch hier: genau die neun, die benutzt werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mass {
    /// Standard-Innenabstand zwischen Elementen.
    Abstand,
    /// Innenrand eines Fensters (Wurzel-Widget zum Rand).
    UiRand,
    /// Hoehe von Knoepfen, Textfeldern, Checkbox-Zeilen.
    ElementHoehe,
    ListenEintragHoehe,
    ScrollbalkenBreite,
    RadiusKlein,
    /// Schriftgroesse in Pixeln (Normaltext).
    SchriftUi,
    /// Hoehe einer Textzeile.
    ZeilenHoehe,
    /// Blink-Periode des Text-Cursors in Mikrosekunden.
    CursorBlinkUs,
}

/// Das Erscheinungsbild — geliefert vom Wirt.
pub trait Thema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe;
    fn mass(&self, mass: Mass) -> i32;
}

// ---------------------------------------------------------------------------
// (2) SCHRIFT — reine Metrik, keine Glyphen
// ---------------------------------------------------------------------------

/// Was das Layout ueber Text wissen muss.
///
/// **Die Schrift selbst zieht NICHT mit**, auch nicht als Daten: Der Kernel
/// benutzt vorgerasterte Bitmaps (~1 MiB), ein Ring-3-Prozess hat sie nicht
/// und bekommt sie auch nicht (es gibt keinen Schrift-Syscall, siehe
/// docs/grenzen.md). Ein Toolkit braucht die Glyphen aber gar nicht — es
/// braucht MASSE, um zu layouten, und einen, der MALT (`Leinwand`).
///
/// `groesse` ist eine Pixelhoehe als `i32` und kein `RasterHeight`: Der Typ
/// der noto-Kiste ist genau die Sorte Abhaengigkeit, die hier nicht
/// hindurchdarf. Welche Raster der Wirt aus der Zahl macht, ist seine Sache.
pub trait Schrift {
    /// Breite EINES Zeichens (die Schrift ist monospace).
    fn zeichen_breite(&self, groesse: i32) -> i32;
    /// Hoehe einer Zeile in dieser Groesse.
    fn zeilen_hoehe(&self, groesse: i32) -> i32;
    /// Breite eines Textes. Voreinstellung: Zeichenzahl x Zeichenbreite —
    /// ein Wirt mit Proportionalschrift ueberschreibt das.
    fn text_breite(&self, text: &str, groesse: i32) -> i32 {
        text.chars().count() as i32 * self.zeichen_breite(groesse)
    }
}

// ---------------------------------------------------------------------------
// (3) UHR — eine Zahl
// ---------------------------------------------------------------------------

/// Gebraucht fuer den Cursor-Blink und die Doppelklick-Erkennung. Mehr
/// nicht — und deshalb genau eine Methode.
pub trait Uhr {
    /// Mikrosekunden seit irgendeinem festen Punkt (monoton).
    fn us(&self) -> u64;
}

// ---------------------------------------------------------------------------
// (4) LEINWAND — die versteckte vierte, und die groesste
// ---------------------------------------------------------------------------

/// Worauf ein Widget zeichnet.
///
/// DIE GROESSTE DER KOPPLUNGEN: Der alte Widget-Trait hiess
/// `zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, ...)` und band das
/// Toolkit damit an ZWEI Kernel-Typen auf einmal — an den
/// Zeichner-Algorithmus und an den konkreten Fenster-Puffer.
///
/// WARUM HOHE OPERATIONEN UND KEINE PIXEL: Ein `pixel_setzen`-Trait waere
/// schmaler, aber dann muesste speedui Bresenham, Alpha-Blending und
/// Rundungs-Ecken selbst mitbringen — also den halben Zeichner
/// duplizieren, und zwar ohne die Zeilen-Schnellpfade aus Serie 3. Diese
/// neun Operationen sind der Schnitt, an dem beide Wirte ihre eigenen
/// Schnellpfade behalten. (Es sind genau die, die die Widgets benutzen;
/// `kreis_*`, `verlauf_*` und `blit` benutzt keines und bleiben draussen.)
pub trait Leinwand {
    /// Breite und Hoehe der Zeichenflaeche.
    fn masse(&self) -> (i32, i32);
    /// Das aktuelle Clip-Rechteck (Widgets fragen es ab, um Unsichtbares
    /// gar nicht erst zu zeichnen — der 4K-Schnellpfad des Editors).
    fn clip(&self) -> Option<Rechteck>;
    fn clip_setzen(&mut self, clip: Option<Rechteck>);

    fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe);
    fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe);
    fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe);
    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe);
    fn text(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe);
    fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32);
}

// ---------------------------------------------------------------------------
// (5) DATEIQUELLE — nur fuer den Datei-Dialog
// ---------------------------------------------------------------------------

/// Woher der Datei-Dialog seine Eintraege bekommt.
///
/// Ein Toolkit, das ein Dateisystem kennt, ist kein Toolkit. Die beiden
/// Pfad-Methoden gehoeren dazu, obwohl sie reine Stringarbeit sind: Was ein
/// Pfad IST (`/` als Trenner, `..`, Mount-Praefixe), ist eine Eigenschaft
/// des Wirts.
pub trait Dateiquelle {
    /// Eintraege eines Ordners: (Name, ist_ordner). Leer bei Fehlern —
    /// ein Dialog, der nichts anzeigen kann, zeigt nichts an.
    fn liste(&self, ordner: &str) -> Vec<(String, bool)>;
    /// `basis` + `name` zu einem Pfad zusammensetzen.
    fn anhaengen(&self, basis: &str, name: &str) -> String;
    /// Eine (womoeglich relative) Eingabe gegen `basis` aufloesen.
    fn aufloesen(&self, basis: &str, eingabe: &str) -> String;
}

// ---------------------------------------------------------------------------
// Die Buendel, mit denen die Traits an die Widgets kommen
// ---------------------------------------------------------------------------

/// Was ein Widget zum RECHNEN braucht (Layout, Ereignisse).
///
/// Ein Buendel statt drei Parametern — sonst waendern vier Referenzen durch
/// jede Signatur. `Copy`, damit Container es ohne Umstaende weiterreichen.
#[derive(Clone, Copy)]
pub struct UiKontext<'a> {
    pub thema: &'a dyn Thema,
    pub schrift: &'a dyn Schrift,
    pub uhr: &'a dyn Uhr,
}

impl<'a> UiKontext<'a> {
    pub fn neu(thema: &'a dyn Thema, schrift: &'a dyn Schrift, uhr: &'a dyn Uhr) -> Self {
        UiKontext { thema, schrift, uhr }
    }

    /// Kurzform fuer `self.thema.farbe(...)`.
    #[inline]
    pub fn farbe(&self, rolle: Farbrolle) -> Farbe {
        self.thema.farbe(rolle)
    }
    /// Kurzform fuer `self.thema.mass(...)`.
    #[inline]
    pub fn mass(&self, mass: Mass) -> i32 {
        self.thema.mass(mass)
    }
    /// Kurzform: der Standard-Abstand (das mit Abstand haeufigste Mass).
    #[inline]
    pub fn abstand(&self) -> i32 {
        self.mass(Mass::Abstand)
    }
    /// Kurzform: die UI-Schriftgroesse.
    #[inline]
    pub fn schrift_ui(&self) -> i32 {
        self.mass(Mass::SchriftUi)
    }
    /// Breite eines Textes in UI-Schriftgroesse.
    #[inline]
    pub fn text_breite(&self, text: &str) -> i32 {
        self.schrift.text_breite(text, self.schrift_ui())
    }
    /// Breite eines Zeichens in UI-Schriftgroesse.
    #[inline]
    pub fn zeichen_breite(&self) -> i32 {
        self.schrift.zeichen_breite(self.schrift_ui())
    }
}

/// Was ein Widget zum ZEICHNEN braucht: Kontext PLUS Leinwand.
pub struct Maler<'a> {
    pub leinwand: &'a mut dyn Leinwand,
    pub kontext: UiKontext<'a>,
}

impl<'a> Maler<'a> {
    pub fn neu(leinwand: &'a mut dyn Leinwand, kontext: UiKontext<'a>) -> Self {
        Maler { leinwand, kontext }
    }

    // --- Durchreichen, damit Widget-Code lesbar bleibt ---

    #[inline]
    pub fn farbe(&self, rolle: Farbrolle) -> Farbe {
        self.kontext.farbe(rolle)
    }
    #[inline]
    pub fn mass(&self, mass: Mass) -> i32 {
        self.kontext.mass(mass)
    }
    #[inline]
    pub fn abstand(&self) -> i32 {
        self.kontext.abstand()
    }
    #[inline]
    pub fn clip(&self) -> Option<Rechteck> {
        self.leinwand.clip()
    }
    #[inline]
    pub fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.leinwand.clip_setzen(clip)
    }
    #[inline]
    pub fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.leinwand.fuellen(rechteck, farbe)
    }
    #[inline]
    pub fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe) {
        self.leinwand.abgerundet(rechteck, radius, farbe)
    }
    #[inline]
    pub fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.leinwand.rahmen(rechteck, farbe)
    }
    #[inline]
    pub fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe) {
        self.leinwand.linie(x0, y0, x1, y1, farbe)
    }
    #[inline]
    pub fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32) {
        self.leinwand.icon(x, y, icon, skalierung)
    }
    /// Text in UI-Groesse, normal.
    #[inline]
    pub fn text(&mut self, x: i32, y: i32, text: &str, farbe: Farbe) {
        let groesse = self.kontext.schrift_ui();
        self.leinwand.text(x, y, text, groesse, false, farbe)
    }
    /// Text mit ausdruecklicher Groesse und Gewicht.
    #[inline]
    pub fn text_mit(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe) {
        self.leinwand.text(x, y, text, groesse, fett, farbe)
    }
}
