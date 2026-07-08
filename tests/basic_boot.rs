// tests/basic_boot.rs — Integrationstest: Bootet der Kernel?
//
// Dieser Test ist ein komplett eigenständiges Mini-Betriebssystem:
// Er wird wie der echte Kernel gebaut, von QEMU gebootet und prüft,
// dass die grundlegende Ausgabe funktioniert. Am Ende beendet er QEMU
// über das isa-debug-exit-Device mit einem Exit-Code — so weiß
// `cargo test`, ob alles geklappt hat.
//
// Starten mit: cargo test

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::{println, serial_println};

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

/// Entry Point dieses Test-Kernels: direkt die Tests starten.
/// Dass wir überhaupt bis hierher kommen, beweist schon,
/// dass der Kernel erfolgreich gebootet hat!
fn main(_boot_info: &'static mut BootInfo) -> ! {
    test_main();
    speed_os::hlt_loop();
}

/// Bei einem Panic: Fehlschlag melden und QEMU beenden.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Test 1: Das println!-Makro funktioniert nach dem Boot
/// (geht seit der Framebuffer-Migration über die serielle Leitung).
#[test_case]
fn println_funktioniert() {
    println!("Testausgabe ueber println!");
}

/// Test 2: Die direkte serielle Debug-Ausgabe funktioniert.
#[test_case]
fn serielle_ausgabe_funktioniert() {
    serial_println!("Testausgabe auf der seriellen Schnittstelle");
}
