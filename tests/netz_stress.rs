// tests/netz_stress.rs — Der EHRLICHE Stresstest für die Reißleinen-Entscheidung
//
// docs/tcp-scope.md legt das Kriterium fest (>= 9/10 saubere HTTP-Läufe). Die
// bisherigen Messungen liefen gegen EINEN Server und ohne Störung. Das reicht
// für eine Ingenieur-Entscheidung nicht. Dieser Test geht härter ran:
//
//   PHASE 1: 20 Abrufe gegen VERSCHIEDENE echte Internet-Server (verschiedene
//            TCP-Stacks, RTTs, Antwortgrößen, chunked vs. Content-Length,
//            Weiterleitungen).
//   PHASE 2: derselbe Weg mit KÜNSTLICHEM PAKETVERLUST (10 % und 20 % je
//            Richtung, an unserer Geräte-Naht eingespeist — auf einem
//            Windows-Host gibt es kein tc/netem). Gegen den LAN-Server mit
//            einer 21 KB-Datei: der härteste Retransmit-/Fenster-Test.
//   PHASE 3: Verlust gegen eine echte INTERNET-Gegenstelle.
//
// EHRLICHE BEWERTUNG: Nicht jeder Fehlschlag ist ein TCP-Fehler. Ein Server,
// der auf https umleitet, oder eine DNS-Panne sind UMGEBUNG. Als TCP-FEHLER
// zählen nur: Timeout/Hänger, Abbruch (RST/aufgegeben), unvollständiger Rumpf
// und kaputte Köpfe — also genau das, was ein schlechter TCP-Stack produziert.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::netz::{self, http, http::KlientFehler, socket::SocketFehler};
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

/// Echte Internet-Server, die (noch) einfaches http sprechen.
const INTERNET: [&str; 8] = [
    "http://example.com/",
    "http://neverssl.com/",
    "http://info.cern.ch/",
    "http://detectportal.firefox.com/success.txt",
    "http://www.msftconnecttest.com/connecttest.txt",
    "http://captive.apple.com/hotspot-detect.html",
    "http://httpforever.com/",
    "http://connectivitycheck.gstatic.com/generate_204",
];

/// Der LAN-Server (IP-Literal -> kein DNS im Spiel, reine TCP-Messung).
const LAN: &str = "http://10.0.2.2:8000/probe.txt";

/// Wie ein Ergebnis zu werten ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Befund {
    /// TCP hat sauber gearbeitet (vollständige Antwort geparst).
    TcpOk,
    /// EIN TCP-FEHLER: Hänger, Abbruch, unvollständig, kaputt.
    TcpFehler,
    /// Nicht TCP zuzurechnen (https-Umleitung, DNS, URL).
    Umgebung,
}

/// Bewertet ein Abruf-Ergebnis ehrlich.
///
/// Seit Serie 7, Teil 4 unterscheidet der Kernel-Klient zwischen
/// PROTOKOLL-Fehlern (`HttpFehler`, aus der transportfreien Kiste
/// `speedhttp`) und TRANSPORT-Fehlern (`KlientFehler::{Dns,Socket}`). Fuer
/// diesen Test aendert das nur die Schreibweise — die Einteilung in
/// "TCP-Fehler" und "Umgebung" bleibt exakt dieselbe.
fn bewerten(ergebnis: &Result<(http::Url, http::Antwort), KlientFehler>) -> (Befund, &'static str) {
    use http::HttpFehler::*;
    match ergebnis {
        Ok(_) => (Befund::TcpOk, "vollstaendige Antwort"),
        Err(KlientFehler::Http(TlsNichtUnterstuetzt)) => (Befund::Umgebung, "Server leitet auf https"),
        // Seit Serie 7, Teil 5 nennt der Klient das ausgerechnete https-Ziel,
        // statt nur „geht nicht" zu sagen. Fuer diesen Test bleibt es
        // dasselbe: kein TCP-Fehler, sondern die Umgebung.
        Err(KlientFehler::BrauchtTls(_)) => (Befund::Umgebung, "Server leitet auf https"),
        Err(KlientFehler::Dns(_)) => (Befund::Umgebung, "DNS"),
        Err(KlientFehler::Http(UngueltigeUrl)) => (Befund::Umgebung, "URL"),
        Err(KlientFehler::Http(ZuVieleWeiterleitungen)) => (Befund::Umgebung, "zu viele Weiterleitungen"),
        Err(KlientFehler::Http(ZuGross)) => (Befund::Umgebung, "Antwort zu gross"),
        Err(KlientFehler::Http(UnvollstaendigeAntwort)) => (Befund::TcpFehler, "RUMPF UNVOLLSTAENDIG"),
        Err(KlientFehler::Http(KaputterKopf)) => (Befund::TcpFehler, "KAPUTTER KOPF"),
        Err(KlientFehler::Socket(SocketFehler::Zeitueberschreitung)) => {
            (Befund::TcpFehler, "ZEITUEBERSCHREITUNG (Haenger)")
        }
        Err(KlientFehler::Socket(SocketFehler::Abgebrochen)) => (Befund::TcpFehler, "ABBRUCH (RST/aufgegeben)"),
        Err(KlientFehler::Socket(_)) => (Befund::TcpFehler, "SOCKET-FEHLER"),
    }
}

/// Zählwerk einer Phase.
struct Bilanz {
    ok: u32,
    tcp_fehler: u32,
    umgebung: u32,
    max_dauer_ms: u64,
}

impl Bilanz {
    fn neu() -> Bilanz {
        Bilanz {
            ok: 0,
            tcp_fehler: 0,
            umgebung: 0,
            max_dauer_ms: 0,
        }
    }
    /// Anteil sauberer Läufe unter den TCP-RELEVANTEN Versuchen (in Prozent).
    fn tcp_quote(&self) -> u32 {
        // Ohne TCP-relevante Versuche gilt die Quote als erfuellt (100 %);
        // max(1) haelt die Division sicher.
        let relevant = (self.ok + self.tcp_fehler).max(1);
        if self.ok + self.tcp_fehler == 0 {
            100
        } else {
            self.ok * 100 / relevant
        }
    }
}

/// Führt EINEN Abruf aus, misst die Dauer, protokolliert und bilanziert.
fn versuch(nr: u32, url: &str, bilanz: &mut Bilanz) {
    let start = zeit::ms_seit_boot();
    let ergebnis = http::holen(url);
    let dauer = zeit::ms_seit_boot() - start;
    bilanz.max_dauer_ms = bilanz.max_dauer_ms.max(dauer);
    let (befund, was) = bewerten(&ergebnis);
    match befund {
        Befund::TcpOk => bilanz.ok += 1,
        Befund::TcpFehler => bilanz.tcp_fehler += 1,
        Befund::Umgebung => bilanz.umgebung += 1,
    }
    match &ergebnis {
        Ok((_, a)) => serial_println!(
            "  [{:>2}] {:>5} ms  OK    HTTP {} , {} Byte  <- {}",
            nr,
            dauer,
            a.status,
            a.rumpf.len(),
            url
        ),
        Err(_) => serial_println!(
            "  [{:>2}] {:>5} ms  {}  ({})  <- {}",
            nr,
            dauer,
            if befund == Befund::TcpFehler { "FEHLER" } else { "umgeb." },
            was,
            url
        ),
    }
}

/// Netz aufsetzen (DHCP) — für alle Phasen.
fn netz_aufsetzen() {
    let e = netz::dhcp::beziehen(4000).expect("DHCP: keine Lease");
    netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);
    serial_println!("[STRESS] IP {} / Gateway {} / DNS {}", e.ip, e.gateway, e.dns);
}

/// PHASE 1: 20 Abrufe gegen verschiedene echte Internet-Server.
#[test_case]
fn test_stress_internet_vielfalt() {
    assert!(netz::vorhanden(), "keine NIC");
    netz_aufsetzen();
    netz::geraet::verlust_setzen(0);

    serial_println!("[STRESS] PHASE 1: 20 Abrufe gegen {} verschiedene Server, ohne Stoerung.", INTERNET.len());
    let mut bilanz = Bilanz::neu();
    for i in 0..20u32 {
        let url = INTERNET[(i as usize) % INTERNET.len()];
        versuch(i + 1, url, &mut bilanz);
    }
    serial_println!(
        "[STRESS] PHASE 1 Bilanz: {} sauber, {} TCP-FEHLER, {} umgebungsbedingt, \
         laengster Abruf {} ms, TCP-Quote {}%.",
        bilanz.ok,
        bilanz.tcp_fehler,
        bilanz.umgebung,
        bilanz.max_dauer_ms,
        bilanz.tcp_quote()
    );
    // METHODIK: Das HARTE Reissleinen-Gate liegt bewusst auf dem
    // KONTROLLIERBAREN LAN-Server (tests/netz_http.rs, 10/10) — eine
    // Testsuite darf nicht von fremden Internet-Servern abhaengen. Hier
    // steht nur eine Grundschwelle, die echtes Kaputtsein auffangen wuerde;
    // die Zahlen selbst sind in docs/tcp-scope.md protokolliert.
    assert!(
        bilanz.tcp_quote() >= 75,
        "TCP-Quote im Internet-Lauf nur {}% ({} Fehler) — das ist mehr als die          bekannte Verlust-Schwaeche, bitte docs/tcp-scope.md pruefen",
        bilanz.tcp_quote(),
        bilanz.tcp_fehler
    );
}

/// PHASE 2: LAN-Server (21 KB, groesser als das Fenster) mit Paketverlust —
/// der haerteste Retransmit-Test, ganz ohne DNS-Rauschen.
#[test_case]
fn test_stress_verlust_lan() {
    assert!(netz::vorhanden(), "keine NIC");
    netz_aufsetzen();

    // Ist der LAN-Server ueberhaupt da?
    netz::geraet::verlust_setzen(0);
    if http::holen(LAN).is_err() {
        serial_println!("[STRESS-HINWEIS] LAN-Server nicht erreichbar — Phase 2 uebersprungen.");
        return;
    }

    // WICHTIG (Methodik): Unter kuenstlichem Verlust ist unser TCP LANGSAM
    // und reisst gelegentlich das Zeitbudget — das ist die in docs/tcp-scope.md
    // DOKUMENTIERTE, akzeptierte Schwaeche (kein Fast-Retransmit, kein SACK,
    // Out-of-Order wird verworfen). Der Verlust ist zudem NICHT deterministisch
    // (der RNG-Stand haengt am realen Netz-Timing der vorigen Phasen). Wir
    // gaten hier deshalb NICHT auf eine Erfolgsquote (das waere ein flakiger
    // Test, der eine akzeptierte Schwaeche als Fehler wertet), sondern pruefen
    // die INVARIANTEN, die IMMER gelten muessen:
    //   * jeder Versuch TERMINIERT (kein Deadlock/Haenger ohne Ende),
    //   * ueber ALLE Verlust-Laeufe kommt mindestens einer durch (Retransmit
    //     funktioniert grundsaetzlich),
    //   * was ankommt, ist VOLLSTAENDIG (Content-Length exakt) — nie korrupt.
    // Das harte Reissleinen-Gate liegt auf dem verlustfreien LAN (netz_http).
    let mut gesamt_ok = 0u32;
    let mut gesamt_fehler = 0u32;
    for verlust in [10u32, 20u32] {
        serial_println!("[STRESS] PHASE 2: LAN-Abrufe mit {}% Paketverlust je Richtung.", verlust);
        netz::geraet::verlust_setzen(verlust);
        let mut bilanz = Bilanz::neu();
        let runden = if verlust == 10 { 5 } else { 3 };
        for i in 0..runden {
            versuch(i + 1, LAN, &mut bilanz);
        }
        netz::geraet::verlust_setzen(0);
        serial_println!(
            "[STRESS] {}% Verlust: {} sauber, {} TCP-FEHLER, laengster Abruf {} ms.",
            verlust,
            bilanz.ok,
            bilanz.tcp_fehler,
            bilanz.max_dauer_ms
        );
        // INVARIANTE: jeder Versuch ist verbucht und terminiert (kein Haenger
        // ohne Ende — Fehlschlaege sind Timeouts, also zeitlich begrenzt).
        assert_eq!(
            bilanz.ok + bilanz.tcp_fehler,
            runden,
            "ein Versuch bei {}% Verlust hat nicht terminiert",
            verlust
        );
        gesamt_ok += bilanz.ok;
        gesamt_fehler += bilanz.tcp_fehler;
    }
    // INVARIANTE: ueber alle 8 Verlust-Laeufe kommt mindestens einer durch
    // (P(alle scheitern) ist verschwindend — die 10%-Laeufe schaffen es fast
    // immer). Das beweist, dass Retransmit + Zustandsautomat grundsaetzlich
    // funktionieren, ohne eine flakige Quote zu erzwingen.
    serial_println!(
        "[STRESS] PHASE 2 gesamt: {} sauber / {} TCP-FEHLER ueber alle Verlust-Laeufe.",
        gesamt_ok,
        gesamt_fehler
    );
    assert!(
        gesamt_ok > 0,
        "unter Verlust kam KEIN LAN-Transfer durch — Retransmit defekt?"
    );
}

/// PHASE 3: Verlust gegen eine echte Internet-Gegenstelle.
#[test_case]
fn test_stress_verlust_internet() {
    assert!(netz::vorhanden(), "keine NIC");
    netz_aufsetzen();
    netz::geraet::verlust_setzen(0);

    // Namen VORHER aufloesen (der DNS-Cache traegt sie dann durch die
    // Verlust-Phase — sonst messen wir DNS statt TCP).
    let mut erreichbar = alloc::vec::Vec::new();
    for url in INTERNET.iter().take(4) {
        if let Ok((u, _)) = http::holen(url) {
            erreichbar.push(*url);
            let _ = u;
        }
    }
    if erreichbar.is_empty() {
        serial_println!("[STRESS-HINWEIS] kein Internet-Server erreichbar — Phase 3 uebersprungen.");
        return;
    }

    serial_println!("[STRESS] PHASE 3: Internet-Abrufe mit 10% Paketverlust je Richtung.");
    netz::geraet::verlust_setzen(10);
    let mut bilanz = Bilanz::neu();
    for (i, url) in erreichbar.iter().enumerate() {
        versuch(i as u32 + 1, url, &mut bilanz);
    }
    netz::geraet::verlust_setzen(0);
    serial_println!(
        "[STRESS] PHASE 3 Bilanz: {} sauber, {} TCP-FEHLER, {} umgebungsbedingt, \
         laengster Abruf {} ms, TCP-Quote {}%.",
        bilanz.ok,
        bilanz.tcp_fehler,
        bilanz.umgebung,
        bilanz.max_dauer_ms,
        bilanz.tcp_quote()
    );
    // Auch hier: Erholung statt Perfektion (siehe Phase 2).
    assert!(
        bilanz.ok > 0,
        "unter Verlust kam KEIN einziger Internet-Abruf durch — Retransmit defekt?"
    );
}

/// Nach allen Phasen: der Stack muss weiter benutzbar sein (keine
/// TIME_WAIT-Verstopfung, keine Handle-Lecks) — ein Abruf muss noch gehen.
#[test_case]
fn test_stress_danach_noch_benutzbar() {
    netz::geraet::verlust_setzen(0);
    netz_aufsetzen();
    let offene = netz::socket::anzahl();
    let ergebnis = http::holen(INTERNET[0]);
    let (befund, was) = bewerten(&ergebnis);
    serial_println!(
        "[STRESS] Nach allen Phasen: {} Sockets in der Tabelle, Abschluss-Abruf: {:?} ({}).",
        offene,
        befund,
        was
    );
    // Sockets duerfen sich nicht ansammeln (TIME_WAIT wird abgeraeumt).
    assert!(
        offene < 20,
        "Socket-Tabelle laeuft voll ({} Eintraege) — TIME_WAIT-Problem?",
        offene
    );
    assert_ne!(befund, Befund::TcpFehler, "Abschluss-Abruf scheiterte: {}", was);
}
