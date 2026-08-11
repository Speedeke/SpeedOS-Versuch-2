// tests/browser_speicher.rs — 30 SITZUNGEN, UND DANACH IST ALLES ZURUECK
//
// ===========================================================================
// WAS EINE SITZUNG HIER IST
//
// Nicht „eine Seite laden". Der Browser ist das Programm mit dem meisten
// beweglichen Zustand im System, und ein Leck versteckt sich in genau den
// Dingen, die eine einzelne Ladung nicht anfasst:
//
//   fuenf Seiten laden (eine davon eine Fehlerseite),
//   drei Tabs oeffnen und in ihnen laden,
//   zwischen allen Tabs wechseln,
//   dreimal zurueck, zweimal vor,
//   zwei Tabs wieder schliessen,
//   Prozess beenden.
//
// Das ist `browser --zyklus`, und davon laufen hier DREISSIG hintereinander.
//
// ===========================================================================
// WAS GEZAEHLT WIRD — und was davon herausgerechnet werden MUSS
//
//   FRAMES   physische Seiten. Die eigentliche Frage.
//   FENSTER  offene Prozess-Fenster im Manager.
//   SOCKETS  (hier immer 0 — es geht kein Netz)
//
// Zwei Posten wachsen bei JEDEM Test und sind KEIN Leck; sie werden
// benannt und herausgerechnet, nicht ignoriert (die Messfalle aus dem
// Serie-6-Abschluss):
//
//   * Der KERNEL-LOG-PUFFER waechst mit jeder Ausgabe bis 64 KiB.
//   * Die P1-TABELLEN-BUCHHALTUNG: `memory::allocate_pages` vergibt
//     virtuellen Raum MONOTON; alle 512 Seiten bleibt eine P1-Tabelle im
//     Kernel-Adressraum zurueck. Das ist kein Prozess-Leck, sondern eine
//     bekannte, AUSGERECHNETE Unschaerfe (docs/grenzen.md §4) — und sie
//     wird hier ausgerechnet statt die Bilanz aufzuweichen.

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

const ZYKLEN: usize = 30;
const FRIST_MS: u64 = 60_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Eine Sitzung: `browser --zyklus`, Ausgabe einsammeln.
fn eine_sitzung() -> (Option<ProzessEnde>, String) {
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("browser");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        &["browser", "speedos:info", "--zyklus", "--fenster=800x600"],
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
    (ende, String::from_utf8_lossy(&gesammelt).into_owned())
}

fn zahl(ausgabe: &str, schluessel: &str) -> u64 {
    for zeile in ausgabe.lines() {
        if let Some(rest) = zeile.strip_prefix(schluessel) {
            return rest.trim().parse::<u64>().unwrap_or(0);
        }
    }
    0
}

// ===========================================================================

/// **DER SPEICHER-PASS.** 30 Sitzungen, und danach ist alles zurueck.
#[test_case]
fn test_dreissig_sitzungen_lecken_nicht() {
    if !programme_vorhanden() {
        return;
    }

    // EIN VORLAUF, dessen Zahlen NICHT zaehlen: Beim ersten Start
    // entstehen einmalige Dinge (Programm-Cache im VFS, Fenster-Manager-
    // Strukturen, der erste Log-Puffer). Sie als Leck zu zaehlen waere
    // derselbe Fehler wie eine Messung ohne Aufwaermrunde.
    let (ende, ausgabe) = eine_sitzung();
    assert_eq!(
        ende,
        Some(ProzessEnde::Beendet(0)),
        "schon der Vorlauf ist nicht sauber durchgelaufen"
    );
    assert_eq!(
        zahl(&ausgabe, "TABS="),
        2,
        "nach dem Zyklus sollen zwei Tabs uebrig sein (4 auf, 2 zu)"
    );
    let spitze_vorlauf = zahl(&ausgabe, "HEAP_SPITZE=");
    scheduler::aufraeumen();

    let (frei_vorher, gesamt) = memory::frame_statistik();
    let fenster_vorher = fenster::prozess_fenster_anzahl();
    let log_vorher = speed_os::protokoll::puffer_bytes();

    let mut spitze_max = 0u64;
    let mut fehlschlaege = 0;
    for runde in 0..ZYKLEN {
        let (ende, ausgabe) = eine_sitzung();
        if ende != Some(ProzessEnde::Beendet(0)) {
            serial_println!("  Runde {}: Ende {:?}", runde, ende);
            fehlschlaege += 1;
        }
        spitze_max = spitze_max.max(zahl(&ausgabe, "HEAP_SPITZE="));
        scheduler::aufraeumen();
    }

    let (frei_nachher, _) = memory::frame_statistik();
    let fenster_nachher = fenster::prozess_fenster_anzahl();
    let log_nachher = speed_os::protokoll::puffer_bytes();

    let belegt_vorher = (gesamt - frei_vorher) as u64;
    let belegt_nachher = (gesamt - frei_nachher) as u64;

    // --- DIE SCHRANKE, ausgerechnet statt geraten ---
    //
    // (a) Der Kernel-Log-Puffer: gewachsen um so viele Bytes, also
    //     hoechstens so viele Frames.
    let log_frames = (log_nachher.saturating_sub(log_vorher) as u64).div_ceil(4096) + 1;
    // (b) Die P1-Buchhaltung: `allocate_pages` vergibt virtuellen Raum
    //     monoton; alle 512 Seiten bleibt EINE P1-Tabelle zurueck. Ein
    //     Browser-Prozess mappt gemessen ~300 Seiten (Programm-Image,
    //     Heap, Stack, Fensterpuffer) — bei 30 Zyklen sind das 9 000
    //     Seiten, also hoechstens 18 P1-Tabellen. Aufgerundet 24.
    let p1_frames = (ZYKLEN as u64 * 300) / 512 + 6;
    let schranke = log_frames + p1_frames;

    serial_println!(
        "[SPEICHER] {} Sitzungen: Frames {} -> {} (+{}), Schranke +{} \
         (Log {} + P1 {})",
        ZYKLEN,
        belegt_vorher,
        belegt_nachher,
        belegt_nachher.saturating_sub(belegt_vorher),
        schranke,
        log_frames,
        p1_frames
    );
    serial_println!(
        "[SPEICHER] Fenster {} -> {} | Heap-Spitze je Sitzung: Vorlauf {} B, \
         hoechstens {} B",
        fenster_vorher,
        fenster_nachher,
        spitze_vorlauf,
        spitze_max
    );

    assert_eq!(fehlschlaege, 0, "{} Sitzungen liefen nicht sauber", fehlschlaege);
    assert_eq!(
        fenster_nachher, fenster_vorher,
        "es sind Fenster offen geblieben"
    );
    assert!(
        belegt_nachher <= belegt_vorher + schranke,
        "{} Sitzungen haben {} Frames gekostet (erlaubt: {})",
        ZYKLEN,
        belegt_nachher.saturating_sub(belegt_vorher),
        schranke
    );

    // Und die Heap-Spitze eines Prozesses darf ueber die Zyklen nicht
    // WACHSEN — sonst leckt der Browser in sich selbst, was von aussen
    // wie „alles in Ordnung" aussaehe (der Adressraum faellt ja am Ende).
    assert!(
        spitze_max <= spitze_vorlauf.saturating_mul(2).max(spitze_vorlauf + 1024 * 1024),
        "die Heap-Spitze ist von {} auf {} Byte gewachsen — der Browser \
         leckt innerhalb seiner eigenen Sitzung",
        spitze_vorlauf,
        spitze_max
    );
}
