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
//
// ==========================================================================
// SOFORTIGES WECKEN (Serie 7, Teil 0 — der Weck-Latenz-Pass)
//
// Bis hierher galt „NACHSEHEN STATT ANSTOSSEN": Der Timer fragte jeden Tick
// nach, ob ein wartender Prozess weiterkann. Das kostete bis zu 4 ms je
// Weckruf — und weil der Leser danach noch seine ganze Zeitscheibe zu Ende
// laufen durfte, in der Praxis eine volle Scheduling-Runde (20 ms). Bei
// 4 KiB Puffer waren das die gemessenen 199 KiB/s.
//
// JETZT STOESST DIE PIPE SELBST AN: Wer Bytes hineinlegt, weckt die Leser;
// wer Bytes herausnimmt, weckt die Schreiber; wer ein Ende schliesst, weckt
// die Gegenseite (Dateiende bzw. EPIPE). Der Timer BLEIBT als
// SICHERHEITSNETZ — das sofortige Wecken ist die schnelle Spur, nicht die
// einzige (siehe `scheduler::wecken`).
//
// DIE LOCK-FALLE DABEI, an der die erste Fassung gescheitert waere: Der
// Timer haelt TABELLE und fragt dann `pipe::lesbar` (try_lock PIPES) — also
// TABELLE -> PIPES. Wuerden wir hier aus `mit_pipes` HERAUS wecken, waere das
// PIPES -> TABELLE, und damit ein klassisches ABBA. Deshalb wird der Weckruf
// INNERHALB des Locks nur ERMITTELT und AUSSERHALB ausgeloest.
// ==========================================================================

use crate::netz::puffer::Ringpuffer;
use crate::prozess::Warteauf;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// Die Nummer einer Pipe (Index in der Tabelle, nur kernel-intern —
/// ein Prozess sieht immer nur seine Handles).
pub type PipeId = u32;

/// Wie viele Pipes es gleichzeitig geben kann.
pub const MAX_PIPES: usize = 16;

// ---------------------------------------------------------------------------
// DAS FASSUNGSVERMOEGEN — und warum 4 KiB zu klein waren
// ---------------------------------------------------------------------------
//
// Die alte Wahl war eine Seite (4 KiB) mit der Begruendung „gross genug, dass
// ein Schreiber nicht bei jeder Zeile blockiert". Das stimmt fuer Zeilen und
// ist falsch fuer Datenstroeme, denn die Puffergroesse ist die STUECKGROESSE
// JE WECKRUF: Ein Schreiber legt hoechstens `kapazitaet()` Bytes ab, bevor er
// blockiert und der Leser geweckt werden muss. Der Durchsatz ist damit
// hoechstens
//
//      Kapazitaet / Weck-Latenz
//
// — bei 4 KiB und einer 20-ms-Runde exakt die gemessenen 199 KiB/s. Das
// sofortige Wecken drueckt die Latenz auf einen Kontext-Wechsel (~450 ns);
// dann begrenzt nur noch, wie oft gewechselt werden MUSS, und das haengt
// wieder an der Kapazitaet.
//
// 64 KiB, weil:
//  * Es ist genau `syscall::MAX_PUFFER`. Damit kann EIN `schreibe`-Syscall
//    eine leere Pipe fuellen und EIN `lese`-Syscall sie leeren — die
//    Untergrenze von einem Kontext-Wechsel je 64 KiB ist erreichbar, ohne
//    dass ein Programm etwas anders machen muesste.
//  * Der Gegendruck wirkt weiterhin: 64 KiB sind gemessen ~0,3 ms
//    Ringpuffer-Zeit, nicht „Megabytes liegen im Kernel".
//  * SPEICHER-OBERGRENZE, ehrlich ausgerechnet: MAX_PIPES (16) x 64 KiB
//    = 1 MiB Heap im schlimmsten Fall. Der Heap wird beim Boot um 1 MiB
//    erweitert und beim Desktop-Start nach Aufloesung weiter — und in der
//    Praxis existieren ein bis zwei Pipes gleichzeitig (je Pipeline-Stufe
//    eine). Der Puffer wird erst beim `anlegen` alloziert, nicht auf Vorrat.
//
// KONFIGURIERBAR ist es, weil genau diese Zahl der Hebel zwischen Durchsatz
// und Kernel-Speicher ist — und weil der ALT/NEU-Vergleich im Messtest sie
// zur Laufzeit zurueckdrehen koennen muss (tests/wecken.rs).

/// Voreinstellung des Fassungsvermoegens (Begruendung siehe oben).
pub const STANDARD_KAPAZITAET: usize = 64 * 1024;
/// Kleinstes erlaubtes Fassungsvermoegen. Darunter wird der Gegendruck zum
/// Dauer-Blockieren; 512 Byte sind eine Zeile mit Reserve.
pub const MIN_KAPAZITAET: usize = 512;
/// Groesstes erlaubtes Fassungsvermoegen (16 x 256 KiB = 4 MiB Worst Case).
pub const MAX_KAPAZITAET: usize = 256 * 1024;

/// Das Fassungsvermoegen, das `anlegen()` benutzt. Zur Laufzeit aenderbar
/// (`kapazitaet_setzen`); bestehende Pipes behalten ihres.
static KAPAZITAET_STANDARD: AtomicUsize = AtomicUsize::new(STANDARD_KAPAZITAET);

/// Das aktuell eingestellte Fassungsvermoegen neuer Pipes.
pub fn kapazitaet() -> usize {
    KAPAZITAET_STANDARD.load(Ordering::Relaxed)
}

/// Stellt das Fassungsvermoegen NEUER Pipes ein (auf `MIN..=MAX` geklemmt)
/// und liefert den wirksam gewordenen Wert. Bestehende Pipes bleiben, wie
/// sie sind — eine Pipe aendert ihre Groesse nie unter dem Benutzer.
pub fn kapazitaet_setzen(bytes: usize) -> usize {
    let wirksam = bytes.clamp(MIN_KAPAZITAET, MAX_KAPAZITAET);
    KAPAZITAET_STANDARD.store(wirksam, Ordering::Relaxed);
    wirksam
}

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
// DER WECKRUF — ermittelt unter dem Lock, ausgeloest ausserhalb
// ---------------------------------------------------------------------------

/// Wer nach einer Pipe-Operation weiterkommen KANN. Reine Daten: Die
/// Entscheidung faellt unter dem PIPES-Lock, das Wecken (das den
/// TABELLE-Lock nimmt) passiert danach — siehe die ABBA-Warnung im Kopf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Weckruf {
    /// Leser dieser Pipe koennen fortfahren (Daten da oder Dateiende).
    leser: bool,
    /// Schreiber koennen fortfahren (Platz da oder kein Leser mehr).
    schreiber: bool,
}

impl Weckruf {
    const NICHTS: Weckruf = Weckruf {
        leser: false,
        schreiber: false,
    };
    const LESER: Weckruf = Weckruf {
        leser: true,
        schreiber: false,
    };
    const SCHREIBER: Weckruf = Weckruf {
        leser: false,
        schreiber: true,
    };
    const BEIDE: Weckruf = Weckruf {
        leser: true,
        schreiber: true,
    };
}

/// Loest einen ermittelten Weckruf aus. **Nur AUSSERHALB von `mit_pipes`
/// aufrufen** — hier wird der Prozess-Tabellen-Lock genommen.
fn wecken(id: PipeId, ruf: Weckruf) {
    if ruf.leser {
        crate::scheduler::wecken(Warteauf::PipeLesen(id));
    }
    if ruf.schreiber {
        crate::scheduler::wecken(Warteauf::PipeSchreiben(id));
    }
}

// ---------------------------------------------------------------------------
// Anlegen und Besitz
// ---------------------------------------------------------------------------

/// Legt eine neue Pipe mit dem eingestellten Fassungsvermoegen an. Der
/// Aufrufer haelt danach BEIDE Enden je einmal — er muss also beide wieder
/// schliessen (oder weitergeben).
pub fn anlegen() -> Option<PipeId> {
    anlegen_mit(kapazitaet())
}

/// Legt eine Pipe mit einem AUSDRUECKLICHEN Fassungsvermoegen an (geklemmt
/// auf `MIN_KAPAZITAET..=MAX_KAPAZITAET`).
pub fn anlegen_mit(bytes: usize) -> Option<PipeId> {
    let gross = bytes.clamp(MIN_KAPAZITAET, MAX_KAPAZITAET);
    mit_pipes(|pipes| {
        let platz = pipes.iter().position(|eintrag| eintrag.is_none())?;
        pipes[platz] = Some(Pipe {
            puffer: Ringpuffer::neu(gross),
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
///
/// SCHLIESSEN IST EIN WECKGRUND, und zwar der wichtigste: Wer auf einer
/// leeren Pipe schlaeft, wartet auf Daten, die nie mehr kommen — er muss
/// aufwachen, um das DATEIENDE zu sehen. Genauso umgekehrt: Ein Schreiber,
/// der auf Platz wartet, den niemand mehr schafft, muss aufwachen, um
/// `Abgebrochen` (EPIPE) zu bekommen. Wird das vergessen, haengt der Prozess
/// bis zur naechsten Timer-Pruefung — oder, wenn man den Timer als
/// Sicherheitsnetz einspart, fuer immer.
pub fn ende_schliessen(id: PipeId, ende: Ende) {
    let ruf = mit_pipes(|pipes| {
        let platz = match pipes.get_mut(id as usize) {
            Some(platz) => platz,
            None => return Weckruf::NICHTS,
        };
        let (leer, ruf) = match platz.as_mut() {
            Some(pipe) => {
                match ende {
                    Ende::Lesen => pipe.leser = pipe.leser.saturating_sub(1),
                    Ende::Schreiben => pipe.schreiber = pipe.schreiber.saturating_sub(1),
                }
                let ruf = match ende {
                    // Letzter Leser weg -> Schreiber wecken (EPIPE).
                    Ende::Lesen if pipe.leser == 0 => Weckruf::SCHREIBER,
                    // Letzter Schreiber weg -> Leser wecken (Dateiende).
                    Ende::Schreiben if pipe.schreiber == 0 => Weckruf::LESER,
                    _ => Weckruf::NICHTS,
                };
                (pipe.leser == 0 && pipe.schreiber == 0, ruf)
            }
            None => (false, Weckruf::NICHTS),
        };
        if leer {
            // Hier fliesst der Ringpuffer-Speicher zurueck. Wer jetzt noch
            // auf DIESER Nummer schlaeft, bekommt beim Neustart des Syscalls
            // `Ungueltig` — auch dafuer muss er geweckt werden, sonst
            // schliefe er auf einer Pipe, die es nicht mehr gibt.
            *platz = None;
            return Weckruf::BEIDE;
        }
        ruf
    });
    wecken(id, ruf);
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
    let (ergebnis, ruf) = mit_pipes(|pipes| {
        let pipe = match pipes.get_mut(id as usize).and_then(|p| p.as_mut()) {
            Some(pipe) => pipe,
            None => return (PipeErgebnis::Ungueltig, Weckruf::NICHTS),
        };
        if ziel.is_empty() {
            return (PipeErgebnis::Bytes(0), Weckruf::NICHTS);
        }
        let gelesen = pipe.puffer.lesen(ziel);
        if gelesen > 0 {
            // PLATZ IST FREI GEWORDEN -> ein blockierter Schreiber kann
            // weiter. Das ist die eine Haelfte des Durchsatz-Gewinns.
            return (PipeErgebnis::Bytes(gelesen), Weckruf::SCHREIBER);
        }
        // Nichts da. Ob das "warte" oder "Ende" heisst, entscheidet allein,
        // ob es ueberhaupt noch jemanden gibt, der schreiben KOENNTE.
        if pipe.schreiber == 0 {
            (PipeErgebnis::Bytes(0), Weckruf::NICHTS)
        } else {
            (PipeErgebnis::Blockiert, Weckruf::NICHTS)
        }
    });
    wecken(id, ruf);
    ergebnis
}

/// Schreibt so viele Bytes wie moeglich.
///
/// * Platz da        -> `Bytes(n)` mit n >= 1 (kann KLEINER als
///   `daten.len()` sein — der Aufrufer ruft dann nochmal)
/// * voll            -> `Blockiert`
/// * kein Leser mehr -> `Abgebrochen` (POSIX-EPIPE)
pub fn schreiben(id: PipeId, daten: &[u8]) -> PipeErgebnis {
    let (ergebnis, ruf) = mit_pipes(|pipes| {
        let pipe = match pipes.get_mut(id as usize).and_then(|p| p.as_mut()) {
            Some(pipe) => pipe,
            None => return (PipeErgebnis::Ungueltig, Weckruf::NICHTS),
        };
        // Die Leser-Pruefung kommt ZUERST: In eine Pipe zu schreiben, die
        // niemand mehr liest, ist auch dann ein Fehler, wenn noch Platz ist.
        if pipe.leser == 0 {
            return (PipeErgebnis::Abgebrochen, Weckruf::NICHTS);
        }
        if daten.is_empty() {
            return (PipeErgebnis::Bytes(0), Weckruf::NICHTS);
        }
        let geschrieben = pipe.puffer.schreiben(daten);
        if geschrieben > 0 {
            // DATEN SIND DA -> ein blockierter Leser kann weiter. Die andere
            // Haelfte des Durchsatz-Gewinns.
            (PipeErgebnis::Bytes(geschrieben), Weckruf::LESER)
        } else {
            (PipeErgebnis::Blockiert, Weckruf::NICHTS)
        }
    });
    wecken(id, ruf);
    ergebnis
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

/// Das Fassungsvermoegen EINER bestehenden Pipe (sie behaelt ihres, auch
/// wenn die Voreinstellung sich seither geaendert hat).
pub fn kapazitaet_von(id: PipeId) -> Option<usize> {
    mit_pipes(|pipes| {
        pipes
            .get(id as usize)
            .and_then(|p| p.as_ref())
            .map(|pipe| pipe.puffer.kapazitaet())
    })
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
        // Klein anlegen: Der Gegendruck ist hier der Prüfgegenstand, nicht
        // der Durchsatz — mit 64 KiB wäre der Test nur langsamer.
        let id = anlegen_mit(MIN_KAPAZITAET).expect("Pipe anlegen");
        let gross = kapazitaet_von(id).expect("Kapazitaet");
        assert_eq!(gross, MIN_KAPAZITAET);
        let brocken = alloc::vec![b'x'; gross];
        assert_eq!(schreiben(id, &brocken), PipeErgebnis::Bytes(gross));
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

    /// DAS FASSUNGSVERMOEGEN ist einstellbar, geklemmt — und eine bestehende
    /// Pipe aendert es NIE unter dem Benutzer.
    #[test_case]
    fn test_pipe_kapazitaet_einstellbar() {
        let vorher = kapazitaet();
        assert_eq!(vorher, STANDARD_KAPAZITAET, "die Voreinstellung ist 64 KiB");
        assert_eq!(STANDARD_KAPAZITAET, 64 * 1024);
        // Der Deckel gilt in beide Richtungen — kein Wert kommt ungeprueft an.
        assert_eq!(kapazitaet_setzen(0), MIN_KAPAZITAET);
        assert_eq!(kapazitaet_setzen(usize::MAX), MAX_KAPAZITAET);
        assert_eq!(kapazitaet_setzen(4096), 4096);
        assert_eq!(kapazitaet(), 4096);

        // Eine JETZT angelegte Pipe bekommt 4096 ...
        let alt = anlegen().expect("Pipe");
        assert_eq!(kapazitaet_von(alt), Some(4096));
        // ... und behaelt sie, auch wenn die Voreinstellung danach steigt.
        kapazitaet_setzen(STANDARD_KAPAZITAET);
        assert_eq!(kapazitaet_von(alt), Some(4096), "Pipe waechst nachtraeglich");
        let neu = anlegen().expect("Pipe");
        assert_eq!(kapazitaet_von(neu), Some(STANDARD_KAPAZITAET));

        // Ausdrueckliche Groesse ueberstimmt die Voreinstellung.
        let klein = anlegen_mit(1024).expect("Pipe");
        assert_eq!(kapazitaet_von(klein), Some(1024));
        // Und sie fasst wirklich genau so viel.
        assert_eq!(schreiben(klein, &alloc::vec![b'y'; 2000]), PipeErgebnis::Bytes(1024));
        assert_eq!(schreiben(klein, b"x"), PipeErgebnis::Blockiert);

        for id in [alt, neu, klein] {
            ende_schliessen(id, Ende::Lesen);
            ende_schliessen(id, Ende::Schreiben);
        }
        assert_eq!(kapazitaet_setzen(vorher), vorher);
        assert_eq!(kapazitaet_von(alt), None, "abgeraeumte Pipe hat keine Groesse");
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
        let id = anlegen_mit(MIN_KAPAZITAET).expect("Pipe anlegen");
        const GESAMT: usize = MIN_KAPAZITAET * 3 + 777;
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
