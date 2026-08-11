// speedui::widgets — Die Grund-Widgets im Aurora-Stil
//
// Label, Trennlinie, Button, Checkbox, Textfeld und ScrollListe. Alle
// Farben ueber `Farbrolle`, alle Masse ueber `Mass` — die Projektregel
// „keine hartcodierten Werte im UI-Code" gilt hier zuallererst, und seit
// dem Umzug kommen beide aus dem `UiKontext` statt aus Kernel-Globals.
//
// Zustandslogik, die sich rechnen laesst (Scroll-Klemmen, sichtbarer
// Ausschnitt), liegt in reinen Funktionen und ist unit-getestet.

use crate::editor::{EditorTaste, Reaktion, Vervollstaendiger, ZeilenEditor};
use crate::typen::{Icon, Rechteck, Taste};
use crate::umgebung::{Farbrolle, Maler, Mass, UiKontext};
use crate::{UiEreignis, UiReaktion, Widget};
use alloc::string::String;
use alloc::vec::Vec;

/// Breite eines Zeichens der UI-Schrift — jetzt eine Frage an die
/// Schrift des Wirts statt an eine Kernel-Kiste.
fn zeichen_breite(k: &UiKontext) -> i32 {
    k.zeichen_breite()
}

/// Text vertikal in einem Bereich zentrieren (y der Textoberkante).
fn text_mitte_y(bereich: Rechteck, k: &UiKontext) -> i32 {
    bereich.y + (bereich.hoehe - k.mass(Mass::ZeilenHoehe)) / 2
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
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        let zeilen = self.text.lines().count().max(1) as i32;
        let laengste = self.text.lines().map(|z| z.chars().count()).max().unwrap_or(0) as i32;
        (laengste * zeichen_breite(k), zeilen * k.mass(Mass::ZeilenHoehe))
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        let farbe = if self.sekundaer { k.farbe(Farbrolle::TextSekundaer) } else { k.farbe(Farbrolle::TextNormal) };
        for (i, zeile) in self.text.lines().enumerate() {
            m.text(
                bereich.x,
                bereich.y + i as i32 * k.mass(Mass::ZeilenHoehe),
                zeile,
                farbe,
            );
        }
    }

    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Trennlinie
// ---------------------------------------------------------------------------

pub struct Trennlinie;

impl Widget for Trennlinie {
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        (0, k.mass(Mass::Abstand))
    }
    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        let y = bereich.y + bereich.hoehe / 2;
        m.linie(
            bereich.x,
            y,
            bereich.x + bereich.breite - 1,
            y,
            k.farbe(Farbrolle::Rahmen),
        );
    }
    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
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
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        let icon_platz = if self.icon.is_some() { 22 } else { 0 };
        (
            self.text.chars().count() as i32 * zeichen_breite(k) + 2 * k.mass(Mass::Abstand) + icon_platz + 8,
            k.mass(Mass::ElementHoehe),
        )
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        // Zustand -> Füllung: gedrückt/aktiv (Auswahl) > hover > normal;
        // deaktiviert bleibt flach und ohne Akzent.
        let fuellung = if self.deaktiviert {
            k.farbe(Farbrolle::Eingabefeld)
        } else if self.gedrueckt || self.aktiv {
            k.farbe(Farbrolle::Auswahl)
        } else if self.hover {
            k.farbe(Farbrolle::KnopfAktiv)
        } else {
            k.farbe(Farbrolle::Eingabefeld)
        };
        m.abgerundet(bereich, k.mass(Mass::RadiusKlein), fuellung);
        m.rahmen(
            bereich,
            if !self.deaktiviert && (self.hover || self.aktiv) {
                k.farbe(Farbrolle::Akzent)
            } else {
                k.farbe(Farbrolle::Rahmen)
            },
        );

        let mut text_x = bereich.x
            + (bereich.breite
                - self.text.chars().count() as i32 * zeichen_breite(k)
                - if self.icon.is_some() { 22 } else { 0 })
                / 2;
        if let Some(icon) = self.icon {
            m.icon(text_x, text_mitte_y(bereich, k), icon, 1);
            text_x += 22;
        }
        m.text(
            text_x,
            text_mitte_y(bereich, k),
            &self.text,
            if self.deaktiviert { k.farbe(Farbrolle::TextGedimmt) } else { k.farbe(Farbrolle::TextStark) },
        );
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
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
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        (
            k.mass(Mass::ZeilenHoehe) + k.mass(Mass::Abstand) + self.text.chars().count() as i32 * zeichen_breite(k),
            k.mass(Mass::ElementHoehe),
        )
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        let kasten = Rechteck::neu(
            bereich.x,
            bereich.y + (bereich.hoehe - k.mass(Mass::ZeilenHoehe)) / 2,
            k.mass(Mass::ZeilenHoehe),
            k.mass(Mass::ZeilenHoehe),
        );
        m.abgerundet(kasten, 3, if self.an { k.farbe(Farbrolle::Akzent) } else { k.farbe(Farbrolle::Eingabefeld) });
        m.rahmen(kasten, if self.hover { k.farbe(Farbrolle::Akzent) } else { k.farbe(Farbrolle::Rahmen) });
        if self.an {
            // Der Haken: zwei Linien in Titel-Textfarbe.
            let (cx, cy) = (kasten.x + kasten.breite / 2, kasten.y + kasten.hoehe / 2);
            m.linie(cx - 4, cy, cx - 1, cy + 3, k.farbe(Farbrolle::TextAufAkzent));
            m.linie(cx - 1, cy + 3, cx + 4, cy - 3, k.farbe(Farbrolle::TextAufAkzent));
        }
        m.text(
            kasten.x + kasten.breite + k.mass(Mass::Abstand),
            text_mitte_y(bereich, k),
            &self.text,
            k.farbe(Farbrolle::TextNormal),
        );
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
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

    /// Den Inhalt von aussen setzen — die Adressleiste eines Browsers
    /// muss zeigen, was WIRKLICH geladen ist (Serie 8, Teil 8).
    pub fn text_setzen(&mut self, text: &str) {
        self.editor.zeile_setzen(text);
    }
}

impl Widget for Textfeld {
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        // Breite: Box-Container strecken quer sowieso auf volle
        // Breite — der Wunsch ist nur das Minimum.
        (120, k.mass(Mass::ElementHoehe))
    }

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        m.abgerundet(bereich, k.mass(Mass::RadiusKlein), k.farbe(Farbrolle::Eingabefeld));
        m.rahmen(bereich, if self.fokus { k.farbe(Farbrolle::Akzent) } else { k.farbe(Farbrolle::Rahmen) });

        // Text (bei Überlänge das ENDE zeigen — dort wird getippt):
        let platz = ((bereich.breite - 2 * k.mass(Mass::Abstand)) / zeichen_breite(k)).max(0) as usize;
        let text = self.editor.zeile();
        let anzahl = text.chars().count();
        let sichtbar: String = text.chars().skip(anzahl.saturating_sub(platz)).collect();
        let text_y = text_mitte_y(bereich, k);
        m.text(
            bereich.x + k.mass(Mass::Abstand),
            text_y,
            &sichtbar,
            k.farbe(Farbrolle::TextStark),
        );

        // Cursor: blinkt über die zeit-API; der Uhr-Task stößt das
        // Neuzeichnen an, solange das Feld fokussiert ist. Das Tempo
        // kommt aus den Einstellungen (Anzeige -> Cursor-Blinken).
        // Cursor: blinkt ueber die UHR DES WIRTS, das Tempo kommt als
        // MASS aus dem Thema (im Kernel die Einstellung „Cursor-Blinken",
        // in einem Prozess irgendetwas Vernuenftiges).
        if self.fokus
            && (k.uhr.us() / (k.mass(Mass::CursorBlinkUs).max(1) as u64)).is_multiple_of(2)
        {
            let cursor_x = bereich.x + k.mass(Mass::Abstand) + sichtbar.chars().count() as i32 * zeichen_breite(k);
            m.fuellen(
                Rechteck::neu(cursor_x, text_y, 2, k.mass(Mass::ZeilenHoehe)),
                k.farbe(Farbrolle::Akzent),
            );
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
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
                // Die Toolkit-Taste in die Editor-Taste uebersetzen.
                // Zwei Enums fuer Tasten sehen nach Doppelarbeit aus, sind
                // aber verschiedene EBENEN: `Taste` ist, was eine Tastatur
                // liefert; `EditorTaste` ist, was eine Eingabezeile kennt
                // (sie hat kein F5 und kein Bild-auf).
                let editor_taste = match taste {
                    Taste::Zeichen('\n') | Taste::Zeichen('\r') => EditorTaste::Enter,
                    Taste::Zeichen('\u{8}') | Taste::Zeichen('\u{7f}') => EditorTaste::Backspace,
                    Taste::Hoch => EditorTaste::HochPfeil,
                    Taste::Runter => EditorTaste::RunterPfeil,
                    Taste::Zeichen(c) if *c >= ' ' => EditorTaste::Zeichen(*c),
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
            self.fokus = false;
            false
        } else {
            self.fokus = true;
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

    fn inhalt_hoehe(&self, k: &UiKontext) -> i32 {
        self.eintraege.len() as i32 * k.mass(Mass::ListenEintragHoehe)
    }

    /// Das Rechteck des Scrollbalken-GRIFFS (None = alles sichtbar).
    fn balken_rechteck(&self, bereich: Rechteck, k: &UiKontext) -> Option<Rechteck> {
        let inhalt = self.inhalt_hoehe(k);
        if inhalt <= bereich.hoehe {
            return None;
        }
        let hoehe = (bereich.hoehe * bereich.hoehe / inhalt).max(24);
        let weg = bereich.hoehe - hoehe;
        let y = bereich.y + weg * self.scroll.get() / (inhalt - bereich.hoehe);
        Some(Rechteck::neu(
            bereich.x + bereich.breite - k.mass(Mass::ScrollbalkenBreite),
            y,
            k.mass(Mass::ScrollbalkenBreite),
            hoehe,
        ))
    }

    /// Scroll-Position aus einer Griff-Position rückrechnen (Drag).
    fn scroll_aus_griff(&self, bereich: Rechteck, griff_y: i32, k: &UiKontext) -> i32 {
        let inhalt = self.inhalt_hoehe(k);
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
    pub fn auswahl_bewegen(&mut self, delta: i32, sicht_hoehe: i32, k: &UiKontext) {
        if self.eintraege.is_empty() {
            self.auswahl = None;
            return;
        }
        let anzahl = self.eintraege.len() as i32;
        let neu = (self.auswahl.unwrap_or(0) as i32 + delta).rem_euclid(anzahl);
        self.auswahl = Some(neu as usize);
        // In den Sichtbereich holen:
        let oben = neu * k.mass(Mass::ListenEintragHoehe);
        let unten = oben + k.mass(Mass::ListenEintragHoehe);
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

    fn eintrag_bei(&self, bereich: Rechteck, x: i32, y: i32, k: &UiKontext) -> Option<usize> {
        if !bereich.enthaelt(x, y) || x >= bereich.x + bereich.breite - k.mass(Mass::ScrollbalkenBreite) {
            return None;
        }
        let index = ((y - bereich.y + self.scroll.get()) / k.mass(Mass::ListenEintragHoehe)) as usize;
        (index < self.eintraege.len()).then_some(index)
    }
}

impl Widget for ScrollListe {
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32) {
        (self.wunsch_breite, 3 * k.mass(Mass::ListenEintragHoehe))
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

    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck) {
        // KOPIE, kein Borrow: `UiKontext` ist `Copy`, und ein
        // `&m.kontext` wuerde den Maler festhalten — danach waere kein
        // einziger Zeichen-Aufruf mehr moeglich (er braucht `&mut m`).
        let kontext = m.kontext;
        let k = &kontext;
        // Auswahl in den Sichtbereich holen (Cell — zeichnen ist
        // &self): für Apps, die die Liste nach jeder Nachricht neu
        // aufbauen und die Auswahl als Zustand mitgeben.
        if self.auswahl_sichtbar {
            if let Some(index) = self.auswahl {
                let oben = index as i32 * k.mass(Mass::ListenEintragHoehe);
                let unten = oben + k.mass(Mass::ListenEintragHoehe);
                if oben < self.scroll.get() {
                    self.scroll.set(oben);
                } else if unten > self.scroll.get() + bereich.hoehe {
                    self.scroll.set(unten - bereich.hoehe);
                }
            }
        }
        m.fuellen(bereich, k.farbe(Farbrolle::Eingabefeld));
        m.rahmen(bereich, if self.fokus { k.farbe(Farbrolle::Akzent) } else { k.farbe(Farbrolle::Rahmen) });

        // Einträge — GECLIPPT auf den Listenbereich (Teilzeilen am Rand):
        m.clip_setzen(Some(Rechteck::neu(bereich.x + 1, bereich.y + 1, bereich.breite - 2, bereich.hoehe - 2)));
        let (erster, letzter) = sichtbare_eintraege(
            self.scroll.get(),
            bereich.hoehe,
            k.mass(Mass::ListenEintragHoehe),
            self.eintraege.len(),
        );
        for index in erster..letzter {
            let eintrag = &self.eintraege[index];
            let y = bereich.y + index as i32 * k.mass(Mass::ListenEintragHoehe) - self.scroll.get();
            let zeile = Rechteck::neu(
                bereich.x + 2,
                y,
                bereich.breite - k.mass(Mass::ScrollbalkenBreite) - 4,
                k.mass(Mass::ListenEintragHoehe),
            );
            if self.auswahl == Some(index) {
                m.abgerundet(zeile, k.mass(Mass::RadiusKlein), k.farbe(Farbrolle::Auswahl));
            }
            let mut text_x = zeile.x + 6;
            if let Some(icon) = eintrag.icon {
                m.icon(text_x, y + (k.mass(Mass::ListenEintragHoehe) - 16) / 2, icon, 1);
                text_x += 22;
            }
            m.text(
                text_x,
                y + (k.mass(Mass::ListenEintragHoehe) - k.mass(Mass::ZeilenHoehe)) / 2,
                &eintrag.text,
                if self.auswahl == Some(index) { k.farbe(Farbrolle::TextStark) } else { k.farbe(Farbrolle::TextNormal) },
            );
        }
        m.clip_setzen(None);

        // Scrollbalken (nur wenn nötig):
        if let Some(griff) = self.balken_rechteck(bereich, k) {
            m.fuellen(
                Rechteck::neu(griff.x, bereich.y, k.mass(Mass::ScrollbalkenBreite), bereich.hoehe),
                k.farbe(Farbrolle::KnopfFlaeche),
            );
            m.abgerundet(
                griff,
                k.mass(Mass::RadiusKlein),
                if self.balken_griff.is_some() { k.farbe(Farbrolle::Akzent) } else { k.farbe(Farbrolle::TextGedimmt) },
            );
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, k: &UiKontext) -> UiReaktion {
        match ereignis {
            UiEreignis::Scroll { delta, x, y } if bereich.enthaelt(*x, *y) => {
                // Rad hoch (delta > 0) = Inhalt nach oben scrollen.
                self.scroll.set(scroll_klemmen(
                    self.scroll.get() - *delta as i32 * 3 * k.mass(Mass::ListenEintragHoehe) / 2,
                    self.inhalt_hoehe(k),
                    bereich.hoehe,
                ));
                UiReaktion::neu_zeichnen_bereich(bereich)
            }
            UiEreignis::Klick { x, y } => {
                if let Some(griff) = self.balken_rechteck(bereich, k) {
                    if griff.enthaelt(*x, *y) {
                        self.balken_griff = Some(*y - griff.y);
                        return UiReaktion::neu_zeichnen_bereich(bereich);
                    }
                    // Klick auf die Balken-Spur: Seite springen.
                    if *x >= griff.x && bereich.enthaelt(*x, *y) {
                        let richtung = if *y < griff.y { -1 } else { 1 };
                        self.scroll.set(scroll_klemmen(
                            self.scroll.get() + richtung * bereich.hoehe,
                            self.inhalt_hoehe(k),
                            bereich.hoehe,
                        ));
                        return UiReaktion::neu_zeichnen_bereich(bereich);
                    }
                }
                if let Some(index) = self.eintrag_bei(bereich, *x, *y, k) {
                    self.auswahl = Some(index);
                    if self.fokussierbar {
                        self.fokus = true; // Klick fokussiert die Liste
                    }
                    return UiReaktion::nachricht(self.auswahl_nachricht(index));
                }
                UiReaktion::ignoriert()
            }
            UiEreignis::Doppelklick { x, y } => {
                if let Some(index) = self.eintrag_bei(bereich, *x, *y, k) {
                    UiReaktion::nachricht(self.doppelklick_nachricht(index))
                } else {
                    UiReaktion::ignoriert()
                }
            }
            // Rechtsklick: Eintrag auswählen + Kontext-Nachricht;
            // freie Fläche innerhalb der Liste: Leer-Nachricht.
            UiEreignis::Rechtsklick { x, y } => {
                if let Some(index) = self.eintrag_bei(bereich, *x, *y, k) {
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
                Taste::Hoch => {
                    self.auswahl_bewegen(-1, bereich.hoehe, k);
                    match self.auswahl {
                        Some(index) => UiReaktion::nachricht(self.auswahl_nachricht(index)),
                        None => UiReaktion::neu_zeichnen_bereich(bereich),
                    }
                }
                Taste::Runter => {
                    self.auswahl_bewegen(1, bereich.hoehe, k);
                    match self.auswahl {
                        Some(index) => UiReaktion::nachricht(self.auswahl_nachricht(index)),
                        None => UiReaktion::neu_zeichnen_bereich(bereich),
                    }
                }
                Taste::Zeichen('\n') | Taste::Zeichen('\r') => match self.auswahl {
                    Some(index) => UiReaktion::nachricht(self.doppelklick_nachricht(index)),
                    None => UiReaktion::verbraucht(),
                },
                _ => UiReaktion::ignoriert(),
            },
            UiEreignis::Bewegt { x: _, y } => {
                if let Some(griff_versatz) = self.balken_griff {
                    self.scroll.set(self.scroll_aus_griff(bereich, *y - griff_versatz, k));
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
// Tests — jetzt auf dem HOST, mit einem Wirt aus Pappe
//
// Sie sind Wort fuer Wort die Tests aus Serie 3; dazugekommen ist nur der
// `TestWirt` und sein Kontext. Genau das ist der Gewinn der Trennung: Was
// vorher einen QEMU-Start brauchte, laeuft jetzt in Millisekunden.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrappe::{MalProtokoll, Strich, TestWirt};
    use crate::{Maler, Schrift, UiKontext};
    use alloc::format;

    /// Scroll-Klemmen: nie negativ, nie hinter das Inhaltsende.
    #[test]
    fn test_scroll_klemmen() {
        assert_eq!(scroll_klemmen(-10, 500, 100), 0);
        assert_eq!(scroll_klemmen(250, 500, 100), 250);
        assert_eq!(scroll_klemmen(999, 500, 100), 400); // max = 500-100
        assert_eq!(scroll_klemmen(50, 80, 100), 0); // alles sichtbar
    }

    /// Sichtbereich der ScrollListe: richtige Index-Spanne, auch mit
    /// angeschnittenen Eintraegen oben und unten.
    #[test]
    fn test_sichtbare_eintraege() {
        // 20 Eintraege a 26 px, Sichtfenster 100 px:
        assert_eq!(sichtbare_eintraege(0, 100, 26, 20), (0, 4));
        assert_eq!(sichtbare_eintraege(30, 100, 26, 20), (1, 5));
        assert_eq!(sichtbare_eintraege(420, 100, 26, 20), (16, 20));
        assert_eq!(sichtbare_eintraege(0, 100, 26, 2), (0, 2));
    }

    /// Auswahl per Klick + Scroll verschieben den sichtbaren Eintrag.
    #[test]
    fn test_liste_auswahl_und_scroll() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let eintraege = (0..20)
            .map(|i| ListenEintrag { icon: None, text: format!("Eintrag {}", i) })
            .collect();
        let mut liste = ScrollListe::neu(eintraege, 100, 101);
        let bereich = Rechteck::neu(0, 0, 200, 100);

        // Klick auf den zweiten Eintrag (y=30, Eintrag-Hoehe 24 im
        // Test-Thema):
        let klick = liste.ereignis(&UiEreignis::Klick { x: 10, y: 30 }, bereich, &k);
        assert_eq!(liste.auswahl, Some(1));
        assert_eq!(klick.nachricht, Some(100));

        // Rad nach unten (delta -1) scrollt; derselbe Klickpunkt trifft
        // jetzt einen spaeteren Eintrag:
        liste.ereignis(&UiEreignis::Scroll { delta: -1, x: 10, y: 30 }, bereich, &k);
        assert!(liste.scroll.get() > 0);
        liste.ereignis(&UiEreignis::Klick { x: 10, y: 30 }, bereich, &k);
        assert!(liste.auswahl > Some(1));

        // Doppelklick meldet die Doppelklick-Nachricht:
        let doppel = liste.ereignis(&UiEreignis::Doppelklick { x: 10, y: 30 }, bereich, &k);
        assert_eq!(doppel.nachricht, Some(101));
    }

    /// Button: Klick + Loslassen im Bereich = Nachricht; wegziehen
    /// bricht ab; Hover kommt ueber MausRein/MausRaus.
    #[test]
    fn test_button_zustaende() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut button = Button::neu("Test", 7);
        let bereich = Rechteck::neu(0, 0, 100, 30);

        button.ereignis(&UiEreignis::MausRein, bereich, &k);
        assert!(button.hover);
        button.ereignis(&UiEreignis::Klick { x: 10, y: 10 }, bereich, &k);
        assert!(button.gedrueckt);
        // Wegziehen und woanders loslassen: KEINE Nachricht.
        let daneben = button.ereignis(&UiEreignis::Losgelassen { x: 300, y: 10 }, bereich, &k);
        assert_eq!(daneben.nachricht, None);
        // Nochmal, diesmal richtig:
        button.ereignis(&UiEreignis::Klick { x: 10, y: 10 }, bereich, &k);
        let klick = button.ereignis(&UiEreignis::Losgelassen { x: 12, y: 12 }, bereich, &k);
        assert_eq!(klick.nachricht, Some(7));
        button.ereignis(&UiEreignis::MausRaus, bereich, &k);
        assert!(!button.hover);
    }

    /// Textfeld: Tippen ueber den ZeilenEditor, Enter meldet die
    /// Nachricht, Checkbox toggelt.
    #[test]
    fn test_textfeld_und_checkbox() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut feld = Textfeld::neu(42);
        let bereich = Rechteck::neu(0, 0, 200, 30);
        feld.ereignis(&UiEreignis::Klick { x: 5, y: 5 }, bereich, &k); // fokussiert
        assert!(feld.hat_fokus());
        for zeichen in "abc".chars() {
            feld.ereignis(&UiEreignis::Taste(Taste::Zeichen(zeichen)), bereich, &k);
        }
        assert_eq!(feld.text(), "abc");
        feld.ereignis(&UiEreignis::Taste(Taste::Zeichen('\u{8}')), bereich, &k);
        assert_eq!(feld.text(), "ab");
        let enter = feld.ereignis(&UiEreignis::Taste(Taste::Zeichen('\n')), bereich, &k);
        assert_eq!(enter.nachricht, Some(42));

        let mut kasten = Checkbox::neu("An?", false, 9);
        let reaktion =
            kasten.ereignis(&UiEreignis::Klick { x: 5, y: 5 }, Rechteck::neu(0, 0, 100, 30), &k);
        assert!(kasten.an);
        assert_eq!(reaktion.nachricht, Some(9));
    }

    // -----------------------------------------------------------------
    // NEU: Tests AN DER TRAIT-GRENZE — sie gab es vorher nicht, weil es
    // vorher keine Grenze gab.
    // -----------------------------------------------------------------

    /// DAS THEMA WIRD WIRKLICH BENUTZT, nicht nur mitgeschleppt: Ein
    /// Button malt seine Flaeche in der Farbe, die das Thema fuer die
    /// Rolle liefert — und eine ANDERE, sobald der Cursor darueber steht.
    #[test]
    fn test_button_fragt_das_thema() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let mut protokoll = MalProtokoll::neu(200, 100);
        let bereich = Rechteck::neu(0, 0, 100, 30);
        let button = Button::neu("Hallo", 1);

        {
            let mut m = Maler::neu(&mut protokoll, k);
            button.zeichnen(&mut m, bereich);
        }
        // Die Flaeche in Eingabefeld-Farbe, der Rahmen in Rahmen-Farbe,
        // der Text ist da:
        assert!(protokoll.striche.contains(&Strich::Abgerundet(
            bereich,
            k.mass(Mass::RadiusKlein),
            k.farbe(Farbrolle::Eingabefeld)
        )));
        assert!(protokoll.striche.contains(&Strich::Rahmen(bereich, k.farbe(Farbrolle::Rahmen))));
        assert!(protokoll.hat_text("Hallo"));

        // Jetzt mit Hover — der Rahmen wechselt auf den Akzent:
        let mut button = button;
        button.ereignis(&UiEreignis::MausRein, bereich, &k);
        protokoll.leeren();
        {
            let mut m = Maler::neu(&mut protokoll, k);
            button.zeichnen(&mut m, bereich);
        }
        assert!(protokoll.striche.contains(&Strich::Rahmen(bereich, k.farbe(Farbrolle::Akzent))));
    }

    /// DIE SCHRIFT WIRD WIRKLICH GEFRAGT: Die Wunschbreite eines Labels
    /// haengt an der Zeichenbreite des Wirts. Zwei Wirte mit
    /// verschiedenen Schriften ergeben verschiedene Breiten — mit einer
    /// eingebauten Schrift waere das nicht moeglich.
    #[test]
    fn test_label_fragt_die_schrift() {
        struct BreiteSchrift;
        impl Schrift for BreiteSchrift {
            fn zeichen_breite(&self, groesse: i32) -> i32 {
                groesse // doppelt so breit wie die Test-Schrift
            }
            fn zeilen_hoehe(&self, groesse: i32) -> i32 {
                groesse + 4
            }
        }
        let wirt = TestWirt::neu();
        let schmal = wirt.kontext();
        let breit = UiKontext::neu(&wirt.thema, &BreiteSchrift, &wirt.uhr);

        let label = Label::neu("12345");
        assert_eq!(label.wunschgroesse(&schmal).0, 5 * 8);
        assert_eq!(label.wunschgroesse(&breit).0, 5 * 16);
    }

    /// DIE UHR WIRD WIRKLICH GEFRAGT: Der Textfeld-Cursor blinkt — also
    /// zeichnet dasselbe Feld bei verschiedenen Uhrzeiten verschieden
    /// viel. Ohne die Attrappe muesste dieser Test WARTEN; so stellt er
    /// die Zeit.
    #[test]
    fn test_textfeld_cursor_blinkt_nach_der_uhr() {
        let wirt = TestWirt::neu();
        let k = wirt.kontext();
        let bereich = Rechteck::neu(0, 0, 200, 30);
        let mut feld = Textfeld::neu(1);
        feld.fokus_setzen(true);

        let striche_bei = |us: u64, feld: &Textfeld| {
            wirt.uhr.setzen(us);
            let mut protokoll = MalProtokoll::neu(200, 100);
            {
                let mut m = Maler::neu(&mut protokoll, k);
                feld.zeichnen(&mut m, bereich);
            }
            protokoll.striche.len()
        };
        // Blink-Periode ist 500_000 us: in der ersten Haelfte ist der
        // Cursor da (ein Strich mehr), in der zweiten nicht.
        let mit = striche_bei(0, &feld);
        let ohne = striche_bei(500_000, &feld);
        assert_eq!(mit, ohne + 1, "der Cursor muss genau einen Strich ausmachen");
        // Und eine Periode weiter ist er wieder da:
        assert_eq!(striche_bei(1_000_000, &feld), mit);
    }
}
