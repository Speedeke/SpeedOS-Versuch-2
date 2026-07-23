// tests/netz_arp.rs — Integrationstest: ARP gegen QEMUs slirp-Netz
//
// Der Meilenstein der Serie-5-Stufe "ARP" ganz konkret und ECHT: SpeedOS
// löst über das virtio-net-Gerät die MAC des QEMU-Gateways auf. Das beweist
// den kompletten Pfad auf einmal — TX (ARP-Request bauen und senden), das
// reale Gerät (Virtqueue/DMA), RX (die Antwort per Used-Ring empfangen),
// Ethernet-Parsing, ARP-Parsing und den Cache.
//
// QEMUs user-mode-Netz (slirp) vergibt dem Gast per Konvention 10.0.2.15,
// stellt das Gateway auf 10.0.2.2 und BEANTWORTET ARP-Requests dafür. Der
// Runner (boot/) hängt die virtio-net-NIC immer an — also läuft dieser Test
// ohne weitere Einrichtung.
//
// Weil im Testmodus kein Executor läuft (keine async Tasks), PUMPEN wir den
// Empfang synchron: `netz::rx_verarbeiten()` in einer Schleife drainiert die
// RX-Queue des Geräts direkt (Polling des Used-Rings) und dispatcht — genau
// das, was auch der Shell-Befehl `arp-ping` tut.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::netz::{self, Ipv4};
use speed_os::{allocator, memory, pci, serial_println, virtio, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Volle Grundausstattung wie im echten Boot: Interrupts (PIT!), TSC-Zeit
    // für die Sende-/Pump-Timeouts, Heap für die Virtqueues, dann PCI
    // enumerieren und die virtio-net-NIC aufsetzen + registrieren.
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // Die virtio-Queues + RX-DMA-Puffer brauchen Heap-Luft (wie in main.rs
    // vor pci/virtio).
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    pci::init();
    virtio::net::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Löst die MAC des QEMU-Gateways (10.0.2.2) per ARP auf. Wenn der Cache
/// danach die MAC trägt, hat der gesamte Netz-Pfad funktioniert.
#[test_case]
fn test_arp_gateway_aufloesen() {
    // Der Runner hängt die NIC an — sie MUSS registriert sein.
    assert!(
        netz::vorhanden(),
        "keine virtio-net-NIC registriert (haengt der Runner sie an?)"
    );

    // slirp-Standard-Adressierung: Gast 10.0.2.15, Gateway 10.0.2.2.
    let gateway = Ipv4([10, 0, 2, 2]);
    netz::konfig_setzen(
        Ipv4([10, 0, 2, 15]),
        Ipv4([255, 255, 255, 0]),
        gateway,
    );

    // ARP-Request an das Gateway senden.
    netz::arp::anfrage_senden(gateway).expect("ARP-Request senden");

    // Empfang synchron pumpen, bis die Antwort da ist (max. 3 s).
    let deadline = zeit::ms_seit_boot() + 3000;
    loop {
        netz::rx_verarbeiten();
        if let Some(mac) = netz::arp::cache_suchen(gateway) {
            serial_println!(
                "[ARP-MEILENSTEIN] Gateway {} ist bei {} — SpeedOS spricht ARP.",
                gateway,
                netz::ethernet::mac_text(&mac)
            );
            return;
        }
        assert!(
            zeit::ms_seit_boot() < deadline,
            "keine ARP-Antwort vom Gateway (slirp) innerhalb 3 s"
        );
        x86_64::instructions::hlt();
    }
}
