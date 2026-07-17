// fenster/mod.rs — Fenster, WindowManager und Compositor: das Herz
//                  des SpeedOS-Desktops (jetzt mit vollem "Gesicht")
//
// ARCHITEKTUR (siehe auch CLAUDE.md):
//   * Jedes Fenster = EIGENER Pixel-Puffer (nur der INHALT) +
//     Metadaten (Position, Größe, Titel, Z-Ordnung über die
//     Vec-Reihenfolge, Fokus, minimiert/maximiert). Apps zeichnen NUR
//     in ihren Puffer — nie auf den Bildschirm!
//   * Der Compositor-Task setzt pro Frame zusammen:
//     Desktop-Hintergrund -> Fenster in Z-Reihenfolge (mit Schatten,
//     Titelleiste, Rahmen — die DEKO zeichnet der Compositor, nicht die
//     App) -> Snap-/Switcher-Overlay -> present() -> Maus-Cursor.
//     Dirty-Flags: nur komponieren, wenn sich etwas geändert hat.
//   * Event-Routing: Maus -> oberstes Fenster unter dem Cursor (in
//     Fenster-Koordinaten); Klick hebt+fokussiert; Titelleisten-Knöpfe
//     (Minimieren/Maximieren/Schließen); Titel-Drag verschiebt;
//     Rand-Drag ändert die Größe (Cursor wechselt die Form).
//     Tastatur -> fokussiertes Fenster. Alt+Tab -> Fensterwechsler.
//     Ziehen an den Bildschirmrand -> halbe Fläche (Snap).

pub mod terminal;

use crate::framebuffer::{self, Farbe};
use crate::grafik::{Rechteck, Rgba, Zeichenflaeche, Zeichner};
use crate::maus::{self, MausEvent, MausTaste};
use crate::theme::{self, metrik};
use crate::ui::Widget as _; // Trait-Methoden (zeichnen/ereignis) für Menü/Switcher-Widgets
use crate::zeit;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use noto_sans_mono_bitmap::{get_raster_width, FontWeight};
use pc_keyboard::DecodedKey;
use spin::Mutex;

// ALLE Farben kommen aus theme::aktuell(), ALLE Abstände und
// Schriftgrößen aus theme::METRIK — hier gibt es keine hartcodierten
// Werte mehr (Projektregel seit dem Theme-System).

// ---------------------------------------------------------------------------
// Fenster und Fenster-Puffer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FensterId(u64);

impl FensterId {
    fn neu() -> Self {
        static NAECHSTE: AtomicU64 = AtomicU64::new(1);
        FensterId(NAECHSTE.fetch_add(1, Ordering::Relaxed))
    }
}

/// Der private Pixel-Puffer eines Fensters (nur der INHALT).
pub struct FensterPuffer {
    breite: usize,
    hoehe: usize,
    pixel: Vec<Farbe>,
}

impl FensterPuffer {
    pub(crate) fn neu(breite: usize, hoehe: usize, fuellung: Farbe) -> Self {
        FensterPuffer {
            breite,
            hoehe,
            pixel: vec![fuellung; breite * hoehe],
        }
    }
}

impl Zeichenflaeche for FensterPuffer {
    fn flaeche_breite(&self) -> usize {
        self.breite
    }
    fn flaeche_hoehe(&self) -> usize {
        self.hoehe
    }
    fn flaeche_setzen(&mut self, x: usize, y: usize, farbe: Farbe) {
        if x < self.breite && y < self.hoehe {
            self.pixel[y * self.breite + x] = farbe;
        }
    }
    fn flaeche_lesen(&self, x: usize, y: usize) -> Option<Farbe> {
        if x < self.breite && y < self.hoehe {
            Some(self.pixel[y * self.breite + x])
        } else {
            None
        }
    }
    // Zeilen-Schnellpfade: Der Puffer ist ein flaches Farb-Array —
    // Füllen und Blitten sind schlicht fill/copy auf einem Slice.
    // (Der Zeichner hat die Bereiche vorab geclippt, trotzdem noch
    // defensiv kappen — ein Panic im Compositor wäre fatal.)
    fn flaeche_zeile_fuellen(&mut self, x: usize, y: usize, breite: usize, farbe: Farbe) {
        if y >= self.hoehe || x >= self.breite {
            return;
        }
        let bis = (x + breite).min(self.breite);
        self.pixel[y * self.breite + x..y * self.breite + bis].fill(farbe);
    }
    fn flaeche_zeile_kopieren(&mut self, x: usize, y: usize, pixel: &[Farbe]) {
        if y >= self.hoehe || x >= self.breite {
            return;
        }
        let anzahl = pixel.len().min(self.breite - x);
        let von = y * self.breite + x;
        self.pixel[von..von + anzahl].copy_from_slice(&pixel[..anzahl]);
    }
}

/// Die Inhalte, die ein Fenster darstellen kann.
pub enum Inhalt {
    /// Eine SpeedShell-SITZUNG als Fenster: Das Raster plus die
    /// Sitzungs-Id (shell::sitzung) — konsole::_print leitet die
    /// Ausgabe der passenden Sitzung hierher um, der Eingabe-Router
    /// wirft Tasten in die Sitzungs-Queue des fokussierten Terminals.
    Terminal { term: terminal::Terminal, sitzung: u64 },
    /// Ein nackter Widget-Baum mit fn(u32)-Handler (zustandslose
    /// Fälle); zustandsbehaftete Apps nehmen Inhalt::App.
    Ui(crate::ui::UiFenster),
    /// Eine Trait-App (ui::App): Zustand + Widget-Baum — DIE Brücke
    /// vom Enum zum Trait. Jede NEUE App implementiert das Trait;
    /// das Enum bleibt für Terminal und die alten Demos.
    App(crate::ui::AppFenster),
    Uhr,
    TastaturEcho { text: String },
    Malflaeche { klicks: Vec<(i32, i32)> },
}

/// Arbeit, die der Aufrufer erst NACH dem Loslassen des MANAGER-Locks
/// erledigen darf (Deadlock-Regel: App-Starts nehmen den Lock selbst,
/// Nachricht-Handler drucken womöglich — print! braucht die
/// KONSOLE-vor-MANAGER-Lock-Ordnung).
pub enum NachLock {
    Keine,
    /// App-Start aus der Registry (zustandslose fn).
    Ausfuehren(fn()),
    /// App-"danach"-Aktion MIT Daten (ui::AppReaktion::danach) —
    /// z. B. "öffne den Betrachter für DIESEN Pfad".
    Einmal(alloc::boxed::Box<dyn FnOnce() + Send>),
    Nachricht(crate::ui::NachrichtHandler, u32),
}

/// Führt NachLock-Arbeit aus — NIEMALS unter dem MANAGER-Lock rufen!
fn nach_lock_ausfuehren(nach: NachLock) {
    match nach {
        NachLock::Keine => {}
        NachLock::Ausfuehren(aktion) => aktion(),
        NachLock::Einmal(aktion) => aktion(),
        NachLock::Nachricht(handler, id) => handler(id),
    }
}

/// Die drei Knöpfe rechts in der Titelleiste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Knopf {
    Minimieren,
    Maximieren,
    Schliessen,
}

/// Welche Kante/Ecke wird beim Resize gezogen?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kante {
    Links,
    Rechts,
    Unten,
    UntenLinks,
    UntenRechts,
}

impl Kante {
    fn cursor_form(self) -> u8 {
        match self {
            Kante::Links | Kante::Rechts => maus::FORM_HORIZONTAL,
            Kante::Unten => maus::FORM_VERTIKAL,
            Kante::UntenRechts => maus::FORM_DIAG_NWSE,
            Kante::UntenLinks => maus::FORM_DIAG_NESW,
        }
    }
}

pub struct Fenster {
    pub id: FensterId,
    titel: String,
    x: i32,
    y: i32,
    puffer: FensterPuffer,
    inhalt: Inhalt,
    dirty: bool,
    /// Der Inhalt hat sich geändert und muss VOR dem nächsten
    /// Komponieren neu in den Puffer gerendert werden (gebündelt pro
    /// Frame — ein Terminal rendert nicht bei jedem print! neu).
    inhalt_neu: bool,
    minimiert: bool,
    /// Vor dem Maximieren/Snappen gespeicherte Geometrie (Rückkehr).
    vorher: Option<(i32, i32, usize, usize)>,
}

impl Fenster {
    fn breite(&self) -> i32 {
        self.puffer.breite as i32
    }
    fn hoehe(&self) -> i32 {
        self.puffer.hoehe as i32
    }

    /// Gesamtfläche inkl. Titelzeile.
    fn gesamt_rechteck(&self) -> Rechteck {
        Rechteck::neu(self.x, self.y, self.breite(), metrik().titel_hoehe + self.hoehe())
    }

    fn in_titelzeile(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.breite() && py >= self.y && py < self.y + metrik().titel_hoehe
    }

    /// Bildschirm- -> Fensterinhalts-Koordinaten (None = außerhalb).
    fn lokal(&self, px: i32, py: i32) -> Option<(i32, i32)> {
        let lx = px - self.x;
        let ly = py - self.y - metrik().titel_hoehe;
        if lx >= 0 && ly >= 0 && lx < self.breite() && ly < self.hoehe() {
            Some((lx, ly))
        } else {
            None
        }
    }

    /// Die drei Knopf-Rechtecke (Bildschirmkoordinaten), von rechts:
    /// Schließen, Maximieren, Minimieren.
    fn knoepfe(&self) -> [(Knopf, Rechteck); 3] {
        let rechts = self.x + self.breite();
        let y = self.y + 3;
        let h = metrik().titel_hoehe - 6;
        [
            (Knopf::Schliessen, Rechteck::neu(rechts - metrik().knopf_breite, y, metrik().knopf_breite - 4, h)),
            (Knopf::Maximieren, Rechteck::neu(rechts - 2 * metrik().knopf_breite, y, metrik().knopf_breite - 4, h)),
            (Knopf::Minimieren, Rechteck::neu(rechts - 3 * metrik().knopf_breite, y, metrik().knopf_breite - 4, h)),
        ]
    }

    fn knopf_bei(&self, px: i32, py: i32) -> Option<Knopf> {
        self.knoepfe()
            .into_iter()
            .find(|(_, r)| r.enthaelt(px, py))
            .map(|(k, _)| k)
    }

    /// Setzt die Inhaltsgröße neu (realloziert den Puffer).
    fn groesse_setzen(&mut self, breite: usize, hoehe: usize) {
        let breite = breite.max(metrik().min_fenster_breite);
        let hoehe = hoehe.max(metrik().min_fenster_hoehe);
        if breite != self.puffer.breite || hoehe != self.puffer.hoehe {
            self.puffer = FensterPuffer::neu(breite, hoehe, theme::aktuell().inhalt_hintergrund);
        }
    }
}

// ---------------------------------------------------------------------------
// Der WindowManager
// ---------------------------------------------------------------------------

/// Was tut die Maus gerade mit gedrückter Taste?
enum Interaktion {
    Keine,
    Verschieben { id: FensterId, griff_dx: i32, griff_dy: i32 },
    Groesse {
        id: FensterId,
        kante: Kante,
        start_px: i32,
        start_py: i32,
        start_x: i32,
        start_y: i32,
        start_breite: i32,
        start_hoehe: i32,
    },
}

/// Der Alt+Tab-Fensterwechsler — seit dem Toolkit-Umzug eine
/// ScrollListe, die in einen Offscreen-Puffer zeichnet (der
/// Compositor blittet ihn zentriert und malt die Deko drumherum).
struct Switcher {
    reihenfolge: Vec<FensterId>,
    liste: crate::ui::widgets::ScrollListe,
    puffer: FensterPuffer,
}

impl Switcher {
    /// Innenmaß der Liste im Puffer.
    fn liste_bereich(&self) -> Rechteck {
        Rechteck::neu(
            metrik().abstand,
            metrik().abstand,
            self.puffer.breite as i32 - 2 * metrik().abstand,
            self.puffer.hoehe as i32 - 2 * metrik().abstand,
        )
    }

    fn zeichnen(&mut self) {
        let thema = theme::aktuell();
        let bereich = self.liste_bereich();
        let (breite, hoehe) = (self.puffer.breite as i32, self.puffer.hoehe as i32);
        let mut z = Zeichner::neu(&mut self.puffer);
        z.rechteck_fuellen(Rechteck::neu(0, 0, breite, hoehe), thema.flaeche);
        self.liste.zeichnen(&mut z, bereich);
    }
}

/// Das GENERISCHE Kontextmenü-Overlay (Rechtsklick): eine Liste von
/// (Beschriftung, Nachricht-ID)-Einträgen, gezeichnet in einen
/// Offscreen-Puffer (dasselbe Muster wie Startmenü/Switcher). Heute
/// füllen es Trait-Apps (AppReaktion::menue); Taskleiste und Desktop
/// können später dasselbe Overlay mit eigenem Empfänger nutzen —
/// dafür ist der Empfänger als FensterId (statt App-Referenz) gelöst.
struct KontextMenue {
    eintraege: Vec<(String, u32)>,
    /// Wer bekommt die Eintrag-Nachricht? (App-Fenster)
    empfaenger: FensterId,
    x: i32,
    y: i32,
    puffer: FensterPuffer,
}

impl KontextMenue {
    fn neu(empfaenger: FensterId, eintraege: Vec<(String, u32)>, x: i32, y: i32) -> Self {
        let zeichen = get_raster_width(FontWeight::Regular, metrik().schrift_ui) as i32;
        let laengste = eintraege.iter().map(|(t, _)| t.chars().count()).max().unwrap_or(4) as i32;
        let breite = (laengste * zeichen + 4 * metrik().abstand).max(120);
        let hoehe = eintraege.len().max(1) as i32 * metrik().listen_eintrag_hoehe + 2;
        let mut menue = KontextMenue {
            eintraege,
            empfaenger,
            x,
            y,
            puffer: FensterPuffer::neu(breite as usize, hoehe as usize, theme::aktuell().inhalt_hintergrund),
        };
        menue.zeichnen();
        menue
    }

    fn rechteck(&self) -> Rechteck {
        Rechteck::neu(self.x, self.y, self.puffer.breite as i32, self.puffer.hoehe as i32)
    }

    fn zeichnen(&mut self) {
        let thema = theme::aktuell();
        let (breite, hoehe) = (self.puffer.breite as i32, self.puffer.hoehe as i32);
        let mut z = Zeichner::neu(&mut self.puffer);
        z.rechteck_fuellen(Rechteck::neu(0, 0, breite, hoehe), thema.flaeche);
        for (i, (text, _)) in self.eintraege.iter().enumerate() {
            let y = 1 + i as i32 * metrik().listen_eintrag_hoehe;
            z.text(
                2 * metrik().abstand,
                y + (metrik().listen_eintrag_hoehe - metrik().zeilen_hoehe) / 2,
                text,
                metrik().schrift_ui,
                FontWeight::Regular,
                thema.text_normal,
            );
        }
    }

    /// Nachricht-ID des Eintrags an der Bildschirmposition.
    fn eintrag_bei(&self, px: i32, py: i32) -> Option<u32> {
        if !self.rechteck().enthaelt(px, py) {
            return None;
        }
        let zeile = ((py - self.y - 1) / metrik().listen_eintrag_hoehe) as usize;
        self.eintraege.get(zeile).map(|(_, id)| *id)
    }
}

// Die Widget-Nachrichten des Startmenüs (u32-IDs, siehe ui-Modul).
const MENUE_ENTER: u32 = 1;
const MENUE_SUCHE_GEAENDERT: u32 = 2;
const MENUE_EINTRAG_KLICK: u32 = 3;

/// Das Startmenü (None im Manager = zu) — seit dem Toolkit-Umzug
/// ein Widget-Verbund: Suchfeld (Textfeld) + App-Liste (ScrollListe)
/// in einem Offscreen-Puffer; der Compositor blittet ihn über der
/// Taskleiste. Der Manager routet Maus (in Panel-Koordinaten) und
/// Tasten hierher — die alte handgestrickte Eintrags-Geometrie,
/// Hover-Pflege und Tasten-Kaskade sind damit Geschichte.
struct StartMenue {
    suchfeld: crate::ui::widgets::Textfeld,
    liste: crate::ui::widgets::ScrollListe,
    puffer: FensterPuffer,
    /// gefiltert[zeile] = Index des Listeneintrags in apps::alle_apps().
    gefiltert: Vec<usize>,
}

impl StartMenue {
    const BREITE: i32 = 340;
    /// Suchfeld + 8 Listenzeilen + Innenränder.
    fn hoehe() -> i32 {
        metrik().ui_element_hoehe + 8 * metrik().listen_eintrag_hoehe + 3 * metrik().abstand
    }

    fn neu() -> Self {
        use crate::ui::widgets::{ScrollListe, Textfeld};

        let mut suchfeld =
            Textfeld::mit_aenderungs_nachricht(MENUE_ENTER, MENUE_SUCHE_GEAENDERT);
        // Das Suchfeld ist immer fokussiert — Tippen filtert sofort.
        suchfeld.fokus_setzen(true);
        let mut menue = StartMenue {
            suchfeld,
            liste: ScrollListe::neu(Vec::new(), MENUE_EINTRAG_KLICK, MENUE_EINTRAG_KLICK),
            puffer: FensterPuffer::neu(
                Self::BREITE as usize,
                Self::hoehe() as usize,
                theme::aktuell().inhalt_hintergrund,
            ),
            gefiltert: Vec::new(),
        };
        menue.filtern();
        menue.zeichnen();
        menue
    }

    fn suchfeld_bereich() -> Rechteck {
        Rechteck::neu(
            metrik().abstand,
            metrik().abstand,
            Self::BREITE - 2 * metrik().abstand,
            metrik().ui_element_hoehe,
        )
    }

    fn liste_bereich() -> Rechteck {
        let oben = 2 * metrik().abstand + metrik().ui_element_hoehe;
        Rechteck::neu(
            metrik().abstand,
            oben,
            Self::BREITE - 2 * metrik().abstand,
            Self::hoehe() - oben - metrik().abstand,
        )
    }

    /// Filtert die App-Liste nach dem Suchfeld-Text.
    fn filtern(&mut self) {
        use crate::ui::widgets::ListenEintrag;

        self.gefiltert = crate::apps::filtern_indizes(self.suchfeld.text());
        let eintraege = self
            .gefiltert
            .iter()
            .map(|&index| {
                let app = &crate::apps::alle_apps()[index];
                ListenEintrag { icon: Some(app.icon), text: String::from(app.name) }
            })
            .collect();
        self.liste.eintraege_setzen(eintraege);
    }

    /// Zeichnet Suchfeld + Liste in den Offscreen-Puffer.
    fn zeichnen(&mut self) {
        let thema = theme::aktuell();
        let (breite, hoehe) = (self.puffer.breite as i32, self.puffer.hoehe as i32);
        let mut z = Zeichner::neu(&mut self.puffer);
        z.rechteck_fuellen(Rechteck::neu(0, 0, breite, hoehe), thema.flaeche);
        self.suchfeld.zeichnen(&mut z, Self::suchfeld_bereich());
        self.liste.zeichnen(&mut z, Self::liste_bereich());
    }

    /// Routet ein Ereignis (Panel-Koordinaten): Pfeiltasten steuern
    /// die Liste, andere Tasten das Suchfeld; Maus-Ereignisse gehen
    /// je nach Position an Suchfeld oder Liste.
    fn ereignis(&mut self, ereignis: &crate::ui::UiEreignis) -> crate::ui::UiReaktion {
        use crate::ui::UiEreignis;
        use pc_keyboard::KeyCode;

        match ereignis {
            UiEreignis::Taste(DecodedKey::RawKey(KeyCode::ArrowUp)) => {
                self.liste.auswahl_bewegen(-1, Self::liste_bereich().hoehe);
                crate::ui::UiReaktion::neu_zeichnen()
            }
            UiEreignis::Taste(DecodedKey::RawKey(KeyCode::ArrowDown)) => {
                self.liste.auswahl_bewegen(1, Self::liste_bereich().hoehe);
                crate::ui::UiReaktion::neu_zeichnen()
            }
            UiEreignis::Taste(_) => self.suchfeld.ereignis(ereignis, Self::suchfeld_bereich()),
            _ => match ereignis.position() {
                Some((x, y)) if Self::suchfeld_bereich().enthaelt(x, y) => {
                    self.suchfeld.ereignis(ereignis, Self::suchfeld_bereich())
                }
                // Alles andere an die Liste (auch Bewegt/Losgelassen
                // außerhalb — für den Scrollbalken-Drag).
                _ => self.liste.ereignis(ereignis, Self::liste_bereich()),
            },
        }
    }
}

pub struct FensterManager {
    /// Z-Ordnung: LETZTES Element = ganz vorne.
    fenster: Vec<Fenster>,
    fokus: Option<FensterId>,
    interaktion: Interaktion,
    /// Snap-Vorschau während des Verschiebens (-1 links, +1 rechts).
    snap_hinweis: i8,
    switcher: Option<Switcher>,
    start_menue: Option<StartMenue>,
    kontext_menue: Option<KontextMenue>,
    alles_dirty: bool,
    bildschirm_breite: i32,
    bildschirm_hoehe: i32,
    /// Zuletzt in der Taskleiste angezeigte Sekunde — nur bei einem
    /// Wechsel wird neu komponiert (nicht bei jedem Uhr-Task-Lauf).
    letzte_uhr_sekunde: u64,
    /// Über welchem Ui-Fenster schwebt der Cursor? (für MausRaus)
    ui_hover_fenster: Option<FensterId>,
    /// Der Desktop-Verlauf muss (neu) in den Framebuffer-Hintergrund-
    /// Cache gerendert werden (erster Frame, Theme-Wechsel). Das
    /// erledigt der Compositor, weil nur er den Framebuffer hat.
    hintergrund_neu: bool,
    /// DIRTY-RECT-PROTOKOLL: Änderungen melden ihre Fläche hier an
    /// (dirty_melden); der Compositor komponiert + presentet NUR diese
    /// Rechtecke. alles_dirty bleibt der Vollbild-Fallback (Theme/
    /// Skalierung/Snap-Vorschau, Überlauf von MAX_DIRTY_RECTS).
    dirty_rects: Vec<Rechteck>,
}

/// Mehr als so viele Einzel-Rechtecke pro Frame -> Vollbild-Fallback.
const MAX_DIRTY_RECTS: usize = 16;

/// Rendert den Desktop-Verlauf in den Back-Buffer und übernimmt ihn
/// als Hintergrund-Cache (byte-identisches memcpy-Format — deshalb
/// lebt der Cache im DoppelPuffer, nicht hier). Welcher Verlauf das
/// ist, entscheidet theme::hintergrund_verlauf() (Preset-Auswahl der
/// Einstellungen-App; Preset 0 = Theme-Aurora).
pub(crate) fn hintergrund_in_cache_rendern(fb: &mut framebuffer::DoppelPuffer) {
    let (oben, unten) = theme::hintergrund_verlauf();
    let hoehe = fb.info().height;
    for y in 0..hoehe {
        let t = (y * 255 / hoehe.max(1)) as u8;
        fb.zeile_fuellen(y, oben.mischen(unten, t));
    }
    fb.hintergrund_uebernehmen();
}

impl FensterManager {
    pub fn neu(bildschirm_breite: i32, bildschirm_hoehe: i32) -> Self {
        FensterManager {
            fenster: Vec::new(),
            fokus: None,
            interaktion: Interaktion::Keine,
            snap_hinweis: 0,
            switcher: None,
            start_menue: None,
            kontext_menue: None,
            alles_dirty: true,
            bildschirm_breite,
            bildschirm_hoehe,
            letzte_uhr_sekunde: 0,
            ui_hover_fenster: None,
            hintergrund_neu: true,
            dirty_rects: Vec::new(),
        }
    }

    /// Meldet eine geänderte Bildschirm-Fläche fürs nächste
    /// Komponieren an (das Dirty-Rect-Protokoll). Läuft die Liste
    /// über, fällt der Frame auf Vollbild zurück.
    fn dirty_melden(&mut self, rect: Rechteck) {
        if self.alles_dirty {
            return;
        }
        if self.dirty_rects.len() >= MAX_DIRTY_RECTS {
            self.alles_dirty = true;
            self.dirty_rects.clear();
            return;
        }
        self.dirty_rects.push(rect);
    }

    /// Die Bildschirm-Fläche eines Fensters INKLUSIVE Schatten
    /// (10 Pixel rechts/unten) — die Einheit des Dirty-Meldens.
    fn fenster_flaeche(&self, index: usize) -> Rechteck {
        let rect = self.fenster[index].gesamt_rechteck();
        Rechteck::neu(rect.x, rect.y, rect.breite + 10, rect.hoehe + 10)
    }

    /// Meldet die Fläche des Fensters mit dieser Id.
    fn fenster_dirty_melden(&mut self, id: FensterId) {
        if let Some(index) = self.index_von(id) {
            let flaeche = self.fenster_flaeche(index);
            self.dirty_melden(flaeche);
        }
    }

    /// Die Systray-Fläche (Uhr + Icons) der Taskleiste.
    fn systray_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            self.bildschirm_breite - metrik().systray_breite,
            self.taskleiste_y(),
            metrik().systray_breite,
            metrik().taskleiste_hoehe,
        )
    }

    /// Holt die zu komponierenden Rechtecke ab und setzt alle Flags
    /// zurück. None = nichts zu tun; alles_dirty -> ein Vollbild-Rect.
    /// Die Rects sind auf den Bildschirm geklemmt.
    fn dirty_abholen(&mut self, breite: i32, hoehe: i32) -> Option<Vec<Rechteck>> {
        // Fenster mit geändertem Inhalt melden ihre Fläche selbst:
        for index in 0..self.fenster.len() {
            if self.fenster[index].dirty {
                self.fenster[index].dirty = false;
                let flaeche = self.fenster_flaeche(index);
                self.dirty_melden(flaeche);
            }
        }
        let vollbild = Rechteck::neu(0, 0, breite, hoehe);
        if self.alles_dirty {
            self.alles_dirty = false;
            self.dirty_rects.clear();
            return Some(vec![vollbild]);
        }
        if self.dirty_rects.is_empty() {
            return None;
        }
        let rects: Vec<Rechteck> = core::mem::take(&mut self.dirty_rects)
            .into_iter()
            .filter_map(|rect| rect.schneiden(&vollbild))
            .collect();
        if rects.is_empty() {
            return None;
        }
        Some(rects)
    }

    pub fn fenster_erstellen(
        &mut self,
        titel: &str,
        x: i32,
        y: i32,
        breite: usize,
        hoehe: usize,
        inhalt: Inhalt,
    ) -> FensterId {
        let id = FensterId::neu();
        let mut fenster = Fenster {
            id,
            titel: String::from(titel),
            x,
            y,
            puffer: FensterPuffer::neu(breite, hoehe, theme::aktuell().inhalt_hintergrund),
            inhalt,
            dirty: true,
            inhalt_neu: false,
            minimiert: false,
            vorher: None,
        };
        inhalt_zeichnen(&mut fenster);
        self.fenster.push(fenster);
        let fokus_vorher = self.fokus;
        self.fokus = Some(id);
        // Neues Fenster + alter Fokus (Titel dimmt) + Taskleiste
        // (neuer Knopf):
        self.fenster_dirty_melden(id);
        if let Some(alt) = fokus_vorher {
            self.fenster_dirty_melden(alt);
        }
        let leiste = self.taskleiste_rechteck();
        self.dirty_melden(leiste);
        id
    }

    /// Die komplette Taskleisten-Fläche.
    fn taskleiste_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            0,
            self.taskleiste_y(),
            self.bildschirm_breite,
            metrik().taskleiste_hoehe,
        )
    }

    fn index_von(&self, id: FensterId) -> Option<usize> {
        self.fenster.iter().position(|f| f.id == id)
    }

    /// Oberstes SICHTBARES Fenster unter dem Punkt.
    pub fn fenster_unter(&self, px: i32, py: i32) -> Option<FensterId> {
        self.fenster
            .iter()
            .rev()
            .find(|f| !f.minimiert && f.gesamt_rechteck().enthaelt(px, py))
            .map(|f| f.id)
    }

    pub fn fokussieren_und_heben(&mut self, id: FensterId) {
        if let Some(index) = self.index_von(id) {
            let fokus_vorher = self.fokus;
            let fenster = self.fenster.remove(index);
            self.fenster.push(fenster);
            self.fokus = Some(id);
            // Gehobenes Fenster + alter Fokus (Titel dimmt) +
            // Taskleiste (Knopf-Highlight wandert):
            self.fenster_dirty_melden(id);
            if let Some(alt) = fokus_vorher {
                self.fenster_dirty_melden(alt);
            }
            let leiste = self.taskleiste_rechteck();
            self.dirty_melden(leiste);
        }
    }

    /// Nach Minimieren/Schließen: das oberste sichtbare Fenster fokussieren.
    fn fokus_neu_bestimmen(&mut self) {
        self.fokus = self
            .fenster
            .iter()
            .rev()
            .find(|f| !f.minimiert)
            .map(|f| f.id);
    }

    // ----- Taskleiste -----

    /// Y-Position der Oberkante der Taskleiste.
    fn taskleiste_y(&self) -> i32 {
        self.bildschirm_hoehe - metrik().taskleiste_hoehe
    }

    /// Das Rechteck des Startknopfs (ganz links in der Leiste).
    fn start_knopf_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            0,
            self.taskleiste_y(),
            metrik().start_knopf_breite,
            metrik().taskleiste_hoehe,
        )
    }

    /// Die Fenster-Knöpfe der Taskleiste: (FensterId, Rechteck).
    /// Nach FensterId (= Erstellungsreihenfolge) sortiert, damit die
    /// Knöpfe beim Fokuswechsel nicht in der Leiste herumspringen —
    /// die Z-Ordnung im fenster-Vec ändert sich ja bei jedem Klick.
    fn taskleisten_knoepfe(&self) -> Vec<(FensterId, Rechteck)> {
        let mut ids: Vec<u64> = self.fenster.iter().map(|f| f.id.0).collect();
        ids.sort_unstable();
        if ids.is_empty() {
            return Vec::new();
        }
        let von = metrik().start_knopf_breite + metrik().abstand;
        let bis = self.bildschirm_breite - metrik().systray_breite;
        // Standardbreite, aber schrumpfen, wenn es eng wird:
        let breite =
            ((bis - von) / ids.len() as i32 - 4).clamp(40, metrik().leisten_knopf_breite);
        let y = self.taskleiste_y() + 5;
        ids.into_iter()
            .enumerate()
            .map(|(i, id)| {
                let x = von + i as i32 * (breite + 4);
                (FensterId(id), Rechteck::neu(x, y, breite, metrik().taskleiste_hoehe - 10))
            })
            .collect()
    }

    /// Klick in die Taskleiste (Startknopf / Fenster-Knöpfe).
    fn taskleiste_klick(&mut self, px: i32, py: i32) {
        if self.start_knopf_rechteck().enthaelt(px, py) {
            self.startmenue_umschalten();
            return;
        }
        for (id, rect) in self.taskleisten_knoepfe() {
            if rect.enthaelt(px, py) {
                self.taskleisten_knopf_aktion(id);
                return;
            }
        }
    }

    /// Fenster-Knopf: fokussieren ODER minimieren (Toggle wie bei den
    /// "Großen": Klick aufs fokussierte Fenster legt es weg).
    fn taskleisten_knopf_aktion(&mut self, id: FensterId) {
        let index = match self.index_von(id) {
            Some(i) => i,
            None => return,
        };
        if self.fokus == Some(id) && !self.fenster[index].minimiert {
            self.fenster[index].minimiert = true;
            self.fokus_neu_bestimmen();
        } else {
            self.fenster[index].minimiert = false;
            self.fokussieren_und_heben(id);
        }
        self.alles_dirty = true;
    }

    // ----- Terminal (die SpeedShell als Fenster) -----

    /// Index des (einzigen) Terminal-Fensters.
    /// Die Sitzung des FOKUSSIERTEN Terminal-Fensters (None = kein
    /// unminimiertes Terminal im Fokus) — die Routing-Grundlage des
    /// Eingabe-Routers, deshalb als eigene, testbare Methode.
    fn fokus_terminal_sitzung(&self) -> Option<u64> {
        self.fokus.and_then(|id| self.index_von(id)).and_then(|i| match self.fenster[i].inhalt {
            Inhalt::Terminal { sitzung, .. } if !self.fenster[i].minimiert => Some(sitzung),
            _ => None,
        })
    }

    /// Der Fenster-Index eines Terminals nach Sitzungs-Id.
    fn terminal_index(&self, sitzung: u64) -> Option<usize> {
        self.fenster.iter().position(
            |f| matches!(f.inhalt, Inhalt::Terminal { sitzung: s, .. } if s == sitzung),
        )
    }

    /// Öffnet ein NEUES Terminal-Fenster mit eigener Shell-Sitzung
    /// (das Ein-Terminal-Limit ist Geschichte) und liefert die
    /// Sitzungs-Id. Das erste Terminal wird HAUPT-Terminal
    /// (Kernel-Log-Ziel) und bekommt den gepufferten Log nachgereicht.
    fn terminal_oeffnen(&mut self) -> u64 {
        // Wunschgröße: 80x24 Zellen — auf kleinen Schirmen weniger.
        let zeichen_breite = get_raster_width(FontWeight::Regular, metrik().schrift_ui);
        let breite = (80 * zeichen_breite)
            .min((self.bildschirm_breite as usize).saturating_sub(80))
            .max(metrik().min_fenster_breite);
        let hoehe = (24 * metrik().zeilen_hoehe as usize)
            .min((self.bildschirm_hoehe as usize).saturating_sub(160))
            .max(metrik().min_fenster_hoehe);
        // Weitere Terminals leicht versetzt (Kaskade), damit sie
        // nicht deckungsgleich übereinander liegen.
        let versatz = (self.fenster.len() as i32 % 5) * 40;
        let x = ((self.bildschirm_breite - breite as i32) / 2 + versatz)
            .min(self.bildschirm_breite - breite as i32);
        let y = (((self.taskleiste_y() - metrik().titel_hoehe - hoehe as i32) / 2).max(20)
            + versatz)
            .min(self.taskleiste_y() - metrik().titel_hoehe - hoehe as i32);
        let term = terminal::Terminal::neu(
            breite / zeichen_breite,
            hoehe / metrik().zeilen_hoehe as usize,
            theme::aktuell().terminal_hintergrund,
        );
        let sitzung = crate::shell::sitzung::neu_registrieren();
        self.fenster_erstellen(
            &format!("Terminal {}", sitzung),
            x,
            y,
            breite,
            hoehe,
            Inhalt::Terminal { term, sitzung },
        );
        // Das erste offene Terminal wird Kernel-Log-Ziel — und holt
        // den in terminalloser Zeit gepufferten Log nach.
        if crate::shell::sitzung::haupt() == 0 {
            crate::shell::sitzung::haupt_setzen(sitzung);
            for (text, vg, hg) in crate::shell::sitzung::log_abholen() {
                let _ = self.terminal_schreiben(sitzung, format_args!("{}", text), vg, hg);
            }
        }
        sitzung
    }

    /// Schreibt formatierten Text ins Terminal-Fenster der Sitzung
    /// (Umleitung von konsole::_print im Desktop-Modus). false =
    /// kein Terminal dieser Sitzung offen. Rendert NICHT sofort —
    /// nur inhalt_neu setzen, der Compositor bündelt pro Frame.
    fn terminal_schreiben(
        &mut self,
        sitzung: u64,
        args: core::fmt::Arguments,
        vg: Farbe,
        hg: Farbe,
    ) -> bool {
        let index = match self.terminal_index(sitzung) {
            Some(index) => index,
            None => return false,
        };
        if let Inhalt::Terminal { term, .. } = &mut self.fenster[index].inhalt {
            struct TerminalZeichner<'a> {
                term: &'a mut terminal::Terminal,
                vg: Farbe,
                hg: Farbe,
            }
            impl core::fmt::Write for TerminalZeichner<'_> {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    for zeichen in s.chars() {
                        self.term.schreiben(zeichen, self.vg, self.hg);
                    }
                    Ok(())
                }
            }
            use core::fmt::Write;
            TerminalZeichner { term, vg, hg }.write_fmt(args).ok();
        }
        self.fenster[index].inhalt_neu = true;
        // PRÄZISE Dirty-Meldung statt fenster.dirty: Nur der
        // geänderte Zeilen-Streifen wird komponiert und übertragen —
        // eine Prompt-Zeile kostet so 16 Pixelzeilen, kein Fenster.
        // (Beim Raster-Scroll ist der Streifen automatisch alles.)
        if !self.fenster[index].minimiert {
            let streifen = match &self.fenster[index].inhalt {
                Inhalt::Terminal { term, .. } => {
                    term.dirty_zeilen().map(|bereich| (bereich, term.zeilen()))
                }
                _ => None,
            };
            if let Some(((von, bis), raster_zeilen)) = streifen {
                let zeilen_hoehe = metrik().zeilen_hoehe;
                let f = &self.fenster[index];
                // Endet der Streifen an der letzten Rasterzeile,
                // gehört der Restsaum unter ihr mit dazu (die
                // Fensterhöhe ist kein Zeilen-Vielfaches):
                let oben = von as i32 * zeilen_hoehe;
                let unten = if bis == raster_zeilen {
                    f.hoehe()
                } else {
                    bis as i32 * zeilen_hoehe
                };
                let rect = Rechteck::neu(
                    f.x,
                    f.y + metrik().titel_hoehe + oben,
                    f.breite(),
                    unten - oben,
                );
                self.dirty_melden(rect);
            }
        }
        true
    }

    /// Leert das Terminal-Raster einer Sitzung (clear-Befehl).
    fn terminal_leeren(&mut self, sitzung: u64) {
        if let Some(index) = self.terminal_index(sitzung) {
            if let Inhalt::Terminal { term, .. } = &mut self.fenster[index].inhalt {
                term.leeren();
            }
            self.fenster[index].inhalt_neu = true;
            self.fenster[index].dirty = true;
        }
    }

    /// Rendert alle geänderten Inhalte (inhalt_neu) in ihre Puffer —
    /// ruft der Compositor EINMAL pro Frame, vor dem Komponieren.
    /// TERMINALS gehen den schlanken Weg: nur die geänderten
    /// Rasterzeilen in den (persistenten) Puffer, und KEIN
    /// fenster.dirty — terminal_schreiben hat den Streifen schon
    /// präzise per dirty_melden angemeldet (Performance-Pass:
    /// eine Prompt-Zeile komponiert 16 px statt der Fensterfläche).
    fn inhalte_rendern(&mut self) {
        for index in 0..self.fenster.len() {
            if self.fenster[index].inhalt_neu {
                self.fenster[index].inhalt_neu = false;
                let fenster = &mut self.fenster[index];
                if let Inhalt::Terminal { term, .. } = &mut fenster.inhalt {
                    let zeichen_breite =
                        get_raster_width(FontWeight::Regular, metrik().schrift_ui);
                    let spalten = (fenster.puffer.breite / zeichen_breite).max(1);
                    let zeilen =
                        (fenster.puffer.hoehe / metrik().zeilen_hoehe as usize).max(1);
                    term.groesse_setzen(spalten, zeilen);
                    terminal_rendern(term, &mut fenster.puffer);
                } else {
                    inhalt_zeichnen(fenster);
                    self.fenster[index].dirty = true;
                }
            }
        }
    }

    // ----- Kontextmenü (generisches Rechtsklick-Overlay) -----

    /// Öffnet das Kontextmenü an der Maus-Position (aufs Bild geklemmt).
    fn kontextmenue_oeffnen(&mut self, empfaenger: FensterId, eintraege: Vec<(String, u32)>) {
        if eintraege.is_empty() {
            return;
        }
        let (mx, my) = maus::position();
        let menue = KontextMenue::neu(empfaenger, eintraege, 0, 0);
        let breite = menue.puffer.breite as i32;
        let hoehe = menue.puffer.hoehe as i32;
        let mut menue = menue;
        menue.x = mx.clamp(0, self.bildschirm_breite - breite);
        menue.y = my.clamp(0, self.bildschirm_hoehe - hoehe);
        let rect = menue.rechteck();
        self.kontext_menue = Some(menue);
        // Fläche inkl. Schatten-Versatz melden:
        self.dirty_melden(Rechteck::neu(
            rect.x,
            rect.y,
            rect.breite + metrik().abstand,
            rect.hoehe + metrik().abstand,
        ));
    }

    fn kontextmenue_schliessen(&mut self) {
        if let Some(menue) = self.kontext_menue.take() {
            let rect = menue.rechteck();
            self.dirty_melden(Rechteck::neu(
                rect.x,
                rect.y,
                rect.breite + metrik().abstand,
                rect.hoehe + metrik().abstand,
            ));
        }
    }

    /// Klick bei offenem Kontextmenü: Eintrag -> Nachricht an den
    /// Empfänger (App); daneben -> nur schließen.
    fn kontextmenue_klick(&mut self, px: i32, py: i32) -> NachLock {
        let (empfaenger, id) = match &self.kontext_menue {
            Some(menue) => (menue.empfaenger, menue.eintrag_bei(px, py)),
            None => return NachLock::Keine,
        };
        self.kontextmenue_schliessen();
        let id = match id {
            Some(id) => id,
            None => return NachLock::Keine,
        };
        match self.index_von(empfaenger) {
            Some(index) => {
                let app_reaktion = match &mut self.fenster[index].inhalt {
                    Inhalt::App(app_fenster) => app_fenster.app.nachricht(id),
                    _ => return NachLock::Keine,
                };
                self.app_reaktion(index, app_reaktion)
            }
            None => NachLock::Keine,
        }
    }

    // ----- Startmenü -----

    /// Öffnet/schließt das Startmenü (Startknopf oder Super-Taste).
    fn startmenue_umschalten(&mut self) {
        self.start_menue = match self.start_menue {
            Some(_) => None,
            None => Some(StartMenue::neu()),
        };
        // Panel-Fläche + Taskleiste (Startknopf-Highlight):
        let panel = self.menue_dirty_rechteck();
        self.dirty_melden(panel);
        let leiste = self.taskleiste_rechteck();
        self.dirty_melden(leiste);
    }

    fn startmenue_schliessen(&mut self) {
        if self.start_menue.take().is_some() {
            let panel = self.menue_dirty_rechteck();
            self.dirty_melden(panel);
            let leiste = self.taskleiste_rechteck();
            self.dirty_melden(leiste);
        }
    }

    /// Panel-Fläche INKLUSIVE Schatten-Versatz — die Dirty-Einheit
    /// des Startmenüs.
    fn menue_dirty_rechteck(&self) -> Rechteck {
        let panel = self.menue_panel_rechteck();
        Rechteck::neu(
            panel.x,
            panel.y,
            panel.breite + metrik().abstand,
            panel.hoehe + metrik().abstand,
        )
    }

    /// Das Panel-Rechteck des Startmenüs (über dem Startknopf).
    fn menue_panel_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            metrik().abstand,
            self.taskleiste_y() - StartMenue::hoehe() - metrik().abstand,
            StartMenue::BREITE,
            StartMenue::hoehe(),
        )
    }

    /// Verarbeitet eine Widget-Reaktion des Startmenüs: Suche filtert
    /// live, Enter/Eintrag-Klick startet die gewählte App (nach dem
    /// Loslassen des Locks — NachLock).
    fn startmenue_reaktion(&mut self, reaktion: crate::ui::UiReaktion) -> NachLock {
        let menue = match &mut self.start_menue {
            Some(menue) => menue,
            None => return NachLock::Keine,
        };
        match reaktion.nachricht {
            Some(MENUE_SUCHE_GEAENDERT) => {
                menue.filtern();
                menue.zeichnen();
                let panel = self.menue_dirty_rechteck();
                self.dirty_melden(panel);
            }
            Some(MENUE_ENTER) | Some(MENUE_EINTRAG_KLICK) => {
                let start = menue
                    .liste
                    .auswahl
                    .and_then(|zeile| menue.gefiltert.get(zeile))
                    .map(|&index| crate::apps::alle_apps()[index].start);
                if let Some(start) = start {
                    self.startmenue_schliessen();
                    return NachLock::Ausfuehren(start);
                }
            }
            _ => {}
        }
        if reaktion.neu_zeichnen {
            if let Some(menue) = &mut self.start_menue {
                menue.zeichnen();
            }
            let panel = self.menue_dirty_rechteck();
            self.dirty_melden(panel);
        }
        NachLock::Keine
    }

    /// Tastatur im offenen Startmenü (ruft die Shell).
    fn startmenue_taste(&mut self, taste: DecodedKey) -> NachLock {
        let reaktion = match &mut self.start_menue {
            Some(menue) => menue.ereignis(&crate::ui::UiEreignis::Taste(taste)),
            None => return NachLock::Keine,
        };
        self.startmenue_reaktion(reaktion)
    }

    /// Maus im offenen Startmenü: Ereignisse gehen in Panel-
    /// Koordinaten an die Widgets; ein Klick außerhalb schließt
    /// (auch auf dem Startknopf — das ist der Toggle-Effekt).
    fn startmenue_maus(&mut self, event: &MausEvent, px: i32, py: i32) -> NachLock {
        use crate::ui::UiEreignis;

        let panel = self.menue_panel_rechteck();
        let (lx, ly) = (px - panel.x, py - panel.y);
        let ereignis = match event {
            MausEvent::Gedrueckt(MausTaste::Links) if !panel.enthaelt(px, py) => {
                self.startmenue_schliessen();
                return NachLock::Keine;
            }
            MausEvent::Gedrueckt(MausTaste::Links) => UiEreignis::Klick { x: lx, y: ly },
            MausEvent::Losgelassen(MausTaste::Links) => UiEreignis::Losgelassen { x: lx, y: ly },
            MausEvent::Bewegt { .. } => UiEreignis::Bewegt { x: lx, y: ly },
            MausEvent::Gescrollt(delta) => UiEreignis::Scroll { delta: *delta, x: lx, y: ly },
            _ => return NachLock::Keine,
        };
        let reaktion = match &mut self.start_menue {
            Some(menue) => menue.ereignis(&ereignis),
            None => return NachLock::Keine,
        };
        self.startmenue_reaktion(reaktion)
    }

    /// Resize-Kante am Punkt (nur unterhalb der Titelzeile relevant).
    fn kante_bei(fenster: &Fenster, px: i32, py: i32) -> Option<Kante> {
        let r = fenster.gesamt_rechteck();
        let links = px >= r.x && px < r.x + metrik().rand;
        let rechts = px < r.x + r.breite && px >= r.x + r.breite - metrik().rand;
        let unten = py < r.y + r.hoehe && py >= r.y + r.hoehe - metrik().rand;
        match (links, rechts, unten) {
            (true, _, true) => Some(Kante::UntenLinks),
            (_, true, true) => Some(Kante::UntenRechts),
            (_, _, true) => Some(Kante::Unten),
            (true, _, _) => Some(Kante::Links),
            (_, true, _) => Some(Kante::Rechts),
            _ => None,
        }
    }

    /// Setzt die passende Cursor-Form für die Hover-Position.
    fn cursor_aktualisieren(&self, px: i32, py: i32) {
        // Über der Taskleiste gibt es nie Resize-Pfeile:
        if py >= self.taskleiste_y() {
            maus::cursor_form_setzen(maus::FORM_PFEIL);
            return;
        }
        let form = self
            .fenster
            .iter()
            .rev()
            .find(|f| !f.minimiert && f.gesamt_rechteck().enthaelt(px, py))
            .and_then(|f| {
                if f.in_titelzeile(px, py) {
                    None
                } else {
                    Self::kante_bei(f, px, py)
                }
            })
            .map(Kante::cursor_form)
            .unwrap_or(maus::FORM_PFEIL);
        maus::cursor_form_setzen(form);
    }

    /// Liefert NachLock-Arbeit (App-Start aus dem Startmenü, Ui-
    /// Nachricht) — der Aufrufer MUSS sie erst nach dem Loslassen des
    /// MANAGER-Locks ausführen (Deadlock-Regel).
    pub fn maus_event(&mut self, event: &MausEvent, px: i32, py: i32) -> NachLock {
        // Ein offenes Kontextmenü fängt Klicks ab (liegt ganz oben):
        if self.kontext_menue.is_some() {
            match event {
                MausEvent::Gedrueckt(MausTaste::Links) => return self.kontextmenue_klick(px, py),
                MausEvent::Gedrueckt(_) => {
                    self.kontextmenue_schliessen();
                    return NachLock::Keine;
                }
                _ => return NachLock::Keine,
            }
        }
        // Ein offenes Startmenü fängt ALLE Maus-Ereignisse ab
        // (liegt zuoberst) — das Widget-Routing übernimmt den Rest.
        if self.start_menue.is_some() {
            return self.startmenue_maus(event, px, py);
        }
        match event {
            MausEvent::Gedrueckt(MausTaste::Links) => self.maus_gedrueckt(px, py),
            MausEvent::Gedrueckt(MausTaste::Rechts) => self.rechtsklick(px, py),
            MausEvent::Losgelassen(MausTaste::Links) => self.maus_losgelassen(px, py),
            MausEvent::Bewegt { x, y } => {
                self.maus_bewegt(*x, *y);
                // Bewegt erzeugt per Design keine App-Nachrichten
                // (nur Hover-Neuzeichnen) — nichts nachzuarbeiten.
                NachLock::Keine
            }
            MausEvent::Gescrollt(delta) => self.maus_scroll(px, py, *delta),
            _ => NachLock::Keine,
        }
    }

    /// Reicht ein Maus-Ereignis an einen Ui-Fensterinhalt weiter und
    /// übersetzt die Widget-Reaktion in dirty-Flags + NachLock.
    fn ui_maus(&mut self, index: usize, ereignis: crate::ui::UiEreignis) -> NachLock {
        let Fenster { inhalt, puffer, .. } = &mut self.fenster[index];
        let reaktion = match inhalt {
            Inhalt::Ui(ui) => ui.maus(ereignis, puffer),
            Inhalt::App(app_fenster) => app_fenster.ui.maus(ereignis, puffer),
            _ => return NachLock::Keine,
        };
        self.ui_reaktion(index, reaktion)
    }

    /// Verarbeitet die Widget-Reaktion eines Ui-/App-Fensterinhalts:
    /// dirty-Flags setzen und die Nachricht zustellen — bei Ui-
    /// Inhalten nach draußen (fn(u32)-Handler), bei Trait-Apps direkt
    /// an App::nachricht (unter dem Lock, Regeln siehe ui/app.rs).
    fn ui_reaktion(&mut self, index: usize, reaktion: crate::ui::UiReaktion) -> NachLock {
        if reaktion.neu_zeichnen {
            self.fenster[index].inhalt_neu = true;
            self.fenster[index].dirty = true;
        }
        let id = match reaktion.nachricht {
            Some(id) => id,
            None => return NachLock::Keine,
        };
        let app_reaktion = match &mut self.fenster[index].inhalt {
            Inhalt::Ui(ui) => return NachLock::Nachricht(ui.handler(), id),
            Inhalt::App(app_fenster) => app_fenster.app.nachricht(id),
            _ => return NachLock::Keine,
        };
        self.app_reaktion(index, app_reaktion)
    }

    /// Setzt eine AppReaktion um: Baum neu aufbauen, Fenster-Titel
    /// aktualisieren (SpeedText: "name.txt *"), Kontextmenü am Cursor
    /// öffnen, Fenster auf App-Wunsch schließen (Nachfrage-Dialog:
    /// "Verwerfen"), danach-Aktion als NachLock nach draußen.
    fn app_reaktion(&mut self, index: usize, app_reaktion: crate::ui::AppReaktion) -> NachLock {
        if app_reaktion.neu_aufbauen {
            if let Inhalt::App(app_fenster) = &mut self.fenster[index].inhalt {
                app_fenster.neu_aufbauen();
            }
            self.fenster[index].inhalt_neu = true;
            self.fenster[index].dirty = true;
        }
        if let Some(titel) = app_reaktion.titel {
            if self.fenster[index].titel != titel {
                self.fenster[index].titel = titel;
                // Die Titelleiste malt der Compositor — Fläche melden:
                self.fenster[index].dirty = true;
            }
        }
        if let Some(eintraege) = app_reaktion.kontextmenue {
            let fenster_id = self.fenster[index].id;
            self.kontextmenue_oeffnen(fenster_id, eintraege);
        }
        let nach = match app_reaktion.danach {
            Some(aktion) => NachLock::Einmal(aktion),
            None => NachLock::Keine,
        };
        if app_reaktion.schliessen {
            self.fenster_schliessen(index);
        }
        nach
    }

    /// Rechtsklick: fokussiert das Fenster und reicht das Ereignis
    /// (in Inhalts-Koordinaten) an den Ui-/App-Inhalt — Widgets wie
    /// die ScrollListe machen daraus Kontextmenü-Nachrichten.
    fn rechtsklick(&mut self, px: i32, py: i32) -> NachLock {
        if py >= self.taskleiste_y() {
            return NachLock::Keine; // Taskleisten-Kontextmenü: später
        }
        let id = match self.fenster_unter(px, py) {
            Some(id) => id,
            None => return NachLock::Keine, // Desktop-Kontextmenü: später
        };
        self.fokussieren_und_heben(id);
        let index = self.fenster.len() - 1;
        if let Some((lx, ly)) = self.fenster[index].lokal(px, py) {
            return self.ui_maus(index, crate::ui::UiEreignis::Rechtsklick { x: lx, y: ly });
        }
        NachLock::Keine
    }

    /// Scrollrad: geht an den Ui-Inhalt unter dem Cursor.
    fn maus_scroll(&mut self, px: i32, py: i32, delta: i8) -> NachLock {
        if let Some(index) = self
            .fenster_unter(px, py)
            .and_then(|id| self.index_von(id))
        {
            if let Some((lx, ly)) = self.fenster[index].lokal(px, py) {
                return self.ui_maus(index, crate::ui::UiEreignis::Scroll { delta, x: lx, y: ly });
            }
        }
        NachLock::Keine
    }

    fn maus_gedrueckt(&mut self, px: i32, py: i32) -> NachLock {
        // Die Taskleiste liegt IMMER obenauf — sie fängt Klicks vor
        // allen Fenstern ab. (Das Startmenü hat maus_event schon
        // davor abgefangen.)
        if py >= self.taskleiste_y() {
            self.taskleiste_klick(px, py);
            return NachLock::Keine;
        }
        let id = match self.fenster_unter(px, py) {
            Some(id) => id,
            None => return NachLock::Keine,
        };
        self.fokussieren_und_heben(id);
        // "fenster" ist jetzt garantiert das letzte Element.
        let index = self.fenster.len() - 1;

        if self.fenster[index].in_titelzeile(px, py) {
            if let Some(knopf) = self.fenster[index].knopf_bei(px, py) {
                return self.knopf_aktion(id, knopf);
            }
            // Maximiertes Fenster beim Ziehen wiederherstellen:
            if self.fenster[index].vorher.is_some() {
                self.wiederherstellen(id, Some((px, py)));
            }
            let f = &self.fenster[index];
            self.interaktion = Interaktion::Verschieben {
                id,
                griff_dx: px - f.x,
                griff_dy: py - f.y,
            };
        } else if let Some(kante) = Self::kante_bei(&self.fenster[index], px, py) {
            let f = &self.fenster[index];
            self.interaktion = Interaktion::Groesse {
                id,
                kante,
                start_px: px,
                start_py: py,
                start_x: f.x,
                start_y: f.y,
                start_breite: f.breite(),
                start_hoehe: f.hoehe(),
            };
        } else if let Some((lx, ly)) = self.fenster[index].lokal(px, py) {
            // Klick in den Inhalt -> an die "App".
            if let Inhalt::Malflaeche { klicks } = &mut self.fenster[index].inhalt {
                klicks.push((lx, ly));
                let f = &mut self.fenster[index];
                inhalt_zeichnen(f);
                f.dirty = true;
            }
            return self.ui_maus(index, crate::ui::UiEreignis::Klick { x: lx, y: ly });
        }
        NachLock::Keine
    }

    fn maus_losgelassen(&mut self, px: i32, py: i32) -> NachLock {
        // Snap anwenden, wenn während des Ziehens die Vorschau lief —
        // so ist das Ergebnis konsistent mit dem, was der Nutzer sah
        // (unabhängig von der exakten Cursor-Position beim Loslassen).
        if let Interaktion::Verschieben { id, .. } = self.interaktion {
            if self.snap_hinweis != 0 {
                self.snappen(id, self.snap_hinweis);
            }
        }
        let hatte_interaktion = !matches!(self.interaktion, Interaktion::Keine);
        self.interaktion = Interaktion::Keine;
        // Nur nach einem Drag/Resize gibt es etwas aufzuräumen
        // (Snap-Vorschau verschwindet -> Vollbild; ein bloßer Klick
        // meldet seine Flächen über die Fenster-/Ui-Pfade selbst):
        if self.snap_hinweis != 0 {
            self.snap_hinweis = 0;
            self.alles_dirty = true;
        }

        // Loslassen an den Ui-Inhalt unter dem Cursor (Buttons feuern
        // beim LOSLASSEN) — aber nicht nach einem Fenster-Drag/Resize.
        if !hatte_interaktion {
            if let Some(index) = self.fenster_unter(px, py).and_then(|id| self.index_von(id)) {
                if let Some((lx, ly)) = self.fenster[index].lokal(px, py) {
                    return self.ui_maus(index, crate::ui::UiEreignis::Losgelassen { x: lx, y: ly });
                }
            }
        }
        NachLock::Keine
    }

    fn maus_bewegt(&mut self, x: i32, y: i32) {
        match &self.interaktion {
            Interaktion::Verschieben { id, griff_dx, griff_dy } => {
                let (id, dx, dy) = (*id, *griff_dx, *griff_dy);
                if let Some(index) = self.index_von(id) {
                    // Dirty-Rects: ALTE Fläche (Hintergrund wird frei)
                    // und NEUE Fläche melden.
                    let alt = self.fenster_flaeche(index);
                    self.dirty_melden(alt);
                    let bb = self.bildschirm_breite;
                    // Die Titelzeile bleibt immer über der Taskleiste greifbar:
                    let max_y = self.taskleiste_y() - metrik().titel_hoehe;
                    let f = &mut self.fenster[index];
                    f.x = (x - dx).clamp(-(f.breite()) + 80, bb - 80);
                    f.y = (y - dy).clamp(0, max_y);
                    // Snap-Vorschau: Ein WECHSEL der Vorschau braucht
                    // das Vollbild (halbe Fläche erscheint/verschwindet).
                    let hinweis = if x <= metrik().snap_rand {
                        -1
                    } else if x >= bb - metrik().snap_rand {
                        1
                    } else {
                        0
                    };
                    if hinweis != self.snap_hinweis {
                        self.snap_hinweis = hinweis;
                        self.alles_dirty = true;
                    }
                    let neu = self.fenster_flaeche(index);
                    self.dirty_melden(neu);
                }
            }
            Interaktion::Groesse {
                id, kante, start_px, start_py, start_x, start_y, start_breite, start_hoehe,
            } => {
                let (id, kante) = (*id, *kante);
                let dx = x - *start_px;
                let dy = y - *start_py;
                let (mut nx, ny) = (*start_x, *start_y);
                let (mut nb, mut nh) = (*start_breite, *start_hoehe);
                match kante {
                    Kante::Rechts => nb = start_breite + dx,
                    Kante::Unten => nh = start_hoehe + dy,
                    Kante::UntenRechts => {
                        nb = start_breite + dx;
                        nh = start_hoehe + dy;
                    }
                    Kante::Links => {
                        nb = start_breite - dx;
                        nx = start_x + dx;
                    }
                    Kante::UntenLinks => {
                        nb = start_breite - dx;
                        nx = start_x + dx;
                        nh = start_hoehe + dy;
                    }
                }
                // Mindestgröße einhalten (und beim Links-Ziehen x korrigieren):
                if nb < metrik().min_fenster_breite as i32 {
                    if matches!(kante, Kante::Links | Kante::UntenLinks) {
                        nx = start_x + (start_breite - metrik().min_fenster_breite as i32);
                    }
                    nb = metrik().min_fenster_breite as i32;
                }
                nh = nh.max(metrik().min_fenster_hoehe as i32);

                if let Some(index) = self.index_von(id) {
                    let alt = self.fenster_flaeche(index);
                    self.dirty_melden(alt);
                    let f = &mut self.fenster[index];
                    f.x = nx;
                    f.y = ny;
                    f.groesse_setzen(nb as usize, nh as usize);
                    inhalt_zeichnen(f);
                    let neu = self.fenster_flaeche(index);
                    self.dirty_melden(neu);
                }
            }
            Interaktion::Keine => {
                // (Bei offenem Startmenü kommt maus_bewegt gar nicht
                // erst hierher — maus_event fängt alles vorher ab.)
                self.cursor_aktualisieren(x, y);
                self.ui_hover(x, y);
            }
        }
    }

    /// Hover-Routing für Ui-Fenster: Bewegt geht (in Inhalts-
    /// Koordinaten) an das oberste Fenster unter dem Cursor; beim
    /// Fensterwechsel oder Verlassen des Inhalts bekommt das alte
    /// Fenster ein MausRaus — sonst bliebe z. B. ein Button für
    /// immer gehovert.
    fn ui_hover(&mut self, x: i32, y: i32) {
        let ziel = self.fenster_unter(x, y).and_then(|id| {
            let index = self.index_von(id)?;
            self.fenster[index].lokal(x, y).map(|lokal| (id, index, lokal))
        });

        if let Some(alt_id) = self.ui_hover_fenster {
            if ziel.map(|(id, _, _)| id) != Some(alt_id) {
                self.ui_hover_fenster = None;
                if let Some(alt_index) = self.index_von(alt_id) {
                    let _ = self.ui_maus(alt_index, crate::ui::UiEreignis::MausRaus);
                }
            }
        }
        if let Some((id, index, (lx, ly))) = ziel {
            if matches!(self.fenster[index].inhalt, Inhalt::Ui(_)) {
                self.ui_hover_fenster = Some(id);
                let _ = self.ui_maus(index, crate::ui::UiEreignis::Bewegt { x: lx, y: ly });
            }
        }
    }

    fn knopf_aktion(&mut self, id: FensterId, knopf: Knopf) -> NachLock {
        match knopf {
            Knopf::Minimieren => {
                if let Some(index) = self.index_von(id) {
                    self.fenster[index].minimiert = true;
                }
                self.fokus_neu_bestimmen();
                self.alles_dirty = true;
            }
            Knopf::Maximieren => {
                let maximiert = self
                    .index_von(id)
                    .map(|i| self.fenster[i].vorher.is_some())
                    .unwrap_or(false);
                if maximiert {
                    self.wiederherstellen(id, None);
                } else {
                    self.maximieren(id);
                }
            }
            Knopf::Schliessen => {
                if let Some(index) = self.index_von(id) {
                    // Trait-Apps dürfen das Schließen ABFANGEN
                    // (ungespeicherte Änderungen -> Nachfrage-Dialog):
                    // Some(reaktion) = nicht schließen, Reaktion
                    // umsetzen (die App schließt später selbst über
                    // AppReaktion.schliessen).
                    let hook = match &mut self.fenster[index].inhalt {
                        Inhalt::App(app_fenster) => app_fenster.app.schliessen_abfragen(),
                        _ => None,
                    };
                    match hook {
                        Some(reaktion) => return self.app_reaktion(index, reaktion),
                        None => self.fenster_schliessen(index),
                    }
                }
            }
        }
        NachLock::Keine
    }

    /// Schließt ein Fenster ENDGÜLTIG: Terminal-Fenster tragen ihre
    /// Shell-Sitzung aus (der Shell-Task endet beim nächsten
    /// Aufwachen sauber; das Haupt-Terminal vererbt seine Rolle an
    /// das nächste offene Terminal). Fenster + Puffer-Vec werden
    /// gedroppt — der Heap-Speicher geht sauber zurück.
    fn fenster_schliessen(&mut self, index: usize) {
        if let Inhalt::Terminal { sitzung, .. } = self.fenster[index].inhalt {
            crate::shell::sitzung::austragen(sitzung);
            self.fenster.remove(index);
            // Haupt-Terminal geschlossen? Erstes verbliebenes
            // Terminal übernimmt das Kernel-Log.
            if crate::shell::sitzung::haupt() == 0 {
                let nachfolger = self.fenster.iter().find_map(|f| match f.inhalt {
                    Inhalt::Terminal { sitzung, .. } => Some(sitzung),
                    _ => None,
                });
                if let Some(sitzung) = nachfolger {
                    crate::shell::sitzung::haupt_setzen(sitzung);
                }
            }
        } else {
            self.fenster.remove(index);
        }
        self.fokus_neu_bestimmen();
        self.alles_dirty = true;
    }

    fn maximieren(&mut self, id: FensterId) {
        let bb = self.bildschirm_breite;
        let bh = self.bildschirm_hoehe;
        if let Some(index) = self.index_von(id) {
            let f = &mut self.fenster[index];
            f.vorher = Some((f.x, f.y, f.puffer.breite, f.puffer.hoehe));
            f.x = 0;
            f.y = 0;
            let breite = bb.max(metrik().min_fenster_breite as i32) as usize;
            let hoehe = (bh - metrik().titel_hoehe - metrik().taskleiste_hoehe).max(metrik().min_fenster_hoehe as i32) as usize;
            f.groesse_setzen(breite, hoehe);
            inhalt_zeichnen(f);
        }
        self.alles_dirty = true;
    }

    /// Stellt Maximieren/Snap zurück. `unter_cursor`: wenn beim Ziehen,
    /// das Fenster an die Cursor-Position setzen.
    fn wiederherstellen(&mut self, id: FensterId, unter_cursor: Option<(i32, i32)>) {
        if let Some(index) = self.index_von(id) {
            let f = &mut self.fenster[index];
            if let Some((vx, vy, vb, vh)) = f.vorher.take() {
                f.groesse_setzen(vb, vh);
                match unter_cursor {
                    Some((px, py)) => {
                        f.x = px - f.breite() / 2;
                        f.y = (py - metrik().titel_hoehe / 2).max(0);
                    }
                    None => {
                        f.x = vx;
                        f.y = vy;
                    }
                }
                inhalt_zeichnen(f);
            }
        }
        self.alles_dirty = true;
    }

    /// Snap an die linke (-1) oder rechte (+1) Bildschirmhälfte.
    fn snappen(&mut self, id: FensterId, seite: i8) {
        let bb = self.bildschirm_breite;
        let bh = self.bildschirm_hoehe;
        if let Some(index) = self.index_von(id) {
            let f = &mut self.fenster[index];
            if f.vorher.is_none() {
                f.vorher = Some((f.x, f.y, f.puffer.breite, f.puffer.hoehe));
            }
            let breite = (bb / 2).max(metrik().min_fenster_breite as i32) as usize;
            let hoehe = (bh - metrik().titel_hoehe - metrik().taskleiste_hoehe).max(metrik().min_fenster_hoehe as i32) as usize;
            f.x = if seite < 0 { 0 } else { bb / 2 };
            f.y = 0;
            f.groesse_setzen(breite, hoehe);
            inhalt_zeichnen(f);
        }
        self.alles_dirty = true;
    }

    pub fn taste_event(&mut self, taste: DecodedKey) -> NachLock {
        let fokus = match self.fokus {
            Some(id) => id,
            None => return NachLock::Keine,
        };
        if let Some(index) = self.index_von(fokus) {
            // Trait-Apps bekommen die Taste ZUERST angeboten
            // (App-Shortcuts, Eingabemodi wie die Explorer-Adresszeile):
            let hook = match &mut self.fenster[index].inhalt {
                Inhalt::App(app_fenster) => app_fenster.app.taste(taste),
                _ => None,
            };
            if let Some(app_reaktion) = hook {
                return self.app_reaktion(index, app_reaktion);
            }

            // Widget-Fenster: Tab-Fokuskette + Tasten ans fokussierte
            // Widget (macht das UiFenster). (Eigener Block, damit die
            // Fenster-Leihe VOR ui_reaktion endet.)
            let widget_reaktion = {
                let Fenster { inhalt, puffer, .. } = &mut self.fenster[index];
                match inhalt {
                    Inhalt::Ui(ui) => Some(ui.taste(taste, puffer)),
                    Inhalt::App(app_fenster) => Some(app_fenster.ui.taste(taste, puffer)),
                    _ => None,
                }
            };
            if let Some(reaktion) = widget_reaktion {
                return self.ui_reaktion(index, reaktion);
            }

            if let Inhalt::TastaturEcho { text } = &mut self.fenster[index].inhalt {
                match taste {
                    DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                        text.pop();
                    }
                    DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => text.clear(),
                    DecodedKey::Unicode(c) if c >= ' ' => {
                        if text.chars().count() < 40 {
                            text.push(c);
                        }
                    }
                    _ => return NachLock::Keine,
                }
                self.fenster[index].inhalt_neu = true;
                self.fenster[index].dirty = true;
            }
        }
        NachLock::Keine
    }

    // ----- Alt+Tab-Fensterwechsler -----

    fn switcher_weiter(&mut self) {
        match &mut self.switcher {
            Some(sw) => {
                sw.liste.auswahl_bewegen(1, sw.liste_bereich().hoehe);
                sw.zeichnen();
            }
            None => {
                use crate::ui::widgets::{ListenEintrag, ScrollListe};

                // Reihenfolge: oberstes zuerst (MRU), inkl. minimierte.
                let reihenfolge: Vec<FensterId> =
                    self.fenster.iter().rev().map(|f| f.id).collect();
                if reihenfolge.is_empty() {
                    return;
                }
                let eintraege = self
                    .fenster
                    .iter()
                    .rev()
                    .map(|f| ListenEintrag {
                        icon: Some(inhalt_icon(&f.inhalt)),
                        text: if f.minimiert {
                            format!("{} (minimiert)", f.titel)
                        } else {
                            f.titel.clone()
                        },
                    })
                    .collect();
                let liste = ScrollListe::neu(eintraege, 0, 0);
                // Höhe: bis zu 8 Zeilen sichtbar, Rest scrollt.
                let zeilen = (reihenfolge.len() as i32).min(8);
                let hoehe = (zeilen * metrik().listen_eintrag_hoehe + 2 * metrik().abstand) as usize;
                let mut sw = Switcher {
                    reihenfolge,
                    liste,
                    puffer: FensterPuffer::neu(420, hoehe, theme::aktuell().inhalt_hintergrund),
                };
                // Erster Tab wählt das NÄCHSTE Fenster (Index 1).
                if sw.reihenfolge.len() > 1 {
                    sw.liste.auswahl_bewegen(1, sw.liste_bereich().hoehe);
                }
                sw.zeichnen();
                self.switcher = Some(sw);
            }
        }
        let overlay = self.switcher_dirty_rechteck();
        self.dirty_melden(overlay);
    }

    /// Die Bildschirm-Fläche des Alt+Tab-Overlays (inkl. Titelband
    /// und Schatten) — die Dirty-Einheit des Switchers.
    fn switcher_dirty_rechteck(&self) -> Rechteck {
        match &self.switcher {
            Some(sw) => {
                let breite = sw.puffer.breite as i32;
                let hoehe = sw.puffer.hoehe as i32 + 36;
                Rechteck::neu(
                    (self.bildschirm_breite - breite) / 2,
                    (self.bildschirm_hoehe - hoehe) / 2,
                    breite + metrik().abstand,
                    hoehe + metrik().abstand,
                )
            }
            None => Rechteck::neu(0, 0, 0, 0),
        }
    }

    fn switcher_bestaetigen(&mut self) {
        // Overlay-Fläche VOR dem Schließen merken (danach ist der
        // Switcher weg und die Fläche muss restauriert werden):
        let overlay = self.switcher_dirty_rechteck();
        if let Some(sw) = self.switcher.take() {
            self.dirty_melden(overlay);
            let auswahl = sw.liste.auswahl.unwrap_or(0);
            if let Some(&id) = sw.reihenfolge.get(auswahl) {
                if let Some(index) = self.index_von(id) {
                    // Ein zurückgeholtes minimiertes Fenster taucht
                    // neu auf — seine Fläche melden:
                    self.fenster[index].minimiert = false;
                    let flaeche = self.fenster_flaeche(index);
                    self.dirty_melden(flaeche);
                }
                self.fokussieren_und_heben(id);
            }
        }
    }

    pub fn uhr_aktualisieren(&mut self) {
        for fenster in self.fenster.iter_mut() {
            if !fenster.minimiert && matches!(fenster.inhalt, Inhalt::Uhr) {
                inhalt_zeichnen(fenster);
                fenster.dirty = true;
            }
            // Widget-Fenster: Cursor-Blinken (fokussiertes Textfeld)
            // und der Tick von Trait-Apps (Live-Inhalte).
            let minimiert = fenster.minimiert;
            let Fenster { inhalt, dirty, inhalt_neu, .. } = fenster;
            match inhalt {
                Inhalt::Ui(ui) if !minimiert && ui.blinkt() => {
                    *inhalt_neu = true;
                    *dirty = true;
                }
                Inhalt::App(app_fenster) if !minimiert => {
                    if app_fenster.app.tick() {
                        app_fenster.neu_aufbauen();
                        *inhalt_neu = true;
                        *dirty = true;
                    } else if app_fenster.ui.blinkt() {
                        *inhalt_neu = true;
                        *dirty = true;
                    }
                }
                _ => {}
            }
        }
        // Taskleisten-Uhr: nur beim Sekundenwechsel — und dann NUR
        // die Systray-Ecke (das Ziel des Dirty-Rect-Umbaus: ein
        // Uhr-Tick darf keinen Vollbild-Frame mehr kosten).
        let sekunde = zeit::ms_seit_boot() / 1000;
        if sekunde != self.letzte_uhr_sekunde {
            self.letzte_uhr_sekunde = sekunde;
            let systray = self.systray_rechteck();
            self.dirty_melden(systray);
        }
    }

    // ----- Test-Hilfen -----

    pub fn fenster_lokal(&self, id: FensterId, px: i32, py: i32) -> Option<(i32, i32)> {
        self.index_von(id).and_then(|i| self.fenster[i].lokal(px, py))
    }
    pub fn fenster_position(&self, id: FensterId) -> Option<(i32, i32)> {
        self.index_von(id).map(|i| (self.fenster[i].x, self.fenster[i].y))
    }
    pub fn fokus(&self) -> Option<FensterId> {
        self.fokus
    }

    // ----- Compositing -----

    /// Komponiert NUR die übergebenen Rechtecke (aus dirty_abholen):
    /// Pro Rect wird der Zeichner-Clip gesetzt — die zeilenweisen
    /// Schnellpfade clippen vorab, Fenster ohne Schnitt werden ganz
    /// übersprungen. Ein Uhr-Tick komponiert so nur die Systray-Ecke.
    fn komponieren(&self, fb: &mut framebuffer::DoppelPuffer, rects: &[Rechteck]) {
        for rect in rects {
            self.komponieren_bereich(fb, *rect);
        }
    }

    fn komponieren_bereich(&self, fb: &mut framebuffer::DoppelPuffer, rect: Rechteck) {
        let thema = theme::aktuell();

        // 1. Desktop-Hintergrund: aus dem byte-identischen Cache des
        // DoppelPuffers wiederherstellen — ein memcpy pro Zeile,
        // statt den Verlauf neu zu rechnen.
        fb.hintergrund_wiederherstellen(
            rect.x as usize,
            rect.y as usize,
            rect.breite as usize,
            rect.hoehe as usize,
        );

        let mut z = Zeichner::neu(fb);
        z.clip_setzen(Some(rect));

        // 2. Snap-Vorschau (halbtransparente Hälfte):
        if self.snap_hinweis != 0 {
            let halb = self.bildschirm_breite / 2;
            let x = if self.snap_hinweis < 0 { 0 } else { halb };
            let akzent = thema.akzent;
            z.rechteck_fuellen(
                Rechteck::neu(x, 0, halb, self.bildschirm_hoehe - metrik().taskleiste_hoehe),
                Rgba::mit_alpha(akzent.r, akzent.g, akzent.b, 60),
            );
        }

        // 3. Fenster von hinten nach vorne — nur die, deren Fläche
        // das Rect überhaupt schneidet (minimierte überspringen):
        for (index, fenster) in self.fenster.iter().enumerate() {
            if fenster.minimiert || self.fenster_flaeche(index).schneiden(&rect).is_none() {
                continue;
            }
            let fokussiert = self.fokus == Some(fenster.id);
            fenster_komponieren(&mut z, fenster, fokussiert);
        }

        // 4. Taskleiste — IMMER im Vordergrund, deshalb NACH den
        // Fenstern (nur wenn das Rect sie berührt):
        if rect.y + rect.hoehe > self.taskleiste_y() {
            self.taskleiste_zeichnen(&mut z);
        }

        // 5. Startmenü (über der Taskleiste): Der Widget-Verbund hat
        // sich in seinen Offscreen-Puffer gezeichnet — hier nur noch
        // Schatten, Blit und Akzent-Rahmen.
        if let Some(menue) = &self.start_menue {
            let panel = self.menue_panel_rechteck();
            z.rechteck_fuellen(
                Rechteck::neu(panel.x + metrik().abstand, panel.y + metrik().abstand, panel.breite, panel.hoehe),
                thema.schatten,
            );
            z.puffer_blit(panel.x, panel.y, menue.puffer.breite, &menue.puffer.pixel);
            z.rechteck_rahmen(panel, thema.akzent);
        }

        // 5b. Kontextmenü (Rechtsklick-Overlay, über allem außer Alt+Tab):
        if let Some(menue) = &self.kontext_menue {
            let panel = menue.rechteck();
            z.rechteck_fuellen(
                Rechteck::neu(panel.x + metrik().abstand / 2, panel.y + metrik().abstand / 2, panel.breite, panel.hoehe),
                thema.schatten,
            );
            z.puffer_blit(panel.x, panel.y, menue.puffer.breite, &menue.puffer.pixel);
            z.rechteck_rahmen(panel, thema.akzent);
        }

        // 6. Alt+Tab-Overlay (ganz oben): Titelband + Listen-Blit.
        if let Some(sw) = &self.switcher {
            let breite = sw.puffer.breite as i32;
            let hoehe = sw.puffer.hoehe as i32 + 36; // Titelband oben
            let bx = (self.bildschirm_breite - breite) / 2;
            let by = (self.bildschirm_hoehe - hoehe) / 2;
            z.rechteck_fuellen(
                Rechteck::neu(bx + metrik().abstand, by + metrik().abstand, breite, hoehe),
                thema.schatten,
            );
            z.rechteck_fuellen(Rechteck::neu(bx, by, breite, 36), thema.flaeche);
            z.text(bx + 14, by + 10, "Fenster wechseln", metrik().schrift_ui, FontWeight::Bold, thema.text_normal);
            z.puffer_blit(bx, by + 36, sw.puffer.breite, &sw.puffer.pixel);
            z.rechteck_rahmen(Rechteck::neu(bx, by, breite, hoehe), thema.akzent);
        }
    }

    /// Zeichnet die Taskleiste: Startknopf | Fenster-Knöpfe | Systray
    /// (Platzhalter-Icons + Uhrzeit/Datum).
    fn taskleiste_zeichnen<F: Zeichenflaeche>(&self, z: &mut Zeichner<'_, F>) {
        let thema = theme::aktuell();
        let y = self.taskleiste_y();
        let breite = self.bildschirm_breite;
        let zeichen_breite = get_raster_width(FontWeight::Regular, metrik().schrift_ui) as i32;

        // Leisten-Grund (leicht transparent — der Desktop schimmert durch):
        z.rechteck_fuellen(
            Rechteck::neu(0, y, breite, metrik().taskleiste_hoehe),
            thema.leiste_hintergrund,
        );
        z.linie(0, y, breite - 1, y, thema.leiste_linie);

        // Startknopf: das SpeedOS-Logo, mit der Leiste skaliert
        // (40px-Leiste -> 2x = 32px, 80px-Leiste -> 4x = 64px);
        // bei offenem Startmenü hervorgehoben.
        let start = self.start_knopf_rechteck();
        if self.start_menue.is_some() {
            z.rechteck_abgerundet(
                Rechteck::neu(start.x + 4, start.y + 3, start.breite - 8, start.hoehe - 6),
                metrik().radius_klein,
                thema.leiste_knopf_aktiv,
            );
        }
        let logo_skala = (metrik().taskleiste_hoehe / 20).max(1);
        z.icon(
            start.x + (start.breite - 16 * logo_skala) / 2,
            y + (metrik().taskleiste_hoehe - 16 * logo_skala) / 2,
            &crate::grafik::ICON_LOGO,
            logo_skala,
        );

        // Ein Knopf pro offenem Fenster:
        for (id, rect) in self.taskleisten_knoepfe() {
            let fenster = match self.index_von(id) {
                Some(index) => &self.fenster[index],
                None => continue,
            };
            let aktiv = self.fokus == Some(id) && !fenster.minimiert;
            z.rechteck_abgerundet(
                rect,
                metrik().radius_klein,
                if aktiv { thema.leiste_knopf_aktiv } else { thema.leiste_knopf },
            );
            if aktiv {
                // Akzent-Streifen unter dem fokussierten Fenster:
                z.rechteck_fuellen(
                    Rechteck::neu(rect.x + 6, rect.y + rect.hoehe - 3, rect.breite - 12, 2),
                    thema.akzent,
                );
            }
            let text_y = rect.y + (rect.hoehe - metrik().zeilen_hoehe) / 2;
            z.icon(rect.x + 6, text_y, inhalt_icon(&fenster.inhalt), 1);
            // Titel auf die Knopfbreite kürzen:
            let platz = ((rect.breite - 34) / zeichen_breite).max(0) as usize;
            let titel: String = fenster.titel.chars().take(platz).collect();
            let farbe = if fenster.minimiert {
                thema.text_gedimmt
            } else if aktiv {
                thema.text_stark
            } else {
                thema.text_sekundaer
            };
            z.text(rect.x + 28, text_y, &titel, metrik().schrift_ui, FontWeight::Regular, farbe);
        }

        // Systray rechts: Platzhalter-Icons (echte Features folgen),
        // daneben Uhrzeit über Datum — als Block vertikal zentriert
        // (skalierte Zeilenhöhen, keine festen Offsets!).
        let systray_x = breite - metrik().systray_breite;
        let block_y = y + (metrik().taskleiste_hoehe - 2 * metrik().zeilen_hoehe) / 2;
        z.icon(systray_x, y + (metrik().taskleiste_hoehe - 16) / 2, &crate::grafik::ICON_ZAHNRAD, 1);
        z.icon(systray_x + 22, y + (metrik().taskleiste_hoehe - 16) / 2, &crate::grafik::ICON_ORDNER, 1);

        // Zeit + Format kommen aus den Einstellungen (UTC-Offset,
        // 12/24h) — einstellungen ist ein Blatt-Lock, hier erlaubt.
        let jetzt = crate::einstellungen::jetzt_lokal();
        let uhr = crate::einstellungen::uhrzeit_text(&jetzt);
        let datum = format!("{:02}.{:02}.{}", jetzt.tag, jetzt.monat, jetzt.jahr);
        let uhr_x = breite - metrik().abstand - uhr.chars().count() as i32 * zeichen_breite;
        let datum_x = breite - metrik().abstand - datum.chars().count() as i32 * zeichen_breite;
        z.text(uhr_x, block_y, &uhr, metrik().schrift_ui, FontWeight::Bold, thema.text_normal);
        z.text(
            datum_x,
            block_y + metrik().zeilen_hoehe,
            &datum,
            metrik().schrift_ui,
            FontWeight::Regular,
            thema.text_gedimmt,
        );
    }

}

/// Zeichnet EIN Fenster (Schatten, Titelleiste + Knöpfe, Rahmen, Inhalt).
fn fenster_komponieren<F: Zeichenflaeche>(z: &mut Zeichner<'_, F>, fenster: &Fenster, fokussiert: bool) {
    let thema = theme::aktuell();
    let rect = fenster.gesamt_rechteck();

    // Schatten (Alpha) rechts und unten:
    z.rechteck_fuellen(Rechteck::neu(rect.x + rect.breite, rect.y + 10, 10, rect.hoehe), thema.schatten);
    z.rechteck_fuellen(Rechteck::neu(rect.x + 10, rect.y + rect.hoehe, rect.breite, 10), thema.schatten);

    // Titelleiste: fokussiert = Aurora-Verlauf, sonst gedimmt.
    let titel_rect = Rechteck::neu(rect.x, rect.y, rect.breite, metrik().titel_hoehe);
    if fokussiert {
        z.verlauf_vertikal(titel_rect, thema.titel_aktiv_oben, thema.titel_aktiv_unten);
    } else {
        z.rechteck_fuellen(titel_rect, thema.titel_passiv);
    }

    // Fenster-Icon links (passend zum Inhalt):
    z.icon(rect.x + 7, rect.y + 7, inhalt_icon(&fenster.inhalt), 1);
    // Titel-Text:
    z.text(
        rect.x + 30,
        rect.y + 7,
        &fenster.titel,
        metrik().schrift_ui,
        FontWeight::Bold,
        if fokussiert { thema.text_titel_aktiv } else { thema.text_titel_passiv },
    );

    // Die drei Knöpfe:
    for (knopf, r) in fenster.knoepfe() {
        let symbol = match knopf {
            Knopf::Schliessen => thema.knopf_schliessen,
            _ => if fokussiert { thema.knopf_symbol_aktiv } else { thema.knopf_symbol_passiv },
        };
        let cx = r.x + r.breite / 2;
        let cy = r.y + r.hoehe / 2;
        match knopf {
            Knopf::Minimieren => z.linie(cx - 5, cy + 4, cx + 5, cy + 4, symbol),
            Knopf::Maximieren => {
                if fenster.vorher.is_some() {
                    // "Wiederherstellen": zwei versetzte Rahmen.
                    z.rechteck_rahmen(Rechteck::neu(cx - 3, cy - 5, 8, 8), symbol);
                    z.rechteck_rahmen(Rechteck::neu(cx - 5, cy - 3, 8, 8), symbol);
                } else {
                    z.rechteck_rahmen(Rechteck::neu(cx - 5, cy - 5, 10, 10), symbol);
                }
            }
            Knopf::Schliessen => {
                z.linie(cx - 5, cy - 5, cx + 5, cy + 5, symbol);
                z.linie(cx + 5, cy - 5, cx - 5, cy + 5, symbol);
            }
        }
    }

    // Rahmen:
    z.rechteck_rahmen(
        rect,
        if fokussiert { thema.rahmen_aktiv } else { thema.rahmen_passiv },
    );

    // Inhalt (privater Puffer, 1:1) — über den Blit-Schnellpfad,
    // nicht Pixel für Pixel (Performance-Pass: ~2x schnellere Frames):
    z.puffer_blit(
        rect.x,
        rect.y + metrik().titel_hoehe,
        fenster.puffer.breite,
        &fenster.puffer.pixel,
    );
}

/// Das Icon, das zu einem Fenster-Inhalt gehört (Titelleiste,
/// Taskleiste und Alt+Tab zeigen dasselbe).
fn inhalt_icon(inhalt: &Inhalt) -> &'static crate::grafik::Icon {
    match inhalt {
        Inhalt::Terminal { .. } => &crate::grafik::ICON_TERMINAL,
        Inhalt::Ui(ui) => ui.icon,
        Inhalt::App(app_fenster) => app_fenster.ui.icon,
        Inhalt::Uhr => &crate::grafik::ICON_UHR,
        Inhalt::TastaturEcho { .. } => &crate::grafik::ICON_TASTATUR,
        Inhalt::Malflaeche { .. } => &crate::grafik::ICON_PINSEL,
    }
}

/// Zeichnet das Terminal-Raster in den Fenster-Puffer: Zellen-
/// Hintergründe, Zeichen (Antialiasing via Alpha) und der Cursor-
/// Unterstrich in Akzentfarbe.
/// SEIT DEM SERIE-3-PERFORMANCE-PASS: rendert NUR die geänderten
/// Rasterzeilen (term.dirty_abholen) — der Fenster-Puffer ist
/// persistent, der Rest steht schon drin. Eine Prompt-Ausgabe malt
/// so eine Zeile statt 24. Volles Neuzeichnen (Theme/Resize) läuft
/// über term.alles_markieren() davor.
fn terminal_rendern(term: &mut terminal::Terminal, puffer: &mut FensterPuffer) {
    let (dirty_von, dirty_bis) = match term.dirty_abholen() {
        Some(bereich) => bereich,
        None => return,
    };
    let thema = theme::aktuell();
    let hintergrund = thema.terminal_hintergrund;
    let zeichen_breite = get_raster_width(FontWeight::Regular, metrik().schrift_ui) as i32;
    let zeilen_hoehe = metrik().zeilen_hoehe;
    let breite = puffer.breite as i32;
    // Unter der letzten Rasterzeile bleibt ein Reststreifen (Fenster-
    // höhe ist kein Vielfaches der Zeilenhöhe) — mitfüllen, wenn die
    // letzte Zeile dirty ist:
    let flaechen_hoehe = puffer.hoehe as i32;
    let streifen_bis = if dirty_bis == term.zeilen() {
        flaechen_hoehe
    } else {
        dirty_bis as i32 * zeilen_hoehe
    };

    let mut z = Zeichner::neu(puffer);
    z.rechteck_fuellen(
        Rechteck::neu(
            0,
            dirty_von as i32 * zeilen_hoehe,
            breite,
            streifen_bis - dirty_von as i32 * zeilen_hoehe,
        ),
        Rgba::neu(hintergrund.r, hintergrund.g, hintergrund.b),
    );
    let mut puffer_utf8 = [0u8; 4];
    for zeile in dirty_von..dirty_bis {
        for spalte in 0..term.spalten() {
            let zelle = term.zelle(spalte, zeile);
            let x = spalte as i32 * zeichen_breite;
            let y = zeile as i32 * zeilen_hoehe;
            if zelle.hg != hintergrund {
                z.rechteck_fuellen(
                    Rechteck::neu(x, y, zeichen_breite, zeilen_hoehe),
                    Rgba::neu(zelle.hg.r, zelle.hg.g, zelle.hg.b),
                );
            }
            if zelle.zeichen != ' ' {
                z.text(
                    x,
                    y,
                    zelle.zeichen.encode_utf8(&mut puffer_utf8),
                    metrik().schrift_ui,
                    FontWeight::Regular,
                    Rgba::neu(zelle.vg.r, zelle.vg.g, zelle.vg.b),
                );
            }
        }
    }
    // Der Terminal-Cursor (ruhig, nicht blinkend — der Konsolen-
    // Blink-Task ist im Desktop-Modus pausiert). Nur zeichnen, wenn
    // seine Zeile im gerenderten Streifen liegt — sonst steht er
    // dort unverändert aus dem letzten Rendern.
    let (cursor_spalte, cursor_zeile) = term.cursor();
    if (dirty_von..dirty_bis).contains(&cursor_zeile) {
        z.rechteck_fuellen(
            Rechteck::neu(
                cursor_spalte as i32 * zeichen_breite,
                cursor_zeile as i32 * zeilen_hoehe + zeilen_hoehe - 2,
                zeichen_breite,
                2,
            ),
            thema.akzent,
        );
    }
}

/// Zeichnet den Inhalt eines Fensters in SEINEN Puffer.
fn inhalt_zeichnen(fenster: &mut Fenster) {
    // Widget-Fenster: Der Baum zeichnet sich selbst (ui-Modul).
    if let Inhalt::Ui(ui) = &fenster.inhalt {
        ui.zeichnen(&mut fenster.puffer);
        return;
    }
    if let Inhalt::App(app_fenster) = &fenster.inhalt {
        app_fenster.ui.zeichnen(&mut fenster.puffer);
        return;
    }
    // Terminal: Rastergröße an die Fenstergröße anpassen, dann rendern.
    // (&mut fenster.inhalt und &mut fenster.puffer sind verschiedene
    // Felder — der Borrow-Checker erlaubt beides gleichzeitig.)
    if let Inhalt::Terminal { term, .. } = &mut fenster.inhalt {
        let zeichen_breite = get_raster_width(FontWeight::Regular, metrik().schrift_ui);
        let spalten = (fenster.puffer.breite / zeichen_breite).max(1);
        let zeilen = (fenster.puffer.hoehe / metrik().zeilen_hoehe as usize).max(1);
        term.groesse_setzen(spalten, zeilen);
        // Dieser Pfad ist das VOLLE Neuzeichnen (Theme-/Skalierungs-
        // Wechsel, alles_neu_zeichnen) — den Frame-Pfad mit nur den
        // geänderten Zeilen geht inhalte_rendern direkt.
        term.alles_markieren();
        terminal_rendern(term, &mut fenster.puffer);
        return;
    }

    let thema = theme::aktuell();
    let breite = fenster.puffer.breite as i32;
    let hoehe = fenster.puffer.hoehe as i32;
    let mut z = Zeichner::neu(&mut fenster.puffer);
    z.rechteck_fuellen(
        Rechteck::neu(0, 0, breite, hoehe),
        Rgba::neu(thema.inhalt_hintergrund.r, thema.inhalt_hintergrund.g, thema.inhalt_hintergrund.b),
    );

    match &fenster.inhalt {
        // Oben schon behandelt (frühe returns):
        Inhalt::Terminal { .. } | Inhalt::Ui(_) | Inhalt::App(_) => {}
        Inhalt::Uhr => {
            let ticks = zeit::ticks();
            let ms = zeit::ms_seit_boot();
            z.text(20, 16, &format!("{} Ticks", ticks), metrik().schrift_gross, FontWeight::Bold, thema.akzent_cyan);
            z.text(20, 60, &format!("Uptime: {},{:03} s", ms / 1000, ms % 1000), metrik().schrift_ui, FontWeight::Regular, thema.text_normal);
            z.text(20, 90, "(aktualisiert sich live)", metrik().schrift_ui, FontWeight::Regular, thema.text_gedimmt);
        }
        Inhalt::TastaturEcho { text } => {
            z.text(20, 12, "Tippe (bei Fokus!):", metrik().schrift_ui, FontWeight::Regular, thema.text_sekundaer);
            z.rechteck_abgerundet(Rechteck::neu(16, 40, breite - 32, 40), metrik().radius_klein, thema.eingabefeld);
            z.text(26, 50, text, metrik().schrift_ui, FontWeight::Regular, thema.text_stark);
            z.text(20, 96, "Enter leert, Backspace loescht", metrik().schrift_ui, FontWeight::Regular, thema.text_gedimmt);
        }
        Inhalt::Malflaeche { klicks } => {
            z.verlauf_vertikal(
                Rechteck::neu(0, 0, breite, 60),
                thema.titel_aktiv_oben.mischen(thema.inhalt_hintergrund, 128),
                thema.inhalt_hintergrund,
            );
            z.text(16, 8, "Statische Grafik + Klicks", metrik().schrift_ui, FontWeight::Bold, thema.text_stark);
            let (akzent, cyan) = (thema.akzent, thema.akzent_cyan);
            z.kreis_fuellen(50, 110, 28, Rgba::mit_alpha(akzent.r, akzent.g, akzent.b, 180));
            z.kreis_fuellen(90, 110, 28, Rgba::mit_alpha(cyan.r, cyan.g, cyan.b, 180));
            z.rechteck_abgerundet(Rechteck::neu(140, 84, 90, 52), 10, thema.akzent_gruen);
            z.icon(breite - 50, 76, &crate::grafik::ICON_LOGO, 2);
            for (kx, ky) in klicks.iter() {
                z.linie(kx - 6, *ky, kx + 6, *ky, thema.akzent_gelb);
                z.linie(*kx, ky - 6, *kx, ky + 6, thema.akzent_gelb);
                z.kreis_rahmen(*kx, *ky, 6, thema.akzent_gelb);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Globaler Zugriff + Desktop-Modus
// ---------------------------------------------------------------------------

static MANAGER: Mutex<Option<FensterManager>> = Mutex::new(None);
static DESKTOP_AKTIV: AtomicBool = AtomicBool::new(false);

pub fn desktop_aktiv() -> bool {
    DESKTOP_AKTIV.load(Ordering::Relaxed)
}

fn mit_manager<T>(f: impl FnOnce(&mut FensterManager) -> T) -> Option<T> {
    x86_64::instructions::interrupts::without_interrupts(|| MANAGER.lock().as_mut().map(f))
}

pub fn desktop_starten() {
    let info = match framebuffer::mit_framebuffer(|fb| fb.info()) {
        Some(info) => info,
        None => return,
    };

    let erster_start =
        x86_64::instructions::interrupts::without_interrupts(|| MANAGER.lock().is_none());
    if erster_start {
        // UI-Skalierung: Hat der Nutzer sie in den Einstellungen
        // gewählt, gilt der GESPEICHERTE Wert — sonst die Auto-Wahl
        // nach Auflösung (ab 2560 breit 1.5, ab 3840 2.0; sonst wäre
        // die 16-px-Schrift bei 4K winzig).
        match crate::einstellungen::hole_opt(crate::einstellungen::S_SKALA) {
            Some(wert) => {
                crate::theme::skala_setzen_halbe(wert.parse().unwrap_or(2));
            }
            None => crate::theme::skala_setzen_nach_breite(info.width),
        }

        // Heap passend zur Auflösung wachsen lassen: maximierte
        // Fenster-Puffer (~3x Breite*Höhe*3 Bytes) PLUS der
        // Hintergrund-Cache des Dirty-Rect-Compositings (1x).
        let noetig_bytes = info.width * info.height * 3 * 4;
        let noetig_pages = noetig_bytes.div_ceil(4096);
        let _ = crate::allocator::heap_erweitern(noetig_pages);

        // Der Desktop startet mit EINEM offenen Terminal-Fenster —
        // die SpeedShell läuft darin weiter. Alles andere (Uhr,
        // Malkasten, ...) öffnet man über das Startmenü.
        let mut manager = FensterManager::neu(info.width as i32, info.height as i32);
        manager.terminal_oeffnen();
        x86_64::instructions::interrupts::without_interrupts(|| {
            *MANAGER.lock() = Some(manager);
        });
    } else {
        let _ = mit_manager(|m| m.alles_dirty = true);
    }

    crate::konsole::cursor_pausieren();
    DESKTOP_AKTIV.store(true, Ordering::Relaxed);
}

pub fn desktop_beenden() {
    DESKTOP_AKTIV.store(false, Ordering::Relaxed);
    maus::cursor_form_setzen(maus::FORM_PFEIL);
}

pub fn maus_event(event: &MausEvent) {
    if !desktop_aktiv() {
        return;
    }
    let (px, py) = maus::position();
    // App-Starts und Ui-Nachrichten werden ERST HIER ausgeführt —
    // nach dem Loslassen des MANAGER-Locks (Deadlock-Regel: Starts
    // nehmen den Lock selbst, Handler drucken womöglich).
    let nach = mit_manager(|m| m.maus_event(event, px, py)).unwrap_or(NachLock::Keine);
    nach_lock_ausfuehren(nach);
}

pub fn taste_event(taste: DecodedKey) {
    let nach = mit_manager(|m| m.taste_event(taste)).unwrap_or(NachLock::Keine);
    nach_lock_ausfuehren(nach);
}

// ----- Startmenü (Startknopf in der Taskleiste oder Super-Taste) -----

pub fn startmenue_offen() -> bool {
    mit_manager(|m| m.start_menue.is_some()).unwrap_or(false)
}

/// Öffnet/schließt das Startmenü (ruft der KeyStream bei Super).
pub fn startmenue_umschalten() {
    let _ = mit_manager(|m| m.startmenue_umschalten());
}

pub fn startmenue_schliessen() {
    let _ = mit_manager(|m| m.startmenue_schliessen());
}

/// Taste ins offene Startmenü (ruft die Shell). Startet die gewählte
/// App — wie bei maus_event erst NACH dem Loslassen des Locks.
pub fn startmenue_taste(taste: DecodedKey) {
    let nach = mit_manager(|m| m.startmenue_taste(taste)).unwrap_or(NachLock::Keine);
    nach_lock_ausfuehren(nach);
}

// ----- Terminal (SpeedShell-Sitzungen als Fenster) -----

/// Öffnet ein NEUES Terminal-Fenster mit eigener Shell-Sitzung und
/// liefert deren Id (None = Desktop läuft nicht). Der Aufrufer
/// spawnt dann den Shell-Task (shell::sitzung_laufen) — siehe
/// apps::terminal_starten.
pub fn terminal_oeffnen() -> Option<u64> {
    mit_manager(|m| m.terminal_oeffnen())
}

/// Die Sitzungs-Id des fokussierten Terminal-Fensters (None = kein
/// Terminal fokussiert). Der Eingabe-Router wirft die Tasten dann in
/// genau diese Sitzungs-Queue.
pub fn terminal_fokus_sitzung() -> Option<u64> {
    mit_manager(|m| m.fokus_terminal_sitzung()).flatten()
}

/// Umleitung von konsole::_print im Desktop-Modus: formatierten Text
/// ins Terminal-Fenster der SITZUNG schreiben. false = kein Terminal
/// dieser Sitzung offen (Aufrufer puffert Kernel-Log dann selbst).
pub fn terminal_schreiben(sitzung: u64, args: core::fmt::Arguments, vg: Farbe, hg: Farbe) -> bool {
    mit_manager(|m| m.terminal_schreiben(sitzung, args, vg, hg)).unwrap_or(false)
}

/// Leert das Terminal der Sitzung (clear-Befehl im Desktop-Modus).
pub fn terminal_leeren(sitzung: u64) {
    let _ = mit_manager(|m| m.terminal_leeren(sitzung));
}

// ----- Schnittstelle für die App-Registry (apps.rs) -----

/// Öffnet ein neues App-Fenster — leicht versetzt (Kaskade), damit
/// mehrere Fenster nicht exakt übereinander liegen.
pub fn app_fenster_oeffnen(titel: &str, breite: usize, hoehe: usize, inhalt: Inhalt) {
    let _ = mit_manager(|m| {
        let versatz = (m.fenster.len() as i32 % 5) * 40;
        m.fenster_erstellen(titel, 120 + versatz, 90 + versatz, breite, hoehe, inhalt);
    });
}

/// Öffnet eine Trait-App (ui::App) als Fenster — die Brücke der
/// App-Registry zum App-Trait: Titel und Icon liefert die App selbst.
pub fn app_starten(app: alloc::boxed::Box<dyn crate::ui::App>, breite: usize, hoehe: usize) {
    let app_fenster = crate::ui::AppFenster::neu(app);
    let titel = app_fenster.app.fenster_titel();
    app_fenster_oeffnen(&titel, breite, hoehe, Inhalt::App(app_fenster));
}

/// Zeichnet ALLE Fenster-Inhalte neu, erneuert den Hintergrund-Cache
/// und komponiert Vollbild — DIE Aufräum-Funktion nach jeder Optik-
/// Änderung (Theme, Akzentfarbe, Hintergrund-Preset, Skalierung).
/// Nimmt den MANAGER-Lock: nie unter gehaltenen Locks rufen
/// (aus Apps immer über AppReaktion.danach).
pub fn alles_neu_zeichnen() {
    let _ = mit_manager(|m| {
        m.hintergrund_neu = true; // Verlauf-Cache invalidieren
        for index in 0..m.fenster.len() {
            inhalt_zeichnen(&mut m.fenster[index]);
        }
        m.alles_dirty = true;
    });
}

/// Wechselt das Theme, merkt die Wahl in den Einstellungen und
/// zeichnet ALLE Fenster neu (Inhalte nutzen Theme-Farben, deshalb
/// reicht alles_dirty allein nicht — und der Hintergrund-Cache trägt
/// die Theme-Farben).
pub fn theme_wechseln() {
    crate::theme::umschalten();
    crate::einstellungen::setze_bool(
        crate::einstellungen::S_THEME_HELL,
        crate::theme::hell_aktiv(),
    );
    alles_neu_zeichnen();
}

/// Schaltet die UI-Skalierung zyklisch weiter (1.0 -> 1.5 -> 2.0)
/// und zeichnet alles neu — dieselbe Mechanik wie der Theme-Wechsel.
/// Terminal-Raster und Widget-Layouts passen sich beim Neu-Rendern
/// automatisch an die neue metrik() an.
/// (Die Einstellungen-App setzt die Skala DIREKT über
/// theme::skala_setzen_halbe + alles_neu_zeichnen — siehe dort.)
pub fn skalierung_wechseln() {
    crate::theme::skala_weiter();
    crate::einstellungen::setze_zahl(
        crate::einstellungen::S_SKALA,
        crate::theme::skala_halbe() as i64,
    );
    alles_neu_zeichnen();
    crate::serial_println!("[UI] Skalierung: {}", crate::theme::skala_name());
}

/// Alt+Tab weiterschalten (ruft der KeyStream).
pub fn switcher_weiter() {
    let _ = mit_manager(|m| m.switcher_weiter());
}

/// Alt losgelassen: Fensterwechsel bestätigen (ruft der KeyStream).
pub fn switcher_bestaetigen() {
    let _ = mit_manager(|m| m.switcher_bestaetigen());
}

// ---------------------------------------------------------------------------
// Die Tasks: Compositor und Uhr
// ---------------------------------------------------------------------------

pub async fn compositor_task() {
    loop {
        zeit::warte_ms(30).await;
        if !desktop_aktiv() {
            continue;
        }
        let (breite, hoehe) = match framebuffer::mit_framebuffer(|fb| {
            (fb.info().width as i32, fb.info().height as i32)
        }) {
            Some(masse) => masse,
            None => continue,
        };
        let (rects, hintergrund_neu) = match mit_manager(|m| {
            // Geänderte Inhalte (z. B. Terminal-Ausgabe) EINMAL pro
            // Frame in die Fenster-Puffer rendern, dann die Dirty-
            // Rechtecke abholen (None = nichts zu tun):
            m.inhalte_rendern();
            let hintergrund_neu = core::mem::take(&mut m.hintergrund_neu);
            (m.dirty_abholen(breite, hoehe), hintergrund_neu)
        }) {
            Some((Some(rects), hintergrund_neu)) => (rects, hintergrund_neu),
            _ => continue,
        };

        // Frischer Desktop-Verlauf (erster Frame / Theme-Wechsel):
        // in den Back-Buffer rendern und als Cache übernehmen.
        if hintergrund_neu {
            framebuffer::mit_framebuffer(hintergrund_in_cache_rendern);
        }

        framebuffer::mit_framebuffer(|fb| {
            // Lock-Ordnung: FRAMEBUFFER -> MANAGER.
            x86_64::instructions::interrupts::without_interrupts(|| {
                if let Some(manager) = MANAGER.lock().as_ref() {
                    manager.komponieren(fb, &rects);
                }
            });
            // Nur die geänderten Rechtecke auf den Bildschirm:
            for rect in &rects {
                fb.present_bereich(
                    rect.x as usize,
                    rect.y as usize,
                    rect.breite as usize,
                    rect.hoehe as usize,
                );
            }
        });
        maus::cursor_neu_zeichnen();
    }
}

pub async fn uhr_task() {
    loop {
        zeit::warte_ms(500).await;
        if desktop_aktiv() {
            let _ = mit_manager(|m| m.uhr_aktualisieren());
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — Manager-Logik pur, ohne Bildschirm
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> (FensterManager, FensterId, FensterId) {
        let mut manager = FensterManager::neu(1000, 800);
        let hinten = manager.fenster_erstellen("Hinten", 100, 100, 220, 100, Inhalt::Uhr);
        let vorne = manager.fenster_erstellen(
            "Vorne", 150, 140, 220, 100,
            Inhalt::TastaturEcho { text: String::new() },
        );
        (manager, hinten, vorne)
    }

    #[test_case]
    fn test_fokus_und_z_ordnung() {
        let (mut manager, hinten, vorne) = test_manager();
        assert_eq!(manager.fenster_unter(200, 160), Some(vorne));
        assert_eq!(manager.fenster_unter(110, 110), Some(hinten));
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 110, 110);
        assert_eq!(manager.fokus(), Some(hinten));
        assert_eq!(manager.fenster_unter(200, 160), Some(hinten));
        assert_eq!(manager.fenster_unter(900, 700), None);
    }

    #[test_case]
    fn test_fenster_verschieben() {
        let (mut manager, _, vorne) = test_manager();
        // In der Titelzeile (y=140..170), links von den Knöpfen greifen:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 200, 150);
        manager.maus_event(&MausEvent::Bewegt { x: 240, y: 220 }, 240, 220);
        assert_eq!(manager.fenster_position(vorne), Some((190, 210)));
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 240, 220);
        manager.maus_event(&MausEvent::Bewegt { x: 500, y: 500 }, 500, 500);
        assert_eq!(manager.fenster_position(vorne), Some((190, 210)));
    }

    #[test_case]
    fn test_koordinaten_und_tastatur_routing() {
        let (mut manager, hinten, vorne) = test_manager();
        assert_eq!(manager.fenster_lokal(vorne, 160, 180), Some((10, 10)));
        assert_eq!(manager.fenster_lokal(vorne, 160, 150), None);

        manager.taste_event(DecodedKey::Unicode('h'));
        manager.taste_event(DecodedKey::Unicode('i'));
        let text = match &manager.fenster[manager.index_von(vorne).unwrap()].inhalt {
            Inhalt::TastaturEcho { text } => text.clone(),
            _ => panic!(),
        };
        assert_eq!(text, "hi");

        manager.fokussieren_und_heben(hinten);
        manager.taste_event(DecodedKey::Unicode('x'));
    }

    /// Minimieren blendet aus (Hit-Test ignoriert es), Schließen
    /// entfernt es ganz und wechselt den Fokus.
    #[test_case]
    fn test_minimieren_und_schliessen() {
        let (mut manager, hinten, vorne) = test_manager();
        // (350,200) liegt NUR im vorderen Fenster (rechts vom hinteren).
        assert_eq!(manager.fenster_unter(350, 200), Some(vorne));
        // "vorne" minimieren:
        manager.knopf_aktion(vorne, Knopf::Minimieren);
        assert_eq!(manager.fenster_unter(350, 200), None); // ausgeblendet
        assert_eq!(manager.fokus(), Some(hinten)); // Fokus fiel aufs hintere
        // Alt+Tab: Reihenfolge [vorne, hinten], erster Tab wählt hinten,
        // zweiter Tab wählt vorne -> bestätigen holt vorne zurück.
        manager.switcher_weiter();
        manager.switcher_weiter();
        manager.switcher_bestaetigen();
        assert_eq!(manager.fokus(), Some(vorne));
        assert_eq!(manager.fenster_unter(350, 200), Some(vorne)); // wieder sichtbar
        // "hinten" schließen -> nur noch "vorne" existiert:
        manager.knopf_aktion(hinten, Knopf::Schliessen);
        assert_eq!(manager.index_von(hinten), None);
        assert_eq!(manager.fokus(), Some(vorne));
    }

    /// Ziehen an den linken Rand snappt auf die linke Bildschirmhälfte.
    #[test_case]
    fn test_snap_links() {
        let (mut manager, _, vorne) = test_manager();
        // Titelzeile greifen (200,150), dann fast an den linken Rand:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 200, 150);
        manager.maus_event(&MausEvent::Bewegt { x: 3, y: 150 }, 3, 150);
        // Loslassen -> Snap: x=0, Breite = halber Bildschirm (1000/2).
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 3, 150);
        let index = manager.index_von(vorne).unwrap();
        assert_eq!(manager.fenster[index].x, 0);
        assert_eq!(manager.fenster[index].breite(), 500);
        // Höhe = Bildschirm - Titelzeile - Taskleisten-Reserve.
        assert_eq!(manager.fenster[index].hoehe(), 800 - metrik().titel_hoehe - metrik().taskleiste_hoehe);
    }

    /// Koordinaten-Umrechnung Bildschirm -> Fensterinhalt: Grenzen,
    /// Titelzeile (zählt NICHT zum Inhalt) und alle vier Ecken.
    #[test_case]
    fn test_koordinaten_umrechnung_grenzen() {
        let (manager, _, vorne) = test_manager();
        // "vorne": x=150, y=140, Inhalt 220x100, Titel 30 hoch.
        // Inhalt beginnt bei (150, 170):
        assert_eq!(manager.fenster_lokal(vorne, 150, 170), Some((0, 0)));
        // Letzter Inhalts-Pixel (369, 269):
        assert_eq!(manager.fenster_lokal(vorne, 369, 269), Some((219, 99)));
        // Direkt dahinter: draußen.
        assert_eq!(manager.fenster_lokal(vorne, 370, 269), None);
        assert_eq!(manager.fenster_lokal(vorne, 369, 270), None);
        // Titelzeile (y 140..169): KEIN Inhalt.
        assert_eq!(manager.fenster_lokal(vorne, 200, 169), None);
        // Links neben dem Fenster: draußen.
        assert_eq!(manager.fenster_lokal(vorne, 149, 170), None);
    }

    /// Resize-Zonen (kante_bei): Kanten, Ecken, Innenfläche.
    #[test_case]
    fn test_resize_kanten_zonen() {
        let (manager, _, vorne) = test_manager();
        let f = &manager.fenster[manager.index_von(vorne).unwrap()];
        // Gesamt: x 150..370, y 140..270, Randzone 6 Pixel.
        assert_eq!(FensterManager::kante_bei(f, 152, 200), Some(Kante::Links));
        assert_eq!(FensterManager::kante_bei(f, 368, 200), Some(Kante::Rechts));
        assert_eq!(FensterManager::kante_bei(f, 250, 268), Some(Kante::Unten));
        assert_eq!(FensterManager::kante_bei(f, 152, 268), Some(Kante::UntenLinks));
        assert_eq!(FensterManager::kante_bei(f, 368, 268), Some(Kante::UntenRechts));
        // Mitte: keine Kante.
        assert_eq!(FensterManager::kante_bei(f, 250, 200), None);
    }

    /// Z-Ordnung: fokussieren_und_heben bringt das Fenster ans
    /// Vec-Ende (= ganz vorne), die anderen rutschen nach.
    #[test_case]
    fn test_z_ordnung_heben() {
        let (mut manager, hinten, vorne) = test_manager();
        assert_eq!(manager.fenster.last().unwrap().id, vorne);
        manager.fokussieren_und_heben(hinten);
        assert_eq!(manager.fenster.last().unwrap().id, hinten);
        assert_eq!(manager.fenster[0].id, vorne);
        // Ein drittes Fenster kommt IMMER ganz nach vorne:
        let neu = manager.fenster_erstellen("Neu", 500, 500, 220, 100, Inhalt::Uhr);
        assert_eq!(manager.fenster.last().unwrap().id, neu);
        assert_eq!(manager.fokus(), Some(neu));
    }

    /// DAS DIRTY-RECT-PROTOKOLL: Änderungen melden genau ihre
    /// Fläche, dirty_abholen räumt auf, Überlauf fällt auf Vollbild
    /// zurück, und ohne Änderung gibt es nichts zu komponieren.
    #[test_case]
    fn test_dirty_rects() {
        let (mut manager, _, _) = test_manager();
        // Frisch erstellt: Vollbild (alles_dirty vom Aufbau).
        let rects = manager.dirty_abholen(1000, 800).unwrap();
        assert_eq!(rects, alloc::vec![Rechteck::neu(0, 0, 1000, 800)]);
        // Danach: nichts mehr zu tun.
        assert!(manager.dirty_abholen(1000, 800).is_none());

        // Mausbewegung OHNE Drag ändert nichts:
        manager.maus_event(&MausEvent::Bewegt { x: 500, y: 500 }, 500, 500);
        assert!(manager.dirty_abholen(1000, 800).is_none());

        // Tastatur ins fokussierte TastaturEcho-Fenster: NUR dessen
        // Fläche wird gemeldet (kein Vollbild):
        manager.taste_event(DecodedKey::Unicode('x'));
        let rects = manager.dirty_abholen(1000, 800).unwrap();
        assert_eq!(rects.len(), 1);
        let flaeche = manager.fenster_flaeche(manager.index_von(manager.fokus().unwrap()).unwrap());
        assert_eq!(rects[0], flaeche);

        // Fenster-Drag meldet ALTE + NEUE Fläche (fern vom Rand,
        // damit keine Snap-Vorschau das Vollbild erzwingt):
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 200, 150);
        let _ = manager.dirty_abholen(1000, 800); // Heben-Meldungen abräumen
        manager.maus_event(&MausEvent::Bewegt { x: 220, y: 160 }, 220, 160);
        let rects = manager.dirty_abholen(1000, 800).unwrap();
        assert!(rects.len() >= 2, "Drag muss alte+neue Flaeche melden");
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 220, 160);

        // Uhr-Update: erst beim SEKUNDENWECHSEL, und dann NUR die
        // Systray-Ecke (das Uhr-Fenster im Test meldet sich separat):
        let _ = manager.dirty_abholen(1000, 800);
        manager.letzte_uhr_sekunde = u64::MAX; // erzwungener "Wechsel"
        manager.uhr_aktualisieren();
        let rects = manager.dirty_abholen(1000, 800).unwrap();
        assert!(rects.contains(&manager.systray_rechteck()));
        assert!(!rects.contains(&Rechteck::neu(0, 0, 1000, 800)));

        // Überlauf: mehr als MAX_DIRTY_RECTS -> Vollbild-Fallback.
        for i in 0..(MAX_DIRTY_RECTS as i32 + 2) {
            manager.dirty_melden(Rechteck::neu(i, i, 5, 5));
        }
        let rects = manager.dirty_abholen(1000, 800).unwrap();
        assert_eq!(rects, alloc::vec![Rechteck::neu(0, 0, 1000, 800)]);
    }

    /// Theme-Umschaltung: inhalt_zeichnen übernimmt die neuen Farben
    /// wirklich in den Fenster-Puffer (und zurück).
    #[test_case]
    fn test_theme_umschaltung_faerbt_inhalte() {
        let mut manager = FensterManager::neu(1000, 800);
        let id = manager.fenster_erstellen("Farbe", 10, 10, 220, 100, Inhalt::Uhr);
        let index = manager.index_von(id).unwrap();

        let vorher = theme::aktuell().inhalt_hintergrund;
        assert_eq!(manager.fenster[index].puffer.pixel[0], vorher);

        let nachher = theme::umschalten().inhalt_hintergrund;
        assert_ne!(vorher, nachher, "Themes muessen sich unterscheiden");
        inhalt_zeichnen(&mut manager.fenster[index]);
        assert_eq!(manager.fenster[index].puffer.pixel[0], nachher);

        // Zurückschalten (globalen Zustand nicht verändert hinterlassen!):
        theme::umschalten();
        inhalt_zeichnen(&mut manager.fenster[index]);
        assert_eq!(manager.fenster[index].puffer.pixel[0], vorher);
    }

    /// Die Zeilen-Schnellpfade respektieren Clipping und Ränder
    /// exakt wie der Pro-Pixel-Weg.
    #[test_case]
    fn test_schnellpfade_clipping() {
        use crate::grafik::{Rechteck, Rgba, Zeichner};

        let mut puffer = FensterPuffer::neu(20, 10, Farbe::neu(0, 0, 0));
        let rot = Farbe::neu(255, 0, 0);
        let mut z = Zeichner::neu(&mut puffer);

        // Voll deckendes Rechteck mit Clip: nur der Schnitt wird rot.
        z.clip_setzen(Some(Rechteck::neu(5, 2, 6, 4)));
        z.rechteck_fuellen(Rechteck::neu(0, 0, 20, 10), Rgba::neu(255, 0, 0));
        z.clip_setzen(None);
        assert_eq!(puffer.pixel[0], Farbe::neu(0, 0, 0)); // (0,0) außerhalb
        assert_eq!(puffer.pixel[2 * 20 + 5], rot); // (5,2) im Clip
        assert_eq!(puffer.pixel[5 * 20 + 10], rot); // (10,5) im Clip
        assert_eq!(puffer.pixel[6 * 20 + 5], Farbe::neu(0, 0, 0)); // (5,6) darunter

        // Blit teils außerhalb der Fläche: wird sauber abgeschnitten.
        let gruen = Farbe::neu(0, 255, 0);
        let quelle = vec![gruen; 8 * 4]; // 8x4-Puffer
        let mut z = Zeichner::neu(&mut puffer);
        z.puffer_blit(16, 8, 8, &quelle); // ragt rechts+unten hinaus
        assert_eq!(puffer.pixel[8 * 20 + 16], gruen); // (16,8) sichtbar
        assert_eq!(puffer.pixel[9 * 20 + 19], gruen); // (19,9) letzte Ecke
        assert_eq!(puffer.pixel[7 * 20 + 16], Farbe::neu(0, 0, 0)); // darüber unberührt
    }

    /// SPEICHER-PASS: Fenster (auch Terminal) in Schleife öffnen und
    /// schließen darf den Heap nicht wachsen lassen — jedes Schließen
    /// muss Puffer UND Terminal-Raster vollständig freigeben.
    #[test_case]
    fn test_fenster_schleife_leckt_nicht() {
        let mut manager = FensterManager::neu(1000, 800);

        let runde = |manager: &mut FensterManager| {
            let id = manager.fenster_erstellen("Leck-Test", 50, 50, 300, 200, Inhalt::Uhr);
            let _ = manager.knopf_aktion(id, Knopf::Schliessen);
            let sitzung = manager.terminal_oeffnen();
            let terminal_id = manager.fenster[manager.terminal_index(sitzung).unwrap()].id;
            manager.terminal_schreiben(sitzung, format_args!("ein paar Zeichen\n"), Farbe::neu(1, 1, 1), Farbe::neu(0, 0, 0));
            let _ = manager.knopf_aktion(terminal_id, Knopf::Schliessen);
        };

        // Aufwärmen: Vec-Kapazitäten, Allocator-Blocklisten usw.
        // dürfen sich EINMAL einpendeln.
        for _ in 0..3 {
            runde(&mut manager);
        }
        let vorher = crate::allocator::heap_statistik().map(|(belegt, _)| belegt);

        for _ in 0..30 {
            runde(&mut manager);
        }
        let nachher = crate::allocator::heap_statistik().map(|(belegt, _)| belegt);

        // (Lern-Allocatoren ohne Statistik überspringen den Vergleich.)
        if let (Some(vorher), Some(nachher)) = (vorher, nachher) {
            assert!(
                nachher <= vorher,
                "Heap waechst beim Fenster-Zyklus: {} -> {} Bytes",
                vorher,
                nachher
            );
        }
    }

    /// SPEICHER-PASS Serie 3: ALLE Apps in Schleife öffnen, benutzen
    /// (Tasten -> Nachrichten -> Neu-Aufbauten) und schließen — der
    /// Heap darf danach nicht gewachsen sein. Deckt die neuen
    /// Besitz-Ketten ab: Terminal-Sitzungen (Registry-Austrag),
    /// SpeedTexts geteilter Arc-Puffer, Explorer-/Task-Manager-/
    /// Einstellungs-Zustand, Fenster-Puffer.
    #[test_case]
    fn test_app_zyklen_lecken_nicht() {
        let haupt_vorher = crate::shell::sitzung::haupt();
        crate::shell::sitzung::haupt_setzen(0);
        let mut manager = FensterManager::neu(1000, 800);
        let sitzungen_vorher = crate::shell::sitzung::haupt(); // 0
        let _ = sitzungen_vorher;

        let runde = |manager: &mut FensterManager| {
            // Terminal: öffnen, schreiben, rendern, schließen.
            let sitzung = manager.terminal_oeffnen();
            manager.terminal_schreiben(
                sitzung,
                format_args!("Zyklus-Ausgabe mit ein paar Zeichen\n"),
                Farbe::neu(200, 200, 200),
                Farbe::neu(0, 0, 0),
            );
            manager.inhalte_rendern();
            let index = manager.terminal_index(sitzung).unwrap();
            manager.fenster_schliessen(index);

            // Die vier Trait-Apps: öffnen, per Tasten benutzen
            // (Pfeil/Enter erzeugen echte Nachrichten samt
            // Neu-Aufbauten), rendern, über den X-Knopf schließen.
            let apps: [alloc::boxed::Box<dyn crate::ui::App>; 4] = [
                alloc::boxed::Box::new(crate::explorer::ExplorerApp::neu()),
                alloc::boxed::Box::new(crate::einstellungen::EinstellungenApp::neu()),
                alloc::boxed::Box::new(crate::taskmanager::TaskManagerApp::neu()),
                alloc::boxed::Box::new(crate::speedtext::SpeedTextApp::neu()),
            ];
            for app in apps {
                let id = manager.fenster_erstellen(
                    "Zyklus", 80, 80, 560, 400,
                    Inhalt::App(crate::ui::AppFenster::neu(app)),
                );
                for taste in [
                    DecodedKey::RawKey(pc_keyboard::KeyCode::ArrowDown),
                    DecodedKey::Unicode('a'),
                    DecodedKey::Unicode('\n'),
                ] {
                    let _ = manager.taste_event(taste);
                }
                manager.inhalte_rendern();
                // SpeedText hat jetzt Änderungen -> der X-Knopf würde
                // den Nachfrage-Dialog zeigen; für den Speicher-Test
                // schließen wir DIREKT (der Dialog-Weg ist per
                // Unit-Test abgedeckt) — auch das muss alles freigeben.
                let index = manager.index_von(id).unwrap();
                manager.fenster_schliessen(index);
            }
        };

        // Aufwärmen: Kapazitäten (Vecs, BTreeMaps, Allocator-Listen)
        // dürfen sich EINMAL einpendeln.
        for _ in 0..3 {
            runde(&mut manager);
        }
        let vorher = crate::allocator::heap_statistik().map(|(belegt, _)| belegt);

        for _ in 0..20 {
            runde(&mut manager);
        }

        let nachher = crate::allocator::heap_statistik().map(|(belegt, _)| belegt);
        assert_eq!(
            vorher, nachher,
            "App-Zyklen lecken Heap: vorher {:?}, nachher {:?}",
            vorher, nachher
        );
        crate::shell::sitzung::haupt_setzen(haupt_vorher);
    }

    /// MESSUNG (kein Pass/Fail) — Serie-3-Lastprofil: 5 offene
    /// App-Fenster (2 Terminal-Sitzungen, Explorer, Task-Manager mit
    /// gefülltem Graph, SpeedText mit 60 Zeilen). Szenarien: Vollbild,
    /// Editor-Tippen (Taste -> Neu-Aufbau -> Rendern -> Komposition),
    /// Terminal-Ausgabe (print -> Raster -> Rendern -> Komposition).
    /// Zahlen in us/Frame seriell — Vergleichswerte im CHANGELOG.
    #[test_case]
    fn messung_serie3_apps_frame_zeit() {
        use crate::serial_println;

        if !framebuffer::ist_initialisiert() {
            serial_println!("[MESSUNG-S3] uebersprungen (kein Framebuffer)");
            return;
        }
        let haupt_vorher = crate::shell::sitzung::haupt();
        crate::shell::sitzung::haupt_setzen(0);
        let (breite, hoehe) = framebuffer::mit_framebuffer(|fb| {
            (fb.info().width as i32, fb.info().height as i32)
        })
        .unwrap();
        let mut manager = FensterManager::neu(breite, hoehe);

        // 2 Terminal-Sitzungen mit etwas Inhalt:
        let sitzung_a = manager.terminal_oeffnen();
        let sitzung_b = manager.terminal_oeffnen();
        for i in 0..20 {
            manager.terminal_schreiben(
                sitzung_a,
                format_args!("Zeile {} mit etwas Text im Raster\n", i),
                Farbe::neu(200, 200, 200),
                Farbe::neu(0, 0, 0),
            );
        }

        // Explorer, Task-Manager (Graph mit 60 Messwerten), SpeedText:
        manager.fenster_erstellen(
            "Explorer", 60, 60, 560, 400,
            Inhalt::App(crate::ui::AppFenster::neu(alloc::boxed::Box::new(
                crate::explorer::ExplorerApp::neu(),
            ))),
        );
        let mut tm = crate::taskmanager::TaskManagerApp::neu();
        tm.cpu_verlauf_fuellen_fuer_messung();
        manager.fenster_erstellen(
            "Task-Manager", 200, 140, 640, 460,
            Inhalt::App(crate::ui::AppFenster::neu(alloc::boxed::Box::new(tm))),
        );
        let editor_text = "Der schnelle braune Fuchs springt ueber den faulen Hund.\n".repeat(60);
        let _ = crate::fs::mit_fs(|f| f.schreiben("/messung_s3.txt", editor_text.as_bytes()));
        manager.fenster_erstellen(
            "SpeedText", 340, 200, 560, 420,
            Inhalt::App(crate::ui::AppFenster::neu(alloc::boxed::Box::new(
                crate::speedtext::SpeedTextApp::mit_datei("/messung_s3.txt"),
            ))),
        );

        framebuffer::mit_framebuffer(hintergrund_in_cache_rendern);
        manager.hintergrund_neu = false;

        const FRAMES: u64 = 40;
        let szenario = |name: &str,
                        manager: &mut FensterManager,
                        schritt: &mut dyn FnMut(&mut FensterManager, u64)| {
            let start = zeit::us_seit_boot();
            for i in 0..FRAMES {
                schritt(manager, i);
                manager.inhalte_rendern();
                framebuffer::mit_framebuffer(|fb| {
                    let rects =
                        manager.dirty_abholen(fb.info().width as i32, fb.info().height as i32);
                    if let Some(rects) = rects {
                        manager.komponieren(fb, &rects);
                        for r in &rects {
                            fb.present_bereich(
                                r.x.max(0) as usize,
                                r.y.max(0) as usize,
                                r.breite as usize,
                                r.hoehe as usize,
                            );
                        }
                    }
                });
            }
            let dauer_us = zeit::us_seit_boot() - start;
            serial_println!(
                "[MESSUNG-S3] {}: {} Frames -> {} us/Frame",
                name,
                FRAMES,
                dauer_us / FRAMES
            );
        };

        // 1. Vollbild: alle 5 Fenster + Taskleiste komplett.
        szenario("Vollbild 5 Fenster", &mut manager, &mut |m, _| m.alles_dirty = true);
        // 2. Editor-Tippen, ALTER WEG simuliert (kompletter Baum-
        //    Neuaufbau + Voll-Zeichnen pro Taste — so war es vor dem
        //    Performance-Pass) vs. NEUER Weg (StatusZeile liest live,
        //    nur Neuzeichnen). Beide im SELBEN Lauf — die Zahlen sind
        //    damit unabhängig davon, ob QEMU mit WHPX oder TCG läuft.
        szenario("Editor-Tippen ALT (Neu-Aufbau)", &mut manager, &mut |m, _| {
            let _ = m.taste_event(DecodedKey::Unicode('a'));
            let index = m.fenster.len() - 1; // SpeedText (fokussiert)
            if let Inhalt::App(app_fenster) = &mut m.fenster[index].inhalt {
                app_fenster.neu_aufbauen();
            }
            m.fenster[index].inhalt_neu = true;
            m.fenster[index].dirty = true;
        });
        szenario("Editor-Tippen NEU", &mut manager, &mut |m, _| {
            let _ = m.taste_event(DecodedKey::Unicode('a'));
        });
        // 3. Terminal-Ausgabe in die (nicht fokussierte) Sitzung A —
        //    ALTER Weg (ganzes Raster rendern + Fensterfläche
        //    komponieren) vs. NEUER Weg (nur der Zeilen-Streifen).
        szenario("Terminal-Ausgabe ALT (voll)", &mut manager, &mut |m, i| {
            m.terminal_schreiben(
                sitzung_a,
                format_args!("Ausgabe-Zeile Nummer {} im Messlauf\n", i),
                Farbe::neu(200, 200, 200),
                Farbe::neu(0, 0, 0),
            );
            let index = m.terminal_index(sitzung_a).unwrap();
            if let Inhalt::Terminal { term, .. } = &mut m.fenster[index].inhalt {
                term.alles_markieren();
            }
            m.fenster[index].dirty = true;
        });
        szenario("Terminal-Ausgabe NEU", &mut manager, &mut |m, i| {
            m.terminal_schreiben(
                sitzung_a,
                format_args!("Ausgabe-Zeile Nummer {} im Messlauf\n", i),
                Farbe::neu(200, 200, 200),
                Farbe::neu(0, 0, 0),
            );
        });

        // Aufräumen: Sitzungen austragen, Messdatei löschen.
        let index = manager.terminal_index(sitzung_a).unwrap();
        manager.fenster_schliessen(index);
        let index = manager.terminal_index(sitzung_b).unwrap();
        manager.fenster_schliessen(index);
        crate::shell::sitzung::haupt_setzen(haupt_vorher);
        let _ = crate::fs::mit_fs(|f| f.loeschen("/messung_s3.txt"));
    }

    /// MESSUNG (kein Pass/Fail): Frame-Zeit des Compositors bei
    /// 3 offenen Fenstern + Mausbewegung (Drag setzt alles_dirty wie
    /// im echten Betrieb). Ausgabe in ms/Frame über die serielle
    /// Konsole — die Vergleichszahlen stehen im CHANGELOG.
    #[test_case]
    fn messung_compositor_frame_zeit() {
        use crate::serial_println;

        if !framebuffer::ist_initialisiert() {
            serial_println!("[MESSUNG] uebersprungen (kein Framebuffer)");
            return;
        }
        let (breite, hoehe) = framebuffer::mit_framebuffer(|fb| {
            (fb.info().width as i32, fb.info().height as i32)
        })
        .unwrap();
        let mut manager = FensterManager::neu(breite, hoehe);
        manager.fenster_erstellen("Uhr", 140, 120, 420, 150, Inhalt::Uhr);
        manager.fenster_erstellen(
            "Tastatur", 420, 320, 560, 140,
            Inhalt::TastaturEcho { text: String::new() },
        );
        manager.fenster_erstellen(
            "Grafik", 700, 200, 380, 220,
            Inhalt::Malflaeche { klicks: Vec::new() },
        );

        // Hintergrund-Cache einmalig füllen (macht sonst der
        // Compositor-Task beim ersten Frame):
        framebuffer::mit_framebuffer(hintergrund_in_cache_rendern);
        manager.hintergrund_neu = false;

        // Drei Szenarien, gemessen mit der TSC-µs-Uhr (läuft auch
        // unter without_interrupts): Vollbild, nur Uhr-Tick, nur
        // Fenster-Drag (Mausbewegung mit gegriffenem Fenster).
        const FRAMES: u64 = 40;
        let szenario = |name: &str, manager: &mut FensterManager, schritt: &mut dyn FnMut(&mut FensterManager, u64)| {
            let start = zeit::us_seit_boot();
            for i in 0..FRAMES {
                schritt(manager, i);
                framebuffer::mit_framebuffer(|fb| {
                    let rects = manager.dirty_abholen(fb.info().width as i32, fb.info().height as i32);
                    if let Some(rects) = rects {
                        manager.komponieren(fb, &rects);
                        for r in &rects {
                            fb.present_bereich(r.x.max(0) as usize, r.y.max(0) as usize, r.breite as usize, r.hoehe as usize);
                        }
                    }
                });
            }
            let dauer_us = zeit::us_seit_boot() - start;
            serial_println!(
                "[MESSUNG] {}: {} Frames -> {} us/Frame",
                name,
                FRAMES,
                dauer_us / FRAMES
            );
        };

        // 1. Vollbild: jede Runde alles neu.
        szenario("Vollbild", &mut manager, &mut |m, _| m.alles_dirty = true);
        // 2. Uhr-Tick: nur die Systray-Uhr (erzwungener Sekundenwechsel).
        szenario("Uhr-Tick", &mut manager, &mut |m, _| {
            m.letzte_uhr_sekunde = u64::MAX;
            m.uhr_aktualisieren();
        });
        // 3. Fenster-Drag:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 720, 210);
        szenario("Fenster-Drag", &mut manager, &mut |m, i| {
            let x = 720 - (i as i32 % 20) * 4;
            m.maus_event(&MausEvent::Bewegt { x, y: 210 }, x, 210);
        });
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 640, 210);

        // A/B-Vergleich der Kern-Optimierung: EIN Fenster-Inhalt
        // (560x140) 100x auf den Bildschirm — alter Weg (Pro-Pixel
        // durch Zeichner::pixel) gegen neuen Blit-Schnellpfad.
        // Dank TSC darf die Zeit jetzt IM mit_framebuffer-Block
        // genommen werden.
        let puffer = FensterPuffer::neu(560, 140, Farbe::neu(30, 40, 50));
        framebuffer::mit_framebuffer(|fb| {
            let mut z = Zeichner::neu(fb);
            let start = zeit::us_seit_boot();
            for _ in 0..100 {
                for zeile in 0..puffer.hoehe {
                    let basis = zeile * puffer.breite;
                    for spalte in 0..puffer.breite {
                        let farbe = puffer.pixel[basis + spalte];
                        z.pixel(
                            100 + spalte as i32,
                            100 + zeile as i32,
                            Rgba::neu(farbe.r, farbe.g, farbe.b),
                        );
                    }
                }
            }
            let alt_us = zeit::us_seit_boot() - start;
            let start = zeit::us_seit_boot();
            for _ in 0..100 {
                z.puffer_blit(100, 100, puffer.breite, &puffer.pixel);
            }
            let neu_us = zeit::us_seit_boot() - start;
            serial_println!(
                "[MESSUNG] Fenster-Blit 560x140, 100 Durchlaeufe: Pro-Pixel {} us, Zeilenkopie {} us",
                alt_us,
                neu_us
            );
        });
    }

    /// FOKUS über Fenster-Wechsel hinweg: Tasten landen im Widget
    /// des FOKUSSIERTEN Fensters — und das fokussierte Textfeld des
    /// anderen Fensters behält seinen Fokus, bis man zurückwechselt.
    #[test_case]
    fn test_fokus_ueber_fensterwechsel() {
        use crate::ui::widgets::Textfeld;
        use crate::ui::UiFenster;

        let mut manager = FensterManager::neu(1000, 800);
        let mut ui_a = UiFenster::neu(
            alloc::boxed::Box::new(Textfeld::neu(100)),
            |_| {},
            &crate::grafik::ICON_LOGO,
        );
        ui_a.fokus_initial();
        let a = manager.fenster_erstellen("A", 100, 100, 300, 120, Inhalt::Ui(ui_a));
        let mut ui_b = UiFenster::neu(
            alloc::boxed::Box::new(Textfeld::neu(200)),
            |_| {},
            &crate::grafik::ICON_LOGO,
        );
        ui_b.fokus_initial();
        let b = manager.fenster_erstellen("B", 500, 100, 300, 120, Inhalt::Ui(ui_b));

        // B ist zuletzt erstellt -> fokussiert: Enter meldet die
        // Nachricht von B-Textfeld (200) als NachLock nach draußen.
        assert_eq!(manager.fokus(), Some(b));
        match manager.taste_event(DecodedKey::Unicode('\n')) {
            NachLock::Nachricht(_, id) => assert_eq!(id, 200),
            _ => panic!("Enter erreichte B nicht"),
        }

        // Klick auf die Titelzeile von A: Fenster-Fokus wechselt,
        // OHNE den Widget-Fokus in A anzutasten — Enter landet
        // jetzt im A-Textfeld (100).
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 150, 110);
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 150, 110);
        assert_eq!(manager.fokus(), Some(a));
        match manager.taste_event(DecodedKey::Unicode('\n')) {
            NachLock::Nachricht(_, id) => assert_eq!(id, 100),
            _ => panic!("Enter erreichte A nicht"),
        }

        // Und zurück: B hat seinen Widget-Fokus ebenfalls behalten.
        manager.fokussieren_und_heben(b);
        match manager.taste_event(DecodedKey::Unicode('\n')) {
            NachLock::Nachricht(_, id) => assert_eq!(id, 200),
            _ => panic!("Enter erreichte B nach dem Rueckwechsel nicht"),
        }
    }

    /// SITZUNGS-ZUORDNUNG: fokus_terminal_sitzung liefert genau die
    /// Sitzung des fokussierten Terminals (die Routing-Grundlage des
    /// Eingabe-Routers) — auch nach Fokuswechsel, bei minimiertem
    /// Terminal und bei fokussiertem Nicht-Terminal.
    #[test_case]
    fn test_terminal_fokus_sitzung_zuordnung() {
        let haupt_vorher = crate::shell::sitzung::haupt();
        crate::shell::sitzung::haupt_setzen(0);
        let mut manager = FensterManager::neu(1000, 800);
        let erste = manager.terminal_oeffnen();
        let zweite = manager.terminal_oeffnen();

        // Zuletzt geöffnet = fokussiert -> zweite Sitzung; Tasten
        // landen (wie im Eingabe-Router) in genau DEREN Queue:
        assert_eq!(manager.fokus_terminal_sitzung(), Some(zweite));
        let ziel = manager.fokus_terminal_sitzung().unwrap();
        crate::shell::sitzung::taste_einwerfen(ziel, DecodedKey::Unicode('x'));
        let s1 = crate::shell::sitzung::holen(erste).unwrap();
        let s2 = crate::shell::sitzung::holen(zweite).unwrap();
        assert_eq!(s2.taste_abholen(), Some(DecodedKey::Unicode('x')));
        assert_eq!(s1.taste_abholen(), None);

        // Fokus aufs erste Terminal -> erste Sitzung.
        let erste_id = manager.fenster[manager.terminal_index(erste).unwrap()].id;
        manager.fokussieren_und_heben(erste_id);
        assert_eq!(manager.fokus_terminal_sitzung(), Some(erste));

        // Minimiert zählt nicht (der Router gäbe die Taste ans
        // nächste fokussierte Fenster statt an eine unsichtbare Shell):
        let index = manager.terminal_index(erste).unwrap();
        manager.fenster[index].minimiert = true;
        assert_eq!(manager.fokus_terminal_sitzung(), None);

        // Ein fokussiertes NICHT-Terminal liefert ebenfalls None:
        let uhr = manager.fenster_erstellen("Uhr", 50, 50, 220, 100, Inhalt::Uhr);
        manager.fokussieren_und_heben(uhr);
        assert_eq!(manager.fokus_terminal_sitzung(), None);

        // Aufräumen (globale Sitzungs-Registry sauber hinterlassen):
        let index = manager.terminal_index(erste).unwrap();
        manager.fenster_schliessen(index);
        let index = manager.terminal_index(zweite).unwrap();
        manager.fenster_schliessen(index);
        crate::shell::sitzung::haupt_setzen(haupt_vorher);
    }

    /// Terminal-SITZUNGEN: Jedes Öffnen erzeugt ein eigenes Fenster
    /// mit eigener Sitzung; Schreiben landet im Raster der RICHTIGEN
    /// Sitzung; Schließen trägt die Sitzung aus (beendet-Flag) und
    /// vererbt die Haupt-Rolle ans verbliebene Terminal.
    #[test_case]
    fn test_terminal_sitzungen_unabhaengig() {
        let haupt_vorher = crate::shell::sitzung::haupt();
        crate::shell::sitzung::haupt_setzen(0);
        let mut manager = FensterManager::neu(1000, 800);
        let erste = manager.terminal_oeffnen();
        let zweite = manager.terminal_oeffnen();
        assert_ne!(erste, zweite); // ZWEI unabhängige Sitzungen
        assert_eq!(crate::shell::sitzung::haupt(), erste); // erstes = Haupt

        let vg = Farbe::neu(200, 200, 200);
        let hg = Farbe::neu(0, 0, 0);
        assert!(manager.terminal_schreiben(erste, format_args!("hi"), vg, hg));
        assert!(manager.terminal_schreiben(zweite, format_args!("du"), vg, hg));
        let index = manager.terminal_index(erste).unwrap();
        if let Inhalt::Terminal { term, .. } = &manager.fenster[index].inhalt {
            assert_eq!(term.zelle(0, 0).zeichen, 'h'); // NICHT 'd'
            assert_eq!(term.zelle(1, 0).zeichen, 'i');
        } else {
            panic!("Terminal-Fenster hat keinen Terminal-Inhalt");
        }
        assert!(manager.fenster[index].inhalt_neu);
        manager.inhalte_rendern();
        assert!(!manager.fenster[index].inhalt_neu);

        // Haupt-Terminal schließen: Sitzung wird beendet, das zweite
        // Terminal erbt die Haupt-Rolle (Kernel-Log-Ziel).
        let sitzung_eins = crate::shell::sitzung::holen(erste).unwrap();
        let index = manager.terminal_index(erste).unwrap();
        manager.fenster_schliessen(index);
        assert!(sitzung_eins.ist_beendet());
        assert!(crate::shell::sitzung::holen(erste).is_none());
        assert_eq!(crate::shell::sitzung::haupt(), zweite);
        // Schreiben an die tote Sitzung schlägt "sauber" fehl:
        assert!(!manager.terminal_schreiben(erste, format_args!("x"), vg, hg));

        // Aufräumen (globale Sitzungs-Registry nicht vermüllen):
        let index = manager.terminal_index(zweite).unwrap();
        manager.fenster_schliessen(index);
        crate::shell::sitzung::haupt_setzen(haupt_vorher);
    }

    /// Startmenü (Widget-Verbund): Suchtext filtert live, Enter
    /// liefert die Start-Funktion als NachLock nach draußen
    /// (Deadlock-Regel), ohne Treffer bleibt das Menü offen.
    #[test_case]
    fn test_startmenue_suche_und_start() {
        let (mut manager, _, _) = test_manager();
        manager.startmenue_umschalten();
        assert!(manager.start_menue.is_some());
        let alle = manager.start_menue.as_ref().unwrap().gefiltert.len();
        assert_eq!(alle, crate::apps::alle_apps().len());

        // "uhr" tippen -> Live-Filter auf genau einen Treffer:
        for zeichen in "uhr".chars() {
            let nach = manager.startmenue_taste(DecodedKey::Unicode(zeichen));
            assert!(matches!(nach, NachLock::Keine));
        }
        assert_eq!(manager.start_menue.as_ref().unwrap().gefiltert.len(), 1);
        // Enter startet ihn (Aktion geht als NachLock nach draußen):
        let aktion = manager.startmenue_taste(DecodedKey::Unicode('\n'));
        assert!(matches!(aktion, NachLock::Ausfuehren(_)));
        assert!(manager.start_menue.is_none()); // Menü hat sich geschlossen

        // Ohne Treffer startet Enter nichts (Menü bleibt offen):
        manager.startmenue_umschalten();
        for zeichen in "xyz".chars() {
            manager.startmenue_taste(DecodedKey::Unicode(zeichen));
        }
        assert!(manager.start_menue.as_ref().unwrap().gefiltert.is_empty());
        let leer = manager.startmenue_taste(DecodedKey::Unicode('\n'));
        assert!(matches!(leer, NachLock::Keine));
        assert!(manager.start_menue.is_some());

        // Klick weit außerhalb schließt das Menü:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 900, 100);
        assert!(manager.start_menue.is_none());
    }

    /// Der Startknopf in der Taskleiste togglet das Menü.
    #[test_case]
    fn test_startknopf_toggle() {
        let (mut manager, _, _) = test_manager();
        // Klick auf den Startknopf (unten links): öffnet ...
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 20, 780);
        assert!(manager.start_menue.is_some());
        // ... zweiter Klick schließt wieder (Klick außerhalb des Panels).
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 20, 780);
        assert!(manager.start_menue.is_none());
    }

    /// Der Taskleisten-Knopf togglet: fokussiert -> minimieren,
    /// minimiert/im Hintergrund -> holen + fokussieren.
    #[test_case]
    fn test_taskleisten_knopf_toggle() {
        let (mut manager, hinten, vorne) = test_manager();
        // Knöpfe sind nach Erstellungs-Reihenfolge sortiert:
        let knoepfe = manager.taskleisten_knoepfe();
        assert_eq!(knoepfe.len(), 2);
        assert_eq!(knoepfe[0].0, hinten);
        let (id, rect) = knoepfe[1];
        assert_eq!(id, vorne);

        // Klick auf den Knopf des FOKUSSIERTEN Fensters minimiert es:
        let (cx, cy) = (rect.x + rect.breite / 2, rect.y + rect.hoehe / 2);
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), cx, cy);
        let index = manager.index_von(vorne).unwrap();
        assert!(manager.fenster[index].minimiert);
        assert_eq!(manager.fokus(), Some(hinten));

        // Zweiter Klick holt es zurück und fokussiert es wieder:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), cx, cy);
        let index = manager.index_von(vorne).unwrap();
        assert!(!manager.fenster[index].minimiert);
        assert_eq!(manager.fokus(), Some(vorne));
    }

    /// Resize an der rechten Kante vergrößert die Breite.
    #[test_case]
    fn test_groesse_aendern() {
        let (mut manager, _, vorne) = test_manager();
        // Rechte Kante des vorderen Fensters: x = 150+220-2 = 368,
        // unterhalb der Titelzeile (y=200):
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 368, 200);
        manager.maus_event(&MausEvent::Bewegt { x: 468, y: 200 }, 468, 200);
        let index = manager.index_von(vorne).unwrap();
        assert_eq!(manager.fenster[index].breite(), 320); // 220 + 100
    }
}
