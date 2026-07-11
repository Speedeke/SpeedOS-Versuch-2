// maus.rs — PS/2-Maus: Treiber, Paket-Parser, async Task und Cursor
//
// Die Maus hängt am ZWEITEN Port des 8042-Controllers — demselben
// Chip, der auch die Tastatur bedient. Deshalb höchste Vorsicht bei
// der Initialisierung: Wir fassen nur die Maus-Bits der Controller-
// Konfiguration an (IRQ 12 an, Maus-Takt an) und lassen alles, was
// der Tastatur gehört, unangetastet.
//
// Arbeitsteilung wie bei der Tastatur:
//   IRQ-12-Handler: Byte von Port 0x60 lesen -> lock-freie Queue ->
//       Waker anstoßen -> fertig (nie blockieren, nie allozieren!).
//   maus_task (async): setzt aus den Bytes Pakete zusammen, pflegt
//       den globalen Maus-Zustand, zeichnet den Cursor und reicht
//       Events an Interessenten weiter (aktuell: die Grafik-Demo).
//
// Cursor-Konzept (passend zum Double Buffering): Der Pfeil wird NUR
// in den Front-Buffer gemalt (Overlay). Der Back-Buffer bleibt die
// "Wahrheit ohne Cursor" — Wiederherstellen des Untergrunds ist
// einfach ein present_bereich() der alten Cursor-Position.

use crate::framebuffer::{self, Farbe};
use crate::zeit;
use conquer_once::spin::OnceCell;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::task::{Context, Poll};
use crossbeam_queue::ArrayQueue;
use futures_util::stream::{Stream, StreamExt};
use futures_util::task::AtomicWaker;
use spin::Mutex;
use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// Events und globaler Zustand
// ---------------------------------------------------------------------------

/// Die Maustasten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MausTaste {
    Links,
    Rechts,
    Mitte,
}

/// Was die Maus gerade getan hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MausEvent {
    /// Neue (bereits auf den Bildschirm begrenzte) Position.
    Bewegt { x: i32, y: i32 },
    Gedrueckt(MausTaste),
    Losgelassen(MausTaste),
    /// Scrollrad: positiv = nach oben, negativ = nach unten.
    Gescrollt(i8),
}

/// Der globale Maus-Zustand.
struct MausZustand {
    x: i32,
    y: i32,
    links: bool,
    rechts: bool,
    mitte: bool,
    /// Bildschirmgrenzen (fürs Klemmen der Position).
    max_x: i32,
    max_y: i32,
}

static MAUS: Mutex<MausZustand> = Mutex::new(MausZustand {
    x: 100,
    y: 100,
    links: false,
    rechts: false,
    mitte: false,
    max_x: 799,
    max_y: 599,
});

/// Aktuelle Cursor-Position (x, y).
pub fn position() -> (i32, i32) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let maus = MAUS.lock();
        (maus.x, maus.y)
    })
}

// ---------------------------------------------------------------------------
// Paket-Parsing (reine Logik — unit-getestet!)
// ---------------------------------------------------------------------------

/// Ein fertig zerlegtes Maus-Paket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paket {
    /// Bewegung: positive Werte = nach rechts / nach UNTEN
    /// (die Hardware meldet Y invertiert — hier schon umgerechnet).
    pub dx: i32,
    pub dy: i32,
    pub links: bool,
    pub rechts: bool,
    pub mitte: bool,
    /// Scrollrad-Schritte (nur im IntelliMouse-Modus, sonst 0).
    pub scroll: i8,
}

/// Zerlegt ein rohes PS/2-Maus-Paket.
///
/// Kopf-Byte: Bit 0-2 = Tasten L/R/M, Bit 3 = IMMER 1 (Sync-Prüfung!),
/// Bit 4/5 = Vorzeichen von X/Y (9-Bit-Zweierkomplement), Bit 6/7 =
/// Überlauf (dann ist die Bewegung unbrauchbar -> Paket verwerfen).
/// `rad`: das 4. Byte im IntelliMouse-Modus (4-Bit-Vorzeichenzahl).
pub fn paket_parsen(kopf: u8, x_byte: u8, y_byte: u8, rad: Option<u8>) -> Option<Paket> {
    // Sync-Bit fehlt? Dann sind wir mitten in einem Paket eingestiegen.
    if kopf & 0b0000_1000 == 0 {
        return None;
    }
    // Überlauf: Bewegung nicht vertrauenswürdig — weg damit.
    if kopf & 0b1100_0000 != 0 {
        return None;
    }

    // 9-Bit-Zweierkomplement: Das Vorzeichen-Bit steckt im Kopf-Byte.
    let dx = x_byte as i32 - if kopf & 0b0001_0000 != 0 { 256 } else { 0 };
    let dy_hardware = y_byte as i32 - if kopf & 0b0010_0000 != 0 { 256 } else { 0 };

    // Das Rad-Byte ist eine 4-Bit-Vorzeichenzahl (-8..=7):
    // erst auf 8 Bit hochschieben, dann arithmetisch zurück.
    let scroll = rad.map(|r| ((r << 4) as i8) >> 4).unwrap_or(0);

    Some(Paket {
        dx,
        // Hardware: positiv = nach oben. Bildschirm: positiv = nach unten.
        dy: -dy_hardware,
        links: kopf & 0b001 != 0,
        rechts: kopf & 0b010 != 0,
        mitte: kopf & 0b100 != 0,
        scroll,
    })
}

// ---------------------------------------------------------------------------
// Controller-Initialisierung (VOR dem Aktivieren der Interrupts!)
// ---------------------------------------------------------------------------

/// Läuft die Maus im IntelliMouse-Modus (4-Byte-Pakete mit Rad)?
static RAD_MODUS: AtomicBool = AtomicBool::new(false);

/// Wartet, bis der Controller Daten HAT (Status-Bit 0). false = Timeout.
fn warte_auf_daten() -> bool {
    let mut status: Port<u8> = Port::new(0x64);
    for _ in 0..100_000 {
        // unsafe (Port-I/O): Status-Register des 8042, nur lesen.
        if unsafe { status.read() } & 0b01 != 0 {
            return true;
        }
    }
    false
}

/// Wartet, bis der Controller Daten ANNIMMT (Status-Bit 1 frei).
fn warte_auf_frei() -> bool {
    let mut status: Port<u8> = Port::new(0x64);
    for _ in 0..100_000 {
        if unsafe { status.read() } & 0b10 == 0 {
            return true;
        }
    }
    false
}

/// Schickt ein Kommando an den CONTROLLER (Port 0x64).
fn controller_kommando(kommando: u8) -> bool {
    if !warte_auf_frei() {
        return false;
    }
    let mut port: Port<u8> = Port::new(0x64);
    unsafe { port.write(kommando) };
    true
}

/// Schickt ein Byte an die MAUS (0xD4-Präfix leitet zum zweiten Port
/// um — sonst würde die Tastatur es bekommen!) und wartet aufs ACK.
fn maus_kommando(byte: u8) -> bool {
    if !controller_kommando(0xD4) || !warte_auf_frei() {
        return false;
    }
    let mut daten: Port<u8> = Port::new(0x60);
    unsafe { daten.write(byte) };
    // Auf das ACK (0xFA) der Maus warten:
    if !warte_auf_daten() {
        return false;
    }
    let antwort: u8 = unsafe { daten.read() };
    antwort == 0xFA
}

/// Liest ein Antwort-Byte der Maus (nach einem Kommando).
fn maus_antwort() -> Option<u8> {
    if !warte_auf_daten() {
        return None;
    }
    let mut daten: Port<u8> = Port::new(0x60);
    Some(unsafe { daten.read() })
}

/// Initialisiert die PS/2-Maus. MUSS mit deaktivierten Interrupts
/// laufen (lib::init, vor sti) — wir pollen die ACKs, da darf kein
/// Handler dazwischenfunken. Gibt false zurück, wenn keine Maus da
/// ist (alle Warteschleifen haben Timeouts — der Boot hängt nie).
pub fn initialisieren() -> bool {
    // 1. Zweiten PS/2-Port (die Maus) einschalten.
    if !controller_kommando(0xA8) {
        return false;
    }

    // 2. Controller-Konfiguration anpassen — NUR die Maus-Bits!
    //    Bit 1 = IRQ 12 aktivieren, Bit 5 = Maus-Takt an (0 = an).
    //    Die Tastatur-Bits (0, 4, 6) bleiben exakt wie sie sind.
    if !controller_kommando(0x20) {
        return false;
    }
    let konfiguration = match maus_antwort() {
        Some(k) => (k | 0b0000_0010) & !0b0010_0000,
        None => return false,
    };
    if !controller_kommando(0x60) || !warte_auf_frei() {
        return false;
    }
    let mut daten: Port<u8> = Port::new(0x60);
    // unsafe (Port-I/O): geprüfte Konfiguration zurückschreiben.
    unsafe { daten.write(konfiguration) };

    // 3. Maus auf Standardwerte setzen.
    if !maus_kommando(0xF6) {
        return false;
    }

    // 4. IntelliMouse-Modus (Scrollrad) aktivieren: die magische
    //    Abtastraten-Sequenz 200, 100, 80 — danach meldet die Maus
    //    bei der ID-Abfrage 0x03 und schickt 4-Byte-Pakete.
    let magie_ok = maus_kommando(0xF3)
        && maus_kommando(200)
        && maus_kommando(0xF3)
        && maus_kommando(100)
        && maus_kommando(0xF3)
        && maus_kommando(80);
    if magie_ok && maus_kommando(0xF2) {
        if let Some(id) = maus_antwort() {
            RAD_MODUS.store(id == 0x03, Ordering::Relaxed);
        }
    }

    // 5. Abtastrate auf 200 Meldungen/s: Die Magie-Sequenz oben hat
    //    sie auf 80 stehen lassen — für flüssige Cursor-Bewegung
    //    wollen wir das Maximum (mehr, kleinere Deltas pro Paket).
    let _ = maus_kommando(0xF3) && maus_kommando(200);

    // 6. Daten-Meldungen einschalten — ab jetzt feuert IRQ 12.
    maus_kommando(0xF4)
}

// ---------------------------------------------------------------------------
// Byte-Queue (Interrupt -> Task), wie bei der Tastatur
// ---------------------------------------------------------------------------

static MAUS_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static MAUS_WAKER: AtomicWaker = AtomicWaker::new();

/// Wird vom IRQ-12-Handler gerufen (interrupts.rs): nie blockieren!
pub(crate) fn byte_hinzufuegen(byte: u8) {
    if let Ok(queue) = MAUS_QUEUE.try_get() {
        // Volle Queue: Byte verwerfen — der Parser resynchronisiert
        // sich über das Sync-Bit im nächsten Paket-Kopf.
        let _ = queue.push(byte);
        MAUS_WAKER.wake();
    }
}

/// Stream der rohen Maus-Bytes.
struct MausByteStream {
    _privat: (),
}

impl MausByteStream {
    fn neu() -> Self {
        MAUS_QUEUE
            .try_init_once(|| ArrayQueue::new(256))
            .expect("MausByteStream::neu darf nur einmal laufen");
        MausByteStream { _privat: () }
    }
}

impl Stream for MausByteStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = MAUS_QUEUE.try_get().expect("Maus-Queue nicht initialisiert");
        if let Some(byte) = queue.pop() {
            return Poll::Ready(Some(byte));
        }
        MAUS_WAKER.register(cx.waker());
        match queue.pop() {
            Some(byte) => {
                MAUS_WAKER.take();
                Poll::Ready(Some(byte))
            }
            None => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Der Maus-Task: Pakete zusammensetzen, Zustand pflegen, Cursor malen
// ---------------------------------------------------------------------------

/// Läuft "ewig" im Executor.
pub async fn maus_task() {
    let mut bytes = MausByteStream::neu();

    // Bildschirmgrenzen aus dem Framebuffer übernehmen:
    if let Some(info) = framebuffer::mit_framebuffer(|fb| fb.info()) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut maus = MAUS.lock();
            maus.max_x = info.width as i32 - 1;
            maus.max_y = info.height as i32 - 1;
            maus.x = info.width as i32 / 2;
            maus.y = info.height as i32 / 2;
        });
    }
    let rad_modus = RAD_MODUS.load(Ordering::Relaxed);
    let paket_laenge: usize = if rad_modus { 4 } else { 3 };

    // Cursor initial zeichnen:
    let (mut cursor_x, mut cursor_y) = position();
    cursor_zeichnen(cursor_x, cursor_y);

    let mut puffer = [0u8; 4];
    let mut erhalten = 0usize;

    while let Some(byte) = bytes.next().await {
        // Resynchronisation: Das erste Byte MUSS das Sync-Bit tragen.
        if erhalten == 0 && byte & 0b0000_1000 == 0 {
            continue;
        }
        puffer[erhalten] = byte;
        erhalten += 1;
        if erhalten < paket_laenge {
            continue;
        }
        erhalten = 0;

        let rad = if rad_modus { Some(puffer[3]) } else { None };
        let paket = match paket_parsen(puffer[0], puffer[1], puffer[2], rad) {
            Some(p) => p,
            None => continue,
        };

        // Zustand aktualisieren + Events ableiten:
        let mut events = [None::<MausEvent>; 5];
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut maus = MAUS.lock();
            let mut n = 0;
            if paket.dx != 0 || paket.dy != 0 {
                maus.x = (maus.x + paket.dx).clamp(0, maus.max_x);
                maus.y = (maus.y + paket.dy).clamp(0, maus.max_y);
                events[n] = Some(MausEvent::Bewegt { x: maus.x, y: maus.y });
                n += 1;
            }
            for (neu, alt, taste) in [
                (paket.links, maus.links, MausTaste::Links),
                (paket.rechts, maus.rechts, MausTaste::Rechts),
                (paket.mitte, maus.mitte, MausTaste::Mitte),
            ] {
                if neu != alt {
                    events[n] = Some(if neu {
                        MausEvent::Gedrueckt(taste)
                    } else {
                        MausEvent::Losgelassen(taste)
                    });
                    n += 1;
                }
            }
            if paket.scroll != 0 {
                events[n] = Some(MausEvent::Gescrollt(-paket.scroll));
            }
            maus.links = paket.links;
            maus.rechts = paket.rechts;
            maus.mitte = paket.mitte;
        });

        // Cursor neu positionieren (Untergrund wiederherstellen,
        // Pfeil an neuer Stelle als Overlay malen):
        let (neu_x, neu_y) = position();
        if (neu_x, neu_y) != (cursor_x, cursor_y) {
            cursor_entfernen(cursor_x, cursor_y);
            cursor_zeichnen(neu_x, neu_y);
            (cursor_x, cursor_y) = (neu_x, neu_y);
        }

        // Events weiterreichen: Desktop-Fenster oder Grafik-Demo.
        for event in events.into_iter().flatten() {
            if crate::fenster::desktop_aktiv() {
                crate::fenster::maus_event(&event);
            } else {
                crate::grafik::demo_maus_event(&event);
                // Nach Demo-Zeichnungen den Cursor wieder obenauf legen:
                if crate::grafik::demo_aktiv() {
                    cursor_zeichnen(cursor_x, cursor_y);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Der Cursor-Pfeil (Overlay im Front-Buffer)
// ---------------------------------------------------------------------------

/// Der klassische Pfeil, 12x17, 'D' = Umriss, 'w' = Füllung,
/// '.' = durchsichtig. Gezeichnet wird 2x skaliert (24x34 Pixel).
const CURSOR_BILD: [&str; 17] = [
    "D...........",
    "DD..........",
    "DwD.........",
    "DwwD........",
    "DwwwD.......",
    "DwwwwD......",
    "DwwwwwD.....",
    "DwwwwwwD....",
    "DwwwwwwwD...",
    "DwwwwwwwwD..",
    "DwwwwwDDDDD.",
    "DwwDwwD.....",
    "DwD.DwwD....",
    "DD..DwwD....",
    "D....DwwD...",
    ".....DwwD...",
    "......DD....",
];
const CURSOR_SKALIERUNG: usize = 2;
/// Restore-Box: groß genug für JEDE Cursor-Form (Pfeil + Resize-Pfeile).
const CURSOR_BREITE: usize = 34;
const CURSOR_HOEHE: usize = 36;

/// Die Cursor-Form (der Fenster-Manager stellt sie an Fensterrändern um).
pub const FORM_PFEIL: u8 = 0;
pub const FORM_HORIZONTAL: u8 = 1; // <->  (Breite ändern)
pub const FORM_VERTIKAL: u8 = 2; //   arrow up/down (Höhe ändern)
pub const FORM_DIAG_NWSE: u8 = 3; // \   (Ecke oben-links / unten-rechts)
pub const FORM_DIAG_NESW: u8 = 4; // /   (Ecke oben-rechts / unten-links)

static CURSOR_FORM: AtomicU8 = AtomicU8::new(FORM_PFEIL);

/// Stellt die Cursor-Form um (0 = Pfeil, siehe FORM_*-Konstanten).
pub fn cursor_form_setzen(form: u8) {
    if CURSOR_FORM.swap(form, Ordering::Relaxed) != form {
        // Form geändert: sofort neu zeichnen (alte Form wegräumen).
        let (x, y) = position();
        cursor_entfernen(x, y);
        cursor_zeichnen(x, y);
    }
}

/// Malt den Cursor als Overlay in den FRONT-Buffer — Pfeil oder,
/// je nach eingestellter Form, einen Resize-Doppelpfeil.
fn cursor_zeichnen(x: i32, y: i32) {
    let form = CURSOR_FORM.load(Ordering::Relaxed);
    framebuffer::mit_framebuffer(|fb| {
        if form == FORM_PFEIL {
            for (zeile, text) in CURSOR_BILD.iter().enumerate() {
                for (spalte, zeichen) in text.chars().enumerate() {
                    let farbe = match zeichen {
                        'D' => Farbe::neu(0x10, 0x14, 0x1c),
                        'w' => Farbe::neu(0xf8, 0xfa, 0xfc),
                        _ => continue,
                    };
                    for dy in 0..CURSOR_SKALIERUNG {
                        for dx in 0..CURSOR_SKALIERUNG {
                            let px = x + (spalte * CURSOR_SKALIERUNG + dx) as i32;
                            let py = y + (zeile * CURSOR_SKALIERUNG + dy) as i32;
                            if px >= 0 && py >= 0 {
                                fb.pixel_setzen_vorne(px as usize, py as usize, farbe);
                            }
                        }
                    }
                }
            }
        } else {
            resize_cursor_zeichnen(fb, x + 14, y + 16, form);
        }
    });
}

/// Zeichnet einen Resize-Doppelpfeil (weiß mit dunklem Rand) um das
/// Zentrum (cx, cy). Richtung ergibt sich aus der Form.
fn resize_cursor_zeichnen(fb: &mut framebuffer::DoppelPuffer, cx: i32, cy: i32, form: u8) {
    let (ex, ey) = match form {
        FORM_HORIZONTAL => (1, 0),
        FORM_VERTIKAL => (0, 1),
        FORM_DIAG_NWSE => (1, 1),
        _ => (1, -1), // FORM_DIAG_NESW
    };
    let (px, py) = (-ey, ex); // Senkrechte (für die Pfeilspitzen)
    let laenge = 9;

    // Alle Pixel des Doppelpfeils einsammeln (Linie + zwei Spitzen):
    let mut punkte: alloc::vec::Vec<(i32, i32)> = alloc::vec::Vec::new();
    for t in -laenge..=laenge {
        punkte.push((cx + ex * t, cy + ey * t));
    }
    for ende in [laenge, -laenge] {
        let (sx, sy) = (cx + ex * ende, cy + ey * ende);
        let (rx, ry) = (-ex * ende.signum(), -ey * ende.signum()); // nach innen
        for k in 1..=4 {
            punkte.push((sx + rx * k + px * k, sy + ry * k + py * k));
            punkte.push((sx + rx * k - px * k, sy + ry * k - py * k));
        }
    }

    // Erst dunkler Rand (3x3 um jeden Punkt), dann weiße Kerne obenauf.
    let rand = Farbe::neu(0x10, 0x14, 0x1c);
    let kern = Farbe::neu(0xf8, 0xfa, 0xfc);
    for &(ax, ay) in punkte.iter() {
        for dy in -1..=1 {
            for dx in -1..=1 {
                if ax + dx >= 0 && ay + dy >= 0 {
                    fb.pixel_setzen_vorne((ax + dx) as usize, (ay + dy) as usize, rand);
                }
            }
        }
    }
    for &(ax, ay) in punkte.iter() {
        if ax >= 0 && ay >= 0 {
            fb.pixel_setzen_vorne(ax as usize, ay as usize, kern);
        }
    }
}

/// Zeichnet den Cursor an der AKTUELLEN Position neu — für den
/// Compositor, dessen present() das Overlay überschrieben hat.
pub fn cursor_neu_zeichnen() {
    let (x, y) = position();
    cursor_zeichnen(x, y);
}

/// Stellt den Untergrund an der alten Cursor-Position wieder her
/// (der Back-Buffer weiß ja, wie es dort ohne Cursor aussieht).
fn cursor_entfernen(x: i32, y: i32) {
    framebuffer::mit_framebuffer(|fb| {
        fb.present_bereich(
            x.max(0) as usize,
            y.max(0) as usize,
            CURSOR_BREITE,
            CURSOR_HOEHE,
        );
    });
}

/// Kleine Pause fürs Demo-Zeichnen (Re-Export der Zeit-API,
/// damit maus.rs keine zweite Abhängigkeit braucht).
#[allow(dead_code)]
async fn kurz_warten() {
    zeit::warte_ms(50).await;
}

// ---------------------------------------------------------------------------
// Tests — reines Paket-Parsing, keine Hardware nötig
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Bewegung mit und ohne Vorzeichen (9-Bit-Zweierkomplement).
    #[test_case]
    fn test_paket_bewegung() {
        // Nach rechts (+5) und HOCH (+3 laut Hardware -> dy = -3):
        let p = paket_parsen(0b0000_1000, 5, 3, None).unwrap();
        assert_eq!((p.dx, p.dy), (5, -3));

        // Nach links: X-Vorzeichen-Bit + 0xFF = -1:
        let p = paket_parsen(0b0001_1000, 0xFF, 0, None).unwrap();
        assert_eq!(p.dx, -1);

        // Nach unten: Y-Vorzeichen-Bit + 0xF6 = -10 (Hardware) = +10 Bildschirm:
        let p = paket_parsen(0b0010_1000, 0, 0xF6, None).unwrap();
        assert_eq!(p.dy, 10);
    }

    /// Sync-Bit und Überlauf führen zum Verwerfen des Pakets.
    #[test_case]
    fn test_paket_verwerfen() {
        // Sync-Bit (Bit 3) fehlt:
        assert_eq!(paket_parsen(0b0000_0000, 1, 1, None), None);
        // X-Überlauf:
        assert_eq!(paket_parsen(0b0100_1000, 1, 1, None), None);
        // Y-Überlauf:
        assert_eq!(paket_parsen(0b1000_1000, 1, 1, None), None);
    }

    /// Tasten und Scrollrad (4-Bit-Vorzeichenzahl im 4. Byte).
    #[test_case]
    fn test_paket_tasten_und_rad() {
        let p = paket_parsen(0b0000_1101, 0, 0, Some(0x01)).unwrap();
        assert!(p.links && p.mitte && !p.rechts);
        assert_eq!(p.scroll, 1);

        // 0x0F = -1 als 4-Bit-Zweierkomplement:
        let p = paket_parsen(0b0000_1000, 0, 0, Some(0x0F)).unwrap();
        assert_eq!(p.scroll, -1);

        // Ohne Rad-Byte (3-Byte-Modus): scroll = 0.
        let p = paket_parsen(0b0000_1000, 0, 0, None).unwrap();
        assert_eq!(p.scroll, 0);
    }
}
