// messung <modus> — misst, was ein Syscall und ein Kontext-Wechsel kosten
//
// Die Messung laeuft ABSICHTLICH in Ring 3 und nicht im Kernel: Gemessen
// werden soll der Weg, den ein echtes Programm nimmt — mit
// Privilegienwechsel, TSS-Stackwechsel und allem, was dazugehoert. Ein
// `int 0x80` aus Ring 0 waere billiger und wuerde die Zahl schoenrechnen.
//
// MODI
//   1  Syscall-Roundtrip: N x getpid, Zeit drumherum, Bestwert melden
//   2  Durchsatz-Blast: schreibt so schnell wie moeglich auf die Ausgabe
//   3  Abgabe-Schleife: ruft endlos `yield` (fuer die Wechsel-Messung)
//   4 <ms>              Pipe-Senke: liest die Eingabe <ms> lang leer und
//                       meldet die Byte-Zahl auf die AUSGABE (Prozess->Prozess)
//   5 <0|1>             Ping-Pong-Partner: 1 Byte lesen, 1 Byte schreiben
//                       (1 = faengt an). Fuer den Fairness-Test.
//   6 <ip> <port> <ms>  Socket-Durchsatz: blaest UDP-Datagramme und meldet
//                       die Byte-Zahl auf die AUSGABE
//
// METHODIK (Modus 1): Es wird die kleinste von mehreren Runden genommen,
// nicht der Mittelwert. Grund: Eine Runde kann mitten drin verdraengt werden
// (der Scheduler nimmt uns alle 20 ms die CPU weg), und diese Fremdzeit
// gehoert nicht zum Syscall. Der Bestwert ist die Runde, die am wenigsten
// gestoert wurde — also die ehrlichste Naeherung an die reinen Kosten.
// Die Uhr hat Millisekunden-Aufloesung; mit 100 000 Aufrufen je Runde ist
// das genau genug (eine Runde dauert Hunderte von Millisekunden).

#![no_std]
#![no_main]

use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

/// Aufrufe je Messrunde.
const AUFRUFE: u64 = 100_000;
/// So viele Runden; der BESTWERT zaehlt.
const RUNDEN: u32 = 7;

/// Blockgroesse fuer die Durchsatz-Messungen: 64 KiB, also genau die
/// Obergrenze EINES Puffers je Syscall (`MAX_PUFFER` der ABI) und zugleich
/// die Voreinstellung der Pipe-Groesse. So fuellt ein einziger `schreibe`
/// eine leere Pipe komplett — die Zahl der Kontext-Wechsel je uebertragenem
/// Megabyte wird dadurch minimal, und gemessen wird der Weg, nicht die
/// Haeppchengroesse. (Vorher standen hier 4096; das kostete 16 Syscalls je
/// Pipe-Fuellung.)
const BLOCK: usize = 64 * 1024;

/// Der Datenblock liegt in `.bss`, NICHT auf dem Stack: Der User-Stack eines
/// SpeedOS-Prozesses ist 64 KiB gross (`prozess::ELF_STACK_SEITEN`) — ein
/// 64-KiB-Feld darauf waere ein sofortiger Guard-Page-Treffer.
static mut PUFFER: [u8; BLOCK] = [0; BLOCK];

/// Liefert den Messpuffer.
///
/// # Safety
/// Einzelner Thread, einzelner Prozess: Es gibt in diesem Programm keinen
/// zweiten Benutzer dieses Puffers und keine Nebenlaeufigkeit.
fn puffer() -> &'static mut [u8; BLOCK] {
    unsafe { &mut *core::ptr::addr_of_mut!(PUFFER) }
}

fn haupt(argumente: &Argumente) -> i32 {
    match argumente.get(1).and_then(zahl_lesen).unwrap_or(1) {
        1 => syscall_roundtrip(),
        2 => durchsatz_blast(),
        3 => abgabe_schleife(),
        4 => pipe_senke(argumente.get(2).and_then(zahl_lesen).unwrap_or(1000)),
        5 => ping_pong(argumente.get(2).and_then(zahl_lesen).unwrap_or(0) == 1),
        6 => socket_blast(
            argumente.get(2).and_then(zahl_lesen).unwrap_or(0) as u32,
            argumente.get(3).and_then(zahl_lesen).unwrap_or(9) as u16,
            argumente.get(4).and_then(zahl_lesen).unwrap_or(1000),
        ),
        _ => {
            println!("Benutzung: {} <1..6> [argumente]", argumente.programm());
            2
        }
    }
}

/// MODUS 1: Was kostet ein `int 0x80` hin und zurueck?
///
/// Gemessen wird `getpid` — der billigste Syscall, den es gibt (eine
/// Tabellen-Abfrage). Was uebrig bleibt, ist also fast reiner
/// Uebergangs-Aufwand: Trap, 15 Register sichern, Dispatch, 15 Register
/// zurueck, `iretq`.
fn syscall_roundtrip() -> i32 {
    let mut bestzeit_ms = u64::MAX;
    for _ in 0..RUNDEN {
        let start = libspeed::zeit_jetzt();
        for _ in 0..AUFRUFE {
            // Der Rueckgabewert wird gebraucht, damit der Aufruf nicht
            // wegoptimiert werden kann.
            core::hint::black_box(libspeed::pid());
        }
        let dauer = libspeed::zeit_jetzt().saturating_sub(start);
        if dauer < bestzeit_ms {
            bestzeit_ms = dauer;
        }
    }
    // Nanosekunden je Aufruf: ms * 1_000_000 / Aufrufe.
    let ns = bestzeit_ms.saturating_mul(1_000_000) / AUFRUFE;
    // Maschinenlesbar auf die AUSGABE (die der Messende mitliest) ...
    println!("SYSCALL_NS={}", ns);
    println!("SYSCALL_RUNDE_MS={}", bestzeit_ms);
    println!("SYSCALL_AUFRUFE={}", AUFRUFE);
    // ... und lesbar auf den Diagnose-Kanal.
    libspeed::diagnoseln!(
        "[messung] {} x getpid in {} ms (Bestwert aus {} Runden) = {} ns/Syscall",
        AUFRUFE,
        bestzeit_ms,
        RUNDEN,
        ns
    );
    0
}

/// MODUS 2: So schnell wie moeglich auf die Standard-Ausgabe schreiben.
///
/// Steht dahinter eine Pipe, misst der Leser damit den Pipe-Durchsatz.
/// Der Inhalt ist gleichgueltig — es geht um Bytes je Sekunde.
fn durchsatz_blast() -> i32 {
    let block = puffer();
    block.fill(b'X');
    loop {
        match libspeed::schreibe(libspeed::AUSGABE, block) {
            Ok(0) => return 0,
            Ok(_) => {}
            // Lese-Ende zu: der Messende ist fertig. Sauber beenden.
            Err(_) => return 0,
        }
    }
}

/// MODUS 4: die andere Seite einer Prozess-zu-Prozess-Pipe.
///
/// Liest `dauer_ms` lang alles, was von der EINGABE kommt, und meldet dann
/// Byte-Zahl und Dauer auf die AUSGABE. Zusammen mit einem `messung 2` als
/// Erzeuger misst das die Strecke **Prozess -> Pipe -> Prozess**, also die
/// Kette, durch die spaeter jedes TLS-Byte laeuft — ganz ohne Kernel-Code
/// dazwischen, der die Zahlen schoenrechnen koennte.
///
/// Die Zeit wird nach JEDEM Lesen geprueft. Das kostet einen zusaetzlichen
/// Syscall je Block (~65 ns bei 64 KiB Nutzlast, also unter 0,1 Promille)
/// und ist der Preis dafuer, dass die Messdauer nicht davon abhaengt, wie
/// gross ein Block gerade ausfaellt.
fn pipe_senke(dauer_ms: u64) -> i32 {
    let ziel = puffer();
    let start = libspeed::zeit_jetzt();
    let mut bytes: u64 = 0;
    loop {
        match libspeed::lese(libspeed::EINGABE, ziel) {
            // 0 = Dateiende: der Erzeuger ist weg, frueher fertig.
            Ok(0) => break,
            Ok(n) => bytes += n,
            Err(_) => break,
        }
        if libspeed::zeit_jetzt().saturating_sub(start) >= dauer_ms {
            break;
        }
    }
    let dauer = libspeed::zeit_jetzt().saturating_sub(start).max(1);
    println!("PP_BYTES={}", bytes);
    println!("PP_MS={}", dauer);
    libspeed::diagnoseln!("[messung] Senke: {} Byte in {} ms", bytes, dauer);
    0
}

/// MODUS 5: Ping-Pong-Partner — der Lasttest fuer die Fairness.
///
/// Zwei davon, ueber Kreuz mit zwei Pipes verbunden, wecken sich gegenseitig
/// bei JEDEM EINZELNEN BYTE. Das ist der Extremfall, gegen den die
/// Fairness-Bremse im Scheduler gebaut ist: Sie duerfen so schnell umschalten
/// wie sie wollen, ein dritter Prozess muss trotzdem CPU bekommen.
///
/// Ein Byte, nicht ein Block: Die Nutzlast soll gerade NICHT ins Gewicht
/// fallen. Gemessen wird, was die Weckerei kostet.
fn ping_pong(faengt_an: bool) -> i32 {
    let mut byte = [0u8; 1];
    if faengt_an && libspeed::schreibe(libspeed::AUSGABE, b"P").is_err() {
        return 1;
    }
    loop {
        match libspeed::lese(libspeed::EINGABE, &mut byte) {
            Ok(0) | Err(_) => return 0, // Gegenseite weg
            Ok(_) => {}
        }
        if libspeed::schreibe(libspeed::AUSGABE, &byte).is_err() {
            return 0;
        }
    }
}

/// MODUS 6: Durchsatz durch einen SOCKET-SYSCALL.
///
/// UDP und nicht TCP, und das ist Absicht: Gemessen werden soll der WEG
/// (Ring 3 -> `int 0x80` -> Zeiger pruefen -> copy-in -> Socket-Schicht ->
/// Geraet), nicht das Fenster- und Wiederholungsverhalten unseres eigenen
/// TCP. Ein TCP-`sende` liefert, sobald der Sendepuffer voll ist, kleinere
/// Zahlen zurueck und misst dann die Gegenstelle mit.
///
/// Fehlt das Netz (kein Geraet, keine Konfiguration), wird das ehrlich als
/// `SOCK_FEHLER=` gemeldet statt eine Null zu erfinden.
fn socket_blast(ip: u32, port: u16, dauer_ms: u64) -> i32 {
    let handle = match libspeed::socket(libspeed::UDP) {
        Ok(handle) => handle,
        Err(fehler) => {
            println!("SOCK_FEHLER={}", fehler.0);
            return 0;
        }
    };
    if let Err(fehler) = libspeed::verbinde(handle, ip, port) {
        println!("SOCK_FEHLER={}", fehler.0);
        let _ = libspeed::schliesse(handle);
        return 0;
    }
    // Ein Datagramm bleibt unter der MTU — alles darueber muesste die
    // IP-Schicht fragmentieren, und Fragmente verwerfen wir bewusst
    // (siehe die IPv4-Entscheidung in CLAUDE.md).
    const DATAGRAMM: usize = 1024;
    let block = &mut puffer()[..DATAGRAMM];
    block.fill(b'U');

    let start = libspeed::zeit_jetzt();
    let mut bytes: u64 = 0;
    let mut aufrufe: u64 = 0;
    loop {
        match libspeed::sende(handle, block) {
            Ok(n) => {
                bytes += n;
                aufrufe += 1;
            }
            Err(fehler) => {
                println!("SOCK_FEHLER={}", fehler.0);
                break;
            }
        }
        if libspeed::zeit_jetzt().saturating_sub(start) >= dauer_ms {
            break;
        }
    }
    let dauer = libspeed::zeit_jetzt().saturating_sub(start).max(1);
    let _ = libspeed::schliesse(handle);
    println!("SOCK_BYTES={}", bytes);
    println!("SOCK_MS={}", dauer);
    println!("SOCK_AUFRUFE={}", aufrufe);
    libspeed::diagnoseln!(
        "[messung] Socket: {} Byte in {} ms ({} Aufrufe)",
        bytes,
        dauer,
        aufrufe
    );
    0
}

/// MODUS 3: endlos die Zeitscheibe abgeben.
///
/// Laufen ZWEI davon, ist jede Abgabe ein Kontext-Wechsel — der Messende
/// zaehlt sie im Scheduler.
fn abgabe_schleife() -> i32 {
    loop {
        libspeed::abgeben();
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
