// grafik.rs — Die Zeichen-Werkzeuge: Formen, Verläufe, Alpha, Icons
//
// Aus diesen Primitiven bestehen später alle Fenster, Buttons und
// Apps. Kernstück ist der `Zeichner`: Er kapselt den Back-Buffer,
// prüft bei JEDEM Pixel das optionale Clip-Rechteck (damit ein
// Fenster nie über seine Grenzen malt) und beherrscht Alpha-Blending
// (halbtransparente Flächen für Schatten und den Aurora-Look).
//
// Die Mathematik (Clipping-Schnitt, Alpha-Mischung) ist bewusst in
// reine Funktionen ausgelagert — die testen wir als Unit-Tests, ganz
// ohne Bildschirm.

use crate::framebuffer::{DoppelPuffer, Farbe};
use crate::maus::{MausEvent, MausTaste};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use noto_sans_mono_bitmap::{get_raster, FontWeight, RasterHeight};

// ---------------------------------------------------------------------------
// Farben mit Alpha
// ---------------------------------------------------------------------------

/// Eine RGB-Farbe mit Alpha-Kanal (Deckkraft): 255 = voll deckend,
/// 0 = unsichtbar. Halbtransparente Flächen entstehen, indem die
/// neue Farbe mit dem vorhandenen Pixel gemischt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// Voll deckende Farbe.
    pub const fn neu(r: u8, g: u8, b: u8) -> Self {
        Rgba { r, g, b, a: 255 }
    }

    /// Farbe mit Deckkraft.
    pub const fn mit_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba { r, g, b, a }
    }

    /// Nur der RGB-Anteil (fürs Mischen / Verläufe).
    pub const fn rgb(self) -> Farbe {
        Farbe::neu(self.r, self.g, self.b)
    }
}

/// DIE Alpha-Formel (reine Funktion, unit-getestet):
/// Ergebnis = Untergrund * (255 - a)/255 + Farbe * a/255, pro Kanal.
pub fn alpha_mischen(untergrund: Farbe, farbe: Rgba) -> Farbe {
    let a = farbe.a as u32;
    let m = |unten: u8, oben: u8| -> u8 {
        ((unten as u32 * (255 - a) + oben as u32 * a) / 255) as u8
    };
    Farbe::neu(
        m(untergrund.r, farbe.r),
        m(untergrund.g, farbe.g),
        m(untergrund.b, farbe.b),
    )
}

// ---------------------------------------------------------------------------
// Rechtecke und Clipping
// ---------------------------------------------------------------------------

/// Ein Rechteck in Pixelkoordinaten. i32, damit auch (teilweise)
/// außerhalb des Bildschirms liegende Formen erlaubt sind — das
/// Clipping schneidet sie einfach ab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rechteck {
    pub x: i32,
    pub y: i32,
    pub breite: i32,
    pub hoehe: i32,
}

impl Rechteck {
    pub const fn neu(x: i32, y: i32, breite: i32, hoehe: i32) -> Self {
        Rechteck { x, y, breite, hoehe }
    }

    /// Liegt der Punkt (x, y) innerhalb?
    pub fn enthaelt(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.breite && y < self.y + self.hoehe
    }

    /// Schnittmenge zweier Rechtecke (None = keine Überlappung).
    /// DAS Herzstück des Clippings — reine Funktion, unit-getestet.
    pub fn schneiden(&self, anderes: &Rechteck) -> Option<Rechteck> {
        let x = self.x.max(anderes.x);
        let y = self.y.max(anderes.y);
        let rechts = (self.x + self.breite).min(anderes.x + anderes.breite);
        let unten = (self.y + self.hoehe).min(anderes.y + anderes.hoehe);
        if x < rechts && y < unten {
            Some(Rechteck::neu(x, y, rechts - x, unten - y))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Der Zeichner — generisch über jede Zeichenfläche
// ---------------------------------------------------------------------------

/// Alles, worauf man zeichnen kann: der Bildschirm-Back-Buffer
/// genauso wie der private Pixel-Puffer eines Fensters. Der Zeichner
/// funktioniert auf beiden identisch — Apps malen mit denselben
/// Primitiven in ihr Fenster wie der Compositor auf den Bildschirm.
pub trait Zeichenflaeche {
    fn flaeche_breite(&self) -> usize;
    fn flaeche_hoehe(&self) -> usize;
    fn flaeche_setzen(&mut self, x: usize, y: usize, farbe: Farbe);
    fn flaeche_lesen(&self, x: usize, y: usize) -> Option<Farbe>;
}

impl Zeichenflaeche for DoppelPuffer {
    fn flaeche_breite(&self) -> usize {
        self.info().width
    }
    fn flaeche_hoehe(&self) -> usize {
        self.info().height
    }
    fn flaeche_setzen(&mut self, x: usize, y: usize, farbe: Farbe) {
        self.pixel_setzen(x, y, farbe);
    }
    fn flaeche_lesen(&self, x: usize, y: usize) -> Option<Farbe> {
        self.pixel_lesen(x, y)
    }
}

/// Zeichnet auf eine Zeichenfläche — mit Clipping und Alpha.
/// Erzeugen mit `Zeichner::neu(flaeche)`, dann Methoden aufrufen
/// (beim Bildschirm zum Schluss `fb.present()` nicht vergessen!).
pub struct Zeichner<'a, F: Zeichenflaeche> {
    fb: &'a mut F,
    /// Optionales Clip-Rechteck: Außerhalb wird NICHTS gezeichnet.
    clip: Option<Rechteck>,
}

impl<'a, F: Zeichenflaeche> Zeichner<'a, F> {
    pub fn neu(fb: &'a mut F) -> Self {
        Zeichner { fb, clip: None }
    }

    /// Setzt das Clip-Rechteck für alle folgenden Operationen.
    pub fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.clip = clip;
    }

    /// Der eine Pixel-Pfad, durch den ALLES läuft: Clip prüfen,
    /// bei Alpha < 255 mit dem Untergrund mischen, setzen.
    pub fn pixel(&mut self, x: i32, y: i32, farbe: Rgba) {
        if x < 0 || y < 0 {
            return;
        }
        if let Some(clip) = &self.clip {
            if !clip.enthaelt(x, y) {
                return;
            }
        }
        let (ux, uy) = (x as usize, y as usize);
        if ux >= self.fb.flaeche_breite() || uy >= self.fb.flaeche_hoehe() {
            return;
        }
        let endgueltig = if farbe.a == 255 {
            farbe.rgb()
        } else {
            match self.fb.flaeche_lesen(ux, uy) {
                Some(untergrund) => alpha_mischen(untergrund, farbe),
                None => return, // außerhalb der Fläche
            }
        };
        self.fb.flaeche_setzen(ux, uy, endgueltig);
    }

    /// Linie von (x0,y0) nach (x1,y1) — Bresenham-Algorithmus:
    /// kommt komplett ohne Fließkomma aus (wir haben ja kein SSE!).
    pub fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Rgba) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut fehler = dx + dy;
        loop {
            self.pixel(x, y, farbe);
            if x == x1 && y == y1 {
                break;
            }
            let f2 = 2 * fehler;
            if f2 >= dy {
                fehler += dy;
                x += sx;
            }
            if f2 <= dx {
                fehler += dx;
                y += sy;
            }
        }
    }

    /// Gefülltes Rechteck.
    pub fn rechteck_fuellen(&mut self, rect: Rechteck, farbe: Rgba) {
        for y in rect.y..rect.y + rect.hoehe {
            for x in rect.x..rect.x + rect.breite {
                self.pixel(x, y, farbe);
            }
        }
    }

    /// Rechteck-Rahmen (1 Pixel dick).
    pub fn rechteck_rahmen(&mut self, rect: Rechteck, farbe: Rgba) {
        let (x1, y1) = (rect.x + rect.breite - 1, rect.y + rect.hoehe - 1);
        self.linie(rect.x, rect.y, x1, rect.y, farbe);
        self.linie(rect.x, y1, x1, y1, farbe);
        self.linie(rect.x, rect.y, rect.x, y1, farbe);
        self.linie(x1, rect.y, x1, y1, farbe);
    }

    /// Gefülltes Rechteck mit abgerundeten Ecken (Radius in Pixeln) —
    /// die Grundform jedes Fensters und Buttons.
    pub fn rechteck_abgerundet(&mut self, rect: Rechteck, radius: i32, farbe: Rgba) {
        let radius = radius.min(rect.breite / 2).min(rect.hoehe / 2);
        for y in rect.y..rect.y + rect.hoehe {
            for x in rect.x..rect.x + rect.breite {
                // Abstand zur nächsten Ecke prüfen: In den vier
                // Eck-Quadraten gilt die Kreisgleichung.
                let ex = if x < rect.x + radius {
                    rect.x + radius - 1 - x
                } else if x >= rect.x + rect.breite - radius {
                    x - (rect.x + rect.breite - radius)
                } else {
                    -1
                };
                let ey = if y < rect.y + radius {
                    rect.y + radius - 1 - y
                } else if y >= rect.y + rect.hoehe - radius {
                    y - (rect.y + rect.hoehe - radius)
                } else {
                    -1
                };
                if ex >= 0 && ey >= 0 && ex * ex + ey * ey > radius * radius {
                    continue; // außerhalb der Ecken-Rundung
                }
                self.pixel(x, y, farbe);
            }
        }
    }

    /// Gefüllter Kreis um (cx, cy).
    pub fn kreis_fuellen(&mut self, cx: i32, cy: i32, radius: i32, farbe: Rgba) {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= radius * radius {
                    self.pixel(cx + dx, cy + dy, farbe);
                }
            }
        }
    }

    /// Kreis-Rahmen — Midpoint-Algorithmus (nur Ganzzahlen).
    pub fn kreis_rahmen(&mut self, cx: i32, cy: i32, radius: i32, farbe: Rgba) {
        let mut x = radius;
        let mut y = 0;
        let mut fehler = 1 - radius;
        while x >= y {
            // Die 8 Spiegelungen des berechneten Oktanten:
            for (px, py) in [
                (x, y), (y, x), (-y, x), (-x, y),
                (-x, -y), (-y, -x), (y, -x), (x, -y),
            ] {
                self.pixel(cx + px, cy + py, farbe);
            }
            y += 1;
            if fehler < 0 {
                fehler += 2 * y + 1;
            } else {
                x -= 1;
                fehler += 2 * (y - x) + 1;
            }
        }
    }

    /// Vertikaler Farbverlauf über ein Rechteck (oben -> unten) —
    /// für Titelleisten und den Desktop-Hintergrund.
    pub fn verlauf_vertikal(&mut self, rect: Rechteck, oben: Farbe, unten: Farbe) {
        for zeile in 0..rect.hoehe {
            let t = if rect.hoehe > 1 {
                (zeile * 255 / (rect.hoehe - 1)) as u8
            } else {
                0
            };
            let farbe = oben.mischen(unten, t);
            let rgba = Rgba::neu(farbe.r, farbe.g, farbe.b);
            for x in rect.x..rect.x + rect.breite {
                self.pixel(x, rect.y + zeile, rgba);
            }
        }
    }

    /// Text an beliebiger Pixelposition. Die Glyphen werden per
    /// Intensität ALPHA-geblendet — Text funktioniert damit auf
    /// jedem Untergrund (Verläufe, Bilder), ohne Hintergrund-Farbe.
    pub fn text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        groesse: RasterHeight,
        gewicht: FontWeight,
        farbe: Rgba,
    ) {
        let mut cx = x;
        for zeichen in text.chars() {
            let raster = match get_raster(zeichen, gewicht, groesse)
                .or_else(|| get_raster('?', gewicht, groesse))
            {
                Some(r) => r,
                None => continue,
            };
            for (dy, zeile) in raster.raster().iter().enumerate() {
                for (dx, &intensitaet) in zeile.iter().enumerate() {
                    if intensitaet == 0 {
                        continue;
                    }
                    // Glyph-Intensität und Farb-Alpha kombinieren:
                    let a = (intensitaet as u16 * farbe.a as u16 / 255) as u8;
                    self.pixel(
                        cx + dx as i32,
                        y + dy as i32,
                        Rgba::mit_alpha(farbe.r, farbe.g, farbe.b, a),
                    );
                }
            }
            cx += raster.width() as i32;
        }
    }

    /// Bitmap-Blitting: kopiert ein Pixel-Array (zeilenweise,
    /// breite*hoehe Einträge) an Position (x, y). Pixel in der
    /// `transparent`-Farbe werden übersprungen (einfache Transparenz
    /// wie bei klassischen Sprites).
    pub fn blit(
        &mut self,
        x: i32,
        y: i32,
        breite: usize,
        pixel: &[Farbe],
        transparent: Option<Farbe>,
    ) {
        for (i, &farbe) in pixel.iter().enumerate() {
            if Some(farbe) == transparent {
                continue;
            }
            let dx = (i % breite) as i32;
            let dy = (i / breite) as i32;
            self.pixel(x + dx, y + dy, Rgba::neu(farbe.r, farbe.g, farbe.b));
        }
    }

    /// Zeichnet ein eingebettetes Icon (skaliert um den Faktor
    /// `skalierung`, 1 = 16x16 Pixel).
    pub fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32) {
        for (zeile, text) in icon.zeilen.iter().enumerate() {
            for (spalte, zeichen) in text.chars().enumerate() {
                let farbe = match icon_palette(zeichen) {
                    Some(f) => f,
                    None => continue, // transparent
                };
                // Jedes Icon-"Pixel" als skaliertes Quadrat:
                let basis_x = x + spalte as i32 * skalierung;
                let basis_y = y + zeile as i32 * skalierung;
                for dy in 0..skalierung {
                    for dx in 0..skalierung {
                        self.pixel(basis_x + dx, basis_y + dy, farbe);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Das eingebettete Icon-Format
//
// Icons sind 16x16-ASCII-Art-Konstanten: gut lesbar im Code, neue
// Icons braucht man nur zu "malen". Jedes Zeichen steht für eine
// Farbe der gemeinsamen Palette, '.' ist transparent.
// ---------------------------------------------------------------------------

/// Ein 16x16-Icon (Zeilen aus Palette-Zeichen).
pub struct Icon {
    pub zeilen: [&'static str; 16],
}

/// Die gemeinsame Icon-Palette (Obsidian-Aurora-Töne).
fn icon_palette(zeichen: char) -> Option<Rgba> {
    match zeichen {
        '.' => None, // transparent
        'w' => Some(Rgba::neu(0xf8, 0xfa, 0xfc)), // Weiß
        'h' => Some(Rgba::neu(0xc4, 0xca, 0xd6)), // Hellgrau
        'd' => Some(Rgba::neu(0x56, 0x5f, 0x73)), // Dunkelgrau
        'D' => Some(Rgba::neu(0x1a, 0x20, 0x29)), // Fast-Schwarz
        'g' => Some(Rgba::neu(0xb4, 0x53, 0x09)), // Gold dunkel
        'G' => Some(Rgba::neu(0xfb, 0xbf, 0x24)), // Gold hell
        'v' => Some(Rgba::neu(0x7c, 0x3a, 0xed)), // Aurora-Violett
        'b' => Some(Rgba::neu(0x3b, 0x82, 0xf6)), // Aurora-Blau
        'c' => Some(Rgba::neu(0x22, 0xd3, 0xee)), // Aurora-Cyan
        _ => Some(Rgba::neu(0xff, 0x00, 0xff)),   // auffälliges Magenta = Tippfehler im Icon!
    }
}

/// Ordner (goldene Mappe mit Reiter).
pub static ICON_ORDNER: Icon = Icon {
    zeilen: [
        "................",
        ".gggggg.........",
        ".gGGGGgg........",
        ".gGGGGGggggggg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".gGGGGGGGGGGGg..",
        ".ggggggggggggg..",
        "................",
        "................",
        "................",
    ],
};

/// Datei (weißes Blatt mit Eselsohr und Textzeilen).
pub static ICON_DATEI: Icon = Icon {
    zeilen: [
        "................",
        "...wwwwwwww.....",
        "...whhhhhhww....",
        "...whhhhhhwhw...",
        "...whhhhhhwwww..",
        "...whhhhhhhhhw..",
        "...wddddddhhhw..",
        "...whhhhhhhhhw..",
        "...wddddddddhw..",
        "...whhhhhhhhhw..",
        "...wddddddddhw..",
        "...whhhhhhhhhw..",
        "...wdddddhhhhw..",
        "...wwwwwwwwwww..",
        "................",
        "................",
    ],
};

/// Zahnrad (Einstellungen).
pub static ICON_ZAHNRAD: Icon = Icon {
    zeilen: [
        "................",
        "......dd........",
        "...dd.dd.dd.....",
        "...dddddddd.....",
        "....dddddddd....",
        "..ddddhhdddd....",
        "..dddhhhhddddd..",
        "....ddhhhhdd....",
        "....ddhhhhdd....",
        "..dddhhhhddddd..",
        "..ddddhhdddd....",
        "....dddddddd....",
        "...dddddddd.....",
        "...dd.dd.dd.....",
        "......dd........",
        "................",
    ],
};

/// Das SpeedOS-Logo: ein "S" im Aurora-Verlauf auf dunkler Kachel.
pub static ICON_LOGO: Icon = Icon {
    zeilen: [
        ".DDDDDDDDDDDDD..",
        "DDDDDDDDDDDDDDD.",
        "DDDvvvvvvvvvDDD.",
        "DDvvvDDDDDDDDDD.",
        "DDvvvDDDDDDDDDD.",
        "DDDvvvvbbDDDDDD.",
        "DDDDDbbbbbbDDDD.",
        "DDDDDDDbbbbbDDD.",
        "DDDDDDDDDbbbDDD.",
        "DDDDDDDDDDcccDD.",
        "DDDDDDDDDDcccDD.",
        "DDcccccccccccDD.",
        "DDDcccccccccDDD.",
        "DDDDDDDDDDDDDDD.",
        ".DDDDDDDDDDDDD..",
        "................",
    ],
};

/// Terminal (dunkle Kachel mit Eingabe-Prompt ">_").
pub static ICON_TERMINAL: Icon = Icon {
    zeilen: [
        "................",
        ".ddddddddddddd..",
        ".dDDDDDDDDDDDd..",
        ".dDcDDDDDDDDDd..",
        ".dDDcDDDDDDDDd..",
        ".dDDDcDDDDDDDd..",
        ".dDDDDcDDDDDDd..",
        ".dDDDcDDDDDDDd..",
        ".dDDcDDDDDDDDd..",
        ".dDcDDDDDDDDDd..",
        ".dDDDDDDDDDDDd..",
        ".dDDDDDwwwwDDd..",
        ".dDDDDDDDDDDDd..",
        ".ddddddddddddd..",
        "................",
        "................",
    ],
};

/// Uhr (helles Zifferblatt mit Zeigern).
pub static ICON_UHR: Icon = Icon {
    zeilen: [
        "................",
        ".....ddddd......",
        "...ddhhhhhdd....",
        "..dhhhhhhhhhd...",
        ".dhhhhhDhhhhhd..",
        "dhhhhhhDhhhhhhd.",
        "dhhhhhhDhhhhhhd.",
        "dhhhhhhDDDhhhhd.",
        "dhhhhhhhhhhhhhd.",
        ".dhhhhhhhhhhhd..",
        ".dhhhhhhhhhhhd..",
        "..dhhhhhhhhhd...",
        "...ddhhhhhdd....",
        ".....ddddd......",
        "................",
        "................",
    ],
};

/// Tastatur (dunkle Platte mit hellen Tasten).
pub static ICON_TASTATUR: Icon = Icon {
    zeilen: [
        "................",
        "................",
        "................",
        "................",
        ".ddddddddddddd..",
        ".dDwDwDwDwDwDd..",
        ".dDDDDDDDDDDDd..",
        ".dDwDwDwDwDwDd..",
        ".dDDDDDDDDDDDd..",
        ".dDwwwwwwwwwDd..",
        ".dDDDDDDDDDDDd..",
        ".ddddddddddddd..",
        "................",
        "................",
        "................",
        "................",
    ],
};

/// Pinsel (goldener Stiel, cyanfarbene Spitze).
pub static ICON_PINSEL: Icon = Icon {
    zeilen: [
        "................",
        "...........gg...",
        "..........gGg...",
        ".........gGg....",
        "........gGg.....",
        ".......gGg......",
        "......gGg.......",
        ".....gGg........",
        "....wgg.........",
        "...www..........",
        "..cww...........",
        ".ccw............",
        ".cc.............",
        "................",
        "................",
        "................",
    ],
};

/// Neustart (Power-Symbol: offener Kreis mit senkrechtem Strich).
pub static ICON_NEUSTART: Icon = Icon {
    zeilen: [
        "................",
        "................",
        ".......cc.......",
        ".......cc.......",
        "....cc.cc.cc....",
        "...c...cc...c...",
        "..c....cc....c..",
        "..c..........c..",
        "..c..........c..",
        "..c..........c..",
        "...c........c...",
        "....cc....cc....",
        "......cccc......",
        "................",
        "................",
        "................",
    ],
};

/// Farbpalette / Theme (zweigeteilter Kreis: dunkel + hell).
pub static ICON_THEME: Icon = Icon {
    zeilen: [
        "................",
        ".....ddddd......",
        "...ddDDDwwdd....",
        "..dDDDDDwwwwd...",
        ".dDDDDDDwwwwwd..",
        "dDDDDDDDwwwwwwd.",
        "dDDDDDDDwwwwwwd.",
        "dDDDDDDDwwwwwwd.",
        "dDDDDDDDwwwwwwd.",
        ".dDDDDDDwwwwwd..",
        ".dDDDDDDwwwwwd..",
        "..dDDDDDwwwwd...",
        "...ddDDDwwdd....",
        ".....ddddd......",
        "................",
        "................",
    ],
};

// ---------------------------------------------------------------------------
// Der grafiktest-Demo-Modus
// ---------------------------------------------------------------------------

/// Ist gerade die Grafik-Demo auf dem Bildschirm? Die Shell schaut
/// hier nach: Nächste Taste = zurück zur Konsole.
static DEMO_AKTIV: AtomicBool = AtomicBool::new(false);

pub fn demo_aktiv() -> bool {
    DEMO_AKTIV.load(Ordering::Relaxed)
}

pub fn demo_beenden() {
    DEMO_AKTIV.store(false, Ordering::Relaxed);
}

/// Zeichnet die komplette Primitive-Schau (für den grafiktest-Befehl).
pub fn demo_zeichnen() {
    use crate::framebuffer::mit_framebuffer;

    DEMO_AKTIV.store(true, Ordering::Relaxed);
    crate::konsole::cursor_pausieren();

    mit_framebuffer(|fb| {
        let breite = fb.info().width as i32;
        let hoehe = fb.info().height as i32;
        let mut z = Zeichner::neu(fb);

        // Desktop-Hintergrund: dunkler vertikaler Aurora-Verlauf.
        z.verlauf_vertikal(
            Rechteck::neu(0, 0, breite, hoehe),
            Farbe::neu(0x14, 0x10, 0x2b), // dunkles Violett oben
            Farbe::neu(0x0b, 0x0e, 0x14), // Obsidian unten
        );

        // Überschrift.
        z.text(
            40, 24,
            "SpeedOS Grafiktest - Formen, Verlaeufe, Alpha, Icons",
            RasterHeight::Size32, FontWeight::Bold,
            Rgba::neu(0xf8, 0xfa, 0xfc),
        );

        // --- Formen-Reihe ---
        z.text(40, 90, "Formen:", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        z.rechteck_fuellen(Rechteck::neu(40, 115, 140, 90), Rgba::neu(0x3b, 0x82, 0xf6));
        z.rechteck_rahmen(Rechteck::neu(200, 115, 140, 90), Rgba::neu(0x22, 0xd3, 0xee));
        z.rechteck_abgerundet(Rechteck::neu(360, 115, 140, 90), 18, Rgba::neu(0x7c, 0x3a, 0xed));
        z.kreis_fuellen(590, 160, 45, Rgba::neu(0x22, 0xc5, 0x5e));
        z.kreis_rahmen(700, 160, 45, Rgba::neu(0xfb, 0xbf, 0x24));
        // Linien-Fächer:
        for i in 0..8 {
            z.linie(790, 205, 790 + i * 18, 115, Rgba::neu(0xfc, 0xa5, 0xa5));
        }

        // --- Verläufe (wie Titelleisten) ---
        z.text(40, 235, "Verlaeufe:", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        z.verlauf_vertikal(
            Rechteck::neu(40, 260, 300, 40),
            Farbe::neu(0x7c, 0x3a, 0xed),
            Farbe::neu(0x3b, 0x82, 0xf6),
        );
        z.verlauf_vertikal(
            Rechteck::neu(360, 260, 300, 40),
            Farbe::neu(0x3b, 0x82, 0xf6),
            Farbe::neu(0x22, 0xd3, 0xee),
        );
        z.text(52, 270, "Titelleiste", RasterHeight::Size16, FontWeight::Bold, Rgba::neu(0xf8, 0xfa, 0xfc));

        // --- Alpha-Blending: überlappende halbtransparente Flächen ---
        z.text(40, 330, "Alpha-Blending (50% / 33%):", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        z.rechteck_abgerundet(Rechteck::neu(40, 355, 180, 110), 12, Rgba::mit_alpha(0x7c, 0x3a, 0xed, 128));
        z.rechteck_abgerundet(Rechteck::neu(130, 385, 180, 110), 12, Rgba::mit_alpha(0x22, 0xd3, 0xee, 128));
        z.kreis_fuellen(390, 410, 55, Rgba::mit_alpha(0xfb, 0xbf, 0x24, 85));
        z.kreis_fuellen(440, 410, 55, Rgba::mit_alpha(0xef, 0x44, 0x44, 85));
        // Fensterschatten-Vorschau: dunkler Alpha-Rand unter einer Kachel.
        z.rechteck_abgerundet(Rechteck::neu(560, 370, 200, 90), 10, Rgba::mit_alpha(0, 0, 0, 100));
        z.rechteck_abgerundet(Rechteck::neu(552, 362, 200, 90), 10, Rgba::neu(0x1a, 0x20, 0x29));
        z.text(568, 390, "Fenster?", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0xc4, 0xca, 0xd6));

        // --- Icons ---
        z.text(40, 505, "Icons (2x skaliert):", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        z.icon(40, 530, &ICON_ORDNER, 2);
        z.icon(90, 530, &ICON_DATEI, 2);
        z.icon(140, 530, &ICON_ZAHNRAD, 2);
        z.icon(190, 530, &ICON_LOGO, 2);

        // --- Blitting: Schachbrett-Kachel mit transparenter Farbe ---
        z.text(300, 505, "Blit (mit Transparenz):", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        let magenta = Farbe::neu(0xff, 0x00, 0xff); // die "Loch"-Farbe
        let mut kachel = [Farbe::neu(0, 0, 0); 32 * 32];
        for (i, pixel) in kachel.iter_mut().enumerate() {
            let (x, y) = (i % 32, i / 32);
            *pixel = if (x / 8 + y / 8) % 2 == 0 {
                Farbe::neu(0x93, 0xc5, 0xfd)
            } else {
                magenta // wird beim Blitten übersprungen -> Verlauf scheint durch
            };
        }
        z.blit(300, 530, 32, &kachel, Some(magenta));
        z.blit(340, 530, 32, &kachel, Some(magenta));

        // --- Clipping-Beweis: Kreis, der nur im Fenster erscheint ---
        z.text(500, 505, "Clipping:", RasterHeight::Size16, FontWeight::Regular, Rgba::neu(0x8a, 0x91, 0xa3));
        let clip = Rechteck::neu(500, 530, 120, 60);
        z.rechteck_rahmen(clip, Rgba::neu(0x56, 0x5f, 0x73));
        z.clip_setzen(Some(clip));
        // Dieser Kreis ist viel größer als das Clip-Rechteck —
        // sichtbar wird nur der Ausschnitt darin:
        z.kreis_fuellen(560, 560, 70, Rgba::mit_alpha(0x22, 0xd3, 0xee, 180));
        z.clip_setzen(None);

        // Fußzeile.
        z.text(
            40, hoehe - 40,
            "Maus: Klick = Punkt, Rad = Farbe  |  Beliebige Taste: zurueck zur Konsole",
            RasterHeight::Size16, FontWeight::Regular,
            Rgba::neu(0x8a, 0x91, 0xa3),
        );

        fb.present();
    });

    // Farbanzeige fürs Malen initial zeichnen:
    demo_farbfeld_zeichnen();
}

// ---------------------------------------------------------------------------
// Maus-Interaktion in der Demo: Klicken malt, das Rad wechselt die Farbe
// ---------------------------------------------------------------------------

/// Die Malfarben, durch die das Scrollrad blättert.
const DEMO_FARBEN: [(Rgba, &str); 6] = [
    (Rgba::neu(0x22, 0xd3, 0xee), "Cyan   "),
    (Rgba::neu(0x7c, 0x3a, 0xed), "Violett"),
    (Rgba::neu(0x3b, 0x82, 0xf6), "Blau   "),
    (Rgba::neu(0x22, 0xc5, 0x5e), "Gruen  "),
    (Rgba::neu(0xfb, 0xbf, 0x24), "Gelb   "),
    (Rgba::neu(0xef, 0x44, 0x44), "Rot    "),
];

/// Index der aktuellen Malfarbe.
static DEMO_FARBE: AtomicUsize = AtomicUsize::new(0);

/// Zeichnet die Farbanzeige unten rechts (aktuelle Malfarbe).
fn demo_farbfeld_zeichnen() {
    use crate::framebuffer::mit_framebuffer;

    let (farbe, name) = DEMO_FARBEN[DEMO_FARBE.load(Ordering::Relaxed) % DEMO_FARBEN.len()];
    mit_framebuffer(|fb| {
        let breite = fb.info().width as i32;
        let hoehe = fb.info().height as i32;
        let mut z = Zeichner::neu(fb);
        z.rechteck_abgerundet(Rechteck::neu(breite - 260, hoehe - 56, 32, 32), 6, farbe);
        // Namensfeld erst leeren (alter Name könnte länger gewesen sein):
        z.rechteck_fuellen(
            Rechteck::neu(breite - 220, hoehe - 52, 200, 24),
            Rgba::neu(0x0b, 0x0e, 0x14),
        );
        z.text(
            breite - 220, hoehe - 52,
            name,
            RasterHeight::Size16, FontWeight::Regular,
            Rgba::neu(0xc4, 0xca, 0xd6),
        );
        let y = (hoehe - 60).max(0) as usize;
        fb.present_zeilen(y, 44);
    });
}

/// Wird vom Maus-Task für jedes Event gerufen — tut nur im
/// Demo-Modus etwas: Linksklick malt einen Punkt an der Cursor-
/// Position, das Scrollrad wechselt die Malfarbe.
pub fn demo_maus_event(event: &MausEvent) {
    use crate::framebuffer::mit_framebuffer;

    if !demo_aktiv() {
        return;
    }
    match event {
        MausEvent::Gedrueckt(MausTaste::Links) => {
            let (x, y) = crate::maus::position();
            let (farbe, _) = DEMO_FARBEN[DEMO_FARBE.load(Ordering::Relaxed) % DEMO_FARBEN.len()];
            mit_framebuffer(|fb| {
                let mut z = Zeichner::neu(fb);
                z.kreis_fuellen(x, y, 8, farbe);
                // Nur den Fleck übertragen:
                fb.present_bereich(
                    (x - 9).max(0) as usize,
                    (y - 9).max(0) as usize,
                    20,
                    20,
                );
            });
        }
        MausEvent::Gescrollt(delta) => {
            let anzahl = DEMO_FARBEN.len() as i32;
            let alt = DEMO_FARBE.load(Ordering::Relaxed) as i32;
            let neu = (alt + *delta as i32).rem_euclid(anzahl) as usize;
            DEMO_FARBE.store(neu, Ordering::Relaxed);
            demo_farbfeld_zeichnen();
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests — reine Mathematik, kein Bildschirm nötig
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Alpha-Formel: 0 = Untergrund, 255 = Farbe, 128 ≈ Mitte.
    #[test_case]
    fn test_alpha_mischen() {
        let unten = Farbe::neu(0, 0, 0);
        let oben_voll = Rgba::neu(200, 100, 50);
        assert_eq!(alpha_mischen(unten, oben_voll), Farbe::neu(200, 100, 50));

        let unsichtbar = Rgba::mit_alpha(200, 100, 50, 0);
        assert_eq!(alpha_mischen(unten, unsichtbar), unten);

        // 50 % über Schwarz = halbe Werte (Ganzzahl-Rundung nach unten).
        let halb = Rgba::mit_alpha(200, 100, 50, 128);
        let ergebnis = alpha_mischen(unten, halb);
        assert_eq!(ergebnis, Farbe::neu(100, 50, 25));

        // 50 % Weiß über Weiß bleibt Weiß.
        let weiss = Farbe::neu(255, 255, 255);
        let halb_weiss = Rgba::mit_alpha(255, 255, 255, 128);
        assert_eq!(alpha_mischen(weiss, halb_weiss), weiss);
    }

    /// Clipping-Schnitt: Überlappung, Enthaltensein, kein Kontakt.
    #[test_case]
    fn test_rechteck_schneiden() {
        let a = Rechteck::neu(0, 0, 100, 100);
        // Teilweise Überlappung:
        let b = Rechteck::neu(50, 50, 100, 100);
        assert_eq!(a.schneiden(&b), Some(Rechteck::neu(50, 50, 50, 50)));
        // Komplett enthalten:
        let innen = Rechteck::neu(10, 20, 30, 40);
        assert_eq!(a.schneiden(&innen), Some(innen));
        // Kein Kontakt:
        let weit_weg = Rechteck::neu(200, 200, 10, 10);
        assert_eq!(a.schneiden(&weit_weg), None);
        // Nur Kante berührt (Breite 0) zählt NICHT als Schnitt:
        let kante = Rechteck::neu(100, 0, 50, 50);
        assert_eq!(a.schneiden(&kante), None);
    }

    /// enthaelt: Grenzen sind halb-offen ([x, x+breite)).
    #[test_case]
    fn test_rechteck_enthaelt() {
        let r = Rechteck::neu(10, 10, 5, 5);
        assert!(r.enthaelt(10, 10)); // obere linke Ecke: drin
        assert!(r.enthaelt(14, 14)); // letzte Zelle: drin
        assert!(!r.enthaelt(15, 10)); // rechts daneben: draußen
        assert!(!r.enthaelt(9, 10)); // links daneben: draußen
    }

    /// Alle Icons müssen exakt 16 Zeilen à 16 Zeichen haben und nur
    /// bekannte Palette-Zeichen benutzen (Magenta = Tippfehler).
    #[test_case]
    fn test_icons_wohlgeformt() {
        for icon in [
            &ICON_ORDNER, &ICON_DATEI, &ICON_ZAHNRAD, &ICON_LOGO,
            &ICON_TERMINAL, &ICON_UHR, &ICON_TASTATUR, &ICON_PINSEL,
            &ICON_NEUSTART, &ICON_THEME,
        ] {
            assert_eq!(icon.zeilen.len(), 16);
            for zeile in icon.zeilen.iter() {
                assert_eq!(zeile.chars().count(), 16, "Icon-Zeile hat falsche Breite");
                for zeichen in zeile.chars() {
                    let farbe = icon_palette(zeichen);
                    assert_ne!(
                        farbe,
                        Some(Rgba::neu(0xff, 0x00, 0xff)),
                        "Unbekanntes Palette-Zeichen '{}'",
                        zeichen
                    );
                }
            }
        }
    }
}
