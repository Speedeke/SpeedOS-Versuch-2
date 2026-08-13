// ===========================================================================
// src/wacht.rs — der Wachhund: ein eingefrorenes System sagt, wo es steht
// ===========================================================================
//
// WOZU DIESE DATEI DA IST
//
// Auf echter Hardware gibt es keine serielle Ausgabe. Bleibt SpeedOS dort
// stehen, ist das Einzige, was uebrig bleibt, ein Foto vom Bildschirm —
// und darauf steht dann genau das, was VOR dem Stillstand da war. Das ist
// keine Information; man kann daran nicht ablesen, WORAN es lag.
//
// Der Wachhund macht daraus eine Aussage. Er laeuft im Timer-Interrupt,
// prueft, ob das System noch vorankommt, und malt bei Stillstand einen
// BALKEN an den oberen Bildschirmrand, der den zuletzt erreichten
// Programmpunkt nennt.
//
// ---------------------------------------------------------------------------
// DIE DREI REGELN, DIE IHN BENUTZBAR MACHEN
//
// (1) ER NIMMT KEINEN LOCK. Er laeuft in genau der Lage, in der ein Lock
//     die Ursache sein koennte — auf einen zu warten hiesse, im selben
//     Loch zu landen. Deshalb merkt sich `einrichten()` Zeiger und Masse
//     des Framebuffers in Atomics, und `balken_malen` schreibt direkt
//     dorthin. Es gibt keinen anderen Weg, der in dieser Lage noch
//     funktioniert.
//
// (2) ER MALT KEINE SCHRIFT, SONDERN KAESTCHEN. Ein Zeichensatz braucht
//     Tabellen und Rechnerei; Kaestchen sind ein paar Schreibzugriffe.
//     Der Programmpunkt steht als ANZAHL weisser Kaestchen auf rotem
//     Grund da — das laesst sich auf einem Handyfoto abzaehlen, und mehr
//     braucht es nicht.
//
// (3) ER MELDET EINMAL, NICHT DAUERND. Ein Wachhund, der im Sekundentakt
//     ueber den Bildschirm malt, macht aus einem stehenden System ein
//     blinkendes. Nach der Meldung ist Ruhe, bis wieder Fortschritt
//     gemessen wurde.
//
// ---------------------------------------------------------------------------
// WAS ER **NICHT** KANN — und das gehoert dazu
//
// Er haengt am Timer-Interrupt. Steht die Maschine mit AUSGESCHALTETEN
// Interrupts (Endlosschleife unter `without_interrupts`, Triple Fault,
// CPU angehalten), dann laeuft auch er nicht mehr. Er faengt die
// haeufigere Sorte: eine Schleife oder ein Warten, das nie endet,
// waehrend die Interrupts weiterlaufen.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Die Programmpunkte
// ---------------------------------------------------------------------------

/// Wo war das System zuletzt? Die Zahl ist zugleich die Anzahl der
/// Kaestchen, die der Wachhund malt — deshalb faengt sie bei 1 an und
/// bleibt klein.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Punkt {
    /// Nichts gesetzt.
    Keiner = 0,
    /// Der Executor dreht seine Runde (der Normalfall).
    Executor = 1,
    /// Ein Bild wird zusammengesetzt.
    Compositor = 2,
    /// Ein Vollbild-/Bereichs-Transfer in den echten Framebuffer.
    Present = 3,
    /// Konsolen-Ausgabe.
    Konsole = 4,
    /// Der Tastatur-Task verarbeitet Scancodes.
    Tastatur = 5,
    /// Der Maus-Task verarbeitet Pakete.
    Maus = 6,
    /// Die Shell fuehrt einen Befehl aus.
    Shell = 7,
    /// Der USB-Task fragt den Controller ab.
    Usb = 8,
    /// Der Audio-Mixer schiebt Abtastwerte nach.
    Audio = 9,
    /// Das Dateisystem arbeitet.
    Datei = 10,
    /// Das Netz arbeitet.
    Netz = 11,
}

impl Punkt {
    /// Der Name fuer die serielle Ausgabe (in QEMU liest man den, auf
    /// Hardware zaehlt man die Kaestchen).
    pub fn text(self) -> &'static str {
        match self {
            Punkt::Keiner => "keiner",
            Punkt::Executor => "Executor",
            Punkt::Compositor => "Compositor",
            Punkt::Present => "Present (Framebuffer-Transfer)",
            Punkt::Konsole => "Konsole",
            Punkt::Tastatur => "Tastatur",
            Punkt::Maus => "Maus",
            Punkt::Shell => "Shell",
            Punkt::Usb => "USB",
            Punkt::Audio => "Audio",
            Punkt::Datei => "Dateisystem",
            Punkt::Netz => "Netz",
        }
    }
}

/// Der zuletzt gemeldete Programmpunkt.
static PUNKT: AtomicU32 = AtomicU32::new(0);
/// Der Fortschrittszaehler. Wer ihn erhoeht, sagt „ich lebe".
static HERZSCHLAG: AtomicU64 = AtomicU64::new(0);

/// Sagt dem Wachhund, wo wir gerade sind. KEIN Fortschritt — nur der Ort.
///
/// Absichtlich getrennt von `schlag()`: Ein Programmpunkt zu betreten ist
/// etwas anderes, als ihn zu verlassen. Wer beim Betreten „ich lebe"
/// meldete, koennte darin ewig haengen, ohne dass es auffiele.
#[inline]
pub fn punkt(p: Punkt) {
    PUNKT.store(p as u32, Ordering::Relaxed);
}

/// Meldet FORTSCHRITT. Der Executor ruft das je Runde.
#[inline]
pub fn schlag() {
    HERZSCHLAG.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// WELCHER TASK — die Frage, die der grobe Programmpunkt offen laesst
// ---------------------------------------------------------------------------
//
// Ein Stillstand mit dem Punkt „Executor" heisst nur: Es haengt in
// IRGENDEINEM Kernel-Task, der keine eigene Wegmarke gesetzt hat. Das
// grenzt ein, benennt aber nicht. Deshalb merkt sich der Executor vor
// JEDEM Poll, welchen Task er gerade anfasst.
//
// Der NAME ist ein `&'static str` — er lebt ewig, also darf sein Zeiger
// gefahrlos in Atomics liegen und aus dem Interrupt heraus gelesen
// werden. Kein Lock, keine Allokation, zwei Speicheroperationen.

/// Der Name des zuletzt angefassten Tasks — KOPIERT, nicht verwiesen.
///
/// Ein Zeiger waere der naheliegende Weg und hier falsch: Task-Namen
/// sind `String`s, und ein Task kann enden. Der Wachhund laeuft im
/// Interrupt und wuerde dann in freigegebenen Speicher lesen. 24 Byte
/// zu kopieren kostet nichts und kann nicht schiefgehen.
///
/// EHRLICHE GRENZE: Die Kopie ist nicht atomar. Trifft der Timer genau
/// waehrend des Schreibens, steht dort ein Mischtext aus zwei Namen.
/// Das ist ein Schoenheitsfehler in einer Diagnose-Ausgabe und kein
/// Sicherheitsproblem — die Laenge wird geklemmt, es wird nie ueber den
/// Puffer hinaus gelesen.
static TASK_NAME: Mutexfrei = Mutexfrei::neu();
static TASK_NAME_LEN: AtomicUsize = AtomicUsize::new(0);
static TASK_NUMMER: AtomicU32 = AtomicU32::new(0);

/// Ein fester Namenspuffer ohne Lock. `UnsafeCell`, weil er aus dem
/// Interrupt gelesen und aus dem Executor geschrieben wird — beides auf
/// EINEM Kern, es gibt also keine echte Gleichzeitigkeit, nur eine
/// Unterbrechung mitten im Schreiben (siehe Grenze oben).
struct Mutexfrei(core::cell::UnsafeCell<[u8; NAME_MAX]>);
const NAME_MAX: usize = 24;
// SAFETY: Einkern-System; der Inhalt ist reiner Diagnosetext, und jeder
// Lesezugriff klemmt auf die gespeicherte Laenge.
unsafe impl Sync for Mutexfrei {}
impl Mutexfrei {
    const fn neu() -> Self {
        Mutexfrei(core::cell::UnsafeCell::new([0; NAME_MAX]))
    }
}

/// Sagt dem Wachhund, welcher Task gleich gepollt wird.
///
/// `nummer` ist die Task-Nummer (Reihenfolge des Spawns) — sie wird als
/// zweite Kaestchenreihe gemalt, denn ein Name laesst sich ohne
/// Zeichensatz nicht zeichnen, eine Anzahl schon.
#[inline]
pub fn task_setzen(nummer: u32, name: &str) {
    let n = name.len().min(NAME_MAX);
    // SAFETY: siehe `Mutexfrei` — Einkern, fester Puffer, geklemmte
    // Laenge. Es wird nie ueber `NAME_MAX` hinaus geschrieben.
    unsafe {
        let ziel = &mut *TASK_NAME.0.get();
        ziel[..n].copy_from_slice(&name.as_bytes()[..n]);
    }
    TASK_NAME_LEN.store(n, Ordering::Relaxed);
    TASK_NUMMER.store(nummer, Ordering::Relaxed);
}

/// Der Name des zuletzt angefassten Tasks — `None`, solange keiner lief.
pub fn letzter_task() -> Option<&'static str> {
    let len = TASK_NAME_LEN.load(Ordering::Relaxed).min(NAME_MAX);
    if len == 0 {
        return None;
    }
    // SAFETY: siehe oben. Ungueltiges UTF-8 (durch eine Unterbrechung
    // mitten im Kopieren) wird zu None statt zu einem Absturz.
    unsafe {
        let puffer = &*TASK_NAME.0.get();
        core::str::from_utf8(&puffer[..len]).ok()
    }
}

/// Die laufende Nummer des zuletzt angefassten Tasks.
pub fn letzte_task_nummer() -> u32 {
    TASK_NUMMER.load(Ordering::Relaxed)
}

/// Der zuletzt gemeldete Punkt (fuer Diagnose-Anzeigen).
pub fn letzter_punkt() -> Punkt {
    match PUNKT.load(Ordering::Relaxed) {
        1 => Punkt::Executor,
        2 => Punkt::Compositor,
        3 => Punkt::Present,
        4 => Punkt::Konsole,
        5 => Punkt::Tastatur,
        6 => Punkt::Maus,
        7 => Punkt::Shell,
        8 => Punkt::Usb,
        9 => Punkt::Audio,
        10 => Punkt::Datei,
        11 => Punkt::Netz,
        _ => Punkt::Keiner,
    }
}

// ---------------------------------------------------------------------------
// Der gemerkte Framebuffer — der einzige Weg, der im Stillstand noch geht
// ---------------------------------------------------------------------------

static FB_ZEIGER: AtomicUsize = AtomicUsize::new(0);
static FB_STRIDE: AtomicUsize = AtomicUsize::new(0);
static FB_BPP: AtomicUsize = AtomicUsize::new(0);
static FB_BREITE: AtomicUsize = AtomicUsize::new(0);
static FB_HOEHE: AtomicUsize = AtomicUsize::new(0);
static SCHARF: AtomicBool = AtomicBool::new(false);

/// Merkt sich den echten Framebuffer, damit der Wachhund ihn ohne Lock
/// erreichen kann, und schaltet ihn scharf.
///
/// # Safety
///
/// `zeiger` muss auf den echten, dauerhaft gemappten Framebuffer zeigen
/// und mindestens `stride * hoehe * bpp` Byte gross sein. Der Bereich
/// gehoert danach dauerhaft auch dem Wachhund — er schreibt hinein, ohne
/// zu fragen. Das ist gefahrlos, weil ein Framebuffer keine Daten
/// enthaelt, die jemand zurueckliest.
pub unsafe fn einrichten(
    zeiger: *mut u8,
    stride: usize,
    bpp: usize,
    breite: usize,
    hoehe: usize,
) {
    FB_ZEIGER.store(zeiger as usize, Ordering::Relaxed);
    FB_STRIDE.store(stride, Ordering::Relaxed);
    FB_BPP.store(bpp, Ordering::Relaxed);
    FB_BREITE.store(breite, Ordering::Relaxed);
    FB_HOEHE.store(hoehe, Ordering::Relaxed);
    SCHARF.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Die Ueberwachung
// ---------------------------------------------------------------------------

/// So viele Ticks ohne Fortschritt gelten als Stillstand. Bei 250 Hz
/// sind 750 Ticks genau 3 Sekunden.
///
/// WARUM NICHT KUERZER: Ein Vollbild-Transfer auf ungecachtem Speicher
/// kann bei 4K ueber eine halbe Sekunde dauern, und ein synchroner
/// Shell-Befehl (der den Executor anhaelt, siehe `starte`) darf laenger
/// laufen. Ein Wachhund, der bei jeder legitimen Verzoegerung anschlaegt,
/// wird ignoriert — und ist dann keiner mehr.
const STILLSTAND_TICKS: u32 = 750;

static LETZTER_HERZSCHLAG: AtomicU64 = AtomicU64::new(0);
static STILL_SEIT: AtomicU32 = AtomicU32::new(0);
static SCHON_GEMELDET: AtomicBool = AtomicBool::new(false);
/// Wie oft der Wachhund insgesamt angeschlagen hat.
static MELDUNGEN: AtomicU32 = AtomicU32::new(0);

/// Wie oft der Wachhund angeschlagen hat (fuer den Befund-Schirm und
/// die Tests).
pub fn meldungen() -> u32 {
    MELDUNGEN.load(Ordering::Relaxed)
}

/// Laeuft im Timer-Interrupt. Muss winzig sein und darf NICHTS
/// blockieren — deshalb ausschliesslich Atomics.
pub fn tick() {
    if !SCHARF.load(Ordering::Relaxed) {
        return;
    }
    let jetzt = HERZSCHLAG.load(Ordering::Relaxed);
    let vorher = LETZTER_HERZSCHLAG.swap(jetzt, Ordering::Relaxed);

    if jetzt != vorher {
        // Es geht voran. Zaehler zuruecksetzen — und die Sperre loesen,
        // damit ein SPAETERER Stillstand wieder gemeldet wird.
        STILL_SEIT.store(0, Ordering::Relaxed);
        SCHON_GEMELDET.store(false, Ordering::Relaxed);
        return;
    }

    let still = STILL_SEIT.fetch_add(1, Ordering::Relaxed) + 1;
    if still < STILLSTAND_TICKS || SCHON_GEMELDET.swap(true, Ordering::Relaxed) {
        return;
    }

    MELDUNGEN.fetch_add(1, Ordering::Relaxed);
    let p = letzter_punkt();
    crate::serial_println!(
        "[wacht] STILLSTAND seit {} Ticks — Punkt: {} ({}), Task: {} (Nr. {})",
        still,
        p.text(),
        p as u32,
        letzter_task().unwrap_or("unbekannt"),
        letzte_task_nummer()
    );
    balken_malen(p as u32, letzte_task_nummer());
}

/// Malt den Befund an den oberen Bildschirmrand: ein roter Streifen und
/// darauf so viele weisse Kaestchen, wie der Programmpunkt gross ist.
///
/// Schreibt OHNE Lock direkt in den Framebuffer — siehe Regel (1) im
/// Kopfkommentar. Das ist der ganze Sinn dieser Funktion.
fn balken_malen(punkt_nummer: u32, task_nummer: u32) {
    let zeiger = FB_ZEIGER.load(Ordering::Relaxed);
    if zeiger == 0 {
        return;
    }
    let stride = FB_STRIDE.load(Ordering::Relaxed);
    let bpp = FB_BPP.load(Ordering::Relaxed);
    let breite = FB_BREITE.load(Ordering::Relaxed);
    let hoehe = FB_HOEHE.load(Ordering::Relaxed);
    if bpp == 0 || breite == 0 || hoehe < BALKEN_HOEHE {
        return;
    }

    // Roter Grund, damit der Balken auch auf einem hellen Desktop
    // unuebersehbar ist.
    let flaeche = Flaeche { zeiger, stride, bpp };
    fuellen(flaeche, 0, 0, breite, BALKEN_HOEHE, 0xE0, 0x20, 0x20);
    // ZWEITE REIHE: die laufende Nummer des Tasks, in dem es haengt.
    // Blau, damit man die beiden Reihen auf einem Foto nicht verwechselt.
    if hoehe >= 2 * BALKEN_HOEHE {
        fuellen(
            flaeche,
            0,
            BALKEN_HOEHE,
            breite,
            BALKEN_HOEHE,
            0x20,
            0x60,
            0xE0,
        );
        let kasten2 = BALKEN_HOEHE - 2 * RAND;
        for i in 0..task_nummer as usize {
            let x = RAND + i * (kasten2 + RAND);
            if x + kasten2 > breite {
                break;
            }
            fuellen(
                flaeche,
                x,
                BALKEN_HOEHE + RAND,
                kasten2,
                kasten2,
                0xFF,
                0xFF,
                0xFF,
            );
        }
    }

    // Die Kaestchen. Sie beginnen mit einem Abstand vom Rand, damit man
    // sieht, wo die Reihe anfaengt, und sind quadratisch.
    let kasten = BALKEN_HOEHE - 2 * RAND;
    for i in 0..punkt_nummer as usize {
        let x = RAND + i * (kasten + RAND);
        if x + kasten > breite {
            break;
        }
        fuellen(flaeche, x, RAND, kasten, kasten, 0xFF, 0xFF, 0xFF);
    }
}

const BALKEN_HOEHE: usize = 24;
const RAND: usize = 4;

/// Die gemerkten Framebuffer-Masse als EIN Argument — sonst haette
/// `fuellen` zehn Parameter, und dann verwechselt man beim Aufruf
/// Breite und Hoehe, ohne dass der Compiler es merkt.
#[derive(Clone, Copy)]
struct Flaeche {
    zeiger: usize,
    stride: usize,
    bpp: usize,
}

/// Fuellt ein Rechteck im echten Framebuffer. Grenzen werden geprueft;
/// die Farbe wird in BEIDEN gaengigen Formaten gleich hell dargestellt,
/// weil der Wachhund das Pixelformat nicht kennt — Rot und Blau
/// vertauscht zu haben ist bei einem Warnbalken bedeutungslos, ein
/// Absturz beim Malen dagegen nicht.
#[allow(clippy::too_many_arguments)]
fn fuellen(f: Flaeche, x: usize, y: usize, breite: usize, hoehe: usize, r: u8, g: u8, b: u8) {
    let Flaeche { zeiger, stride, bpp } = f;
    for zeile in y..y + hoehe {
        for spalte in x..x + breite {
            let versatz = (zeile * stride + spalte) * bpp;
            // SAFETY: `zeiger` wurde in `einrichten` als gueltiger
            // Framebuffer mit diesen Massen hinterlegt; `versatz` bleibt
            // durch die Schleifengrenzen darin. Wir schreiben nur.
            unsafe {
                let ziel = (zeiger as *mut u8).add(versatz);
                ziel.write_volatile(b);
                if bpp > 1 {
                    ziel.add(1).write_volatile(g);
                }
                if bpp > 2 {
                    ziel.add(2).write_volatile(r);
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Punkt muss als Zahl hin und zurueck reisen — er geht durch
    /// ein Atomic, und die Kaestchen-Anzahl haengt an genau dieser Zahl.
    #[test_case]
    fn test_punkt_rundreise() {
        for p in [
            Punkt::Executor,
            Punkt::Compositor,
            Punkt::Present,
            Punkt::Konsole,
            Punkt::Tastatur,
            Punkt::Maus,
            Punkt::Shell,
            Punkt::Usb,
            Punkt::Audio,
            Punkt::Datei,
            Punkt::Netz,
        ] {
            punkt(p);
            assert_eq!(letzter_punkt(), p, "{} kam nicht zurueck", p.text());
            assert!(!p.text().is_empty());
        }
    }

    /// Solange es vorangeht, schlaegt der Wachhund NIE an — das ist die
    /// Eigenschaft, ohne die er unbrauchbar waere.
    #[test_case]
    fn test_kein_fehlalarm_bei_fortschritt() {
        let vorher = meldungen();
        SCHARF.store(true, Ordering::Relaxed);
        // Deutlich mehr Ticks als die Schwelle — aber mit Herzschlag.
        for _ in 0..(STILLSTAND_TICKS * 3) {
            schlag();
            tick();
        }
        assert_eq!(
            meldungen(),
            vorher,
            "der Wachhund hat angeschlagen, obwohl es voranging"
        );
    }

    /// Und umgekehrt: ohne Fortschritt schlaegt er an — GENAU EINMAL,
    /// nicht bei jedem Tick.
    #[test_case]
    fn test_stillstand_wird_genau_einmal_gemeldet() {
        let vorher = meldungen();
        SCHARF.store(true, Ordering::Relaxed);
        punkt(Punkt::Present);
        // Erst einen Schlag, damit der Zustand definiert ist.
        schlag();
        tick();
        for _ in 0..(STILLSTAND_TICKS * 3) {
            tick(); // kein schlag() — Stillstand
        }
        assert_eq!(
            meldungen(),
            vorher + 1,
            "genau eine Meldung erwartet, nicht eine je Tick"
        );

        // Und nach neuem Fortschritt darf er wieder anschlagen.
        schlag();
        tick();
        for _ in 0..(STILLSTAND_TICKS * 3) {
            tick();
        }
        assert_eq!(meldungen(), vorher + 2, "der zweite Stillstand fehlt");
    }
}
