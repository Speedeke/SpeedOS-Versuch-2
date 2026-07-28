// protokoll.rs — das rotierende Platten-Log des Kernels
//
// Jede println!-Ausgabe (konsole::_print) landet ZUSÄTZLICH in einem
// RAM-Puffer; ein async Task schreibt ihn sekündlich nach
// /platte/system/log.txt. Läuft die Datei über die Maximalgröße,
// wird ROTIERT: log.txt -> log.alt.txt (rename ersetzt die alte
// Alt-Datei atomar), dann beginnt log.txt neu — es gibt also immer
// bis zu zwei Generationen Log auf der Platte. Das Anhängen selbst
// ist ein write_at ans Dateiende: genau der Offset-Schreibpfad,
// für den die VFS-Naht gebaut wurde.
//
// WARUM ein Puffer + Task statt direkt in _print schreiben?
// Lock-Ordnung! _print hält den KONSOLE-Lock; Dateisystem-Schreiben
// bräuchte VFS -> LAUFWERKE. Shell-Befehle halten aber VFS und
// drucken DANN (VFS -> KONSOLE) — ein synchrones Schreiben aus
// _print (KONSOLE -> VFS) wäre der klassische ABBA-Deadlock.
// Der PUFFER ist deshalb ein reiner BLATT-Lock: anhaengen() fasst
// keinen anderen Lock an, und der Task holt den Puffer ab, BEVOR
// er das VFS nimmt.
//
// Ohne (gemountete) Platte puffert das RAM-Fenster gedeckelt weiter:
// Läuft es über, fliegt das Älteste raus — das Log ist ein Fenster
// der jüngsten Vergangenheit, kein Archiv.

use crate::fs::{self, FsErgebnis, FsFehler};
use crate::zeit;
use alloc::vec::Vec;
use spin::Mutex;

/// RAM-Puffer-Deckel: Mehr als 64 KiB ungeschriebenes Log halten
/// wir nicht vor (ohne Platte würde er sonst ewig wachsen).
const PUFFER_MAX: usize = 64 * 1024;
/// Maximalgröße von log.txt, dann wird rotiert.
pub const LOG_MAX: usize = 64 * 1024;
const LOG_PFAD: &str = "/platte/system/log.txt";
const LOG_ALT_PFAD: &str = "/platte/system/log.alt.txt";

/// Der Sammel-Puffer. BLATT-Lock: Unter ihm wird NIE ein anderer
/// Lock genommen (nur der Allocator fürs Vec-Wachsen — der druckt
/// nicht und nimmt selbst keine weiteren Locks).
static PUFFER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Der Einstieg aus konsole::_print: formatiert und puffert — aber
/// NUR, wenn der Heap schon steht. Ganz frühe Boot-Ausgaben (z. B.
/// die Scancode-Warnung vor der Heap-Initialisierung) würden sonst
/// beim format!-Allozieren den Kernel reißen. Kein Heap, kein Log —
/// diese Zeilen stehen weiterhin auf der seriellen Schnittstelle.
pub fn anhaengen_args(args: core::fmt::Arguments) {
    if crate::allocator::heap_groesse() == 0 {
        return;
    }
    anhaengen(&alloc::format!("{}", args));
}

/// Hängt Text an den RAM-Puffer an. Wird aus konsole::_print
/// gerufen (unter dem KONSOLE-Lock, Interrupts aus) — hier darf
/// deshalb NIE gedruckt oder ein Nicht-Blatt-Lock genommen werden.
pub fn anhaengen(text: &str) {
    let mut puffer = PUFFER.lock();
    let neu = puffer.len() + text.len();
    if neu > PUFFER_MAX {
        // Fenster-Semantik: das Älteste weicht dem Neuen.
        let weg = (neu - PUFFER_MAX).min(puffer.len());
        puffer.drain(..weg);
    }
    puffer.extend_from_slice(text.as_bytes());
}

/// Wie viel Heap belegt der Log-Puffer gerade?
///
/// Für die SPEICHER-BILANZEN der Abschluss-Tests: Der Puffer wächst mit jeder
/// Ausgabe (bis `PUFFER_MAX`), und ein Test, der viel druckt, sähe das sonst
/// als „Leck". Es ist keins — es ist beschränktes, beabsichtigtes Wachstum.
/// Wer eine Bilanz zieht, rechnet diesen Anteil heraus und benennt ihn damit,
/// statt ihn zu verschweigen.
pub fn puffer_bytes() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| PUFFER.lock().capacity())
}

/// Holt den gesamten Puffer-Inhalt ab (und leert ihn).
fn abholen() -> Vec<u8> {
    core::mem::take(&mut *PUFFER.lock())
}

/// Schreibt einen Schub Log-Bytes ans Ende der Log-Datei und
/// rotiert bei Überlauf (pfad -> alt_pfad, dann neu beginnen).
/// Pfade und Maximalgröße sind Parameter — dadurch als reine
/// VFS-Funktion testbar (kleines max, RamFs-Pfade).
pub fn schub_schreiben(
    pfad: &str,
    alt_pfad: &str,
    max: usize,
    daten: &[u8],
) -> FsErgebnis<()> {
    if daten.is_empty() {
        return Ok(());
    }
    // Aktuelle Größe (fehlende Datei = 0 — write_at legt sie an):
    let groesse = match fs::mit_fs(|f| f.stat(pfad)) {
        Ok(meta) => meta.groesse,
        Err(FsFehler::NichtGefunden) => 0,
        Err(fehler) => return Err(fehler),
    };
    if groesse > 0 && groesse + daten.len() > max {
        // Rotation: rename ersetzt eine vorhandene Alt-Datei atomar
        // (Datei-auf-Datei, die rename-Semantik der VFS-Naht).
        fs::mit_fs(|f| f.rename(pfad, alt_pfad))?;
        fs::mit_fs(|f| f.write_at(pfad, 0, daten))?;
    } else {
        fs::mit_fs(|f| f.write_at(pfad, groesse, daten))?;
    }
    Ok(())
}

/// Der Log-Task: schreibt den Puffer sekündlich auf die Platte —
/// wenn sie gemountet ist; sonst bleibt er im RAM-Fenster liegen.
pub async fn log_task() {
    let mut fehler_gemeldet = false;
    loop {
        zeit::warte_ms(1000).await;
        if !fs::ist_gemountet(fs::PLATTE) {
            continue;
        }
        let daten = abholen();
        if daten.is_empty() {
            continue;
        }
        match schub_schreiben(LOG_PFAD, LOG_ALT_PFAD, LOG_MAX, &daten) {
            Ok(()) => fehler_gemeldet = false,
            Err(fehler) => {
                // NUR seriell melden — println! würde wieder in den
                // Log-Puffer laufen (Endlos-Schleife aus Fehlern).
                // Und nur EINMAL pro Fehler-Phase, nicht sekündlich.
                if !fehler_gemeldet {
                    crate::serial_println!(
                        "[LOG] Schreiben nach {} fehlgeschlagen: {}",
                        LOG_PFAD,
                        fehler.meldung()
                    );
                    fehler_gemeldet = true;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — die Rotations-Logik gegen das globale Test-VFS (RamFs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Anhängen wächst, Rotation schneidet: Nach dem Überlauf liegt
    /// der alte Stand in der Alt-Datei und die Log-Datei beginnt neu.
    #[test_case]
    fn test_protokoll_rotation() {
        let pfad = "/log_rotation_test.txt";
        let alt = "/log_rotation_test.alt.txt";
        let max = 100;

        // 1. Zwei Schübe unterhalb der Grenze: reines Anhängen.
        schub_schreiben(pfad, alt, max, b"AAAA").unwrap();
        schub_schreiben(pfad, alt, max, b"BBBB").unwrap();
        assert_eq!(fs::mit_fs(|f| f.lesen(pfad)).unwrap(), b"AAAABBBB");

        // 2. Ein Schub, der die Grenze reißt -> Rotation: die 8 alten
        //    Bytes wandern in die Alt-Datei, der Schub beginnt neu.
        let gross = vec![b'C'; 95];
        schub_schreiben(pfad, alt, max, &gross).unwrap();
        assert_eq!(fs::mit_fs(|f| f.lesen(alt)).unwrap(), b"AAAABBBB");
        assert_eq!(fs::mit_fs(|f| f.lesen(pfad)).unwrap(), gross);

        // 3. Nächste Rotation ERSETZT die Alt-Datei (rename auf
        //    existierende Datei):
        schub_schreiben(pfad, alt, max, &[b'D'; 20]).unwrap();
        assert_eq!(fs::mit_fs(|f| f.lesen(alt)).unwrap(), gross);
        assert_eq!(fs::mit_fs(|f| f.lesen(pfad)).unwrap(), [b'D'; 20]);

        // Aufräumen fürs nächste Test-Gericht:
        fs::mit_fs(|f| f.loeschen(pfad)).unwrap();
        fs::mit_fs(|f| f.loeschen(alt)).unwrap();
    }

    /// Der RAM-Puffer ist ein FENSTER: Überlauf verdrängt das Älteste.
    #[test_case]
    fn test_protokoll_puffer_fenster() {
        // Der globale Puffer könnte schon Boot-Ausgaben enthalten —
        // erst leeren, dann kontrolliert füllen:
        let _ = abholen();
        anhaengen("start-");
        let riesig = alloc::string::String::from_utf8(vec![b'x'; PUFFER_MAX]).unwrap();
        anhaengen(&riesig);
        let inhalt = abholen();
        assert_eq!(inhalt.len(), PUFFER_MAX);
        // Das "start-" ist verdrängt, nur noch x übrig:
        assert!(inhalt.iter().all(|b| *b == b'x'));
    }
}
