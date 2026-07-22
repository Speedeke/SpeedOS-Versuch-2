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
// Wiederverwendung: Der Transport (PCI-Legacy-Port-I/O) und die
// Virtqueue (virtio/virtqueue.rs) sind IDENTISCH zu virtio-blk — nur
// dass das Netz mehrere Queues (RX=0, TX=1) hat und über Interrupts
// statt Polling arbeitet. Register-Offsets: siehe blk.rs.

use crate::{pci, serial_println};

// --- PCI-Kennung ---------------------------------------------------------
const VIRTIO_VENDOR: u16 = 0x1AF4;
/// Legacy/transitional virtio-net (0x1000) bzw. modern-only (0x1041).
const VIRTIO_NET_IDS: [u16; 2] = [0x1000, 0x1041];

/// Erkennt eine virtio-net-NIC am PCI-Bus und loggt Fund + Register.
/// Läuft beim Boot NACH pci::init(). Kein Gerät / modern-only -> stille
/// Rückkehr (dann gibt es eben kein Netz).
pub fn init() {
    let geraet = match pci::finde(VIRTIO_VENDOR, &VIRTIO_NET_IDS) {
        Some(g) => g,
        None => {
            serial_println!("[virtio-net] Kein virtio-net-Geraet am PCI-Bus.");
            return;
        }
    };

    // Wir brauchen die I/O-BAR (Legacy-Transport, wie bei virtio-blk).
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

    // PCI scharfschalten: I/O-Space (Bit 0) + Bus-Master/DMA (Bit 2).
    geraet.command_setzen(0b101);

    let irq = geraet.interrupt_line();
    serial_println!(
        "[virtio-net] Gefunden {:04x}:{:04x} an {:02x}:{:02x}.{} — I/O-Basis 0x{:04x}, IRQ {}.",
        geraet.vendor_id,
        geraet.device_id,
        geraet.bus,
        geraet.geraet,
        geraet.funktion,
        io_basis,
        irq
    );
}
