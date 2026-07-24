// tests/netz_shell.rs — Smoke-Test der Netz-Shell-Befehle (end-to-end)
//
// Führt die Netz-Befehle so aus, wie ein Nutzer sie tippt (über die echte
// Befehls-Registry), und beweist damit doppelt: (1) die Befehle laufen ohne
// Panik durch, (2) ihre Ausgabe ist die, die im Devlog/README steht. Der
// Runner spiegelt die serielle Ausgabe — so entstehen die „Screenshots".

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::shell::befehle::{alle_befehle, ShellKontext};
use speed_os::shell::befehl_ausfuehren;
use speed_os::{allocator, memory, pci, serial_println, virtio, zeit};
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
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    speed_os::ata::init();
    pci::init();
    speed_os::virtio::blk::init();
    virtio::net::init();
    speed_os::fs::init();
    speed_os::fs::platte_automounten();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Fährt eine ganze Netz-Sitzung durch die Befehls-Registry — wie getippt.
#[test_case]
fn test_netz_shell_sitzung() {
    // DHCP beim Boot nachstellen, damit netz-status etwas zeigt.
    speed_os::netz::dhcp::autokonfig(4000);

    let registry = alle_befehle();
    let mut ctx = ShellKontext::neu();

    let befehle = [
        "netz-status",
        "nslookup example.com",
        "ping 10.0.2.2",
        "hole http://10.0.2.2:8000/probe.txt",
        "arp",
    ];
    for zeile in befehle {
        serial_println!("\n----- SpeedOS:/> {} -----", zeile);
        befehl_ausfuehren(&registry, &mut ctx, zeile);
    }
    serial_println!("\n----- Ende der Netz-Sitzung -----");
}
