// tests/speedfs_platte.rs — SpeedFS auf der ECHTEN (emulierten) Platte
//
// Der Nachfolger des rohen Sektor-Persistenz-Tests: Jetzt überlebt
// eine ECHTE DATEI in einem ECHTEN DATEISYSTEM den QEMU-Neustart.
// Der Test läuft über die komplette Naht-Kette, genau wie die Shell:
//   VFS (mit_fs) -> Mount-Tabelle (/platte) -> SpeedFS -> ATA-Treiber.
//
// Ablauf je Lauf: Platte mounten (beim allerersten Mal: mkfs, weil
// noch kein SpeedFS drauf ist), /platte/beweis.txt lesen — ist sie
// da, IST das der Persistenz-Beweis ([PERSISTENZ-BEWEIS]-Zeile in
// der seriellen Ausgabe) — und die nächste Generation schreiben.
//
// WICHTIG: tests/ata_platte.rs (läuft alphabetisch davor) schreibt
// seine Roh-Sektoren seit SpeedFS nur noch ans PLATTEN-ENDE, damit
// er dieses Dateisystem nicht zerstört.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::fs::speedfs::{self, SpeedFs};
use speed_os::fs::{self, FsFehler, NodeTyp};
use speed_os::{allocator, ata, memory, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Voller Boot-Unterbau wie in main.rs: Interrupts, TSC-Zeit,
    // Speicher, Heap, RamFs-Wurzel und die ATA-Erkennung.
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // Luft für Block-Cache und Puffer:
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    fs::init();
    ata::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// DER Test: Eine Datei in SpeedFS auf der ATA-Platte überlebt den
/// QEMU-Neustart — gelesen und geschrieben über das VFS, wie es
/// auch dir/type/write tun.
#[test_case]
fn speedfs_datei_ueberlebt_qemu_neustart() {
    // 1. Die Daten-Platte als SpeedFS mounten; beim allerersten
    //    Lauf (oder nach Roh-Sektor-Altlasten auf Block 0) trägt
    //    sie noch kein SpeedFS -> formatieren. Selbstheilend, und
    //    genau der Weg, den auch mkfs.speedfs/mount in der Shell
    //    nehmen.
    let platte = ata::daten_platte().expect("keine Daten-Platte erkannt");
    let speedfs = match SpeedFs::mounten(alloc::boxed::Box::new(platte)) {
        Ok(gemountet) => gemountet,
        Err((FsFehler::KeinSpeedFs, mut geraet)) => {
            serial_println!("[SPEEDFS] Kein Dateisystem auf der Platte — mkfs ...");
            speedfs::formatieren(geraet.as_mut()).expect("mkfs fehlgeschlagen");
            SpeedFs::mounten(geraet)
                .map_err(|(fehler, _)| fehler)
                .expect("Mount direkt nach mkfs fehlgeschlagen")
        }
        Err((fehler, _)) => panic!("Mount-Fehler: {:?}", fehler),
    };
    fs::mounten("/platte", alloc::boxed::Box::new(speedfs)).expect("VFS-Mount fehlgeschlagen");

    // 2. Die Beweis-Datei ÜBER DAS VFS lesen (erste Zeile:
    //    "generation=N") — existiert sie, ist der Beweis erbracht.
    let generation = match fs::mit_fs(|f| f.lesen("/platte/beweis.txt")) {
        Ok(inhalt) => {
            let text = core::str::from_utf8(&inhalt).expect("Beweis-Datei ist kein UTF-8");
            let generation: u64 = text
                .lines()
                .next()
                .and_then(|zeile| zeile.strip_prefix("generation="))
                .and_then(|zahl| zahl.parse().ok())
                .expect("Beweis-Datei hat ein unerwartetes Format");
            serial_println!(
                "[PERSISTENZ-BEWEIS] /platte/beweis.txt (Generation {}) hat den \
                 QEMU-Neustart als ECHTE DATEI in SpeedFS ueberlebt!",
                generation
            );
            generation + 1
        }
        Err(FsFehler::NichtGefunden) => {
            serial_println!(
                "[PERSISTENZ] Noch keine Beweis-Datei — erster Lauf, schreibe Generation 1."
            );
            1
        }
        Err(fehler) => panic!("Lesen fehlgeschlagen: {:?}", fehler),
    };

    // 3. Die nächste Generation für den kommenden Lauf hinterlegen.
    let text = alloc::format!(
        "generation={}\nDiese Datei liegt in SpeedFS auf der (emulierten) ATA-Platte\n\
         und wurde ueber das VFS geschrieben — wie von write/SpeedText auch.\n",
        generation
    );
    fs::mit_fs(|f| f.schreiben("/platte/beweis.txt", text.as_bytes()))
        .expect("Schreiben fehlgeschlagen");
    fs::sync().expect("sync fehlgeschlagen");

    // 4. Kontrolle im selben Lauf + die Mount-Naht von außen: Die
    //    Wurzel-Liste (RamFs!) zeigt /platte als Verzeichnis.
    let zurueck = fs::mit_fs(|f| f.lesen("/platte/beweis.txt")).unwrap();
    assert_eq!(zurueck, text.as_bytes());
    let stat = fs::mit_fs(|f| f.stat("/platte/beweis.txt")).unwrap();
    assert_eq!(stat.groesse, text.len());
    let wurzel = fs::mit_fs(|f| f.liste("/")).unwrap();
    assert!(wurzel
        .iter()
        .any(|eintrag| eintrag.name == "platte" && eintrag.typ == NodeTyp::Verzeichnis));

    // 5. Sauber aushängen (synct noch einmal) — wie der umount-Befehl.
    fs::unmounten("/platte").expect("umount fehlgeschlagen");
}
