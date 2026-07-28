// syscall/prozess.rs — Prozesse reden miteinander (Serie 6, Teil 6)
//
// Die vier Syscalls, mit denen aus einzelnen Prozessen ein ZUSAMMENSPIEL
// wird: `pipe`, `starte`, `warte`, `beende` — plus `lese`, der Strom-
// Gegenpart zu `schreibe`.
//
// ==========================================================================
// DREI VON IHNEN KOENNEN BLOCKIEREN — und deshalb stehen sie hier und nicht
// in der Tabelle von `ausfuehren()`.
//
// Ein blockierender Syscall braucht den Trap-Rahmen: Er muss den Prozess
// schlafen legen UND `rip` um zwei Bytes zurückstellen, damit der Syscall
// nach dem Aufwachen von vorn läuft (die Begründung steht ausführlich an
// `scheduler::blockieren`). Deshalb liefern die Funktionen hier kein
// fertiges Ergebnis, sondern einen `Ausgang`: entweder „fertig, das ist das
// Ergebnis" oder „ich kann noch nicht — leg mich aus DIESEM Grund schlafen".
//
// Die eiserne Regel dabei: **Bis zum `Blockieren` darf nichts verändert
// worden sein.** Sonst passiert es beim Neustart ein zweites Mal.
// ==========================================================================

use super::handle::{self, KernelObjekt};
use super::{laenge_pruefen, pfad_lesen, puffer_lesen, puffer_schreiben, Fehler, SysErgebnis};
use crate::pipe::{self, PipeErgebnis};
use crate::prozess::Warteauf;
use crate::scheduler;

/// Wie ein (möglicherweise) blockierender Syscall ausgeht.
pub enum Ausgang {
    /// Fertig — Fehlercode und Ergebnis stehen fest.
    Fertig(SysErgebnis),
    /// Noch nicht möglich. Der Dispatcher legt den Prozess mit diesem Grund
    /// schlafen und lässt den Syscall danach neu laufen.
    Blockieren(Warteauf),
}

// ---------------------------------------------------------------------------
// pipe()
// ---------------------------------------------------------------------------

/// `pipe()` -> Lese-Handle | (Schreib-Handle << 32).
///
/// ZWEI HANDLES IN EINEM REGISTER: Die ABI hat genau ein Ergebnis-Register
/// (rdx). Der Alternativweg wäre ein copy-out in einen User-Puffer — also
/// ein Zeiger-Argument, eine Bereichsprüfung und eine mögliche Fehlerquelle
/// mehr, für zwei Zahlen, die zusammen nie über 64 bleiben (ein Handle ist
/// < 32, siehe `MAX_HANDLES`). Also gepackt; das ist ABI und steht in
/// docs/syscalls.md.
pub fn sys_pipe() -> SysErgebnis {
    let id = pipe::anlegen().ok_or(Fehler::KeinPlatz)?;

    // Beide Enden in die Handle-Tabelle des Prozesses. Schlägt eines fehl,
    // muss ALLES zurück — sonst bliebe eine Pipe ohne Besitzer liegen.
    let lese_handle = match handle::einfuegen_aktuell(KernelObjekt::PipeLesen(id)) {
        Ok(handle) => handle,
        Err(fehler) => {
            pipe::ende_schliessen(id, pipe::Ende::Lesen);
            pipe::ende_schliessen(id, pipe::Ende::Schreiben);
            return Err(fehler);
        }
    };
    let schreib_handle = match handle::einfuegen_aktuell(KernelObjekt::PipeSchreiben(id)) {
        Ok(handle) => handle,
        Err(fehler) => {
            // Das Lese-Handle ist schon vergeben — es wieder einzusammeln
            // schliesst auch sein Pipe-Ende (sys_schliesse tut genau das).
            let _ = handle::sys_schliesse(lese_handle);
            pipe::ende_schliessen(id, pipe::Ende::Schreiben);
            return Err(fehler);
        }
    };
    Ok(lese_handle | (schreib_handle << 32))
}

// ---------------------------------------------------------------------------
// lese()
// ---------------------------------------------------------------------------

/// `lese(handle, ptr, len)` -> Bytes (0 = Dateiende).
///
/// Der STROM-Gegenpart zu `schreibe`: kein Offset, weil eine Pipe keine
/// Position hat. Auf einer leeren Pipe BLOCKIERT er, solange es noch einen
/// Schreiber gibt — ist der letzte Schreiber weg, liefert er 0 (Dateiende).
/// Genau daran erkennt ein Programm wie `filter`, dass es fertig ist.
pub fn sys_lese(h: u64, ptr: u64, laenge: u64) -> Ausgang {
    let laenge = match laenge_pruefen(laenge) {
        Ok(laenge) => laenge,
        Err(fehler) => return Ausgang::Fertig(Err(fehler)),
    };
    if laenge == 0 {
        return Ausgang::Fertig(Ok(0));
    }
    let quelle = match handle::lese_quelle(h) {
        Ok(quelle) => quelle,
        Err(fehler) => return Ausgang::Fertig(Err(fehler)),
    };
    // Den Zielbereich VOR dem Lesen prüfen: Sonst hätten wir die Bytes aus
    // der Pipe genommen (sie sind dann WEG) und könnten sie nicht abliefern.
    if let Err(fehler) = crate::ring3::user_bereich_pruefen(ptr, laenge, true) {
        return Ausgang::Fertig(Err(Fehler::von_copy(fehler)));
    }

    match quelle {
        handle::LeseQuelle::Pipe(id) => {
            let mut puffer = alloc::vec![0u8; laenge];
            match pipe::lesen(id, &mut puffer) {
                PipeErgebnis::Bytes(0) => Ausgang::Fertig(Ok(0)), // Dateiende
                PipeErgebnis::Bytes(n) => {
                    match puffer_schreiben(ptr, &puffer[..n]) {
                        Ok(()) => Ausgang::Fertig(Ok(n as u64)),
                        Err(fehler) => Ausgang::Fertig(Err(fehler)),
                    }
                }
                // NICHTS wurde entnommen -> der Neustart ist gefahrlos.
                PipeErgebnis::Blockiert => Ausgang::Blockieren(Warteauf::PipeLesen(id)),
                PipeErgebnis::Abgebrochen => Ausgang::Fertig(Err(Fehler::Abgebrochen)),
                PipeErgebnis::Ungueltig => Ausgang::Fertig(Err(Fehler::UngueltigerHandle)),
            }
        }
        // Handle 0 ohne umgeleitete Pipe: Es gibt (noch) keine Tastatur-
        // Quelle für Prozesse. Ehrlich ablehnen statt ewig zu blockieren.
        handle::LeseQuelle::Eingabe => Ausgang::Fertig(Err(Fehler::NichtUnterstuetzt)),
        handle::LeseQuelle::Datei { pfad, .. } => {
            // Dateien haben eine Position — die kennt `lese` nicht. Wer aus
            // einer Datei liest, nimmt `lese_at`. Nur damit `lese` auf einem
            // Datei-Handle nicht schweigend etwas Falsches tut.
            let _ = pfad;
            Ausgang::Fertig(Err(Fehler::FalscherHandleTyp))
        }
    }
}

// ---------------------------------------------------------------------------
// Der Pipe-Zweig von schreibe()
// ---------------------------------------------------------------------------

/// `schreibe` auf ein Pipe-Handle. Voll -> blockieren, kein Leser mehr ->
/// `Abgebrochen` (das POSIX-EPIPE).
pub fn pipe_schreiben(id: pipe::PipeId, ptr: u64, laenge: usize) -> Ausgang {
    // ERST prüfen, ob überhaupt Platz ist — und zwar mit einer LEEREN Probe,
    // bevor wir 64 KiB aus dem User-Speicher kopieren. Beim Neustart wäre
    // die Kopie sonst jedes Mal umsonst gewesen.
    match pipe::schreiben(id, &[]) {
        PipeErgebnis::Abgebrochen => return Ausgang::Fertig(Err(Fehler::Abgebrochen)),
        PipeErgebnis::Ungueltig => return Ausgang::Fertig(Err(Fehler::UngueltigerHandle)),
        _ => {}
    }
    let daten = match puffer_lesen(ptr, laenge as u64) {
        Ok(daten) => daten,
        Err(fehler) => return Ausgang::Fertig(Err(fehler)),
    };
    match pipe::schreiben(id, &daten) {
        PipeErgebnis::Bytes(n) => Ausgang::Fertig(Ok(n as u64)),
        PipeErgebnis::Blockiert => Ausgang::Blockieren(Warteauf::PipeSchreiben(id)),
        PipeErgebnis::Abgebrochen => Ausgang::Fertig(Err(Fehler::Abgebrochen)),
        PipeErgebnis::Ungueltig => Ausgang::Fertig(Err(Fehler::UngueltigerHandle)),
    }
}

// ---------------------------------------------------------------------------
// warte()
// ---------------------------------------------------------------------------

/// `warte(pid)` -> Exit-Code des Kindes (`pid = 0` = irgendeines).
///
/// Das Ergebnis liegt schon beim ELTERNTEIL (siehe `Prozess::kinder_enden`)
/// — es gibt keinen Zombie, den man erst noch einsammeln müsste. Dieser
/// Syscall holt es dort ab; ist noch keines da und lebt das Kind noch,
/// blockiert er.
///
/// `Fehler::NichtGefunden`, wenn es kein solches Kind gibt: Darauf zu warten
/// wäre ein Hänger für immer, und das ist ein Programmierfehler, kein
/// Zustand.
pub fn sys_warte(pid: u64) -> Ausgang {
    if pid > u32::MAX as u64 {
        return Ausgang::Fertig(Err(Fehler::UngueltigesArgument));
    }
    let gesucht = pid as crate::prozess::Pid;
    match scheduler::kind_ende_abholen(gesucht) {
        Ok(Some((_, ende))) => Ausgang::Fertig(Ok(ende.code())),
        Ok(None) => Ausgang::Blockieren(Warteauf::Kind(gesucht)),
        Err(_) => Ausgang::Fertig(Err(Fehler::NichtGefunden)),
    }
}

// ---------------------------------------------------------------------------
// beende()
// ---------------------------------------------------------------------------

/// `beende(pid)` — beendet einen Prozess von aussen.
///
/// EHRLICHE GRENZE: Es gibt (noch) keine Rechte in SpeedOS, also darf jeder
/// Prozess jeden anderen beenden. Das ist keine Nachlässigkeit, sondern die
/// Folge davon, dass es keinen Benutzerbegriff gibt — ein Rechte-Modell ohne
/// Benutzer wäre Attrappe (docs/syscalls.md §10). Nur der KERNEL-Prozess ist
/// geschützt, denn ihn zu beenden hiesse, das System anzuhalten.
pub fn sys_beende(pid: u64) -> SysErgebnis {
    if pid > u32::MAX as u64 {
        return Err(Fehler::UngueltigesArgument);
    }
    let ziel = pid as crate::prozess::Pid;
    if ziel == crate::prozess::KERNEL_PID {
        return Err(Fehler::UngueltigesArgument);
    }
    if scheduler::beenden(ziel) {
        Ok(0)
    } else {
        Err(Fehler::NichtGefunden)
    }
}

// ---------------------------------------------------------------------------
// starte()
// ---------------------------------------------------------------------------

/// `starte(pfad_ptr, pfad_len, eingabe_handle, ausgabe_handle)` -> PID.
///
/// DIE HANDLE-WEITERGABE ist der eigentliche Inhalt dieses Syscalls: Die
/// beiden Handles werden zu den Standard-Handles 0 und 1 DES KINDES. Damit
/// baut ein Prozess eine Pipeline, ohne dass das Kind etwas davon wissen
/// muss — es schreibt auf Handle 1 wie immer, und dahinter steckt diesmal
/// eine Pipe statt des Bildschirms.
///
/// `u64::MAX` als Handle heisst „nicht umleiten, erben was der Kernel
/// vorgibt". Wir nehmen bewusst nicht 0 dafür: 0 IST ein gültiges Handle.
///
/// BEWUSSTE GRENZEN (keine POSIX-Nachbildung):
///  * Keine Argumente aus Ring 3. Ein Kind bekommt `argv[0]` = Dateiname,
///    mehr nicht. Argumente zu übergeben hiesse, ein Feld von Zeigern aus
///    dem User-Speicher zu prüfen und zu kopieren — machbar, aber es kauft
///    nichts, solange die Shell (Kernel) die Pipelines baut.
///  * Kein `fork`. Ein Prozess entsteht immer aus einer DATEI, nie als
///    Kopie. Das erspart uns Copy-on-Write und die halbe Semantik von Unix.
pub fn sys_starte(pfad_ptr: u64, pfad_len: u64, eingabe: u64, ausgabe: u64) -> SysErgebnis {
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;

    // Die weiterzugebenden Handles AUFLÖSEN, bevor irgendetwas gebaut wird.
    // Ein ungültiges Handle soll nicht erst auffallen, wenn der Adressraum
    // des Kindes schon steht.
    let erbe_eingabe = handle::erbe_aufloesen(eingabe)?;
    let erbe_ausgabe = handle::erbe_aufloesen(ausgabe)?;

    let eltern = scheduler::aktuelle_user_pid();
    let ergebnis = crate::prozess::prozess_starten_mit(
        &pfad,
        &[],
        eltern,
        erbe_eingabe,
        erbe_ausgabe,
        // Aus einem Syscall heraus darf `fs::mit_fs` nicht blockierend
        // genommen werden (Lock-Disziplin, syscall/mod.rs) — deshalb der
        // Weg über `mit_vfs` (try_lock + Wartefenster).
        true,
    );
    match ergebnis {
        Ok(pid) => Ok(pid as u64),
        Err(fehler) => Err(start_fehler_abbilden(fehler)),
    }
}

/// Bildet einen Start-Fehler auf die ABI ab.
fn start_fehler_abbilden(fehler: crate::prozess::StartFehler) -> Fehler {
    use crate::prozess::StartFehler as S;
    match fehler {
        S::Fs(fs_fehler) => Fehler::von_fs(fs_fehler),
        // WELCHER Formfehler im ELF steckt, ist für den Aufrufer nicht
        // handlungsleitend — die Datei ist einfach kein Programm.
        S::Elf(_) => Fehler::UngueltigesArgument,
        S::Argumente => Fehler::UngueltigesArgument,
        S::KeinSpeicher | S::TabelleVoll => Fehler::KeinPlatz,
    }
}
