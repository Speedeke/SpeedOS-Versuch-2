// tests/ata_platte.rs — Integrationstests für den ATA-PIO-Treiber
//
// Läuft gegen die ECHTE (emulierte) Daten-Platte, die der Runner als
// Primary Slave anhängt (speedos-daten-test.img — bewusst ein eigenes
// Image, damit Tests nie die Nutzerdaten der normalen Daten-Platte
// anfassen).
//
// Das Herzstück ist der PERSISTENZ-TEST: Er prüft, ob ein früherer
// Testlauf sein Muster hinterlassen hat, und schreibt dann selbst
// eines. Beim ERSTEN Lauf wird nur geschrieben; jeder weitere Lauf
// findet das Muster des vorigen — der Beweis, dass Daten einen
// QEMU-Neustart überleben (der erste echte persistente Speicher von
// SpeedOS!). Der Beweis steht als [PERSISTENZ-BEWEIS]-Zeile in der
// seriellen Ausgabe.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::fs::block::{BlockDevice, IoFehler};
use speed_os::{allocator, ata, memory, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Volle Kernel-Grundausstattung: Interrupts (PIT!), TSC-Zeit für
    // die Polling-Timeouts, Heap für die Treiber-Strings — dann die
    // Laufwerks-Erkennung, genau wie im echten Boot-Ablauf.
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    ata::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// SEIT SPEEDFS: Alle Roh-Sektor-Tests leben am PLATTEN-ENDE!
/// Am Anfang liegen jetzt SpeedFS-Superblock und -Metadaten
/// (tests/speedfs_platte.rs) — Sektor 0 zu überschreiben würde das
/// Dateisystem zerstören. Die letzten ~600 Sektoren bleiben vom
/// First-Fit-Allokator des SpeedFS praktisch unberührt.
/// Sektor fürs Persistenz-Muster (Platte hat 131072 Sektoren).
const PERSISTENZ_LBA: u64 = 131000;
/// Start des 300-Sektoren-Roundtrip-Bereichs.
const ROUNDTRIP_LBA: u64 = 130500;
/// Erkennungszeichen am Sektor-Anfang.
const MAGIE: &[u8; 8] = b"SPEEDOS!";

/// Das Testmuster ist eine FUNKTION von Generation und Position —
/// so kann jeder spätere Lauf das Muster des vorigen VOLLSTÄNDIG
/// nachrechnen und Byte für Byte prüfen.
fn muster_byte(generation: u64, position: usize) -> u8 {
    (position as u64)
        .wrapping_mul(7)
        .wrapping_add(generation)
        .wrapping_add(13) as u8
}

/// Die Daten-Platte ist da und hat die erwartete Geometrie
/// (64-MiB-Image = 131072 Sektoren zu 512 Bytes).
#[test_case]
fn daten_platte_erkannt() {
    // Uebersprungen, wenn die Daten-Platte ueber virtio laeuft (dann
    // gibt es kein ATA-Daten-Laufwerk). Volle ATA-Abdeckung:
    // SPEEDOS_PLATTE=ide cargo test --test ata_platte
    if ata::daten_platte().is_none() {
        serial_println!("[ATA-TEST] uebersprungen — Daten-Platte laeuft ueber virtio.");
        return;
    }

    ata::mit_datenlaufwerk(|laufwerk| {
        assert_eq!(laufwerk.sektor_groesse(), 512);
        assert_eq!(laufwerk.anzahl_sektoren(), 131072);
        assert!(laufwerk.ist_beschreibbar());
        Ok(())
    })
    .expect("Daten-Laufwerk fehlt — haengt der Runner die zweite Platte an?");
}

/// Die BOOT-Platte ist erkannt, lesbar — aber Schreibversuche prallt
/// sie PER KONSTRUKTION ab (die Sicherheitsregel).
#[test_case]
fn boot_platte_ist_schreibgeschuetzt() {
    ata::mit_laufwerken(|laufwerke| {
        let boot = laufwerke
            .iter_mut()
            .find(|l| !l.ist_beschreibbar())
            .expect("Boot-Laufwerk nicht erkannt");
        // Lesen ist erlaubt (Sektor 0 = GPT-Schutz-MBR des Images):
        let mut sektor = [0u8; 512];
        boot.lese_sektoren(0, &mut sektor).expect("Boot-Platte unlesbar");
        // ... Schreiben nicht — egal wohin:
        assert_eq!(
            boot.schreibe_sektoren(0, &sektor),
            Err(IoFehler::Schreibgeschuetzt)
        );
        assert_eq!(
            boot.schreibe_sektoren(100, &sektor),
            Err(IoFehler::Schreibgeschuetzt)
        );
    });
}

/// Schreiben und Zurücklesen im selben Lauf — auch über die
/// 256-Sektoren-Grenze eines einzelnen ATA-Kommandos hinweg.
#[test_case]
fn roundtrip_schreiben_lesen() {
    // Uebersprungen, wenn die Daten-Platte ueber virtio laeuft (dann
    // gibt es kein ATA-Daten-Laufwerk). Volle ATA-Abdeckung:
    // SPEEDOS_PLATTE=ide cargo test --test ata_platte
    if ata::daten_platte().is_none() {
        serial_println!("[ATA-TEST] uebersprungen — Daten-Platte laeuft ueber virtio.");
        return;
    }

    // Zwei 150-KiB-Puffer passen nicht in den Start-Heap — nach
    // Projektregel VOR großen Puffern bewusst erweitern (128 Pages
    // = 512 KiB dazu):
    allocator::heap_erweitern(128).expect("Heap-Erweiterung fehlgeschlagen");
    ata::mit_datenlaufwerk(|laufwerk| {
        // 300 Sektoren zwingen den Treiber, in zwei Kommandos zu
        // zerlegen (256 + 44):
        let mut hin = vec![0u8; 300 * 512];
        for (i, byte) in hin.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        laufwerk.schreibe_sektoren(ROUNDTRIP_LBA, &hin)?;

        let mut zurueck = vec![0u8; 300 * 512];
        laufwerk.lese_sektoren(ROUNDTRIP_LBA, &mut zurueck)?;
        assert_eq!(hin, zurueck, "Gelesenes weicht vom Geschriebenen ab");

        // Grenzfälle wie bei der RamDisk: hinterm Ende, krumme Puffer.
        let mut sektor = [0u8; 512];
        assert_eq!(
            laufwerk.lese_sektoren(131072, &mut sektor),
            Err(IoFehler::AusserhalbDesGeraets)
        );
        assert_eq!(
            laufwerk.lese_sektoren(0, &mut [0u8; 100]),
            Err(IoFehler::UngueltigePufferGroesse)
        );
        Ok(())
    })
    .expect("Roundtrip fehlgeschlagen");
}

/// DER Persistenz-Test: findet (ab dem zweiten Lauf) das Muster des
/// vorigen Laufs, prüft es Byte für Byte und legt das eigene ab.
#[test_case]
fn persistenz_ueber_qemu_neustart() {
    // Uebersprungen, wenn die Daten-Platte ueber virtio laeuft (dann
    // gibt es kein ATA-Daten-Laufwerk). Volle ATA-Abdeckung:
    // SPEEDOS_PLATTE=ide cargo test --test ata_platte
    if ata::daten_platte().is_none() {
        serial_println!("[ATA-TEST] uebersprungen — Daten-Platte laeuft ueber virtio.");
        return;
    }

    ata::mit_datenlaufwerk(|laufwerk| {
        let mut sektor = [0u8; 512];
        laufwerk.lese_sektoren(PERSISTENZ_LBA, &mut sektor)?;

        let generation = if sektor[0..8] == MAGIE[..] {
            let alte = u64::from_le_bytes(sektor[8..16].try_into().unwrap());
            // Das komplette Muster des vorigen Laufs nachrechnen:
            for (position, byte) in sektor.iter().enumerate().skip(16) {
                assert_eq!(
                    *byte,
                    muster_byte(alte, position),
                    "Muster von Generation {} ist beschaedigt (Position {})",
                    alte,
                    position
                );
            }
            serial_println!(
                "[PERSISTENZ-BEWEIS] Muster von Generation {} intakt gefunden — \
                 die Daten haben den QEMU-Neustart ueberlebt!",
                alte
            );
            alte + 1
        } else {
            serial_println!(
                "[PERSISTENZ] Sektor {} noch leer — erster Lauf, schreibe Generation 1. \
                 (Test erneut starten fuer den Beweis.)",
                PERSISTENZ_LBA
            );
            1
        };

        // Das eigene Muster für den NÄCHSTEN Lauf hinterlassen:
        sektor[0..8].copy_from_slice(MAGIE);
        sektor[8..16].copy_from_slice(&generation.to_le_bytes());
        for (position, byte) in sektor.iter_mut().enumerate().skip(16) {
            *byte = muster_byte(generation, position);
        }
        laufwerk.schreibe_sektoren(PERSISTENZ_LBA, &sektor)?;
        // sync = FLUSH CACHE: erst damit ist "geschrieben" ehrlich.
        laufwerk.sync()?;

        // Kontrolle: sofort zurücklesen.
        let mut kontrolle = [0u8; 512];
        laufwerk.lese_sektoren(PERSISTENZ_LBA, &mut kontrolle)?;
        assert_eq!(sektor, kontrolle);
        Ok(())
    })
    .expect("Persistenz-Test fehlgeschlagen");
}

/// Ein Steckplatz OHNE Laufwerk (Secondary Slave) muss schnell und
/// sauber mit einem Fehler antworten — nicht hängen: Das ist der
/// Timeout-/Erkennungs-Pfad des Treibers.
#[test_case]
fn fehlendes_laufwerk_haengt_nicht() {
    let start = zeit::us_seit_boot();
    let ergebnis = ata::probe(0x170, 0x376, true);
    let dauer_us = zeit::us_seit_boot() - start;

    assert!(
        ergebnis.is_err(),
        "Am Secondary Slave darf kein Laufwerk sein"
    );
    // Der schnelle Erkennungspfad (Status 0x00/0xFF) braucht Mikro-
    // sekunden; selbst der Sicherheitsnetz-Timeout liegt bei 1 s.
    // Alles unter 1,5 s beweist: kein Endlos-Hänger.
    assert!(
        dauer_us < 1_500_000,
        "Erkennung hat {} us gebraucht — haengt das Polling?",
        dauer_us
    );
    serial_println!(
        "[ATA] Leerer Steckplatz sauber erkannt in {} us ({:?})",
        dauer_us,
        ergebnis.unwrap_err()
    );
}
