// framebuffer.rs — Der Grafik-Unterbau: Double Buffering + Text-Zeichnen
//
// Der Bootloader übergibt uns einen LINEAREN FRAMEBUFFER: ein Stück
// Speicher, in dem jedes Pixel des Bildschirms als 3-4 Bytes liegt.
// Direkt hineinzumalen wäre sichtbar langsam und würde flackern —
// Framebuffer-Speicher ist Hardware-Speicher (langsame Schreibzugriffe,
// niemals lesen!). Deshalb DOUBLE BUFFERING:
//
//   zeichnen -> Back-Buffer (normales RAM, schnell, darf gelesen werden)
//   present() -> EIN grosser Block-Kopiervorgang in den echten Framebuffer
//
// Der Back-Buffer kommt aus memory::allocate_pages — genau der
// zusammenhängende Speicher, für den wir den Bitmap-Frame-Allocator
// gebaut haben.

use crate::memory;
use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt;
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};
use spin::Mutex;

/// Eine RGB-Farbe (8 Bit pro Kanal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Farbe {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Farbe {
    pub const fn neu(r: u8, g: u8, b: u8) -> Self {
        Farbe { r, g, b }
    }

    /// Linear zwischen zwei Farben mischen (t: 0..=255).
    /// Braucht das Antialiasing des Fonts und die Farbverläufe.
    /// (Rechnung in i32 — delta * 255 sprengt i16!)
    pub fn mischen(self, ziel: Farbe, t: u8) -> Farbe {
        let m = |a: u8, b: u8| -> u8 {
            let (a, b) = (a as i32, b as i32);
            (a + (b - a) * t as i32 / 255) as u8
        };
        Farbe::neu(m(self.r, ziel.r), m(self.g, ziel.g), m(self.b, ziel.b))
    }
}

/// Der doppelt gepufferte Framebuffer.
pub struct DoppelPuffer {
    /// Der ECHTE Framebuffer (Hardware/MMIO) — nur Ziel von present().
    vorne: &'static mut [u8],
    /// Der Back-Buffer im RAM — hierauf zeichnet alles.
    hinten: &'static mut [u8],
    /// Hintergrund-Cache fürs Dirty-Rect-Compositing: BYTE-identisch
    /// zum Back-Buffer (gleiches Pixelformat!), damit die Wieder-
    /// herstellung ein reines memcpy pro Zeile ist. None, bis der
    /// Compositor ihn per hintergrund_uebernehmen() füllt.
    hintergrund: Option<&'static mut [u8]>,
    info: FrameBufferInfo,
}

impl DoppelPuffer {
    pub fn info(&self) -> FrameBufferInfo {
        self.info
    }

    /// Setzt ein Pixel im Back-Buffer (Koordinaten ausserhalb: ignoriert).
    pub fn pixel_setzen(&mut self, x: usize, y: usize, farbe: Farbe) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        // stride = Pixel pro Speicherzeile (kann breiter sein als width!)
        let offset = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let pixel = &mut self.hinten[offset..offset + self.info.bytes_per_pixel];
        match self.info.pixel_format {
            PixelFormat::Rgb => {
                pixel[0] = farbe.r;
                pixel[1] = farbe.g;
                pixel[2] = farbe.b;
            }
            PixelFormat::Bgr => {
                pixel[0] = farbe.b;
                pixel[1] = farbe.g;
                pixel[2] = farbe.r;
            }
            // Graustufe/unbekannt: Helligkeit als Mittelwert.
            _ => pixel.fill(((farbe.r as u16 + farbe.g as u16 + farbe.b as u16) / 3) as u8),
        }
    }

    /// Liest ein Pixel aus dem BACK-Buffer (der ist normales RAM —
    /// den echten Framebuffer darf man nie lesen!). Braucht das
    /// Alpha-Blending im grafik-Modul: Mischen geht nur, wenn man
    /// weiß, was schon da ist.
    pub fn pixel_lesen(&self, x: usize, y: usize) -> Option<Farbe> {
        if x >= self.info.width || y >= self.info.height {
            return None;
        }
        let offset = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let pixel = &self.hinten[offset..offset + self.info.bytes_per_pixel];
        Some(match self.info.pixel_format {
            PixelFormat::Rgb => Farbe::neu(pixel[0], pixel[1], pixel[2]),
            PixelFormat::Bgr => Farbe::neu(pixel[2], pixel[1], pixel[0]),
            _ => Farbe::neu(pixel[0], pixel[0], pixel[0]),
        })
    }

    /// Füllt eine komplette Pixelzeile SCHNELL: baut das Byte-Muster
    /// des ersten Pixels und vervielfältigt es per copy_within
    /// (verdoppelnd — O(log n) memcpy-Aufrufe statt Schleife über
    /// jeden Pixel). Der Compositor braucht das für den Hintergrund.
    pub fn zeile_fuellen(&mut self, y: usize, farbe: Farbe) {
        if y >= self.info.height {
            return;
        }
        let bpp = self.info.bytes_per_pixel;
        let von = y * self.info.stride * bpp;
        let breite_bytes = self.info.width * bpp;
        // Erstes Pixel setzen (übernimmt die Formatwandlung) ...
        self.pixel_setzen(0, y, farbe);
        // ... dann verdoppelnd über die Zeile kopieren:
        let zeile = &mut self.hinten[von..von + breite_bytes];
        let mut gefuellt = bpp;
        while gefuellt < breite_bytes {
            let kopieren = gefuellt.min(breite_bytes - gefuellt);
            zeile.copy_within(0..kopieren, gefuellt);
            gefuellt += kopieren;
        }
    }

    /// Füllt einen TEIL einer Pixelzeile schnell (Muster verdoppelnd
    /// kopieren statt Pixel für Pixel) — der Zeilen-Schnellpfad des
    /// Zeichners. Koordinaten außerhalb werden abgeschnitten.
    pub fn zeile_teil_fuellen(&mut self, x: usize, y: usize, breite: usize, farbe: Farbe) {
        if y >= self.info.height || x >= self.info.width {
            return;
        }
        let breite = breite.min(self.info.width - x);
        if breite == 0 {
            return;
        }
        let bpp = self.info.bytes_per_pixel;
        // Erstes Pixel setzen (übernimmt die Formatwandlung) ...
        self.pixel_setzen(x, y, farbe);
        // ... dann verdoppelnd über den Zeilenausschnitt kopieren:
        let von = (y * self.info.stride + x) * bpp;
        let ausschnitt = &mut self.hinten[von..von + breite * bpp];
        let mut gefuellt = bpp;
        while gefuellt < ausschnitt.len() {
            let kopieren = gefuellt.min(ausschnitt.len() - gefuellt);
            ausschnitt.copy_within(0..kopieren, gefuellt);
            gefuellt += kopieren;
        }
    }

    /// Kopiert eine fertige Farbzeile in den Back-Buffer — Format-
    /// wandlung EINMAL pro Zeile entscheiden, dann eng schleifen
    /// (der Blit-Schnellpfad für Fenster-Inhalte im Compositor).
    pub fn zeile_kopieren(&mut self, x: usize, y: usize, pixel: &[Farbe]) {
        if y >= self.info.height || x >= self.info.width {
            return;
        }
        let anzahl = pixel.len().min(self.info.width - x);
        let bpp = self.info.bytes_per_pixel;
        let von = (y * self.info.stride + x) * bpp;
        let ziel = &mut self.hinten[von..von + anzahl * bpp];
        match self.info.pixel_format {
            PixelFormat::Rgb => {
                for (ziel, farbe) in ziel.chunks_exact_mut(bpp).zip(pixel) {
                    ziel[0] = farbe.r;
                    ziel[1] = farbe.g;
                    ziel[2] = farbe.b;
                }
            }
            PixelFormat::Bgr => {
                for (ziel, farbe) in ziel.chunks_exact_mut(bpp).zip(pixel) {
                    ziel[0] = farbe.b;
                    ziel[1] = farbe.g;
                    ziel[2] = farbe.r;
                }
            }
            _ => {
                for (ziel, farbe) in ziel.chunks_exact_mut(bpp).zip(pixel) {
                    ziel.fill(((farbe.r as u16 + farbe.g as u16 + farbe.b as u16) / 3) as u8);
                }
            }
        }
    }

    /// Merkt sich den AKTUELLEN Back-Buffer-Inhalt als Hintergrund
    /// (einmal rufen, nachdem der Desktop-Verlauf gerendert wurde;
    /// alloziert den Cache beim ersten Mal).
    pub fn hintergrund_uebernehmen(&mut self) {
        if self.hintergrund.is_none() {
            let pages = self.info.byte_len.div_ceil(4096);
            let start = match crate::memory::allocate_pages(pages) {
                Ok(start) => start,
                Err(_) => return, // kein Speicher: Cache bleibt aus
            };
            // unsafe: allocate_pages hat genau diesen Bereich frisch
            // gemappt und niemand sonst kennt ihn — die Slice-
            // Erzeugung ist exklusiv (dasselbe Muster wie `hinten`).
            self.hintergrund = Some(unsafe {
                core::slice::from_raw_parts_mut(start.as_mut_ptr::<u8>(), self.info.byte_len)
            });
        }
        if let Some(cache) = &mut self.hintergrund {
            cache.copy_from_slice(self.hinten);
        }
    }

    /// Stellt den Hintergrund in einem Rechteck des Back-Buffers
    /// wieder her — ein memcpy pro Zeile (DER Dirty-Rect-Startpunkt).
    /// Ohne Cache (hintergrund_uebernehmen nie gerufen): no-op,
    /// der Aufrufer malt dann eben auf den alten Inhalt.
    pub fn hintergrund_wiederherstellen(&mut self, x: usize, y: usize, breite: usize, hoehe: usize) {
        let cache = match &self.hintergrund {
            Some(cache) => cache,
            None => return,
        };
        let bpp = self.info.bytes_per_pixel;
        let x = x.min(self.info.width);
        let breite = breite.min(self.info.width - x);
        for zeile in y..(y + hoehe).min(self.info.height) {
            let von = (zeile * self.info.stride + x) * bpp;
            let bis = von + breite * bpp;
            self.hinten[von..bis].copy_from_slice(&cache[von..bis]);
        }
    }

    /// Füllt den ganzen Back-Buffer mit einer Farbe.
    pub fn fuellen(&mut self, farbe: Farbe) {
        for y in 0..self.info.height {
            self.zeile_fuellen(y, farbe);
        }
    }

    /// Kopiert den kompletten Back-Buffer auf den Bildschirm.
    pub fn present(&mut self) {
        self.vorne.copy_from_slice(self.hinten);
    }

    /// Kopiert nur die Pixelzeilen [y, y+hoehe) auf den Bildschirm —
    /// z. B. fürs Cursor-Blinken, damit nicht ständig der ganze
    /// Bildschirm übertragen wird.
    pub fn present_zeilen(&mut self, y: usize, hoehe: usize) {
        let von = y.min(self.info.height) * self.info.stride * self.info.bytes_per_pixel;
        let bis = (y + hoehe).min(self.info.height) * self.info.stride * self.info.bytes_per_pixel;
        self.vorne[von..bis].copy_from_slice(&self.hinten[von..bis]);
    }

    /// Kopiert ein RECHTECK vom Back- in den Front-Buffer — stellt
    /// z. B. das Bild unter dem Maus-Cursor wieder her.
    pub fn present_bereich(&mut self, x: usize, y: usize, breite: usize, hoehe: usize) {
        let bpp = self.info.bytes_per_pixel;
        for zeile in y..(y + hoehe).min(self.info.height) {
            let von = (zeile * self.info.stride + x.min(self.info.width)) * bpp;
            let bis = (zeile * self.info.stride + (x + breite).min(self.info.width)) * bpp;
            if von < bis {
                self.vorne[von..bis].copy_from_slice(&self.hinten[von..bis]);
            }
        }
    }

    /// Setzt ein Pixel DIREKT im echten Framebuffer (Front-Buffer) —
    /// NUR für Overlays wie den Maus-Cursor! Der Back-Buffer bleibt
    /// unberührt und ist damit die "Wahrheit ohne Cursor": Das nächste
    /// present_bereich stellt den Untergrund automatisch wieder her.
    pub fn pixel_setzen_vorne(&mut self, x: usize, y: usize, farbe: Farbe) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let offset = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let pixel = &mut self.vorne[offset..offset + self.info.bytes_per_pixel];
        match self.info.pixel_format {
            PixelFormat::Rgb => {
                pixel[0] = farbe.r;
                pixel[1] = farbe.g;
                pixel[2] = farbe.b;
            }
            PixelFormat::Bgr => {
                pixel[0] = farbe.b;
                pixel[1] = farbe.g;
                pixel[2] = farbe.r;
            }
            _ => pixel.fill(((farbe.r as u16 + farbe.g as u16 + farbe.b as u16) / 3) as u8),
        }
    }

    /// Scrollt den Back-Buffer um `pixel_zeilen` nach oben (memmove —
    /// KEIN Neu-Rendern!) und füllt den freigewordenen Streifen unten.
    pub fn hochscrollen(&mut self, pixel_zeilen: usize, fuellfarbe: Farbe) {
        let zeilen_bytes = self.info.stride * self.info.bytes_per_pixel;
        let versatz = pixel_zeilen * zeilen_bytes;
        if versatz >= self.hinten.len() {
            self.fuellen(fuellfarbe);
            return;
        }
        // memmove innerhalb des Back-Buffers:
        self.hinten.copy_within(versatz.., 0);
        // Unteren Streifen leeren:
        for y in self.info.height.saturating_sub(pixel_zeilen)..self.info.height {
            for x in 0..self.info.width {
                self.pixel_setzen(x, y, fuellfarbe);
            }
        }
    }

    /// Zeichnet ein Zeichen an Pixelposition (x, y) und liefert die
    /// gezeichnete Breite. Die Intensitätswerte des Fonts (0-255)
    /// mischen Vorder- und Hintergrundfarbe — das ist das Antialiasing.
    /// (8 Argumente sind für eine Zeichen-Primitive in Ordnung —
    /// Position, Zeichen, Schrift und Farben gehören nun mal dazu.)
    #[allow(clippy::too_many_arguments)]
    pub fn zeichen_zeichnen(
        &mut self,
        x: usize,
        y: usize,
        zeichen: char,
        groesse: RasterHeight,
        gewicht: FontWeight,
        vordergrund: Farbe,
        hintergrund: Farbe,
    ) -> usize {
        // Unbekannte Zeichen als '?' — get_raster(' ') gibt es immer.
        let raster = get_raster(zeichen, gewicht, groesse)
            .or_else(|| get_raster('?', gewicht, groesse))
            .or_else(|| get_raster(' ', gewicht, groesse))
            .expect("Font enthaelt nicht einmal das Leerzeichen");

        for (dy, zeile) in raster.raster().iter().enumerate() {
            for (dx, &intensitaet) in zeile.iter().enumerate() {
                self.pixel_setzen(x + dx, y + dy, hintergrund.mischen(vordergrund, intensitaet));
            }
        }
        raster.width()
    }

    /// Zeichnet einen String ab Pixelposition (x, y); kein Umbruch —
    /// für Boot-Screen und andere frei platzierte Texte.
    #[allow(clippy::too_many_arguments)]
    pub fn text_zeichnen(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        groesse: RasterHeight,
        gewicht: FontWeight,
        vordergrund: Farbe,
        hintergrund: Farbe,
    ) {
        let mut cx = x;
        for zeichen in text.chars() {
            cx += self.zeichen_zeichnen(cx, y, zeichen, groesse, gewicht, vordergrund, hintergrund);
        }
    }
}

/// Der globale Framebuffer (Muster wie VFS und Speicher-API).
static FRAMEBUFFER: Mutex<Option<DoppelPuffer>> = Mutex::new(None);

/// Initialisiert den Doppel-Puffer aus dem Bootloader-Framebuffer.
/// Voraussetzung: memory::init + Heap laufen (allocate_pages!).
pub fn init(framebuffer: FrameBuffer) {
    let info = framebuffer.info();

    // Back-Buffer: gleiche Größe wie der echte Framebuffer, aus
    // zusammenhängenden Pages (unser allocate_pages-Anwendungsfall!).
    let pages = info.byte_len.div_ceil(4096);
    let hinten_start = memory::allocate_pages(pages)
        .expect("Kein Speicher fuer den Framebuffer-Back-Buffer");
    // unsafe: allocate_pages hat genau diesen Bereich frisch gemappt
    // und niemand sonst kennt ihn — die Slice-Erzeugung ist exklusiv.
    let hinten =
        unsafe { core::slice::from_raw_parts_mut(hinten_start.as_mut_ptr::<u8>(), info.byte_len) };
    hinten.fill(0);

    *FRAMEBUFFER.lock() = Some(DoppelPuffer {
        vorne: framebuffer.into_buffer(),
        hinten,
        hintergrund: None,
        info,
    });
}

/// Ist der Framebuffer initialisiert? (Tests ohne Grafik: nein.)
pub fn ist_initialisiert() -> bool {
    FRAMEBUFFER.lock().is_some()
}

/// Führt eine Zeichen-Operation auf dem globalen Framebuffer aus.
/// Interrupts sind währenddessen aus (Deadlock-Regel wie bei den
/// Ausgabe-Locks). Tut nichts, wenn kein Framebuffer da ist.
pub fn mit_framebuffer<T>(f: impl FnOnce(&mut DoppelPuffer) -> T) -> Option<T> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        FRAMEBUFFER.lock().as_mut().map(f)
    })
}

impl fmt::Debug for DoppelPuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DoppelPuffer").field("info", &self.info).finish()
    }
}

// ---------------------------------------------------------------------------
// Der Boot-Screen: dunkler Obsidian-Hintergrund, Aurora-Farbverlauf
// ---------------------------------------------------------------------------

/// Obsidian-Aurora-Palette.
const OBSIDIAN: Farbe = Farbe::neu(0x0b, 0x0e, 0x14); // fast schwarz, kalt
const AURORA_VIOLETT: Farbe = Farbe::neu(0x7c, 0x3a, 0xed);
const AURORA_BLAU: Farbe = Farbe::neu(0x3b, 0x82, 0xf6);
const AURORA_CYAN: Farbe = Farbe::neu(0x22, 0xd3, 0xee);

/// Farbe auf der Aurora-Achse (t: 0..=255): Violett -> Blau -> Cyan.
fn aurora(t: u8) -> Farbe {
    if t < 128 {
        AURORA_VIOLETT.mischen(AURORA_BLAU, t * 2)
    } else {
        AURORA_BLAU.mischen(AURORA_CYAN, (t - 128) * 2)
    }
}

/// Malt den SpeedOS-Boot-Screen (Aurora auf Obsidian) EINMAL. Das
/// Verweilen übernimmt der Aufrufer — so kann kernel_main während der
/// Verweilzeit auf die D-Taste (Diagnose-Modus) lauschen, ohne dass
/// der Framebuffer die Tastatur kennen muss (saubere Schichtung).
pub fn bootscreen_malen() {
    mit_framebuffer(|fb| {
        let breite = fb.info().width;
        let hoehe = fb.info().height;
        fb.fuellen(OBSIDIAN);

        // Aurora-Schleier: weiche horizontale Farbbänder im oberen
        // Drittel, die zum Hintergrund hin auslaufen.
        for y in 0..hoehe / 3 {
            // Wie stark leuchtet dieses Band? Oben kräftig, unten weg.
            let staerke = 60u32.saturating_sub((y * 60 / (hoehe / 3)) as u32) as u8;
            for x in 0..breite {
                let ton = aurora((x * 255 / breite) as u8);
                let pixel = OBSIDIAN.mischen(ton, staerke);
                fb.pixel_setzen(x, y, pixel);
            }
        }

        // Schriftzug "SpeedOS" gross und fett, Zeichen für Zeichen
        // entlang des Aurora-Verlaufs eingefärbt:
        let text = "SpeedOS";
        let zeichen_breite = get_raster_width(FontWeight::Bold, RasterHeight::Size32);
        let text_breite = text.chars().count() * zeichen_breite;
        let start_x = (breite - text_breite) / 2;
        let start_y = hoehe / 2 - 32;
        for (i, zeichen) in text.chars().enumerate() {
            let t = (i * 255 / (text.chars().count() - 1)) as u8;
            fb.zeichen_zeichnen(
                start_x + i * zeichen_breite,
                start_y,
                zeichen,
                RasterHeight::Size32,
                FontWeight::Bold,
                aurora(t),
                OBSIDIAN,
            );
        }

        // Unterzeile, dezent grau:
        let unterzeile = "ein Betriebssystem in Rust";
        let uz_breite =
            unterzeile.chars().count() * get_raster_width(FontWeight::Regular, RasterHeight::Size16);
        fb.text_zeichnen(
            (breite - uz_breite) / 2,
            start_y + 44,
            unterzeile,
            RasterHeight::Size16,
            FontWeight::Regular,
            Farbe::neu(0x8a, 0x91, 0xa3),
            OBSIDIAN,
        );

        // Aurora-Trennlinie unter dem Schriftzug:
        let linie_y = start_y + 76;
        for x in breite / 4..breite * 3 / 4 {
            let t = ((x - breite / 4) * 255 / (breite / 2)) as u8;
            fb.pixel_setzen(x, linie_y, aurora(t));
            fb.pixel_setzen(x, linie_y + 1, OBSIDIAN.mischen(aurora(t), 128));
        }

        fb.present();
    });
}

/// Zeichnet den Boot-Screen und zeigt ihn `dauer_ms` lang (Malen +
/// Verweilen). Wird für den normalen, nicht-Diagnose-Boot benutzt.
pub fn bootscreen_zeigen(dauer_ms: u64) {
    bootscreen_malen();
    // Den Moment wirken lassen (hlt-freundlich, Timer weckt uns):
    let start = crate::zeit::ms_seit_boot();
    while crate::zeit::ms_seit_boot() < start + dauer_ms {
        x86_64::instructions::hlt();
    }
}

/// Zeigt eine zentrierte Text-Meldung auf dunklem Obsidian-Grund und
/// hält sie `dauer_ms` lang (hlt-freundlich). Für Hinweise, die es
/// NICHT auf die serielle Schnittstelle schaffen müssen, sondern die
/// der Nutzer auf echter Hardware am Bildschirm sehen soll — etwa
/// „keine PS/2-Eingabe gefunden". Die erste Zeile wird hervorgehoben.
pub fn meldung_zeigen(zeilen: &[&str], dauer_ms: u64) {
    mit_framebuffer(|fb| {
        let breite = fb.info().width;
        let hoehe = fb.info().height;
        fb.fuellen(OBSIDIAN);

        let zeilen_hoehe = 26usize;
        let block_hoehe = zeilen.len() * zeilen_hoehe;
        let mut y = hoehe.saturating_sub(block_hoehe) / 2;
        for (i, zeile) in zeilen.iter().enumerate() {
            // Erste Zeile fett/hell (Aurora-Cyan), Rest dezent grau:
            let (gewicht, farbe) = if i == 0 {
                (FontWeight::Bold, AURORA_CYAN)
            } else {
                (FontWeight::Regular, Farbe::neu(0xb8, 0xc0, 0xd0))
            };
            let zeichen_breite = get_raster_width(gewicht, RasterHeight::Size16);
            let text_breite = zeile.chars().count() * zeichen_breite;
            let x = breite.saturating_sub(text_breite) / 2;
            fb.text_zeichnen(x, y, zeile, RasterHeight::Size16, gewicht, farbe, OBSIDIAN);
            y += zeilen_hoehe;
        }
        fb.present();
    });

    let start = crate::zeit::ms_seit_boot();
    while crate::zeit::ms_seit_boot() < start + dauer_ms {
        x86_64::instructions::hlt();
    }
}
