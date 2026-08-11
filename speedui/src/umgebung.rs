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

/// Schriftschnitt: fett und/oder kursiv.
///
/// EIN STRUCT UND KEIN `fett: bool` MEHR, weil es ab jetzt zwei Achsen
/// sind und ein zweiter `bool`-Parameter an jeder Signatur die Sorte
/// Aufruf erzeugt, die man nicht mehr lesen kann (`text(.., true, false)`).
///
/// EHRLICH BENANNT: `kursiv` heisst „schraeg gestellt", nicht „kursiver
/// Schnitt". Unsere Schrift HAT keinen kursiven Schnitt (siehe
/// `Schrift::kursiv_echt`), der Wirt schert die Glyphen. Ein echter
/// Italic-Font hat andere Buchstabenformen — ein geschertes `a` bleibt ein
/// gerades `a`, das schief steht. Fuer einen Renderer, der `<i>` von
/// normalem Text unterscheidbar machen soll, reicht das; wer etwas anderes
/// behauptet, luegt ueber den Font-Bestand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stil {
    pub fett: bool,
    pub kursiv: bool,
}

impl Stil {
    pub const NORMAL: Stil = Stil { fett: false, kursiv: false };
    pub const FETT: Stil = Stil { fett: true, kursiv: false };
    pub const KURSIV: Stil = Stil { fett: false, kursiv: true };
    pub const FETT_KURSIV: Stil = Stil { fett: true, kursiv: true };

    pub const fn neu(fett: bool, kursiv: bool) -> Stil {
        Stil { fett, kursiv }
    }
}

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
///
/// ===================================================================
/// DIE ERWEITERUNG VON SERIE 8, TEIL 3
///
/// Ein Widget kam mit EINER Groesse aus. Ein HTML-Renderer nicht: Er hat
/// Ueberschriften, Fliesstext und Kleingedrucktes GLEICHZEITIG auf einer
/// Seite, und er muss die Breite eines Textstuecks kennen, BEVOR er es
/// zeichnet — ohne das gibt es keinen Zeilenumbruch.
///
/// Dazu sind drei Dinge neu, und alle drei haben eine Voreinstellung,
/// damit bestehende Wirte unveraendert weiterlaufen:
///
/// * `groessen()` — welche Groessen es WIRKLICH gibt. Ein Renderer darf
///   nicht raten: Bei uns sind es vier vorgerasterte, keine beliebigen
///   (die Begruendung und die Folgen stehen vollstaendig in
///   docs/schrift-groessen.md).
/// * `groesse_waehlen()` — `font-size: 13px` auf die naechstliegende
///   vorhandene abbilden.
/// * `text_breite_stil()` — die Metrik, die der Umbruch braucht. Sie ist
///   STIL-ABHAENGIG, weil fett breiter sein kann als normal (bei uns ist
///   es das nicht — bei einer Proportionalschrift schon).
pub trait Schrift {
    /// Breite EINES Zeichens (die Schrift ist monospace).
    fn zeichen_breite(&self, groesse: i32) -> i32;
    /// Hoehe einer Zeile in dieser Groesse.
    fn zeilen_hoehe(&self, groesse: i32) -> i32;
    /// Breite eines Textes. Voreinstellung: Zeichenzahl x Zeichenbreite —
    /// ein Wirt mit Proportionalschrift ueberschreibt das.
    ///
    /// `chars().count()` UND NICHT `len()`: `len()` ist die Zahl der
    /// UTF-8-BYTES. „Grüße" hat 5 Zeichen und 7 Bytes — wer `len()` nimmt,
    /// rechnet fuer jeden Umlaut eine Zeichenbreite zu viel und bricht
    /// deutsche Zeilen zu frueh um. Der Test dazu steht in
    /// `speedui::text::tests`.
    fn text_breite(&self, text: &str, groesse: i32) -> i32 {
        text.chars().count() as i32 * self.zeichen_breite(groesse)
    }

    /// Die Groessen, die dieser Wirt WIRKLICH hat — aufsteigend sortiert.
    ///
    /// Voreinstellung: eine leere Liste, was „jede Groesse" bedeutet (ein
    /// Wirt mit echtem Rasterizer sagt nichts anderes). Wer vorgerasterte
    /// Bitmaps hat, zaehlt sie hier auf, und `groesse_waehlen` rundet
    /// darauf.
    fn groessen(&self) -> &[i32] {
        &[]
    }

    /// Die vorhandene Groesse, die `wunsch` am naechsten kommt.
    ///
    /// BEI GLEICHSTAND DIE KLEINERE. Ein Wunsch genau zwischen zwei
    /// Rastern (18 bei 16 und 20) wird abgerundet: Zu gross sprengt das
    /// Layout, zu klein ist nur haesslich — und ein gesprengtes Layout
    /// faellt mehr auf.
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        let vorhandene = self.groessen();
        if vorhandene.is_empty() {
            return wunsch.max(1);
        }
        let mut beste = vorhandene[0];
        let mut abstand = (beste - wunsch).abs();
        for &g in vorhandene.iter().skip(1) {
            let d = (g - wunsch).abs();
            // `<` und nicht `<=`: Bei Gleichstand gewinnt die zuerst
            // gesehene, und die Liste ist aufsteigend — also die kleinere.
            if d < abstand {
                beste = g;
                abstand = d;
            }
        }
        beste
    }

    /// Breite eines Textes in einem bestimmten Schnitt.
    ///
    /// Voreinstellung: wie `text_breite` — unsere Monospace-Raster sind in
    /// jedem Schnitt gleich breit. Ein Wirt mit Proportionalschrift
    /// ueberschreibt das, sonst umbricht er fetten Text falsch.
    fn text_breite_stil(&self, text: &str, groesse: i32, _stil: Stil) -> i32 {
        self.text_breite(text, groesse)
    }

    /// Hat die Schrift einen ECHTEN Fettschnitt (statt Doppelzeichnung)?
    fn fett_echt(&self) -> bool {
        false
    }

    /// Hat die Schrift einen ECHTEN Kursivschnitt (statt Scherung)?
    ///
    /// WOZU DAS EIN TRAIT-MITGLIED IST und kein Kommentar: Damit die
    /// Auskunft im PROGRAMM steht und nicht nur in der Doku. Ein
    /// Renderer, der wissen will, ob er `<i>` ehrlich darstellen kann,
    /// fragt hier — und die Diagnose-Anzeige kann es zeigen.
    fn kursiv_echt(&self) -> bool {
        false
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

    /// Text mit vollem Schnitt (fett UND kursiv).
    ///
    /// WARUM ALS ZEHNTE OPERATION MIT VOREINSTELLUNG und nicht als
    /// Aenderung an `text`: `text` steht in jedem Widget und in beiden
    /// Wirten. Eine geaenderte Signatur waere ein Umbau von zwanzig
    /// Aufrufstellen fuer eine Faehigkeit, die KEIN Widget benutzt — nur
    /// der kommende Renderer. Die Voreinstellung wirft das Kursiv weg und
    /// zeichnet den Rest korrekt; ein Wirt, der scheren kann,
    /// ueberschreibt sie.
    ///
    /// Das ist dieselbe Ueberlegung wie bei den Zeilen-Schnellpfaden des
    /// `Zeichenflaeche`-Traits im Kernel: eine Voreinstellung, die richtig
    /// ist, und ein Wirt, der es besser kann, wenn er will.
    fn text_stil(&mut self, x: i32, y: i32, text: &str, groesse: i32, stil: Stil, farbe: Farbe) {
        self.text(x, y, text, groesse, stil.fett, farbe);
    }

    /// Ein RGBA-Bild in ein Rechteck malen (Serie 8, Teil 7).
    ///
    /// `rgba` sind `quell_breite * quell_hoehe` Pixel zu je vier Byte
    /// (R, G, B, A) — das Format, das `libspeed::bild` liefert. Passt die
    /// Quellgroesse nicht zum Ziel, SKALIERT der Wirt (Punktabtastung
    /// genuegt; ein Browser ohne Interpolation sieht kantig aus, aber
    /// richtig).
    ///
    /// WARUM ALS ELFTE OPERATION MIT VOREINSTELLUNG — dieselbe
    /// Ueberlegung wie bei `text_stil` eine Ebene hoeher: KEIN Widget
    /// braucht sie, nur der Renderer. Eine Pflicht-Methode waere ein
    /// Umbau beider Wirte fuer eine Faehigkeit, die die Widgets nie
    /// aufrufen.
    ///
    /// DIE VOREINSTELLUNG ZEICHNET EINEN RAHMEN und nicht nichts: Ein
    /// Wirt, der keine Bilder kann, soll den PLATZ des Bildes zeigen.
    /// Unsichtbar zu scheitern ist die schlechtere Haelfte jeder
    /// Fehlerbehandlung — dieselbe Haltung wie das Magenta bei einem
    /// unbekannten Icon-Zeichen.
    fn bild(&mut self, ziel: Rechteck, _quell_breite: i32, _quell_hoehe: i32, _rgba: &[u8]) {
        self.rahmen(ziel, Farbe::neu(150, 150, 150));
    }
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
    /// Text mit ausdruecklicher Groesse und vollem Schnitt.
    #[inline]
    pub fn text_stil(&mut self, x: i32, y: i32, text: &str, groesse: i32, stil: Stil, farbe: Farbe) {
        self.leinwand.text_stil(x, y, text, groesse, stil, farbe)
    }
}
