// lib.rs — Kern-Bibliothek von SpeedOS
//
// Hier liegt alles, was sowohl der Kernel (main.rs) als auch die
// Integrationstests (tests/) brauchen: die Treiber-Module, das eigene
// Test-Framework und die Funktion zum Beenden von QEMU.
//
// Warum ein eigenes Test-Framework? Rusts normales Test-Framework
// braucht die Standardbibliothek (std) und ein Betriebssystem — beides
// haben wir nicht. Mit dem Nightly-Feature `custom_test_frameworks`
// bauen wir uns einen minimalen Ersatz: Er sammelt alle mit
// #[test_case] markierten Funktionen und führt sie nacheinander aus.

#![no_std] // Keine Standardbibliothek — wir sind das Betriebssystem!
#![cfg_attr(test, no_main)] // Im Testmodus: kein normales main()
#![feature(custom_test_frameworks)] // Eigenes Test-Framework (Nightly-Feature)
#![test_runner(crate::test_runner)] // Unsere Funktion führt die Tests aus
#![reexport_test_harness_main = "test_main"] // Generierte Test-Startfunktion heißt test_main

use core::panic::PanicInfo;

// Unsere Treiber-Module — bewusst voneinander isoliert (Mikrokernel-Prinzip).
pub mod serial;
pub mod vga_buffer;

// ---------------------------------------------------------------------------
// Die zentralen Ausgabe-Makros print! und println!
//
// Projektregel: Ausgaben gehen IMMER auf VGA UND die serielle
// Schnittstelle gleichzeitig — niemals nur VGA. Deshalb leben die
// Makros hier in lib.rs und rufen beide Treiber auf, statt in einem
// der Treiber-Module (die bleiben so voneinander isoliert).
// Für reine Debug-Ausgaben ohne Bildschirm gibt es serial_println!.
// ---------------------------------------------------------------------------

/// Interne Hilfsfunktion der Makros: schreibt auf beide Kanäle.
/// `fmt::Arguments` ist Copy, daher können wir es zweimal übergeben.
#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    vga_buffer::_print(args);
    serial::_print(args);
}

/// Gibt formatierten Text auf VGA UND seriell aus (wie print! in normalem Rust).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_print(format_args!($($arg)*)));
}

/// Wie print!, aber mit Zeilenumbruch am Ende.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Exit-Codes, die wir an QEMU übergeben.
/// QEMU rechnet daraus (wert << 1) | 1, also:
///   Success (0x10) -> Prozess-Exit-Code 33 (in Cargo.toml als Erfolg konfiguriert)
///   Failed  (0x11) -> Prozess-Exit-Code 35 (alles außer 33 gilt als Fehlschlag)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Beendet QEMU, indem wir einen Wert an das isa-debug-exit-Device
/// schreiben (I/O-Port 0xf4, siehe test-args in Cargo.toml).
/// Auf echter Hardware existiert dieses Device nicht — die Funktion
/// ist nur für Tests in QEMU gedacht.
pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}

/// Legt die CPU schlafen, bis der nächste Interrupt kommt — für immer,
/// solange wir keine Interrupts aktiviert haben. Sparsamer als eine
/// leere Endlosschleife, die die CPU auf 100 % Last halten würde.
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

/// Jeder Test bekommt über dieses Trait eine schöne Ausgabe:
/// Vor dem Test wird sein Name gedruckt, danach "[ok]".
pub trait Testable {
    fn run(&self);
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        // type_name gibt den vollen Funktionsnamen des Tests aus.
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

/// Unser Test-Runner: führt alle Tests aus und beendet QEMU mit Erfolg.
/// Schlägt ein Test fehl (Panic), übernimmt test_panic_handler.
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("{} Test(s) werden ausgefuehrt", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

/// Panic-Handler für den Testmodus: Fehler seriell ausgeben und
/// QEMU mit Fehlschlag-Code beenden, damit `cargo test` rot wird.
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[fehlgeschlagen]\n");
    serial_println!("Fehler: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

// ----- Ab hier: Nur für `cargo test` der Bibliothek selbst -----

/// Entry Point, wenn die Bibliothek selbst getestet wird (cargo test --lib).
#[cfg(test)]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
