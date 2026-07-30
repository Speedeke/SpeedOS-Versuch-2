// angreifer <nummer> — ein BOESWILLIGES Programm, mit Absicht
//
// ==========================================================================
// WARUM DIESES PROGRAMM IM REPOSITORY LIEGT
//
// Jede Sicherheitszusage des Kernels ist bis zu ihrer Pruefung eine
// BEHAUPTUNG. „Ein User-Programm kann keinen Kernel-Speicher lesen" ist
// solange nur ein Satz im Kommentar, bis jemand es ernsthaft VERSUCHT.
//
// Dieses Programm versucht es. Es ist kein Demo und kein Spielzeug: Es ist
// der Gegner, gegen den `tests/sicherheit.rs` den Kernel antreten laesst.
// Jeder Angriff hier bildet einen echten nach — und die Erwartung ist immer
// dieselbe:
//
//     ENTWEDER der Syscall lehnt sauber mit einem Fehlercode ab,
//     ODER der Angreifer wird vom Kernel beendet.
//     NIEMALS: der Kernel stirbt, haengt, oder gibt etwas preis.
//
// Und in beiden Faellen muessen die ANDEREN Prozesse weiterlaufen.
//
// ==========================================================================
// ZWEI ARTEN VON ANGRIFFEN — deshalb die Nummern
//
// Manche Angriffe UEBERLEBT der Angreifer (ein abgelehnter Syscall kommt
// zurueck) — die kann er alle hintereinander ausfuehren und selbst pruefen.
// Andere toeten ihn (Page Fault, #GP, #UD): Fuer jeden davon braucht es
// einen eigenen Prozess-Start, denn nach dem ersten ist er tot.
//
// Deshalb waehlt das Argument den Angriff. Rueckgabe:
//   0  = alle geprueften Angriffe wurden korrekt ABGELEHNT
//   1  = ein Angriff ist DURCHGEKOMMEN (der Kernel hat eine Luecke!)
//   2  = falsche Benutzung
//   (kein Rueckgabewert = der Prozess wurde beendet; bei 20..29 erwartet)

#![no_std]
#![no_main]

use libspeed::{println, Argumente, Fehler};

libspeed::hauptprogramm!(haupt);

// --- Adressen, die dem KERNEL gehoeren (aus dem Kopf des Kernels bekannt) ---
/// Der Kernel-Heap (`allocator::HEAP_START`). In JEDEM Adressraum gemappt
/// (der Kernel ist gespiegelt) — aber ohne USER_ACCESSIBLE.
const KERNEL_HEAP: u64 = 0x4444_4444_0000;
/// Das Physik-Komplettmapping des Bootloaders liegt irgendwo in der unteren
/// Haelfte; eine klassische „obere Haelfte"-Kernel-Adresse tut es auch.
const OBERE_HAELFTE: u64 = 0xffff_8000_0000_0000;
/// Der Anfang unseres eigenen User-Bereichs (gemappt und erlaubt).
const USER_BASIS: u64 = 0x80_0000_0000;

fn haupt(argumente: &Argumente) -> i32 {
    let nummer = match argumente.get(1).and_then(zahl_lesen) {
        Some(nummer) => nummer,
        None => {
            println!("Benutzung: {} <nummer>", argumente.programm());
            println!("  1  Syscalls mit Kernel-Zeigern");
            println!("  2  fremde und erfundene Handles");
            println!("  3  ungueltige Syscall-Nummern");
            println!("  4  Zeiger mit Integer-Ueberlauf");
            println!("  5  riesige Laengen");
            println!("  6  Pfad-Angriffe");
            println!("  7  zufall-Syscall mit boesen Zeigern/Laengen");
            println!("  8  Speicher-Syscall mit absurden Groessen");
            println!("  9  riesige und kaputte PEM-Buendel");
            println!(" 20  Kernel-Speicher LESEN      (-> Absturz erwartet)");
            println!(" 21  Kernel-Speicher SCHREIBEN  (-> Absturz erwartet)");
            println!(" 22  Stack-Ueberlauf            (-> Absturz erwartet)");
            println!(" 23  privilegierte Instruktion  (-> Absturz erwartet)");
            println!(" 24  ungueltiger Opcode         (-> Absturz erwartet)");
            println!(" 25  Division durch Null        (-> Absturz erwartet)");
            println!(" 26  Sprung ins Nichts          (-> Absturz erwartet)");
            println!(" 30  Endlosschleife ohne Abgabe (muss praemptiert werden)");
            return 2;
        }
    };

    match nummer {
        1 => kernel_zeiger(),
        2 => fremde_handles(),
        3 => ungueltige_nummern(),
        4 => zeiger_ueberlauf(),
        5 => riesige_laengen(),
        6 => pfad_angriffe(),
        7 => zufall_angriffe(),
        8 => speicher_angriffe(),
        9 => pem_angriffe(),
        20 => kernel_lesen(),
        21 => kernel_schreiben(),
        22 => stack_ueberlauf(),
        23 => privilegierte_instruktion(),
        24 => ungueltiger_opcode(),
        25 => division_durch_null(),
        26 => sprung_ins_nichts(),
        30 => endlosschleife(),
        _ => {
            println!("angreifer: Angriff {} gibt es nicht.", nummer);
            2
        }
    }
}

/// Meldet einen DURCHGEKOMMENEN Angriff — das waere eine echte Luecke.
fn durchgekommen(was: &str) -> i32 {
    println!("!!! LUECKE: {} wurde NICHT abgelehnt !!!", was);
    1
}

/// Prueft, dass ein Syscall fehlgeschlagen ist.
fn muss_scheitern(was: &str, ergebnis: Result<u64, Fehler>) -> Option<i32> {
    match ergebnis {
        Err(fehler) => {
            libspeed::diagnoseln!("[angreifer] {} -> abgelehnt ({})", was, fehler.text());
            None
        }
        Ok(wert) => {
            println!("  {} lieferte {} statt eines Fehlers", was, wert);
            Some(durchgekommen(was))
        }
    }
}

// ===========================================================================
// TEIL A: Angriffe, die der Angreifer ueberlebt (abgelehnte Syscalls)
// ===========================================================================

/// ANGRIFF 1: Dem Kernel Zeiger auf SEINEN EIGENEN Speicher unterschieben.
///
/// Der klassische „confused deputy": Der Kernel DARF diese Adressen lesen und
/// schreiben — wenn er einem User-Zeiger blind folgte, wuerde er es fuer uns
/// tun. Genau davor schuetzt `ring3::copy_in`/`copy_out` (Dauerregel I).
fn kernel_zeiger() -> i32 {
    println!("angreifer: Syscalls mit Kernel-Zeigern ...");
    let mut fehler_gesamt = 0;

    for (name, adresse) in [
        ("Kernel-Heap", KERNEL_HEAP),
        ("obere Haelfte", OBERE_HAELFTE),
        ("Nullzeiger", 0u64),
        ("Seite unter dem User-Bereich", USER_BASIS - 0x1000),
        ("Seite ueber dem User-Bereich", 0x100_0000_0000),
    ] {
        // LESEN lassen: Der Kernel soll aus Kernel-Speicher schreiben ...
        if let Some(code) = muss_scheitern(
            name,
            libspeed::schreibe(libspeed::DIAGNOSE, unsafe {
                core::slice::from_raw_parts(adresse as *const u8, 32)
            }),
        ) {
            fehler_gesamt = code;
        }
        // ... und SCHREIBEN lassen: `stat` macht ein copy-OUT in unser Ziel.
        // Zeigt das Ziel in den Kernel, wuerde der Kernel sich selbst
        // ueberschreiben lassen — der gefaehrlichste Fall ueberhaupt.
        let mut ziel = [0u8; 32];
        let _ = &mut ziel; // (der echte Aufruf unten nutzt die Rohadresse)
        if let Some(code) = muss_scheitern(
            "stat schreibt in den Kernel",
            stat_nach(adresse),
        ) {
            fehler_gesamt = code;
        }
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle Kernel-Zeiger korrekt abgelehnt.");
    }
    fehler_gesamt
}

/// `stat` mit einem ROHEN Ziel-Zeiger (der copy-OUT-Angriff).
fn stat_nach(ziel: u64) -> Result<u64, Fehler> {
    // Ein gueltiger Pfad, damit der Angriff wirklich am ZEIGER scheitert
    // und nicht schon am Pfad.
    let pfad = "/platte/programme/hallo";
    // Sicherheit: Der Kernel prueft den Zeiger selbst — genau das ist der
    // Test. Ein durchkommender Zugriff waere die Luecke, die wir suchen.
    unsafe {
        syscall4(
            libspeed::SYS_STAT,
            pfad.as_ptr() as u64,
            pfad.len() as u64,
            ziel,
            0,
        )
    }
}

/// ANGRIFF 2: Handles benutzen, die uns nicht gehoeren.
///
/// Handles sind bei SpeedOS INDIZES in die eigene Tabelle. Ein Prozess kann
/// also gar keine Zahl bilden, die auf ein fremdes Objekt zeigt — das ist die
/// Zusage, und hier wird sie systematisch durchprobiert.
fn fremde_handles() -> i32 {
    println!("angreifer: fremde und erfundene Handles ...");
    let mut fehler_gesamt = 0;
    let puffer = [0u8; 16];

    // ALLE moeglichen Handle-Zahlen durchprobieren (die Tabelle hat 32
    // Plaetze, 0..2 sind unsere Standard-Kanaele).
    for handle in 3u64..64 {
        if let Some(code) = muss_scheitern(
            "lese von fremdem Handle",
            libspeed::lese(handle, &mut [0u8; 16]),
        ) {
            fehler_gesamt = code;
        }
        if let Some(code) = muss_scheitern(
            "schliesse fremdes Handle",
            libspeed::schliesse(handle),
        ) {
            fehler_gesamt = code;
        }
    }
    // Und die „negativen" (als u64 riesigen) Zahlen.
    for handle in [u64::MAX, u64::MAX - 1, 1 << 32, 1 << 63] {
        if let Some(code) = muss_scheitern("riesiges Handle", libspeed::schreibe(handle, &puffer)) {
            fehler_gesamt = code;
        }
        if let Some(code) = muss_scheitern("riesiges Handle (Socket)", libspeed::socket_zustand(handle)) {
            fehler_gesamt = code;
        }
    }
    // Die reservierten Kanaele darf niemand schliessen — sie gehoeren dem
    // Kernel, nicht uns.
    for reserviert in [0u64, 1, 2] {
        if let Some(code) = muss_scheitern(
            "reserviertes Handle schliessen",
            libspeed::schliesse(reserviert),
        ) {
            fehler_gesamt = code;
        }
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle fremden Handles korrekt abgelehnt.");
    }
    fehler_gesamt
}

/// ANGRIFF 3: Syscall-Nummern, die es nicht gibt.
fn ungueltige_nummern() -> i32 {
    println!("angreifer: ungueltige Syscall-Nummern ...");
    let mut fehler_gesamt = 0;
    // 12 (`zufall`) und 13 (`zeit_geprueft`) sind seit Serie 7 vergeben und
    // deshalb hier ausgetragen — eine belegte Nummer in dieser Liste wuerde
    // nicht den Kernel widerlegen, sondern den Angreifer.
    for nummer in [
        14u64, 15, 25, 26, 31, 38, 39, 64, 100, 200, 239, 241, 255, 256,
        1 << 16, 1 << 32, u64::MAX - 1, u64::MAX,
    ] {
        // Sicherheit: Ein unbekannter Syscall darf hoechstens einen
        // Fehlercode liefern — genau das wird hier geprueft.
        let ergebnis = unsafe { syscall4(nummer, 0, 0, 0, 0) };
        if let Some(code) = muss_scheitern("unbekannte Nummer", ergebnis) {
            fehler_gesamt = code;
        }
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle unbekannten Nummern korrekt abgelehnt.");
    }
    fehler_gesamt
}

/// ANGRIFF 4: Zeiger + Laenge so waehlen, dass die Addition UEBERLAEUFT.
///
/// Der Klassiker: Prueft der Kernel „ptr >= USER_START && ptr+len <= USER_ENDE"
/// ohne Ueberlauf-Schutz, dann ist `ptr = u64::MAX-4, len = 16` scheinbar
/// gueltig — das Ende „kommt hinten wieder heraus" und umschliesst dabei den
/// ganzen Kernel.
fn zeiger_ueberlauf() -> i32 {
    println!("angreifer: Zeiger mit Integer-Ueberlauf ...");
    let mut fehler_gesamt = 0;
    for (ptr, len) in [
        (u64::MAX, 1u64),
        (u64::MAX - 4, 16),
        (u64::MAX - 0xfff, 0x2000),
        (USER_BASIS, u64::MAX),
        (USER_BASIS, u64::MAX - 0x1000),
        // Genau an der Obergrenze des User-Bereichs vorbei:
        (0x100_0000_0000 - 8, 16),
    ] {
        // Sicherheit: Der Kernel prueft mit checked_add — genau das ist der
        // Test.
        let ergebnis = unsafe { syscall4(libspeed::SYS_SCHREIBE, libspeed::DIAGNOSE, ptr, len, 0) };
        if let Some(code) = muss_scheitern("Zeiger-Ueberlauf", ergebnis) {
            fehler_gesamt = code;
        }
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle Ueberlauf-Zeiger korrekt abgelehnt.");
    }
    fehler_gesamt
}

/// ANGRIFF 5: Absurde Laengen — der Kernel soll gigantische Puffer anlegen
/// oder gigantische Mengen kopieren.
fn riesige_laengen() -> i32 {
    println!("angreifer: riesige Laengen ...");
    let mut fehler_gesamt = 0;
    for laenge in [
        64 * 1024 + 1, // knapp ueber MAX_PUFFER
        1 << 20,
        1 << 30,
        1u64 << 40,
        u64::MAX / 2,
        u64::MAX,
    ] {
        // Sicherheit: gedeckelt durch MAX_PUFFER, bevor irgendetwas
        // alloziert wird.
        let ergebnis = unsafe {
            syscall4(libspeed::SYS_SCHREIBE, libspeed::DIAGNOSE, USER_BASIS, laenge, 0)
        };
        if let Some(code) = muss_scheitern("riesige Laenge", ergebnis) {
            fehler_gesamt = code;
        }
        let ergebnis = unsafe {
            syscall4(libspeed::SYS_LESE, libspeed::EINGABE, USER_BASIS, laenge, 0)
        };
        if let Some(code) = muss_scheitern("riesige Lese-Laenge", ergebnis) {
            fehler_gesamt = code;
        }
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle riesigen Laengen korrekt abgelehnt.");
    }
    fehler_gesamt
}

/// ANGRIFF 6: Pfade, die aus dem erlaubten Rahmen fallen.
fn pfad_angriffe() -> i32 {
    println!("angreifer: Pfad-Angriffe ...");
    let mut fehler_gesamt = 0;
    // Relativ (es gibt kein Arbeitsverzeichnis), zu lang, leer.
    for pfad in ["relativ/ohne/slash", "", "."] {
        if let Some(code) = muss_scheitern("ungueltiger Pfad", libspeed::stat(pfad).map(|_| 0)) {
            fehler_gesamt = code;
        }
    }
    // Ein Pfad ueber der Laengengrenze (MAX_PFAD = 255).
    let lang = [b'a'; 400];
    // Sicherheit: Der Kernel deckelt die Laenge, bevor er kopiert.
    let ergebnis = unsafe {
        syscall4(
            libspeed::SYS_STAT,
            lang.as_ptr() as u64,
            lang.len() as u64,
            USER_BASIS,
            0,
        )
    };
    if let Some(code) = muss_scheitern("zu langer Pfad", ergebnis) {
        fehler_gesamt = code;
    }
    if fehler_gesamt == 0 {
        println!("angreifer: alle Pfad-Angriffe korrekt abgelehnt.");
    }
    fehler_gesamt
}

// ===========================================================================
// TEIL A2: DIE NEUEN FLAECHEN AUS SERIE 7 (Zufall, User-Heap, Zertifikate)
// ===========================================================================

/// ANGRIFF 7: Den Zufallsgenerator missbrauchen.
///
/// ==========================================================================
/// WARUM AUSGERECHNET `zufall` EIN LOHNENDES ZIEL IST
///
/// Es ist der einzige Syscall, der UNAUFGEFORDERT in fremden Speicher
/// schreibt und dabei etwas hineinschreibt, das der Angreifer nicht kennen
/// soll. Zwei Angriffe liegen nahe:
///
///  (a) Den Kernel dazu bringen, SICH SELBST zu ueberschreiben, indem man
///      ihm einen Kernel-Zeiger als Ziel gibt (copy-OUT, Dauerregel I).
///  (b) Den Puffer TEILWEISE fuellen lassen. Das waere heimtueckischer als
///      ein Fehler: Halb gefuellter Zufall sieht aus wie Zufall, und die
///      Nullen am Ende faellt niemandem auf. RNG-Dauerregel IV sagt
///      deshalb: Im Fehlerfall bleibt der Puffer IN RUHE. Genau das wird
///      hier nachgemessen und nicht geglaubt.
/// ==========================================================================
fn zufall_angriffe() -> i32 {
    println!("angreifer: zufall-Syscall mit boesen Zeigern und Laengen ...");
    let mut fehler_gesamt = 0;

    // (a) Kernel-Adressen als ZIEL des copy-OUT.
    for (name, adresse) in [
        ("Kernel-Heap", KERNEL_HEAP),
        ("obere Haelfte", OBERE_HAELFTE),
        ("Nullzeiger", 0u64),
        ("Seite unter dem User-Bereich", USER_BASIS - 0x1000),
        ("Seite ueber dem User-Bereich", 0x100_0000_0000),
    ] {
        // Sicherheit: Der Kernel prueft den Zeiger selbst — das IST der Test.
        let ergebnis = unsafe { syscall4(libspeed::SYS_ZUFALL, adresse, 32, 0, 0) };
        if let Some(code) = muss_scheitern("zufall schreibt nach", ergebnis) {
            println!("  ... naemlich nach {}", name);
            fehler_gesamt = code;
        }
    }

    // (b) Laengen, die nicht gehen duerfen.
    let mut puffer = [0u8; 64];
    let zeiger = puffer.as_mut_ptr() as u64;
    for laenge in [
        64 * 1024 + 1,  // knapp ueber MAX_PUFFER
        1u64 << 30,
        u64::MAX / 2,
        u64::MAX,       // Integer-Ueberlauf beim Addieren
    ] {
        let ergebnis = unsafe { syscall4(libspeed::SYS_ZUFALL, zeiger, laenge, 0, 0) };
        if let Some(code) = muss_scheitern("zufall mit riesiger Laenge", ergebnis) {
            fehler_gesamt = code;
        }
    }

    // (c) DIE WICHTIGSTE PRUEFUNG: Bleibt der Puffer im Fehlerfall UNBERUEHRT?
    //
    // Ein Zielbereich, der in unserem Speicher BEGINNT und darueber
    // hinausreicht. Ein Kernel, der erst schreibt und dann prueft, wuerde
    // den Anfang fuellen — und der Aufrufer haette halben Zufall, ohne es
    // zu merken.
    const MUSTER: u8 = 0xA5;
    let mut probe = [MUSTER; 64];
    let probe_zeiger = probe.as_mut_ptr() as u64;
    // Weit ueber das Ende unseres Stacks hinaus.
    let ergebnis = unsafe { syscall4(libspeed::SYS_ZUFALL, probe_zeiger, 16 * 1024 * 1024, 0, 0) };
    if let Some(code) = muss_scheitern("zufall ueber die Bereichsgrenze", ergebnis) {
        fehler_gesamt = code;
    }
    if probe.iter().any(|byte| *byte != MUSTER) {
        println!("  !!! Der Puffer wurde TEILWEISE gefuellt — RNG-Dauerregel IV verletzt !!!");
        fehler_gesamt = durchgekommen("zufall hat halb gefuellt");
    } else {
        libspeed::diagnoseln!("[angreifer] Puffer nach abgelehntem zufall unveraendert — gut.");
    }

    // (d) Laenge 0 ist erlaubt und muss folgenlos bleiben.
    let mut leer = [MUSTER; 8];
    let _ = unsafe { syscall4(libspeed::SYS_ZUFALL, leer.as_mut_ptr() as u64, 0, 0, 0) };
    if leer.iter().any(|byte| *byte != MUSTER) {
        fehler_gesamt = durchgekommen("zufall mit Laenge 0 hat geschrieben");
    }

    // (e) Und der GUTE Fall muss weiterhin funktionieren — ein Kernel, der
    //     nach den Angriffen keinen Zufall mehr liefert, waere auch kaputt.
    let mut echt = [0u8; 32];
    match libspeed::zufall(&mut echt) {
        Ok(_) => {
            if echt.iter().all(|byte| *byte == echt[0]) {
                println!("  !!! zufall lieferte 32 gleiche Bytes !!!");
                fehler_gesamt = 1;
            }
        }
        Err(fehler) => {
            // NichtGesaet ist zulaessig (der Pool braucht Zeit) — alles
            // andere waere ein Schaden aus den Angriffen davor.
            if fehler != Fehler::NICHT_GESAET {
                println!("  zufall ist nach den Angriffen kaputt: {}", fehler.text());
                fehler_gesamt = 1;
            }
        }
    }

    if fehler_gesamt == 0 {
        println!("angreifer: zufall haelt stand (Puffer blieb unberuehrt).");
    }
    fehler_gesamt
}

/// ANGRIFF 8: Den User-Heap-Syscall mit absurden Groessen fuettern.
///
/// `SYS_SPEICHER` mappt Seiten in den eigenen Adressraum. Die Gefahr ist
/// nicht, dass ein Prozess sich selbst schadet (das darf er), sondern dass
/// er den KERNEL beim Rechnen aus dem Tritt bringt: ein Ueberlauf in der
/// Groessen-Arithmetik, eine Obergrenze, die nicht greift, oder ein
/// Speicherfresser, der andere Prozesse aushungert.
fn speicher_angriffe() -> i32 {
    println!("angreifer: Speicher-Syscall mit absurden Groessen ...");
    let mut fehler_gesamt = 0;

    // Absurde Groessen muessen ABGELEHNT werden — und zwar, ohne dass der
    // Kernel dabei rechnet, bis etwas ueberlaeuft.
    for bytes in [u64::MAX, u64::MAX - 4095, 1u64 << 60, 1u64 << 40] {
        let ergebnis = libspeed::speicher_anfordern(bytes);
        if let Some(code) = muss_scheitern("Speicher in absurder Groesse", ergebnis) {
            fehler_gesamt = code;
        }
    }

    // Und jetzt ehrlich anfordern, bis der Kernel Nein sagt. Die Obergrenze
    // MUSS greifen (12 MiB laut docs/syscalls.md) — ein Prozess darf nicht
    // beliebig viel Speicher an sich ziehen.
    let mut geholt: u64 = 0;
    let mut runden = 0;
    while libspeed::speicher_anfordern(1024 * 1024).is_ok() {
        geholt += 1024 * 1024;
        runden += 1;
        if runden > 64 {
            println!("  !!! Der Kernel gibt ohne Ende Speicher heraus !!!");
            fehler_gesamt = durchgekommen("Speicher ohne Obergrenze");
            break;
        }
    }
    libspeed::diagnoseln!(
        "[angreifer] Der Kernel gab {} MiB und sagte dann Nein.",
        geholt / (1024 * 1024)
    );

    // Der Prozess muss danach WEITERLAUFEN koennen — ein Syscall, der nach
    // erschoepftem Heap nicht mehr geht, waere ein Folgeschaden.
    if libspeed::pid() == 0 {
        fehler_gesamt = 1;
        println!("  Nach dem Speicher-Angriff geht getpid nicht mehr.");
    }

    if fehler_gesamt == 0 {
        println!("angreifer: der Speicher-Syscall haelt seine Obergrenze ein.");
    }
    fehler_gesamt
}

// --- Der Arbeitsspeicher fuer die PEM-Angriffe (in `.bss`, kein Heap) ---
const BUENDEL_BYTES: usize = 256 * 1024;
static mut BUENDEL: [u8; BUENDEL_BYTES] = [0; BUENDEL_BYTES];
static mut DER_PUFFER: [u8; libspeed::pem::MAX_DER_BYTES] = [0; libspeed::pem::MAX_DER_BYTES];

/// # Safety
/// Ein SpeedOS-Prozess hat einen Ausfuehrungsstrang.
fn buendel() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(BUENDEL) }
}
fn der_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DER_PUFFER) }
}

/// ANGRIFF 9: Den Zertifikats-Parser mit Unsinn fuettern.
///
/// ==========================================================================
/// WARUM DAS EIN ANGRIFF UND KEINE SPIELEREI IST
///
/// Der Vertrauensanker ist eine DATEI. Wer sie ersetzen kann, bestimmt, was
/// als „geprueft" durchgeht — und der erste Schritt dahin ist, den Parser
/// zum Absturz oder in eine Endlosschleife zu bringen. Deshalb liegt er in
/// Ring 3 (docs/tls-vertrauen.md §4): Ein Fehler soll einen PROZESS treffen.
///
/// Die Zusage aus pem.rs lautet: **panickt nie, feste Obergrenzen, und ein
/// kaputter Block macht nur DIESEN Block ungueltig.** Hier wird sie
/// angegriffen — mit dem Ergebnis, dass der Angreifer NORMAL zurueckkehrt.
/// Ein Absturz waere hier ein Fehlschlag, kein Erfolg.
/// ==========================================================================
fn pem_angriffe() -> i32 {
    println!("angreifer: riesige und kaputte PEM-Buendel ...");
    let mut fehler_gesamt = 0;
    let der = der_puffer();

    // (1) BEGIN ohne END — der Rest der Datei ist unbrauchbar, alles davor
    //     muss gueltig bleiben.
    let laenge = fuellen(b"-----BEGIN CERTIFICATE-----\nQUJD\n", 1);
    let bestand = libspeed::pem::bloecke_durchgehen(&buendel()[..laenge], der, |_| {});
    libspeed::diagnoseln!(
        "[angreifer] BEGIN ohne END: {} gelesen, {} kaputt",
        bestand.gelesen,
        bestand.kaputt
    );

    // (2) Base64-Muell zwischen gueltigen Marken.
    let laenge = fuellen(
        b"-----BEGIN CERTIFICATE-----\n!!!!!????\n-----END CERTIFICATE-----\n",
        1,
    );
    let bestand = libspeed::pem::bloecke_durchgehen(&buendel()[..laenge], der, |_| {});
    if bestand.gelesen != 0 {
        fehler_gesamt = durchgekommen("Base64-Muell wurde als Zertifikat gelesen");
    }

    // (3) SEHR VIELE Bloecke — die Obergrenze MAX_ZERTIFIKATE (512) muss
    //     greifen, und zwar ohne dass es ewig dauert.
    let laenge = fuellen(
        b"-----BEGIN CERTIFICATE-----\nQUJDRA==\n-----END CERTIFICATE-----\n",
        4000,
    );
    let start = libspeed::zeit_jetzt();
    let bestand = libspeed::pem::bloecke_durchgehen(&buendel()[..laenge], der, |_| {});
    let dauer = libspeed::zeit_jetzt() - start;
    libspeed::diagnoseln!(
        "[angreifer] {} Bloecke: {} gelesen, {} uebrig, {} ms",
        4000,
        bestand.gelesen,
        bestand.uebrig,
        dauer
    );
    if bestand.gelesen > libspeed::pem::MAX_ZERTIFIKATE {
        fehler_gesamt = durchgekommen("MAX_ZERTIFIKATE wurde ueberschritten");
    }

    // (4) EIN Block, der groesser ist als der Arbeitspuffer.
    let laenge = riesigen_block_bauen();
    let bestand = libspeed::pem::bloecke_durchgehen(&buendel()[..laenge], der, |_| {});
    if bestand.gelesen != 0 {
        fehler_gesamt = durchgekommen("ein uebergrosser Block wurde angenommen");
    }

    // (5) DER-LAEUFER auf Unsinn: abgeschnitten, Laengen, die ins Nichts
    //     zeigen, und ein Tag-Salat. `kurzinfo` darf NIE panicken und muss
    //     IMMER zurueckkehren.
    let muster: [&[u8]; 7] = [
        &[],
        &[0x30],
        &[0x30, 0x82],
        &[0x30, 0x82, 0xff, 0xff],              // Laenge 65535, nichts dahinter
        &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],  // Laenge 4 GiB
        &[0x30, 0x80, 0x30, 0x80, 0x30, 0x80],  // unbestimmte Laenge (BER)
        &[0xff; 64],
    ];
    for (nummer, probe) in muster.iter().enumerate() {
        let info = libspeed::pem::kurzinfo(probe);
        libspeed::diagnoseln!(
            "[angreifer] DER-Muster {}: Name {} Byte, gueltig {}..{}",
            nummer,
            info.name.len(),
            info.gueltig_ab,
            info.gueltig_bis
        );
    }

    // (6) Selbstaehnlichkeit: ein Block, dessen Inhalt wieder wie ein Block
    //     aussieht. Ein Parser, der rekursiv sucht, haengt hier.
    let laenge = fuellen(
        b"-----BEGIN CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nQUJD\n",
        200,
    );
    let start = libspeed::zeit_jetzt();
    let _ = libspeed::pem::bloecke_durchgehen(&buendel()[..laenge], der, |_| {});
    let dauer = libspeed::zeit_jetzt() - start;
    if dauer > 5_000 {
        println!("  !!! Verschachtelte Marken brauchten {} ms !!!", dauer);
        fehler_gesamt = 1;
    }

    if fehler_gesamt == 0 {
        println!("angreifer: der PEM-Parser haelt stand (kein Absturz, keine Schleife).");
    }
    fehler_gesamt
}

/// Fuellt den Buendel-Puffer `wiederholungen`-mal mit `block` und liefert die
/// benutzte Laenge.
fn fuellen(block: &[u8], wiederholungen: usize) -> usize {
    let ziel = buendel();
    let mut stelle = 0usize;
    for _ in 0..wiederholungen {
        if stelle + block.len() > ziel.len() {
            break;
        }
        ziel[stelle..stelle + block.len()].copy_from_slice(block);
        stelle += block.len();
    }
    stelle
}

/// Baut EINEN Block, dessen Base64-Inhalt weit groesser ist als
/// `MAX_DER_BYTES` — der Arbeitspuffer darf davon nicht ueberlaufen.
fn riesigen_block_bauen() -> usize {
    let ziel = buendel();
    let kopf = b"-----BEGIN CERTIFICATE-----\n";
    let fuss = b"\n-----END CERTIFICATE-----\n";
    ziel[..kopf.len()].copy_from_slice(kopf);
    let mut stelle = kopf.len();
    while stelle + fuss.len() + 4 < ziel.len() {
        ziel[stelle..stelle + 4].copy_from_slice(b"QUJD");
        stelle += 4;
    }
    ziel[stelle..stelle + fuss.len()].copy_from_slice(fuss);
    stelle + fuss.len()
}

// ===========================================================================
// TEIL B: Angriffe, die den Angreifer das Leben kosten
//
// Jeder davon MUSS den Prozess beenden — und den Kernel unbeschadet lassen.
// Sie kehren nie zurueck; der `println!` danach ist nur der Beweis, dass sie
// es nicht tun (erscheint er, ist etwas faul).
// ===========================================================================

/// ANGRIFF 20: Kernel-Speicher direkt lesen (ohne Umweg ueber einen Syscall).
///
/// Die Seite IST in unserem Adressraum gemappt — der Kernel ist ja gespiegelt.
/// Was uns aufhaelt, ist allein das fehlende USER_ACCESSIBLE-Bit. Genau das
/// ist der Unterschied zwischen „nicht da" und „nicht erlaubt".
fn kernel_lesen() -> i32 {
    println!("angreifer: lese Kernel-Speicher bei {:#x} ...", KERNEL_HEAP);
    // Sicherheit: KEINE. Das ist der Angriff. Erwartung: Page Fault.
    let geklaut = unsafe { core::ptr::read_volatile(KERNEL_HEAP as *const u64) };
    durchgekommen("Kernel-Speicher lesen");
    println!("  erbeutet: {:#x}", geklaut);
    1
}

/// ANGRIFF 21: Kernel-Speicher ueberschreiben.
fn kernel_schreiben() -> i32 {
    println!("angreifer: schreibe in Kernel-Speicher bei {:#x} ...", KERNEL_HEAP);
    // Sicherheit: KEINE. Erwartung: Page Fault.
    unsafe { core::ptr::write_volatile(KERNEL_HEAP as *mut u64, 0xDEAD_BEEF) };
    durchgekommen("Kernel-Speicher schreiben")
}

/// ANGRIFF 22: Den eigenen Stack ueberlaufen lassen.
///
/// Unter dem Stack liegt eine GUARD-PAGE (ungemappt). Ohne sie wuerde der
/// Ueberlauf still in den darunterliegenden Speicher schreiben — der uebelste
/// Fehlerfall ueberhaupt, weil er erst viel spaeter und ganz woanders auffaellt.
fn stack_ueberlauf() -> i32 {
    println!("angreifer: lasse den Stack ueberlaufen ...");
    tief(0);
    durchgekommen("Stack-Ueberlauf")
}

/// Endlose Rekursion mit einem grossen Rahmen — frisst den Stack schnell auf.
///
/// Die Warnung „cannot return without recursing" ist hier genau die Absicht:
/// Das Programm SOLL den Stack ueberlaufen lassen.
#[allow(unconditional_recursion)]
#[inline(never)]
fn tief(tiefe: u64) -> u64 {
    let bremse = [tiefe; 128];
    // core::hint::black_box gibt es in core — verhindert, dass der Compiler
    // die Rekursion wegoptimiert.
    let summe = core::hint::black_box(&bremse)[0];
    summe + tief(tiefe + 1)
}

/// ANGRIFF 23: Privilegierte Instruktionen ausfuehren.
///
/// `cli` waere der Jackpot: Interrupts aus heisst, der Timer kann uns die CPU
/// nicht mehr wegnehmen — ein einziger Prozess wuerde die Maschine anhalten.
/// Ring 3 hat IOPL 0, also gibt es #GP.
fn privilegierte_instruktion() -> i32 {
    println!("angreifer: versuche 'cli' (Interrupts abschalten) ...");
    // Sicherheit: KEINE. Erwartung: #GP.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    durchgekommen("cli aus Ring 3")
}

/// ANGRIFF 24: Ein Opcode, den es nicht gibt.
///
/// Bis zum Serie-6-Abschluss hatte SpeedOS fuer #UD KEINEN Handler — und ein
/// Vektor ohne IDT-Eintrag eskaliert zum Double Fault, der den Kernel anhaelt.
/// Dieser Angriff hat die Luecke gefunden.
fn ungueltiger_opcode() -> i32 {
    println!("angreifer: fuehre einen ungueltigen Opcode aus ...");
    // Sicherheit: KEINE. Erwartung: #UD.
    unsafe { core::arch::asm!("ud2", options(nomem, nostack)) };
    durchgekommen("ungueltiger Opcode")
}

/// ANGRIFF 25: Division durch Null — der haeufigste Programmfehler ueberhaupt.
///
/// Auch das war eine Luecke: #DE hatte keinen Handler.
fn division_durch_null() -> i32 {
    println!("angreifer: teile durch Null ...");
    let null = core::hint::black_box(0u64);
    let ergebnis: u64;
    // Sicherheit: KEINE. Erwartung: #DE.
    unsafe {
        core::arch::asm!(
            "xor rdx, rdx",
            "div {teiler}",
            teiler = in(reg) null,
            inout("rax") 42u64 => ergebnis,
            out("rdx") _,
        );
    }
    durchgekommen("Division durch Null");
    println!("  Ergebnis: {}", ergebnis);
    1
}

/// ANGRIFF 26: In ungemappten Speicher springen.
fn sprung_ins_nichts() -> i32 {
    println!("angreifer: springe ins Nichts ...");
    let ziel = core::hint::black_box(USER_BASIS + 0x0F00_0000);
    // Sicherheit: KEINE. Erwartung: Page Fault beim Befehls-Laden.
    unsafe {
        let funktion: extern "C" fn() = core::mem::transmute(ziel as *const ());
        funktion();
    }
    durchgekommen("Sprung ins Nichts")
}

/// ANGRIFF 30: Rechnen und NIE abgeben.
///
/// Der Angriff auf die VERFUEGBARKEIT: Ein kooperatives System waere hier tot
/// — der Prozess gibt die CPU nie freiwillig her, und niemand koennte sie ihm
/// abnehmen. Bei uns nimmt der PIT sie ihm alle 20 ms weg.
///
/// Das Programm laeuft ABSICHTLICH ewig; der Test beendet es von aussen und
/// prueft, dass es dabei verdraengt wurde (`praemptionen > 0`) und dass die
/// anderen Prozesse vorangekommen sind.
fn endlosschleife() -> i32 {
    println!("angreifer: rechne endlos, ohne je abzugeben ...");
    let mut zaehler = 0u64;
    loop {
        zaehler = zaehler.wrapping_add(1);
        core::hint::black_box(zaehler);
        // KEIN yield, KEIN schlafe, KEIN Syscall. Nichts.
    }
}

// ---------------------------------------------------------------------------
// Roher Syscall (fuer Angriffe, die libspeed bewusst nicht anbietet)
// ---------------------------------------------------------------------------

/// # Safety
/// Ruft einen beliebigen Syscall mit beliebigen Argumenten. Genau das ist
/// hier der Zweck — der Kernel muss jeden Unsinn selbst abfangen.
unsafe fn syscall4(nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> Result<u64, Fehler> {
    let fehler: u64;
    let ergebnis: u64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") nummer => fehler,
        in("rdi") a0,
        in("rsi") a1,
        inlateout("rdx") a2 => ergebnis,
        in("r10") a3,
    );
    if fehler == 0 {
        Ok(ergebnis)
    } else {
        Err(Fehler(fehler))
    }
}

fn zahl_lesen(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let mut wert: u64 = 0;
    for ziffer in text.bytes() {
        if !ziffer.is_ascii_digit() {
            return None;
        }
        wert = wert.checked_mul(10)?.checked_add((ziffer - b'0') as u64)?;
    }
    Some(wert)
}
