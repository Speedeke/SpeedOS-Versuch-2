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

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::vga_buffer::{self, Color};
use speed_os::{memory, println, serial_println};
use x86_64::VirtAddr;

// Das entry_point!-Makro erzeugt die echte _start-Funktion für uns und
// ruft dann unser kernel_main auf. Vorteil: Die Signatur (BootInfo!)
// wird zur Compile-Zeit geprüft — mit einem handgeschriebenen
// `extern "C" fn _start` könnte man sie stillschweigend falsch machen.
entry_point!(kernel_main);

/// Der eigentliche Kernel-Einstieg. Der Bootloader übergibt uns die
/// BootInfo-Struktur: darin stecken die Memory Map (Landkarte des RAM)
/// und der Offset, ab dem der komplette physische Speicher virtuell
/// gemappt ist. Rückgabetyp `!`: kehrt niemals zurück.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
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

    // ----- Speicherverwaltung: Paging -----
    // Zugriff auf die Page Tables über das Komplett-Mapping des
    // physischen Speichers (Offset kommt vom Bootloader).
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_map) };

    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!("Paging initialisiert (phys. Speicher gemappt ab {:#x}).", boot_info.physical_memory_offset);

    // Adressübersetzungs-Demo, nur seriell (Debug):
    {
        use x86_64::structures::paging::Translate;
        for &adresse in &[0xb8000u64, boot_info.physical_memory_offset] {
            let virt = VirtAddr::new(adresse);
            serial_println!("[DEBUG] virtuell {:?} -> physisch {:?}", virt, mapper.translate_addr(virt));
        }
    }

    // Aufforderung zum Tippen — die Tastatur lebt!
    vga_buffer::set_color(Color::Yellow, Color::Black);
    println!();
    println!("Tippe etwas (QWERTZ, auch ä ö ü ß - Backspace/Entf loeschen):");

    // Demonstration: eine nagelneue virtuelle Page auf den VGA-Frame
    // (physisch 0xb8000) mappen und DARÜBER in die oberste
    // Bildschirmzeile schreiben. Zwei völlig verschiedene virtuelle
    // Adressen zeigen jetzt auf denselben physischen Speicher!
    // (Bewusst NACH der letzten println!-Zeile: jedes println scrollt
    // den Bildschirm — es würde unsere Zeile 0 sonst wegschieben.)
    {
        use x86_64::structures::paging::Page;
        let page = Page::containing_address(VirtAddr::new(0x_4444_4444_0000));
        memory::create_example_mapping(page, &mut mapper, &mut frame_allocator);

        let nachricht = b"PAGING FUNKTIONIERT! (via neuer virtueller Page geschrieben)";
        let page_ptr: *mut u8 = page.start_address().as_mut_ptr();
        for (i, &zeichen) in nachricht.iter().enumerate() {
            // Direkt in den (neu gemappten) VGA-Speicher: Zeichen-Byte
            // + Farb-Byte 0x2F = weiß auf grün. write_volatile, damit
            // der Compiler die Schreibzugriffe nicht wegoptimiert.
            unsafe {
                page_ptr.add(i * 2).write_volatile(zeichen);
                page_ptr.add(i * 2 + 1).write_volatile(0x2F);
            }
        }
    }

    // Zurück zur Standardfarbe für alles Weitere.
    vga_buffer::set_color(Color::LightGray, Color::Black);

    // Reine Debug-Info: geht NUR über die serielle Schnittstelle,
    // erscheint also im Terminal, aber nicht auf dem Bildschirm.
    serial_println!("[DEBUG] Kernel-Initialisierung abgeschlossen (nur seriell sichtbar).");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // CPU schlafen legen, bis der nächste Interrupt kommt (Timer oder
    // Tastatur) — so verbraucht SpeedOS im Leerlauf fast keine Rechenzeit,
    // statt in einer Endlosschleife 100 % CPU zu fressen.
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
