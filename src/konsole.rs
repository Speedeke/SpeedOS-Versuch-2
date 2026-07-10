// konsole.rs — Die FramebufferKonsole: Text auf dem Grafik-Bildschirm
//
// Ersetzt den alten VGA-Textmodus vollwertig: Zeichenraster,
// Zeilenumbruch, Scrolling (memmove im Back-Buffer!), Farben pro
// Zeichen und ein blinkender Software-Cursor. Gezeichnet wird mit dem
// vorgerasterten Noto-Sans-Mono-Font (Antialiasing, Umlaute!).
//
// Die Naht bleibt: print!/println! (lib.rs) rufen konsole::_print —
// und damit gilt wieder die alte Projektregel "Ausgabe immer doppelt":
// jedes Zeichen geht auf den Bildschirm UND seriell (dort weiterhin
// mit ANSI-Farben fürs Terminal).
//
// Lock-Ordnung (Deadlock-Regel): KONSOLE vor FRAMEBUFFER, beides nur
// mit deaktivierten Interrupts (passiert in _print/mit_framebuffer).

use crate::framebuffer::{self, DoppelPuffer, Farbe};
use crate::serial_print;
use core::fmt;
use noto_sans_mono_bitmap::{get_raster_width, FontWeight, RasterHeight};
use spin::Mutex;

/// Schriftgröße der Konsole: 16 Pixel hoch (Breite kommt vom Font).
const FONT_GROESSE: RasterHeight = RasterHeight::Size16;
const FONT_GEWICHT: FontWeight = FontWeight::Regular;

/// Die 16 klassischen Konsolen-Farben — Namen wie zu VGA-Zeiten,
/// damit Shell & Co. unverändert bleiben; die RGB-Werte sind auf den
/// dunklen Obsidian-Look abgestimmt.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Blue,
    Green,
    Cyan,
    Red,
    Magenta,
    Brown,
    LightGray,
    DarkGray,
    LightBlue,
    LightGreen,
    LightCyan,
    LightRed,
    Pink,
    Yellow,
    White,
}

impl Color {
    /// Die RGB-Übersetzung (Obsidian-Aurora-Palette).
    fn farbe(self) -> Farbe {
        match self {
            Color::Black => Farbe::neu(0x0b, 0x0e, 0x14), // Obsidian
            Color::Blue => Farbe::neu(0x3b, 0x82, 0xf6),
            Color::Green => Farbe::neu(0x22, 0xc5, 0x5e),
            Color::Cyan => Farbe::neu(0x22, 0xd3, 0xee),
            Color::Red => Farbe::neu(0xef, 0x44, 0x44),
            Color::Magenta => Farbe::neu(0xd9, 0x46, 0xef),
            Color::Brown => Farbe::neu(0xb4, 0x53, 0x09),
            Color::LightGray => Farbe::neu(0xc4, 0xca, 0xd6),
            Color::DarkGray => Farbe::neu(0x56, 0x5f, 0x73),
            Color::LightBlue => Farbe::neu(0x93, 0xc5, 0xfd),
            Color::LightGreen => Farbe::neu(0x86, 0xef, 0xac),
            Color::LightCyan => Farbe::neu(0xa5, 0xf3, 0xfc),
            Color::LightRed => Farbe::neu(0xfc, 0xa5, 0xa5),
            Color::Pink => Farbe::neu(0xf9, 0xa8, 0xd4),
            Color::Yellow => Farbe::neu(0xfb, 0xbf, 0x24),
            Color::White => Farbe::neu(0xf8, 0xfa, 0xfc),
        }
    }

    /// Der ANSI-Farbcode fürs serielle Terminal (Hintergrund = +10).
    fn ansi_code(self) -> u8 {
        match self {
            Color::Black => 30,
            Color::Red => 31,
            Color::Green => 32,
            Color::Brown => 33,
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
            Color::LightGray => 37,
            Color::DarkGray => 90,
            Color::LightRed => 91,
            Color::LightGreen => 92,
            Color::Yellow => 93,
            Color::LightBlue => 94,
            Color::Pink => 95,
            Color::LightCyan => 96,
            Color::White => 97,
        }
    }
}

/// Der Zustand der Konsole (Rasterposition, Farben, Cursor).
struct KonsolenZustand {
    spalte: usize,
    zeile: usize,
    /// Rastergröße — wird bei init() aus der Auflösung berechnet.
    spalten: usize,
    zeilen: usize,
    zeichen_breite: usize,
    vordergrund: Farbe,
    hintergrund: Farbe,
    /// Standard-Hintergrund (fürs Scrolling — keine Farbstreifen!).
    standard_hintergrund: Farbe,
    /// Blinkt der Cursor gerade sichtbar?
    cursor_sichtbar: bool,
    /// Soll überhaupt ein Cursor gezeigt werden?
    cursor_aktiv: bool,
    /// Dirty-Bereich in PIXELZEILEN (von, bis-exklusiv): Nur dieser
    /// Streifen muss beim present übertragen werden. Ein Tastendruck
    /// überträgt so ~16 Zeilen statt des ganzen Bildschirms (bei
    /// 2560x1600 wären das sonst 16 MB pro Zeichen!).
    dirty_von: usize,
    dirty_bis: usize,
}

static KONSOLE: Mutex<KonsolenZustand> = Mutex::new(KonsolenZustand {
    spalte: 0,
    zeile: 0,
    spalten: 0,
    zeilen: 0,
    zeichen_breite: 8,
    vordergrund: Farbe::neu(0xc4, 0xca, 0xd6),
    hintergrund: Farbe::neu(0x0b, 0x0e, 0x14),
    standard_hintergrund: Farbe::neu(0x0b, 0x0e, 0x14),
    cursor_sichtbar: false,
    cursor_aktiv: false,
    dirty_von: usize::MAX,
    dirty_bis: 0,
});

/// Höhe einer Zeichenzelle in Pixeln.
const ZELLEN_HOEHE: usize = 16;

impl KonsolenZustand {
    /// Merkt Pixelzeilen [von, bis) als geändert vor.
    fn dirty_markieren(&mut self, von: usize, bis: usize) {
        self.dirty_von = self.dirty_von.min(von);
        self.dirty_bis = self.dirty_bis.max(bis);
    }

    /// Holt den Dirty-Bereich ab und setzt ihn zurück.
    fn dirty_abholen(&mut self) -> Option<(usize, usize)> {
        if self.dirty_von >= self.dirty_bis {
            return None;
        }
        let bereich = (self.dirty_von, self.dirty_bis - self.dirty_von);
        self.dirty_von = usize::MAX;
        self.dirty_bis = 0;
        Some(bereich)
    }

    /// Zeichnet oder löscht den Cursor (Unterstrich in der Zelle).
    fn cursor_zeichnen(&self, fb: &mut DoppelPuffer, sichtbar: bool) {
        if !self.cursor_aktiv {
            return;
        }
        let x0 = self.spalte * self.zeichen_breite;
        let y0 = self.zeile * ZELLEN_HOEHE;
        let farbe = if sichtbar {
            self.vordergrund
        } else {
            self.standard_hintergrund
        };
        // Unterstrich: die untersten 2 Pixelzeilen der Zelle.
        for dy in ZELLEN_HOEHE - 2..ZELLEN_HOEHE {
            for dx in 0..self.zeichen_breite {
                fb.pixel_setzen(x0 + dx, y0 + dy, farbe);
            }
        }
    }

    /// Schreibt ein Zeichen an die aktuelle Rasterposition.
    fn zeichen_schreiben(&mut self, fb: &mut DoppelPuffer, zeichen: char) {
        match zeichen {
            '\n' => self.neue_zeile(fb),
            // Backspace: nur zurücksetzen — die Shell schickt danach
            // ein Leerzeichen zum Ausradieren ("\b \b", wie Terminals).
            '\u{8}' => self.spalte = self.spalte.saturating_sub(1),
            zeichen => {
                if self.spalte >= self.spalten {
                    self.neue_zeile(fb);
                }
                fb.zeichen_zeichnen(
                    self.spalte * self.zeichen_breite,
                    self.zeile * ZELLEN_HOEHE,
                    zeichen,
                    FONT_GROESSE,
                    FONT_GEWICHT,
                    self.vordergrund,
                    self.hintergrund,
                );
                self.spalte += 1;
                let pixel_zeile = self.zeile * ZELLEN_HOEHE;
                self.dirty_markieren(pixel_zeile, pixel_zeile + ZELLEN_HOEHE);
            }
        }
    }

    /// Zeilenumbruch — scrollt bei Bedarf (memmove im Back-Buffer).
    fn neue_zeile(&mut self, fb: &mut DoppelPuffer) {
        self.spalte = 0;
        if self.zeile + 1 < self.zeilen {
            self.zeile += 1;
        } else {
            // Bewusst mit der STANDARD-Hintergrundfarbe leeren —
            // die Lektion aus VGA-Zeiten gegen Farbstreifen.
            fb.hochscrollen(ZELLEN_HOEHE, self.standard_hintergrund);
            // Nach dem Scrollen hat sich ALLES verschoben:
            self.dirty_markieren(0, self.zeilen * ZELLEN_HOEHE);
        }
    }
}

/// Initialisiert das Zeichenraster aus der Framebuffer-Auflösung.
/// Nach framebuffer::init() aufrufen; ohne Framebuffer: no-op.
pub fn init() {
    let info = match framebuffer::mit_framebuffer(|fb| fb.info()) {
        Some(info) => info,
        None => return,
    };
    let zeichen_breite = get_raster_width(FONT_GEWICHT, FONT_GROESSE);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        konsole.zeichen_breite = zeichen_breite;
        konsole.spalten = info.width / zeichen_breite;
        konsole.zeilen = info.height / ZELLEN_HOEHE;
    });
}

/// Schreib-Adapter: verbindet Konsolen-Zustand und Framebuffer
/// für core::fmt::Write (damit format_args! funktioniert).
struct KonsolenZeichner<'a> {
    zustand: &'a mut KonsolenZustand,
    fb: &'a mut DoppelPuffer,
}

impl fmt::Write for KonsolenZeichner<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for zeichen in s.chars() {
            self.zustand.zeichen_schreiben(self.fb, zeichen);
        }
        Ok(())
    }
}

/// Interne Hilfsfunktion der print!-Makros: schreibt auf den
/// Framebuffer (falls initialisiert) UND seriell — die alte
/// Doppel-Ausgabe-Regel lebt wieder!
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut zustand = KONSOLE.lock();
        framebuffer::mit_framebuffer(|fb| {
            // Cursor vor dem Schreiben wegnehmen (er wandert gleich).
            zustand.cursor_zeichnen(fb, false);
            let cursor_zeile = zustand.zeile * ZELLEN_HOEHE;
            zustand.dirty_markieren(cursor_zeile, cursor_zeile + ZELLEN_HOEHE);
            KonsolenZeichner {
                zustand: &mut zustand,
                fb,
            }
            .write_fmt(args)
            .ok();
            // Cursor an der neuen Position wieder zeigen.
            let sichtbar = zustand.cursor_sichtbar;
            zustand.cursor_zeichnen(fb, sichtbar);
            let cursor_zeile = zustand.zeile * ZELLEN_HOEHE;
            zustand.dirty_markieren(cursor_zeile, cursor_zeile + ZELLEN_HOEHE);
            // Nur den geänderten Streifen übertragen (Dirty-Region):
            if let Some((von, hoehe)) = zustand.dirty_abholen() {
                fb.present_zeilen(von, hoehe);
            }
        });
    });
    // Seriell (mit den ANSI-Farben aus set_color):
    crate::serial::_print(args);
}

/// Setzt Vorder- und Hintergrundfarbe — auf dem Bildschirm UND als
/// ANSI-Sequenz im seriellen Terminal.
pub fn set_color(foreground: Color, background: Color) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        konsole.vordergrund = foreground.farbe();
        konsole.hintergrund = background.farbe();
    });
    serial_print!(
        "\x1b[{};{}m",
        foreground.ansi_code(),
        background.ansi_code() + 10
    );
}

/// Leert den Bildschirm (und das serielle Terminal per ANSI).
pub fn clear_screen() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        konsole.spalte = 0;
        konsole.zeile = 0;
        let hintergrund = konsole.standard_hintergrund;
        framebuffer::mit_framebuffer(|fb| {
            fb.fuellen(hintergrund);
            fb.present();
        });
    });
    serial_print!("\x1b[2J\x1b[H");
}

/// Schaltet den blinkenden Software-Cursor ein
/// (den Blink-Takt macht cursor_blink_task).
pub fn cursor_aktivieren() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        KONSOLE.lock().cursor_aktiv = true;
    });
}

/// Pausiert den Cursor (z. B. während der Grafik-Demo den ganzen
/// Bildschirm gehört — sonst malt der Blink-Task hinein).
pub fn cursor_pausieren() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        KONSOLE.lock().cursor_aktiv = false;
    });
}

/// Der Cursor-Blink-Task: läuft ewig im Executor und schaltet den
/// Cursor im Halbsekunden-Takt um. Wartet ASYNCHRON auf Timer-Ticks
/// (zeit::warte_ms) — zwischen den Blinks schläft die CPU.
pub async fn cursor_blink_task() {
    loop {
        crate::zeit::warte_ms(500).await;
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut konsole = KONSOLE.lock();
            if !konsole.cursor_aktiv {
                return;
            }
            konsole.cursor_sichtbar = !konsole.cursor_sichtbar;
            let sichtbar = konsole.cursor_sichtbar;
            let cursor_pixelzeile = konsole.zeile * ZELLEN_HOEHE;
            framebuffer::mit_framebuffer(|fb| {
                konsole.cursor_zeichnen(fb, sichtbar);
                // Nur den Cursor-Streifen übertragen, nicht 3,6 MB.
                fb.present_zeilen(cursor_pixelzeile, ZELLEN_HOEHE);
            });
        });
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::println;
    use noto_sans_mono_bitmap::get_raster;

    /// Der Font MUSS die deutschen Umlaute liefern (Latin-1-Feature).
    #[test_case]
    fn test_font_hat_umlaute() {
        for zeichen in "äöüÄÖÜß".chars() {
            assert!(
                get_raster(zeichen, FONT_GEWICHT, FONT_GROESSE).is_some(),
                "Font-Raster fehlt fuer '{}'",
                zeichen
            );
        }
    }

    /// Viele Zeilen drucken: treibt die Konsole durchs Scrolling
    /// (memmove-Pfad) — darf nicht panicken oder hängen.
    #[test_case]
    fn test_scrolling_ohne_panic() {
        for i in 0..60 {
            println!("Framebuffer-Scrolltest Zeile {} mit Umlauten: äöüß", i);
        }
    }
}
