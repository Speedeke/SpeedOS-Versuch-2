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

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::task::{executor::Executor, keyboard, yield_now, Task};
use speed_os::vga_buffer::{self, Color};
use speed_os::{allocator, memory, println, serial_println};
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

    // Heap mappen und den Allocator scharf schalten — ab hier
    // funktionieren Box, Vec, String und alle anderen alloc-Typen!
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("Heap-Initialisierung fehlgeschlagen");
    println!("Heap initialisiert ({} KiB ab {:#x}).", allocator::HEAP_SIZE / 1024, allocator::HEAP_START);

    // Demo: ein Vec mit Strings — zur Compile-Zeit unbekannte Menge
    // dynamischen Speichers, im Kernel bisher undenkbar!
    {
        let mut features: Vec<&str> = Vec::new();
        features.push("VGA-Textmodus mit CP437-Umlauten");
        features.push("Serielle Debug-Ausgabe (COM1)");
        features.push("Exceptions: Breakpoint, Page Fault, Double Fault");
        features.push("Hardware-Interrupts: Timer + QWERTZ-Tastatur");
        features.push("Paging mit eigenem Frame-Allocator");
        features.push("Kernel-Heap: Box, Vec, String, BTreeMap");

        vga_buffer::set_color(Color::LightCyan, Color::Black);
        println!("SpeedOS-Features (aus einem Vec auf dem Heap, {} Stueck):", features.len());
        for (nr, feature) in features.iter().enumerate() {
            println!("  {}. {}", nr + 1, feature);
        }
    }

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

    // Multitasking-Demo: zwei Zähl-Tasks, die sich die CPU freiwillig
    // teilen (yield_now) — ihre Ausgaben erscheinen gleich verschränkt.
    vga_buffer::set_color(Color::Pink, Color::Black);
    println!("Kooperatives Multitasking, 2 Tasks verschraenkt:");
    vga_buffer::set_color(Color::LightGray, Color::Black);

    // Demonstration: eine nagelneue virtuelle Page auf den VGA-Frame
    // (physisch 0xb8000) mappen und DARÜBER in die oberste
    // Bildschirmzeile schreiben. Zwei völlig verschiedene virtuelle
    // Adressen zeigen jetzt auf denselben physischen Speicher!
    // (Bewusst NACH der letzten println!-Zeile: jedes println scrollt
    // den Bildschirm — es würde unsere Zeile 0 sonst wegschieben.)
    // Achtung: Adresse 0x6666..., denn ab 0x4444_4444_0000 liegt
    // jetzt der Kernel-Heap (siehe allocator.rs)!
    {
        use x86_64::structures::paging::Page;
        let page = Page::containing_address(VirtAddr::new(0x_6666_6666_0000));
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

    // Der Executor übernimmt: Er ist ab jetzt die Hauptschleife des
    // Kernels. Drei Tasks laufen "gleichzeitig" (kooperativ):
    // zwei Zähler und die Tastatur-Verarbeitung. Ist nichts zu tun,
    // legt der Executor die CPU mit hlt schlafen — wie früher unsere
    // hlt_loop, nur schlauer.
    let mut executor = Executor::new();
    executor.spawn(Task::new(zaehler_task("Task A", 1)));
    executor.spawn(Task::new(zaehler_task("Task B", 100)));
    executor.spawn(Task::new(keyboard::print_keypresses()));
    executor.run();
}

/// Demo-Task: zählt 5 Schritte ab `start` und gibt nach JEDEM Schritt
/// die CPU freiwillig ab (yield_now). Weil zwei Instanzen dieses Tasks
/// laufen, wechseln sich ihre Ausgaben sichtbar ab — der Beweis, dass
/// hier wirklich zwei Abläufe verschränkt vorankommen.
async fn zaehler_task(name: &'static str, start: u64) {
    for i in start..start + 5 {
        println!("  [{}] zaehlt: {}", name, i);
        yield_now().await; // Kooperation: andere Tasks sind dran!
    }
    println!("  [{}] fertig.", name);
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
