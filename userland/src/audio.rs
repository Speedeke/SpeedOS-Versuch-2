// libspeed::audio — Ton ausgeben aus Ring 3
//
// ===========================================================================
// DIE NAHT ZUM KERNEL, IN VIER SYSCALLS
//
//   audio_oeffnen()                 -> Handle (eine Tonquelle im Mixer)
//   audio_schreiben(h, ptr, laenge) -> uebernommene Bytes
//   audio_status(h)                 -> Samples, die noch warten
//   audio_lautstaerke(h, promille)  -> Lautstaerke DIESER Quelle
//
// Die GESAMT-Lautstaerke stellt der Benutzer im Systray. Ein Programm
// kann sie nicht anfassen — das ist Absicht und keine Luecke.
//
// ===========================================================================
// SCHREIBEN BLOCKIERT NICHT — und was das fuer den Aufrufer heisst
//
// `schreiben` liefert `Belegt`, wenn der Vorlauf im Kernel voll ist
// (rund fuenf Sekunden). Das ist KEIN Fehler, sondern die Aufforderung
// zu warten: Ein blockierender Audio-Aufruf waere ein Programm, das im
// Syscall steht, waehrend sein eigener Ton laeuft — dann koennte es
// weder eine Fortschrittsanzeige zeichnen noch auf eine Taste
// reagieren.
//
// `Strom::nachfuellen` macht daraus die bequeme Form: Es schiebt so
// viel, wie hineingeht, und sagt, wie weit es gekommen ist.

use crate::{syscall_roh, Fehler, SYS_AUDIO_LAUTSTAERKE, SYS_AUDIO_OEFFNEN, SYS_AUDIO_SCHREIBEN,
            SYS_AUDIO_STATUS};

/// Die Abtastrate, mit der der Kernel arbeitet. **Wer etwas anderes
/// schickt, spielt zu schnell oder zu langsam** — es wird nicht
/// umgerechnet (siehe docs/grenzen.md).
pub const ABTASTRATE: u32 = 48_000;
/// Stereo, verschraenkt L/R.
pub const KANAELE: usize = 2;

/// Groesste Menge je Aufruf (muss zu `MAX_AUDIO_BYTES` im Kernel
/// passen — eine ABI ist ein Vertrag, kein geteilter Header).
pub const MAX_BYTES: usize = 64 * 1024;

/// Eine offene Tonquelle.
pub struct Strom {
    handle: u64,
}

impl Strom {
    /// Eine Tonquelle anmelden.
    ///
    /// `NichtKonfiguriert` heisst: Es gibt kein Audio-Geraet. Das ist
    /// kein Programmfehler und sollte auch nicht so aussehen.
    pub fn oeffnen() -> Result<Strom, Fehler> {
        // SAFETY: keine Zeiger im Spiel.
        let handle = unsafe { syscall_roh(SYS_AUDIO_OEFFNEN, 0, 0, 0, 0) }?;
        Ok(Strom { handle })
    }

    /// So viel wie moeglich von `samples` uebergeben.
    ///
    /// Liefert die Zahl der uebernommenen SAMPLES (nicht Bytes). Ist der
    /// Vorlauf voll, sind es 0 — dann warten und erneut versuchen.
    pub fn nachfuellen(&mut self, samples: &[i16]) -> Result<usize, Fehler> {
        if samples.is_empty() {
            return Ok(0);
        }
        // Auf die Syscall-Grenze kuerzen. Ein Aufrufer soll sich darum
        // nicht kuemmern muessen.
        let hoechstens = (MAX_BYTES / 2).min(samples.len());
        let bytes = hoechstens * 2;
        // SAFETY: `samples` zeigt auf unseren eigenen Speicher, `bytes`
        // liegt innerhalb. Der Kernel PRUEFT den Zeiger ohnehin selbst
        // und kopiert (Dauerregel I) — er folgt ihm nie blind.
        let ergebnis = unsafe {
            syscall_roh(
                SYS_AUDIO_SCHREIBEN,
                self.handle,
                samples.as_ptr() as u64,
                bytes as u64,
                0,
            )
        };
        match ergebnis {
            Ok(uebernommen) => Ok(uebernommen as usize / 2),
            // BELEGT IST KEIN FEHLER, sondern „warte kurz".
            Err(Fehler::BELEGT) => Ok(0),
            Err(f) => Err(f),
        }
    }

    /// Wie viele Samples noch warten. 0 = alles abgespielt.
    pub fn wartend(&self) -> Result<usize, Fehler> {
        // SAFETY: keine Zeiger im Spiel.
        let n = unsafe { syscall_roh(SYS_AUDIO_STATUS, self.handle, 0, 0, 0) }?;
        Ok(n as usize)
    }

    /// Lautstaerke DIESER Quelle, in Promille (0..=1000).
    pub fn lautstaerke_setzen(&mut self, promille: u16) -> Result<(), Fehler> {
        // SAFETY: keine Zeiger im Spiel.
        unsafe { syscall_roh(SYS_AUDIO_LAUTSTAERKE, self.handle, promille as u64, 0, 0) }?;
        Ok(())
    }

    pub fn handle(&self) -> u64 {
        self.handle
    }
}

impl Drop for Strom {
    fn drop(&mut self) {
        // Das Handle schliessen — der Kernel meldet die Quelle dabei
        // vom Mixer ab. Faellt das aus (Absturz), erledigt es der `Drop`
        // der Handle-Tabelle; es gibt keinen Pfad, auf dem die Quelle
        // haengenbleibt.
        let _ = crate::schliesse(self.handle);
    }
}
