// tests/e2e_speedfs.rs — Der große End-to-End-Test gegen die ECHTE Platte
//
// Fährt die komplette SpeedFS-Sequenz (mkfs-Naht → Dateien anlegen →
// Editor-Roundtrip → rename-Orgie → fsck → alles noch da) gegen den
// echten Block-Treiber — IDE oder virtio, je nach SPEEDOS_PLATTE. Die
// Sequenz selbst (speedfs::e2e_ops / e2e_verifizieren) ist EXAKT die,
// die der RamDisk-Unit-Test (inkl. Absturz-Simulation) fährt — hier nur
// gegen echte Hardware statt gegen RAM.
//
// NON-DESTRUKTIV: arbeitet in einem eigenen Unterbaum /platte/e2e und
// räumt ihn wieder weg. So bleiben der Persistenz-Beweis von
// speedfs_platte.rs und die Roh-Sektoren von ata_platte.rs (alle teilen
// sich speedos-daten-test.img!) unangetastet. Die Absturz-Simulation
// gibt es bewusst NUR auf der RamDisk — man kann echte Writes nicht
// "verschwinden" lassen; das deckt der Unit-Test ab.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::fs::speedfs::{self, SpeedFs};
use speed_os::fs::{self, FsFehler};
use speed_os::{allocator, ata, memory, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Voller Boot-Unterbau wie in tests/speedfs_platte.rs:
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    fs::init();
    ata::init();
    // PCI + virtio-blk — dann findet fs::daten_geraet die Platte, egal
    // ob der Runner sie per IDE oder virtio anhängt:
    speed_os::pci::init();
    speed_os::virtio::blk::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Mountet die Daten-Platte als SpeedFS ins VFS (/platte); beim aller-
/// ersten Lauf (oder nach Roh-Altlasten auf Block 0) trägt sie noch
/// kein SpeedFS → formatieren (selbstheilend, wie mkfs.speedfs/mount).
fn platte_mounten() {
    let platte = fs::daten_geraet().expect("keine Daten-Platte erkannt");
    let speedfs = match SpeedFs::mounten(platte) {
        Ok(gemountet) => gemountet,
        Err((FsFehler::KeinSpeedFs, mut geraet)) => {
            serial_println!("[E2E] Kein SpeedFS auf der Platte — mkfs ...");
            speedfs::formatieren(geraet.as_mut()).expect("mkfs fehlgeschlagen");
            SpeedFs::mounten(geraet)
                .map_err(|(f, _)| f)
                .expect("Mount direkt nach mkfs fehlgeschlagen")
        }
        Err((f, _)) => panic!("Mount-Fehler: {:?}", f),
    };
    fs::mounten("/platte", alloc::boxed::Box::new(speedfs)).expect("VFS-Mount fehlgeschlagen");
}

/// DER E2E-Test: die geteilte Sequenz gegen den echten Block-Treiber,
/// dann fsck über die ganze Platte — alles non-destruktiv im Unterbaum.
#[test_case]
fn e2e_gegen_echte_platte() {
    const BASIS: &str = "/platte/e2e";

    platte_mounten();

    // Pre-Cleanup: Reste eines evtl. abgebrochenen Vorlaufs wegräumen.
    if fs::mit_fs(|f| f.node_typ(BASIS)).is_ok() {
        fs::loeschen_rekursiv(BASIS).expect("Pre-Cleanup fehlgeschlagen");
    }

    // Die geteilte Sequenz (identisch zum RamDisk-Unit-Test) gegen den
    // echten Block-Treiber fahren und den End-Zustand prüfen. e2e_ops /
    // e2e_verifizieren panicken selbst bei Abweichung, daher hier nur
    // ein Ok(()) für die Result-Signatur von mit_fs:
    fs::mit_fs(|f| {
        speedfs::e2e_ops(f, BASIS);
        Ok(())
    })
    .expect("e2e_ops");
    fs::mit_fs(|f| {
        speedfs::e2e_verifizieren(f, BASIS);
        Ok(())
    })
    .expect("e2e_verifizieren");
    fs::sync().expect("sync fehlgeschlagen");
    serial_println!(
        "[E2E-BEWEIS] Die komplette SpeedFS-Sequenz lief gegen die ECHTE Platte durch ({}).",
        BASIS
    );

    // Cleanup: den Unterbaum wieder entfernen, damit das Image so bleibt,
    // wie wir es fanden (Nachbar-Tests teilen dasselbe Image).
    fs::loeschen_rekursiv(BASIS).expect("Cleanup fehlgeschlagen");
    fs::sync().expect("sync nach Cleanup fehlgeschlagen");

    // Sauber aushängen, dann fsck über die GANZE Platte (roh gemountet,
    // wie pruefe.speedfs): unsere Ops + das Aufräumen dürfen 0 Defekte
    // hinterlassen.
    fs::unmounten("/platte").expect("umount fehlgeschlagen");
    let platte = fs::daten_geraet().expect("keine Daten-Platte (fsck)");
    let speedfs = SpeedFs::mounten(platte)
        .map_err(|(f, _)| f)
        .expect("Mount fuer fsck fehlgeschlagen");
    let bericht = speedfs.pruefen(false).expect("pruefen fehlgeschlagen");
    assert!(
        bericht.defekte.is_empty(),
        "fsck nach dem E2E-Lauf fand DEFEKTE: {:?}",
        bericht.defekte
    );
    serial_println!("[E2E-BEWEIS] fsck sauber (0 Defekte) nach dem Lauf.");
}
