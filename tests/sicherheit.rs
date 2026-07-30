// tests/sicherheit.rs — DER KERNEL UNTER ANGRIFF (Serie-6-Abschluss)
//
// ==========================================================================
// DER WERTVOLLSTE TEST DES PROJEKTS
//
// Sechs Serien lang wurden Sicherheitszusagen gemacht: „Der Kernel folgt
// niemals blind einem User-Zeiger." „Ein Fehler im User-Mode reisst den
// Kernel nie mit." „Ein Prozess kann die Handles eines anderen nicht
// erraten." „Ein endlos rechnender Prozess kann die Maschine nicht anhalten."
//
// Jede davon war bis hierhin ein SATZ. Dieser Test macht daraus eine
// gepruefte Aussage — mit einem echten Gegner: `userland/angreifer` ist ein
// unprivilegiertes Programm, das systematisch versucht, aus seinem Gefaengnis
// auszubrechen.
//
// DIE ERWARTUNG IST IMMER DIESELBE:
//   * ENTWEDER der Syscall lehnt sauber mit einem Fehlercode ab,
//   * ODER der Angreifer wird vom Kernel beendet.
//   * NIEMALS: der Kernel stirbt, haengt oder gibt etwas preis.
// Und in beiden Faellen laufen die ANDEREN Prozesse unbeirrt weiter.
//
// ==========================================================================
// WAS DIESER TEST GEFUNDEN HAT
//
// Er ist nicht nur Bestaetigung — er hat eine echte Luecke aufgedeckt: Bis
// zum Serie-6-Abschluss hatten nur #PF und #GP einen IDT-Handler. Ein
// Ring-3-Programm mit `ud2` (#UD) oder einer Division durch Null (#DE) traf
// auf einen Vektor OHNE Eintrag — und das eskaliert zum Double Fault, der
// SpeedOS anhaelt. Ein einziges `div rax, 0` in einem unprivilegierten
// Programm haette also den ganzen Kernel gestoppt.
//
// Behoben in interrupts.rs, indem nicht das einzelne Loch gestopft, sondern
// die KLASSE geschlossen wurde: Jede aus Ring 3 erreichbare CPU-Exception hat
// jetzt einen Handler, und alle laufen durch dieselbe `user_recovery`.

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
use speed_os::prozess::{self, ProzessEnde};
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

fn frames_frei() -> usize {
    scheduler::aufraeumen();
    memory::frame_statistik().0
}

/// Belegter Heap OHNE den Kernel-Log-Puffer.
///
/// Der Log-Puffer waechst mit jeder Ausgabe bis zu seiner Obergrenze
/// (64 KiB) und schrumpft nie — das ist Absicht und kein Leck. Fuer eine
/// Speicher-Bilanz muss er heraus, sonst misst man das Protokoll statt der
/// Prozess-Schicht.
fn heap_ohne_log() -> usize {
    let belegt = allocator::heap_statistik().map(|(belegt, _)| belegt).unwrap_or(0);
    belegt.saturating_sub(speed_os::protokoll::puffer_bytes())
}

/// Startet den Angreifer mit einer Angriffsnummer und wartet auf sein Ende.
fn angreifen(nummer: u32) -> Option<ProzessEnde> {
    let pfad = programme::pfad("angreifer");
    let argument = alloc::format!("{}", nummer);
    let pid = prozess::prozess_starten(&pfad, &["angreifer", &argument])
        .unwrap_or_else(|fehler| panic!("angreifer {} starten: {}", nummer, fehler.meldung()));
    scheduler::warten_auf(pid, FRIST_MS)
}

/// DER LEBENSBEWEIS: Läuft der Kernel nach dem Angriff noch normal?
///
/// Nicht „ist er nicht abgestürzt" (das sähe man am Test-Abbruch), sondern
/// „funktioniert er noch": Ein echtes Programm von der Platte laden, in
/// Ring 3 ausführen, sauber beenden. Wenn DAS geht, ist der Kernel gesund.
fn kernel_lebt_noch() {
    let pfad = programme::pfad("hallo");
    let pid = prozess::prozess_starten(&pfad, &["hallo"]).expect("hallo starten");
    let ende = scheduler::warten_auf(pid, FRIST_MS).expect("hallo wurde nicht fertig");
    assert_eq!(
        ende,
        ProzessEnde::Beendet(0),
        "der Kernel arbeitet nach dem Angriff nicht mehr korrekt"
    );
}

// ===========================================================================
// TEIL A: Angriffe, die der Angreifer ÜBERLEBEN muss (abgelehnte Syscalls)
// ===========================================================================

/// Die sechs Syscall-Angriffe. Der Angreifer prüft jeden selbst und meldet
/// über seinen Exit-Code:
///   0 = alles korrekt abgelehnt,
///   1 = **ein Angriff ist durchgekommen** (echte Lücke).
///
/// Dass er dabei am Leben bleibt, ist selbst Teil der Zusage: Ein
/// abgelehnter Syscall ist ein FEHLERCODE, kein Absturz. Ein Programm darf
/// beliebig viel Unsinn übergeben.
#[test_case]
fn test_syscall_angriffe_werden_abgelehnt() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    for (nummer, was) in [
        (1u32, "Syscalls mit Kernel-Zeigern"),
        (2, "fremde und erfundene Handles"),
        (3, "ungueltige Syscall-Nummern"),
        (4, "Zeiger mit Integer-Ueberlauf"),
        (5, "riesige Laengen"),
        (6, "Pfad-Angriffe"),
    ] {
        serial_println!("  ANGRIFF {}: {}", nummer, was);
        let ende = angreifen(nummer)
            .unwrap_or_else(|| panic!("angreifer {} ({}) wurde nicht fertig", nummer, was));
        assert_eq!(
            ende,
            ProzessEnde::Beendet(0),
            "ANGRIFF {} ({}) ist DURCHGEKOMMEN oder hat den Angreifer getoetet — \
             ein abgelehnter Syscall muss ein Fehlercode sein, kein Absturz",
            nummer,
            was
        );
    }

    kernel_lebt_noch();
    assert_eq!(
        frei_vorher,
        frames_frei(),
        "die Syscall-Angriffe haben Frames geleckt"
    );
}

// ===========================================================================
// TEIL B: Angriffe, die den Angreifer das Leben kosten MÜSSEN
// ===========================================================================

/// Sieben Wege, sich umzubringen — und jeder davon darf **nur** den
/// Angreifer treffen.
///
/// Zwei davon (`ud2` und Division durch Null) haben beim Schreiben dieses
/// Tests eine echte Lücke aufgedeckt: Sie hätten den Kernel angehalten, weil
/// #UD und #DE keinen IDT-Handler hatten (siehe Kopfkommentar).
#[test_case]
fn test_toedliche_angriffe_toeten_nur_den_angreifer() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    for (nummer, was) in [
        (20u32, "Kernel-Speicher LESEN"),
        (21, "Kernel-Speicher SCHREIBEN"),
        (22, "Stack-Ueberlauf (Guard-Page)"),
        (23, "privilegierte Instruktion (cli)"),
        (24, "ungueltiger Opcode (ud2 -> #UD)"),
        (25, "Division durch Null (-> #DE)"),
        (26, "Sprung ins Nichts"),
    ] {
        serial_println!("  ANGRIFF {}: {}", nummer, was);
        let ende = angreifen(nummer)
            .unwrap_or_else(|| panic!("angreifer {} ({}) wurde nicht fertig", nummer, was));
        assert_eq!(
            ende,
            ProzessEnde::Abgestuerzt,
            "ANGRIFF {} ({}) haette den Angreifer toeten muessen, endete aber mit {:?}",
            nummer,
            was,
            ende
        );
        // Nach JEDEM einzelnen: Läuft der Kernel noch? (Nicht erst am Ende —
        // sonst wüsste man nicht, welcher Angriff ihn beschädigt hat.)
        kernel_lebt_noch();
    }

    assert_eq!(
        frei_vorher,
        frames_frei(),
        "die toedlichen Angriffe haben Frames geleckt"
    );
}

// ===========================================================================
// TEIL C: Der Angriff auf die VERFÜGBARKEIT
// ===========================================================================

/// Ein Prozess, der endlos rechnet und NIE abgibt, darf die Maschine nicht
/// anhalten.
///
/// In einem kooperativen System wäre das das Ende — niemand könnte ihm die
/// CPU abnehmen. Hier nimmt der PIT sie ihm alle 20 ms weg. Geprüft wird
/// nicht nur „der Kernel lebt", sondern dass ein ANDERER Prozess in dieser
/// Zeit **vorangekommen** ist: Verfügbarkeit heisst, dass die anderen
/// weiterarbeiten, nicht nur dass niemand abstürzt.
#[test_case]
fn test_endlosschleife_wird_praemptiert() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    // Ein friedlicher Prozess, der sichtbar Fortschritt macht: Er schreibt
    // Zahlen in eine Pipe, die wir mitlesen können.
    let leitung = pipe::anlegen().expect("Pipe anlegen");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let friedlich_pfad = programme::pfad("zaehle");
    let friedlich = prozess::prozess_starten_mit(
        &friedlich_pfad,
        &["zaehle", "100000", "5"],
        None,
        None,
        Some(speed_os::syscall::handle::KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("zaehle starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    // Und jetzt der Angreifer: rechnet endlos, gibt nie ab.
    let angreifer_pfad = programme::pfad("angreifer");
    let angreifer = prozess::prozess_starten(&angreifer_pfad, &["angreifer", "30"])
        .expect("angreifer starten");

    // Wie weit war der friedliche Prozess, bevor der Angreifer loslegte?
    let mut puffer = [0u8; 512];
    let mut vorher = 0usize;
    let bis = zeit::ms_seit_boot() + 300;
    while zeit::ms_seit_boot() < bis {
        if let pipe::PipeErgebnis::Bytes(n) = pipe::lesen(leitung, &mut puffer) {
            vorher += n;
        }
        zeit::warte_auf_interrupt();
    }

    // Zwei Sekunden unter Dauerlast.
    let mut nachher = 0usize;
    let bis = zeit::ms_seit_boot() + 2_000;
    while zeit::ms_seit_boot() < bis {
        if let pipe::PipeErgebnis::Bytes(n) = pipe::lesen(leitung, &mut puffer) {
            nachher += n;
        }
        zeit::warte_auf_interrupt();
    }

    // (1) DER ANGREIFER WURDE VERDRÄNGT — aus Ring 3, nachweislich.
    let moment = scheduler::momentaufnahme();
    let angreifer_zeile = moment
        .iter()
        .find(|zeile| zeile.pid == angreifer)
        .expect("der Angreifer muss noch laufen");
    assert!(
        angreifer_zeile.praemptionen > 0,
        "dem Angreifer wurde die CPU nie weggenommen ({} Praemptionen)",
        angreifer_zeile.praemptionen
    );
    assert_eq!(
        angreifer_zeile.abgaben, 0,
        "der Angreifer hat freiwillig abgegeben — dann beweist der Test nichts"
    );

    // (2) DER FRIEDLICHE PROZESS KAM VORAN, obwohl der Angreifer rechnet.
    assert!(
        nachher > 0,
        "der friedliche Prozess hat unter Last NICHTS geschafft ({} Byte)",
        nachher
    );
    serial_println!(
        "  Unter Dauerlast: {} Byte in 2 s (davor {} Byte in 0,3 s), \
         Angreifer {}x verdraengt.",
        nachher,
        vorher,
        angreifer_zeile.praemptionen
    );

    // (3) Und der Kernel bedient weiterhin normale Anfragen.
    kernel_lebt_noch();

    // Aufräumen: beide beenden.
    scheduler::beenden(angreifer);
    scheduler::beenden(friedlich);
    scheduler::warten_auf(angreifer, FRIST_MS);
    scheduler::warten_auf(friedlich, FRIST_MS);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();

    assert_eq!(frei_vorher, frames_frei(), "der Dauerlast-Test hat Frames geleckt");
    assert_eq!(pipe::anzahl(), 0, "der Dauerlast-Test hat Pipes geleckt");
}

// ===========================================================================
// TEIL D: Isolation zwischen Prozessen
// ===========================================================================

/// Zwei Angreifer gleichzeitig — der eine stirbt, der andere darf davon
/// nichts merken.
///
/// Das ist die Zusage, die über „der Kernel lebt" hinausgeht: Ein
/// abstürzender Prozess reisst auch seine NACHBARN nicht mit.
#[test_case]
fn test_absturz_verschont_die_nachbarn() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    // Ein Nachbar, der geduldig zählt.
    let nachbar_pfad = programme::pfad("zaehle");
    let nachbar = prozess::prozess_starten(&nachbar_pfad, &["zaehle", "100000", "20"])
        .expect("Nachbar starten");

    // Fünf Angreifer nacheinander, jeder stirbt anders.
    for nummer in [20u32, 21, 23, 24, 25] {
        let ende = angreifen(nummer).expect("Angreifer endet");
        assert_eq!(ende, ProzessEnde::Abgestuerzt, "Angriff {}", nummer);
        // Der Nachbar lebt — und zwar in jedem einzelnen Durchgang.
        let lebt = scheduler::momentaufnahme()
            .iter()
            .any(|zeile| zeile.pid == nachbar && zeile.zustand != prozess::Zustand::Beendet);
        assert!(
            lebt,
            "Angriff {} hat den unbeteiligten Nachbarn mitgerissen",
            nummer
        );
    }

    // Und er hat in der Zeit auch wirklich gearbeitet.
    let cpu = scheduler::momentaufnahme()
        .iter()
        .find(|zeile| zeile.pid == nachbar)
        .map(|zeile| zeile.cpu_us)
        .unwrap_or(0);
    assert!(cpu > 0, "der Nachbar hat keine CPU-Zeit bekommen");

    scheduler::beenden(nachbar);
    scheduler::warten_auf(nachbar, FRIST_MS);
    scheduler::aufraeumen();
    assert_eq!(frei_vorher, frames_frei(), "Frames geleckt");
}

/// Ein Prozess kann den Speicher eines anderen nicht sehen — auch nicht an
/// DERSELBEN virtuellen Adresse.
///
/// Direkt nachgemessen: Zwei Prozesse bekommen an derselben Adresse
/// verschiedene Geheimnisse, und keiner der beiden Adressräume enthält das
/// des anderen.
#[test_case]
fn test_prozesse_sehen_fremden_speicher_nicht() {
    if !programme_vorhanden() {
        return;
    }
    let bytes = {
        let pfad = programme::pfad("hallo");
        fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad)).expect("hallo lesen")
    };

    let mut a = prozess::prozess_aus_elf("angreifer-A", &bytes, &["hallo"]).expect("A");
    let mut b = prozess::prozess_aus_elf("angreifer-B", &bytes, &["hallo"]).expect("B");

    // Beide bekommen an DERSELBEN Adresse ein anderes Geheimnis.
    let stelle = VirtAddr::new(prozess::ELF_STACK_OBEN - 4096);
    a.raum
        .as_mut()
        .unwrap()
        .schreiben(stelle, b"GEHEIMNIS-VON-A")
        .expect("A beschreiben");
    b.raum
        .as_mut()
        .unwrap()
        .schreiben(stelle, b"GEHEIMNIS-VON-B")
        .expect("B beschreiben");

    let mut aus_a = [0u8; 15];
    let mut aus_b = [0u8; 15];
    a.raum.as_ref().unwrap().lesen(stelle, &mut aus_a).expect("A lesen");
    b.raum.as_ref().unwrap().lesen(stelle, &mut aus_b).expect("B lesen");

    assert_eq!(&aus_a, b"GEHEIMNIS-VON-A");
    assert_eq!(&aus_b, b"GEHEIMNIS-VON-B");
    assert_ne!(
        aus_a, aus_b,
        "beide Prozesse sehen an derselben Adresse dasselbe — es gibt keine Isolation"
    );

    // Und im KERNEL-Adressraum ist dort NICHTS — der Kernel kann User-Daten
    // gar nicht versehentlich anfassen.
    speed_os::adressraum::kernel_aktivieren();
    assert!(memory::seiten_flags(stelle).is_none());

    drop(a);
    drop(b);
}

// ===========================================================================
// TEIL E: Die Handle-Isolation unter Feuer
// ===========================================================================

/// Prozess A öffnet Handles; Prozess B probiert JEDE mögliche Zahl durch.
///
/// Handles sind bei uns INDIZES in die eigene Tabelle — B kann also gar keine
/// Zahl bilden, die auf A's Objekte zeigt. Der Angreifer probiert es
/// trotzdem systematisch (Angriff 2 durchläuft 3..64 plus die u64-Extreme),
/// hier wird zusätzlich geprüft, dass A dabei nichts verliert.
#[test_case]
fn test_handle_isolation_unter_angriff() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei();

    // A hält eine Pipe offen und zählt hinein.
    let leitung = pipe::anlegen().expect("Pipe");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let a_pfad = programme::pfad("zaehle");
    let a = prozess::prozess_starten_mit(
        &a_pfad,
        &["zaehle", "100000", "10"],
        None,
        None,
        Some(speed_os::syscall::handle::KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("A starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let (offen_vorher, _) = scheduler::handle_anzahl(a).expect("A lebt");

    // B greift alle Handle-Zahlen an.
    let ende = angreifen(2).expect("Angreifer endet");
    assert_eq!(
        ende,
        ProzessEnde::Beendet(0),
        "der Handle-Angriff ist durchgekommen"
    );

    // A hat GENAU dieselben Handles wie vorher — B konnte keines schliessen.
    let (offen_nachher, _) = scheduler::handle_anzahl(a).expect("A lebt noch");
    assert_eq!(
        offen_vorher, offen_nachher,
        "der Angreifer hat A ein Handle weggenommen"
    );

    // Und A's Pipe funktioniert weiterhin.
    let mut puffer = [0u8; 256];
    let mut gelesen = 0usize;
    let bis = zeit::ms_seit_boot() + 500;
    while zeit::ms_seit_boot() < bis && gelesen == 0 {
        if let pipe::PipeErgebnis::Bytes(n) = pipe::lesen(leitung, &mut puffer) {
            gelesen += n;
        }
        zeit::warte_auf_interrupt();
    }
    assert!(gelesen > 0, "A's Pipe wurde durch den Angriff unbrauchbar");

    scheduler::beenden(a);
    scheduler::warten_auf(a, FRIST_MS);
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    assert_eq!(frei_vorher, frames_frei(), "Frames geleckt");
    assert_eq!(pipe::anzahl(), 0, "Pipes geleckt");
}

// ===========================================================================
// TEIL F: Der Dauerbeschuss
// ===========================================================================

/// ALLE Angriffe, mehrfach, in wechselnder Reihenfolge — und danach muss
/// **jede Bilanz** wieder stimmen.
///
/// Ein einzelner Angriff kann ein Leck verdecken; erst die Wiederholung
/// zeigt, ob wirklich alles zurückfliesst. Das ist derselbe Gedanke wie beim
/// Speicher-Pass von Serie 5 (150 Zyklen hole/nslookup/ping).
#[test_case]
fn test_dauerbeschuss_bilanz_bleibt_null() {
    if !programme_vorhanden() {
        return;
    }
    // Aufwärmen (einmalige Kosten aus der Messung nehmen).
    angreifen(1);
    angreifen(20);
    let frei_vorher = frames_frei();
    // HEAP OHNE DEN LOG-PUFFER: `konsole::_print` haengt jede Ausgabe an einen
    // 64-KiB-Ringpuffer (protokoll.rs), und der Angreifer druckt viel. Das
    // ist BESCHRAENKTES, beabsichtigtes Wachstum — kein Leck. Es einfach
    // mitzumessen wuerde den Test falsch rot faerben; es zu ignorieren waere
    // unehrlich. Also wird es BENANNT und herausgerechnet.
    let heap_vorher = heap_ohne_log();
    let pipes_vorher = pipe::anzahl();

    const RUNDEN: usize = 3;
    let angriffe = [1u32, 20, 2, 21, 3, 23, 4, 24, 5, 25, 6, 26, 22];
    for runde in 0..RUNDEN {
        for nummer in angriffe {
            let ende = angreifen(nummer)
                .unwrap_or_else(|| panic!("Runde {}: Angriff {} haengt", runde, nummer));
            // Erwartung je nach Art — aber NIE etwas anderes als diese zwei.
            let erwartet_absturz = nummer >= 20;
            if erwartet_absturz {
                assert_eq!(ende, ProzessEnde::Abgestuerzt, "Runde {}, Angriff {}", runde, nummer);
            } else {
                assert_eq!(ende, ProzessEnde::Beendet(0), "Runde {}, Angriff {}", runde, nummer);
            }
        }
        kernel_lebt_noch();
    }

    let heap_nachher = heap_ohne_log();
    serial_println!(
        "  {} Angriffe ueberstanden. Heap (ohne Log) {} -> {} Byte, \
         Log-Puffer {} Byte, Frames {} -> {}.",
        RUNDEN * angriffe.len(),
        heap_vorher,
        heap_nachher,
        speed_os::protokoll::puffer_bytes(),
        frei_vorher,
        frames_frei()
    );
    assert_eq!(frei_vorher, frames_frei(), "Dauerbeschuss hat Frames geleckt");

    // ==================================================================
    // DER HEAP: eine SCHRANKE, keine Gleichheit — und warum das kein
    // Aufweichen ist
    //
    // `heap_ohne_log` zieht die `capacity()` des Log-Puffers vom belegten
    // Heap ab. Waechst der Puffer WAEHREND der Messung (er tut es — der
    // Angreifer druckt viel), zieht er um: Der Allocator bucht dann den
    // tatsaechlich belegten, GERUNDETEN Block, `capacity()` meldet die
    // angeforderte Groesse. Die Differenz verschiebt sich dadurch um einige
    // hundert Byte, und zwar in BEIDE Richtungen — gemessen wurde hier ein
    // Rueckgang um 368 Byte. Ein Rueckgang ist definitiv kein Leck.
    //
    // Warum die Schranke trotzdem scharf ist: Ein echtes Leck waere
    // PROPORTIONAL zur Zahl der Prozesse. Jeder Angreifer-Lauf alloziert
    // Kilobytes (ELF-Datei, Adressraum-Buchhaltung, Handle-Tabelle); bliebe
    // davon auch nur ein Bruchteil liegen, waere die Differenz nach 39
    // Laeufen weit jenseits von 4 KiB. Die Schranke faengt genau die
    // Rundung und nichts sonst.
    // ==================================================================
    const LOG_RUNDUNG: usize = 4096;
    let abweichung = heap_nachher.abs_diff(heap_vorher);
    assert!(
        abweichung <= LOG_RUNDUNG,
        "Dauerbeschuss hat Heap geleckt: {} -> {} Byte ({} Byte Abweichung, \
         erlaubt sind {} Byte Umzugs-Rundung des Log-Puffers)",
        heap_vorher,
        heap_nachher,
        abweichung,
        LOG_RUNDUNG
    );
    assert_eq!(pipes_vorher, pipe::anzahl(), "Dauerbeschuss hat Pipes geleckt");
    assert_eq!(
        scheduler::momentaufnahme()
            .iter()
            .filter(|zeile| zeile.ist_user)
            .count(),
        0,
        "nach dem Dauerbeschuss sind Prozesse uebrig"
    );
}

/// Damit der Test nicht unbemerkt gar nichts prüft: Der Angreifer MUSS
/// wirklich unprivilegiert laufen und die Programme müssen die erwarteten
/// sein. (Ein Angreifer, der versehentlich in Ring 0 liefe, würde jeden
/// dieser Tests „bestehen".)
#[test_case]
fn test_der_angreifer_laeuft_wirklich_unprivilegiert() {
    if !programme_vorhanden() {
        return;
    }
    let bytes = {
        let pfad = programme::pfad("angreifer");
        fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad)).expect("angreifer lesen")
    };
    let prozess = prozess::prozess_aus_elf("angreifer-pruefung", &bytes, &["angreifer", "1"])
        .expect("bauen");

    // Er hat einen EIGENEN Adressraum ...
    assert!(prozess.ist_user(), "der Angreifer ist kein User-Prozess");
    // ... und sein Start-Rahmen zeigt nach RING 3.
    // unsafe: Der Rahmen liegt im Kernel-Stack dieses Prozesses, den wir
    // gerade selbst angelegt haben.
    let rahmen = unsafe { *(prozess.kontext as *const prozess::TrapFrame) };
    assert!(
        rahmen.aus_ring3(),
        "der Angreifer wuerde in Ring 0 starten — der ganze Test waere wertlos"
    );
    assert_eq!(rahmen.cs, speed_os::gdt::user_code_selektor());
    assert_eq!(rahmen.ss, speed_os::gdt::user_data_selektor());
    // Interrupts sind AN — sonst könnte ihn der Timer nicht verdrängen.
    assert_eq!(rahmen.rflags & (1 << 9), 1 << 9, "IF fehlt im Start-Rahmen");

    let namen: Vec<&str> = programme::PROGRAMME.iter().map(|p| p.name).collect();
    assert!(namen.contains(&"angreifer"), "der Angreifer fehlt im Image");
    drop(prozess);
    let _ = String::new();
}
