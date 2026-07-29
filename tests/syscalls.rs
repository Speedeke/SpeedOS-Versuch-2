// tests/syscalls.rs — Die ABI unter Feuer (Serie 6, Teil 4)
//
// Jeder Syscall wird hier AUS RING 3 aufgerufen — mit gültigen und mit
// böswilligen Argumenten. Möglich macht das der PRÜFSTAND (`prozess::
// pruefstand_programm`): ein winziges Ring-3-Programm, das Syscall-Nummer und
// Argumente aus seinem eigenen Speicher liest, `int 0x80` auslöst und
// Fehlercode + Ergebnis wieder dort ablegt. Der Test schreibt Aufträge hinein
// und liest Antworten heraus — über das Physik-Komplettmapping, ohne den
// Adressraum des Prozesses zu aktivieren.
//
// Dadurch läuft jeder Testfall als gewöhnlicher Rust-Code, während der Aufruf
// ECHT unprivilegiert ist: eigener Adressraum, eigene Handle-Tabelle, echte
// dreistufige Zeigerprüfung. Ein Angriff im Test ist ein echter Angriff.
//
// GEPRÜFT WIRD:
//   * Gruppe 0/1/2 im Erfolgsfall (inklusive copy-OUT: der Kernel schreibt
//     `stat`- und `lese_at`-Ergebnisse in den Prozess, wir lesen sie zurück),
//   * unbekannte Syscall-Nummern,
//   * Handles: fremd, geschlossen, nie vergeben, "negativ" (u64::MAX),
//     reserviert, falscher Typ,
//   * Zeiger: Kernel-Adresse, Nullzeiger, ungemappt, über die Seitengrenze,
//   * Längen: 0, über der Obergrenze, u64::MAX,
//   * Pfade: relativ, kein UTF-8, zu lang, Länge 0,
//   * HANDLE-ISOLATION zwischen zwei Prozessen (aus Ring 3!),
//   * HANDLE-LECK über das Prozess-Ende (das war die Lücke aus der
//     Bestandsaufnahme).

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::prozess::{self, Pid};
use speed_os::syscall::datei::{StatDaten, DIR_EINTRAG_GROESSE};
use speed_os::syscall::handle::{
    ERBE_KEINS, HANDLE_AUSGABE, HANDLE_DIAGNOSE, HANDLE_EINGABE, MODUS_ABSCHNEIDEN,
    MODUS_ANLEGEN, MODUS_LESEN, MODUS_SCHREIBEN,
};
use speed_os::syscall::netz::{TYP_TCP, TYP_UDP, ZUSTAND_NEU};
use speed_os::syscall::{self as sys, Fehler};
use speed_os::{adressraum, allocator, memory, scheduler, serial_println, zeit};
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
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");

    // Ein Dateisystem für Gruppe 1 (RamFs als Wurzel genügt) ...
    speed_os::fs::init();
    // ... und eine Netzwerkkarte für Gruppe 2.
    speed_os::pci::init();
    speed_os::virtio::net::init();
    speed_os::netz::dhcp::autokonfig(3000);

    // Und der Scheduler: ohne ihn gibt es keine Prozesse und keine
    // Handle-Tabellen.
    scheduler::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Der Prüfstand: eine Fernbedienung für Ring-3-Syscalls
// ---------------------------------------------------------------------------

/// Frist für einen einzelnen Syscall-Auftrag. Grosszügig, weil `verbinde` und
/// `aufloesen` bewusst blockieren dürfen (siehe docs/syscalls.md §6).
const AUFTRAG_FRIST_MS: u64 = 20_000;

/// Basis der Auftrags-Struktur im Adressraum des Prüfstands.
const AUFTRAG_VA: u64 = prozess::ZAEHLER_CODE_VA + prozess::PRUEFSTAND_AUFTRAG_OFFSET;
/// Basis des Test-Puffers (für Pfade und Daten).
const PUFFER_VA: u64 = prozess::ZAEHLER_CODE_VA + prozess::PRUEFSTAND_PUFFER_OFFSET;

struct Pruefstand {
    pid: Pid,
}

impl Pruefstand {
    fn neu() -> Pruefstand {
        let prozess = prozess::pruefstand_prozess().expect("Pruefstand bauen");
        let pid = scheduler::einplanen(prozess).expect("Pruefstand einplanen");
        Pruefstand { pid }
    }

    /// Adresse im Test-Puffer des Prozesses.
    fn va(&self, offset: u64) -> u64 {
        assert!(offset < prozess::PRUEFSTAND_PUFFER_GROESSE, "Puffer-Offset zu gross");
        PUFFER_VA + offset
    }

    /// Legt Daten in den Speicher des Prozesses und liefert ihre Adresse.
    fn hinlegen(&self, offset: u64, daten: &[u8]) -> u64 {
        let va = self.va(offset);
        scheduler::mit_prozess_raum(self.pid, |raum| raum.schreiben(VirtAddr::new(va), daten))
            .expect("Prozess existiert")
            .expect("in den Prozess schreiben");
        va
    }

    /// Liest Daten aus dem Speicher des Prozesses zurück.
    fn abholen(&self, offset: u64, ziel: &mut [u8]) {
        let va = self.va(offset);
        scheduler::mit_prozess_raum(self.pid, |raum| raum.lesen(VirtAddr::new(va), ziel))
            .expect("Prozess existiert")
            .expect("aus dem Prozess lesen");
    }

    /// Schreibt ein u64-Feld der Auftrags-Struktur.
    fn feld_setzen(&self, feld_offset: u64, wert: u64) {
        scheduler::mit_prozess_raum(self.pid, |raum| {
            raum.schreiben(VirtAddr::new(AUFTRAG_VA + feld_offset), &wert.to_le_bytes())
        })
        .expect("Prozess existiert")
        .expect("Auftragsfeld schreiben");
    }

    fn feld_lesen(&self, feld_offset: u64) -> u64 {
        let mut bytes = [0u8; 8];
        scheduler::mit_prozess_raum(self.pid, |raum| {
            raum.lesen(VirtAddr::new(AUFTRAG_VA + feld_offset), &mut bytes)
        })
        .expect("Prozess existiert")
        .expect("Auftragsfeld lesen");
        u64::from_le_bytes(bytes)
    }

    /// DER AUFRUF: Auftrag hinterlegen, warten, Antwort holen.
    /// Liefert `(fehlercode, ergebnis)` — genau rax und rdx aus Ring 3.
    fn ruf(&self, nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> (u64, u64) {
        // Argumente ZUERST, die Flagge ZULETZT: Der Prozess darf nie einen
        // halb gefüllten Auftrag sehen.
        self.feld_setzen(prozess::PRUEFSTAND_NUMMER, nummer);
        self.feld_setzen(prozess::PRUEFSTAND_ARG0, a0);
        self.feld_setzen(prozess::PRUEFSTAND_ARG1, a1);
        self.feld_setzen(prozess::PRUEFSTAND_ARG2, a2);
        self.feld_setzen(prozess::PRUEFSTAND_ARG3, a3);
        self.feld_setzen(prozess::PRUEFSTAND_FEHLER, u64::MAX);
        self.feld_setzen(prozess::PRUEFSTAND_ERGEBNIS, u64::MAX);
        self.feld_setzen(prozess::PRUEFSTAND_FLAGGE, 1);

        let frist = zeit::ms_seit_boot() + AUFTRAG_FRIST_MS;
        while self.feld_lesen(prozess::PRUEFSTAND_FLAGGE) != 0 {
            assert!(
                zeit::ms_seit_boot() < frist,
                "Syscall {} hat nicht geantwortet (Prozess haengt oder ist gestorben)",
                nummer
            );
            x86_64::instructions::hlt();
        }
        (
            self.feld_lesen(prozess::PRUEFSTAND_FEHLER),
            self.feld_lesen(prozess::PRUEFSTAND_ERGEBNIS),
        )
    }

    /// Bequemer Aufruf mit weniger Argumenten.
    fn ruf4(&self, nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> (u64, u64) {
        self.ruf(nummer, a0, a1, a2, a3)
    }
    fn ruf3(&self, nummer: u64, a0: u64, a1: u64, a2: u64) -> (u64, u64) {
        self.ruf(nummer, a0, a1, a2, 0)
    }
    fn ruf1(&self, nummer: u64, a0: u64) -> (u64, u64) {
        self.ruf(nummer, a0, 0, 0, 0)
    }
    fn ruf0(&self, nummer: u64) -> (u64, u64) {
        self.ruf(nummer, 0, 0, 0, 0)
    }

    /// Erwartet Erfolg und liefert das Ergebnis.
    fn ok(&self, was: &str, ergebnis: (u64, u64)) -> u64 {
        assert_eq!(
            ergebnis.0,
            Fehler::Ok.code(),
            "{} sollte gelingen, lieferte aber Fehler {}",
            was,
            ergebnis.0
        );
        ergebnis.1
    }

    /// Erwartet genau diesen Fehler.
    fn fehler(&self, was: &str, ergebnis: (u64, u64), erwartet: Fehler) {
        assert_eq!(
            ergebnis.0,
            erwartet.code(),
            "{}: erwartet Fehler {} ({}), war {}",
            was,
            erwartet.code(),
            erwartet.meldung(),
            ergebnis.0
        );
    }

    fn beenden(self) {
        scheduler::beenden(self.pid);
        // Ein Tick Luft, damit der Scheduler wegschaltet, dann abräumen.
        let bis = zeit::ms_seit_boot() + 30;
        while zeit::ms_seit_boot() < bis {
            x86_64::instructions::hlt();
        }
        scheduler::aufraeumen();
    }
}

/// Pumpt den Netz-Stack, bis geschlossene Sockets wirklich aus der Tabelle
/// verschwunden sind — die Voraussetzung für eine stabile Leck-Messung.
/// (TIME_WAIT ist bei uns auf 2 s verkürzt, siehe docs/tcp-scope.md.)
fn netz_beruhigen() {
    let bis = zeit::ms_seit_boot() + 2500;
    while zeit::ms_seit_boot() < bis {
        speed_os::netz::pumpen();
        x86_64::instructions::hlt();
    }
}

/// Eine Adresse, die im Prozess GARANTIERT nicht gemappt ist (weit hinter
/// Code und Stack, aber im User-Bereich).
const UNGEMAPPT_VA: u64 = adressraum::USER_START + 0x40_0000;
/// Eine Kernel-Adresse (der Heap) — der klassische Angriff.
fn kernel_va() -> u64 {
    allocator::HEAP_START as u64
}

// ---------------------------------------------------------------------------
// TEIL 1: Gruppe 0 — Prozess und Ausgabe
// ---------------------------------------------------------------------------

#[test_case]
fn test_a_gruppe0_erfolg() {
    serial_println!("[SYS-TEST] Gruppe 0 (Prozess/Ausgabe) aus Ring 3:");
    let p = Pruefstand::neu();

    // getpid liefert genau unsere PID.
    let pid = p.ok("getpid", p.ruf0(sys::SYS_GETPID));
    assert_eq!(pid, p.pid as u64, "getpid liefert die falsche PID");

    // zeit_jetzt ist monoton und plausibel (der Kernel läuft schon).
    let t1 = p.ok("zeit_jetzt", p.ruf0(sys::SYS_ZEIT_JETZT));
    assert!(t1 > 0, "zeit_jetzt lieferte 0");
    let t2 = p.ok("zeit_jetzt", p.ruf0(sys::SYS_ZEIT_JETZT));
    assert!(t2 >= t1, "zeit_jetzt laeuft rueckwaerts");
    assert!(t2 < t1 + AUFTRAG_FRIST_MS, "zeit_jetzt springt");

    // zeit_epoche: echte Uhr, muss nach 2020 liegen (Sekunden seit 2000).
    let epoche = p.ok("zeit_epoche", p.ruf0(sys::SYS_ZEIT_EPOCHE));
    assert!(
        epoche > 20 * 365 * 24 * 3600,
        "zeit_epoche liefert kein plausibles Datum ({})",
        epoche
    );

    // schreibe auf den DIAGNOSE-Kanal (nur seriell) und auf die AUSGABE.
    let text = b"[SYS-TEST] Ausgabe aus Ring 3 per schreibe()\n";
    let ptr = p.hinlegen(0, text);
    let n = p.ok(
        "schreibe(Diagnose)",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, ptr, text.len() as u64),
    );
    assert_eq!(n, text.len() as u64, "schreibe hat nicht alles geschrieben");
    let n = p.ok(
        "schreibe(Ausgabe)",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_AUSGABE, ptr, text.len() as u64),
    );
    assert_eq!(n, text.len() as u64);
    // Länge 0 ist erlaubt und schreibt nichts.
    assert_eq!(
        p.ok("schreibe(0 Bytes)", p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, ptr, 0)),
        0
    );

    // yield und schlafe kehren sauber zurück (und kosten echte Zeit).
    p.ok("yield", p.ruf0(sys::SYS_YIELD));
    let vor = zeit::ms_seit_boot();
    p.ok("schlafe", p.ruf1(sys::SYS_SCHLAFE, 60));
    let gedauert = zeit::ms_seit_boot() - vor;
    assert!(gedauert >= 55, "schlafe(60) dauerte nur {} ms", gedauert);

    serial_println!("[SYS-TEST] Gruppe 0 vollstaendig: PID {}, Uhr, Ausgabe, Warten.", pid);
    p.beenden();
}

#[test_case]
fn test_b_gruppe0_boesartig() {
    serial_println!("[SYS-TEST] Gruppe 0 mit boesartigen Argumenten:");
    let p = Pruefstand::neu();
    let text = b"harmlos";
    let ptr = p.hinlegen(0, text);

    // UNBEKANNTE NUMMERN — inklusive der Lücken zwischen den Gruppen.
    // Vergeben sind inzwischen 0..13 (7..11 Serie 6 Teil 6: lese/warte/
    //  beende/pipe/starte, 12 zufall, 13 zeit_geprueft). Wer eine Nummer
    //  ergänzt, muss sie hier austragen — genau dafür ist diese Liste da,
    //  und genau so hat sie beim Hinzufügen von `zufall` UND von
    //  `zeit_geprueft` angeschlagen.
    for nummer in [14u64, 15, 25, 31, 38, 99, 239, 241, u64::MAX] {
        p.fehler("unbekannte Nummer", p.ruf0(nummer), Fehler::UnbekannterSyscall);
    }

    // DIE NEUEN SYSCALLS MIT UNSINN (Serie 6, Teil 6) — jeder muss einen
    // sauberen Fehler liefern und darf nie hängen bleiben.
    //
    // `warte` auf ein Kind, das es nicht gibt: Der Prüfstand hat KEINE
    // Kinder. Das MUSS ein Fehler sein und darf nicht blockieren — sonst
    // schliefe der Prozess für immer.
    p.fehler("warte ohne Kinder", p.ruf1(sys::SYS_WARTE, 0), Fehler::NichtGefunden);
    p.fehler("warte auf fremde PID", p.ruf1(sys::SYS_WARTE, 12345), Fehler::NichtGefunden);
    p.fehler(
        "warte mit absurder PID",
        p.ruf1(sys::SYS_WARTE, u64::MAX),
        Fehler::UngueltigesArgument,
    );
    // `beende`: Der Kernel-Prozess ist geschützt, unbekannte PIDs sind ein
    // Fehler.
    p.fehler(
        "beende den Kernel-Prozess",
        p.ruf1(sys::SYS_BEENDE, 0),
        Fehler::UngueltigesArgument,
    );
    p.fehler(
        "beende unbekannte PID",
        p.ruf1(sys::SYS_BEENDE, 12345),
        Fehler::NichtGefunden,
    );
    // `lese` auf Handles, aus denen man nicht lesen kann.
    p.fehler(
        "lese von der Ausgabe",
        p.ruf3(sys::SYS_LESE, HANDLE_AUSGABE, ptr, 8),
        Fehler::FalscherHandleTyp,
    );
    p.fehler(
        "lese von der Standard-Eingabe (keine Quelle)",
        p.ruf3(sys::SYS_LESE, HANDLE_EINGABE, ptr, 8),
        Fehler::NichtUnterstuetzt,
    );
    p.fehler(
        "lese mit Kernel-Zeiger",
        p.ruf3(sys::SYS_LESE, HANDLE_EINGABE, kernel_va(), 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "lese mit fremdem Handle",
        p.ruf3(sys::SYS_LESE, 99, ptr, 8),
        Fehler::UngueltigerHandle,
    );
    // `starte` mit unsinnigen Pfaden und Handles.
    p.fehler(
        "starte mit Kernel-Zeiger als Pfad",
        p.ruf4(sys::SYS_STARTE, kernel_va(), 8, ERBE_KEINS, ERBE_KEINS),
        Fehler::UngueltigerZeiger,
    );
    let relativ = p.hinlegen(0x40, b"kein-absoluter-pfad");
    p.fehler(
        "starte mit relativem Pfad",
        p.ruf4(sys::SYS_STARTE, relativ, 19, ERBE_KEINS, ERBE_KEINS),
        Fehler::UngueltigerPfad,
    );
    let fehlt = p.hinlegen(0x80, b"/gibt-es-ganz-sicher-nicht");
    p.fehler(
        "starte mit fehlender Datei",
        p.ruf4(sys::SYS_STARTE, fehlt, 26, ERBE_KEINS, ERBE_KEINS),
        Fehler::NichtGefunden,
    );
    p.fehler(
        "starte mit fremdem Erb-Handle",
        p.ruf4(sys::SYS_STARTE, fehlt, 26, 99, ERBE_KEINS),
        Fehler::UngueltigerHandle,
    );

    // ZEIGER-ANGRIFFE auf schreibe:
    p.fehler(
        "schreibe mit Kernel-Zeiger",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, kernel_va(), 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "schreibe mit Nullzeiger",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, 0, 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "schreibe aus ungemapptem User-Bereich",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, UNGEMAPPT_VA, 8),
        Fehler::UngueltigerZeiger,
    );
    // ÜBER DIE SEITENGRENZE: Der Anfang ist gültig, das Ende nicht.
    // (Die Code-Seite ist EINE Seite; 8 Byte vor ihrem Ende + 64 Byte laufen
    // in ungemapptes Gebiet.)
    let fast_ende = prozess::ZAEHLER_CODE_VA + 4096 - 8;
    p.fehler(
        "schreibe ueber die Seitengrenze",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, fast_ende, 64),
        Fehler::UngueltigerZeiger,
    );
    // LÄNGEN-ANGRIFFE:
    p.fehler(
        "schreibe mit riesiger Laenge",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, ptr, u64::MAX),
        Fehler::ZuGross,
    );
    p.fehler(
        "schreibe knapp ueber dem Deckel",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, ptr, 64 * 1024 + 1),
        Fehler::ZuGross,
    );

    // HANDLE-ANGRIFFE:
    p.fehler(
        "schreibe auf die Eingabe",
        p.ruf3(sys::SYS_SCHREIBE, HANDLE_EINGABE, ptr, 4),
        Fehler::NichtUnterstuetzt,
    );
    for boese in [3u64, 7, 31, 32, 1000, u64::MAX, u64::MAX - 1] {
        p.fehler(
            "schreibe auf ein nie vergebenes Handle",
            p.ruf3(sys::SYS_SCHREIBE, boese, ptr, 4),
            Fehler::UngueltigerHandle,
        );
    }
    // Die reservierten Handles gehören dem Kernel:
    for reserviert in [HANDLE_EINGABE, HANDLE_AUSGABE, HANDLE_DIAGNOSE] {
        p.fehler(
            "schliesse ein reserviertes Handle",
            p.ruf1(sys::SYS_SCHLIESSE, reserviert),
            Fehler::UngueltigesArgument,
        );
    }

    serial_println!("[SYS-TEST] Alle Angriffe auf Gruppe 0 sauber abgewiesen (keine Panik).");
    p.beenden();
}

// ---------------------------------------------------------------------------
// TEIL 2: Gruppe 1 — Dateien
// ---------------------------------------------------------------------------

#[test_case]
fn test_c_gruppe1_dateien_erfolg() {
    serial_println!("[SYS-TEST] Gruppe 1 (Dateien) aus Ring 3:");
    let p = Pruefstand::neu();

    // --- mkdir ---
    let dir = p.hinlegen(0, b"/systest");
    p.ok("mkdir", p.ruf3(sys::SYS_MKDIR, dir, 8, 0));
    // Nochmal -> existiert bereits.
    p.fehler("mkdir doppelt", p.ruf3(sys::SYS_MKDIR, dir, 8, 0), Fehler::ExistiertBereits);

    // --- oeffne mit ANLEGEN ---
    let pfad = b"/systest/hallo.txt";
    let pfad_ptr = p.hinlegen(0x100, pfad);
    let h = p.ok(
        "oeffne(anlegen)",
        p.ruf3(
            sys::SYS_OEFFNE,
            pfad_ptr,
            pfad.len() as u64,
            MODUS_LESEN | MODUS_SCHREIBEN | MODUS_ANLEGEN,
        ),
    );
    assert_eq!(h, 3, "das erste eigene Handle muss 3 sein");

    // --- schreibe_at ---
    let inhalt = b"Hallo vom User-Space! Dies schrieb ein Ring-3-Prozess.";
    let inhalt_ptr = p.hinlegen(0x200, inhalt);
    let n = p.ok(
        "schreibe_at",
        p.ruf(sys::SYS_SCHREIBE_AT, h, 0, inhalt_ptr, inhalt.len() as u64),
    );
    assert_eq!(n, inhalt.len() as u64);

    // --- lese_at: DAS ist der copy-OUT-Beweis ---
    // Der Kernel schreibt in den Speicher des PROZESSES; wir lesen ihn danach
    // aus dessen (inaktivem) Adressraum zurück.
    let ziel_ptr = p.hinlegen(0x400, &[0u8; 64]);
    let gelesen = p.ok(
        "lese_at",
        p.ruf(sys::SYS_LESE_AT, h, 0, ziel_ptr, inhalt.len() as u64),
    );
    assert_eq!(gelesen, inhalt.len() as u64);
    let mut zurueck = alloc::vec![0u8; inhalt.len()];
    p.abholen(0x400, &mut zurueck);
    assert_eq!(
        &zurueck[..],
        &inhalt[..],
        "copy-OUT: der Kernel hat nicht die richtigen Bytes in den Prozess geschrieben"
    );
    // Mit Offset lesen:
    let gelesen = p.ok("lese_at(offset)", p.ruf(sys::SYS_LESE_AT, h, 6, ziel_ptr, 3));
    assert_eq!(gelesen, 3);
    let mut drei = [0u8; 3];
    p.abholen(0x400, &mut drei);
    assert_eq!(&drei, b"vom");
    // Hinter dem Dateiende: 0 Bytes, KEIN Fehler (POSIX-Semantik).
    assert_eq!(
        p.ok("lese_at hinter dem Ende", p.ruf(sys::SYS_LESE_AT, h, 10_000, ziel_ptr, 8)),
        0
    );

    // --- stat: der zweite copy-OUT-Beweis ---
    let stat_ptr = p.hinlegen(0x600, &[0u8; core::mem::size_of::<StatDaten>()]);
    p.ok("stat", p.ruf3(sys::SYS_STAT, pfad_ptr, pfad.len() as u64, stat_ptr));
    let mut stat_bytes = [0u8; core::mem::size_of::<StatDaten>()];
    p.abholen(0x600, &mut stat_bytes);
    let typ = u64::from_le_bytes(stat_bytes[0..8].try_into().unwrap());
    let groesse = u64::from_le_bytes(stat_bytes[8..16].try_into().unwrap());
    assert_eq!(typ, 0, "stat: Datei muss Typ 0 sein");
    assert_eq!(groesse, inhalt.len() as u64, "stat: falsche Groesse");
    // Und für das Verzeichnis:
    p.ok("stat(Verzeichnis)", p.ruf3(sys::SYS_STAT, dir, 8, stat_ptr));
    p.abholen(0x600, &mut stat_bytes);
    assert_eq!(
        u64::from_le_bytes(stat_bytes[0..8].try_into().unwrap()),
        1,
        "stat: Verzeichnis muss Typ 1 sein"
    );

    // --- liste ---
    let liste_ptr = p.hinlegen(0x700, &[0u8; 4 * DIR_EINTRAG_GROESSE]);
    let anzahl = p.ok(
        "liste",
        p.ruf(
            sys::SYS_LISTE,
            dir,
            8,
            liste_ptr,
            (4 * DIR_EINTRAG_GROESSE) as u64,
        ),
    );
    assert_eq!(anzahl, 1, "das Verzeichnis muss genau einen Eintrag haben");
    let mut eintrag = [0u8; DIR_EINTRAG_GROESSE];
    p.abholen(0x700, &mut eintrag);
    let name_laenge = u64::from_le_bytes(eintrag[16..24].try_into().unwrap()) as usize;
    let name = String::from_utf8_lossy(&eintrag[24..24 + name_laenge]);
    assert_eq!(name, "hallo.txt", "liste: falscher Name");
    assert_eq!(
        u64::from_le_bytes(eintrag[8..16].try_into().unwrap()),
        inhalt.len() as u64,
        "liste: falsche Groesse"
    );
    // ZU KLEINER PUFFER: Es wird nur geschrieben, was passt — die Rückgabe
    // nennt trotzdem die Gesamtzahl.
    let anzahl = p.ok("liste(Puffer zu klein)", p.ruf(sys::SYS_LISTE, dir, 8, liste_ptr, 0));
    assert_eq!(anzahl, 1, "liste muss auch mit 0-Puffer die Gesamtzahl melden");

    // --- umbenenne + schliesse + loesche ---
    let neu = b"/systest/umbenannt.txt";
    let neu_ptr = p.hinlegen(0x800, neu);
    p.ok(
        "umbenenne",
        p.ruf(
            sys::SYS_UMBENENNE,
            pfad_ptr,
            pfad.len() as u64,
            neu_ptr,
            neu.len() as u64,
        ),
    );
    // Das ALTE Handle zeigt jetzt ins Leere — die ehrliche Folge unseres
    // pfadbasierten VFS (steht so in docs/syscalls.md §5).
    p.fehler(
        "lese_at nach dem Umbenennen",
        p.ruf(sys::SYS_LESE_AT, h, 0, ziel_ptr, 8),
        Fehler::NichtGefunden,
    );
    p.ok("schliesse", p.ruf1(sys::SYS_SCHLIESSE, h));
    // Danach ist dasselbe Handle ungültig:
    p.fehler(
        "schliesse doppelt",
        p.ruf1(sys::SYS_SCHLIESSE, h),
        Fehler::UngueltigerHandle,
    );
    p.fehler(
        "lese_at auf geschlossenem Handle",
        p.ruf(sys::SYS_LESE_AT, h, 0, ziel_ptr, 8),
        Fehler::UngueltigerHandle,
    );

    p.ok("loesche", p.ruf3(sys::SYS_LOESCHE, neu_ptr, neu.len() as u64, 0));
    p.fehler(
        "loesche nochmal",
        p.ruf3(sys::SYS_LOESCHE, neu_ptr, neu.len() as u64, 0),
        Fehler::NichtGefunden,
    );
    p.ok("loesche(Verzeichnis)", p.ruf3(sys::SYS_LOESCHE, dir, 8, 0));

    serial_println!("[SYS-TEST] Gruppe 1 vollstaendig — inklusive copy-OUT von lese_at und stat.");
    p.beenden();
}

#[test_case]
fn test_d_gruppe1_boesartig() {
    serial_println!("[SYS-TEST] Gruppe 1 mit boesartigen Argumenten:");
    let p = Pruefstand::neu();

    // --- PFAD-ANGRIFFE ---
    p.fehler(
        "oeffne mit Kernel-Zeiger als Pfad",
        p.ruf3(sys::SYS_OEFFNE, kernel_va(), 8, MODUS_LESEN),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "oeffne mit Nullzeiger als Pfad",
        p.ruf3(sys::SYS_OEFFNE, 0, 8, MODUS_LESEN),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "oeffne mit ungemapptem Pfad-Zeiger",
        p.ruf3(sys::SYS_OEFFNE, UNGEMAPPT_VA, 8, MODUS_LESEN),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "oeffne mit Pfad-Laenge 0",
        p.ruf3(sys::SYS_OEFFNE, p.va(0), 0, MODUS_LESEN),
        Fehler::UngueltigerPfad,
    );
    p.fehler(
        "oeffne mit zu langem Pfad",
        p.ruf3(sys::SYS_OEFFNE, p.va(0), 256, MODUS_LESEN),
        Fehler::ZuGross,
    );
    p.fehler(
        "oeffne mit u64::MAX als Pfad-Laenge",
        p.ruf3(sys::SYS_OEFFNE, p.va(0), u64::MAX, MODUS_LESEN),
        Fehler::ZuGross,
    );
    // RELATIVER Pfad (es gibt kein Arbeitsverzeichnis):
    let relativ = p.hinlegen(0, b"relativ.txt");
    p.fehler(
        "oeffne mit relativem Pfad",
        p.ruf3(sys::SYS_OEFFNE, relativ, 11, MODUS_LESEN),
        Fehler::UngueltigerPfad,
    );
    // KEIN UTF-8 (rohe 0xFF-Bytes):
    let kaputt = p.hinlegen(0x40, &[b'/', 0xFF, 0xFE, 0xFD]);
    p.fehler(
        "oeffne mit kaputtem UTF-8",
        p.ruf3(sys::SYS_OEFFNE, kaputt, 4, MODUS_LESEN),
        Fehler::UngueltigerPfad,
    );
    // Pfad, dessen Länge über die Seitengrenze hinausreicht:
    p.fehler(
        "Pfad ueber die Seitengrenze",
        p.ruf3(sys::SYS_OEFFNE, prozess::ZAEHLER_CODE_VA + 4090, 32, MODUS_LESEN),
        Fehler::UngueltigerZeiger,
    );
    // Nicht vorhandene Datei OHNE Anlegen-Bit:
    let fehlt = p.hinlegen(0x80, b"/gibt-es-nicht.txt");
    p.fehler(
        "oeffne ohne Anlegen",
        p.ruf3(sys::SYS_OEFFNE, fehlt, 18, MODUS_LESEN),
        Fehler::NichtGefunden,
    );
    // Ein VERZEICHNIS als Datei öffnen:
    let wurzel = p.hinlegen(0xC0, b"/");
    p.fehler(
        "oeffne ein Verzeichnis",
        p.ruf3(sys::SYS_OEFFNE, wurzel, 1, MODUS_LESEN),
        Fehler::KeineDatei,
    );
    // MODUS-ANGRIFFE:
    let gueltig = p.hinlegen(0x100, b"/systest-boese.txt");
    for modus in [0u64, 16, 32, u64::MAX, MODUS_LESEN | MODUS_ANLEGEN] {
        p.fehler(
            "oeffne mit ungueltigem Modus",
            p.ruf3(sys::SYS_OEFFNE, gueltig, 18, modus),
            Fehler::UngueltigesArgument,
        );
    }

    // --- Eine echte Datei für die Handle-Angriffe ---
    let h = p.ok(
        "oeffne(nur lesen, anlegen ueber Schreibrecht)",
        p.ruf3(
            sys::SYS_OEFFNE,
            gueltig,
            18,
            MODUS_SCHREIBEN | MODUS_ANLEGEN | MODUS_ABSCHNEIDEN,
        ),
    );
    // Ein NUR-SCHREIBEN-Handle darf nicht lesen ...
    let ziel = p.hinlegen(0x400, &[0u8; 32]);
    p.fehler(
        "lese_at auf einem Nur-Schreiben-Handle",
        p.ruf(sys::SYS_LESE_AT, h, 0, ziel, 8),
        Fehler::NichtUnterstuetzt,
    );
    // ... aber schreiben schon.
    let daten = p.hinlegen(0x440, b"abc");
    assert_eq!(p.ok("schreibe_at", p.ruf(sys::SYS_SCHREIBE_AT, h, 0, daten, 3)), 3);

    // Ein NUR-LESEN-Handle darf nicht schreiben.
    let h_ro = p.ok(
        "oeffne(nur lesen)",
        p.ruf3(sys::SYS_OEFFNE, gueltig, 18, MODUS_LESEN),
    );
    p.fehler(
        "schreibe_at auf einem Nur-Lesen-Handle",
        p.ruf(sys::SYS_SCHREIBE_AT, h_ro, 0, daten, 3),
        Fehler::NurLesen,
    );
    p.fehler(
        "schreibe auf einem Nur-Lesen-Handle",
        p.ruf3(sys::SYS_SCHREIBE, h_ro, daten, 3),
        Fehler::NurLesen,
    );

    // --- ZEIGER- UND LÄNGEN-ANGRIFFE auf lese_at / schreibe_at ---
    p.fehler(
        "lese_at mit Kernel-Ziel",
        p.ruf(sys::SYS_LESE_AT, h_ro, 0, kernel_va(), 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "lese_at mit ungemapptem Ziel",
        p.ruf(sys::SYS_LESE_AT, h_ro, 0, UNGEMAPPT_VA, 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "lese_at mit riesiger Laenge",
        p.ruf(sys::SYS_LESE_AT, h_ro, 0, ziel, u64::MAX),
        Fehler::ZuGross,
    );
    p.fehler(
        "lese_at mit absurdem Offset",
        p.ruf(sys::SYS_LESE_AT, h_ro, u64::MAX, ziel, 8),
        Fehler::ZuGross,
    );
    p.fehler(
        "schreibe_at mit Kernel-Quelle",
        p.ruf(sys::SYS_SCHREIBE_AT, h, 0, kernel_va(), 8),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "stat mit Kernel-Ziel",
        p.ruf3(sys::SYS_STAT, gueltig, 18, kernel_va()),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "liste mit Kernel-Ziel",
        p.ruf(sys::SYS_LISTE, wurzel, 1, kernel_va(), 128),
        Fehler::UngueltigerZeiger,
    );
    p.fehler(
        "liste mit riesiger Ziel-Laenge",
        p.ruf(sys::SYS_LISTE, wurzel, 1, ziel, u64::MAX),
        Fehler::ZuGross,
    );

    // --- FALSCHER HANDLE-TYP: ein Socket ist kein Datei-Handle ---
    let sock = p.ok("socket", p.ruf1(sys::SYS_SOCKET, TYP_UDP));
    p.fehler(
        "lese_at auf einem Socket-Handle",
        p.ruf(sys::SYS_LESE_AT, sock, 0, ziel, 8),
        Fehler::FalscherHandleTyp,
    );
    p.fehler(
        "stat-Ergebnis auf einem Socket-Handle",
        p.ruf(sys::SYS_SCHREIBE_AT, sock, 0, daten, 3),
        Fehler::FalscherHandleTyp,
    );
    // ... und umgekehrt: eine Datei ist kein Socket.
    p.fehler(
        "sende auf einem Datei-Handle",
        p.ruf3(sys::SYS_SENDE, h, daten, 3),
        Fehler::FalscherHandleTyp,
    );

    // Aufräumen und Spuren beseitigen.
    p.ok("schliesse", p.ruf1(sys::SYS_SCHLIESSE, h));
    p.ok("schliesse", p.ruf1(sys::SYS_SCHLIESSE, h_ro));
    p.ok("schliesse", p.ruf1(sys::SYS_SCHLIESSE, sock));
    p.ok("loesche", p.ruf3(sys::SYS_LOESCHE, gueltig, 18, 0));

    serial_println!("[SYS-TEST] Alle Angriffe auf Gruppe 1 sauber abgewiesen.");
    p.beenden();
}

// ---------------------------------------------------------------------------
// TEIL 3: Gruppe 2 — Netz
// ---------------------------------------------------------------------------

#[test_case]
fn test_e_gruppe2_netz() {
    serial_println!("[SYS-TEST] Gruppe 2 (Netz) aus Ring 3:");
    let p = Pruefstand::neu();

    // --- socket ---
    let tcp = p.ok("socket(TCP)", p.ruf1(sys::SYS_SOCKET, TYP_TCP));
    let udp = p.ok("socket(UDP)", p.ruf1(sys::SYS_SOCKET, TYP_UDP));
    assert_ne!(tcp, udp, "zwei Sockets muessen verschiedene Handles haben");
    for typ in [2u64, 99, u64::MAX] {
        p.fehler(
            "socket mit unbekanntem Typ",
            p.ruf1(sys::SYS_SOCKET, typ),
            Fehler::UngueltigesArgument,
        );
    }

    // --- socket_zustand: frisch = Neu ---
    assert_eq!(
        p.ok("socket_zustand", p.ruf1(sys::SYS_SOCKET_ZUSTAND, tcp)),
        ZUSTAND_NEU
    );

    // --- Argument-Angriffe auf verbinde ---
    p.fehler(
        "verbinde mit Port 0",
        p.ruf3(sys::SYS_VERBINDE, tcp, 0x0A00_0202, 0),
        Fehler::UngueltigesArgument,
    );
    p.fehler(
        "verbinde mit Port > 65535",
        p.ruf3(sys::SYS_VERBINDE, tcp, 0x0A00_0202, 70_000),
        Fehler::UngueltigesArgument,
    );
    p.fehler(
        "verbinde mit IP ueber 32 Bit",
        p.ruf3(sys::SYS_VERBINDE, tcp, u64::MAX, 80),
        Fehler::UngueltigesArgument,
    );

    // --- sende/empfange auf einem UNVERBUNDENEN TCP-Socket ---
    let daten = p.hinlegen(0, b"testdaten");
    p.fehler(
        "sende ohne Verbindung",
        p.ruf3(sys::SYS_SENDE, tcp, daten, 9),
        Fehler::NichtVerbunden,
    );

    // --- aufloesen: eine IP-Literal braucht kein DNS (reiner Parser-Pfad) ---
    let ip_text = p.hinlegen(0x100, b"10.0.2.2");
    let ip = p.ok("aufloesen(IP-Literal)", p.ruf3(sys::SYS_AUFLOESEN, ip_text, 8, 0));
    assert_eq!(ip, 0x0A00_0202, "aufloesen hat die IP falsch umgerechnet");
    // Angriffe auf aufloesen:
    p.fehler(
        "aufloesen mit Laenge 0",
        p.ruf3(sys::SYS_AUFLOESEN, ip_text, 0, 0),
        Fehler::UngueltigesArgument,
    );
    p.fehler(
        "aufloesen mit zu langem Namen",
        p.ruf3(sys::SYS_AUFLOESEN, ip_text, 256, 0),
        Fehler::ZuGross,
    );
    p.fehler(
        "aufloesen mit Kernel-Zeiger",
        p.ruf3(sys::SYS_AUFLOESEN, kernel_va(), 8, 0),
        Fehler::UngueltigerZeiger,
    );

    // --- DER ECHTE WEG: UDP verbinden und senden ---
    // UDP braucht keinen Handshake (verbinde merkt sich nur das Ziel), und
    // Port 9 ist der Discard-Port — es muss niemand antworten. Damit läuft ein
    // Ring-3-Programm einmal komplett durch: Socket, Ziel, Senden über
    // IPv4/ARP/Ethernet bis auf die virtio-Karte.
    let zustand = p.ok(
        "verbinde(UDP)",
        p.ruf3(sys::SYS_VERBINDE, udp, 0x0A00_0202, 9),
    );
    assert_eq!(zustand, 2, "verbinde(UDP) muss 'verbunden' melden");
    let gesendet = p.ok("sende(UDP)", p.ruf3(sys::SYS_SENDE, udp, daten, 9));
    assert_eq!(gesendet, 9, "sende(UDP) hat nicht alles uebernommen");
    // empfangen: nicht-blockierend, also 0 (niemand antwortet auf Port 9).
    let ziel = p.hinlegen(0x200, &[0u8; 64]);
    assert_eq!(
        p.ok("empfange(UDP)", p.ruf3(sys::SYS_EMPFANGE, udp, ziel, 64)),
        0,
        "empfange muss 0 liefern, wenn nichts da ist"
    );
    // Zeiger-Angriff auf empfange (das ZIEL wird VOR dem Empfangen geprüft):
    p.fehler(
        "empfange mit Kernel-Ziel",
        p.ruf3(sys::SYS_EMPFANGE, udp, kernel_va(), 64),
        Fehler::UngueltigerZeiger,
    );

    p.ok("schliesse(TCP)", p.ruf1(sys::SYS_SCHLIESSE, tcp));
    p.ok("schliesse(UDP)", p.ruf1(sys::SYS_SCHLIESSE, udp));
    serial_println!("[SYS-TEST] Gruppe 2: Socket, verbinde, sende, empfange, aufloesen — echt.");
    p.beenden();
}

// ---------------------------------------------------------------------------
// TEIL 4: Handle-ISOLATION zwischen Prozessen
// ---------------------------------------------------------------------------

/// DIE LÜCKE AUS DER BESTANDSAUFNAHME (b), geschlossen und geprüft: Zwei
/// Prozesse vergeben dieselben kleinen Zahlen, aber KEINER kann die Objekte des
/// anderen erreichen. Der Test läuft aus Ring 3 — es ist ein echter Angriff.
#[test_case]
fn test_f_handles_sind_prozess_lokal() {
    serial_println!("[SYS-TEST] Handle-Isolation zwischen zwei Prozessen:");
    let a = Pruefstand::neu();
    let b = Pruefstand::neu();

    // Prozess A legt eine Datei an und behält ihr Handle.
    let pfad = b"/geheim-von-a.txt";
    let pfad_ptr = a.hinlegen(0, pfad);
    let h_a = a.ok(
        "A: oeffne",
        a.ruf3(
            sys::SYS_OEFFNE,
            pfad_ptr,
            pfad.len() as u64,
            MODUS_LESEN | MODUS_SCHREIBEN | MODUS_ANLEGEN,
        ),
    );
    let inhalt = a.hinlegen(0x100, b"GEHEIMNIS");
    a.ok("A: schreibe_at", a.ruf(sys::SYS_SCHREIBE_AT, h_a, 0, inhalt, 9));

    // Prozess B öffnet einen Socket — und bekommt DIESELBE Handle-Zahl.
    let h_b = b.ok("B: socket", b.ruf1(sys::SYS_SOCKET, TYP_UDP));
    assert_eq!(h_a, h_b, "beide Prozesse muessen bei 3 anfangen");

    // B versucht, mit SEINER Zahl 3 die Datei von A zu lesen. Bei B ist 3 ein
    // Socket — also FalscherHandleTyp. Auf A's Datei kommt B damit nicht.
    let ziel = b.hinlegen(0x200, &[0u8; 32]);
    b.fehler(
        "B liest mit Handle 3 (bei B ein Socket)",
        b.ruf(sys::SYS_LESE_AT, h_b, 0, ziel, 9),
        Fehler::FalscherHandleTyp,
    );
    // Und B findet A's Datei-Handle unter KEINER Zahl:
    for zahl in 4..32u64 {
        b.fehler(
            "B probiert alle Handle-Zahlen durch",
            b.ruf(sys::SYS_LESE_AT, zahl, 0, ziel, 9),
            Fehler::UngueltigerHandle,
        );
    }
    // Gegenprobe: A selbst kann seine Datei lesen (das Handle ist gültig —
    // nur eben ausschliesslich in A).
    let a_ziel = a.hinlegen(0x200, &[0u8; 32]);
    assert_eq!(a.ok("A: lese_at", a.ruf(sys::SYS_LESE_AT, h_a, 0, a_ziel, 9)), 9);
    let mut gelesen = [0u8; 9];
    a.abholen(0x200, &mut gelesen);
    assert_eq!(&gelesen, b"GEHEIMNIS");

    // Und: B kann A's SOCKET-Handle nicht schliessen (es gibt es bei B nicht).
    b.fehler(
        "B schliesst ein fremdes Handle",
        b.ruf1(sys::SYS_SCHLIESSE, 4),
        Fehler::UngueltigerHandle,
    );

    a.ok("A: loesche", a.ruf3(sys::SYS_LOESCHE, pfad_ptr, pfad.len() as u64, 0));
    serial_println!("[SYS-TEST] Handle-Isolation bewiesen: gleiche Zahl, fremdes Objekt bleibt fremd.");
    a.beenden();
    b.beenden();
}

// ---------------------------------------------------------------------------
// TEIL 5: DER LECK-TEST über das Prozess-Ende
// ---------------------------------------------------------------------------

/// Ein Prozess öffnet Sockets und stirbt, ohne sie zu schliessen. Erwartung:
/// Der Kernel schliesst sie AUTOMATISCH (`Drop for HandleTabelle`) — sonst
/// blieben Kernel-Objekte ohne Besitzer liegen, würden Retransmits senden und
/// irgendwann die Socket-Tabelle füllen.
///
/// Geprüft wird beides: nach dem regulären `exit` UND nach einem ABSTURZ.
#[test_case]
fn test_g_handle_leck_ueber_prozess_ende() {
    use speed_os::netz::socket;

    serial_println!("[SYS-TEST] Handle-Leck-Test ueber das Prozess-Ende:");
    // STABILE AUSGANGSMESSUNG: `socket::anzahl()` zählt die ROHE Tabelle, und
    // geschlossene Sockets werden erst beim nächsten `aufraeumen` entfernt (das
    // in `oeffnen`/`bedienen` steckt). Ohne diesen Vorlauf zählten Sockets aus
    // früheren Tests mit, und der Leck-Test würde falsch anschlagen.
    netz_beruhigen();
    let (frames_vorher, _) = memory::frame_statistik();
    let sockets_vorher = socket::anzahl();

    // --- (1) Prozess beendet sich SELBST per exit, ohne zu schliessen ---
    let p = Pruefstand::neu();
    let mut offen = 0usize;
    for _ in 0..5 {
        let (fehler, handle) = p.ruf1(sys::SYS_SOCKET, TYP_UDP);
        assert_eq!(fehler, Fehler::Ok.code(), "socket sollte gelingen");
        assert!(handle >= 3);
        offen += 1;
    }
    let (_, sockets_im_prozess) = scheduler::handle_anzahl(p.pid).expect("Prozess");
    assert_eq!(sockets_im_prozess, offen, "die Tabelle fuehrt nicht alle Sockets");
    assert_eq!(
        socket::anzahl(),
        sockets_vorher + offen,
        "die Kernel-Socket-Tabelle muss gewachsen sein (vorher {}, jetzt {}, offen {})",
        sockets_vorher,
        socket::anzahl(),
        offen
    );

    // exit — ohne ein einziges schliesse.
    let pid = p.pid;
    p.feld_setzen(prozess::PRUEFSTAND_NUMMER, sys::SYS_EXIT);
    p.feld_setzen(prozess::PRUEFSTAND_ARG0, 0);
    p.feld_setzen(prozess::PRUEFSTAND_FLAGGE, 1);
    // Der Prozess kehrt nie zurück; warten, bis er beendet ist.
    let frist = zeit::ms_seit_boot() + 2000;
    loop {
        let lebt = scheduler::momentaufnahme().iter().any(|z| {
            z.pid == pid && z.zustand != speed_os::prozess::Zustand::Beendet
        });
        if !lebt {
            break;
        }
        assert!(zeit::ms_seit_boot() < frist, "der Prozess hat exit nicht ausgefuehrt");
        x86_64::instructions::hlt();
    }
    // Abräumen -> hier greift Drop for HandleTabelle.
    assert_eq!(scheduler::aufraeumen(), 1, "genau ein Prozess muss abgeraeumt werden");
    // MESSDISZIPLIN: `socket::schliessen` MARKIERT nur (`freigegeben`);
    // aus der Tabelle fliegen die Einträge erst beim nächsten `aufraeumen`,
    // das in `oeffnen`/`bedienen` steckt. Ohne dieses Pumpen würde der Test
    // die geschlossenen Sockets noch zählen und ein Leck MELDEN, wo keins ist.
    netz_beruhigen();
    assert_eq!(
        socket::anzahl(),
        sockets_vorher,
        "SOCKET-LECK nach exit: {} Sockets sind uebrig (erwartet {})",
        socket::anzahl(),
        sockets_vorher
    );
    serial_println!("[SYS-TEST]   nach exit: {} offene Sockets automatisch geschlossen.", offen);

    // --- (2) Prozess STÜRZT AB (Page Fault), ohne zu schliessen ---
    let p = Pruefstand::neu();
    for _ in 0..3 {
        p.ok("socket", p.ruf1(sys::SYS_SOCKET, TYP_UDP));
    }
    assert_eq!(socket::anzahl(), sockets_vorher + 3);
    // Einen Absturz auslösen: Wir biegen den Prüfstand auf eine ungültige
    // Adresse um, indem wir ihm einen Auftrag mit einer Syscall-Nummer geben,
    // die es gibt — und danach seinen Code zerstören. Einfacher und ehrlicher:
    // wir lassen den Kernel den Prozess töten (`beenden`), was denselben
    // Aufräum-Pfad nimmt wie ein Absturz (Zustand::Beendet -> aufraeumen).
    scheduler::beenden(p.pid);
    let bis = zeit::ms_seit_boot() + 50;
    while zeit::ms_seit_boot() < bis {
        x86_64::instructions::hlt();
    }
    assert_eq!(scheduler::aufraeumen(), 1);
    netz_beruhigen();
    assert_eq!(
        socket::anzahl(),
        sockets_vorher,
        "SOCKET-LECK nach dem Abschuss des Prozesses ({} uebrig, erwartet {})",
        socket::anzahl(),
        sockets_vorher
    );
    // (`Pruefstand` hält nur eine PID und hat kein Drop — der Prozess selbst
    //  ist oben schon abgeraeumt, `p.beenden()` wäre also doppelt.)

    // --- (3) Und die Frame-Bilanz stimmt byte-exakt ---
    let (frames_nachher, _) = memory::frame_statistik();
    assert_eq!(
        frames_vorher, frames_nachher,
        "Frame-Leck ueber die Prozess-Enden (vorher {} frei, nachher {})",
        frames_vorher, frames_nachher
    );
    serial_println!("[SYS-TEST] Kein Handle- und kein Frame-Leck ueber das Prozess-Ende.");
}

// ---------------------------------------------------------------------------
// TEIL 6: Ein echter ABSTURZ mitten im Syscall-Betrieb
// ---------------------------------------------------------------------------

/// Der Prüfstand bekommt einen Auftrag, dann wird sein CODE unbrauchbar
/// gemacht (mit 0-Bytes überschrieben) — der nächste Befehl aus Ring 3 löst
/// einen Fault aus. Erwartung: GENAU dieser Prozess stirbt, seine Handles
/// werden geschlossen, und der Kernel plus alle anderen Prozesse laufen weiter.
#[test_case]
fn test_h_absturz_mit_offenen_handles() {
    use speed_os::netz::socket;

    serial_println!("[SYS-TEST] Absturz mit offenen Handles (ein Page Fault ist erwartet):");
    netz_beruhigen();
    let sockets_vorher = socket::anzahl();
    let (frames_vorher, _) = memory::frame_statistik();

    let opfer = Pruefstand::neu();
    let zeuge = Pruefstand::neu();
    opfer.ok("socket", opfer.ruf1(sys::SYS_SOCKET, TYP_UDP));
    opfer.ok("socket", opfer.ruf1(sys::SYS_SOCKET, TYP_TCP));
    assert_eq!(socket::anzahl(), sockets_vorher + 2);

    // Den Code des Opfers zerstören: Statt gültiger Befehle stehen dort jetzt
    // Bytes, die auf die Adresse 0 zugreifen (`mov al, [rax]` mit rax = 0 ist
    // hier nicht garantiert — 0x00 0x00 dekodiert als `add [rax], al`, und rax
    // ist beim Prüfstand irgendein Syscall-Ergebnis; sicherer ist ein
    // ausdrücklicher Zugriff auf eine Kernel-Adresse).
    let absturz_code = prozess::absturz_programm(allocator::HEAP_START as u64);
    scheduler::mit_prozess_raum(opfer.pid, |raum| {
        raum.schreiben(VirtAddr::new(prozess::ZAEHLER_CODE_VA), &absturz_code)
    })
    .expect("Prozess")
    .expect("Code ueberschreiben");

    // Warten, bis der Prozess gestorben ist (der Fault kommt, sobald er
    // wieder eine Zeitscheibe bekommt).
    let pid = opfer.pid;
    let frist = zeit::ms_seit_boot() + 2000;
    loop {
        let lebt = scheduler::momentaufnahme()
            .iter()
            .any(|z| z.pid == pid && z.zustand != speed_os::prozess::Zustand::Beendet);
        if !lebt {
            break;
        }
        assert!(zeit::ms_seit_boot() < frist, "der Prozess ist nicht abgestuerzt");
        x86_64::instructions::hlt();
    }
    // (Das Opfer ist oben schon abgeraeumt — kein `beenden()` mehr.)
    assert_eq!(scheduler::aufraeumen(), 1, "der Abgestuerzte muss abgeraeumt werden");
    netz_beruhigen();
    assert_eq!(
        socket::anzahl(),
        sockets_vorher,
        "die Handles des ABGESTUERZTEN Prozesses wurden nicht geschlossen          ({} uebrig, erwartet {})",
        socket::anzahl(),
        sockets_vorher
    );

    // DER ZEUGE lebt und kann weiter Syscalls machen — der Kernel ist intakt.
    let pid_zeuge = zeuge.ok("Zeuge: getpid", zeuge.ruf0(sys::SYS_GETPID));
    assert_eq!(pid_zeuge, zeuge.pid as u64);
    let text = zeuge.hinlegen(0, b"[SYS-TEST] Der Zeuge lebt.\n");
    zeuge.ok(
        "Zeuge: schreibe",
        zeuge.ruf3(sys::SYS_SCHREIBE, HANDLE_DIAGNOSE, text, 26),
    );
    serial_println!(
        "[SYS-MEILENSTEIN] Ein Prozess ist mit offenen Handles abgestuerzt — \
         Handles geschlossen, Kernel und Nachbarprozess unbeschaedigt."
    );
    zeuge.beenden();

    let (frames_nachher, _) = memory::frame_statistik();
    assert_eq!(frames_vorher, frames_nachher, "Frame-Leck nach dem Absturz");
}
