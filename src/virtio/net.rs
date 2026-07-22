// virtio/net.rs — virtio-net (Serie 5, Schritt 1): interrupt-getriebener
//                 EMPFANG von Ethernet-Frames — nur RX + Hexdump.
//
// Dies ist der erste ASYNCHRONE Hardware-Event jenseits von Tastatur,
// Maus und Timer: Netzwerk-Pakete kommen UNAUFGEFORDERT, deshalb kann
// man sie nicht wie die Platte pollen — sie müssen INTERRUPTS auslösen.
//
// BEWUSST KLEIN: Es gibt hier KEINEN Netzwerk-Stack. Wir finden das
// Gerät, richten den IRQ-Pfad ein, stellen RX-Puffer bereit und geben
// ankommende Frames roh (hexdump) aus. ARP/IP/UDP/TCP sind der Fahrplan
// aus docs/serie5-netzwerk.md — hier noch nicht.
//
// Wiederverwendung: Transport (PCI-Legacy-Port-I/O) und Virtqueue
// (virtio/virtqueue.rs) sind IDENTISCH zu virtio-blk; neu sind nur:
//   * MEHRERE Queues (RX = 0, TX = 1) statt einer,
//   * INTERRUPTS statt Polling (RX-Pakete kommen von selbst),
//   * RX-Puffer, die wir VORAB einstellen (das Gerät DMA-t hinein) und
//     nach dem Verbrauch wieder einstellen (RxRing).
//
// ARP-POKE: QEMUs user-mode-Netz (slirp) ist REAKTIV — es sendet von
// sich aus nichts. Damit "nur empfangen" überhaupt etwas empfängt,
// schickt `netz-lausch` beim Einschalten EIN statisches Broadcast-ARP-
// Frame (who-has Gateway); slirp antwortet garantiert. Das ist KEIN
// Stack — ein hand-gebautes 42-Byte-Frame.

use crate::virtio::virtqueue::Virtqueue;
use crate::{memory, pci, println, serial_println};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use core::task::Poll;
use futures_util::task::AtomicWaker;
use spin::Mutex;
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

// ---------------------------------------------------------------------------
// Globaler Zustand
// ---------------------------------------------------------------------------

/// I/O-Basis des Geräts — LOCK-FREI, damit der IRQ-Handler das
/// ISR-Register lesen kann, ohne den NET-Mutex zu nehmen. 0 = kein Gerät.
static IO_BASIS: AtomicU16 = AtomicU16::new(0);
/// Der RX-Task schläft hier, der IRQ-Handler weckt.
static RX_WAKER: AtomicWaker = AtomicWaker::new();
/// Vom IRQ gesetzt: "es gibt empfangene Frames abzuholen".
static RX_BEREIT: AtomicBool = AtomicBool::new(false);
/// Schaltet den Hexdump an/aus (Shell-Befehl netz-lausch).
static LAUSCH_AKTIV: AtomicBool = AtomicBool::new(false);

/// Die einzige virtio-net-Instanz (Blatt-Lock wie virtio-blk — nur aus
/// Task-Kontext, NIE aus dem Interrupt-Handler).
static NET: Mutex<Option<VirtioNet>> = Mutex::new(None);

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
// VirtioNet — der Treiber-Zustand
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

// SICHERHEIT: enthält rohe DMA-Zeiger, ist aber nur über den Mutex
// erreichbar und wird nie zwischen Threads geteilt — Send ist erfüllt.
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

    /// Sendet EIN statisches Broadcast-ARP-Frame (who-has 10.0.2.2), um
    /// slirp zu einer Antwort zu provozieren — der einzige TX-Einsatz.
    fn poke_senden(&mut self) {
        // Vorher die alten TX-Deskriptoren zurückgeben (wir pollen die
        // winzige TX-Queue kurz, sie ist auf interrupts_aus gestellt):
        while self.tx.used_abholen().is_some() {}

        let mut frame = [0u8; 42];
        // --- Ethernet-Header ---
        frame[0..6].copy_from_slice(&[0xff; 6]); // Ziel: Broadcast
        frame[6..12].copy_from_slice(&self.mac); // Quelle: unsere MAC
        frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes()); // EtherType ARP
        // --- ARP (IPv4 über Ethernet) ---
        frame[14..16].copy_from_slice(&1u16.to_be_bytes()); // htype = Ethernet
        frame[16..18].copy_from_slice(&0x0800u16.to_be_bytes()); // ptype = IPv4
        frame[18] = 6; // hlen
        frame[19] = 4; // plen
        frame[20..22].copy_from_slice(&1u16.to_be_bytes()); // op = request
        frame[22..28].copy_from_slice(&self.mac); // sha = unsere MAC
        frame[28..32].copy_from_slice(&[10, 0, 2, 15]); // spa = QEMU-Gast-IP
        // tha (32..38) bleibt 0
        frame[38..42].copy_from_slice(&[10, 0, 2, 2]); // tpa = Gateway

        // In den TX-DMA-Puffer: [virtio_net_hdr genullt | Frame]:
        // unsafe: tx_puffer_virt ist unser DMA-Puffer (>= hdr_len + 42).
        unsafe {
            let p = self.tx_puffer_virt.as_mut_ptr::<u8>();
            core::ptr::write_bytes(p, 0, self.hdr_len);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), p.add(self.hdr_len), frame.len());
        }
        let gesamt = (self.hdr_len + frame.len()) as u32;
        // Das GERÄT LIEST diesen Puffer (schreibt nicht) -> false.
        if let Some(kopf) = self.tx.kette_anlegen(&[(self.tx_puffer_phys, gesamt, false)]) {
            self.tx.verfuegbar_machen(kopf);
            self.notify_tx();
        }
    }
}

// ---------------------------------------------------------------------------
// Interrupt-Pfad (aus interrupts.rs gerufen — KEIN Lock, KEINE Allokation)
// ---------------------------------------------------------------------------

/// Wird vom PCI-IRQ-Handler gerufen. Liest das ISR-Register des Geräts
/// (das QUITTIERT den Interrupt an der Hardware) und weckt den RX-Task,
/// WENN eine Queue sich bewegt hat. Shared Interrupts: War es nicht
/// unser Gerät (ISR == 0), passiert nichts (der Handler macht trotzdem
/// EOI). Interrupt-tauglich: nur Atomics + ein Port-Read.
pub fn irq_pruefen_und_wecken() {
    let io_basis = IO_BASIS.load(Ordering::Acquire);
    if io_basis == 0 {
        return; // Gerät (noch) nicht bereit
    }
    // unsafe (Port-I/O): ISR-Register lesen quittiert den Geräte-IRQ.
    let isr = unsafe { Port::<u8>::new(io_basis + REG_ISR).read() };
    if isr & ISR_QUEUE != 0 {
        RX_BEREIT.store(true, Ordering::Release);
        RX_WAKER.wake();
    }
}

// ---------------------------------------------------------------------------
// Der RX-Task (async) und der Hexdump
// ---------------------------------------------------------------------------

/// Wartet, bis der IRQ RX_BEREIT setzt (race-frei per Doppel-Check wie
/// beim Scancode-Stream).
async fn rx_warten() {
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

/// Der RX-Task: vom IRQ geweckt, empfangene Frames einsammeln und (wenn
/// netz-lausch an ist) hexdumpen; Puffer immer wieder einstellen.
pub async fn rx_task() {
    loop {
        rx_warten().await;
        rx_verarbeiten();
    }
}

/// Holt alle bereitliegenden Frames ab, stellt die Puffer wieder ein und
/// gibt die zu dumpenden Frames zurück NACH dem Loslassen des Locks
/// (println unter dem NET-Lock vermeiden). Kopiert nur, wenn gelauscht
/// wird.
fn rx_verarbeiten() {
    let lauschen = LAUSCH_AKTIV.load(Ordering::Relaxed);
    let mut zum_dumpen: Vec<Vec<u8>> = Vec::new();
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut lock = NET.lock();
        let net = match lock.as_mut() {
            Some(n) => n,
            None => return,
        };
        let hdr = net.hdr_len;
        let mut etwas = false;
        while let Some((index, laenge)) = net.rx.abholen() {
            let laenge = laenge as usize;
            if lauschen && laenge > hdr {
                // unsafe: der Puffer index gehört uns, laenge <= PUFFER_BYTES.
                let frame = unsafe {
                    core::slice::from_raw_parts(
                        net.rx.puffer_virt[index].as_ptr::<u8>().add(hdr),
                        laenge - hdr,
                    )
                };
                zum_dumpen.push(frame.to_vec());
            }
            net.rx.einstellen(index);
            etwas = true;
        }
        if etwas {
            net.notify_rx();
        }
    });
    for frame in &zum_dumpen {
        frame_hexdump(frame);
    }
}

/// Formatiert eine MAC-Adresse als xx:xx:xx:xx:xx:xx.
fn mac_text(m: &[u8]) -> String {
    alloc::format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

/// Gibt ein rohes Ethernet-Frame lesbar aus: Ziel-/Quell-MAC, EtherType
/// (annotiert) und einen Hexdump der ersten Bytes.
fn frame_hexdump(frame: &[u8]) {
    if frame.len() < 14 {
        println!("[netz] Frame zu kurz ({} Byte)", frame.len());
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let typ_name = match ethertype {
        0x0806 => "ARP",
        0x0800 => "IPv4",
        0x86DD => "IPv6",
        0x8100 => "VLAN",
        _ => "?",
    };
    println!(
        "[netz] Frame {} Byte | Ziel {} | Quelle {} | EtherType 0x{:04x} ({})",
        frame.len(),
        mac_text(&frame[0..6]),
        mac_text(&frame[6..12]),
        ethertype,
        typ_name
    );
    // Roh-Hexdump, gedeckelt (16 Byte je Zeile):
    let max = frame.len().min(64);
    let mut i = 0;
    while i < max {
        let ende = (i + 16).min(max);
        let mut hex = String::new();
        let mut asc = String::new();
        for &b in &frame[i..ende] {
            hex.push_str(&alloc::format!("{:02x} ", b));
            asc.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        println!("  {:04x}  {:<48}{}", i, hex, asc);
        i += 16;
    }
    if frame.len() > max {
        println!("  ... ({} weitere Byte)", frame.len() - max);
    }
}

// ---------------------------------------------------------------------------
// Öffentliche API für die Shell
// ---------------------------------------------------------------------------

/// Ist eine virtio-net-NIC aufgesetzt?
pub fn vorhanden() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| NET.lock().is_some())
}

/// Schaltet den Hexdump um. Beim EINschalten wird ein ARP-Poke gesendet
/// (slirp antwortet -> sichtbarer Verkehr). Liefert den neuen Zustand.
pub fn lausch_umschalten() -> bool {
    let neu = !LAUSCH_AKTIV.load(Ordering::Relaxed);
    LAUSCH_AKTIV.store(neu, Ordering::Relaxed);
    if neu {
        x86_64::instructions::interrupts::without_interrupts(|| {
            if let Some(net) = NET.lock().as_mut() {
                net.poke_senden();
            }
        });
    }
    neu
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

/// Erkennt eine virtio-net-NIC am PCI-Bus und richtet den RX-Empfang
/// ein (Legacy-Transport, RX+TX-Queue, IRQ). Läuft beim Boot NACH
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

    // 5b. TX-Queue (1) aufsetzen — Interrupts aus (wir pollen/senden nur
    //     den Poke), sonst ungenutzt.
    let tx_size = queue_groesse(io_basis, 1);
    let tx = Virtqueue::neu(tx_size.max(2));
    tx.interrupts_aus();
    queue_adresse_setzen(io_basis, 1, tx.phys_basis());
    // Ein kleiner TX-DMA-Puffer für den ARP-Poke (Header + Frame):
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

    // 6. DRIVER_OK, dann Zustand veröffentlichen und den IRQ scharf
    //    schalten. Reihenfolge: NET setzen BEVOR die IRQ feuert (der
    //    RX-Task braucht NET), IO_BASIS BEVOR der Handler das ISR liest.
    status_setzen(io_basis, STATUS_DRIVER_OK);
    net.notify_rx(); // dem Gerät sagen: RX-Puffer stehen bereit
    x86_64::instructions::interrupts::without_interrupts(|| {
        *NET.lock() = Some(net);
    });
    IO_BASIS.store(io_basis, Ordering::Release);
    crate::interrupts::irq_freischalten(irq);

    serial_println!(
        "[virtio-net] Bereit: MAC {}, RX-Queue {} ({} Puffer), TX-Queue {}, Header {} Byte, IRQ {} (I/O 0x{:04x}).",
        mac_text(&mac),
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
