// main.rs — Einstiegspunkt des SpeedOS-Kernels
//
// Diese Datei ist das, was nach dem Bootloader als Erstes läuft.
// Es gibt kein Betriebssystem unter uns — kein main(), keine
// Standardbibliothek, kein Speicher-Management. Nur wir und die CPU.
//
// Ablauf beim Booten:
//   BIOS -> Bootloader (bootloader-Crate) -> _start() (hier!)

#![no_std] // Keine Standardbibliothek — es gibt ja noch kein OS, das sie tragen könnte
#![no_main] // Kein normales main(): Der Bootloader springt direkt zu _start
#![feature(custom_test_frameworks)] // Eigenes Test-Framework (siehe lib.rs)
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use speed_os::{println, serial_println};

/// Der Entry Point unseres Kernels.
///
/// `#[no_mangle]` verhindert, dass Rust den Funktionsnamen verändert —
/// der Bootloader sucht im Binary nach genau dem Symbol "_start".
/// `extern "C"` legt die C-Aufrufkonvention fest, die der Bootloader benutzt.
/// Der Rückgabetyp `!` heißt: Diese Funktion kehrt NIEMALS zurück —
/// es gibt ja niemanden, zu dem sie zurückkehren könnte.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Unsere Begrüßung: einmal auf den Bildschirm (VGA) ...
    println!("SpeedOS v0.1 - Hello World!");
    // ... und einmal über die serielle Schnittstelle ins Terminal.
    // Projektregel: Debug-Ausgaben IMMER auch seriell, niemals nur VGA.
    serial_println!("SpeedOS v0.1 - Hello World!");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // Fertig — CPU schlafen legen (für immer, bis wir mehr können).
    speed_os::hlt_loop();
}

/// Panic-Handler für den normalen Betrieb: Wenn irgendwo im Kernel
/// ein Panic auftritt (z. B. unwrap() auf einem Fehler), landen wir hier.
/// Wir geben die Fehlermeldung auf beiden Kanälen aus und halten an.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("KERNEL PANIC: {}", info);
    serial_println!("KERNEL PANIC: {}", info);
    speed_os::hlt_loop();
}

/// Panic-Handler im Testmodus: an das Test-Framework weiterreichen,
/// das QEMU mit Fehlschlag-Code beendet.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
