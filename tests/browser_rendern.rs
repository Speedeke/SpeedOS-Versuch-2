// tests/browser_rendern.rs — DER SCROLL-FRAME, UND WAS ER KOSTET
// (Serie 8, Teil 7)
//
// ===========================================================================
// WOZU DIESER TEST DA IST
//
// Er beantwortet die Frage, die in docs/fenster-syscalls.md 5 VOR dem
// ersten Renderer festgeschrieben wurde:
//
//   > Geteilter Speicher wird neu bewertet, wenn ein Scroll-Frame ueber
//   > ~8 ms braucht UND die Kopie mehr als die Haelfte davon ausmacht.
//
// Bis Serie 8, Teil 1 liess sich das nur SCHAETZEN: Dort malte ein
// Programm eine einfarbige Flaeche, und die Zahlen waren cache-warm und
// unrealistisch guenstig ("Ehrliche Einordnung" in 4 desselben
// Dokuments). Jetzt gibt es einen echten Renderer und eine echte grosse
// Seite — also wird gemessen statt geschaetzt.
//
// ===========================================================================
// DIE METHODE
//
// Gemessen wird AUS RING 3, mit dem echten Programm `browser`, an
// Pruefseite B (der Wikipedia-Artikel, ~293 KiB, im Image eingebettet).
// Ein kernel-seitiger Aufbau waere billiger und wuerde die Zahl
// schoenrechnen — er haette weder Privilegienwechsel noch Zeigerpruefung.
//
// Je Lauf entstehen die zwei Posten, nach denen das Kriterium fragt:
//   MALEN_US  — verschieben + den neu sichtbaren Streifen malen
//   KOPIE_US  — `fenster_zeichnen`, also die Naht selbst
// dazu VOLL_MALEN_US als Vergleich (ganze Flaeche malen, ohne Streifen-
// Trick) — sonst waere nicht zu sehen, was der Streifen bringt.
//
// AUFLOESUNG: Der Runner bestimmt sie (`SPEEDOS_AUFLOESUNG=720p` bzw.
// `4k`); der Test nimmt die tatsaechliche Bildschirmgroesse und misst
// daran. Deshalb zwei Laeufe fuer die zwei Zahlenreihen im CHANGELOG.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::format;
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
    // Wie in tests/fenster_messung.rs: Bei 4K braucht ein Fenster-Puffer
    // im KERNEL 24 MiB. 12288 Seiten = 48 MiB lassen Luft fuer
    // Back-Buffer und Hintergrund — und fuer die 293 KiB Testseite, die
    // `programme::installieren` beim Vergleichen ganz in den Heap liest.
    allocator::heap_erweitern(12288).expect("Heap-Erweiterung fehlgeschlagen");

    fs::init();
    programme::installieren();
    programme::testseite_installieren();
    scheduler::init();

    if let Some(fb) = framebuffer {
        speed_os::framebuffer::init(fb);
    }
    assert!(
        fenster::manager_fuer_test_starten(),
        "ohne Framebuffer gibt es hier nichts zu messen"
    );

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

const FRIST_MS: u64 = 300_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Startet `browser` mit den Argumenten und liest seine Ausgabe zurueck.
fn browser_laufen_lassen(argumente: &[&str]) -> String {
    let leitung = pipe::anlegen().expect("Pipe anlegen");
    let pfad = programme::pfad("browser");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("browser starten: {}", fehler.meldung()));
    // Die EIGENE Kopie des Schreib-Endes abgeben, sonst kommt nie ein
    // Dateiende (der Klassiker aus Serie 6, Teil 6).
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

fn zahl_aus(ausgabe: &str, schluessel: &str) -> Option<u64> {
    let start = ausgabe.find(schluessel)? + schluessel.len();
    let rest = &ausgabe[start..];
    let ende = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..ende].parse::<u64>().ok()
}

fn bildschirm() -> (usize, usize) {
    speed_os::framebuffer::mit_framebuffer(|fb| {
        let info = fb.info();
        (info.width, info.height)
    })
    .expect("Framebuffer")
}

// ===========================================================================
// (1) DIE ERSTE GERENDERTE WEBSEITE
// ===========================================================================

/// Die kleine Pruefseite A (die erste Webseite der Welt) geht durch die
/// GANZE Kette: holen, parsen, kaskadieren, setzen, malen — in Ring 3.
///
/// Er prueft ABSICHTLICH keine Pixel, sondern die Zahlen davor: Ob der
/// Text an der richtigen Stelle landet, sagen die 33 Host-Tests von
/// `speedpaint` genauer, als ein Bildschirmfoto es je koennte. Was NUR
/// hier prueftbar ist: dass das Ganze bare-metal in einem eigenen
/// Adressraum ueberhaupt laeuft.
#[test_case]
fn test_erste_webseite_wird_gerendert() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programme::testseite_pfad();
    let ausgabe = browser_laufen_lassen(&["browser", &pfad, "--messen=10", "--fenster=900x620"]);
    serial_println!("--- browser auf {} ---\n{}", pfad, ausgabe);

    let befehle = zahl_aus(&ausgabe, "BEFEHLE=").expect("BEFEHLE fehlt");
    let hoehe = zahl_aus(&ausgabe, "DOKUMENT_HOEHE=").expect("DOKUMENT_HOEHE fehlt");
    assert!(befehle > 50, "die CERN-Seite hat mehr als 50 Befehle (war {})", befehle);
    assert!(hoehe > 200, "und sie ist hoeher als 200 px (war {})", hoehe);
    serial_println!(
        "[BROWSER] Pruefseite A gerendert: {} Anzeige-Befehle, {} px hoch",
        befehle,
        hoehe
    );
}

// ===========================================================================
// (2) DIE MESSUNG UND DAS KRITERIUM
// ===========================================================================

/// DIE MESSUNG. Ein Bericht plus die EINE harte Zusage: Ein Streifen ist
/// billiger als die ganze Flaeche zu malen — sonst waere die ganze
/// Scroll-Mechanik dieses Teils sinnlos.
#[test_case]
fn messung_scroll_frame() {
    if !programme_vorhanden() {
        return;
    }
    let (bb, bh) = bildschirm();
    // So gross, wie ein maximiertes Fenster waere (Titelleiste und
    // Taskleiste gehen ab) — das ist die Flaeche, um die es beim
    // Scrollen wirklich geht.
    let breite = bb;
    let hoehe = bh.saturating_sub(72);
    let fenster_arg = format!("--fenster={}x{}", breite, hoehe);

    let pfad = programme::grosse_testseite_pfad();
    let ausgabe = browser_laufen_lassen(&["browser", &pfad, "--messen=200", &fenster_arg]);
    serial_println!("--- Rohausgabe von 'browser --messen=200' ---\n{}", ausgabe);

    let hole = |schluessel: &str| {
        zahl_aus(&ausgabe, schluessel)
            .unwrap_or_else(|| panic!("{} fehlt in der Ausgabe", schluessel))
    };
    let f_breite = hole("FENSTER_BREITE=");
    let f_hoehe = hole("FENSTER_HOEHE=");
    let befehle = hole("BEFEHLE=");
    let dok_hoehe = hole("DOKUMENT_HOEHE=");
    let schritte = hole("SCHRITTE=");
    let malen = hole("MALEN_US=");
    let kopie = hole("KOPIE_US=");
    let voll = hole("VOLL_MALEN_US=");

    serial_println!(
        "[MESSUNG-BROWSER] Bildschirm {}x{}, Fenster {}x{} — Pruefseite B: \
         {} Anzeige-Befehle, {} px hoch",
        bb,
        bh,
        f_breite,
        f_hoehe,
        befehle,
        dok_hoehe
    );
    serial_println!(
        "[MESSUNG-BROWSER] {} Scroll-Schritte: malen (Streifen) {} us | \
         Kopie (fenster_zeichnen) {} us | Vollbild malen {} us",
        schritte,
        malen,
        kopie,
        voll
    );

    // DAS UMSTIEGSKRITERIUM, ausgerechnet statt behauptet
    // (docs/fenster-syscalls.md 5): Scroll-Frame > 8000 us UND die
    // Kopie mehr als die Haelfte davon. BEIDE Bedingungen — ein
    // langsamer Frame kann genauso gut am Malen liegen, und dann wuerde
    // geteilter Speicher nichts aendern.
    let frame = malen + kopie;
    let anteil = (kopie * 100).checked_div(frame.max(1)).unwrap_or(0);
    let erfuellt = frame > 8000 && anteil > 50;
    serial_println!(
        "[MESSUNG-BROWSER] Scroll-Frame (malen + uebertragen) = {} us, \
         davon Kopie {} % -> Umstiegskriterium {}",
        frame,
        anteil,
        if erfuellt {
            "ERFUELLT: geteilten Speicher neu bewerten (docs/browser-rendern.md)"
        } else {
            "NICHT erfuellt: Pixelpuffer per Syscall bleibt"
        }
    );

    // Was der Streifen bringt: das Verhaeltnis Vollbild-Malen zu
    // Streifen-Malen.
    if malen > 0 {
        serial_println!(
            "[MESSUNG-BROWSER] Streifen statt Vollbild malen: {} us statt {} us = Faktor {}",
            malen,
            voll,
            voll / malen.max(1)
        );
    }

    // DIE GEGENRECHNUNG, und sie ist die eigentliche Auskunft dieser
    // Messung: Wie stuende das Kriterium OHNE das Streifen-Zeichnen —
    // also wenn ein Scroll-Frame die ganze Flaeche neu malte?
    //
    // Sie wird hier AUSGERECHNET und nicht in der Doku behauptet, damit
    // sie bei jedem Lauf mitwandert. Wer die Streifen-Mechanik ausbaut,
    // sieht sofort, was er sich einhandelt.
    let frame_ohne = voll + kopie;
    let anteil_ohne = (kopie * 100).checked_div(frame_ohne.max(1)).unwrap_or(0);
    serial_println!(
        "[MESSUNG-BROWSER] Gegenrechnung OHNE Streifen (Vollbild malen + Kopie) = {} us, \
         davon Kopie {} % -> Kriterium waere {}",
        frame_ohne,
        anteil_ohne,
        if frame_ohne > 8000 && anteil_ohne > 50 {
            "ERFUELLT"
        } else {
            "nicht erfuellt"
        }
    );

    // DIE HARTE ZUSAGE: Einen Streifen zu malen muss billiger sein, als
    // die ganze Flaeche zu malen. Ohne sie waere die Verschiebe-Mechanik
    // dieses Teils Arbeit ohne Gewinn — und der Scroll-Frame haette
    // keinen Grund, schneller zu sein als ein Neuaufbau.
    assert!(
        malen < voll,
        "ein Streifen ({} us) muss billiger zu malen sein als die ganze \
         Flaeche ({} us) — sonst braucht es die Verschiebung nicht",
        malen,
        voll
    );
    // Und die Seite muss wirklich gross sein, sonst misst der Test nichts.
    assert!(
        befehle > 1000,
        "Pruefseite B soll ein grosses Dokument sein (war {} Befehle) — \
         fehlt assets/testseiten/wikipedia-betriebssystem.html?",
        befehle
    );
}

// ===========================================================================
// (3) KEIN LECK
// ===========================================================================

/// Fuenf Browser-Laeufe hintereinander duerfen keinen Frame verlieren.
///
/// Ein Renderer alloziert viel (Baum, Stile, Kaesten, Befehlsliste,
/// Fensterpuffer) — genau die Sorte Programm, bei der ein vergessener
/// Puffer nicht auffaellt, weil der Prozess ohnehin endet. Geprueft wird
/// deshalb die KERNEL-Seite: Faellt der Adressraum samt Fensterpuffer
/// wirklich vollstaendig?
#[test_case]
fn test_browser_leckt_keine_frames() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programme::testseite_pfad();
    // Ein Vorlauf, damit einmalige Allokationen (Programm-Cache,
    // Fenster-Manager) nicht als Leck erscheinen.
    let _ = browser_laufen_lassen(&["browser", &pfad, "--messen=2", "--fenster=640x480"]);
    scheduler::aufraeumen();

    // `frame_statistik` liefert (frei, gesamt) — ein Leck senkt also die
    // freien Frames. Gerechnet wird mit den BELEGTEN, weil sich das
    // leichter liest.
    let (frei_vorher, gesamt) = memory::frame_statistik();
    for _ in 0..5 {
        let _ = browser_laufen_lassen(&["browser", &pfad, "--messen=2", "--fenster=640x480"]);
        scheduler::aufraeumen();
    }
    let (frei_nachher, _) = memory::frame_statistik();
    let vorher = (gesamt - frei_vorher) as u64;
    let nachher = (gesamt - frei_nachher) as u64;

    // Der Kernel-Log-Puffer waechst mit jeder Ausgabe (bis 64 KiB) und
    // wird herausgerechnet, nicht ignoriert — dieselbe Messfalle wie im
    // Serie-6-Abschluss.
    let log_frames = (speed_os::protokoll::puffer_bytes() / 4096) as u64 + 2;
    serial_println!(
        "[BROWSER] Frames vorher {} nachher {} (Schranke +{} fuer den Log-Puffer)",
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
