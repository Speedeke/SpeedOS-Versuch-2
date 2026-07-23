// tests/netz_tcp.rs — Integrationstest: echtes TCP/HTTP über QEMUs slirp
//
// Dies ist zugleich die MESSUNG für die Reißleine (docs/tcp-scope.md): eine
// HTTP/1.0-Anfrage gegen einen echten Server muss in >= 9 von 10 Versuchen
// SAUBER laden — Handshake, vollständige Antwort (Status + Header + Rumpf),
// sauberer Close. Wird das nicht erreicht, ist die Reißleine (smoltcp nur für
// die TCP-Schicht) zu ziehen.
//
// slirp NAT-et ausgehendes TCP ins echte Internet. Der Test braucht also
// Host-Internet; ist keins da (DNS-Auflösung schlägt fehl), wird die Messung
// ÜBERSPRUNGEN (der TCP-Kern ist per Unit-Test — inkl. Loopback mit
// Paketverlust — bereits bewiesen), damit der Test offline nicht rot wird.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::netz;
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

/// Die Reißleinen-Messung: 10 HTTP-Abrufe, zähle die sauberen.
#[test_case]
fn test_http_reissleine() {
    assert!(netz::vorhanden(), "keine virtio-net-NIC registriert");

    // IP per DHCP (deterministisch über slirp).
    let e = netz::dhcp::beziehen(4000).expect("DHCP: keine Lease");
    netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);

    // Host auflösen. Kein Internet? -> Messung überspringen (Unit-Tests decken
    // den TCP-Kern ab).
    let ziel = match netz::dns::aufloesen("example.com") {
        Ok(ip) => ip,
        Err(fehler) => {
            serial_println!(
                "[TCP-HINWEIS] example.com nicht aufloesbar ({}) — HTTP-Messung uebersprungen \
                 (TCP-Kern per Loopback-Unit-Test bewiesen).",
                fehler.meldung()
            );
            return;
        }
    };

    // Über die Socket-API + den HTTP-Client (die neue öffentliche Fassade).
    let _ = ziel; // die Auflösung macht der HTTP-Client selbst
    let mut sauber = 0u32;
    for versuch in 1..=10 {
        match netz::http::holen("http://example.com/") {
            Ok((_url, antwort)) => {
                // "Sauber" = 2xx-Status UND vollständiger Rumpf (antwort_parsen
                // prüft Content-Length bzw. den chunked-Abschluss).
                if (200..300).contains(&antwort.status) && !antwort.rumpf.is_empty() {
                    sauber += 1;
                    serial_println!(
                        "[TCP] Versuch {:>2}: OK — HTTP {} {}, {} Byte Rumpf",
                        versuch,
                        antwort.status,
                        antwort.grund,
                        antwort.rumpf.len()
                    );
                } else {
                    serial_println!(
                        "[TCP] Versuch {:>2}: Status {} / {} Byte Rumpf",
                        versuch,
                        antwort.status,
                        antwort.rumpf.len()
                    );
                }
            }
            Err(fehler) => {
                serial_println!("[TCP] Versuch {:>2}: Fehler — {}", versuch, fehler.meldung());
            }
        }
    }

    serial_println!(
        "[TCP-REISSLEINE] {}/10 HTTP-Abrufe sauber. Kriterium (docs/tcp-scope.md): >= 9/10.",
        sauber
    );
    assert!(
        sauber >= 9,
        "REISSLEINE: nur {}/10 sauber -> smoltcp nur fuer die TCP-Schicht ziehen",
        sauber
    );
    serial_println!("[TCP-REISSLEINE] Kriterium erfuellt — Eigenbau-TCP bleibt.");
}
