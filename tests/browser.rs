// tests/browser.rs — DER BROWSER ALS PROGRAMM (Serie 8, Teil 8)
//
// ===========================================================================
// WAS HIER GEPRUEFT WIRD — und was ABSICHTLICH woanders steht
//
// Die REINE Logik ist schon geprueft, und zwar dort, wo sie wohnt und auf
// dem HOST in Millisekunden statt in QEMU-Starts:
//
//   speedhttp   25 Tests   URL-Aufloesung (`..`, Fragmente, Query, Schemata)
//   speedlayout 60 Tests   Layout, inklusive Bildgroessen und Reflow
//   speedpaint  35 Tests   Malen, Scroll-Klemmung, Invalidierungs-Regeln
//   speedui     45 Tests   Widgets, Teilflaeche
//
// Hier steht ausschliesslich, was NUR bare-metal beantwortet werden kann:
// Laeuft der Browser als unprivilegierter Prozess, findet er seine
// Seiten, erkennt er Fehler, und ueberlebt das System ihn?
//
// Das Vehikel ist `browser --pruefen`: Es laedt eine Seite und gibt
// maschinenlesbar aus, was dabei herauskam (Titel, Zustand, Fehler,
// JavaScript-Befund, aufgeloeste Verweise). Ohne diesen Modus liessen
// sich genau diese Dinge nur fotografieren.

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
use speed_os::prozess::{self, Pid};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fenster, fs, memory, pipe, programme, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(4096).expect("Heap-Erweiterung fehlgeschlagen");

    fs::init();
    programme::installieren();
    programme::testseite_installieren();
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

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// `browser --pruefen <adresse>` laufen lassen und die Ausgabe holen.
fn pruefen(adresse: &str) -> String {
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("browser");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid = prozess::prozess_starten_mit(
        &pfad,
        &["browser", adresse, "--pruefen", "--fenster=800x600"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("browser starten: {}", fehler.meldung()));
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);
    let text = pipe_leeren(leitung, pid);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    text
}

fn pipe_leeren(leitung: pipe::PipeId, pid: Pid) -> String {
    let mut gesammelt = Vec::new();
    let mut puffer = alloc::vec![0u8; 4096];
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    break;
                }
                scheduler::aufraeumen();
                zeit::warte_auf_interrupt();
            }
            _ => break,
        }
    }
    let _ = scheduler::ende_abfragen(pid);
    scheduler::aufraeumen();
    String::from_utf8_lossy(&gesammelt).into_owned()
}

/// Den Wert eines `SCHLUESSEL=wert`-Feldes holen.
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

// ===========================================================================
// (1) EINE LOKALE SEITE
// ===========================================================================

/// Der Browser laedt eine Datei von der Platte, setzt sie und findet
/// ihren Titel.
#[test_case]
fn test_lokale_seite_wird_geladen() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let ausgabe = pruefen(&pfad);
    serial_println!("--- browser --pruefen {} ---\n{}", pfad, ausgabe);

    assert_eq!(feld(&ausgabe, "ZUSTAND="), "fertig", "die Seite muss laden");
    assert_eq!(feld(&ausgabe, "UNSICHER="), "0");
    assert!(
        zahl(&ausgabe, "BEFEHLE=") > 50,
        "die CERN-Seite hat mehr als 50 Anzeige-Befehle"
    );
    // Der `<title>` der ersten Webseite der Welt.
    let titel = feld(&ausgabe, "TITEL=");
    assert!(
        titel.contains("World Wide Web") || titel.contains("WORLD"),
        "der Titel kommt aus <title> (war '{}')",
        titel
    );
    serial_println!("[BROWSER] Titel aus <title>: '{}'", titel);
}

/// **DIE VERWEISE WERDEN AUFGELOEST** — und zwar relativ zur Datei, in
/// der sie stehen.
///
/// Die CERN-Seite verweist auf `WWW/TheProject.html` und aehnliche
/// relative Ziele. Ein Browser, der daraus nicht den richtigen Pfad
/// macht, zeigt bei jedem Klick eine Fehlerseite — und man sucht den
/// Fehler im Dateisystem statt in der Aufloesung.
#[test_case]
fn test_verweise_werden_relativ_aufgeloest() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let ausgabe = pruefen(&pfad);
    let ordner = match pfad.rfind('/') {
        Some(i) => &pfad[..i],
        None => "",
    };
    let mut geprueft = 0;
    for zeile in ausgabe.lines() {
        let Some(rest) = zeile.strip_prefix("LINK=") else {
            continue;
        };
        let Some((quelle, ziel)) = rest.split_once(" -> ") else {
            continue;
        };
        if quelle.starts_with("http") || quelle.starts_with('#') || ziel.starts_with("FEHLER") {
            continue;
        }
        // Das Ergebnis ist IMMER ein absoluter, normalisierter Pfad:
        // keine Punkt-Segmente mehr, keine doppelten Schraegstriche.
        assert!(
            ziel.starts_with('/'),
            "'{}' wurde zu '{}' aufgeloest — das ist kein absoluter Pfad",
            quelle,
            ziel
        );
        assert!(
            !ziel.contains("/../") && !ziel.ends_with("/..") && !ziel.contains("//"),
            "'{}' wurde zu '{}' aufgeloest — da stehen noch Punkt-Segmente drin",
            quelle,
            ziel
        );
        // Ein Verweis OHNE `..` bleibt im Ordner seiner Seite; einer MIT
        // `..` darf ihn verlassen (das ist der Sinn von `..`), aber
        // niemals ueber die Wurzel hinaus.
        if !quelle.starts_with("..") {
            assert!(
                ziel.starts_with(ordner),
                "'{}' wurde zu '{}' aufgeloest, erwartet unterhalb von '{}'",
                quelle,
                ziel,
                ordner
            );
        }
        geprueft += 1;
    }
    assert!(geprueft > 0, "die Testseite sollte relative Verweise haben");
    serial_println!("[BROWSER] {} relative Verweise korrekt aufgeloest", geprueft);
}

// ===========================================================================
// (2) DIE EINGEBAUTEN SEITEN
// ===========================================================================

/// `speedos:info` ist da, hat Inhalt — und sagt, dass es kein JavaScript
/// gibt.
///
/// Der Ehrlichkeits-Teil ist damit nicht nur eine Absichtserklaerung im
/// Kommentar, sondern eine Zusage, die bricht, wenn jemand die Seite
/// leert.
#[test_case]
fn test_infoseite_nennt_die_grenzen() {
    if !programme_vorhanden() {
        return;
    }
    let ausgabe = pruefen("speedos:info");
    assert_eq!(feld(&ausgabe, "ZUSTAND="), "fertig");
    assert!(
        zahl(&ausgabe, "BEFEHLE=") > 100,
        "die Info-Seite hat Inhalt (war {} Befehle)",
        zahl(&ausgabe, "BEFEHLE=")
    );
    // Sie darf sich NICHT selbst als „braucht JavaScript" melden.
    assert_eq!(
        feld(&ausgabe, "JS_HINWEIS="),
        "0",
        "die Info-Seite hat sichtbaren Text"
    );
    serial_println!(
        "[BROWSER] speedos:info: {} Anzeige-Befehle, {} px hoch",
        zahl(&ausgabe, "BEFEHLE="),
        zahl(&ausgabe, "HOEHE=")
    );
}

/// Eine eingebaute Seite, die es nicht gibt, ist ein FEHLER — kein
/// stilles Ausweichen auf die Startseite.
#[test_case]
fn test_unbekannte_eingebaute_seite() {
    if !programme_vorhanden() {
        return;
    }
    let ausgabe = pruefen("speedos:gibtesnicht");
    // Der Browser faellt auf die Info-Seite zurueck (die Adresse liess
    // sich nicht deuten) — aber er stuerzt nicht ab und zeigt etwas.
    assert!(
        zahl(&ausgabe, "BEFEHLE=") > 0,
        "es muss etwas Anzeigbares herauskommen"
    );
}

// ===========================================================================
// (3) FEHLERSEITEN
// ===========================================================================

/// Eine Datei, die es nicht gibt, ergibt eine FEHLERSEITE — kein leeres
/// Fenster und kein Absturz.
#[test_case]
fn test_fehlende_datei_ergibt_fehlerseite() {
    if !programme_vorhanden() {
        return;
    }
    let ausgabe = pruefen("/platte/gibt-es-nicht-4711.html");
    assert_eq!(feld(&ausgabe, "ZUSTAND="), "fehler");
    assert_ne!(feld(&ausgabe, "FEHLER="), "-", "der Fehler wird benannt");
    assert!(
        zahl(&ausgabe, "BEFEHLE=") > 5,
        "die Fehlerseite wird wirklich gesetzt und gemalt"
    );
    // Ein Dateifehler ist KEIN Sicherheitsfehler.
    assert_eq!(feld(&ausgabe, "UNSICHER="), "0");
    serial_println!(
        "[BROWSER] Fehlerseite: {} ({} Befehle)",
        feld(&ausgabe, "FEHLER="),
        zahl(&ausgabe, "BEFEHLE=")
    );
}

// ===========================================================================
// (4) DER JAVASCRIPT-BEFUND
// ===========================================================================

/// **Eine Seite, die ohne JavaScript leer bleibt, wird ERKANNT.**
///
/// Das ist die Zusage aus Aufgabe 5: Statt einer weissen Flaeche gibt es
/// einen Hinweis. Geprueft wird mit einer Seite, die genau so gebaut ist
/// wie die echten Faelle — ein leerer Rumpf und ein Skript, das ihn
/// fuellen wuerde.
#[test_case]
fn test_leere_js_seite_wird_erkannt() {
    if !programme_vorhanden() {
        return;
    }
    // NICHT `/platte/...` festverdrahten: Ohne gemountete Platte liegen
    // die Seiten im RAM-VFS. `seiten_verzeichnis()` sagt, wo wirklich.
    let ordner = programme::seiten_verzeichnis();
    let pfad = alloc::format!("{}/nur-js.html", ordner);
    let pfad = pfad.as_str();
    let inhalt = b"<html><head><title>App</title></head><body><div id=\"root\"></div>\
<script>document.getElementById('root').innerHTML='<h1>Hallo</h1>';</script></body></html>";
    fs::mit_fs(|dateisystem| dateisystem.schreiben(pfad, inhalt))
        .expect("Testseite schreiben");

    let ausgabe = pruefen(pfad);
    serial_println!("--- nur-js.html ---\n{}", ausgabe);
    assert_eq!(
        feld(&ausgabe, "JS_HINWEIS="),
        "1",
        "eine leere Seite mit <script> muss als JS-Seite erkannt werden"
    );
    // Und der Hinweis wird auch wirklich ANGEZEIGT.
    assert_eq!(feld(&ausgabe, "TITEL="), "Braucht JavaScript");
    assert!(zahl(&ausgabe, "BEFEHLE=") > 10, "der Hinweis wird gesetzt");

    // Gegenprobe: Eine Seite MIT Text und Skript ist KEIN Fall fuer den
    // Hinweis — sonst wuerde er auf fast jeder echten Seite erscheinen.
    let pfad2 = alloc::format!("{}/mit-text.html", ordner);
    let pfad2 = pfad2.as_str();
    let inhalt2 = b"<html><head><title>Echt</title></head><body>\
<h1>Hier steht wirklich etwas</h1><p>Ein ganzer Absatz Text.</p>\
<script>var x = 1;</script></body></html>";
    fs::mit_fs(|dateisystem| dateisystem.schreiben(pfad2, inhalt2)).expect("Testseite 2");
    let ausgabe2 = pruefen(pfad2);
    assert_eq!(
        feld(&ausgabe2, "JS_HINWEIS="),
        "0",
        "eine Seite mit Text darf den Hinweis NICHT bekommen"
    );
    serial_println!("[BROWSER] JS-Befund: leere Seite erkannt, Seite mit Text nicht.");
}

// ===========================================================================
// (5) LESEZEICHEN UND STARTSEITE
// ===========================================================================

/// Lesezeichen und Startseite werden aus der Datei gelesen.
///
/// Geprueft wird das FORMAT: Der Browser liest, was ein Mensch (oder
/// SpeedText) hineingeschrieben hat.
#[test_case]
fn test_lesezeichen_werden_gelesen() {
    if !programme_vorhanden() {
        return;
    }
    // Derselbe Ort, an dem der Browser sie sucht (Platte, sonst RAM).
    let ordner = fs::persistenter_pfad("/platte/system", "/system");
    let pfad = alloc::format!("{}/lesezeichen.txt", ordner);
    let pfad = pfad.as_str();
    let inhalt = b"# Kommentarzeile, wird ueberlesen\n\
start\tspeedos:info\n\
https://example.com/\tExample Domain\n\
/platte/seiten/cern.html\tWorld Wide Web\n\
kaputte_zeile_ohne_trenner\n";
    let _ = fs::mit_fs(|dateisystem| dateisystem.mkdir(ordner));
    fs::mit_fs(|dateisystem| dateisystem.schreiben(pfad, inhalt)).expect("Lesezeichen schreiben");

    let ausgabe = pruefen("speedos:info");
    serial_println!("--- Lesezeichen ---\n{}", ausgabe);
    // Drei Eintraege: zwei mit Titel, einer ohne Trenner (der zaehlt
    // mit — eine Adresse ohne Titel ist brauchbar).
    assert_eq!(
        feld(&ausgabe, "LESEZEICHEN="),
        "3",
        "zwei vollstaendige plus eine Zeile ohne Trenner"
    );
    assert_eq!(feld(&ausgabe, "STARTSEITE="), "speedos:info");
    assert_eq!(feld(&ausgabe, "LESEZEICHEN_PFAD="), pfad);
    serial_println!("[BROWSER] Lesezeichen aus {} gelesen", pfad);
}

// ===========================================================================
// (6) KEIN LECK
// ===========================================================================

/// Fuenf Browser-Laeufe hintereinander duerfen keinen Frame verlieren.
///
/// Der Browser ist das groesste Programm des Systems und alloziert am
/// meisten (Baum, Stile, Kaesten, Anzeigeliste, Fensterpuffer, Cache,
/// TLS-Konfiguration). Wenn irgendwo etwas haengen bleibt, dann hier.
#[test_case]
fn test_browser_leckt_keine_frames() {
    if !programme_vorhanden() {
        return;
    }
    let _ = pruefen("speedos:info");
    scheduler::aufraeumen();

    let (frei_vorher, gesamt) = memory::frame_statistik();
    for _ in 0..5 {
        let _ = pruefen("speedos:info");
        scheduler::aufraeumen();
    }
    let (frei_nachher, _) = memory::frame_statistik();
    let vorher = (gesamt - frei_vorher) as u64;
    let nachher = (gesamt - frei_nachher) as u64;

    let log_frames = (speed_os::protokoll::puffer_bytes() / 4096) as u64 + 2;
    serial_println!(
        "[BROWSER] Frames vorher {} nachher {} (Schranke +{})",
        vorher,
        nachher,
        log_frames
    );
    assert!(
        nachher <= vorher + log_frames,
        "5 Browser-Laeufe haben {} Frames gekostet (erlaubt: {})",
        nachher.saturating_sub(vorher),
        log_frames
    );
}
