// speedui — Das Widget-Toolkit von SpeedOS, ohne jeden Wirt
//
// ==========================================================================
// HERKUNFT: DAS HIER IST DAS TOOLKIT AUS SERIE 3
//
// Jede Zeile stand bis Serie 8, Teil 2 in `src/ui/` — mit einem
// Unterschied zum Umzug von `speedhttp`: Dort war es Wort fuer Wort
// dasselbe, weil ein Parser nur Bytes kennt. HIER musste an drei Stellen
// etwas geaendert werden, und genau die sind der Punkt der Uebung:
//
//   * `metrik()`            ->  `k.mass(Mass::…)`
//   * `theme::aktuell()`    ->  `k.farbe(Farbrolle::…)`
//   * `zeit::us_seit_boot()`->  `k.uhr.us()`
//   * `Zeichner<FensterPuffer>` -> `Maler` (ueber das Leinwand-Trait)
//
// Der Entwurf mit allen gefundenen Kopplungen — auch den versteckten —
// steht in docs/speedui-trennung.md und entstand VOR dem Umzug.
//
// ==========================================================================
// ARCHITEKTUR (unveraendert seit Serie 3)
//
//   * Ein Fenster-Inhalt = EIN Widget-Baum (Wurzel meist VBox/HBox).
//     Jedes Widget zeichnet sich auf die Leinwand und reagiert auf
//     Ereignisse — Farben und Abstaende IMMER aus dem UiKontext.
//   * Koordinaten: ALLES in Fensterinhalt-Koordinaten. Ein Widget bekommt
//     sein zugewiesenes Rechteck (`bereich`) mitgereicht und prueft selbst
//     `bereich.enthaelt(x, y)`.
//   * Ereignisse wandern den Baum HINUNTER (Container routen an das Kind
//     unter dem Cursor und erzeugen dabei MausRein/MausRaus — das
//     Hover-Konzept). Reaktionen wandern HINAUF (UiReaktion).
//   * App-Nachrichten als u32-ID an einen `fn(u32)`-Handler. WARUM IDs
//     statt Closures oder einem generischen Nachrichtentyp? (1)
//     `Box<dyn FnMut>` braeuchte Zugriff auf den App-Zustand, der teils
//     selbst im Baum steckt — Borrow-Hoelle. (2) Ein generischer Typ
//     `Widget<M>` wuerde Manager und Fenster infizieren und das Trait
//     un-objektsicher machen. (3) `fn(u32)` ist `Send` und zustandslos.
//   * Fokus-Kette: Tab wandert durch alle fokussierbaren Widgets
//     (`fokus_weiter`, mit Wrap-Around).
//
// DEADLOCK-REGEL DES KERNELS (gilt fuer ihn, nicht fuer die Kiste):
// Nachrichten-Handler werden NIE unter dem MANAGER-Lock ausgefuehrt — der
// Fenster-Manager reicht sie als NachLock-Wert nach draussen.

// `no_std` — AUSSER beim Testen: Der Test-Harness von Rust braucht `std`,
// und die Toolkit-Tests sollen auf dem HOST laufen (ohne QEMU, in
// Millisekunden). Genau dafuer gibt es dieses `cfg_attr`; am ausgelieferten
// Code aendert es nichts.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dialog;
pub mod editor;
pub mod typen;
pub mod umgebung;
pub mod widgets;

pub use typen::{icon_farbe, Farbe, Icon, Rechteck, Taste};
pub use umgebung::{
    Dateiquelle, Farbrolle, Leinwand, Maler, Mass, Schrift, Thema, Uhr, UiKontext,
};

use alloc::boxed::Box;
use alloc::vec::Vec;

/// Der Nachrichten-Kanal vom Widget-Baum zur App (siehe Kopfkommentar).
pub type NachrichtHandler = fn(u32);

/// Kurzform fuer `Box::new(widget) as Box<dyn Widget>` (der `as`-Cast ist
/// am ersten Vec-Element noetig, damit der Vec-Typ stimmt).
pub fn w(widget: impl Widget + 'static) -> Box<dyn Widget> {
    Box::new(widget)
}

// ---------------------------------------------------------------------------
// Ereignisse und Reaktionen
// ---------------------------------------------------------------------------

/// Ein Ereignis fuer ein Widget — Positionen in Fensterinhalt-Koordinaten
/// (dasselbe System wie das `bereich`-Rechteck).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEreignis {
    /// Linke Maustaste gedrueckt.
    Klick { x: i32, y: i32 },
    /// Rechte Maustaste gedrueckt (Kontextmenue-Anlass).
    Rechtsklick { x: i32, y: i32 },
    /// Zweiter Klick kurz nach dem ersten an (fast) derselben Stelle —
    /// erkennt das UiFenster ueber die Uhr.
    Doppelklick { x: i32, y: i32 },
    /// Linke Maustaste losgelassen.
    Losgelassen { x: i32, y: i32 },
    /// Mausbewegung (auch ohne gedrueckte Taste — fuers Hovern).
    Bewegt { x: i32, y: i32 },
    /// Scrollrad (delta > 0 = nach oben).
    Scroll { delta: i8, x: i32, y: i32 },
    /// Taste ans fokussierte Widget.
    Taste(Taste),
    /// Der Cursor ist in den Widget-Bereich eingetreten (vom Routing
    /// erzeugt — Widgets setzen darauf ihren Hover-Zustand).
    MausRein,
    /// Der Cursor hat den Widget-Bereich verlassen.
    MausRaus,
    /// Das Widget hat den Tastatur-Fokus bekommen.
    FokusRein,
    /// Das Widget hat den Tastatur-Fokus verloren.
    FokusRaus,
}

impl UiEreignis {
    /// Die Maus-Position des Ereignisses (None bei Taste/Fokus/Rein/Raus).
    pub fn position(&self) -> Option<(i32, i32)> {
        match self {
            UiEreignis::Klick { x, y }
            | UiEreignis::Rechtsklick { x, y }
            | UiEreignis::Doppelklick { x, y }
            | UiEreignis::Losgelassen { x, y }
            | UiEreignis::Bewegt { x, y }
            | UiEreignis::Scroll { x, y, .. } => Some((*x, *y)),
            _ => None,
        }
    }
}

/// Was ein Widget als Antwort meldet. Bewusst ein STRUCT statt Enum: Ein
/// Klick auf einen Button ist verbraucht UND will neu gezeichnet werden UND
/// traegt eine Nachricht — das sind kombinierbare Wirkungen, keine
/// Alternativen.
///
/// SCHADENS-RECHTECK: `neu_zeichnen` allein bedeutet „das GANZE Fenster
/// neu" (der ehrliche Fallback — Korrektheit vor Eleganz). Ein Widget, das
/// genau weiss, WELCHE Flaeche es geaendert hat, meldet sie ueber `schaden`
/// (in Fensterinhalt-Koordinaten) — dann rendert und komponiert das Fenster
/// nur diesen Bereich. Container reichen die Meldung unveraendert nach oben.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiReaktion {
    /// Das Ereignis ist behandelt — nicht weiterreichen.
    pub verbraucht: bool,
    /// Der Fenster-Inhalt muss neu gezeichnet werden.
    pub neu_zeichnen: bool,
    /// Der GEAENDERTE Bereich, wenn das Widget ihn kennt.
    /// None + neu_zeichnen = ganzes Fenster.
    pub schaden: Option<Rechteck>,
    /// Eine Nachricht an die App (Widget-abhaengige ID).
    pub nachricht: Option<u32>,
}

impl UiReaktion {
    pub const fn ignoriert() -> Self {
        UiReaktion { verbraucht: false, neu_zeichnen: false, schaden: None, nachricht: None }
    }
    pub const fn verbraucht() -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: false, schaden: None, nachricht: None }
    }
    /// Neuzeichnen des GANZEN Fensters (der Fallback ohne Schadens-Info).
    pub const fn neu_zeichnen() -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: true, schaden: None, nachricht: None }
    }
    /// Neuzeichnen NUR des gemeldeten Bereichs.
    pub const fn neu_zeichnen_bereich(bereich: Rechteck) -> Self {
        UiReaktion {
            verbraucht: true,
            neu_zeichnen: true,
            schaden: Some(bereich),
            nachricht: None,
        }
    }
    pub const fn nachricht(id: u32) -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: true, schaden: None, nachricht: Some(id) }
    }

    /// Setzt das Schadens-Rechteck einer Nachricht-Reaktion.
    pub const fn mit_schaden(mut self, bereich: Rechteck) -> Self {
        self.schaden = Some(bereich);
        self
    }

    /// Der Schadens-BEITRAG beim Kombinieren:
    ///   None             = traegt nichts bei (zeichnet nicht neu),
    ///   Some(None)       = Vollbild-Beitrag,
    ///   Some(Some(rect)) = genau dieser Bereich.
    fn schaden_beitrag(&self) -> Option<Option<Rechteck>> {
        if self.neu_zeichnen {
            Some(self.schaden)
        } else {
            None
        }
    }

    /// Kombiniert zwei Reaktionen (Container sammeln Kind-Reaktionen).
    /// Der Schaden wird zur Bounding-Box vereint; sobald ein Beteiligter
    /// vollflaechig neu will, gewinnt das Vollbild.
    pub fn und(self, andere: UiReaktion) -> UiReaktion {
        let schaden = match (self.schaden_beitrag(), andere.schaden_beitrag()) {
            (None, None) => None,
            (Some(x), None) | (None, Some(x)) => x,
            (Some(Some(a)), Some(Some(b))) => Some(a.umschliessen(&b)),
            (Some(_), Some(_)) => None,
        };
        UiReaktion {
            verbraucht: self.verbraucht || andere.verbraucht,
            neu_zeichnen: self.neu_zeichnen || andere.neu_zeichnen,
            schaden,
            nachricht: self.nachricht.or(andere.nachricht),
        }
    }
}

// ---------------------------------------------------------------------------
// Das Widget-Trait
// ---------------------------------------------------------------------------

/// Ein UI-Baustein.
///
/// `Send`, weil Fenster-Inhalte im Kernel durch einen globalen Manager
/// wandern. Ein einzelner Prozess braucht die Schranke nicht, sie kostet
/// ihn aber nichts.
///
/// DER SIGNATUR-BRUCH GEGENUEBER SERIE 3 ist genau dieser: `wunschgroesse`
/// und `ereignis` bekommen einen `&UiKontext`, und `zeichnen` bekommt einen
/// `Maler` statt eines `Zeichner<'_, FensterPuffer>`. Mehr aendert sich
/// nicht — die vier Voreinstellungen darunter sind unberuehrt.
pub trait Widget: Send {
    /// Gewuenschte Groesse (breite, hoehe) in Pixeln. Container summieren
    /// die Wuensche; Fueller wuenschen 0 und dehnen sich ueber `flex()`.
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32);

    /// Zeichnet das Widget in seinen zugewiesenen Bereich.
    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck);

    /// Verarbeitet ein Ereignis (`bereich` = der eigene Bereich aus
    /// demselben Layout-Durchlauf).
    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, k: &UiKontext) -> UiReaktion;

    /// Flex-Faktor: 0 = feste Wunschgroesse, >0 = Anteil am uebrigen Platz.
    fn flex(&self) -> i32 {
        0
    }

    /// Haelt dieses Widget (oder ein Kind) gerade den Tastatur-Fokus?
    fn hat_fokus(&self) -> bool {
        false
    }

    /// Tab-Kette: Fokus im Teilbaum einen Schritt weiterschieben.
    /// Blaetter: Fokus NEHMEN, wenn sie ihn nicht haben (-> true),
    /// ABGEBEN, wenn sie ihn haben (-> false, der Naechste ist dran).
    fn fokus_weiter(&mut self) -> bool {
        false
    }

    /// Fokus im ganzen Teilbaum entfernen (vor Klick-Fokuswechsel).
    fn fokus_entfernen(&mut self) {}
}

// ---------------------------------------------------------------------------
// Layout: bewusst primitiv — VBox, HBox, Fueller (kein Constraint-Solver)
// ---------------------------------------------------------------------------

/// DIE Layout-Rechnung (reine Funktion, unit-getestet): verteilt `laenge`
/// Pixel auf Elemente mit (wunsch, flex). Feste Elemente bekommen ihren
/// Wunsch, der Rest geht anteilig an die flex-Elemente; dazwischen liegt je
/// `abstand`. Rueckgabe: (start, groesse) je Element.
pub fn laengen_verteilen(elemente: &[(i32, i32)], laenge: i32, abstand: i32) -> Vec<(i32, i32)> {
    let anzahl = elemente.len() as i32;
    if anzahl == 0 {
        return Vec::new();
    }
    let abstaende = abstand * (anzahl - 1);
    let fest: i32 = elemente.iter().filter(|(_, f)| *f == 0).map(|(w, _)| w).sum();
    let flex_summe: i32 = elemente.iter().map(|(_, f)| *f).sum();
    let uebrig = (laenge - abstaende - fest).max(0);

    let mut ergebnis = Vec::with_capacity(elemente.len());
    let mut position = 0;
    let mut flex_vergeben = 0;
    let mut flex_gesehen = 0;
    for &(wunsch, flex) in elemente {
        let groesse = if flex == 0 {
            wunsch
        } else {
            // Ganzzahlig fair verteilen: kumulativ runden, damit die Summe
            // exakt aufgeht (der Letzte bekommt den Rest).
            flex_gesehen += flex;
            let bis_hier = uebrig * flex_gesehen / flex_summe;
            let anteil = bis_hier - flex_vergeben;
            flex_vergeben = bis_hier;
            anteil
        };
        ergebnis.push((position, groesse));
        position += groesse + abstand;
    }
    ergebnis
}

/// Richtung eines Box-Containers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Richtung {
    Vertikal,
    Horizontal,
}

/// Der Box-Container: reiht Kinder mit festem Abstand auf.
/// VBox = untereinander, HBox = nebeneinander.
pub struct BoxContainer {
    richtung: Richtung,
    kinder: Vec<Box<dyn Widget>>,
    /// Ueber welchem Kind schwebt der Cursor? (fuer MausRein/MausRaus)
    hover_kind: Option<usize>,
    /// Flex-Faktor des Containers SELBST im Eltern-Layout.
    flex: i32,
}

/// Vertikale Box (Kinder untereinander).
pub fn vbox(kinder: Vec<Box<dyn Widget>>) -> BoxContainer {
    BoxContainer { richtung: Richtung::Vertikal, kinder, hover_kind: None, flex: 0 }
}

/// Horizontale Box (Kinder nebeneinander).
pub fn hbox(kinder: Vec<Box<dyn Widget>>) -> BoxContainer {
    BoxContainer { richtung: Richtung::Horizontal, kinder, hover_kind: None, flex: 0 }
}

impl BoxContainer {
    /// Builder: Der Container dehnt sich im Eltern-Layout.
    pub fn mit_flex(mut self, flex: i32) -> Self {
        self.flex = flex;
        self
    }

    /// Die Kind-Bereiche fuer einen Gesamtbereich (Layout-Durchlauf).
    fn bereiche(&self, bereich: Rechteck, k: &UiKontext) -> Vec<Rechteck> {
        let elemente: Vec<(i32, i32)> = self
            .kinder
            .iter()
            .map(|kind| {
                let (b, h) = kind.wunschgroesse(k);
                let wunsch = if self.richtung == Richtung::Vertikal { h } else { b };
                (wunsch, kind.flex())
            })
            .collect();
        let laenge = if self.richtung == Richtung::Vertikal {
            bereich.hoehe
        } else {
            bereich.breite
        };
        laengen_verteilen(&elemente, laenge, k.abstand())
            .into_iter()
            .map(|(start, groesse)| match self.richtung {
                Richtung::Vertikal => {
                    Rechteck::neu(bereich.x, bereich.y + start, bereich.breite, groesse)
                }
                Richtung::Horizontal => {
                    Rechteck::neu(bereich.x + start, bereich.y, groesse, bereich.hoehe)
                }
            })
            .collect()
    }

    /// Kind-Index unter der Position.
    fn kind_bei(&self, bereiche: &[Rechteck], x: i32, y: i32) -> Option<usize> {
        bereiche.iter().position(|r| r.enthaelt(x, y))
    }
}

impl Widget for BoxContainer {
    fn flex(&self) -> i32 {
        self.flex
    }

    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        let mut haupt = 0;
        let mut quer = 0;
        for (i, kind) in self.kinder.iter().enumerate() {
            let (b, h) = kind.wunschgroesse(k);
            let (kind_haupt, kind_quer) = match self.richtung {
                Richtung::Vertikal => (h, b),
                Richtung::Horizontal => (b, h),
            };
            haupt += kind_haupt + if i > 0 { k.abstand() } else { 0 };
            quer = quer.max(kind_quer);
        }
        match self.richtung {
            Richtung::Vertikal => (quer, haupt),
            Richtung::Horizontal => (haupt, quer),
        }
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        for (kind, kind_bereich) in self.kinder.iter().zip(self.bereiche(bereich, &m.kontext)) {
            kind.zeichnen(m, kind_bereich);
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, k: &UiKontext) -> UiReaktion {
        let bereiche = self.bereiche(bereich, k);
        match ereignis {
            // Maus-Ereignisse: ans Kind unter dem Cursor — Bewegt erzeugt
            // dabei MausRein/MausRaus (DAS Hover-Routing).
            UiEreignis::Bewegt { x, y } => {
                let neu = self.kind_bei(&bereiche, *x, *y);
                let mut reaktion = UiReaktion::ignoriert();
                if neu != self.hover_kind {
                    if let Some(alt) = self.hover_kind {
                        reaktion = reaktion.und(self.kinder[alt].ereignis(
                            &UiEreignis::MausRaus,
                            bereiche[alt],
                            k,
                        ));
                    }
                    if let Some(index) = neu {
                        reaktion = reaktion.und(self.kinder[index].ereignis(
                            &UiEreignis::MausRein,
                            bereiche[index],
                            k,
                        ));
                    }
                    self.hover_kind = neu;
                }
                if let Some(index) = neu {
                    reaktion =
                        reaktion.und(self.kinder[index].ereignis(ereignis, bereiche[index], k));
                }
                reaktion
            }
            UiEreignis::MausRaus => {
                // Der Cursor hat den GANZEN Container verlassen:
                let mut reaktion = UiReaktion::ignoriert();
                if let Some(alt) = self.hover_kind.take() {
                    reaktion =
                        self.kinder[alt].ereignis(&UiEreignis::MausRaus, bereiche[alt], k);
                }
                reaktion
            }
            UiEreignis::Klick { x, y }
            | UiEreignis::Rechtsklick { x, y }
            | UiEreignis::Doppelklick { x, y }
            | UiEreignis::Losgelassen { x, y }
            | UiEreignis::Scroll { x, y, .. } => match self.kind_bei(&bereiche, *x, *y) {
                Some(index) => self.kinder[index].ereignis(ereignis, bereiche[index], k),
                None => UiReaktion::ignoriert(),
            },
            // MausRein an den Container selbst: ignorieren — das folgende
            // Bewegt hovert gezielt das richtige Kind.
            UiEreignis::MausRein => UiReaktion::ignoriert(),
            // Tastatur/Fokus: der Reihe nach, bis ein Kind es verbraucht.
            UiEreignis::Taste(_) | UiEreignis::FokusRein | UiEreignis::FokusRaus => {
                for (kind, kind_bereich) in self.kinder.iter_mut().zip(bereiche.iter()) {
                    let reaktion = kind.ereignis(ereignis, *kind_bereich, k);
                    if reaktion.verbraucht {
                        return reaktion;
                    }
                }
                UiReaktion::ignoriert()
            }
        }
    }

    fn hat_fokus(&self) -> bool {
        self.kinder.iter().any(|kind| kind.hat_fokus())
    }

    fn fokus_weiter(&mut self) -> bool {
        // Haelt ein Kind den Fokus? Dann DORT weiterschieben; wandert er
        // dort hinaus, sind die NACHFOLGENDEN Kinder dran.
        let ab = match self.kinder.iter().position(|kind| kind.hat_fokus()) {
            Some(index) => {
                if self.kinder[index].fokus_weiter() {
                    return true;
                }
                index + 1
            }
            None => 0,
        };
        for kind in &mut self.kinder[ab..] {
            if kind.fokus_weiter() {
                return true;
            }
        }
        false
    }

    fn fokus_entfernen(&mut self) {
        for kind in &mut self.kinder {
            kind.fokus_entfernen();
        }
    }
}

/// Der Fueller: wuenscht nichts, dehnt sich ueber flex — schiebt z. B. in
/// einer HBox die Buttons nach rechts.
pub struct Fueller;

impl Widget for Fueller {
    fn wunschgroesse(&self, _k: &UiKontext) -> (i32, i32) {
        (0, 0)
    }
    fn flex(&self) -> i32 {
        1
    }
    fn zeichnen(&self, _m: &mut Maler<'_>, _bereich: Rechteck) {}
    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// UiFenster — die Bruecke zwischen Widget-Baum und Fenster-System
// ---------------------------------------------------------------------------

/// Doppelklick-Erkennung: maximaler Abstand zweier Klicks
/// (500 ms — der klassische Standardwert).
const DOPPELKLICK_US: u64 = 500_000;
const DOPPELKLICK_PIXEL: i32 = 6;

/// Ein kompletter Widget-Fenster-Inhalt: Wurzel-Widget, Nachricht-Handler,
/// Doppelklick-Erkennung.
///
/// Was hier NICHT mehr steht: der konkrete Fenster-Puffer. Statt seiner
/// bekommen die Methoden die MASSE der Flaeche und (beim Zeichnen) eine
/// Leinwand — dadurch ist dieselbe Klasse im Kernel-Fenster und im
/// Prozess-Fenster benutzbar.
pub struct UiFenster {
    wurzel: Box<dyn Widget>,
    handler: NachrichtHandler,
    pub icon: &'static Icon,
    letzter_klick_us: u64,
    letzter_klick: (i32, i32),
}

impl UiFenster {
    pub fn neu(
        wurzel: Box<dyn Widget>,
        handler: NachrichtHandler,
        icon: &'static Icon,
    ) -> Self {
        // `letzter_klick` harmlos initialisieren (NICHT i32::MIN — die
        // Abstands-Subtraktion wuerde beim ersten schnellen Klick nach dem
        // Start ueberlaufen):
        UiFenster { wurzel, handler, icon, letzter_klick_us: 0, letzter_klick: (-1000, -1000) }
    }

    pub fn handler(&self) -> NachrichtHandler {
        self.handler
    }

    /// Ersetzt den Widget-Baum (App-Trait: nach Zustandsaenderung liefert
    /// `aufbau()` einen frischen Baum).
    pub fn wurzel_setzen(&mut self, wurzel: Box<dyn Widget>) {
        self.wurzel = wurzel;
    }

    /// Der Bereich des Wurzel-Widgets (Flaeche minus Rand).
    pub fn wurzel_bereich(masse: (i32, i32), k: &UiKontext) -> Rechteck {
        let rand = k.mass(Mass::UiRand);
        Rechteck::neu(rand, rand, masse.0 - 2 * rand, masse.1 - 2 * rand)
    }

    /// Zeichnet den kompletten Baum auf die Leinwand.
    pub fn zeichnen(&self, leinwand: &mut dyn Leinwand, k: &UiKontext) {
        let masse = leinwand.masse();
        let bereich = Self::wurzel_bereich(masse, k);
        let hintergrund = k.farbe(Farbrolle::InhaltHintergrund);
        let mut m = Maler::neu(leinwand, *k);
        m.fuellen(Rechteck::neu(0, 0, masse.0, masse.1), hintergrund);
        self.wurzel.zeichnen(&mut m, bereich);
    }

    /// Zeichnet NUR den Schadensbereich neu (Performance-Pfad). Die
    /// Leinwand bekommt `schaden` als Clip — der Baum-Durchlauf ist
    /// derselbe, aber es werden nur Pixel INNERHALB des Schadens
    /// geschrieben (und clip-bewusste Widgets wie der Editor sparen sich
    /// die Zeilen ausserhalb ganz).
    pub fn zeichnen_bereich(&self, leinwand: &mut dyn Leinwand, schaden: Rechteck, k: &UiKontext) {
        let masse = leinwand.masse();
        let bereich = Self::wurzel_bereich(masse, k);
        let hintergrund = k.farbe(Farbrolle::InhaltHintergrund);
        let mut m = Maler::neu(leinwand, *k);
        m.clip_setzen(Some(schaden));
        m.fuellen(schaden, hintergrund);
        self.wurzel.zeichnen(&mut m, bereich);
    }

    /// Ein Maus-Ereignis in Fenster-Koordinaten. Erkennt Doppelklicks.
    pub fn maus(&mut self, ereignis: UiEreignis, masse: (i32, i32), k: &UiKontext) -> UiReaktion {
        let bereich = Self::wurzel_bereich(masse, k);
        // Klick auf Doppelklick pruefen (und Fokus per Klick wechseln:
        // erst allen Fokus nehmen, das getroffene Widget nimmt ihn sich in
        // seiner Klick-Behandlung zurueck):
        if let UiEreignis::Klick { x, y } = ereignis {
            self.wurzel.fokus_entfernen();
            let jetzt = k.uhr.us();
            let (lx, ly) = self.letzter_klick;
            let doppel = jetzt.saturating_sub(self.letzter_klick_us) < DOPPELKLICK_US
                && (x - lx).abs() <= DOPPELKLICK_PIXEL
                && (y - ly).abs() <= DOPPELKLICK_PIXEL;
            self.letzter_klick_us = jetzt;
            self.letzter_klick = (x, y);
            if doppel {
                // Doppelklick ERSETZT den zweiten Klick nicht — beide
                // Ereignisse laufen (erst Klick, dann Doppelklick), damit
                // z. B. die Listen-Auswahl konsistent bleibt. Die
                // DOPPELKLICK-Nachricht hat aber Vorrang (`und` behaelt nur
                // eine): Die Auswahl-Nachricht kam schon beim ERSTEN Klick
                // bei der App an.
                let klick = self.wurzel.ereignis(&ereignis, bereich, k);
                let doppelklick =
                    self.wurzel
                        .ereignis(&UiEreignis::Doppelklick { x, y }, bereich, k);
                return doppelklick.und(klick).und(UiReaktion::neu_zeichnen());
            }
            // Klick zeichnet immer neu (Fokus koennte gewandert sein).
            return self
                .wurzel
                .ereignis(&ereignis, bereich, k)
                .und(UiReaktion::neu_zeichnen());
        }
        self.wurzel.ereignis(&ereignis, bereich, k)
    }

    /// Eine Taste (das Fenster hat den Tastatur-Fokus). Tab schaltet die
    /// Fokus-Kette weiter (mit Wrap-Around).
    pub fn taste(&mut self, taste: Taste, masse: (i32, i32), k: &UiKontext) -> UiReaktion {
        if taste.ist_tab() {
            if !self.wurzel.fokus_weiter() {
                // Am Ende angekommen: von vorn (Wrap-Around).
                self.wurzel.fokus_weiter();
            }
            return UiReaktion::neu_zeichnen();
        }
        let bereich = Self::wurzel_bereich(masse, k);
        self.wurzel.ereignis(&UiEreignis::Taste(taste), bereich, k)
    }

    /// Setzt den Fokus aufs erste fokussierbare Widget (falls noch keins
    /// fokussiert ist) — z. B. die Dateiliste des Explorers, damit
    /// Pfeiltasten sofort funktionieren.
    pub fn fokus_initial(&mut self) {
        if !self.wurzel.hat_fokus() {
            self.wurzel.fokus_weiter();
        }
    }

    /// Braucht das Fenster periodisches Neuzeichnen? (blinkender
    /// Textfeld-Cursor)
    pub fn blinkt(&self) -> bool {
        self.wurzel.hat_fokus()
    }
}

// ---------------------------------------------------------------------------
// Attrappen — der Wirt fuer Tests (und die Vorlage fuer echte Wirte)
// ---------------------------------------------------------------------------

pub mod attrappe;

// ---------------------------------------------------------------------------
// Tests — Layout und Routing, ganz ohne Bildschirm
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use attrappe::TestWirt;
    use widgets::{Button, Label, Textfeld};

    /// Layout: feste Wuensche, Abstaende, flex-Verteilung geht exakt auf.
    #[test]
    fn test_laengen_verteilen() {
        // Zwei feste Elemente mit Abstand 8:
        let fest = laengen_verteilen(&[(30, 0), (50, 0)], 200, 8);
        assert_eq!(fest, vec![(0, 30), (38, 50)]);

        // Fest + zwei flex (1:3) auf 100 Pixel, Abstand 0:
        // uebrig = 100 - 20 = 80 -> 20 und 60.
        let flex = laengen_verteilen(&[(20, 0), (0, 1), (0, 3)], 100, 0);
        assert_eq!(flex, vec![(0, 20), (20, 20), (40, 60)]);

        // Krumme Teilung: 3 x flex(1) auf 100 -> 33+33+34 = 100.
        let krumm = laengen_verteilen(&[(0, 1), (0, 1), (0, 1)], 100, 0);
        let summe: i32 = krumm.iter().map(|(_, g)| g).sum();
        assert_eq!(summe, 100);
        assert_eq!(krumm[2].0 + krumm[2].1, 100);
    }

    /// Event-Routing: Der Klick landet im richtigen Kind (und nur dort).
    #[test]
    fn test_klick_routing() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut baum = vbox(vec![
            w(Button::neu("Eins", 1)),
            w(Button::neu("Zwei", 2)),
        ]);
        let bereich = Rechteck::neu(0, 0, 200, 200);
        // Button-Hoehe = ElementHoehe (30), Abstand 8:
        // Button "Zwei" beginnt bei y=38. Klick+Loslassen bei y=45:
        baum.ereignis(&UiEreignis::Klick { x: 10, y: 45 }, bereich, &k);
        let reaktion = baum.ereignis(&UiEreignis::Losgelassen { x: 10, y: 45 }, bereich, &k);
        assert_eq!(reaktion.nachricht, Some(2));

        // Klick ins Leere (y=150): keine Nachricht.
        baum.ereignis(&UiEreignis::Klick { x: 10, y: 150 }, bereich, &k);
        let leer = baum.ereignis(&UiEreignis::Losgelassen { x: 10, y: 150 }, bereich, &k);
        assert_eq!(leer.nachricht, None);
    }

    /// Hover-Routing: Bewegt erzeugt MausRein/MausRaus beim Wechsel.
    #[test]
    fn test_hover_enter_leave() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut baum = vbox(vec![
            w(Button::neu("Eins", 1)),
            w(Button::neu("Zwei", 2)),
        ]);
        let bereich = Rechteck::neu(0, 0, 200, 200);

        baum.ereignis(&UiEreignis::Bewegt { x: 10, y: 10 }, bereich, &k);
        assert_eq!(baum.hover_kind, Some(0));
        baum.ereignis(&UiEreignis::Bewegt { x: 10, y: 45 }, bereich, &k);
        assert_eq!(baum.hover_kind, Some(1));
        baum.ereignis(&UiEreignis::MausRaus, bereich, &k);
        assert_eq!(baum.hover_kind, None);
    }

    /// Fokus-Kette: Tab wandert durch beide Textfelder und faengt danach
    /// wieder von vorn an; Tasten landen NUR im fokussierten.
    #[test]
    fn test_fokus_kette_mit_tab() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut ui = UiFenster::neu(
            Box::new(vbox(vec![
                w(Label::neu("Titel")),
                w(Textfeld::neu(10)),
                w(Textfeld::neu(11)),
            ])),
            |_| {},
            attrappe::TEST_ICON,
        );
        let masse = (300, 200);

        // Erstes Tab: das erste Textfeld bekommt den Fokus.
        ui.taste(Taste::Zeichen('\t'), masse, &k);
        ui.taste(Taste::Zeichen('a'), masse, &k);
        // Zweites Tab: der Fokus wandert weiter.
        ui.taste(Taste::Zeichen('\t'), masse, &k);
        ui.taste(Taste::Zeichen('b'), masse, &k);
        assert!(ui.blinkt(), "irgendwo muss der Fokus sein");
    }

    /// Die Schadens-Kombination: zwei Teilschaeden ergeben ihre
    /// Bounding-Box, ein Vollbild-Beitrag gewinnt gegen alles.
    #[test]
    fn test_schaden_kombinieren() {
        let a = UiReaktion::neu_zeichnen_bereich(Rechteck::neu(0, 0, 10, 10));
        let b = UiReaktion::neu_zeichnen_bereich(Rechteck::neu(90, 90, 10, 10));
        assert_eq!(a.und(b).schaden, Some(Rechteck::neu(0, 0, 100, 100)));

        // Vollbild (neu_zeichnen ohne Rect) schlaegt jeden Teilschaden:
        assert_eq!(a.und(UiReaktion::neu_zeichnen()).schaden, None);
        // Wer nicht neu zeichnet, traegt nichts bei:
        assert_eq!(a.und(UiReaktion::ignoriert()).schaden, a.schaden);
        // Und die Nachricht des ERSTEN gewinnt:
        let mit = UiReaktion::nachricht(7).und(UiReaktion::nachricht(9));
        assert_eq!(mit.nachricht, Some(7));
    }
}
