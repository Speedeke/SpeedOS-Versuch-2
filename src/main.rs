// main.rs — Einstiegspunkt des SpeedOS-Kernels
//
// Diese Datei ist das, was nach dem Bootloader als Erstes läuft.
// Es gibt kein Betriebssystem unter uns — nur wir und die CPU.
//
// Seit der Migration auf bootloader 0.11 booten wir in einen
// GRAFIKMODUS: Der Bootloader richtet per VESA einen linearen
// Framebuffer ein und übergibt ihn uns in der BootInfo. Text läuft
// übergangsweise nur über die serielle Schnittstelle (siehe konsole.rs),
// bis der Framebuffer-Text-Renderer gebaut ist.
//
// Ablauf beim Booten:
//   BIOS -> bootloader (Stage 1-4) -> kernel_main (hier!)
//   -> CPU-Strukturen (GDT/IDT/PIC) -> Speicher -> Heap -> Dateisystem
//   -> Framebuffer-Demo -> Executor startet die SpeedShell (seriell)

#![no_std] // Keine Standardbibliothek — es gibt ja noch kein OS, das sie tragen könnte
#![no_main] // Kein normales main(): Der Bootloader springt direkt zu unserem Entry Point
#![feature(custom_test_frameworks)] // Eigenes Test-Framework (siehe lib.rs)
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::task::{executor::Executor, Task};
use speed_os::{allocator, framebuffer, konsole, memory, serial_println, shell};
use x86_64::VirtAddr;

// Das entry_point!-Makro erzeugt die echte _start-Funktion und prüft
// die Signatur zur Compile-Zeit; die Config wird in eine spezielle
// ELF-Sektion serialisiert, aus der der Bootloader sie liest.
// (Die Framebuffer-Mindestauflösung 1280x720 wird seit bootloader
// 0.11.x NICHT hier, sondern beim Image-Bau konfiguriert — siehe
// boot/src/main.rs, BootConfig.)
entry_point!(kernel_main, config = &speed_os::BOOTLOADER_CONFIG);

/// Der eigentliche Kernel-Einstieg. Rückgabetyp `!`: kehrt nie zurück.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // 1. CPU-Strukturen: GDT, TSS, IDT laden, PIC scharf schalten.
    speed_os::init();
    serial_println!("[BOOT] SpeedOS startet (bootloader_api 0.11) ...");

    // 2. Den Framebuffer aus der BootInfo HERAUSNEHMEN (take), bevor
    //    wir die BootInfo zu &'static abwerten — sonst Borrow-Konflikt.
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;

    // 3. Speicherverwaltung: globaler Mapper + Bitmap-Frame-Allocator,
    //    dann Heap. Danach funktionieren Box, Vec, String & Co.
    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("Bootloader hat kein Physik-Mapping angelegt");
    memory::init(VirtAddr::new(phys_mem_offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    // 4. Grafik: Doppel-Puffer + Text-Konsole auf dem Framebuffer,
    //    dann der Boot-Screen (Obsidian-Aurora, ~1,5 Sekunden).
    match framebuffer {
        Some(fb) => {
            let info = fb.info();
            serial_println!(
                "[FB] Linearer Framebuffer: {}x{} Pixel, Format {:?}, {} Bytes/Pixel, Stride {} Pixel, Puffer {} KiB",
                info.width,
                info.height,
                info.pixel_format,
                info.bytes_per_pixel,
                info.stride,
                info.byte_len / 1024
            );
            framebuffer::init(fb);
            konsole::init();
            framebuffer::bootscreen_zeigen(1500);
            konsole::clear_screen();
        }
        None => serial_println!("[FB] WARNUNG: Kein Framebuffer — Ausgabe nur seriell!"),
    }

    // 5. Dateisystem: RamFs als Wurzel mounten (mit Demo-Dateien).
    speed_os::fs::init();

    serial_println!("[BOOT] GDT/IDT/PIC, Speicher, Heap, Grafik und RamFs initialisiert.");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // 6. Der Executor übernimmt als Hauptschleife: SpeedShell +
    //    Cursor-Blinken laufen als kooperative Tasks.
    let mut executor = Executor::new();
    executor.spawn(Task::new(shell::run()));
    executor.spawn(Task::new(konsole::cursor_blink_task()));
    executor.spawn(Task::new(speed_os::maus::maus_task()));
    executor.run();
}

/// Panic-Handler für den normalen Betrieb: Meldung in Rot (ANSI) über
/// die serielle Konsole, dann anhalten.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use speed_os::konsole::{self, Color};

    konsole::set_color(Color::LightRed, Color::Black);
    speed_os::println!("KERNEL PANIC: {}", info);
    speed_os::hlt_loop();
}

/// Panic-Handler im Testmodus: an das Test-Framework weiterreichen.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
