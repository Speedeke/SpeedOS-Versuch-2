// pci.rs — PCI-Bus-Enumeration von SpeedOS
//
// PCI (Peripheral Component Interconnect) ist der Bus, an dem moderne
// Geräte hängen: Netzwerk, Massenspeicher (AHCI, NVMe, virtio), USB-
// Controller. Jedes Gerät hat einen 256-Byte "Config-Space" mit
// Hersteller-/Geräte-ID, Geräteklasse und den BARs (Base Address
// Registers — wo die Register/Puffer des Geräts im Speicher- oder
// I/O-Adressraum liegen).
//
// ZUGRIFF über die zwei Legacy-Ports (das älteste, einfachste
// Verfahren — kein MMIO-ECAM nötig, funktioniert überall):
//   * Port 0xCF8 (ADRESSE): 32-Bit-Adresse = Enable-Bit | Bus |
//     Gerät | Funktion | Register-Offset.
//   * Port 0xCFC (DATEN): liest/schreibt das adressierte 32-Bit-Wort.
//
// Wir ENUMERIEREN stumpf alle Bus/Gerät/Funktion-Tripel (Bus 0..=255,
// Gerät 0..32, Funktion 0..8) und sammeln, was antwortet (Vendor-ID
// != 0xFFFF). Das reicht für QEMU; PCI-Bridges/Bus-Rekursion sind
// bewusst weggelassen (QEMU legt unsere Geräte auf Bus 0).
//
// Dieses Modul ist ABSICHTLICH allgemein: Der virtio-blk-Treiber
// (und später virtio-net, AHCI, ...) findet sein Gerät hierüber statt
// mit fest verdrahteten Ports.

use crate::serial_println;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

const CONFIG_ADRESSE: u16 = 0xCF8;
const CONFIG_DATEN: u16 = 0xCFC;

/// Baut die 32-Bit-Adresse für Port 0xCF8: Enable-Bit (31), Bus
/// (23-16), Gerät (15-11), Funktion (10-8), Register-Offset (7-2;
/// die unteren 2 Bits sind immer 0 — es wird wortweise adressiert).
/// Reine Funktion, unten unit-getestet.
fn config_adresse(bus: u8, geraet: u8, funktion: u8, offset: u8) -> u32 {
    debug_assert!(geraet < 32 && funktion < 8);
    (1 << 31)
        | ((bus as u32) << 16)
        | ((geraet as u32) << 11)
        | ((funktion as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Liest ein 32-Bit-Wort aus dem Config-Space.
fn lese_config(bus: u8, geraet: u8, funktion: u8, offset: u8) -> u32 {
    let adresse = config_adresse(bus, geraet, funktion, offset);
    // unsafe (Port-I/O): Die beiden PCI-Config-Ports sind Standard und
    // haben keine Nebenwirkungen außer dem adressierten Zugriff.
    unsafe {
        Port::<u32>::new(CONFIG_ADRESSE).write(adresse);
        Port::<u32>::new(CONFIG_DATEN).read()
    }
}

/// Schreibt ein 32-Bit-Wort in den Config-Space (für die BAR-Größen-
/// Ermittlung und das Scharfschalten des Command-Registers).
fn schreibe_config(bus: u8, geraet: u8, funktion: u8, offset: u8, wert: u32) {
    let adresse = config_adresse(bus, geraet, funktion, offset);
    // unsafe (Port-I/O): siehe lese_config.
    unsafe {
        Port::<u32>::new(CONFIG_ADRESSE).write(adresse);
        Port::<u32>::new(CONFIG_DATEN).write(wert);
    }
}

/// Ein Base Address Register: entweder ein I/O-Port-Bereich (Legacy-
/// Geräte, virtio-blk-legacy) oder ein Speicher-Bereich (MMIO).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// I/O-Port-Basis (die unteren 2 Bits des BAR waren 0b01).
    Port(u16),
    /// Physische MMIO-Basis + ob 64-Bit-BAR (belegt dann 2 Slots).
    Speicher { basis: u64, bit64: bool },
    /// Der BAR ist unbenutzt (0).
    Leer,
}

/// Ein erkanntes PCI-Gerät (eine Bus/Gerät/Funktion-Adresse).
#[derive(Debug, Clone)]
pub struct PciGeraet {
    pub bus: u8,
    pub geraet: u8,
    pub funktion: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    /// Klassen-Code (Basisklasse), z. B. 0x01 = Massenspeicher.
    pub klasse: u8,
    /// Unterklasse, z. B. 0x00 = SCSI, 0x01 = IDE.
    pub unterklasse: u8,
    /// Die sechs BARs (dekodiert).
    pub bars: [Bar; 6],
}

impl PciGeraet {
    /// Setzt Bits im Command-Register (0x04): Bus-Master (Bit 2, für
    /// DMA), I/O-Space (Bit 0), Memory-Space (Bit 1). virtio braucht
    /// mindestens Bus-Master + I/O-Space (Legacy) scharf.
    pub fn command_setzen(&self, bits: u16) {
        let alt = lese_config(self.bus, self.geraet, self.funktion, 0x04);
        let neu = (alt & 0xFFFF_0000) | (alt as u16 | bits) as u32;
        schreibe_config(self.bus, self.geraet, self.funktion, 0x04, neu);
    }

    /// Der Interrupt-Line-Wert (Config-Offset 0x3C, unteres Byte) —
    /// die IRQ-Nummer, die die Firmware dem Gerät zugewiesen hat.
    pub fn interrupt_line(&self) -> u8 {
        lese_config(self.bus, self.geraet, self.funktion, 0x3C) as u8
    }

    /// Menschlicher Name der Geräteklasse (für den pci-Befehl).
    pub fn klasse_text(&self) -> &'static str {
        klasse_text(self.klasse, self.unterklasse)
    }
}

/// Dekodiert einen BAR-Rohwert (und den Folge-BAR für 64-Bit).
/// Reine Funktion — unit-getestet. `naechster` ist nur bei 64-Bit-
/// Memory-BARs relevant (dann das obere Adress-Wort).
fn bar_dekodieren(roh: u32, naechster: u32) -> Bar {
    if roh == 0 {
        return Bar::Leer;
    }
    if roh & 1 == 1 {
        // I/O-Space-BAR: Bit 0 = 1, Adresse in Bits 31..2.
        Bar::Port((roh & 0xFFFF_FFFC) as u16)
    } else {
        // Memory-BAR: Bits 2-1 = Typ (0b10 = 64-Bit), Adresse in 31..4.
        let bit64 = (roh >> 1) & 0b11 == 0b10;
        let basis_low = (roh & 0xFFFF_FFF0) as u64;
        let basis = if bit64 {
            basis_low | ((naechster as u64) << 32)
        } else {
            basis_low
        };
        Bar::Speicher { basis, bit64 }
    }
}

/// Liest und dekodiert alle sechs BARs eines Geräts.
fn bars_lesen(bus: u8, geraet: u8, funktion: u8) -> [Bar; 6] {
    let mut bars = [Bar::Leer; 6];
    let mut i = 0;
    while i < 6 {
        let offset = 0x10 + (i as u8) * 4;
        let roh = lese_config(bus, geraet, funktion, offset);
        let naechster = if i < 5 {
            lese_config(bus, geraet, funktion, offset + 4)
        } else {
            0
        };
        let bar = bar_dekodieren(roh, naechster);
        bars[i] = bar;
        // Ein 64-Bit-Memory-BAR belegt ZWEI Slots — den nächsten
        // überspringen:
        if matches!(bar, Bar::Speicher { bit64: true, .. }) {
            i += 2;
        } else {
            i += 1;
        }
    }
    bars
}

/// Menschlicher Klassen-Name (die wichtigsten für uns).
fn klasse_text(klasse: u8, unterklasse: u8) -> &'static str {
    match (klasse, unterklasse) {
        (0x01, 0x01) => "Massenspeicher (IDE)",
        (0x01, 0x06) => "Massenspeicher (SATA/AHCI)",
        (0x01, 0x08) => "Massenspeicher (NVMe)",
        (0x01, _) => "Massenspeicher",
        (0x02, _) => "Netzwerk",
        (0x03, _) => "Grafik",
        (0x06, 0x00) => "Host-Bridge",
        (0x06, 0x01) => "ISA-Bridge",
        (0x06, _) => "Bridge",
        (0x0C, 0x03) => "USB-Controller",
        (0x0C, _) => "Serieller Bus",
        _ => "sonstiges",
    }
}

/// Die beim Boot erfassten PCI-Geräte (Blatt-Lock).
static GERAETE: Mutex<Vec<PciGeraet>> = Mutex::new(Vec::new());

/// Enumeriert den PCI-Bus und füllt die Geräte-Liste. Idempotent —
/// darf mehrfach laufen (Tests). Läuft beim Boot vor den PCI-Treibern
/// (virtio-blk fragt danach die Liste ab).
pub fn init() {
    let mut geraete = Vec::new();
    for bus in 0u8..=255 {
        for geraet in 0u8..32 {
            // Funktion 0 zuerst: Vendor 0xFFFF = kein Gerät im Slot.
            let vendor = lese_config(bus, geraet, 0, 0x00) as u16;
            if vendor == 0xFFFF {
                continue;
            }
            // Multifunktions-Bit (Header-Typ, Offset 0x0E, Bit 7):
            let header_typ = (lese_config(bus, geraet, 0, 0x0C) >> 16) as u8;
            let funktionen = if header_typ & 0x80 != 0 { 8 } else { 1 };
            for funktion in 0..funktionen {
                if let Some(g) = geraet_lesen(bus, geraet, funktion) {
                    geraete.push(g);
                }
            }
        }
    }
    serial_println!("[PCI] {} Geraet(e) gefunden.", geraete.len());
    for g in &geraete {
        serial_println!(
            "[PCI]   {:02x}:{:02x}.{} {:04x}:{:04x} {}",
            g.bus,
            g.geraet,
            g.funktion,
            g.vendor_id,
            g.device_id,
            g.klasse_text()
        );
    }
    *GERAETE.lock() = geraete;
}

/// Liest ein einzelnes Gerät ein (None = kein Gerät an der Adresse).
fn geraet_lesen(bus: u8, geraet: u8, funktion: u8) -> Option<PciGeraet> {
    let id = lese_config(bus, geraet, funktion, 0x00);
    let vendor_id = id as u16;
    if vendor_id == 0xFFFF {
        return None;
    }
    let device_id = (id >> 16) as u16;
    let klasse_wort = lese_config(bus, geraet, funktion, 0x08);
    let klasse = (klasse_wort >> 24) as u8;
    let unterklasse = (klasse_wort >> 16) as u8;
    Some(PciGeraet {
        bus,
        geraet,
        funktion,
        vendor_id,
        device_id,
        klasse,
        unterklasse,
        bars: bars_lesen(bus, geraet, funktion),
    })
}

/// Führt `f` mit allen erfassten Geräten aus (für den pci-Befehl und
/// die Treiber-Suche).
pub fn mit_geraeten<R>(f: impl FnOnce(&[PciGeraet]) -> R) -> R {
    f(&GERAETE.lock())
}

/// Findet das erste Gerät einer KLASSE (Basisklasse/Unterklasse/Prog-IF).
///
/// ===================================================================
/// WARUM NICHT NACH VENDOR/DEVICE
///
/// `finde` sucht nach Hersteller und Modell — richtig für virtio, wo
/// es genau ein bekanntes Gerät gibt. Für einen xHCI-Controller wäre
/// es falsch: Es gibt Dutzende Hersteller (Intel, AMD, Renesas, VIA,
/// ASMedia …), und eine Liste davon wäre ab dem nächsten Notebook
/// unvollständig.
///
/// Genau dafür gibt es die Klassenkennung: `0x0C` (Serial Bus) /
/// `0x03` (USB) / Prog-IF `0x30` (xHCI) bezeichnet JEDEN xHCI-
/// Controller, unabhängig vom Hersteller. Das ist der Sinn der
/// Kennung, und sie zu benutzen ist der Unterschied zwischen einem
/// Treiber für unsere Testmaschine und einem Treiber für PCs.
pub fn finde_klasse(klasse: u8, unterklasse: u8, prog_if: u8) -> Option<PciGeraet> {
    GERAETE
        .lock()
        .iter()
        .find(|g| {
            g.klasse == klasse
                && g.unterklasse == unterklasse
                && lese_prog_if(g.bus, g.geraet, g.funktion) == prog_if
        })
        .cloned()
}

/// Das Prog-IF-Byte (Config-Offset 0x08, Bits 8..15).
///
/// Es wird nicht in `PciGeraet` mitgeführt, weil es nur für diese eine
/// Unterscheidung gebraucht wird — bei USB trennt es UHCI (0x00),
/// OHCI (0x10), EHCI (0x20) und xHCI (0x30) voneinander.
pub fn lese_prog_if(bus: u8, geraet: u8, funktion: u8) -> u8 {
    (lese_config(bus, geraet, funktion, 0x08) >> 8) as u8
}

/// Findet das erste Gerät mit passendem Vendor/Device (für Treiber).
/// Liefert eine KOPIE (die Adresse reicht für alle Config-Zugriffe).
pub fn finde(vendor_id: u16, device_ids: &[u16]) -> Option<PciGeraet> {
    GERAETE
        .lock()
        .iter()
        .find(|g| g.vendor_id == vendor_id && device_ids.contains(&g.device_id))
        .cloned()
}

// ---------------------------------------------------------------------------
// Tests — die reinen Dekoder-Funktionen (kein echtes PCI nötig)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_pci_config_adresse() {
        // Enable-Bit gesetzt, Offset auf 4 ausgerichtet:
        assert_eq!(config_adresse(0, 0, 0, 0), 0x8000_0000);
        // Bus 0, Gerät 3, Funktion 0, Offset 0x10:
        assert_eq!(config_adresse(0, 3, 0, 0x10), 0x8000_0000 | (3 << 11) | 0x10);
        // Offset wird auf Wortgrenze geklemmt (untere 2 Bits weg):
        assert_eq!(
            config_adresse(1, 2, 1, 0x13),
            0x8000_0000 | (1 << 16) | (2 << 11) | (1 << 8) | 0x10
        );
    }

    #[test_case]
    fn test_pci_bar_dekodieren() {
        assert_eq!(bar_dekodieren(0, 0), Bar::Leer);
        // I/O-BAR: Bit 0 = 1, Portbasis in den oberen Bits.
        assert_eq!(bar_dekodieren(0xC001, 0), Bar::Port(0xC000));
        // 32-Bit-Memory-BAR:
        assert_eq!(
            bar_dekodieren(0xFEBD_0000, 0),
            Bar::Speicher { basis: 0xFEBD_0000, bit64: false }
        );
        // 64-Bit-Memory-BAR (Typ-Bits 0b10): oberes Wort aus naechster.
        assert_eq!(
            bar_dekodieren(0xFEBD_0004, 0x1),
            Bar::Speicher { basis: 0x1_FEBD_0000, bit64: true }
        );
    }

    #[test_case]
    fn test_pci_klasse_text() {
        assert_eq!(klasse_text(0x01, 0x01), "Massenspeicher (IDE)");
        assert_eq!(klasse_text(0x02, 0x00), "Netzwerk");
        assert_eq!(klasse_text(0x99, 0x99), "sonstiges");
    }
}
