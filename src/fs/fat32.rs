// fs/fat32.rs — FAT32-Treiber (NUR LESEN) für SpeedOS
//
// FAT32 ist DAS Austausch-Dateisystem der Welt: USB-Sticks, SD-Karten,
// Kamera-Speicher. SpeedOS lernt es LESEN — ehrlich und sicher: kein
// Schreiben (jeder Schreibversuch -> IoFehler::NurLesen), keine
// Reparatur, kein Raten. Ein fremdes oder kaputtes Format wird sauber
// mit FsFehler::KeinFat32 abgelehnt, NIE mit einer Panik (die
// BPB-Validierung ist eine reine, unit-getestete Funktion, die JEDEN
// Wert prüft, bevor irgendetwas gerechnet wird).
//
// FAT32-Grundbegriffe (nur was wir zum Lesen brauchen):
//   * BPB (BIOS Parameter Block): der Bootsektor (Sektor 0) beschreibt
//     die Geometrie — Sektorgröße, Sektoren/Cluster, wo die FAT liegt,
//     wo die Daten beginnen, welcher Cluster die Wurzel ist.
//   * FAT (File Allocation Table): ein Array, das für jeden Cluster
//     sagt, welcher der NÄCHSTE in der Datei ist (Cluster-KETTE) —
//     0x0FFFFFF8+ = Ende. Wir lesen die ganze FAT einmal in den RAM.
//   * Verzeichnis: eine "Datei" aus 32-Byte-Einträgen. Kurze Namen
//     stehen im 8.3-Format; lange Namen (VFAT-LFN) verteilen sich auf
//     mehrere Zusatz-Einträge davor, UTF-16-LE — daher die Umlaute.
//
// Wie SpeedFS spricht der Treiber NUR das BlockDevice-Trait (läuft auf
// RamDisk-Tests und der echten ATA-Platte) und nutzt Innen-Mutabilität
// per RefCell (kein Lock — der VFS-Mutex serialisiert schon).

use super::block::{BlockDevice, IoFehler};
use super::{DirEintrag, FileSystem, FsErgebnis, FsFehler, Metadaten, NodeTyp};
use alloc::string::String;
use alloc::{boxed::Box, vec, vec::Vec};
use core::cell::RefCell;

/// Ein Verzeichnis-Eintrag = 32 Bytes.
const EINTRAG_GROESSE: usize = 32;
/// Attribut-Byte: dieser Wert (statt Bit-Flags) markiert einen
/// LFN-Zusatzeintrag (Read-Only|Hidden|System|VolumeID zusammen).
const ATTR_LFN: u8 = 0x0F;
const ATTR_VERZEICHNIS: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
/// Ab diesem FAT-Wert ist die Cluster-Kette zu Ende (End Of Chain).
const EOC: u32 = 0x0FFF_FFF8;
/// FAT32 verlangt laut Microsoft-Algorithmus mind. 65525 Cluster —
/// weniger wäre FAT12/FAT16 (die wir NICHT lesen).
const MIN_FAT32_CLUSTER: u32 = 65525;

// ---------------------------------------------------------------------------
// BPB — der BIOS Parameter Block (reine Parse-/Validierungs-Funktion)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bpb {
    bytes_pro_sektor: u32,
    sektoren_pro_cluster: u32,
    reservierte_sektoren: u32,
    anzahl_fats: u32,
    fat_groesse_sektoren: u32,
    wurzel_cluster: u32,
    /// Erster Datensektor (nach reservierten Sektoren + allen FATs).
    daten_start_sektor: u64,
    /// Nutzbare Datencluster gesamt (bestimmt FAT12/16/32).
    anzahl_cluster: u32,
}

impl Bpb {
    /// Parst UND VALIDIERT den Bootsektor. `geraet_sektoren` ist die
    /// Gerätegröße in GERÄTE-Sektoren — jeder Wert wird gegen die
    /// Realität geprüft, bevor gerechnet wird. Kein Wert führt je zu
    /// Panik: unplausibel -> FsFehler::KeinFat32.
    fn parsen(sektor: &[u8], geraet_sektoren: u64, geraet_sektor_groesse: usize) -> FsErgebnis<Bpb> {
        if sektor.len() < 512 {
            return Err(FsFehler::KeinFat32);
        }
        // Boot-Signatur 0x55AA am Sektor-Ende:
        if sektor[510] != 0x55 || sektor[511] != 0xAA {
            return Err(FsFehler::KeinFat32);
        }
        let u16_bei = |o: usize| u16::from_le_bytes([sektor[o], sektor[o + 1]]) as u32;
        let u32_bei = |o: usize| {
            u32::from_le_bytes([sektor[o], sektor[o + 1], sektor[o + 2], sektor[o + 3]])
        };

        let bytes_pro_sektor = u16_bei(11);
        let sektoren_pro_cluster = sektor[13] as u32;
        let reservierte_sektoren = u16_bei(14);
        let anzahl_fats = sektor[16] as u32;
        // FAT32 hat KEINE festen Root-Einträge und total16 == 0:
        let root_eintraege_16 = u16_bei(17);
        let total16 = u16_bei(19);
        let fat16_groesse = u16_bei(22);
        let total32 = u32_bei(32);
        let fat32_groesse = u32_bei(36);
        let wurzel_cluster = u32_bei(44);

        // --- Plausibilitäts-Prüfungen (jede einzeln, klarer Fehler) ---
        // bytes_pro_sektor muss eine Zweierpotenz 512..=4096 sein und
        // zur Gerätesektorgröße passen (Vielfaches in eine Richtung):
        if !matches!(bytes_pro_sektor, 512 | 1024 | 2048 | 4096) {
            return Err(FsFehler::KeinFat32);
        }
        if geraet_sektor_groesse == 0
            || !bytes_pro_sektor.is_multiple_of(geraet_sektor_groesse as u32)
        {
            return Err(FsFehler::KeinFat32);
        }
        // Sektoren/Cluster muss eine Zweierpotenz 1..=128 sein:
        if sektoren_pro_cluster == 0
            || !sektoren_pro_cluster.is_power_of_two()
            || sektoren_pro_cluster > 128
        {
            return Err(FsFehler::KeinFat32);
        }
        if anzahl_fats == 0 || reservierte_sektoren == 0 {
            return Err(FsFehler::KeinFat32);
        }
        // FAT32-Kennzeichen: keine festen Root-Einträge, total16/fat16
        // sind 0, dafür total32/fat32 gesetzt:
        if root_eintraege_16 != 0 || total16 != 0 || fat16_groesse != 0 {
            return Err(FsFehler::KeinFat32);
        }
        if fat32_groesse == 0 || total32 == 0 {
            return Err(FsFehler::KeinFat32);
        }
        if wurzel_cluster < 2 {
            return Err(FsFehler::KeinFat32);
        }

        // Das Layout muss OHNE Überlauf ins Gerät passen. Alles in u64.
        let daten_start_sektor = reservierte_sektoren as u64
            + anzahl_fats as u64 * fat32_groesse as u64;
        if daten_start_sektor >= total32 as u64 {
            return Err(FsFehler::KeinFat32);
        }
        // Das FAT32 darf nicht mehr Sektoren beanspruchen, als das
        // Gerät (in FAT-Sektoren umgerechnet) hergibt:
        let geraet_in_fat_sektoren =
            geraet_sektoren / (bytes_pro_sektor as u64 / geraet_sektor_groesse as u64);
        if total32 as u64 > geraet_in_fat_sektoren {
            return Err(FsFehler::KeinFat32);
        }

        let daten_sektoren = total32 as u64 - daten_start_sektor;
        let anzahl_cluster = (daten_sektoren / sektoren_pro_cluster as u64) as u32;
        // < 65525 Cluster wäre FAT12/16 — das lesen wir bewusst nicht:
        if anzahl_cluster < MIN_FAT32_CLUSTER {
            return Err(FsFehler::KeinFat32);
        }
        // Der Wurzel-Cluster muss ein echter Datencluster sein:
        if wurzel_cluster >= anzahl_cluster + 2 {
            return Err(FsFehler::KeinFat32);
        }
        // Die FAT muss groß genug für alle Cluster-Einträge sein
        // (je Cluster 4 Bytes):
        if (fat32_groesse as u64 * bytes_pro_sektor as u64) < ((anzahl_cluster as u64 + 2) * 4) {
            return Err(FsFehler::KeinFat32);
        }

        Ok(Bpb {
            bytes_pro_sektor,
            sektoren_pro_cluster,
            reservierte_sektoren,
            anzahl_fats,
            fat_groesse_sektoren: fat32_groesse,
            wurzel_cluster,
            daten_start_sektor,
            anzahl_cluster,
        })
    }

    fn cluster_groesse(&self) -> usize {
        (self.bytes_pro_sektor * self.sektoren_pro_cluster) as usize
    }
}

// ---------------------------------------------------------------------------
// Reine Hilfsfunktionen (unit-getestet)
// ---------------------------------------------------------------------------

/// Die 8.3-Prüfsumme eines Kurznamens (11 Bytes) — jeder LFN-
/// Zusatzeintrag trägt sie, damit man verwaiste LFNs erkennt.
fn kurzname_pruefsumme(kurz: &[u8]) -> u8 {
    let mut summe: u8 = 0;
    for &b in &kurz[..11] {
        summe = (((summe & 1) << 7).wrapping_add(summe >> 1)).wrapping_add(b);
    }
    summe
}

/// Liest den 8.3-Kurznamen aus einem Verzeichnis-Eintrag zu einem
/// String ("HALLO   TXT" -> "hallo.txt"). Für reine ASCII-Namen; bei
/// Nicht-ASCII gibt es immer einen LFN, der Vorrang hat.
fn kurzname_lesen(eintrag: &[u8]) -> String {
    let klein_basis = eintrag[12] & 0x08 != 0; // VFAT-Kleinschreibungs-Flags
    let klein_endung = eintrag[12] & 0x10 != 0;
    let mut name = String::new();
    for (i, &b) in eintrag[0..8].iter().enumerate() {
        if b == b' ' {
            break;
        }
        // 0x05 am Namensanfang steht für das echte 0xE5 (KanjiLead):
        let b = if i == 0 && b == 0x05 { 0xE5 } else { b };
        let c = b as char;
        name.push(if klein_basis { c.to_ascii_lowercase() } else { c });
    }
    let mut endung = String::new();
    for &b in &eintrag[8..11] {
        if b == b' ' {
            break;
        }
        let c = b as char;
        endung.push(if klein_endung { c.to_ascii_lowercase() } else { c });
    }
    if !endung.is_empty() {
        name.push('.');
        name.push_str(&endung);
    }
    name
}

/// Die 13 UTF-16-Einheiten eines LFN-Zusatzeintrags (Positionen
/// 1..11, 14..26, 28..32).
fn lfn_einheiten(eintrag: &[u8]) -> [u16; 13] {
    let mut u = [0u16; 13];
    let lese = |o: usize| u16::from_le_bytes([eintrag[o], eintrag[o + 1]]);
    for (i, ziel) in u.iter_mut().enumerate().take(5) {
        *ziel = lese(1 + i * 2);
    }
    for (i, ziel) in u.iter_mut().enumerate().skip(5).take(6) {
        *ziel = lese(14 + (i - 5) * 2);
    }
    for (i, ziel) in u.iter_mut().enumerate().skip(11).take(2) {
        *ziel = lese(28 + (i - 11) * 2);
    }
    u
}

/// Setzt einen langen Namen aus den (bereits nach Sequenz sortierten)
/// UTF-16-Stücken zusammen. Terminator 0x0000 beendet, 0xFFFF ist
/// Füllung. Ungültige UTF-16-Sequenzen werden zu U+FFFD (nie Panik).
fn lfn_zusammensetzen(stuecke: &[[u16; 13]]) -> String {
    let mut einheiten: Vec<u16> = Vec::new();
    'aussen: for stueck in stuecke {
        for &u in stueck {
            if u == 0x0000 {
                break 'aussen;
            }
            if u == 0xFFFF {
                continue;
            }
            einheiten.push(u);
        }
    }
    char::decode_utf16(einheiten)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// FAT-Datum (Bits 15-9 Jahr ab 1980, 8-5 Monat, 4-0 Tag) + FAT-Zeit
/// (15-11 Stunde, 10-5 Minute, 4-0 Sekunde/2) -> Sekunden seit dem
/// 1.1.2000 (unsere zeit-Epoche). Unplausibles Datum -> 0.
fn fat_zeitstempel(datum: u16, zeit: u16) -> u64 {
    if datum == 0 {
        return 0;
    }
    let jahr = 1980 + ((datum >> 9) & 0x7F) as u64;
    let monat = ((datum >> 5) & 0x0F) as u64;
    let tag = (datum & 0x1F) as u64;
    let stunde = ((zeit >> 11) & 0x1F) as u64;
    let minute = ((zeit >> 5) & 0x3F) as u64;
    let sekunde = ((zeit & 0x1F) * 2) as u64;
    // FAT kennt Datteln vor 2000; unsere Epoche beginnt dort — ältere
    // Stempel klemmen wir auf 0 (statt zu unterlaufen):
    if jahr < 2000 || !(1..=12).contains(&monat) || !(1..=31).contains(&tag) {
        return 0;
    }
    crate::zeit::sekunden_seit_2000(&crate::zeit::DatumUhrzeit {
        jahr,
        monat,
        tag,
        stunde: stunde.min(23),
        minute: minute.min(59),
        sekunde: sekunde.min(59),
    })
}

// ---------------------------------------------------------------------------
// Der geparste Verzeichnis-Eintrag
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Eintrag {
    name: String,
    ist_verzeichnis: bool,
    erster_cluster: u32,
    groesse: u32,
    erstellt: u64,
    geaendert: u64,
}

// ---------------------------------------------------------------------------
// Fat32 — Mounten und Betrieb
// ---------------------------------------------------------------------------

struct Inner {
    geraet: Box<dyn BlockDevice>,
    bpb: Bpb,
    /// Die komplette erste FAT im RAM (ein u32 je Cluster, maskiert).
    fat: Vec<u32>,
    geraet_sektor_groesse: usize,
}

pub struct Fat32 {
    inner: RefCell<Inner>,
    volume_name: String,
}

// SICHERHEIT: RefCell ist nicht Sync, aber FileSystem verlangt nur
// Send — Inner (Box<dyn BlockDevice + Send>, Bpb, Vec) ist Send. Der
// VFS-Mutex serialisiert alle Zugriffe.

impl Fat32 {
    /// Mountet ein FAT32 vom Gerät (nur lesend). Kein/kaputtes FAT32
    /// -> (FsFehler::KeinFat32, geraet) zurück (das Gerät kommt mit,
    /// damit der Aufrufer es weiterverwenden kann).
    pub fn mounten(
        mut geraet: Box<dyn BlockDevice>,
    ) -> Result<Fat32, (FsFehler, Box<dyn BlockDevice>)> {
        let geraet_sektor = geraet.sektor_groesse();
        if geraet_sektor == 0 {
            return Err((FsFehler::KeinFat32, geraet));
        }
        let geraet_sektoren = geraet.anzahl_sektoren();

        // Bootsektor (mind. 512 B) lesen — in Geräte-Sektoren:
        let sektoren_fuer_512 = 512usize.div_ceil(geraet_sektor);
        let mut boot = vec![0u8; sektoren_fuer_512 * geraet_sektor];
        if let Err(io) = geraet.lese_sektoren(0, &mut boot) {
            return Err((FsFehler::Io(io), geraet));
        }
        let bpb = match Bpb::parsen(&boot, geraet_sektoren, geraet_sektor) {
            Ok(bpb) => bpb,
            Err(fehler) => return Err((fehler, geraet)),
        };

        // Volume-Label: die 11 Bytes ab Offset 71 im Bootsektor
        // (fdisk/mformat schreiben es dort hin; sauber getrimmt):
        let volume_name = {
            let roh = &boot[71..82];
            let s: String = roh.iter().map(|&b| b as char).collect();
            let getrimmt = String::from(s.trim());
            if getrimmt.is_empty() {
                String::from("FAT32")
            } else {
                getrimmt
            }
        };

        // Die komplette FAT in den RAM lesen (ein u32 je Cluster).
        // Bytes je FAT-Sektor -> Geräte-Sektoren umrechnen:
        let g_pro_fat = bpb.bytes_pro_sektor as u64 / geraet_sektor as u64;
        let fat_start_g = bpb.reservierte_sektoren as u64 * g_pro_fat;
        let fat_bytes = bpb.fat_groesse_sektoren as usize * bpb.bytes_pro_sektor as usize;
        let mut fat_roh = vec![0u8; fat_bytes];
        if let Err(io) = geraet.lese_sektoren(fat_start_g, &mut fat_roh) {
            return Err((FsFehler::Io(io), geraet));
        }
        let anzahl_eintraege = (bpb.anzahl_cluster + 2) as usize;
        let mut fat = Vec::with_capacity(anzahl_eintraege);
        for i in 0..anzahl_eintraege {
            let b = i * 4;
            let wert = u32::from_le_bytes([fat_roh[b], fat_roh[b + 1], fat_roh[b + 2], fat_roh[b + 3]]);
            fat.push(wert & 0x0FFF_FFFF); // obere 4 Bits sind reserviert
        }

        Ok(Fat32 {
            inner: RefCell::new(Inner {
                geraet,
                bpb,
                fat,
                geraet_sektor_groesse: geraet_sektor,
            }),
            volume_name,
        })
    }

    /// Der Volume-Name (Label) — für die Mount-Meldung und `platten`.
    pub fn volume_name(&self) -> String {
        self.volume_name.clone()
    }
}

impl Inner {
    /// Liest einen ganzen Cluster (cluster >= 2) in einen frischen Vec.
    fn cluster_lesen(&mut self, cluster: u32) -> FsErgebnis<Vec<u8>> {
        if cluster < 2 || cluster >= self.bpb.anzahl_cluster + 2 {
            return Err(FsFehler::Io(IoFehler::Geraetefehler));
        }
        let g_pro_fat = self.bpb.bytes_pro_sektor as u64 / self.geraet_sektor_groesse as u64;
        let erster_fat_sektor = self.bpb.daten_start_sektor
            + (cluster as u64 - 2) * self.bpb.sektoren_pro_cluster as u64;
        let erster_g_sektor = erster_fat_sektor * g_pro_fat;
        let mut puffer = vec![0u8; self.bpb.cluster_groesse()];
        self.geraet.lese_sektoren(erster_g_sektor, &mut puffer)?;
        Ok(puffer)
    }

    /// Der nächste Cluster in der Kette (oder None am Ende / bei
    /// defektem Wert).
    fn naechster_cluster(&self, cluster: u32) -> Option<u32> {
        let wert = *self.fat.get(cluster as usize)?;
        if !(2..EOC).contains(&wert) || wert >= self.bpb.anzahl_cluster + 2 {
            None
        } else {
            Some(wert)
        }
    }

    /// Sammelt die Cluster-Kette ab `start`. SCHLEIFEN-SCHUTZ: höchstens
    /// so viele Cluster, wie es überhaupt gibt — ein Ring in einer
    /// kaputten FAT bringt uns nie zum Hängen (Geraetefehler stattdessen).
    fn kette(&self, start: u32) -> FsErgebnis<Vec<u32>> {
        let mut kette = Vec::new();
        let mut cluster = start;
        let grenze = self.bpb.anzahl_cluster as usize + 2;
        loop {
            if cluster < 2 || cluster >= self.bpb.anzahl_cluster + 2 {
                return Err(FsFehler::Io(IoFehler::Geraetefehler));
            }
            kette.push(cluster);
            if kette.len() > grenze {
                // Zyklus erkannt — lieber Fehler als Endlosschleife.
                return Err(FsFehler::Io(IoFehler::Geraetefehler));
            }
            match self.naechster_cluster(cluster) {
                Some(n) => cluster = n,
                None => return Ok(kette),
            }
        }
    }

    /// Liest den kompletten Inhalt einer Cluster-Kette bis `groesse`.
    fn ganze_datei(&mut self, start: u32, groesse: u32) -> FsErgebnis<Vec<u8>> {
        if groesse == 0 || start < 2 {
            return Ok(Vec::new());
        }
        let mut daten = Vec::with_capacity(groesse as usize);
        for cluster in self.kette(start)? {
            let block = self.cluster_lesen(cluster)?;
            daten.extend_from_slice(&block);
            if daten.len() >= groesse as usize {
                break;
            }
        }
        daten.truncate(groesse as usize);
        Ok(daten)
    }

    /// read_at ab `offset` in den Puffer (POSIX-Semantik: liefert die
    /// gelesene Anzahl, 0 am/hinter dem Dateiende).
    fn read_at(&mut self, start: u32, groesse: u32, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize> {
        let groesse = groesse as usize;
        if offset >= groesse || puffer.is_empty() || start < 2 {
            return Ok(0);
        }
        let lesbar = puffer.len().min(groesse - offset);
        let cluster_groesse = self.bpb.cluster_groesse();
        let kette = self.kette(start)?;

        let mut gelesen = 0usize;
        while gelesen < lesbar {
            let position = offset + gelesen;
            let cluster_index = position / cluster_groesse;
            let im_cluster = position % cluster_groesse;
            let cluster = match kette.get(cluster_index) {
                Some(c) => *c,
                None => break, // Kette kürzer als die Größe behauptet
            };
            let block = self.cluster_lesen(cluster)?;
            let stueck = (cluster_groesse - im_cluster).min(lesbar - gelesen);
            puffer[gelesen..gelesen + stueck]
                .copy_from_slice(&block[im_cluster..im_cluster + stueck]);
            gelesen += stueck;
        }
        Ok(gelesen)
    }

    /// Parst ein Verzeichnis (Cluster-Kette) zu Einträgen. LFN-Stücke
    /// werden gesammelt und beim Kurznamen-Eintrag zusammengesetzt;
    /// "." und ".." sowie das Volume-Label werden übersprungen.
    fn verzeichnis(&mut self, start_cluster: u32) -> FsErgebnis<Vec<Eintrag>> {
        // Alle Roh-Bytes des Verzeichnisses einsammeln:
        let mut roh = Vec::new();
        for cluster in self.kette(start_cluster)? {
            roh.extend_from_slice(&self.cluster_lesen(cluster)?);
        }

        let mut eintraege = Vec::new();
        // Gesammelte LFN-Stücke: (Sequenz, Prüfsumme, 13 Einheiten).
        let mut lfn: Vec<(u8, u8, [u16; 13])> = Vec::new();

        for e in roh.as_chunks::<EINTRAG_GROESSE>().0 {
            let erstes = e[0];
            if erstes == 0x00 {
                break; // Ende des Verzeichnisses
            }
            if erstes == 0xE5 {
                lfn.clear(); // gelöschter Eintrag
                continue;
            }
            let attr = e[11];
            if attr == ATTR_LFN {
                let seq = erstes & 0x1F;
                lfn.push((seq, e[13], lfn_einheiten(e)));
                continue;
            }
            if attr & ATTR_VOLUME_ID != 0 {
                lfn.clear(); // Volume-Label ist kein Datei-Eintrag
                continue;
            }
            // Ein echter 8.3-Eintrag: Namen bestimmen.
            let kurz_pruef = kurzname_pruefsumme(&e[0..11]);
            let name = if !lfn.is_empty() && lfn.iter().all(|(_, ck, _)| *ck == kurz_pruef) {
                // Die LFN-Stücke gehören zu diesem Kurznamen:
                lfn.sort_by_key(|(seq, _, _)| *seq);
                let stuecke: Vec<[u16; 13]> = lfn.iter().map(|(_, _, u)| *u).collect();
                lfn_zusammensetzen(&stuecke)
            } else {
                kurzname_lesen(e)
            };
            lfn.clear();

            // "." und ".." interessieren das VFS nicht (Pfad-Auflösung
            // im VFS kennt sie schon):
            if name == "." || name == ".." || name.is_empty() {
                continue;
            }
            let erster_cluster = (u16::from_le_bytes([e[20], e[21]]) as u32) << 16
                | u16::from_le_bytes([e[26], e[27]]) as u32;
            let groesse = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
            eintraege.push(Eintrag {
                name,
                ist_verzeichnis: attr & ATTR_VERZEICHNIS != 0,
                erster_cluster,
                groesse,
                erstellt: fat_zeitstempel(
                    u16::from_le_bytes([e[16], e[17]]),
                    u16::from_le_bytes([e[14], e[15]]),
                ),
                geaendert: fat_zeitstempel(
                    u16::from_le_bytes([e[24], e[25]]),
                    u16::from_le_bytes([e[22], e[23]]),
                ),
            });
        }
        Ok(eintraege)
    }

    /// Löst einen absoluten, normalisierten Pfad zu seinem Eintrag auf.
    /// Der Wurzel-Pfad "/" hat keinen Eintrag -> None-Rückgabe im Ok
    /// (Aufrufer behandeln die Wurzel als Verzeichnis selbst).
    fn aufloesen(&mut self, pfad: &str) -> FsErgebnis<Option<Eintrag>> {
        let mut aktuell: Option<Eintrag> = None; // None = Wurzel
        for teil in pfad.split('/').filter(|t| !t.is_empty()) {
            // In welchem Verzeichnis suchen wir?
            let (dir_cluster, ist_dir) = match &aktuell {
                None => (self.bpb.wurzel_cluster, true),
                Some(e) => (e.erster_cluster, e.ist_verzeichnis),
            };
            if !ist_dir {
                return Err(FsFehler::KeinVerzeichnis);
            }
            let eintraege = self.verzeichnis(dir_cluster)?;
            // FAT ist Groß-/Kleinschreibungs-UNabhängig (ASCII):
            match eintraege
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(teil))
            {
                Some(e) => aktuell = Some(e),
                None => return Err(FsFehler::NichtGefunden),
            }
        }
        Ok(aktuell)
    }
}

// ---------------------------------------------------------------------------
// Die FileSystem-Trait-Implementierung (nur lesen)
// ---------------------------------------------------------------------------

impl FileSystem for Fat32 {
    fn lesen(&self, pfad: &str) -> FsErgebnis<Vec<u8>> {
        let mut inner = self.inner.borrow_mut();
        match inner.aufloesen(pfad)? {
            Some(e) if !e.ist_verzeichnis => {
                let (start, groesse) = (e.erster_cluster, e.groesse);
                inner.ganze_datei(start, groesse)
            }
            _ => Err(FsFehler::KeineDatei), // Wurzel oder Verzeichnis
        }
    }

    fn schreiben(&mut self, _pfad: &str, _inhalt: &[u8]) -> FsErgebnis<()> {
        Err(FsFehler::Io(IoFehler::NurLesen))
    }

    fn liste(&self, pfad: &str) -> FsErgebnis<Vec<DirEintrag>> {
        let mut inner = self.inner.borrow_mut();
        let cluster = match inner.aufloesen(pfad)? {
            None => inner.bpb.wurzel_cluster, // Wurzel
            Some(e) if e.ist_verzeichnis => e.erster_cluster,
            Some(_) => return Err(FsFehler::KeinVerzeichnis),
        };
        let mut eintraege: Vec<DirEintrag> = inner
            .verzeichnis(cluster)?
            .into_iter()
            .map(|e| DirEintrag {
                name: e.name,
                typ: if e.ist_verzeichnis {
                    NodeTyp::Verzeichnis
                } else {
                    NodeTyp::Datei
                },
                groesse: if e.ist_verzeichnis { 0 } else { e.groesse as usize },
                geaendert: e.geaendert,
            })
            .collect();
        eintraege.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(eintraege)
    }

    fn mkdir(&mut self, _pfad: &str) -> FsErgebnis<()> {
        Err(FsFehler::Io(IoFehler::NurLesen))
    }

    fn loeschen(&mut self, _pfad: &str) -> FsErgebnis<()> {
        Err(FsFehler::Io(IoFehler::NurLesen))
    }

    fn node_typ(&self, pfad: &str) -> FsErgebnis<NodeTyp> {
        let mut inner = self.inner.borrow_mut();
        match inner.aufloesen(pfad)? {
            None => Ok(NodeTyp::Verzeichnis), // Wurzel
            Some(e) if e.ist_verzeichnis => Ok(NodeTyp::Verzeichnis),
            Some(_) => Ok(NodeTyp::Datei),
        }
    }

    fn read_at(&self, pfad: &str, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize> {
        let mut inner = self.inner.borrow_mut();
        match inner.aufloesen(pfad)? {
            Some(e) if !e.ist_verzeichnis => {
                let (start, groesse) = (e.erster_cluster, e.groesse);
                inner.read_at(start, groesse, offset, puffer)
            }
            _ => Err(FsFehler::KeineDatei),
        }
    }

    fn write_at(&mut self, _pfad: &str, _offset: usize, _daten: &[u8]) -> FsErgebnis<usize> {
        Err(FsFehler::Io(IoFehler::NurLesen))
    }

    fn stat(&self, pfad: &str) -> FsErgebnis<Metadaten> {
        let mut inner = self.inner.borrow_mut();
        match inner.aufloesen(pfad)? {
            None => Ok(Metadaten {
                typ: NodeTyp::Verzeichnis,
                groesse: 0,
                erstellt: 0,
                geaendert: 0,
            }),
            Some(e) => Ok(Metadaten {
                typ: if e.ist_verzeichnis {
                    NodeTyp::Verzeichnis
                } else {
                    NodeTyp::Datei
                },
                groesse: if e.ist_verzeichnis { 0 } else { e.groesse as usize },
                erstellt: e.erstellt,
                geaendert: e.geaendert,
            }),
        }
    }

    fn rename(&mut self, _von: &str, _nach: &str) -> FsErgebnis<()> {
        Err(FsFehler::Io(IoFehler::NurLesen))
    }

    fn sync(&mut self) -> FsErgebnis<()> {
        // Nur-Lese-Dateisystem: es gibt nichts zu schreiben.
        Ok(())
    }

    fn speicher_info(&self) -> FsErgebnis<Option<(u64, u64)>> {
        // frei = Cluster mit FAT-Eintrag 0; gesamt = alle Datencluster.
        let inner = self.inner.borrow();
        let cluster_bytes = inner.bpb.cluster_groesse() as u64;
        let frei = inner.fat[2..(inner.bpb.anzahl_cluster as usize + 2)]
            .iter()
            .filter(|&&w| w == 0)
            .count() as u64;
        let gesamt = inner.bpb.anzahl_cluster as u64;
        Ok(Some((frei * cluster_bytes, gesamt * cluster_bytes)))
    }

    fn ist_beschreibbar(&self, _pfad: &str) -> bool {
        false // FAT32-Treiber liest nur
    }

    fn typ_name(&self) -> &'static str {
        "FAT32"
    }
}

// ---------------------------------------------------------------------------
// Tests — reine Funktionen + ein komplettes Mini-FAT32 auf der RamDisk
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Prüfsumme, LFN-Zusammensetzung (inkl. Umlaute!) und die
    /// Zeitstempel-Umrechnung — die reinen Funktionen.
    #[test_case]
    fn test_fat32_reine_funktionen() {
        // Kurzname "HALLO   TXT" -> "hallo.txt" nur mit Case-Flags:
        let mut e = [b' '; 32];
        e[0..11].copy_from_slice(b"HALLO   TXT");
        e[12] = 0x18; // Basis + Endung klein
        assert_eq!(kurzname_lesen(&e), "hallo.txt");

        // LFN mit Umlauten: "Grüße äöüß" korrekt aus UTF-16 lesen.
        let name = "Grüße äöüß.txt";
        let utf16: Vec<u16> = name.encode_utf16().collect();
        let mut stuecke: Vec<[u16; 13]> = Vec::new();
        for teil in utf16.chunks(13) {
            let mut s = [0u16; 13];
            s[..teil.len()].copy_from_slice(teil);
            // Rest mit Terminator + 0xFFFF-Füllung (wie echt):
            for (i, slot) in s.iter_mut().enumerate() {
                if i >= teil.len() {
                    *slot = if i == teil.len() { 0x0000 } else { 0xFFFF };
                }
            }
            stuecke.push(s);
        }
        assert_eq!(lfn_zusammensetzen(&stuecke), name);

        // Zeitstempel: 20.07.2026 12:00:00.
        let datum = (((2026 - 1980) << 9) | (7 << 5) | 20) as u16;
        let zeit = (12u16 << 11) | (30 << 5); // 12:30:00
        let sek = fat_zeitstempel(datum, zeit);
        let zurueck = crate::zeit::datum_von_sekunden_seit_2000(sek);
        assert_eq!((zurueck.jahr, zurueck.monat, zurueck.tag), (2026, 7, 20));
        assert_eq!((zurueck.stunde, zurueck.minute), (12, 30));
        // Datum 0 -> Stempel 0 (kein Zeitwert):
        assert_eq!(fat_zeitstempel(0, 0), 0);
    }

    /// BPB-Parsing gegen KAPUTTE Werte: NIE Panik, immer KeinFat32.
    #[test_case]
    fn test_fat32_bpb_validierung() {
        let baue = |f: &dyn Fn(&mut [u8])| {
            let mut s = vec![0u8; 512];
            // Ein Minimal-gültiges FAT32: 512 B/Sektor, 1 Sektor/Cluster,
            // 32 reserviert, 2 FATs, fat32-Größe passend, viele Cluster.
            s[11] = 0x00;
            s[12] = 0x02; // bytes_pro_sektor = 512
            s[13] = 1; // sektoren_pro_cluster
            s[14] = 32;
            s[15] = 0; // reserviert = 32
            s[16] = 2; // fats
            // total32 (Offset 32) = 200000 Sektoren:
            s[32..36].copy_from_slice(&200000u32.to_le_bytes());
            // fat32-Größe (Offset 36) = 1600 Sektoren (gross genug):
            s[36..40].copy_from_slice(&1600u32.to_le_bytes());
            // Wurzel-Cluster (Offset 44) = 2:
            s[44..48].copy_from_slice(&2u32.to_le_bytes());
            s[510] = 0x55;
            s[511] = 0xAA;
            f(&mut s);
            s
        };

        // Der Basis-Sektor ist gültig:
        assert!(Bpb::parsen(&baue(&|_| {}), 200000, 512).is_ok());
        // Fehlende Boot-Signatur:
        assert_eq!(
            Bpb::parsen(&baue(&|s| s[511] = 0x00), 200000, 512),
            Err(FsFehler::KeinFat32)
        );
        // Sektoren/Cluster = 0 (Division!) oder keine Zweierpotenz:
        assert!(Bpb::parsen(&baue(&|s| s[13] = 0), 200000, 512).is_err());
        assert!(Bpb::parsen(&baue(&|s| s[13] = 3), 200000, 512).is_err());
        // 0 FATs:
        assert!(Bpb::parsen(&baue(&|s| s[16] = 0), 200000, 512).is_err());
        // FAT16-Kennzeichen (feste Root-Einträge) gesetzt:
        assert!(Bpb::parsen(&baue(&|s| s[17] = 0x10), 200000, 512).is_err());
        // Zu wenige Cluster (wäre FAT16): total32 winzig:
        assert!(Bpb::parsen(
            &baue(&|s| s[32..36].copy_from_slice(&2000u32.to_le_bytes())),
            2000,
            512
        )
        .is_err());
        // Layout größer als das Gerät:
        assert!(Bpb::parsen(&baue(&|_| {}), 100, 512).is_err());
        // Ein rein zufälliger Sektor (0xAA-Signatur trifft evtl. zufällig,
        // aber die Werte sind Unsinn):
        assert!(Bpb::parsen(&[0xABu8; 512], 200000, 512).is_err());
    }

    /// Baut ein winziges, aber echtes FAT32 auf einer RamDisk und
    /// prüft den ganzen Lese-Weg: Wurzel-Liste, Datei-Inhalt (auch
    /// über Cluster-Grenzen), Unterordner, read_at, und das saubere
    /// Ablehnen aller Schreib-Operationen.
    #[test_case]
    fn test_fat32_ramdisk_komplett() {
        let (disk, gross) = super::testbau::mini_fat32();
        let fs = Fat32::mounten(Box::new(disk)).map_err(|(f, _)| f).unwrap();

        // Wurzel-Liste (alphabetisch): ordner/, gross.bin, hallo.txt.
        let namen: Vec<String> = fs.liste("/").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(
            namen,
            vec![
                String::from("gross.bin"),
                String::from("hallo.txt"),
                String::from("ordner"),
            ]
        );
        assert_eq!(fs.volume_name(), "TESTFAT");

        // Kleine Datei im Wurzelverzeichnis:
        assert_eq!(fs.lesen("/hallo.txt").unwrap(), b"Hallo FAT32!");
        assert_eq!(fs.node_typ("/hallo.txt").unwrap(), NodeTyp::Datei);

        // Große Datei ueber mehrere Cluster — Byte fuer Byte:
        assert_eq!(fs.lesen("/gross.bin").unwrap(), gross);
        // read_at ueber eine Cluster-Grenze (Cluster = 512 B hier):
        let mut stueck = [0u8; 100];
        let n = fs.read_at("/gross.bin", 470, &mut stueck).unwrap();
        assert_eq!(n, 100);
        assert_eq!(stueck[..], gross[470..570]);
        // read_at hinterm Dateiende = 0:
        assert_eq!(fs.read_at("/gross.bin", gross.len(), &mut stueck).unwrap(), 0);

        // Unterordner mit langem Namen:
        assert_eq!(fs.node_typ("/ordner").unwrap(), NodeTyp::Verzeichnis);
        let unter: Vec<String> =
            fs.liste("/ordner").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(unter, vec![String::from("langer dateiname äöü.txt")]);
        assert_eq!(
            fs.lesen("/ordner/langer dateiname äöü.txt").unwrap(),
            "Inhalt mit Umlaut: ß".as_bytes()
        );

        // Alles Schreibende wird sauber abgelehnt:
        assert_eq!(fs.node_typ("/gibtsnicht"), Err(FsFehler::NichtGefunden));
        let mut fs = fs;
        assert_eq!(
            fs.schreiben("/neu.txt", b"x"),
            Err(FsFehler::Io(IoFehler::NurLesen))
        );
        assert_eq!(fs.mkdir("/neu"), Err(FsFehler::Io(IoFehler::NurLesen)));
        assert_eq!(fs.loeschen("/hallo.txt"), Err(FsFehler::Io(IoFehler::NurLesen)));
        assert!(!fs.ist_beschreibbar("/"));
        assert_eq!(fs.typ_name(), "FAT32");
    }
}

// ---------------------------------------------------------------------------
// testbau — ein Mini-FAT32-Image im RAM (nur für Tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod testbau {
    use super::super::block::{BlockDevice, IoFehler};
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Eine SPARSE Test-Platte: FAT32 braucht >= 65525 Cluster (~34 MiB
    /// bei 512-B-Sektoren), aber ein Test-Image ist zu 99% leer. Diese
    /// Disk speichert nur die WIRKLICH geschriebenen Sektoren (Rest =
    /// Nullen) — so passt das "34-MiB-Image" locker in den Test-Heap.
    pub struct SparseDisk {
        sektoren: BTreeMap<u64, Vec<u8>>,
        anzahl: u64,
    }

    impl SparseDisk {
        fn neu(anzahl: u64) -> SparseDisk {
            SparseDisk { sektoren: BTreeMap::new(), anzahl }
        }
    }

    impl BlockDevice for SparseDisk {
        fn sektor_groesse(&self) -> usize {
            BPS
        }
        fn anzahl_sektoren(&self) -> u64 {
            self.anzahl
        }
        fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
            if !puffer.len().is_multiple_of(BPS) {
                return Err(IoFehler::UngueltigePufferGroesse);
            }
            let anzahl = (puffer.len() / BPS) as u64;
            if start + anzahl > self.anzahl {
                return Err(IoFehler::AusserhalbDesGeraets);
            }
            for i in 0..anzahl {
                let ziel = &mut puffer[i as usize * BPS..(i as usize + 1) * BPS];
                match self.sektoren.get(&(start + i)) {
                    Some(block) => ziel.copy_from_slice(block),
                    None => ziel.fill(0),
                }
            }
            Ok(())
        }
        fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
            if !puffer.len().is_multiple_of(BPS) {
                return Err(IoFehler::UngueltigePufferGroesse);
            }
            let anzahl = (puffer.len() / BPS) as u64;
            if start + anzahl > self.anzahl {
                return Err(IoFehler::AusserhalbDesGeraets);
            }
            for i in 0..anzahl {
                self.sektoren.insert(
                    start + i,
                    puffer[i as usize * BPS..(i as usize + 1) * BPS].to_vec(),
                );
            }
            Ok(())
        }
        fn sync(&mut self) -> Result<(), IoFehler> {
            Ok(())
        }
    }

    const BPS: usize = 512;
    const SPC: usize = 1;
    const RESERVIERT: usize = 32;
    const NFATS: usize = 2;
    const CLUSTER: usize = 66000; // > 65525 -> echtes FAT32
    const FATSZ: usize = (CLUSTER + 2) * 4 / BPS + 1;

    fn kurz(basis: &str, endung: &str) -> [u8; 11] {
        let mut k = [b' '; 11];
        for (i, b) in basis.bytes().take(8).enumerate() {
            k[i] = b;
        }
        for (i, b) in endung.bytes().take(3).enumerate() {
            k[8 + i] = b;
        }
        k
    }

    fn pruefsumme(kurz: &[u8; 11]) -> u8 {
        let mut s: u8 = 0;
        for &b in kurz {
            s = (((s & 1) << 7).wrapping_add(s >> 1)).wrapping_add(b);
        }
        s
    }

    fn kurz_eintrag(k: &[u8; 11], attr: u8, cluster: u32, groesse: u32) -> Vec<u8> {
        kurz_eintrag_case(k, attr, cluster, groesse, 0)
    }

    /// Wie kurz_eintrag, aber mit VFAT-Kleinschreibungs-Flags (Byte 12):
    /// 0x08 = Basis klein, 0x10 = Endung klein — genau das, was echte
    /// Werkzeuge für reine Kleinschreib-8.3-Namen setzen.
    fn kurz_eintrag_case(k: &[u8; 11], attr: u8, cluster: u32, groesse: u32, case: u8) -> Vec<u8> {
        let mut e = vec![0u8; 32];
        e[0..11].copy_from_slice(k);
        e[11] = attr;
        e[12] = case;
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&((cluster & 0xFFFF) as u16).to_le_bytes());
        e[28..32].copy_from_slice(&groesse.to_le_bytes());
        e
    }

    /// LFN-Zusatzeinträge für `name`, rückwärts nummeriert.
    fn lfn_eintraege(name: &str, kurz: &[u8; 11]) -> Vec<u8> {
        let mut einheiten: Vec<u16> = name.encode_utf16().collect();
        einheiten.push(0x0000); // Terminator
        while !einheiten.len().is_multiple_of(13) {
            einheiten.push(0xFFFF); // Füllung
        }
        let stuecke: Vec<&[u16]> = einheiten.chunks(13).collect();
        let ck = pruefsumme(kurz);
        let mut aus = Vec::new();
        for (idx, stueck) in stuecke.iter().enumerate().rev() {
            let mut e = vec![0u8; 32];
            let nummer = (idx + 1) as u8;
            e[0] = nummer | if idx + 1 == stuecke.len() { 0x40 } else { 0 };
            e[11] = 0x0F;
            e[13] = ck;
            let setze = |e: &mut Vec<u8>, off: usize, u: u16| {
                e[off..off + 2].copy_from_slice(&u.to_le_bytes());
            };
            for i in 0..5 {
                setze(&mut e, 1 + i * 2, stueck[i]);
            }
            for i in 0..6 {
                setze(&mut e, 14 + i * 2, stueck[5 + i]);
            }
            for i in 0..2 {
                setze(&mut e, 28 + i * 2, stueck[11 + i]);
            }
            aus.extend_from_slice(&e);
        }
        aus
    }

    /// Baut ein Mini-FAT32 mit hallo.txt, gross.bin (mehrere Cluster)
    /// und ordner/langer-name. Liefert (SparseDisk, gross.bin-Inhalt).
    pub fn mini_fat32() -> (SparseDisk, Vec<u8>) {
        let total = RESERVIERT + NFATS * FATSZ + CLUSTER;
        let mut fat = vec![0u32; CLUSTER + 2];
        fat[0] = 0x0FFFFFF8;
        fat[1] = 0x0FFFFFFF;
        // cluster -> Cluster-Inhalt (je BPS Bytes)
        let mut daten: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut naechster = 3u32; // 2 = Wurzel

        // Eine Datei in eine Cluster-Kette legen, Startcluster zurück
        // (mindestens EIN Cluster, auch für kurze Inhalte):
        let lege = |inhalt: &[u8], fat: &mut Vec<u32>, daten: &mut Vec<(u32, Vec<u8>)>, naechster: &mut u32| -> u32 {
            let mut start = 0u32;
            let mut vorher = 0u32;
            let mut i = 0;
            while i < inhalt.len().max(1) {
                let c = *naechster;
                *naechster += 1;
                let mut block = vec![0u8; BPS];
                let kopiere = inhalt.len().saturating_sub(i).min(BPS);
                block[..kopiere].copy_from_slice(&inhalt[i..i + kopiere]);
                daten.push((c, block));
                if vorher != 0 {
                    fat[vorher as usize] = c;
                } else {
                    start = c;
                }
                vorher = c;
                i += BPS;
            }
            fat[vorher as usize] = 0x0FFFFFFF;
            start
        };

        let hallo = b"Hallo FAT32!".to_vec();
        let hallo_start = lege(&hallo, &mut fat, &mut daten, &mut naechster);
        let gross: Vec<u8> = (0..2000).map(|i| (i % 251) as u8).collect();
        let gross_start = lege(&gross, &mut fat, &mut daten, &mut naechster);

        // Unterordner mit einer Datei (langer Name):
        let unter_inhalt = "Inhalt mit Umlaut: ß".as_bytes().to_vec();
        let unter_start = lege(&unter_inhalt, &mut fat, &mut daten, &mut naechster);
        let unter_kurz = kurz("LANGER~1", "TXT");
        let mut ordner_dir = Vec::new();
        // . und .. (Cluster traegt der Writer nach):
        ordner_dir.extend_from_slice(&kurz_eintrag(&kurz(".", ""), 0x10, 0, 0));
        ordner_dir.extend_from_slice(&kurz_eintrag(&kurz("..", ""), 0x10, 0, 0));
        ordner_dir.extend_from_slice(&lfn_eintraege("langer dateiname äöü.txt", &unter_kurz));
        ordner_dir.extend_from_slice(&kurz_eintrag(&unter_kurz, 0x20, unter_start, unter_inhalt.len() as u32));
        let ordner_start = lege(&ordner_dir, &mut fat, &mut daten, &mut naechster);
        // "." auf sich selbst zeigen lassen:
        if let Some((_, block)) = daten.iter_mut().find(|(c, _)| *c == ordner_start) {
            block[20..22].copy_from_slice(&((ordner_start >> 16) as u16).to_le_bytes());
            block[26..28].copy_from_slice(&((ordner_start & 0xFFFF) as u16).to_le_bytes());
        }

        // Wurzelverzeichnis (Cluster 2):
        fat[2] = 0x0FFFFFFF;
        let mut root = Vec::new();
        root.extend_from_slice(&kurz_eintrag(&kurz("TESTFAT", ""), 0x08, 0, 0)); // Label
        // hallo.txt/gross.bin: reine Kleinschreib-Namen -> Case-Flags
        // (0x18 = Basis + Endung klein), wie echte Werkzeuge es tun:
        root.extend_from_slice(&kurz_eintrag_case(&kurz("HALLO", "TXT"), 0x20, hallo_start, hallo.len() as u32, 0x18));
        root.extend_from_slice(&kurz_eintrag_case(&kurz("GROSS", "BIN"), 0x20, gross_start, gross.len() as u32, 0x18));
        let ordner_kurz = kurz("ORDNER", "");
        root.extend_from_slice(&lfn_eintraege("ordner", &ordner_kurz));
        root.extend_from_slice(&kurz_eintrag(&ordner_kurz, 0x10, ordner_start, 0));
        // Den Wurzel-Cluster (Cluster 2) selbst als Datencluster ablegen
        // — sonst steht das Verzeichnis nirgends auf der Platte:
        root.resize(BPS, 0);
        daten.push((2, root));

        // ---- Alles in die (sparse) Test-Disk gießen ----
        let mut disk = SparseDisk::neu(total as u64);
        let mut boot = vec![0u8; BPS];
        boot[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
        boot[13] = SPC as u8;
        boot[14..16].copy_from_slice(&(RESERVIERT as u16).to_le_bytes());
        boot[16] = NFATS as u8;
        boot[32..36].copy_from_slice(&(total as u32).to_le_bytes());
        boot[36..40].copy_from_slice(&(FATSZ as u32).to_le_bytes());
        boot[44..48].copy_from_slice(&2u32.to_le_bytes());
        boot[71..82].copy_from_slice(b"TESTFAT    ");
        boot[510] = 0x55;
        boot[511] = 0xAA;
        disk.schreibe_sektoren(0, &boot).unwrap();

        // FAT (beide Kopien):
        let mut fat_roh = Vec::with_capacity(FATSZ * BPS);
        for w in &fat {
            fat_roh.extend_from_slice(&w.to_le_bytes());
        }
        fat_roh.resize(FATSZ * BPS, 0);
        disk.schreibe_sektoren(RESERVIERT as u64, &fat_roh).unwrap();
        disk.schreibe_sektoren((RESERVIERT + FATSZ) as u64, &fat_roh).unwrap();

        // Datencluster:
        let daten_start = (RESERVIERT + NFATS * FATSZ) as u64;
        for (c, block) in &daten {
            let sektor = daten_start + (*c as u64 - 2) * SPC as u64;
            disk.schreibe_sektoren(sektor, block).unwrap();
        }

        (disk, gross)
    }
}
