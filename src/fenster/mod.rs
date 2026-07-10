// fenster/mod.rs — Fenster, WindowManager und Compositor: das Herz
//                  des SpeedOS-Desktops
//
// ARCHITEKTUR (siehe auch CLAUDE.md):
//   * Jedes Fenster = EIGENER Pixel-Puffer + Metadaten (Position,
//     Größe, Titel, Z-Ordnung über die Vec-Reihenfolge, Fokus).
//     Apps zeichnen NUR in ihren Puffer — nie auf den Bildschirm!
//   * Der Compositor-Task setzt pro Frame zusammen:
//     Desktop-Hintergrund -> Fenster in Z-Reihenfolge (hinten zuerst)
//     -> present() -> Maus-Cursor obenauf.
//     Dirty-Flags sorgen dafür, dass NUR komponiert wird, wenn sich
//     wirklich etwas geändert hat.
//   * Event-Routing: Maus-Events treffen das oberste Fenster unter
//     dem Cursor (in Fenster-Koordinaten umgerechnet); Klick holt es
//     nach vorn und fokussiert es. Tastatur-Events gehen ans
//     fokussierte Fenster. Drag an der Titelzeile verschiebt.
//
// Die Titel-/Griffzeile zeichnet der COMPOSITOR (Apps besitzen nur
// den Inhalt) — hübsche Titelleisten mit Knöpfen kommen später.

use crate::framebuffer::{self, Farbe};
use crate::grafik::{Rechteck, Rgba, Zeichenflaeche, Zeichner};
use crate::maus::{MausEvent, MausTaste};
use crate::zeit;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight};
use pc_keyboard::DecodedKey;
use spin::Mutex;

/// Höhe der Griff-/Titelzeile in Pixeln (Drag-Zone).
pub const TITEL_HOEHE: i32 = 28;

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

/// Der private Pixel-Puffer eines Fensters (nur der INHALT —
/// Titelzeile und Rahmen malt der Compositor drumherum).
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

/// Die Demo-Inhalte der drei Test-Fenster. (Sobald es echte Apps
/// gibt, wird hieraus ein Trait — für den Meilenstein reicht das Enum.)
pub enum Inhalt {
    /// Zeigt Ticks und Uptime, aktualisiert vom uhr_task.
    Uhr,
    /// Zeigt Tastatureingaben (bekommt Events, wenn fokussiert).
    TastaturEcho { text: String },
    /// Statische Grafik; Klicks setzen Markierungen (beweist die
    /// Umrechnung in Fenster-Koordinaten).
    Malflaeche { klicks: Vec<(i32, i32)> },
}

pub struct Fenster {
    pub id: FensterId,
    titel: String,
    /// Position der oberen linken Ecke (der TITELZEILE) am Bildschirm.
    x: i32,
    y: i32,
    puffer: FensterPuffer,
    inhalt: Inhalt,
    /// Muss der Inhalt neu komponiert werden?
    dirty: bool,
}

impl Fenster {
    /// Gesamtfläche inkl. Titelzeile (fürs Hit-Testing).
    fn gesamt_rechteck(&self) -> Rechteck {
        Rechteck::neu(
            self.x,
            self.y,
            self.puffer.breite as i32,
            TITEL_HOEHE + self.puffer.hoehe as i32,
        )
    }

    /// Liegt der Punkt in der Griff-/Titelzeile?
    fn in_titelzeile(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.puffer.breite as i32
            && py >= self.y
            && py < self.y + TITEL_HOEHE
    }

    /// Bildschirm- -> Fensterinhalts-Koordinaten (None = außerhalb).
    fn lokal(&self, px: i32, py: i32) -> Option<(i32, i32)> {
        let lx = px - self.x;
        let ly = py - self.y - TITEL_HOEHE;
        if lx >= 0 && ly >= 0 && lx < self.puffer.breite as i32 && ly < self.puffer.hoehe as i32 {
            Some((lx, ly))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Der WindowManager
// ---------------------------------------------------------------------------

/// Ein laufender Verschiebe-Vorgang (Drag an der Titelzeile).
struct DragZustand {
    id: FensterId,
    /// Wo im Fenster wurde gegriffen (damit es nicht "springt")?
    griff_dx: i32,
    griff_dy: i32,
}

pub struct FensterManager {
    /// Z-Ordnung über die Reihenfolge: LETZTES Element = ganz vorne.
    fenster: Vec<Fenster>,
    fokus: Option<FensterId>,
    drag: Option<DragZustand>,
    /// Alles neu komponieren (Hintergrund sichtbar geworden o. Ä.)?
    alles_dirty: bool,
    bildschirm_breite: i32,
    bildschirm_hoehe: i32,
}

impl FensterManager {
    pub fn neu(bildschirm_breite: i32, bildschirm_hoehe: i32) -> Self {
        FensterManager {
            fenster: Vec::new(),
            fokus: None,
            drag: None,
            alles_dirty: true,
            bildschirm_breite,
            bildschirm_hoehe,
        }
    }

    /// Erzeugt ein Fenster und legt es ganz nach vorne (mit Fokus).
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
            puffer: FensterPuffer::neu(breite, hoehe, INHALT_HINTERGRUND),
            inhalt,
            dirty: true,
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

    /// Das OBERSTE Fenster unter dem Punkt (Suche von vorne = hinten
    /// in der Vec nach vorne).
    pub fn fenster_unter(&self, px: i32, py: i32) -> Option<FensterId> {
        self.fenster
            .iter()
            .rev()
            .find(|f| f.gesamt_rechteck().enthaelt(px, py))
            .map(|f| f.id)
    }

    /// Holt ein Fenster nach ganz vorne und gibt ihm den Fokus.
    pub fn fokussieren_und_heben(&mut self, id: FensterId) {
        if let Some(index) = self.index_von(id) {
            let fenster = self.fenster.remove(index);
            self.fenster.push(fenster);
            if self.fokus != Some(id) || index != self.fenster.len() - 1 {
                self.alles_dirty = true; // Z-Ordnung/Fokus-Optik ändert sich
            }
            self.fokus = Some(id);
        }
    }

    /// Verarbeitet ein Maus-Event an Bildschirmposition (px, py).
    pub fn maus_event(&mut self, event: &MausEvent, px: i32, py: i32) {
        match event {
            MausEvent::Gedrueckt(MausTaste::Links) => {
                if let Some(id) = self.fenster_unter(px, py) {
                    self.fokussieren_und_heben(id);
                    let fenster = self.fenster.last_mut().unwrap();
                    if fenster.in_titelzeile(px, py) {
                        // Drag beginnen: Griffpunkt merken.
                        self.drag = Some(DragZustand {
                            id,
                            griff_dx: px - fenster.x,
                            griff_dy: py - fenster.y,
                        });
                    } else if let Some((lx, ly)) = fenster.lokal(px, py) {
                        // Klick in den Inhalt: an die "App" weiterreichen.
                        if let Inhalt::Malflaeche { klicks } = &mut fenster.inhalt {
                            klicks.push((lx, ly));
                            inhalt_zeichnen(fenster);
                            fenster.dirty = true;
                        }
                    }
                }
            }
            MausEvent::Losgelassen(MausTaste::Links) => {
                self.drag = None;
            }
            MausEvent::Bewegt { x, y } => {
                if let Some(drag) = &self.drag {
                    let (id, dx, dy) = (drag.id, drag.griff_dx, drag.griff_dy);
                    let (max_x, max_y) = (self.bildschirm_breite, self.bildschirm_hoehe);
                    if let Some(index) = self.index_von(id) {
                        let fenster = &mut self.fenster[index];
                        // Titelzeile muss greifbar bleiben: klemmen.
                        fenster.x = (x - dx)
                            .clamp(-(fenster.puffer.breite as i32) + 60, max_x - 60);
                        fenster.y = (y - dy).clamp(0, max_y - TITEL_HOEHE);
                        self.alles_dirty = true; // Hintergrund wird frei
                    }
                }
            }
            _ => {}
        }
    }

    /// Tastatur-Event ans fokussierte Fenster.
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
                        if text.chars().count() < 28 {
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

    /// Uhr-Fenster neu zeichnen (ruft der uhr_task periodisch).
    pub fn uhr_aktualisieren(&mut self) {
        for fenster in self.fenster.iter_mut() {
            if matches!(fenster.inhalt, Inhalt::Uhr) {
                inhalt_zeichnen(fenster);
                fenster.dirty = true;
            }
        }
    }

    /// Muss komponiert werden?
    pub fn ist_dirty(&self) -> bool {
        self.alles_dirty || self.fenster.iter().any(|f| f.dirty)
    }

    fn dirty_zuruecksetzen(&mut self) {
        self.alles_dirty = false;
        for fenster in self.fenster.iter_mut() {
            fenster.dirty = false;
        }
    }

    /// Bildschirm- in Fensterinhalts-Koordinaten (für Tests/Router).
    pub fn fenster_lokal(&self, id: FensterId, px: i32, py: i32) -> Option<(i32, i32)> {
        self.index_von(id)
            .and_then(|i| self.fenster[i].lokal(px, py))
    }

    /// Position eines Fensters (für Tests).
    pub fn fenster_position(&self, id: FensterId) -> Option<(i32, i32)> {
        self.index_von(id).map(|i| (self.fenster[i].x, self.fenster[i].y))
    }

    /// Das aktuell fokussierte Fenster (für Tests).
    pub fn fokus(&self) -> Option<FensterId> {
        self.fokus
    }

    /// Komponiert alles in den Back-Buffer (ohne present).
    fn komponieren(&self, fb: &mut framebuffer::DoppelPuffer) {
        // 1. Desktop-Hintergrund: Obsidian-Aurora-Verlauf, Zeile für
        //    Zeile über den SCHNELLEN Zeilen-Füller.
        let hoehe = fb.info().height;
        let oben = Farbe::neu(0x17, 0x12, 0x33); // dunkles Aurora-Violett
        let unten = Farbe::neu(0x0b, 0x0e, 0x14); // Obsidian
        for y in 0..hoehe {
            let t = (y * 255 / hoehe.max(1)) as u8;
            fb.zeile_fuellen(y, oben.mischen(unten, t));
        }

        {
            let mut z = Zeichner::neu(fb);
            z.text(
                24,
                (hoehe - 36) as i32,
                "SpeedOS Desktop  |  Fenster: Klick = Fokus, Titelzeile ziehen = verschieben  |  ESC = Konsole",
                RasterHeight::Size16,
                FontWeight::Regular,
                Rgba::mit_alpha(0xc4, 0xca, 0xd6, 200),
            );

            // 2. Fenster von hinten nach vorne:
            for fenster in self.fenster.iter() {
                let fokussiert = self.fokus == Some(fenster.id);
                fenster_komponieren(&mut z, fenster, fokussiert);
            }
        }
    }
}

/// Hintergrundfarbe frischer Fenster-Puffer.
const INHALT_HINTERGRUND: Farbe = Farbe::neu(0x12, 0x16, 0x20);

/// Zeichnet EIN Fenster (Schatten, Titelzeile, Rahmen, Inhalt) in
/// die Zielfläche.
fn fenster_komponieren<F: Zeichenflaeche>(z: &mut Zeichner<'_, F>, fenster: &Fenster, fokussiert: bool) {
    let rect = fenster.gesamt_rechteck();

    // Schatten: zwei halbtransparente Streifen rechts und unten.
    z.rechteck_fuellen(
        Rechteck::neu(rect.x + rect.breite, rect.y + 8, 8, rect.hoehe),
        Rgba::mit_alpha(0, 0, 0, 90),
    );
    z.rechteck_fuellen(
        Rechteck::neu(rect.x + 8, rect.y + rect.hoehe, rect.breite, 8),
        Rgba::mit_alpha(0, 0, 0, 90),
    );

    // Titel-/Griffzeile: fokussiert = Aurora-Verlauf, sonst gedeckt.
    let titel_rect = Rechteck::neu(rect.x, rect.y, rect.breite, TITEL_HOEHE);
    if fokussiert {
        z.verlauf_vertikal(titel_rect, Farbe::neu(0x5b, 0x2e, 0xc7), Farbe::neu(0x2a, 0x4a, 0x9e));
    } else {
        z.rechteck_fuellen(titel_rect, Rgba::neu(0x2a, 0x30, 0x3e));
    }
    z.text(
        rect.x + 10,
        rect.y + 5,
        &fenster.titel,
        RasterHeight::Size16,
        FontWeight::Bold,
        if fokussiert {
            Rgba::neu(0xf8, 0xfa, 0xfc)
        } else {
            Rgba::neu(0x8a, 0x91, 0xa3)
        },
    );

    // Rahmen um alles:
    z.rechteck_rahmen(
        rect,
        if fokussiert {
            Rgba::neu(0x7c, 0x3a, 0xed)
        } else {
            Rgba::neu(0x3a, 0x41, 0x52)
        },
    );

    // Inhalt: der private Fenster-Puffer, 1:1 kopiert.
    for zeile in 0..fenster.puffer.hoehe {
        for spalte in 0..fenster.puffer.breite {
            let farbe = fenster.puffer.pixel[zeile * fenster.puffer.breite + spalte];
            z.pixel(
                rect.x + spalte as i32,
                rect.y + TITEL_HOEHE + zeile as i32,
                Rgba::neu(farbe.r, farbe.g, farbe.b),
            );
        }
    }
}

/// Zeichnet den Demo-Inhalt eines Fensters in SEINEN Puffer.
/// (Die "App" — sie kennt nur ihren Puffer, nie den Bildschirm.)
fn inhalt_zeichnen(fenster: &mut Fenster) {
    let breite = fenster.puffer.breite as i32;
    let hoehe = fenster.puffer.hoehe as i32;
    let mut z = Zeichner::neu(&mut fenster.puffer);
    z.rechteck_fuellen(Rechteck::neu(0, 0, breite, hoehe), Rgba::neu(0x12, 0x16, 0x20));

    match &fenster.inhalt {
        Inhalt::Uhr => {
            let ticks = zeit::ticks();
            let ms = zeit::ms_seit_boot();
            z.text(
                20, 16,
                &format!("{} Ticks", ticks),
                RasterHeight::Size32, FontWeight::Bold,
                Rgba::neu(0x22, 0xd3, 0xee),
            );
            z.text(
                20, 60,
                &format!("Uptime: {},{:03} s", ms / 1000, ms % 1000),
                RasterHeight::Size16, FontWeight::Regular,
                Rgba::neu(0xc4, 0xca, 0xd6),
            );
            z.text(
                20, 90,
                "(aktualisiert sich live)",
                RasterHeight::Size16, FontWeight::Regular,
                Rgba::neu(0x56, 0x5f, 0x73),
            );
        }
        Inhalt::TastaturEcho { text } => {
            z.text(
                20, 12,
                "Tippe (bei Fokus!):",
                RasterHeight::Size16, FontWeight::Regular,
                Rgba::neu(0x8a, 0x91, 0xa3),
            );
            z.rechteck_abgerundet(Rechteck::neu(16, 40, breite - 32, 40), 8, Rgba::neu(0x1c, 0x22, 0x30));
            z.text(
                26, 50,
                text,
                RasterHeight::Size16, FontWeight::Regular,
                Rgba::neu(0xf8, 0xfa, 0xfc),
            );
            z.text(
                20, 96,
                "Enter leert, Backspace loescht",
                RasterHeight::Size16, FontWeight::Regular,
                Rgba::neu(0x56, 0x5f, 0x73),
            );
        }
        Inhalt::Malflaeche { klicks } => {
            // Statische Grafik: kleiner Verlauf + Formen ...
            z.verlauf_vertikal(
                Rechteck::neu(0, 0, breite, 60),
                Farbe::neu(0x2a, 0x1e, 0x52),
                Farbe::neu(0x12, 0x16, 0x20),
            );
            z.text(
                16, 8,
                "Statische Grafik + Klicks",
                RasterHeight::Size16, FontWeight::Bold,
                Rgba::neu(0xf8, 0xfa, 0xfc),
            );
            z.kreis_fuellen(50, 110, 28, Rgba::mit_alpha(0x7c, 0x3a, 0xed, 180));
            z.kreis_fuellen(90, 110, 28, Rgba::mit_alpha(0x22, 0xd3, 0xee, 180));
            z.rechteck_abgerundet(Rechteck::neu(140, 84, 90, 52), 10, Rgba::neu(0x22, 0xc5, 0x5e));
            z.icon(breite - 50, 76, &crate::grafik::ICON_LOGO, 2);
            // ... plus eine Markierung pro Klick (in FENSTER-Koordinaten!):
            for (kx, ky) in klicks.iter() {
                z.linie(kx - 6, *ky, kx + 6, *ky, Rgba::neu(0xfb, 0xbf, 0x24));
                z.linie(*kx, ky - 6, *kx, ky + 6, Rgba::neu(0xfb, 0xbf, 0x24));
                z.kreis_rahmen(*kx, *ky, 6, Rgba::neu(0xfb, 0xbf, 0x24));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Globaler Zugriff + Desktop-Modus
// ---------------------------------------------------------------------------

static MANAGER: Mutex<Option<FensterManager>> = Mutex::new(None);
static DESKTOP_AKTIV: AtomicBool = AtomicBool::new(false);

/// Läuft gerade der Desktop (Shell/Konsole schlafen dann)?
pub fn desktop_aktiv() -> bool {
    DESKTOP_AKTIV.load(Ordering::Relaxed)
}

fn mit_manager<T>(f: impl FnOnce(&mut FensterManager) -> T) -> Option<T> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        MANAGER.lock().as_mut().map(f)
    })
}

/// Startet den Desktop (Shell-Befehl `desktop`). Beim ersten Mal
/// werden Heap erweitert und die drei Demo-Fenster erzeugt — danach
/// überleben die Fenster das Verlassen (ESC) und kommen wieder.
pub fn desktop_starten() {
    let info = match framebuffer::mit_framebuffer(|fb| fb.info()) {
        Some(info) => info,
        None => return,
    };

    let erster_start = x86_64::instructions::interrupts::without_interrupts(|| {
        MANAGER.lock().is_none()
    });
    if erster_start {
        // Platz für die Fenster-Puffer (einmalig, großzügig 4 MiB):
        let _ = crate::allocator::heap_erweitern(1024);

        let mut manager = FensterManager::neu(info.width as i32, info.height as i32);
        manager.fenster_erstellen(
            "Uhr", 140, 120, 420, 150, Inhalt::Uhr,
        );
        manager.fenster_erstellen(
            "Tastatur", 420, 300, 520, 140,
            Inhalt::TastaturEcho { text: String::new() },
        );
        manager.fenster_erstellen(
            "Grafik", 760, 180, 380, 220,
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

/// Beendet den Desktop (ESC) — die Fenster bleiben für später erhalten.
pub fn desktop_beenden() {
    DESKTOP_AKTIV.store(false, Ordering::Relaxed);
}

/// Maus-Router (ruft der maus_task): Position kommt vom globalen
/// Maus-Zustand, das Event wandert zum Manager.
pub fn maus_event(event: &MausEvent) {
    if !desktop_aktiv() {
        return;
    }
    let (px, py) = crate::maus::position();
    let _ = mit_manager(|m| m.maus_event(event, px, py));
}

/// Tastatur-Router (ruft die Shell im Desktop-Modus).
pub fn taste_event(taste: DecodedKey) {
    let _ = mit_manager(|m| m.taste_event(taste));
}

// ---------------------------------------------------------------------------
// Die Tasks: Compositor und Uhr
// ---------------------------------------------------------------------------

/// Der Compositor: prüft ~20x pro Sekunde die Dirty-Flags und setzt
/// NUR DANN neu zusammen. Reihenfolge pro Frame: Hintergrund ->
/// Fenster (Z-Ordnung) -> present -> Cursor obenauf.
pub async fn compositor_task() {
    loop {
        zeit::warte_ms(50).await;
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
            // Lock-Ordnung: FRAMEBUFFER -> MANAGER (nirgendwo anders
            // werden beide zugleich gehalten — kein Deadlock möglich).
            x86_64::instructions::interrupts::without_interrupts(|| {
                if let Some(manager) = MANAGER.lock().as_ref() {
                    manager.komponieren(fb);
                }
            });
            fb.present();
        });
        // Cursor wieder obenauf (present hat ihn überschrieben):
        crate::maus::cursor_neu_zeichnen();
    }
}

/// Hält das Uhr-Fenster lebendig (2x pro Sekunde).
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

    /// Kleine Fenster (100x60) — die Logik ist größenunabhängig,
    /// aber der Test-Heap ist begrenzt.
    fn test_manager() -> (FensterManager, FensterId, FensterId) {
        let mut manager = FensterManager::neu(1000, 800);
        let hinten = manager.fenster_erstellen(
            "Hinten", 100, 100, 100, 60, Inhalt::Uhr,
        );
        let vorne = manager.fenster_erstellen(
            "Vorne", 150, 140, 100, 60,
            Inhalt::TastaturEcho { text: String::new() },
        );
        (manager, hinten, vorne)
    }

    /// Klick trifft das OBERSTE Fenster; Klick aufs hintere holt es
    /// nach vorne und fokussiert es.
    #[test_case]
    fn test_fokus_und_z_ordnung() {
        let (mut manager, hinten, vorne) = test_manager();
        // Beide überlappen bei (160, 150) -> das vordere gewinnt:
        assert_eq!(manager.fenster_unter(160, 150), Some(vorne));
        // (110, 110) liegt nur im hinteren:
        assert_eq!(manager.fenster_unter(110, 110), Some(hinten));
        // Klick dorthin: hinteres kommt nach vorn + Fokus.
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 110, 110);
        assert_eq!(manager.fokus(), Some(hinten));
        assert_eq!(manager.fenster_unter(160, 150), Some(hinten));
        // Daneben (Desktop): niemand.
        assert_eq!(manager.fenster_unter(900, 700), None);
    }

    /// Drag an der Titelzeile verschiebt das Fenster exakt ums
    /// Maus-Delta; Loslassen beendet den Drag.
    #[test_case]
    fn test_fenster_verschieben() {
        let (mut manager, _, vorne) = test_manager();
        // In der Titelzeile des vorderen Fensters (y=140..168) greifen:
        manager.maus_event(&MausEvent::Gedrueckt(MausTaste::Links), 160, 150);
        manager.maus_event(&MausEvent::Bewegt { x: 200, y: 220 }, 200, 220);
        assert_eq!(manager.fenster_position(vorne), Some((190, 210)));
        // Loslassen -> weitere Bewegung verschiebt NICHT mehr:
        manager.maus_event(&MausEvent::Losgelassen(MausTaste::Links), 200, 220);
        manager.maus_event(&MausEvent::Bewegt { x: 500, y: 500 }, 500, 500);
        assert_eq!(manager.fenster_position(vorne), Some((190, 210)));
    }

    /// Bildschirm- -> Fensterkoordinaten (Titelzeile zählt nicht
    /// zum Inhalt!), und Tasten landen nur im fokussierten Fenster.
    #[test_case]
    fn test_koordinaten_und_tastatur_routing() {
        let (mut manager, hinten, vorne) = test_manager();
        // Fenster "vorne" bei (150,140): Inhalt beginnt bei y=168.
        assert_eq!(manager.fenster_lokal(vorne, 160, 178), Some((10, 10)));
        // In der Titelzeile: KEINE Inhalts-Koordinate.
        assert_eq!(manager.fenster_lokal(vorne, 160, 150), None);

        // Tasten gehen ans fokussierte Echo-Fenster ("vorne"):
        manager.taste_event(DecodedKey::Unicode('h'));
        manager.taste_event(DecodedKey::Unicode('i'));
        let text = match &manager.fenster[manager.index_von(vorne).unwrap()].inhalt {
            Inhalt::TastaturEcho { text } => text.clone(),
            _ => panic!(),
        };
        assert_eq!(text, "hi");

        // Fokus aufs Uhr-Fenster: Tasten verpuffen (kein Panic).
        manager.fokussieren_und_heben(hinten);
        manager.taste_event(DecodedKey::Unicode('x'));
    }
}
