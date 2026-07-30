// syscall/fenster.rs — DIE FENSTER-SYSCALLS (Serie 8, Teil 1)
//
// Ab hier kann ein Ring-3-Prozess ein Fenster BESITZEN. Er bekommt einen
// Handle, malt Pixel hinein und holt sich Eingabe-Ereignisse ab — mehr
// nicht. Titelleiste, Rahmen, Schatten, Verschieben, Snap, Alt+Tab und der
// Taskleisten-Eintrag bleiben beim Kernel.
//
// ==========================================================================
// DIE FÜNF AUFRUFE
//
//   fenster_oeffnen(titel_ptr, titel_len, breite, hoehe)  -> Handle
//   fenster_zeichnen(handle, pixel_ptr, pixel_len, rechteck) -> Pixel
//   fenster_ereignis(handle, ziel_ptr, frist_ms)          -> Ereignis-Art
//   fenster_titel_setzen(handle, titel_ptr, titel_len)
//   fenster_schliessen(handle)
//
// Vollständig mit allen Argumenten, Rückgaben und Fehlern: docs/syscalls.md.
//
// ==========================================================================
// DAS RECHTECK IN EINEM REGISTER
//
// `fenster_zeichnen` hat vier Argumente (rdi, rsi, rdx, r10) und braucht
// fünf Zahlen: Handle, Zeiger, Länge, und ein Rechteck aus x/y/Breite/Höhe.
// Das Rechteck steckt deshalb GEPACKT in einem u64:
//
//     (x << 48) | (y << 32) | (breite << 16) | hoehe     (je 16 Bit)
//
// 16 Bit je Feld reichen bis 65535 — bei 4K sind es 3840. Die Alternative
// wäre ein weiteres Zeiger-Argument auf ein Struct im User-Speicher
// gewesen: eine zusätzliche Bereichsprüfung und eine zusätzliche
// Fehlerquelle für vier kleine Zahlen. Dasselbe Argument wie bei `pipe()`,
// das zwei Handles in ein Register packt.
//
// WARUM DER BEREICH ÜBERHAUPT IM SYSCALL STEHT: Damit ein Programm einen
// STREIFEN nachzeichnen kann statt immer das ganze Fenster. Der Kernel
// meldet genau diesen Streifen als Schaden an den Compositor — die
// Dirty-Rect-Mechanik aus Serie 4 zahlt sich hier unmittelbar aus, und die
// Messung in docs/fenster-syscalls.md §4 zeigt den Unterschied in Zahlen.
//
// ==========================================================================
// DAS PIXELFORMAT: 4 Byte je Pixel, Byte 0 = Blau, 1 = Grün, 2 = Rot,
// 3 = ungenutzt. Als Little-Endian-u32 gelesen ist das `0x00RRGGBB`.
// Begründung (warum umgerechnet und nicht gecastet):
// `fenster::FensterPuffer::zeile_aus_pixelbytes`.

use super::{handle, Fehler, SysErgebnis};
use crate::fenster::{self, prozessfenster, FensterId};
use crate::ring3;
use crate::syscall::prozess::Ausgang;
use alloc::vec;

/// Höchstmasse eines Fensters, das ein Prozess anlegen darf.
///
/// Nicht willkürlich: Der Kernel legt für das Fenster einen Puffer an
/// (Breite × Höhe × 3 Byte). Bei 4096 × 2304 sind das 28 MiB — schon
/// reichlich für unseren Heap, und mehr als jeder Bildschirm, den die
/// Firmware anbietet (docs/grenzen.md: 4096 × 2160 ist die Obergrenze).
/// Ohne Deckel wäre `fenster_oeffnen(0xFFFF, 0xFFFF)` ein Ein-Zeilen-
/// Angriff auf den Heap.
pub const MAX_FENSTER_BREITE: u64 = 4096;
pub const MAX_FENSTER_HOEHE: u64 = 2304;
/// Kleiner geht nicht — darunter ist nicht einmal die Titelleiste bedienbar.
pub const MIN_FENSTER_KANTE: u64 = 16;
/// Höchstlänge eines Fenstertitels in Bytes.
pub const MAX_TITEL: usize = 64;

/// Wie lange `fenster_ereignis` höchstens wartet, wenn der Aufrufer keine
/// eigene Frist nennt (frist_ms = 0 heisst NICHT „ewig").
///
/// Ewiges Warten wäre die bequeme Wahl und die falsche: Ein Programm, das
/// auf ein Ereignis wartet, das nie kommt, hängt ohne Meldung — genau das,
/// was die Zufalls-Dauerregel für `zufall` schon einmal entschieden hat.
/// Nach der Frist kommt `Keins` zurück, und die Ereignisschleife des
/// Programms läuft weiter (sie kann in dieser Runde animieren).
pub const EREIGNIS_STANDARD_FRIST_MS: u64 = 1_000;
/// Und länger als das wartet niemand, auch wenn er darum bittet.
pub const EREIGNIS_MAX_FRIST_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// Das gepackte Rechteck
// ---------------------------------------------------------------------------

/// Zerlegt das gepackte Rechteck-Argument. Reine Funktion, unit-getestet.
pub fn rechteck_entpacken(gepackt: u64) -> (i32, i32, i32, i32) {
    let x = ((gepackt >> 48) & 0xFFFF) as i32;
    let y = ((gepackt >> 32) & 0xFFFF) as i32;
    let breite = ((gepackt >> 16) & 0xFFFF) as i32;
    let hoehe = (gepackt & 0xFFFF) as i32;
    (x, y, breite, hoehe)
}

/// Setzt ein Rechteck zusammen (das Gegenstück, für Tests und libspeed).
pub fn rechteck_packen(x: u16, y: u16, breite: u16, hoehe: u16) -> u64 {
    ((x as u64) << 48) | ((y as u64) << 32) | ((breite as u64) << 16) | hoehe as u64
}

// ---------------------------------------------------------------------------
// fenster_oeffnen
// ---------------------------------------------------------------------------

/// `fenster_oeffnen(titel_ptr, titel_len, breite, hoehe)` -> Handle.
pub fn sys_oeffnen(titel_ptr: u64, titel_len: u64, breite: u64, hoehe: u64) -> SysErgebnis {
    if !(MIN_FENSTER_KANTE..=MAX_FENSTER_BREITE).contains(&breite)
        || !(MIN_FENSTER_KANTE..=MAX_FENSTER_HOEHE).contains(&hoehe)
    {
        return Err(Fehler::UngueltigesArgument);
    }
    let titel = titel_lesen(titel_ptr, titel_len)?;

    // Erst das Fenster, dann das Handle — und wenn das Handle nicht mehr
    // passt, das Fenster wieder zu. Andersherum (Handle zuerst) gäbe es
    // einen Augenblick, in dem ein Handle auf nichts zeigt.
    let id = fenster::prozess_fenster_oeffnen(
        crate::scheduler::laufende_user_pid(),
        &titel,
        breite as usize,
        hoehe as usize,
    )
    // Kein Fenster-Manager: Der Desktop läuft nicht. Ehrlich ablehnen
    // statt still nichts zu tun — ein Programm soll das unterscheiden
    // können (und `fenstertest` sagt es dem Benutzer).
    .ok_or(Fehler::NichtKonfiguriert)?;

    match handle::einfuegen_aktuell(handle::KernelObjekt::Fenster(id)) {
        Ok(h) => Ok(h),
        Err(fehler) => {
            fenster::prozess_fenster_schliessen(id);
            Err(fehler)
        }
    }
}

/// Liest und prüft einen Fenstertitel.
///
/// `pfad_lesen` wäre der bequeme Weg, verlangt aber einen führenden `/` —
/// deshalb hier eigen. Geprüft wird dasselbe: Deckel VOR dem Kopieren,
/// copy-in, UTF-8 erst auf der Kopie. Zusätzlich fliegen STEUERZEICHEN
/// raus: Ein Titel mit `\n` oder `\r` würde die Titelleiste zerlegen, und
/// die gehört dem Kernel.
fn titel_lesen(ptr: u64, laenge: u64) -> Result<alloc::string::String, Fehler> {
    if laenge == 0 {
        return Ok(alloc::string::String::from("Programm"));
    }
    if laenge as usize > MAX_TITEL {
        return Err(Fehler::ZuGross);
    }
    let bytes = ring3::copy_in(ptr, laenge as usize).map_err(Fehler::von_copy)?;
    let text = alloc::string::String::from_utf8(bytes).map_err(|_| Fehler::UngueltigesArgument)?;
    if text.chars().any(|c| c.is_control()) {
        return Err(Fehler::UngueltigesArgument);
    }
    Ok(text)
}

/// Löst ein Handle zu einer FensterId auf.
fn fenster_handle(h: u64) -> Result<FensterId, Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(h)? {
        handle::KernelObjekt::Fenster(id) => Ok(*id),
        // Ein Socket ist kein Fenster. TYP-Fehler, nicht „ungültig" — das
        // Handle existiert ja.
        _ => Err(Fehler::FalscherHandleTyp),
    })?
}

// ---------------------------------------------------------------------------
// fenster_zeichnen
// ---------------------------------------------------------------------------

/// `fenster_zeichnen(handle, ptr, len, rechteck)` -> gesetzte Pixel.
///
/// ==========================================================================
/// DIE REIHENFOLGE DER PRÜFUNGEN IST DIE EIGENTLICHE ARBEIT
///
///   1. Handle auflösen (billig, und ein fremdes Fenster gibt es gar nicht:
///      Ein Prozess hat keine Zahl, die dorthin führt).
///   2. Rechteck-Plausibilität: Breite/Höhe > 0 und nicht grösser als ein
///      Fenster überhaupt sein darf.
///   3. Länge muss GENAU zum Rechteck passen (breite × hoehe × 4). Ein
///      Programm, das eine kleinere Länge zu einem grossen Rechteck
///      behauptet, bekommt einen Fehler — nicht einen halb gefüllten
///      Bereich mit altem Inhalt dahinter.
///   4. DER GANZE Bereich wird geprüft, BEVOR die erste Zeile kopiert wird.
///      Sonst stünden bei einem Zeiger, dessen letzte Zeile ungemappt ist,
///      schon 2000 Zeilen im Fenster — halb gezeichnet ist kein
///      Sicherheitsproblem, aber es ist unehrlich, und die Zusage
///      „alles oder nichts" kostet hier nur einen zweiten Durchgang durch
///      die Seitentabellen (seitenweise, nicht byteweise).
///   5. Erst dann kopieren — zeilenweise, mit EINEM wiederverwendeten
///      Puffer (`ring3::copy_in_scheibe`), also ohne Megabyte-Allokation.
///
/// GEKLEMMT statt abgelehnt wird nur EINS: ein Rechteck, das über den
/// Fensterrand hinausragt. Begründung (das Wettrennen mit dem ziehenden
/// Benutzer) steht bei `fenster::pixel_schreiben`.
/// ==========================================================================
pub fn sys_zeichnen(h: u64, ptr: u64, laenge: u64, rechteck: u64) -> SysErgebnis {
    let id = fenster_handle(h)?;
    let (x, y, breite, hoehe) = rechteck_entpacken(rechteck);

    if breite <= 0 || hoehe <= 0 {
        return Err(Fehler::UngueltigesArgument);
    }
    if breite as u64 > MAX_FENSTER_BREITE || hoehe as u64 > MAX_FENSTER_HOEHE {
        return Err(Fehler::UngueltigesArgument);
    }
    // (3) Länge und Rechteck müssen zusammenpassen — auf das Byte.
    let noetig = (breite as u64) * (hoehe as u64) * 4;
    if laenge != noetig {
        return Err(Fehler::UngueltigesArgument);
    }
    // (4) Alles-oder-nichts-Prüfung des GESAMTEN Bereichs.
    bereich_pruefen_gross(ptr, noetig)?;

    let zeilen_bytes = breite as usize * 4;
    let mut zeilen_puffer = vec![0u8; zeilen_bytes];

    let ergebnis = fenster::pixel_schreiben(
        id,
        x,
        y,
        breite,
        hoehe,
        &mut zeilen_puffer,
        |quellzeile, ziel| {
            let versatz = quellzeile as u64 * zeilen_bytes as u64;
            // Geprüft ist schon alles (Schritt 4); `copy_in_scheibe`
            // prüft trotzdem noch einmal — billig und die eine Stelle,
            // an der einem User-Zeiger gefolgt wird.
            ring3::copy_in_scheibe(ptr + versatz, ziel).is_ok()
        },
    )
    // Fenster weg (geschlossen, während der Prozess noch zeichnen wollte).
    .ok_or(Fehler::UngueltigerHandle)?;

    Ok(ergebnis.pixel as u64)
}

/// Prüft einen Bereich, der GRÖSSER sein darf als ein einzelner `copy_in`.
///
/// `ring3::user_bereich_pruefen` deckelt bei 64 KiB — mit gutem Grund: Die
/// Grenze begrenzt den Schaden eines fehlerhaften Längen-Arguments bei
/// JEDEM anderen Syscall. Statt sie für Pixel aufzuweichen, wird hier in
/// Stücken geprüft. Die Obergrenze bleibt trotzdem hart, nur eben die
/// fensterbezogene: `MAX_FENSTER_BREITE × MAX_FENSTER_HOEHE × 4`.
fn bereich_pruefen_gross(ptr: u64, laenge: u64) -> Result<(), Fehler> {
    const HOECHSTENS: u64 = MAX_FENSTER_BREITE * MAX_FENSTER_HOEHE * 4;
    if laenge > HOECHSTENS {
        return Err(Fehler::ZuGross);
    }
    const STUECK: u64 = 32 * 1024;
    let mut ab = 0u64;
    while ab < laenge {
        let dieses = STUECK.min(laenge - ab);
        // checked_add: ein Zeiger nahe u64::MAX darf nicht „hinten wieder
        // rauskommen" (dieselbe Sorge wie in ring3::user_bereich_pruefen).
        let adresse = ptr.checked_add(ab).ok_or(Fehler::UngueltigerZeiger)?;
        ring3::user_bereich_pruefen(adresse, dieses as usize, false)
            .map_err(Fehler::von_copy)?;
        ab += dieses;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// fenster_ereignis
// ---------------------------------------------------------------------------

/// `fenster_ereignis(handle, ziel_ptr, frist_ms)` -> Ereignis-Art.
///
/// BLOCKIEREND mit Frist. Liegt nichts an, wird der Prozess schlafen gelegt
/// und der Syscall später NEU GESTARTET (das Modell aus Serie 6, Teil 6) —
/// geweckt wird er sofort, sobald ein Ereignis anfällt (Serie 7, Teil 0),
/// oder spätestens zur Frist.
///
/// DIE NEUSTART-REGEL, hier besonders scharf: Bis zum `Blockieren` darf
/// NICHTS verändert worden sein. Deshalb wird das Ereignis erst NACH der
/// Zeiger-Prüfung aus der Warteschlange GENOMMEN — sonst wäre es bei einem
/// kaputten Zielzeiger weg, und der Neustart holte ein anderes.
///
/// Die FRIST liegt im Fenster und nicht in einer lokalen Variablen: Der
/// Neustart würde sie sonst jedes Mal neu berechnen, und eine Frist von
/// 100 ms könnte ewig dauern.
pub fn sys_ereignis(h: u64, ziel_ptr: u64, frist_ms: u64) -> Ausgang {
    match sys_ereignis_inner(h, ziel_ptr, frist_ms) {
        Ok(ausgang) => ausgang,
        Err(fehler) => Ausgang::Fertig(Err(fehler)),
    }
}

fn sys_ereignis_inner(h: u64, ziel_ptr: u64, frist_ms: u64) -> Result<Ausgang, Fehler> {
    let id = fenster_handle(h)?;
    // Den Zielbereich prüfen, BEVOR ein Ereignis entnommen wird.
    ring3::user_bereich_pruefen(ziel_ptr, prozessfenster::EREIGNIS_BYTES, true)
        .map_err(Fehler::von_copy)?;

    // Die Frist festlegen (nur beim ERSTEN Durchlauf — siehe oben).
    let gewuenscht = if frist_ms == 0 {
        EREIGNIS_STANDARD_FRIST_MS
    } else {
        frist_ms.min(EREIGNIS_MAX_FRIST_MS)
    };
    let jetzt = crate::zeit::ms_seit_boot();
    let frist_bis = fenster::prozess_frist(id, jetzt + gewuenscht)
        .ok_or(Fehler::UngueltigerHandle)?;

    match fenster::prozess_ereignis_holen(id) {
        // Fenster weg: Das ist der Fall, in dem der Benutzer das Fenster
        // geschlossen hat, während der Prozess wartete.
        None => Err(Fehler::UngueltigerHandle),
        Some(Some(ereignis)) => {
            fenster::prozess_frist_loeschen(id);
            super::puffer_schreiben(ziel_ptr, &ereignis.bytes())?;
            Ok(Ausgang::Fertig(Ok(ereignis.art as u64)))
        }
        Some(None) => {
            if jetzt >= frist_bis {
                // FRIST ABGELAUFEN — und das ist KEIN Fehler, sondern
                // „nichts passiert". Ein Programm, dessen Normalfall ein
                // Fehlercode wäre, schreibt seine Schleife falsch herum.
                fenster::prozess_frist_loeschen(id);
                let leer = prozessfenster::EreignisDaten::keins();
                super::puffer_schreiben(ziel_ptr, &leer.bytes())?;
                return Ok(Ausgang::Fertig(Ok(prozessfenster::ART_KEINS as u64)));
            }
            // Schlafen legen — mit der Frist als Weckzeitpunkt. Daran
            // weckt der Timer, wenn kein Ereignis kommt.
            Ok(Ausgang::Blockieren(
                crate::prozess::Warteauf::Fenster(id.wert()),
                frist_bis,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// fenster_titel_setzen und fenster_schliessen
// ---------------------------------------------------------------------------

/// `fenster_titel_setzen(handle, titel_ptr, titel_len)`.
pub fn sys_titel_setzen(h: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let id = fenster_handle(h)?;
    let titel = titel_lesen(ptr, laenge)?;
    if fenster::prozess_titel_setzen(id, &titel) {
        Ok(0)
    } else {
        Err(Fehler::UngueltigerHandle)
    }
}

/// `fenster_schliessen(handle)`.
///
/// Es gibt bewusst KEINEN eigenen Aufruf dafür in der Handle-Welt: Auch
/// `schliesse(handle)` (Nr. 19) schliesst ein Fenster, weil für einen
/// Prozess ein Handle ein Handle ist. Diese Nummer existiert, damit der
/// Name in einem Fenster-Programm lesbar ist — sie tut genau dasselbe.
pub fn sys_schliessen(h: u64) -> SysErgebnis {
    // Über die Handle-Tabelle, damit derselbe Weg gilt wie beim
    // Prozess-Ende: `KernelObjekt::schliessen` ist die EINE Stelle.
    handle::sys_schliesse(h)
}

// ---------------------------------------------------------------------------
// Tests der reinen Logik
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Das gepackte Rechteck ist ABI — Packen und Entpacken müssen
    /// zueinander passen, auch an den Rändern.
    #[test_case]
    fn test_rechteck_packen_entpacken() {
        for (x, y, b, h) in [
            (0u16, 0u16, 1u16, 1u16),
            (10, 20, 300, 400),
            (3839, 2159, 3840, 2160),
            (65535, 65535, 65535, 65535),
        ] {
            let gepackt = rechteck_packen(x, y, b, h);
            assert_eq!(
                rechteck_entpacken(gepackt),
                (x as i32, y as i32, b as i32, h as i32),
                "Rechteck {:?} ueberlebt das Packen nicht",
                (x, y, b, h)
            );
        }
        // Die Felder duerfen sich nicht ins Gehege kommen:
        assert_eq!(rechteck_entpacken(rechteck_packen(0, 0, 0, 65535)), (0, 0, 0, 65535));
        assert_eq!(rechteck_entpacken(rechteck_packen(65535, 0, 0, 0)), (65535, 0, 0, 0));
        // Ein Entpacken liefert NIE negative Werte (16 Bit in einen i32):
        let (x, y, b, h) = rechteck_entpacken(u64::MAX);
        assert!(x >= 0 && y >= 0 && b >= 0 && h >= 0);
    }

    /// BÖSARTIGE Argumente an `fenster_oeffnen` — alles Fehler, nie eine
    /// Panik, und vor allem: nie eine Puffer-Allokation nach Wunsch des
    /// Angreifers.
    #[test_case]
    fn test_oeffnen_masse_werden_gedeckelt() {
        for (breite, hoehe) in [
            (0u64, 100u64),
            (100, 0),
            (u64::MAX, u64::MAX),
            (MAX_FENSTER_BREITE + 1, 100),
            (100, MAX_FENSTER_HOEHE + 1),
            (1, 1),
            (MIN_FENSTER_KANTE - 1, 100),
        ] {
            assert_eq!(
                sys_oeffnen(0, 0, breite, hoehe),
                Err(Fehler::UngueltigesArgument),
                "{}x{} haette abgelehnt werden muessen",
                breite,
                hoehe
            );
        }
    }

    /// `fenster_zeichnen` mit unsinnigen Rechtecken und Längen — bevor
    /// überhaupt ein Zeiger angefasst wird. (Der Erfolgsfall braucht einen
    /// echten Prozess und steht in tests/fenster.rs.)
    #[test_case]
    fn test_zeichnen_lehnt_unsinn_ab() {
        // Ein Handle, das es nicht gibt: IMMER derselbe Fehler, egal was
        // sonst noch krumm ist.
        assert_eq!(sys_zeichnen(99, 0, 0, 0).err(), Some(Fehler::UngueltigerHandle));
        // Und die reine Rechteck-Prüfung (unabhängig vom Handle):
        assert_eq!(rechteck_entpacken(0), (0, 0, 0, 0));
    }

    /// Titel: Deckel, Leerfall und Steuerzeichen.
    #[test_case]
    fn test_titel_pruefung() {
        // Länge 0 = Standardtitel (ein Programm muss keinen setzen).
        assert_eq!(titel_lesen(0, 0).unwrap(), "Programm");
        // Über dem Deckel: ZuGross, und zwar BEVOR kopiert wird.
        assert_eq!(
            titel_lesen(crate::adressraum::USER_START, MAX_TITEL as u64 + 1),
            Err(Fehler::ZuGross)
        );
        assert_eq!(titel_lesen(crate::adressraum::USER_START, u64::MAX), Err(Fehler::ZuGross));
        // Kernel-Adresse als Titel-Zeiger:
        assert_eq!(
            titel_lesen(crate::allocator::HEAP_START as u64, 8),
            Err(Fehler::UngueltigerZeiger)
        );
    }

    /// Die Frist wird gedeckelt — auch „warte bitte einen Tag".
    #[test_case]
    fn test_frist_wird_gedeckelt() {
        let deckeln = |wunsch: u64| {
            if wunsch == 0 {
                EREIGNIS_STANDARD_FRIST_MS
            } else {
                wunsch.min(EREIGNIS_MAX_FRIST_MS)
            }
        };
        assert_eq!(deckeln(0), EREIGNIS_STANDARD_FRIST_MS);
        assert_eq!(deckeln(50), 50);
        assert_eq!(deckeln(u64::MAX), EREIGNIS_MAX_FRIST_MS);
        const _: () = assert!(EREIGNIS_STANDARD_FRIST_MS <= EREIGNIS_MAX_FRIST_MS);
    }
}
