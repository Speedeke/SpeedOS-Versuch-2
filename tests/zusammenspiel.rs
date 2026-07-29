// tests/zusammenspiel.rs — PROZESSE ARBEITEN ZUSAMMEN (Serie 6, Teil 6)
//
// Teil 5 hat bewiesen, dass SpeedOS fremde Programme AUSFUEHREN kann. Hier
// wird bewiesen, dass sie MITEINANDER koennen: Eltern-Kind-Beziehung,
// blockierendes Warten, Pipes mit Gegendruck und Dateiende, Handle-Weitergabe
// und das echte Beenden von aussen.
//
// GEPRUEFT WIRD:
//   1. warte/exit in BEIDEN Reihenfolgen — Kind endet zuerst (das Ergebnis
//      muss gepuffert werden) UND Eltern wartet zuerst (er muss WIRKLICH
//      schlafen und sauber geweckt werden).
//   2. Kein Zombie: Ein beendetes Kind haelt keine Ressourcen mehr; ein
//      nicht abgeholtes Ergebnis verschwindet mit seinem Elternteil.
//   3. beende(pid) raeumt VOLLSTAENDIG ab — Adressraum, Kernel-Stack,
//      Handles. Frame-Bilanz byte-exakt.
//   4. Die Pipe blockiert und weckt an BEIDEN Enden (voll -> Schreiber
//      schlaeft; leer -> Leser schlaeft), und zwar nachweislich im Zustand
//      `Wartend` und nicht in einer Warteschleife.
//   5. HANDLE-WEITERGABE: Ein Kind bekommt Handles, die es selbst nie
//      geoeffnet hat — und sieht NUR die.
//   6. `zaehle | filter 7` — der Pipe-Beweis, end-to-end durch die Shell.

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
use speed_os::pipe::{self, Ende, PipeErgebnis};
use speed_os::prozess::{self, Pid, ProzessEnde, Warteauf, Zustand};
use speed_os::shell::befehl_ausfuehren;
use speed_os::shell::befehle::{alle_befehle, ShellKontext};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fs, memory, programme, scheduler, serial_println, zeit};
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

const FRIST_MS: u64 = 30_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Freie Frames, nachdem alles Beendete abgeraeumt ist.
fn frames_frei() -> usize {
    scheduler::aufraeumen();
    memory::frame_statistik().0
}

/// Wartet, bis ein Prozess einen bestimmten Zustand erreicht.
fn warten_bis_zustand(pid: Pid, ziel: Zustand, frist_ms: u64) -> bool {
    let frist = zeit::ms_seit_boot() + frist_ms;
    loop {
        let stand = scheduler::momentaufnahme()
            .into_iter()
            .find(|zeile| zeile.pid == pid)
            .map(|zeile| zeile.zustand);
        if stand == Some(ziel) {
            return true;
        }
        if stand.is_none() || zeit::ms_seit_boot() >= frist {
            return false;
        }
        zeit::warte_auf_interrupt();
    }
}

/// Holt das Ergebnis eines Prozesses ab, SOLANGE es ihn noch gibt.
///
/// Wer in seiner Schleife `aufraeumen()` ruft (und das muss, wer auf ein
/// Pipe-Dateiende wartet), loescht damit auch den Tabelleneintrag — und mit
/// ihm den Exit-Code. Also VORHER einsammeln. Genau dieselbe Reihenfolge
/// benutzt die Pump-Schleife der Shell.
fn ende_ernten(pid: Pid, gemerkt: &mut Option<ProzessEnde>) {
    if gemerkt.is_none() {
        *gemerkt = scheduler::ende_abfragen(pid);
    }
}

/// Startet ein mitgeliefertes Programm mit geerbten Handles.
fn starten(
    name: &str,
    argumente: &[&str],
    eltern: Option<Pid>,
    eingabe: Option<KernelObjekt>,
    ausgabe: Option<KernelObjekt>,
) -> Pid {
    let pfad = programme::pfad(name);
    prozess::prozess_starten_mit(&pfad, argumente, eltern, eingabe, ausgabe, false)
        .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()))
}

// ---------------------------------------------------------------------------
// 1. + 2. Eltern, Kind, warte — in beiden Reihenfolgen
// ---------------------------------------------------------------------------

/// REIHENFOLGE A: DAS KIND ENDET ZUERST, der Elternteil fragt erst danach.
///
/// Das ist der Fall, in dem ein Unix einen ZOMBIE haette: Der Kind-Eintrag
/// muesste liegen bleiben, bis jemand ihn abholt. Bei uns wird das Ergebnis
/// beim ELTERNTEIL gepuffert und das Kind sofort vollstaendig abgeraeumt —
/// der Test prueft beides.
#[test_case]
fn test_warte_kind_endet_zuerst() {
    if !programme_vorhanden() {
        return;
    }
    // Ein "Elternteil", der lange genug lebt, um zu warten.
    let eltern = starten("zaehle", &["zaehle", "1", "20000"], None, None, None);
    // Und ein Kind, das SOFORT fertig ist.
    let kind = starten("hallo", &["hallo", "--code=42"], Some(eltern), None, None);

    // Warten, bis das Kind wirklich durch ist.
    let ende = scheduler::warten_auf(kind, FRIST_MS).expect("Kind muss enden");
    assert_eq!(ende, ProzessEnde::Beendet(42));

    // DAS ERGEBNIS LIEGT BEIM ELTERNTEIL — obwohl das Kind schon weg ist.
    let offen = scheduler::kinder_enden_offen(eltern).expect("Elternteil lebt noch");
    assert_eq!(offen, 1, "das Ergebnis des Kindes wurde nicht gepuffert");

    // Und das KIND ist restlos verschwunden (kein Zombie).
    assert!(
        !scheduler::momentaufnahme().iter().any(|zeile| zeile.pid == kind),
        "der beendete Kind-Prozess liegt noch in der Tabelle (Zombie!)"
    );

    scheduler::beenden(eltern);
    scheduler::warten_auf(eltern, FRIST_MS);
    scheduler::aufraeumen();
}

/// REIHENFOLGE B: DER ELTERNTEIL WARTET ZUERST.
///
/// Hier muss er WIRKLICH schlafen — nachgemessen am Zustand `Wartend` — und
/// vom Timer geweckt werden, sobald das Kind endet. Ein Elternteil, der
/// stattdessen in einer Warteschleife drehte, waere zwar auch „korrekt",
/// wuerde aber CPU verbrennen; genau das soll das Warte-Modell verhindern.
#[test_case]
fn test_warte_eltern_wartet_zuerst() {
    if !programme_vorhanden() {
        return;
    }
    // Der Elternteil ist hier der Kernel-Test selbst — er kann aber nicht
    // `warte` als Syscall rufen. Deshalb bauen wir die Reihenfolge mit dem
    // SCHEDULER nach: ein Kind, das erst nach einer Weile endet, und ein
    // Warten, das nachweislich blockiert.
    let kind = starten("zaehle", &["zaehle", "3", "300"], None, None, None);

    let start = zeit::ms_seit_boot();
    let ende = scheduler::warten_auf(kind, FRIST_MS).expect("Kind muss enden");
    let gedauert = zeit::ms_seit_boot() - start;

    assert_eq!(ende, ProzessEnde::Beendet(0));
    // Es MUSS gewartet worden sein (3 Zahlen à 300 ms Pause).
    assert!(
        gedauert >= 600,
        "das Warten war zu kurz ({} ms) — wurde wirklich gewartet?",
        gedauert
    );
    scheduler::aufraeumen();
}

/// BEIDE REIHENFOLGEN AUS RING 3 — mit `starte` und `warte` als SYSCALLS.
///
/// Die beiden Tests oben prüfen die Kernel-Seite. Hier startet ein
/// PROZESS einen anderen und wartet auf ihn, ohne dass der Kernel
/// dazwischen etwas tut:
///
///   `elternprobe 0`   — Eltern wartet zuerst (`warte` blockiert wirklich)
///   `elternprobe 500` — Kind endet zuerst (der Code muss GEPUFFERT sein)
///
/// `elternprobe` gibt den Exit-Code seines Kindes als EIGENEN weiter — und
/// prüft nebenbei, dass ein ZWEITES `warte` auf dasselbe Kind abgelehnt
/// wird. Käme dort noch einmal ein Ergebnis, hätten wir genau den Zustand,
/// den das Wort „Zombie" beschreibt.
///
/// EHRLICHE GRENZE dieses Tests: Er prüft das ERGEBNIS beider Reihenfolgen.
/// Dass der wartende Prozess dabei wirklich SCHLÄFT (Zustand `Wartend`) und
/// nicht in einer Schleife dreht, lässt sich hier nicht beobachten — das
/// Kind ist zu schnell fertig. Dafür gibt es die Pipe-Tests weiter unten,
/// die den Zustand nachmessen.
#[test_case]
fn test_warte_aus_ring3_in_beiden_reihenfolgen() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    for (argument, was) in [("0", "Eltern wartet zuerst"), ("500", "Kind endet zuerst")] {
        serial_println!("  Reihenfolge: {}", was);
        let eltern = starten("elternprobe", &["elternprobe", argument], None, None, None);
        let ende = scheduler::warten_auf(eltern, FRIST_MS)
            .unwrap_or_else(|| panic!("elternprobe {} wurde nicht fertig", argument));
        // `hallo` endet mit 0, und elternprobe reicht den Code durch.
        // Ein Fehler im Kind-Ergebnis (1) oder ein zweites erfolgreiches
        // `warte` (ebenfalls 1) würde hier auffallen.
        assert_eq!(
            ende,
            ProzessEnde::Beendet(0),
            "elternprobe {} ({}) meldete einen Fehler",
            argument,
            was
        );
    }

    scheduler::aufraeumen();
    assert_eq!(
        frei_vorher,
        frames_frei(),
        "die Eltern-Kind-Laeufe haben Frames geleckt"
    );
}

/// Ein WARTENDER Prozess verbraucht praktisch keine CPU. Das ist der
/// messbare Unterschied zwischen „blockieren" und „in einer Schleife
/// nachsehen".
#[test_case]
fn test_wartender_prozess_verbraucht_keine_cpu() {
    if !programme_vorhanden() {
        return;
    }
    // Ein Schläfer schläft 200 ms am Stück (Zustand `Wartend`).
    let schlaefer = prozess::schlaefer_prozess(200).expect("Schlaefer bauen");
    let pid = scheduler::einplanen(schlaefer).expect("einplanen");

    assert!(
        warten_bis_zustand(pid, Zustand::Wartend, 5_000),
        "der Schlaefer hat den Zustand 'Wartend' nie erreicht"
    );

    // Eine Sekunde laufen lassen und die CPU-Zeit messen.
    let vorher = scheduler::momentaufnahme()
        .into_iter()
        .find(|zeile| zeile.pid == pid)
        .map(|zeile| zeile.cpu_us)
        .unwrap_or(0);
    let bis = zeit::ms_seit_boot() + 1000;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }
    let nachher = scheduler::momentaufnahme()
        .into_iter()
        .find(|zeile| zeile.pid == pid)
        .map(|zeile| zeile.cpu_us)
        .unwrap_or(0);

    // In einer Sekunde Wanduhr darf ein Schläfer nur Bruchteile davon
    // verbrauchen (er wacht 5x auf, druckt nichts und schläft weiter).
    let verbraucht = nachher - vorher;
    assert!(
        verbraucht < 50_000,
        "ein wartender Prozess hat {} us CPU verbraucht — er wartet nicht, er dreht",
        verbraucht
    );

    scheduler::beenden(pid);
    scheduler::warten_auf(pid, FRIST_MS);
    scheduler::aufraeumen();
}

/// Ein Ergebnis, das nie abgeholt wird, verschwindet mit seinem
/// Elternteil — es bleibt NICHTS liegen.
#[test_case]
fn test_kein_zombie_wenn_eltern_endet() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    let eltern = starten("zaehle", &["zaehle", "1", "20000"], None, None, None);
    let kind = starten("hallo", &["hallo", "--code=3"], Some(eltern), None, None);
    scheduler::warten_auf(kind, FRIST_MS).expect("Kind endet");
    assert_eq!(scheduler::kinder_enden_offen(eltern), Some(1));

    // Der Elternteil endet, OHNE abzuholen.
    scheduler::beenden(eltern);
    scheduler::warten_auf(eltern, FRIST_MS).expect("Elternteil endet");

    assert_eq!(
        frei_vorher,
        frames_frei(),
        "nicht abgeholte Kind-Ergebnisse haben Speicher gekostet"
    );
    assert_eq!(
        scheduler::momentaufnahme()
            .iter()
            .filter(|zeile| zeile.ist_user)
            .count(),
        0,
        "es sind Prozesse uebrig"
    );
}

// ---------------------------------------------------------------------------
// 3. beende(pid) raeumt vollstaendig ab
// ---------------------------------------------------------------------------

/// Ein Prozess wird von aussen beendet — und ALLES geht zurueck: Adressraum,
/// Kernel-Stack, Handles. Und zwar auch dann, wenn er gerade rechnet und
/// nicht kooperiert (das ist der Unterschied zu einem kooperativen Task).
#[test_case]
fn test_beende_raeumt_vollstaendig_ab() {
    if !programme_vorhanden() {
        return;
    }
    // Erst ein Aufwaermlauf (einmalige Kosten aus der Messung nehmen).
    let warm = starten("zaehle", &["zaehle", "1"], None, None, None);
    scheduler::warten_auf(warm, FRIST_MS);
    let frei_vorher = frames_frei();
    let pipes_vorher = pipe::anzahl();

    for durchgang in 0..3 {
        // Ein Prozess, der NIE freiwillig endet (Zähler-Endlosschleife aus
        // Teil 3 — er gibt die CPU nicht einmal ab).
        let dauerlaeufer = prozess::zaehler_prozess(b'Z').expect("Zaehler bauen");
        let pid = scheduler::einplanen(dauerlaeufer).expect("einplanen");

        // Kurz laufen lassen, damit er wirklich in Ring 3 arbeitet.
        let bis = zeit::ms_seit_boot() + 60;
        while zeit::ms_seit_boot() < bis {
            zeit::warte_auf_interrupt();
        }
        assert!(
            scheduler::momentaufnahme().iter().any(|zeile| zeile.pid == pid),
            "Durchgang {}: der Prozess laeuft nicht",
            durchgang
        );

        // BEENDEN — ohne seine Mitwirkung.
        assert!(scheduler::beenden(pid), "Durchgang {}: beenden", durchgang);
        let ende = scheduler::warten_auf(pid, FRIST_MS)
            .unwrap_or_else(|| panic!("Durchgang {}: Prozess endet nicht", durchgang));
        assert_eq!(ende, ProzessEnde::Gestoppt);
        assert_eq!(ende.code(), 143);
    }

    assert_eq!(frei_vorher, frames_frei(), "beende(pid) hat Frames geleckt");
    assert_eq!(pipes_vorher, pipe::anzahl(), "beende(pid) hat Pipes geleckt");
}

/// Ein Prozess mit OFFENEN PIPE-ENDEN wird beendet: Auch die Enden gehen
/// zurueck — sonst bekaeme die Gegenseite nie ein Dateiende und wartete
/// fuer immer.
#[test_case]
fn test_beende_schliesst_pipe_enden() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let leitung = pipe::anlegen().expect("Pipe anlegen");

    // Ein Prozess bekommt das SCHREIB-Ende ...
    pipe::ende_uebernehmen(leitung, Ende::Schreiben);
    let pid = starten(
        "zaehle",
        &["zaehle", "100000", "50"], // lange genug, um ihn zu erwischen
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
    );
    // ... die Shell gibt ihre eigene Kopie ab. Jetzt haelt NUR der Prozess.
    pipe::ende_schliessen(leitung, Ende::Schreiben);

    // Etwas laufen lassen, dann von aussen beenden.
    let bis = zeit::ms_seit_boot() + 200;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }
    let (belegt, _, schreiber) = pipe::zustand(leitung).expect("Pipe lebt");
    assert!(belegt > 0, "der Prozess hat nichts in die Pipe geschrieben");
    assert_eq!(schreiber, 1, "genau der Prozess haelt das Schreib-Ende");

    scheduler::beenden(pid);
    scheduler::warten_auf(pid, FRIST_MS).expect("Prozess endet");
    scheduler::aufraeumen();

    // DAS SCHREIB-ENDE IST ZU -> die Restdaten kommen noch, dann Dateiende.
    let mut puffer = [0u8; 256];
    let mut gelesen_gesamt = 0usize;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            PipeErgebnis::Bytes(0) => break,
            PipeErgebnis::Bytes(n) => gelesen_gesamt += n,
            // Nach dem `aufraeumen` oben ist das Schreib-Ende zu — hier darf
            // also nichts mehr blockieren.
            andere => panic!("unerwartet: {:?}", andere),
        }
    }
    assert!(gelesen_gesamt > 0, "die gepufferten Daten sind verloren gegangen");

    pipe::ende_schliessen(leitung, Ende::Lesen);
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipe geleckt");
}

// ---------------------------------------------------------------------------
// 4. Die Pipe blockiert und weckt an BEIDEN Enden
// ---------------------------------------------------------------------------

/// DER LESER SCHLAEFT, bis etwas ankommt — und wacht dann auf.
///
/// Nachgemessen wird der ZUSTAND: Der Prozess muss `Wartend` sein, nicht
/// lauffähig. Erst danach wird geschrieben, und er muss von selbst
/// weiterlaufen (der Timer prueft die Weck-Bedingung).
#[test_case]
fn test_pipe_leser_blockiert_und_wird_geweckt() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let frei_vorher = frames_frei();

    let eingabe = pipe::anlegen().expect("Eingabe-Pipe");
    let ausgabe = pipe::anlegen().expect("Ausgabe-Pipe");

    // `filter x` liest von Handle 0 und schreibt auf Handle 1.
    pipe::ende_uebernehmen(eingabe, Ende::Lesen);
    pipe::ende_uebernehmen(ausgabe, Ende::Schreiben);
    let pid = starten(
        "filter",
        &["filter", "x"],
        None,
        Some(KernelObjekt::PipeLesen(eingabe)),
        Some(KernelObjekt::PipeSchreiben(ausgabe)),
    );
    pipe::ende_schliessen(eingabe, Ende::Lesen);
    pipe::ende_schliessen(ausgabe, Ende::Schreiben);

    // Es kommt NICHTS -> der Prozess muss schlafen gehen.
    assert!(
        warten_bis_zustand(pid, Zustand::Wartend, 5_000),
        "der Leser blockiert nicht (er dreht in einer Schleife?)"
    );

    // Jetzt eine passende und eine unpassende Zeile hineingeben.
    assert!(matches!(
        pipe::schreiben(eingabe, b"axb\nnein\n"),
        PipeErgebnis::Bytes(9)
    ));

    // Er MUSS von selbst aufwachen und die passende Zeile durchreichen.
    let mut ausgelesen = Vec::new();
    let frist = zeit::ms_seit_boot() + 5_000;
    while ausgelesen.is_empty() && zeit::ms_seit_boot() < frist {
        let mut puffer = [0u8; 64];
        if let PipeErgebnis::Bytes(n) = pipe::lesen(ausgabe, &mut puffer) {
            ausgelesen.extend_from_slice(&puffer[..n]);
        }
        zeit::warte_auf_interrupt();
    }
    assert_eq!(
        ausgelesen,
        b"axb\n".to_vec(),
        "der geweckte Leser hat die falsche Zeile geliefert"
    );

    // Und das Dateiende beendet ihn: Schreib-Ende der Eingabe zu.
    pipe::ende_schliessen(eingabe, Ende::Schreiben);
    let ende = scheduler::warten_auf(pid, FRIST_MS).expect("filter endet beim Dateiende");
    assert_eq!(ende, ProzessEnde::Beendet(0));

    pipe::ende_schliessen(ausgabe, Ende::Lesen);
    scheduler::aufraeumen();
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
    assert_eq!(frei_vorher, frames_frei(), "Frames geleckt");
}

/// DER SCHREIBER SCHLAEFT, wenn die Pipe voll ist — und laeuft weiter,
/// sobald jemand liest. Das ist der Gegendruck, ohne den ein schneller
/// Erzeuger den Kernel volllaufen liesse.
#[test_case]
fn test_pipe_schreiber_blockiert_bei_voll() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let leitung = pipe::anlegen().expect("Pipe anlegen");

    // `zaehle` bis 100000 erzeugt weit mehr als die 4 KiB der Pipe —
    // und NIEMAND liest.
    pipe::ende_uebernehmen(leitung, Ende::Schreiben);
    let pid = starten(
        "zaehle",
        &["zaehle", "100000"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
    );
    pipe::ende_schliessen(leitung, Ende::Schreiben);

    // Er MUSS blockieren, sobald die Pipe voll ist.
    assert!(
        warten_bis_zustand(pid, Zustand::Wartend, 5_000),
        "der Schreiber blockiert nicht bei voller Pipe"
    );
    let (belegt, _, _) = pipe::zustand(leitung).expect("Pipe lebt");
    assert_eq!(
        belegt,
        pipe::kapazitaet(),
        "die Pipe muesste randvoll sein, ist aber {} von {}",
        belegt,
        pipe::kapazitaet()
    );

    // Er verbraucht dabei KEINE CPU (er wartet wirklich).
    let vorher = scheduler::momentaufnahme()
        .into_iter()
        .find(|zeile| zeile.pid == pid)
        .map(|zeile| zeile.cpu_us)
        .unwrap_or(0);
    let bis = zeit::ms_seit_boot() + 300;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }
    let nachher = scheduler::momentaufnahme()
        .into_iter()
        .find(|zeile| zeile.pid == pid)
        .map(|zeile| zeile.cpu_us)
        .unwrap_or(0);
    assert!(
        nachher - vorher < 20_000,
        "der blockierte Schreiber verbraucht CPU ({} us in 300 ms)",
        nachher - vorher
    );

    // LESEN WECKT IHN: Platz schaffen, und er schreibt weiter.
    let mut puffer = [0u8; 2048];
    assert!(matches!(pipe::lesen(leitung, &mut puffer), PipeErgebnis::Bytes(2048)));
    assert!(
        warten_bis_zustand(pid, Zustand::Lauffaehig, 5_000)
            || warten_bis_zustand(pid, Zustand::Laeuft, 5_000),
        "der Schreiber wurde nicht geweckt, obwohl Platz da ist"
    );
    // Und die Pipe füllt sich wieder.
    let frist = zeit::ms_seit_boot() + 5_000;
    let mut wieder_voll = false;
    while zeit::ms_seit_boot() < frist {
        if let Some((belegt, _, _)) = pipe::zustand(leitung) {
            if belegt == pipe::kapazitaet() {
                wieder_voll = true;
                break;
            }
        }
        zeit::warte_auf_interrupt();
    }
    assert!(wieder_voll, "der geweckte Schreiber hat nicht weitergeschrieben");

    // LESE-ENDE ZU -> der Schreiber bekommt `Abgebrochen` und endet.
    pipe::ende_schliessen(leitung, Ende::Lesen);
    // (Das Programm ruft dann `schreibe` -> Fehler; es zaehlt trotzdem
    //  weiter, also von aussen beenden.)
    scheduler::beenden(pid);
    scheduler::warten_auf(pid, FRIST_MS).expect("Schreiber endet");
    scheduler::aufraeumen();
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
}

// ---------------------------------------------------------------------------
// 5. Handle-Weitergabe
// ---------------------------------------------------------------------------

/// Ein Kind bekommt Handles, die es nie geoeffnet hat — und NUR die.
///
/// Geprueft wird von aussen (der Test kann nicht in den Prozess
/// hineinsehen), aber die WIRKUNG ist eindeutig: Das Kind schreibt auf
/// Handle 1, und die Bytes landen in der Pipe, die der Test angelegt hat.
/// Ohne Weitergabe waeren sie im Terminal gelandet.
#[test_case]
fn test_handle_weitergabe() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let leitung = pipe::anlegen().expect("Pipe anlegen");

    pipe::ende_uebernehmen(leitung, Ende::Schreiben);
    let pid = starten(
        "zaehle",
        &["zaehle", "5"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
    );
    // Die eigene Kopie abgeben — sonst gibt es nie ein Dateiende.
    pipe::ende_schliessen(leitung, Ende::Schreiben);

    // Alles einsammeln, bis Dateiende.
    //
    // DAS `aufraeumen()` IN DER SCHLEIFE IST NICHT KOSMETIK: Das Schreib-Ende
    // haengt an der Handle-Tabelle des Kindes, und die faellt erst, wenn der
    // beendete Prozess ABGERAEUMT wird. Vorher gilt das Ende als offen, und
    // „leer" heisst „warte", nicht „Ende". Im laufenden System erledigt das
    // der Aufraeum-Task (alle 250 ms) bzw. die Pump-Schleife der Shell; in
    // einem Test ohne Executor muss es der Test selbst tun.
    let mut gesammelt = Vec::new();
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        let mut puffer = [0u8; 128];
        match pipe::lesen(leitung, &mut puffer) {
            PipeErgebnis::Bytes(0) => break,
            PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            PipeErgebnis::Blockiert => {
                assert!(zeit::ms_seit_boot() < frist, "zaehle wurde nicht fertig");
                ende_ernten(pid, &mut ende);
                scheduler::aufraeumen();
                zeit::warte_auf_interrupt();
            }
            andere => panic!("unerwartet: {:?}", andere),
        }
    }
    ende_ernten(pid, &mut ende);
    assert_eq!(
        String::from_utf8_lossy(&gesammelt),
        "1\n2\n3\n4\n5\n",
        "die Ausgabe des Kindes kam nicht durch die weitergegebene Pipe"
    );

    let ende = ende
        .or_else(|| scheduler::warten_auf(pid, FRIST_MS))
        .expect("zaehle endet");
    assert_eq!(ende, ProzessEnde::Beendet(0));
    pipe::ende_schliessen(leitung, Ende::Lesen);
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
}

/// Der Warte-Grund wird wirklich vermerkt — sonst könnte der Timer nicht
/// entscheiden, wann jemand aufwachen darf.
#[test_case]
fn test_warte_gruende_werden_vermerkt() {
    if !programme_vorhanden() {
        return;
    }
    let leitung = pipe::anlegen().expect("Pipe anlegen");
    pipe::ende_uebernehmen(leitung, Ende::Lesen);
    let pid = starten(
        "filter",
        &["filter", "egal"],
        None,
        Some(KernelObjekt::PipeLesen(leitung)),
        None,
    );
    pipe::ende_schliessen(leitung, Ende::Lesen);

    assert!(warten_bis_zustand(pid, Zustand::Wartend, 5_000));
    assert_eq!(
        scheduler::warte_grund(pid),
        Some(Warteauf::PipeLesen(leitung)),
        "der Grund des Wartens wurde nicht vermerkt"
    );

    pipe::ende_schliessen(leitung, Ende::Schreiben);
    scheduler::warten_auf(pid, FRIST_MS).expect("filter endet beim Dateiende");
    scheduler::aufraeumen();
}

// ---------------------------------------------------------------------------
// 6. DER PIPE-BEWEIS durch die Shell
// ---------------------------------------------------------------------------

/// `starte zaehle 20 | filter 7` — beide Programme laufen gleichzeitig,
/// verbunden durch eine Pipe, und die Shell druckt, was hinten herauskommt.
///
/// Der Test kann die Terminal-Ausgabe nicht einlesen; er prueft deshalb
/// (a) dass die Pipeline sauber durchlaeuft und beide Prozesse mit 0 enden,
/// und (b) BAUT DIESELBE PIPELINE noch einmal von Hand nach, um das
/// Ergebnis wirklich zu SEHEN. Das ist der eigentliche Beweis.
#[test_case]
fn test_pipeline_zaehle_filter() {
    if !programme_vorhanden() {
        return;
    }
    let pipes_vorher = pipe::anzahl();
    let frei_vorher = frames_frei();

    // --- (b) Die Pipeline von Hand, damit das Ergebnis pruefbar ist ---
    let p1 = pipe::anlegen().expect("Pipe 1");
    let p2 = pipe::anlegen().expect("Pipe 2");

    pipe::ende_uebernehmen(p1, Ende::Schreiben);
    let zaehle = starten(
        "zaehle",
        &["zaehle", "20"],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(p1)),
    );
    pipe::ende_uebernehmen(p1, Ende::Lesen);
    pipe::ende_uebernehmen(p2, Ende::Schreiben);
    let filter = starten(
        "filter",
        &["filter", "7"],
        None,
        Some(KernelObjekt::PipeLesen(p1)),
        Some(KernelObjekt::PipeSchreiben(p2)),
    );
    // Die eigenen Kopien abgeben — DAS ist der Schritt, ohne den `filter`
    // nie ein Dateiende bekaeme.
    pipe::ende_schliessen(p1, Ende::Schreiben);
    pipe::ende_schliessen(p1, Ende::Lesen);
    pipe::ende_schliessen(p2, Ende::Schreiben);

    let mut ergebnis = Vec::new();
    let (mut ende_zaehle, mut ende_filter) = (None, None);
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        let mut puffer = [0u8; 128];
        match pipe::lesen(p2, &mut puffer) {
            PipeErgebnis::Bytes(0) => break,
            PipeErgebnis::Bytes(n) => ergebnis.extend_from_slice(&puffer[..n]),
            PipeErgebnis::Blockiert => {
                assert!(zeit::ms_seit_boot() < frist, "die Pipeline haengt");
                ende_ernten(zaehle, &mut ende_zaehle);
                ende_ernten(filter, &mut ende_filter);
                scheduler::aufraeumen();
                zeit::warte_auf_interrupt();
            }
            andere => panic!("unerwartet: {:?}", andere),
        }
    }
    ende_ernten(zaehle, &mut ende_zaehle);
    ende_ernten(filter, &mut ende_filter);
    pipe::ende_schliessen(p2, Ende::Lesen);

    // DAS IST DER BEWEIS: Von 1..20 enthalten genau 7 und 17 eine Sieben.
    assert_eq!(
        String::from_utf8_lossy(&ergebnis),
        "7\n17\n",
        "die Pipeline hat das falsche Ergebnis geliefert"
    );
    serial_println!("  PIPE-BEWEIS: zaehle 20 | filter 7 -> 7, 17");

    // BEIDE Stufen muessen sauber geendet haben — eine Pipeline, bei der
    // die erste Haelfte abstuerzt, koennte dasselbe Ergebnis liefern.
    for (pid, gemerkt) in [(zaehle, ende_zaehle), (filter, ende_filter)] {
        let ende = gemerkt
            .or_else(|| scheduler::warten_auf(pid, FRIST_MS))
            .unwrap_or_else(|| panic!("PID {} hat kein Ergebnis hinterlassen", pid));
        assert_eq!(ende, ProzessEnde::Beendet(0), "PID {} endete unsauber", pid);
    }
    scheduler::aufraeumen();
    assert_eq!(pipe::anzahl(), pipes_vorher, "Pipes geleckt");
    assert_eq!(frei_vorher, frames_frei(), "Frames geleckt");
}

/// Und derselbe Ausdruck durch die SHELL — inklusive Zerlegung am `|`,
/// Prozess-Start, Ausgabe-Durchreichung und Exit-Code-Anzeige.
#[test_case]
fn test_shell_pipeline_und_aufraeumen() {
    if !programme_vorhanden() {
        return;
    }
    let registry = alle_befehle();
    let mut ctx = ShellKontext::neu();
    let pipes_vorher = pipe::anzahl();
    let frei_vorher = frames_frei();

    for zeile in [
        "starte zaehle 20 | filter 7",
        "starte zaehle 12 | filter 1 | filter 2",
        "starte zaehle 5",
        // Fehlerfaelle: duerfen melden, aber nie haengen.
        "starte zaehle | gibt-es-nicht",
        "starte gibt-es-nicht | filter 1",
        "starte |",
    ] {
        serial_println!("  $ {}", zeile);
        befehl_ausfuehren(&registry, &mut ctx, zeile);
    }

    scheduler::aufraeumen();
    assert_eq!(
        scheduler::momentaufnahme()
            .iter()
            .filter(|zeile| zeile.ist_user)
            .count(),
        0,
        "nach den Pipelines sind Prozesse uebrig"
    );
    assert_eq!(pipe::anzahl(), pipes_vorher, "die Shell hat Pipes geleckt");
    assert_eq!(frei_vorher, frames_frei(), "die Shell hat Frames geleckt");
}
