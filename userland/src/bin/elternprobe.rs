// elternprobe [schlaf_ms] — ein Prozess, der einen anderen startet
//
// Das erste SpeedOS-Programm, das SELBST ein Programm startet. Bis hierhin
// kam jeder Prozess von der Shell (also aus dem Kernel); dieses hier baut
// die Eltern-Kind-Beziehung aus RING 3 auf:
//
//     starte(pfad, eingabe, ausgabe) -> PID des Kindes
//     warte(pid)                     -> sein Exit-Code
//
// UND ES PRUEFT BEIDE REIHENFOLGEN, je nach Argument:
//
//   elternprobe 0     — DER ELTERNTEIL WARTET ZUERST. `warte` blockiert,
//                       der Kernel legt den Prozess schlafen und weckt ihn,
//                       wenn das Kind endet.
//   elternprobe 500   — DAS KIND ENDET ZUERST. Erst 500 ms schlafen (das
//                       Kind ist laengst durch), dann `warte`. Der Exit-Code
//                       muss TROTZDEM ankommen — er wurde gepuffert.
//
// Der zweite Fall ist der interessante: Bei einem Unix haette das Kind bis
// zum `wait` als ZOMBIE herumgelegen. Bei SpeedOS liegt sein Ergebnis beim
// Elternteil, und das Kind selbst ist restlos weg (siehe
// `prozess::Prozess::kinder_enden`).

#![no_std]
#![no_main]

use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

/// Das Kind. Ein fester Pfad, weil `starte` bewusst KEINE Argumente an das
/// Kind weitergibt (docs/syscalls.md) — `hallo` kommt ohne aus und beendet
/// sich mit 0.
const KIND: &str = "/platte/programme/hallo";

fn haupt(argumente: &Argumente) -> i32 {
    let schlaf_ms = argumente.get(1).and_then(zahl_lesen).unwrap_or(0);

    // Das Kind bekommt UNSERE Standard-Kanaele (ERBE_KEINS = nicht
    // umleiten) — seine Ausgabe erscheint also dort, wo auch unsere landet.
    let kind = match libspeed::starte(KIND, libspeed::ERBE_KEINS, libspeed::ERBE_KEINS) {
        Ok(pid) => pid,
        Err(fehler) => {
            println!("elternprobe: Kind starten fehlgeschlagen: {}", fehler.text());
            return 1;
        }
    };
    println!("elternprobe: Kind {} gestartet.", kind);

    if schlaf_ms > 0 {
        // REIHENFOLGE A: erst schlafen, damit das Kind sicher fertig ist.
        libspeed::schlafe(schlaf_ms);
        println!("elternprobe: {} ms geschlafen — das Kind ist laengst durch.", schlaf_ms);
    }

    // REIHENFOLGE B kommt hier direkt an: `warte` blockiert, bis das Kind
    // endet. In beiden Faellen MUSS derselbe Exit-Code herauskommen.
    let code = match libspeed::warte(kind) {
        Ok(code) => code,
        Err(fehler) => {
            println!("elternprobe: warte({}) fehlgeschlagen: {}", kind, fehler.text());
            return 1;
        }
    };
    println!("elternprobe: Kind {} endete mit Code {}.", kind, code);

    // Ein zweites `warte` auf dasselbe Kind MUSS fehlschlagen — das
    // Ergebnis wurde abgeholt, es gibt kein Kind mehr. (Ein Ergebnis, das
    // sich beliebig oft abholen liesse, waere genau der Zustand, den
    // „Zombie" beschreibt.)
    match libspeed::warte(kind) {
        Err(_) => println!("elternprobe: zweites warte() abgelehnt — richtig so."),
        Ok(nochmal) => {
            println!("elternprobe: FEHLER — warte() lieferte {} ein zweites Mal.", nochmal);
            return 1;
        }
    }

    // Der eigene Exit-Code ist der des Kindes — daran prueft der Test, dass
    // der Wert wirklich durch beide Ebenen gekommen ist.
    code as i32
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
