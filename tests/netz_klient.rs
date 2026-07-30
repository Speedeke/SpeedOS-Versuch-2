// tests/netz_klient.rs — Die Abrufschicht unter Druck (Serie 7, Teil 5)
//
// ==========================================================================
// DER ROBUSTHEITS-PASS, in der Tradition von tests/netz_abschluss.rs
//
// Eine Abrufschicht ist erst brauchbar, wenn sie auch dann sauber
// zurueckkommt, wenn die Gegenstelle sich schlecht benimmt. Geprueft wird
// deshalb NICHT der Erfolgsfall (den zeigt tests/netz_https.rs), sondern
// die Fehlerfaelle — und zwar jeder mit derselben Zusage:
//
//     ein SAUBERER Fehler, IN FRIST, kein Haenger, kein Panic,
//     und danach ist kein Socket und kein Handle uebrig.
//
// Die boesartigen Gegenstellen liefert `tools/tls_testserver.py`:
//
//   http://10.0.2.2:8080/abbruch   kappt die Leitung MITTEN im Rumpf
//   http://10.0.2.2:8080/endlos    sendet ohne Ende
//   http://10.0.2.2:8080/schleife  leitet auf sich selbst weiter
//   http://10.0.2.2:8080/kette     zehn Weiterleitungen hintereinander
//   http://10.0.2.2:8080/nach-tls  leitet auf https weiter (Schema-Wechsel)
//   10.0.2.2:8444                  nimmt an und schweigt (Handshake-Frist)
//
// WARUM DIE RUMPF-FAELLE UEBER KLARTEXT LAUFEN: Das Zertifikat des
// TLS-Testservers wird zu Recht abgelehnt, BEVOR je ein Byte Rumpf fliesst.
// Ueber http laesst sich derselbe Klient-Code (`libspeed::netz`) also
// deterministisch pruefen — es ist dieselbe Zustandsmaschine, nur ohne
// Verschluesselung darunter. Die TLS-eigenen Faelle (Handshake-Frist,
// Zertifikat) laufen ueber TLS.
//
// VORBEREITUNG:  python tools/tls_testserver.py

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
use speed_os::{allocator, fs, memory, netz, pci, pipe, programme, scheduler, serial_println};
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
    // 8 MiB wie im echten Boot — `holes` ist ~950 KiB und wird bei jedem
    // Prozess-Start am Stueck in den Kernel-Heap gelesen.
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
    netz::dhcp::autokonfig(5000);

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ===========================================================================
// Adressen und Werkzeug
// ===========================================================================

const KLAR: &str = "http://10.0.2.2:8080";
const STUMM: &str = "https://10.0.2.2:8444/";
/// Ein Name, den es per Norm (RFC 2606) NIEMALS geben kann.
const KEIN_NAME: &str = "https://gibt.es.nicht.invalid/";

/// Wie lange ein einzelner Prozess hoechstens laufen darf, bevor der TEST
/// aufgibt. Grosszuegiger als jede Frist, die wir dem Klienten geben — sonst
/// misst der Test seine eigene Ungeduld statt der des Klienten.
const TEST_FRIST_MS: u64 = 90_000;

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

/// Was ein Programmlauf ergeben hat.
struct Lauf {
    ende: Option<ProzessEnde>,
    ausgabe: String,
    dauer_ms: u64,
}

impl Lauf {
    /// Steht dieses Schlagwort in der Ausgabe? (`AbrufFehler::kurz`)
    fn grund(&self, kurz: &str) -> bool {
        self.ausgabe.contains(&alloc::format!("({}).", kurz))
    }
    fn code(&self) -> i32 {
        match self.ende {
            Some(ende) => ende.code() as i32,
            None => -1,
        }
    }
}

/// Startet ein Programm mit einer Pipe als Ausgabe und liest alles mit.
fn starten(name: &str, argumente: &[&str]) -> Lauf {
    let start = zeit::ms_seit_boot();
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
    let frist = zeit::ms_seit_boot() + TEST_FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    serial_println!("  !! TEST-Frist abgelaufen — der Prozess haengt.");
                    break;
                }
                if ende.is_none() {
                    ende = scheduler::ende_abfragen(pid);
                }
                scheduler::aufraeumen();
                // Kein Executor im Testkernel: der Stack wird von Hand gepumpt.
                netz::pumpen();
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

    // ==================================================================
    // DEN ABBAU ZU ENDE PUMPEN — und warum das hier stehen MUSS
    //
    // Wenn ein Prozess endet, schliesst seine Handle-Tabelle die Sockets.
    // „Schliessen" heisst bei TCP aber nicht „weg", sondern „FIN schicken,
    // ACK abwarten, TIME_WAIT" — und dafuer muss jemand den Stack drehen.
    // Im laufenden System tun das der `netz_task` und der Socket-Takt
    // (alle 100 ms); ein TESTKERNEL hat keinen Executor, hier muss es von
    // Hand passieren.
    //
    // Das ist kein Test-Trick, sondern das Nachstellen dessen, was im
    // Betrieb ohnehin laeuft — dieselbe Ueberlegung wie beim `pumpen()` in
    // der Warteschleife oben.
    //
    // EHRLICH DAZU: Diese Zeilen haben die Laeufe deutlich beruhigt, aber
    // sie beseitigen den fluechtigen Fehlschlag NICHT. Er tritt auf dem
    // Prozess-Pfad weiterhin ein paar Mal je Lauf auf (sichtbar an den
    // „lieferte 0 Byte"-Zeilen auf dem Diagnose-Kanal); was die Laeufe
    // durchtragen laesst, ist die Wiederholung in `libspeed::netz`.
    // Was WEITERHIN GILT und die Suche eingrenzt: Der KERNEL-Klient, der
    // ganz ohne Prozesse arbeitet, sieht in 30 Abrufen 0 Fehlschlaege
    // (`test_fluechtige_fehlschlaege_zaehlen`). Es liegt also am
    // Prozess-Pfad oder an slirps Sicht darauf, nicht an TCP selbst.
    // ==================================================================
    for _ in 0..60 {
        netz::pumpen();
        zeit::warte_auf_interrupt();
    }

    Lauf {
        ende,
        ausgabe: String::from_utf8_lossy(&gesammelt).into_owned(),
        dauer_ms: zeit::ms_seit_boot() - start,
    }
}

fn zeigen(titel: &str, lauf: &Lauf) {
    serial_println!("  === {} ({} ms) ===", titel, lauf.dauer_ms);
    for zeile in lauf.ausgabe.lines() {
        serial_println!("  | {}", zeile);
    }
    serial_println!("  === Ende: {:?} ===", lauf.ende);
}

/// Lauscht der Testserver? Sonst werden die Live-Faelle uebersprungen.
fn server_da() -> bool {
    match netz::http::holen(&alloc::format!("{}/klein.txt", KLAR)) {
        Ok((_, antwort)) => antwort.status == 200,
        Err(_) => {
            serial_println!(
                "  (uebersprungen: auf {} lauscht nichts — \
                 `python tools/tls_testserver.py` starten)",
                KLAR
            );
            false
        }
    }
}

// ===========================================================================
// 1. DER URL-PARSER — reine Logik, kein Netz
// ===========================================================================

/// Schema, Host, Port und Pfad — auch bei krummen Eingaben.
///
/// Diese Funktion ist die EINE Stelle, an der SpeedOS entscheidet, was eine
/// Adresse bedeutet. Vor Serie 7, Teil 5 tat das jeder Aufrufer selbst
/// (`holes` so, der Kernel-Klient anders), und die Standard-Ports standen an
/// drei Stellen.
#[test_case]
fn test_ziel_parsen() {
    use speed_os::netz::http::{ziel_parsen, HttpFehler};

    // --- Die Normalfaelle ---
    let z = ziel_parsen("https://example.com/index.html").unwrap();
    assert!(z.tls);
    assert_eq!(z.url.host, "example.com");
    assert_eq!(z.url.port, 443, "https ohne Port -> 443");
    assert_eq!(z.url.pfad, "/index.html");

    let z = ziel_parsen("http://example.com/index.html").unwrap();
    assert!(!z.tls);
    assert_eq!(z.url.port, 80, "http ohne Port -> 80");

    // OHNE SCHEMA: https. Das ist die bewusste Umkehrung gegenueber der
    // Serie-5-Funktion `url_parsen` (dort http) — 2026 ist Klartext die
    // Ausnahme.
    let z = ziel_parsen("example.com").unwrap();
    assert!(z.tls, "ohne Schema wird https angenommen");
    assert_eq!(z.url.port, 443);
    assert_eq!(z.url.pfad, "/", "ohne Pfad -> /");

    // --- Ports, ausdruecklich ---
    let z = ziel_parsen("https://example.com:8443/a/b").unwrap();
    assert_eq!(z.url.port, 8443);
    assert_eq!(z.url.pfad, "/a/b");
    assert!(!z.port_ist_standard());
    let z = ziel_parsen("http://10.0.2.2:8080/").unwrap();
    assert_eq!(z.url.host, "10.0.2.2");
    assert_eq!(z.url.port, 8080);
    // Der ausdrueckliche Standard-Port zaehlt als Standard:
    assert!(ziel_parsen("https://example.com:443/").unwrap().port_ist_standard());
    assert!(ziel_parsen("http://example.com:80/").unwrap().port_ist_standard());
    // ... und der ausdrueckliche FALSCHE nicht:
    assert!(!ziel_parsen("http://example.com:443/").unwrap().port_ist_standard());

    // --- Der Host-Kopf: der Standard-Port gehoert NICHT hinein ---
    assert_eq!(ziel_parsen("https://a.de/").unwrap().host_kopf(), "a.de");
    assert_eq!(ziel_parsen("http://a.de/").unwrap().host_kopf(), "a.de");
    assert_eq!(
        ziel_parsen("https://a.de:8443/").unwrap().host_kopf(),
        "a.de:8443"
    );

    // --- Text-Rueckweg (der Schluessel des Schleifenschutzes) ---
    // Standard-Port wird weggelassen — sonst waeren „https://a/" und
    // „https://a:443/" zwei verschiedene Stellen und eine Schleife liefe.
    assert_eq!(ziel_parsen("https://a.de:443/x").unwrap().als_text(), "https://a.de/x");
    assert_eq!(ziel_parsen("a.de").unwrap().als_text(), "https://a.de/");
    assert_eq!(
        ziel_parsen("http://a.de:8080/x").unwrap().als_text(),
        "http://a.de:8080/x"
    );

    // --- KRUMME EINGABEN: jede einzelne ein sauberer Fehler, kein Panic ---
    for krumm in [
        "",
        "   ",
        "https://",
        "http://",
        "https:///pfad",          // Host fehlt
        "ftp://example.com/",     // fremdes Schema
        "gopher://a.de",
        "https://a.de:99999/",    // Port passt nicht in u16
        "https://a.de:abc/",      // Port ist keine Zahl
        "https://a.de:/",         // Doppelpunkt ohne Port
        "https://:8443/",         // Port ohne Host
    ] {
        let ergebnis = ziel_parsen(krumm);
        assert!(
            ergebnis.is_err(),
            "'{}' haette abgelehnt werden muessen, wurde aber {:?}",
            krumm,
            ergebnis
        );
    }
    // Und der Fehler ist der PASSENDE, nicht irgendeiner:
    assert_eq!(ziel_parsen("ftp://a.de/"), Err(HttpFehler::UngueltigeUrl));

    // Leerzeichen aussen herum stoeren nicht.
    assert_eq!(
        ziel_parsen("  https://a.de/x  ").unwrap().als_text(),
        "https://a.de/x"
    );
    serial_println!("  {} krumme Eingaben sauber abgelehnt, kein Panic.", 11);
}

/// Weiterleitungen: absolut, relativ — und ueber das Schema hinweg.
#[test_case]
fn test_naechstes_ziel_mit_schema_wechsel() {
    use speed_os::netz::http::{naechstes_ziel, ziel_parsen, HttpFehler};

    let basis = ziel_parsen("http://alt.example/verzeichnis/seite.html").unwrap();

    // DER FALL, UM DEN ES GEHT: http -> https.
    let z = naechstes_ziel(&basis, "https://neu.example/ziel").unwrap();
    assert!(z.tls, "der Schema-Wechsel muss mitgehen");
    assert_eq!(z.url.host, "neu.example");
    assert_eq!(z.url.port, 443, "das neue Schema bestimmt den Standard-Port");
    assert_eq!(z.url.pfad, "/ziel");

    // Und zurueck (https -> http) — genauso erlaubt, wenn auch selten.
    let sicher = ziel_parsen("https://a.de/").unwrap();
    let z = naechstes_ziel(&sicher, "http://b.de/x").unwrap();
    assert!(!z.tls);
    assert_eq!(z.url.port, 80);

    // Absoluter Pfad: Schema, Host und Port BLEIBEN.
    let z = naechstes_ziel(&basis, "/woanders").unwrap();
    assert!(!z.tls);
    assert_eq!(z.url.host, "alt.example");
    assert_eq!(z.url.pfad, "/woanders");

    // Relativ: gegen das VERZEICHNIS der Basis.
    let z = naechstes_ziel(&basis, "nachbar.html").unwrap();
    assert_eq!(z.url.pfad, "/verzeichnis/nachbar.html");

    // Port bleibt bei relativer Weiterleitung erhalten.
    let mit_port = ziel_parsen("https://a.de:8443/x/y").unwrap();
    let z = naechstes_ziel(&mit_port, "z").unwrap();
    assert_eq!(z.url.port, 8443);
    assert_eq!(z.als_text(), "https://a.de:8443/x/z");

    // Unsinn bleibt Unsinn.
    assert_eq!(naechstes_ziel(&basis, "   "), Err(HttpFehler::UngueltigeUrl));
    assert_eq!(
        naechstes_ziel(&basis, "ftp://a.de/"),
        Err(HttpFehler::UngueltigeUrl)
    );

    // WICHTIG: Die Serie-5-Funktion bleibt, wie sie war — sie lehnt https
    // weiterhin ab. `naechstes_ziel` ist eine ERGAENZUNG, kein Ersatz.
    let alt = speed_os::netz::http::naechste_url(&basis.url, "https://neu.example/");
    assert_eq!(alt, Err(HttpFehler::TlsNichtUnterstuetzt));
}

// ===========================================================================
// 2. WEITERLEITUNGEN, ECHT
// ===========================================================================

/// Eine Kette wird verfolgt — und der Klient sagt, wie oft.
#[test_case]
fn test_weiterleitungskette() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let lauf = starten("holes", &["holes", &alloc::format!("{}/weiter1", KLAR)]);
    zeigen("Weiterleitung /weiter1 -> /weiter2 -> /klein.txt", &lauf);

    assert_eq!(lauf.code(), 0, "die Kette haette durchlaufen muessen");
    assert!(
        lauf.ausgabe.contains("2 Weiterleitung(en)"),
        "der Klient muss die Zahl der Weiterleitungen nennen"
    );
    assert!(
        lauf.ausgabe.contains("SpeedOS TLS-Testserver:"),
        "der Inhalt der ENDGUELTIGEN Adresse fehlt"
    );
}

/// SCHEMA-WECHSEL: http leitet auf https weiter, und der Klient geht mit.
///
/// Der Beweis, dass er wirklich mitgeht, ist der FEHLER: Auf der
/// https-Seite steht unser selbst ausgestelltes Zertifikat, und das wird
/// abgelehnt. Ein Klient, der den Wechsel ignoriert, bekaeme stattdessen
/// den http-Rumpf und Exit 0.
#[test_case]
fn test_weiterleitung_wechselt_das_schema() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    zufall_bereitmachen();
    let lauf = starten("holes", &["holes", &alloc::format!("{}/nach-tls", KLAR)]);
    zeigen("Schema-Wechsel http -> https", &lauf);

    assert_eq!(lauf.code(), 4, "es haette an der TLS-Pruefung enden muessen");
    assert!(
        lauf.grund("unbekannte-ca"),
        "der Klient ist dem Wechsel auf https NICHT gefolgt"
    );
    assert!(
        !lauf.ausgabe.contains("HTTP 200"),
        "es kam ein Klartext-Rumpf durch, obwohl weitergeleitet wurde"
    );
}

/// SCHLEIFENSCHUTZ: eine Weiterleitung auf sich selbst, und zwei, die sich
/// gegenseitig meinen.
#[test_case]
fn test_weiterleitungs_schleife() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    for pfad in ["/schleife", "/ringelreihen"] {
        let lauf = starten("holes", &["holes", &alloc::format!("{}{}", KLAR, pfad)]);
        zeigen(&alloc::format!("Schleife {}", pfad), &lauf);
        assert_eq!(lauf.code(), 5, "{}: haette abbrechen muessen", pfad);
        assert!(
            lauf.grund("schleife"),
            "{}: der Grund muss 'schleife' sein, nicht bloss 'zu viele'",
            pfad
        );
        // Eine Schleife muss SOFORT auffallen und nicht erst nach dem
        // Ausschoepfen des Zaehlers.
        assert!(
            lauf.dauer_ms < 30_000,
            "{}: {} ms — die Schleife wurde zu spaet erkannt",
            pfad,
            lauf.dauer_ms
        );
    }
}

/// ZAEHLER-GRENZE: zehn verschiedene Weiterleitungen sind zu viele.
#[test_case]
fn test_zu_viele_weiterleitungen() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let lauf = starten("holes", &["holes", &alloc::format!("{}/kette", KLAR)]);
    zeigen("zehn Weiterleitungen hintereinander", &lauf);
    assert_eq!(lauf.code(), 5);
    assert!(
        lauf.grund("zu-viele-weiterleitungen"),
        "der Zaehler haette greifen muessen"
    );
}

// ===========================================================================
// 3. ROBUSTHEIT — die Gegenstelle benimmt sich schlecht
// ===========================================================================

/// DAS GROESSENLIMIT: Ein Server, der ohne Ende sendet, wird abgeschnitten.
///
/// Ohne diese Grenze laedt der Prozess, bis der Heap voll ist — und dann
/// beendet ihn der `alloc_error_handler` mit Code 102. Das waere kein
/// Absturz, aber auch keine Antwort.
#[test_case]
fn test_groessenlimit_bricht_ab() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let lauf = starten(
        "holes",
        &["holes", &alloc::format!("{}/endlos", KLAR), "--max=100000", "--frist=20000"],
    );
    zeigen("endlos sendender Server, Limit 100 000 Byte", &lauf);

    assert_eq!(lauf.code(), 5, "haette am Limit abbrechen muessen");
    assert!(lauf.grund("zu-gross"), "der Grund muss das Limit sein");
    assert!(
        lauf.dauer_ms < 30_000,
        "{} ms — das Limit hat zu spaet gegriffen",
        lauf.dauer_ms
    );
    // Und der Speicher-Fehlerpfad wurde NICHT genommen:
    assert_ne!(lauf.code(), 102, "dem Prozess ist der Speicher ausgegangen");
}

/// DIE GEGENSTELLE KAPPT DIE LEITUNG MITTEN IM RUMPF.
///
/// Der Kopf kuendigt 512 KiB an, es kommen 4 KiB, dann RST. Der Klient muss
/// das MERKEN (der Rumpf ist kuerzer als angekuendigt) statt die 4 KiB fuer
/// die ganze Antwort zu halten.
#[test_case]
fn test_abbruch_mitten_im_strom() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let lauf = starten(
        "holes",
        &["holes", &alloc::format!("{}/abbruch", KLAR), "--frist=20000"],
    );
    zeigen("Server kappt die Leitung mitten im Rumpf", &lauf);

    assert_ne!(lauf.code(), 0, "ein abgeschnittener Rumpf ist kein Erfolg");
    assert!(
        lauf.grund("http") || lauf.grund("verbindung") || lauf.grund("frist"),
        "der Abbruch muss als Fehler ankommen"
    );
    assert!(
        lauf.dauer_ms < 40_000,
        "{} ms — der Abbruch haette schneller auffallen muessen",
        lauf.dauer_ms
    );
}

/// HANDSHAKE-FRIST: Die Gegenstelle nimmt an und schweigt.
///
/// TCP steht, aber es kommt nie ein ServerHello. Ohne Frist wartet ein
/// TLS-Klient hier fuer immer.
#[test_case]
fn test_handshake_frist() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    let lauf = starten("holes", &["holes", STUMM, "--frist=8000"]);
    zeigen("Gegenstelle nimmt an und schweigt", &lauf);

    // Lauscht dort gar nichts, scheitert schon `verbinde` — und zwar
    // SOFORT. Das ist ein anderer Fall und kein Ergebnis fuer diesen Test.
    if lauf.dauer_ms < 2_000 {
        serial_println!("  (uebersprungen: auf {} lauscht nichts)", STUMM);
        return;
    }
    assert_ne!(lauf.code(), 0, "das haette nicht gelingen duerfen");
    assert!(
        lauf.grund("frist") || lauf.grund("handshake-abgebrochen"),
        "die Frist haette greifen muessen"
    );
    // Die Frist war 8000 ms — sie muss ungefaehr eingehalten worden sein und
    // nicht erst von einer Frist weiter oben aufgefangen.
    assert!(
        lauf.dauer_ms >= 7_000,
        "{} ms — zu frueh aufgegeben, das war nicht die Frist",
        lauf.dauer_ms
    );
    assert!(
        lauf.dauer_ms < 40_000,
        "{} ms — die 8-Sekunden-Frist wurde nicht eingehalten",
        lauf.dauer_ms
    );
}

/// DNS LIEFERT NICHTS: ein Name, den es per Norm nicht geben kann.
#[test_case]
fn test_dns_ohne_antwort() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    let lauf = starten("holes", &["holes", KEIN_NAME]);
    zeigen("DNS-Name existiert nicht", &lauf);

    assert_eq!(lauf.code(), 3, "ein DNS-Fehler ist ein Netzfehler");
    assert!(lauf.grund("dns"), "der Grund muss DNS sein");
    // Der Resolver versucht es dreimal a 1,2 s (Serie-5-Abschluss) — mehr
    // als das Doppelte davon waere ein Haenger.
    assert!(
        lauf.dauer_ms < 30_000,
        "{} ms fuer eine DNS-Absage",
        lauf.dauer_ms
    );
}

/// DIE VERBINDUNG GEHT WAEHREND DES DOWNLOADS VERLOREN.
///
/// Kein Server-Trick, sondern das Kabel: `geraet::verlust_setzen(100)`
/// verwirft ab sofort jedes Paket in beide Richtungen (dieselbe Mechanik
/// wie in tests/netz_abschluss.rs). Der laufende Prozess muss in seiner
/// Frist aufgeben — und der Kernel danach weiterarbeiten koennen.
#[test_case]
fn test_kabel_weg_waehrend_des_downloads() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("holes");
    let url = alloc::format!("{}/gross.bin", KLAR);
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid = prozess::prozess_starten_mit(
        &pfad,
        &["holes", &url, "--frist=8000"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("holes starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    // Kurz laufen lassen, dann das Kabel ziehen.
    let bis = zeit::ms_seit_boot() + 300;
    while zeit::ms_seit_boot() < bis {
        netz::pumpen();
        zeit::warte_auf_interrupt();
    }
    serial_println!("  Kabel ab (100 % Verlust) waehrend des Downloads ...");
    netz::geraet::verlust_setzen(100);

    let mut gesammelt = Vec::new();
    let mut puffer = alloc::vec![0u8; 4096];
    let mut ende = None;
    let start = zeit::ms_seit_boot();
    let frist = start + TEST_FRIST_MS;
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
                netz::pumpen();
                zeit::warte_auf_interrupt();
            }
            _ => break,
        }
    }
    let dauer = zeit::ms_seit_boot() - start;
    if ende.is_none() {
        ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 10_000));
    }
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();

    // Kabel wieder dran — die folgenden Tests brauchen Netz.
    netz::geraet::verlust_setzen(0);

    let ausgabe = String::from_utf8_lossy(&gesammelt).into_owned();
    serial_println!("  === Kabel weg ({} ms) ===", dauer);
    for zeile in ausgabe.lines() {
        serial_println!("  | {}", zeile);
    }
    serial_println!("  === Ende: {:?} ===", ende);

    assert!(ende.is_some(), "der Prozess haengt — er hat nie geendet");
    assert_ne!(
        ende.map(|e| e.code()),
        Some(0),
        "ein unterbrochener Download ist kein Erfolg"
    );
    assert!(
        dauer < 40_000,
        "{} ms — die 8-Sekunden-Frist wurde nicht eingehalten",
        dauer
    );
    // Und der Kernel lebt: ein Abruf gleich danach muss wieder gehen.
    assert!(
        netz::http::holen(&alloc::format!("{}/klein.txt", KLAR)).is_ok(),
        "nach dem Link-up geht nichts mehr — der Stack hat den Abriss nicht verkraftet"
    );
    serial_println!("  Nach dem Link-up laeuft der Stack wieder.");
}

// ===========================================================================
// 3b. DER FLUECHTIGE FEHLSCHLAG — gemessen statt geraten
// ===========================================================================

/// WIE OFT liefert eine frische Verbindung null Bytes?
///
/// ==========================================================================
/// WARUM DIESER TEST EXISTIERT
///
/// Beim Robustheits-Pass fiel auf, dass in schnellen Abrufserien gelegentlich
/// eine Verbindung ANGENOMMEN wird und dann sofort ohne ein einziges Byte
/// endet. Vom Host aus ist derselbe Server in 15 von 15 Versuchen
/// einwandfrei — es liegt also an uns oder an QEMUs slirp dazwischen.
///
/// `libspeed::netz` wiederholt diesen einen Fall deshalb (siehe
/// `Klient::wiederholungen`). Eine Wiederholung, die man nicht MISST, ist
/// aber bloss ein Teppich, unter den man kehrt. Also wird hier gezaehlt —
/// und zwar mit dem KERNEL-Klienten, ganz ohne Prozesse und ohne TLS. Damit
/// beantwortet der Test die eigentliche Frage: Liegt es am Prozess-Pfad
/// (Handle-Tabelle, Adressraum-Abbau) oder am Stack selbst?
///
/// Der Test SCHLAEGT NICHT FEHL, solange die Rate klein bleibt — er
/// berichtet. Steigt sie, ist das eine Regression und der Wert faellt auf.
/// ==========================================================================
#[test_case]
fn test_fluechtige_fehlschlaege_zaehlen() {
    if !server_da() {
        return;
    }
    let url = alloc::format!("{}/klein.txt", KLAR);
    let versuche = 30u32;
    let mut leer = 0u32;
    let mut andere = 0u32;
    for _ in 0..versuche {
        match netz::http::holen(&url) {
            Ok((_, antwort)) if antwort.status == 200 && !antwort.rumpf.is_empty() => {}
            Ok((_, antwort)) => {
                andere += 1;
                serial_println!("    unerwartet: Status {}", antwort.status);
            }
            Err(fehler) => {
                // Der Kernel-Klient meldet „kein gueltiger Kopf", wenn gar
                // nichts ankam — dieselbe Lage wie `LeereAntwort` in Ring 3.
                if matches!(
                    fehler,
                    netz::http::KlientFehler::Http(netz::http::HttpFehler::KaputterKopf)
                ) {
                    leer += 1;
                } else {
                    andere += 1;
                    serial_println!("    Fehler: {}", fehler.meldung());
                }
            }
        }
    }
    serial_println!(
        "  KERNEL-Klient, {} schnelle Abrufe: {} leere Antworten, {} andere Fehler.",
        versuche,
        leer,
        andere
    );
    if leer == 0 {
        serial_println!(
            "  -> Der Stack selbst ist sauber; der fluechtige Fehlschlag haengt \
             am Prozess-Pfad (Socket-Abbau beim Prozess-Ende)."
        );
    } else {
        serial_println!(
            "  -> Auch OHNE Prozesse: {} von {} — es liegt am Stack oder an slirp, \
             nicht am Prozess-Abbau.",
            leer,
            versuche
        );
    }
    // Die Schranke ist grosszuegig und trotzdem eine Aussage: Mehr als ein
    // Fuenftel waere kein „fluechtiger" Fehler mehr, sondern ein kaputter
    // Stack.
    assert!(
        leer * 5 < versuche,
        "{} von {} Abrufen lieferten NICHTS — das ist keine Fluechtigkeit mehr",
        leer,
        versuche
    );
    assert_eq!(andere, 0, "unerwartete Fehler neben den leeren Antworten");
}

// ===========================================================================
// 4. DER LECK-TEST
// ===========================================================================

/// Nach vielen Abrufen — erfolgreichen UND gescheiterten — ist nichts uebrig.
///
/// GEPRUEFT WIRD DREIERLEI: Sockets (die Handle-Tabelle des Prozesses muss
/// beim Ende alles schliessen, auch nach einem Fehler), Frames (der
/// Adressraum muss ganz zurueckfliessen) und der Kernel-Heap.
///
/// ZWEI BEKANNTE UNSCHAERFEN, benannt statt wegdefiniert:
///  * `memory::allocate_pages` laesst alle 512 Seiten eine P1-Tabelle im
///    Kernel-Adressraum zurueck (CLAUDE.md, Serie-6-Abschluss).
///  * Der Log-Puffer (protokoll.rs) waechst mit jeder Ausgabe bis 64 KiB;
///    er wird herausgerechnet und BENANNT.
#[test_case]
fn test_kein_leck_ueber_viele_abrufe() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    zufall_bereitmachen();

    // Aufraeumen und Ruhe herstellen, BEVOR gemessen wird.
    for _ in 0..20 {
        netz::pumpen();
        zeit::warte_auf_interrupt();
    }
    scheduler::aufraeumen();
    let sockets_vorher = netz::socket::anzahl();
    let frames_vorher = memory::frame_statistik().0;
    let log_vorher = speed_os::protokoll::puffer_bytes();

    // Eine Mischung aus Erfolg und jeder Sorte Fehler — gerade die
    // Fehlerwege sind es, auf denen ein Handle liegen bleibt.
    let runden = 6;
    for runde in 0..runden {
        let lauf = starten(
            "holes",
            &["holes", &alloc::format!("{}/klein.txt", KLAR), "--still"],
        );
        assert_eq!(lauf.code(), 0, "Runde {}: der Abruf ging schief", runde);
        starten("holes", &["holes", &alloc::format!("{}/schleife", KLAR)]);
        starten(
            "holes",
            &["holes", &alloc::format!("{}/abbruch", KLAR), "--frist=6000"],
        );
    }

    for _ in 0..40 {
        netz::pumpen();
        zeit::warte_auf_interrupt();
    }
    scheduler::aufraeumen();
    let sockets_nachher = netz::socket::anzahl();
    let frames_nachher = memory::frame_statistik().0;
    let log_nachher = speed_os::protokoll::puffer_bytes();

    serial_println!(
        "  {} Runden a 3 Abrufe: Sockets {} -> {}, freie Frames {} -> {}.",
        runden,
        sockets_vorher,
        sockets_nachher,
        frames_vorher,
        frames_nachher
    );
    serial_println!(
        "  (Log-Puffer wuchs dabei von {} auf {} Byte — herausgerechnet.)",
        log_vorher,
        log_nachher
    );

    // SOCKETS: byte-exakt. Ein Prozess, der endet, gibt ALLES zurueck —
    // dafuer steckt die Handle-Tabelle im Prozess-Kontrollblock.
    assert_eq!(
        sockets_nachher, sockets_vorher,
        "es sind Sockets uebrig geblieben"
    );

    // FRAMES: die ausgerechnete Schranke, keine aufgeweichte Bilanz.
    // Je Prozess ~340 Frames; 18 Prozesse also ~6120 Seiten virtuellen
    // Raums, was hoechstens 12 P1-Tabellen zurueckliegen laesst. Plus der
    // Log-Puffer, der in Seiten gerechnet wird.
    let log_seiten = (log_nachher.saturating_sub(log_vorher)).div_ceil(4096) as u64;
    let schranke = 16 + log_seiten;
    let verlust = frames_vorher.saturating_sub(frames_nachher);
    serial_println!(
        "  Frame-Differenz {} (erlaubt: {} = 12 P1-Tabellen + {} Log-Seiten + Reserve).",
        verlust,
        schranke,
        log_seiten
    );
    assert!(
        verlust <= schranke as usize,
        "{} Frames nach {} Prozessen verloren — das sieht nach einem Leck aus",
        verlust,
        runden * 3
    );
}

// ===========================================================================
// 5. `news` — der Beweis, dass die Schicht traegt
// ===========================================================================

/// Ein zweites Programm, dieselbe Schicht, ein voellig anderes Ergebnis.
#[test_case]
fn test_news_zeigt_text_statt_tags() {
    if !programme_vorhanden() || !server_da() {
        return;
    }
    let lauf = starten("news", &["news", &alloc::format!("{}/html", KLAR)]);
    zeigen("news gegen die Testseite", &lauf);

    assert_eq!(lauf.code(), 0);
    // Der Titel wurde gefunden und die Entity aufgeloest.
    assert!(
        lauf.ausgabe.contains("SpeedOS - Testseite")
            || lauf.ausgabe.contains("SpeedOS &ndash; Testseite"),
        "der Titel fehlt"
    );
    assert!(lauf.ausgabe.contains("Willkommen bei SpeedOS"), "die Ueberschrift fehlt");
    // Umlaut-Entities sind aufgeloest ...
    assert!(
        lauf.ausgabe.contains("über") || lauf.ausgabe.contains("ueber"),
        "die Entities wurden nicht aufgeloest"
    );
    // ... und die STUMMEN BLOECKE sind weg, mitsamt Inhalt. Das ist die
    // Entscheidung, die eine naive Tag-Entfernung nicht trifft.
    assert!(
        !lauf.ausgabe.contains("var geheim"),
        "der Inhalt von <script> steht im Text"
    );
    assert!(
        !lauf.ausgabe.contains("color: #123"),
        "der Inhalt von <style> steht im Text"
    );
    // Und keine Tags:
    assert!(!lauf.ausgabe.contains("<h1>"), "es sind Tags uebrig");
}
