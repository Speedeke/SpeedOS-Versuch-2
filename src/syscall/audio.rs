// syscall::audio — Tonausgabe fuer Ring-3-Programme
//
// ===========================================================================
// DASSELBE MUSTER WIE DIE FENSTER-SYSCALLS (Serie 8, Teil 1)
//
// **Eine Tonquelle ist ein HANDLE.** Daraus folgt von selbst, was man
// sonst einzeln bauen muesste:
//
//   * aus einem fremden Prozess unerreichbar (die Handle-Tabelle ist
//     pro Prozess),
//   * mit `schliesse` schliessbar wie jedes andere Handle,
//   * und der `Drop` der Handle-Tabelle meldet die Quelle beim
//     Prozess-Ende automatisch ab — auch nach einem Absturz.
//
// Kein Pfad kann es vergessen. Genau dieselbe Ueberlegung wie beim
// Fenster.
//
// ===========================================================================
// DIE COPY-IN-DISZIPLIN GILT UNVERAENDERT (Dauerregel I)
//
// Der Prozess uebergibt einen Zeiger auf PCM-Daten. Der Kernel folgt
// ihm NIE direkt: `ring3::copy_in` prueft dreistufig (User-Bereich,
// gemappt und USER_ACCESSIBLE im Adressraum DES AUFRUFERS) und
// KOPIERT. Ein Programm kann uns damit weder fremden Speicher
// vorlesen lassen noch uns zum Absturz bringen.
//
// ===========================================================================
// WARUM DIE SAMPLES KOPIERT WERDEN UND NICHT GETEILT
//
// Geteilter Speicher waere schneller — und er kostet dieselbe
// Sicherheitszusage wie beim Fenster: Dieselbe Seite laege in zwei
// Adressraeumen, und „pruefen, dann kopieren" gaelte nicht mehr. Bei
// 48 kHz Stereo sind es 192 KiB je Sekunde; das ist Kopierarbeit, die
// nicht auffaellt.

use super::handle::{self, KernelObjekt};
use super::{Fehler, SysErgebnis};
use crate::audio::dienst;
use crate::ring3;

/// Groesste Menge, die ein Prozess in EINEM Aufruf uebergeben darf.
///
/// 64 KiB sind rund 170 ms Ton — genug, dass ein Programm nicht im
/// Millisekundentakt aufrufen muss, und wenig genug, dass die Kopie im
/// Syscall keine spuerbare Pause erzeugt. Dieselbe Groesse wie
/// `MAX_PUFFER` bei den Pipes, und aus demselben Grund.
pub const MAX_AUDIO_BYTES: u64 = 64 * 1024;

/// Wie viele Samples eine Quelle hoechstens vorhalten darf.
///
/// **DIE WICHTIGE GRENZE.** Ohne sie koennte ein Programm in einer
/// Schleife schreiben, ohne je zu warten — der Kernel-Heap liefe voll,
/// und zwar durch einen ganz gewoehnlichen Ring-3-Prozess. Bei
/// 480 000 Samples sind es rund fuenf Sekunden Vorlauf; wer mehr
/// schicken will, bekommt `Belegt` und soll warten.
pub const MAX_VORLAUF_SAMPLES: usize = 480_000;

/// `audio_oeffnen()` — eine Tonquelle anmelden. Liefert ein Handle.
pub fn sys_oeffnen() -> SysErgebnis {
    if !crate::audio::vorhanden() {
        return Err(Fehler::NichtKonfiguriert);
    }
    let pid = crate::scheduler::aktuelle_pid();
    let id = dienst::quelle_anmelden(alloc::format!("PID {}", pid)).ok_or(Fehler::Belegt)?;
    match handle::einfuegen_aktuell(KernelObjekt::AudioQuelle(id)) {
        Ok(h) => Ok(h),
        Err(f) => {
            // DIE QUELLE WIEDER ABMELDEN. Ohne das bliebe sie im Mixer
            // stehen, obwohl niemand sie je erreichen kann — und der
            // Stream liefe fuer immer weiter.
            dienst::quelle_abmelden(id);
            Err(f)
        }
    }
}

/// `audio_schreiben(handle, ptr, laenge)` — PCM anhaengen.
///
/// `laenge` ist in BYTES; die Daten sind 16-Bit-Samples, verschraenkt
/// L/R. Liefert die Zahl der uebernommenen Bytes.
pub fn sys_schreiben(h: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let id = audio_handle(h)?;
    if laenge == 0 {
        return Ok(0);
    }
    if laenge > MAX_AUDIO_BYTES {
        return Err(Fehler::UngueltigesArgument);
    }
    // Ein halbes Sample gibt es nicht — eine ungerade Laenge ist ein
    // Fehler und keine Rundungsaufgabe.
    if !laenge.is_multiple_of(2) {
        return Err(Fehler::UngueltigesArgument);
    }

    // GEGENDRUCK. Ist der Vorlauf voll, wird NICHTS uebernommen und der
    // Prozess bekommt `Belegt` — er soll warten, statt uns den Heap zu
    // fuellen. Das ist dieselbe Haltung wie bei einer vollen Pipe, nur
    // ohne Blockieren: Ein blockierender Audio-Schreibaufruf waere ein
    // Programm, das im Syscall haengt, waehrend sein Ton laeuft.
    match dienst::quelle_wartend(id) {
        Some(wartend) if wartend >= MAX_VORLAUF_SAMPLES => return Err(Fehler::Belegt),
        Some(_) => {}
        None => return Err(Fehler::UngueltigerHandle),
    }

    // DER GEPRUEFTE WEG — nie direkt dereferenzieren (Dauerregel I).
    let bytes = ring3::copy_in(ptr, laenge as usize).map_err(Fehler::von_copy)?;

    // Bytes zu Samples. `chunks_exact` laesst ein einzelnes Restbyte
    // liegen; die Laenge ist oben schon als gerade geprueft, das ist
    // der zweite Riegel.
    let samples: alloc::vec::Vec<i16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| i16::from_le_bytes(*p))
        .collect();
    let anzahl = samples.len();
    if !dienst::quelle_anhaengen(id, &samples) {
        return Err(Fehler::UngueltigerHandle);
    }
    Ok(anzahl as u64 * 2)
}

/// `audio_status(handle)` — wie viele Samples noch warten.
///
/// Das ist die Zahl, an der ein Programm seine Fortschrittsanzeige
/// aufhaengt und an der es merkt, wann es nachlegen muss. 0 heisst:
/// alles abgespielt.
pub fn sys_status(h: u64) -> SysErgebnis {
    let id = audio_handle(h)?;
    match dienst::quelle_wartend(id) {
        Some(wartend) => Ok(wartend as u64),
        None => Err(Fehler::UngueltigerHandle),
    }
}

/// `audio_lautstaerke(handle, promille)` — Lautstaerke DIESER Quelle.
///
/// Die GESAMT-Lautstaerke stellt der Benutzer (Systray), nicht das
/// Programm. Ein Programm, das die Systemlautstaerke hochdrehen
/// koennte, waere eine Zumutung — deshalb geht das ueber diesen
/// Syscall ausdruecklich NICHT.
pub fn sys_lautstaerke(h: u64, promille: u64) -> SysErgebnis {
    let id = audio_handle(h)?;
    if promille > 1000 {
        return Err(Fehler::UngueltigesArgument);
    }
    if !dienst::quelle_lautstaerke(id, promille as u16) {
        return Err(Fehler::UngueltigerHandle);
    }
    Ok(0)
}

/// Das Handle in eine Quellen-Id aufloesen.
fn audio_handle(h: u64) -> Result<u32, Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(h)? {
        KernelObjekt::AudioQuelle(id) => Ok(*id),
        // Ein Socket ist keine Tonquelle. TYP-Fehler, nicht
        // „ungueltig" — das Handle existiert ja.
        _ => Err(Fehler::FalscherHandleTyp),
    })?
}
