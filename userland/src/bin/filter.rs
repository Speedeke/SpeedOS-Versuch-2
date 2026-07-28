// filter <text> — gibt nur die Zeilen der Standard-Eingabe aus, die <text>
//                  enthalten
//
// Die rechte Haelfte des Pipe-Beweises — und das erste SpeedOS-Programm, das
// ueberhaupt etwas LIEST, das kein Dateiinhalt ist.
//
//     starte zaehle 20 | filter 7
//     7
//     17
//
// Auch hier: `filter` weiss nicht, woher seine Eingabe kommt. Es liest von
// Handle 0. Ob dahinter eine Pipe steckt, ein Terminal oder gar nichts,
// entscheidet der, der es startet.
//
// DAS DATEIENDE IST DER GANZE TRICK: `lese` blockiert, solange die Pipe leer
// ist UND es noch einen Schreiber gibt. Erst wenn `zaehle` fertig ist und
// sein Schreib-Ende zugeht, liefert `lese` eine 0 — und daran (und NUR
// daran) erkennt `filter`, dass es aufhoeren darf. Ohne diese Unterscheidung
// wuerde es entweder zu frueh enden (bei jeder kurzen Pause) oder nie.

#![no_std]
#![no_main]

use libspeed::{println, Argumente, ZeilenLeser};

libspeed::hauptprogramm!(haupt);

/// Groesste Zeilenlaenge, die am Stueck verarbeitet wird.
const ZEILE_MAX: usize = 512;

fn haupt(argumente: &Argumente) -> i32 {
    let muster = match argumente.get(1) {
        Some(muster) if !muster.is_empty() => muster,
        _ => {
            println!("Benutzung: {} <text>", argumente.programm());
            println!("Gibt die Zeilen der Standard-Eingabe aus, die <text> enthalten.");
            println!("Beispiel:  starte zaehle 20 | filter 7");
            return 2;
        }
    };

    let mut leser: ZeilenLeser<ZEILE_MAX> = ZeilenLeser::neu(libspeed::EINGABE);
    let mut zeile = [0u8; ZEILE_MAX];
    let mut gelesen = 0u64;
    let mut passend = 0u64;

    loop {
        match leser.zeile(&mut zeile) {
            Ok(Some(laenge)) => {
                gelesen += 1;
                // Bytes vergleichen, nicht Zeichen: Die Eingabe muss kein
                // gueltiges UTF-8 sein, und ein Byte-Vergleich kann daran
                // nicht scheitern.
                if enthaelt(&zeile[..laenge], muster.as_bytes()) {
                    passend += 1;
                    // Als BYTES ausgeben und den Umbruch selbst anhaengen —
                    // die Zeile koennte beliebige Bytes enthalten.
                    let _ = libspeed::schreibe(libspeed::AUSGABE, &zeile[..laenge]);
                    let _ = libspeed::schreibe(libspeed::AUSGABE, b"\n");
                }
            }
            // DATEIENDE: Die Gegenseite hat ihr Schreib-Ende geschlossen.
            Ok(None) => break,
            Err(fehler) => {
                println!("filter: Lesefehler ({})", fehler.text());
                return 1;
            }
        }
    }

    // Die Bilanz auf den DIAGNOSE-Kanal (nur seriell): Sie gehoert nicht in
    // die Ausgabe, die vielleicht das naechste Programm einer Pipeline liest.
    libspeed::diagnoseln!(
        "[filter] {} Zeile(n) gelesen, {} enthielten '{}'.",
        gelesen,
        passend,
        muster
    );
    // Kein Treffer ist KEIN Fehler — nur eine Antwort.
    0
}

/// Enthaelt `heuhaufen` die Folge `nadel`? (Byte-weise, ohne Allokation.)
fn enthaelt(heuhaufen: &[u8], nadel: &[u8]) -> bool {
    if nadel.is_empty() {
        return true;
    }
    if nadel.len() > heuhaufen.len() {
        return false;
    }
    heuhaufen
        .windows(nadel.len())
        .any(|fenster| fenster == nadel)
}
