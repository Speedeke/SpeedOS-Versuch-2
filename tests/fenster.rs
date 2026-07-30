// tests/fenster.rs — DIE FENSTER-SYSCALLS UNTER FEUER (Serie 8, Teil 1)
//
// Derselbe Prüfstand wie in tests/syscalls.rs: ein winziges Ring-3-Programm
// als FERNBEDIENUNG. Der Test schreibt Syscall-Nummer und Argumente in
// seinen Speicher, das Programm löst `int 0x80` aus und legt rax/rdx
// zurück. Dadurch ist jeder Testfall gewöhnlicher Rust-Code, während der
// Aufruf ECHT unprivilegiert ist — eigener Adressraum, eigene
// Handle-Tabelle, echte dreistufige Zeigerprüfung.
//
// GEPRÜFT WIRD:
//   * der Erfolgsfall: öffnen, zeichnen, die Pixel kommen wirklich an,
//   * BÖSE ZEIGER: Kernel-Adresse, Nullzeiger, ungemappt, Überlauf,
//   * BÖSE RECHTECKE: teilweise draussen (geklemmt), ganz draussen
//     (abgelehnt), Länge passt nicht zum Rechteck — und in JEDEM dieser
//     Fälle: nebenan darf sich NICHTS geändert haben,
//   * ein FREMDES Fenster-Handle (der zweite Prozess probiert alle 32
//     Zahlen durch),
//   * die volle EREIGNIS-QUEUE (Schliessen und Groesse überleben),
//   * das blockierende `fenster_ereignis` samt Frist und sofortigem Wecken,
//   * das PROZESS-ENDE: Fenster weg, Puffer weg, Frame-Bilanz null.
//
// Der Fenster-Manager läuft hier OHNE Desktop-Modus
// (`manager_fuer_test_starten`) — sonst würde `print!` in ein
// Terminal-Fenster umgeleitet und der Test verlöre seine eigene Ausgabe.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::fenster::{self, prozessfenster, FensterId};
use speed_os::prozess::{self, Pid};
use speed_os::syscall::fenster as sysfenster;
use speed_os::syscall::{self as sys, Fehler};
use speed_os::{adressraum, allocator, memory, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    // Der Framebuffer muss HERAUSGENOMMEN werden, bevor die BootInfo zu
    // &'static wird (Borrow-Konflikt) — genau wie in main.rs.
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // Fenster-Puffer sind gross: 512 Seiten reichen für die kleinen
    // Testfenster reichlich.
    allocator::heap_erweitern(512).expect("Heap-Erweiterung fehlgeschlagen");
    speed_os::fs::init();
    scheduler::init();

    // Der Framebuffer bestimmt die Bildschirmgrösse des Managers — also
    // erst jetzt (er braucht den Heap für seinen Back-Buffer).
    if let Some(fb) = framebuffer {
        speed_os::framebuffer::init(fb);
    }
    assert!(
        fenster::manager_fuer_test_starten(),
        "ohne Framebuffer gibt es hier nichts zu testen"
    );

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Der Prüfstand (wie tests/syscalls.rs)
// ---------------------------------------------------------------------------

const AUFTRAG_FRIST_MS: u64 = 20_000;
const AUFTRAG_VA: u64 = prozess::ZAEHLER_CODE_VA + prozess::PRUEFSTAND_AUFTRAG_OFFSET;
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

    fn va(&self, offset: u64) -> u64 {
        assert!(offset < prozess::PRUEFSTAND_PUFFER_GROESSE);
        PUFFER_VA + offset
    }

    fn hinlegen(&self, offset: u64, daten: &[u8]) -> u64 {
        let va = self.va(offset);
        scheduler::mit_prozess_raum(self.pid, |raum| raum.schreiben(VirtAddr::new(va), daten))
            .expect("Prozess existiert")
            .expect("in den Prozess schreiben");
        va
    }

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

    fn ruf(&self, nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> (u64, u64) {
        self.auftrag_stellen(nummer, a0, a1, a2, a3);
        self.antwort_abwarten(nummer, AUFTRAG_FRIST_MS)
            .expect("Syscall hat nicht geantwortet")
    }

    /// Trägt einen Auftrag ein, OHNE auf die Antwort zu warten — für den
    /// Test des blockierenden `fenster_ereignis`.
    fn auftrag_stellen(&self, nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) {
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
    }

    /// Ist die Antwort schon da?
    fn fertig(&self) -> bool {
        self.feld_lesen(prozess::PRUEFSTAND_FLAGGE) == 0
    }

    fn antwort_abwarten(&self, nummer: u64, frist_ms: u64) -> Option<(u64, u64)> {
        let frist = zeit::ms_seit_boot() + frist_ms;
        while !self.fertig() {
            if zeit::ms_seit_boot() >= frist {
                serial_println!("[test] Syscall {} hat nicht geantwortet", nummer);
                return None;
            }
            // `zeit::warte_auf_interrupt` statt eines nackten `hlt`: Seit
            // Serie 7, Teil 0 ist genau das der RESCHEDULE-PUNKT im
            // Kernel-Kontext. Mit blossem `hlt` würde hier nicht die
            // Weck-Latenz gemessen, sondern die Zeitscheibe von PID 0 —
            // dieselbe Messfalle wie beim Kontext-Wechsel in Serie 6.
            zeit::warte_auf_interrupt();
        }
        Some((
            self.feld_lesen(prozess::PRUEFSTAND_FEHLER),
            self.feld_lesen(prozess::PRUEFSTAND_ERGEBNIS),
        ))
    }

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

    /// Öffnet ein Fenster und liefert (Handle, FensterId).
    fn fenster_oeffnen(&self, titel: &str, breite: u64, hoehe: u64) -> (u64, FensterId) {
        let vorher = fenster::prozess_fenster_anzahl();
        let titel_va = self.hinlegen(0, titel.as_bytes());
        let handle = self.ok(
            "fenster_oeffnen",
            self.ruf(
                sys::SYS_FENSTER_OEFFNEN,
                titel_va,
                titel.len() as u64,
                breite,
                hoehe,
            ),
        );
        assert_eq!(
            fenster::prozess_fenster_anzahl(),
            vorher + 1,
            "es haette genau ein Fenster dazukommen muessen"
        );
        (handle, juengste_fenster_id())
    }

    fn beenden(self) {
        scheduler::beenden(self.pid);
        let bis = zeit::ms_seit_boot() + 60;
        while zeit::ms_seit_boot() < bis {
            x86_64::instructions::hlt();
        }
        scheduler::aufraeumen();
    }
}

/// Die FensterId des zuletzt geöffneten Fensters.
///
/// Der Prozess bekommt nur ein HANDLE (eine kleine Zahl in seiner eigenen
/// Tabelle) — die FensterId bleibt im Kernel. Der Test braucht sie, um von
/// aussen nachzusehen, also wird sie hier gesucht: die höchste vergebene.
fn juengste_fenster_id() -> FensterId {
    // FensterIds wachsen monoton ab 1. Rückwärts suchen, bis eine existiert.
    for kandidat in (1..10_000u64).rev() {
        let id = FensterId::aus_wert(kandidat);
        if fenster::prozess_fenster_groesse(id).is_some() {
            return id;
        }
    }
    panic!("kein Prozess-Fenster gefunden");
}

/// Ein Pixelblock im ABI-Format (4 Byte je Pixel, B/G/R/ungenutzt).
fn pixel_block(breite: usize, hoehe: usize, farbe: u32) -> alloc::vec::Vec<u8> {
    let mut aus = alloc::vec::Vec::with_capacity(breite * hoehe * 4);
    for _ in 0..breite * hoehe {
        aus.push((farbe & 0xFF) as u8);
        aus.push(((farbe >> 8) & 0xFF) as u8);
        aus.push(((farbe >> 16) & 0xFF) as u8);
        aus.push(0);
    }
    aus
}

/// Das gepackte Rechteck-Argument (dieselbe Rechnung wie in libspeed).
fn rechteck(x: u16, y: u16, breite: u16, hoehe: u16) -> u64 {
    sysfenster::rechteck_packen(x, y, breite, hoehe)
}

// ===========================================================================
// (1) DER ERFOLGSFALL
// ===========================================================================

/// Öffnen, zeichnen, und die Pixel sind wirklich im Fenster.
///
/// Das ist der Meilenstein in seiner kleinsten Form: Ein Ring-3-Prozess
/// hat Speicher gefüllt, der Kernel hat ihn geprüft und kopiert, und der
/// Compositor würde ihn zeigen.
#[test_case]
fn test_fenster_oeffnen_und_zeichnen() {
    let p = Pruefstand::neu();
    let (handle, id) = p.fenster_oeffnen("Testfenster", 64, 32);
    assert_eq!(fenster::prozess_fenster_groesse(id), Some((64, 32)));

    // Ein 8x4-Block in Rot-Orange, abgelegt bei Offset 64 (der Titel liegt
    // bei 0 und soll nicht überschrieben werden).
    let block = pixel_block(8, 4, 0xFF8020);
    let block_va = p.hinlegen(64, &block);
    let gesetzt = p.ok(
        "fenster_zeichnen",
        p.ruf(
            sys::SYS_FENSTER_ZEICHNEN,
            handle,
            block_va,
            block.len() as u64,
            rechteck(10, 5, 8, 4),
        ),
    );
    assert_eq!(gesetzt, 32, "8x4 Pixel muessen gesetzt worden sein");

    // Innen die Farbe, aussen NICHT:
    assert_eq!(fenster::test_pixel_lesen(id, 10, 5), Some(0xFF8020));
    assert_eq!(fenster::test_pixel_lesen(id, 17, 8), Some(0xFF8020));
    assert_ne!(fenster::test_pixel_lesen(id, 9, 5), Some(0xFF8020));
    assert_ne!(fenster::test_pixel_lesen(id, 18, 5), Some(0xFF8020));
    assert_ne!(fenster::test_pixel_lesen(id, 10, 9), Some(0xFF8020));

    // Titel setzen und schliessen gehen auch.
    let titel_va = p.hinlegen(0, b"Neuer Titel");
    p.ok(
        "fenster_titel_setzen",
        p.ruf(sys::SYS_FENSTER_TITEL, handle, titel_va, 11, 0),
    );
    p.ok(
        "fenster_schliessen",
        p.ruf(sys::SYS_FENSTER_SCHLIESSEN, handle, 0, 0, 0),
    );
    assert_eq!(fenster::prozess_fenster_anzahl(), 0);
    // Danach ist das Handle tot — jede weitere Nutzung ist ein Fehler.
    p.fehler(
        "zeichnen nach dem Schliessen",
        p.ruf(sys::SYS_FENSTER_ZEICHNEN, handle, 0, 0, rechteck(0, 0, 1, 1)),
        Fehler::UngueltigerHandle,
    );
    p.beenden();
    serial_println!("[fenster] Erfolgsfall: oeffnen, zeichnen, Titel, schliessen — alles ok.");
}

// ===========================================================================
// (2) BÖSE ZEIGER UND BÖSE RECHTECKE
// ===========================================================================

/// DIE WICHTIGSTE ZUSAGE: Was auch immer ein Prozess übergibt — es wird
/// NIE über den Fenster-Puffer hinaus geschrieben, und der Kernel panickt
/// nie.
///
/// Nachgewiesen mit KANARIENVÖGELN: Vor jedem Angriff wird das ganze
/// Fenster auf eine bekannte Farbe gesetzt; danach werden Punkte in allen
/// vier Ecken geprüft. Ein Angriff, der irgendwo hinschreibt, wo er nicht
/// darf, fällt damit auf.
#[test_case]
fn test_boese_zeiger_und_rechtecke() {
    let p = Pruefstand::neu();
    let (handle, id) = p.fenster_oeffnen("Angriff", 64, 32);

    // Kanarienvogel: das ganze Fenster einfärben.
    let voll = pixel_block(64, 32, 0x111111);
    // 64*32*4 = 8192 Byte — passt nicht in den 3-KiB-Prüfstand-Puffer.
    // Also zeilenweise, das reicht für den Zweck völlig.
    let zeile = pixel_block(64, 1, 0x111111);
    let zeile_va = p.hinlegen(0, &zeile);
    for y in 0..32u16 {
        p.ok(
            "Kanarienvogel malen",
            p.ruf(
                sys::SYS_FENSTER_ZEICHNEN,
                handle,
                zeile_va,
                zeile.len() as u64,
                rechteck(0, y, 64, 1),
            ),
        );
    }
    let _ = voll;
    let unveraendert = |wo: &str| {
        for (x, y) in [(0usize, 0usize), (63, 0), (0, 31), (63, 31), (32, 16)] {
            assert_eq!(
                fenster::test_pixel_lesen(id, x, y),
                Some(0x111111),
                "{}: Pixel ({}, {}) wurde ueberschrieben",
                wo,
                x,
                y
            );
        }
    };
    unveraendert("Ausgangslage");

    let gueltige_laenge = (4u64 * 4) * 4; // 4x4 Pixel

    // --- Zeiger, die nicht dem Prozess gehören ---
    for (name, ptr) in [
        ("Nullzeiger", 0u64),
        ("Kernel-Heap", allocator::HEAP_START as u64),
        ("obere Haelfte", 0xFFFF_8000_0000_0000),
        ("ungemappt im User-Bereich", adressraum::USER_START + 0x40_0000),
        ("kurz vor u64::MAX", u64::MAX - 16),
    ] {
        p.fehler(
            name,
            p.ruf(
                sys::SYS_FENSTER_ZEICHNEN,
                handle,
                ptr,
                gueltige_laenge,
                rechteck(0, 0, 4, 4),
            ),
            Fehler::UngueltigerZeiger,
        );
        unveraendert(name);
    }

    // --- Längen, die nicht zum Rechteck passen ---
    let block_va = p.hinlegen(0, &pixel_block(4, 4, 0xFF0000));
    for (name, laenge) in [
        ("Laenge 0", 0u64),
        ("Laenge zu klein", gueltige_laenge - 4),
        ("Laenge zu gross", gueltige_laenge + 4),
        ("Laenge u64::MAX", u64::MAX),
    ] {
        p.fehler(
            name,
            p.ruf(
                sys::SYS_FENSTER_ZEICHNEN,
                handle,
                block_va,
                laenge,
                rechteck(0, 0, 4, 4),
            ),
            Fehler::UngueltigesArgument,
        );
        unveraendert(name);
    }

    // --- Rechtecke ohne Fläche und absurd grosse ---
    for (name, r) in [
        ("Breite 0", rechteck(0, 0, 0, 4)),
        ("Hoehe 0", rechteck(0, 0, 4, 0)),
        ("alles 0", 0u64),
        ("Riesenrechteck", rechteck(0, 0, 65535, 65535)),
    ] {
        let ergebnis = p.ruf(sys::SYS_FENSTER_ZEICHNEN, handle, block_va, 64, r);
        assert_ne!(ergebnis.0, Fehler::Ok.code(), "{} haette scheitern muessen", name);
        unveraendert(name);
    }

    // --- Rechteck GANZ ausserhalb: abgelehnt, nichts geschrieben ---
    let ergebnis = p.ruf(
        sys::SYS_FENSTER_ZEICHNEN,
        handle,
        block_va,
        gueltige_laenge,
        rechteck(1000, 1000, 4, 4),
    );
    assert_ne!(
        ergebnis.0,
        Fehler::Ok.code(),
        "ein Rechteck komplett ausserhalb muss abgelehnt werden"
    );
    unveraendert("ganz ausserhalb");

    // --- Rechteck TEILWEISE ausserhalb: GEKLEMMT, und nur innen gemalt ---
    // 4x4 ab (62, 30) — nur 2x2 liegen noch im Fenster.
    let gesetzt = p.ok(
        "teilweise ausserhalb",
        p.ruf(
            sys::SYS_FENSTER_ZEICHNEN,
            handle,
            block_va,
            gueltige_laenge,
            rechteck(62, 30, 4, 4),
        ),
    );
    assert_eq!(gesetzt, 4, "es duerfen genau 2x2 Pixel gesetzt worden sein");
    assert_eq!(fenster::test_pixel_lesen(id, 62, 30), Some(0xFF0000));
    assert_eq!(fenster::test_pixel_lesen(id, 63, 31), Some(0xFF0000));
    // ... und die Ecke gegenüber ist unangetastet:
    assert_eq!(fenster::test_pixel_lesen(id, 0, 0), Some(0x111111));

    p.beenden();
    serial_println!(
        "[fenster] Angriffe: 5 boese Zeiger, 4 falsche Laengen, 4 kaputte Rechtecke — \
         alle abgelehnt, kein Pixel daneben."
    );
}

// ===========================================================================
// (3) EIN FREMDES FENSTER-HANDLE GIBT ES NICHT
// ===========================================================================

/// Prozess B probiert ALLE 32 möglichen Handle-Zahlen durch und erreicht
/// A's Fenster mit keiner davon.
///
/// Das ist dieselbe Zusage wie bei Sockets und Dateien (Serie 6, Teil 4):
/// Ein Handle ist ein INDEX in die EIGENE Tabelle. Es gibt keine Zahl, die
/// aus B heraus nach A führt.
#[test_case]
fn test_fremdes_fenster_handle_unerreichbar() {
    let a = Pruefstand::neu();
    let b = Pruefstand::neu();
    let (a_handle, a_id) = a.fenster_oeffnen("Fenster von A", 48, 24);

    // A malt seinen Kanarienvogel.
    let zeile = pixel_block(48, 1, 0x00AA00);
    let zeile_va = a.hinlegen(0, &zeile);
    for y in 0..24u16 {
        a.ok(
            "A malt",
            a.ruf(
                sys::SYS_FENSTER_ZEICHNEN,
                a_handle,
                zeile_va,
                zeile.len() as u64,
                rechteck(0, y, 48, 1),
            ),
        );
    }

    // B versucht alles.
    let b_block = pixel_block(4, 4, 0xFF00FF);
    let b_va = b.hinlegen(0, &b_block);
    for zahl in 0..32u64 {
        let ergebnis = b.ruf(
            sys::SYS_FENSTER_ZEICHNEN,
            zahl,
            b_va,
            b_block.len() as u64,
            rechteck(0, 0, 4, 4),
        );
        assert_ne!(
            ergebnis.0,
            Fehler::Ok.code(),
            "B durfte mit Handle {} NICHT zeichnen",
            zahl
        );
        // Und auch die anderen drei Aufrufe nicht:
        assert_ne!(
            b.ruf(sys::SYS_FENSTER_TITEL, zahl, b_va, 4, 0).0,
            Fehler::Ok.code()
        );
    }
    // A's Fenster ist unversehrt.
    assert_eq!(fenster::test_pixel_lesen(a_id, 0, 0), Some(0x00AA00));
    assert_eq!(fenster::test_pixel_lesen(a_id, 47, 23), Some(0x00AA00));
    assert_eq!(fenster::prozess_fenster_anzahl(), 1);

    a.beenden();
    b.beenden();
    serial_println!("[fenster] Isolation: B hat alle 32 Handle-Zahlen probiert — keine fuehrt zu A.");
}

// ===========================================================================
// (4) DIE VOLLE EREIGNIS-WARTESCHLANGE
// ===========================================================================

/// Eine überlaufende Queue verliert Eingaben — aber NIE den
/// Schliessen-Wunsch und nie die Grösse. Und sie zählt, was sie verwirft.
#[test_case]
fn test_ereignis_queue_laeuft_ueber_ohne_schaden() {
    let p = Pruefstand::neu();
    let (handle, id) = p.fenster_oeffnen("Flut", 64, 32);
    let ziel_va = p.hinlegen(0, &[0u8; prozessfenster::EREIGNIS_BYTES]);

    // Erst das Groesse-Ereignis abholen, das beim Oeffnen entsteht.
    let art = p.ok(
        "erstes Ereignis",
        p.ruf(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 100, 0),
    );
    assert_eq!(art as u32, prozessfenster::ART_GROESSE);

    // Weit über die Kapazität hinaus fluten.
    let flut = prozessfenster::MAX_EREIGNISSE * 3;
    for i in 0..flut {
        assert!(fenster::test_ereignis_einspeisen(
            id,
            prozessfenster::EreignisDaten::taste((b'a' + (i % 26) as u8) as char)
        ));
    }
    let verworfen = fenster::prozess_verworfen(id).expect("Fenster da");
    assert_eq!(
        verworfen,
        (flut - prozessfenster::MAX_EREIGNISSE) as u64,
        "verlorene Eingabe muss GEZAEHLT werden"
    );

    // Jetzt Grösse und Schliessen — beide müssen ankommen, und zwar zuerst.
    assert!(fenster::test_groesse_aendern(id, 80, 40));
    assert!(fenster::test_schliessen_klicken(id));

    let art = p.ok(
        "Groesse trotz voller Queue",
        p.ruf(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 100, 0),
    );
    assert_eq!(
        art as u32,
        prozessfenster::ART_GROESSE,
        "die Groesse muss zuerst kommen — wer danach zeichnet, zeichnet richtig"
    );
    let art = p.ok(
        "Schliessen trotz voller Queue",
        p.ruf(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 100, 0),
    );
    assert_eq!(
        art as u32,
        prozessfenster::ART_SCHLIESSEN,
        "der Schliessen-Wunsch darf NIE in einer vollen Queue verschwinden"
    );

    // Und der zweite Klick auf das X erzwingt es (das Fenster hängt sonst
    // an einem Programm, das nicht reagiert).
    assert!(fenster::test_schliessen_klicken(id));
    assert_eq!(
        fenster::prozess_fenster_anzahl(),
        0,
        "der zweite Klick muss das Fenster schliessen"
    );

    p.beenden();
    serial_println!(
        "[fenster] Queue-Ueberlauf: {} Ereignisse verworfen und gezaehlt, \
         Groesse und Schliessen kamen durch.",
        verworfen
    );
}

// ===========================================================================
// (5) DAS BLOCKIERENDE fenster_ereignis
// ===========================================================================

/// Ohne Ereignis wartet der Syscall — und kehrt SOFORT zurück, sobald
/// eines anfällt (der Weck-Pfad aus Serie 7, Teil 0). Kommt keines, endet
/// er nach seiner Frist mit `Keins` und NICHT mit einem Fehler.
#[test_case]
fn test_ereignis_blockiert_und_weckt_sofort() {
    let p = Pruefstand::neu();
    let (handle, id) = p.fenster_oeffnen("Warten", 32, 16);
    let ziel_va = p.hinlegen(0, &[0u8; prozessfenster::EREIGNIS_BYTES]);
    // Das Öffnungs-Groesse-Ereignis wegnehmen.
    p.ok(
        "Groesse abholen",
        p.ruf(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 100, 0),
    );

    // (a) FRIST: nichts liegt an, also muss der Aufruf nach ~200 ms mit
    // `Keins` zurückkommen — kein Fehler, kein Hänger.
    let start = zeit::ms_seit_boot();
    let art = p.ok(
        "Frist ohne Ereignis",
        p.ruf(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 200, 0),
    );
    let gedauert = zeit::ms_seit_boot() - start;
    assert_eq!(
        art as u32,
        prozessfenster::ART_KEINS,
        "eine abgelaufene Frist ist KEIN Fehler, sondern 'nichts passiert'"
    );
    assert!(
        (150..2000).contains(&gedauert),
        "die Frist von 200 ms wurde nicht eingehalten ({} ms)",
        gedauert
    );

    // (b) SOFORTIGES WECKEN: Auftrag stellen, kurz warten (der Prozess
    // blockiert jetzt), dann ein Ereignis einspeisen.
    p.auftrag_stellen(sys::SYS_FENSTER_EREIGNIS, handle, ziel_va, 5000, 0);
    let bis = zeit::ms_seit_boot() + 60;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }
    assert!(!p.fertig(), "der Syscall haette blockieren muessen");

    let losgeschickt = zeit::ms_seit_boot();
    assert!(fenster::test_ereignis_einspeisen(
        id,
        prozessfenster::EreignisDaten::maus(
            prozessfenster::ART_MAUS_AB,
            7,
            9,
            prozessfenster::KNOPF_LINKS
        )
    ));
    let (fehler, art) = p
        .antwort_abwarten(sys::SYS_FENSTER_EREIGNIS, 3000)
        .expect("nach dem Wecken muss der Syscall zurueckkommen");
    let latenz = zeit::ms_seit_boot() - losgeschickt;
    assert_eq!(fehler, Fehler::Ok.code());
    assert_eq!(art as u32, prozessfenster::ART_MAUS_AB);
    assert!(
        latenz < 50,
        "der Weckruf hat {} ms gebraucht — das sieht nach dem Timer-Netz \
         statt nach sofortigem Wecken aus",
        latenz
    );

    // Die Koordinaten sind wirklich angekommen (copy-out geprüft).
    let mut roh = [0u8; prozessfenster::EREIGNIS_BYTES];
    scheduler::mit_prozess_raum(p.pid, |raum| raum.lesen(VirtAddr::new(ziel_va), &mut roh))
        .expect("Prozess")
        .expect("lesen");
    assert_eq!(i32::from_le_bytes([roh[4], roh[5], roh[6], roh[7]]), 7);
    assert_eq!(i32::from_le_bytes([roh[8], roh[9], roh[10], roh[11]]), 9);

    p.beenden();
    serial_println!("[fenster] Blockieren: Frist gehalten, Weckruf in {} ms.", latenz);
}

// ===========================================================================
// (6) DAS PROZESS-ENDE RÄUMT AUF
// ===========================================================================

/// Ein Prozess öffnet fünf Fenster und wird GETÖTET, ohne eines zu
/// schliessen. Danach ist keines mehr da, und die Frame-Bilanz stimmt.
///
/// Das ist derselbe Beweis wie für Sockets und Pipes in Serie 6: Weil das
/// Fenster ein HANDLE ist und die Handle-Tabelle im Prozess steckt, räumt
/// ihr `Drop` alles ab — es gibt keinen Pfad, der es vergessen könnte.
#[test_case]
fn test_prozess_ende_raeumt_fenster_auf() {
    let vorher_frames = memory::frame_statistik().1;
    assert_eq!(fenster::prozess_fenster_anzahl(), 0, "Ausgangslage");

    let p = Pruefstand::neu();
    let titel_va = p.hinlegen(0, b"Leck");
    for nummer in 0..5 {
        p.ok(
            "Fenster oeffnen",
            p.ruf(sys::SYS_FENSTER_OEFFNEN, titel_va, 4, 200, 120),
        );
        assert_eq!(fenster::prozess_fenster_anzahl(), nummer + 1);
    }
    // KEIN schliesse — der Prozess wird einfach getötet.
    p.beenden();

    assert_eq!(
        fenster::prozess_fenster_anzahl(),
        0,
        "beim Prozess-Ende muessen ALLE Fenster automatisch schliessen"
    );
    let nachher_frames = memory::frame_statistik().1;
    assert_eq!(
        vorher_frames, nachher_frames,
        "Frame-Bilanz: vorher {} belegt, nachher {}",
        vorher_frames, nachher_frames
    );
    serial_println!(
        "[fenster] Leck-Test: 5 Fenster ohne schliesse, Prozess getoetet -> \
         0 Fenster, Frame-Bilanz byte-exakt null."
    );
}

/// Und derselbe Test noch einmal über zehn Runden — ein Leck von einem
/// Fenster je Runde fiele hier auf, ein einmaliger Ausreisser nicht.
#[test_case]
fn test_zehn_runden_fenster_lecken_nicht() {
    let vorher = memory::frame_statistik().1;
    for runde in 0..10 {
        let p = Pruefstand::neu();
        let titel_va = p.hinlegen(0, b"Runde");
        let handle = p.ok(
            "oeffnen",
            p.ruf(sys::SYS_FENSTER_OEFFNEN, titel_va, 5, 160, 100),
        );
        let zeile = pixel_block(160, 1, 0x0000FF);
        let zeile_va = p.hinlegen(64, &zeile);
        p.ok(
            "zeichnen",
            p.ruf(
                sys::SYS_FENSTER_ZEICHNEN,
                handle,
                zeile_va,
                zeile.len() as u64,
                rechteck(0, 50, 160, 1),
            ),
        );
        p.beenden();
        assert_eq!(
            fenster::prozess_fenster_anzahl(),
            0,
            "Runde {}: Fenster blieb liegen",
            runde
        );
    }
    let nachher = memory::frame_statistik().1;
    assert_eq!(
        vorher, nachher,
        "10 Runden: vorher {} Frames belegt, nachher {}",
        vorher, nachher
    );
    serial_println!("[fenster] 10 Runden oeffnen/zeichnen/toeten: Frame-Bilanz null.");
}
