// main.rs — Einstiegspunkt des SpeedOS-Kernels
//
// Diese Datei ist das, was nach dem Bootloader als Erstes läuft.
// Es gibt kein Betriebssystem unter uns — nur wir und die CPU.
//
// Ablauf beim Booten:
//   BIOS -> Bootloader (bootloader-Crate) -> kernel_main (hier!)
//   -> CPU-Strukturen (GDT/IDT/PIC) -> Paging -> Heap
//   -> Executor startet die SpeedShell

#![no_std] // Keine Standardbibliothek — es gibt ja noch kein OS, das sie tragen könnte
#![no_main] // Kein normales main(): Der Bootloader springt direkt zu unserem Entry Point
#![feature(custom_test_frameworks)] // Eigenes Test-Framework (siehe lib.rs)
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::task::{executor::Executor, Task};
use speed_os::{allocator, memory, serial_println, shell};
use x86_64::VirtAddr;

// Das entry_point!-Makro erzeugt die echte _start-Funktion für uns und
// ruft dann unser kernel_main auf — mit zur Compile-Zeit geprüfter
// Signatur (BootInfo!).
entry_point!(kernel_main);

/// Der eigentliche Kernel-Einstieg. Rückgabetyp `!`: kehrt nie zurück.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // 1. CPU-Strukturen: GDT, TSS, IDT laden, PIC scharf schalten.
    //    Ab jetzt werden Exceptions gemeldet statt neu zu booten,
    //    und Timer + Tastatur melden sich per Interrupt.
    speed_os::init();

    // 2. Speicherverwaltung: Zugriff auf die Page Tables über das
    //    Komplett-Mapping des Bootloaders, dann den Heap mappen.
    //    Danach funktionieren Box, Vec, String & Co.
    //    unsafe: einmaliger Aufruf mit dem garantiert korrekten Mapping/
    //    der Memory Map des Bootloaders (siehe # Safety in memory.rs).
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("Heap-Initialisierung fehlgeschlagen");

    // 3. Dateisystem: RamFs als Wurzel mounten (mit Demo-Dateien).
    speed_os::fs::init();

    serial_println!("[DEBUG] GDT/IDT/PIC, Paging, Heap und RamFs initialisiert. Starte SpeedShell.");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // 4. Der Executor übernimmt als Hauptschleife des Kernels und
    //    startet die SpeedShell — SpeedOS ist jetzt interaktiv!
    //    (Die Shell liest die Tastatur über den async KeyStream;
    //    ist nichts zu tun, schläft die CPU per hlt.)
    let mut executor = Executor::new();
    executor.spawn(Task::new(shell::run()));
    executor.run();
}

/// Panic-Handler für den normalen Betrieb: Meldung in Rot auf VGA
/// (und automatisch auch seriell), dann anhalten.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use speed_os::vga_buffer::{self, Color};

    vga_buffer::set_color(Color::LightRed, Color::Black);
    speed_os::println!("KERNEL PANIC: {}", info);
    speed_os::hlt_loop();
}

/// Panic-Handler im Testmodus: an das Test-Framework weiterreichen.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
