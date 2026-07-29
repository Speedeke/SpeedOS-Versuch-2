// tests/wecken.rs — DER WECK-LATENZ-PASS (Serie 7, Teil 0)
//
// AUSGANGSLAGE (gemessen im Serie-6-Abschluss): Eine Pipe uebertrug vom
// Prozess zum Kernel **199 KiB/s**, waehrend derselbe Ringpuffer im Kernel
// allein **241 MiB/s** schaffte. Der Faktor 1200 war NICHT das Kopieren,
// sondern die WECK-LATENZ: 4 KiB Puffer je Scheduling-Runde (20 ms).
//
// Das musste vor TLS weg, denn TLS im User-Space schickt jedes einzelne Byte
// durch genau diese Kette.
//
// DIESE DATEI BEWEIST DREI DINGE UND MISST DREI:
//
//   BEWEISE
//     1. Wecken weckt WIRKLICH SOFORT (Latenz ALT/NEU, im selben Lauf).
//     2. Ein Ping-Pong-Paar hungert niemanden aus (Fairness unter Last).
//     3. Kein Weckruf geht verloren, wenn gleichzeitig geschlossen wird.
//
//   MESSUNGEN, jeweils ALT und NEU im SELBEN LAUF
//     4. Pipe Prozess -> Kernel
//     5. Pipe Prozess -> Prozess
//     6. Durchsatz durch einen Socket-Syscall
//
// ALT/NEU IM SELBEN LAUF ist Methodik, keine Bequemlichkeit: Zwei Zahlen aus
// zwei QEMU-Starts sind auf einem Host mit anderer Last nicht vergleichbar.
// Deshalb sind das sofortige Wecken (`scheduler::sofort_wecken_setzen`) und
// die Pipe-Groesse (`pipe::kapazitaet_setzen`) zur Laufzeit umschaltbar —
// dieselbe Methodik wie `messung_serie3` beim Compositor.
//
// DIE MESSFALLE AUS DEM SERIE-6-ABSCHLUSS, hier bewusst vermieden: Wer misst,
// darf waehrenddessen nicht `hlt`-en — sonst misst er die Tick-Rate statt der
// Sache. Alle Schleifen unten geben entweder ab (`abgeben`) oder pollen
// aktiv; wo `zeit::warte_auf_interrupt()` steht, gibt es seit diesem Pass
// selbst ab, statt zu schlafen, solange ein anderer Prozess laufen kann.

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
use speed_os::prozess::{self, Pid, ProzessEnde, Warteauf};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fs, memory, netz, pci, pipe, programme, scheduler, serial_println, virtio, zeit};
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
    pci::init();
    virtio::blk::init();
    virtio::net::init();
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

/// Startet ein Programm mit frei waehlbaren Standard-Handles.
///
/// Die Besitz-Buchhaltung ist der heikle Teil und deshalb genau hier an EINER
/// Stelle: Vor dem Start wird das weitergegebene Ende ZUSAETZLICH uebernommen
/// (der neue Prozess ist ein zweiter Besitzer), danach gibt der Aufrufer
/// seine eigene Kopie ab. Wer das vergisst, bekommt nie ein Dateiende — der
/// Klassiker aus Serie 6, Teil 6.
fn starten_mit(
    name: &str,
    argumente: &[&str],
    eingabe: Option<pipe::PipeId>,
    ausgabe: Option<pipe::PipeId>,
) -> Pid {
    let pfad = programme::pfad(name);
    if let Some(id) = eingabe {
        pipe::ende_uebernehmen(id, pipe::Ende::Lesen);
    }
    if let Some(id) = ausgabe {
        pipe::ende_uebernehmen(id, pipe::Ende::Schreiben);
    }
    let pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        eingabe.map(KernelObjekt::PipeLesen),
        ausgabe.map(KernelObjekt::PipeSchreiben),
        false,
    )
    .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()));
    if let Some(id) = eingabe {
        pipe::ende_schliessen(id, pipe::Ende::Lesen);
    }
    if let Some(id) = ausgabe {
        pipe::ende_schliessen(id, pipe::Ende::Schreiben);
    }
    pid
}

/// Beendet einen Prozess und raeumt ihn ab.
fn abraeumen(pid: Pid) {
    scheduler::beenden(pid);
    scheduler::warten_auf(pid, FRIST_MS);
    scheduler::aufraeumen();
}

/// Wartet, bis ein Prozess aus dem angegebenen Grund blockiert.
fn warten_bis_blockiert(pid: Pid, grund: Warteauf, frist_ms: u64) -> bool {
    let frist = zeit::ms_seit_boot() + frist_ms;
    while zeit::ms_seit_boot() < frist {
        if scheduler::warte_grund(pid) == Some(grund) {
            return true;
        }
        // ABGEBEN, nicht schlafen: Der beobachtete Prozess soll ja gerade
        // laufen duerfen, damit er ueberhaupt an seinen Blockier-Punkt kommt.
        scheduler::abgeben();
    }
    false
}

/// Liest eine Pipe bis zum Dateiende (oder bis zur Frist) leer und faengt
/// dabei das Ende des beobachteten Prozesses ab.
///
/// DASS BEIDES IN EINER SCHLEIFE PASSIEREN MUSS, ist keine Bequemlichkeit,
/// sondern Zwang — und eine Falle, in die dieses Projekt schon einmal
/// gelaufen ist (CLAUDE.md, Serie 6 Teil 6):
///
///  * Das DATEIENDE kommt erst, wenn das Schreib-Ende des Erzeugers faellt.
///    Das haengt an seiner Handle-Tabelle und faellt erst beim ABRAEUMEN
///    (im Interrupt darf kein Speicher freigegeben werden). Ohne
///    `aufraeumen()` in dieser Schleife wartet man ewig auf ein Dateiende.
///  * `aufraeumen()` LOESCHT aber den Tabelleneintrag — und damit den
///    EXIT-CODE. Wer danach `warten_auf` fragt, bekommt `None` und haelt
///    einen sauber beendeten Prozess faelschlich fuer verschwunden.
///
/// Also wird der Exit-Code VOR jedem Abraeumen eingesammelt.
fn pipe_leeren(leitung: pipe::PipeId, pid: Option<Pid>, frist_ms: u64) -> (Vec<u8>, Option<ProzessEnde>) {
    let mut gesammelt = Vec::new();
    let mut ende = None;
    let mut puffer = alloc::vec![0u8; 4096];
    let frist = zeit::ms_seit_boot() + frist_ms;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    break;
                }
                if let Some(pid) = pid {
                    // ZUERST einsammeln, DANN abraeumen — genau in dieser
                    // Reihenfolge, siehe oben.
                    if ende.is_none() {
                        ende = scheduler::ende_abfragen(pid);
                    }
                }
                scheduler::aufraeumen();
                zeit::warte_auf_interrupt();
            }
            _ => break,
        }
    }
    if let Some(pid) = pid {
        if ende.is_none() {
            ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 5_000));
        }
    }
    (gesammelt, ende)
}

/// Sucht `SCHLUESSEL=<zahl>` in einer Ausgabe.
fn zahl_aus(ausgabe: &str, schluessel: &str) -> Option<u64> {
    let start = ausgabe.find(schluessel)? + schluessel.len();
    let rest = &ausgabe[start..];
    let ende = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..ende].parse::<u64>().ok()
}

/// Schaltet auf den ALT-Zustand (nur Timer-Pruefung, 4-KiB-Pipes) bzw. auf
/// NEU (sofortiges Wecken, 64-KiB-Pipes) und liefert eine Beschriftung.
fn modus_setzen(neu: bool) -> &'static str {
    scheduler::sofort_wecken_setzen(neu);
    pipe::kapazitaet_setzen(if neu { pipe::STANDARD_KAPAZITAET } else { 4096 });
    if neu {
        "NEU"
    } else {
        "ALT"
    }
}

/// Stellt den Auslieferungs-Zustand wieder her (jeder Test raeumt hinter sich
/// auf — sonst faerbt eine ALT-Messung den naechsten Test ein).
fn modus_zuruecksetzen() {
    scheduler::sofort_wecken_setzen(true);
    pipe::kapazitaet_setzen(pipe::STANDARD_KAPAZITAET);
}

// ===========================================================================
// BEWEIS 1: WECKEN WECKT SOFORT
// ===========================================================================

/// Wie lange dauert es vom „Platz ist frei" bis zum „der Schreiber ist wieder
/// lauffaehig"?
///
/// AUFBAU: Ein `messung 2` blaest in eine Pipe, bis sie voll ist, und
/// blockiert (`Warteauf::PipeSchreiben`). Dann nimmt der Kernel Bytes heraus
/// und stoppt, wie lange es dauert, bis der Prozess nicht mehr wartet.
///
/// GEMESSEN WIRD AKTIV POLLEND, nicht schlafend — genau die Messfalle aus dem
/// Serie-6-Abschluss. Wer hier `hlt` schriebe, bekaeme in BEIDEN Modi die
/// Tick-Rate zu sehen und haette den Unterschied wegmessen.
fn weck_latenz_messen(runden: u32) -> u64 {
    let leitung = pipe::anlegen().expect("Pipe");
    let pid = starten_mit("messung", &["messung", "2"], None, Some(leitung));

    let mut summe_us = 0u64;
    let mut gezaehlt = 0u64;
    let mut puffer = alloc::vec![0u8; 2048];

    for _ in 0..runden {
        if !warten_bis_blockiert(pid, Warteauf::PipeSchreiben(leitung), 5_000) {
            break;
        }
        // Der Startpunkt liegt VOR dem Lesen: Der Weckruf entsteht IM Lesen.
        let vor_us = zeit::us_seit_boot();
        let gelesen = pipe::lesen(leitung, &mut puffer);
        assert!(
            matches!(gelesen, pipe::PipeErgebnis::Bytes(n) if n > 0),
            "die volle Pipe muesste Bytes liefern, lieferte aber {:?}",
            gelesen
        );
        // AKTIV pollen, bis der Prozess wieder lauffaehig ist.
        let frist_us = vor_us + 50_000;
        loop {
            if scheduler::warte_grund(pid).is_none() {
                break;
            }
            if zeit::us_seit_boot() >= frist_us {
                break;
            }
            core::hint::spin_loop();
        }
        summe_us += zeit::us_seit_boot() - vor_us;
        gezaehlt += 1;
        // Dem Schreiber Zeit geben, die Pipe wieder zu fuellen.
        zeit::warte_auf_interrupt();
    }

    abraeumen(pid);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    summe_us / gezaehlt.max(1)
}

#[test_case]
fn test_wecken_ist_sofort() {
    if !programme_vorhanden() {
        return;
    }
    const RUNDEN: u32 = 20;

    modus_setzen(false);
    let alt_us = weck_latenz_messen(RUNDEN);
    modus_setzen(true);
    let neu_us = weck_latenz_messen(RUNDEN);
    modus_zuruecksetzen();

    serial_println!("  === WECK-LATENZ (Mittel aus {} Runden) ===", RUNDEN);
    serial_println!("    ALT (nur Timer-Pruefung): {} us", alt_us);
    serial_println!("    NEU (sofortiges Wecken):  {} us", neu_us);
    serial_println!(
        "    ALT ist im Mittel ein halber Timer-Tick ({} ms / 2 = {} us) —",
        zeit::ms_von_ticks(1),
        zeit::ms_von_ticks(1) * 500
    );
    serial_println!("    genau das, was die Nachpruefung im Timer kostet.");

    // Sofort heisst: im selben Aufruf, nicht im naechsten Tick.
    assert!(
        neu_us < 500,
        "sofortiges Wecken braucht {} us — das ist nicht sofort",
        neu_us
    );
    // Und der ALT-Weg muss messbar langsamer sein, sonst misst der Test nichts.
    assert!(
        alt_us > neu_us * 4,
        "ALT ({} us) ist nicht messbar langsamer als NEU ({} us) — \
         misst der Test ueberhaupt die Weck-Latenz?",
        alt_us,
        neu_us
    );
}

// ===========================================================================
// BEWEIS 2: KEIN VERLORENES WECKEN BEIM SCHLIESSEN
// ===========================================================================

/// DER GEFAEHRLICHSTE FALL des ganzen Passes.
///
/// Ein Leser schlaeft auf einer leeren Pipe. Jetzt passieren ZWEI Dinge
/// dicht hintereinander: Es kommen Daten, UND das Schreib-Ende geht zu. Wird
/// dabei ein Weckruf verschluckt, schlaeft der Leser fuer immer — er wartet
/// auf ein Dateiende, das ihm niemand mehr meldet.
///
/// Genau deshalb weckt `ende_schliessen` ebenfalls, und genau deshalb bleibt
/// die Timer-Pruefung als Sicherheitsnetz bestehen. Der Test faehrt die
/// Sequenz vielfach, weil ein Rennen sich nicht beim ersten Versuch zeigt.
#[test_case]
fn test_kein_verlorenes_wecken_beim_schliessen() {
    if !programme_vorhanden() {
        return;
    }
    const RUNDEN: usize = 25;
    let pipes_vorher = pipe::anzahl();

    for runde in 0..RUNDEN {
        let eingabe = pipe::anlegen().expect("Eingabe-Pipe");
        let ausgabe = pipe::anlegen().expect("Ausgabe-Pipe");
        // Lange Frist: Der Prozess soll NICHT von selbst fertig werden — nur
        // das Dateiende darf ihn beenden.
        let pid = starten_mit(
            "messung",
            &["messung", "4", "60000"],
            Some(eingabe),
            Some(ausgabe),
        );

        // Er MUSS erst wirklich schlafen, sonst pruefen wir das Rennen nicht.
        assert!(
            warten_bis_blockiert(pid, Warteauf::PipeLesen(eingabe), 5_000),
            "Runde {}: der Leser blockiert nicht auf der leeren Pipe",
            runde
        );

        // DATEN UND SCHLIESSEN DICHT HINTEREINANDER — ohne jede Pause
        // dazwischen. In der Zwischenzeit darf der Leser sogar schon
        // aufgewacht sein; beide Reihenfolgen muessen funktionieren.
        assert_eq!(pipe::schreiben(eingabe, b"abc"), pipe::PipeErgebnis::Bytes(3));
        pipe::ende_schliessen(eingabe, pipe::Ende::Schreiben);

        // Jetzt darf NICHTS mehr haengen: Der Leser muss die 3 Bytes sehen,
        // danach das Dateiende — und sich beenden.
        let (bericht, ende) = pipe_leeren(ausgabe, Some(pid), 5_000);
        assert_eq!(
            ende,
            Some(ProzessEnde::Beendet(0)),
            "Runde {}: der Leser wurde nicht geweckt (Ende: {:?})",
            runde,
            ende
        );
        let text = String::from_utf8_lossy(&bericht);
        assert_eq!(
            zahl_aus(&text, "PP_BYTES="),
            Some(3),
            "Runde {}: der Leser hat die Daten vor dem Dateiende verloren ({:?})",
            runde,
            text
        );

        pipe::ende_schliessen(eingabe, pipe::Ende::Lesen);
        pipe::ende_schliessen(ausgabe, pipe::Ende::Lesen);
        pipe::ende_schliessen(ausgabe, pipe::Ende::Schreiben);
        scheduler::aufraeumen();
    }

    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
    serial_println!(
        "  {} Runden 'Daten + Schliessen gleichzeitig': kein verlorenes Wecken, kein Haenger.",
        RUNDEN
    );
}

// ===========================================================================
// BEWEIS 3: FAIRNESS UNTER PING-PONG-LAST
// ===========================================================================

/// Zwei Prozesse werfen sich EINZELNE BYTES zu und wecken sich damit
/// gegenseitig, so schnell sie koennen. Ein dritter Prozess rechnet nur.
///
/// DIE FRAGE: Bekommt der Dritte noch CPU? Bei einer direkten Uebergabe an
/// den Geweckten („handoff") waere die Antwort NEIN — genau deshalb laeuft
/// der Reschedule-Punkt ueber die normale zyklische Round-Robin-Wahl
/// (Begruendung in scheduler.rs).
///
/// GEMESSEN WIRD DIE CPU-ZEIT des Dritten. Der Messende gibt dabei selbst ab
/// (Messfalle!) — wuerde er `hlt`-en, verschoebe er das Ergebnis zugunsten
/// aller anderen und maesse die Fairness nicht, sondern erzwaenge sie.
#[test_case]
fn test_fairness_unter_pingpong() {
    if !programme_vorhanden() {
        return;
    }
    modus_zuruecksetzen();
    let pipes_vorher = pipe::anzahl();
    let (weckungen_vorher, wechsel_vorher, gebremst_vorher) = scheduler::sofort_statistik();

    // Zwei Pipes ueber Kreuz: A schreibt in hin, liest aus her; B umgekehrt.
    let hin = pipe::anlegen().expect("Pipe hin");
    let her = pipe::anlegen().expect("Pipe her");
    let a = starten_mit("messung", &["messung", "5", "1"], Some(her), Some(hin));
    let b = starten_mit("messung", &["messung", "5", "0"], Some(hin), Some(her));

    // Der unbeteiligte Dritte: rechnet, gibt nie ab, wartet auf nichts.
    let rechner = scheduler::einplanen(
        prozess::zaehler_prozess(b'C').expect("Zaehler-Prozess"),
    )
    .expect("Zaehler einplanen");

    const MESSDAUER_MS: u64 = 1_000;
    let cpu_von = |pid: Pid| {
        scheduler::momentaufnahme()
            .into_iter()
            .find(|zeile| zeile.pid == pid)
            .map(|zeile| zeile.cpu_us)
            .unwrap_or(0)
    };

    // Anlaufen lassen, dann messen.
    let bis = zeit::ms_seit_boot() + 200;
    while zeit::ms_seit_boot() < bis {
        scheduler::abgeben();
    }
    let cpu_vorher = cpu_von(rechner);
    let start_us = zeit::us_seit_boot();
    let bis = zeit::ms_seit_boot() + MESSDAUER_MS;
    while zeit::ms_seit_boot() < bis {
        scheduler::abgeben();
    }
    let dauer_us = zeit::us_seit_boot() - start_us;
    let cpu_rechner = cpu_von(rechner) - cpu_vorher;

    abraeumen(a);
    abraeumen(b);
    abraeumen(rechner);
    for id in [hin, her] {
        pipe::ende_schliessen(id, pipe::Ende::Lesen);
        pipe::ende_schliessen(id, pipe::Ende::Schreiben);
    }
    scheduler::aufraeumen();

    let (weckungen, wechsel, gebremst) = scheduler::sofort_statistik();
    let anteil = cpu_rechner * 100 / dauer_us.max(1);

    serial_println!("  === FAIRNESS UNTER PING-PONG-LAST ===");
    serial_println!(
        "    Ping-Pong-Paar: {} sofortige Weckungen, {} davon mit Umplanung,",
        weckungen - weckungen_vorher,
        wechsel - wechsel_vorher
    );
    serial_println!(
        "    {} vom Fairness-Budget gebremst (das ist die Bremse bei der Arbeit).",
        gebremst - gebremst_vorher
    );
    serial_println!(
        "    Der unbeteiligte Rechner bekam {} us von {} us = {} % CPU.",
        cpu_rechner,
        dauer_us,
        anteil
    );
    serial_println!(
        "    Bei direkter Uebergabe an den Geweckten waeren es 0 % — die"
    );
    serial_println!("    zyklische Round-Robin-Wahl ist der Schutz, nicht die Bremse.");

    assert!(
        weckungen > weckungen_vorher,
        "das Ping-Pong-Paar hat gar nicht geweckt — misst der Test etwas?"
    );
    assert!(
        anteil >= 10,
        "der unbeteiligte Prozess bekam nur {} % CPU — das Ping-Pong-Paar hungert ihn aus",
        anteil
    );
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
}

// ===========================================================================
// MESSUNG 4: PIPE PROZESS -> KERNEL
// ===========================================================================

/// Misst, wie viele Bytes ein Ring-3-Programm in einer Sekunde durch eine
/// Pipe an den Kernel bringt. Liefert `(Bytes, Mikrosekunden)`.
fn durchsatz_prozess_kernel(dauer_ms: u64) -> (u64, u64) {
    let leitung = pipe::anlegen().expect("Pipe");
    let pid = starten_mit("messung", &["messung", "2"], None, Some(leitung));

    let mut puffer = alloc::vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    let start_us = zeit::us_seit_boot();
    let bis = zeit::ms_seit_boot() + dauer_ms;
    while zeit::ms_seit_boot() < bis {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(n) if n > 0 => bytes += n as u64,
            // Leer: dem Schreiber Platz machen. `warte_auf_interrupt` gibt
            // seit diesem Pass ab, statt zu schlafen — im ALT-Modus (sofort
            // aus) schlaeft es wie frueher, und genau das ist der Unterschied.
            _ => zeit::warte_auf_interrupt(),
        }
    }
    let dauer_us = zeit::us_seit_boot() - start_us;

    abraeumen(pid);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    (bytes, dauer_us)
}

/// Der REINE Ringpuffer im Kernel — die Obergrenze, die die Pipe selbst setzt.
fn durchsatz_ringpuffer(dauer_ms: u64) -> (u64, u64) {
    let roh = pipe::anlegen().expect("Roh-Pipe");
    let block = alloc::vec![b'X'; 64 * 1024];
    let mut puffer = alloc::vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    let start_us = zeit::us_seit_boot();
    let bis = zeit::ms_seit_boot() + dauer_ms;
    while zeit::ms_seit_boot() < bis {
        for _ in 0..64 {
            if let pipe::PipeErgebnis::Bytes(n) = pipe::schreiben(roh, &block) {
                bytes += n as u64;
            }
            let _ = pipe::lesen(roh, &mut puffer);
        }
    }
    let dauer_us = zeit::us_seit_boot() - start_us;
    pipe::ende_schliessen(roh, pipe::Ende::Lesen);
    pipe::ende_schliessen(roh, pipe::Ende::Schreiben);
    (bytes, dauer_us)
}

fn kib_pro_s(bytes: u64, dauer_us: u64) -> u64 {
    bytes * 1_000_000 / dauer_us.max(1) / 1024
}

#[test_case]
fn test_durchsatz_pipe_prozess_kernel() {
    if !programme_vorhanden() {
        return;
    }
    const DAUER_MS: u64 = 1_000;

    modus_setzen(false);
    let (alt_bytes, alt_us) = durchsatz_prozess_kernel(DAUER_MS);
    modus_setzen(true);
    let (neu_bytes, neu_us) = durchsatz_prozess_kernel(DAUER_MS);
    let (roh_bytes, roh_us) = durchsatz_ringpuffer(200);
    modus_zuruecksetzen();

    let alt = kib_pro_s(alt_bytes, alt_us);
    let neu = kib_pro_s(neu_bytes, neu_us);
    let roh = kib_pro_s(roh_bytes, roh_us);

    serial_println!("  === DURCHSATZ: Pipe Prozess -> Kernel ===");
    serial_println!(
        "    ALT (4 KiB Puffer, nur Timer):  {} KiB/s ({} Byte in {} us)",
        alt,
        alt_bytes,
        alt_us
    );
    serial_println!(
        "    NEU (64 KiB, sofort geweckt):   {} KiB/s = {} MiB/s",
        neu,
        neu / 1024
    );
    serial_println!("    Ringpuffer allein (Obergrenze): {} KiB/s", roh);
    if alt > 0 {
        serial_println!("    Faktor NEU/ALT: {}x", neu / alt.max(1));
    }

    assert!(alt_bytes > 0 && neu_bytes > 0, "durch die Pipe kam nichts");
    assert!(
        neu > alt,
        "NEU ({} KiB/s) ist nicht schneller als ALT ({} KiB/s)",
        neu,
        alt
    );
    // DAS ZIEL DER AUFGABE: im MiB/s-Bereich statt im KiB/s-Bereich.
    assert!(
        neu >= 1024,
        "Prozess -> Kernel liegt bei {} KiB/s — das Ziel war der MiB/s-Bereich",
        neu
    );
}

// ===========================================================================
// MESSUNG 5: PIPE PROZESS -> PROZESS
// ===========================================================================

/// Erzeuger und Verbraucher sind BEIDE Ring-3-Programme; der Kernel liest nur
/// den Schlussbericht. Das ist die Strecke, durch die spaeter jedes TLS-Byte
/// laeuft — und die einzige Messung, in der zwei Prozesse einander wecken.
fn durchsatz_prozess_prozess(dauer_ms: u64) -> (u64, u64) {
    let leitung = pipe::anlegen().expect("Daten-Pipe");
    let bericht = pipe::anlegen().expect("Bericht-Pipe");

    let dauer_text = alloc::format!("{}", dauer_ms);
    let senke = starten_mit(
        "messung",
        &["messung", "4", &dauer_text],
        Some(leitung),
        Some(bericht),
    );
    let quelle = starten_mit("messung", &["messung", "2"], None, Some(leitung));

    // Der Kernel haelt sich heraus und liest nur den Bericht.
    let (text_bytes, _) = pipe_leeren(bericht, Some(senke), dauer_ms + 10_000);
    abraeumen(quelle);
    scheduler::aufraeumen();
    for id in [leitung, bericht] {
        pipe::ende_schliessen(id, pipe::Ende::Lesen);
        pipe::ende_schliessen(id, pipe::Ende::Schreiben);
    }

    let text = String::from_utf8_lossy(&text_bytes);
    let bytes = zahl_aus(&text, "PP_BYTES=").unwrap_or(0);
    let ms = zahl_aus(&text, "PP_MS=").unwrap_or(0);
    (bytes, ms * 1_000)
}

#[test_case]
fn test_durchsatz_pipe_prozess_prozess() {
    if !programme_vorhanden() {
        return;
    }
    const DAUER_MS: u64 = 1_000;

    modus_setzen(false);
    let (alt_bytes, alt_us) = durchsatz_prozess_prozess(DAUER_MS);
    modus_setzen(true);
    let (neu_bytes, neu_us) = durchsatz_prozess_prozess(DAUER_MS);
    modus_zuruecksetzen();

    let alt = kib_pro_s(alt_bytes, alt_us);
    let neu = kib_pro_s(neu_bytes, neu_us);

    serial_println!("  === DURCHSATZ: Pipe Prozess -> Prozess ===");
    serial_println!("    ALT: {} KiB/s ({} Byte in {} us)", alt, alt_bytes, alt_us);
    serial_println!(
        "    NEU: {} KiB/s = {} MiB/s ({} Byte)",
        neu,
        neu / 1024,
        neu_bytes
    );
    serial_println!(
        "    Hier weckt ein PROZESS den anderen — der Reschedule-Punkt liegt"
    );
    serial_println!("    im Syscall-Rueckweg, nicht in einer Kernel-Schleife.");

    assert!(neu_bytes > 0, "zwischen den Prozessen kam nichts an");
    assert!(
        neu > alt,
        "NEU ({} KiB/s) ist nicht schneller als ALT ({} KiB/s)",
        neu,
        alt
    );
    assert!(
        neu >= 1024,
        "Prozess -> Prozess liegt bei {} KiB/s — das Ziel war der MiB/s-Bereich",
        neu
    );
}

// ===========================================================================
// MESSUNG 6: DURCHSATZ DURCH EINEN SOCKET-SYSCALL
// ===========================================================================

/// Wie viele Bytes bringt ein Ring-3-Programm durch `sende`?
///
/// EHRLICHE EINORDNUNG, die in den Bericht gehoert: Sockets sind von diesem
/// Pass NICHT betroffen, und zwar aus einem nachpruefbaren Grund — kein
/// Prozess WARTET je auf einen Socket. `empfange` ist laut ABI
/// nicht-blockierend (0 = noch nichts da, docs/syscalls.md), es gibt also
/// keinen `Warteauf::Socket`-Zustand und damit niemanden, den ein
/// ankommendes Paket wecken koennte. Die Weck-Maschinerie ist trotzdem
/// allgemein gebaut (`scheduler::wecken` nimmt jeden `Warteauf`), sodass ein
/// spaeteres blockierendes `empfange` nur einen Aufruf im Zustell-Pfad
/// braucht.
///
/// Gemessen wird deshalb hier der WEG: Ring 3 -> `int 0x80` -> Zeiger
/// pruefen -> copy-in -> UDP -> virtio-net. Genau der Weg, den TLS je
/// Datensatz zweimal geht.
#[test_case]
fn test_durchsatz_socket_syscall() {
    if !programme_vorhanden() {
        return;
    }
    modus_zuruecksetzen();

    // Netz hochbringen; ohne Geraet/Lease wird sauber uebersprungen.
    let gateway = match netz::dhcp::beziehen(4_000) {
        Some(lease) => {
            netz::konfig_setzen_dhcp(
                lease.ip,
                lease.maske,
                lease.gateway,
                lease.dns,
                lease.lease_sekunden,
            );
            lease.gateway
        }
        None => {
            serial_println!("  (Socket-Messung uebersprungen: keine DHCP-Lease)");
            return;
        }
    };
    // Port 9 = DISCARD. slirp verwirft es, und genau das ist gewollt: Wir
    // messen unseren Sendeweg, nicht die Gegenstelle.
    let ip_zahl = ((gateway.0[0] as u64) << 24)
        | ((gateway.0[1] as u64) << 16)
        | ((gateway.0[2] as u64) << 8)
        | gateway.0[3] as u64;

    let bericht = pipe::anlegen().expect("Bericht-Pipe");
    let ip_text = alloc::format!("{}", ip_zahl);
    let pid = starten_mit(
        "messung",
        &["messung", "6", &ip_text, "9", "1000"],
        None,
        Some(bericht),
    );
    let (text_bytes, _) = pipe_leeren(bericht, Some(pid), 20_000);
    scheduler::aufraeumen();
    pipe::ende_schliessen(bericht, pipe::Ende::Lesen);
    pipe::ende_schliessen(bericht, pipe::Ende::Schreiben);

    let text = String::from_utf8_lossy(&text_bytes);
    if let Some(code) = zahl_aus(&text, "SOCK_FEHLER=") {
        serial_println!("  (Socket-Messung uebersprungen: Fehlercode {})", code);
        return;
    }
    let bytes = zahl_aus(&text, "SOCK_BYTES=").unwrap_or(0);
    let ms = zahl_aus(&text, "SOCK_MS=").unwrap_or(0);
    let aufrufe = zahl_aus(&text, "SOCK_AUFRUFE=").unwrap_or(0);
    let us = ms * 1_000;

    serial_println!("  === DURCHSATZ: Socket-Syscall (UDP, 1 KiB je Datagramm) ===");
    serial_println!(
        "    {} Byte in {} ms = {} KiB/s ueber {} x sende()",
        bytes,
        ms,
        kib_pro_s(bytes, us),
        aufrufe
    );
    if let Some(je_aufruf) = us.checked_div(aufrufe) {
        serial_println!("    {} us je sende() inkl. Geraete-Uebergabe", je_aufruf);
    }
    serial_println!(
        "    KEIN Weck-Anteil: `empfange` ist laut ABI nicht-blockierend, es"
    );
    serial_println!("    wartet also nie ein Prozess auf einen Socket.");

    assert!(bytes > 0, "durch den Socket ging kein einziges Byte");
}
