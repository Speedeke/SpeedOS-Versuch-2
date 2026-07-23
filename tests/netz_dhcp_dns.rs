// tests/netz_dhcp_dns.rs — Integrationstest: DHCP + DNS gegen QEMUs slirp
//
// Der „SpeedOS ist im Internet"-Meilenstein, ECHT: SpeedOS bezieht per DHCP
// eine IP von QEMUs eingebautem DHCP-Server und löst dann einen Namen über
// QEMUs DNS-Weiterleitung (10.0.2.3) auf. Beweist den ganzen neuen Stapel:
// UDP (Bau + Pseudo-Header-Prüfsumme), DHCP (DISCOVER/OFFER/REQUEST/ACK),
// DNS (A-Query bauen, komprimierte Antwort parsen) — alles über das echte
// virtio-net-Gerät.
//
// DHCP ist deterministisch (slirp antwortet immer). Die DNS-Auflösung braucht
// echten Internet-Zugang des HOSTS (slirp leitet an dessen Resolver weiter);
// gelingt sie, prüfen wir die Adresse hart, sonst wird nur eine Notiz
// geloggt (der DNS-PROTOKOLL-Teil ist ohnehin per Unit-Test abgedeckt).

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

/// DHCP beziehen und danach einen Namen auflösen — der volle Meilenstein.
#[test_case]
fn test_dhcp_dann_dns() {
    assert!(netz::vorhanden(), "keine virtio-net-NIC registriert");

    // --- DHCP (hart geprüft — slirp antwortet immer) ---
    let ergebnis = netz::dhcp::beziehen(4000).expect("DHCP: keine Lease bezogen");
    netz::konfig_setzen_dhcp(
        ergebnis.ip,
        ergebnis.maske,
        ergebnis.gateway,
        ergebnis.dns,
        ergebnis.lease_sekunden,
    );
    serial_println!(
        "[DHCP-MEILENSTEIN] IP {} / Maske {} / Gateway {} / DNS {} / Lease {} s",
        ergebnis.ip,
        ergebnis.maske,
        ergebnis.gateway,
        ergebnis.dns,
        ergebnis.lease_sekunden
    );
    // slirp vergibt aus 10.0.2.x mit Gateway 10.0.2.2 und DNS 10.0.2.3.
    assert_ne!(ergebnis.ip, Ipv4::NULL, "IP darf nicht 0.0.0.0 sein");
    assert_eq!(&ergebnis.ip.oktette()[0..3], &[10, 0, 2], "slirp-Subnetz 10.0.2.x");
    assert_eq!(ergebnis.maske, Ipv4([255, 255, 255, 0]));
    assert_eq!(ergebnis.gateway, Ipv4([10, 0, 2, 2]));
    assert_eq!(ergebnis.dns, Ipv4([10, 0, 2, 3]));

    // --- DNS (weich: braucht echten Internet-Zugang des Hosts) ---
    match netz::dns::aufloesen("example.com") {
        Ok(ip) => {
            assert_ne!(ip, Ipv4::NULL, "aufgeloeste IP darf nicht 0.0.0.0 sein");
            serial_println!("[DNS-MEILENSTEIN] example.com -> {} — SpeedOS ist im Internet.", ip);
        }
        Err(fehler) => {
            // Kein Internet im Test-Host: der DNS-Protokoll-Teil ist per
            // Unit-Test bewiesen; hier nur eine Notiz, kein Fehlschlag.
            serial_println!(
                "[DNS-HINWEIS] example.com nicht aufgeloest ({}). Host offline? \
                 (DNS-Parsing ist per Unit-Test abgedeckt.)",
                fehler.meldung()
            );
        }
    }
}
