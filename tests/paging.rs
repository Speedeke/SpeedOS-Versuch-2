// tests/paging.rs — Integrationstests für die Adressübersetzung
//
// Diese Tests booten in QEMU und prüfen, dass unsere Speicherverwaltung
// virtuelle Adressen korrekt in physische übersetzt — sowohl bei
// Mappings, die der Bootloader angelegt hat, als auch bei einem
// Mapping, das wir selbst mit map_to erzeugen.
//
// Besonderheit: Die Test-Funktionen (#[test_case]) bekommen keine
// Argumente, aber wir brauchen den Mapper aus der BootInfo. Deshalb
// legt der Entry Point Mapper & FrameAllocator in globale, gelockte
// Variablen, aus denen sich die Tests bedienen.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use spin::Mutex;
use speed_os::memory::{self, BootInfoFrameAllocator};
use x86_64::structures::paging::{OffsetPageTable, Translate};
use x86_64::{PhysAddr, VirtAddr};

/// Globale Ablage für Mapper & Co., gefüllt vom Entry Point.
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);
static PHYS_OFFSET: Mutex<u64> = Mutex::new(0);

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    speed_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    *MAPPER.lock() = Some(unsafe { memory::init(phys_mem_offset) });
    *FRAME_ALLOCATOR.lock() =
        Some(unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) });
    *PHYS_OFFSET.lock() = boot_info.physical_memory_offset;

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Der VGA-Puffer ist vom Bootloader identisch gemappt (identity
/// mapping): virtuell 0xb8000 muss physisch 0xb8000 ergeben.
#[test_case]
fn vga_puffer_ist_identisch_gemappt() {
    let mapper = MAPPER.lock();
    let mapper = mapper.as_ref().unwrap();
    let phys = mapper.translate_addr(VirtAddr::new(0xb8000));
    assert_eq!(phys, Some(PhysAddr::new(0xb8000)));
}

/// Das Komplett-Mapping des physischen Speichers: virtuelle Adresse
/// (offset + X) muss physische Adresse X ergeben — wir prüfen X = 0.
#[test_case]
fn phys_offset_mapping_zeigt_auf_phys_null() {
    let offset = *PHYS_OFFSET.lock();
    let mapper = MAPPER.lock();
    let mapper = mapper.as_ref().unwrap();
    let phys = mapper.translate_addr(VirtAddr::new(offset));
    assert_eq!(phys, Some(PhysAddr::new(0)));
}

/// Eine wilde, nie gemappte Adresse darf KEINE Übersetzung haben.
#[test_case]
fn ungemappte_adresse_hat_keine_uebersetzung() {
    let mapper = MAPPER.lock();
    let mapper = mapper.as_ref().unwrap();
    let phys = mapper.translate_addr(VirtAddr::new(0x_dead_beef_0000));
    assert_eq!(phys, None);
}

/// Königsdisziplin: Wir mappen selbst eine neue Page auf den VGA-Frame
/// und prüfen, dass die Übersetzung danach exakt dorthin zeigt.
#[test_case]
fn eigenes_mapping_wird_korrekt_uebersetzt() {
    use x86_64::structures::paging::Page;

    let mut mapper = MAPPER.lock();
    let mapper = mapper.as_mut().unwrap();
    let mut frame_allocator = FRAME_ALLOCATOR.lock();
    let frame_allocator = frame_allocator.as_mut().unwrap();

    let page = Page::containing_address(VirtAddr::new(0x_5555_5555_0000));
    memory::create_example_mapping(page, mapper, frame_allocator);

    let phys = mapper.translate_addr(page.start_address());
    assert_eq!(phys, Some(PhysAddr::new(0xb8000)));
}
