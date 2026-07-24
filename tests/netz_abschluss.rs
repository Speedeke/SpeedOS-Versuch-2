// tests/netz_abschluss.rs — Serie-5-Abschluss: Speicher, Robustheit, Leistung
//
// Drei Härteprüfungen, die den Netz-Stack für Serie 6 abnehmen:
//   TEIL 2  Speicher-Stabilität: viele hole/nslookup/ping-Zyklen — der Heap
//           darf NICHT wachsen, keine Sockets/DMA-Puffer/ARP-Einträge lecken.
//   TEIL 3  Robustheit: Kabel weg (Link down), Server stumm, DNS-Server tot,
//           Gateway-MAC wechselt — nichts davon darf HÄNGEN oder PANICKEN,
//           alles saubere Fehler in begrenzter Zeit.
//   TEIL 4  Leistung: Durchsatz von hole gegen den LAN-Server (MiB/s) und die
//           RTT-Verteilung von ping — ehrlich, inkl. wo/warum langsam.
//
// Braucht Host-Internet und (fuer Teil 2/4) einen LAN-Server:
//   python -m http.server 8000   (mit probe.txt ~21 KB, probe_big.txt ~512 KB)
// Fehlt etwas, wird der jeweilige Teil sauber UEBERSPRUNGEN.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::netz::{self, http, Ipv4};
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
    pci::init();
    virtio::net::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

const LAN_KLEIN: &str = "http://10.0.2.2:8000/probe.txt";
const LAN_GROSS: &str = "http://10.0.2.2:8000/probe_big.txt";

/// DHCP-Setup + Rückgabe des Gateways.
fn netz_auf() -> Ipv4 {
    netz::geraet::verlust_setzen(0);
    let e = netz::dhcp::beziehen(4000).expect("DHCP: keine Lease");
    netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);
    e.gateway
}

/// Pumpt den Stack für `ms` Millisekunden (verarbeitet Empfang + Timer).
fn pumpen_fuer(ms: u64) {
    let bis = zeit::ms_seit_boot() + ms;
    while zeit::ms_seit_boot() < bis {
        netz::pumpen();
        x86_64::instructions::hlt();
    }
}

/// Ein ICMP-Echo an `ziel` senden und bis `timeout_ms` auf die Antwort
/// warten. Liefert die RTT in Mikrosekunden, None bei Timeout.
fn ping_einmal(ziel: Ipv4, seq: u16, timeout_ms: u64) -> Option<u64> {
    netz::icmp::antworten_leeren();
    let start = zeit::us_seit_boot();
    netz::icmp::echo_senden(ziel, 0x4242, seq, &[0u8; 32]).ok()?;
    let bis = zeit::ms_seit_boot() + timeout_ms;
    loop {
        netz::pumpen();
        if netz::icmp::antwort_empfangen(0x4242, seq).is_some() {
            return Some(zeit::us_seit_boot() - start);
        }
        if zeit::ms_seit_boot() >= bis {
            return None;
        }
        x86_64::instructions::hlt();
    }
}

// ---------------------------------------------------------------------------
// TEIL 2 — Speicher-Stabilität
// ---------------------------------------------------------------------------

/// Viele Netz-Zyklen dürfen den Heap NICHT wachsen lassen und keine Sockets/
/// DMA-Puffer/ARP-Einträge lecken.
#[test_case]
fn test_speicher_stabil() {
    let gateway = netz_auf();
    let lan_da = http::holen(LAN_KLEIN).is_ok();

    // Warmlaufen: Caches (DNS, ARP) füllen sich EINMALIG — das ist kein Leck.
    let _ = ping_einmal(gateway, 0, 2000);
    if lan_da {
        let _ = http::holen(LAN_KLEIN);
    }
    // TIME_WAIT-Sockets abklingen lassen.
    pumpen_fuer(2500);

    // Basiswerte NACH dem Warmlaufen.
    let (belegt_basis, _) = allocator::heap_statistik().expect("Heap-Statistik");
    let (frames_basis, _) = memory::frame_statistik();
    let sockets_basis = netz::socket::anzahl();
    serial_println!(
        "[SPEICHER] Basis: Heap belegt {} B, Frames frei {}, Sockets {}",
        belegt_basis,
        frames_basis,
        sockets_basis
    );

    // Die Arbeitslast: 150 Zyklen. Jeder Zyklus fasst alle Leck-Flächen an —
    // Socket-Tabelle (UDP auf/zu), Frame-Vecs (ping), und alle 15 Runden ein
    // vollständiger TCP-Abruf (Ringpuffer + TIME_WAIT-Abbau).
    const ZYKLEN: u32 = 150;
    for i in 0..ZYKLEN {
        // (a) UDP-Socket auf und zu — Handle-Tabelle + Empfangs-Vec.
        let h = netz::socket::oeffnen(netz::socket::SocketTyp::Udp).expect("oeffnen");
        let _ = netz::socket::schliessen(h);
        // (b) ein Ping — RX/TX-Frame-Vecs, ARP-Cache-Zugriff.
        let _ = ping_einmal(gateway, (i & 0xffff) as u16, 1500);
        // (c) alle 15 Runden ein TCP-Abruf.
        if lan_da && i % 15 == 0 {
            let _ = http::holen(LAN_KLEIN);
        }
        if i % 50 == 0 {
            netz::socket::bedienen();
        }
    }
    // TIME_WAIT vollständig abklingen lassen.
    pumpen_fuer(3000);
    netz::socket::bedienen();

    let (belegt_ende, _) = allocator::heap_statistik().unwrap();
    let (frames_ende, _) = memory::frame_statistik();
    let sockets_ende = netz::socket::anzahl();
    let arp_eintraege = netz::arp::cache_eintraege().len();
    let wachstum = belegt_ende as i64 - belegt_basis as i64;
    serial_println!(
        "[SPEICHER] Ende:  Heap belegt {} B (Wachstum {} B), Frames frei {}, Sockets {}, ARP {}",
        belegt_ende,
        wachstum,
        frames_ende,
        sockets_ende,
        arp_eintraege
    );

    // KEINE geleakten Sockets (TIME_WAIT ist abgeraeumt).
    assert_eq!(sockets_ende, 0, "geleakte Sockets: {}", sockets_ende);
    // KEINE geleakten DMA-Puffer/Pages: der Frame-Allocator muss EXAKT stabil
    // sein (hole/ping/nslookup allozieren nach dem Setup keine Pages mehr).
    assert_eq!(frames_ende, frames_basis, "Frame-Leck: {} -> {}", frames_basis, frames_ende);
    // ARP-Cache bleibt klein (Gateway + evtl. DNS-Server).
    assert!(arp_eintraege <= 8, "ARP-Cache waechst unerwartet: {}", arp_eintraege);
    // Der Heap darf nach dem Warmlaufen nicht mehr nennenswert wachsen
    // (ein Leck von >~50 B/Zyklus waere bei 150 Zyklen deutlich sichtbar).
    assert!(
        wachstum < 8 * 1024,
        "Heap-Leck: {} B ueber {} Zyklen",
        wachstum,
        ZYKLEN
    );
}

// ---------------------------------------------------------------------------
// TEIL 3 — Robustheit
// ---------------------------------------------------------------------------

/// KABEL WEG (Link down = 100 % Paketverlust): DNS, Ping und HTTP müssen
/// SAUBER fehlschlagen (in begrenzter Zeit), nichts hängt/panickt — und nach
/// dem „Einstecken" läuft alles wieder.
#[test_case]
fn test_robust_kabel_weg() {
    let gateway = netz_auf();
    let lan_da = http::holen(LAN_KLEIN).is_ok();

    // Kabel ziehen.
    netz::geraet::verlust_setzen(100);
    serial_println!("[ROBUST] Kabel weg (100 % Verlust).");

    // Ping: keine Antwort, aber begrenzt (kein Haenger).
    let t0 = zeit::ms_seit_boot();
    assert!(ping_einmal(gateway, 1, 1500).is_none(), "Ping trotz Link down?");
    assert!(zeit::ms_seit_boot() - t0 < 2500, "Ping-Timeout zu lang");

    // DNS: ein NICHT gecachter Name -> Timeout (Retry laeuft), sauberer Fehler.
    let t0 = zeit::ms_seit_boot();
    let r = netz::dns::aufloesen("kabel-weg.speedos.test");
    assert!(r.is_err(), "DNS lieferte trotz Link down eine Antwort?");
    assert!(zeit::ms_seit_boot() - t0 < 6000, "DNS-Timeout zu lang");

    // HTTP: sauberer Fehler, kein Absturz.
    let r = http::holen(LAN_GROSS);
    assert!(r.is_err(), "HTTP lieferte trotz Link down eine Antwort?");

    // Kabel wieder einstecken -> Erholung.
    netz::geraet::verlust_setzen(0);
    pumpen_fuer(300);
    if lan_da {
        assert!(http::holen(LAN_KLEIN).is_ok(), "keine Erholung nach Link-up");
        serial_println!("[ROBUST] Nach Link-up sofort wieder erreichbar.");
    }
    // Der Stack ist danach leer (keine haengenden Sockets).
    pumpen_fuer(2500);
    assert_eq!(netz::socket::anzahl(), 0, "haengende Sockets nach Link down");
}

/// SERVER STUMM: Verbindung auf einen Port, an dem niemand horcht — sauberer
/// Fehler (RST oder Timeout), kein Haenger.
#[test_case]
fn test_robust_server_stumm() {
    netz_auf();
    let t0 = zeit::ms_seit_boot();
    let r = http::holen("http://10.0.2.2:1/nichts");
    let dauer = zeit::ms_seit_boot() - t0;
    serial_println!("[ROBUST] Server-stumm-Abruf: {:?} nach {} ms", r.is_err(), dauer);
    assert!(r.is_err(), "ein toter Port lieferte eine Antwort?");
    assert!(dauer < 16_000, "Timeout zu lang ({} ms)", dauer);
    pumpen_fuer(3000); // TIME_WAIT/Abbau abklingen lassen
    assert_eq!(netz::socket::anzahl(), 0, "Socket-Leck nach RST/Timeout");
}

/// DNS-SERVER TOT: DNS auf eine unerreichbare IP zeigen -> Auflösung schlägt
/// SAUBER fehl (Timeout), hängt nicht.
#[test_case]
fn test_robust_dns_tot() {
    let _ = netz_auf();
    let k = netz::konfig();
    // DNS auf eine tote, aber im Subnetz liegende IP zeigen (ARP bleibt aus).
    netz::konfig_setzen_dhcp(k.ip, k.maske, k.gateway, Ipv4([10, 0, 2, 99]), k.lease_sekunden);

    let t0 = zeit::ms_seit_boot();
    let r = netz::dns::aufloesen("toter-dns.speedos.test");
    let dauer = zeit::ms_seit_boot() - t0;
    serial_println!("[ROBUST] Toter DNS: {:?} nach {} ms", r.err(), dauer);
    assert!(
        matches!(r, Err(netz::dns::DnsFehler::Zeitueberschreitung)),
        "erwartet: DNS-Timeout, war: {:?}",
        r
    );
    assert!(dauer < 6000, "DNS-Timeout zu lang ({} ms)", dauer);

    // Konfiguration wiederherstellen (frisches DHCP).
    netz_auf();
}

/// GATEWAY-MAC WECHSELT: Kündigt das Gateway (per ARP) eine neue MAC an, muss
/// unser ARP-Cache SIE übernehmen — sonst würde Verkehr an die alte MAC ins
/// Leere laufen. (Slirp ändert seine MAC nicht, also spielen wir die
/// ARP-Ankündigung selbst ein — die Cache-LOGIK ist das Prüfziel.)
#[test_case]
fn test_robust_gateway_mac_wechsel() {
    let gateway = netz_auf();
    let unsere_mac = netz::mac().expect("MAC");
    let unsere_ip = netz::konfig().ip;

    // Erst die echte Gateway-MAC lernen (per Ping).
    let _ = ping_einmal(gateway, 5, 2000);
    let echte_mac = netz::arp::cache_suchen(gateway).expect("Gateway-MAC gelernt");
    serial_println!("[ROBUST] echte Gateway-MAC {:02x?}", echte_mac);

    // Das Gateway „kuendigt" eine neue MAC an (gratuitous ARP reply).
    let neue_mac = [0x02, 0xAB, 0xCD, 0xEF, 0x00, 0x99];
    let ankuendigung = netz::arp::ArpPaket {
        operation: netz::arp::OP_REPLY,
        absender_mac: neue_mac,
        absender_ip: gateway,
        ziel_mac: unsere_mac,
        ziel_ip: unsere_ip,
    };
    netz::arp::verarbeiten(&ankuendigung.bauen());
    assert_eq!(
        netz::arp::cache_suchen(gateway),
        Some(neue_mac),
        "ARP-Cache hat die neue MAC nicht uebernommen"
    );

    // Und wieder zurueck auf die echte MAC (naechster MAC-Wechsel) — so
    // bleibt der Cache korrekt fuer die folgenden Tests.
    let zurueck = netz::arp::ArpPaket {
        operation: netz::arp::OP_REPLY,
        absender_mac: echte_mac,
        absender_ip: gateway,
        ziel_mac: unsere_mac,
        ziel_ip: unsere_ip,
    };
    netz::arp::verarbeiten(&zurueck.bauen());
    assert_eq!(netz::arp::cache_suchen(gateway), Some(echte_mac), "Ruecknahme klappt nicht");
    serial_println!("[ROBUST] MAC-Wechsel wird vom Cache uebernommen (beide Richtungen).");
}

// ---------------------------------------------------------------------------
// TEIL 4 — Leistung (ehrlich)
// ---------------------------------------------------------------------------

/// Durchsatz von hole gegen den LAN-Server (MiB/s) + RTT-Verteilung von ping.
/// KEIN Fix — nur ehrliche Transparenz, wo/warum unser Stack langsam ist.
#[test_case]
fn test_performance() {
    let gateway = netz_auf();

    // --- RTT-Verteilung: 20 Pings ans Gateway ---
    let mut rtts = alloc::vec::Vec::new();
    for seq in 0..20u16 {
        if let Some(us) = ping_einmal(gateway, 100 + seq, 1500) {
            rtts.push(us);
        }
        pumpen_fuer(20);
    }
    if !rtts.is_empty() {
        let min = *rtts.iter().min().unwrap();
        let max = *rtts.iter().max().unwrap();
        let schnitt = rtts.iter().sum::<u64>() / rtts.len() as u64;
        serial_println!(
            "[LEISTUNG] Ping-RTT ueber {} Antworten: min {} us, schnitt {} us, max {} us \
             (der Hoechstwert ist der erste Ping inkl. ARP-Aufloesung)",
            rtts.len(),
            min,
            schnitt,
            max
        );
    }

    // --- Durchsatz: die grosse Datei holen und die Zeit messen ---
    let (url, bytes_soll) = if let Ok((_, a)) = http::holen(LAN_GROSS) {
        (LAN_GROSS, a.rumpf.len())
    } else if let Ok((_, a)) = http::holen(LAN_KLEIN) {
        (LAN_KLEIN, a.rumpf.len())
    } else {
        serial_println!("[LEISTUNG] LAN-Server nicht erreichbar — Durchsatz uebersprungen.");
        return;
    };
    // Drei Messungen, die beste zaehlt (Warmlauf-Effekte raus).
    let mut beste_us = u64::MAX;
    let mut bytes = 0usize;
    for _ in 0..3 {
        let start = zeit::us_seit_boot();
        let (_, a) = http::holen(url).expect("Durchsatz-Abruf");
        let us = zeit::us_seit_boot() - start;
        bytes = a.rumpf.len();
        beste_us = beste_us.min(us);
    }
    // MiB/s in Hundertsteln (kein Fliesskomma im Kernel).
    let hundertstel = if beste_us > 0 {
        (bytes as u64) * 1_000_000 * 100 / (beste_us * 1024 * 1024)
    } else {
        0
    };
    serial_println!(
        "[LEISTUNG] Durchsatz {} Byte in {} us = {},{:02} MiB/s (beste von 3).",
        bytes,
        beste_us,
        hundertstel / 100,
        hundertstel % 100
    );
    serial_println!(
        "[LEISTUNG] EHRLICH: langsam durch (1) 8-KiB-Fenster ohne Scaling — hoechstens \
         8 KiB pro RTT unterwegs, (2) synchrones Pumpen pro Segment (ein VM-Exit je \
         Notify), (3) kein Fast-Retransmit. Fuer LAN/Lernzweck voellig ausreichend."
    );
    let _ = bytes_soll;
    assert!(bytes > 0, "kein Durchsatz gemessen");
}
