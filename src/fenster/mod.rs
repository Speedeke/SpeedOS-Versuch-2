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
use crate::theme::{self, METRIK};
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
    fn neu(breite: usize, hoehe: usize, fuellung: Farbe) -> Self {
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
}

/// Die Inhalte, die ein Fenster darstellen kann.
pub enum Inhalt {
    /// Die SpeedShell als Fenster (konsole::_print leitet hierher um).
    Terminal(terminal::Terminal),
    Uhr,
    TastaturEcho { text: String },
    Malflaeche { klicks: Vec<(i32, i32)> },
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
        Rechteck::neu(self.x, self.y, self.breite(), METRIK.titel_hoehe + self.hoehe())
    }

    fn in_titelzeile(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.breite() && py >= self.y && py < self.y + METRIK.titel_hoehe
    }

    /// Bildschirm- -> Fensterinhalts-Koordinaten (None = außerhalb).
    fn lokal(&self, px: i32, py: i32) -> Option<(i32, i32)> {
        let lx = px - self.x;
        let ly = py - self.y - METRIK.titel_hoehe;
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
        let h = METRIK.titel_hoehe - 6;
        [
            (Knopf::Schliessen, Rechteck::neu(rechts - METRIK.knopf_breite, y, METRIK.knopf_breite - 4, h)),
            (Knopf::Maximieren, Rechteck::neu(rechts - 2 * METRIK.knopf_breite, y, METRIK.knopf_breite - 4, h)),
            (Knopf::Minimieren, Rechteck::neu(rechts - 3 * METRIK.knopf_breite, y, METRIK.knopf_breite - 4, h)),
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
        let breite = breite.max(METRIK.min_fenster_breite);
        let hoehe = hoehe.max(METRIK.min_fenster_hoehe);
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

/// Der Alt+Tab-Fensterwechsler.
struct Switcher {
    reihenfolge: Vec<FensterId>,
    auswahl: usize,
}

/// Das offene Startmenü (None im Manager = zu).
struct StartMenue {
    /// Der getippte Suchtext — filtert die App-Liste live.
    suchtext: String,
    /// Ausgewählter Eintrag (Index in der GEFILTERTEN Liste) —
    /// Pfeiltasten bewegen ihn, Enter startet ihn, Maus-Hover setzt ihn.
    auswahl: usize,
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
    alles_dirty: bool,
    bildschirm_breite: i32,
    bildschirm_hoehe: i32,
    /// Zuletzt in der Taskleiste angezeigte Sekunde — nur bei einem
    /// Wechsel wird neu komponiert (nicht bei jedem Uhr-Task-Lauf).
    letzte_uhr_sekunde: u64,
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
            alles_dirty: true,
            bildschirm_breite,
            bildschirm_hoehe,
            letzte_uhr_sekunde: 0,
        }
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
        self.fokus = Some(id);
        self.alles_dirty = true;
        id
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
            let fenster = self.fenster.remove(index);
            self.fenster.push(fenster);
            self.fokus = Some(id);
            self.alles_dirty = true;
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
        self.bildschirm_hoehe - METRIK.taskleiste_hoehe
    }

    /// Das Rechteck des Startknopfs (ganz links in der Leiste).
    fn start_knopf_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            0,
            self.taskleiste_y(),
            METRIK.start_knopf_breite,
            METRIK.taskleiste_hoehe,
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
        let von = METRIK.start_knopf_breite + METRIK.abstand;
        let bis = self.bildschirm_breite - METRIK.systray_breite;
        // Standardbreite, aber schrumpfen, wenn es eng wird:
        let breite =
            ((bis - von) / ids.len() as i32 - 4).clamp(40, METRIK.leisten_knopf_breite);
        let y = self.taskleiste_y() + 5;
        ids.into_iter()
            .enumerate()
            .map(|(i, id)| {
                let x = von + i as i32 * (breite + 4);
                (FensterId(id), Rechteck::neu(x, y, breite, METRIK.taskleiste_hoehe - 10))
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
    fn terminal_index(&self) -> Option<usize> {
        self.fenster
            .iter()
            .position(|f| matches!(f.inhalt, Inhalt::Terminal(_)))
    }

    /// Öffnet das Terminal-Fenster oder holt ein vorhandenes nach
    /// vorn. Liefert true, wenn es NEU erstellt wurde.
    fn terminal_oeffnen(&mut self) -> bool {
        if let Some(index) = self.terminal_index() {
            let id = self.fenster[index].id;
            self.fenster[index].minimiert = false;
            self.fokussieren_und_heben(id);
            return false;
        }
        // Wunschgröße: 80x24 Zellen — auf kleinen Schirmen weniger.
        let zeichen_breite = get_raster_width(FontWeight::Regular, METRIK.schrift_ui);
        let breite = (80 * zeichen_breite)
            .min((self.bildschirm_breite as usize).saturating_sub(80))
            .max(METRIK.min_fenster_breite);
        let hoehe = (24 * METRIK.zeilen_hoehe as usize)
            .min((self.bildschirm_hoehe as usize).saturating_sub(160))
            .max(METRIK.min_fenster_hoehe);
        let x = (self.bildschirm_breite - breite as i32) / 2;
        let y = ((self.taskleiste_y() - METRIK.titel_hoehe - hoehe as i32) / 2).max(20);
        let term = terminal::Terminal::neu(
            breite / zeichen_breite,
            hoehe / METRIK.zeilen_hoehe as usize,
            theme::aktuell().terminal_hintergrund,
        );
        self.fenster_erstellen("Terminal", x, y, breite, hoehe, Inhalt::Terminal(term));
        true
    }

    /// Schreibt formatierten Text ins Terminal-Fenster (Umleitung von
    /// konsole::_print im Desktop-Modus). false = kein Terminal offen.
    /// Rendert NICHT sofort — nur inhalt_neu setzen, der Compositor
    /// bündelt das Rendern pro Frame.
    fn terminal_schreiben(&mut self, args: core::fmt::Arguments, vg: Farbe, hg: Farbe) -> bool {
        let index = match self.terminal_index() {
            Some(index) => index,
            None => return false,
        };
        if let Inhalt::Terminal(term) = &mut self.fenster[index].inhalt {
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
        self.fenster[index].dirty = true;
        true
    }

    /// Leert das Terminal-Raster (clear-Befehl im Desktop-Modus).
    fn terminal_leeren(&mut self) {
        if let Some(index) = self.terminal_index() {
            if let Inhalt::Terminal(term) = &mut self.fenster[index].inhalt {
                term.leeren();
            }
            self.fenster[index].inhalt_neu = true;
            self.fenster[index].dirty = true;
        }
    }

    /// Rendert alle geänderten Inhalte (inhalt_neu) in ihre Puffer —
    /// ruft der Compositor EINMAL pro Frame, vor dem Komponieren.
    fn inhalte_rendern(&mut self) {
        for index in 0..self.fenster.len() {
            if self.fenster[index].inhalt_neu {
                self.fenster[index].inhalt_neu = false;
                inhalt_zeichnen(&mut self.fenster[index]);
                self.fenster[index].dirty = true;
            }
        }
    }

    // ----- Startmenü -----

    /// Öffnet/schließt das Startmenü (Startknopf oder Super-Taste).
    fn startmenue_umschalten(&mut self) {
        self.start_menue = match self.start_menue {
            Some(_) => None,
            None => Some(StartMenue { suchtext: String::new(), auswahl: 0 }),
        };
        self.alles_dirty = true;
    }

    fn startmenue_schliessen(&mut self) {
        if self.start_menue.take().is_some() {
            self.alles_dirty = true;
        }
    }

    /// Das Panel-Rechteck des Startmenüs — direkt über dem Startknopf,
    /// die Höhe wächst mit der (gefilterten) App-Liste.
    fn menue_rechteck(&self, anzahl_eintraege: usize) -> Rechteck {
        let n = anzahl_eintraege.max(1) as i32;
        let hoehe = METRIK.menue_suchfeld_hoehe + n * METRIK.menue_eintrag_hoehe + METRIK.abstand;
        Rechteck::neu(
            METRIK.abstand,
            self.taskleiste_y() - hoehe - METRIK.abstand,
            METRIK.menue_breite,
            hoehe,
        )
    }

    /// Das Rechteck des i-ten Menü-Eintrags (innerhalb von `menue`).
    fn menue_eintrag_rechteck(menue: Rechteck, i: usize) -> Rechteck {
        Rechteck::neu(
            menue.x + METRIK.abstand,
            menue.y + METRIK.menue_suchfeld_hoehe + i as i32 * METRIK.menue_eintrag_hoehe,
            menue.breite - 2 * METRIK.abstand,
            METRIK.menue_eintrag_hoehe,
        )
    }

    /// Tastatur im offenen Startmenü: Tippen filtert, Pfeile wählen,
    /// Enter startet. Liefert die Start-Funktion der gewählten App —
    /// der Aufrufer führt sie NACH dem Loslassen des Locks aus!
    fn startmenue_taste(&mut self, taste: DecodedKey) -> Option<fn()> {
        self.start_menue.as_ref()?;

        // Enter: gewählte App starten und Menü schließen.
        if matches!(taste, DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r')) {
            let menue = self.start_menue.as_ref().unwrap();
            let start = crate::apps::filtern(&menue.suchtext)
                .get(menue.auswahl)
                .map(|app| app.start);
            if start.is_some() {
                self.start_menue = None;
                self.alles_dirty = true;
            }
            return start;
        }

        let menue = self.start_menue.as_mut().unwrap();
        match taste {
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                menue.suchtext.pop();
                menue.auswahl = 0;
            }
            DecodedKey::RawKey(pc_keyboard::KeyCode::ArrowUp) => {
                menue.auswahl = menue.auswahl.saturating_sub(1);
            }
            DecodedKey::RawKey(pc_keyboard::KeyCode::ArrowDown) => {
                let anzahl = crate::apps::filtern(&menue.suchtext).len();
                if menue.auswahl + 1 < anzahl {
                    menue.auswahl += 1;
                }
            }
            DecodedKey::Unicode(zeichen) if zeichen >= ' ' => {
                if menue.suchtext.chars().count() < 24 {
                    menue.suchtext.push(zeichen);
                    menue.auswahl = 0;
                }
            }
            _ => return None,
        }
        self.alles_dirty = true;
        None
    }

    /// Klick bei offenem Startmenü. Liefert wie startmenue_taste ggf.
    /// eine Start-Funktion nach draußen.
    fn startmenue_klick(&mut self, px: i32, py: i32) -> Option<fn()> {
        let suchtext = self.start_menue.as_ref()?.suchtext.clone();
        let gefiltert = crate::apps::filtern(&suchtext);
        let rect = self.menue_rechteck(gefiltert.len());

        if rect.enthaelt(px, py) {
            for (i, app) in gefiltert.iter().enumerate() {
                if Self::menue_eintrag_rechteck(rect, i).enthaelt(px, py) {
                    self.start_menue = None;
                    self.alles_dirty = true;
                    return Some(app.start);
                }
            }
            // Klick ins Menü, aber auf keinen Eintrag (Suchfeld, Rand):
            return None;
        }

        // Klick AUSSERHALB schließt das Menü und wird verschluckt —
        // auch auf dem Startknopf (das ist der Toggle-Effekt).
        self.startmenue_schliessen();
        None
    }

    /// Maus-Hover im offenen Menü wählt den Eintrag unter dem Cursor.
    fn startmenue_hover(&mut self, px: i32, py: i32) {
        let suchtext = match &self.start_menue {
            Some(menue) => menue.suchtext.clone(),
            None => return,
        };
        let anzahl = crate::apps::filtern(&suchtext).len();
        let rect = self.menue_rechteck(anzahl);
        for i in 0..anzahl {
            if Self::menue_eintrag_rechteck(rect, i).enthaelt(px, py) {
                let menue = self.start_menue.as_mut().unwrap();
                if menue.auswahl != i {
                    menue.auswahl = i;
                    self.alles_dirty = true;
                }
                return;
            }
        }
    }

    /// Resize-Kante am Punkt (nur unterhalb der Titelzeile relevant).
    fn kante_bei(fenster: &Fenster, px: i32, py: i32) -> Option<Kante> {
        let r = fenster.gesamt_rechteck();
        let links = px >= r.x && px < r.x + METRIK.rand;
        let rechts = px < r.x + r.breite && px >= r.x + r.breite - METRIK.rand;
        let unten = py < r.y + r.hoehe && py >= r.y + r.hoehe - METRIK.rand;
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

    /// Liefert ggf. eine App-Start-Funktion (aus dem Startmenü) —
    /// der Aufrufer MUSS sie erst nach dem Loslassen des MANAGER-Locks
    /// ausführen (Deadlock-Regel: Start-Funktionen nehmen den Lock).
    pub fn maus_event(&mut self, event: &MausEvent, px: i32, py: i32) -> Option<fn()> {
        match event {
            MausEvent::Gedrueckt(MausTaste::Links) => return self.maus_gedrueckt(px, py),
            MausEvent::Losgelassen(MausTaste::Links) => self.maus_losgelassen(px, py),
            MausEvent::Bewegt { x, y } => self.maus_bewegt(*x, *y),
            _ => {}
        }
        None
    }

    fn maus_gedrueckt(&mut self, px: i32, py: i32) -> Option<fn()> {
        // Ein offenes Startmenü fängt ALLE Klicks ab (liegt zuoberst).
        if self.start_menue.is_some() {
            return self.startmenue_klick(px, py);
        }
        // Die Taskleiste liegt IMMER obenauf — sie fängt Klicks vor
        // allen Fenstern ab.
        if py >= self.taskleiste_y() {
            self.taskleiste_klick(px, py);
            return None;
        }
        let id = self.fenster_unter(px, py)?;
        self.fokussieren_und_heben(id);
        // "fenster" ist jetzt garantiert das letzte Element.
        let index = self.fenster.len() - 1;

        if self.fenster[index].in_titelzeile(px, py) {
            if let Some(knopf) = self.fenster[index].knopf_bei(px, py) {
                self.knopf_aktion(id, knopf);
                return None;
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
        }
        None
    }

    fn maus_losgelassen(&mut self, _px: i32, _py: i32) {
        // Snap anwenden, wenn während des Ziehens die Vorschau lief —
        // so ist das Ergebnis konsistent mit dem, was der Nutzer sah
        // (unabhängig von der exakten Cursor-Position beim Loslassen).
        if let Interaktion::Verschieben { id, .. } = self.interaktion {
            if self.snap_hinweis != 0 {
                self.snappen(id, self.snap_hinweis);
            }
        }
        self.interaktion = Interaktion::Keine;
        self.snap_hinweis = 0;
        self.alles_dirty = true;
    }

    fn maus_bewegt(&mut self, x: i32, y: i32) {
        match &self.interaktion {
            Interaktion::Verschieben { id, griff_dx, griff_dy } => {
                let (id, dx, dy) = (*id, *griff_dx, *griff_dy);
                if let Some(index) = self.index_von(id) {
                    let bb = self.bildschirm_breite;
                    // Die Titelzeile bleibt immer über der Taskleiste greifbar:
                    let max_y = self.taskleiste_y() - METRIK.titel_hoehe;
                    let f = &mut self.fenster[index];
                    f.x = (x - dx).clamp(-(f.breite()) + 80, bb - 80);
                    f.y = (y - dy).clamp(0, max_y);
                    // Snap-Vorschau:
                    self.snap_hinweis = if x <= METRIK.snap_rand {
                        -1
                    } else if x >= bb - METRIK.snap_rand {
                        1
                    } else {
                        0
                    };
                    self.alles_dirty = true;
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
                if nb < METRIK.min_fenster_breite as i32 {
                    if matches!(kante, Kante::Links | Kante::UntenLinks) {
                        nx = start_x + (start_breite - METRIK.min_fenster_breite as i32);
                    }
                    nb = METRIK.min_fenster_breite as i32;
                }
                nh = nh.max(METRIK.min_fenster_hoehe as i32);

                if let Some(index) = self.index_von(id) {
                    let f = &mut self.fenster[index];
                    f.x = nx;
                    f.y = ny;
                    f.groesse_setzen(nb as usize, nh as usize);
                    inhalt_zeichnen(f);
                    self.alles_dirty = true;
                }
            }
            Interaktion::Keine => {
                // Hover im offenen Startmenü wählt den Eintrag darunter:
                if self.start_menue.is_some() {
                    self.startmenue_hover(x, y);
                }
                self.cursor_aktualisieren(x, y);
            }
        }
    }

    fn knopf_aktion(&mut self, id: FensterId, knopf: Knopf) {
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
                    // Fenster (und sein Puffer-Vec) wird hier gedroppt —
                    // der Heap-Speicher geht sauber zurück.
                    self.fenster.remove(index);
                }
                self.fokus_neu_bestimmen();
                self.alles_dirty = true;
            }
        }
    }

    fn maximieren(&mut self, id: FensterId) {
        let bb = self.bildschirm_breite;
        let bh = self.bildschirm_hoehe;
        if let Some(index) = self.index_von(id) {
            let f = &mut self.fenster[index];
            f.vorher = Some((f.x, f.y, f.puffer.breite, f.puffer.hoehe));
            f.x = 0;
            f.y = 0;
            let breite = bb.max(METRIK.min_fenster_breite as i32) as usize;
            let hoehe = (bh - METRIK.titel_hoehe - METRIK.taskleiste_hoehe).max(METRIK.min_fenster_hoehe as i32) as usize;
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
                        f.y = (py - METRIK.titel_hoehe / 2).max(0);
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
            let breite = (bb / 2).max(METRIK.min_fenster_breite as i32) as usize;
            let hoehe = (bh - METRIK.titel_hoehe - METRIK.taskleiste_hoehe).max(METRIK.min_fenster_hoehe as i32) as usize;
            f.x = if seite < 0 { 0 } else { bb / 2 };
            f.y = 0;
            f.groesse_setzen(breite, hoehe);
            inhalt_zeichnen(f);
        }
        self.alles_dirty = true;
    }

    pub fn taste_event(&mut self, taste: DecodedKey) {
        let fokus = match self.fokus {
            Some(id) => id,
            None => return,
        };
        if let Some(index) = self.index_von(fokus) {
            let fenster = &mut self.fenster[index];
            if let Inhalt::TastaturEcho { text } = &mut fenster.inhalt {
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
                    _ => return,
                }
                inhalt_zeichnen(fenster);
                fenster.dirty = true;
            }
        }
    }

    // ----- Alt+Tab-Fensterwechsler -----

    fn switcher_weiter(&mut self) {
        match &mut self.switcher {
            Some(sw) => {
                if !sw.reihenfolge.is_empty() {
                    sw.auswahl = (sw.auswahl + 1) % sw.reihenfolge.len();
                }
            }
            None => {
                // Reihenfolge: oberstes zuerst (MRU), inkl. minimierte.
                let reihenfolge: Vec<FensterId> =
                    self.fenster.iter().rev().map(|f| f.id).collect();
                if !reihenfolge.is_empty() {
                    // Erster Tab wählt das NÄCHSTE Fenster.
                    let auswahl = if reihenfolge.len() > 1 { 1 } else { 0 };
                    self.switcher = Some(Switcher { reihenfolge, auswahl });
                }
            }
        }
        self.alles_dirty = true;
    }

    fn switcher_bestaetigen(&mut self) {
        if let Some(sw) = self.switcher.take() {
            if let Some(&id) = sw.reihenfolge.get(sw.auswahl) {
                if let Some(index) = self.index_von(id) {
                    self.fenster[index].minimiert = false;
                }
                self.fokussieren_und_heben(id);
            }
            self.alles_dirty = true;
        }
    }

    pub fn uhr_aktualisieren(&mut self) {
        for fenster in self.fenster.iter_mut() {
            if !fenster.minimiert && matches!(fenster.inhalt, Inhalt::Uhr) {
                inhalt_zeichnen(fenster);
                fenster.dirty = true;
            }
        }
        // Taskleisten-Uhr: nur neu komponieren, wenn die angezeigte
        // Sekunde wirklich gewechselt hat (der Uhr-Task läuft öfter).
        let sekunde = zeit::ms_seit_boot() / 1000;
        if sekunde != self.letzte_uhr_sekunde {
            self.letzte_uhr_sekunde = sekunde;
            self.alles_dirty = true;
        }
    }

    pub fn ist_dirty(&self) -> bool {
        self.alles_dirty || self.fenster.iter().any(|f| f.dirty)
    }

    fn dirty_zuruecksetzen(&mut self) {
        self.alles_dirty = false;
        for fenster in self.fenster.iter_mut() {
            fenster.dirty = false;
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

    fn komponieren(&self, fb: &mut framebuffer::DoppelPuffer) {
        let hoehe = fb.info().height;
        let thema = theme::aktuell();

        // 1. Desktop-Hintergrund: Aurora-Verlauf des Themes (schnell).
        for y in 0..hoehe {
            let t = (y * 255 / hoehe.max(1)) as u8;
            fb.zeile_fuellen(y, thema.desktop_oben.mischen(thema.desktop_unten, t));
        }

        let mut z = Zeichner::neu(fb);

        // 2. Snap-Vorschau (halbtransparente Hälfte):
        if self.snap_hinweis != 0 {
            let halb = self.bildschirm_breite / 2;
            let x = if self.snap_hinweis < 0 { 0 } else { halb };
            let akzent = thema.akzent;
            z.rechteck_fuellen(
                Rechteck::neu(x, 0, halb, self.bildschirm_hoehe - METRIK.taskleiste_hoehe),
                Rgba::mit_alpha(akzent.r, akzent.g, akzent.b, 60),
            );
        }

        // 3. Fenster von hinten nach vorne (minimierte überspringen):
        for fenster in self.fenster.iter() {
            if fenster.minimiert {
                continue;
            }
            let fokussiert = self.fokus == Some(fenster.id);
            fenster_komponieren(&mut z, fenster, fokussiert);
        }

        // 4. Taskleiste — IMMER im Vordergrund, deshalb NACH den Fenstern:
        self.taskleiste_zeichnen(&mut z);

        // 5. Startmenü (über der Taskleiste):
        if let Some(menue) = &self.start_menue {
            self.startmenue_zeichnen(&mut z, menue);
        }

        // 6. Alt+Tab-Overlay (ganz oben):
        if let Some(sw) = &self.switcher {
            self.switcher_zeichnen(&mut z, sw);
        }
    }

    /// Zeichnet das Startmenü: Panel im Aurora-Stil, Suchfeld oben,
    /// darunter die (gefilterte) App-Liste aus der Registry.
    fn startmenue_zeichnen<F: Zeichenflaeche>(&self, z: &mut Zeichner<'_, F>, menue: &StartMenue) {
        let thema = theme::aktuell();
        let gefiltert = crate::apps::filtern(&menue.suchtext);
        let rect = self.menue_rechteck(gefiltert.len());

        // Schatten, Panel, Akzent-Rahmen:
        z.rechteck_abgerundet(
            Rechteck::neu(rect.x + METRIK.abstand, rect.y + METRIK.abstand, rect.breite, rect.hoehe),
            METRIK.radius_gross,
            thema.schatten,
        );
        z.rechteck_abgerundet(rect, METRIK.radius_gross, thema.flaeche);
        z.rechteck_rahmen(rect, thema.akzent);

        // Suchfeld: getippter Text oder Platzhalter, dazu ein Cursor.
        let feld = Rechteck::neu(
            rect.x + METRIK.abstand,
            rect.y + METRIK.abstand,
            rect.breite - 2 * METRIK.abstand,
            METRIK.menue_suchfeld_hoehe - 2 * METRIK.abstand + 2,
        );
        z.rechteck_abgerundet(feld, METRIK.radius_klein, thema.eingabefeld);
        let feld_text_y = feld.y + (feld.hoehe - METRIK.zeilen_hoehe) / 2;
        if menue.suchtext.is_empty() {
            z.text(feld.x + 10, feld_text_y, "Suchen ...", METRIK.schrift_ui, FontWeight::Regular, thema.text_gedimmt);
        } else {
            let text = format!("{}_", menue.suchtext);
            z.text(feld.x + 10, feld_text_y, &text, METRIK.schrift_ui, FontWeight::Regular, thema.text_stark);
        }

        // Die App-Liste (jeder Eintrag: Icon + Name):
        if gefiltert.is_empty() {
            let leer = Self::menue_eintrag_rechteck(rect, 0);
            z.text(
                leer.x + 10,
                leer.y + (leer.hoehe - METRIK.zeilen_hoehe) / 2,
                "Keine Treffer",
                METRIK.schrift_ui,
                FontWeight::Regular,
                thema.text_gedimmt,
            );
            return;
        }
        for (i, app) in gefiltert.iter().enumerate() {
            let eintrag = Self::menue_eintrag_rechteck(rect, i);
            let gewaehlt = i == menue.auswahl;
            if gewaehlt {
                z.rechteck_abgerundet(eintrag, METRIK.radius_klein, thema.auswahl);
            }
            let text_y = eintrag.y + (eintrag.hoehe - METRIK.zeilen_hoehe) / 2;
            z.icon(eintrag.x + 10, text_y, app.icon, 1);
            z.text(
                eintrag.x + 38,
                text_y,
                app.name,
                METRIK.schrift_ui,
                FontWeight::Regular,
                if gewaehlt { thema.text_stark } else { thema.text_normal },
            );
        }
    }

    /// Zeichnet die Taskleiste: Startknopf | Fenster-Knöpfe | Systray
    /// (Platzhalter-Icons + Uhrzeit/Datum).
    fn taskleiste_zeichnen<F: Zeichenflaeche>(&self, z: &mut Zeichner<'_, F>) {
        let thema = theme::aktuell();
        let y = self.taskleiste_y();
        let breite = self.bildschirm_breite;
        let zeichen_breite = get_raster_width(FontWeight::Regular, METRIK.schrift_ui) as i32;

        // Leisten-Grund (leicht transparent — der Desktop schimmert durch):
        z.rechteck_fuellen(
            Rechteck::neu(0, y, breite, METRIK.taskleiste_hoehe),
            thema.leiste_hintergrund,
        );
        z.linie(0, y, breite - 1, y, thema.leiste_linie);

        // Startknopf: das SpeedOS-Logo (2x skaliert = 32 Pixel);
        // bei offenem Startmenü hervorgehoben.
        let start = self.start_knopf_rechteck();
        if self.start_menue.is_some() {
            z.rechteck_abgerundet(
                Rechteck::neu(start.x + 4, start.y + 3, start.breite - 8, start.hoehe - 6),
                METRIK.radius_klein,
                thema.leiste_knopf_aktiv,
            );
        }
        z.icon(start.x + (start.breite - 32) / 2, y + 4, &crate::grafik::ICON_LOGO, 2);

        // Ein Knopf pro offenem Fenster:
        for (id, rect) in self.taskleisten_knoepfe() {
            let fenster = match self.index_von(id) {
                Some(index) => &self.fenster[index],
                None => continue,
            };
            let aktiv = self.fokus == Some(id) && !fenster.minimiert;
            z.rechteck_abgerundet(
                rect,
                METRIK.radius_klein,
                if aktiv { thema.leiste_knopf_aktiv } else { thema.leiste_knopf },
            );
            if aktiv {
                // Akzent-Streifen unter dem fokussierten Fenster:
                z.rechteck_fuellen(
                    Rechteck::neu(rect.x + 6, rect.y + rect.hoehe - 3, rect.breite - 12, 2),
                    thema.akzent,
                );
            }
            let text_y = rect.y + (rect.hoehe - METRIK.zeilen_hoehe) / 2;
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
            z.text(rect.x + 28, text_y, &titel, METRIK.schrift_ui, FontWeight::Regular, farbe);
        }

        // Systray rechts: Platzhalter-Icons (echte Features folgen),
        // daneben Uhrzeit und Datum (aus Ticks — Kalibrierung: TODO).
        let systray_x = breite - METRIK.systray_breite;
        z.icon(systray_x, y + 12, &crate::grafik::ICON_ZAHNRAD, 1);
        z.icon(systray_x + 22, y + 12, &crate::grafik::ICON_ORDNER, 1);

        let jetzt = zeit::datum_uhrzeit();
        let uhr = format!("{:02}:{:02}:{:02}", jetzt.stunde, jetzt.minute, jetzt.sekunde);
        let datum = format!("{:02}.{:02}.{}", jetzt.tag, jetzt.monat, jetzt.jahr);
        let uhr_x = breite - METRIK.abstand - uhr.chars().count() as i32 * zeichen_breite;
        let datum_x = breite - METRIK.abstand - datum.chars().count() as i32 * zeichen_breite;
        z.text(uhr_x, y + 4, &uhr, METRIK.schrift_ui, FontWeight::Bold, thema.text_normal);
        z.text(datum_x, y + 21, &datum, METRIK.schrift_ui, FontWeight::Regular, thema.text_gedimmt);
    }

    fn switcher_zeichnen<F: Zeichenflaeche>(&self, z: &mut Zeichner<'_, F>, sw: &Switcher) {
        let thema = theme::aktuell();
        let n = sw.reihenfolge.len() as i32;
        let zeilen_h = 30;
        let box_b = 420;
        let box_h = 54 + n * zeilen_h;
        let bx = (self.bildschirm_breite - box_b) / 2;
        let by = (self.bildschirm_hoehe - box_h) / 2;

        z.rechteck_abgerundet(
            Rechteck::neu(bx + METRIK.abstand, by + METRIK.abstand, box_b, box_h),
            METRIK.radius_gross,
            thema.schatten,
        );
        z.rechteck_abgerundet(Rechteck::neu(bx, by, box_b, box_h), METRIK.radius_gross, thema.flaeche);
        z.rechteck_rahmen(Rechteck::neu(bx, by, box_b, box_h), thema.akzent);
        z.text(bx + 18, by + 14, "Fenster wechseln", METRIK.schrift_ui, FontWeight::Bold, thema.text_normal);

        for (i, id) in sw.reihenfolge.iter().enumerate() {
            let zy = by + 46 + i as i32 * zeilen_h;
            if i == sw.auswahl {
                z.rechteck_abgerundet(
                    Rechteck::neu(bx + 10, zy - 4, box_b - 20, zeilen_h - 2),
                    METRIK.radius_klein,
                    thema.auswahl,
                );
            }
            let (titel, icon, minimiert) = self
                .index_von(*id)
                .map(|idx| {
                    let f = &self.fenster[idx];
                    (f.titel.as_str(), inhalt_icon(&f.inhalt), f.minimiert)
                })
                .unwrap_or(("?", &crate::grafik::ICON_LOGO, false));
            let farbe = if i == sw.auswahl {
                thema.text_stark
            } else {
                thema.text_sekundaer
            };
            z.icon(bx + 16, zy, icon, 1);
            z.text(bx + 40, zy, titel, METRIK.schrift_ui, FontWeight::Regular, farbe);
            if minimiert {
                z.text(bx + box_b - 110, zy, "(minimiert)", METRIK.schrift_ui, FontWeight::Regular, thema.text_gedimmt);
            }
        }
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
    let titel_rect = Rechteck::neu(rect.x, rect.y, rect.breite, METRIK.titel_hoehe);
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
        METRIK.schrift_ui,
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

    // Inhalt (privater Puffer, 1:1):
    for zeile in 0..fenster.puffer.hoehe {
        let basis = zeile * fenster.puffer.breite;
        for spalte in 0..fenster.puffer.breite {
            let farbe = fenster.puffer.pixel[basis + spalte];
            z.pixel(
                rect.x + spalte as i32,
                rect.y + METRIK.titel_hoehe + zeile as i32,
                Rgba::neu(farbe.r, farbe.g, farbe.b),
            );
        }
    }
}

/// Das Icon, das zu einem Fenster-Inhalt gehört (Titelleiste,
/// Taskleiste und Alt+Tab zeigen dasselbe).
fn inhalt_icon(inhalt: &Inhalt) -> &'static crate::grafik::Icon {
    match inhalt {
        Inhalt::Terminal(_) => &crate::grafik::ICON_TERMINAL,
        Inhalt::Uhr => &crate::grafik::ICON_UHR,
        Inhalt::TastaturEcho { .. } => &crate::grafik::ICON_TASTATUR,
        Inhalt::Malflaeche { .. } => &crate::grafik::ICON_PINSEL,
    }
}

/// Zeichnet das Terminal-Raster in den Fenster-Puffer: Zellen-
/// Hintergründe, Zeichen (Antialiasing via Alpha) und der Cursor-
/// Unterstrich in Akzentfarbe.
fn terminal_rendern(term: &terminal::Terminal, puffer: &mut FensterPuffer) {
    let thema = theme::aktuell();
    let hintergrund = thema.terminal_hintergrund;
    let zeichen_breite = get_raster_width(FontWeight::Regular, METRIK.schrift_ui) as i32;
    let zeilen_hoehe = METRIK.zeilen_hoehe;
    let (breite, hoehe) = (puffer.breite as i32, puffer.hoehe as i32);

    let mut z = Zeichner::neu(puffer);
    z.rechteck_fuellen(
        Rechteck::neu(0, 0, breite, hoehe),
        Rgba::neu(hintergrund.r, hintergrund.g, hintergrund.b),
    );
    let mut puffer_utf8 = [0u8; 4];
    for zeile in 0..term.zeilen() {
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
                    METRIK.schrift_ui,
                    FontWeight::Regular,
                    Rgba::neu(zelle.vg.r, zelle.vg.g, zelle.vg.b),
                );
            }
        }
    }
    // Der Terminal-Cursor (ruhig, nicht blinkend — der Konsolen-
    // Blink-Task ist im Desktop-Modus pausiert):
    let (cursor_spalte, cursor_zeile) = term.cursor();
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

/// Zeichnet den Inhalt eines Fensters in SEINEN Puffer.
fn inhalt_zeichnen(fenster: &mut Fenster) {
    // Terminal: Rastergröße an die Fenstergröße anpassen, dann rendern.
    // (&mut fenster.inhalt und &mut fenster.puffer sind verschiedene
    // Felder — der Borrow-Checker erlaubt beides gleichzeitig.)
    if let Inhalt::Terminal(term) = &mut fenster.inhalt {
        let zeichen_breite = get_raster_width(FontWeight::Regular, METRIK.schrift_ui);
        let spalten = (fenster.puffer.breite / zeichen_breite).max(1);
        let zeilen = (fenster.puffer.hoehe / METRIK.zeilen_hoehe as usize).max(1);
        term.groesse_setzen(spalten, zeilen);
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
        // Oben schon behandelt (früher return):
        Inhalt::Terminal(_) => {}
        Inhalt::Uhr => {
            let ticks = zeit::ticks();
            let ms = zeit::ms_seit_boot();
            z.text(20, 16, &format!("{} Ticks", ticks), METRIK.schrift_gross, FontWeight::Bold, thema.akzent_cyan);
            z.text(20, 60, &format!("Uptime: {},{:03} s", ms / 1000, ms % 1000), METRIK.schrift_ui, FontWeight::Regular, thema.text_normal);
            z.text(20, 90, "(aktualisiert sich live)", METRIK.schrift_ui, FontWeight::Regular, thema.text_gedimmt);
        }
        Inhalt::TastaturEcho { text } => {
            z.text(20, 12, "Tippe (bei Fokus!):", METRIK.schrift_ui, FontWeight::Regular, thema.text_sekundaer);
            z.rechteck_abgerundet(Rechteck::neu(16, 40, breite - 32, 40), METRIK.radius_klein, thema.eingabefeld);
            z.text(26, 50, text, METRIK.schrift_ui, FontWeight::Regular, thema.text_stark);
            z.text(20, 96, "Enter leert, Backspace loescht", METRIK.schrift_ui, FontWeight::Regular, thema.text_gedimmt);
        }
        Inhalt::Malflaeche { klicks } => {
            z.verlauf_vertikal(
                Rechteck::neu(0, 0, breite, 60),
                thema.titel_aktiv_oben.mischen(thema.inhalt_hintergrund, 128),
                thema.inhalt_hintergrund,
            );
            z.text(16, 8, "Statische Grafik + Klicks", METRIK.schrift_ui, FontWeight::Bold, thema.text_stark);
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
        // Heap passend zur Auflösung wachsen lassen: ein maximiertes
        // Fenster braucht einen fast bildschirmgroßen Puffer
        // (Breite*Höhe*3 Bytes). Wir reservieren Platz für ~3 solche
        // Puffer plus Reserve — bei 2560x1600 sind das ~37 MiB.
        let noetig_bytes = info.width * info.height * 3 * 3;
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
    // Eine vom Startmenü gewählte App wird ERST HIER gestartet —
    // nach dem Loslassen des MANAGER-Locks (die Start-Funktion nimmt
    // ihn selbst wieder, sonst gäbe es einen Deadlock).
    let aktion = mit_manager(|m| m.maus_event(event, px, py)).flatten();
    if let Some(start) = aktion {
        start();
    }
}

pub fn taste_event(taste: DecodedKey) {
    let _ = mit_manager(|m| m.taste_event(taste));
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
    let aktion = mit_manager(|m| m.startmenue_taste(taste)).flatten();
    if let Some(start) = aktion {
        start();
    }
}

// ----- Terminal (die SpeedShell als Fenster) -----

/// Öffnet das Terminal-Fenster (oder holt es nach vorn).
/// Liefert true, wenn es NEU erstellt wurde — dann sollte der
/// Aufrufer per shell::prompt_nachholen() einen Prompt hineindrucken.
pub fn terminal_oeffnen() -> bool {
    mit_manager(|m| m.terminal_oeffnen()).unwrap_or(false)
}

/// Existiert (irgendwo, auch minimiert) ein Terminal-Fenster?
pub fn terminal_vorhanden() -> bool {
    mit_manager(|m| m.terminal_index().is_some()).unwrap_or(false)
}

/// Ist das fokussierte Fenster das Terminal? Dann verarbeitet die
/// Shell Tasten selbst (ZeilenEditor), statt sie ans Fenster zu geben.
pub fn terminal_fokussiert() -> bool {
    mit_manager(|m| {
        m.fokus
            .and_then(|id| m.index_von(id))
            .map(|i| matches!(m.fenster[i].inhalt, Inhalt::Terminal(_)) && !m.fenster[i].minimiert)
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Umleitung von konsole::_print im Desktop-Modus: formatierten Text
/// ins Terminal-Fenster schreiben. false = kein Terminal offen.
pub fn terminal_schreiben(args: core::fmt::Arguments, vg: Farbe, hg: Farbe) -> bool {
    mit_manager(|m| m.terminal_schreiben(args, vg, hg)).unwrap_or(false)
}

/// Leert das Terminal (clear-Befehl im Desktop-Modus).
pub fn terminal_leeren() {
    let _ = mit_manager(|m| m.terminal_leeren());
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

/// Wechselt das Theme und zeichnet ALLE Fenster neu (Inhalte nutzen
/// Theme-Farben, deshalb reicht alles_dirty allein nicht).
pub fn theme_wechseln() {
    crate::theme::umschalten();
    let _ = mit_manager(|m| {
        for index in 0..m.fenster.len() {
            inhalt_zeichnen(&mut m.fenster[index]);
        }
        m.alles_dirty = true;
    });
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
        let dirty = mit_manager(|m| {
            // Geänderte Inhalte (z. B. Terminal-Ausgabe) EINMAL pro
            // Frame in die Fenster-Puffer rendern:
            m.inhalte_rendern();
            let dirty = m.ist_dirty();
            if dirty {
                m.dirty_zuruecksetzen();
            }
            dirty
        })
        .unwrap_or(false);
        if !dirty {
            continue;
        }

        framebuffer::mit_framebuffer(|fb| {
            // Lock-Ordnung: FRAMEBUFFER -> MANAGER.
            x86_64::instructions::interrupts::without_interrupts(|| {
                if let Some(manager) = MANAGER.lock().as_ref() {
                    manager.komponieren(fb);
                }
            });
            fb.present();
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
        assert_eq!(manager.fenster[index].hoehe(), 800 - METRIK.titel_hoehe - METRIK.taskleiste_hoehe);
    }

    /// Terminal: einmal öffnen, danach nur noch fokussieren; Schreiben
    /// landet im Raster, inhalte_rendern setzt das Render-Flag zurück.
    #[test_case]
    fn test_terminal_oeffnen_und_schreiben() {
        let mut manager = FensterManager::neu(1000, 800);
        assert!(manager.terminal_oeffnen()); // neu erstellt
        assert!(!manager.terminal_oeffnen()); // schon da -> nur fokussiert
        let index = manager.terminal_index().unwrap();

        let vg = Farbe::neu(200, 200, 200);
        let hg = Farbe::neu(0, 0, 0);
        assert!(manager.terminal_schreiben(format_args!("hi"), vg, hg));
        if let Inhalt::Terminal(term) = &manager.fenster[index].inhalt {
            assert_eq!(term.zelle(0, 0).zeichen, 'h');
            assert_eq!(term.zelle(1, 0).zeichen, 'i');
        } else {
            panic!("Terminal-Fenster hat keinen Terminal-Inhalt");
        }
        assert!(manager.fenster[index].inhalt_neu);
        manager.inhalte_rendern();
        assert!(!manager.fenster[index].inhalt_neu);

        // Ohne Terminal schlägt das Schreiben "sauber" fehl:
        let mut leer = FensterManager::neu(1000, 800);
        assert!(!leer.terminal_schreiben(format_args!("x"), vg, hg));
    }

    /// Startmenü: Umschalten, Suchtext filtert, Enter liefert die
    /// Start-Funktion des Treffers nach draußen (Deadlock-Regel).
    #[test_case]
    fn test_startmenue_suche_und_start() {
        let (mut manager, _, _) = test_manager();
        manager.startmenue_umschalten();
        assert!(manager.start_menue.is_some());

        // "uhr" tippen -> genau ein Treffer, Enter startet ihn:
        for zeichen in "uhr".chars() {
            assert!(manager.startmenue_taste(DecodedKey::Unicode(zeichen)).is_none());
        }
        let aktion = manager.startmenue_taste(DecodedKey::Unicode('\n'));
        assert!(aktion.is_some());
        assert!(manager.start_menue.is_none()); // Menü hat sich geschlossen

        // Ohne Treffer startet Enter nichts (Menü bleibt offen):
        manager.startmenue_umschalten();
        for zeichen in "xyz".chars() {
            manager.startmenue_taste(DecodedKey::Unicode(zeichen));
        }
        assert!(manager.startmenue_taste(DecodedKey::Unicode('\n')).is_none());
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
