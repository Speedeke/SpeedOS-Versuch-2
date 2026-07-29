// tests/tlsspike.rs — Der TLS-Machbarkeitsnachweis, echt in Ring 3
//
// Der Spike selbst (`userland/tlsspike`) beantwortet EINE Frage: Kann
// `rustls` in SpeedOS überhaupt laufen? Dieser Test führt ihn aus und hält
// das Ergebnis fest — inklusive der Zahl, um die es eigentlich geht: dem
// Heap-Bedarf.
//
// Geprüft wird ausserdem der neue Unterbau für sich:
//   * `SYS_SPEICHER` (14) — der User-Heap, inklusive der harten Obergrenze
//     und der Ablehnung aus dem Kernel-Prozess.
//   * dass ein Prozess seinen Heap beim Ende vollständig zurückgibt.

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
use speed_os::{allocator, fs, memory, pipe, programme, scheduler, serial_println, zeit, zufall};
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
    allocator::heap_erweitern(512).expect("Heap-Erweiterung fehlgeschlagen");

    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();
    fs::init();
    fs::platte_automounten();
    programme::installieren();
    programme::ca_buendel_installieren();
    scheduler::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

const FRIST_MS: u64 = 120_000;

/// Wartet, bis der Zufallsgenerator gesät ist.
///
/// NÖTIG, WEIL DER TESTKERNEL KEINEN NACHSAAT-TASK HAT: Im Betrieb erledigt
/// das `zufall::nachsaat_task` (main.rs, alle 5 s). Hier gibt es keinen
/// Executor, also wird von Hand angestossen, sobald die Schwelle erreicht
/// ist — genau wie in `tests/zufall.rs`.
///
/// Ohne das lief der Spike in seine eigene 10-Sekunden-Frist und beendete
/// sich mit `NichtGesaet` — korrekt, aber es misst dann den RNG statt TLS.
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
                // Exit-Code VOR dem Abraeumen einsammeln (CLAUDE.md).
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
        ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 10_000));
    }
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    (ende, String::from_utf8_lossy(&gesammelt).into_owned())
}

// ===========================================================================
// 1. DER SPIKE
// ===========================================================================

/// DIE FRAGE DIESES GANZEN ABSCHNITTS: Läuft `rustls` in SpeedOS?
///
/// Der Spike baut eine ClientConfig samt Wurzelzertifikaten und legt eine
/// `UnbufferedClientConnection` an. **Er führt keinen Handshake** — das ist
/// der nächste Schritt und ausdrücklich nicht dieser.
#[test_case]
fn test_spike_laeuft_in_ring3() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    let (ende, ausgabe) = starten_und_lesen("tlsspike", &["tlsspike", "example.com"]);

    serial_println!("  === AUSGABE DES SPIKES ===");
    for zeile in ausgabe.lines() {
        serial_println!("  | {}", zeile);
    }
    serial_println!("  === Ende (Prozess: {:?}) ===", ende);

    assert_eq!(
        ende,
        Some(ProzessEnde::Beendet(0)),
        "der Spike ist nicht sauber durchgelaufen"
    );
    // Die Marken, an denen die einzelnen Stufen hängen:
    for marke in [
        "1) Heap:",
        "2) Zufall:",
        "3) Zeit:",
        "4) Wurzeln:",
        "5) Krypto:",
        "6) Konfig:  ClientConfig steht",
        "7) Zustand: ClientConnection",
        "rustls laeuft in Ring 3",
    ] {
        assert!(
            ausgabe.contains(marke),
            "die Stufe '{}' fehlt in der Ausgabe",
            marke
        );
    }
}

// ===========================================================================
// 2. DER NEUE UNTERBAU: SYS_SPEICHER
// ===========================================================================

/// Der User-Heap wächst — und hört an der Obergrenze auf.
///
/// Geprüft aus KERNEL-Sicht über `scheduler::heap_erweitern`; die
/// Ring-3-Seite deckt der Spike ab (ohne Heap käme er nicht bis Stufe 1).
#[test_case]
fn test_heap_syscall_grenzen() {
    use speed_os::syscall::Fehler;

    // Aus dem KERNEL-Prozess heraus gibt es keinen User-Heap — dort ist die
    // Frage sinnlos, und der Kernel hat seinen eigenen.
    assert_eq!(
        scheduler::heap_erweitern(4096),
        Err(Fehler::NichtUnterstuetzt),
        "der Kernel-Prozess darf keinen User-Heap bekommen"
    );

    // Die Layout-Zusagen, an denen der Allocator im User-Space hängt.
    assert_eq!(
        prozess::HEAP_START,
        speed_os::elf::IMAGE_ENDE + 4096,
        "der Heap muss eine Seite Abstand zum Image halten"
    );
    // `black_box`, damit die Vergleiche nicht zur Compilezeit wegfallen —
    // sonst stünden hier Zusicherungen, die nichts zusichern.
    let heap_ende = core::hint::black_box(prozess::HEAP_ENDE);
    let stack_oben = core::hint::black_box(prozess::ELF_STACK_OBEN);
    assert!(
        heap_ende < stack_oben - 64 * 1024,
        "zwischen Heap-Ende und Stack muss ungemappter Abstand bleiben"
    );
    // Und die Obergrenze ist kleiner als die Lücke (16 MiB).
    assert!(core::hint::black_box(prozess::HEAP_MAX_BYTES) < 16 * 1024 * 1024);
    serial_println!(
        "  User-Heap: {:#x}..{:#x} ({} MiB), danach {} MiB Abstand zum Stack.",
        prozess::HEAP_START,
        prozess::HEAP_ENDE,
        prozess::HEAP_MAX_BYTES / (1024 * 1024),
        (prozess::ELF_STACK_OBEN - prozess::HEAP_ENDE) / (1024 * 1024)
    );
}

/// KEIN LECK: Der Heap eines Prozesses fliesst beim Ende vollständig zurück.
///
/// Das ist die Frage, die ein neuer Speicher-Syscall immer aufwirft — und
/// sie beantwortet sich hier von selbst: Der Heap liegt IM Adressraum des
/// Prozesses, und der fällt beim Ende als Ganzes (Serie 6, Teil 3). Der
/// Test misst es trotzdem, statt es zu glauben.
#[test_case]
fn test_heap_kein_leck() {
    if !programme_vorhanden() {
        return;
    }
    zufall_bereitmachen();
    scheduler::aufraeumen();
    let frei_vorher = memory::frame_statistik().0;

    for runde in 0..3 {
        let (ende, ausgabe) = starten_und_lesen("tlsspike", &["tlsspike"]);
        if ende != Some(ProzessEnde::Beendet(0)) {
            serial_println!("  Runde {} — Ausgabe des Spikes:", runde);
            for zeile in ausgabe.lines() {
                serial_println!("  | {}", zeile);
            }
            panic!("Runde {}: Spike endete als {:?}", runde, ende);
        }
    }
    scheduler::aufraeumen();
    let frei_nachher = memory::frame_statistik().0;

    // Die bekannte, ausgerechnete Unschärfe (CLAUDE.md, Serie-6-Abschluss):
    // `memory::allocate_pages` lässt alle 512 Seiten eine P1-Tabelle im
    // Kernel-Adressraum zurück. Bei drei Prozessen sind das höchstens ein
    // paar Frames — deshalb eine Schranke, keine byte-exakte Gleichheit.
    let verlust = frei_vorher.saturating_sub(frei_nachher);
    serial_println!(
        "  3 Spike-Läufe: {} Frames vorher, {} nachher (Differenz {}).",
        frei_vorher,
        frei_nachher,
        verlust
    );
    assert!(
        verlust <= 8,
        "{} Frames nach drei Läufen verloren — das sieht nach einem Heap-Leck aus",
        verlust
    );
}
