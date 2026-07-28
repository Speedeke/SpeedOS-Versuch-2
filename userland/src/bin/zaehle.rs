// zaehle [bis] — gibt Zahlen aus, eine je Zeile
//
// Die linke Haelfte des Pipe-Beweises. Bewusst stumpfsinnig: Es zaehlt und
// schreibt auf die Standard-Ausgabe. Was DAHINTER steckt — das
// Terminal-Fenster oder das Lese-Ende einer Pipe — weiss es nicht und geht
// es nichts an. Genau das ist der Punkt einer Pipe:
//
//     starte zaehle 20              -> Zahlen erscheinen im Terminal
//     starte zaehle 20 | filter 7   -> dieselben Zahlen gehen an `filter`
//
// Das Programm ist in beiden Faellen BYTEGLEICH. Es wurde nicht angepasst,
// nicht neu uebersetzt, es hat keine Fallunterscheidung. Die Umleitung
// passiert vollstaendig ausserhalb, beim Start (Handle-Weitergabe).
//
// UND ES BLOCKIERT: Zaehlt es schneller, als die Gegenseite liest, laeuft
// die Pipe voll und der Kernel legt `zaehle` schlafen, bis wieder Platz ist.
// Auch davon steht hier keine Zeile — `schreibe` dauert dann eben laenger.

#![no_std]
#![no_main]

use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

/// Bis wohin gezaehlt wird, wenn nichts angegeben ist.
const STANDARD_BIS: u64 = 20;
/// Obergrenze — ein Tippfehler soll das Terminal nicht stundenlang fluten.
const MAX_BIS: u64 = 100_000;

fn haupt(argumente: &Argumente) -> i32 {
    let bis = match argumente.get(1) {
        None => STANDARD_BIS,
        Some(text) => match zahl_lesen(text) {
            Some(zahl) if zahl >= 1 => zahl.min(MAX_BIS),
            _ => {
                println!("Benutzung: {} [bis]   (1 .. {})", argumente.programm(), MAX_BIS);
                return 2;
            }
        },
    };

    // Optional: eine Pause je Zahl (zaehle <bis> <pause_ms>) — damit laesst
    // sich in der Shell ausprobieren, wie Strg+C einen laufenden Prozess
    // beendet.
    let pause_ms = argumente.get(2).and_then(zahl_lesen).unwrap_or(0);

    for zahl in 1..=bis {
        println!("{}", zahl);
        if pause_ms > 0 {
            libspeed::schlafe(pause_ms);
        }
    }
    0
}

/// Winzige Zahl-aus-Text-Funktion (`core` hat `parse`, aber das zieht
/// Fehlertypen und Formatierung mit — hier reicht das).
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
