// fs/mod.rs — Das virtuelle Dateisystem (VFS) von SpeedOS
//
// Architektur-Entscheidung (siehe auch CLAUDE.md):
// Shell und Kernel reden NIE direkt mit einem konkreten Dateisystem,
// sondern nur mit dem FileSystem-Trait. Heute steckt dahinter das
// RamFs (fs/ramfs.rs, rein im Arbeitsspeicher) — später sollen FAT32
// und ein eigenes Disk-Dateisystem exakt dieselbe Schnittstelle
// bedienen. Dann muss sich an Shell-Befehlen wie dir/type/copy
// NICHTS ändern, nur das gemountete Dateisystem wird ausgetauscht.
//
// Pfade: immer mit '/' getrennt, absolute Pfade beginnen mit '/'.
// Die Auflösung relativer Pfade (., .., aktuelles Verzeichnis)
// übernimmt pfad_aufloesen() — die Trait-Methoden bekommen bereits
// fertige absolute Pfade.

pub mod block;
pub mod ramfs;

pub use block::{BlockDevice, IoFehler, RamDisk};

use alloc::{boxed::Box, format, string::String, vec::Vec};
use spin::Mutex;

/// Was für ein Eintrag ist das?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeTyp {
    Datei,
    Verzeichnis,
}

/// Ein Eintrag in einer Verzeichnisliste (für dir, tree, Tab-Taste).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEintrag {
    pub name: String,
    pub typ: NodeTyp,
    /// Dateigröße in Bytes (bei Verzeichnissen 0).
    pub groesse: usize,
    /// Zuletzt geändert (Sekunden seit dem 1.1.2000, zeit-Epoche).
    pub geaendert: u64,
}

/// Das Ergebnis von stat(): alles, was das Dateisystem über einen
/// Eintrag weiß. Zeitstempel als Sekunden seit dem 1.1.2000 (dieselbe
/// Epoche wie zeit::sekunden_seit_2000 — anzeigen über
/// einstellungen::stempel_text, das den Anzeige-Offset einrechnet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadaten {
    pub typ: NodeTyp,
    /// Dateigröße in Bytes (bei Verzeichnissen 0).
    pub groesse: usize,
    pub erstellt: u64,
    pub geaendert: u64,
}

/// Alle Fehler, die Dateisystem-Operationen melden können.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsFehler {
    NichtGefunden,
    ExistiertBereits,
    KeinVerzeichnis,
    KeineDatei,
    VerzeichnisNichtLeer,
    UngueltigerPfad,
    NichtInitialisiert,
    /// Ein Blockgeräte-Fehler von unter dem Dateisystem (Disk-FS ab
    /// Serie 4; das RamFs erzeugt ihn nie). Er wird NIE verschluckt,
    /// sondern wandert bis in Shell-Ausgabe bzw. App-Dialog.
    Io(IoFehler),
}

impl FsFehler {
    /// Deutsche Fehlermeldung für Shell und Dialoge.
    pub fn meldung(&self) -> &'static str {
        match self {
            FsFehler::NichtGefunden => "Datei oder Verzeichnis nicht gefunden",
            FsFehler::ExistiertBereits => "existiert bereits",
            FsFehler::KeinVerzeichnis => "ein Pfad-Bestandteil ist kein Verzeichnis",
            FsFehler::KeineDatei => "das ist keine Datei, sondern ein Verzeichnis",
            FsFehler::VerzeichnisNichtLeer => "das Verzeichnis ist nicht leer",
            FsFehler::UngueltigerPfad => "ungueltiger Pfad",
            FsFehler::NichtInitialisiert => "kein Dateisystem gemountet",
            FsFehler::Io(io) => io.meldung(),
        }
    }
}

impl From<IoFehler> for FsFehler {
    fn from(io: IoFehler) -> Self {
        FsFehler::Io(io)
    }
}

pub type FsErgebnis<T> = Result<T, FsFehler>;

/// DIE Schnittstelle, die jedes Dateisystem von SpeedOS erfüllen muss.
/// Alle Pfade sind absolut und bereits normalisiert (kein . oder ..).
pub trait FileSystem: Send {
    /// Liest den kompletten Inhalt einer Datei.
    fn lesen(&self, pfad: &str) -> FsErgebnis<Vec<u8>>;
    /// Schreibt eine Datei (legt sie an oder überschreibt sie).
    /// Das Eltern-Verzeichnis muss existieren.
    fn schreiben(&mut self, pfad: &str, inhalt: &[u8]) -> FsErgebnis<()>;
    /// Listet den Inhalt eines Verzeichnisses (alphabetisch).
    fn liste(&self, pfad: &str) -> FsErgebnis<Vec<DirEintrag>>;
    /// Legt ein neues, leeres Verzeichnis an.
    fn mkdir(&mut self, pfad: &str) -> FsErgebnis<()>;
    /// Löscht eine Datei oder ein LEERES Verzeichnis.
    fn loeschen(&mut self, pfad: &str) -> FsErgebnis<()>;
    /// Sagt, ob unter dem Pfad eine Datei oder ein Verzeichnis liegt.
    fn node_typ(&self, pfad: &str) -> FsErgebnis<NodeTyp>;

    // --- Offset-basierter Zugriff (die Naht für große Dateien und
    // --- das Disk-Dateisystem der Serie 4) ---

    /// Liest ab `offset` so viele Bytes, wie in den Puffer passen
    /// (weniger am Dateiende). Liefert die GELESENE Anzahl —
    /// 0 heißt: Offset liegt am oder hinter dem Dateiende.
    fn read_at(&self, pfad: &str, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize>;
    /// Schreibt `daten` ab `offset` (legt die Datei bei Bedarf an,
    /// wie schreiben). Ein Offset HINTER dem Dateiende füllt die
    /// Lücke mit Nullbytes (klassische Sparse-Semantik, nur ehrlich
    /// materialisiert). Liefert die geschriebene Anzahl.
    fn write_at(&mut self, pfad: &str, offset: usize, daten: &[u8]) -> FsErgebnis<usize>;
    /// Typ, Größe und Zeitstempel eines Eintrags.
    fn stat(&self, pfad: &str) -> FsErgebnis<Metadaten>;
    /// Benennt um bzw. verschiebt — ATOMAR: Entweder liegt der
    /// Eintrag danach vollständig unter `nach`, oder alles ist wie
    /// vorher. Eine existierende ZIEL-DATEI wird ersetzt (Datei auf
    /// Datei, wie POSIX-rename); ein existierendes Ziel-VERZEICHNIS
    /// ist ein Fehler. Zeitstempel des Eintrags bleiben erhalten.
    fn rename(&mut self, von: &str, nach: &str) -> FsErgebnis<()>;
    /// Drückt alle gepufferten Änderungen auf das Medium (fürs RamFs
    /// ein No-Op; das Disk-FS reicht es an sein BlockDevice weiter).
    fn sync(&mut self) -> FsErgebnis<()>;
}

/// Das global gemountete Dateisystem ("Root-Mount").
/// Später kann hieraus eine Mount-Tabelle werden (/, /floppy, ...).
static VFS: Mutex<Option<Box<dyn FileSystem>>> = Mutex::new(None);

/// Mountet das RamFs als Wurzel und legt die Demo-Dateien an.
/// Muss nach der Heap-Initialisierung aufgerufen werden!
pub fn init() {
    let mut ramfs = ramfs::RamFs::neu();

    ramfs
        .schreiben(
            "/willkommen.txt",
            b"Willkommen bei SpeedOS!\n\
              Dies ist eine Datei im RAM-Dateisystem.\n\
              Probiere: dir, cd system, type info.txt, tree\n\
              (Tab-Taste vervollstaendigt Datei- und Ordnernamen!)\n",
        )
        .expect("Demo-Datei konnte nicht angelegt werden");
    ramfs.mkdir("/system").expect("Demo-Verzeichnis fehlgeschlagen");
    ramfs
        .schreiben(
            "/system/info.txt",
            b"SpeedOS v0.1\n\
              Kernel:      Rust nightly, no_std, x86_64\n\
              Dateisystem: RamFs (in-memory) hinter dem VFS-Trait\n\
              Multitasking: kooperativ (async/await)\n",
        )
        .expect("Demo-Datei konnte nicht angelegt werden");

    *VFS.lock() = Some(Box::new(ramfs));
}

/// Führt eine Operation auf dem gemounteten Dateisystem aus.
/// Kapselt das Locking — und meldet einen sauberen Fehler, falls
/// (noch) kein Dateisystem gemountet ist.
/// ACHTUNG: Innerhalb der Closure nie wieder mit_fs aufrufen
/// (der Mutex ist nicht reentrant -> Deadlock)!
pub fn mit_fs<T>(f: impl FnOnce(&mut dyn FileSystem) -> FsErgebnis<T>) -> FsErgebnis<T> {
    let mut vfs = VFS.lock();
    match vfs.as_mut() {
        Some(fs) => f(fs.as_mut()),
        None => Err(FsFehler::NichtInitialisiert),
    }
}

/// Kopiert eine Datei. Ist das Ziel ein existierendes Verzeichnis,
/// landet die Kopie DARIN (unter gleichem Namen) — wie bei cmd/copy.
pub fn kopieren(quelle: &str, ziel: &str) -> FsErgebnis<()> {
    mit_fs(|fs| {
        let inhalt = fs.lesen(quelle)?;
        let ziel_pfad = ziel_pfad_bestimmen(fs, quelle, ziel);
        fs.schreiben(&ziel_pfad, &inhalt)
    })
}

/// Verschiebt (oder benennt um) — seit der rename-Primitive ATOMAR
/// statt kopieren+löschen, und damit auch für ganze Ordner richtig.
pub fn verschieben(quelle: &str, ziel: &str) -> FsErgebnis<()> {
    mit_fs(|fs| {
        let ziel_pfad = ziel_pfad_bestimmen(fs, quelle, ziel);
        fs.rename(quelle, &ziel_pfad)
    })
}

/// Schiebt alle gepufferten Änderungen des gemounteten Dateisystems
/// aufs Medium (die API-Naht für das Disk-FS der Serie 4).
pub fn sync() -> FsErgebnis<()> {
    mit_fs(|fs| fs.sync())
}

/// Kopiert einen Pfad REKURSIV (Datei: 1:1; Ordner: mkdir + alle
/// Kinder). WICHTIG (Deadlock-Regel): Jede Ebene schließt ihr
/// liste() ab, BEVOR sie absteigt — mit_fs nie verschachteln.
pub fn kopieren_rekursiv(quelle: &str, ziel: &str) -> FsErgebnis<()> {
    match mit_fs(|fs| fs.node_typ(quelle))? {
        NodeTyp::Datei => mit_fs(|fs| {
            let inhalt = fs.lesen(quelle)?;
            fs.schreiben(ziel, &inhalt)
        }),
        NodeTyp::Verzeichnis => {
            mit_fs(|fs| fs.mkdir(ziel))?;
            let kinder: alloc::vec::Vec<String> =
                mit_fs(|fs| fs.liste(quelle))?.into_iter().map(|e| e.name).collect();
            for kind in kinder {
                let kind_quelle = pfad_anhaengen(quelle, &kind);
                let kind_ziel = pfad_anhaengen(ziel, &kind);
                kopieren_rekursiv(&kind_quelle, &kind_ziel)?;
            }
            Ok(())
        }
    }
}

/// Löscht einen Pfad REKURSIV (Ordner samt Inhalt, Kinder zuerst).
pub fn loeschen_rekursiv(pfad: &str) -> FsErgebnis<()> {
    if mit_fs(|fs| fs.node_typ(pfad))? == NodeTyp::Verzeichnis {
        let kinder: alloc::vec::Vec<String> =
            mit_fs(|fs| fs.liste(pfad))?.into_iter().map(|e| e.name).collect();
        for kind in kinder {
            loeschen_rekursiv(&pfad_anhaengen(pfad, &kind))?;
        }
    }
    mit_fs(|fs| fs.loeschen(pfad))
}

/// Verschiebt einen Pfad (auch ganze Ordner) an einen EXAKTEN
/// Zielpfad. Seit der rename-Primitive ist das EIN atomarer Schritt
/// statt kopieren+löschen — kein halb verschobener Baum mehr, wenn
/// mittendrin etwas schiefgeht. (Der Name bleibt wegen der vielen
/// Aufrufer; sobald es mehrere Mounts gibt, braucht der Fall
/// "über Dateisystem-Grenze" wieder kopieren+löschen als Fallback.)
pub fn verschieben_rekursiv(quelle: &str, ziel: &str) -> FsErgebnis<()> {
    mit_fs(|fs| fs.rename(quelle, ziel))
}

/// Hängt einen Namen an einen absoluten Pfad ("/": kein Doppel-Slash).
pub fn pfad_anhaengen(basis: &str, name: &str) -> String {
    if basis == "/" {
        format!("/{}", name)
    } else {
        format!("{}/{}", basis, name)
    }
}

/// Hilfsfunktion für copy/move: Zeigt das Ziel auf ein Verzeichnis,
/// wird der Dateiname der Quelle angehängt.
fn ziel_pfad_bestimmen(fs: &mut dyn FileSystem, quelle: &str, ziel: &str) -> String {
    match fs.node_typ(ziel) {
        Ok(NodeTyp::Verzeichnis) => {
            let dateiname = quelle.rsplit('/').next().unwrap_or(quelle);
            if ziel == "/" {
                format!("/{}", dateiname)
            } else {
                format!("{}/{}", ziel.trim_end_matches('/'), dateiname)
            }
        }
        _ => String::from(ziel),
    }
}

/// Löst eine Pfad-Eingabe relativ zu einem Basis-Verzeichnis auf:
/// versteht absolute Pfade, ".", ".." und mehrfache Schrägstriche.
/// Ergebnis ist immer ein absoluter, normalisierter Pfad.
///
/// Beispiele: ("/a/b", "../c") -> "/a/c",  ("/", "system/") -> "/system"
pub fn pfad_aufloesen(basis: &str, eingabe: &str) -> String {
    // Absolut? Dann bei der Wurzel anfangen, sonst beim Basis-Pfad.
    let mut teile: Vec<&str> = if eingabe.starts_with('/') {
        Vec::new()
    } else {
        basis.split('/').filter(|t| !t.is_empty()).collect()
    };

    for teil in eingabe.split('/') {
        match teil {
            "" | "." => {} // leere Teile (durch //) und "." ignorieren
            ".." => {
                teile.pop(); // eine Ebene hoch (an der Wurzel: bleibt Wurzel)
            }
            name => teile.push(name),
        }
    }

    if teile.is_empty() {
        String::from("/")
    } else {
        format!("/{}", teile.join("/"))
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Pfad-Auflösung muss alle Fälle korrekt behandeln.
    #[test_case]
    fn test_pfad_aufloesen() {
        assert_eq!(pfad_aufloesen("/", "a"), "/a");
        assert_eq!(pfad_aufloesen("/a", "b/c"), "/a/b/c");
        assert_eq!(pfad_aufloesen("/a/b", ".."), "/a");
        assert_eq!(pfad_aufloesen("/a/b", "../c"), "/a/c");
        assert_eq!(pfad_aufloesen("/a", "/x"), "/x");
        assert_eq!(pfad_aufloesen("/a", "."), "/a");
        assert_eq!(pfad_aufloesen("/", ".."), "/");
        assert_eq!(pfad_aufloesen("/", "system/"), "/system");
        assert_eq!(pfad_aufloesen("/a", "b//c"), "/a/b/c");
    }

    /// kopieren und verschieben über das globale VFS (das der
    /// Test-Entry-Point gemountet hat).
    #[test_case]
    fn test_kopieren_und_verschieben() {
        mit_fs(|fs| fs.schreiben("/quelle.txt", b"Inhalt 123")).unwrap();

        // Kopieren: beide existieren, Inhalt identisch.
        kopieren("/quelle.txt", "/kopie.txt").unwrap();
        let kopie = mit_fs(|fs| fs.lesen("/kopie.txt")).unwrap();
        assert_eq!(kopie, b"Inhalt 123");
        assert!(mit_fs(|fs| fs.lesen("/quelle.txt")).is_ok());

        // Verschieben: Ziel existiert, Quelle ist weg.
        verschieben("/kopie.txt", "/verschoben.txt").unwrap();
        assert_eq!(
            mit_fs(|fs| fs.lesen("/kopie.txt")),
            Err(FsFehler::NichtGefunden)
        );
        assert!(mit_fs(|fs| fs.lesen("/verschoben.txt")).is_ok());

        // Kopieren IN ein Verzeichnis: Dateiname wird angehängt.
        mit_fs(|fs| fs.mkdir("/ablage")).unwrap();
        kopieren("/verschoben.txt", "/ablage").unwrap();
        assert!(mit_fs(|fs| fs.lesen("/ablage/verschoben.txt")).is_ok());

        // Aufräumen, damit andere Tests ein sauberes FS sehen.
        mit_fs(|fs| fs.loeschen("/quelle.txt")).unwrap();
        mit_fs(|fs| fs.loeschen("/verschoben.txt")).unwrap();
        mit_fs(|fs| fs.loeschen("/ablage/verschoben.txt")).unwrap();
        mit_fs(|fs| fs.loeschen("/ablage")).unwrap();
    }

    /// verschieben läuft seit der rename-Primitive atomar und kann
    /// damit auch ganze Ordner; fs::sync() läuft durch (RamFs: No-Op).
    #[test_case]
    fn test_verschieben_ordner_und_sync() {
        mit_fs(|fs| fs.mkdir("/vtest")).unwrap();
        mit_fs(|fs| fs.schreiben("/vtest/inhalt.txt", b"x")).unwrap();

        // Ordner umbenennen (Ziel existiert nicht):
        verschieben("/vtest", "/vtest2").unwrap();
        assert_eq!(mit_fs(|fs| fs.lesen("/vtest2/inhalt.txt")).unwrap(), b"x");
        assert_eq!(
            mit_fs(|fs| fs.node_typ("/vtest")),
            Err(FsFehler::NichtGefunden)
        );

        // Verschieben IN ein existierendes Verzeichnis hängt den
        // Quellnamen an (cmd-Semantik, wie bei copy):
        mit_fs(|fs| fs.mkdir("/vziel")).unwrap();
        verschieben("/vtest2", "/vziel").unwrap();
        assert_eq!(
            mit_fs(|fs| fs.lesen("/vziel/vtest2/inhalt.txt")).unwrap(),
            b"x"
        );

        sync().unwrap();
        loeschen_rekursiv("/vziel").unwrap();
    }
}
