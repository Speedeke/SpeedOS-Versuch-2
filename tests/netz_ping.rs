// tests/netz_ping.rs — Integrationstest: SpeedOS pingt das QEMU-Gateway
//
// Der klassische Netzwerk-Meilenstein, ECHT: SpeedOS schickt einen ICMP-
// Echo-Request an das slirp-Gateway (10.0.2.2) und bekommt eine Antwort.
// Das beweist den KOMPLETTEN Pfad — ARP (Next-Hop-MAC auflösen), IPv4
// (Kopf + Prüfsumme), ICMP (Echo bauen), TX über das virtio-net-Gerät, und
// den ganzen Empfangsweg zurück (RX -> Ethernet -> IPv4 -> ICMP-Reply).
//
// QEMUs user-mode-Netz (slirp) beantwortet Pings an das Gateway. Der Runner
// hängt die NIC immer an — der Test läuft ohne weitere Einrichtung.
//
// Wie in tests/netz_arp.rs PUMPEN wir den Empfang synchron (im Testmodus
// läuft kein Executor): `netz::rx_verarbeiten()` drainiert die RX-Queue und
// dispatcht — genau das tut auch der Shell-Befehl `ping`.

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
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
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

/// Pingt das Gateway (10.0.2.2): Echo-Request senden, Empfang pumpen, bis
/// die Echo-Antwort da ist. Gelingt das, funktioniert der ganze IPv4/ICMP-
/// Pfad end-to-end.
#[test_case]
fn test_ping_gateway() {
    assert!(netz::vorhanden(), "keine virtio-net-NIC registriert");

    let gateway = Ipv4([10, 0, 2, 2]);
    netz::konfig_setzen(Ipv4([10, 0, 2, 15]), Ipv4([255, 255, 255, 0]), gateway);

    let ident = 0x5057u16;
    let daten = [0x10u8; 56];
    netz::icmp::antworten_leeren();

    // Bis zu 3 Runden versuchen (die erste löst ggf. erst per ARP die MAC
    // auf — dann geht das Echo zurückgestellt raus, sobald ARP antwortet).
    for sequenz in 0..3u16 {
        let start_us = zeit::us_seit_boot();
        netz::icmp::echo_senden(gateway, ident, sequenz, &daten).expect("Echo senden");
        let deadline = zeit::ms_seit_boot() + 2000;
        loop {
            netz::rx_verarbeiten();
            if let Some(ttl) = netz::icmp::antwort_empfangen(ident, sequenz) {
                let rtt = zeit::us_seit_boot() - start_us;
                serial_println!(
                    "[PING-MEILENSTEIN] Antwort von {} seq={} ttl={} zeit={}us — SpeedOS pingt.",
                    gateway,
                    sequenz,
                    ttl,
                    rtt
                );
                return;
            }
            if zeit::ms_seit_boot() >= deadline {
                break; // nächste Sequenz versuchen
            }
            x86_64::instructions::hlt();
        }
    }
    panic!("keine Echo-Antwort vom Gateway {} in 3 Versuchen", gateway);
}
