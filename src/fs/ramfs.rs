// fs/ramfs.rs — RamFs: das erste Dateisystem von SpeedOS
//
// Lebt komplett im Arbeitsspeicher (auf unserem Kernel-Heap!) und
// vergisst beim Ausschalten alles — Persistenz auf einer "Platte"
// kommt später als eigenes Dateisystem hinter demselben VFS-Trait.
//
// Aufbau: ein Baum aus Knoten. Jedes Verzeichnis ist eine BTreeMap
// von Name -> Knoten (BTreeMap statt HashMap, weil sie die Einträge
// automatisch alphabetisch sortiert hält — genau richtig für dir).

use super::{DirEintrag, FileSystem, FsErgebnis, FsFehler, NodeTyp};
use alloc::{
    collections::BTreeMap,
    string::String,
    vec::Vec,
};

/// Ein Knoten im Dateibaum: entweder Datei (mit Inhalt als Bytes)
/// oder Verzeichnis (mit benannten Kindern).
enum Knoten {
    Datei(Vec<u8>),
    Verzeichnis(BTreeMap<String, Knoten>),
}

/// Das RAM-Dateisystem. Die Wurzel "/" ist direkt die oberste Map.
pub struct RamFs {
    wurzel: BTreeMap<String, Knoten>,
}

impl RamFs {
    pub fn neu() -> Self {
        RamFs {
            wurzel: BTreeMap::new(),
        }
    }

    /// Zerlegt einen absoluten Pfad in (Eltern-Komponenten, Name).
    /// "/a/b/c.txt" -> (["a", "b"], "c.txt"). Für "/" gibt es keinen
    /// Namen -> UngueltigerPfad (die Wurzel behandeln Aufrufer selbst).
    fn eltern_und_name(pfad: &str) -> FsErgebnis<(Vec<&str>, &str)> {
        let mut teile: Vec<&str> = pfad.split('/').filter(|t| !t.is_empty()).collect();
        match teile.pop() {
            Some(name) => Ok((teile, name)),
            None => Err(FsFehler::UngueltigerPfad),
        }
    }

    /// Wandert die Komponenten entlang und liefert die Kinder-Map des
    /// Ziel-Verzeichnisses (lesend).
    fn verzeichnis(&self, komponenten: &[&str]) -> FsErgebnis<&BTreeMap<String, Knoten>> {
        let mut aktuell = &self.wurzel;
        for k in komponenten {
            match aktuell.get(*k) {
                Some(Knoten::Verzeichnis(kinder)) => aktuell = kinder,
                Some(Knoten::Datei(_)) => return Err(FsFehler::KeinVerzeichnis),
                None => return Err(FsFehler::NichtGefunden),
            }
        }
        Ok(aktuell)
    }

    /// Wie `verzeichnis`, aber mit Schreibzugriff.
    fn verzeichnis_mut(
        &mut self,
        komponenten: &[&str],
    ) -> FsErgebnis<&mut BTreeMap<String, Knoten>> {
        let mut aktuell = &mut self.wurzel;
        for k in komponenten {
            match aktuell.get_mut(*k) {
                Some(Knoten::Verzeichnis(kinder)) => aktuell = kinder,
                Some(Knoten::Datei(_)) => return Err(FsFehler::KeinVerzeichnis),
                None => return Err(FsFehler::NichtGefunden),
            }
        }
        Ok(aktuell)
    }
}

impl FileSystem for RamFs {
    fn lesen(&self, pfad: &str) -> FsErgebnis<Vec<u8>> {
        let (eltern, name) = Self::eltern_und_name(pfad).map_err(|_| FsFehler::KeineDatei)?;
        let verzeichnis = self.verzeichnis(&eltern)?;
        match verzeichnis.get(name) {
            Some(Knoten::Datei(inhalt)) => Ok(inhalt.clone()),
            Some(Knoten::Verzeichnis(_)) => Err(FsFehler::KeineDatei),
            None => Err(FsFehler::NichtGefunden),
        }
    }

    fn schreiben(&mut self, pfad: &str, inhalt: &[u8]) -> FsErgebnis<()> {
        let (eltern, name) = Self::eltern_und_name(pfad)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        match verzeichnis.get_mut(name) {
            // Datei existiert: Inhalt ersetzen (Überschreiben).
            Some(Knoten::Datei(alt)) => {
                *alt = inhalt.to_vec();
                Ok(())
            }
            // Unter dem Namen liegt ein Verzeichnis: verboten.
            Some(Knoten::Verzeichnis(_)) => Err(FsFehler::ExistiertBereits),
            // Neu anlegen.
            None => {
                verzeichnis.insert(String::from(name), Knoten::Datei(inhalt.to_vec()));
                Ok(())
            }
        }
    }

    fn liste(&self, pfad: &str) -> FsErgebnis<Vec<DirEintrag>> {
        // "/" hat keine Eltern — die Komponenten sind der ganze Pfad.
        let komponenten: Vec<&str> = pfad.split('/').filter(|t| !t.is_empty()).collect();
        let verzeichnis = self.verzeichnis(&komponenten)?;
        // BTreeMap iteriert sortiert -> die Liste ist alphabetisch.
        Ok(verzeichnis
            .iter()
            .map(|(name, knoten)| DirEintrag {
                name: name.clone(),
                typ: match knoten {
                    Knoten::Datei(_) => NodeTyp::Datei,
                    Knoten::Verzeichnis(_) => NodeTyp::Verzeichnis,
                },
                groesse: match knoten {
                    Knoten::Datei(inhalt) => inhalt.len(),
                    Knoten::Verzeichnis(_) => 0,
                },
            })
            .collect())
    }

    fn mkdir(&mut self, pfad: &str) -> FsErgebnis<()> {
        let (eltern, name) = Self::eltern_und_name(pfad).map_err(|_| FsFehler::ExistiertBereits)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        if verzeichnis.contains_key(name) {
            return Err(FsFehler::ExistiertBereits);
        }
        verzeichnis.insert(String::from(name), Knoten::Verzeichnis(BTreeMap::new()));
        Ok(())
    }

    fn loeschen(&mut self, pfad: &str) -> FsErgebnis<()> {
        // Die Wurzel selbst kann man nicht löschen (eltern_und_name
        // liefert für "/" UngueltigerPfad).
        let (eltern, name) = Self::eltern_und_name(pfad)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        match verzeichnis.get(name) {
            Some(Knoten::Datei(_)) => {
                verzeichnis.remove(name);
                Ok(())
            }
            Some(Knoten::Verzeichnis(kinder)) => {
                // Sicherheitsnetz wie bei cmd/rmdir: nur leere
                // Verzeichnisse — schützt vor versehentlichem
                // Löschen ganzer Bäume.
                if kinder.is_empty() {
                    verzeichnis.remove(name);
                    Ok(())
                } else {
                    Err(FsFehler::VerzeichnisNichtLeer)
                }
            }
            None => Err(FsFehler::NichtGefunden),
        }
    }

    fn node_typ(&self, pfad: &str) -> FsErgebnis<NodeTyp> {
        // Die Wurzel ist immer ein Verzeichnis.
        let (eltern, name) = match Self::eltern_und_name(pfad) {
            Ok(x) => x,
            Err(_) => return Ok(NodeTyp::Verzeichnis),
        };
        let verzeichnis = self.verzeichnis(&eltern)?;
        match verzeichnis.get(name) {
            Some(Knoten::Datei(_)) => Ok(NodeTyp::Datei),
            Some(Knoten::Verzeichnis(_)) => Ok(NodeTyp::Verzeichnis),
            None => Err(FsFehler::NichtGefunden),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Schreiben und Zurücklesen — der Grundfall.
    #[test_case]
    fn test_schreiben_und_lesen() {
        let mut fs = RamFs::neu();
        fs.schreiben("/test.txt", b"Hallo SpeedOS").unwrap();
        assert_eq!(fs.lesen("/test.txt").unwrap(), b"Hallo SpeedOS");
    }

    /// Überschreiben ersetzt den Inhalt komplett.
    #[test_case]
    fn test_ueberschreiben() {
        let mut fs = RamFs::neu();
        fs.schreiben("/a.txt", b"alter langer Inhalt").unwrap();
        fs.schreiben("/a.txt", b"neu").unwrap();
        assert_eq!(fs.lesen("/a.txt").unwrap(), b"neu");
    }

    /// Verschachtelte Verzeichnisse: anlegen, hineinschreiben, listen.
    #[test_case]
    fn test_verzeichnisse_und_liste() {
        let mut fs = RamFs::neu();
        fs.mkdir("/dokumente").unwrap();
        fs.mkdir("/dokumente/notizen").unwrap();
        fs.schreiben("/dokumente/notizen/heute.txt", b"12345").unwrap();
        fs.schreiben("/dokumente/brief.txt", b"Sehr geehrte...").unwrap();

        let liste = fs.liste("/dokumente").unwrap();
        assert_eq!(liste.len(), 2);
        // BTreeMap sortiert alphabetisch: brief.txt vor notizen.
        assert_eq!(liste[0].name, "brief.txt");
        assert_eq!(liste[0].typ, NodeTyp::Datei);
        assert_eq!(liste[0].groesse, 15);
        assert_eq!(liste[1].name, "notizen");
        assert_eq!(liste[1].typ, NodeTyp::Verzeichnis);

        assert_eq!(fs.lesen("/dokumente/notizen/heute.txt").unwrap(), b"12345");
    }

    /// Alle wichtigen Fehlerfälle liefern den richtigen Fehler.
    #[test_case]
    fn test_fehlerfaelle() {
        let mut fs = RamFs::neu();
        fs.mkdir("/ordner").unwrap();
        fs.schreiben("/datei.txt", b"x").unwrap();

        // Nicht vorhanden:
        assert_eq!(fs.lesen("/fehlt.txt"), Err(FsFehler::NichtGefunden));
        assert_eq!(fs.loeschen("/fehlt.txt"), Err(FsFehler::NichtGefunden));
        // In nicht existierendes Verzeichnis schreiben:
        assert_eq!(
            fs.schreiben("/gibtsnicht/x.txt", b"x"),
            Err(FsFehler::NichtGefunden)
        );
        // Verzeichnis wie Datei lesen (und umgekehrt):
        assert_eq!(fs.lesen("/ordner"), Err(FsFehler::KeineDatei));
        assert_eq!(fs.liste("/datei.txt"), Err(FsFehler::KeinVerzeichnis));
        // Doppelt anlegen:
        assert_eq!(fs.mkdir("/ordner"), Err(FsFehler::ExistiertBereits));
        assert_eq!(fs.schreiben("/ordner", b"x"), Err(FsFehler::ExistiertBereits));
        // Datei "durchqueren" wie ein Verzeichnis:
        assert_eq!(
            fs.lesen("/datei.txt/tiefer.txt"),
            Err(FsFehler::KeinVerzeichnis)
        );
    }

    /// Löschen: Dateien immer, Verzeichnisse nur wenn leer.
    #[test_case]
    fn test_loeschen() {
        let mut fs = RamFs::neu();
        fs.mkdir("/ordner").unwrap();
        fs.schreiben("/ordner/inhalt.txt", b"x").unwrap();

        // Nicht leer -> Fehler.
        assert_eq!(fs.loeschen("/ordner"), Err(FsFehler::VerzeichnisNichtLeer));
        // Erst die Datei, dann klappt es.
        fs.loeschen("/ordner/inhalt.txt").unwrap();
        fs.loeschen("/ordner").unwrap();
        assert_eq!(fs.node_typ("/ordner"), Err(FsFehler::NichtGefunden));
        // Die Wurzel ist unantastbar.
        assert_eq!(fs.loeschen("/"), Err(FsFehler::UngueltigerPfad));
    }

    /// node_typ unterscheidet Datei, Verzeichnis und Wurzel.
    #[test_case]
    fn test_node_typ() {
        let mut fs = RamFs::neu();
        fs.mkdir("/v").unwrap();
        fs.schreiben("/d.txt", b"x").unwrap();
        assert_eq!(fs.node_typ("/"), Ok(NodeTyp::Verzeichnis));
        assert_eq!(fs.node_typ("/v"), Ok(NodeTyp::Verzeichnis));
        assert_eq!(fs.node_typ("/d.txt"), Ok(NodeTyp::Datei));
        assert_eq!(fs.node_typ("/nix"), Err(FsFehler::NichtGefunden));
    }
}
