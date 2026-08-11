// tests/html.rs — DAS BROWSER-FUNDAMENT IN RING 3 (Serie 8, Teil 4-6)
//
// ===========================================================================
// WAS HIER GEPRUEFT WIRD — UND WAS AUSDRUECKLICH WOANDERS
//
// Die PARSER- und KASKADEN-LOGIK wird auf dem HOST geprueft: 63 Tests in
// `speedhtml` und 56 in `speedcss`, zusammen unter einer Sekunde — darunter
// 20 MB Muell, ein Wikipedia-Artikel, die Zeichenreferenz-Tabelle,
// Spezifitaet, Vererbung und `!important`. Sie hier zu wiederholen waere
// Verschwendung: jeder Fall kostete einen QEMU-Start.
//
// Hier wird das geprueft, was der Host NICHT zeigen kann:
//
//   1. `speedhtml`, `speedcss` UND `speedlayout` uebersetzen und laufen
//      BARE-METAL, no_std, in Ring 3.
//   2. `htmldump` und `cssdump` finden ihre Datei, verarbeiten sie und
//      schreiben das Ergebnis durch eine PIPE — der ganze Weg also.
//   3. Ein Ring-3-Prozess mit 12 MiB Heap kommt mit einem echten Dokument
//      und dem Standard-Stylesheet zurecht.
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
    // 8 MiB, nicht 2: `programme::installieren()` liest beim Boot JEDE
    // vorhandene Datei ganz in den Heap, um sie mit der eingebetteten
    // Fassung zu vergleichen. Mit `htmldump` (1,0 MB) und `cssdump`
    // (1,06 MB) reichten 2 MiB nicht mehr — der Testkernel starb mit
    // „Heap-Allokation fehlgeschlagen: size 1083792", also genau der
    // Groesse von cssdump. Dieselbe Zahl und derselbe Grund wie in
    // main.rs.
    allocator::heap_erweitern(2048).expect("Heap-Erweiterung fehlgeschlagen");

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
    programm_laufen("htmldump", argumente)
}

/// Startet ein Programm mit einer Pipe als Ausgabe und liest alles mit.
fn programm_laufen(programm: &str, argumente: &[&str]) -> Lauf {
    let start = zeit::ms_seit_boot();
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad(programm);
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);

    // argv[0] gehoert dazu — `prozess_starten_mit` stellt den Namen nicht
    // selbst voran.
    let mut argv: Vec<&str> = alloc::vec![programm];
    argv.extend_from_slice(argumente);

    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        &argv,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|f| panic!("'{}' starten: {}", programm, f.meldung()));
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
                    serial_println!("  !! Frist abgelaufen — {} haengt.", programm);
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

// ===========================================================================
// 4. CSS IN RING 3 (Serie 8, Teil 5)
// ===========================================================================

/// Startet `cssdump` und sammelt die Ausgabe ein.
fn cssdump(argumente: &[&str]) -> Lauf {
    programm_laufen("cssdump", argumente)
}

/// Legt eine HTML-Datei im Testordner an und liefert ihren Pfad.
fn testdatei(name: &str, inhalt: &str) -> String {
    let ordner = fs::persistenter_pfad("/platte/htmltest", "/htmltest");
    let _ = fs::mit_fs(|f| f.mkdir(ordner));
    let pfad = fs::pfad_anhaengen(ordner, name);
    datei_schreiben(&pfad, inhalt);
    pfad
}

/// DER MEILENSTEIN VON TEIL 5: Ein Ring-3-Prozess rechnet die Kaskade
/// durch — Standard-Stylesheet, Vererbung, Spezifitaet — und sagt, welche
/// Regel welchen Wert gesetzt hat.
#[test_case]
fn test_kaskade_in_ring3() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let lauf = cssdump(&[&pfad, "h1"]);

    assert_ne!(lauf.code(), CODE_PANIK, "cssdump ist gepanickt:\n{}", lauf.ausgabe);
    assert_eq!(lauf.code(), 0, "Ausgabe:\n{}", lauf.ausgabe);

    // Das Standard-Stylesheet hat gegriffen: <h1> ist Block, fett und
    // 32 px gross (2em von 16px).
    assert!(lauf.ausgabe.contains("<h1>"), "Kopfzeile fehlt:\n{}", lauf.ausgabe);
    assert!(lauf.ausgabe.contains("Block"), "display fehlt:\n{}", lauf.ausgabe);
    assert!(lauf.ausgabe.contains("32px"), "font-size fehlt:\n{}", lauf.ausgabe);
    assert!(lauf.ausgabe.contains("bold"), "font-weight fehlt");

    // Und die HERKUNFT steht dabei — das ist der eigentliche Zweck.
    assert!(
        lauf.ausgabe.contains("Standard"),
        "keine Herkunftsangabe:\n{}",
        lauf.ausgabe
    );
    assert!(lauf.ausgabe.contains("(0,0,1)"), "keine Spezifitaet");

    serial_println!(
        "  Kaskade in Ring 3: {} Zeilen fuer <h1> in {} ms.",
        lauf.zeilen(),
        lauf.dauer_ms
    );
}

/// Vererbung ueber die Prozessgrenze: `color` erbt bis nach unten,
/// `margin` nicht.
#[test_case]
fn test_vererbung_in_ring3() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = testdatei(
        "erben.html",
        "<html><body><style>div { color: #abcdef; margin: 40px }</style>\
         <div><p><span>tief</span></p></div></body></html>",
    );

    let lauf = cssdump(&[&pfad, "span"]);
    assert_eq!(lauf.code(), 0, "{}", lauf.ausgabe);
    assert!(
        lauf.ausgabe.contains("#abcdef"),
        "color haette erben muessen:\n{}",
        lauf.ausgabe
    );
    assert!(
        lauf.ausgabe.contains("geerbt"),
        "die Quelle 'geerbt' fehlt:\n{}",
        lauf.ausgabe
    );
    // margin erbt NICHT — der Wert muss 0 sein.
    let margin_zeile = lauf
        .ausgabe
        .lines()
        .find(|z| z.starts_with("margin"))
        .unwrap_or("");
    assert!(
        margin_zeile.contains("0px 0px 0px 0px"),
        "margin darf nicht erben, steht aber als: {margin_zeile}"
    );

    serial_println!("  Vererbung verhaelt sich in Ring 3 wie auf dem Host.");
}

/// Autor schlaegt Standard, `!important` schlaegt Spezifitaet — beides
/// ueber den ganzen Weg.
#[test_case]
fn test_kaskadenrang_in_ring3() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = testdatei(
        "rang.html",
        "<html><head><style>\
         h1 { font-weight: normal }\
         #a { color: #ff0000 }\
         h1 { color: #0000ff !important }\
         </style></head><body><h1 id=a>T</h1></body></html>",
    );

    let lauf = cssdump(&[&pfad, "h1"]);
    assert_eq!(lauf.code(), 0, "{}", lauf.ausgabe);
    // Autor schlaegt Standard: nicht mehr fett.
    assert!(
        lauf.ausgabe.contains("normal"),
        "der Autor haette den Standard schlagen muessen:\n{}",
        lauf.ausgabe
    );
    // !important schlaegt die Id-Regel.
    assert!(
        lauf.ausgabe.contains("#0000ff"),
        "!important haette die Id-Regel schlagen muessen:\n{}",
        lauf.ausgabe
    );
    assert!(lauf.ausgabe.contains("!important"), "der Hinweis fehlt");
    // Und die ueberstimmte Regel wird genannt.
    assert!(
        lauf.ausgabe.contains("ueberstimmt"),
        "die verlorene Regel muss sichtbar sein:\n{}",
        lauf.ausgabe
    );

    serial_println!("  Kaskadenrang (Autor > Standard, !important) stimmt in Ring 3.");
}

/// Kaputtes CSS toetet den Prozess nicht.
#[test_case]
fn test_kaputtes_css_in_ring3() {
    if !programme_vorhanden() {
        return;
    }
    let faelle: &[(&str, &str)] = &[
        ("css_leer.html", "<style></style><p>x</p>"),
        (
            "css_klammern.html",
            "<style>}}}{{{ p { color: red </style><p>x</p>",
        ),
        (
            "css_media.html",
            "<style>@media print { p { color: green } } p { color: #ff0000 }</style><p>x</p>",
        ),
        ("css_muell.html", "<style>@@@ ;;; ::: p{{{color:::red}}}</style><p>x</p>"),
        (
            "css_zahlen.html",
            "<style>p { color: rgb(999999999999,2,3); width: 99999999999px }</style><p>x</p>",
        ),
        ("css_umlaute.html", "<style>p.groesse { color: red }</style><p>x</p>"),
    ];
    for (name, inhalt) in faelle {
        let pfad = testdatei(name, inhalt);
        let lauf = cssdump(&[&pfad, "p"]);
        assert_ne!(
            lauf.code(),
            CODE_PANIK,
            "{} hat cssdump zum PANICKEN gebracht:\n{}",
            name,
            lauf.ausgabe
        );
        assert!(lauf.ende.is_some(), "{} hat cssdump haengen lassen", name);
        assert_eq!(lauf.code(), 0, "{}: Exit {}", name, lauf.code());
    }
    serial_println!("  {} kaputte Stylesheets, 0 Paniken.", faelle.len());
}

/// `--befund` zeigt, was uebersprungen wurde — inklusive `@media`.
#[test_case]
fn test_cssdump_befund() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = testdatei(
        "befund.html",
        "<style>@media print { p { color: green } } div > p { color: red } p { color: blue }</style><p>x</p>",
    );
    let lauf = cssdump(&[&pfad, "--befund"]);
    assert_eq!(lauf.code(), 0, "{}", lauf.ausgabe);
    assert!(
        lauf.ausgabe.contains("At-Regel"),
        "@media haette gemeldet werden muessen:\n{}",
        lauf.ausgabe
    );
    assert!(
        lauf.ausgabe.contains("koennen wir nicht"),
        "der Kind-Kombinator haette gemeldet werden muessen:\n{}",
        lauf.ausgabe
    );
    serial_println!("  --befund meldet @media und unerfuellbare Selektoren.");
}

/// Zehn Kaskaden-Durchlaeufe lecken nichts.
#[test_case]
fn test_kein_leck_cssdump() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    scheduler::aufraeumen();
    let vorher = memory::frame_statistik().0;
    for _ in 0..10 {
        let lauf = cssdump(&[&pfad, "h1"]);
        assert_eq!(lauf.code(), 0);
    }
    scheduler::aufraeumen();
    let nachher = memory::frame_statistik().0;
    let schranke = (10 * 340) / 512 + 2;
    let verloren = vorher.saturating_sub(nachher);
    serial_println!(
        "  10 cssdump-Prozesse: {} Frames verloren (Schranke {}).",
        verloren,
        schranke
    );
    assert!(verloren <= schranke, "Frame-Leck: {verloren} > {schranke}");
}

// ===========================================================================
// 5. LAYOUT IN RING 3 (Serie 8, Teil 6)
// ===========================================================================

/// DER MEILENSTEIN VON TEIL 6: Ein Ring-3-Prozess setzt eine Seite und
/// liefert Anzeige-Befehle mit absoluten Koordinaten.
///
/// Die Layout-LOGIK ist auf dem Host geprueft (55 Tests mit einer
/// Attrappen-Metrik, exakt nachgerechnet). Hier zaehlt nur, dass die
/// Kiste bare-metal uebersetzt, in 12 MiB Heap passt und ein echtes
/// Dokument durchrechnet, ohne zu panicken.
#[test_case]
fn test_layout_in_ring3() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let lauf = cssdump(&[&pfad, "--layout", "--breite=600"]);

    assert_ne!(lauf.code(), CODE_PANIK, "das Layout ist gepanickt:\n{}", lauf.ausgabe);
    assert_eq!(lauf.code(), 0, "Ausgabe:\n{}", lauf.ausgabe);

    // Es sind Befehle herausgekommen, und zwar Text.
    assert!(
        lauf.ausgabe.contains("TEXT"),
        "keine Textbefehle:\n{}",
        lauf.ausgabe
    );
    // WORTWEISE suchen, nicht als Satz: Jedes Wort ist ein EIGENER
    // Textbefehl, weil jedes seine eigene Position hat (der Zeilenbau
    // setzt sie einzeln). „World Wide Web" als zusammenhaengende Zeichen-
    // folge gibt es in der Liste deshalb nicht.
    for wort in ["World", "Wide", "Web"] {
        assert!(
            lauf.ausgabe.contains(wort),
            "'{wort}' fehlt im Layout:
{}",
            lauf.ausgabe.lines().take(20).collect::<Vec<_>>().join("
")
        );
    }
    assert!(lauf.ausgabe.contains("Anzeige-Befehle"));
    assert!(lauf.ausgabe.contains("Gesamthoehe"));

    // Die Seite hat eine sinnvolle Hoehe (die erste Webseite der Welt ist
    // bei 600 px Breite mehrere Bildschirme lang).
    let hoehe: i32 = lauf
        .ausgabe
        .lines()
        .find(|z| z.contains("Gesamthoehe"))
        .and_then(|z| {
            z.split_whitespace()
                .skip_while(|w| *w != "Gesamthoehe")
                .nth(1)
                .and_then(|w| w.parse().ok())
        })
        .unwrap_or(0);
    assert!(hoehe > 200, "Gesamthoehe {hoehe} px — das kann nicht stimmen");

    serial_println!(
        "  Layout in Ring 3: {} Zeilen Befehle, Seite {} px hoch ({} ms).",
        lauf.zeilen(),
        hoehe,
        lauf.dauer_ms
    );
}

/// Die Koordinaten steigen — der Text steht untereinander und nicht alles
/// auf y=0.
#[test_case]
fn test_layout_koordinaten_steigen() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let lauf = cssdump(&[&pfad, "--layout", "--breite=400"]);
    assert_eq!(lauf.code(), 0);

    // Aus den TEXT-Zeilen die y-Werte holen: "TEXT   xxxxx,yyyyy ..."
    let mut ys: Vec<i32> = Vec::new();
    for zeile in lauf.ausgabe.lines() {
        if let Some(rest) = zeile.strip_prefix("TEXT") {
            if let Some((_, y_teil)) = rest.split_whitespace().next().and_then(|k| k.split_once(','))
            {
                if let Ok(y) = y_teil.trim().parse::<i32>() {
                    ys.push(y);
                }
            }
        }
    }
    assert!(ys.len() > 20, "zu wenige Textbefehle: {}", ys.len());
    let hoechstes = ys.iter().copied().max().unwrap_or(0);
    assert!(
        hoechstes > 300,
        "alles klebt oben — der Blockfluss stapelt nicht: max y = {hoechstes}"
    );
    serial_println!("  {} Textbefehle, tiefster bei y={}.", ys.len(), hoechstes);
}

/// Schmale und breite Fenster ergeben verschieden hohe Seiten — der
/// Zeilenumbruch wirkt wirklich.
#[test_case]
fn test_layout_bricht_nach_breite_um() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let hoehe_von = |breite: &str| -> i32 {
        let lauf = cssdump(&[&pfad, "--layout", breite]);
        assert_eq!(lauf.code(), 0, "{}", lauf.ausgabe);
        lauf.ausgabe
            .lines()
            .find(|z| z.contains("Gesamthoehe"))
            .and_then(|z| {
                z.split_whitespace()
                    .skip_while(|w| *w != "Gesamthoehe")
                    .nth(1)
                    .and_then(|w| w.parse().ok())
            })
            .unwrap_or(0)
    };
    let schmal = hoehe_von("--breite=200");
    let breit = hoehe_von("--breite=1200");
    assert!(
        schmal > breit,
        "schmal ({schmal}) muesste hoeher sein als breit ({breit})"
    );
    serial_println!("  200px -> {} px hoch, 1200px -> {} px hoch.", schmal, breit);
}

/// Kaputte und entartete Eingaben toeten das Layout nicht.
#[test_case]
fn test_layout_haelt_muell_aus() {
    if !programme_vorhanden() {
        return;
    }
    let faelle: &[(&str, &str)] = &[
        ("l_leer.html", ""),
        ("l_tief.html", "<div><div><div><div><div><div>tief</div></div></div></div></div></div>"),
        (
            "l_tabelle.html",
            "<table><tr><td>a<td>b<tr><td>c<td>d</table>",
        ),
        (
            "l_liste.html",
            "<ul><li>a<li>b<ul><li>c</ul></ul><ol><li>1<li>2</ol>",
        ),
        (
            "l_absurd.html",
            "<style>p{width:99999999px;margin:99999px;font-size:9999px}</style><p>x</p>",
        ),
        ("l_lang.html", "<p>WortOhneLeerzeichenDasSehrLangIstUndNichtPasst</p>"),
        ("l_bilder.html", "<p><img src=a><img src=b width=999999></p>"),
        ("l_pre.html", "<pre>  eins\n    zwei\n</pre>"),
    ];
    for (name, inhalt) in faelle {
        let pfad = testdatei(name, inhalt);
        for breite in ["--breite=1", "--breite=300", "--breite=100000"] {
            let lauf = cssdump(&[&pfad, "--layout", breite]);
            assert_ne!(
                lauf.code(),
                CODE_PANIK,
                "{} bei {} hat das Layout zum PANICKEN gebracht:\n{}",
                name,
                breite,
                lauf.ausgabe
            );
            assert!(lauf.ende.is_some(), "{name} bei {breite} haengt");
            assert_eq!(lauf.code(), 0, "{}: Exit {}", name, lauf.code());
        }
    }
    serial_println!("  {} Faelle x 3 Breiten, 0 Paniken.", faelle.len());
}

/// Zehn Layout-Durchlaeufe lecken nichts.
#[test_case]
fn test_kein_leck_layout() {
    if !programme_vorhanden() || programme::TESTSEITE.is_empty() {
        return;
    }
    let pfad = programme::testseite_pfad();
    scheduler::aufraeumen();
    let vorher = memory::frame_statistik().0;
    for _ in 0..10 {
        let lauf = cssdump(&[&pfad, "--layout"]);
        assert_eq!(lauf.code(), 0);
    }
    scheduler::aufraeumen();
    let nachher = memory::frame_statistik().0;
    let schranke = (10 * 340) / 512 + 2;
    let verloren = vorher.saturating_sub(nachher);
    serial_println!(
        "  10 Layout-Prozesse: {} Frames verloren (Schranke {}).",
        verloren,
        schranke
    );
    assert!(verloren <= schranke, "Frame-Leck: {verloren} > {schranke}");
}
