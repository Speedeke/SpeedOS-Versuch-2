// pipe.rs — Pipes: der erste Weg, auf dem zwei Prozesse miteinander reden
//           (Serie 6, Teil 6)
//
// ==========================================================================
// WAS EINE PIPE IST
//
// Ein Byte-Rohr mit zwei Enden. Was vorne hineingeschrieben wird, kommt
// hinten in derselben Reihenfolge wieder heraus — mehr nicht. Keine
// Nachrichten-Grenzen, keine Struktur, keine Adressen:
//
//   Prozess A                 PIPE (Ringpuffer)              Prozess B
//   schreibe(h_w, ...) --->  [ 7 4 2 9 ... ]  --->  lese(h_r, ...)
//                             ^ voll -> A blockiert
//                             ^ leer -> B blockiert
//
// Genau daraus wird `zaehle | filter 7`: Die Shell legt eine Pipe an, gibt
// das Schreib-Ende an `zaehle` als Standard-AUSGABE und das Lese-Ende an
// `filter` als Standard-EINGABE. Beide Programme merken nichts davon — sie
// schreiben auf Handle 1 und lesen von Handle 0, wie immer.
//
// ==========================================================================
// DER RINGPUFFER IST NICHT NEU
//
// `netz::puffer::Ringpuffer` gibt es seit Serie 5: ein Byte-Ring fester
// Kapazitaet mit `schreiben`/`lesen`/`frei`/`len`, unit-getestet, im
// TCP-Sende- und Empfangspuffer im Einsatz. Eine Pipe ist genau das — plus
// zwei Zaehler, wer die Enden noch offen haelt. Also wird er benutzt und
// nicht nachgebaut. Das ist keine Sparsamkeit: Zwei Ringpuffer-
// Implementierungen im selben Kernel waeren zwei Stellen, an denen derselbe
// Off-by-one wohnen kann.
//
// ==========================================================================
// DIE DREI ENTSCHEIDUNGEN, DIE EINE PIPE AUSMACHEN
//
//  (1) VOLL -> DER SCHREIBER WARTET. Nicht "Fehler", nicht "Bytes
//      verwerfen": Ein `zaehle`, das schneller zaehlt als `filter` liest,
//      soll gebremst werden, nicht abgeschnitten. Das ist der Gegendruck,
//      der eine Pipe erst brauchbar macht.
//
//  (2) LEER -> DER LESER WARTET. Aber NUR, solange es noch einen Schreiber
//      gibt. Ist das Schreib-Ende zu, ist "leer" das DATEIENDE: `lese`
//      liefert 0, und der Leser weiss, dass nichts mehr kommt. Genau daran
//      erkennt `filter`, wann es fertig ist.
//
//  (3) LESE-ENDE ZU -> DER SCHREIBER BEKOMMT EINEN FEHLER (`Abgebrochen`,
//      das POSIX-EPIPE). Weiterzuschreiben waere sinnlos — niemand holt es
//      je ab. Ein `zaehle | filter`, bei dem `filter` vorzeitig endet, soll
//      `zaehle` beenden und nicht ewig weiterzaehlen lassen.
//
// ==========================================================================
// BESITZ: ZAEHLER STATT FLAGS
//
// Jedes Ende hat einen ZAEHLER, kein Ja/Nein. Denn ein Ende kann mehrere
// Besitzer haben: Beim `zaehle | filter` haelt die Shell das Schreib-Ende
// kurz selbst, waehrend sie es dem Kind gibt. Erst wenn der Zaehler auf 0
// faellt, gilt das Ende als geschlossen; sind BEIDE 0, verschwindet die Pipe
// samt Puffer. Ein Flag waere hier ein Leck oder ein zu frueh gemeldetes
// Dateiende — je nachdem, wer zuerst schliesst.
//
// LOCK: `PIPES` ist ein BLATT-Lock (nimmt keine weiteren Locks). Der
// Timer-Interrupt fragt ihn mit `try_lock` ab (nie warten!), Syscalls mit
// ausgeschalteten Interrupts — dann haelt ihn garantiert niemand.
// ==========================================================================

use crate::netz::puffer::Ringpuffer;
use spin::Mutex;

/// Die Nummer einer Pipe (Index in der Tabelle, nur kernel-intern —
/// ein Prozess sieht immer nur seine Handles).
pub type PipeId = u32;

/// Wie viele Pipes es gleichzeitig geben kann.
pub const MAX_PIPES: usize = 16;

/// Fassungsvermoegen einer Pipe. 4 KiB ist die klassische Wahl (eine Seite):
/// gross genug, dass ein Schreiber nicht bei jeder Zeile blockiert, klein
/// genug, dass der Gegendruck wirkt, bevor Megabytes im Kernel liegen.
pub const KAPAZITAET: usize = 4096;

/// Welches Ende einer Pipe ist gemeint?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ende {
    Lesen,
    Schreiben,
}

/// Was bei einer Pipe-Operation herauskommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeErgebnis {
    /// So viele Bytes wurden uebertragen (0 beim Lesen = DATEIENDE).
    Bytes(usize),
    /// Die Operation kann GERADE nicht ausgefuehrt werden (voll bzw. leer),
    /// waere aber spaeter moeglich. Der Aufrufer blockiert den Prozess.
    Blockiert,
    /// Die Gegenseite ist weg — beim Schreiben, wenn kein Leser mehr da ist.
    Abgebrochen,
    /// Diese Pipe gibt es nicht (mehr).
    Ungueltig,
}

/// Eine Pipe: ein Ringpuffer plus die Besitz-Zaehler beider Enden.
struct Pipe {
    puffer: Ringpuffer,
    leser: u32,
    schreiber: u32,
}

static PIPES: Mutex<[Option<Pipe>; MAX_PIPES]> = Mutex::new([const { None }; MAX_PIPES]);

/// Zugriff aus KERNEL-Kontext — immer mit ausgeschalteten Interrupts, damit
/// der Timer nicht mitten hinein feuert (dieselbe Regel wie bei der
/// Prozess-Tabelle).
fn mit_pipes<T>(f: impl FnOnce(&mut [Option<Pipe>; MAX_PIPES]) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut PIPES.lock()))
}

// ---------------------------------------------------------------------------
// Anlegen und Besitz
// ---------------------------------------------------------------------------

/// Legt eine neue Pipe an. Der Aufrufer haelt danach BEIDE Enden je einmal —
/// er muss also beide wieder schliessen (oder weitergeben).
pub fn anlegen() -> Option<PipeId> {
    mit_pipes(|pipes| {
        let platz = pipes.iter().position(|eintrag| eintrag.is_none())?;
        pipes[platz] = Some(Pipe {
            puffer: Ringpuffer::neu(KAPAZITAET),
            leser: 1,
            schreiber: 1,
        });
        Some(platz as PipeId)
    })
}

/// Nimmt ein Ende ZUSAETZLICH in Besitz (z. B. beim Weitergeben an ein Kind,
/// bevor der Elternteil seine eigene Kopie schliesst).
pub fn ende_uebernehmen(id: PipeId, ende: Ende) -> bool {
    mit_pipes(|pipes| match pipes.get_mut(id as usize).and_then(|p| p.as_mut()) {
        Some(pipe) => {
            match ende {
                Ende::Lesen => pipe.leser += 1,
                Ende::Schreiben => pipe.schreiber += 1,
            }
            true
        }
        None => false,
    })
}

/// Gibt ein Ende frei. Faellt sein Zaehler auf 0, gilt das Ende als
/// geschlossen; sind BEIDE 0, verschwindet die Pipe mitsamt Puffer.
pub fn ende_schliessen(id: PipeId, ende: Ende) {
    mit_pipes(|pipes| {
        let platz = match pipes.get_mut(id as usize) {
            Some(platz) => platz,
            None => return,
        };
        let leer = match platz.as_mut() {
            Some(pipe) => {
                match ende {
                    Ende::Lesen => pipe.leser = pipe.leser.saturating_sub(1),
                    Ende::Schreiben => pipe.schreiber = pipe.schreiber.saturating_sub(1),
                }
                pipe.leser == 0 && pipe.schreiber == 0
            }
            None => false,
        };
        if leer {
            // Hier fliesst der Ringpuffer-Speicher zurueck.
            *platz = None;
        }
    })
}

// ---------------------------------------------------------------------------
// Lesen und Schreiben
// ---------------------------------------------------------------------------

/// Liest bis zu `ziel.len()` Bytes.
///
/// * Daten da            -> `Bytes(n)` mit n >= 1
/// * leer, Schreiber da  -> `Blockiert` (der Aufrufer legt den Prozess schlafen)
/// * leer, kein Schreiber-> `Bytes(0)` = **Dateiende**
pub fn lesen(id: PipeId, ziel: &mut [u8]) -> PipeErgebnis {
    mit_pipes(|pipes| {
        let pipe = match pipes.get_mut(id as usize).and_then(|p| p.as_mut()) {
            Some(pipe) => pipe,
            None => return PipeErgebnis::Ungueltig,
        };
        if ziel.is_empty() {
            return PipeErgebnis::Bytes(0);
        }
        let gelesen = pipe.puffer.lesen(ziel);
        if gelesen > 0 {
            return PipeErgebnis::Bytes(gelesen);
        }
        // Nichts da. Ob das "warte" oder "Ende" heisst, entscheidet allein,
        // ob es ueberhaupt noch jemanden gibt, der schreiben KOENNTE.
        if pipe.schreiber == 0 {
            PipeErgebnis::Bytes(0)
        } else {
            PipeErgebnis::Blockiert
        }
    })
}

/// Schreibt so viele Bytes wie moeglich.
///
/// * Platz da        -> `Bytes(n)` mit n >= 1 (kann KLEINER als
///   `daten.len()` sein — der Aufrufer ruft dann nochmal)
/// * voll            -> `Blockiert`
/// * kein Leser mehr -> `Abgebrochen` (POSIX-EPIPE)
pub fn schreiben(id: PipeId, daten: &[u8]) -> PipeErgebnis {
    mit_pipes(|pipes| {
        let pipe = match pipes.get_mut(id as usize).and_then(|p| p.as_mut()) {
            Some(pipe) => pipe,
            None => return PipeErgebnis::Ungueltig,
        };
        // Die Leser-Pruefung kommt ZUERST: In eine Pipe zu schreiben, die
        // niemand mehr liest, ist auch dann ein Fehler, wenn noch Platz ist.
        if pipe.leser == 0 {
            return PipeErgebnis::Abgebrochen;
        }
        if daten.is_empty() {
            return PipeErgebnis::Bytes(0);
        }
        let geschrieben = pipe.puffer.schreiben(daten);
        if geschrieben > 0 {
            PipeErgebnis::Bytes(geschrieben)
        } else {
            PipeErgebnis::Blockiert
        }
    })
}

// ---------------------------------------------------------------------------
// Die Weck-Bedingungen (der Timer fragt sie ab)
// ---------------------------------------------------------------------------
//
// WICHTIG: Beide benutzen `try_lock`, NIE `lock`. Sie werden aus dem
// Timer-Interrupt gerufen, und dort auf einen Lock zu warten waere ein
// Haenger (Interrupt-Handler-Regel des Projekts). Ist der Lock gerade
// belegt, gilt "noch nicht bereit" — der naechste Tick kommt in 4 ms.

/// Kann ein Leser jetzt fortfahren? (Daten da ODER Dateiende erreicht.)
pub fn lesbar(id: PipeId) -> bool {
    match PIPES.try_lock() {
        Some(pipes) => match pipes.get(id as usize).and_then(|p| p.as_ref()) {
            // Eine verschwundene Pipe weckt auch — der Syscall liefert dann
            // sauber einen Fehler, statt dass jemand ewig schlaeft.
            None => true,
            Some(pipe) => !pipe.puffer.is_empty() || pipe.schreiber == 0,
        },
        None => false,
    }
}

/// Kann ein Schreiber jetzt fortfahren? (Platz da ODER kein Leser mehr —
/// dann weckt er auf, um den Fehler `Abgebrochen` abzuholen.)
pub fn schreibbar(id: PipeId) -> bool {
    match PIPES.try_lock() {
        Some(pipes) => match pipes.get(id as usize).and_then(|p| p.as_ref()) {
            None => true,
            Some(pipe) => pipe.puffer.frei() > 0 || pipe.leser == 0,
        },
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Diagnose
// ---------------------------------------------------------------------------

/// Wie viele Pipes sind gerade offen? (Leck-Tests.)
pub fn anzahl() -> usize {
    mit_pipes(|pipes| pipes.iter().filter(|eintrag| eintrag.is_some()).count())
}

/// Momentaufnahme einer Pipe: `(belegte Bytes, Leser, Schreiber)`.
pub fn zustand(id: PipeId) -> Option<(usize, u32, u32)> {
    mit_pipes(|pipes| {
        pipes
            .get(id as usize)
            .and_then(|p| p.as_ref())
            .map(|pipe| (pipe.puffer.len(), pipe.leser, pipe.schreiber))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Grundfall: hineinschreiben, herauslesen, Reihenfolge stimmt.
    #[test_case]
    fn test_pipe_grundlagen() {
        let vorher = anzahl();
        let id = anlegen().expect("Pipe anlegen");
        assert_eq!(anzahl(), vorher + 1);
        assert_eq!(zustand(id), Some((0, 1, 1)));

        assert_eq!(schreiben(id, b"Hallo "), PipeErgebnis::Bytes(6));
        assert_eq!(schreiben(id, b"Pipe"), PipeErgebnis::Bytes(4));
        assert_eq!(zustand(id), Some((10, 1, 1)));

        // In Stuecken lesen — die Reihenfolge muss stimmen (FIFO).
        let mut ziel = [0u8; 6];
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(6));
        assert_eq!(&ziel, b"Hallo ");
        let mut rest = [0u8; 16];
        assert_eq!(lesen(id, &mut rest), PipeErgebnis::Bytes(4));
        assert_eq!(&rest[..4], b"Pipe");

        // Jetzt leer, aber der Schreiber lebt -> blockieren, NICHT Ende.
        assert_eq!(lesen(id, &mut rest), PipeErgebnis::Blockiert);
        assert!(!lesbar(id));

        ende_schliessen(id, Ende::Lesen);
        ende_schliessen(id, Ende::Schreiben);
        assert_eq!(anzahl(), vorher, "die Pipe wurde nicht freigegeben");
        // Danach ist die Nummer ungueltig — kein Zugriff auf eine tote Pipe.
        assert_eq!(lesen(id, &mut rest), PipeErgebnis::Ungueltig);
        assert_eq!(schreiben(id, b"x"), PipeErgebnis::Ungueltig);
    }

    /// GEGENDRUCK: Ist die Pipe voll, blockiert der Schreiber — und sobald
    /// gelesen wurde, geht es genau um so viel weiter.
    #[test_case]
    fn test_pipe_voll_blockiert_und_weckt() {
        let id = anlegen().expect("Pipe anlegen");
        let brocken = alloc::vec![b'x'; KAPAZITAET];
        assert_eq!(schreiben(id, &brocken), PipeErgebnis::Bytes(KAPAZITAET));
        // Randvoll: kein Platz mehr.
        assert_eq!(schreiben(id, b"noch was"), PipeErgebnis::Blockiert);
        assert!(!schreibbar(id), "volle Pipe darf nicht schreibbar heissen");
        // Aber lesbar ist sie sehr wohl.
        assert!(lesbar(id));

        // 10 Bytes abholen -> genau 10 Bytes Platz.
        let mut ziel = [0u8; 10];
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(10));
        assert!(schreibbar(id), "nach dem Lesen muss Platz sein");
        assert_eq!(schreiben(id, b"0123456789abc"), PipeErgebnis::Bytes(10));
        assert_eq!(schreiben(id, b"mehr"), PipeErgebnis::Blockiert);

        ende_schliessen(id, Ende::Lesen);
        ende_schliessen(id, Ende::Schreiben);
    }

    /// SCHREIB-ENDE ZU = DATEIENDE. Gepufferte Daten kommen aber noch
    /// vollstaendig heraus — erst DANACH ist Schluss. (Wer das falsch macht,
    /// verliert bei `zaehle | filter` die letzten Zeilen.)
    #[test_case]
    fn test_pipe_schreiber_zu_ist_dateiende() {
        let id = anlegen().expect("Pipe anlegen");
        assert_eq!(schreiben(id, b"letzte Worte"), PipeErgebnis::Bytes(12));
        ende_schliessen(id, Ende::Schreiben);

        // Der Rest kommt noch ...
        let mut ziel = [0u8; 32];
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(12));
        assert_eq!(&ziel[..12], b"letzte Worte");
        // ... und DANN ist Dateiende (0), nicht "blockiert".
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(0));
        assert!(lesbar(id), "Dateiende muss als lesbar gelten (sonst Haenger)");
        // Und es bleibt dabei, beliebig oft.
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(0));

        ende_schliessen(id, Ende::Lesen);
    }

    /// LESE-ENDE ZU = der Schreiber bekommt `Abgebrochen` (EPIPE) — auch
    /// dann, wenn noch Platz waere. Sonst schriebe `zaehle` ewig weiter,
    /// obwohl `filter` laengst weg ist.
    #[test_case]
    fn test_pipe_leser_zu_bricht_ab() {
        let id = anlegen().expect("Pipe anlegen");
        assert_eq!(schreiben(id, b"noch jemand da?"), PipeErgebnis::Bytes(15));
        ende_schliessen(id, Ende::Lesen);

        assert_eq!(schreiben(id, b"hallo?"), PipeErgebnis::Abgebrochen);
        // Und der Schreiber gilt als "bereit" — er soll aufwachen, um den
        // Fehler abzuholen, statt ewig auf Platz zu warten.
        assert!(schreibbar(id));

        ende_schliessen(id, Ende::Schreiben);
        assert_eq!(zustand(id), None, "beide Enden zu -> Pipe muss weg sein");
    }

    /// ZAEHLER STATT FLAGS: Solange ein zweiter Besitzer da ist, ist das Ende
    /// NICHT zu. Genau das passiert, wenn die Shell ein Ende an ein Kind
    /// weitergibt und ihre eigene Kopie schliesst.
    #[test_case]
    fn test_pipe_besitz_zaehler() {
        let id = anlegen().expect("Pipe anlegen");
        // Das Schreib-Ende bekommt einen zweiten Besitzer (das "Kind").
        assert!(ende_uebernehmen(id, Ende::Schreiben));
        assert_eq!(zustand(id), Some((0, 1, 2)));

        // Die "Shell" gibt ihre Kopie ab — das Ende ist damit NICHT zu.
        ende_schliessen(id, Ende::Schreiben);
        assert_eq!(zustand(id), Some((0, 1, 1)));
        let mut ziel = [0u8; 4];
        assert_eq!(
            lesen(id, &mut ziel),
            PipeErgebnis::Blockiert,
            "solange ein Schreiber lebt, ist leer != Ende"
        );

        // Erst der letzte Besitzer macht das Ende wirklich zu.
        ende_schliessen(id, Ende::Schreiben);
        assert_eq!(lesen(id, &mut ziel), PipeErgebnis::Bytes(0));

        ende_schliessen(id, Ende::Lesen);
        assert_eq!(zustand(id), None);
        // Ueberzaehliges Schliessen darf nicht unterlaufen/panicken.
        ende_schliessen(id, Ende::Lesen);
        assert!(!ende_uebernehmen(id, Ende::Lesen));
    }

    /// Die Tabelle ist endlich, und das Volllaufen ist ein sauberer Fehler.
    #[test_case]
    fn test_pipe_tabelle_voll() {
        let vorher = anzahl();
        let mut offen = alloc::vec::Vec::new();
        while let Some(id) = anlegen() {
            offen.push(id);
            assert!(offen.len() <= MAX_PIPES, "Pipe-Tabelle waechst unbegrenzt");
        }
        assert_eq!(offen.len(), MAX_PIPES - vorher);
        for id in &offen {
            ende_schliessen(*id, Ende::Lesen);
            ende_schliessen(*id, Ende::Schreiben);
        }
        assert_eq!(anzahl(), vorher, "Pipes wurden nicht freigegeben");
    }

    /// Grosse Uebertragung in kleinen Stuecken: Nichts geht verloren, nichts
    /// kommt doppelt — der Ringpuffer laeuft dabei mehrfach ueber.
    #[test_case]
    fn test_pipe_ringlauf_ohne_verlust() {
        let id = anlegen().expect("Pipe anlegen");
        const GESAMT: usize = KAPAZITAET * 3 + 777;
        let mut gesendet = 0usize;
        let mut empfangen = 0usize;
        let mut stueck = [0u8; 512];

        while empfangen < GESAMT {
            // Solange Platz ist, das jeweils naechste Byte-Muster senden.
            while gesendet < GESAMT {
                let wert = (gesendet % 251) as u8;
                match schreiben(id, &[wert]) {
                    PipeErgebnis::Bytes(1) => gesendet += 1,
                    PipeErgebnis::Blockiert => break,
                    andere => panic!("unerwartet beim Schreiben: {:?}", andere),
                }
            }
            match lesen(id, &mut stueck) {
                PipeErgebnis::Bytes(n) => {
                    for (i, byte) in stueck[..n].iter().enumerate() {
                        assert_eq!(
                            *byte,
                            ((empfangen + i) % 251) as u8,
                            "Byte {} kam falsch an",
                            empfangen + i
                        );
                    }
                    empfangen += n;
                }
                andere => panic!("unerwartet beim Lesen: {:?}", andere),
            }
        }
        assert_eq!(gesendet, GESAMT);
        assert_eq!(empfangen, GESAMT);

        ende_schliessen(id, Ende::Lesen);
        ende_schliessen(id, Ende::Schreiben);
    }
}
