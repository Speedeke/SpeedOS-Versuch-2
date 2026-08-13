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
use alloc::vec;
use alloc::vec::Vec;
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

/// Eine Zelle des Konsolen-Rasters — NUR fuer den Rueckblick gefuehrt.
///
/// Die Konsole malt jedes Zeichen weiterhin direkt in den Back-Buffer
/// (das ist der schnelle Weg und bleibt es). Zusaetzlich merkt sie sich
/// hier, WAS an welcher Stelle steht — sonst liesse sich ein
/// herausgescrolltes Bild nicht wiederherstellen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KZelle {
    zeichen: char,
    vg: Farbe,
    hg: Farbe,
}

/// Wie viele herausgescrollte Zeilen die Vollbild-Konsole aufhebt.
///
/// Kleiner als beim Terminal-Fenster (1000), weil die Konsole so breit ist
/// wie der Bildschirm: Bei 4K sind das 480 Spalten, also 300 x 480 x 12 Byte
/// = rund 1,7 MiB. Bei 720p ein Drittel davon.
const MAX_HISTORIE: usize = 300;

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

    // ----- Der Rueckblick (Scrollback) -----
    //
    // WICHTIG: Diese Puffer werden EINMAL angelegt (`rueckblick_einrichten`,
    // nach der Heap-Erweiterung) und danach NIE wieder alloziert. Eine
    // Allokation im print!-Pfad waere gefaehrlich: `_print` haelt den
    // KONSOLE-Lock, und wenn dabei der Speicher ausginge, wollte der
    // alloc_error_handler seinerseits drucken — ein Deadlock in genau der
    // Funktion, die ihn melden soll.
    /// Was auf dem sichtbaren Raster steht (leer = Rueckblick aus).
    zellen: Vec<KZelle>,
    /// Ring der herausgescrollten Zeilen (leer = Rueckblick aus).
    historie: Vec<KZelle>,
    historie_zeilen: usize,
    historie_kopf: usize,
    /// Wie weit zurueckgeblaettert ist. 0 = live.
    blick_ab: usize,
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
    zellen: Vec::new(),
    historie: Vec::new(),
    historie_zeilen: 0,
    historie_kopf: 0,
    blick_ab: 0,
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

    /// Ist der Rueckblick eingerichtet (Puffer vorhanden)?
    fn rueckblick_da(&self) -> bool {
        !self.zellen.is_empty()
    }

    /// Merkt sich, was an der aktuellen Position steht.
    fn zelle_merken(&mut self, zeichen: char) {
        if !self.rueckblick_da() || self.spalte >= self.spalten || self.zeile >= self.zeilen {
            return;
        }
        let index = self.zeile * self.spalten + self.spalte;
        self.zellen[index] = KZelle {
            zeichen,
            vg: self.vordergrund,
            hg: self.hintergrund,
        };
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
                self.zelle_merken(zeichen);
                // ZURUECKGEBLAETTERT wird NICHT gemalt: Auf dem Schirm steht
                // gerade Vergangenheit, und neue Ausgabe wuerde sie
                // uebermalen. GEMERKT ist sie trotzdem — beim Sprung ans
                // Ende erscheint sie vollstaendig.
                if self.blick_ab == 0 {
                    fb.zeichen_zeichnen(
                        self.spalte * self.zeichen_breite,
                        self.zeile * ZELLEN_HOEHE,
                        zeichen,
                        FONT_GROESSE,
                        FONT_GEWICHT,
                        self.vordergrund,
                        self.hintergrund,
                    );
                    let pixel_zeile = self.zeile * ZELLEN_HOEHE;
                    self.dirty_markieren(pixel_zeile, pixel_zeile + ZELLEN_HOEHE);
                }
                self.spalte += 1;
            }
        }
    }

    /// Zeilenumbruch — scrollt bei Bedarf (memmove im Back-Buffer).
    fn neue_zeile(&mut self, fb: &mut DoppelPuffer) {
        self.spalte = 0;
        if self.zeile + 1 < self.zeilen {
            self.zeile += 1;
        } else {
            // Die oberste Zeile verlaesst den Schirm — ab in den Rueckblick.
            self.oberste_zeile_aufheben();
            if self.blick_ab == 0 {
                // Bewusst mit der STANDARD-Hintergrundfarbe leeren —
                // die Lektion aus VGA-Zeiten gegen Farbstreifen.
                fb.hochscrollen(ZELLEN_HOEHE, self.standard_hintergrund);
                // Nach dem Scrollen hat sich ALLES verschoben:
                self.dirty_markieren(0, self.zeilen * ZELLEN_HOEHE);
            }
        }
    }
}

impl KonsolenZustand {
    /// Legt die oberste Rasterzeile im Rueckblick ab und schiebt die
    /// Zellen nach.
    fn oberste_zeile_aufheben(&mut self) {
        if !self.rueckblick_da() {
            return;
        }
        if !self.historie.is_empty() {
            let ziel = self.historie_kopf * self.spalten;
            self.historie[ziel..ziel + self.spalten]
                .copy_from_slice(&self.zellen[..self.spalten]);
            self.historie_kopf = (self.historie_kopf + 1) % MAX_HISTORIE;
            self.historie_zeilen = (self.historie_zeilen + 1).min(MAX_HISTORIE);
            // Den Blick MITZIEHEN, damit zurueckgeblaetterte Sicht steht.
            if self.blick_ab > 0 {
                self.blick_ab = (self.blick_ab + 1).min(self.historie_zeilen);
            }
        }
        // Zellen eine Zeile hoch, unterste leeren.
        self.zellen.copy_within(self.spalten.., 0);
        let ab = (self.zeilen - 1) * self.spalten;
        let leer = KZelle {
            zeichen: ' ',
            vg: self.standard_hintergrund,
            hg: self.standard_hintergrund,
        };
        self.zellen[ab..].fill(leer);
    }

    /// Die Zelle, die an dieser Bildschirmposition zu sehen sein soll.
    fn sicht_zelle(&self, spalte: usize, zeile: usize) -> KZelle {
        let leer = KZelle {
            zeichen: ' ',
            vg: self.standard_hintergrund,
            hg: self.standard_hintergrund,
        };
        if self.blick_ab == 0 || zeile >= self.blick_ab {
            let live = zeile - self.blick_ab;
            return self
                .zellen
                .get(live * self.spalten + spalte)
                .copied()
                .unwrap_or(leer);
        }
        let hinauf = self.blick_ab - zeile;
        if hinauf > self.historie_zeilen || self.historie.is_empty() {
            return leer;
        }
        let ring = MAX_HISTORIE;
        let index = (self.historie_kopf + ring - hinauf % ring) % ring;
        self.historie
            .get(index * self.spalten + spalte)
            .copied()
            .unwrap_or(leer)
    }

    /// Zeichnet den GANZEN sichtbaren Bereich aus den Zellen neu.
    ///
    /// Das ist der Preis des Rueckblicks: Beim Blaettern gibt es kein
    /// memmove-Kunststueck, es muss gemalt werden. Bei ~160x45 Zellen sind
    /// das 7200 Glyphen — einmal je Tastendruck, nicht je Zeichen.
    fn neu_zeichnen(&mut self, fb: &mut DoppelPuffer) {
        fb.fuellen(self.standard_hintergrund);
        for zeile in 0..self.zeilen {
            for spalte in 0..self.spalten {
                let zelle = self.sicht_zelle(spalte, zeile);
                if zelle.zeichen == ' ' && zelle.hg == self.standard_hintergrund {
                    continue;
                }
                fb.zeichen_zeichnen(
                    spalte * self.zeichen_breite,
                    zeile * ZELLEN_HOEHE,
                    zelle.zeichen,
                    FONT_GROESSE,
                    FONT_GEWICHT,
                    zelle.vg,
                    zelle.hg,
                );
            }
        }
        self.dirty_markieren(0, self.zeilen * ZELLEN_HOEHE);
    }
}

/// RICHTET DEN RUECKBLICK EIN — einmalig, nach der Heap-Erweiterung.
///
/// WARUM NICHT IN `init()`: Das laeuft direkt nach `framebuffer::init`, und
/// zu dem Zeitpunkt hat der Kernel nur den kleinen Anfangs-Heap. Der
/// Rueckblick braucht je nach Aufloesung ein bis zwei MiB — die gibt es
/// erst nach `allocator::heap_erweitern`.
///
/// WARUM UEBERHAUPT VORAB: Damit im `print!`-Pfad NIE alloziert wird. `_print`
/// haelt den KONSOLE-Lock; ginge dabei der Speicher aus, wollte der
/// `alloc_error_handler` seinerseits drucken — ein Deadlock in genau der
/// Funktion, die ihn melden soll.
///
/// Vor diesem Aufruf laeuft die Konsole wie immer, nur ohne Rueckblick:
/// Die Boot-Meldungen sind also nicht zurueckblaetterbar.
pub fn rueckblick_einrichten() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        if konsole.spalten == 0 || konsole.zeilen == 0 || konsole.rueckblick_da() {
            return;
        }
        let leer = KZelle {
            zeichen: ' ',
            vg: konsole.standard_hintergrund,
            hg: konsole.standard_hintergrund,
        };
        let (spalten, zeilen) = (konsole.spalten, konsole.zeilen);
        konsole.zellen = vec![leer; spalten * zeilen];
        konsole.historie = vec![leer; MAX_HISTORIE * spalten];
        konsole.historie_zeilen = 0;
        konsole.historie_kopf = 0;
        konsole.blick_ab = 0;
    });
    crate::serial_println!("[konsole] Rueckblick bereit ({} Zeilen).", MAX_HISTORIE);
}

/// BLAETTERT in der Vollbild-Konsole. Positiv = nach oben.
/// `seitenweise` blaettert einen ganzen Schirm. Liefert `true`, wenn sich
/// etwas geaendert hat.
pub fn blaettern(zeilen: isize, seitenweise: bool) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        if !konsole.rueckblick_da() {
            return false;
        }
        let schritt = if seitenweise {
            zeilen.signum() * (konsole.zeilen.saturating_sub(2)).max(1) as isize
        } else {
            zeilen
        };
        let vorher = konsole.blick_ab;
        let ziel = konsole.blick_ab as isize + schritt;
        konsole.blick_ab = ziel.clamp(0, konsole.historie_zeilen as isize) as usize;
        if konsole.blick_ab == vorher {
            return false;
        }
        framebuffer::mit_framebuffer(|fb| {
            konsole.neu_zeichnen(fb);
            fb.present();
        });
        true
    })
}

/// Springt ans Ende (zum Live-Bild) — beim Tippen.
pub fn zum_ende() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        if !konsole.rueckblick_da() || konsole.blick_ab == 0 {
            return;
        }
        konsole.blick_ab = 0;
        framebuffer::mit_framebuffer(|fb| {
            konsole.neu_zeichnen(fb);
            fb.present();
        });
    });
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
    crate::wacht::punkt(crate::wacht::Punkt::Konsole);
    use core::fmt::Write;

    x86_64::instructions::interrupts::without_interrupts(|| {
        // Seit dem Platten-Log läuft ALLES dreifach: Bildschirm,
        // seriell — und in den Log-Puffer (reiner Blatt-Lock; der
        // Log-Task schreibt ihn rotierend nach /platte/system).
        // Vor der Heap-Initialisierung ist das ein No-Op (siehe
        // protokoll::anhaengen_args).
        crate::protokoll::anhaengen_args(args);
        let mut zustand = KONSOLE.lock();
        // Desktop-Modus: Der Bildschirm gehört dem Compositor! Die
        // Ausgabe geht ins Terminal-Fenster ihrer SITZUNG — Shell-
        // Ausgaben in ihr eigenes Fenster (ausgabe_setzen im
        // Shell-Task), Kernel-Log ins Haupt-Terminal. Ist KEIN
        // Terminal offen, wird Kernel-Log GEPUFFERT (und beim
        // nächsten Terminal-Öffnen nachgereicht); Ausgaben einer
        // geschlossenen Shell-Sitzung verfallen dagegen bewusst.
        // (Lock-Ordnung: KONSOLE -> MANAGER, siehe Deadlock-Regeln.)
        if crate::fenster::desktop_aktiv() {
            let ziel = crate::shell::sitzung::ausgabe_ziel();
            let geschrieben = ziel != 0
                && crate::fenster::terminal_schreiben(
                    ziel,
                    args,
                    zustand.vordergrund,
                    zustand.hintergrund,
                );
            if !geschrieben && crate::shell::sitzung::ausgabe_ist_kernel_log() {
                crate::shell::sitzung::log_puffern(
                    alloc::format!("{}", args),
                    zustand.vordergrund,
                    zustand.hintergrund,
                );
            }
            return;
        }
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
    // Desktop-Modus: Der clear-Befehl leert das Terminal-Fenster
    // SEINER Sitzung (bzw. das Haupt-Terminal beim Kernel-Log).
    if crate::fenster::desktop_aktiv() {
        crate::fenster::terminal_leeren(crate::shell::sitzung::ausgabe_ziel());
        serial_print!("\x1b[2J\x1b[H");
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut konsole = KONSOLE.lock();
        konsole.spalte = 0;
        konsole.zeile = 0;
        // `clear` wirft auch den Rueckblick — dieselbe Entscheidung wie im
        // Terminal-Fenster: „mach sauber", nicht „schieb es aus dem Blick".
        let leer = KZelle {
            zeichen: ' ',
            vg: konsole.standard_hintergrund,
            hg: konsole.standard_hintergrund,
        };
        konsole.zellen.fill(leer);
        konsole.historie_zeilen = 0;
        konsole.historie_kopf = 0;
        konsole.blick_ab = 0;
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
    // Im Desktop-Modus bleibt der Konsolen-Cursor aus — der Blink-
    // Task würde sonst mitten in den Desktop malen. Das Terminal-
    // Fenster zeichnet seinen eigenen Cursor.
    if crate::fenster::desktop_aktiv() {
        return;
    }
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
        // Blink-Tempo aus den Einstellungen (wie der Textfeld-Cursor).
        crate::zeit::warte_ms(crate::einstellungen::cursor_blink_ms()).await;
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
