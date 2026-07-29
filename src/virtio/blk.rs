// virtio/blk.rs — virtio-blk-Treiber (virtuelle Block-Platte)
//
// virtio-blk ist die schnelle, para-virtualisierte Platte von QEMU.
// Statt IDE-Register zu pollen (ATA) übergeben wir Aufträge über eine
// VIRTQUEUE (virtio/virtqueue.rs) — das ist der Kern, den auch
// virtio-net später nutzt.
//
// TRANSPORT-ENTSCHEIDUNG: LEGACY (virtio 0.9.5) über die PCI-I/O-BAR,
// NICHT Modern (1.0). Begründung:
//   * QEMUs virtio-blk-pci ist "transitional" — es bietet BEIDES an;
//     Legacy funktioniert also garantiert.
//   * Legacy nutzt PORT-I/O (BAR0 ist ein I/O-Bereich) — genau die
//     Technik, die wir vom ATA-Treiber schon beherrschen; kein
//     MMIO-Mapping, kein Parsen der PCI-Capability-Liste nötig.
//   * Der WIEDERVERWENDBARE Teil (die Virtqueue) ist bei Legacy und
//     Modern IDENTISCH — nur das Finden/Anstoßen der Register
//     unterscheidet sich. Für virtio-net ließe sich später ein
//     Modern-Transport ergänzen, ohne die Virtqueue anzufassen.
//
// LEGACY-REGISTER (Offsets ab der I/O-BAR-Basis):
//   0x00 Device Features (r)   0x02 ...    0x04 Driver Features (w)
//   0x08 Queue Address (rw, = phys>>12)    0x0C Queue Size (r)
//   0x0E Queue Select (w)      0x10 Queue Notify (w)
//   0x12 Device Status (rw)    0x13 ISR Status (r)
//   0x14 Geräte-Config (virtio-blk: Kapazität als u64, in 512-B-Sektoren)
//
// DMA & BOUNCE-PUFFER: Das Gerät liest/schreibt PHYSISCHE Adressen.
// Der Puffer des Aufrufers (Kernel-Heap) ist aber nicht garantiert
// physisch zusammenhängend. Deshalb nutzen wir einen physisch
// zusammenhängenden BOUNCE-Puffer (memory::allocate_pages): beim Lesen
// schreibt das Gerät dorthin, dann kopieren wir in den Zielpuffer;
// beim Schreiben umgekehrt. Größere Aufträge zerlegt der Treiber in
// Bounce-große Häppchen. Einfach und korrekt (schnell genug — der
// Benchmark plattentest zeigt es).

use super::virtqueue::Virtqueue;
use crate::fs::block::{BlockDevice, IoFehler};
use crate::{memory, pci, serial_println, zeit};
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::{PhysAddr, VirtAddr};

// --- PCI-Kennung ---------------------------------------------------------
const VIRTIO_VENDOR: u16 = 0x1AF4;
/// Legacy/transitional virtio-blk (0x1001) bzw. modern-only (0x1042).
const VIRTIO_BLK_IDS: [u16; 2] = [0x1001, 0x1042];

// --- Legacy-Register-Offsets (ab I/O-Basis) -----------------------------
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_DRIVER_FEATURES: u16 = 0x04;
const REG_QUEUE_ADDRESS: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_CONFIG: u16 = 0x14; // Geräte-Config (ohne MSI-X)

// --- Device-Status-Bits --------------------------------------------------
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 128;

// --- virtio-blk Request-Typen -------------------------------------------
const BLK_T_IN: u32 = 0; // lesen (Gerät -> Speicher)
const BLK_T_OUT: u32 = 1; // schreiben (Speicher -> Gerät)
const BLK_T_FLUSH: u32 = 4; // Cache aufs Medium
/// Feature-Bit: Gerät kann FLUSH (bit 9).
const BLK_F_FLUSH: u32 = 1 << 9;

/// Sektorgröße von virtio-blk (immer 512, wie bei ATA).
const SEKTOR_GROESSE: usize = 512;
/// Bounce-Puffer: 64 KiB = 128 Sektoren pro Auftrag (größere Aufträge
/// zerlegt der Treiber selbst).
const BOUNCE_SEKTOREN: usize = 128;
const BOUNCE_BYTES: usize = BOUNCE_SEKTOREN * SEKTOR_GROESSE;

/// Polling-Frist wie beim ATA-Treiber — nie endlos auf Hardware warten.
const TIMEOUT_US: u64 = 2_000_000;

/// Der virtio-blk-Treiber-Zustand (eine Instanz, im globalen Lock).
struct VirtioBlk {
    io_basis: u16,
    vq: Virtqueue,
    kapazitaet_sektoren: u64,
    flush_moeglich: bool,
    // DMA-Puffer (physisch zusammenhängend):
    header_virt: VirtAddr,
    header_phys: PhysAddr,
    status_virt: VirtAddr,
    status_phys: PhysAddr,
    bounce_virt: VirtAddr,
    bounce_phys: PhysAddr,
}

// SICHERHEIT: Der VirtioBlk enthält rohe Zeiger (VirtAddr) auf DMA-
// Speicher, ist aber nur über den Mutex erreichbar und wird nie
// zwischen Threads geteilt — Send ist damit erfüllt.
unsafe impl Send for VirtioBlk {}

/// Die einzige virtio-blk-Instanz (Blatt-Lock wie ata::LAUFWERKE:
/// nur aus Task-Kontext, nie im Interrupt; Lock-Ordnung VFS -> hier).
static VIRTIO_BLK: Mutex<Option<VirtioBlk>> = Mutex::new(None);

impl VirtioBlk {
    fn status_lesen(&self) -> u8 {
        // unsafe (Port-I/O): Legacy-Status-Register des Geräts.
        unsafe { Port::<u8>::new(self.io_basis + REG_STATUS).read() }
    }
    fn status_setzen(&self, bits: u8) {
        let alt = self.status_lesen();
        // unsafe (Port-I/O): Status-Bits sind additiv (Spec).
        unsafe { Port::<u8>::new(self.io_basis + REG_STATUS).write(alt | bits) };
    }

    /// Schickt den Auftrag (Header + optional Daten + Status) über die
    /// Virtqueue und wartet gepollt mit Timeout auf die Antwort.
    /// `daten` ist bereits <= BOUNCE_BYTES groß. Bei BLK_T_IN wird nach
    /// Erfolg der Bounce-Puffer in `daten` kopiert; bei BLK_T_OUT muss
    /// der Aufrufer `daten` VORHER in den Bounce-Puffer gelegt haben.
    fn auftrag(&mut self, typ: u32, sektor: u64, daten_len: usize) -> Result<(), IoFehler> {
        // 1. Header schreiben (type u32 | reserved u32 | sector u64):
        // unsafe: header_virt zeigt auf unseren 16-Byte-DMA-Header.
        unsafe {
            let h = self.header_virt.as_mut_ptr::<u8>();
            core::ptr::write_volatile(h as *mut u32, typ);
            core::ptr::write_volatile(h.add(4) as *mut u32, 0);
            core::ptr::write_volatile(h.add(8) as *mut u64, sektor);
            // Status-Byte auf einen Wert setzen, den das Gerät überschreibt:
            core::ptr::write_volatile(self.status_virt.as_mut_ptr::<u8>(), 0xFF);
        }

        // 2. Deskriptor-Kette bauen: Header (Gerät liest), [Daten],
        //    Status (Gerät schreibt). FLUSH hat keinen Datenpuffer.
        let geraet_schreibt_daten = typ == BLK_T_IN;
        let kopf = if daten_len == 0 {
            self.vq.kette_anlegen(&[
                (self.header_phys, 16, false),
                (self.status_phys, 1, true),
            ])
        } else {
            self.vq.kette_anlegen(&[
                (self.header_phys, 16, false),
                (self.bounce_phys, daten_len as u32, geraet_schreibt_daten),
                (self.status_phys, 1, true),
            ])
        }
        .ok_or(IoFehler::Geraetefehler)?; // Queue voll -> sollte nie passieren

        // 3. Verfügbar machen + Gerät anstoßen (Queue 0 notifizieren):
        self.vq.verfuegbar_machen(kopf);
        // unsafe (Port-I/O): Notify-Register erwartet den Queue-Index.
        unsafe { Port::<u16>::new(self.io_basis + REG_QUEUE_NOTIFY).write(0) };

        // 4. Gepollt auf das Used-Element warten (mit Timeout):
        let start = zeit::us_seit_boot();
        loop {
            if let Some((fertig_kopf, _laenge)) = self.vq.used_abholen() {
                debug_assert_eq!(fertig_kopf, kopf);
                // ENTROPIE (Serie 7, Teil 1): WANN das Gerät fertig wurde.
                // Die Antwortzeit schwankt mit den Warteschlangen des Wirts —
                // bewusst niedrig bewertet, weil sie eben dort entsteht und
                // nicht bei uns (docs/zufall.md §3).
                crate::zufall::einspeisen(crate::zufall::Quelle::Platte);
                break;
            }
            if zeit::us_seit_boot() - start > TIMEOUT_US {
                return Err(IoFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }

        // 5. Status-Byte prüfen (0 = OK, sonst Fehler):
        // unsafe: status_virt zeigt auf unser 1-Byte-Status-DMA.
        let status = unsafe { core::ptr::read_volatile(self.status_virt.as_ptr::<u8>()) };
        if status != 0 {
            return Err(IoFehler::Geraetefehler);
        }
        Ok(())
    }

    /// Prüft Start/Länge und liefert die Sektorenzahl (gemeinsame
    /// Validierung für Lesen und Schreiben).
    fn pruefen(&self, start: u64, puffer_laenge: usize) -> Result<u64, IoFehler> {
        if puffer_laenge == 0 || !puffer_laenge.is_multiple_of(SEKTOR_GROESSE) {
            return Err(IoFehler::UngueltigePufferGroesse);
        }
        let sektoren = (puffer_laenge / SEKTOR_GROESSE) as u64;
        match start.checked_add(sektoren) {
            Some(ende) if ende <= self.kapazitaet_sektoren => Ok(sektoren),
            _ => Err(IoFehler::AusserhalbDesGeraets),
        }
    }
}

impl BlockDevice for VirtioBlkGeraet {
    fn sektor_groesse(&self) -> usize {
        SEKTOR_GROESSE
    }
    fn anzahl_sektoren(&self) -> u64 {
        self.sektoren
    }

    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
        mit_blk(|blk| {
            blk.pruefen(start, puffer.len())?;
            // In Bounce-große Häppchen zerlegen:
            let mut ab = 0usize;
            let mut lba = start;
            while ab < puffer.len() {
                let stueck = (puffer.len() - ab).min(BOUNCE_BYTES);
                blk.auftrag(BLK_T_IN, lba, stueck)?;
                // Gerät hat in den Bounce-Puffer geschrieben -> kopieren:
                // unsafe: bounce_virt..+stueck ist unser DMA-Puffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        blk.bounce_virt.as_ptr::<u8>(),
                        puffer[ab..].as_mut_ptr(),
                        stueck,
                    );
                }
                ab += stueck;
                lba += (stueck / SEKTOR_GROESSE) as u64;
            }
            Ok(())
        })
    }

    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
        mit_blk(|blk| {
            blk.pruefen(start, puffer.len())?;
            let mut ab = 0usize;
            let mut lba = start;
            while ab < puffer.len() {
                let stueck = (puffer.len() - ab).min(BOUNCE_BYTES);
                // Zuerst die Nutzdaten in den Bounce-Puffer legen:
                // unsafe: bounce_virt..+stueck ist unser DMA-Puffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        puffer[ab..].as_ptr(),
                        blk.bounce_virt.as_mut_ptr::<u8>(),
                        stueck,
                    );
                }
                blk.auftrag(BLK_T_OUT, lba, stueck)?;
                ab += stueck;
                lba += (stueck / SEKTOR_GROESSE) as u64;
            }
            Ok(())
        })
    }

    fn sync(&mut self) -> Result<(), IoFehler> {
        mit_blk(|blk| {
            if blk.flush_moeglich {
                blk.auftrag(BLK_T_FLUSH, 0, 0)
            } else {
                Ok(()) // Gerät ohne Schreib-Cache — nichts zu flushen
            }
        })
    }
}

/// Führt `f` mit der virtio-blk-Instanz aus (None -> NichtBereit).
fn mit_blk<R>(f: impl FnOnce(&mut VirtioBlk) -> Result<R, IoFehler>) -> Result<R, IoFehler> {
    let mut lock = VIRTIO_BLK.lock();
    match lock.as_mut() {
        Some(blk) => f(blk),
        None => Err(IoFehler::NichtBereit),
    }
}

/// Ein besitzbares BlockDevice-Handle auf die virtio-blk-Platte —
/// delegiert an die globale Instanz (Muster wie ata::DatenPlatte).
pub struct VirtioBlkGeraet {
    sektoren: u64,
}

/// Liefert das Handle, wenn eine virtio-blk-Platte aufgesetzt wurde.
pub fn daten_platte() -> Option<VirtioBlkGeraet> {
    VIRTIO_BLK
        .lock()
        .as_ref()
        .map(|blk| VirtioBlkGeraet {
            sektoren: blk.kapazitaet_sektoren,
        })
}

/// Erkennt eine virtio-blk-Platte am PCI-Bus und richtet sie ein
/// (Legacy-Transport, Feature-Negotiation, eine Virtqueue). Läuft beim
/// Boot NACH pci::init(). Kein Gerät / modern-only -> stille Rückkehr
/// (dann trägt ATA die Daten-Platte).
pub fn init() {
    let geraet = match pci::finde(VIRTIO_VENDOR, &VIRTIO_BLK_IDS) {
        Some(g) => g,
        None => {
            serial_println!("[virtio-blk] Kein virtio-blk-Geraet am PCI-Bus.");
            return;
        }
    };

    // Wir brauchen die I/O-BAR (Legacy). Ist BAR0 ein MMIO-Bereich, ist
    // das Gerät modern-only — das unterstützt unser Legacy-Treiber nicht.
    let io_basis = match geraet.bars[0] {
        pci::Bar::Port(basis) => basis,
        _ => {
            serial_println!(
                "[virtio-blk] Geraet {:04x}:{:04x} ohne I/O-BAR (modern-only?) — uebersprungen.",
                geraet.vendor_id,
                geraet.device_id
            );
            return;
        }
    };

    // PCI scharfschalten: I/O-Space (Bit 0) + Bus-Master/DMA (Bit 2).
    geraet.command_setzen(0b101);

    // --- virtio-Init-Sequenz (Legacy) ---
    let status_port = || Port::<u8>::new(io_basis + REG_STATUS);
    // 1. Reset (Status = 0), 2. ACKNOWLEDGE, 3. DRIVER.
    // unsafe (Port-I/O): alle drei schreiben nur ins Legacy-Status-
    // Register (io_basis + REG_STATUS) — reine Gerätesteuerung, kann
    // keinen Kernel-Speicher berühren.
    unsafe { status_port().write(0) };
    unsafe { status_port().write(STATUS_ACKNOWLEDGE) };
    unsafe { status_port().write(STATUS_ACKNOWLEDGE | STATUS_DRIVER) };

    // 4. Feature-Negotiation: Wir akzeptieren NUR FLUSH (falls
    //    angeboten) — mehr braucht unser einfacher Treiber nicht.
    // unsafe (Port-I/O): Device-/Driver-Features-Register.
    let device_features = unsafe { Port::<u32>::new(io_basis + REG_DEVICE_FEATURES).read() };
    let flush_moeglich = device_features & BLK_F_FLUSH != 0;
    let driver_features = device_features & BLK_F_FLUSH; // sonst nichts
    unsafe { Port::<u32>::new(io_basis + REG_DRIVER_FEATURES).write(driver_features) };

    // 5. Virtqueue 0 aufsetzen: auswählen, Größe lesen, anlegen,
    //    physische Page-Nummer ins Queue-Address-Register.
    // unsafe (Port-I/O): Queue-Select/Size-Register.
    unsafe { Port::<u16>::new(io_basis + REG_QUEUE_SELECT).write(0) };
    let queue_size = unsafe { Port::<u16>::new(io_basis + REG_QUEUE_SIZE).read() };
    if queue_size == 0 || !queue_size.is_power_of_two() {
        serial_println!("[virtio-blk] Ungueltige Queue-Groesse {} — abgebrochen.", queue_size);
        // unsafe (Port-I/O): FAILED ins Legacy-Status-Register (dito).
        unsafe { status_port().write(STATUS_FAILED) };
        return;
    }
    let vq = Virtqueue::neu(queue_size);
    // Diese Queue wird GEPOLLT (siehe auftrag) — das Gerät soll dafür
    // nie interrupten. Wichtig, seit virtio-net eine PCI-IRQ-Leitung
    // benutzt: läge blk auf derselben Leitung, gäbe es sonst einen
    // Interrupt-Sturm bei jeder Platten-Operation.
    vq.interrupts_aus();
    let pfn = (vq.phys_basis().as_u64() >> 12) as u32;
    // unsafe (Port-I/O): Queue-Address-Register erwartet die Page-Nummer.
    unsafe { Port::<u32>::new(io_basis + REG_QUEUE_ADDRESS).write(pfn) };

    // Kapazität aus der Geräte-Config lesen (u64 in 512-B-Sektoren):
    // unsafe (Port-I/O): virtio-blk-Config ab Offset 0x14.
    let kapazitaet_sektoren = unsafe {
        let low = Port::<u32>::new(io_basis + REG_CONFIG).read() as u64;
        let high = Port::<u32>::new(io_basis + REG_CONFIG + 4).read() as u64;
        low | (high << 32)
    };

    // DMA-Puffer anlegen (physisch zusammenhängend): Header+Status in
    // die erste Page, Bounce-Puffer separat.
    let meta = memory::allocate_pages(1).expect("virtio-blk DMA-Meta");
    let meta_phys = memory::uebersetzen(meta).expect("virtio-blk Meta-Physik");
    let bounce_pages = BOUNCE_BYTES.div_ceil(4096);
    let bounce = memory::allocate_pages(bounce_pages).expect("virtio-blk Bounce");
    let bounce_phys = memory::uebersetzen(bounce).expect("virtio-blk Bounce-Physik");

    let blk = VirtioBlk {
        io_basis,
        vq,
        kapazitaet_sektoren,
        flush_moeglich,
        header_virt: meta,
        header_phys: meta_phys,
        // Status-Byte 16 Byte hinter dem Header (in derselben Page):
        status_virt: meta + 16u64,
        status_phys: meta_phys + 16u64,
        bounce_virt: bounce,
        bounce_phys,
    };

    // 6. DRIVER_OK — ab jetzt ist das Gerät betriebsbereit.
    blk.status_setzen(STATUS_DRIVER_OK);

    serial_println!(
        "[virtio-blk] Bereit: {} Sektoren = {} MiB, Queue-Groesse {}, Flush {} (I/O-Basis 0x{:04x}).",
        kapazitaet_sektoren,
        kapazitaet_sektoren * SEKTOR_GROESSE as u64 / 1024 / 1024,
        queue_size,
        if flush_moeglich { "ja" } else { "nein" },
        io_basis
    );
    *VIRTIO_BLK.lock() = Some(blk);
}
