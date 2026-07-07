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
use speed_os::vga_buffer::{self, Color};
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
    // Als Allererstes: GDT, TSS und IDT laden. Ab jetzt werden
    // CPU-Exceptions sauber gemeldet statt den Rechner neu zu starten.
    speed_os::init();

    // println! schreibt seit v0.1.1 IMMER auf VGA UND seriell gleichzeitig
    // (Projektregel: niemals nur VGA) — wir müssen nichts doppelt ausgeben.

    // Farbiger SpeedOS-Schriftzug: Gelb auf Blau.
    // "{:<60}" füllt jede Zeile mit Leerzeichen auf 60 Spalten auf,
    // damit der blaue Hintergrund als sauberer Block erscheint.
    vga_buffer::set_color(Color::Yellow, Color::Blue);
    println!("{:<60}", "");
    println!("{:<60}", "    ____                      _  ___  ____");
    println!("{:<60}", "   / ___| _ __   ___  ___  __| |/ _ \\/ ___|");
    println!("{:<60}", "   \\___ \\| '_ \\ / _ \\/ _ \\/ _` | | | \\___ \\");
    println!("{:<60}", "    ___) | |_) |  __/  __/ (_| | |_| |___) |");
    println!("{:<60}", "   |____/| .__/ \\___|\\___|\\__,_|\\___/|____/");
    println!("{:<60}", "         |_|            v0.1 - Hello World!");
    println!("{:<60}", "");

    // Ein paar Testzeilen in verschiedenen Farben:
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!("Kernel gebootet, VGA-Treiber und serieller Port laufen.");

    vga_buffer::set_color(Color::LightCyan, Color::Black);
    println!("Umlaut-Test (CP437): ä ö ü Ä Ö Ü ß — und é è ° § ²");
    println!("Nicht darstellbar (wird zu Ersatzzeichen): Euro-Symbol €");

    vga_buffer::set_color(Color::Pink, Color::Black);
    println!("Formatierung funktioniert: {} + {} = {}", 2, 3, 2 + 3);

    // Live-Beweis, dass das Exception-Handling funktioniert:
    // int3 löst eine Breakpoint-Exception aus — unser Handler meldet
    // sie, und die Ausführung geht danach einfach weiter.
    vga_buffer::set_color(Color::White, Color::Black);
    println!("Loese jetzt absichtlich eine Breakpoint-Exception aus ...");
    x86_64::instructions::interrupts::int3();
    println!("... und der Kernel laeuft einfach weiter!");

    // Zurück zur Standardfarbe für alles Weitere.
    vga_buffer::set_color(Color::LightGray, Color::Black);

    // Reine Debug-Info: geht NUR über die serielle Schnittstelle,
    // erscheint also im Terminal, aber nicht auf dem Bildschirm.
    serial_println!("[DEBUG] Kernel-Initialisierung abgeschlossen (nur seriell sichtbar).");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // Fertig — CPU schlafen legen (für immer, bis wir mehr können).
    speed_os::hlt_loop();
}

/// Panic-Handler für den normalen Betrieb: Wenn irgendwo im Kernel
/// ein Panic auftritt (z. B. unwrap() auf einem Fehler), landen wir hier.
/// println! gibt die Meldung automatisch auf beiden Kanälen aus.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_buffer::set_color(Color::LightRed, Color::Black);
    println!("KERNEL PANIC: {}", info);
    speed_os::hlt_loop();
}

/// Panic-Handler im Testmodus: an das Test-Framework weiterreichen,
/// das QEMU mit Fehlschlag-Code beendet.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
