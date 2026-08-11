// tests/programme.rs — ECHTE PROGRAMME (Serie 6, Teil 5)
//
// Hier wird bewiesen, was Teil 5 behauptet: SpeedOS kann fremde Programme
// ausfuehren. "Fremd" heisst dabei buchstaeblich — die Programme, die dieser
// Test startet, sind KEIN Teil des Kernel-Binaers im Sinne von Code: Sie
// wurden getrennt uebersetzt, getrennt gelinkt, kennen keinen einzigen
// Kernel-Typ und erreichen SpeedOS ausschliesslich ueber `int 0x80`.
//
// GEPRUEFT WIRD:
//   1. Die eingebetteten Programme sind gueltige ELFs und landen auf /platte.
//   2. Der LEBENSZYKLUS: starten -> laufen -> exit -> Adressraum, Kernel-
//      Stack und Handles vollstaendig frei. Frame-Bilanz BYTE-EXAKT.
//   3. Der EXIT-CODE kommt wirklich aus Ring 3 zurueck.
//   4. ARGUMENTE (argv) kommen im Programm an.
//   5. `kopiere` kopiert wirklich — ueber Syscalls, aus Ring 3.
//   6. W^X und NX wirken WIRKLICH in den Page Tables.
//   7. Ein kaputtes/boesartiges ELF wird abgelehnt, ohne etwas zu lecken.
//   8. Ein abstuerzendes Programm reisst den Kernel nicht mit.
//   9. DER MEILENSTEIN: netzhole holt eine echte Webseite aus dem Internet.
//
// Der ELF-PARSER selbst (kaputte Header, Kernel-Adressen, Ueberlappungen,
// Ueberlaeufe) wird in den Unit-Tests von src/elf.rs zerlegt — die brauchen
// keinen Adressraum und laufen mit `cargo test --lib`.

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
use speed_os::shell::befehl_ausfuehren;
use speed_os::shell::befehle::{alle_befehle, ShellKontext};
use speed_os::{adressraum, allocator, elf, fs, memory, programme, scheduler, serial_println, zeit};
use x86_64::structures::paging::PageTableFlags;
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
    // 2048 Seiten = 8 MiB. Waren bis Serie 8, Teil 7 nur 512 (2 MiB):
    // `programme::installieren()` liest beim Boot JEDE Programmdatei GANZ
    // in den Heap, um sie zu vergleichen, und mit `browser` (1,27 MiB) ist
    // das groesste Programm wieder gewachsen. Dieser Testkern starb genau
    // daran — es ist derselbe Fall wie bei `cssdump` in Teil 5, und der
    // Kommentar in main.rs bittet zu Recht darum, die Zahl bei jedem
    // grossen Programm zu pruefen.
    allocator::heap_erweitern(2048).expect("Heap-Erweiterung fehlgeschlagen");

    // Massenspeicher + Dateisystem: Die Programme sollen von einer ECHTEN
    // Platte geladen werden, nicht aus dem RAM — genau das ist der Punkt.
    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();
    // Netzwerk fuer den Meilenstein-Test.
    speed_os::virtio::net::init();
    speed_os::netz::dhcp::autokonfig(3000);

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

/// Frist, in der ein Programm fertig sein muss.
const FRIST_MS: u64 = 30_000;
/// Frist fuer den Meilenstein (DNS + TCP ueber das echte Internet).
const NETZ_FRIST_MS: u64 = 60_000;

/// Startet ein Programm und wartet auf sein Ende.
fn laufen_lassen(pfad: &str, argumente: &[&str], frist_ms: u64) -> Option<ProzessEnde> {
    let pid = prozess::prozess_starten(pfad, argumente)
        .unwrap_or_else(|fehler| panic!("'{}' starten: {}", pfad, fehler.meldung()));
    scheduler::warten_auf(pid, frist_ms)
}

/// Der Pfad eines mitgelieferten Programms.
fn programm_pfad(name: &str) -> String {
    programme::pfad(name)
}

/// Gibt es die Programme ueberhaupt? (Mit SPEEDOS_OHNE_USERLAND=1 gebaut
/// waeren sie leer — dann ueberspringen die Tests sich selbst, statt falsch
/// zu behaupten, es sei alles in Ordnung.)
fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Wartet, bis alle beendeten Prozesse abgeraeumt sind, und liefert dann die
/// Zahl freier Frames. `aufraeumen` laeuft sonst im Aufraeum-Task, der in
/// einem Test ohne Executor nie drankommt.
fn frames_frei_nach_aufraeumen() -> usize {
    scheduler::aufraeumen();
    // Sockets werden von `socket::schliessen` nur MARKIERT; erst das
    // naechste `aufraeumen` der Netz-Schicht gibt sie frei (die Messfalle
    // aus Serie 6, Teil 4).
    speed_os::netz::pumpen();
    memory::frame_statistik().0
}

// ---------------------------------------------------------------------------
// 1. Die Programme sind da und liegen auf der Platte
// ---------------------------------------------------------------------------

/// Die eingebetteten Programme wurden beim Boot installiert und sind
/// byte-identisch mit dem, was im Kernel-Image steht.
#[test_case]
fn test_programme_liegen_auf_der_platte() {
    if !programme_vorhanden() {
        return;
    }
    for programm in programme::PROGRAMME {
        let pfad = programm_pfad(programm.name);
        let inhalt = fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad))
            .unwrap_or_else(|fehler| panic!("{} lesen: {:?}", pfad, fehler));
        assert_eq!(
            inhalt.len(),
            programm.elf.len(),
            "{} hat auf der Platte die falsche Groesse",
            pfad
        );
        assert!(inhalt == programm.elf, "{} ist auf der Platte verfaelscht", pfad);
        // Und der Kernel erkennt sie als ausfuehrbar (das nutzt der Explorer).
        assert!(prozess::ist_programm(&pfad), "{} gilt nicht als Programm", pfad);
    }
    // Eine gewoehnliche Textdatei ist KEIN Programm — sonst wuerde der
    // Explorer beim Doppelklick das Falsche tun.
    let text = String::from("/test-keine-programmdatei.txt");
    fs::mit_fs(|dateisystem| dateisystem.schreiben(&text, b"Ich bin nur Text.\n"))
        .expect("Testdatei schreiben");
    assert!(!prozess::ist_programm(&text));
    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&text));
}

// ---------------------------------------------------------------------------
// 2. + 3. + 4. Der Lebenszyklus, der Exit-Code und die Argumente
// ---------------------------------------------------------------------------

/// DER GRUNDBEWEIS: `hallo` von der Platte laden, in Ring 3 laufen lassen,
/// sauber mit Code 0 beenden — und danach ist JEDER Frame zurueck.
#[test_case]
fn test_hallo_laeuft_und_raeumt_vollstaendig_auf() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("hallo");

    // Einen Lauf VORWEG, damit alles Einmalige (Heap-Wachstum durch die
    // Dateisystem-Caches, erste Socket-Strukturen) schon passiert ist —
    // sonst misst die Bilanz Anlaufkosten statt Lecks.
    laufen_lassen(&pfad, &["hallo"], FRIST_MS).expect("Aufwaermlauf");
    let frei_vorher = frames_frei_nach_aufraeumen();

    // Fuenf Laeufe: Ein einzelner koennte ein Leck zufaellig verdecken.
    for durchgang in 0..5 {
        let ende = laufen_lassen(&pfad, &["hallo"], FRIST_MS)
            .unwrap_or_else(|| panic!("hallo (Durchgang {}) wurde nicht fertig", durchgang));
        assert_eq!(
            ende,
            ProzessEnde::Beendet(0),
            "hallo muss mit Code 0 enden (Durchgang {})",
            durchgang
        );
    }

    let frei_nachher = frames_frei_nach_aufraeumen();
    assert_eq!(
        frei_vorher, frei_nachher,
        "Prozess-Lebenszyklus leckt Frames: vorher {} frei, nachher {}",
        frei_vorher, frei_nachher
    );

    // Und die Prozess-Tabelle ist wieder leer (ausser PID 0).
    let laufende = scheduler::momentaufnahme();
    assert_eq!(
        laufende.iter().filter(|zeile| zeile.ist_user).count(),
        0,
        "es sind User-Prozesse uebrig geblieben"
    );
}

/// Der EXIT-CODE stammt wirklich aus Ring 3: `hallo --code=N` beendet sich
/// mit N. Das prueft die ganze Kette rdi -> exit-Syscall -> Prozess-
/// Kontrollblock -> `warten_auf`.
#[test_case]
fn test_exit_code_kommt_aus_ring3_zurueck() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("hallo");
    for erwartet in [0u64, 1, 7, 42, 255] {
        let argument = alloc::format!("--code={}", erwartet);
        let ende = laufen_lassen(&pfad, &["hallo", &argument], FRIST_MS)
            .expect("hallo wurde nicht fertig");
        assert_eq!(
            ende,
            ProzessEnde::Beendet(erwartet),
            "Exit-Code {} kam nicht zurueck",
            erwartet
        );
        assert_eq!(ende.code(), erwartet);
    }
    scheduler::aufraeumen();
}

/// ARGUMENTE kommen an. Geprueft wird das ueber den Exit-Code: `hallo` gibt
/// nur dann den Wunsch-Code zurueck, wenn es `argv[1]` wirklich gelesen hat
/// — und `argv[1]` existiert nur, wenn rdi/rsi und der argv-Aufbau auf dem
/// User-Stack stimmen.
#[test_case]
fn test_argumente_kommen_im_programm_an() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("hallo");

    // OHNE Argument (nur argv[0]) -> Code 0, kein Wunsch.
    assert_eq!(
        laufen_lassen(&pfad, &["hallo"], FRIST_MS).expect("Lauf ohne Argument"),
        ProzessEnde::Beendet(0)
    );
    // MIT Argument -> der Wunsch-Code. Damit ist argv[1] nachweislich
    // gelesen worden.
    assert_eq!(
        laufen_lassen(&pfad, &["hallo", "--code=33"], FRIST_MS).expect("Lauf mit Argument"),
        ProzessEnde::Beendet(33)
    );
    // Auch mit vielen Argumenten (der Aufbau darf nicht kippen).
    let viele: Vec<&str> = alloc::vec![
        "hallo", "--code=9", "zwei", "drei", "vier", "fuenf", "sechs", "sieben"
    ];
    assert_eq!(
        laufen_lassen(&pfad, &viele, FRIST_MS).expect("Lauf mit vielen Argumenten"),
        ProzessEnde::Beendet(9)
    );
    scheduler::aufraeumen();
}

/// Zu viele/zu lange Argumente werden sauber ABGELEHNT — und zwar BEVOR ein
/// Prozess entsteht (kein halb gebauter Adressraum, kein Leck).
#[test_case]
fn test_argument_grenzen() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("hallo");
    let frei_vorher = frames_frei_nach_aufraeumen();

    // Mehr als MAX_ARGUMENTE:
    let zu_viele: Vec<&str> = alloc::vec!["x"; prozess::MAX_ARGUMENTE + 1];
    assert_eq!(
        prozess::prozess_starten(&pfad, &zu_viele),
        Err(prozess::StartFehler::Argumente)
    );
    // Ein einzelnes Argument ueber MAX_ARGUMENT_BYTES:
    let lang = String::from_utf8(alloc::vec![b'a'; prozess::MAX_ARGUMENT_BYTES + 1]).unwrap();
    assert_eq!(
        prozess::prozess_starten(&pfad, &["hallo", &lang]),
        Err(prozess::StartFehler::Argumente)
    );
    // Viele mittellange Argumente, die zusammen den Deckel sprengen:
    let mittel = String::from_utf8(alloc::vec![b'b'; 200]).unwrap();
    let summe: Vec<&str> = alloc::vec![mittel.as_str(); prozess::MAX_ARGUMENTE];
    assert_eq!(
        prozess::prozess_starten(&pfad, &summe),
        Err(prozess::StartFehler::Argumente)
    );

    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "abgelehnte Argumente haben Frames geleckt"
    );
}

// ---------------------------------------------------------------------------
// 5. `kopiere` — ein echtes Werkzeug ueber Syscalls
// ---------------------------------------------------------------------------

/// `kopiere` kopiert eine Datei WIRKLICH — aus Ring 3, ueber oeffne/lese_at/
/// schreibe_at/schliesse. Der Test schreibt die Quelle, laesst das Programm
/// laufen und vergleicht das Ergebnis byteweise.
#[test_case]
fn test_kopiere_kopiert_wirklich() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("kopiere");
    let basis = String::from(fs::persistenter_pfad("/platte", ""));
    let quelle = alloc::format!("{}/test-kopiere-quelle.bin", basis);
    let ziel = alloc::format!("{}/test-kopiere-ziel.bin", basis);

    // Ein Inhalt, der ueber mehrere Kopier-Stuecke (4096 B) hinausgeht und
    // ein erkennbares Muster hat.
    let inhalt: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    fs::mit_fs(|dateisystem| dateisystem.schreiben(&quelle, &inhalt)).expect("Quelle schreiben");
    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&ziel));

    let ende = laufen_lassen(&pfad, &["kopiere", &quelle, &ziel], FRIST_MS)
        .expect("kopiere wurde nicht fertig");
    assert_eq!(ende, ProzessEnde::Beendet(0), "kopiere meldete einen Fehler");

    let kopie = fs::mit_fs(|dateisystem| dateisystem.lesen(&ziel)).expect("Ziel lesen");
    assert_eq!(kopie.len(), inhalt.len(), "die Kopie hat die falsche Laenge");
    assert!(kopie == inhalt, "die Kopie ist nicht byte-identisch");

    // Fehlerfaelle: fehlende Quelle und fehlende Argumente enden mit
    // Fehlercode, nicht mit einem Absturz.
    let fehlt = alloc::format!("{}/gibt-es-nicht-12345.bin", basis);
    let ende = laufen_lassen(&pfad, &["kopiere", &fehlt, &ziel], FRIST_MS).expect("Lauf");
    assert_eq!(ende, ProzessEnde::Beendet(1), "fehlende Quelle -> Code 1");
    let ende = laufen_lassen(&pfad, &["kopiere"], FRIST_MS).expect("Lauf");
    assert_eq!(ende, ProzessEnde::Beendet(2), "fehlende Argumente -> Code 2");

    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&quelle));
    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&ziel));
    scheduler::aufraeumen();
}

// ---------------------------------------------------------------------------
// 6. W^X und NX — nicht behauptet, sondern in den Page Tables nachgesehen
// ---------------------------------------------------------------------------

/// Die Segment-Rechte landen WIRKLICH in den Page Tables: Code-Seiten sind
/// nicht beschreibbar, Daten- und Stack-Seiten nicht ausfuehrbar.
///
/// Geprueft wird im INAKTIVEN Adressraum (der Prozess laeuft noch nicht) —
/// genau dafuer gibt es `AdressRaum::seiten_flags`.
#[test_case]
fn test_segment_rechte_stehen_in_den_page_tables() {
    if !programme_vorhanden() {
        return;
    }
    // NX muss ueberhaupt benutzbar sein, sonst ist die Haelfte des Tests
    // gegenstandslos — und das waere eine stille Sicherheitsluecke.
    assert!(
        memory::nx_aktiv(),
        "EFER.NXE ist aus — nicht ausfuehrbare Seiten waeren wirkungslos"
    );

    let pfad = programm_pfad("netzhole"); // hat alle drei Segment-Arten + .bss
    let bytes = fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad)).expect("netzhole lesen");
    let geprueft = elf::pruefen(&bytes).expect("netzhole muss gueltig sein");
    let prozess =
        prozess::prozess_aus_elf("netzhole-rechte-test", &bytes, &["netzhole"]).expect("bauen");
    let raum = prozess.raum.as_ref().expect("User-Prozess hat einen Adressraum");

    for segment in &geprueft.segmente {
        let mut seite = segment.erste_seite();
        while seite < segment.seite_dahinter() {
            let flags = raum
                .seiten_flags(VirtAddr::new(seite))
                .unwrap_or_else(|| panic!("Seite {:#x} ist nicht gemappt", seite));
            assert!(flags.contains(PageTableFlags::PRESENT));
            // Ring 3 muss ueberhaupt herankommen ...
            assert!(
                flags.contains(PageTableFlags::USER_ACCESSIBLE),
                "Seite {:#x} ist nicht user-zugaenglich",
                seite
            );
            // ... und genau die Rechte haben, die im ELF stehen.
            assert_eq!(
                flags.contains(PageTableFlags::WRITABLE),
                segment.rechte.schreiben,
                "Schreibrecht der Seite {:#x} stimmt nicht",
                seite
            );
            assert_eq!(
                flags.contains(PageTableFlags::NO_EXECUTE),
                !segment.rechte.ausfuehren,
                "Ausfuehrrecht der Seite {:#x} stimmt nicht",
                seite
            );
            // W^X: niemals beides.
            assert!(
                !(flags.contains(PageTableFlags::WRITABLE)
                    && !flags.contains(PageTableFlags::NO_EXECUTE)),
                "Seite {:#x} ist schreibbar UND ausfuehrbar",
                seite
            );
            seite += 4096;
        }
    }

    // Der STACK: beschreibbar, aber nicht ausfuehrbar — und mit Guard-Page.
    let stack_seite = VirtAddr::new(prozess::ELF_STACK_OBEN - 4096);
    let stack_flags = raum.seiten_flags(stack_seite).expect("Stack-Seite fehlt");
    assert!(stack_flags.contains(PageTableFlags::WRITABLE));
    assert!(
        stack_flags.contains(PageTableFlags::NO_EXECUTE),
        "der Stack darf nicht ausfuehrbar sein"
    );
    let guard = VirtAddr::new(
        prozess::ELF_STACK_OBEN - (prozess::ELF_STACK_SEITEN as u64 + 1) * 4096,
    );
    assert!(
        raum.seiten_flags(guard).is_none(),
        "unter dem Stack fehlt die Guard-Page"
    );

    // Die LUECKE zwischen Programm und Stack ist wirklich ungemappt.
    assert!(raum
        .seiten_flags(VirtAddr::new(elf::IMAGE_ENDE))
        .is_none());

    // Und die .bss ist GENULLT — der Beweis, dass memsz > filesz richtig
    // behandelt wird (netzhole hat 64 KiB davon).
    let bss_segment = geprueft
        .segmente
        .iter()
        .find(|segment| segment.bss_bytes() >= 64 * 1024)
        .expect("netzhole muss ein Segment mit grosser .bss haben");
    let bss_start = bss_segment.virt_adresse + bss_segment.datei_bytes as u64;
    let mut probe = [0xAAu8; 256];
    raum.lesen(VirtAddr::new(bss_start), &mut probe)
        .expect(".bss lesen");
    assert_eq!(probe, [0u8; 256], ".bss ist nicht genullt");
    // Auch weit hinten (nicht nur die erste Seite).
    raum.lesen(VirtAddr::new(bss_start + 60 * 1024), &mut probe)
        .expect(".bss (hinten) lesen");
    assert_eq!(probe, [0u8; 256], ".bss ist hinten nicht genullt");

    drop(prozess);
}

// ---------------------------------------------------------------------------
// 7. Kaputte und boesartige Programmdateien
// ---------------------------------------------------------------------------

/// Eine kaputte Programmdatei wird abgelehnt — mit einem klaren Fehler und
/// OHNE einen einzigen geleckten Frame. Der Loader mappt erst, nachdem er
/// alles geprueft hat; hier wird nachgemessen, dass das stimmt.
#[test_case]
fn test_kaputte_programme_werden_abgelehnt_ohne_leck() {
    if !programme_vorhanden() {
        return;
    }
    // Den Pfad VORHER binden: `programm_pfad` nimmt selbst den VFS-Lock, und
    // ihn innerhalb einer `mit_fs`-Closure auszuwerten waere ein Deadlock
    // (der Klassiker aus CLAUDE.md — hier prompt hineingelaufen).
    let hallo_pfad = programm_pfad("hallo");
    let echt = fs::mit_fs(|dateisystem| dateisystem.lesen(&hallo_pfad)).expect("hallo lesen");
    let basis = String::from(fs::persistenter_pfad("/platte", ""));
    let pfad = alloc::format!("{}/test-kaputt.bin", basis);

    let frei_vorher = frames_frei_nach_aufraeumen();

    // Eine Reihe von Verstuemmelungen — jede muss einen Fehler geben.
    let mut faelle: Vec<(&str, Vec<u8>)> = Vec::new();
    faelle.push(("leer", Vec::new()));
    faelle.push(("nur Text", b"Ich bin eine Textdatei, kein Programm.\n".to_vec()));
    // Abgeschnitten an mehreren Stellen (auch mitten im Segment-Inhalt).
    for teil in [1usize, 4, 63, 64, 100, 1000] {
        if teil < echt.len() {
            faelle.push(("abgeschnitten", echt[..teil].to_vec()));
        }
    }
    // Magie zerstoert:
    let mut ohne_magie = echt.clone();
    ohne_magie[2] = b'X';
    faelle.push(("falsche Magie", ohne_magie));
    // Als 32-Bit-ELF ausgegeben:
    let mut bit32 = echt.clone();
    bit32[4] = 1;
    faelle.push(("32 Bit", bit32));
    // Als ET_DYN (PIE) ausgegeben:
    let mut dyn_elf = echt.clone();
    dyn_elf[16..18].copy_from_slice(&3u16.to_le_bytes());
    faelle.push(("ET_DYN", dyn_elf));
    // Einsprung auf eine KERNEL-Adresse:
    let mut boeser_einsprung = echt.clone();
    boeser_einsprung[24..32]
        .copy_from_slice(&(allocator::HEAP_START as u64).to_le_bytes());
    faelle.push(("Einsprung im Kernel", boeser_einsprung));
    // Erstes Segment auf eine Kernel-Adresse umgebogen (p_vaddr im ersten
    // Program-Header, Offset 64+16):
    let mut boeses_segment = echt.clone();
    boeses_segment[80..88].copy_from_slice(&(allocator::HEAP_START as u64).to_le_bytes());
    faelle.push(("Segment im Kernel", boeses_segment));
    // Segment-Groesse absurd (p_memsz, Offset 64+40):
    let mut riesig = echt.clone();
    riesig[104..112].copy_from_slice(&u64::MAX.to_le_bytes());
    faelle.push(("Segment u64::MAX", riesig));
    // Segment schreibbar UND ausfuehrbar (p_flags, Offset 64+4):
    let mut wx = echt.clone();
    wx[68..72].copy_from_slice(&7u32.to_le_bytes());
    faelle.push(("W+X", wx));

    for (name, bytes) in &faelle {
        // Direkt ueber den Lader ...
        let fehler = prozess::prozess_aus_elf("kaputt", bytes, &["kaputt"])
            .err()
            .unwrap_or_else(|| panic!("'{}' haette abgelehnt werden muessen", name));
        assert!(
            matches!(fehler, prozess::StartFehler::Elf(_)),
            "'{}' ergab {:?} statt eines ELF-Fehlers",
            name,
            fehler
        );
        // ... und ueber den ganzen Weg von der Platte.
        fs::mit_fs(|dateisystem| dateisystem.schreiben(&pfad, bytes)).expect("schreiben");
        assert!(
            prozess::prozess_starten(&pfad, &["kaputt"]).is_err(),
            "'{}' wurde von der Platte gestartet, obwohl es kaputt ist",
            name
        );
    }

    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&pfad));
    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "abgelehnte Programme haben Frames geleckt"
    );
}

// ---------------------------------------------------------------------------
// 8. Ein abstuerzendes Programm reisst den Kernel nicht mit
// ---------------------------------------------------------------------------

/// Dauerregel II an einem ECHTEN Programm: Wir nehmen `hallo` und biegen
/// seinen Einsprung in die (ungemappte) Luecke zwischen Programm und Stack.
/// Das ELF bleibt formal gueltig — der Fehler tritt erst beim AUSFUEHREN
/// auf. Erwartung: Der Prozess stirbt, alles wird frei, der Kernel laeuft
/// weiter, und danach laufen weitere Programme ganz normal.
#[test_case]
fn test_absturz_reisst_den_kernel_nicht_mit() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("hallo");
    let frei_vorher = frames_frei_nach_aufraeumen();

    // Ein Programm, das SOFORT auf eine Kernel-Adresse zugreift (der
    // bewaehrte Absturz-Kandidat aus Teil 3) — nur dass wir hier pruefen,
    // dass danach ECHTE Programme weiterlaufen.
    let absturz = prozess::absturz_prozess().expect("Absturz-Prozess bauen");
    let absturz_pid = scheduler::einplanen(absturz).expect("einplanen");
    let ende = scheduler::warten_auf(absturz_pid, FRIST_MS).expect("Absturz-Prozess endet");
    assert_eq!(
        ende,
        ProzessEnde::Abgestuerzt,
        "ein Zugriff auf Kernel-Speicher muss als Absturz enden"
    );
    assert_eq!(ende.code(), 139);

    // Der Kernel lebt: ein echtes Programm laeuft danach normal.
    assert_eq!(
        laufen_lassen(&pfad, &["hallo"], FRIST_MS).expect("hallo nach dem Absturz"),
        ProzessEnde::Beendet(0)
    );

    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "ein abgestuerzter Prozess hat Frames geleckt"
    );
}

/// Ein Prozess, der von aussen GESTOPPT wird, gibt ebenfalls alles zurueck.
#[test_case]
fn test_gestoppter_prozess_raeumt_auf() {
    if !programme_vorhanden() {
        return;
    }
    let frei_vorher = frames_frei_nach_aufraeumen();
    // Ein Schlaefer laeuft, bis ihn jemand beendet.
    let schlaefer = prozess::schlaefer_prozess(50).expect("Schlaefer bauen");
    let pid: Pid = scheduler::einplanen(schlaefer).expect("einplanen");
    // Kurz laufen lassen, dann stoppen.
    let bis = zeit::ms_seit_boot() + 100;
    while zeit::ms_seit_boot() < bis {
        x86_64::instructions::hlt();
    }
    assert!(scheduler::beenden(pid), "Beenden muss gelingen");
    let ende = scheduler::warten_auf(pid, FRIST_MS).expect("gestoppter Prozess endet");
    assert_eq!(ende, ProzessEnde::Gestoppt);

    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "ein gestoppter Prozess hat Frames geleckt"
    );
}

// ---------------------------------------------------------------------------
// 9. DER MEILENSTEIN
// ---------------------------------------------------------------------------

/// „Ein eigenstaendiges Programm, von der eigenen Platte geladen, im eigenen
/// Adressraum, holt ueber den eigenen Netzwerk-Stack eine Webseite aus dem
/// Internet."
///
/// Genau das, in einem Test. `netzhole` speichert den Rumpf auf die Platte,
/// damit der Test das Ergebnis nachpruefen kann — die Ausgabe eines
/// Ring-3-Programms kann er ja sonst nicht einsammeln.
///
/// EHRLICHE EINSCHRAENKUNG: Dieser Test braucht Internet auf dem HOST. Ohne
/// Netz gibt es keinen Meilenstein zu beweisen — dann meldet er das laut und
/// gilt trotzdem als bestanden, statt eine fremde Leitung zum Testkriterium
/// zu machen (dieselbe Methodik wie bei tests/netz_stress.rs: das harte Gate
/// liegt auf dem, was wir kontrollieren).
#[test_case]
fn test_meilenstein_netzhole_holt_eine_webseite() {
    if !programme_vorhanden() {
        return;
    }
    let konfig = speed_os::netz::konfig();
    if konfig.ip == speed_os::netz::Ipv4([0, 0, 0, 0]) {
        serial_println!("  (uebersprungen: keine IP-Konfiguration — kein Netz vorhanden)");
        return;
    }

    let pfad = programm_pfad("netzhole");
    let basis = String::from(fs::persistenter_pfad("/platte", ""));
    let ziel = alloc::format!("{}/test-netzhole.html", basis);
    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&ziel));

    let frei_vorher = frames_frei_nach_aufraeumen();

    serial_println!();
    serial_println!("  === MEILENSTEIN: netzhole http://example.com ===");
    let ende = laufen_lassen(&pfad, &["netzhole", "http://example.com", &ziel], NETZ_FRIST_MS);

    match ende {
        Some(ProzessEnde::Beendet(0)) => {
            let seite = fs::mit_fs(|dateisystem| dateisystem.lesen(&ziel))
                .expect("die geholte Seite muss auf der Platte liegen");
            assert!(
                seite.len() > 100,
                "die geholte Seite ist verdaechtig kurz ({} Byte)",
                seite.len()
            );
            // Es muss wirklich HTML sein, das example.com liefert.
            let text = alloc::string::String::from_utf8_lossy(&seite);
            let klein = text.to_lowercase();
            assert!(
                klein.contains("<html") || klein.contains("<!doctype"),
                "die Antwort sieht nicht wie HTML aus"
            );
            assert!(
                klein.contains("example domain") || klein.contains("example"),
                "die Antwort stammt offenbar nicht von example.com"
            );
            serial_println!(
                "  MEILENSTEIN ERREICHT: {} Byte von example.com — geholt von einem",
                seite.len()
            );
            serial_println!("  Ring-3-Programm, das von der eigenen Platte geladen wurde.");
        }
        Some(anderes) => {
            // Kein Absturz und kein Haenger — aber der Abruf hat nicht
            // geklappt (fremder Server, kein Internet auf dem Host).
            serial_println!(
                "  (Meilenstein nicht messbar: netzhole endete mit {:?} — \
                 vermutlich kein Internet auf dem Host)",
                anderes
            );
            assert_ne!(
                anderes,
                ProzessEnde::Abgestuerzt,
                "netzhole darf NIE abstuerzen, auch ohne Netz nicht"
            );
        }
        None => panic!("netzhole wurde in {} s nicht fertig — Haenger!", NETZ_FRIST_MS / 1000),
    }

    let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&ziel));
    // AUCH HIER: Nach einem Netz-Programm muss alles zurueck sein — offene
    // Sockets inklusive (die Handle-Tabelle im Prozess schliesst sie).
    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "netzhole hat Frames geleckt"
    );
    // Und der Socket ist zurueck. NICHT sofort messen: `schliessen` MARKIERT
    // nur, und eine TCP-Verbindung wird geordnet abgebaut (FIN/ACK, dann
    // TIME_WAIT — bei uns auf 2 s verkuerzt). Erst `bedienen()` raeumt den
    // fertigen Socket ab. Wer sofort misst, misst den Abbau, nicht ein Leck
    // (die Messfalle aus Serie 6, Teil 4).
    assert!(
        sockets_abwarten(10_000),
        "netzhole hat einen Socket offen gelassen ({} nach 10 s)",
        speed_os::netz::socket::anzahl()
    );
}

/// Pumpt den Netz-Stack, bis kein Socket mehr offen ist. `true` = alle weg.
fn sockets_abwarten(frist_ms: u64) -> bool {
    let frist = zeit::ms_seit_boot() + frist_ms;
    loop {
        speed_os::netz::pumpen();
        if speed_os::netz::socket::anzahl() == 0 {
            return true;
        }
        if zeit::ms_seit_boot() >= frist {
            return false;
        }
        zeit::warte_auf_interrupt();
    }
}

/// `netzhole` mit unsinnigen Argumenten: sauberer Fehlercode, nie ein
/// Absturz und nie ein Haenger.
#[test_case]
fn test_netzhole_boesartige_argumente() {
    if !programme_vorhanden() {
        return;
    }
    let pfad = programm_pfad("netzhole");
    for (argument, erwartet) in [
        ("", 2u64),                       // fehlt ganz
        ("nichts-mit-schema", 2),         // kein http://
        ("https://example.com", 2),       // TLS gibt es nicht
        ("http://", 2),                   // kein Gastgeber
        ("http://x:99999/", 2),           // Port zu gross
        ("http://x:abc/", 2),             // Port keine Zahl
    ] {
        let argumente: Vec<&str> = if argument.is_empty() {
            alloc::vec!["netzhole"]
        } else {
            alloc::vec!["netzhole", argument]
        };
        let ende = laufen_lassen(&pfad, &argumente, FRIST_MS)
            .unwrap_or_else(|| panic!("netzhole '{}' wurde nicht fertig", argument));
        assert_eq!(
            ende,
            ProzessEnde::Beendet(erwartet),
            "netzhole '{}' erwartete Code {}",
            argument,
            erwartet
        );
    }
    scheduler::aufraeumen();
}

// ---------------------------------------------------------------------------
// Die SHELL-Seite: `starte`, `programme`, `elfinfo`
// ---------------------------------------------------------------------------

/// Die neuen Shell-Befehle laufen end-to-end durch die Registry — genau so,
/// wie ein Mensch sie tippt. Das prueft den Weg, den `tests/netz_shell.rs`
/// fuer die Netz-Befehle prueft: Argument-Zerlegung, Pfad-Aufloesung,
/// Kurznamen, Fehlermeldungen.
///
/// Der Beweis, dass `starte` wirklich etwas gestartet hat, ist die Ausgabe
/// des Programms selbst (sie landet ueber print! auf der seriellen
/// Schnittstelle) — und dass die Prozess-Tabelle danach wieder leer ist.
#[test_case]
fn test_shell_befehle_starte_und_programme() {
    if !programme_vorhanden() {
        return;
    }
    let registry = alle_befehle();
    let mut ctx = ShellKontext::neu();

    // Die drei neuen Befehle muessen in der Registry stehen (sonst faende
    // sie `help` nicht, und der Dispatcher meldete "unbekannter Befehl").
    for name in ["starte", "programme", "elfinfo"] {
        assert!(
            registry.iter().any(|befehl| befehl.name() == name),
            "Befehl '{}' fehlt in der Registry",
            name
        );
    }

    let frei_vorher = frames_frei_nach_aufraeumen();

    for zeile in [
        "programme",
        // DER PIPE-BEWEIS (Serie 6, Teil 6).
        "starte zaehle 20 | filter 7",
        "starte zaehle 5",
        // KURZNAME statt Pfad — `starte` sucht im Programm-Verzeichnis.
        "starte hallo",
        // Mit Argument (und damit mit gesetztem Exit-Code).
        "starte hallo --code=5",
        // Voller Pfad.
        "starte /platte/programme/hallo",
        "elfinfo hallo",
        "elfinfo netzhole",
        // FEHLERFAELLE: duerfen melden, aber nie haengen oder panicken.
        "starte gibt-es-nicht",
        "starte /platte/programme",
        "elfinfo gibt-es-nicht",
        "elfinfo",
        "starte",
    ] {
        serial_println!("  $ {}", zeile);
        befehl_ausfuehren(&registry, &mut ctx, zeile);
    }

    // Nach allen Laeufen ist kein Prozess uebrig ...
    assert_eq!(
        scheduler::momentaufnahme()
            .iter()
            .filter(|zeile| zeile.ist_user)
            .count(),
        0,
        "nach den Shell-Befehlen sind Prozesse uebrig"
    );
    // ... und kein Frame verloren.
    assert_eq!(
        frei_vorher,
        frames_frei_nach_aufraeumen(),
        "die Shell-Befehle haben Frames geleckt"
    );
}

// ---------------------------------------------------------------------------
// Isolation: zwei Programme, dieselben Adressen, verschiedene Welten
// ---------------------------------------------------------------------------

/// Zwei gleichzeitig laufende Programme haben denselben Einsprung und
/// denselben Stack — in VERSCHIEDENEN Adressraeumen. Das ist die Zusage von
/// Teil 2, hier an echten Programmen nachgemessen.
#[test_case]
fn test_zwei_programme_gleiche_adressen_getrennte_welten() {
    if !programme_vorhanden() {
        return;
    }
    // Pfad vorher binden (VFS-Lock, siehe oben).
    let hallo_pfad = programm_pfad("hallo");
    let bytes = fs::mit_fs(|dateisystem| dateisystem.lesen(&hallo_pfad)).expect("hallo lesen");

    let a = prozess::prozess_aus_elf("hallo-A", &bytes, &["hallo", "A"]).expect("A bauen");
    let b = prozess::prozess_aus_elf("hallo-B", &bytes, &["hallo", "BB"]).expect("B bauen");

    let raum_a = a.raum.as_ref().expect("A hat einen Adressraum");
    let raum_b = b.raum.as_ref().expect("B hat einen Adressraum");

    // Verschiedene Level-4-Tabellen ...
    assert_ne!(
        raum_a.p4_frame(),
        raum_b.p4_frame(),
        "zwei Prozesse teilen sich eine P4 — es gibt keine Isolation"
    );
    // ... aber derselbe Programmcode an derselben Adresse.
    let mut code_a = [0u8; 32];
    let mut code_b = [0u8; 32];
    raum_a
        .lesen(VirtAddr::new(elf::IMAGE_START), &mut code_a)
        .expect("Code A");
    raum_b
        .lesen(VirtAddr::new(elf::IMAGE_START), &mut code_b)
        .expect("Code B");
    assert_eq!(code_a, code_b, "beide sollten denselben Code sehen");

    // Und im KERNEL-Adressraum ist an dieser Adresse NICHTS — der Kernel
    // kann User-Speicher gar nicht versehentlich anfassen.
    adressraum::kernel_aktivieren();
    assert!(memory::seiten_flags(VirtAddr::new(elf::IMAGE_START)).is_none());

    // DIE ARGUMENTE LIEGEN GETRENNT — an DERSELBEN Stack-Adresse, mit
    // VERSCHIEDENEM Inhalt. Das ist die Isolation in einem Satz.
    //
    // Die Zeichenketten stehen dicht gepackt am oberen Stack-Ende
    // (`argv_schreiben`): erst argv[0], dann argv[1]. Beide Prozesse haben
    // dasselbe argv[0] ("hallo") — der Unterschied steht DAHINTER, und genau
    // dort wird nachgesehen. (Nachgelesen wird ueber das Physik-Mapping,
    // ohne einen der beiden Adressraeume zu aktivieren.)
    let mut argv_a = [0u8; 8];
    let mut argv_b = [0u8; 8];
    let stelle = VirtAddr::new(prozess::ELF_STACK_OBEN - 16);
    raum_a.lesen(stelle, &mut argv_a).expect("argv A");
    raum_b.lesen(stelle, &mut argv_b).expect("argv B");
    // Gleicher Programmname ...
    assert_eq!(&argv_a[..5], b"hallo", "argv[0] von A stimmt nicht");
    assert_eq!(&argv_b[..5], b"hallo", "argv[0] von B stimmt nicht");
    // ... verschiedenes zweites Argument, an derselben Adresse.
    assert_eq!(&argv_a[5..6], b"A", "argv[1] von A stimmt nicht");
    assert_eq!(&argv_b[5..7], b"BB", "argv[1] von B stimmt nicht");
    assert_ne!(
        argv_a, argv_b,
        "beide Prozesse haben dieselben argv-Bytes — sie teilen sich Speicher!"
    );

    drop(a);
    drop(b);
}
