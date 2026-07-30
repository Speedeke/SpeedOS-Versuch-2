// tests/fenster_messung.rs — WAS KOSTET fenster_zeichnen? (Serie 8, Teil 1)
//
// Ein BERICHTS-Test: Er misst und schreibt die Zahlen ins Protokoll, statt
// eine Schwelle zu behaupten. Nur eine einzige Zusage wird geprüft — dass
// ein Streifen billiger ist als ein Vollbild —, denn genau darauf beruht
// die Entscheidung, den Bereich überhaupt in den Syscall zu legen.
//
// ==========================================================================
// DIE METHODE (damit die Zahlen später nachprüfbar sind)
//
// Gemessen wird AUS RING 3, mit dem echten Programm `messung 7`. Ein
// Kernel-seitiger Aufruf wäre billiger und würde die Zahl schönrechnen —
// er hätte weder Privilegienwechsel noch Zeigerprüfung.
//
// Vier Zahlen entstehen je Auflösung:
//   MALEN_US     — der Prozess füllt seinen EIGENEN Puffer (kein Syscall)
//   VOLLBILD_US  — dieselbe Fläche einmal übertragen
//   STREIFEN_US  — volle Breite, 16 Zeilen
//   BLOCK_US     — 32x32 (also der Fall MIT Umkopieren im Programm)
//
// AUFLÖSUNG: Der Runner bestimmt sie (`SPEEDOS_AUFLOESUNG=720p` bzw. `4k`);
// der Test nimmt die tatsächliche Bildschirmgrösse und misst daran. Deshalb
// zwei Läufe für die zwei Zahlenreihen im CHANGELOG.
//
// DAS UMSTIEGSKRITERIUM, auf das diese Zahlen zielen, steht in
// docs/fenster-syscalls.md §5 — VOR der Messung festgelegt, wie bei der
// TCP-Reissleine.

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
    // Bei 4K braucht ein Fenster-Puffer im KERNEL 3840*2160*3 = 24 MiB.
    // 12288 Seiten = 48 MiB lassen Luft für Back-Buffer und Hintergrund.
    allocator::heap_erweitern(12288).expect("Heap-Erweiterung fehlgeschlagen");

    fs::init();
    programme::installieren();
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

const FRIST_MS: u64 = 120_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Startet `messung` mit den Argumenten und liest seine Ausgabe über eine
/// Pipe zurück.
fn messung_laufen_lassen(argumente: &[&str]) -> String {
    let leitung = pipe::anlegen().expect("Pipe anlegen");
    let pfad = programme::pfad("messung");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("messung starten: {}", fehler.meldung()));
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
                // Aufräumen MUSS mitlaufen: Das Schreib-Ende des Kindes
                // hängt an seiner Handle-Tabelle und fällt erst beim
                // Abräumen — ohne das wartet man ewig auf ein Dateiende.
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

/// Die Bildschirmgrösse, die der Runner gewählt hat.
fn bildschirm() -> (usize, usize) {
    speed_os::framebuffer::mit_framebuffer(|fb| {
        let info = fb.info();
        (info.width, info.height)
    })
    .expect("Framebuffer")
}

// ===========================================================================

/// DIE MESSUNG. Ein Bericht — plus die EINE Zusage, auf der der Entwurf
/// steht: Ein Streifen ist billiger als ein Vollbild.
#[test_case]
fn messung_fenster_zeichnen() {
    if !programme_vorhanden() {
        return;
    }
    let (bb, bh) = bildschirm();
    // So gross, wie ein maximiertes Fenster wäre (Titelleiste und
    // Taskleiste gehen ab) — das ist die Fläche, um die es beim Scrollen
    // wirklich geht.
    let breite = bb;
    let hoehe = bh.saturating_sub(72);
    let breite_text = format!("{}", breite);
    let hoehe_text = format!("{}", hoehe);

    let ausgabe = messung_laufen_lassen(&["messung", "7", &breite_text, &hoehe_text]);
    serial_println!("--- Rohausgabe von 'messung 7' ---\n{}", ausgabe);

    let hole = |schluessel: &str| {
        zahl_aus(&ausgabe, schluessel)
            .unwrap_or_else(|| panic!("{} fehlt in der Ausgabe", schluessel))
    };
    let gemessene_breite = hole("FENSTER_BREITE=");
    let gemessene_hoehe = hole("FENSTER_HOEHE=");
    let gekuerzt = hole("HOEHE_GEKUERZT=") == 1;
    let malen = hole("MALEN_US=");
    let vollbild = hole("VOLLBILD_US=");
    let streifen = hole("STREIFEN_US=");
    let streifen_zeilen = hole("STREIFEN_ZEILEN=");
    let block = hole("BLOCK_US=");
    let block_kante = hole("BLOCK_KANTE=");

    serial_println!(
        "[MESSUNG-FENSTER] Bildschirm {}x{}, gemessenes Fenster {}x{}{}",
        bb,
        bh,
        gemessene_breite,
        gemessene_hoehe,
        if gekuerzt {
            " (HOEHE GEKUERZT — der User-Heap fasst kein volles 4K-Fenster, \
              siehe docs/fenster-syscalls.md 6)"
        } else {
            ""
        }
    );
    serial_println!(
        "[MESSUNG-FENSTER] malen (ohne Syscall) {} us | Vollbild {} us | \
         Streifen {} Zeilen {} us | Block {}x{} {} us",
        malen,
        vollbild,
        streifen_zeilen,
        streifen,
        block_kante,
        block_kante,
        block
    );
    let pixel = gemessene_breite * gemessene_hoehe;
    if vollbild > 0 {
        serial_println!(
            "[MESSUNG-FENSTER] Vollbild: {} Pixel in {} us = {} Mio. Pixel/s",
            pixel,
            vollbild,
            pixel / vollbild.max(1)
        );
    }
    // DAS UMSTIEGSKRITERIUM, ausgerechnet statt behauptet
    // (docs/fenster-syscalls.md 5): Scroll-Frame > 8000 us UND die Kopie
    // mehr als die Haelfte davon.
    let frame = malen + vollbild;
    let kopie_anteil = (vollbild * 100).checked_div(frame).unwrap_or(0);
    serial_println!(
        "[MESSUNG-FENSTER] Scroll-Frame (malen + uebertragen) = {} us, \
         davon Kopie {} % -> Umstiegskriterium {}",
        frame,
        kopie_anteil,
        if frame > 8000 && kopie_anteil > 50 {
            "ERFUELLT: geteilten Speicher neu bewerten"
        } else {
            "NICHT erfuellt: Pixelpuffer per Syscall bleibt"
        }
    );

    // Die einzige harte Zusage: Ein Streifen ist billiger als ein Vollbild.
    // Ohne sie waere der Bereich im Syscall sinnlos.
    assert!(
        streifen < vollbild,
        "ein Streifen ({} us) muss billiger sein als ein Vollbild ({} us) — \
         sonst braucht der Syscall kein Rechteck",
        streifen,
        vollbild
    );
}
