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
#![feature(abi_x86_interrupt)] // Aufrufkonvention für Exception-Handler (Nightly-Feature)
#![feature(alloc_error_handler)] // Eigener Handler für fehlgeschlagene Allokationen
#![test_runner(crate::test_runner)] // Unsere Funktion führt die Tests aus
#![reexport_test_harness_main = "test_main"] // Generierte Test-Startfunktion heißt test_main

// Das alloc-Crate ist der Teil der Standardbibliothek, der dynamischen
// Speicher braucht: Box, Vec, String, BTreeMap, Rc, ... Es funktioniert
// auch ohne OS — solange jemand einen #[global_allocator] bereitstellt.
// Das tun wir in allocator.rs!
extern crate alloc;

use core::panic::PanicInfo;

// Unsere Treiber- und System-Module — bewusst voneinander isoliert
// (Mikrokernel-Prinzip).
pub mod allocator;
pub mod fs;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod serial;
pub mod shell;
pub mod task;
pub mod vga_buffer;

/// Wird aufgerufen, wenn eine Allokation fehlschlägt (Heap voll).
/// Mehr als kontrolliert panicken können wir dann nicht — aber die
/// Meldung landet dank unserem Panic-Handler auf VGA UND seriell.
#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    panic!("Heap-Allokation fehlgeschlagen: {:?}", layout);
}

/// Initialisiert alle CPU-Strukturen des Kernels.
/// MUSS als Allererstes beim Boot aufgerufen werden — vorher führt
/// jede Exception zum Triple Fault und damit zum Reboot.
/// Reihenfolge wichtig: erst GDT/TSS (stellt den Notfall-Stack bereit),
/// dann die IDT (verweist auf diesen Stack), dann den PIC scharf
/// schalten und erst GANZ zum Schluss Interrupts erlauben.
pub fn init() {
    gdt::init();
    interrupts::init_idt();
    // `unsafe`: Der PIC ist falsch konfiguriert gefährlich —
    // unsere Offsets (32/40) sind die bewährte Standard-Wahl.
    unsafe { interrupts::PICS.lock().initialize() };
    // Interrupts auf der CPU aktivieren (sti-Befehl):
    // Ab jetzt können Timer und Tastatur jederzeit "dazwischenfunken".
    x86_64::instructions::interrupts::enable();
}

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

    // unsafe (Port-I/O): Port 0xf4 ist das isa-debug-exit-Device aus
    // unserer QEMU-Konfiguration (Cargo.toml test-args). Der Schreib-
    // zugriff kann keinen Speicher korrumpieren — er beendet höchstens
    // QEMU, und genau das ist hier der Zweck.
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

// Entry Point, wenn die Bibliothek selbst getestet wird (cargo test --lib).
// Das entry_point!-Makro des Bootloaders prüft die Signatur unserer
// Startfunktion zur Compile-Zeit — so kann man die BootInfo-Struktur
// gar nicht erst falsch entgegennehmen.
#[cfg(test)]
bootloader::entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(boot_info: &'static bootloader::BootInfo) -> ! {
    use x86_64::VirtAddr;

    init(); // GDT + IDT laden, damit Exception-Tests funktionieren

    // Auch Speicherverwaltung + Heap aufsetzen — die Shell-Tests
    // brauchen Box & Vec, die memory-Tests die globale API.
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    memory::init(phys_mem_offset, &boot_info.memory_map);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    // Dateisystem mounten — die fs- und Shell-Tests brauchen es.
    fs::init();

    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
