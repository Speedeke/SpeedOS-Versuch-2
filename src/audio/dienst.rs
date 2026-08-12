// audio::dienst — der globale Mixer und der Pump-Task
//
// ===========================================================================
// WER SCHIEBT DIE SAMPLES?
//
// Der Mixer ist eine reine Rechenmaschine (audio::mixer) — er weiss
// nichts von Hardware und nichts von Zeit. Die Hardware wiederum
// verlangt, dass ihr Ringpuffer nachgefuellt wird, BEVOR der Lesezeiger
// ihn eingeholt hat. Dazwischen fehlt jemand, der regelmaessig
// nachschaut.
//
// Das ist dieser Task. Er laeuft alle paar Millisekunden, fragt den
// Mixer nach dem naechsten Stueck und schiebt es in die Hardware.
//
// ===========================================================================
// WARUM DER TASK IMMER LAEUFT UND NICHT NUR BEI BEDARF
//
// Die naheliegende Sparmassnahme waere: Task nur starten, wenn jemand
// Ton will. Sie ist falsch, und zwar aus einem Grund, den man erst im
// Betrieb hoert: Der HDA-Stream braucht nach dem `RUN` eine Weile, bis
// er sauber laeuft, und ein Start mitten in einer Tonfolge erzeugt ein
// Knacken.
//
// Statt dessen laeuft der Stream, solange irgendeine Quelle da ist, und
// der Task schiebt STILLE, wenn gerade nichts anliegt. Das kostet ein
// paar Kilobyte Nullen je Sekunde und klingt dafuer richtig.
//
// Sind GAR KEINE Quellen angemeldet, wird der Stream angehalten — sonst
// liefe die Hardware auch nachts durch.

use super::mixer::{Mixer, Promille, VOLL};
use super::{AudioGeraet, Sample, KANAELE};
use crate::{serial_println, zeit};
use alloc::string::String;
use spin::Mutex;

/// Wie viele Frames je Durchgang gemischt werden.
///
/// 512 Frames sind rund 10 ms. Kleiner waere unnoetig oft geweckt,
/// groesser hiesse, dass eine Lautstaerkeaenderung spaeter wirkt.
const STUECK_FRAMES: usize = 512;

/// Der globale Mixer.
///
/// **Ein BLATT-LOCK** wie die Ablage und das USB-Verzeichnis: Er wird
/// genommen, um Samples anzuhaengen oder die Lautstaerke zu stellen —
/// nie waehrend eines Registerzugriffs und nie mit dem HDA-Lock in der
/// Hand. Lock-Ordnung: MIXER -> HDA, nie andersherum.
static MIXER: Mutex<Option<Mixer>> = Mutex::new(None);

/// Mit dem Mixer arbeiten. Legt ihn beim ersten Zugriff an.
pub fn mit_mixer<R>(f: impl FnOnce(&mut Mixer) -> R) -> R {
    let mut g = MIXER.lock();
    if g.is_none() {
        *g = Some(Mixer::default());
    }
    f(g.as_mut().expect("gerade angelegt"))
}

/// Eine Quelle anmelden. `None`, wenn kein Platz ist.
pub fn quelle_anmelden(name: String) -> Option<u32> {
    mit_mixer(|m| m.anmelden(name))
}

pub fn quelle_abmelden(id: u32) {
    mit_mixer(|m| m.abmelden(id));
}

/// Samples an eine Quelle anhaengen. Liefert `false`, wenn es die
/// Quelle nicht (mehr) gibt.
pub fn quelle_anhaengen(id: u32, samples: &[Sample]) -> bool {
    mit_mixer(|m| match m.quelle_mut(id) {
        Some(q) => {
            q.anhaengen(samples);
            true
        }
        None => false,
    })
}

/// Wie viele Samples bei einer Quelle noch warten.
pub fn quelle_wartend(id: u32) -> Option<usize> {
    mit_mixer(|m| m.quelle_mut(id).map(|q| q.wartend()))
}

/// Lautstaerke einer Quelle setzen.
pub fn quelle_lautstaerke(id: u32, wert: Promille) -> bool {
    mit_mixer(|m| match m.quelle_mut(id) {
        Some(q) => {
            q.lautstaerke = wert.min(VOLL);
            true
        }
        None => false,
    })
}

// ---------------------------------------------------------------------------
// GESAMTLAUTSTAERKE — das, was Systray und Einstellungen stellen
// ---------------------------------------------------------------------------

/// Die Gesamtlautstaerke in Promille (0..=1000).
pub fn lautstaerke() -> Promille {
    mit_mixer(|m| m.gesamt)
}

pub fn lautstaerke_setzen(wert: Promille) {
    mit_mixer(|m| m.gesamt = wert.min(VOLL));
}

pub fn stumm() -> bool {
    mit_mixer(|m| m.stumm)
}

pub fn stumm_setzen(wert: bool) {
    mit_mixer(|m| m.stumm = wert);
}

/// Lautstaerke in PROZENT — die Form, in der die Oberflaeche sie zeigt.
pub fn lautstaerke_prozent() -> u16 {
    lautstaerke() / 10
}

pub fn lautstaerke_prozent_setzen(prozent: u16) {
    lautstaerke_setzen(prozent.min(100) * 10);
}

// ---------------------------------------------------------------------------
// PUMPEN — auch synchron
// ---------------------------------------------------------------------------

/// EIN Durchgang: Stream verwalten, mischen, schieben.
///
/// ===================================================================
/// WARUM ES DAS SYNCHRON GEBEN MUSS
///
/// Solange ein SHELL-BEFEHL laeuft, kommt kein anderer Kernel-Task
/// dran — der kooperative Executor bekommt die CPU erst zurueck, wenn
/// der Befehl fertig ist (CLAUDE.md, Fenster-Regel). Ein `ton`, der
/// darauf wartet, dass der Mixer-Task seinen Puffer leert, wartet
/// deshalb fuer immer.
///
/// Dasselbe Problem hatten `ping`, `nslookup` und `hole` mit dem
/// Netz-Stack, und es hat dieselbe Loesung: Wer synchron wartet, PUMPT
/// SELBST. `audio_task` und diese Funktion rufen denselben Code.
pub fn pumpen(lief: &mut bool) {
    let quellen = mit_mixer(|m| {
        m.leere_entfernen();
        m.anzahl()
    });

    if quellen == 0 {
        if *lief {
            super::hda::mit_hda(|h| {
                if let Some(hda) = h {
                    hda.stoppen();
                }
            });
            *lief = false;
        }
        return;
    }

    if !*lief {
        super::hda::mit_hda(|h| {
            if let Some(hda) = h {
                hda.leeren();
                let _ = hda.starten();
            }
        });
        *lief = true;
    }

    // ERST PLATZ HOLEN, DANN MISCHEN — `mischen` VERBRAUCHT die
    // Samples seiner Quellen. Wer erst mischt und dann merkt, dass der
    // Ringpuffer voll ist, hat sie weggeworfen, und das hoert man als
    // Aussetzer.
    let platz = super::hda::mit_hda(|h| match h {
        Some(hda) => hda.freie_frames(),
        None => 0,
    });
    let nehmen = platz.min(STUECK_FRAMES);
    if nehmen == 0 {
        return;
    }
    let mut stueck = [0 as Sample; STUECK_FRAMES * KANAELE];
    let scheibe = &mut stueck[..nehmen * KANAELE];
    mit_mixer(|m| {
        m.mischen(scheibe);
    });
    super::hda::mit_hda(|h| {
        if let Some(hda) = h {
            hda.schreiben(scheibe);
        }
    });
}

/// Ob der Stream gerade laeuft — fuer synchrone Pumper, die sich
/// keinen eigenen Zustand merken wollen.
static LIEF: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Ein Durchgang mit dem GEMEINSAMEN Laufzustand.
///
/// Der Zustand liegt global, damit Task und synchroner Pumper sich
/// nicht gegenseitig den Stream an- und ausschalten.
pub fn pumpen_global() {
    use core::sync::atomic::Ordering;
    let mut lief = LIEF.load(Ordering::Relaxed);
    pumpen(&mut lief);
    LIEF.store(lief, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// DER PUMP-TASK
// ---------------------------------------------------------------------------

/// Schiebt gemischte Samples in die Hardware.
pub async fn audio_task() {
    if !super::vorhanden() {
        return;
    }
    serial_println!(
        "[audio] Mixer-Task laeuft ({} Frames je Durchgang).",
        STUECK_FRAMES
    );
    loop {
        pumpen_global();
        zeit::warte_ms(4).await;
    }
}
