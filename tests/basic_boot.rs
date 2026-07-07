// tests/basic_boot.rs — Integrationstest: Bootet der Kernel?
//
// Dieser Test ist ein komplett eigenständiges Mini-Betriebssystem:
// Er wird wie der echte Kernel gebaut, von QEMU gebootet und prüft,
// dass die grundlegenden Ausgabekanäle (VGA und seriell) funktionieren.
// Am Ende beendet er QEMU über das isa-debug-exit-Device mit einem
// Exit-Code — so weiß `cargo test`, ob alles geklappt hat.
//
// Starten mit: cargo test

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)] // Test-Runner aus unserer lib.rs benutzen
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::{println, serial_println};

entry_point!(main);

/// Entry Point dieses Test-Kernels: direkt die Tests starten.
/// Dass wir überhaupt bis hierher kommen, beweist schon,
/// dass der Kernel erfolgreich gebootet hat!
fn main(_boot_info: &'static BootInfo) -> ! {
    test_main();
    speed_os::hlt_loop();
}

/// Bei einem Panic: Fehlschlag melden und QEMU beenden.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Test 1: Die VGA-Ausgabe funktioniert nach dem Boot.
/// Würde println! abstürzen (z. B. weil der VGA-Puffer nicht
/// erreichbar ist), gäbe es einen Panic und der Test schlüge fehl.
#[test_case]
fn vga_ausgabe_funktioniert() {
    println!("Testausgabe auf VGA");
}

/// Test 2: Die serielle Ausgabe funktioniert nach dem Boot.
#[test_case]
fn serielle_ausgabe_funktioniert() {
    serial_println!("Testausgabe auf der seriellen Schnittstelle");
}
