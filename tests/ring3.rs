// tests/ring3.rs — Der Ring-3-Beweis (Serie 6, Teil 1)
//
// Zwei Dinge, die es in SpeedOS vorher NICHT gab:
//   1. CPU-Code läuft in RING 3 (User-Mode), druckt per Syscall und kehrt
//      SAUBER in den Kernel zurück.
//   2. Ein ABSTURZ im User-Mode (verbotener Zugriff) wird aufgefangen — der
//      Kernel LEBT WEITER, statt (wie bei jedem bisherigen Fehler) anzuhalten.
//
// Erreichen wir die Erfolgsmeldung am Ende, hat der Kernel BEIDES überlebt.
// Ein Hänger/Triple-Fault (kaputter Privilegienwechsel oder kaputte Recovery)
// würde den Test in den Timeout laufen lassen.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::{allocator, memory, ring3, serial_println};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Volle CPU-Grundausstattung: GDT (jetzt MIT User-Segmenten + RSP0), IDT
    // (mit dem INT-0x80-Syscall-Gate), Speicher + Heap.
    speed_os::init();
    speed_os::zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// TEIL 1: Ring-3-Code druckt per Syscall und kehrt sauber zurück.
#[test_case]
fn test_ring3_erfolg() {
    serial_println!("[RING3-TEST] Erfolgs-Lauf:");
    ring3::ring3_erfolg();
    // Kommen wir hier an, ist der Kernel aus Ring 3 zurück (Ring 0).
    serial_println!("[RING3-TEST] Erfolgs-Lauf abgeschlossen — zurueck im Kernel.");
}

/// TEIL 2: Ein Absturz in Ring 3 (verbotener Kernel-Zugriff) reisst den
/// Kernel NICHT mit — nach der Page-Fault-Meldung läuft er weiter.
#[test_case]
fn test_ring3_z_absturz_wird_aufgefangen() {
    serial_println!("[RING3-TEST] Absturz-Lauf (ein Page Fault ist HIER erwartet):");
    ring3::ring3_absturz();
    // Erreichen wir DIESE Zeile, hat der Kernel den User-Absturz überlebt —
    // genau der Unterschied zu allem Bisherigen.
    serial_println!("[RING3-MEILENSTEIN] Kernel lebt nach dem User-Mode-Absturz weiter.");

    // Und der Beweis, dass NICHTS kaputt zurückblieb (GDT/TSS/Mappings):
    // Ein weiterer Ring-3-Lauf muss genauso sauber funktionieren.
    serial_println!("[RING3-TEST] Erneuter Ring-3-Lauf nach dem Absturz:");
    ring3::ring3_erfolg();
    serial_println!("[RING3-TEST] Auch nach dem Absturz laeuft Ring 3 fehlerfrei.");
}
