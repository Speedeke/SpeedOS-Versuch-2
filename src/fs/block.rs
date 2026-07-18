// fs/block.rs — Die Blockgeräte-Naht von SpeedOS
//
// JEDER Massenspeicher (heute die RamDisk, später AHCI/NVMe/virtio)
// steckt hinter diesem schmalen Trait. Disk-Dateisysteme (FAT32, ein
// eigenes SpeedFS) reden NUR mit BlockDevice — nie mit einem
// konkreten Treiber. Die Naht entsteht BEWUSST vor dem ersten echten
// Treiber: Die RamDisk ist die Referenz-Implementierung, gegen die
// aller Dateisystem-Code entwickelt und getestet wird; ein echter
// Treiber muss danach nur dieses Trait erfüllen.
//
// Entwurfsentscheidungen:
//   * SEKTOR-Adressierung (LBA), keine Byte-Offsets — genau das
//     können echte Platten. Byte-genaues Lesen baut das Dateisystem
//     darüber (siehe read_at/write_at im VFS-Trait).
//   * Der Puffer muss ein VIELFACHES der Sektorgröße sein; jede
//     Implementierung validiert das und meldet IoFehler — niemals
//     panicken, niemals still abschneiden.
//   * sync() drückt Schreib-Caches aufs Medium. Die RamDisk hat
//     keinen Cache — Ok(()) ist dort die ehrliche Antwort.

use alloc::vec;
use alloc::vec::Vec;

/// Alle Fehler, die ein Blockgerät (oder das Dateisystem darüber)
/// melden kann. Bewusst klein und Copy — er wandert als FsFehler::Io
/// durch das ganze VFS bis in Shell-Ausgaben und App-Dialoge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoFehler {
    /// Start-Sektor oder Länge liegen hinter dem Ende des Geräts.
    AusserhalbDesGeraets,
    /// Die Puffer-Länge ist kein Vielfaches der Sektorgröße.
    UngueltigePufferGroesse,
    /// Das Gerät meldet einen Hardware-Fehler (Lese-/Schreibfehler).
    Geraetefehler,
    /// Kein Gerät vorhanden oder (noch) nicht bereit.
    NichtBereit,
}

impl IoFehler {
    /// Deutsche Fehlermeldung für Shell und Dialoge.
    pub fn meldung(&self) -> &'static str {
        match self {
            IoFehler::AusserhalbDesGeraets => "Zugriff hinter dem Ende des Geraets",
            IoFehler::UngueltigePufferGroesse => {
                "Puffer ist kein Vielfaches der Sektorgroesse"
            }
            IoFehler::Geraetefehler => "das Geraet meldet einen Hardware-Fehler",
            IoFehler::NichtBereit => "das Geraet ist nicht bereit",
        }
    }
}

/// DIE Schnittstelle, die jeder Blockgeräte-Treiber von SpeedOS
/// erfüllen muss. `&mut self` auch beim Lesen: Echte Treiber haben
/// veränderlichen Zustand (Kommando-Queues, DMA-Puffer).
pub trait BlockDevice: Send {
    /// Größe eines Sektors in Bytes (klassisch 512, modern oft 4096).
    fn sektor_groesse(&self) -> usize;
    /// Wie viele Sektoren das Gerät hat.
    fn anzahl_sektoren(&self) -> u64;
    /// Liest ab Sektor `start` so viele Sektoren, wie in den Puffer
    /// passen (Puffer-Länge = Vielfaches der Sektorgröße!).
    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler>;
    /// Schreibt den Puffer ab Sektor `start` (gleiche Puffer-Regel).
    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler>;
    /// Drückt alle gepufferten Schreibvorgänge aufs Medium.
    fn sync(&mut self) -> Result<(), IoFehler>;
}

// ---------------------------------------------------------------------------
// RamDisk — die Referenz-Implementierung (ein Vec im Kernel-Heap)
// ---------------------------------------------------------------------------

/// Ein Blockgerät im Arbeitsspeicher: für Tests, als Vorlage für
/// echte Treiber — und später als Unterbau, um ein Disk-Dateisystem
/// zu entwickeln, BEVOR der erste Platten-Treiber existiert.
pub struct RamDisk {
    daten: Vec<u8>,
    sektor_groesse: usize,
}

impl RamDisk {
    /// Legt eine RamDisk mit `anzahl_sektoren` Sektoren zu je
    /// `sektor_groesse` Bytes an (alles genullt — wie eine fabrikneue
    /// Platte nach dem Low-Level-Format).
    pub fn neu(sektor_groesse: usize, anzahl_sektoren: u64) -> Self {
        RamDisk {
            daten: vec![0; sektor_groesse * anzahl_sektoren as usize],
            sektor_groesse,
        }
    }

    /// Prüft Start-Sektor und Puffer-Länge und liefert den passenden
    /// Byte-Bereich in `daten` — die GEMEINSAME Validierung für Lesen
    /// und Schreiben (reine Rechnung, kein Zugriff).
    fn bereich(&self, start: u64, puffer_laenge: usize) -> Result<core::ops::Range<usize>, IoFehler> {
        if puffer_laenge == 0 || !puffer_laenge.is_multiple_of(self.sektor_groesse) {
            return Err(IoFehler::UngueltigePufferGroesse);
        }
        let sektoren = (puffer_laenge / self.sektor_groesse) as u64;
        // start + sektoren könnte bei absurden Werten überlaufen —
        // checked_add macht daraus einen sauberen Fehler.
        match start.checked_add(sektoren) {
            Some(ende) if ende <= self.anzahl_sektoren() => {
                let von = start as usize * self.sektor_groesse;
                Ok(von..von + puffer_laenge)
            }
            _ => Err(IoFehler::AusserhalbDesGeraets),
        }
    }
}

impl BlockDevice for RamDisk {
    fn sektor_groesse(&self) -> usize {
        self.sektor_groesse
    }

    fn anzahl_sektoren(&self) -> u64 {
        (self.daten.len() / self.sektor_groesse) as u64
    }

    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
        let bereich = self.bereich(start, puffer.len())?;
        puffer.copy_from_slice(&self.daten[bereich]);
        Ok(())
    }

    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
        let bereich = self.bereich(start, puffer.len())?;
        self.daten[bereich].copy_from_slice(puffer);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), IoFehler> {
        // Kein Cache, nichts zu tun — die Daten SIND schon "auf dem
        // Medium" (dem Heap).
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Schreiben und Zurücklesen über mehrere Sektoren — der Grundfall.
    #[test_case]
    fn test_ramdisk_roundtrip() {
        let mut disk = RamDisk::neu(512, 8);
        assert_eq!(disk.sektor_groesse(), 512);
        assert_eq!(disk.anzahl_sektoren(), 8);

        // Zwei Sektoren mit erkennbarem Muster ab Sektor 3 schreiben:
        let mut hin = vec![0u8; 1024];
        for (i, byte) in hin.iter_mut().enumerate() {
            *byte = (i % 251) as u8; // Primzahl-Modulo: kein 512er-Muster
        }
        disk.schreibe_sektoren(3, &hin).unwrap();

        let mut zurueck = vec![0u8; 1024];
        disk.lese_sektoren(3, &mut zurueck).unwrap();
        assert_eq!(hin, zurueck);

        // Nachbar-Sektoren blieben unangetastet (genullt):
        let mut nachbar = vec![0xAAu8; 512];
        disk.lese_sektoren(2, &mut nachbar).unwrap();
        assert!(nachbar.iter().all(|&b| b == 0));
        disk.lese_sektoren(5, &mut nachbar).unwrap();
        assert!(nachbar.iter().all(|&b| b == 0));

        // sync ist auf der RamDisk ein ehrliches No-Op:
        assert_eq!(disk.sync(), Ok(()));
    }

    /// Alle Grenz- und Fehlerfälle liefern IoFehler statt Panik.
    #[test_case]
    fn test_ramdisk_grenzen() {
        let mut disk = RamDisk::neu(512, 4);
        let mut puffer = vec![0u8; 512];

        // Letzter gültiger Sektor geht, dahinter nicht mehr:
        assert_eq!(disk.lese_sektoren(3, &mut puffer), Ok(()));
        assert_eq!(
            disk.lese_sektoren(4, &mut puffer),
            Err(IoFehler::AusserhalbDesGeraets)
        );
        // Zwei Sektoren ab dem letzten ragen hinaus:
        let mut doppelt = vec![0u8; 1024];
        assert_eq!(
            disk.lese_sektoren(3, &mut doppelt),
            Err(IoFehler::AusserhalbDesGeraets)
        );
        assert_eq!(
            disk.schreibe_sektoren(3, &doppelt),
            Err(IoFehler::AusserhalbDesGeraets)
        );
        // Ein Start-Sektor nahe u64::MAX darf nicht überlaufen:
        assert_eq!(
            disk.lese_sektoren(u64::MAX, &mut puffer),
            Err(IoFehler::AusserhalbDesGeraets)
        );

        // Puffer, die kein Sektor-Vielfaches sind (auch leer):
        let mut krumm = vec![0u8; 100];
        assert_eq!(
            disk.lese_sektoren(0, &mut krumm),
            Err(IoFehler::UngueltigePufferGroesse)
        );
        assert_eq!(
            disk.schreibe_sektoren(0, &[]),
            Err(IoFehler::UngueltigePufferGroesse)
        );
    }
}
