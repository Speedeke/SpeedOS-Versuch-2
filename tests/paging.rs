// tests/paging.rs — Integrationstests für die Speicherverwaltung
//
// Testet die globale memory-API: Adressübersetzung, Mapping/Unmapping,
// den Bitmap-Frame-Allocator (inkl. Wiederverwendung freigegebener
// Frames!) und zusammenhängende Allokationen für Framebuffer/DMA.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::memory;
use x86_64::structures::paging::Page;
use x86_64::{PhysAddr, VirtAddr};

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    let boot_info: &'static BootInfo = boot_info;
    // Die globale Speicherverwaltung ist alles, was diese Tests brauchen
    // (kein Heap nötig — die memory-API alloziert selbst nichts).
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Das Komplett-Mapping des physischen Speichers: virtuelle Adresse
/// (offset + X) muss physische Adresse X ergeben — wir prüfen X = 0.
#[test_case]
fn phys_offset_mapping_zeigt_auf_phys_null() {
    let phys = memory::uebersetzen(memory::phys_offset());
    assert_eq!(phys, Some(PhysAddr::new(0)));
}

/// Eine wilde, nie gemappte Adresse darf KEINE Übersetzung haben.
#[test_case]
fn ungemappte_adresse_hat_keine_uebersetzung() {
    let phys = memory::uebersetzen(VirtAddr::new(0x_dead_beef_0000));
    assert_eq!(phys, None);
}

/// map_page_zu auf einen bestimmten Frame (wie beim Framebuffer/MMIO):
/// Die Übersetzung muss exakt auf diesen Frame zeigen.
#[test_case]
fn map_page_zu_wird_korrekt_uebersetzt() {
    let page = Page::containing_address(VirtAddr::new(0x_5555_5555_0000));
    // Einen konkreten Frame besorgen und die Page GENAU dorthin mappen:
    let frame = memory::frame_allozieren().expect("kein freier Frame");
    // unsafe: Der Frame kommt exklusiv aus dem Allocator — kein Aliasing.
    unsafe { memory::map_page_zu(page, frame).unwrap() };

    let phys = memory::uebersetzen(page.start_address());
    assert_eq!(phys, Some(frame.start_address()));
}

/// map_page + Schreiben/Lesen + unmap_page: der volle Lebenszyklus
/// einer Page. Nach dem Unmap ist die Adresse wieder unübersetzbar.
#[test_case]
fn map_schreiben_unmap() {
    let page = Page::containing_address(VirtAddr::new(0x_6666_6666_0000));
    memory::map_page(page).unwrap();

    // Über das frische Mapping schreiben und zurücklesen:
    let zeiger: *mut u64 = page.start_address().as_mut_ptr();
    // unsafe: Die Page ist soeben gültig gemappt, exklusiv unsere.
    unsafe {
        zeiger.write_volatile(0xdead_beef_1234_5678);
        assert_eq!(zeiger.read_volatile(), 0xdead_beef_1234_5678);
    }

    let frame = memory::unmap_page(page).unwrap();
    // unsafe: Die Page ist unmapped, der Frame nirgendwo mehr in Benutzung.
    unsafe { memory::frame_freigeben(frame) };
    assert_eq!(memory::uebersetzen(page.start_address()), None);
}

/// DER Wiederverwendungs-Test: allozieren -> freigeben -> wieder
/// allozieren muss DENSELBEN Frame liefern (der Next-Fit-Zeiger wird
/// beim Freigeben zurückgesetzt). Vorher ging jeder freigegebene
/// Frame für immer verloren!
#[test_case]
fn frame_wiederverwendung() {
    let erster = memory::frame_allozieren().expect("kein freier Frame");
    // unsafe: Frame wurde nie gemappt, niemand benutzt ihn.
    unsafe { memory::frame_freigeben(erster) };

    let zweiter = memory::frame_allozieren().expect("kein freier Frame");
    assert_eq!(erster, zweiter, "freigegebener Frame wurde nicht wiederverwendet");

    // Aufräumen + Statistik-Gegenprobe:
    let (frei_vorher, _) = memory::frame_statistik();
    unsafe { memory::frame_freigeben(zweiter) };
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(frei_nachher, frei_vorher + 1);
}

/// Zusammenhängende Allokation (Framebuffer/DMA-Fall): 4 Pages, die
/// virtuell UND physisch lückenlos aufeinanderfolgen.
#[test_case]
fn zusammenhaengende_pages() {
    const ANZAHL: usize = 4;
    let start = memory::allocate_pages(ANZAHL).expect("allocate_pages fehlgeschlagen");

    // Über alle Page-Grenzen hinweg schreiben und stichprobenartig lesen:
    let zeiger: *mut u8 = start.as_mut_ptr();
    for i in 0..ANZAHL * 4096 {
        // unsafe: Der Bereich ist frisch gemappt und exklusiv unserer.
        unsafe { zeiger.add(i).write_volatile((i % 251) as u8) };
    }
    for i in (0..ANZAHL * 4096).step_by(1013) {
        unsafe { assert_eq!(zeiger.add(i).read_volatile(), (i % 251) as u8) };
    }

    // Physische Kontiguität: jede Page liegt exakt 4096 Bytes
    // hinter der vorherigen.
    let phys_start = memory::uebersetzen(start).expect("nicht gemappt");
    for i in 1..ANZAHL {
        let phys = memory::uebersetzen(start + (i * 4096) as u64).expect("nicht gemappt");
        assert_eq!(
            phys.as_u64(),
            phys_start.as_u64() + (i * 4096) as u64,
            "Frames sind nicht zusammenhaengend"
        );
    }
}
