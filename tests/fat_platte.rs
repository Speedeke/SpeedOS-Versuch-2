// tests/fat_platte.rs — FAT32-Treiber gegen das ECHTE Beispiel-Image
//
// Der Runner hängt ein FAT32-Image an (Secondary Master), das ein
// FREMDES Werkzeug erzeugt hat (mtools mformat/mcopy, siehe
// tools/fat32_image_erzeugen.py) — also liest der Treiber hier NICHT
// sein eigenes Geschreibsel, sondern ein Standard-FAT32. Der Test
// vergleicht die gelesenen Inhalte Byte für Byte mit den bekannten
// Vorlagen (die im Generator-Skript stehen — SYNCHRON halten!):
//   * Texte mit und ohne Umlaute (LFN + UTF-16 -> unsere Strings),
//   * eine große Datei über viele Cluster (gross.bin, i % 251),
//   * ein Unterordner mit einem sehr langen Dateinamen (viele LFN-
//     Zusatzeinträge),
//   * der Alltag: eine Datei von /fat nach /platte KOPIEREN,
//   * und das saubere Ablehnen jeder Schreib-Operation (NurLesen).

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::fs::fat32::Fat32;
use speed_os::fs::{self, FsFehler, IoFehler, NodeTyp};
use speed_os::{allocator, ata, memory, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // Luft für die 100-KiB-Datei + den SpeedFS-Block-Cache beim Kopieren:
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    fs::init();
    ata::init();
    // PCI + virtio-blk aufsetzen — dann findet fs::daten_geraet die
    // Daten-Platte egal ob der Runner sie per IDE oder virtio anhaengt:
    speed_os::pci::init();
    speed_os::virtio::blk::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Hängt das FAT-Laufwerk unter /fat ein (wie fs::fat_automounten,
/// aber im Test explizit, damit ein fehlendes Image sofort auffällt).
/// IDEMPOTENT: Jeder #[test_case] ruft es, aber /fat bleibt über die
/// Tests hinweg gemountet — der zweite Aufruf ist ein No-Op.
fn fat_mounten() {
    if fs::ist_gemountet("/fat") {
        return;
    }
    let platte = ata::fat_platte().expect(
        "kein FAT-Laufwerk erkannt — erzeugt der Runner speedos-fat.img (mtools/python)?",
    );
    let fs = Fat32::mounten(alloc::boxed::Box::new(platte))
        .map_err(|(fehler, _)| fehler)
        .expect("FAT32-Mount fehlgeschlagen (kein gueltiges FAT32?)");
    fs::mounten("/fat", alloc::boxed::Box::new(fs)).expect("VFS-Mount /fat fehlgeschlagen");
}

/// Die Wurzel und ein Unterordner tragen die erwarteten Einträge
/// (Umlaut-Dateiname inklusive — der Beweis, dass LFN/UTF-16 stimmt).
#[test_case]
fn fat_verzeichnisse_und_umlaut_namen() {
    fat_mounten();

    let wurzel: Vec<String> = fs::mit_fs(|f| f.liste("/fat"))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(
        wurzel,
        alloc::vec![
            String::from("Dokumente"),
            String::from("Grüße und Umlaute äöüß.txt"),
            String::from("gross.bin"),
            String::from("hallo.txt"),
        ]
    );

    // /fat/Dokumente enthält den sehr langen Namen (viele LFN-Einträge):
    let doks: Vec<String> = fs::mit_fs(|f| f.liste("/fat/Dokumente"))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    // Sortierung ist nach Unicode-Codepoint: 'e' (U+0065) kommt vor
    // 'Ü' (U+00DC), der lange Name steht also zuerst.
    assert_eq!(
        doks,
        alloc::vec![
            String::from("ein-sehr-langer-dateiname-der-mehrere-lfn-eintraege-braucht.txt"),
            String::from("Übergabe-Protokoll.txt"),
        ]
    );

    // node_typ/stat unterscheiden Datei und Ordner korrekt:
    assert_eq!(fs::mit_fs(|f| f.node_typ("/fat/Dokumente")).unwrap(), NodeTyp::Verzeichnis);
    assert_eq!(fs::mit_fs(|f| f.node_typ("/fat/hallo.txt")).unwrap(), NodeTyp::Datei);
}

/// Datei-Inhalte Byte für Byte gegen die bekannten Vorlagen — Texte,
/// Umlaute und die Datei im Unterordner.
#[test_case]
fn fat_datei_inhalte_stimmen() {
    fat_mounten();

    assert_eq!(
        fs::mit_fs(|f| f.lesen("/fat/hallo.txt")).unwrap(),
        b"Hallo von FAT32!\nDiese Datei liest SpeedOS von einem fremden Dateisystem.\n"
    );
    assert_eq!(
        fs::mit_fs(|f| f.lesen("/fat/Grüße und Umlaute äöüß.txt")).unwrap(),
        "Grüße vom FAT-Laufwerk!\nUmlaute funktionieren: Ä Ö Ü ä ö ü ß\n".as_bytes()
    );
    assert_eq!(
        fs::mit_fs(|f| f.lesen("/fat/Dokumente/Übergabe-Protokoll.txt")).unwrap(),
        "Protokoll der Übergabe:\nDateien vom USB-Stick nach SpeedOS holen.\n".as_bytes()
    );
    assert_eq!(
        fs::mit_fs(|f| f.lesen(
            "/fat/Dokumente/ein-sehr-langer-dateiname-der-mehrere-lfn-eintraege-braucht.txt"
        ))
        .unwrap(),
        b"Langer Name, kurzer Inhalt.\n"
    );
}

/// Die große Datei über viele Cluster: 100 000 Bytes = i % 251. Das
/// prüft die Cluster-Ketten-Verfolgung über Cluster-Sprünge hinweg,
/// plus read_at mitten in die Datei.
#[test_case]
fn fat_grosse_datei_ueber_cluster() {
    fat_mounten();

    let inhalt = fs::mit_fs(|f| f.lesen("/fat/gross.bin")).unwrap();
    assert_eq!(inhalt.len(), 100_000);
    for (i, b) in inhalt.iter().enumerate() {
        assert_eq!(*b, (i % 251) as u8, "gross.bin weicht bei Byte {} ab", i);
    }

    // read_at tief in der Datei (jenseits vieler Cluster-Grenzen):
    let mut stueck = [0u8; 500];
    let n = fs::mit_fs(|f| f.read_at("/fat/gross.bin", 55_000, &mut stueck)).unwrap();
    assert_eq!(n, 500);
    for (i, b) in stueck.iter().enumerate() {
        assert_eq!(*b, ((55_000 + i) % 251) as u8);
    }
    // stat liefert die richtige Größe:
    assert_eq!(fs::mit_fs(|f| f.stat("/fat/gross.bin")).unwrap().groesse, 100_000);
}

/// DER Alltagsfall: eine Datei vom FAT-Stick nach /platte holen. Dazu
/// wird /platte (SpeedFS) frisch formatiert, dann kopiert und der
/// Inhalt auf der Ziel-Platte gegengelesen.
#[test_case]
fn fat_kopieren_nach_platte() {
    fat_mounten();

    // /platte (SpeedFS) bereitstellen — formatieren, falls nötig:
    let platte = fs::daten_geraet().expect("keine Daten-Platte");
    let speedfs = match speed_os::fs::speedfs::SpeedFs::mounten(platte) {
        Ok(fs) => fs,
        Err((FsFehler::KeinSpeedFs, mut geraet)) => {
            speed_os::fs::speedfs::formatieren(geraet.as_mut()).expect("mkfs");
            speed_os::fs::speedfs::SpeedFs::mounten(geraet)
                .map_err(|(f, _)| f)
                .expect("Mount nach mkfs")
        }
        Err((f, _)) => panic!("SpeedFS-Mount: {:?}", f),
    };
    fs::mounten("/platte", alloc::boxed::Box::new(speedfs)).expect("Mount /platte");

    // Kopieren über die Mount-Grenze (fs::kopieren nutzt lesen+schreiben):
    fs::kopieren("/fat/hallo.txt", "/platte/hallo-vom-stick.txt").expect("Kopieren fehlgeschlagen");
    assert_eq!(
        fs::mit_fs(|f| f.lesen("/platte/hallo-vom-stick.txt")).unwrap(),
        b"Hallo von FAT32!\nDiese Datei liest SpeedOS von einem fremden Dateisystem.\n"
    );
    // Auch die große Datei kommt heil an:
    fs::kopieren("/fat/gross.bin", "/platte/gross-kopie.bin").expect("gross kopieren");
    let kopie = fs::mit_fs(|f| f.lesen("/platte/gross-kopie.bin")).unwrap();
    assert_eq!(kopie.len(), 100_000);
    assert!(kopie.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));

    serial_println!("[FAT] Dateien erfolgreich von /fat nach /platte kopiert.");
    fs::unmounten("/platte").expect("umount /platte");
}

/// FAT32 ist NUR LESEN: jede Schreib-Operation prallt sauber mit
/// IoFehler::NurLesen ab, und das VFS meldet /fat als nicht
/// beschreibbar (die Grundlage für die ausgegrauten Explorer-Aktionen).
#[test_case]
fn fat_ist_nur_lesbar() {
    fat_mounten();

    assert!(!fs::pfad_beschreibbar("/fat"));
    assert!(!fs::pfad_beschreibbar("/fat/Dokumente"));
    // Die RAM-Wurzel bleibt beschreibbar:
    assert!(fs::pfad_beschreibbar("/"));

    assert_eq!(
        fs::mit_fs(|f| f.schreiben("/fat/neu.txt", b"nein")),
        Err(FsFehler::Io(IoFehler::NurLesen))
    );
    assert_eq!(
        fs::mit_fs(|f| f.mkdir("/fat/neuerordner")),
        Err(FsFehler::Io(IoFehler::NurLesen))
    );
    assert_eq!(
        fs::mit_fs(|f| f.loeschen("/fat/hallo.txt")),
        Err(FsFehler::Io(IoFehler::NurLesen))
    );
    assert_eq!(
        fs::mit_fs(|f| f.write_at("/fat/hallo.txt", 0, b"x")),
        Err(FsFehler::Io(IoFehler::NurLesen))
    );

    // In der Mount-Übersicht steht FAT32 als "nur lesen":
    let mounts = fs::mount_uebersicht();
    let fat = mounts.iter().find(|m| m.praefix == "/fat").expect("/fat fehlt in der Uebersicht");
    assert_eq!(fat.typ, "FAT32");
    assert!(!fat.beschreibbar);
}
