// netz/geraet.rs — Die geräteunabhängige Netzwerk-Naht (analog BlockDevice)
//
// Genau wie JEDER Massenspeicher hinter dem schmalen `BlockDevice`-Trait
// steckt (fs/block.rs), steckt JEDE Netzwerkkarte hinter `NetzGeraet`.
// Der künftige Stack (ARP, IPv4, UDP, TCP) redet AUSSCHLIESSLICH mit
// diesem Trait — nie mit virtio-net direkt. Dadurch ließe sich ein
// e1000-/rtl8139-Treiber später ergänzen, ohne eine Zeile Stack-Code zu
// ändern (er müsste nur `NetzGeraet` erfüllen).
//
// Die drei Fähigkeiten einer NIC, die der Stack braucht:
//   * `mac()`           — unsere Hardware-Adresse (steht in jedem Frame,
//                         das wir senden, als Quelle).
//   * `sende_frame()`   — ein rohes Ethernet-Frame rausschicken (der
//                         Treiber packt gerätespezifische Köpfe wie den
//                         virtio_net_hdr selbst davor).
//   * `empfange_frame()`— das nächste empfangene Frame abholen (der
//                         Treiber drainiert dafür seine Hardware-Queue).
//
// DER RX-WEG (empfangen) ist das Neue gegenüber BlockDevice: Netz-Pakete
// kommen UNAUFGEFORDERT. Deshalb signalisiert der GERÄTE-IRQ nur "es liegt
// etwas an" (rx_signal → Waker), und der async `netz_task` (mod.rs) wacht
// auf, sammelt die Frames per `frames_einsammeln` ein und verteilt sie.
// Das ist exakt das Tastatur-/Maus-Muster: winziger Interrupt-Handler,
// die Arbeit macht ein Task. Der Handler LOCKT NICHTS und ALLOZIERT NICHT
// (das Kopieren der Frame-Bytes passiert im Task-Kontext).

use super::ethernet::Mac;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::task::Poll;
use futures_util::task::AtomicWaker;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// Fehler auf dem Netz-Pfad. Bewusst klein und `Copy` — wandert als
/// Ergebnis von `sende_frame` bis in Shell-Ausgaben.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetzFehler {
    /// Keine Netzwerkkarte registriert (oder noch nicht bereit).
    KeinGeraet,
    /// Das Gerät konnte das Frame nicht in die Sende-Queue legen
    /// (Queue voll — sollte bei unserem niedrigen Takt nie passieren).
    Sendefehler,
    /// Das Frame passt nicht in den Sende-DMA-Puffer (> MTU).
    FrameZuGross,
    /// Das Gerät hat das Senden nicht rechtzeitig bestätigt (Timeout).
    Zeitueberschreitung,
    /// Es ist keine statische IP konfiguriert (netz-ip ...), aber die
    /// Operation braucht eine (z. B. ein ARP-Request mit Absender-IP).
    NichtKonfiguriert,
}

impl NetzFehler {
    /// Deutsche Fehlermeldung für Shell und Diagnose.
    pub fn meldung(&self) -> &'static str {
        match self {
            NetzFehler::KeinGeraet => "keine Netzwerkkarte vorhanden",
            NetzFehler::Sendefehler => "die Sende-Queue ist voll",
            NetzFehler::FrameZuGross => "das Frame ist zu gross (ueber MTU)",
            NetzFehler::Zeitueberschreitung => "das Geraet bestaetigt das Senden nicht (Timeout)",
            NetzFehler::NichtKonfiguriert => "keine IP konfiguriert (netz-ip <ip> <maske> <gateway>)",
        }
    }
}

/// DIE Schnittstelle, die jeder Netzwerkkarten-Treiber erfüllen muss.
/// `&mut self` überall: echte NICs haben veränderlichen Zustand
/// (DMA-Ringe, Sende-Puffer).
pub trait NetzGeraet: Send {
    /// Unsere MAC-Adresse (aus der Geräte-Config gelesen).
    fn mac(&self) -> Mac;
    /// Sendet ein rohes Ethernet-Frame (ohne gerätespezifische Köpfe —
    /// die fügt der Treiber selbst hinzu). Blockiert bis zur Bestätigung
    /// oder einem Timeout.
    fn sende_frame(&mut self, frame: &[u8]) -> Result<(), NetzFehler>;
    /// Holt das nächste empfangene Frame (Ethernet-Nutzlast, ohne
    /// gerätespezifische Köpfe). None = die Empfangs-Queue ist leer.
    /// Nicht-blockierend — der Aufrufer ruft in einer Schleife, bis None.
    fn empfange_frame(&mut self) -> Option<Vec<u8>>;
}

// ---------------------------------------------------------------------------
// Globaler Zustand: die registrierte NIC + der RX-Weckmechanismus
// ---------------------------------------------------------------------------

/// Die registrierte Netzwerkkarte (heute genau eine: virtio-net). Wie die
/// Laufwerks-Registry ein BLATT-Lock — nur aus Task-Kontext genommen, NIE
/// aus dem Interrupt-Handler.
static GERAET: Mutex<Option<Box<dyn NetzGeraet>>> = Mutex::new(None);

/// Der `netz_task` schläft hier, der Geräte-IRQ weckt.
static RX_WAKER: AtomicWaker = AtomicWaker::new();
/// Vom IRQ gesetzt: "es gibt empfangene Frames abzuholen".
static RX_BEREIT: AtomicBool = AtomicBool::new(false);

/// Registriert die Netzwerkkarte (der Treiber ruft das am Ende seiner
/// Init). Ab jetzt kann der Stack senden und empfangen.
pub fn geraet_registrieren(geraet: Box<dyn NetzGeraet>) {
    without_interrupts(|| *GERAET.lock() = Some(geraet));
}

/// Ist eine Netzwerkkarte registriert?
pub fn vorhanden() -> bool {
    without_interrupts(|| GERAET.lock().is_some())
}

/// NUR FÜR TESTS: entfernt die registrierte NIC wieder, damit spätere
/// Tests wieder den sauberen „keine NIC"-Zustand vorfinden.
#[cfg(test)]
pub fn geraet_zuruecksetzen() {
    without_interrupts(|| *GERAET.lock() = None);
}

/// Unsere MAC-Adresse (None, wenn keine NIC da ist).
pub fn mac() -> Option<Mac> {
    without_interrupts(|| GERAET.lock().as_ref().map(|g| g.mac()))
}

/// Sendet ein rohes Ethernet-Frame über die registrierte NIC.
pub fn sende_frame(frame: &[u8]) -> Result<(), NetzFehler> {
    // TEST-Verlust: so tun, als wäre das Frame auf der Leitung verlorengegangen
    // (der Absender erfährt davon nichts — genau wie in echt).
    if verlust_wuerfeln() {
        return Ok(());
    }
    without_interrupts(|| match GERAET.lock().as_mut() {
        Some(geraet) => geraet.sende_frame(frame),
        None => Err(NetzFehler::KeinGeraet),
    })
}

// ---------------------------------------------------------------------------
// Künstlicher Paketverlust — ein TEST-/DIAGNOSE-Werkzeug
// ---------------------------------------------------------------------------
//
// Um den Retransmit-Pfad gegen ECHTE Gegenstellen zu prüfen, brauchen wir
// Verlust. Auf einem Windows-Host mit QEMU-slirp gibt es kein tc/netem, also
// werfen wir die Frames an UNSERER Geräte-Naht weg — in beide Richtungen.
// Für die obere Schicht ist das ununterscheidbar von echtem Verlust.
// STANDARD 0 (aus); nur Tests/Diagnose schalten es ein.

static VERLUST_PROZENT: AtomicU32 = AtomicU32::new(0);
static VERLUST_RNG: AtomicU64 = AtomicU64::new(0x1234_5678_9abc_def0);

/// Setzt den künstlichen Verlust je Richtung in Prozent (0 = aus).
pub fn verlust_setzen(prozent: u32) {
    VERLUST_PROZENT.store(prozent.min(100), Ordering::Relaxed);
}

/// Der aktuell eingestellte künstliche Verlust.
pub fn verlust_prozent() -> u32 {
    VERLUST_PROZENT.load(Ordering::Relaxed)
}

/// Würfelt lock-frei, ob dieses Frame "verlorengeht".
fn verlust_wuerfeln() -> bool {
    let p = VERLUST_PROZENT.load(Ordering::Relaxed) as u64;
    if p == 0 {
        return false;
    }
    let alt = VERLUST_RNG.load(Ordering::Relaxed);
    let neu = alt
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    VERLUST_RNG.store(neu, Ordering::Relaxed);
    (neu >> 33) % 100 < p
}

/// Wird vom GERÄTE-IRQ (über den Treiber) gerufen: RX-Bereitschaft
/// signalisieren und den `netz_task` wecken. Interrupt-tauglich: nur
/// Atomics, KEIN Lock, KEINE Allokation.
pub fn rx_signal() {
    RX_BEREIT.store(true, Ordering::Release);
    RX_WAKER.wake();
}

/// Sammelt ALLE bereitliegenden Frames vom Gerät ein und gibt sie zurück.
/// WICHTIG: Der GERÄT-Lock wird HIER gehalten, aber VOR dem Dispatch
/// (im netz_task) wieder losgelassen — so kann das Verarbeiten eines
/// Frames gefahrlos ein Antwort-Frame senden (das nimmt den Lock erneut),
/// ohne einen verschachtelten Lock / Deadlock. Genau das Muster, das
/// auch der alte RX-Hexdump nutzte.
pub fn frames_einsammeln() -> Vec<Vec<u8>> {
    without_interrupts(|| {
        let mut lock = GERAET.lock();
        let mut frames = Vec::new();
        if let Some(geraet) = lock.as_mut() {
            while let Some(frame) = geraet.empfange_frame() {
                // Leere Frames (Runts) überspringen, aber weiter drainieren.
                // TEST-Verlust wirkt auch auf dem Empfangsweg.
                if !frame.is_empty() && !verlust_wuerfeln() {
                    frames.push(frame);
                }
            }
        }
        frames
    })
}

/// Wartet asynchron, bis der IRQ RX_BEREIT setzt — race-frei per
/// Doppel-Check (wie der Scancode-Stream): Kam der Interrupt genau
/// zwischen dem ersten Check und register(), fängt der zweite Check ihn.
pub async fn rx_warten() {
    core::future::poll_fn(|cx| {
        if RX_BEREIT.swap(false, Ordering::Acquire) {
            return Poll::Ready(());
        }
        RX_WAKER.register(cx.waker());
        if RX_BEREIT.swap(false, Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}
