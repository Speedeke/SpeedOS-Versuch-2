// tests/html.rs — DER HTML-PARSER IN RING 3 (Serie 8, Teil 4)
//
// ===========================================================================
// WAS HIER GEPRUEFT WIRD — UND WAS AUSDRUECKLICH WOANDERS
//
// Die PARSER-LOGIK wird in `speedhtml` auf dem HOST geprueft: 63 Tests in
// 0,6 Sekunden, darunter 20 MB Muell in fuenf Varianten, ein
// Wikipedia-Artikel und die Zeichenreferenz-Tabelle. Sie hier zu
// wiederholen waere Verschwendung — jeder Fall kostete einen QEMU-Start.
//
// Hier wird das geprueft, was der Host NICHT zeigen kann:
//
//   1. `speedhtml` uebersetzt und laeuft BARE-METAL, no_std, in Ring 3.
//   2. `htmldump` findet seine Datei, parst sie und schreibt das Ergebnis
//      durch eine PIPE — der ganze Weg also, nicht nur die Funktion.
//   3. Ein Ring-3-Prozess mit 12 MiB Heap kommt mit einem echten Dokument
//      zurecht.
//   4. Kaputte Eingaben lassen den Prozess nicht sterben (Exit 101).
//
// Das ist dieselbe Arbeitsteilung wie bei `speedui` (Toolkit-Logik auf dem
// Host, `uidemo` in QEMU) und bei `libspeed::bild` (Dekoder-Logik in
// tests/bilder.rs, Betrachter von Hand).

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
use speed_os::{allocator, fs, memory, pipe, programme, scheduler, serial_println, zeit};
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

    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();

    fs::init();
    fs::platte_automounten();
    programme::installieren();
    programme::testseite_installieren();

    scheduler::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

const FRIST_MS: u64 = 30_000;
/// Exit-Code des Panic-Handlers von libspeed. DARF NIE VORKOMMEN.
const CODE_PANIK: i32 = 101;

struct Lauf {
    ausgabe: String,
    ende: Option<ProzessEnde>,
    dauer_ms: u64,
}

impl Lauf {
    fn code(&self) -> i32 {
        match self.ende {
            Some(ende) => ende.code() as i32,
            None => -1,
        }
    }
    fn zeilen(&self) -> usize {
        self.ausgabe.lines().count()
    }
}

/// Startet `htmldump` mit den gegebenen Argumenten und sammelt die Ausgabe.
fn htmldump(argumente: &[&str]) -> Lauf {
    let start = zeit::ms_seit_boot();
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("htmldump");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);

    // argv[0] gehoert dazu — `prozess_starten_mit` stellt den Namen nicht
    // selbst voran.
    let mut argv: Vec<&str> = alloc::vec!["htmldump"];
    argv.extend_from_slice(argumente);

    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        &argv,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("'htmldump' starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let mut gesammelt: Vec<u8> = Vec::new();
    let mut puffer = alloc::vec![0u8; 8192];
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    serial_println!("  !! Frist abgelaufen — htmldump haengt.");
                    break;
                }
                if ende.is_none() {
                    ende = scheduler::ende_abfragen(pid);
                }
                scheduler::aufraeumen();
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

    Lauf {
        ausgabe: String::from_utf8_lossy(&gesammelt).into_owned(),
        ende,
        dauer_ms: zeit::ms_seit_boot() - start,
    }
}

/// Schreibt eine Datei aufs Dateisystem (fuer die selbstgebauten Faelle).
fn datei_schreiben(pfad: &str, inhalt: &str) {
    fs::mit_fs(|f| f.schreiben(pfad, inhalt.as_bytes()))
        .unwrap_or_else(|fehler| panic!("{} schreiben: {:?}", pfad, fehler));
}

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

// ---------------------------------------------------------------------------
// 1. Die Testseite liegt da und laesst sich parsen
// ---------------------------------------------------------------------------

/// DER MEILENSTEIN DIESES TEILS: Ein Ring-3-Prozess parst die erste
/// Webseite der Welt und gibt ihren Baum aus.
#[test_case]
fn test_erste_webseite_der_welt() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        serial_println!("  (uebersprungen: keine Testseite eingebettet)");
        return;
    }
    let pfad = programme::testseite_pfad();
    let lauf = htmldump(&[&pfad]);

    assert_ne!(lauf.code(), CODE_PANIK, "htmldump ist gepanickt:\n{}", lauf.ausgabe);
    assert_eq!(lauf.code(), 0, "Ausgabe:\n{}", lauf.ausgabe);

    // Der Baum ist da und hat Struktur.
    assert!(lauf.zeilen() > 40, "nur {} Zeilen Ausgabe", lauf.zeilen());
    assert!(lauf.ausgabe.contains("<title>"), "kein <title>:\n{}", lauf.ausgabe);
    assert!(lauf.ausgabe.contains("<h1>"), "kein <h1>");
    // `href=` und nicht `<a href=`: Die Seite von 1991 schreibt
    // `<A NAME=0 HREF="WhatIs.html">` — das ZIEL ist also nicht das erste
    // Attribut. (Genau die Sorte Annahme, die eine echte Seite widerlegt.)
    assert!(lauf.ausgabe.contains("href="), "keine Links mit Ziel");
    assert!(
        lauf.ausgabe.contains("World Wide Web"),
        "der Text der Seite fehlt"
    );

    // Und der Befund: Die Seite von 1991 IST nach heutigen Massstaeben
    // kaputt — der Parser muss das gemeldet haben.
    assert!(
        lauf.ausgabe.contains("offen") || lauf.ausgabe.contains("implizit"),
        "kein Befund zu einer Seite, die keinen einzigen </P> hat:\n{}",
        lauf.ausgabe
    );

    serial_println!(
        "  Erste Webseite der Welt: {} Zeilen Baum in {} ms (Ring 3).",
        lauf.zeilen(),
        lauf.dauer_ms
    );
}

/// `--tags` zaehlt, `--text` extrahiert, `--befund` fasst zusammen.
#[test_case]
fn test_die_drei_betriebsarten() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();

    let tags = htmldump(&[&pfad, "--tags"]);
    assert_eq!(tags.code(), 0);
    assert!(tags.ausgabe.contains(" a\n") || tags.ausgabe.contains("  a"), "kein <a> gezaehlt:\n{}", tags.ausgabe);

    let text = htmldump(&[&pfad, "--text"]);
    assert_eq!(text.code(), 0);
    assert!(text.ausgabe.contains("World Wide Web"));
    // Im reinen Text steht KEIN Markup mehr.
    assert!(!text.ausgabe.contains("<a href"), "Markup im Text-Modus");

    let befund = htmldump(&[&pfad, "--befund"]);
    assert_eq!(befund.code(), 0);
    assert!(befund.ausgabe.contains("Knoten"));
    // Der Befund ist kurz — das ist sein Zweck.
    assert!(befund.zeilen() < 12, "Befund zu lang: {}", befund.zeilen());

    serial_println!("  --tags / --text / --befund liefern alle drei etwas.");
}

// ---------------------------------------------------------------------------
// 2. Die fiesen Faelle — hier nur als Ring-3-Durchlauf
// ---------------------------------------------------------------------------

/// Kaputte Dokumente duerfen den PROZESS nicht toeten.
///
/// Die Faelle selbst sind in `speedhtml` einzeln geprueft; hier zaehlt
/// nur, dass keiner davon in Ring 3 anders ausgeht — insbesondere, dass
/// keiner den Panic-Handler von libspeed erreicht.
#[test_case]
fn test_kaputte_dokumente_toeten_den_prozess_nicht() {
    if !programme_vorhanden() {
        return;
    }
    let faelle: &[(&str, &str)] = &[
        ("leer.html", ""),
        ("nur_spitz.html", "<<<<<<<<<<"),
        ("nie_geschlossen.html", "<div><section><article><p>Text"),
        ("verschachtelt.html", "<p>eins<p>zwei<p>drei"),
        ("tabelle.html", "<table><tr><td>a<td>b<tr><td>c<td>d</table>"),
        ("nackte_attribute.html", "<a href=/x/y class=k>Text</a>"),
        ("kleiner_im_text.html", "<p>5 < 7 und 3 > 1</p>"),
        ("kleiner_im_tag.html", "<p>a<b</p>"),
        ("skript.html", "<script>if (a < b) { x = '</p>'; }</script><p>da</p>"),
        ("kommentar_offen.html", "<p>x<!-- nie zu Ende"),
        ("abgeschnitten.html", "<html><body><p>Text</p><div class=\"unvoll"),
        ("umlaute.html", "<p title=\"Schöße\">Grüße aus München &uuml;</p>"),
        ("endtags.html", "</p></div></span><p>x</p></b>"),
    ];

    let ordner = fs::persistenter_pfad("/platte/htmltest", "/htmltest");
    let _ = fs::mit_fs(|f| f.mkdir(ordner));

    for (name, inhalt) in faelle {
        let pfad = fs::pfad_anhaengen(ordner, name);
        datei_schreiben(&pfad, inhalt);
        let lauf = htmldump(&[&pfad]);
        assert_ne!(
            lauf.code(),
            CODE_PANIK,
            "{} hat htmldump zum PANICKEN gebracht:\n{}",
            name,
            lauf.ausgabe
        );
        assert!(lauf.ende.is_some(), "{} hat htmldump haengen lassen", name);
        assert_eq!(lauf.code(), 0, "{}: Exit {}", name, lauf.code());
    }
    serial_println!("  {} kaputte Dokumente, 0 Paniken.", faelle.len());
}

/// Die Fehlererholung liefert in Ring 3 dieselben Ergebnisse wie auf dem
/// Host — Stichprobe an den drei Faellen, die am meisten schiefgehen
/// koennen.
#[test_case]
fn test_fehlererholung_in_ring3() {
    if !programme_vorhanden() {
        return;
    }
    let ordner = fs::persistenter_pfad("/platte/htmltest", "/htmltest");
    let _ = fs::mit_fs(|f| f.mkdir(ordner));

    // (a) Verschachtelte <p> werden zu Geschwistern — also drei <p> auf
    //     derselben Einrueckung.
    let pfad = fs::pfad_anhaengen(ordner, "p.html");
    datei_schreiben(&pfad, "<p>eins<p>zwei<p>drei");
    let lauf = htmldump(&[&pfad]);
    let einrueckungen: Vec<usize> = lauf
        .ausgabe
        .lines()
        .filter(|z| z.trim_start().starts_with("<p>"))
        .map(|z| z.len() - z.trim_start().len())
        .collect();
    assert_eq!(einrueckungen.len(), 3, "nicht drei <p>:\n{}", lauf.ausgabe);
    assert!(
        einrueckungen.iter().all(|t| *t == einrueckungen[0]),
        "die <p> sind verschachtelt statt Geschwister:\n{}",
        lauf.ausgabe
    );

    // (b) Skript-Inhalt taucht im Text NICHT auf.
    let pfad = fs::pfad_anhaengen(ordner, "s.html");
    datei_schreiben(&pfad, "<p>sichtbar</p><script>var geheim = 42;</script>");
    let lauf = htmldump(&[&pfad, "--text"]);
    assert!(lauf.ausgabe.contains("sichtbar"));
    assert!(
        !lauf.ausgabe.contains("geheim"),
        "Skript-Inhalt im Text:\n{}",
        lauf.ausgabe
    );

    // (c) Zeichenreferenzen sind aufgeloest.
    let pfad = fs::pfad_anhaengen(ordner, "e.html");
    datei_schreiben(&pfad, "<p>Gr&uuml;&szlig;e &amp; 5 &lt; 7</p>");
    let lauf = htmldump(&[&pfad, "--text"]);
    assert!(
        lauf.ausgabe.contains("Grüße & 5 < 7"),
        "Referenzen nicht aufgeloest:\n{}",
        lauf.ausgabe
    );

    serial_println!("  Fehlererholung verhaelt sich in Ring 3 wie auf dem Host.");
}

// ---------------------------------------------------------------------------
// 3. Speicher
// ---------------------------------------------------------------------------

/// Zehn Durchlaeufe lecken nichts.
#[test_case]
fn test_kein_leck() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    scheduler::aufraeumen();
    let vorher = memory::frame_statistik().0;

    for _ in 0..10 {
        let lauf = htmldump(&[&pfad, "--befund"]);
        assert_eq!(lauf.code(), 0);
    }
    scheduler::aufraeumen();

    let nachher = memory::frame_statistik().0;
    // Dieselbe P1-Schranke wie in den anderen Prozess-Tests: ~340 Seiten
    // je Prozess, eine P1-Tabelle je 512 Seiten.
    let schranke = (10 * 340) / 512 + 2;
    let verloren = vorher.saturating_sub(nachher);
    serial_println!(
        "  10 htmldump-Prozesse: {} Frames verloren (Schranke {}).",
        verloren,
        schranke
    );
    assert!(verloren <= schranke, "Frame-Leck: {verloren} > {schranke}");
}

/// Eine fehlende Datei ist ein sauberer Fehler, keine Panik.
#[test_case]
fn test_fehlende_datei() {
    if !programme_vorhanden() {
        return;
    }
    let lauf = htmldump(&["/platte/gibt-es-nicht-99999.html"]);
    assert_ne!(lauf.code(), CODE_PANIK);
    assert_ne!(lauf.code(), 0, "eine fehlende Datei muss ein Fehler sein");
    serial_println!("  Fehlende Datei: Exit {} (kein Absturz).", lauf.code());
}
