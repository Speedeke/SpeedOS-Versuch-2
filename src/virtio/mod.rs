// virtio/mod.rs — virtio-Geräte für SpeedOS
//
// "virtio" ist der Standard für PARAVIRTUALISIERTE Geräte: Statt echte
// Hardware Bit für Bit nachzuahmen (wie unser ATA-Treiber die IDE-
// Register bedient), reden Gast und Hypervisor über eine schlanke,
// für Virtualisierung entworfene Schnittstelle — das ist deutlich
// schneller. Alle virtio-Geräte teilen sich denselben Mechanismus:
//   * Sie hängen am PCI-Bus (siehe src/pci.rs).
//   * Sie kommunizieren über VIRTQUEUES (src/virtio/virtqueue.rs) —
//     denselben Ringpuffer-Mechanismus für Block, Netz, Konsole, ...
//
// Diese Trennung ist Absicht: `virtqueue` ist geräteunabhängig, `blk`
// ist der erste konkrete Treiber, und `virtio-net` (Serie 5) wird
// `virtqueue` unverändert weiterbenutzen.

pub mod blk;
pub mod virtqueue;
