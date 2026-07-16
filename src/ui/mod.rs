// ui/mod.rs — Das Widget-Toolkit: das UI-Fundament aller SpeedOS-Apps
//
// ARCHITEKTUR (retained Widget-Baum, siehe CLAUDE.md):
//   * Ein Fenster-Inhalt = EIN Widget-Baum (Wurzel meist VBox/HBox).
//     Jedes Widget zeichnet sich in den Fenster-Puffer und reagiert
//     auf Ereignisse — Farben/Abstände IMMER aus theme/metrik().
//   * Koordinaten: ALLES in Fensterinhalt-Koordinaten. Ein Widget
//     bekommt sein zugewiesenes Rechteck (`bereich`) mitgereicht und
//     prüft selbst `bereich.enthaelt(x, y)` — dasselbe Rechteck-
//     Routing wie kante_bei im Fenster-Manager, nur im Kleinen.
//   * Ereignisse wandern den Baum HINUNTER (Container routen an das
//     Kind unter dem Cursor und erzeugen dabei MausRein/MausRaus —
//     das Hover-Konzept, das dem Fenster-System bisher fehlte).
//     Reaktionen wandern HINAUF (UiReaktion, siehe unten).
//   * App-Nachrichten: Widgets melden Klicks & Co. als u32-ID nach
//     oben (UiReaktion.nachricht); das UiFenster reicht sie an einen
//     fn(u32)-Handler. WARUM IDs statt Closures oder generischem
//     Nachrichtentyp? (1) Box<dyn FnMut> bräuchte Zugriff auf den
//     App-Zustand, der teils selbst im Baum steckt — Borrow-Hölle.
//     (2) Ein generischer Typ Widget<M> würde Manager und Fenster
//     infizieren und das Trait un-objektsicher machen. (3) fn(u32)
//     ist Send, zustandslos und passt zum App-Registry-Muster;
//     zustandsbehaftete Apps bekommen später ein App-Trait
//     (Bestandsaufnahme Serie 2). Das klassische Message-Modell.
//   * Fokus-Kette: Tab wandert durch alle fokussierbaren Widgets
//     (fokus_weiter, mit Wrap-Around), Tastatur läuft den Baum
//     hinunter, bis das fokussierte Widget sie verbraucht.
//
// DEADLOCK-REGEL (wie App-Starts): Nachrichten-Handler werden NIE
// unter dem MANAGER-Lock ausgeführt — der Fenster-Manager reicht sie
// als NachLock-Wert nach draußen.

pub mod app;
pub mod widgets;

pub use app::{App, AppFenster, AppReaktion};

use crate::fenster::FensterPuffer;
use crate::grafik::{Rechteck, Zeichenflaeche, Zeichner};
use crate::theme::metrik;
use alloc::boxed::Box;
use alloc::vec::Vec;
use pc_keyboard::DecodedKey;

/// Der Nachrichten-Kanal vom Widget-Baum zur App (siehe Kopfkommentar).
pub type NachrichtHandler = fn(u32);

// ---------------------------------------------------------------------------
// Ereignisse und Reaktionen
// ---------------------------------------------------------------------------

/// Ein Ereignis für ein Widget — Positionen in Fensterinhalt-
/// Koordinaten (dasselbe System wie das `bereich`-Rechteck).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEreignis {
    /// Linke Maustaste gedrückt.
    Klick { x: i32, y: i32 },
    /// Rechte Maustaste gedrückt (Kontextmenü-Anlass).
    Rechtsklick { x: i32, y: i32 },
    /// Zweiter Klick kurz nach dem ersten an (fast) derselben Stelle —
    /// erkennt das UiFenster über die zeit-API.
    Doppelklick { x: i32, y: i32 },
    /// Linke Maustaste losgelassen.
    Losgelassen { x: i32, y: i32 },
    /// Mausbewegung (auch ohne gedrückte Taste — fürs Hovern).
    Bewegt { x: i32, y: i32 },
    /// Scrollrad (delta > 0 = nach oben).
    Scroll { delta: i8, x: i32, y: i32 },
    /// Taste ans fokussierte Widget.
    Taste(DecodedKey),
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

/// Was ein Widget als Antwort meldet. Bewusst ein STRUCT statt Enum:
/// Ein Klick auf einen Button ist verbraucht UND will neu gezeichnet
/// werden UND trägt eine Nachricht — das sind kombinierbare Wirkungen,
/// keine Alternativen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiReaktion {
    /// Das Ereignis ist behandelt — nicht weiterreichen.
    pub verbraucht: bool,
    /// Der Fenster-Inhalt muss neu gezeichnet werden.
    pub neu_zeichnen: bool,
    /// Eine Nachricht an die App (Widget-abhängige ID).
    pub nachricht: Option<u32>,
}

impl UiReaktion {
    pub const fn ignoriert() -> Self {
        UiReaktion { verbraucht: false, neu_zeichnen: false, nachricht: None }
    }
    pub const fn verbraucht() -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: false, nachricht: None }
    }
    pub const fn neu_zeichnen() -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: true, nachricht: None }
    }
    pub const fn nachricht(id: u32) -> Self {
        UiReaktion { verbraucht: true, neu_zeichnen: true, nachricht: Some(id) }
    }
    /// Kombiniert zwei Reaktionen (Container sammeln Kind-Reaktionen).
    pub fn und(self, andere: UiReaktion) -> UiReaktion {
        UiReaktion {
            verbraucht: self.verbraucht || andere.verbraucht,
            neu_zeichnen: self.neu_zeichnen || andere.neu_zeichnen,
            nachricht: self.nachricht.or(andere.nachricht),
        }
    }
}

// ---------------------------------------------------------------------------
// Das Widget-Trait
// ---------------------------------------------------------------------------

/// Ein UI-Baustein. `Send`, weil Fenster-Inhalte durch den globalen
/// Manager wandern (dieselbe Anforderung wie bei Tasks).
pub trait Widget: Send {
    /// Gewünschte Größe (breite, hoehe) in Pixeln. Container summieren
    /// die Wünsche; Füller wünschen 0 und dehnen sich über flex().
    fn wunschgroesse(&self) -> (i32, i32);

    /// Zeichnet das Widget in seinen zugewiesenen Bereich.
    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck);

    /// Verarbeitet ein Ereignis (Positionen in Fenster-Koordinaten,
    /// `bereich` = der eigene Bereich aus demselben Layout-Durchlauf).
    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion;

    /// Flex-Faktor: 0 = feste Wunschgröße, >0 = bekommt einen Anteil
    /// des übrigen Platzes (Verhältnis der Faktoren).
    fn flex(&self) -> i32 {
        0
    }

    /// Hält dieses Widget (oder ein Kind) gerade den Tastatur-Fokus?
    fn hat_fokus(&self) -> bool {
        false
    }

    /// Tab-Kette: Fokus im Teilbaum einen Schritt weiterschieben.
    /// Blätter: Fokus NEHMEN, wenn sie ihn nicht haben (-> true),
    /// ABGEBEN, wenn sie ihn haben (-> false, der Nächste ist dran).
    fn fokus_weiter(&mut self) -> bool {
        false
    }

    /// Fokus im ganzen Teilbaum entfernen (vor Klick-Fokuswechsel).
    fn fokus_entfernen(&mut self) {}
}

// ---------------------------------------------------------------------------
// Layout: bewusst primitiv — VBox, HBox, Füller (kein Constraint-Solver)
// ---------------------------------------------------------------------------

/// DIE Layout-Rechnung (reine Funktion, unit-getestet): verteilt
/// `laenge` Pixel auf Elemente mit (wunsch, flex). Feste Elemente
/// bekommen ihren Wunsch, der Rest geht anteilig an die flex-Elemente;
/// dazwischen liegt je `abstand`. Rückgabe: (start, groesse) je Element.
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
            // Ganzzahlig fair verteilen: kumulativ runden, damit die
            // Summe exakt aufgeht (der Letzte bekommt den Rest).
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

/// Der Box-Container: reiht Kinder mit festem Abstand (METRIK) auf.
/// VBox = untereinander, HBox = nebeneinander.
pub struct BoxContainer {
    richtung: Richtung,
    kinder: Vec<Box<dyn Widget>>,
    /// Über welchem Kind schwebt der Cursor? (für MausRein/MausRaus)
    hover_kind: Option<usize>,
    /// Flex-Faktor des Containers SELBST im Eltern-Layout (0 = fest —
    /// z. B. die Explorer-Mittelzeile dehnt sich mit flex 1).
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
}

impl BoxContainer {
    /// Die Kind-Bereiche für einen Gesamtbereich (Layout-Durchlauf).
    fn bereiche(&self, bereich: Rechteck) -> Vec<Rechteck> {
        let elemente: Vec<(i32, i32)> = self
            .kinder
            .iter()
            .map(|kind| {
                let (b, h) = kind.wunschgroesse();
                let wunsch = if self.richtung == Richtung::Vertikal { h } else { b };
                (wunsch, kind.flex())
            })
            .collect();
        let laenge = if self.richtung == Richtung::Vertikal { bereich.hoehe } else { bereich.breite };
        laengen_verteilen(&elemente, laenge, metrik().abstand)
            .into_iter()
            .map(|(start, groesse)| match self.richtung {
                Richtung::Vertikal => Rechteck::neu(bereich.x, bereich.y + start, bereich.breite, groesse),
                Richtung::Horizontal => Rechteck::neu(bereich.x + start, bereich.y, groesse, bereich.hoehe),
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

    fn wunschgroesse(&self) -> (i32, i32) {
        let mut haupt = 0;
        let mut quer = 0;
        for (i, kind) in self.kinder.iter().enumerate() {
            let (b, h) = kind.wunschgroesse();
            let (kind_haupt, kind_quer) = match self.richtung {
                Richtung::Vertikal => (h, b),
                Richtung::Horizontal => (b, h),
            };
            haupt += kind_haupt + if i > 0 { metrik().abstand } else { 0 };
            quer = quer.max(kind_quer);
        }
        match self.richtung {
            Richtung::Vertikal => (quer, haupt),
            Richtung::Horizontal => (haupt, quer),
        }
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        for (kind, kind_bereich) in self.kinder.iter().zip(self.bereiche(bereich)) {
            kind.zeichnen(z, kind_bereich);
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        let bereiche = self.bereiche(bereich);
        match ereignis {
            // Maus-Ereignisse: ans Kind unter dem Cursor — Bewegt
            // erzeugt dabei MausRein/MausRaus (DAS Hover-Routing).
            UiEreignis::Bewegt { x, y } => {
                let neu = self.kind_bei(&bereiche, *x, *y);
                let mut reaktion = UiReaktion::ignoriert();
                if neu != self.hover_kind {
                    if let Some(alt) = self.hover_kind {
                        reaktion =
                            reaktion.und(self.kinder[alt].ereignis(&UiEreignis::MausRaus, bereiche[alt]));
                    }
                    if let Some(index) = neu {
                        reaktion = reaktion
                            .und(self.kinder[index].ereignis(&UiEreignis::MausRein, bereiche[index]));
                    }
                    self.hover_kind = neu;
                }
                if let Some(index) = neu {
                    reaktion = reaktion.und(self.kinder[index].ereignis(ereignis, bereiche[index]));
                }
                reaktion
            }
            UiEreignis::MausRaus => {
                // Der Cursor hat den GANZEN Container verlassen:
                let mut reaktion = UiReaktion::ignoriert();
                if let Some(alt) = self.hover_kind.take() {
                    reaktion = self.kinder[alt].ereignis(&UiEreignis::MausRaus, bereiche[alt]);
                }
                reaktion
            }
            UiEreignis::Klick { x, y }
            | UiEreignis::Rechtsklick { x, y }
            | UiEreignis::Doppelklick { x, y }
            | UiEreignis::Losgelassen { x, y }
            | UiEreignis::Scroll { x, y, .. } => match self.kind_bei(&bereiche, *x, *y) {
                Some(index) => self.kinder[index].ereignis(ereignis, bereiche[index]),
                None => UiReaktion::ignoriert(),
            },
            // MausRein an den Container selbst: ignorieren — das
            // folgende Bewegt hovert gezielt das richtige Kind
            // (alle Kinder zu hovern wäre falsch).
            UiEreignis::MausRein => UiReaktion::ignoriert(),
            // Tastatur/Fokus: der Reihe nach, bis ein Kind es verbraucht
            // (nur das fokussierte Widget verbraucht Tasten).
            UiEreignis::Taste(_) | UiEreignis::FokusRein | UiEreignis::FokusRaus => {
                for (kind, kind_bereich) in self.kinder.iter_mut().zip(bereiche.iter()) {
                    let reaktion = kind.ereignis(ereignis, *kind_bereich);
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
        // Hält ein Kind den Fokus? Dann DORT weiterschieben; wandert
        // er dort hinaus, sind die NACHFOLGENDEN Kinder dran.
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

/// Der Füller: wünscht nichts, dehnt sich über flex — schiebt z. B.
/// in einer HBox die Buttons nach rechts.
pub struct Fueller;

impl Widget for Fueller {
    fn wunschgroesse(&self) -> (i32, i32) {
        (0, 0)
    }
    fn flex(&self) -> i32 {
        1
    }
    fn zeichnen(&self, _z: &mut Zeichner<'_, FensterPuffer>, _bereich: Rechteck) {}
    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// UiFenster — die Brücke zwischen Widget-Baum und Fenster-System
// ---------------------------------------------------------------------------

/// Doppelklick-Erkennung: maximaler Abstand zweier Klicks
/// (500 ms — der klassische Windows-Standardwert).
const DOPPELKLICK_US: u64 = 500_000;
const DOPPELKLICK_PIXEL: i32 = 6;

/// Ein kompletter Widget-Fenster-Inhalt: Wurzel-Widget, Nachricht-
/// Handler, Doppelklick-Erkennung. Lebt als `Inhalt::Ui` im Fenster.
pub struct UiFenster {
    wurzel: Box<dyn Widget>,
    handler: NachrichtHandler,
    pub icon: &'static crate::grafik::Icon,
    letzter_klick_us: u64,
    letzter_klick: (i32, i32),
}

impl UiFenster {
    pub fn neu(
        wurzel: Box<dyn Widget>,
        handler: NachrichtHandler,
        icon: &'static crate::grafik::Icon,
    ) -> Self {
        // letzter_klick harmlos initialisieren (NICHT i32::MIN — die
        // Abstands-Subtraktion würde beim ersten schnellen Klick
        // nach dem Boot überlaufen):
        UiFenster { wurzel, handler, icon, letzter_klick_us: 0, letzter_klick: (-1000, -1000) }
    }

    pub fn handler(&self) -> NachrichtHandler {
        self.handler
    }

    /// Ersetzt den Widget-Baum (App-Trait: nach Zustandsänderung
    /// liefert aufbau() einen frischen Baum).
    pub fn wurzel_setzen(&mut self, wurzel: Box<dyn Widget>) {
        self.wurzel = wurzel;
    }

    /// Der Bereich des Wurzel-Widgets (Fenster-Inhalt minus Rand).
    fn wurzel_bereich(puffer: &FensterPuffer) -> Rechteck {
        Rechteck::neu(
            metrik().ui_rand,
            metrik().ui_rand,
            puffer.flaeche_breite() as i32 - 2 * metrik().ui_rand,
            puffer.flaeche_hoehe() as i32 - 2 * metrik().ui_rand,
        )
    }

    /// Zeichnet den kompletten Baum in den Fenster-Puffer.
    pub fn zeichnen(&self, puffer: &mut FensterPuffer) {
        use crate::grafik::Rgba;

        let thema = crate::theme::aktuell();
        let bereich = Self::wurzel_bereich(puffer);
        let (breite, hoehe) = (puffer.flaeche_breite() as i32, puffer.flaeche_hoehe() as i32);
        let mut z = Zeichner::neu(puffer);
        let hintergrund = thema.inhalt_hintergrund;
        z.rechteck_fuellen(
            Rechteck::neu(0, 0, breite, hoehe),
            Rgba::neu(hintergrund.r, hintergrund.g, hintergrund.b),
        );
        self.wurzel.zeichnen(&mut z, bereich);
    }

    /// Ein Maus-Ereignis in Fenster-Koordinaten. Erkennt Doppelklicks.
    pub fn maus(&mut self, ereignis: UiEreignis, puffer: &FensterPuffer) -> UiReaktion {
        let bereich = Self::wurzel_bereich(puffer);
        // Klick auf Doppelklick prüfen (und Fokus per Klick wechseln:
        // erst allen Fokus nehmen, das getroffene Widget nimmt ihn
        // sich in seiner Klick-Behandlung zurück):
        if let UiEreignis::Klick { x, y } = ereignis {
            self.wurzel.fokus_entfernen();
            let jetzt = crate::zeit::us_seit_boot();
            let (lx, ly) = self.letzter_klick;
            let doppel = jetzt - self.letzter_klick_us < DOPPELKLICK_US
                && (x - lx).abs() <= DOPPELKLICK_PIXEL
                && (y - ly).abs() <= DOPPELKLICK_PIXEL;
            self.letzter_klick_us = jetzt;
            self.letzter_klick = (x, y);
            if doppel {
                // Doppelklick ERSETZT den zweiten Klick nicht — beide
                // Ereignisse laufen (erst Klick, dann Doppelklick),
                // damit z. B. die Listen-Auswahl konsistent bleibt.
                // Die DOPPELKLICK-Nachricht hat aber Vorrang (und()
                // behält nur eine): Die Auswahl-Nachricht kam schon
                // beim ERSTEN Klick bei der App an.
                let klick = self.wurzel.ereignis(&ereignis, bereich);
                let doppelklick = self
                    .wurzel
                    .ereignis(&UiEreignis::Doppelklick { x, y }, bereich);
                return doppelklick.und(klick).und(UiReaktion::neu_zeichnen());
            }
            // Klick zeichnet immer neu (Fokus könnte gewandert sein).
            return self.wurzel.ereignis(&ereignis, bereich).und(UiReaktion::neu_zeichnen());
        }
        self.wurzel.ereignis(&ereignis, bereich)
    }

    /// Eine Taste (das Fenster hat den Tastatur-Fokus). Tab schaltet
    /// die Fokus-Kette weiter (mit Wrap-Around).
    pub fn taste(&mut self, taste: DecodedKey, puffer: &FensterPuffer) -> UiReaktion {
        if taste == DecodedKey::Unicode('\t') {
            if !self.wurzel.fokus_weiter() {
                // Am Ende angekommen: von vorn (Wrap-Around).
                self.wurzel.fokus_weiter();
            }
            return UiReaktion::neu_zeichnen();
        }
        let bereich = Self::wurzel_bereich(puffer);
        self.wurzel.ereignis(&UiEreignis::Taste(taste), bereich)
    }

    /// Braucht das Fenster periodisches Neuzeichnen? (blinkender
    /// Textfeld-Cursor — der Uhr-Task stößt es an)
    /// Setzt den Fokus aufs erste fokussierbare Widget (falls noch
    /// keins fokussiert ist) — z. B. die Dateiliste des Explorers,
    /// damit Pfeiltasten sofort funktionieren.
    pub fn fokus_initial(&mut self) {
        if !self.wurzel.hat_fokus() {
            self.wurzel.fokus_weiter();
        }
    }

    pub fn blinkt(&self) -> bool {
        self.wurzel.hat_fokus()
    }
}

// ---------------------------------------------------------------------------
// Tests — Layout und Routing, ganz ohne Bildschirm
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use widgets::{Button, Label, Textfeld};

    /// Layout: feste Wünsche, Abstände, flex-Verteilung geht exakt auf.
    #[test_case]
    fn test_laengen_verteilen() {
        // Zwei feste Elemente mit Abstand 8:
        let fest = laengen_verteilen(&[(30, 0), (50, 0)], 200, 8);
        assert_eq!(fest, alloc::vec![(0, 30), (38, 50)]);

        // Fest + zwei flex (1:3) auf 100 Pixel, Abstand 0:
        // übrig = 100 - 20 = 80 -> 20 und 60.
        let flex = laengen_verteilen(&[(20, 0), (0, 1), (0, 3)], 100, 0);
        assert_eq!(flex, alloc::vec![(0, 20), (20, 20), (40, 60)]);

        // Krumme Teilung: 3 x flex(1) auf 100 -> 33+33+34 = 100.
        let krumm = laengen_verteilen(&[(0, 1), (0, 1), (0, 1)], 100, 0);
        let summe: i32 = krumm.iter().map(|(_, g)| g).sum();
        assert_eq!(summe, 100);
        assert_eq!(krumm[2].0 + krumm[2].1, 100);
    }

    /// Event-Routing: Der Klick landet im richtigen Kind (und nur dort).
    #[test_case]
    fn test_klick_routing() {
        let mut baum = vbox(alloc::vec![
            Box::new(Button::neu("Eins", 1)) as Box<dyn Widget>,
            Box::new(Button::neu("Zwei", 2)),
        ]);
        let bereich = Rechteck::neu(0, 0, 200, 200);
        // Button-Höhe = metrik().ui_element_hoehe (30), Abstand 8:
        // Button "Zwei" beginnt bei y=38. Klick+Loslassen bei y=45:
        baum.ereignis(&UiEreignis::Klick { x: 10, y: 45 }, bereich);
        let reaktion = baum.ereignis(&UiEreignis::Losgelassen { x: 10, y: 45 }, bereich);
        assert_eq!(reaktion.nachricht, Some(2));

        // Klick ins Leere (y=150): keine Nachricht.
        baum.ereignis(&UiEreignis::Klick { x: 10, y: 150 }, bereich);
        let leer = baum.ereignis(&UiEreignis::Losgelassen { x: 10, y: 150 }, bereich);
        assert_eq!(leer.nachricht, None);
    }

    /// Hover-Routing: Bewegt erzeugt MausRein/MausRaus beim Wechsel.
    #[test_case]
    fn test_hover_enter_leave() {
        let mut baum = vbox(alloc::vec![
            Box::new(Button::neu("Eins", 1)) as Box<dyn Widget>,
            Box::new(Button::neu("Zwei", 2)),
        ]);
        let bereich = Rechteck::neu(0, 0, 200, 200);

        // Über Button Eins schweben -> Container merkt Kind 0:
        baum.ereignis(&UiEreignis::Bewegt { x: 10, y: 10 }, bereich);
        assert_eq!(baum.hover_kind, Some(0));
        // Zu Button Zwei wechseln -> hover wandert (Eins bekam MausRaus):
        baum.ereignis(&UiEreignis::Bewegt { x: 10, y: 45 }, bereich);
        assert_eq!(baum.hover_kind, Some(1));
        // Ganz hinaus -> kein Hover mehr:
        baum.ereignis(&UiEreignis::MausRaus, bereich);
        assert_eq!(baum.hover_kind, None);
    }

    /// Fokus-Kette: Tab wandert durch beide Textfelder und fängt
    /// danach wieder von vorn an; Tasten landen NUR im fokussierten.
    #[test_case]
    fn test_fokus_kette_mit_tab() {
        let mut ui = UiFenster::neu(
            Box::new(vbox(alloc::vec![
                Box::new(Label::neu("Titel")) as Box<dyn Widget>,
                Box::new(Textfeld::neu(10)),
                Box::new(Textfeld::neu(11)),
            ])),
            |_| {},
            &crate::grafik::ICON_LOGO,
        );
        let puffer = FensterPuffer::neu(300, 200, crate::framebuffer::Farbe::neu(0, 0, 0));

        assert!(!ui.wurzel.hat_fokus());
        ui.taste(DecodedKey::Unicode('\t'), &puffer); // -> Feld 1
        assert!(ui.wurzel.hat_fokus());
        ui.taste(DecodedKey::Unicode('a'), &puffer);
        ui.taste(DecodedKey::Unicode('\t'), &puffer); // -> Feld 2
        ui.taste(DecodedKey::Unicode('b'), &puffer);
        ui.taste(DecodedKey::Unicode('\t'), &puffer); // -> Wrap zu Feld 1
        ui.taste(DecodedKey::Unicode('c'), &puffer);

        // Nach dem Wrap hält wieder Feld 1 (ID 10) den Fokus —
        // sein Enter meldet seine Nachricht-ID:
        let enter = ui.taste(DecodedKey::Unicode('\n'), &puffer);
        assert_eq!(enter.nachricht, Some(10));
    }

    /// Doppelklick-Erkennung im UiFenster: zwei schnelle Klicks auf
    /// dieselbe Listen-Stelle melden die Doppelklick-Nachricht (die
    /// Vorrang vor der zweiten Auswahl-Nachricht hat).
    #[test_case]
    fn test_doppelklick_erkennung() {
        use widgets::{ListenEintrag, ScrollListe};

        let eintraege = (0..5)
            .map(|i| ListenEintrag { icon: None, text: alloc::format!("E{}", i) })
            .collect();
        let mut ui = UiFenster::neu(
            Box::new(ScrollListe::neu(eintraege, 50, 60)),
            |_| {},
            &crate::grafik::ICON_LOGO,
        );
        let puffer = FensterPuffer::neu(300, 200, crate::framebuffer::Farbe::neu(0, 0, 0));

        // Erster Klick: Auswahl-Nachricht.
        let erster = ui.maus(UiEreignis::Klick { x: 50, y: 30 }, &puffer);
        assert_eq!(erster.nachricht, Some(50));
        // Sofortiger zweiter Klick (µs-Abstand): Doppelklick gewinnt.
        let zweiter = ui.maus(UiEreignis::Klick { x: 51, y: 31 }, &puffer);
        assert_eq!(zweiter.nachricht, Some(60));
    }
}
