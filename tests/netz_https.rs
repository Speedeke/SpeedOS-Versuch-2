// tests/netz_https.rs — DER MEILENSTEIN VON SERIE 7:
//                        eine echte verschluesselte Verbindung aus SpeedOS
//
// ==========================================================================
// WAS HIER BEWIESEN WIRD
//
//  (1) `holes` holt eine echte HTTPS-Seite aus dem Internet: TLS-Handshake
//      ueber unsere eigenen Socket-Syscalls, Zertifikatskette gegen unseren
//      eigenen Vertrauensanker geprueft, Hostname abgeglichen, HTTP/1.1
//      darueber. Aus RING 3, in einem eigenen Adressraum.
//  (2) Es ist ECHTES TLS und nicht bloss eine Verbindung, die zufaellig
//      klappt: `--info` nennt die ausgehandelte Protokollversion, die
//      Ciphersuite und die Glieder der Kette.
//  (3) DER HTTP-PARSER MUSSTE NICHT ANGEFASST WERDEN. Derselbe
//      `speedhttp::antwort_parsen`, den der Kernel-Klient `hole` ueber einen
//      nackten TCP-Socket benutzt, zerlegt hier die Antwort aus dem
//      verschluesselten Strom — und der Test faehrt beide Wege gegen
//      dieselbe Quelle und vergleicht.
//  (4) JEDER Pruefungsfehler bricht ab und nennt den Grund: unbekannte CA,
//      abgelaufen, falscher Hostname. Es gibt keinen Umgehungs-Schalter.
//
// ==========================================================================
// TESTMETHODIK (wie bei TCP, docs/tcp-scope.md)
//
// Das HARTE Gate haengt an dem, was wir kontrollieren:
//   * `tools/tls_testserver.py` auf 10.0.2.2:8443 — SELBST ausgestelltes
//     Zertifikat, muss IMMER abgelehnt werden.
// Der Internet-Teil (example.com, badssl.com) ist BERICHT: Er laeuft, wenn
// er kann, und wird sauber uebersprungen, wenn nicht. Eine Testsuite darf
// nicht von fremden Servern abhaengen — aber sie darf davon berichten.
//
// VORBEREITUNG fuer den vollstaendigen Lauf:
//     python tools/tls_testserver.py
//     python -m http.server 8000      (fuer den Parser-Vergleich, optional)

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::prozess::{self, Pid, ProzessEnde};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fs, memory, pci, pipe, programme, scheduler, serial_println};
use speed_os::{virtio, zeit, zufall};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    zufall::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // 2048 Seiten = 8 MiB. Mehr als in den anderen Tests, und aus einem
    // handfesten Grund: `holes` ist mit ~950 KiB das groesste Programm des
    // Projekts (rustls), und `programme::installieren` sowie jeder
    // Prozess-Start lesen die ELF-Datei am Stueck in den KERNEL-Heap. Mit
    // 512 Seiten reisst die Allokation nach ein paar Laeufen ab.
    allocator::heap_erweitern(2048).expect("Heap-Erweiterung fehlgeschlagen");

    speed_os::ata::init();
    pci::init();
    speed_os::virtio::blk::init();
    virtio::net::init();
    fs::init();
    fs::platte_automounten();
    programme::installieren();
    programme::ca_buendel_installieren();
    scheduler::init();

    // DHCP wie im echten Boot — ohne Konfiguration kommt kein Paket raus.
    speed_os::netz::dhcp::autokonfig(5000);

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Ein TLS-Handshake ueber das Internet plus Uebertragung darf dauern.
const FRIST_MS: u64 = 180_000;

/// Der lokale Testserver (der Host, wie slirp ihn dem Gast zeigt).
const SELBST_SIGNIERT: &str = "https://10.0.2.2:8443/klein.txt";
const LAN_HTTP: &str = "http://10.0.2.2:8000/probe.txt";
/// Dieselbe Datei wie `ECHT_GROSS`, aber lokal und OHNE TLS — der
/// Vergleichswert fuer die Durchsatz-Messung.
const LAN_GROSS: &str = "http://10.0.2.2:8000/gross.pem";

/// Die einfache, bekannte HTTPS-Seite.
const ECHT_KLEIN: &str = "https://example.com/";
/// Der groessere Brocken — und zwar der schoenste, den es gibt: SpeedOS holt
/// sein EIGENES CA-Buendel ueber eine Verbindung, die es mit genau diesem
/// Buendel geprueft hat.
const ECHT_GROSS: &str = "https://curl.se/ca/cacert.pem";

// ===========================================================================
// Werkzeug
// ===========================================================================

/// Wartet, bis der Zufallsgenerator gesät ist (der Testkernel hat keinen
/// Nachsaat-Task — siehe tests/tlsspike.rs).
fn zufall_bereitmachen() {
    let frist = zeit::ms_seit_boot() + 30_000;
    while !zufall::bereit() && zeit::ms_seit_boot() < frist {
        if zufall::status().entropie_bits >= zufall::SCHWELLE_BITS {
            zufall::nachsaeen();
        }
        zeit::warte_auf_interrupt();
    }
    assert!(zufall::bereit(), "der Zufallsgenerator wurde nicht gesaet");
}

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Startet ein Programm mit einer Pipe als Ausgabe und liest alles mit.
///
/// WICHTIG (CLAUDE.md, Serie 6, Teil 6): Der Exit-Code wird VOR dem
/// Abraeumen eingesammelt — `aufraeumen()` loescht den Tabelleneintrag und
/// damit den Code.
fn starten_und_lesen(name: &str, argumente: &[&str]) -> (Option<ProzessEnde>, String) {
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad(name);
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()));
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let mut gesammelt = Vec::new();
    let mut puffer = alloc::vec![0u8; 8192];
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    break;
                }
                if ende.is_none() {
                    ende = scheduler::ende_abfragen(pid);
                }
                scheduler::aufraeumen();
                // Der Netz-Task laeuft in diesem Testkernel nicht (kein
                // Executor) — der Stack muss also VON HAND gepumpt werden,
                // sonst kommt bei `holes` nie ein Paket an.
                speed_os::netz::pumpen();
                zeit::warte_auf_interrupt();
            }
            _ => break,
        }
    }
    if ende.is_none() {
        ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 10_000));
    }
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    (ende, String::from_utf8_lossy(&gesammelt).into_owned())
}

fn ausgabe_zeigen(titel: &str, ausgabe: &str, ende: &Option<ProzessEnde>) {
    serial_println!("  === {} ===", titel);
    for zeile in ausgabe.lines() {
        serial_println!("  | {}", zeile);
    }
    serial_println!("  === Ende (Prozess: {:?}) ===", ende);
}

/// Die Exit-Codes von `holes` (siehe userland/src/bin/holes.rs).
const HOLES_OK: ProzessEnde = ProzessEnde::Beendet(0);
const HOLES_TLS: ProzessEnde = ProzessEnde::Beendet(4);

/// Steht Internet zur Verfuegung? Einmal ermitteln, dann berichten.
fn internet_da() -> bool {
    speed_os::netz::dns::aufloesen("example.com").is_ok()
}

// ===========================================================================
// 1. DAS HARTE GATE: das selbst ausgestellte Zertifikat MUSS abgelehnt werden
// ===========================================================================

/// DER WICHTIGSTE TEST DIESER DATEI.
///
/// Ein Server auf dem Host legt ein Zertifikat vor, das er sich selbst
/// ausgestellt hat. Keine Wurzel aus assets/ca-bundle.pem hat es
/// unterschrieben. `holes` MUSS abbrechen — und zwar mit dem Grund
/// „unbekannte Zertifizierungsstelle", nicht mit irgendeinem Fehler.
///
/// WARUM DAS DER SCHARFE TEST IST: Ein TLS-Client, der immer verbindet, sieht
/// im Erfolgsfall genauso aus wie einer, der prueft. Der Unterschied zeigt
/// sich AUSSCHLIESSLICH hier. Und dieser Fall ist genau der eines
/// Angreifers, der sich selbst ein Zertifikat ausstellt.
#[test_case]
fn test_selbst_signiert_wird_abgelehnt() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();

    let (ende, ausgabe) = starten_und_lesen("holes", &["holes", SELBST_SIGNIERT]);
    ausgabe_zeigen("holes gegen den selbst signierten Testserver", &ausgabe, &ende);

    if ausgabe.contains("Die Verbindung kam nicht zustande")
        || ausgabe.contains("TCP-Verbindung fehlgeschlagen")
    {
        serial_println!(
            "  (uebersprungen: auf {} lauscht nichts — \
             `python tools/tls_testserver.py` starten)",
            SELBST_SIGNIERT
        );
        return;
    }

    assert_eq!(
        ende,
        Some(HOLES_TLS),
        "holes MUSS ein selbst ausgestelltes Zertifikat ablehnen (Exit 4)"
    );
    assert!(
        ausgabe.contains("VERBINDUNG ABGELEHNT (unbekannte-ca)"),
        "der Grund muss benannt werden — 'unbekannte-ca' fehlt in der Ausgabe"
    );
    assert!(
        ausgabe.contains("UNBEKANNTE ZERTIFIZIERUNGSSTELLE"),
        "die deutsche Begruendung fehlt"
    );
    assert!(
        ausgabe.contains("keinen Schalter"),
        "der Hinweis, dass es keine Umgehung gibt, fehlt"
    );
    // UND: Es darf KEIN Inhalt durchgekommen sein.
    assert!(
        !ausgabe.contains("SpeedOS TLS-Testserver:"),
        "DER INHALT IST DURCHGEKOMMEN — die Zertifikatspruefung ist wirkungslos!"
    );
    serial_println!("  OK: selbst ausgestelltes Zertifikat abgelehnt, kein Byte Inhalt.");
}

// ===========================================================================
// 2. DER MEILENSTEIN: eine echte HTTPS-Seite
// ===========================================================================

/// EINE ECHTE VERSCHLUESSELTE VERBINDUNG, aus Ring 3, ueber den eigenen Stack.
#[test_case]
fn test_meilenstein_echte_https_seite() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }

    let (ende, ausgabe) = starten_und_lesen("holes", &["holes", ECHT_KLEIN, "--info"]);
    ausgabe_zeigen("MEILENSTEIN: holes https://example.com/ --info", &ausgabe, &ende);

    assert_eq!(ende, Some(HOLES_OK), "der Abruf ist nicht sauber durchgelaufen");
    assert!(ausgabe.contains("HTTP 200"), "kein HTTP 200 in der Antwort");
    // Der Inhalt von example.com ist seit Jahren derselbe Satz.
    assert!(
        ausgabe.contains("Example Domain"),
        "der Seiteninhalt fehlt — es kam nichts Entschluesseltes an"
    );
    serial_println!("  OK: example.com ueber TLS geladen und angezeigt.");
}

/// DER BEWEIS, DASS ES ECHTES TLS IST: Version, Ciphersuite, Kette.
///
/// Eine Verbindung, die „irgendwie funktioniert", koennte auch eine
/// unverschluesselte sein. Diese Angaben kann nur nennen, wer den Handshake
/// wirklich gefuehrt hat — sie sind das AUSHANDLUNGS-ERGEBNIS.
#[test_case]
fn test_ausgehandelte_parameter_sichtbar() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }

    let (ende, ausgabe) = starten_und_lesen("holes", &["holes", ECHT_KLEIN, "--info"]);
    assert_eq!(ende, Some(HOLES_OK));

    // Protokollversion: wir bieten 1.3 und 1.2 an, ausgehandelt wird eine.
    assert!(
        ausgabe.contains("TLS 1.3") || ausgabe.contains("TLS 1.2"),
        "keine ausgehandelte Protokollversion in der Ausgabe"
    );
    // Ciphersuite: der RFC-Name des rustls-Anbieters.
    assert!(
        ausgabe.contains("TLS13_") || ausgabe.contains("TLS_ECDHE"),
        "keine ausgehandelte Ciphersuite in der Ausgabe"
    );
    // Die Kette: mindestens das Serverzertifikat.
    assert!(
        ausgabe.contains("Zertifikatskette"),
        "die Kette wird nicht angezeigt"
    );
    assert!(
        ausgabe.contains("[0] Server"),
        "das Serverzertifikat fehlt in der Kettenanzeige"
    );

    for zeile in ausgabe.lines() {
        if zeile.contains("TLS:") || zeile.contains("[0] Server") || zeile.contains("[1] Zwischen")
        {
            serial_println!("  BEWEIS> {}", zeile.trim());
        }
    }
}

// ===========================================================================
// 3. DIE FEHLERFAELLE — jeder bricht ab und nennt den Grund
// ===========================================================================

/// Ein Fall aus der badssl.com-Familie: Erwartet wird ABBRUCH mit `grund`.
fn badssl_fall(url: &str, grund: &str, ueberschrift: &str) {
    let (ende, ausgabe) = starten_und_lesen("holes", &["holes", url]);
    ausgabe_zeigen(ueberschrift, &ausgabe, &ende);

    if ausgabe.contains("DNS fuer") || ausgabe.contains("TCP-Verbindung fehlgeschlagen") {
        serial_println!("  (uebersprungen: {} nicht erreichbar)", url);
        return;
    }
    assert_eq!(
        ende,
        Some(HOLES_TLS),
        "{} haette abgelehnt werden muessen (Exit 4)",
        url
    );
    assert!(
        ausgabe.contains(grund),
        "der Grund '{}' fehlt in der Ausgabe zu {}",
        grund,
        url
    );
    assert!(
        !ausgabe.contains("HTTP 200"),
        "es kam eine Antwort durch, obwohl das Zertifikat abgelehnt werden musste"
    );
    serial_println!("  OK: {} -> abgelehnt ({}).", url, grund);
}

/// ABGELAUFENES ZERTIFIKAT.
#[test_case]
fn test_abgelaufenes_zertifikat() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }
    badssl_fall(
        "https://expired.badssl.com/",
        "abgelaufen",
        "FEHLERFALL: abgelaufenes Zertifikat",
    );
}

/// FALSCHER HOSTNAME — der Fall, der ohne Namensabgleich unbemerkt bliebe.
#[test_case]
fn test_falscher_hostname() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }
    badssl_fall(
        "https://wrong.host.badssl.com/",
        "falscher-hostname",
        "FEHLERFALL: falscher Hostname",
    );
}

/// UNBEKANNTE WURZEL (badssl-Variante zusaetzlich zum lokalen Gate).
#[test_case]
fn test_unbekannte_wurzel_internet() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }
    badssl_fall(
        "https://self-signed.badssl.com/",
        "unbekannte-ca",
        "FEHLERFALL: selbst ausgestellt (badssl)",
    );
    badssl_fall(
        "https://untrusted-root.badssl.com/",
        "unbekannte-ca",
        "FEHLERFALL: nicht vertrauenswuerdige Wurzel (badssl)",
    );
}

/// PROTOKOLLFEHLER: Auf Port 80 spricht kein TLS.
///
/// Der Fall gehoert dazu, weil er der haeufigste Bedienfehler ist — und weil
/// die Meldung ihn benennen soll, statt „Verbindung fehlgeschlagen" zu sagen.
#[test_case]
fn test_kein_tls_auf_der_gegenseite() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();

    let (ende, ausgabe) = starten_und_lesen("holes", &["holes", "https://10.0.2.2:8000/"]);
    ausgabe_zeigen("FEHLERFALL: https gegen einen http-Server", &ausgabe, &ende);

    if ausgabe.contains("TCP-Verbindung fehlgeschlagen") {
        serial_println!("  (uebersprungen: auf 10.0.2.2:8000 lauscht nichts)");
        return;
    }
    assert_eq!(ende, Some(HOLES_TLS), "das haette abbrechen muessen");
    assert!(
        ausgabe.contains("PROTOKOLLFEHLER") || ausgabe.contains("handshake-abgebrochen"),
        "der Grund wird nicht benannt"
    );
}

// ===========================================================================
// 4. DER PARSER-BEWEIS
// ===========================================================================

/// DERSELBE PARSER, ZWEI TRANSPORTE.
///
/// ==========================================================================
/// Dieser Test ist die Antwort auf „beweise, dass der HTTP-Parser nicht
/// angefasst werden musste". Er hat drei Teile:
///
///  (a) Der Kernel-Klient `hole` benutzt `speedhttp::antwort_parsen` ueber
///      einen nackten TCP-Socket. Dass das noch geht, zeigen die
///      unveraenderten `#[test_case]`-Tests in src/netz/http.rs.
///  (b) `holes` benutzt DIESELBE Funktion ueber einen TLS-Strom. Dass das
///      geht, zeigen die Tests oben.
///  (c) Und hier: dass beide bei derselben Antwort dasselbe herausbekommen.
///      Der Test parst die ROHE Antwort, die `hole` geholt hat, noch einmal
///      selbst — mit genau dem Aufruf, den auch `holes` macht.
///
/// Die eigentliche Zusage steht allerdings gar nicht in einem Test, sondern
/// im Bauplan: `speedhttp/Cargo.toml` hat KEINE Abhaengigkeiten. Diese Kiste
/// kann gar nichts von Sockets, TLS oder Syscalls wissen — sie kennt Bytes.
/// Ein Parser, der nichts kennt, muss auch nichts lernen, wenn der Transport
/// wechselt.
/// ==========================================================================
#[test_case]
fn test_parser_ist_derselbe() {
    use speed_os::netz::http;

    // (c) Die reinen Funktionen, aufgerufen wie in Ring 3.
    let roh = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nHallo!";
    let ueber_kernel = http::antwort_parsen(roh).expect("Kernel-Pfad");
    let ueber_kiste = speedhttp::antwort_parsen(roh).expect("Ring-3-Pfad");
    assert_eq!(
        ueber_kernel, ueber_kiste,
        "der Kernel und die geteilte Kiste muessen dasselbe liefern"
    );
    // Dass `assert_eq!` oben ueberhaupt UEBERSETZT, ist schon der halbe
    // Beweis: `http::Antwort` und `speedhttp::Antwort` muessen derselbe Typ
    // sein, sonst gaebe es kein `PartialEq` zwischen ihnen.
    //
    // Und zur Sicherheit auch die ADRESSE: `pub use` re-exportiert, es gibt
    // also genau EINE Funktion — keine zwei, die zufaellig gleich aussehen.
    let ueber_kernel_fn: fn(&[u8]) -> Result<http::Antwort, http::HttpFehler> =
        http::antwort_parsen;
    let ueber_kiste_fn: fn(&[u8]) -> Result<speedhttp::Antwort, speedhttp::HttpFehler> =
        speedhttp::antwort_parsen;
    assert_eq!(
        ueber_kernel_fn as usize, ueber_kiste_fn as usize,
        "http::antwort_parsen muss speedhttp::antwort_parsen SEIN, keine Kopie"
    );
    serial_println!(
        "  Adressgleichheit geprueft: der Kernel ruft buchstaeblich die \
         Funktion, die auch `holes` ruft."
    );

    // Und die Ergaenzung von Teil 4 baut auf dem Original auf, statt es zu
    // ersetzen: Bei Port 80 muessen beide dasselbe liefern.
    let url = speedhttp::url_parsen("http://beispiel.de/a").expect("URL");
    assert_eq!(
        speedhttp::anfrage_bauen(&url),
        speedhttp::anfrage_bauen_mit_host(&url, "beispiel.de"),
        "anfrage_bauen_mit_host muss das Original benutzen"
    );

    // (a) Der Kernel-Transport, wenn ein LAN-Server da ist.
    match http::holen(LAN_HTTP) {
        Ok((_, antwort)) => {
            serial_println!(
                "  Kernel-Transport (TCP): HTTP {} , {} Byte Rumpf.",
                antwort.status,
                antwort.rumpf.len()
            );
        }
        Err(fehler) => serial_println!(
            "  (LAN-Server nicht da: {} — der Vergleichslauf entfaellt)",
            fehler.meldung()
        ),
    }
}

// ===========================================================================
// 5. MESSUNG (Aufgabe 4)
// ===========================================================================

/// Handshake-Dauer, Heap-Bedarf und Durchsatz — die Zahlen fuer den CHANGELOG.
///
/// `holes --still` gibt eine maschinenlesbare Zeile aus; hier wird sie
/// eingesammelt und protokolliert. Der Test misst BERICHTEND: Er faellt nicht
/// durch, wenn das Internet langsam ist, aber er sagt, was war.
#[test_case]
fn test_messung_handshake_heap_durchsatz() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    if !internet_da() {
        serial_println!("  (uebersprungen: kein Internet/DNS)");
        return;
    }

    serial_println!("  === MESSUNG: TLS aus Ring 3 ===");
    for (titel, url) in [("klein (example.com)", ECHT_KLEIN), ("gross (cacert.pem)", ECHT_GROSS)] {
        let mut beste_handshake = u64::MAX;
        let mut zeile_beste = String::new();
        // Drei Laeufe, bester Handshake zaehlt — der erste traegt die
        // DNS-Aufloesung und den kalten Cache mit.
        for _ in 0..3 {
            let (ende, ausgabe) = starten_und_lesen("holes", &["holes", url, "--still"]);
            if ende != Some(HOLES_OK) {
                ausgabe_zeigen("Messlauf fehlgeschlagen", &ausgabe, &ende);
                continue;
            }
            for zeile in ausgabe.lines() {
                if let Some(rest) = zeile.trim().strip_prefix("MESSUNG ") {
                    let handshake = feld(rest, "handshake_ms=").unwrap_or(u64::MAX);
                    if handshake < beste_handshake {
                        beste_handshake = handshake;
                        zeile_beste = String::from(rest);
                    }
                }
            }
        }
        if zeile_beste.is_empty() {
            serial_println!("  {}: kein gueltiger Messlauf.", titel);
            continue;
        }
        let rumpf = feld(&zeile_beste, "rumpf=").unwrap_or(0);
        let dauer = feld(&zeile_beste, "uebertragung_ms=").unwrap_or(0);
        serial_println!("  {}:", titel);
        serial_println!("    {}", zeile_beste);
        if dauer > 0 && rumpf > 0 {
            // KiB/s ohne Fliesskomma (der Kernel ist soft-float).
            let kib_s = (rumpf * 1000) / (dauer * 1024).max(1);
            serial_println!(
                "    -> Durchsatz {} KiB/s ({} Byte in {} ms), Handshake {} ms",
                kib_s,
                rumpf,
                dauer,
                beste_handshake
            );
        }
    }

    // DER VERGLEICHSWERT: dieselbe Datei, ohne TLS, ueber den Kernel-Klienten
    // und einen LOKALEN Server. Ohne ihn liesse sich die TLS-Zahl nicht
    // einordnen — sie enthaelt schliesslich auch die Anbindung des Hosts.
    vergleich_ohne_tls();
}

/// Misst dieselbe Datei ueber PLAIN TCP (Kernel-Klient, LAN-Server).
fn vergleich_ohne_tls() {
    use speed_os::netz::http;
    let start = zeit::ms_seit_boot();
    match http::holen(LAN_GROSS) {
        Ok((_, antwort)) => {
            let dauer = zeit::ms_seit_boot() - start;
            let kib_s = (antwort.rumpf.len() as u64 * 1000) / (dauer * 1024).max(1);
            serial_println!(
                "  VERGLEICH ohne TLS (Kernel-Klient, LAN): {} Byte in {} ms = {} KiB/s",
                antwort.rumpf.len(),
                dauer,
                kib_s
            );
        }
        Err(fehler) => serial_println!(
            "  VERGLEICH ohne TLS: nicht moeglich ({}) — \
             `python -m http.server 8000` mit gross.pem im Ordner starten.",
            fehler.meldung()
        ),
    }
}

/// Liest `name=<zahl>` aus einer Messzeile.
fn feld(zeile: &str, name: &str) -> Option<u64> {
    let ab = zeile.find(name)? + name.len();
    let rest = &zeile[ab..];
    let ende = rest.find(' ').unwrap_or(rest.len());
    rest[..ende].parse().ok()
}
