// kopiere <von> <nach> — Ein echtes Datei-Werkzeug im User-Space
//
// Der Unterschied zum Shell-Befehl `kopiere` ist die PRIVILEGSTUFE: Der
// Shell-Befehl laeuft im Kernel und ruft `fs::kopieren` direkt auf. Dieses
// Programm hier laeuft in Ring 3 und darf das VFS nicht einmal ansehen — es
// bittet den Kernel um jede einzelne Operation:
//
//   oeffne(von, LESEN)                    -> Handle
//   oeffne(nach, SCHREIBEN|ANLEGEN|ABSCHNEIDEN) -> Handle
//   lese_at / schreibe_at im Wechsel      -> Bytes
//   schliesse                             -> fertig
//
// Kopiert wird in Stuecken (STUECK Bytes), nicht am Stueck: Der Puffer liegt
// auf unserem 64-KiB-Stack, und die Datei koennte groesser sein als der
// ganze Adressraum, den wir dafuer haetten. Das ist derselbe Grund, aus dem
// jedes ernsthafte Kopier-Werkzeug in Bloecken arbeitet.

#![no_std]
#![no_main]

use libspeed::{println, Argumente, Fehler};

libspeed::hauptprogramm!(haupt);

/// Groesse eines Kopier-Stuecks. Passt bequem auf den Stack und bleibt weit
/// unter der 64-KiB-Grenze eines einzelnen Syscalls (MAX_PUFFER).
const STUECK: usize = 4096;

fn haupt(argumente: &Argumente) -> i32 {
    let (von, nach) = match (argumente.get(1), argumente.get(2)) {
        (Some(von), Some(nach)) => (von, nach),
        _ => {
            println!("Benutzung: {} <von> <nach>", argumente.programm());
            println!("Beide Pfade muessen absolut sein (mit / beginnen).");
            return 2;
        }
    };

    if von == nach {
        println!("Quelle und Ziel sind dieselbe Datei.");
        return 2;
    }

    // Erst nachsehen, WAS wir kopieren — ein Verzeichnis kann dieses
    // Werkzeug nicht (dafuer braeuchte es Rekursion und `liste`).
    let meta = match libspeed::stat(von) {
        Ok(meta) => meta,
        Err(fehler) => return abbrechen("Quelle", von, fehler),
    };
    if meta.ist_verzeichnis() {
        println!("'{}' ist ein Verzeichnis — kopiere kann nur Dateien.", von);
        return 2;
    }

    let quelle = match libspeed::oeffne(von, libspeed::LESEN) {
        Ok(handle) => handle,
        Err(fehler) => return abbrechen("Oeffnen von", von, fehler),
    };
    let ziel = match libspeed::oeffne(
        nach,
        libspeed::SCHREIBEN | libspeed::ANLEGEN | libspeed::ABSCHNEIDEN,
    ) {
        Ok(handle) => handle,
        Err(fehler) => {
            let _ = libspeed::schliesse(quelle);
            return abbrechen("Anlegen von", nach, fehler);
        }
    };

    let mut puffer = [0u8; STUECK];
    let mut versetzt: u64 = 0;
    let start = libspeed::zeit_jetzt();

    let ergebnis = loop {
        let gelesen = match libspeed::lese_at(quelle, versetzt, &mut puffer) {
            Ok(anzahl) => anzahl as usize,
            Err(fehler) => break Err(("Lesen", von, fehler)),
        };
        // 0 heisst Dateiende — das ist kein Fehler, sondern das Ziel.
        if gelesen == 0 {
            break Ok(());
        }

        // Ein einzelner `schreibe_at` darf weniger uebernehmen, als wir
        // anbieten. Deshalb wird geschrieben, bis das Stueck durch ist —
        // nicht einmal versucht und gehofft.
        let mut geschrieben = 0usize;
        let schreib_fehler = loop {
            if geschrieben == gelesen {
                break None;
            }
            match libspeed::schreibe_at(
                ziel,
                versetzt + geschrieben as u64,
                &puffer[geschrieben..gelesen],
            ) {
                Ok(anzahl) if anzahl > 0 => geschrieben += anzahl as usize,
                // 0 uebernommene Bytes ohne Fehler heisst: Es geht nicht
                // mehr weiter (Dateisystem voll). Endlos weiterprobieren
                // waere ein Haenger.
                Ok(_) => break Some(Fehler::KEIN_PLATZ),
                Err(fehler) => break Some(fehler),
            }
        };
        if let Some(fehler) = schreib_fehler {
            break Err(("Schreiben nach", nach, fehler));
        }
        versetzt += gelesen as u64;
    };

    let _ = libspeed::schliesse(quelle);
    let _ = libspeed::schliesse(ziel);

    match ergebnis {
        Ok(()) => {
            let dauer = libspeed::zeit_jetzt().saturating_sub(start);
            println!(
                "{} Byte von '{}' nach '{}' kopiert ({} ms).",
                versetzt, von, nach, dauer
            );
            // Die Gegenprobe: Hat das Ziel wirklich die richtige Groesse?
            // Ein Kopier-Werkzeug, das seinen Erfolg nur behauptet, ist
            // wertlos (Daten-Integritaets-Regel des Projekts).
            match libspeed::stat(nach) {
                Ok(ziel_meta) if ziel_meta.groesse == versetzt => 0,
                Ok(ziel_meta) => {
                    println!(
                        "WARNUNG: Ziel hat {} Byte statt {} — die Kopie ist unvollstaendig!",
                        ziel_meta.groesse, versetzt
                    );
                    1
                }
                Err(fehler) => abbrechen("Nachpruefen von", nach, fehler),
            }
        }
        Err((was, pfad, fehler)) => abbrechen(was, pfad, fehler),
    }
}

/// Meldet einen Fehler in Klartext und liefert den Exit-Code.
fn abbrechen(was: &str, pfad: &str, fehler: Fehler) -> i32 {
    println!("{} '{}' fehlgeschlagen: {}", was, pfad, fehler.text());
    1
}
