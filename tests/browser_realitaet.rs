// tests/browser_realitaet.rs — ZEHN ECHTE SEITEN, EHRLICH BEWERTET
//
// ===========================================================================
// WARUM DIESER TEST KEINE ZUSAGEN PRUEFT
//
// Er ist ein BERICHT. Er behauptet nicht „Wikipedia funktioniert", sondern
// holt zehn echte Seiten und schreibt auf, was dabei herauskommt: wie viele
// Knoten, wie viele Anzeige-Befehle, ob sichtbarer Text entsteht, ob der
// JavaScript-Hinweis greift, wie lange es dauert.
//
// Aus diesen Zahlen entsteht docs/browser-realitaet.md — und DIE Liste ist
// wertvoller als jede Feature-Aufzaehlung: Sie sagt, was der Browser
// wirklich kann, und zwar an Seiten, die niemand fuer uns gebaut hat.
//
// ===========================================================================
// DIE METHODIK — dieselbe wie bei den Netz-Tests seit Serie 5
//
// Das HARTE Gate liegt auf dem, was wir kontrollieren (tests/browser.rs,
// tests/browser_boesartig.rs). Dieser Lauf haengt an fremden Servern und
// am Internet des Hosts; er wird deshalb SAUBER UEBERSPRUNGEN, wenn nichts
// erreichbar ist, und er laesst den Testlauf NIE rot werden, wenn eine
// fremde Seite sich geaendert hat.
//
// Die eine Ausnahme steht am Ende: Nach zehn fremden Seiten muss das
// System noch leben. DAS ist eine Zusage, und die gilt unabhaengig davon,
// was die Server geantwortet haben.

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
use speed_os::{allocator, fenster, fs, memory, netz, pipe, programme, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    speed_os::zufall::init();
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(4096).expect("Heap-Erweiterung fehlgeschlagen");

    speed_os::pci::init();
    speed_os::virtio::blk::init();
    speed_os::virtio::net::init();
    fs::init();
    programme::installieren();
    programme::ca_buendel_installieren();
    programme::testseite_installieren();
    netz::dhcp::autokonfig(5000);
    scheduler::init();

    if let Some(fb) = framebuffer {
        speed_os::framebuffer::init(fb);
    }
    assert!(
        fenster::manager_fuer_test_starten(),
        "der Browser braucht einen Fenster-Manager"
    );

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

const FRIST_MS: u64 = 120_000;

/// Die zehn Seiten. Bewusst gemischt: alte und neue, schlanke und
/// aufgeblasene, Text und Anwendung.
const SEITEN: &[(&str, &str)] = &[
    ("http://info.cern.ch/hypertext/WWW/TheProject.html", "Die erste Webseite (1991)"),
    ("https://example.com/", "example.com (Referenz)"),
    ("https://de.wikipedia.org/wiki/Betriebssystem", "Wikipedia (Enzyklopaedie)"),
    ("https://danluu.com/", "danluu.com (Blog, schlichtes HTML)"),
    ("https://text.npr.org/", "text.npr.org (Nachrichten, Textfassung)"),
    ("https://lite.cnn.com/", "lite.cnn.com (Nachrichten, schlank)"),
    ("https://news.ycombinator.com/", "Hacker News (Forum)"),
    ("https://doc.rust-lang.org/book/ch01-00-getting-started.html", "Rust-Buch (Doku)"),
    ("https://suckless.org/", "suckless.org (Projektseite)"),
    ("https://github.com/", "github.com (Anwendung, JS-lastig)"),
];

/// Wartet, bis der Zufallsgenerator gesaet ist.
///
/// NOETIG, WEIL DER TESTKERNEL KEINEN NACHSAAT-TASK HAT: Im Betrieb
/// erledigt das `zufall::nachsaat_task` (main.rs, alle 5 s). Ohne das
/// liefert der `zufall`-Syscall nach 10 s `NichtGesaet` — und JEDE
/// TLS-Verbindung scheitert mit `kein-zufall`, was in diesem Bericht wie
/// „die Seite ist kaputt" aussaehe. Dieselbe Vorbereitung wie in
/// tests/tlsspike.rs und tests/netz_https.rs.
fn zufall_bereitmachen() -> bool {
    let frist = zeit::ms_seit_boot() + 30_000;
    while !speed_os::zufall::bereit() && zeit::ms_seit_boot() < frist {
        if speed_os::zufall::status().entropie_bits >= speed_os::zufall::SCHWELLE_BITS {
            speed_os::zufall::nachsaeen();
        }
        netz::pumpen();
        zeit::warte_auf_interrupt();
    }
    if !speed_os::zufall::bereit() {
        serial_println!("  (uebersprungen: der Zufallsgenerator wurde nicht gesaet)");
        return false;
    }
    true
}

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Ist ueberhaupt Internet da? (Sonst hat dieser Bericht keinen Sinn.)
fn internet_da() -> bool {
    match netz::dns::aufloesen("example.com") {
        Ok(_) => true,
        Err(_) => {
            serial_println!(
                "  (uebersprungen: kein Internet — dieser Bericht braucht echte Server)"
            );
            false
        }
    }
}

fn browser_pruefen(adresse: &str) -> (Option<ProzessEnde>, String, u64) {
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("browser");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let start = zeit::ms_seit_boot();
    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        &["browser", adresse, "--pruefen", "--fenster=1200x700"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("browser starten: {}", fehler.meldung()));
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let mut gesammelt = Vec::new();
    let mut puffer = alloc::vec![0u8; 4096];
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    scheduler::beenden(pid);
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
    if ende.is_none() {
        ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 5_000));
    }
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    let dauer = zeit::ms_seit_boot().saturating_sub(start);
    (ende, String::from_utf8_lossy(&gesammelt).into_owned(), dauer)
}

fn feld<'a>(ausgabe: &'a str, schluessel: &str) -> &'a str {
    for zeile in ausgabe.lines() {
        if let Some(rest) = zeile.strip_prefix(schluessel) {
            return rest.trim();
        }
    }
    ""
}

fn zahl(ausgabe: &str, schluessel: &str) -> u64 {
    feld(ausgabe, schluessel).parse::<u64>().unwrap_or(0)
}

/// Die Bewertung — nach FESTEN Regeln, damit sie nicht Geschmack ist.
///
/// LESBAR       viel Text, kein JS-Hinweis, kein Fehler.
/// TEILWEISE    es kommt etwas an, aber wenig — oder mit Fehlern.
/// UNBRAUCHBAR  JS-Hinweis, Sicherheitsfehler, gar kein Inhalt.
fn bewerten(ausgabe: &str, ende: Option<ProzessEnde>) -> (&'static str, String) {
    if ende.is_none() {
        return ("UNBRAUCHBAR", String::from("kein Ende (Frist abgelaufen)"));
    }
    if ende != Some(ProzessEnde::Beendet(0)) {
        return (
            "UNBRAUCHBAR",
            alloc::format!("Prozess endete mit {:?}", ende),
        );
    }
    if feld(ausgabe, "UNSICHER=") == "1" {
        return (
            "UNBRAUCHBAR",
            alloc::format!("Sicherheitsfehler: {}", feld(ausgabe, "FEHLER=")),
        );
    }
    if feld(ausgabe, "JS_HINWEIS=") == "1" {
        return (
            "UNBRAUCHBAR",
            String::from("leer ohne JavaScript (Hinweis wurde gezeigt)"),
        );
    }
    if feld(ausgabe, "ZUSTAND=") == "fehler" {
        return (
            "UNBRAUCHBAR",
            alloc::format!("Ladefehler: {}", feld(ausgabe, "FEHLER=")),
        );
    }
    let befehle = zahl(ausgabe, "BEFEHLE=");
    let hoehe = zahl(ausgabe, "HOEHE=");
    if befehle >= 200 && hoehe >= 500 {
        (
            "LESBAR",
            alloc::format!("{} Anzeige-Befehle, {} px hoch", befehle, hoehe),
        )
    } else if befehle >= 20 {
        (
            "TEILWEISE",
            alloc::format!("nur {} Anzeige-Befehle, {} px hoch", befehle, hoehe),
        )
    } else {
        (
            "UNBRAUCHBAR",
            alloc::format!("fast nichts angekommen ({} Befehle)", befehle),
        )
    }
}

// ===========================================================================

/// **DER REALITAETS-BERICHT.** Zehn echte Seiten, eine Tabelle.
#[test_case]
fn bericht_zehn_echte_seiten() {
    if !programme_vorhanden() || !zufall_bereitmachen() || !internet_da() {
        return;
    }

    serial_println!("");
    serial_println!("=========================================================");
    serial_println!("  DER REALITAETS-BERICHT — zehn echte Seiten");
    serial_println!("=========================================================");

    let mut lesbar = 0;
    let mut teilweise = 0;
    let mut unbrauchbar = 0;

    for (adresse, name) in SEITEN {
        let (ende, ausgabe, dauer) = browser_pruefen(adresse);
        let (urteil, grund) = bewerten(&ausgabe, ende);
        match urteil {
            "LESBAR" => lesbar += 1,
            "TEILWEISE" => teilweise += 1,
            _ => unbrauchbar += 1,
        }
        serial_println!("");
        serial_println!("[REALITAET] {} — {}", name, adresse);
        serial_println!(
            "[REALITAET]   {:<12} {} ({} ms)",
            urteil,
            grund,
            dauer
        );
        serial_println!(
            "[REALITAET]   Titel: '{}' | Ort: {}",
            feld(&ausgabe, "TITEL="),
            feld(&ausgabe, "ORT=")
        );
        serial_println!(
            "[REALITAET]   Befehle {} | Hoehe {} px | Links {} | Heap-Spitze {} B",
            zahl(&ausgabe, "BEFEHLE="),
            zahl(&ausgabe, "HOEHE="),
            zahl(&ausgabe, "LINKS="),
            zahl(&ausgabe, "HEAP_SPITZE=")
        );
        // ===============================================================
        // DIE ZAHLEN DER ZWEITEN MESSUNG (Serie 9, Teil 1)
        //
        // Sie gehen NICHT in die Bewertung ein — die Methode bleibt
        // Wort fuer Wort dieselbe wie in der ersten Messung, sonst waere
        // der Vergleich wertlos. Sie sind der BELEG dafuer, WARUM sich
        // ein Urteil geaendert hat: `VERSTECKT` sagt, wie viele
        // Teilbaeume durch die geholten Blaetter aus der Seite gefallen
        // sind, und das ist genau der Unterschied zwischen „github zeigt
        // Screenreader-Text" und „github zeigt ihn nicht mehr".
        serial_println!(
            "[REALITAET]   CSS: {}/{} Blaetter, {} Regeln, {} B, {} Teilbaeume versteckt{}",
            zahl(&ausgabe, "STIL_EXTERN_GELADEN="),
            zahl(&ausgabe, "STIL_EXTERN_VERSUCHT="),
            zahl(&ausgabe, "STIL_REGELN="),
            zahl(&ausgabe, "STIL_BYTES="),
            zahl(&ausgabe, "STIL_VERSTECKT="),
            if feld(&ausgabe, "STIL_MELDUNG=") == "-" {
                String::new()
            } else {
                alloc::format!(" | {}", feld(&ausgabe, "STIL_MELDUNG="))
            }
        );
        // Der ANFANG des sichtbaren Textes. Er beantwortet die Frage, an
        // der die mechanische Bewertung in der ersten Messung gescheitert
        // ist: Ein Browser kann viel Text zeichnen und trotzdem
        // unbenutzbar sein, wenn es der FALSCHE Text ist.
        let text = feld(&ausgabe, "TEXT=");
        // GEKUERZT WIRD NACH ZEICHEN, NICHT NACH BYTES. `&text[..160]`
        // waere der Klassiker: Der sichtbare Text kommt aus der
        // Anzeigeliste und damit woertlich aus einer fremden Seite —
        // Umlaute im Wikipedia-Artikel, typografische Anfuehrungszeichen
        // und Gedankenstriche bei NPR und CNN sind der Normalfall, nicht
        // der Randfall. Faellt die Bytegrenze mitten in ein solches
        // Zeichen, PANICKT das Slicing, und zwar im TESTKERNEL — ein
        // Bericht, der die Messung abbricht, ueber die er berichtet.
        // Dieselbe Regel wie bei `text_breite` (chars, nie len).
        let kurz: String = text.chars().take(160).collect();
        serial_println!("[REALITAET]   Text: {}", kurz);
        scheduler::aufraeumen();
    }

    serial_println!("");
    serial_println!(
        "[REALITAET] BILANZ: {} lesbar, {} teilweise, {} unbrauchbar (von {})",
        lesbar,
        teilweise,
        unbrauchbar,
        SEITEN.len()
    );

    // DIE EINZIGE ZUSAGE dieses Berichts: Nach zehn fremden Seiten laeuft
    // das System noch. Was die Server geantwortet haben, ist Bericht —
    // dass SpeedOS es ueberlebt, ist eine Zusage.
    let leitung = pipe::anlegen().expect("Pipe");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pfad = programme::pfad("hallo");
    let pid = prozess::prozess_starten_mit(
        &pfad,
        &["hallo"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("hallo starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);
    let ende = scheduler::warten_auf(pid, 10_000);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    assert_eq!(
        ende,
        Some(ProzessEnde::Beendet(0)),
        "nach zehn echten Seiten laeuft kein gewoehnlicher Prozess mehr"
    );
    serial_println!("[REALITAET] Das System lebt nach zehn fremden Seiten.");
}
