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
/// Blockgroesse fuer den Durchsatz-Blast.
const BLOCK: usize = 4096;

fn haupt(argumente: &Argumente) -> i32 {
    match argumente.get(1).and_then(zahl_lesen).unwrap_or(1) {
        1 => syscall_roundtrip(),
        2 => durchsatz_blast(),
        3 => abgabe_schleife(),
        _ => {
            println!("Benutzung: {} <1|2|3>", argumente.programm());
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
    let block = [b'X'; BLOCK];
    loop {
        match libspeed::schreibe(libspeed::AUSGABE, &block) {
            Ok(0) => return 0,
            Ok(_) => {}
            // Lese-Ende zu: der Messende ist fertig. Sauber beenden.
            Err(_) => return 0,
        }
    }
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
