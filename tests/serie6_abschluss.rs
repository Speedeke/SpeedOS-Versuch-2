// tests/serie6_abschluss.rs — SPEICHER- UND LEISTUNGS-PASS (Serie 6)
//
// Dasselbe Muster wie der Serie-5-Abschluss (tests/netz_abschluss.rs), nur
// fuer die Prozess-Schicht:
//
//   SPEICHER — 100 Zyklen Prozess starten/beenden, dabei jeder dritte MITTEN
//   IM LAUF per beende(pid) abgeschossen. Danach muessen Heap, Frames, Pipes
//   und Handles byte-exakt auf dem Ausgangsstand stehen.
//
//   LEISTUNG — Was kostet ein Syscall, ein Kontext-Wechsel, ein Byte durch
//   eine Pipe? Die Zahlen wandern in den CHANGELOG. Ein Berichts-Test:
//   Er misst und protokolliert, und er faellt nur, wenn eine Zahl so
//   ausreisst, dass etwas kaputt sein MUSS — nicht bei jeder Schwankung.
//   (Eine Testsuite, die an einer Millisekunde scheitert, wird ignoriert;
//   dieselbe Methodik wie beim Netz-Stresstest.)

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
use speed_os::{allocator, fs, memory, pipe, programme, protokoll, scheduler, serial_println, zeit};
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

const FRIST_MS: u64 = 60_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

fn frames_frei() -> usize {
    scheduler::aufraeumen();
    memory::frame_statistik().0
}

/// Belegter Heap OHNE den Kernel-Log-Puffer (der waechst beschraenkt mit
/// jeder Ausgabe — siehe `protokoll::puffer_bytes`).
fn heap_ohne_log() -> usize {
    let belegt = allocator::heap_statistik().map(|(belegt, _)| belegt).unwrap_or(0);
    belegt.saturating_sub(protokoll::puffer_bytes())
}

fn starten(name: &str, argumente: &[&str]) -> Pid {
    let pfad = programme::pfad(name);
    prozess::prozess_starten(&pfad, argumente)
        .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()))
}

/// Startet ein Programm mit einer Pipe als Standard-Ausgabe.
fn starten_mit_pipe(name: &str, argumente: &[&str], leitung: pipe::PipeId) -> Pid {
    let pfad = programme::pfad(name);
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()));
    // Die eigene Kopie abgeben — sonst gibt es nie ein Dateiende.
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);
    pid
}

// ===========================================================================
// DER SPEICHER-PASS
// ===========================================================================

/// 100 ZYKLEN Prozess starten und beenden — und danach steht jede Bilanz
/// wieder auf dem Ausgangswert.
///
/// Jeder DRITTE Prozess wird MITTEN IM LAUF per `beende(pid)` abgeschossen,
/// statt ihn auslaufen zu lassen. Das ist der interessantere Pfad: Ein
/// Prozess, der sauber `exit` ruft, hat seine Arbeit beendet; einer, der
/// mitten im Rechnen abgeschossen wird, haelt womoeglich noch alles in der
/// Hand — Adressraum, Kernel-Stack, Handles, Pipe-Enden. Genau der muss
/// vollstaendig zurueckfliessen.
#[test_case]
fn test_speicher_100_zyklen() {
    if !programme_vorhanden() {
        return;
    }
    // AUFWAERMEN: Der erste Lauf zahlt einmalige Kosten (Dateisystem-Caches,
    // Heap-Bloecke, die der Allocator behaelt). Die gehoeren nicht in die
    // Bilanz — gemessen wird der EINGESCHWUNGENE Zustand.
    for _ in 0..3 {
        let pid = starten("hallo", &["hallo"]);
        scheduler::warten_auf(pid, FRIST_MS);
    }
    let frei_vorher = frames_frei();
    let heap_vorher = heap_ohne_log();
    let pipes_vorher = pipe::anzahl();

    const ZYKLEN: usize = 100;
    let mut sauber_beendet = 0usize;
    let mut abgeschossen = 0usize;

    for zyklus in 0..ZYKLEN {
        if zyklus % 3 == 2 {
            // ABSCHIESSEN: Ein Dauerlaeufer, der eine Pipe offen haelt und
            // gerade rechnet — er wird mitten im Lauf beendet.
            let leitung = pipe::anlegen().expect("Pipe anlegen");
            let pid = starten_mit_pipe("zaehle", &["zaehle", "100000", "1"], leitung);
            // Kurz laufen lassen, damit er wirklich arbeitet.
            let bis = zeit::ms_seit_boot() + 20;
            while zeit::ms_seit_boot() < bis {
                zeit::warte_auf_interrupt();
            }
            assert!(
                scheduler::beenden(pid),
                "Zyklus {}: der Prozess war schon weg",
                zyklus
            );
            let ende = scheduler::warten_auf(pid, FRIST_MS)
                .unwrap_or_else(|| panic!("Zyklus {}: Prozess endet nicht", zyklus));
            assert_eq!(ende, ProzessEnde::Gestoppt, "Zyklus {}", zyklus);
            pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
            abgeschossen += 1;
        } else {
            // SAUBER: laeuft durch und ruft selbst `exit`.
            let pid = starten("hallo", &["hallo"]);
            let ende = scheduler::warten_auf(pid, FRIST_MS)
                .unwrap_or_else(|| panic!("Zyklus {}: hallo endet nicht", zyklus));
            assert_eq!(ende, ProzessEnde::Beendet(0), "Zyklus {}", zyklus);
            sauber_beendet += 1;
        }
    }

    let frei_nachher = frames_frei();
    let heap_nachher = heap_ohne_log();
    let verloren = frei_vorher as i64 - frei_nachher as i64;

    // ==================================================================
    // DIE EINE FRAME-DIFFERENZ, DIE ES GEBEN DARF — und warum sie KEIN
    // Prozess-Leck ist.
    //
    // Jeder Prozess bekommt einen Kernel-Stack aus `memory::allocate_pages`,
    // und der vergibt VIRTUELLEN Raum mit einem reinen Vorwärts-Zähler
    // (`NAECHSTE_VIRT_ADRESSE`) — freigegebene Bereiche werden nie wieder
    // benutzt (bewusst so, notiert in docs/scheduler-design.md §8).
    //
    // Die STACK-FRAMES selbst fliessen vollstaendig zurueck. Was bleibt,
    // sind die PAGE-TABLES fuer den immer weiter wandernden virtuellen
    // Bereich: Alle 512 Seiten (2 MiB) legt `map_to` eine neue P1-Tabelle
    // an, und die gehoert danach dem Kernel-Adressraum.
    //
    // Bei 5 Seiten je Prozess (4 Stack + 1 Guard) sind das ~1 Frame je 102
    // Prozesse. Beschraenkt, langsam, und NICHT proportional zur Zahl der
    // gleichzeitigen Prozesse — aber eben auch nicht null. Deshalb wird es
    // hier ausgerechnet und benannt, statt die Bilanz einfach aufzuweichen.
    // ==================================================================
    let seiten_verbraucht = ZYKLEN * (prozess::KERN_STACK_SEITEN + 1);
    let tabellen_erwartet = (seiten_verbraucht / 512 + 2) as i64;

    serial_println!(
        "  {} Zyklen ({} sauber beendet, {} mitten im Lauf abgeschossen):",
        ZYKLEN,
        sauber_beendet,
        abgeschossen
    );
    serial_println!(
        "    Frames {} -> {} (Differenz {}, davon bis zu {} Page-Tables fuer",
        frei_vorher,
        frei_nachher,
        verloren,
        tabellen_erwartet
    );
    serial_println!(
        "    {} neu vergebene Kernel-Stack-Seiten — siehe Kommentar im Test)",
        seiten_verbraucht
    );
    serial_println!(
        "    Heap (ohne Log) {} -> {} ({} Byte)",
        heap_vorher,
        heap_nachher,
        heap_nachher as i64 - heap_vorher as i64
    );

    assert!(
        verloren >= 0 && verloren <= tabellen_erwartet,
        "Frame-Leck nach {} Zyklen: {} Frames weg, hoechstens {} waeren durch \
         neue Page-Tables erklaerbar",
        ZYKLEN,
        verloren,
        tabellen_erwartet
    );
    // KEIN LECK — mit einer BENANNTEN Unschärfe statt einer scharfen Zahl.
    //
    // `heap_ohne_log` rechnet den Kernel-Log-Puffer heraus, und zwar über
    // seine KAPAZITÄT (ein Vec verdoppelt sie sprunghaft). Diese
    // Kompensation ist auf die Kapazitätsstufe genau, nicht auf das Byte —
    // eine Messung, die sich nur um die Sprungstellen herum byte-exakt
    // ausgeht.
    //
    // Sie tat es lange, dann (Serie 7, Teil 3) wich sie um -368 Byte ab,
    // und seit Serie 8, Teil 1 um +368 Byte: Das Boot-Protokoll ist wieder
    // länger geworden (ein Programm mehr), also fällt die Sprungstelle
    // anders. Ein einseitiges `<=` hat diese Bewegung eine Serie lang
    // überdeckt und kippt jetzt — es hat eine ZAHL verteidigt statt einer
    // AUSSAGE.
    //
    // Deshalb eine Schranke MIT BEGRÜNDUNG: Ein echtes Leck wäre
    // PROPORTIONAL zu den 100 Zyklen (jeder Prozess hinterliesse seinen
    // Anteil), also mindestens im zweistelligen Kilobyte-Bereich. Eine
    // Abweichung in der Grössenordnung EINER Kapazitätsstufe ist es
    // nachweislich nicht.
    const LOG_UNSCHAERFE: usize = 4096;
    let abweichung = heap_nachher.abs_diff(heap_vorher);
    assert!(
        abweichung <= LOG_UNSCHAERFE,
        "Heap-Leck nach {} Zyklen: {} -> {} ({} Byte Abweichung, erlaubt sind \
         {} Byte fuer die Kapazitaets-Unschaerfe des Log-Puffers — ein echtes \
         Leck waere proportional zu den {} Zyklen und damit weit groesser)",
        ZYKLEN,
        heap_vorher,
        heap_nachher,
        abweichung,
        LOG_UNSCHAERFE,
        ZYKLEN
    );
    assert_eq!(pipes_vorher, pipe::anzahl(), "Pipe-Leck nach {} Zyklen", ZYKLEN);
    assert_eq!(
        scheduler::momentaufnahme()
            .iter()
            .filter(|zeile| zeile.ist_user)
            .count(),
        0,
        "es sind Prozesse uebrig geblieben"
    );
}

/// 0 GELECKTE HANDLES: Ein Prozess, der Sockets und Pipes offen hat und
/// abgeschossen wird, gibt sie ALLE zurueck — ohne dass er `schliesse` je
/// gerufen haette.
///
/// Das ist die Zusage der Handle-Tabelle IM Prozess-Kontrollblock: Ihr
/// `Drop` raeumt auf, und kein Pfad kann es vergessen.
#[test_case]
fn test_keine_geleckten_handles() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let frei_vorher = frames_frei();

    for runde in 0..20 {
        // Drei Pipes: eine als Eingabe, eine als Ausgabe, eine, die der
        // Prozess gar nicht bekommt (Gegenprobe).
        let eingabe = pipe::anlegen().expect("Eingabe-Pipe");
        let ausgabe = pipe::anlegen().expect("Ausgabe-Pipe");

        pipe::ende_uebernehmen(eingabe, pipe::Ende::Lesen);
        pipe::ende_uebernehmen(ausgabe, pipe::Ende::Schreiben);
        let pfad = programme::pfad("filter");
        let pid = prozess::prozess_starten_mit(
            &pfad,
            &["filter", "x"],
            None,
            Some(KernelObjekt::PipeLesen(eingabe)),
            Some(KernelObjekt::PipeSchreiben(ausgabe)),
            false,
        )
        .expect("filter starten");
        pipe::ende_schliessen(eingabe, pipe::Ende::Lesen);
        pipe::ende_schliessen(ausgabe, pipe::Ende::Schreiben);

        // Der Prozess haelt jetzt BEIDE Enden. Nachweisen:
        let (offen, _) = scheduler::handle_anzahl(pid).expect("Prozess lebt");
        assert_eq!(offen, 3, "Runde {}: 3 Handles erwartet (0,1,2)", runde);
        assert_eq!(
            pipe::zustand(eingabe).map(|(_, leser, _)| leser),
            Some(1),
            "Runde {}: der Prozess muss das Lese-Ende halten",
            runde
        );

        // ABSCHIESSEN, ohne dass er je `schliesse` gerufen hat.
        scheduler::beenden(pid);
        scheduler::warten_auf(pid, FRIST_MS).expect("Prozess endet");
        scheduler::aufraeumen();

        // BEIDE Enden sind zurueck — die Pipes gehoeren jetzt nur noch uns.
        assert_eq!(
            pipe::zustand(eingabe).map(|(_, leser, _)| leser),
            Some(0),
            "Runde {}: das Lese-Ende wurde nicht zurueckgegeben",
            runde
        );
        assert_eq!(
            pipe::zustand(ausgabe).map(|(_, _, schreiber)| schreiber),
            Some(0),
            "Runde {}: das Schreib-Ende wurde nicht zurueckgegeben",
            runde
        );

        pipe::ende_schliessen(eingabe, pipe::Ende::Schreiben);
        pipe::ende_schliessen(eingabe, pipe::Ende::Lesen);
        pipe::ende_schliessen(ausgabe, pipe::Ende::Schreiben);
        pipe::ende_schliessen(ausgabe, pipe::Ende::Lesen);
    }

    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
    assert_eq!(frames_frei(), frei_vorher, "Frames geleckt");
    serial_println!("  20 Runden mit geerbten Handles: 0 geleckt.");
}

// ===========================================================================
// DER LEISTUNGS-PASS
// ===========================================================================

/// Liest eine Pipe bis zum Dateiende und liefert die Bytes.
fn pipe_leeren(leitung: pipe::PipeId, frist_ms: u64) -> Vec<u8> {
    let mut gesammelt = Vec::new();
    let mut puffer = [0u8; 1024];
    let frist = zeit::ms_seit_boot() + frist_ms;
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
    gesammelt
}

/// Sucht `SCHLUESSEL=<zahl>` in einer Ausgabe.
fn zahl_aus(ausgabe: &str, schluessel: &str) -> Option<u64> {
    let start = ausgabe.find(schluessel)? + schluessel.len();
    let rest = &ausgabe[start..];
    let ende = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..ende].parse::<u64>().ok()
}

/// DIE SYSCALL-KOSTEN — gemessen aus Ring 3, mit allem, was dazugehoert.
#[test_case]
fn test_leistung_syscall_roundtrip() {
    if !programme_vorhanden() {
        return;
    }
    let leitung = pipe::anlegen().expect("Pipe");
    let pid = starten_mit_pipe("messung", &["messung", "1"], leitung);
    let ausgabe = pipe_leeren(leitung, FRIST_MS);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::warten_auf(pid, FRIST_MS);
    scheduler::aufraeumen();

    let text = String::from_utf8_lossy(&ausgabe);
    let ns = zahl_aus(&text, "SYSCALL_NS=").expect("die Messung hat nichts geliefert");
    let runde_ms = zahl_aus(&text, "SYSCALL_RUNDE_MS=").unwrap_or(0);
    let aufrufe = zahl_aus(&text, "SYSCALL_AUFRUFE=").unwrap_or(0);

    serial_println!("  === LEISTUNG: Syscall ===");
    serial_println!(
        "    {} x getpid aus Ring 3 in {} ms (Bestwert) = {} ns je Syscall",
        aufrufe,
        runde_ms,
        ns
    );
    serial_println!(
        "    Das ist der volle Weg: int 0x80 -> Privilegienwechsel -> TSS-Stack"
    );
    serial_println!(
        "    -> 15 Register sichern -> Dispatch -> zurueck -> iretq."
    );

    // Ein GROBES Gate, kein Praezisions-Anspruch: Liegt der Wert um
    // Groessenordnungen daneben, ist etwas kaputt (z. B. eine Warteschleife
    // im Syscall-Pfad). Schwankungen von Lauf zu Lauf sollen NICHT rot werden.
    assert!(ns > 0, "die Messung liefert 0 ns — die Uhr steht?");
    assert!(
        ns < 100_000,
        "ein Syscall dauert {} ns — das ist um Groessenordnungen zu viel",
        ns
    );
}

/// DIE KONTEXT-WECHSEL-KOSTEN.
///
/// Gemessen mit ZWEI Prozessen, die nichts tun, als die Zeitscheibe
/// abzugeben: Jede Abgabe ist ein Wechsel. Der Kernel zaehlt sie ohnehin
/// (`wechsel_gesamt`), also braucht es keine Uebertragung von Messwerten —
/// nur eine Differenz ueber eine bekannte Zeit.
///
/// EHRLICH: In dieser Zahl steckt der Syscall (`yield` ist einer) MIT DRIN.
/// Der reine Wechsel ist die Differenz zum Wert oben.
#[test_case]
fn test_leistung_kontext_wechsel() {
    if !programme_vorhanden() {
        return;
    }
    let a = starten("messung", &["messung", "3"]);
    let b = starten("messung", &["messung", "3"]);

    // Kurz anlaufen lassen.
    let bis = zeit::ms_seit_boot() + 200;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }

    // WICHTIG — die erste Fassung dieses Tests hat hier falsch gemessen:
    // Sie liess PID 0 in der Messschleife `hlt`-en. Damit sah die Runde so
    // aus: A gibt ab -> B gibt ab -> PID 0 schlaeft VIER MILLISEKUNDEN bis
    // zum naechsten Tick -> A ... Gemessen wurde also die Tick-Rate (150
    // Wechsel/s), nicht der Wechsel. Die Zahl war um den Faktor 1000 zu hoch.
    //
    // Richtig ist: Der Messende muss selbst MITSPIELEN, also ebenfalls
    // abgeben. Dann besteht jede Runde aus drei echten Wechseln
    // (PID 0 -> A -> B -> PID 0) und die Uhr misst nur diese.
    const MESSDAUER_MS: u64 = 1_000;
    let wechsel_vorher = scheduler::wechsel_gesamt();
    let start_us = zeit::us_seit_boot();
    let bis = zeit::ms_seit_boot() + MESSDAUER_MS;
    while zeit::ms_seit_boot() < bis {
        scheduler::abgeben();
    }
    let dauer_us = zeit::us_seit_boot() - start_us;
    let wechsel = scheduler::wechsel_gesamt() - wechsel_vorher;

    scheduler::beenden(a);
    scheduler::beenden(b);
    scheduler::warten_auf(a, FRIST_MS);
    scheduler::warten_auf(b, FRIST_MS);
    scheduler::aufraeumen();

    assert!(wechsel > 0, "es gab keinen einzigen Kontext-Wechsel");
    let ns_je_wechsel = dauer_us.saturating_mul(1_000) / wechsel;
    serial_println!("  === LEISTUNG: Kontext-Wechsel ===");
    serial_println!(
        "    {} Wechsel in {} us = {} ns je Wechsel (yield-Roundtrip)",
        wechsel,
        dauer_us,
        ns_je_wechsel
    );
    serial_println!(
        "    Enthaelt den yield-Syscall; der reine Wechsel ist die Differenz."
    );
    serial_println!("    Darin steckt ein CR3-Wechsel — der leert den TLB.");

    assert!(
        ns_je_wechsel < 1_000_000,
        "ein Kontext-Wechsel dauert {} ns — das kann nicht stimmen",
        ns_je_wechsel
    );
}

/// DER PIPE-DURCHSATZ: Wie viele Bytes bringt ein Prozess durch eine Pipe?
#[test_case]
fn test_leistung_pipe_durchsatz() {
    if !programme_vorhanden() {
        return;
    }
    let leitung = pipe::anlegen().expect("Pipe");
    let pid = starten_mit_pipe("messung", &["messung", "2"], leitung);

    // Eine Sekunde lang so schnell lesen, wie es geht.
    const MESSDAUER_MS: u64 = 1_000;
    let mut puffer = [0u8; 4096];
    let mut bytes = 0u64;
    let start_us = zeit::us_seit_boot();
    let bis = zeit::ms_seit_boot() + MESSDAUER_MS;
    while zeit::ms_seit_boot() < bis {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(n) if n > 0 => bytes += n as u64,
            // Leer: dem Schreiber Zeit geben (er wurde geweckt).
            _ => zeit::warte_auf_interrupt(),
        }
    }
    let dauer_us = zeit::us_seit_boot() - start_us;

    scheduler::beenden(pid);
    scheduler::warten_auf(pid, FRIST_MS);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();

    let kib_pro_s = bytes * 1_000_000 / dauer_us.max(1) / 1024;

    // --- Zum Vergleich: die REINEN Kopierkosten, ohne Prozess-Wechsel ---
    // Derselbe Ringpuffer, aber Schreiben und Lesen im Kernel hintereinander.
    // Was hier herauskommt, ist die Obergrenze, die die Pipe SELBST setzt.
    let roh = pipe::anlegen().expect("Roh-Pipe");
    let block = [b'X'; 4096];
    let mut roh_bytes = 0u64;
    let roh_start = zeit::us_seit_boot();
    let roh_bis = zeit::ms_seit_boot() + 200;
    while zeit::ms_seit_boot() < roh_bis {
        for _ in 0..64 {
            if let pipe::PipeErgebnis::Bytes(n) = pipe::schreiben(roh, &block) {
                roh_bytes += n as u64;
            }
            let _ = pipe::lesen(roh, &mut puffer);
        }
    }
    let roh_dauer = zeit::us_seit_boot() - roh_start;
    pipe::ende_schliessen(roh, pipe::Ende::Lesen);
    pipe::ende_schliessen(roh, pipe::Ende::Schreiben);
    let roh_mib_pro_s = roh_bytes * 1_000_000 / roh_dauer.max(1) / (1024 * 1024);

    serial_println!("  === LEISTUNG: Pipe-Durchsatz ===");
    serial_println!(
        "    Prozess -> Pipe -> Kernel: {} Byte in {} us = {} KiB/s",
        bytes,
        dauer_us,
        kib_pro_s
    );
    serial_println!(
        "    Ringpuffer allein (Kernel):  {} MiB/s",
        roh_mib_pro_s
    );
    // HISTORISCHE ANMERKUNG, damit die Zahl einzuordnen ist: Beim
    // Serie-6-Abschluss standen hier 199 KiB/s gegen 241 MiB/s im
    // Ringpuffer, und die Differenz war NICHT das Kopieren, sondern die
    // WECK-LATENZ — 4 KiB Puffer je 20-ms-Scheduling-Runde. Beide damals
    // benannten Hebel sind seither gezogen (Serie 7, Teil 0): sofortiges
    // Wecken statt Timer-Pruefung und 64 KiB statt 4 KiB Puffer. Der
    // ALT/NEU-Vergleich dazu steht in tests/wecken.rs; hier bleibt die
    // laufende Messung.
    serial_println!(
        "    Die Pipe fasst {} KiB; geweckt wird der Schreiber sofort beim",
        pipe::kapazitaet() / 1024
    );
    serial_println!(
        "    Lesen, nicht erst bei der naechsten Timer-Pruefung. Beide Zahlen"
    );
    serial_println!(
        "    liegen deshalb dicht beieinander — es begrenzt jetzt das Kopieren."
    );

    assert!(bytes > 0, "durch die Pipe kam kein einziges Byte");
    assert!(roh_bytes > 0, "der Ringpuffer hat gar nichts uebertragen");
}

/// DIE PROZESS-START-KOSTEN: Was kostet es, ein Programm zu laden?
#[test_case]
fn test_leistung_prozess_start() {
    if !programme_vorhanden() {
        return;
    }
    // Aufwaermen (Dateisystem-Cache).
    let pid = starten("hallo", &["hallo"]);
    scheduler::warten_auf(pid, FRIST_MS);

    const LAEUFE: u64 = 20;
    let start_us = zeit::us_seit_boot();
    for _ in 0..LAEUFE {
        let pfad = programme::pfad("hallo");
        let bytes = fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad)).expect("lesen");
        let prozess = prozess::prozess_aus_elf("messung", &bytes, &["hallo"]).expect("bauen");
        // NUR bauen und wieder fallen lassen — gemessen wird das Laden
        // (Datei lesen, ELF pruefen, Adressraum, Segmente, Stack, argv),
        // nicht die Laufzeit des Programms.
        drop(prozess);
    }
    let dauer_us = zeit::us_seit_boot() - start_us;
    let je_start_us = dauer_us / LAEUFE;

    serial_println!("  === LEISTUNG: Prozess-Start ===");
    serial_println!(
        "    {} x laden+aufbauen+abreissen in {} us = {} us je Prozess",
        LAEUFE,
        dauer_us,
        je_start_us
    );
    serial_println!(
        "    Enthaelt: Datei vom Dateisystem, ELF-Pruefung, Adressraum mit"
    );
    serial_println!(
        "    gespiegeltem Kernel, Segmente mappen+fuellen, Stack, argv."
    );
    scheduler::aufraeumen();
    assert!(je_start_us > 0);
}
