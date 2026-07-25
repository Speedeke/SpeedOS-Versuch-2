// syscall/datei.rs — Die Datei-Syscalls über das VFS (Serie 6, Teil 4)
//
// Hier zahlt sich die VFS-Naht aus Serie 3/4 aus: `FileSystem` arbeitet schon
// mit absoluten Pfaden, `read_at`/`write_at` liefern schon Byte-Zahlen statt
// Panics, `stat` liefert schon eine reine Datenstruktur, und `FsFehler` ist
// schon ein vollständiges Fehler-Enum. Es musste NICHTS an der VFS-API
// geändert werden — es kam nur die Grenze davor: copy-in für Pfade und Puffer,
// die Fehler-Abbildung und die Handle-Tabelle.
//
// ZWEI EHRLICHE FOLGEN unseres pfadbasierten VFS:
//
//  (1) Ein Datei-Handle merkt sich den PFAD, nicht ein Kernel-Objekt. Wird die
//      Datei zwischen `oeffne` und `lese_at` umbenannt oder gelöscht, liefert
//      das Handle `NichtGefunden`. POSIX-Semantik („das Handle hält den Inode
//      fest, auch wenn der Name verschwindet") gibt unser VFS nicht her — das
//      wäre eine Änderung am `FileSystem`-Trait, keine an der Syscall-Schicht.
//
//  (2) `oeffne` hat KEINE Dateiposition. Alle Zugriffe nennen ihren Offset
//      explizit (`lese_at`/`schreibe_at`). Das ist weniger bequem als
//      POSIX-`read`, aber ehrlicher: Eine Position im Kernel zu halten, wäre
//      Zustand, den unser VFS gar nicht braucht. `schreibe(handle, ...)` aus
//      Gruppe 0 hängt deshalb bewusst AN (die einzige sinnvolle Deutung eines
//      Strom-Schreibvorgangs ohne Position).

use super::handle::{
    self, KernelObjekt, MODUS_ABSCHNEIDEN, MODUS_ALLE, MODUS_ANLEGEN, MODUS_LESEN, MODUS_SCHREIBEN,
};
use super::{laenge_pruefen, mit_vfs, pfad_lesen, puffer_lesen, puffer_schreiben, Fehler, SysErgebnis};
use crate::fs::NodeTyp;

/// Die Metadaten-Struktur, die `stat` in den User-Puffer schreibt.
/// 32 Byte, feste Reihenfolge — das ist ABI (docs/syscalls.md).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatDaten {
    /// 0 = Datei, 1 = Verzeichnis.
    pub typ: u64,
    /// Größe in Bytes (Verzeichnisse: 0).
    pub groesse: u64,
    /// Sekunden seit dem 1.1.2000.
    pub erstellt: u64,
    pub geaendert: u64,
}

/// Ein Verzeichnis-Eintrag, wie `liste` ihn schreibt. 128 Byte, feste
/// Reihenfolge — ABI.
///
/// Namen über `NAME_MAX_ABI` Bytes werden ABGESCHNITTEN, aber `name_laenge`
/// nennt die ECHTE Länge: Der Aufrufer merkt also, dass er etwas verpasst,
/// statt stillschweigend einen falschen Namen zu bekommen.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirEintragDaten {
    pub typ: u64,
    pub groesse: u64,
    pub name_laenge: u64,
    pub name: [u8; NAME_MAX_ABI],
}

/// Platz für den Namen in einem `DirEintragDaten` (128 - 3*8).
pub const NAME_MAX_ABI: usize = 104;
/// Größe eines Verzeichnis-Eintrags im Puffer.
pub const DIR_EINTRAG_GROESSE: usize = core::mem::size_of::<DirEintragDaten>();

impl Default for DirEintragDaten {
    fn default() -> Self {
        DirEintragDaten {
            typ: 0,
            groesse: 0,
            name_laenge: 0,
            name: [0; NAME_MAX_ABI],
        }
    }
}

/// Wandelt einen Node-Typ in seine ABI-Zahl.
fn typ_zahl(typ: NodeTyp) -> u64 {
    match typ {
        NodeTyp::Datei => 0,
        NodeTyp::Verzeichnis => 1,
    }
}

/// Serialisiert eine Struktur in Bytes (für copy-out).
///
/// # Safety
/// `T` muss `repr(C)` sein und darf keine Padding-Löcher mit undefiniertem
/// Inhalt haben — unsere beiden ABI-Strukturen bestehen nur aus u64 und
/// u8-Arrays und erfüllen das (der Test unten nagelt die Größen fest).
fn als_bytes<T>(wert: &T) -> &[u8] {
    // unsafe: siehe # Safety — nur Lesezugriff auf eine gültige Struktur.
    unsafe { core::slice::from_raw_parts((wert as *const T) as *const u8, size_of::<T>()) }
}

use core::mem::size_of;

// ---------------------------------------------------------------------------
// oeffne / schliesse
// ---------------------------------------------------------------------------

/// `oeffne(pfad_ptr, pfad_len, modus)` -> Handle.
///
/// Der Modus wird VOLLSTÄNDIG geprüft: Unbekannte Bits sind ein Fehler (statt
/// stillschweigend ignoriert zu werden), und ohne Lese- ODER Schreibrecht
/// wäre das Handle sinnlos.
pub fn sys_oeffne(pfad_ptr: u64, pfad_len: u64, modus: u64) -> SysErgebnis {
    if modus & !MODUS_ALLE != 0 {
        return Err(Fehler::UngueltigesArgument);
    }
    if modus & (MODUS_LESEN | MODUS_SCHREIBEN) == 0 {
        return Err(Fehler::UngueltigesArgument);
    }
    // Anlegen/Abschneiden ohne Schreibrecht ist ein Widerspruch.
    if modus & (MODUS_ANLEGEN | MODUS_ABSCHNEIDEN) != 0 && modus & MODUS_SCHREIBEN == 0 {
        return Err(Fehler::UngueltigesArgument);
    }
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;

    // Existenz und Typ prüfen — und bei ANLEGEN gegebenenfalls erzeugen.
    let vorhanden = mit_vfs(|fs| Ok(fs.node_typ(&pfad).ok()))?;
    match vorhanden {
        Some(NodeTyp::Verzeichnis) => return Err(Fehler::KeineDatei),
        Some(NodeTyp::Datei) => {
            if modus & MODUS_ABSCHNEIDEN != 0 {
                // Auf Länge 0 bringen = leer überschreiben.
                let leer_pfad = pfad.clone();
                mit_vfs(move |fs| fs.schreiben(&leer_pfad, &[]))?;
            }
        }
        None => {
            if modus & MODUS_ANLEGEN == 0 {
                return Err(Fehler::NichtGefunden);
            }
            let neu_pfad = pfad.clone();
            mit_vfs(move |fs| fs.schreiben(&neu_pfad, &[]))?;
        }
    }
    handle::einfuegen_aktuell(KernelObjekt::Datei { pfad, modus })
}

// ---------------------------------------------------------------------------
// lese_at / schreibe_at
// ---------------------------------------------------------------------------

/// `lese_at(handle, offset, ptr, len)` -> gelesene Bytes (0 = Dateiende).
///
/// Ablauf mit Bedacht: Erst Handle und Rechte prüfen, dann die Länge deckeln,
/// dann in einen KERNEL-Puffer lesen — und erst danach per copy-out in den
/// User-Puffer. Nie direkt in User-Speicher lesen: Der Zielbereich wird von
/// `copy_out` geprüft, und schlägt die Prüfung fehl, hat der Prozess NICHTS
/// bekommen (statt eines halb gefüllten Puffers).
pub fn sys_lese_at(h: u64, offset: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let (pfad, modus) = handle::datei_handle(h)?;
    if modus & MODUS_LESEN == 0 {
        return Err(Fehler::NichtUnterstuetzt);
    }
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(0);
    }
    // Der Zielbereich muss beschreibbar sein — VOR der (teuren) Leseoperation
    // prüfen, damit ein kaputter Zeiger keine Platten-I/O auslöst.
    crate::ring3::user_bereich_pruefen(ptr, laenge, true).map_err(Fehler::von_copy)?;

    let offset = usize_aus(offset)?;
    let mut puffer = alloc::vec![0u8; laenge];
    let gelesen = mit_vfs(|fs| fs.read_at(&pfad, offset, &mut puffer))?;
    puffer_schreiben(ptr, &puffer[..gelesen])?;
    Ok(gelesen as u64)
}

/// `schreibe_at(handle, offset, ptr, len)` -> geschriebene Bytes.
pub fn sys_schreibe_at(h: u64, offset: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let (pfad, modus) = handle::datei_handle(h)?;
    if modus & MODUS_SCHREIBEN == 0 {
        return Err(Fehler::NurLesen);
    }
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(0);
    }
    let offset = usize_aus(offset)?;
    let daten = puffer_lesen(ptr, laenge as u64)?;
    let geschrieben = mit_vfs(|fs| fs.write_at(&pfad, offset, &daten))?;
    Ok(geschrieben as u64)
}

/// Ein Offset aus dem User-Space in einen `usize` — mit Deckel.
///
/// Auf 64 Bit ist usize == u64, die Umwandlung könnte also nie fehlschlagen.
/// Trotzdem wird gedeckelt: Ein absurder Offset (u64::MAX) würde im VFS zu
/// riesigen Lücken-Füllungen führen. 1 GiB ist weit über allem, was unsere
/// Dateisysteme können (SpeedFS: ~4 MiB pro Datei).
fn usize_aus(wert: u64) -> Result<usize, Fehler> {
    const MAX_OFFSET: u64 = 1024 * 1024 * 1024;
    if wert > MAX_OFFSET {
        return Err(Fehler::ZuGross);
    }
    Ok(wert as usize)
}

// ---------------------------------------------------------------------------
// stat / liste
// ---------------------------------------------------------------------------

/// `stat(pfad_ptr, pfad_len, ziel_ptr)` — schreibt `StatDaten` (32 Byte).
pub fn sys_stat(pfad_ptr: u64, pfad_len: u64, ziel_ptr: u64) -> SysErgebnis {
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;
    // Zielbereich VOR der Operation prüfen.
    crate::ring3::user_bereich_pruefen(ziel_ptr, size_of::<StatDaten>(), true)
        .map_err(Fehler::von_copy)?;
    let meta = mit_vfs(|fs| fs.stat(&pfad))?;
    let daten = StatDaten {
        typ: typ_zahl(meta.typ),
        groesse: meta.groesse as u64,
        erstellt: meta.erstellt,
        geaendert: meta.geaendert,
    };
    puffer_schreiben(ziel_ptr, als_bytes(&daten))?;
    Ok(size_of::<StatDaten>() as u64)
}

/// `liste(pfad_ptr, pfad_len, ziel_ptr, ziel_len)` -> Anzahl Einträge im
/// Verzeichnis.
///
/// Die Rückgabe ist die GESAMTZAHL, nicht die Zahl der geschriebenen Einträge:
/// So merkt der Aufrufer, dass sein Puffer zu klein war, und kann mit einem
/// größeren wiederkommen. (Ein Verzeichnis-Iterator mit Zustand im Kernel
/// wäre die Alternative — mehr Zustand für keinen Gewinn bei unseren Größen.)
pub fn sys_liste(pfad_ptr: u64, pfad_len: u64, ziel_ptr: u64, ziel_len: u64) -> SysErgebnis {
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;
    let ziel_len = laenge_pruefen(ziel_len)?;
    let platz = ziel_len / DIR_EINTRAG_GROESSE;
    if platz > 0 {
        crate::ring3::user_bereich_pruefen(ziel_ptr, platz * DIR_EINTRAG_GROESSE, true)
            .map_err(Fehler::von_copy)?;
    }
    let eintraege = mit_vfs(|fs| fs.liste(&pfad))?;

    // Nur so viele schreiben, wie hineinpassen — der Rest bleibt unberührt.
    let mut bytes = alloc::vec::Vec::with_capacity(platz.min(eintraege.len()) * DIR_EINTRAG_GROESSE);
    for eintrag in eintraege.iter().take(platz) {
        let mut daten = DirEintragDaten {
            typ: typ_zahl(eintrag.typ),
            groesse: eintrag.groesse as u64,
            name_laenge: eintrag.name.len() as u64,
            ..DirEintragDaten::default()
        };
        let namen_bytes = eintrag.name.as_bytes();
        let kopieren = namen_bytes.len().min(NAME_MAX_ABI);
        daten.name[..kopieren].copy_from_slice(&namen_bytes[..kopieren]);
        bytes.extend_from_slice(als_bytes(&daten));
    }
    if !bytes.is_empty() {
        puffer_schreiben(ziel_ptr, &bytes)?;
    }
    Ok(eintraege.len() as u64)
}

// ---------------------------------------------------------------------------
// loesche / umbenenne / mkdir
// ---------------------------------------------------------------------------

/// `loesche(pfad_ptr, pfad_len)` — Datei oder LEERES Verzeichnis.
pub fn sys_loesche(pfad_ptr: u64, pfad_len: u64) -> SysErgebnis {
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;
    mit_vfs(move |fs| fs.loeschen(&pfad))?;
    Ok(0)
}

/// `umbenenne(von_ptr, von_len, nach_ptr, nach_len)` — die ATOMARE Primitive
/// des VFS, direkt durchgereicht.
pub fn sys_umbenenne(von_ptr: u64, von_len: u64, nach_ptr: u64, nach_len: u64) -> SysErgebnis {
    let von = pfad_lesen(von_ptr, von_len)?;
    let nach = pfad_lesen(nach_ptr, nach_len)?;
    mit_vfs(move |fs| fs.rename(&von, &nach))?;
    Ok(0)
}

/// `mkdir(pfad_ptr, pfad_len)`.
pub fn sys_mkdir(pfad_ptr: u64, pfad_len: u64) -> SysErgebnis {
    let pfad = pfad_lesen(pfad_ptr, pfad_len)?;
    mit_vfs(move |fs| fs.mkdir(&pfad))?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die ABI-STRUKTUREN sind ein Versprechen an compilierte Programme:
    /// Größen und Feld-Offsets dürfen sich nicht verschieben.
    #[test_case]
    fn test_abi_strukturen_stabil() {
        use core::mem::offset_of;
        assert_eq!(size_of::<StatDaten>(), 32);
        assert_eq!(offset_of!(StatDaten, typ), 0);
        assert_eq!(offset_of!(StatDaten, groesse), 8);
        assert_eq!(offset_of!(StatDaten, erstellt), 16);
        assert_eq!(offset_of!(StatDaten, geaendert), 24);

        assert_eq!(DIR_EINTRAG_GROESSE, 128);
        assert_eq!(offset_of!(DirEintragDaten, typ), 0);
        assert_eq!(offset_of!(DirEintragDaten, groesse), 8);
        assert_eq!(offset_of!(DirEintragDaten, name_laenge), 16);
        assert_eq!(offset_of!(DirEintragDaten, name), 24);
        assert_eq!(NAME_MAX_ABI, 104);
        // als_bytes liefert genau die Struktur-Bytes.
        assert_eq!(als_bytes(&StatDaten::default()).len(), 32);
        assert_eq!(als_bytes(&DirEintragDaten::default()).len(), 128);
        // Typ-Zahlen (ABI).
        assert_eq!(typ_zahl(NodeTyp::Datei), 0);
        assert_eq!(typ_zahl(NodeTyp::Verzeichnis), 1);
    }

    /// `oeffne` prüft den Modus VOLLSTÄNDIG — und zwar bevor irgendein Pfad
    /// gelesen wird (deshalb genügen hier Null-Zeiger).
    #[test_case]
    fn test_oeffne_modus_pruefung() {
        // Unbekannte Bits:
        assert_eq!(sys_oeffne(0, 4, 16), Err(Fehler::UngueltigesArgument));
        assert_eq!(sys_oeffne(0, 4, u64::MAX), Err(Fehler::UngueltigesArgument));
        // Weder lesen noch schreiben:
        assert_eq!(sys_oeffne(0, 4, 0), Err(Fehler::UngueltigesArgument));
        // Anlegen ohne Schreibrecht ist widersprüchlich:
        assert_eq!(
            sys_oeffne(0, 4, MODUS_LESEN | MODUS_ANLEGEN),
            Err(Fehler::UngueltigesArgument)
        );
        assert_eq!(
            sys_oeffne(0, 4, MODUS_LESEN | MODUS_ABSCHNEIDEN),
            Err(Fehler::UngueltigesArgument)
        );
        // Gültiger Modus, aber Null-Zeiger als Pfad -> Zeiger-Fehler
        // (der Modus war also in Ordnung).
        assert_eq!(sys_oeffne(0, 4, MODUS_LESEN), Err(Fehler::UngueltigerZeiger));
    }

    /// Offsets werden gedeckelt (kein 4-GiB-Lückenfüllen durch ein Argument).
    #[test_case]
    fn test_offset_deckel() {
        assert_eq!(usize_aus(0), Ok(0));
        assert_eq!(usize_aus(1024 * 1024 * 1024), Ok(1024 * 1024 * 1024));
        assert_eq!(usize_aus(1024 * 1024 * 1024 + 1), Err(Fehler::ZuGross));
        assert_eq!(usize_aus(u64::MAX), Err(Fehler::ZuGross));
    }

    /// Alle Datei-Syscalls mit FREMDEN/ungültigen Handles: sauberer Fehler,
    /// keine Panik. (Ohne laufenden Prozess gibt es überhaupt keine
    /// Handle-Tabelle — auch das muss ein Fehler sein, kein Absturz.)
    #[test_case]
    fn test_datei_syscalls_mit_boesen_handles() {
        for boese in [0u64, 1, 2, 3, 99, u64::MAX] {
            // Jeder dieser Aufrufe MUSS einen Fehler liefern und darf nicht
            // panicken — welchen genau, hängt davon ab, ob gerade ein Prozess
            // läuft (im Lib-Test: keiner).
            assert!(sys_lese_at(boese, 0, 0, 16).is_err());
            assert!(sys_schreibe_at(boese, 0, 0, 16).is_err());
            assert!(handle::sys_schliesse(boese).is_err());
        }
    }
}
