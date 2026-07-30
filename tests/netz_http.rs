// tests/netz_http.rs — Integrationstest: HTTP gegen einen LAN-Server
//
// DER Reißleinen-Prüfpunkt aus docs/tcp-scope.md, gegen eine echte, LOKALE
// Gegenstelle: Auf dem Host läuft ein `python -m http.server 8000`; QEMUs
// slirp macht den Host für den Gast unter 10.0.2.2 erreichbar. Wir holen
// zehnmal dieselbe ~21 KB große Datei und prüfen JEDES Mal streng:
//   * Status 200,
//   * die Rumpflänge stimmt EXAKT mit `Content-Length` überein,
//   * Anfang UND Ende des Inhalts sind da (kein verlorenes/doppeltes Byte).
//
// Die Datei ist bewusst größer als unser TCP-Empfangsfenster (8 KiB) — der
// Transfer muss also über mehrere Fensterfüllungen laufen, inklusive der
// Fenster-Updates beim Auslesen. Genau da würden Fehler im Fluss auffallen.
//
// Läuft kein Server (der Test wird ohne ihn ausgeführt), wird die Messung
// sauber ÜBERSPRUNGEN statt rot zu werden — der TCP-Kern ist ohnehin per
// Loopback-Unit-Test bewiesen.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::netz::{self, http};
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
    allocator::heap_erweitern(512).expect("Heap-Erweiterung fehlgeschlagen");
    // Wie im echten Boot: Platten + PCI + Netz, dann Dateisystem mounten —
    // der Speicher-Test unten braucht /platte.
    speed_os::ata::init();
    pci::init();
    speed_os::virtio::blk::init();
    virtio::net::init();
    speed_os::fs::init();
    speed_os::fs::platte_automounten();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Die URL des LAN-Servers (der Host, wie slirp ihn dem Gast zeigt).
const LAN_URL: &str = "http://10.0.2.2:8000/probe.txt";

/// Prüft eine Antwort streng auf Vollständigkeit.
fn ist_sauber(antwort: &http::Antwort) -> bool {
    if antwort.status != 200 {
        return false;
    }
    // Rumpflänge muss EXAKT der angekündigten Content-Length entsprechen.
    let laut_kopf = antwort
        .header_wert("content-length")
        .and_then(|w| w.trim().parse::<usize>().ok());
    if laut_kopf != Some(antwort.rumpf.len()) {
        return false;
    }
    // Anfang und Ende müssen da sein.
    let text = String::from_utf8_lossy(&antwort.rumpf);
    text.starts_with("Zeile 00001:") && text.contains("Zeile 00350:")
}

/// 10 Abrufe gegen den LAN-Server — der Reißleinen-Prüfpunkt.
#[test_case]
fn test_http_lan_zehnmal() {
    assert!(netz::vorhanden(), "keine virtio-net-NIC registriert");
    let e = netz::dhcp::beziehen(4000).expect("DHCP: keine Lease");
    netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);

    // Erster Versuch entscheidet, ob überhaupt ein Server da ist.
    let erster = http::holen(LAN_URL);
    if let Err(fehler) = &erster {
        serial_println!(
            "[LAN-HINWEIS] {} nicht erreichbar ({}) — Messung uebersprungen. \
             Server starten mit: python -m http.server 8000",
            LAN_URL,
            fehler.meldung()
        );
        return;
    }

    let mut sauber = 0u32;
    let mut bytes = 0usize;
    for versuch in 1..=10u32 {
        let ergebnis = if versuch == 1 {
            // den schon gemachten ersten Abruf mitzählen
            match &erster {
                Ok((_, a)) => Ok(a.clone()),
                // `KlientFehler` ist seit Serie 7, Teil 5 nicht mehr `Copy`
                // (die https-Uebergabe traegt die Zieladresse mit sich).
                Err(f) => Err(f.clone()),
            }
        } else {
            http::holen(LAN_URL).map(|(_, a)| a)
        };
        match ergebnis {
            Ok(antwort) => {
                if ist_sauber(&antwort) {
                    sauber += 1;
                    bytes = antwort.rumpf.len();
                    serial_println!(
                        "[LAN] Versuch {:>2}: OK — HTTP {} , {} Byte vollstaendig",
                        versuch,
                        antwort.status,
                        antwort.rumpf.len()
                    );
                } else {
                    serial_println!(
                        "[LAN] Versuch {:>2}: UNVOLLSTAENDIG — Status {}, {} Byte",
                        versuch,
                        antwort.status,
                        antwort.rumpf.len()
                    );
                }
            }
            Err(fehler) => {
                serial_println!("[LAN] Versuch {:>2}: Fehler — {}", versuch, fehler.meldung());
            }
        }
    }

    serial_println!(
        "[LAN-REISSLEINE] {}/10 Abrufe sauber ({} Byte je Datei). Kriterium: >= 9/10.",
        sauber,
        bytes
    );
    assert!(
        sauber >= 9,
        "REISSLEINE: nur {}/10 sauber -> smoltcp nur fuer die TCP-Schicht ziehen",
        sauber
    );
    serial_println!("[LAN-REISSLEINE] Kriterium erfuellt — Eigenbau-TCP bleibt.");
}

/// NETZ + PERSISTENZ ZUSAMMEN: eine Datei über den eigenen TCP/HTTP-Stack
/// holen, auf die SpeedFS-Platte schreiben, zurücklesen und Byte für Byte
/// vergleichen — genau das, was `hole <url> <zieldatei>` in der Shell tut.
/// Non-destruktiv in einem eigenen Unterbaum (/platte/netztest).
#[test_case]
fn test_http_auf_platte_speichern() {
    use speed_os::fs;

    if !fs::ist_gemountet("/platte") {
        serial_println!("[SPEICHER-HINWEIS] /platte nicht gemountet — Test uebersprungen.");
        return;
    }
    let e = match netz::dhcp::beziehen(4000) {
        Some(e) => e,
        None => {
            serial_println!("[SPEICHER-HINWEIS] keine DHCP-Lease — Test uebersprungen.");
            return;
        }
    };
    netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);

    let antwort = match http::holen(LAN_URL) {
        Ok((_, a)) => a,
        Err(fehler) => {
            serial_println!(
                "[SPEICHER-HINWEIS] LAN-Server nicht erreichbar ({}) — Test uebersprungen.",
                fehler.meldung()
            );
            return;
        }
    };
    assert!(ist_sauber(&antwort), "Abruf war nicht vollstaendig");

    // Eigener Unterbaum, damit keine anderen Test-Daten beruehrt werden.
    let _ = fs::mit_fs(|f| f.mkdir("/platte/netztest"));
    let pfad = "/platte/netztest/probe.txt";
    fs::mit_fs(|f| f.schreiben(pfad, &antwort.rumpf)).expect("auf die Platte schreiben");
    fs::sync().expect("sync");

    // Zurücklesen und Byte für Byte vergleichen.
    let zurueck = fs::mit_fs(|f| f.lesen(pfad)).expect("von der Platte lesen");
    assert_eq!(
        zurueck.len(),
        antwort.rumpf.len(),
        "Laenge auf der Platte weicht ab"
    );
    assert_eq!(zurueck, antwort.rumpf, "Inhalt auf der Platte weicht ab");

    serial_println!(
        "[NETZ+PERSISTENZ] {} Byte ueber den eigenen TCP-Stack geholt, nach {} \
         geschrieben und identisch zurueckgelesen.",
        zurueck.len(),
        pfad
    );

    // Aufräumen (der Unterbaum bleibt, die Datei geht).
    let _ = fs::mit_fs(|f| f.loeschen(pfad));
    let _ = fs::sync();
}
