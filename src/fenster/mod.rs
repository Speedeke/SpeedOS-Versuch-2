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
use noto_sans_mono_bitmap::FontWeight;
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

/// Die Demo-Inhalte der Test-Fenster.
pub enum Inhalt {
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

pub struct FensterManager {
    /// Z-Ordnung: LETZTES Element = ganz vorne.
    fenster: Vec<Fenster>,
    fokus: Option<FensterId>,
    interaktion: Interaktion,
    /// Snap-Vorschau während des Verschiebens (-1 links, +1 rechts).
    snap_hinweis: i8,
    switcher: Option<Switcher>,
    alles_dirty: bool,
    bildschirm_breite: i32,
    bildschirm_hoehe: i32,
}

impl FensterManager {
    pub fn neu(bildschirm_breite: i32, bildschirm_hoehe: i32) -> Self {
        FensterManager {
            fenster: Vec::new(),
            fokus: None,
            interaktion: Interaktion::Keine,
            snap_hinweis: 0,
            switcher: None,
            alles_dirty: true,
            bildschirm_breite,
            bildschirm_hoehe,
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

    pub fn maus_event(&mut self, event: &MausEvent, px: i32, py: i32) {
        match event {
            MausEvent::Gedrueckt(MausTaste::Links) => self.maus_gedrueckt(px, py),
            MausEvent::Losgelassen(MausTaste::Links) => self.maus_losgelassen(px, py),
            MausEvent::Bewegt { x, y } => self.maus_bewegt(*x, *y),
            _ => {}
        }
    }

    fn maus_gedrueckt(&mut self, px: i32, py: i32) {
        let id = match self.fenster_unter(px, py) {
            Some(id) => id,
            None => return,
        };
        self.fokussieren_und_heben(id);
        // "fenster" ist jetzt garantiert das letzte Element.
        let index = self.fenster.len() - 1;

        if self.fenster[index].in_titelzeile(px, py) {
            if let Some(knopf) = self.fenster[index].knopf_bei(px, py) {
                self.knopf_aktion(id, knopf);
                return;
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
                    let (bb, bh) = (self.bildschirm_breite, self.bildschirm_hoehe);
                    let f = &mut self.fenster[index];
                    f.x = (x - dx).clamp(-(f.breite()) + 80, bb - 80);
                    f.y = (y - dy).clamp(0, bh - METRIK.titel_hoehe);
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

        // 4. Alt+Tab-Overlay:
        if let Some(sw) = &self.switcher {
            self.switcher_zeichnen(&mut z, sw);
        }
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
        Inhalt::Uhr => &crate::grafik::ICON_UHR,
        Inhalt::TastaturEcho { .. } => &crate::grafik::ICON_TASTATUR,
        Inhalt::Malflaeche { .. } => &crate::grafik::ICON_PINSEL,
    }
}

/// Zeichnet den Demo-Inhalt eines Fensters in SEINEN Puffer.
fn inhalt_zeichnen(fenster: &mut Fenster) {
    let thema = theme::aktuell();
    let breite = fenster.puffer.breite as i32;
    let hoehe = fenster.puffer.hoehe as i32;
    let mut z = Zeichner::neu(&mut fenster.puffer);
    z.rechteck_fuellen(
        Rechteck::neu(0, 0, breite, hoehe),
        Rgba::neu(thema.inhalt_hintergrund.r, thema.inhalt_hintergrund.g, thema.inhalt_hintergrund.b),
    );

    match &fenster.inhalt {
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

        let mut manager = FensterManager::neu(info.width as i32, info.height as i32);
        manager.fenster_erstellen("Uhr", 140, 120, 420, 150, Inhalt::Uhr);
        manager.fenster_erstellen(
            "Tastatur", 420, 320, 560, 140,
            Inhalt::TastaturEcho { text: String::new() },
        );
        manager.fenster_erstellen(
            "Grafik", 820, 200, 380, 220,
            Inhalt::Malflaeche { klicks: Vec::new() },
        );
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
    let _ = mit_manager(|m| m.maus_event(event, px, py));
}

pub fn taste_event(taste: DecodedKey) {
    let _ = mit_manager(|m| m.taste_event(taste));
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
