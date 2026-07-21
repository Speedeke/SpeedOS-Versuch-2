// ui/widgets.rs — Die Grund-Widgets im Aurora-Stil
//
// Label, Trennlinie, Button, Checkbox, Textfeld und ScrollListe —
// alle Farben aus theme::aktuell(), alle Maße aus METRIK (die
// Projektregel "keine hartcodierten Werte im UI-Code" gilt hier
// zuallererst). Zustandslogik, die sich rechnen lässt (Scroll-
// Klemmen, sichtbarer Ausschnitt), liegt in reinen Funktionen und
// ist unit-getestet.

use super::{UiEreignis, UiReaktion, Widget};
use crate::fenster::FensterPuffer;
use crate::grafik::{Icon, Rechteck, Zeichner};
use crate::shell::editor::{Reaktion, Taste, Vervollstaendiger, ZeilenEditor};
use crate::theme::{self, metrik};
use alloc::string::String;
use alloc::vec::Vec;
use noto_sans_mono_bitmap::{get_raster_width, FontWeight};
use pc_keyboard::{DecodedKey, KeyCode};

/// Breite eines Zeichens der UI-Schrift in Pixeln.
fn zeichen_breite() -> i32 {
    get_raster_width(FontWeight::Regular, metrik().schrift_ui) as i32
}

/// Text vertikal in einem Bereich zentrieren (y der Textoberkante).
fn text_mitte_y(bereich: Rechteck) -> i32 {
    bereich.y + (bereich.hoehe - metrik().zeilen_hoehe) / 2
}

// ---------------------------------------------------------------------------
// Label — auch mehrzeilig
// ---------------------------------------------------------------------------

pub struct Label {
    text: String,
    /// Gedimmter Nebentext statt normalem Text?
    sekundaer: bool,
}

impl Label {
    pub fn neu(text: &str) -> Self {
        Label { text: String::from(text), sekundaer: false }
    }
    /// Gedimmte Variante (Hinweis-/Nebentexte).
    pub fn sekundaer(text: &str) -> Self {
        Label { text: String::from(text), sekundaer: true }
    }
}

impl Widget for Label {
    fn wunschgroesse(&self) -> (i32, i32) {
        let zeilen = self.text.lines().count().max(1) as i32;
        let laengste = self.text.lines().map(|z| z.chars().count()).max().unwrap_or(0) as i32;
        (laengste * zeichen_breite(), zeilen * metrik().zeilen_hoehe)
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        let farbe = if self.sekundaer { thema.text_sekundaer } else { thema.text_normal };
        for (i, zeile) in self.text.lines().enumerate() {
            z.text(
                bereich.x,
                bereich.y + i as i32 * metrik().zeilen_hoehe,
                zeile,
                metrik().schrift_ui,
                FontWeight::Regular,
                farbe,
            );
        }
    }

    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Trennlinie
// ---------------------------------------------------------------------------

pub struct Trennlinie;

impl Widget for Trennlinie {
    fn wunschgroesse(&self) -> (i32, i32) {
        (0, metrik().abstand)
    }
    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let y = bereich.y + bereich.hoehe / 2;
        z.linie(bereich.x, y, bereich.x + bereich.breite - 1, y, theme::aktuell().rahmen_passiv);
    }
    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Button — mit Hover- und Gedrückt-Zustand, Icon optional
// ---------------------------------------------------------------------------

pub struct Button {
    text: String,
    icon: Option<&'static Icon>,
    nachricht: u32,
    hover: bool,
    gedrueckt: bool,
    /// Dauerhaft hervorgehoben (die AKTIVE Wahl in einer Options-
    /// Gruppe, z. B. das gewählte Theme in den Einstellungen).
    aktiv: bool,
    /// Ausgegraut und ohne Wirkung (z. B. "Task beenden" bei einem
    /// geschützten Kernel-Task).
    deaktiviert: bool,
}

impl Button {
    pub fn neu(text: &str, nachricht: u32) -> Self {
        Button {
            text: String::from(text),
            icon: None,
            nachricht,
            hover: false,
            gedrueckt: false,
            aktiv: false,
            deaktiviert: false,
        }
    }
    pub fn mit_icon(text: &str, icon: &'static Icon, nachricht: u32) -> Self {
        Button { icon: Some(icon), ..Button::neu(text, nachricht) }
    }
    /// Builder: als aktive Wahl markieren (Auswahl-Füllung + Akzent).
    pub fn mit_aktiv(mut self, aktiv: bool) -> Self {
        self.aktiv = aktiv;
        self
    }
    /// Builder: deaktivieren (gedimmt, schluckt keine Klicks).
    pub fn mit_deaktiviert(mut self, deaktiviert: bool) -> Self {
        self.deaktiviert = deaktiviert;
        self
    }
}

impl Widget for Button {
    fn wunschgroesse(&self) -> (i32, i32) {
        let icon_platz = if self.icon.is_some() { 22 } else { 0 };
        (
            self.text.chars().count() as i32 * zeichen_breite() + 2 * metrik().abstand + icon_platz + 8,
            metrik().ui_element_hoehe,
        )
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        // Zustand -> Füllung: gedrückt/aktiv (Auswahl) > hover > normal;
        // deaktiviert bleibt flach und ohne Akzent.
        let fuellung = if self.deaktiviert {
            thema.eingabefeld
        } else if self.gedrueckt || self.aktiv {
            thema.auswahl
        } else if self.hover {
            thema.leiste_knopf_aktiv
        } else {
            thema.eingabefeld
        };
        z.rechteck_abgerundet(bereich, metrik().radius_klein, fuellung);
        z.rechteck_rahmen(
            bereich,
            if !self.deaktiviert && (self.hover || self.aktiv) {
                thema.akzent
            } else {
                thema.rahmen_passiv
            },
        );

        let mut text_x = bereich.x
            + (bereich.breite
                - self.text.chars().count() as i32 * zeichen_breite()
                - if self.icon.is_some() { 22 } else { 0 })
                / 2;
        if let Some(icon) = self.icon {
            z.icon(text_x, text_mitte_y(bereich), icon, 1);
            text_x += 22;
        }
        z.text(
            text_x,
            text_mitte_y(bereich),
            &self.text,
            metrik().schrift_ui,
            FontWeight::Regular,
            if self.deaktiviert { thema.text_gedimmt } else { thema.text_stark },
        );
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        // Deaktiviert: reagiert auf nichts (auch kein Hover-Akzent).
        if self.deaktiviert {
            return UiReaktion::ignoriert();
        }
        // Ein Button ändert bei Hover/Klick NUR seine eigene Fläche —
        // er meldet exakt `bereich` als Schaden (statt das Fenster).
        match ereignis {
            UiEreignis::MausRein => {
                self.hover = true;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::MausRaus => {
                self.hover = false;
                self.gedrueckt = false;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                self.gedrueckt = true;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Losgelassen { x, y } => {
                // Nur ein Loslassen ÜBER dem gedrückten Button klickt —
                // wegziehen bricht ab (wie bei den "Großen").
                let war_gedrueckt = self.gedrueckt;
                self.gedrueckt = false;
                if war_gedrueckt && bereich.enthaelt(*x, *y) {
                    // Die Klick-Nachricht kann den ganzen Baum umbauen
                    // (App-Reaktion) — hier KEIN Bereichs-Schaden, das
                    // entscheidet die App/der Manager (Voll-Fallback).
                    UiReaktion::nachricht(self.nachricht)
                } else if war_gedrueckt {
                    UiReaktion::neu_zeichnen_bereich(bereich)
                } else {
                    UiReaktion::ignoriert()
                }
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

// ---------------------------------------------------------------------------
// Checkbox — Kästchen mit Haken, Klick toggelt
// ---------------------------------------------------------------------------

pub struct Checkbox {
    text: String,
    pub an: bool,
    nachricht: u32,
    hover: bool,
}

impl Checkbox {
    pub fn neu(text: &str, an: bool, nachricht: u32) -> Self {
        Checkbox { text: String::from(text), an, nachricht, hover: false }
    }
}

impl Widget for Checkbox {
    fn wunschgroesse(&self) -> (i32, i32) {
        (
            metrik().zeilen_hoehe + metrik().abstand + self.text.chars().count() as i32 * zeichen_breite(),
            metrik().ui_element_hoehe,
        )
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        let kasten = Rechteck::neu(
            bereich.x,
            bereich.y + (bereich.hoehe - metrik().zeilen_hoehe) / 2,
            metrik().zeilen_hoehe,
            metrik().zeilen_hoehe,
        );
        z.rechteck_abgerundet(kasten, 3, if self.an { thema.akzent } else { thema.eingabefeld });
        z.rechteck_rahmen(kasten, if self.hover { thema.akzent } else { thema.rahmen_passiv });
        if self.an {
            // Der Haken: zwei Linien in Titel-Textfarbe.
            let (cx, cy) = (kasten.x + kasten.breite / 2, kasten.y + kasten.hoehe / 2);
            z.linie(cx - 4, cy, cx - 1, cy + 3, thema.text_titel_aktiv);
            z.linie(cx - 1, cy + 3, cx + 4, cy - 3, thema.text_titel_aktiv);
        }
        z.text(
            kasten.x + kasten.breite + metrik().abstand,
            text_mitte_y(bereich),
            &self.text,
            metrik().schrift_ui,
            FontWeight::Regular,
            thema.text_normal,
        );
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        match ereignis {
            UiEreignis::MausRein => {
                self.hover = true;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::MausRaus => {
                self.hover = false;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                self.an = !self.an;
                UiReaktion::nachricht(self.nachricht)
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

// ---------------------------------------------------------------------------
// Textfeld — einzeilig, Innenleben ist der vorhandene ZeilenEditor
// ---------------------------------------------------------------------------

/// Das Textfeld hat keine Tab-Vervollständigung (die gehört zur Shell).
struct KeineVervollstaendigung;

impl Vervollstaendiger for KeineVervollstaendigung {
    fn eintraege(&self, _pfad: &str) -> Vec<(String, bool)> {
        Vec::new()
    }
}

pub struct Textfeld {
    /// Das anzeige-freie Innenleben aus der Shell (Tippen, Backspace,
    /// Verlauf mit Pfeiltasten) — genau dafür wurde es getrennt.
    editor: ZeilenEditor,
    /// Nachricht bei Enter (der Text steht danach im Verlauf).
    nachricht: u32,
    /// Nachricht bei JEDER Textänderung (Live-Filter wie im
    /// Startmenü-Suchfeld); None = nur neu zeichnen.
    nachricht_geaendert: Option<u32>,
    fokus: bool,
}

impl Textfeld {
    pub fn neu(nachricht: u32) -> Self {
        Textfeld { editor: ZeilenEditor::neu(10), nachricht, nachricht_geaendert: None, fokus: false }
    }

    /// Textfeld, das zusätzlich jede Textänderung meldet.
    pub fn mit_aenderungs_nachricht(nachricht_enter: u32, nachricht_geaendert: u32) -> Self {
        Textfeld { nachricht_geaendert: Some(nachricht_geaendert), ..Textfeld::neu(nachricht_enter) }
    }

    pub fn text(&self) -> &str {
        self.editor.zeile()
    }

    /// Fokus direkt setzen (z. B. Suchfeld beim Menü-Öffnen).
    pub fn fokus_setzen(&mut self, fokus: bool) {
        self.fokus = fokus;
    }
}

impl Widget for Textfeld {
    fn wunschgroesse(&self) -> (i32, i32) {
        // Breite: Box-Container strecken quer sowieso auf volle
        // Breite — der Wunsch ist nur das Minimum.
        (120, metrik().ui_element_hoehe)
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        z.rechteck_abgerundet(bereich, metrik().radius_klein, thema.eingabefeld);
        z.rechteck_rahmen(bereich, if self.fokus { thema.akzent } else { thema.rahmen_passiv });

        // Text (bei Überlänge das ENDE zeigen — dort wird getippt):
        let platz = ((bereich.breite - 2 * metrik().abstand) / zeichen_breite()).max(0) as usize;
        let text = self.editor.zeile();
        let anzahl = text.chars().count();
        let sichtbar: String = text.chars().skip(anzahl.saturating_sub(platz)).collect();
        let text_y = text_mitte_y(bereich);
        z.text(
            bereich.x + metrik().abstand,
            text_y,
            &sichtbar,
            metrik().schrift_ui,
            FontWeight::Regular,
            thema.text_stark,
        );

        // Cursor: blinkt über die zeit-API; der Uhr-Task stößt das
        // Neuzeichnen an, solange das Feld fokussiert ist. Das Tempo
        // kommt aus den Einstellungen (Anzeige -> Cursor-Blinken).
        if self.fokus
            && (crate::zeit::us_seit_boot() / crate::einstellungen::cursor_blink_us())
                .is_multiple_of(2)
        {
            let cursor_x = bereich.x + metrik().abstand + sichtbar.chars().count() as i32 * zeichen_breite();
            z.rechteck_fuellen(
                Rechteck::neu(cursor_x, text_y, 2, metrik().zeilen_hoehe),
                theme::aktuell().akzent,
            );
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        // Ein einzeiliges Textfeld ändert nur seine eigene Fläche —
        // `bereich` als Schaden statt das ganze Fenster (Tippen!).
        match ereignis {
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                // Klick fokussiert (das UiFenster hat vorher allen
                // Fokus entfernt).
                self.fokus = true;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::FokusRein => {
                self.fokus = true;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::FokusRaus => {
                self.fokus = false;
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Taste(taste) if self.fokus => {
                let editor_taste = match taste {
                    DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => Taste::Enter,
                    DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => Taste::Backspace,
                    DecodedKey::RawKey(KeyCode::ArrowUp) => Taste::HochPfeil,
                    DecodedKey::RawKey(KeyCode::ArrowDown) => Taste::RunterPfeil,
                    DecodedKey::Unicode(c) if *c >= ' ' => Taste::Zeichen(*c),
                    _ => return UiReaktion::verbraucht(), // fokussiert: schlucken
                };
                let text_vorher = alloc::string::String::from(self.editor.zeile());
                match self.editor.taste(editor_taste, "/", &KeineVervollstaendigung) {
                    // Enter/Änderungs-Nachricht können anderswo (Filter,
                    // App) größere Umbauten auslösen — die Schadensfläche
                    // trägt das eigene Feld bei; der Empfänger ergänzt.
                    Reaktion::Fertig(_) => {
                        UiReaktion::nachricht(self.nachricht).mit_schaden(bereich)
                    }
                    _ => match self.nachricht_geaendert {
                        Some(id) if self.editor.zeile() != text_vorher => {
                            UiReaktion::nachricht(id).mit_schaden(bereich)
                        }
                        _ => UiReaktion::neu_zeichnen_bereich(bereich),
                    },
                }
            }
            _ => UiReaktion::ignoriert(),
        }
    }

    fn hat_fokus(&self) -> bool {
        self.fokus
    }

    fn fokus_weiter(&mut self) -> bool {
        // Blatt-Regel der Tab-Kette: nehmen, wenn frei; abgeben,
        // wenn gehalten (dann ist das nächste Widget dran).
        if self.fokus {
            self.ereignis(&UiEreignis::FokusRaus, Rechteck::neu(0, 0, 0, 0));
            false
        } else {
            self.ereignis(&UiEreignis::FokusRein, Rechteck::neu(0, 0, 0, 0));
            true
        }
    }

    fn fokus_entfernen(&mut self) {
        self.fokus = false;
    }
}

// ---------------------------------------------------------------------------
// ScrollListe — vertikal scrollbar, Auswahl, Doppelklick
// ---------------------------------------------------------------------------

/// Klemmt eine Scroll-Position auf den gültigen Bereich (reine
/// Funktion): nie unter 0, nie weiter als "Inhalt minus Sichtfenster".
pub fn scroll_klemmen(scroll: i32, inhalt_hoehe: i32, sicht_hoehe: i32) -> i32 {
    scroll.clamp(0, (inhalt_hoehe - sicht_hoehe).max(0))
}

/// Welche Einträge sind sichtbar? (reine Funktion): Indizes
/// [erster, letzter) für Scroll-Position und Sichthöhe.
pub fn sichtbare_eintraege(
    scroll: i32,
    sicht_hoehe: i32,
    eintrag_hoehe: i32,
    anzahl: usize,
) -> (usize, usize) {
    let erster = (scroll / eintrag_hoehe).max(0) as usize;
    let letzter = ((scroll + sicht_hoehe + eintrag_hoehe - 1) / eintrag_hoehe).max(0) as usize;
    (erster.min(anzahl), letzter.min(anzahl))
}

pub struct ListenEintrag {
    pub icon: Option<&'static Icon>,
    pub text: String,
}

pub struct ScrollListe {
    pub eintraege: Vec<ListenEintrag>,
    pub auswahl: Option<usize>,
    /// Cell statt i32: zeichnen(&self) darf die Auswahl in den
    /// Sichtbereich scrollen (auswahl_sichtbar), ohne &mut zu brauchen.
    scroll: core::cell::Cell<i32>,
    /// Nachricht bei Auswahl-Klick bzw. Doppelklick/Enter.
    nachricht_auswahl: u32,
    nachricht_doppelklick: u32,
    /// Nachrichten als BASIS + Eintrag-Index kodieren? (Explorer & Co.
    /// erfahren so, WELCHER Eintrag gemeint ist.)
    index_kodierung: bool,
    /// Kann die Liste den Tastatur-Fokus halten? (Pfeile/Enter)
    fokus: bool,
    fokussierbar: bool,
    /// Beim Zeichnen die Auswahl in den Sichtbereich holen (für
    /// Apps, die die Liste nach jeder Nachricht neu aufbauen).
    pub auswahl_sichtbar: bool,
    /// Rechtsklick-Nachrichten: (Basis + Index) auf Einträgen,
    /// feste Nachricht auf der freien Fläche (Kontextmenüs).
    rechtsklick_basis: Option<u32>,
    rechtsklick_leer: Option<u32>,
    /// Flex-Faktor im Layout (Standard 1: nimmt den Restplatz).
    flex: i32,
    /// Fester Breiten-Wunsch (z. B. schmale Ordnerbaum-Spalte).
    wunsch_breite: i32,
    /// Wird der Scrollbalken gerade gezogen? (Anker: y-Versatz im Griff)
    balken_griff: Option<i32>,
}

impl ScrollListe {
    pub fn neu(eintraege: Vec<ListenEintrag>, nachricht_auswahl: u32, nachricht_doppelklick: u32) -> Self {
        ScrollListe {
            eintraege,
            auswahl: None,
            scroll: core::cell::Cell::new(0),
            nachricht_auswahl,
            nachricht_doppelklick,
            index_kodierung: false,
            fokus: false,
            fokussierbar: false,
            auswahl_sichtbar: false,
            rechtsklick_basis: None,
            rechtsklick_leer: None,
            flex: 1,
            wunsch_breite: 160,
            balken_griff: None,
        }
    }

    /// Liste mit Index-Kodierung: Nachrichten sind BASIS + Index —
    /// die Basen müssen weiter auseinander liegen als die Listenlänge!
    pub fn mit_index_nachrichten(
        eintraege: Vec<ListenEintrag>,
        auswahl_basis: u32,
        doppelklick_basis: u32,
    ) -> Self {
        ScrollListe {
            index_kodierung: true,
            fokussierbar: true,
            ..ScrollListe::neu(eintraege, auswahl_basis, doppelklick_basis)
        }
    }

    /// Builder: Auswahl vorbelegen (Zustand der App).
    pub fn mit_auswahl(mut self, auswahl: Option<usize>) -> Self {
        self.auswahl = auswahl.filter(|&i| i < self.eintraege.len());
        self.auswahl_sichtbar = true;
        self
    }

    /// Builder: Layout-Verhalten (flex 0 = feste Breite).
    pub fn mit_layout(mut self, wunsch_breite: i32, flex: i32) -> Self {
        self.wunsch_breite = wunsch_breite;
        self.flex = flex;
        self
    }

    /// Builder: Rechtsklick-Nachrichten (Eintrag: Basis + Index,
    /// freie Fläche: feste ID) — für Kontextmenüs.
    pub fn mit_rechtsklick(mut self, eintrag_basis: u32, leer: u32) -> Self {
        self.rechtsklick_basis = Some(eintrag_basis);
        self.rechtsklick_leer = Some(leer);
        self
    }

    /// Builder: Fokus direkt setzen (z. B. Dateiliste beim Öffnen).
    pub fn mit_fokus(mut self, fokus: bool) -> Self {
        self.fokus = fokus && self.fokussierbar;
        self
    }

    /// Die (ggf. index-kodierte) Nachricht für einen Eintrag.
    fn auswahl_nachricht(&self, index: usize) -> u32 {
        if self.index_kodierung {
            self.nachricht_auswahl + index as u32
        } else {
            self.nachricht_auswahl
        }
    }
    fn doppelklick_nachricht(&self, index: usize) -> u32 {
        if self.index_kodierung {
            self.nachricht_doppelklick + index as u32
        } else {
            self.nachricht_doppelklick
        }
    }

    fn inhalt_hoehe(&self) -> i32 {
        self.eintraege.len() as i32 * metrik().listen_eintrag_hoehe
    }

    /// Das Rechteck des Scrollbalken-GRIFFS (None = alles sichtbar).
    fn balken_rechteck(&self, bereich: Rechteck) -> Option<Rechteck> {
        let inhalt = self.inhalt_hoehe();
        if inhalt <= bereich.hoehe {
            return None;
        }
        let hoehe = (bereich.hoehe * bereich.hoehe / inhalt).max(24);
        let weg = bereich.hoehe - hoehe;
        let y = bereich.y + weg * self.scroll.get() / (inhalt - bereich.hoehe);
        Some(Rechteck::neu(
            bereich.x + bereich.breite - metrik().scrollbalken_breite,
            y,
            metrik().scrollbalken_breite,
            hoehe,
        ))
    }

    /// Scroll-Position aus einer Griff-Position rückrechnen (Drag).
    fn scroll_aus_griff(&self, bereich: Rechteck, griff_y: i32) -> i32 {
        let inhalt = self.inhalt_hoehe();
        let griff_hoehe = (bereich.hoehe * bereich.hoehe / inhalt).max(24);
        let weg = (bereich.hoehe - griff_hoehe).max(1);
        scroll_klemmen(
            (griff_y - bereich.y) * (inhalt - bereich.hoehe) / weg,
            inhalt,
            bereich.hoehe,
        )
    }

    /// Bewegt die Auswahl um `delta` Einträge (mit Wrap-Around) und
    /// scrollt sie in den Sichtbereich — für Pfeiltasten-Navigation
    /// (Startmenü) und Alt+Tab.
    pub fn auswahl_bewegen(&mut self, delta: i32, sicht_hoehe: i32) {
        if self.eintraege.is_empty() {
            self.auswahl = None;
            return;
        }
        let anzahl = self.eintraege.len() as i32;
        let neu = (self.auswahl.unwrap_or(0) as i32 + delta).rem_euclid(anzahl);
        self.auswahl = Some(neu as usize);
        // In den Sichtbereich holen:
        let oben = neu * metrik().listen_eintrag_hoehe;
        let unten = oben + metrik().listen_eintrag_hoehe;
        if oben < self.scroll.get() {
            self.scroll.set(oben);
        } else if unten > self.scroll.get() + sicht_hoehe {
            self.scroll.set(unten - sicht_hoehe);
        }
    }

    /// Setzt neue Einträge (Live-Filter) und beginnt oben.
    pub fn eintraege_setzen(&mut self, eintraege: Vec<ListenEintrag>) {
        self.eintraege = eintraege;
        self.scroll.set(0);
        self.auswahl = if self.eintraege.is_empty() { None } else { Some(0) };
    }

    fn eintrag_bei(&self, bereich: Rechteck, x: i32, y: i32) -> Option<usize> {
        if !bereich.enthaelt(x, y) || x >= bereich.x + bereich.breite - metrik().scrollbalken_breite {
            return None;
        }
        let index = ((y - bereich.y + self.scroll.get()) / metrik().listen_eintrag_hoehe) as usize;
        (index < self.eintraege.len()).then_some(index)
    }
}

impl Widget for ScrollListe {
    fn wunschgroesse(&self) -> (i32, i32) {
        (self.wunsch_breite, 3 * metrik().listen_eintrag_hoehe)
    }

    fn flex(&self) -> i32 {
        self.flex
    }

    fn hat_fokus(&self) -> bool {
        self.fokus
    }

    fn fokus_weiter(&mut self) -> bool {
        if !self.fokussierbar {
            return false;
        }
        // Blatt-Regel: nehmen wenn frei, abgeben wenn gehalten.
        self.fokus = !self.fokus;
        self.fokus
    }

    fn fokus_entfernen(&mut self) {
        self.fokus = false;
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        // Auswahl in den Sichtbereich holen (Cell — zeichnen ist
        // &self): für Apps, die die Liste nach jeder Nachricht neu
        // aufbauen und die Auswahl als Zustand mitgeben.
        if self.auswahl_sichtbar {
            if let Some(index) = self.auswahl {
                let oben = index as i32 * metrik().listen_eintrag_hoehe;
                let unten = oben + metrik().listen_eintrag_hoehe;
                if oben < self.scroll.get() {
                    self.scroll.set(oben);
                } else if unten > self.scroll.get() + bereich.hoehe {
                    self.scroll.set(unten - bereich.hoehe);
                }
            }
        }
        z.rechteck_fuellen(bereich, thema.eingabefeld);
        z.rechteck_rahmen(bereich, if self.fokus { thema.akzent } else { thema.rahmen_passiv });

        // Einträge — GECLIPPT auf den Listenbereich (Teilzeilen am Rand):
        z.clip_setzen(Some(Rechteck::neu(bereich.x + 1, bereich.y + 1, bereich.breite - 2, bereich.hoehe - 2)));
        let (erster, letzter) = sichtbare_eintraege(
            self.scroll.get(),
            bereich.hoehe,
            metrik().listen_eintrag_hoehe,
            self.eintraege.len(),
        );
        for index in erster..letzter {
            let eintrag = &self.eintraege[index];
            let y = bereich.y + index as i32 * metrik().listen_eintrag_hoehe - self.scroll.get();
            let zeile = Rechteck::neu(
                bereich.x + 2,
                y,
                bereich.breite - metrik().scrollbalken_breite - 4,
                metrik().listen_eintrag_hoehe,
            );
            if self.auswahl == Some(index) {
                z.rechteck_abgerundet(zeile, metrik().radius_klein, thema.auswahl);
            }
            let mut text_x = zeile.x + 6;
            if let Some(icon) = eintrag.icon {
                z.icon(text_x, y + (metrik().listen_eintrag_hoehe - 16) / 2, icon, 1);
                text_x += 22;
            }
            z.text(
                text_x,
                y + (metrik().listen_eintrag_hoehe - metrik().zeilen_hoehe) / 2,
                &eintrag.text,
                metrik().schrift_ui,
                FontWeight::Regular,
                if self.auswahl == Some(index) { thema.text_stark } else { thema.text_normal },
            );
        }
        z.clip_setzen(None);

        // Scrollbalken (nur wenn nötig):
        if let Some(griff) = self.balken_rechteck(bereich) {
            z.rechteck_fuellen(
                Rechteck::neu(griff.x, bereich.y, metrik().scrollbalken_breite, bereich.hoehe),
                thema.leiste_knopf,
            );
            z.rechteck_abgerundet(
                griff,
                metrik().radius_klein,
                if self.balken_griff.is_some() { thema.akzent } else { thema.text_gedimmt },
            );
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        match ereignis {
            UiEreignis::Scroll { delta, x, y } if bereich.enthaelt(*x, *y) => {
                // Rad hoch (delta > 0) = Inhalt nach oben scrollen.
                self.scroll.set(scroll_klemmen(
                    self.scroll.get() - *delta as i32 * 3 * metrik().listen_eintrag_hoehe / 2,
                    self.inhalt_hoehe(),
                    bereich.hoehe,
                ));
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Klick { x, y } => {
                if let Some(griff) = self.balken_rechteck(bereich) {
                    if griff.enthaelt(*x, *y) {
                        self.balken_griff = Some(*y - griff.y);
                        return UiReaktion::neu_zeichnen_bereich(bereich);
                    }
                    // Klick auf die Balken-Spur: Seite springen.
                    if *x >= griff.x && bereich.enthaelt(*x, *y) {
                        let richtung = if *y < griff.y { -1 } else { 1 };
                        self.scroll.set(scroll_klemmen(
                            self.scroll.get() + richtung * bereich.hoehe,
                            self.inhalt_hoehe(),
                            bereich.hoehe,
                        ));
                        return UiReaktion::neu_zeichnen_bereich(bereich);
                    }
                }
                if let Some(index) = self.eintrag_bei(bereich, *x, *y) {
                    self.auswahl = Some(index);
                    if self.fokussierbar {
                        self.fokus = true; // Klick fokussiert die Liste
                    }
                    return UiReaktion::nachricht(self.auswahl_nachricht(index));
                }
                UiReaktion::ignoriert()
            }
            UiEreignis::Doppelklick { x, y } => {
                if let Some(index) = self.eintrag_bei(bereich, *x, *y) {
                    UiReaktion::nachricht(self.doppelklick_nachricht(index))
                } else {
                    UiReaktion::ignoriert()
                }
            }
            // Rechtsklick: Eintrag auswählen + Kontext-Nachricht;
            // freie Fläche innerhalb der Liste: Leer-Nachricht.
            UiEreignis::Rechtsklick { x, y } => {
                if let Some(index) = self.eintrag_bei(bereich, *x, *y) {
                    if let Some(basis) = self.rechtsklick_basis {
                        self.auswahl = Some(index);
                        return UiReaktion::nachricht(basis + index as u32);
                    }
                } else if bereich.enthaelt(*x, *y) {
                    if let Some(leer) = self.rechtsklick_leer {
                        return UiReaktion::nachricht(leer);
                    }
                }
                UiReaktion::ignoriert()
            }
            // Tastatur (nur mit Fokus): Pfeile bewegen die Auswahl,
            // Enter wirkt wie ein Doppelklick auf den Eintrag.
            UiEreignis::Taste(taste) if self.fokus => match taste {
                DecodedKey::RawKey(KeyCode::ArrowUp) => {
                    self.auswahl_bewegen(-1, bereich.hoehe);
                    match self.auswahl {
                        Some(index) => UiReaktion::nachricht(self.auswahl_nachricht(index)),
                        None => UiReaktion::neu_zeichnen_bereich(bereich),
                    }
                }
                DecodedKey::RawKey(KeyCode::ArrowDown) => {
                    self.auswahl_bewegen(1, bereich.hoehe);
                    match self.auswahl {
                        Some(index) => UiReaktion::nachricht(self.auswahl_nachricht(index)),
                        None => UiReaktion::neu_zeichnen_bereich(bereich),
                    }
                }
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => match self.auswahl {
                    Some(index) => UiReaktion::nachricht(self.doppelklick_nachricht(index)),
                    None => UiReaktion::verbraucht(),
                },
                _ => UiReaktion::ignoriert(),
            },
            UiEreignis::Bewegt { x: _, y } => {
                if let Some(griff_versatz) = self.balken_griff {
                    self.scroll.set(self.scroll_aus_griff(bereich, *y - griff_versatz));
                    return UiReaktion::neu_zeichnen_bereich(bereich);
                }
                UiReaktion::ignoriert()
            }
            UiEreignis::Losgelassen { .. } | UiEreignis::MausRaus => {
                if self.balken_griff.take().is_some() {
                    return UiReaktion::neu_zeichnen_bereich(bereich);
                }
                UiReaktion::ignoriert()
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Scroll-Klemmen: nie negativ, nie hinter das Inhaltsende.
    #[test_case]
    fn test_scroll_klemmen() {
        assert_eq!(scroll_klemmen(-10, 500, 100), 0);
        assert_eq!(scroll_klemmen(250, 500, 100), 250);
        assert_eq!(scroll_klemmen(999, 500, 100), 400); // max = 500-100
        assert_eq!(scroll_klemmen(50, 80, 100), 0); // alles sichtbar
    }

    /// Sichtbereich der ScrollListe: richtige Index-Spanne, auch mit
    /// angeschnittenen Einträgen oben und unten.
    #[test_case]
    fn test_sichtbare_eintraege() {
        // 20 Einträge à 26 px, Sichtfenster 100 px:
        // scroll 0: Einträge 0..4 (Eintrag 3 angeschnitten).
        assert_eq!(sichtbare_eintraege(0, 100, 26, 20), (0, 4));
        // scroll 30: erster = 1 (Eintrag 1 ab Pixel 26), bis Pixel 130 -> 0..5.
        assert_eq!(sichtbare_eintraege(30, 100, 26, 20), (1, 5));
        // Ganz unten (scroll = 20*26-100 = 420): 16..20.
        assert_eq!(sichtbare_eintraege(420, 100, 26, 20), (16, 20));
        // Wenige Einträge: Spanne wird auf die Anzahl gekappt.
        assert_eq!(sichtbare_eintraege(0, 100, 26, 2), (0, 2));
    }

    /// Auswahl per Klick + Scroll verschieben den sichtbaren Eintrag.
    #[test_case]
    fn test_liste_auswahl_und_scroll() {
        let eintraege = (0..20)
            .map(|i| ListenEintrag { icon: None, text: alloc::format!("Eintrag {}", i) })
            .collect();
        let mut liste = ScrollListe::neu(eintraege, 100, 101);
        let bereich = Rechteck::neu(0, 0, 200, 100);

        // Klick auf den zweiten Eintrag (y=30, Eintrag-Höhe 26):
        let klick = liste.ereignis(&UiEreignis::Klick { x: 10, y: 30 }, bereich);
        assert_eq!(liste.auswahl, Some(1));
        assert_eq!(klick.nachricht, Some(100));

        // Rad nach unten (delta -1) scrollt; derselbe Klickpunkt
        // trifft jetzt einen späteren Eintrag:
        liste.ereignis(&UiEreignis::Scroll { delta: -1, x: 10, y: 30 }, bereich);
        assert!(liste.scroll.get() > 0);
        liste.ereignis(&UiEreignis::Klick { x: 10, y: 30 }, bereich);
        assert!(liste.auswahl > Some(1));

        // Doppelklick meldet die Doppelklick-Nachricht:
        let doppel = liste.ereignis(&UiEreignis::Doppelklick { x: 10, y: 30 }, bereich);
        assert_eq!(doppel.nachricht, Some(101));
    }

    /// Button: Klick + Loslassen im Bereich = Nachricht; wegziehen
    /// bricht ab; Hover kommt über MausRein/MausRaus.
    #[test_case]
    fn test_button_zustaende() {
        let mut button = Button::neu("Test", 7);
        let bereich = Rechteck::neu(0, 0, 100, 30);

        button.ereignis(&UiEreignis::MausRein, bereich);
        assert!(button.hover);
        button.ereignis(&UiEreignis::Klick { x: 10, y: 10 }, bereich);
        assert!(button.gedrueckt);
        // Wegziehen und woanders loslassen: KEINE Nachricht.
        let daneben = button.ereignis(&UiEreignis::Losgelassen { x: 300, y: 10 }, bereich);
        assert_eq!(daneben.nachricht, None);
        // Nochmal, diesmal richtig:
        button.ereignis(&UiEreignis::Klick { x: 10, y: 10 }, bereich);
        let klick = button.ereignis(&UiEreignis::Losgelassen { x: 12, y: 12 }, bereich);
        assert_eq!(klick.nachricht, Some(7));
        button.ereignis(&UiEreignis::MausRaus, bereich);
        assert!(!button.hover);
    }

    /// Textfeld: Tippen über den ZeilenEditor, Enter meldet die
    /// Nachricht, Checkbox toggelt.
    #[test_case]
    fn test_textfeld_und_checkbox() {
        let mut feld = Textfeld::neu(42);
        let bereich = Rechteck::neu(0, 0, 200, 30);
        feld.ereignis(&UiEreignis::Klick { x: 5, y: 5 }, bereich); // fokussiert
        assert!(feld.hat_fokus());
        for zeichen in "abc".chars() {
            feld.ereignis(&UiEreignis::Taste(DecodedKey::Unicode(zeichen)), bereich);
        }
        assert_eq!(feld.text(), "abc");
        feld.ereignis(&UiEreignis::Taste(DecodedKey::Unicode('\u{8}')), bereich);
        assert_eq!(feld.text(), "ab");
        let enter = feld.ereignis(&UiEreignis::Taste(DecodedKey::Unicode('\n')), bereich);
        assert_eq!(enter.nachricht, Some(42));

        let mut kasten = Checkbox::neu("An?", false, 9);
        let reaktion = kasten.ereignis(&UiEreignis::Klick { x: 5, y: 5 }, Rechteck::neu(0, 0, 100, 30));
        assert!(kasten.an);
        assert_eq!(reaktion.nachricht, Some(9));
    }
}
