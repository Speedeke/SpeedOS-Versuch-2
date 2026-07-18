// fs/ramfs.rs — RamFs: das erste Dateisystem von SpeedOS
//
// Lebt komplett im Arbeitsspeicher (auf unserem Kernel-Heap!) und
// vergisst beim Ausschalten alles — Persistenz auf einer "Platte"
// kommt später als eigenes Dateisystem hinter demselben VFS-Trait.
//
// Aufbau: ein Baum aus Knoten. Jeder Knoten trägt seinen Inhalt
// (Datei-Bytes oder Kinder-Map) PLUS Metadaten (erstellt/geändert
// als Sekunden seit dem 1.1.2000, aus der zeit-API). Verzeichnisse
// sind BTreeMaps von Name -> Knoten (BTreeMap statt HashMap, weil
// sie die Einträge automatisch alphabetisch sortiert hält — genau
// richtig für dir).

use super::{DirEintrag, FileSystem, FsErgebnis, FsFehler, Metadaten, NodeTyp};
use alloc::{
    collections::BTreeMap,
    format,
    string::String,
    vec::Vec,
};

/// Der aktuelle Zeitstempel für Datei-Metadaten (Sekunden seit dem
/// 1.1.2000 — dieselbe Epoche wie zeit::sekunden_seit_2000).
fn jetzt_stempel() -> u64 {
    crate::zeit::sekunden_seit_2000(&crate::zeit::jetzt())
}

/// Was in einem Knoten steckt: Datei-Bytes oder benannte Kinder.
enum Inhalt {
    Datei(Vec<u8>),
    Verzeichnis(BTreeMap<String, Knoten>),
}

/// Ein Knoten im Dateibaum: Inhalt plus Metadaten. Die Zeitstempel
/// wandern beim rename MIT (der Knoten bleibt derselbe — nur sein
/// Name/Ort ändert sich).
struct Knoten {
    inhalt: Inhalt,
    erstellt: u64,
    geaendert: u64,
}

impl Knoten {
    fn neu(inhalt: Inhalt) -> Self {
        let stempel = jetzt_stempel();
        Knoten { inhalt, erstellt: stempel, geaendert: stempel }
    }

    fn typ(&self) -> NodeTyp {
        match self.inhalt {
            Inhalt::Datei(_) => NodeTyp::Datei,
            Inhalt::Verzeichnis(_) => NodeTyp::Verzeichnis,
        }
    }

    fn groesse(&self) -> usize {
        match &self.inhalt {
            Inhalt::Datei(inhalt) => inhalt.len(),
            Inhalt::Verzeichnis(_) => 0,
        }
    }
}

/// Das RAM-Dateisystem. Die Wurzel "/" ist direkt die oberste Map;
/// ihre Zeitstempel (die Wurzel hat keinen eigenen Knoten) sind der
/// Anlege-Zeitpunkt des Dateisystems.
pub struct RamFs {
    wurzel: BTreeMap<String, Knoten>,
    wurzel_stempel: u64,
}

impl RamFs {
    pub fn neu() -> Self {
        RamFs {
            wurzel: BTreeMap::new(),
            wurzel_stempel: jetzt_stempel(),
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
            match aktuell.get(*k).map(|knoten| &knoten.inhalt) {
                Some(Inhalt::Verzeichnis(kinder)) => aktuell = kinder,
                Some(Inhalt::Datei(_)) => return Err(FsFehler::KeinVerzeichnis),
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
            match aktuell.get_mut(*k).map(|knoten| &mut knoten.inhalt) {
                Some(Inhalt::Verzeichnis(kinder)) => aktuell = kinder,
                Some(Inhalt::Datei(_)) => return Err(FsFehler::KeinVerzeichnis),
                None => return Err(FsFehler::NichtGefunden),
            }
        }
        Ok(aktuell)
    }

    /// Der Knoten zu einem Pfad (lesend; "/" hat keinen Knoten ->
    /// UngueltigerPfad, die stat-Methode fängt das ab).
    fn knoten(&self, pfad: &str) -> FsErgebnis<&Knoten> {
        let (eltern, name) = Self::eltern_und_name(pfad)?;
        let verzeichnis = self.verzeichnis(&eltern)?;
        verzeichnis.get(name).ok_or(FsFehler::NichtGefunden)
    }

    /// Die Datei-Bytes zu einem Pfad, veränderbar — legt die Datei
    /// bei Bedarf LEER an (der gemeinsame Unterbau von schreiben und
    /// write_at). Aktualisiert den geaendert-Stempel.
    fn datei_mut(&mut self, pfad: &str) -> FsErgebnis<&mut Vec<u8>> {
        let (eltern, name) = Self::eltern_und_name(pfad)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        if !verzeichnis.contains_key(name) {
            verzeichnis.insert(String::from(name), Knoten::neu(Inhalt::Datei(Vec::new())));
        }
        // Der Eintrag existiert jetzt sicher:
        let knoten = verzeichnis.get_mut(name).ok_or(FsFehler::NichtGefunden)?;
        match &mut knoten.inhalt {
            Inhalt::Datei(_) => knoten.geaendert = jetzt_stempel(),
            // Unter dem Namen liegt ein Verzeichnis: verboten.
            Inhalt::Verzeichnis(_) => return Err(FsFehler::ExistiertBereits),
        }
        match &mut knoten.inhalt {
            Inhalt::Datei(bytes) => Ok(bytes),
            Inhalt::Verzeichnis(_) => unreachable!(),
        }
    }
}

impl FileSystem for RamFs {
    fn lesen(&self, pfad: &str) -> FsErgebnis<Vec<u8>> {
        let knoten = self.knoten(pfad).map_err(|f| match f {
            FsFehler::UngueltigerPfad => FsFehler::KeineDatei, // "/" lesen
            andere => andere,
        })?;
        match &knoten.inhalt {
            Inhalt::Datei(inhalt) => Ok(inhalt.clone()),
            Inhalt::Verzeichnis(_) => Err(FsFehler::KeineDatei),
        }
    }

    fn schreiben(&mut self, pfad: &str, inhalt: &[u8]) -> FsErgebnis<()> {
        let bytes = self.datei_mut(pfad)?;
        *bytes = inhalt.to_vec();
        Ok(())
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
                typ: knoten.typ(),
                groesse: knoten.groesse(),
                geaendert: knoten.geaendert,
            })
            .collect())
    }

    fn mkdir(&mut self, pfad: &str) -> FsErgebnis<()> {
        let (eltern, name) = Self::eltern_und_name(pfad).map_err(|_| FsFehler::ExistiertBereits)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        if verzeichnis.contains_key(name) {
            return Err(FsFehler::ExistiertBereits);
        }
        verzeichnis.insert(
            String::from(name),
            Knoten::neu(Inhalt::Verzeichnis(BTreeMap::new())),
        );
        Ok(())
    }

    fn loeschen(&mut self, pfad: &str) -> FsErgebnis<()> {
        // Die Wurzel selbst kann man nicht löschen (eltern_und_name
        // liefert für "/" UngueltigerPfad).
        let (eltern, name) = Self::eltern_und_name(pfad)?;
        let verzeichnis = self.verzeichnis_mut(&eltern)?;
        match verzeichnis.get(name).map(|knoten| &knoten.inhalt) {
            Some(Inhalt::Datei(_)) => {
                verzeichnis.remove(name);
                Ok(())
            }
            Some(Inhalt::Verzeichnis(kinder)) => {
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
        match self.knoten(pfad) {
            Ok(knoten) => Ok(knoten.typ()),
            Err(FsFehler::UngueltigerPfad) => Ok(NodeTyp::Verzeichnis),
            Err(andere) => Err(andere),
        }
    }

    fn read_at(&self, pfad: &str, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize> {
        let knoten = self.knoten(pfad)?;
        let inhalt = match &knoten.inhalt {
            Inhalt::Datei(inhalt) => inhalt,
            Inhalt::Verzeichnis(_) => return Err(FsFehler::KeineDatei),
        };
        // Am oder hinter dem Dateiende gibt es nichts zu lesen —
        // 0 gelesene Bytes sind KEIN Fehler (wie bei POSIX-read).
        if offset >= inhalt.len() {
            return Ok(0);
        }
        let anzahl = puffer.len().min(inhalt.len() - offset);
        puffer[..anzahl].copy_from_slice(&inhalt[offset..offset + anzahl]);
        Ok(anzahl)
    }

    fn write_at(&mut self, pfad: &str, offset: usize, daten: &[u8]) -> FsErgebnis<usize> {
        let bytes = self.datei_mut(pfad)?;
        // Lücke hinterm Dateiende mit Nullbytes auffüllen (ehrlich
        // materialisierte Sparse-Semantik):
        if offset > bytes.len() {
            bytes.resize(offset, 0);
        }
        let ende = offset + daten.len();
        if ende > bytes.len() {
            bytes.resize(ende, 0);
        }
        bytes[offset..ende].copy_from_slice(daten);
        Ok(daten.len())
    }

    fn stat(&self, pfad: &str) -> FsErgebnis<Metadaten> {
        match self.knoten(pfad) {
            Ok(knoten) => Ok(Metadaten {
                typ: knoten.typ(),
                groesse: knoten.groesse(),
                erstellt: knoten.erstellt,
                geaendert: knoten.geaendert,
            }),
            // Die Wurzel hat keinen Knoten — ihre Stempel sind der
            // Anlege-Zeitpunkt des Dateisystems.
            Err(FsFehler::UngueltigerPfad) => Ok(Metadaten {
                typ: NodeTyp::Verzeichnis,
                groesse: 0,
                erstellt: self.wurzel_stempel,
                geaendert: self.wurzel_stempel,
            }),
            Err(andere) => Err(andere),
        }
    }

    fn rename(&mut self, von: &str, nach: &str) -> FsErgebnis<()> {
        // Auf sich selbst umbenennen: Erfolg, wenn es die Quelle gibt
        // (POSIX-Verhalten — und schützt vor Datenverlust unten).
        if von == nach {
            return self.knoten(von).map(|_| ());
        }
        // Das Ziel darf nicht IM Quell-Teilbaum liegen ("/a" nach
        // "/a/b"): Nach dem Herausnehmen der Quelle gäbe es den
        // Zielpfad nicht mehr — der Eintrag wäre verloren.
        if nach.starts_with(&format!("{}/", von)) {
            return Err(FsFehler::UngueltigerPfad);
        }
        let (von_eltern, von_name) = Self::eltern_und_name(von)?;
        let (nach_eltern, nach_name) = Self::eltern_und_name(nach)?;

        // PHASE 1 — alles prüfen, nichts verändern (das macht die
        // Operation atomar: Nach dieser Phase kann nichts mehr
        // scheitern).
        let quelle_typ = {
            let verzeichnis = self.verzeichnis(&von_eltern)?;
            verzeichnis
                .get(von_name)
                .map(|knoten| knoten.typ())
                .ok_or(FsFehler::NichtGefunden)?
        };
        {
            let verzeichnis = self.verzeichnis(&nach_eltern)?;
            match verzeichnis.get(nach_name).map(|knoten| knoten.typ()) {
                // Datei ersetzt Datei — atomar, wie POSIX-rename.
                Some(NodeTyp::Datei) if quelle_typ == NodeTyp::Datei => {}
                // Alles andere auf ein existierendes Ziel: Fehler
                // (ein Verzeichnis still zu ersetzen wäre Datenverlust).
                Some(_) => return Err(FsFehler::ExistiertBereits),
                None => {}
            }
        }

        // PHASE 2 — ausführen. Beide Pfade sind validiert; das
        // remove kann das insert nicht mehr beeinflussen (Ziel liegt
        // nicht im Quell-Teilbaum, siehe oben).
        let knoten = self
            .verzeichnis_mut(&von_eltern)?
            .remove(von_name)
            .ok_or(FsFehler::NichtGefunden)?;
        self.verzeichnis_mut(&nach_eltern)?
            .insert(String::from(nach_name), knoten);
        Ok(())
    }

    fn sync(&mut self) -> FsErgebnis<()> {
        // Das RamFs hat kein Medium unter sich — nichts zu tun.
        Ok(())
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

    /// read_at an allen Grenzen: mitten in der Datei, über das Ende
    /// hinaus, am/hinter dem Ende, und die Fehlerfälle.
    #[test_case]
    fn test_read_at_grenzen() {
        let mut fs = RamFs::neu();
        fs.schreiben("/r.txt", b"0123456789").unwrap();

        // Mitten hinein, Puffer passt komplett:
        let mut puffer = [0u8; 4];
        assert_eq!(fs.read_at("/r.txt", 3, &mut puffer), Ok(4));
        assert_eq!(&puffer, b"3456");

        // Über das Dateiende: nur der Rest wird geliefert.
        let mut gross = [0u8; 32];
        assert_eq!(fs.read_at("/r.txt", 7, &mut gross), Ok(3));
        assert_eq!(&gross[..3], b"789");

        // Am und hinter dem Ende: 0 Bytes, KEIN Fehler.
        assert_eq!(fs.read_at("/r.txt", 10, &mut puffer), Ok(0));
        assert_eq!(fs.read_at("/r.txt", 999, &mut puffer), Ok(0));

        // Fehlerfälle: fehlende Datei, Verzeichnis.
        assert_eq!(
            fs.read_at("/fehlt.txt", 0, &mut puffer),
            Err(FsFehler::NichtGefunden)
        );
        fs.mkdir("/ordner").unwrap();
        assert_eq!(
            fs.read_at("/ordner", 0, &mut puffer),
            Err(FsFehler::KeineDatei)
        );
    }

    /// write_at an allen Grenzen: mitten hinein, verlängernd, mit
    /// Nullbyte-Lücke hinterm Ende, und als Datei-Anleger.
    #[test_case]
    fn test_write_at_grenzen() {
        let mut fs = RamFs::neu();
        fs.schreiben("/w.txt", b"AAAAAAAAAA").unwrap();

        // Mitten hinein (Größe unverändert):
        assert_eq!(fs.write_at("/w.txt", 2, b"BB"), Ok(2));
        assert_eq!(fs.lesen("/w.txt").unwrap(), b"AABBAAAAAA");

        // Über das Ende hinaus: verlängert.
        assert_eq!(fs.write_at("/w.txt", 8, b"CCCC"), Ok(4));
        assert_eq!(fs.lesen("/w.txt").unwrap(), b"AABBAAAACCCC");

        // Offset HINTER dem Ende: Lücke wird mit Nullbytes gefüllt.
        assert_eq!(fs.write_at("/w.txt", 14, b"DD"), Ok(2));
        assert_eq!(fs.lesen("/w.txt").unwrap(), b"AABBAAAACCCC\0\0DD");

        // write_at legt fehlende Dateien an (wie schreiben):
        assert_eq!(fs.write_at("/neu.txt", 0, b"frisch"), Ok(6));
        assert_eq!(fs.lesen("/neu.txt").unwrap(), b"frisch");

        // Fehler: Verzeichnis beschreiben, fehlendes Eltern-Verzeichnis.
        fs.mkdir("/ordner").unwrap();
        assert_eq!(fs.write_at("/ordner", 0, b"x"), Err(FsFehler::ExistiertBereits));
        assert_eq!(
            fs.write_at("/gibtsnicht/x.txt", 0, b"x"),
            Err(FsFehler::NichtGefunden)
        );
    }

    /// stat: Typ, Größe und plausible Zeitstempel; Schreiben
    /// aktualisiert geaendert, erstellt bleibt stehen.
    #[test_case]
    fn test_stat_zeitstempel() {
        let mut fs = RamFs::neu();
        fs.schreiben("/s.txt", b"12345").unwrap();

        let stat = fs.stat("/s.txt").unwrap();
        assert_eq!(stat.typ, NodeTyp::Datei);
        assert_eq!(stat.groesse, 5);
        // Die Stempel kommen aus der echten Uhr (Test-Kernel hat
        // zeit::init gerufen): nach dem 1.1.2026 und konsistent.
        assert!(stat.erstellt > 26 * 365 * 86_400, "Stempel unplausibel klein");
        assert_eq!(stat.erstellt, stat.geaendert);

        // Überschreiben: geaendert läuft weiter (>=, Sekunden-
        // Auflösung!), erstellt bleibt exakt stehen.
        fs.schreiben("/s.txt", b"1234567").unwrap();
        let neu = fs.stat("/s.txt").unwrap();
        assert_eq!(neu.erstellt, stat.erstellt);
        assert!(neu.geaendert >= stat.geaendert);
        assert_eq!(neu.groesse, 7);

        // Verzeichnis und Wurzel haben auch Metadaten:
        fs.mkdir("/ordner").unwrap();
        assert_eq!(fs.stat("/ordner").unwrap().typ, NodeTyp::Verzeichnis);
        let wurzel = fs.stat("/").unwrap();
        assert_eq!(wurzel.typ, NodeTyp::Verzeichnis);
        assert!(wurzel.erstellt > 0);
        // Die liste() trägt denselben geaendert-Stempel:
        let liste = fs.liste("/").unwrap();
        let eintrag = liste.iter().find(|e| e.name == "s.txt").unwrap();
        assert_eq!(eintrag.geaendert, neu.geaendert);

        assert_eq!(fs.stat("/fehlt"), Err(FsFehler::NichtGefunden));
    }

    /// rename-Semantik: Ziel existiert nicht (Umbenennen/Verschieben),
    /// Ziel existiert als Datei (atomares Ersetzen), Ziel existiert
    /// als Verzeichnis (Fehler) — plus die Schutz-Fälle.
    #[test_case]
    fn test_rename_semantik() {
        let mut fs = RamFs::neu();
        fs.schreiben("/alt.txt", b"Inhalt").unwrap();

        // Ziel existiert NICHT: klassisches Umbenennen.
        fs.rename("/alt.txt", "/neu.txt").unwrap();
        assert_eq!(fs.lesen("/alt.txt"), Err(FsFehler::NichtGefunden));
        assert_eq!(fs.lesen("/neu.txt").unwrap(), b"Inhalt");

        // Ziel existiert als DATEI: wird atomar ersetzt.
        fs.schreiben("/opfer.txt", b"wird ersetzt").unwrap();
        fs.rename("/neu.txt", "/opfer.txt").unwrap();
        assert_eq!(fs.lesen("/opfer.txt").unwrap(), b"Inhalt");
        assert_eq!(fs.lesen("/neu.txt"), Err(FsFehler::NichtGefunden));

        // Verschieben in ein anderes Verzeichnis (exakter Zielpfad):
        fs.mkdir("/ziel").unwrap();
        fs.rename("/opfer.txt", "/ziel/da.txt").unwrap();
        assert_eq!(fs.lesen("/ziel/da.txt").unwrap(), b"Inhalt");

        // Ganze ORDNER wandern atomar mit (samt Inhalt):
        fs.mkdir("/quelle").unwrap();
        fs.schreiben("/quelle/tief.txt", b"T").unwrap();
        fs.rename("/quelle", "/umgezogen").unwrap();
        assert_eq!(fs.lesen("/umgezogen/tief.txt").unwrap(), b"T");

        // Ziel existiert als VERZEICHNIS: Fehler (kein stilles
        // Ersetzen ganzer Bäume).
        fs.mkdir("/anderes").unwrap();
        assert_eq!(
            fs.rename("/umgezogen", "/anderes"),
            Err(FsFehler::ExistiertBereits)
        );
        // Datei auf existierendes Verzeichnis: ebenfalls Fehler.
        assert_eq!(
            fs.rename("/ziel/da.txt", "/anderes"),
            Err(FsFehler::ExistiertBereits)
        );

        // Quelle fehlt:
        assert_eq!(
            fs.rename("/fehlt.txt", "/egal.txt"),
            Err(FsFehler::NichtGefunden)
        );
        // In den EIGENEN Teilbaum verschieben ist verboten:
        assert_eq!(
            fs.rename("/umgezogen", "/umgezogen/unten"),
            Err(FsFehler::UngueltigerPfad)
        );
        // Auf sich selbst: Erfolg (No-Op), Inhalt unversehrt.
        fs.rename("/umgezogen", "/umgezogen").unwrap();
        assert_eq!(fs.lesen("/umgezogen/tief.txt").unwrap(), b"T");

        // rename erhält die Zeitstempel des Knotens:
        let vorher = fs.stat("/ziel/da.txt").unwrap();
        fs.rename("/ziel/da.txt", "/ziel/dort.txt").unwrap();
        let nachher = fs.stat("/ziel/dort.txt").unwrap();
        assert_eq!(vorher.erstellt, nachher.erstellt);
        assert_eq!(vorher.geaendert, nachher.geaendert);
    }
}
