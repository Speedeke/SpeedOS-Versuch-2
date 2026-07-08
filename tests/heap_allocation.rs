// tests/heap_allocation.rs — Integrationstests für den Kernel-Heap
//
// Diese Tests booten in QEMU, initialisieren Paging + Heap und prüfen
// dann, dass dynamische Allokationen (Box, Vec, String, BTreeMap)
// wirklich funktionieren — inklusive der wichtigen Frage, ob
// freigegebener Speicher WIEDERVERWENDET wird.
//
// Tipp zum Lernen: Lass die Tests mal mit den anderen Allocatoren laufen!
//   cargo test --test heap_allocation --features bump-allocator
//     -> langlebige_allokation_bleibt_intakt schlägt fehl! Die eine
//        dauerhafte Box hält den Allokations-Zähler über 0, deshalb
//        wird der Heap nie zurückgesetzt und läuft voll. (Der Test
//        viele_boxen_nacheinander besteht dagegen: Dort fällt der
//        Zähler nach jeder Box auf 0 -> Komplett-Reset.)
//   cargo test --test heap_allocation --features fixed-block-allocator
//     -> alles grün

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::allocator::{self, HEAP_SIZE};
use speed_os::memory;
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    let boot_info: &'static BootInfo = boot_info;

    // Speicherverwaltung + Heap aufsetzen — ohne das gäbe es bei der
    // ersten Allokation einen Panic im alloc_error_handler.
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Einfachste Allokation: zwei Boxen, Werte müssen stimmen.
#[test_case]
fn einfache_box_allokation() {
    let wert_1 = Box::new(41);
    let wert_2 = Box::new(13);
    assert_eq!(*wert_1, 41);
    assert_eq!(*wert_2, 13);
}

/// Ein großer Vec: wächst schrittweise (realloc!) auf 1000 Elemente.
/// Nebenbei ein Rechen-Check über die Gauß-Summenformel.
#[test_case]
fn grosser_vec() {
    let n: u64 = 1000;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.len(), n as usize);
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

/// Wiederverwendungs-Test: 102.400 Allokationen nacheinander —
/// zusammen weit mehr als die 100 KiB Heap. Das geht nur gut, wenn
/// der Allocator freigegebenen Speicher wieder hergibt.
#[test_case]
fn viele_boxen_nacheinander() {
    for i in 0..HEAP_SIZE {
        let x = Box::new(i);
        assert_eq!(*x, i);
        // x fällt hier aus dem Scope -> dealloc -> wiederverwendbar
    }
}

/// String und BTreeMap aus dem alloc-Crate funktionieren auch.
#[test_case]
fn string_und_btreemap() {
    let mut s = String::new();
    s.push_str("Speed");
    s.push_str("OS");
    assert_eq!(s, "SpeedOS");

    let mut map = BTreeMap::new();
    map.insert("kernel", "Rust");
    map.insert("bootloader", "0.9");
    assert_eq!(map.get("kernel"), Some(&"Rust"));
    assert_eq!(map.len(), 2);
}

/// Heap-Erweiterung zur Laufzeit: Eine Allokation, die größer ist als
/// der GESAMTE bisherige Heap, kann erst nach heap_erweitern() klappen.
/// try_reserve statt normalem push: Es gibt ein Result zurück, statt
/// bei Speichermangel in den alloc_error_handler zu panicken.
#[test_case]
fn heap_erweiterung_zur_laufzeit() {
    let mut puffer: Vec<u8> = Vec::new();
    // Größer als der komplette aktuelle Heap -> unmöglich:
    let riesig = allocator::heap_groesse() + 10 * 4096;
    assert!(
        puffer.try_reserve(riesig).is_err(),
        "Riesen-Allokation haette fehlschlagen muessen"
    );

    // Heap um 100 Pages (400 KiB) erweitern ...
    let neue_groesse = allocator::heap_erweitern(100).expect("heap_erweitern fehlgeschlagen");
    assert_eq!(neue_groesse, allocator::heap_groesse());
    assert!(neue_groesse >= riesig);

    // ... und jetzt passt sie:
    assert!(
        puffer.try_reserve(riesig).is_ok(),
        "Allokation muesste nach der Erweiterung klappen"
    );
    // Den Speicher auch wirklich benutzen:
    for i in 0..riesig {
        puffer.push((i % 256) as u8);
    }
    assert_eq!(puffer.len(), riesig);
}

/// Langlebige + kurzlebige Allokationen gemischt: Eine Box muss ihren
/// Wert behalten, während drumherum tausendfach allokiert/freigegeben
/// wird — ein Klassiker, um Allocator-Bugs (Überschreiben!) zu finden.
/// DER Härtetest fürs Wiederverwenden: Der Bump-Allocator scheitert
/// hier, weil die langlebige Box seinen Komplett-Reset verhindert.
#[test_case]
fn langlebige_allokation_bleibt_intakt() {
    let langlebig = Box::new(1);
    for i in 0..HEAP_SIZE {
        let kurzlebig = Box::new(i);
        assert_eq!(*kurzlebig, i);
    }
    assert_eq!(*langlebig, 1);
}
