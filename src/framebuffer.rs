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
use x86_64::VirtAddr;

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
        crate::wacht::punkt(crate::wacht::Punkt::Present);
        self.vorne.copy_from_slice(self.hinten);
    }

    /// Kopiert nur die Pixelzeilen [y, y+hoehe) auf den Bildschirm —
    /// z. B. fürs Cursor-Blinken, damit nicht ständig der ganze
    /// Bildschirm übertragen wird.
    pub fn present_zeilen(&mut self, y: usize, hoehe: usize) {
        crate::wacht::punkt(crate::wacht::Punkt::Present);
        let von = y.min(self.info.height) * self.info.stride * self.info.bytes_per_pixel;
        let bis = (y + hoehe).min(self.info.height) * self.info.stride * self.info.bytes_per_pixel;
        self.vorne[von..bis].copy_from_slice(&self.hinten[von..bis]);
    }

    /// Kopiert ein RECHTECK vom Back- in den Front-Buffer — stellt
    /// z. B. das Bild unter dem Maus-Cursor wieder her.
    pub fn present_bereich(&mut self, x: usize, y: usize, breite: usize, hoehe: usize) {
        // Wegmarke fuer den Wachhund: Der Transfer in den echten
        // Framebuffer ist auf ungecachter Hardware die mit Abstand
        // teuerste Einzelhandlung im System. Bleibt das System hier
        // stehen, soll das auf dem Bildschirm ablesbar sein.
        crate::wacht::punkt(crate::wacht::Punkt::Present);
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

        // EIN 32-BIT-SCHREIBZUGRIFF STATT DREI EINZELNER BYTES.
        //
        // Das ist kein Mikro-Optimieren, sondern der Unterschied
        // zwischen ruckelnder und fluessiger Maus: Diese Funktion
        // schreibt in den ECHTEN Framebuffer, und der ist auf echter
        // Hardware Geraetespeicher. Dort ist JEDER Schreibzugriff eine
        // eigene Bus-Transaktion — drei Bytes kosten dreimal so viel wie
        // ein Doppelwort. Der Mauszeiger sind rund 1000 Pixel, und er
        // wird bis zu 200-mal je Sekunde neu gezeichnet.
        //
        // Erlaubt ist es, wenn ein Pixel wirklich 4 Byte breit ist (dann
        // ist `offset` durch 4 teilbar, weil der Puffer seitenausgerichtet
        // beginnt) und das Format eines der beiden bekannten ist. Das
        // vierte Byte ist in beiden Faellen reserviert und wird von der
        // Firmware ignoriert.
        if self.info.bytes_per_pixel == 4 {
            let wort = match self.info.pixel_format {
                PixelFormat::Rgb => {
                    u32::from_le_bytes([farbe.r, farbe.g, farbe.b, 0])
                }
                PixelFormat::Bgr => {
                    u32::from_le_bytes([farbe.b, farbe.g, farbe.r, 0])
                }
                _ => {
                    let grau = ((farbe.r as u16 + farbe.g as u16 + farbe.b as u16) / 3) as u8;
                    u32::from_le_bytes([grau; 4])
                }
            };
            self.vorne[offset..offset + 4].copy_from_slice(&wort.to_le_bytes());
            return;
        }

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

    let vorne = framebuffer.into_buffer();

    // ===================================================================
    // DEN ECHTEN FRAMEBUFFER AUF WRITE-COMBINING UMSTELLEN
    //
    // Der Bootloader mappt ihn mit den Cache-Eigenschaften der Firmware
    // — auf echter Hardware heisst das meist UNGECACHT, und dann ist
    // jeder einzelne Schreibzugriff eine eigene PCIe-Transaktion. Bei
    // 1080p sind das 8,3 MB je Vollbild; uncached kostet das leicht
    // 50 ms, und zwar bei JEDEM `present()`.
    //
    // In QEMU faellt das nicht auf (dort ist es Host-RAM), auf dem
    // Laptop fror dadurch das Tippen ein. Siehe
    // `memory::write_combining_einrichten` fuer die Mechanik.
    //
    // ZWEI SCHICHTEN, BEIDE NOETIG — und das ist der Kern der Sache:
    // Der effektive Speichertyp ergibt sich aus MTRR **und** PAT, und
    // dabei gewinnt der RESTRIKTIVERE. Steht der Bereich im MTRR auf
    // UC, kann die Seitentabelle ihn NICHT auf WC heben. Deshalb kommt
    // der MTRR ZUERST (src/mtrr.rs) und der PAT-Eintrag danach; wer die
    // Reihenfolge dreht, setzt ein Flag, das nichts bewirkt.
    //
    // NUR DER VORDERE Puffer wird umgestellt. Der Back-Buffer ist
    // normales RAM und soll gecacht bleiben — dort wird gezeichnet, und
    // Lesen aus WC-Speicher waere langsam.
    let vorne_virt = VirtAddr::new(vorne.as_ptr() as u64);
    match memory::uebersetzen(vorne_virt) {
        Some(physik) => {
            crate::mtrr::framebuffer_beschleunigen(physik.as_u64(), info.byte_len as u64);
        }
        None => {
            // Kann eigentlich nicht sein — wir schreiben gerade hinein.
            // Trotzdem kein Grund anzuhalten: Der PAT-Weg unten laeuft
            // weiter, und auf Maschinen mit WB-Vorgabe genuegt er.
            crate::serial_println!(
                "[FB] Physikadresse des Framebuffers nicht ermittelbar — MTRR uebersprungen."
            );
        }
    }
    let umgestellt = memory::bereich_write_combining(vorne_virt, info.byte_len);
    crate::serial_println!(
        "[FB] {} Seiten des Framebuffers auf Write-Combining umgestellt ({} KiB).",
        umgestellt,
        info.byte_len / 1024
    );

    // DEN WACHHUND SCHARF SCHALTEN (src/wacht.rs).
    //
    // Er bekommt den ROHEN Zeiger, weil er im Stillstand keinen Lock
    // nehmen darf — genau dann koennte der Lock ja die Ursache sein.
    // Ab hier kann ein eingefrorenes System seinen letzten Programmpunkt
    // selbst auf den Bildschirm malen.
    //
    // SAFETY: `vorne` IST der echte, dauerhaft gemappte Framebuffer mit
    // genau diesen Massen — wir halten ihn gerade in der Hand.
    unsafe {
        crate::wacht::einrichten(
            vorne.as_mut_ptr(),
            info.stride,
            info.bytes_per_pixel,
            info.width,
            info.height,
        );
    }

    *FRAMEBUFFER.lock() = Some(DoppelPuffer {
        vorne,
        hinten,
        hintergrund: None,
        info,
    });
}

/// Misst, wie lange ein VOLLBILD-`present()` dauert.
///
/// ===================================================================
/// DIE ZAHL, DIE DEN UNTERSCHIED ZWISCHEN QEMU UND BLECH ZEIGT
///
/// Sie beantwortet die Frage, die man auf echter Hardware sonst nicht
/// beantworten kann, weil es dort keine serielle Ausgabe gibt: **Wie
/// teuer ist der Weg zum Bildschirm?**
///
/// Erwartungswerte bei 1080p:
///
/// * unter 2 ms — gecacht oder write-combining, alles in Ordnung
/// * 5–15 ms — write-combining auf langsamem Bus, brauchbar
/// * ueber 30 ms — UNGECACHT. Das ist die Ursache, wenn das System beim
///   Tippen einfriert.
pub fn present_messen() -> u64 {
    let start = crate::zeit::us_seit_boot();
    mit_framebuffer(|fb| fb.present());
    crate::zeit::us_seit_boot().saturating_sub(start)
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
/// Zeigt den SYSTEMBEFUND — die Zahlen, die man auf echter Hardware
/// sonst nirgends sieht.
///
/// ===================================================================
/// WARUM DAS OHNE TASTENDRUCK ERSCHEINT
///
/// Auf dem Blech gibt es keine serielle Ausgabe. Bisher stand die
/// Leistungsmessung nur im Diagnose-Modus hinter Taste D — und
/// ausgerechnet die Tastatur ist auf der Testmaschine das Problem.
/// Eine Messung, die man nur mit dem kaputten Geraet abrufen kann,
/// ist keine Messung.
///
/// DIE WICHTIGSTE ZEILE IST `present`: Sie sagt, wie teuer ein
/// Vollbild-Transfer in den echten Framebuffer ist. Bei 1080p sind das
/// 8,3 MB; ist der Speicher ungecacht, kostet er zehnmal so viel wie
/// mit Write-Combining — und genau das entscheidet, ob das System beim
/// Tippen einfriert (jede gescrollte Konsolenzeile ist ein
/// Vollbild-Transfer).
pub fn befund_zeigen(dauer_ms: u64) {
    use core::fmt::Write;

    // Zweimal messen: Der erste Durchgang faellt oft aus dem Rahmen
    // (kalte Caches, TLB), der zweite ist der ehrliche Wert.
    let _ = present_messen();
    let present_us = present_messen();

    let (breite, hoehe) =
        mit_framebuffer(|fb| (fb.info().width, fb.info().height)).unwrap_or((0, 0));

    // Feste Puffer statt `format!` — dieser Weg laeuft frueh und soll
    // nicht vom Heap abhaengen.
    let mut z1: heapless_zeile::Zeile = Default::default();
    let mut z2: heapless_zeile::Zeile = Default::default();
    let mut z3: heapless_zeile::Zeile = Default::default();
    let mut z4: heapless_zeile::Zeile = Default::default();

    let _ = write!(z1, "Bildschirm  {}x{}   present {} us", breite, hoehe, present_us);
    // Der Zustand VOR unserem Eingriff steht mit dabei — nur so laesst
    // sich sagen, ob der Eingriff ueberhaupt etwas zu tun hatte.
    let _ = write!(z2, "Speichertyp  MTRR {}", crate::mtrr::befund().text());
    if let Some(t) = crate::mtrr::typ_vorher() {
        let _ = write!(z2, " (vorher {})", crate::mtrr::typ_text(t));
    }
    let _ = write!(
        z2,
        "   PAT-WC {}",
        if memory::write_combining_verfuegbar() {
            "ja"
        } else {
            "nein"
        }
    );
    let _ = write!(
        z3,
        "Eingabe  PS/2-Tastatur {}   PS/2-Maus {}   USB-HID {}",
        if crate::diagnose::tastatur_vorhanden() { "ja" } else { "nein" },
        if crate::maus::zeiger_vorhanden() { "ja" } else { "nein" },
        crate::usb::geraet::anzahl()
    );
    // Die BEWERTUNG gleich dazu — eine nackte Zahl hilft nur dem, der
    // die Schwellen auswendig kennt.
    let _ = write!(
        z4,
        "{}",
        if present_us > 30_000 {
            "BEFUND: Bildschirm UNGECACHT — das ist die Ursache."
        } else if present_us > 15_000 {
            "BEFUND: Bildschirm langsam, aber benutzbar."
        } else {
            "BEFUND: Bildschirm in Ordnung."
        }
    );

    meldung_zeigen(
        &[
            "SpeedOS — Systembefund",
            "",
            z1.als_str(),
            z2.als_str(),
            z3.als_str(),
            "",
            z4.als_str(),
            "",
            "Roter Balken oben = Stillstand. Kaestchen zaehlen:",
            "1 Executor  2 Compositor  3 Bildschirm  4 Konsole",
            "5 Tastatur  6 Maus  7 Shell  8 USB  9 Audio",
        ],
        dauer_ms,
    );
}

/// Eine Textzeile mit fester Groesse — damit `befund_zeigen` ohne Heap
/// auskommt (es laeuft frueh und soll nichts voraussetzen).
mod heapless_zeile {
    pub struct Zeile {
        puffer: [u8; 96],
        laenge: usize,
    }

    impl Default for Zeile {
        fn default() -> Self {
            Zeile {
                puffer: [0; 96],
                laenge: 0,
            }
        }
    }

    impl Zeile {
        pub fn als_str(&self) -> &str {
            core::str::from_utf8(&self.puffer[..self.laenge]).unwrap_or("?")
        }
    }

    impl core::fmt::Write for Zeile {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for b in s.bytes() {
                if self.laenge < self.puffer.len() {
                    self.puffer[self.laenge] = b;
                    self.laenge += 1;
                }
            }
            Ok(())
        }
    }
}

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
