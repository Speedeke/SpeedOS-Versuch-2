// virtio/net.rs — virtio-net als NetzGeraet (Serie 5, Aufgabe 1)
//
// Schritt 1 der Serie 5 war der reine EMPFANG (Hexdump, kein Stack). Jetzt
// bekommt der Treiber seine Architektur-Naht: Er implementiert das
// geräteunabhängige Trait `netz::NetzGeraet` (analog `BlockDevice`) und
// registriert sich beim Boot in der Netz-Schicht. Ab hier redet der Stack
// (Ethernet/ARP/…) NUR noch über das Trait — ein e1000/rtl8139 ließe sich
// später ohne Stack-Änderung ergänzen.
//
// Was der Treiber weiter selbst macht (geräteSPEZIFISCH):
//   * PCI-Legacy-Transport (Port-I/O-BAR), Init-Sequenz wie virtio-blk,
//   * MEHRERE Virtqueues: RX = 0 (Empfang), TX = 1 (Senden),
//   * INTERRUPTS statt Polling für RX (Pakete kommen unaufgefordert) —
//     der IRQ liest nur das ISR (quittiert) und WECKT die Netz-Schicht,
//   * RX-Puffer vorab einstellen und nach dem Verbrauch neu einstellen
//     (RxRing), jedes Frame trägt vorne einen virtio_net_hdr (10/12 Byte).
//
// Die Virtqueue (virtio/virtqueue.rs) wird UNVERÄNDERT weiterbenutzt.

use crate::netz::{self, NetzFehler, NetzGeraet};
use crate::virtio::virtqueue::Virtqueue;
use crate::{memory, pci, serial_println};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};
use x86_64::instructions::port::Port;
use x86_64::{PhysAddr, VirtAddr};

// --- PCI-Kennung ---------------------------------------------------------
const VIRTIO_VENDOR: u16 = 0x1AF4;
/// Legacy/transitional virtio-net (0x1000) bzw. modern-only (0x1041).
const VIRTIO_NET_IDS: [u16; 2] = [0x1000, 0x1041];

// --- Legacy-Register-Offsets (ab I/O-Basis, wie in blk.rs) --------------
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_DRIVER_FEATURES: u16 = 0x04;
const REG_QUEUE_ADDRESS: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_ISR: u16 = 0x13;
const REG_CONFIG: u16 = 0x14; // Device-Config: net = mac[6], status u16, ...

// --- Device-Status-Bits --------------------------------------------------
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 128;

// --- virtio-net Feature-Bits --------------------------------------------
/// Das Gerät liefert eine gültige MAC in der Config.
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
/// Empfangs-Puffer dürfen zusammengefasst werden (dann 12-Byte-Header).
const VIRTIO_NET_F_MRG_RXBUF: u32 = 1 << 15;

// --- ISR-Bits ------------------------------------------------------------
/// Bit 0: eine Virtqueue hat sich bewegt (Used-Ring aktualisiert).
const ISR_QUEUE: u8 = 1;

// --- RX-Puffer -----------------------------------------------------------
/// So viele RX-Puffer stellen wir dem Gerät bereit.
const RX_ANZAHL: usize = 16;
/// Größe je RX-Puffer: reichlich für ein Standard-Frame (1514) + Header.
/// 2048 teilt 4096 glatt, also liegt jeder Puffer in EINER Page (=
/// physisch zusammenhängend, keine Seitengrenze überschritten).
const PUFFER_BYTES: usize = 2048;
/// Größe des (einen) TX-DMA-Puffers — eine ganze Page.
const TX_PUFFER_BYTES: usize = 4096;
/// Wie lange wir höchstens auf die TX-Bestätigung warten (µs).
const TX_TIMEOUT_US: u64 = 100_000;

// ---------------------------------------------------------------------------
// Globaler Zustand (nur das, was der IRQ lock-frei braucht)
// ---------------------------------------------------------------------------

/// I/O-Basis des Geräts — LOCK-FREI, damit der IRQ-Handler das
/// ISR-Register lesen kann, ohne einen Lock zu nehmen. 0 = kein Gerät.
static IO_BASIS: AtomicU16 = AtomicU16::new(0);

/// Die reine Header-Längen-Logik: Ohne MRG_RXBUF ist der virtio_net_hdr
/// 10 Byte, mit 12 (er trägt dann zusätzlich num_buffers: u16).
fn hdr_laenge(features_negotiated: u32) -> usize {
    if features_negotiated & VIRTIO_NET_F_MRG_RXBUF != 0 {
        12
    } else {
        10
    }
}

// ---------------------------------------------------------------------------
// RxRing — die RX-Puffer-Ringführung (der ohne-Gerät-testbare Kern)
// ---------------------------------------------------------------------------

/// Verwaltet die vorab eingestellten RX-Puffer und ihre Zuordnung zu
/// den Deskriptoren. Jeder Puffer ist EIN gerätebeschreibbarer
/// Deskriptor; nach dem Verbrauch wird derselbe Puffer wieder
/// eingestellt (der Ring bleibt immer voll).
struct RxRing {
    vq: Virtqueue,
    /// Virtuelle/physische Basis je Puffer (Index = Puffer-Nummer).
    puffer_virt: Vec<VirtAddr>,
    puffer_phys: Vec<PhysAddr>,
    /// Deskriptor-Kopf -> Puffer-Index (-1 = dieser Deskriptor trägt
    /// gerade keinen unserer Puffer). Länge = Queue-Größe.
    kopf_zu_index: Vec<i32>,
}

impl RxRing {
    /// Legt `anzahl` DMA-Puffer physisch zusammenhängend an (kein
    /// Bounce nötig — sie gehören uns) und richtet die Zuordnungs-Tabelle
    /// ein. Die Puffer werden noch NICHT eingestellt (das macht
    /// `alle_einstellen`, nachdem die Queue-Adresse im Gerät steht).
    fn neu(queue_size: u16, anzahl: usize, puffer_bytes: usize) -> RxRing {
        let pages = (anzahl * puffer_bytes).div_ceil(4096);
        let basis = memory::allocate_pages(pages).expect("virtio-net RX-DMA");
        let mut puffer_virt = Vec::with_capacity(anzahl);
        let mut puffer_phys = Vec::with_capacity(anzahl);
        for i in 0..anzahl {
            let v = basis + (i * puffer_bytes) as u64;
            puffer_virt.push(v);
            puffer_phys.push(memory::uebersetzen(v).expect("virtio-net RX-Physik"));
        }
        RxRing {
            vq: Virtqueue::neu(queue_size),
            puffer_virt,
            puffer_phys,
            kopf_zu_index: alloc::vec![-1; queue_size as usize],
        }
    }

    /// Stellt EINEN Puffer (gerätebeschreibbar) in die Queue ein und
    /// merkt sich Kopf -> Index. Liefert false, wenn kein Deskriptor
    /// frei ist (sollte nie passieren, N < Queue-Größe).
    fn einstellen(&mut self, index: usize) -> bool {
        let phys = self.puffer_phys[index];
        let bytes = PUFFER_BYTES as u32;
        match self.vq.kette_anlegen(&[(phys, bytes, true)]) {
            Some(kopf) => {
                self.kopf_zu_index[kopf as usize] = index as i32;
                self.vq.verfuegbar_machen(kopf);
                true
            }
            None => false,
        }
    }

    /// Stellt alle Puffer ein (beim Aufsetzen).
    fn alle_einstellen(&mut self) {
        for i in 0..self.puffer_virt.len() {
            self.einstellen(i);
        }
    }

    /// Holt das nächste empfangene Frame ab: (Puffer-Index, Gesamtlänge
    /// inkl. virtio_net_hdr). Der Puffer ist danach FREI und muss vom
    /// Aufrufer wieder eingestellt werden.
    fn abholen(&mut self) -> Option<(usize, u32)> {
        let (kopf, laenge) = self.vq.used_abholen()?;
        let index = self.kopf_zu_index[kopf as usize];
        self.kopf_zu_index[kopf as usize] = -1;
        if index < 0 {
            // Sollte nie passieren — defensiv: Puffer verwerfen.
            return None;
        }
        Some((index as usize, laenge))
    }
}

// ---------------------------------------------------------------------------
// VirtioNet — der Treiber-Zustand, hinter dem NetzGeraet-Trait
// ---------------------------------------------------------------------------

struct VirtioNet {
    io_basis: u16,
    rx: RxRing,
    tx: Virtqueue,
    tx_puffer_virt: VirtAddr,
    tx_puffer_phys: PhysAddr,
    hdr_len: usize,
    mac: [u8; 6],
}

// SICHERHEIT: enthält rohe DMA-Zeiger, ist aber nur über den GERAET-Mutex
// der Netz-Schicht erreichbar und wird nie zwischen Threads geteilt —
// Send ist erfüllt.
unsafe impl Send for VirtioNet {}

impl VirtioNet {
    /// Stößt das Gerät für die RX-Queue (0) an.
    fn notify_rx(&self) {
        // unsafe (Port-I/O): Queue-Notify erwartet den Queue-Index.
        unsafe { Port::<u16>::new(self.io_basis + REG_QUEUE_NOTIFY).write(0) };
    }
    /// Stößt das Gerät für die TX-Queue (1) an.
    fn notify_tx(&self) {
        // unsafe (Port-I/O): dito, Queue 1.
        unsafe { Port::<u16>::new(self.io_basis + REG_QUEUE_NOTIFY).write(1) };
    }
}

impl NetzGeraet for VirtioNet {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Sendet ein rohes Ethernet-Frame über die TX-Queue: [virtio_net_hdr
    /// (genullt) | Frame] in den TX-DMA-Puffer, an das Gerät verfügbar
    /// machen, anstoßen und auf die Bestätigung warten (damit der EINE
    /// TX-Puffer fürs nächste Senden wieder frei ist).
    fn sende_frame(&mut self, frame: &[u8]) -> Result<(), NetzFehler> {
        if frame.len() > TX_PUFFER_BYTES - self.hdr_len {
            return Err(NetzFehler::FrameZuGross);
        }
        // Alte, bereits erledigte TX-Deskriptoren zurückgeben (die
        // TX-Queue läuft interruptfrei, wir räumen sie hier selbst ab).
        while self.tx.used_abholen().is_some() {}

        // In den TX-DMA-Puffer: [virtio_net_hdr genullt | Frame].
        // unsafe: tx_puffer_virt ist unser DMA-Puffer (>= hdr_len + Frame).
        unsafe {
            let p = self.tx_puffer_virt.as_mut_ptr::<u8>();
            core::ptr::write_bytes(p, 0, self.hdr_len);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), p.add(self.hdr_len), frame.len());
        }
        let gesamt = (self.hdr_len + frame.len()) as u32;
        // Das GERÄT LIEST diesen Puffer (schreibt nicht) -> false.
        let kopf = self
            .tx
            .kette_anlegen(&[(self.tx_puffer_phys, gesamt, false)])
            .ok_or(NetzFehler::Sendefehler)?;
        self.tx.verfuegbar_machen(kopf);
        self.notify_tx();

        // Auf Bestätigung warten (gepollt mit TSC-Timeout — nie endlos).
        let start = crate::zeit::us_seit_boot();
        loop {
            if self.tx.used_abholen().is_some() {
                return Ok(());
            }
            if crate::zeit::us_seit_boot().saturating_sub(start) > TX_TIMEOUT_US {
                return Err(NetzFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }
    }

    /// Holt das nächste empfangene Frame (ohne virtio_net_hdr) und stellt
    /// den Puffer wieder ein. None = die RX-Queue ist leer. Bei einem
    /// Runt (Länge <= Header) gibt es einen leeren Vec zurück — der
    /// Aufrufer (frames_einsammeln) überspringt ihn, drainiert aber weiter.
    fn empfange_frame(&mut self) -> Option<Vec<u8>> {
        let (index, laenge) = self.rx.abholen()?;
        // AUDIT-HÄRTUNG: `laenge` kommt aus dem Used-Ring, also VOM GERÄT. Ein
        // fehlerhaftes/böswilliges Gerät könnte mehr melden, als der Puffer
        // fasst — wir KLEMMEN deshalb auf PUFFER_BYTES, bevor wir daraus einen
        // Slice bauen. So gilt die Sicherheits-Invariante unten IMMER, egal was
        // das Gerät behauptet.
        let laenge = (laenge as usize).min(PUFFER_BYTES);
        let hdr = self.hdr_len;
        let frame = if laenge > hdr {
            // SICHERHEIT: `index` ist ein gültiger, uns gehörender Puffer-Index
            // (aus abholen, 0..RX_ANZAHL), und laenge <= PUFFER_BYTES ist durch
            // die Klemmung oben garantiert — der Slice bleibt also vollständig
            // im DMA-Puffer.
            let slice = unsafe {
                core::slice::from_raw_parts(
                    self.rx.puffer_virt[index].as_ptr::<u8>().add(hdr),
                    laenge - hdr,
                )
            };
            slice.to_vec()
        } else {
            Vec::new()
        };
        // Puffer wieder einstellen und das Gerät anstoßen.
        self.rx.einstellen(index);
        self.notify_rx();
        Some(frame)
    }
}

// ---------------------------------------------------------------------------
// Interrupt-Pfad (aus interrupts.rs gerufen — KEIN Lock, KEINE Allokation)
// ---------------------------------------------------------------------------

/// Wird vom PCI-IRQ-Handler gerufen. Liest das ISR-Register des Geräts
/// (das QUITTIERT den Interrupt an der Hardware) und weckt die Netz-
/// Schicht, WENN eine Queue sich bewegt hat. Shared Interrupts: War es
/// nicht unser Gerät (ISR == 0), passiert nichts (der Handler macht
/// trotzdem EOI). Interrupt-tauglich: nur Atomics + ein Port-Read.
pub fn irq_pruefen_und_wecken() {
    let io_basis = IO_BASIS.load(Ordering::Acquire);
    if io_basis == 0 {
        return; // Gerät (noch) nicht bereit
    }
    // unsafe (Port-I/O): ISR-Register lesen quittiert den Geräte-IRQ.
    let isr = unsafe { Port::<u8>::new(io_basis + REG_ISR).read() };
    if isr & ISR_QUEUE != 0 {
        // Die geräteunabhängige Netz-Schicht wecken (nur Atomics/Waker).
        netz::geraet::rx_signal();
    }
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

/// Kleine Port-Helfer für die Init-Sequenz (io_basis ist lokal bekannt).
fn status_setzen(io_basis: u16, bits: u8) {
    // unsafe (Port-I/O): Status-Bits sind additiv (virtio-Spec).
    unsafe {
        let alt = Port::<u8>::new(io_basis + REG_STATUS).read();
        Port::<u8>::new(io_basis + REG_STATUS).write(alt | bits);
    }
}

/// Erkennt eine virtio-net-NIC am PCI-Bus, richtet RX+TX und den IRQ ein
/// und REGISTRIERT das Gerät in der Netz-Schicht. Läuft beim Boot NACH
/// pci::init(). Kein Gerät / modern-only -> stille Rückkehr.
pub fn init() {
    let geraet = match pci::finde(VIRTIO_VENDOR, &VIRTIO_NET_IDS) {
        Some(g) => g,
        None => {
            serial_println!("[virtio-net] Kein virtio-net-Geraet am PCI-Bus.");
            return;
        }
    };
    let io_basis = match geraet.bars[0] {
        pci::Bar::Port(basis) => basis,
        _ => {
            serial_println!(
                "[virtio-net] {:04x}:{:04x} ohne I/O-BAR (modern-only?) — uebersprungen.",
                geraet.vendor_id,
                geraet.device_id
            );
            return;
        }
    };
    let irq = geraet.interrupt_line();

    // PCI scharfschalten: I/O-Space (Bit 0) + Bus-Master/DMA (Bit 2).
    geraet.command_setzen(0b101);

    // --- virtio-Init-Sequenz (Legacy), wie bei blk ---
    // 1. Reset, 2. ACKNOWLEDGE, 3. DRIVER:
    status_setzen(io_basis, 0);
    status_setzen(io_basis, STATUS_ACKNOWLEDGE);
    status_setzen(io_basis, STATUS_DRIVER);

    // 4. Feature-Negotiation: nur MAC akzeptieren (falls angeboten);
    //    MRG_RXBUF bewusst NICHT -> 10-Byte-Header, ein Paket pro Puffer.
    // unsafe (Port-I/O): Device-/Driver-Features-Register.
    let device_features = unsafe { Port::<u32>::new(io_basis + REG_DEVICE_FEATURES).read() };
    let driver_features = device_features & VIRTIO_NET_F_MAC;
    unsafe { Port::<u32>::new(io_basis + REG_DRIVER_FEATURES).write(driver_features) };
    let hdr_len = hdr_laenge(driver_features);

    // MAC aus der Device-Config lesen (6 Byte ab REG_CONFIG):
    let mut mac = [0u8; 6];
    // unsafe (Port-I/O): virtio-net-Config beginnt bei Offset 0x14.
    for (i, b) in mac.iter_mut().enumerate() {
        *b = unsafe { Port::<u8>::new(io_basis + REG_CONFIG + i as u16).read() };
    }

    // 5a. RX-Queue (0) aufsetzen:
    let rx_size = queue_groesse(io_basis, 0);
    if rx_size == 0 {
        serial_println!("[virtio-net] RX-Queue-Groesse 0 — abgebrochen.");
        status_setzen(io_basis, STATUS_FAILED);
        return;
    }
    let mut rx = RxRing::neu(rx_size, RX_ANZAHL, PUFFER_BYTES);
    queue_adresse_setzen(io_basis, 0, rx.vq.phys_basis());
    rx.alle_einstellen();

    // 5b. TX-Queue (1) aufsetzen — Interrupts aus (wir senden/pollen
    //     selbst; kein Interrupt-Sturm auf der geteilten PCI-Leitung).
    let tx_size = queue_groesse(io_basis, 1);
    let tx = Virtqueue::neu(tx_size.max(2));
    tx.interrupts_aus();
    queue_adresse_setzen(io_basis, 1, tx.phys_basis());
    // Ein TX-DMA-Puffer (eine Page) für [virtio_net_hdr | Frame].
    let tx_puffer_virt = memory::allocate_pages(1).expect("virtio-net TX-DMA");
    let tx_puffer_phys = memory::uebersetzen(tx_puffer_virt).expect("virtio-net TX-Physik");

    let net = VirtioNet {
        io_basis,
        rx,
        tx,
        tx_puffer_virt,
        tx_puffer_phys,
        hdr_len,
        mac,
    };

    // 6. DRIVER_OK, dann das Gerät der Netz-Schicht übergeben und den IRQ
    //    scharf schalten. Reihenfolge: GERÄT registrieren BEVOR die IRQ
    //    feuert (der netz_task braucht es), IO_BASIS BEVOR der Handler das
    //    ISR liest.
    status_setzen(io_basis, STATUS_DRIVER_OK);
    net.notify_rx(); // dem Gerät sagen: RX-Puffer stehen bereit
    let mac_text = netz::ethernet::mac_text(&mac);
    netz::geraet::geraet_registrieren(Box::new(net));
    IO_BASIS.store(io_basis, Ordering::Release);
    crate::interrupts::irq_freischalten(irq);

    serial_println!(
        "[virtio-net] Bereit: MAC {}, RX-Queue {} ({} Puffer), TX-Queue {}, Header {} Byte, IRQ {} (I/O 0x{:04x}).",
        mac_text,
        rx_size,
        RX_ANZAHL,
        tx_size,
        hdr_len,
        irq,
        io_basis
    );
}

/// Wählt Queue `index` und liest ihre Größe (0 = gibt es nicht).
fn queue_groesse(io_basis: u16, index: u16) -> u16 {
    // unsafe (Port-I/O): Queue-Select + Queue-Size-Register.
    unsafe {
        Port::<u16>::new(io_basis + REG_QUEUE_SELECT).write(index);
        Port::<u16>::new(io_basis + REG_QUEUE_SIZE).read()
    }
}

/// Trägt die physische Basis (als Page-Nummer) einer Queue ins Gerät ein.
fn queue_adresse_setzen(io_basis: u16, index: u16, phys: PhysAddr) {
    let pfn = (phys.as_u64() >> 12) as u32;
    // unsafe (Port-I/O): Queue muss zuvor selektiert sein.
    unsafe {
        Port::<u16>::new(io_basis + REG_QUEUE_SELECT).write(index);
        Port::<u32>::new(io_basis + REG_QUEUE_ADDRESS).write(pfn);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die reine Header-Längen-Logik (10 ohne, 12 mit MRG_RXBUF).
    #[test_case]
    fn test_virtio_net_hdr_laenge() {
        assert_eq!(hdr_laenge(0), 10);
        assert_eq!(hdr_laenge(VIRTIO_NET_F_MAC), 10); // MAC ändert nichts
        assert_eq!(hdr_laenge(VIRTIO_NET_F_MRG_RXBUF), 12);
        assert_eq!(hdr_laenge(VIRTIO_NET_F_MAC | VIRTIO_NET_F_MRG_RXBUF), 12);
    }

    /// RX-Puffer-Ringführung OHNE echtes Gerät: Puffer einstellen, das
    /// "Gerät" von Hand ein Frame erledigen lassen (test_geraet_erledigt),
    /// abholen (richtiger Puffer-Index!), wieder einstellen — die
    /// Kopf→Index-Zuordnung und die Freiliste bleiben konsistent.
    #[test_case]
    fn test_virtio_net_rx_ringfuehrung() {
        let mut rx = RxRing::neu(16, 4, PUFFER_BYTES);
        rx.alle_einstellen(); // Puffer 0..3 -> Deskriptor-Köpfe 0..3

        // Das "Gerät" erledigt den Puffer an Kopf 2 (Länge 60):
        rx.vq.test_geraet_erledigt(2, 60);
        let (index, laenge) = rx.abholen().expect("ein Frame sollte da sein");
        assert_eq!(index, 2, "Kopf 2 -> Puffer-Index 2 (frische Queue)");
        assert_eq!(laenge, 60);
        assert_eq!(rx.kopf_zu_index[2], -1, "abgeholter Kopf ist wieder frei");
        assert!(rx.abholen().is_none(), "nur eines war erledigt");

        // Wieder einstellen: kette_freigeben setzte frei_kopf=2 -> Kopf 2:
        assert!(rx.einstellen(index), "wieder einstellen muss klappen");
        assert_eq!(rx.kopf_zu_index[2], 2);

        // Nächste Runde an einem anderen Kopf:
        rx.vq.test_geraet_erledigt(0, 42);
        let (i2, l2) = rx.abholen().unwrap();
        assert_eq!((i2, l2), (0, 42));
    }

    /// IRQ-STURM-ROBUSTHEIT: Kommen VIELE Pakete in kurzer Zeit (das Gerät
    /// erledigt alle Puffer auf einmal), müssen wir JEDEN Puffer abholen und
    /// wieder einstellen — über viele Runden, OHNE dass ein Puffer verloren
    /// geht oder die Zuordnung durcheinandergerät. (Ein voller Ring bedeutet,
    /// dass die NIC weitere Pakete verwirft — das ist erlaubt, TCP sendet
    /// erneut; verboten ist nur, einen unserer DMA-Puffer zu VERLIEREN.)
    #[test_case]
    fn test_virtio_net_rx_sturm() {
        // Ring mit 16 Slots, alle 16 Puffer belegt = der Ring ist voll.
        let mut rx = RxRing::neu(16, 16, PUFFER_BYTES);
        rx.alle_einstellen();

        for runde in 0..8 {
            // Das Gerät erledigt in EINEM Schwung ALLE 16 Deskriptoren
            // (alle Köpfe 0..15 tragen einen unserer Puffer, da 16 Puffer in
            // 16 Slots -> jeder Deskriptor ist belegt).
            for kopf in 0..16u16 {
                rx.vq.test_geraet_erledigt(kopf, 100);
            }
            // Alle abholen + sofort wieder einstellen.
            let mut gesehen = alloc::vec::Vec::new();
            while let Some((index, laenge)) = rx.abholen() {
                assert_eq!(laenge, 100);
                assert!(index < 16, "Puffer-Index im Bereich");
                assert!(rx.einstellen(index), "wieder einstellen muss klappen");
                gesehen.push(index);
            }
            // GENAU 16 Puffer, jeder GENAU einmal — nichts verloren, nichts doppelt.
            assert_eq!(gesehen.len(), 16, "Runde {}: alle 16 Puffer abgeholt", runde);
            gesehen.sort_unstable();
            gesehen.dedup();
            assert_eq!(gesehen.len(), 16, "Runde {}: kein Puffer doppelt/verloren", runde);
        }

        // Nach dem Sturm ist der Ring wieder voll (alle 16 eingestellt) — der
        // nächste einzelne Frame kommt sauber durch.
        rx.vq.test_geraet_erledigt(0, 55);
        let (_index, laenge) = rx.abholen().expect("nach dem Sturm weiter empfangsbereit");
        assert_eq!(laenge, 55);
    }

    /// Die virtio-net-NIC wird am PCI-Bus gefunden (der Runner hängt sie
    /// auch im Test-VM an) — mit I/O-BAR und plausibler IRQ.
    #[test_case]
    fn test_virtio_net_pci_gefunden() {
        crate::pci::init(); // Geräte-Liste füllen (idempotent)
        let g = crate::pci::finde(VIRTIO_VENDOR, &VIRTIO_NET_IDS)
            .expect("virtio-net-NIC nicht gefunden (haengt der Runner sie an?)");
        assert!(
            matches!(g.bars[0], crate::pci::Bar::Port(_)),
            "erwartete eine I/O-BAR (Legacy-Transport)"
        );
        let irq = g.interrupt_line();
        assert!((1..=15).contains(&irq), "unplausible IRQ {}", irq);
    }
}
