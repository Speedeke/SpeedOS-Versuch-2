// fs/speedfs.rs — SpeedFS: das eigene Disk-Dateisystem von SpeedOS
//
// Das On-Disk-Format ist in docs/speedfs-format.md SPEZIFIZIERT —
// das Dokument ist die Wahrheit, dieser Code setzt es um. Kurzfassung:
// Superblock (Magic "SPFS") | Block-Bitmap | Inode-Tabelle | Daten,
// alles in 4-KiB-Blöcken, alle Zahlen Little-Endian. Inodes haben
// 22 direkte + 1024 einfach-indirekte Blockzeiger (max. ~4,09 MiB
// pro Datei); Verzeichnisse sind Byte-Listen [Inode|Länge|Name].
//
// ABSTURZ-DISZIPLIN statt Journal (docs/speedfs-format.md §7):
//   1. Belegen vor Benutzen (Bitmap/Inode-Slot zuerst),
//   2. Inhalt vor Verweis (Daten -> Inode -> Verzeichnis-Eintrag),
//   3. Entkoppeln vor Freigeben.
// Jede Operation hat EINEN sektor-atomaren Commit-Punkt; ein Absturz
// hinterlässt schlimmstenfalls Lecks (belegt-aber-unreferenziert),
// nie Metadaten, die auf falsche Daten zeigen. Die Reihenfolgen im
// Code sind mit "Ordnung:" kommentiert.
//
// BLOCK-CACHE: Write-Through (Entscheidung in CLAUDE.md) — jeder
// Schreibvorgang geht SOFORT ans Gerät, der Cache beschleunigt nur
// das Lesen. Damit ist die Code-Reihenfolge == Platten-Reihenfolge
// und die Absturz-Analyse gilt ohne Zusatzannahmen.
//
// SpeedFS kennt nur das BlockDevice-Trait: dieselbe Implementierung
// läuft auf der RamDisk (alle Unit-Tests) und der echten ATA-Platte.
//
// Innen-Mutabilität: Die Trait-Lesemethoden sind &self, aber Gerät
// und Cache brauchen &mut — deshalb steckt alles in einer RefCell.
// Das ist KEIN Lock (das VFS-Mutex serialisiert bereits alle
// Zugriffe), nur die billige Leih-Prüfung; die Methoden borgen
// genau einmal und nie verschachtelt.

use super::block::{BlockDevice, IoFehler};
use super::{DirEintrag, FileSystem, FsErgebnis, FsFehler, Metadaten, NodeTyp};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::{boxed::Box, vec, vec::Vec};
use core::cell::RefCell;

/// Dateisystem-Blockgröße (docs/speedfs-format.md §1).
pub const BLOCK_GROESSE: usize = 4096;
const MAGIC: &[u8; 4] = b"SPFS";
const VERSION: u32 = 1;
const INODE_GROESSE: usize = 128;
const INODES_PRO_BLOCK: usize = BLOCK_GROESSE / INODE_GROESSE; // 32
/// Direkte Blockzeiger je Inode (§5).
const DIREKTE: usize = 22;
/// Zeiger im einfach-indirekten Block: 4096 / 4.
const ZEIGER_PRO_BLOCK: usize = BLOCK_GROESSE / 4; // 1024
/// Maximale Dateigröße: (22 + 1024) * 4096 = 4.284.416 Bytes (§5).
pub const MAX_DATEI: usize = (DIREKTE + ZEIGER_PRO_BLOCK) * BLOCK_GROESSE;
/// Lese-Cache-Kapazität: 64 Blöcke = 256 KiB Heap.
const CACHE_KAPAZITAET: usize = 64;

const TYP_FREI: u32 = 0;
const TYP_DATEI: u32 = 1;
const TYP_VERZEICHNIS: u32 = 2;
/// Inode-Nummer der Wurzel; 0 ist "kein Eintrag" (§5).
const WURZEL_INODE: u32 = 1;

/// Zeitstempel "jetzt" in der zeit-Epoche (Sekunden seit 1.1.2000).
fn jetzt_stempel() -> u64 {
    crate::zeit::sekunden_seit_2000(&crate::zeit::jetzt())
}

// ---------------------------------------------------------------------------
// Superblock (§3) — parse/serialize als reine, unit-getestete Funktionen
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Superblock {
    anzahl_inodes: u32,
    anzahl_bloecke: u64,
    bitmap_start: u64,
    bitmap_bloecke: u64,
    inode_start: u64,
    inode_bloecke: u64,
    daten_start: u64,
}

impl Superblock {
    /// Serialisiert in einen 4-KiB-Block (Offsets aus §3).
    fn serialisieren(&self) -> Vec<u8> {
        let mut block = vec![0u8; BLOCK_GROESSE];
        block[0..4].copy_from_slice(MAGIC);
        block[4..8].copy_from_slice(&VERSION.to_le_bytes());
        block[8..12].copy_from_slice(&(BLOCK_GROESSE as u32).to_le_bytes());
        block[12..16].copy_from_slice(&self.anzahl_inodes.to_le_bytes());
        block[16..24].copy_from_slice(&self.anzahl_bloecke.to_le_bytes());
        block[24..32].copy_from_slice(&self.bitmap_start.to_le_bytes());
        block[32..40].copy_from_slice(&self.bitmap_bloecke.to_le_bytes());
        block[40..48].copy_from_slice(&self.inode_start.to_le_bytes());
        block[48..56].copy_from_slice(&self.inode_bloecke.to_le_bytes());
        block[56..64].copy_from_slice(&self.daten_start.to_le_bytes());
        block[64..68].copy_from_slice(&WURZEL_INODE.to_le_bytes());
        block
    }

    /// Parst und VALIDIERT einen Superblock-Block. Eine fremde oder
    /// leere Platte ist kein Systemfehler, sondern `KeinSpeedFs`.
    fn parsen(block: &[u8], geraet_bloecke: u64) -> FsErgebnis<Superblock> {
        let u32_bei = |o: usize| u32::from_le_bytes(block[o..o + 4].try_into().unwrap());
        let u64_bei = |o: usize| u64::from_le_bytes(block[o..o + 8].try_into().unwrap());
        if &block[0..4] != MAGIC || u32_bei(4) != VERSION || u32_bei(8) != BLOCK_GROESSE as u32 {
            return Err(FsFehler::KeinSpeedFs);
        }
        let sb = Superblock {
            anzahl_inodes: u32_bei(12),
            anzahl_bloecke: u64_bei(16),
            bitmap_start: u64_bei(24),
            bitmap_bloecke: u64_bei(32),
            inode_start: u64_bei(40),
            inode_bloecke: u64_bei(48),
            daten_start: u64_bei(56),
        };
        // Die Bereiche müssen aufs Gerät passen und aufeinander folgen —
        // ein kaputter Superblock darf uns nirgendwohin schicken:
        let plausibel = u32_bei(64) == WURZEL_INODE
            && sb.anzahl_bloecke <= geraet_bloecke
            && sb.bitmap_start == 1
            && sb.inode_start == sb.bitmap_start + sb.bitmap_bloecke
            && sb.daten_start == sb.inode_start + sb.inode_bloecke
            && sb.daten_start < sb.anzahl_bloecke
            && sb.anzahl_inodes as u64 <= sb.inode_bloecke * INODES_PRO_BLOCK as u64;
        if !plausibel {
            return Err(FsFehler::KeinSpeedFs);
        }
        Ok(sb)
    }
}

// ---------------------------------------------------------------------------
// Inode (§5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Inode {
    typ: u32,
    groesse: u64,
    erstellt: u64,
    geaendert: u64,
    direkt: [u32; DIREKTE],
    indirekt: u32,
}

impl Inode {
    fn leer(typ: u32) -> Inode {
        let jetzt = jetzt_stempel();
        Inode {
            typ,
            groesse: 0,
            erstellt: jetzt,
            geaendert: jetzt,
            direkt: [0; DIREKTE],
            indirekt: 0,
        }
    }

    fn node_typ(&self) -> NodeTyp {
        if self.typ == TYP_VERZEICHNIS {
            NodeTyp::Verzeichnis
        } else {
            NodeTyp::Datei
        }
    }

    /// Serialisiert in die 128 Inode-Bytes (Offsets aus §5).
    fn serialisieren(&self) -> [u8; INODE_GROESSE] {
        let mut roh = [0u8; INODE_GROESSE];
        roh[0..4].copy_from_slice(&self.typ.to_le_bytes());
        roh[8..16].copy_from_slice(&self.groesse.to_le_bytes());
        roh[16..24].copy_from_slice(&self.erstellt.to_le_bytes());
        roh[24..32].copy_from_slice(&self.geaendert.to_le_bytes());
        for (i, zeiger) in self.direkt.iter().enumerate() {
            roh[32 + i * 4..36 + i * 4].copy_from_slice(&zeiger.to_le_bytes());
        }
        roh[120..124].copy_from_slice(&self.indirekt.to_le_bytes());
        roh
    }

    fn parsen(roh: &[u8]) -> Inode {
        let u32_bei = |o: usize| u32::from_le_bytes(roh[o..o + 4].try_into().unwrap());
        let u64_bei = |o: usize| u64::from_le_bytes(roh[o..o + 8].try_into().unwrap());
        let mut direkt = [0u32; DIREKTE];
        for (i, zeiger) in direkt.iter_mut().enumerate() {
            *zeiger = u32_bei(32 + i * 4);
        }
        Inode {
            typ: u32_bei(0),
            groesse: u64_bei(8),
            erstellt: u64_bei(16),
            geaendert: u64_bei(24),
            direkt,
            indirekt: u32_bei(120),
        }
    }
}

// ---------------------------------------------------------------------------
// Verzeichnis-Einträge (§6) — reine Funktionen
// ---------------------------------------------------------------------------

/// Serialisiert die Eintragsliste: [Inode u32 | Länge u8 | Name].
fn dir_serialisieren(eintraege: &[(u32, String)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (inode, name) in eintraege {
        bytes.extend_from_slice(&inode.to_le_bytes());
        bytes.push(name.len() as u8);
        bytes.extend_from_slice(name.as_bytes());
    }
    bytes
}

/// Parst die Eintragsliste. Ein syntaktisch kaputtes Verzeichnis
/// (abgeschnittener Eintrag, kein UTF-8) ist ein Geräte-/Format-
/// Problem und wird als Io(Geraetefehler) gemeldet — nie geraten.
fn dir_parsen(bytes: &[u8]) -> FsErgebnis<Vec<(u32, String)>> {
    let mut eintraege = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        if pos + 5 > bytes.len() {
            return Err(FsFehler::Io(IoFehler::Geraetefehler));
        }
        let inode = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let laenge = bytes[pos + 4] as usize;
        pos += 5;
        if laenge == 0 || pos + laenge > bytes.len() {
            return Err(FsFehler::Io(IoFehler::Geraetefehler));
        }
        let name = core::str::from_utf8(&bytes[pos..pos + laenge])
            .map_err(|_| FsFehler::Io(IoFehler::Geraetefehler))?;
        eintraege.push((inode, String::from(name)));
        pos += laenge;
    }
    Ok(eintraege)
}

/// Ein gültiger Name: 1..=255 UTF-8-Bytes, kein '/', nicht "."/"..".
fn name_pruefen(name: &str) -> FsErgebnis<()> {
    if name.is_empty() || name.len() > 255 || name.contains('/') || name == "." || name == ".." {
        return Err(FsFehler::UngueltigerPfad);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Der Block-Cache (Write-Through, FIFO-Verdrängung)
// ---------------------------------------------------------------------------

struct BlockCache {
    bloecke: BTreeMap<u64, Vec<u8>>,
    reihenfolge: VecDeque<u64>,
}

impl BlockCache {
    fn neu() -> BlockCache {
        BlockCache {
            bloecke: BTreeMap::new(),
            reihenfolge: VecDeque::new(),
        }
    }

    fn holen(&self, nr: u64) -> Option<&Vec<u8>> {
        self.bloecke.get(&nr)
    }

    fn einfuegen(&mut self, nr: u64, daten: Vec<u8>) {
        if self.bloecke.insert(nr, daten).is_none() {
            self.reihenfolge.push_back(nr);
            if self.reihenfolge.len() > CACHE_KAPAZITAET {
                if let Some(alt) = self.reihenfolge.pop_front() {
                    self.bloecke.remove(&alt);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// mkfs — formatieren (§8)
// ---------------------------------------------------------------------------

/// Rechnet das Layout für ein Gerät aus (reine Funktion, §8).
fn layout_berechnen(geraet_bytes: u64) -> Superblock {
    let anzahl_bloecke = geraet_bytes / BLOCK_GROESSE as u64;
    let bitmap_bloecke = anzahl_bloecke.div_ceil(BLOCK_GROESSE as u64 * 8);
    let anzahl_inodes = (anzahl_bloecke / 16).clamp(64, 65536) as u32;
    let inode_bloecke = (anzahl_inodes as u64).div_ceil(INODES_PRO_BLOCK as u64);
    let inode_start = 1 + bitmap_bloecke;
    Superblock {
        anzahl_inodes,
        anzahl_bloecke,
        bitmap_start: 1,
        bitmap_bloecke,
        inode_start,
        inode_bloecke,
        daten_start: inode_start + inode_bloecke,
    }
}

/// Formatiert das Gerät mit einem leeren SpeedFS v1. LÖSCHT logisch
/// alle Daten! Ordnung (§7 mkfs): erst Bitmap, Inode-Tabelle und
/// Wurzel-Inode, ZULETZT der Superblock — ein halbes mkfs ist damit
/// unsichtbar (die Platte ist dann einfach "kein SpeedFS").
pub fn formatieren(geraet: &mut dyn BlockDevice) -> FsErgebnis<()> {
    let geraet_bytes = geraet.anzahl_sektoren() * geraet.sektor_groesse() as u64;
    let sb = layout_berechnen(geraet_bytes);
    if sb.daten_start >= sb.anzahl_bloecke {
        return Err(FsFehler::Voll); // Gerät zu klein für Metadaten
    }
    let sektoren_pro_block = (BLOCK_GROESSE / geraet.sektor_groesse()) as u64;
    let mut block_schreiben = |nr: u64, daten: &[u8]| -> Result<(), IoFehler> {
        geraet.schreibe_sektoren(nr * sektoren_pro_block, daten)
    };

    // 1. Bitmap: Metadaten-Blöcke (0 .. daten_start) als belegt.
    for bitmap_block in 0..sb.bitmap_bloecke {
        let mut block = vec![0u8; BLOCK_GROESSE];
        let erster_bit = bitmap_block * BLOCK_GROESSE as u64 * 8;
        for bit in 0..(BLOCK_GROESSE as u64 * 8) {
            let block_nr = erster_bit + bit;
            if block_nr < sb.daten_start {
                block[(bit / 8) as usize] |= 1 << (bit % 8);
            }
            // Blöcke HINTER dem Geräteende als belegt markieren, damit
            // die Allokation nie dorthin greift:
            if block_nr >= sb.anzahl_bloecke {
                block[(bit / 8) as usize] |= 1 << (bit % 8);
            }
        }
        block_schreiben(sb.bitmap_start + bitmap_block, &block)?;
    }

    // 2. Inode-Tabelle nullen; die Wurzel (Inode 1, leeres
    //    Verzeichnis) liegt im ersten Tabellen-Block.
    for tabellen_block in 0..sb.inode_bloecke {
        let mut block = vec![0u8; BLOCK_GROESSE];
        if tabellen_block == 0 {
            let wurzel = Inode::leer(TYP_VERZEICHNIS);
            let ab = WURZEL_INODE as usize * INODE_GROESSE;
            block[ab..ab + INODE_GROESSE].copy_from_slice(&wurzel.serialisieren());
        }
        block_schreiben(sb.inode_start + tabellen_block, &block)?;
    }

    // 3. ZULETZT der Superblock — erst jetzt "existiert" das FS.
    block_schreiben(0, &sb.serialisieren())?;
    geraet.sync()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SpeedFs — Mounten und Betrieb
// ---------------------------------------------------------------------------

struct Inner {
    geraet: Box<dyn BlockDevice>,
    sb: Superblock,
    cache: BlockCache,
    sektoren_pro_block: u64,
}

pub struct SpeedFs {
    inner: RefCell<Inner>,
}

// SICHERHEIT: RefCell ist nicht Sync, aber FileSystem verlangt nur
// Send — und Inner (Box<dyn BlockDevice + Send>, Superblock, Cache)
// ist Send. Der VFS-Mutex serialisiert alle Zugriffe.

impl SpeedFs {
    /// Mountet ein SpeedFS vom Gerät. Kein/kaputtes SpeedFS ist ein
    /// normaler Fehler — das Gerät kommt dann mit zurück, damit der
    /// Aufrufer z. B. formatieren kann.
    pub fn mounten(
        mut geraet: Box<dyn BlockDevice>,
    ) -> Result<SpeedFs, (FsFehler, Box<dyn BlockDevice>)> {
        let sektor = geraet.sektor_groesse();
        if sektor == 0 || !BLOCK_GROESSE.is_multiple_of(sektor) {
            return Err((FsFehler::Io(IoFehler::UngueltigePufferGroesse), geraet));
        }
        let sektoren_pro_block = (BLOCK_GROESSE / sektor) as u64;
        let geraet_bloecke = geraet.anzahl_sektoren() / sektoren_pro_block;

        let mut block0 = vec![0u8; BLOCK_GROESSE];
        if let Err(io) = geraet.lese_sektoren(0, &mut block0) {
            return Err((FsFehler::Io(io), geraet));
        }
        match Superblock::parsen(&block0, geraet_bloecke) {
            Ok(sb) => Ok(SpeedFs {
                inner: RefCell::new(Inner {
                    geraet,
                    sb,
                    cache: BlockCache::neu(),
                    sektoren_pro_block,
                }),
            }),
            Err(fehler) => Err((fehler, geraet)),
        }
    }

    /// Gibt das Gerät zurück (für Wiedermount-Tests und umount).
    /// Der Aufrufer hat vorher sync() gerufen — aushaengen selbst
    /// schreibt nichts mehr.
    pub fn aushaengen(self) -> Box<dyn BlockDevice> {
        self.inner.into_inner().geraet
    }

    /// Anzahl freier Blöcke laut Bitmap — für Tests (Leck-Prüfung!)
    /// und die Platten-Info der Shell.
    pub fn freie_bloecke(&self) -> FsErgebnis<u64> {
        let mut inner = self.inner.borrow_mut();
        let mut frei = 0u64;
        for bitmap_block in 0..inner.sb.bitmap_bloecke {
            let nr = inner.sb.bitmap_start + bitmap_block;
            let block = inner.block_lesen(nr)?;
            frei += block.iter().map(|b| b.count_zeros() as u64).sum::<u64>();
        }
        // Die Bits hinter dem Geräteende sind als belegt markiert
        // (mkfs) — count_zeros zählt also genau die echten freien.
        Ok(frei)
    }
}

impl Inner {
    // --- Block-Ebene (mit Write-Through-Cache) ---------------------------

    fn block_lesen(&mut self, nr: u64) -> FsErgebnis<Vec<u8>> {
        if let Some(block) = self.cache.holen(nr) {
            return Ok(block.clone());
        }
        let mut puffer = vec![0u8; BLOCK_GROESSE];
        self.geraet
            .lese_sektoren(nr * self.sektoren_pro_block, &mut puffer)?;
        self.cache.einfuegen(nr, puffer.clone());
        Ok(puffer)
    }

    /// Write-Through: SOFORT aufs Gerät, dann in den Cache.
    fn block_schreiben(&mut self, nr: u64, daten: &[u8]) -> FsErgebnis<()> {
        self.geraet
            .schreibe_sektoren(nr * self.sektoren_pro_block, daten)?;
        self.cache.einfuegen(nr, daten.to_vec());
        Ok(())
    }

    // --- Bitmap (§4) ------------------------------------------------------

    /// Alloziert `anzahl` freie Blöcke (First-Fit ab Datenbereich) und
    /// markiert sie SOFORT als belegt (Ordnung: Belegen vor Benutzen).
    /// Alles-oder-nichts: Reicht der Platz nicht, wird NICHTS geändert.
    fn bloecke_allozieren(&mut self, anzahl: usize) -> FsErgebnis<Vec<u32>> {
        if anzahl == 0 {
            return Ok(Vec::new());
        }
        // Phase 1: Kandidaten suchen (noch keine Änderung).
        let mut gefunden = Vec::with_capacity(anzahl);
        'suche: for bitmap_block in 0..self.sb.bitmap_bloecke {
            let block = self.block_lesen(self.sb.bitmap_start + bitmap_block)?;
            let erster = bitmap_block * BLOCK_GROESSE as u64 * 8;
            for (byte_i, byte) in block.iter().enumerate() {
                if *byte == 0xFF {
                    continue;
                }
                for bit in 0..8 {
                    if byte & (1 << bit) == 0 {
                        let nr = erster + byte_i as u64 * 8 + bit as u64;
                        gefunden.push(nr as u32);
                        if gefunden.len() == anzahl {
                            break 'suche;
                        }
                    }
                }
            }
        }
        if gefunden.len() < anzahl {
            return Err(FsFehler::Voll);
        }
        // Phase 2: Bits setzen und betroffene Bitmap-Blöcke schreiben.
        self.bitmap_setzen(&gefunden, true)?;
        Ok(gefunden)
    }

    /// Markiert Blöcke als frei (Ordnung: Entkoppeln vor Freigeben —
    /// der Aufrufer hat die Verweise bereits entfernt).
    fn bloecke_freigeben(&mut self, bloecke: &[u32]) -> FsErgebnis<()> {
        self.bitmap_setzen(bloecke, false)
    }

    /// Setzt die Bitmap-Bits der Blöcke auf belegt/frei und schreibt
    /// jeden betroffenen Bitmap-Block genau einmal (Write-Through).
    fn bitmap_setzen(&mut self, bloecke: &[u32], belegt: bool) -> FsErgebnis<()> {
        let bits_pro_block = BLOCK_GROESSE as u64 * 8;
        let mut betroffene: Vec<u64> = bloecke
            .iter()
            .map(|nr| *nr as u64 / bits_pro_block)
            .collect();
        betroffene.sort_unstable();
        betroffene.dedup();
        for bitmap_block in betroffene {
            let nr = self.sb.bitmap_start + bitmap_block;
            let mut block = self.block_lesen(nr)?;
            for block_nr in bloecke {
                let bit_global = *block_nr as u64;
                if bit_global / bits_pro_block != bitmap_block {
                    continue;
                }
                let bit = bit_global % bits_pro_block;
                let byte = &mut block[(bit / 8) as usize];
                let maske = 1u8 << (bit % 8);
                if belegt {
                    debug_assert!(*byte & maske == 0, "Block doppelt alloziert");
                    *byte |= maske;
                } else {
                    debug_assert!(*byte & maske != 0, "Block doppelt freigegeben");
                    *byte &= !maske;
                }
            }
            self.block_schreiben(nr, &block)?;
        }
        Ok(())
    }

    // --- Inode-Tabelle (§5) ----------------------------------------------

    fn inode_lesen(&mut self, nr: u32) -> FsErgebnis<Inode> {
        debug_assert!(nr >= 1 && nr < self.sb.anzahl_inodes);
        let block_nr = self.sb.inode_start + nr as u64 / INODES_PRO_BLOCK as u64;
        let block = self.block_lesen(block_nr)?;
        let ab = (nr as usize % INODES_PRO_BLOCK) * INODE_GROESSE;
        Ok(Inode::parsen(&block[ab..ab + INODE_GROESSE]))
    }

    fn inode_schreiben(&mut self, nr: u32, inode: &Inode) -> FsErgebnis<()> {
        let block_nr = self.sb.inode_start + nr as u64 / INODES_PRO_BLOCK as u64;
        let mut block = self.block_lesen(block_nr)?;
        let ab = (nr as usize % INODES_PRO_BLOCK) * INODE_GROESSE;
        block[ab..ab + INODE_GROESSE].copy_from_slice(&inode.serialisieren());
        self.block_schreiben(block_nr, &block)
    }

    /// Sucht einen freien Inode-Slot und BELEGT ihn sofort mit dem
    /// leeren Inode (Ordnung: Belegen vor Benutzen — noch zeigt kein
    /// Verzeichnis auf ihn; nach einem Absturz wäre er nur ein Leck).
    fn inode_allozieren(&mut self, typ: u32) -> FsErgebnis<(u32, Inode)> {
        for nr in 1..self.sb.anzahl_inodes {
            let inode = self.inode_lesen(nr)?;
            if inode.typ == TYP_FREI {
                let neuer = Inode::leer(typ);
                self.inode_schreiben(nr, &neuer)?;
                return Ok((nr, neuer));
            }
        }
        Err(FsFehler::Voll)
    }

    // --- Datei-Inhalt: Blockliste, Lesen, Ersetzen ------------------------

    /// Liefert die Blockliste des Inodes in Dateireihenfolge
    /// (nur so viele, wie die Größe braucht).
    fn blockliste(&mut self, inode: &Inode) -> FsErgebnis<Vec<u32>> {
        let benoetigt = (inode.groesse as usize).div_ceil(BLOCK_GROESSE);
        let mut liste = Vec::with_capacity(benoetigt);
        for zeiger in inode.direkt.iter().take(benoetigt) {
            liste.push(*zeiger);
        }
        if benoetigt > DIREKTE {
            let indirekt = self.block_lesen(inode.indirekt as u64)?;
            for i in 0..(benoetigt - DIREKTE) {
                liste.push(u32::from_le_bytes(
                    indirekt[i * 4..i * 4 + 4].try_into().unwrap(),
                ));
            }
        }
        Ok(liste)
    }

    /// Liest ab `offset` in den Puffer (read_at-Semantik: liefert die
    /// gelesene Anzahl, 0 am/hinter dem Dateiende).
    fn daten_lesen(&mut self, inode: &Inode, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize> {
        let groesse = inode.groesse as usize;
        if offset >= groesse || puffer.is_empty() {
            return Ok(0);
        }
        let lesbar = puffer.len().min(groesse - offset);
        let liste = self.blockliste(inode)?;
        let mut gelesen = 0usize;
        while gelesen < lesbar {
            let position = offset + gelesen;
            let block_index = position / BLOCK_GROESSE;
            let im_block = position % BLOCK_GROESSE;
            let stueck = (BLOCK_GROESSE - im_block).min(lesbar - gelesen);
            let block = self.block_lesen(liste[block_index] as u64)?;
            puffer[gelesen..gelesen + stueck].copy_from_slice(&block[im_block..im_block + stueck]);
            gelesen += stueck;
        }
        Ok(lesbar)
    }

    fn inhalt_lesen(&mut self, inode: &Inode) -> FsErgebnis<Vec<u8>> {
        let mut puffer = vec![0u8; inode.groesse as usize];
        self.daten_lesen(inode, 0, &mut puffer)?;
        Ok(puffer)
    }

    /// Schreibt die Blockliste in den Inode (direkt + ggf. indirekt).
    /// Der Indirekt-Block wird VOR dem Inode geschrieben (Ordnung:
    /// Inhalt vor Verweis). `indirekt_block` muss bei Bedarf schon
    /// alloziert sein.
    fn zeiger_setzen(
        &mut self,
        inode: &mut Inode,
        liste: &[u32],
        indirekt_block: u32,
    ) -> FsErgebnis<()> {
        inode.direkt = [0; DIREKTE];
        for (i, nr) in liste.iter().take(DIREKTE).enumerate() {
            inode.direkt[i] = *nr;
        }
        if liste.len() > DIREKTE {
            debug_assert!(indirekt_block != 0);
            let mut block = vec![0u8; BLOCK_GROESSE];
            for (i, nr) in liste[DIREKTE..].iter().enumerate() {
                block[i * 4..i * 4 + 4].copy_from_slice(&nr.to_le_bytes());
            }
            self.block_schreiben(indirekt_block as u64, &block)?;
            inode.indirekt = indirekt_block;
        } else {
            inode.indirekt = 0;
        }
        Ok(())
    }

    /// Ersetzt den kompletten Inhalt eines Inodes (Datei-Inhalt oder
    /// Verzeichnis-Liste). Ordnung (§7 write): 1. neue Blöcke belegen,
    /// 2. Daten (+ Indirektblock) schreiben, 3. COMMIT per Inode,
    /// 4. alte Blöcke freigeben. Bis zum Commit zeigt der Inode
    /// vollständig auf den alten Inhalt.
    fn inhalt_ersetzen(&mut self, nr: u32, inode: &mut Inode, daten: &[u8]) -> FsErgebnis<()> {
        if daten.len() > MAX_DATEI {
            return Err(FsFehler::DateiZuGross);
        }
        let alte_liste = self.blockliste(inode)?;
        let alter_indirekt = inode.indirekt;

        let benoetigt = daten.len().div_ceil(BLOCK_GROESSE);
        let indirekt_noetig = benoetigt > DIREKTE;
        // 1. Belegen vor Benutzen (alles-oder-nichts):
        let mut neue = self.bloecke_allozieren(benoetigt + usize::from(indirekt_noetig))?;
        let indirekt_block = if indirekt_noetig { neue.pop().unwrap() } else { 0 };

        // 2. Volle Blöcke schreiben (der letzte mit Nullen aufgefüllt —
        //    Invariante: hinter dem Dateiende stehen im Block Nullen).
        for (i, block_nr) in neue.iter().enumerate() {
            let von = i * BLOCK_GROESSE;
            let bis = (von + BLOCK_GROESSE).min(daten.len());
            let mut block = vec![0u8; BLOCK_GROESSE];
            block[..bis - von].copy_from_slice(&daten[von..bis]);
            self.block_schreiben(*block_nr as u64, &block)?;
        }

        // 3. COMMIT: Zeiger (inkl. Indirektblock) + Größe + Zeit.
        self.zeiger_setzen(inode, &neue, indirekt_block)?;
        inode.groesse = daten.len() as u64;
        inode.geaendert = jetzt_stempel();
        self.inode_schreiben(nr, inode)?;

        // 4. Entkoppeln vor Freigeben: erst jetzt die alten Blöcke.
        let mut freigeben = alte_liste;
        if alter_indirekt != 0 {
            freigeben.push(alter_indirekt);
        }
        self.bloecke_freigeben(&freigeben)?;
        Ok(())
    }

    /// write_at (§7): Der Bereich in BESTEHENDEN Blöcken wird in-place
    /// überschrieben; Wachstum hängt neue Blöcke an (Bitmap zuerst,
    /// Inode-Commit zuletzt). Lücken hinterm Dateiende werden zu
    /// Nullbyte-Blöcken (Sparse-Semantik, ehrlich materialisiert).
    fn daten_schreiben_ab(
        &mut self,
        nr: u32,
        inode: &mut Inode,
        offset: usize,
        daten: &[u8],
    ) -> FsErgebnis<usize> {
        if daten.is_empty() {
            return Ok(0);
        }
        let alte_groesse = inode.groesse as usize;
        let neues_ende = alte_groesse.max(offset + daten.len());
        if neues_ende > MAX_DATEI {
            return Err(FsFehler::DateiZuGross);
        }
        let alte_bloecke = alte_groesse.div_ceil(BLOCK_GROESSE);
        let benoetigt = neues_ende.div_ceil(BLOCK_GROESSE);
        let indirekt_neu = benoetigt > DIREKTE && inode.indirekt == 0;

        // 1. Belegen vor Benutzen: zusätzliche Blöcke (+ ggf. der
        //    erste Indirektblock) — alles-oder-nichts.
        let mut liste = self.blockliste(inode)?;
        let mut indirekt_block = inode.indirekt;
        if benoetigt > alte_bloecke || indirekt_neu {
            let mut neue = self
                .bloecke_allozieren(benoetigt - alte_bloecke + usize::from(indirekt_neu))?;
            if indirekt_neu {
                indirekt_block = neue.pop().unwrap();
            }
            liste.extend_from_slice(&neue);
        }

        // 2. Datenblöcke schreiben: Lücken-Blöcke als Nullen, den
        //    Schreibbereich block-weise (Randblöcke read-modify-write).
        for (block_index, &block_nr) in liste.iter().enumerate() {
            let block_von = block_index * BLOCK_GROESSE;
            let block_bis = block_von + BLOCK_GROESSE;
            let schreib_von = block_von.max(offset);
            let schreib_bis = block_bis.min(offset + daten.len());
            let ist_neu = block_index >= alte_bloecke;
            if schreib_von >= schreib_bis {
                // Kein Schreib-Anteil: nur neue Lücken-Blöcke nullen.
                if ist_neu {
                    self.block_schreiben(block_nr as u64, &vec![0u8; BLOCK_GROESSE])?;
                }
                continue;
            }
            let mut block = if ist_neu || (schreib_von == block_von && schreib_bis == block_bis) {
                vec![0u8; BLOCK_GROESSE] // ganz neu oder ganz überschrieben
            } else {
                self.block_lesen(block_nr as u64)? // Randblock: RMW
            };
            block[schreib_von - block_von..schreib_bis - block_von]
                .copy_from_slice(&daten[schreib_von - offset..schreib_bis - offset]);
            self.block_schreiben(block_nr as u64, &block)?;
        }

        // 3. COMMIT: Zeigerliste (schreibt vorher den Indirektblock),
        //    Größe, Zeitstempel.
        self.zeiger_setzen(inode, &liste, indirekt_block)?;
        inode.groesse = neues_ende as u64;
        inode.geaendert = jetzt_stempel();
        self.inode_schreiben(nr, inode)?;
        Ok(daten.len())
    }

    // --- Verzeichnisse (§6) ----------------------------------------------

    fn dir_eintraege(&mut self, inode: &Inode) -> FsErgebnis<Vec<(u32, String)>> {
        let bytes = self.inhalt_lesen(inode)?;
        dir_parsen(&bytes)
    }

    /// Der Verzeichnis-COMMIT: schreibt die neue Eintragsliste in
    /// frische Blöcke und stellt den Verzeichnis-Inode um (§7).
    fn dir_umstellen(
        &mut self,
        nr: u32,
        inode: &mut Inode,
        eintraege: &[(u32, String)],
    ) -> FsErgebnis<()> {
        let bytes = dir_serialisieren(eintraege);
        self.inhalt_ersetzen(nr, inode, &bytes)
    }

    // --- Pfad-Auflösung ---------------------------------------------------

    /// Löst einen absoluten, normalisierten Pfad zur Inode-Nummer auf.
    fn aufloesen(&mut self, pfad: &str) -> FsErgebnis<u32> {
        let mut aktuell = WURZEL_INODE;
        for teil in pfad.split('/').filter(|t| !t.is_empty()) {
            let inode = self.inode_lesen(aktuell)?;
            if inode.typ != TYP_VERZEICHNIS {
                return Err(FsFehler::KeinVerzeichnis);
            }
            let eintraege = self.dir_eintraege(&inode)?;
            match eintraege.iter().find(|(_, name)| name == teil) {
                Some((nr, _)) => aktuell = *nr,
                None => return Err(FsFehler::NichtGefunden),
            }
        }
        Ok(aktuell)
    }

    /// Zerlegt den Pfad in (Eltern-Inode-Nr, Name) — für create,
    /// delete, rename. Die Wurzel selbst hat keinen Namen.
    fn eltern_aufloesen(&mut self, pfad: &str) -> FsErgebnis<(u32, String)> {
        let mut teile: Vec<&str> = pfad.split('/').filter(|t| !t.is_empty()).collect();
        let name = teile.pop().ok_or(FsFehler::UngueltigerPfad)?;
        name_pruefen(name)?;
        let mut eltern_pfad = String::from("/");
        eltern_pfad.push_str(&teile.join("/"));
        let eltern = self.aufloesen(&eltern_pfad)?;
        Ok((eltern, String::from(name)))
    }

    /// Gibt Inode + alle Blöcke eines Eintrags frei (Ordnung: der
    /// Verzeichnis-Verweis ist bereits weg — nur noch Lecks tilgen).
    fn inode_entsorgen(&mut self, nr: u32, inode: &Inode) -> FsErgebnis<()> {
        let mut bloecke = self.blockliste(inode)?;
        if inode.indirekt != 0 {
            bloecke.push(inode.indirekt);
        }
        let mut frei = inode.clone();
        frei.typ = TYP_FREI;
        self.inode_schreiben(nr, &frei)?;
        self.bloecke_freigeben(&bloecke)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// pruefe.speedfs — das Prüfwerkzeug (docs/speedfs-format.md §10)
// ---------------------------------------------------------------------------

/// Das Ergebnis eines Dateisystem-Checks. LECKS (belegt, aber
/// unreferenziert) sind der erwartete Absturz-Schaden und
/// reparierbar; DEFEKTE (Metadaten zeigen auf Falsches) dürfen dank
/// der Schreib-Reihenfolge nie entstehen und werden NUR gemeldet.
pub struct PruefBericht {
    /// Erreichbare (referenzierte) Inodes inkl. Wurzel.
    pub inodes_erreichbar: u32,
    /// Vom Baum referenzierte Blöcke (Daten + indirekte).
    pub bloecke_referenziert: u64,
    /// Belegte, aber unreferenzierte Blöcke (Absturz-Lecks).
    pub block_lecks: Vec<u32>,
    /// Belegte, aber unerreichbare Inodes (Absturz-Lecks).
    pub inode_lecks: Vec<u32>,
    /// Inode aus mehreren Verzeichnissen erreichbar (Befund nach
    /// rename-Absturz, §7) — harmlos, aber sichtbar gemacht.
    pub doppel_eintraege: Vec<String>,
    /// Harte Metadaten-Fehler. Werden NIE automatisch repariert.
    pub defekte: Vec<String>,
    /// true, wenn Lecks in diesem Lauf repariert wurden.
    pub repariert: bool,
}

impl PruefBericht {
    pub fn hat_lecks(&self) -> bool {
        !self.block_lecks.is_empty() || !self.inode_lecks.is_empty()
    }
}

impl SpeedFs {
    /// Prüft das komplette Dateisystem (Baum-Scan + Bilanz gegen
    /// Bitmap und Inode-Tabelle). `reparieren` gibt geleckte Blöcke
    /// und Inodes wieder frei — aber NUR, wenn es keine Defekte
    /// gibt (in ein defektes Dateisystem schreibt man nicht).
    /// Ein Err kommt nur bei ECHTEN Gerätefehlern; alles, was am
    /// Format kaputt ist, landet als Defekt im Bericht.
    pub fn pruefen(&self, reparieren: bool) -> FsErgebnis<PruefBericht> {
        let mut inner = self.inner.borrow_mut();
        let sb = inner.sb;
        let mut bericht = PruefBericht {
            inodes_erreichbar: 0,
            bloecke_referenziert: 0,
            block_lecks: Vec::new(),
            inode_lecks: Vec::new(),
            doppel_eintraege: Vec::new(),
            defekte: Vec::new(),
            repariert: false,
        };

        // Merkzettel: Welche Blöcke/Inodes erreicht der Baum?
        let mut block_ref = vec![false; sb.anzahl_bloecke as usize];
        let mut inode_ref = vec![false; sb.anzahl_inodes as usize];
        let block_gueltig = |nr: u32| (nr as u64) >= sb.daten_start && (nr as u64) < sb.anzahl_bloecke;

        // --- 1. Baum-Scan ab der Wurzel (iterativ, Pfade für die
        // --- Meldungen) ---------------------------------------------------
        let mut stapel: Vec<(u32, String)> = vec![(WURZEL_INODE, String::from("/"))];
        inode_ref[WURZEL_INODE as usize] = true;
        while let Some((nr, pfad)) = stapel.pop() {
            let inode = inner.inode_lesen(nr)?;
            bericht.inodes_erreichbar += 1;
            if inode.typ != TYP_DATEI && inode.typ != TYP_VERZEICHNIS {
                bericht
                    .defekte
                    .push(alloc::format!("{}: Inode {} hat Typ {} (frei/unbekannt)", pfad, nr, inode.typ));
                continue;
            }

            // Zeiger-Konsistenz zur Größe (§10): genau ⌈Größe/4096⌉
            // Zeiger, überzählige müssen 0 sein.
            let benoetigt = (inode.groesse as usize).div_ceil(BLOCK_GROESSE);
            if benoetigt > DIREKTE + ZEIGER_PRO_BLOCK {
                bericht
                    .defekte
                    .push(alloc::format!("{}: Groesse {} sprengt das Format", pfad, inode.groesse));
                continue;
            }
            let mut zeiger_defekt = false;
            for (i, zeiger) in inode.direkt.iter().enumerate() {
                let gebraucht = i < benoetigt.min(DIREKTE);
                if gebraucht && (*zeiger == 0 || !block_gueltig(*zeiger)) {
                    bericht.defekte.push(alloc::format!(
                        "{}: direkter Zeiger {} ungueltig ({})",
                        pfad, i, zeiger
                    ));
                    zeiger_defekt = true;
                }
                if !gebraucht && *zeiger != 0 {
                    bericht.defekte.push(alloc::format!(
                        "{}: direkter Zeiger {} muesste 0 sein (Groesse {})",
                        pfad, i, inode.groesse
                    ));
                    zeiger_defekt = true;
                }
            }
            if benoetigt > DIREKTE {
                if inode.indirekt == 0 || !block_gueltig(inode.indirekt) {
                    bericht.defekte.push(alloc::format!(
                        "{}: Indirektblock ungueltig ({})",
                        pfad, inode.indirekt
                    ));
                    zeiger_defekt = true;
                }
            } else if inode.indirekt != 0 {
                bericht
                    .defekte
                    .push(alloc::format!("{}: Indirektblock ohne Bedarf", pfad));
                zeiger_defekt = true;
            }
            if zeiger_defekt {
                continue; // kaputte Zeiger nicht auch noch dereferenzieren
            }

            // Blöcke einsammeln (Indirektblock zählt mit):
            let liste = inner.blockliste(&inode)?;
            let mut alle = liste;
            if inode.indirekt != 0 {
                alle.push(inode.indirekt);
            }
            let mut inhalt_lesbar = true;
            for block_nr in alle {
                if !block_gueltig(block_nr) {
                    bericht.defekte.push(alloc::format!(
                        "{}: indirekter Zeiger auf Block {} ausserhalb des Datenbereichs",
                        pfad, block_nr
                    ));
                    inhalt_lesbar = false;
                } else if block_ref[block_nr as usize] {
                    bericht.defekte.push(alloc::format!(
                        "{}: Block {} ist doppelt referenziert",
                        pfad, block_nr
                    ));
                } else {
                    block_ref[block_nr as usize] = true;
                    bericht.bloecke_referenziert += 1;
                }
            }

            // Verzeichnisse: Einträge prüfen und absteigen.
            if inode.typ == TYP_VERZEICHNIS && inhalt_lesbar {
                let bytes = inner.inhalt_lesen(&inode)?;
                let eintraege = match dir_parsen(&bytes) {
                    Ok(eintraege) => eintraege,
                    Err(_) => {
                        bericht
                            .defekte
                            .push(alloc::format!("{}: Eintragsliste nicht parsbar", pfad));
                        continue;
                    }
                };
                for (kind_nr, name) in eintraege {
                    let kind_pfad = if pfad == "/" {
                        alloc::format!("/{}", name)
                    } else {
                        alloc::format!("{}/{}", pfad, name)
                    };
                    if name_pruefen(&name).is_err() {
                        bericht
                            .defekte
                            .push(alloc::format!("{}: ungueltiger Name", kind_pfad));
                        continue;
                    }
                    if kind_nr == 0 || kind_nr >= sb.anzahl_inodes {
                        bericht.defekte.push(alloc::format!(
                            "{}: Eintrag zeigt auf Inode {} (ausserhalb der Tabelle)",
                            kind_pfad, kind_nr
                        ));
                        continue;
                    }
                    if inode_ref[kind_nr as usize] {
                        // §7: Doppel-Eintrag nach rename-Absturz —
                        // ein BEFUND, kein Defekt; nicht erneut absteigen.
                        bericht
                            .doppel_eintraege
                            .push(alloc::format!("{} (Inode {})", kind_pfad, kind_nr));
                        continue;
                    }
                    inode_ref[kind_nr as usize] = true;
                    stapel.push((kind_nr, kind_pfad));
                }
            }
        }

        // --- 2. Bilanz gegen Bitmap und Inode-Tabelle ---------------------
        // Die Bitmap EINMAL komplett einlesen (statt je Block durch
        // den Cache zu gehen):
        let mut bitmap = Vec::with_capacity((sb.bitmap_bloecke as usize) * BLOCK_GROESSE);
        for i in 0..sb.bitmap_bloecke {
            bitmap.extend_from_slice(&inner.block_lesen(sb.bitmap_start + i)?);
        }
        for block_nr in 0..sb.anzahl_bloecke {
            let belegt = bitmap[(block_nr / 8) as usize] & (1 << (block_nr % 8)) != 0;
            if block_nr < sb.daten_start {
                if !belegt {
                    bericht
                        .defekte
                        .push(alloc::format!("Metadaten-Block {} als frei markiert", block_nr));
                }
                continue;
            }
            match (belegt, block_ref[block_nr as usize]) {
                (true, false) => bericht.block_lecks.push(block_nr as u32),
                (false, true) => bericht.defekte.push(alloc::format!(
                    "Block {} referenziert, aber als frei markiert",
                    block_nr
                )),
                _ => {}
            }
        }
        for nr in 1..sb.anzahl_inodes {
            let inode = inner.inode_lesen(nr)?;
            if inode.typ != TYP_FREI && !inode_ref[nr as usize] {
                bericht.inode_lecks.push(nr);
            }
        }

        // --- 3. Reparatur: NUR Lecks, und NUR ohne Defekte ---------------
        if reparieren && bericht.defekte.is_empty() && bericht.hat_lecks() {
            for nr in &bericht.inode_lecks {
                let mut frei = inner.inode_lesen(*nr)?;
                frei.typ = TYP_FREI;
                inner.inode_schreiben(*nr, &frei)?;
            }
            inner.bloecke_freigeben(&bericht.block_lecks)?;
            bericht.repariert = true;
        }
        Ok(bericht)
    }
}

// ---------------------------------------------------------------------------
// Die FileSystem-Trait-Implementierung (die VFS-Naht)
// ---------------------------------------------------------------------------

impl FileSystem for SpeedFs {
    fn lesen(&self, pfad: &str) -> FsErgebnis<Vec<u8>> {
        let mut inner = self.inner.borrow_mut();
        let nr = inner.aufloesen(pfad)?;
        let inode = inner.inode_lesen(nr)?;
        if inode.typ != TYP_DATEI {
            return Err(FsFehler::KeineDatei);
        }
        inner.inhalt_lesen(&inode)
    }

    fn schreiben(&mut self, pfad: &str, inhalt: &[u8]) -> FsErgebnis<()> {
        let mut inner = self.inner.borrow_mut();
        let (eltern_nr, name) = inner.eltern_aufloesen(pfad)?;
        let mut eltern = inner.inode_lesen(eltern_nr)?;
        if eltern.typ != TYP_VERZEICHNIS {
            return Err(FsFehler::KeinVerzeichnis);
        }
        let eintraege = inner.dir_eintraege(&eltern)?;
        match eintraege.iter().find(|(_, n)| *n == name) {
            Some((nr, _)) => {
                // Existiert: Datei überschreiben, Verzeichnis ist Fehler.
                let nr = *nr;
                let mut inode = inner.inode_lesen(nr)?;
                if inode.typ != TYP_DATEI {
                    return Err(FsFehler::KeineDatei);
                }
                inner.inhalt_ersetzen(nr, &mut inode, inhalt)
            }
            None => {
                // Neu — Ordnung (§7 create): Inode belegen, Inhalt
                // schreiben (committet den Inode), ZULETZT der
                // Verzeichnis-Eintrag als Commit der Sichtbarkeit.
                let (nr, mut inode) = inner.inode_allozieren(TYP_DATEI)?;
                inner.inhalt_ersetzen(nr, &mut inode, inhalt)?;
                let mut neue = eintraege;
                neue.push((nr, name));
                inner.dir_umstellen(eltern_nr, &mut eltern, &neue)
            }
        }
    }

    fn liste(&self, pfad: &str) -> FsErgebnis<Vec<DirEintrag>> {
        let mut inner = self.inner.borrow_mut();
        let nr = inner.aufloesen(pfad)?;
        let inode = inner.inode_lesen(nr)?;
        if inode.typ != TYP_VERZEICHNIS {
            return Err(FsFehler::KeinVerzeichnis);
        }
        let mut eintraege = Vec::new();
        for (kind_nr, name) in inner.dir_eintraege(&inode)? {
            let kind = inner.inode_lesen(kind_nr)?;
            eintraege.push(DirEintrag {
                name,
                typ: kind.node_typ(),
                groesse: if kind.typ == TYP_DATEI {
                    kind.groesse as usize
                } else {
                    0
                },
                geaendert: kind.geaendert,
            });
        }
        eintraege.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(eintraege)
    }

    fn mkdir(&mut self, pfad: &str) -> FsErgebnis<()> {
        let mut inner = self.inner.borrow_mut();
        let (eltern_nr, name) = inner.eltern_aufloesen(pfad)?;
        let mut eltern = inner.inode_lesen(eltern_nr)?;
        if eltern.typ != TYP_VERZEICHNIS {
            return Err(FsFehler::KeinVerzeichnis);
        }
        let eintraege = inner.dir_eintraege(&eltern)?;
        if eintraege.iter().any(|(_, n)| *n == name) {
            return Err(FsFehler::ExistiertBereits);
        }
        // Ordnung wie create: Inode zuerst (leeres Verzeichnis hat
        // keine Datenblöcke), dann der Eltern-Eintrag als Commit.
        let (nr, _) = inner.inode_allozieren(TYP_VERZEICHNIS)?;
        let mut neue = eintraege;
        neue.push((nr, name));
        inner.dir_umstellen(eltern_nr, &mut eltern, &neue)
    }

    fn loeschen(&mut self, pfad: &str) -> FsErgebnis<()> {
        let mut inner = self.inner.borrow_mut();
        let (eltern_nr, name) = inner.eltern_aufloesen(pfad)?;
        let mut eltern = inner.inode_lesen(eltern_nr)?;
        let eintraege = inner.dir_eintraege(&eltern)?;
        let (nr, _) = *eintraege
            .iter()
            .find(|(_, n)| *n == name)
            .ok_or(FsFehler::NichtGefunden)?;
        let inode = inner.inode_lesen(nr)?;
        if inode.typ == TYP_VERZEICHNIS && inode.groesse > 0 {
            return Err(FsFehler::VerzeichnisNichtLeer);
        }
        // Ordnung (§7 delete): 1. COMMIT — der Eintrag verschwindet,
        // 2. Inode freigeben, 3. Blöcke freigeben.
        let neue: Vec<(u32, String)> = eintraege
            .into_iter()
            .filter(|(_, n)| *n != name)
            .collect();
        inner.dir_umstellen(eltern_nr, &mut eltern, &neue)?;
        inner.inode_entsorgen(nr, &inode)
    }

    fn node_typ(&self, pfad: &str) -> FsErgebnis<NodeTyp> {
        let mut inner = self.inner.borrow_mut();
        let nr = inner.aufloesen(pfad)?;
        Ok(inner.inode_lesen(nr)?.node_typ())
    }

    fn read_at(&self, pfad: &str, offset: usize, puffer: &mut [u8]) -> FsErgebnis<usize> {
        let mut inner = self.inner.borrow_mut();
        let nr = inner.aufloesen(pfad)?;
        let inode = inner.inode_lesen(nr)?;
        if inode.typ != TYP_DATEI {
            return Err(FsFehler::KeineDatei);
        }
        inner.daten_lesen(&inode, offset, puffer)
    }

    fn write_at(&mut self, pfad: &str, offset: usize, daten: &[u8]) -> FsErgebnis<usize> {
        let mut inner = self.inner.borrow_mut();
        let (eltern_nr, name) = inner.eltern_aufloesen(pfad)?;
        let mut eltern = inner.inode_lesen(eltern_nr)?;
        if eltern.typ != TYP_VERZEICHNIS {
            return Err(FsFehler::KeinVerzeichnis);
        }
        let eintraege = inner.dir_eintraege(&eltern)?;
        match eintraege.iter().find(|(_, n)| *n == name) {
            Some((nr, _)) => {
                let nr = *nr;
                let mut inode = inner.inode_lesen(nr)?;
                if inode.typ != TYP_DATEI {
                    return Err(FsFehler::KeineDatei);
                }
                inner.daten_schreiben_ab(nr, &mut inode, offset, daten)
            }
            None => {
                // write_at legt fehlende Dateien an (VFS-Vertrag).
                let (nr, mut inode) = inner.inode_allozieren(TYP_DATEI)?;
                let geschrieben = inner.daten_schreiben_ab(nr, &mut inode, offset, daten)?;
                let mut neue = eintraege;
                neue.push((nr, name));
                inner.dir_umstellen(eltern_nr, &mut eltern, &neue)?;
                Ok(geschrieben)
            }
        }
    }

    fn stat(&self, pfad: &str) -> FsErgebnis<Metadaten> {
        let mut inner = self.inner.borrow_mut();
        let nr = inner.aufloesen(pfad)?;
        let inode = inner.inode_lesen(nr)?;
        Ok(Metadaten {
            typ: inode.node_typ(),
            groesse: if inode.typ == TYP_DATEI {
                inode.groesse as usize
            } else {
                0
            },
            erstellt: inode.erstellt,
            geaendert: inode.geaendert,
        })
    }

    fn rename(&mut self, von: &str, nach: &str) -> FsErgebnis<()> {
        let mut inner = self.inner.borrow_mut();
        // Auf sich selbst: Erfolg, wenn es die Quelle gibt (POSIX).
        if von == nach {
            inner.aufloesen(von)?;
            return Ok(());
        }
        // Ziel im eigenen Teilbaum wäre Datenverlust (wie RamFs):
        if nach.starts_with(&alloc::format!("{}/", von)) {
            return Err(FsFehler::UngueltigerPfad);
        }
        let (von_eltern_nr, von_name) = inner.eltern_aufloesen(von)?;
        let (nach_eltern_nr, nach_name) = inner.eltern_aufloesen(nach)?;

        let von_eltern = inner.inode_lesen(von_eltern_nr)?;
        let von_eintraege = inner.dir_eintraege(&von_eltern)?;
        let (quelle_nr, _) = *von_eintraege
            .iter()
            .find(|(_, n)| *n == von_name)
            .ok_or(FsFehler::NichtGefunden)?;
        let quelle = inner.inode_lesen(quelle_nr)?;

        // Ziel prüfen: Datei-auf-Datei ersetzt, Verzeichnis ist Fehler.
        let mut nach_eltern = inner.inode_lesen(nach_eltern_nr)?;
        let nach_eintraege = inner.dir_eintraege(&nach_eltern)?;
        let ersetzt = match nach_eintraege.iter().find(|(_, n)| *n == nach_name) {
            Some((ziel_nr, _)) => {
                let ziel = inner.inode_lesen(*ziel_nr)?;
                if ziel.typ == TYP_VERZEICHNIS || quelle.typ == TYP_VERZEICHNIS {
                    return Err(FsFehler::ExistiertBereits);
                }
                Some((*ziel_nr, ziel))
            }
            None => None,
        };

        if von_eltern_nr == nach_eltern_nr {
            // Gleiches Verzeichnis: EIN Commit stellt die Liste um
            // (alter Name raus, neuer rein — nie beides, nie keins).
            let mut neue: Vec<(u32, String)> = von_eintraege
                .into_iter()
                .filter(|(_, n)| *n != von_name && *n != nach_name)
                .collect();
            neue.push((quelle_nr, nach_name));
            let mut eltern = von_eltern;
            inner.dir_umstellen(von_eltern_nr, &mut eltern, &neue)?;
        } else {
            // Ordnung (§7 rename): ZUERST der Ziel-Eintrag (nach einem
            // Absturz existiert der Eintrag schlimmstenfalls doppelt,
            // beide zeigen auf denselben Inode), DANN die Quelle raus.
            let mut neue_nach: Vec<(u32, String)> = nach_eintraege
                .into_iter()
                .filter(|(_, n)| *n != nach_name)
                .collect();
            neue_nach.push((quelle_nr, nach_name));
            inner.dir_umstellen(nach_eltern_nr, &mut nach_eltern, &neue_nach)?;

            let mut von_eltern = inner.inode_lesen(von_eltern_nr)?;
            let neue_von: Vec<(u32, String)> = inner
                .dir_eintraege(&von_eltern)?
                .into_iter()
                .filter(|(_, n)| *n != von_name)
                .collect();
            inner.dir_umstellen(von_eltern_nr, &mut von_eltern, &neue_von)?;
        }

        // Entkoppeln vor Freigeben: die ersetzte Ziel-Datei entsorgen.
        if let Some((ziel_nr, ziel)) = ersetzt {
            inner.inode_entsorgen(ziel_nr, &ziel)?;
        }
        Ok(())
    }

    fn sync(&mut self) -> FsErgebnis<()> {
        // Write-Through: Alle FS-Blöcke sind schon beim Gerät — nur
        // dessen interner Schreib-Cache muss noch aufs Medium.
        let mut inner = self.inner.borrow_mut();
        inner.geraet.sync()?;
        Ok(())
    }

    fn speicher_info(&self) -> FsErgebnis<Option<(u64, u64)>> {
        // frei = Bitmap-Zählung; gesamt = der DATENbereich (die
        // Metadaten-Blöcke sind kein nutzbarer Platz).
        let frei = self.freie_bloecke()?;
        let gesamt = {
            let inner = self.inner.borrow();
            inner.sb.anzahl_bloecke - inner.sb.daten_start
        };
        Ok(Some((
            frei * BLOCK_GROESSE as u64,
            gesamt * BLOCK_GROESSE as u64,
        )))
    }

    fn typ_name(&self) -> &'static str {
        "SpeedFS"
    }
}

// ---------------------------------------------------------------------------
// End-to-End-Sequenz (von den Tests geteilt: RamDisk-Unit-Test UND
// tests/e2e_speedfs.rs gegen die echte IDE-/virtio-Platte). Deshalb
// `pub` (Integrationstests sind eigene Crates und sehen nur die
// öffentliche API), aber `#[doc(hidden)]` — es ist reines Testgerüst.
// ---------------------------------------------------------------------------

/// Der große Ablauf auf EINEM Dateisystem, backend-agnostisch: Ordner +
/// Dateien anlegen, Editor-Roundtrip (write_at/read_at wie SpeedText
/// speichert/lädt, inkl. Editieren mitten in der Datei) und eine
/// rename-Orgie (gleicher Ordner / ordnerübergreifend / Ziel ersetzen).
/// ALLE Pfade unter `basis`, damit die Sequenz auch in einem Unterbaum
/// einer echten Platte läuft, ohne Nachbardaten zu stören. Panickt bei
/// jeder Abweichung — es ist Testcode.
#[doc(hidden)]
pub fn e2e_ops(fs: &mut dyn crate::fs::FileSystem, basis: &str) {
    let p = |name: &str| alloc::format!("{}/{}", basis, name);

    // mkfs hat der Aufrufer erledigt; hier nur der Basis-Ordner:
    fs.mkdir(basis).expect("e2e: mkdir basis");

    // 1. Dateien + Unterordner:
    fs.schreiben(&p("hallo.txt"), b"Hallo SpeedOS").expect("e2e: schreiben hallo");
    fs.mkdir(&p("unter")).expect("e2e: mkdir unter");
    fs.schreiben(&p("unter/tief.txt"), b"tief verschachtelt").expect("e2e: schreiben tief");

    // 2. Editor-Roundtrip: schreiben wie SpeedText speichert (write_at ab
    //    0), lesen wie es lädt (read_at) — Inhalt identisch. Dann mitten
    //    in der Datei editieren und erneut prüfen.
    let text = b"Zeile eins\nZeile zwei\nZeile drei\n";
    let n = fs.write_at(&p("doc.txt"), 0, text).expect("e2e: write_at doc");
    assert_eq!(n, text.len(), "e2e: write_at schrieb zu wenig");
    let mut puffer = alloc::vec![0u8; text.len()];
    let gelesen = fs.read_at(&p("doc.txt"), 0, &mut puffer).expect("e2e: read_at doc");
    assert_eq!(gelesen, text.len(), "e2e: read_at las zu wenig");
    assert_eq!(&puffer[..], &text[..], "e2e: doc-Inhalt weicht ab");
    // "eins" (Offset 6) -> "EINS" (in-place-Overwrite):
    fs.write_at(&p("doc.txt"), 6, b"EINS").expect("e2e: write_at edit");
    assert_eq!(
        fs.lesen(&p("doc.txt")).expect("e2e: lesen doc"),
        b"Zeile EINS\nZeile zwei\nZeile drei\n"
    );

    // 3. rename-Orgie (alle Spielarten, innerhalb basis):
    fs.rename(&p("hallo.txt"), &p("hallo2.txt")).expect("e2e: rename gleicher Ordner");
    fs.rename(&p("hallo2.txt"), &p("unter/hallo3.txt")).expect("e2e: rename ordneruebergreifend");
    fs.schreiben(&p("ziel.txt"), b"wird ersetzt").expect("e2e: schreiben ziel");
    fs.rename(&p("unter/hallo3.txt"), &p("ziel.txt")).expect("e2e: rename Ziel ersetzen");

    fs.sync().expect("e2e: sync");
}

/// Prüft den End-Zustand von e2e_ops (nach optionalem Absturz+Remount):
/// alle erwarteten Dateien mit erwartetem Inhalt, die weggerenamten weg.
#[doc(hidden)]
pub fn e2e_verifizieren(fs: &mut dyn crate::fs::FileSystem, basis: &str) {
    let p = |name: &str| alloc::format!("{}/{}", basis, name);
    // hallo.txt wanderte über die rename-Orgie nach ziel.txt (mit dem
    // Inhalt der ersten Datei); die Zwischennamen existieren nicht mehr:
    assert_eq!(fs.lesen(&p("ziel.txt")).expect("e2e-v: ziel"), b"Hallo SpeedOS");
    assert!(fs.lesen(&p("hallo.txt")).is_err(), "e2e-v: hallo.txt sollte weg sein");
    assert!(fs.lesen(&p("hallo2.txt")).is_err(), "e2e-v: hallo2.txt sollte weg sein");
    assert!(fs.lesen(&p("unter/hallo3.txt")).is_err(), "e2e-v: hallo3 sollte weg sein");
    // doc.txt (editiert) + tief.txt sind unverändert da:
    assert_eq!(
        fs.lesen(&p("doc.txt")).expect("e2e-v: doc"),
        b"Zeile EINS\nZeile zwei\nZeile drei\n"
    );
    assert_eq!(fs.lesen(&p("unter/tief.txt")).expect("e2e-v: tief"), b"tief verschachtelt");
}

// ---------------------------------------------------------------------------
// Tests — auf der RamDisk (schnell, ohne QEMU-Neustart)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::block::RamDisk;
    use super::*;

    /// Frisch formatiertes 8-MiB-SpeedFS auf einer RamDisk.
    fn test_fs() -> SpeedFs {
        let mut disk = RamDisk::neu(512, 16384); // 8 MiB
        formatieren(&mut disk).expect("mkfs fehlgeschlagen");
        SpeedFs::mounten(Box::new(disk)).map_err(|(f, _)| f).expect("Mount fehlgeschlagen")
    }

    /// Superblock-Roundtrip und die Ablehnung fremder Platten.
    #[test_case]
    fn test_speedfs_superblock() {
        let sb = layout_berechnen(64 * 1024 * 1024);
        assert_eq!(sb.anzahl_bloecke, 16384);
        assert_eq!(sb.bitmap_bloecke, 1);
        assert_eq!(sb.anzahl_inodes, 1024);
        assert_eq!(sb.inode_bloecke, 32);
        assert_eq!(sb.daten_start, 34);
        // Roundtrip:
        let block = sb.serialisieren();
        assert_eq!(Superblock::parsen(&block, 16384), Ok(sb));
        // Fremde Platte (kein Magic) -> KeinSpeedFs:
        assert_eq!(
            Superblock::parsen(&vec![0u8; BLOCK_GROESSE], 16384),
            Err(FsFehler::KeinSpeedFs)
        );
    }

    /// Inode- und Verzeichnis-Serialisierung sind exakte Roundtrips.
    #[test_case]
    fn test_speedfs_serialisierung() {
        let mut inode = Inode::leer(TYP_DATEI);
        inode.groesse = 4711;
        inode.direkt[0] = 34;
        inode.direkt[21] = 99;
        inode.indirekt = 100;
        assert_eq!(Inode::parsen(&inode.serialisieren()), inode);

        let eintraege = vec![
            (1u32, String::from("hallo.txt")),
            (7u32, String::from("Ordner mit Leerzeichen")),
        ];
        let bytes = dir_serialisieren(&eintraege);
        assert_eq!(dir_parsen(&bytes), Ok(eintraege));
        // Abgeschnittene Daten sind ein erkannter Fehler, kein Panik:
        assert!(dir_parsen(&bytes[..bytes.len() - 3]).is_err());
    }

    /// mkfs + mount: leere Wurzel, dann Datei-Roundtrip über die
    /// VFS-Trait-Methoden.
    #[test_case]
    fn test_speedfs_mkfs_mount_roundtrip() {
        let mut fs = test_fs();
        assert!(fs.liste("/").unwrap().is_empty());

        fs.schreiben("/gruss.txt", b"Hallo, SpeedFS!").unwrap();
        assert_eq!(fs.lesen("/gruss.txt").unwrap(), b"Hallo, SpeedFS!");
        assert_eq!(fs.node_typ("/gruss.txt").unwrap(), NodeTyp::Datei);

        let stat = fs.stat("/gruss.txt").unwrap();
        assert_eq!(stat.groesse, 15);
        assert_eq!(stat.typ, NodeTyp::Datei);

        // Überschreiben ändert Inhalt und Größe:
        fs.schreiben("/gruss.txt", b"neu").unwrap();
        assert_eq!(fs.lesen("/gruss.txt").unwrap(), b"neu");

        // Mount einer UNformatierten Platte scheitert sauber:
        let roh = RamDisk::neu(512, 2048);
        match SpeedFs::mounten(Box::new(roh)) {
            Err((FsFehler::KeinSpeedFs, _)) => {}
            _ => panic!("unformatierte Platte darf nicht mounten"),
        }
    }

    /// Eine Datei über mehrere Blockgrenzen (3,5 Blöcke) plus
    /// read_at/write_at an den Rändern.
    #[test_case]
    fn test_speedfs_datei_ueber_blockgrenzen() {
        let mut fs = test_fs();
        let laenge = 3 * BLOCK_GROESSE + BLOCK_GROESSE / 2;
        let mut daten = vec![0u8; laenge];
        for (i, byte) in daten.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        fs.schreiben("/gross.bin", &daten).unwrap();
        assert_eq!(fs.lesen("/gross.bin").unwrap(), daten);

        // read_at mitten über eine Blockgrenze:
        let mut stueck = [0u8; 100];
        let gelesen = fs.read_at("/gross.bin", BLOCK_GROESSE - 50, &mut stueck).unwrap();
        assert_eq!(gelesen, 100);
        assert_eq!(stueck[..], daten[BLOCK_GROESSE - 50..BLOCK_GROESSE + 50]);

        // write_at über die Grenze, dann zurücklesen:
        fs.write_at("/gross.bin", BLOCK_GROESSE - 2, b"XXXX").unwrap();
        let mut kontrolle = [0u8; 4];
        fs.read_at("/gross.bin", BLOCK_GROESSE - 2, &mut kontrolle).unwrap();
        assert_eq!(&kontrolle, b"XXXX");

        // write_at hinterm Dateiende: Lücke wird zu Nullen.
        let ende = laenge + BLOCK_GROESSE;
        fs.write_at("/gross.bin", ende, b"ENDE").unwrap();
        assert_eq!(fs.stat("/gross.bin").unwrap().groesse, ende + 4);
        let mut luecke = [0xAAu8; 16];
        fs.read_at("/gross.bin", laenge + 10, &mut luecke).unwrap();
        assert!(luecke.iter().all(|b| *b == 0));
    }

    /// Der einfach-indirekte Block: eine Datei größer als die 22
    /// direkten Zeiger (88 KiB) muss korrekt geschrieben, gelesen
    /// und wieder freigegeben werden.
    #[test_case]
    fn test_speedfs_indirekter_block() {
        let mut fs = test_fs();
        let frei_vorher = fs.freie_bloecke().unwrap();

        let laenge = (DIREKTE + 10) * BLOCK_GROESSE + 123; // 32 Blöcke + Rest
        let mut daten = vec![0u8; laenge];
        for (i, byte) in daten.iter_mut().enumerate() {
            *byte = (i % 239) as u8;
        }
        fs.schreiben("/indirekt.bin", &daten).unwrap();
        assert_eq!(fs.lesen("/indirekt.bin").unwrap(), daten);

        // Zu große Datei: sauberer Fehler statt Kaputt-Schreiben.
        assert_eq!(
            fs.write_at("/indirekt.bin", MAX_DATEI, b"x"),
            Err(FsFehler::DateiZuGross)
        );

        // Löschen gibt ALLES frei — auch den Indirektblock (das
        // prüft die Bilanz: exakt der Ausgangsstand).
        fs.loeschen("/indirekt.bin").unwrap();
        assert_eq!(fs.freie_bloecke().unwrap(), frei_vorher);
    }

    /// Verzeichnis mit so vielen Einträgen, dass die Liste über
    /// mehrere Blöcke geht (Blocküberlauf im Verzeichnis).
    /// 100 Einträge mit langen Namen: ~50 Bytes je Eintrag = ~5 KiB
    /// Verzeichnis-Liste > 1 Block — und 100 Inodes passen in die
    /// 128 der 8-MiB-Test-Platte (docs/speedfs-format.md §8).
    #[test_case]
    fn test_speedfs_grosses_verzeichnis() {
        let mut fs = test_fs();
        fs.mkdir("/viele").unwrap();
        for i in 0..100 {
            fs.schreiben(
                &alloc::format!("/viele/eintrag-{:03}-mit-einem-extra-langen-namen.txt", i),
                b"x",
            )
            .unwrap();
        }
        let liste = fs.liste("/viele").unwrap();
        assert_eq!(liste.len(), 100);
        // Alphabetisch sortiert, und ein Stichproben-Eintrag stimmt:
        assert_eq!(liste[0].name, "eintrag-000-mit-einem-extra-langen-namen.txt");
        assert_eq!(liste[99].name, "eintrag-099-mit-einem-extra-langen-namen.txt");
        assert_eq!(
            fs.lesen("/viele/eintrag-050-mit-einem-extra-langen-namen.txt")
                .unwrap(),
            b"x"
        );
        // Nicht-leeres Verzeichnis löschen ist ein Fehler:
        assert_eq!(fs.loeschen("/viele"), Err(FsFehler::VerzeichnisNichtLeer));
    }

    /// Löschen gibt Blöcke frei — die Bitmap-Bilanz stimmt exakt,
    /// auch nach Verzeichnis-Umbauten.
    #[test_case]
    fn test_speedfs_loeschen_gibt_bloecke_frei() {
        let mut fs = test_fs();
        let frei_vorher = fs.freie_bloecke().unwrap();

        fs.mkdir("/tmp").unwrap();
        fs.schreiben("/tmp/a.bin", &vec![7u8; 3 * BLOCK_GROESSE]).unwrap();
        fs.schreiben("/tmp/b.txt", b"kurz").unwrap();
        assert!(fs.freie_bloecke().unwrap() < frei_vorher);

        fs.loeschen("/tmp/a.bin").unwrap();
        fs.loeschen("/tmp/b.txt").unwrap();
        fs.loeschen("/tmp").unwrap();
        assert_eq!(fs.freie_bloecke().unwrap(), frei_vorher);
    }

    /// rename: gleiche/verschiedene Verzeichnisse, Ersetzen, Fehler.
    #[test_case]
    fn test_speedfs_rename() {
        let mut fs = test_fs();
        fs.schreiben("/alt.txt", b"inhalt").unwrap();
        let stat_vorher = fs.stat("/alt.txt").unwrap();

        // Umbenennen im selben Verzeichnis; Zeitstempel wandern mit.
        fs.rename("/alt.txt", "/neu.txt").unwrap();
        assert_eq!(fs.lesen("/neu.txt").unwrap(), b"inhalt");
        assert_eq!(fs.stat("/neu.txt").unwrap(), stat_vorher);
        assert_eq!(fs.lesen("/alt.txt"), Err(FsFehler::NichtGefunden));

        // In ein anderes Verzeichnis verschieben:
        fs.mkdir("/ordner").unwrap();
        fs.rename("/neu.txt", "/ordner/drin.txt").unwrap();
        assert_eq!(fs.lesen("/ordner/drin.txt").unwrap(), b"inhalt");

        // Ziel-Datei wird ersetzt (und ihre Blöcke kommen zurück):
        let frei_vorher = fs.freie_bloecke().unwrap();
        fs.schreiben("/opfer.txt", &vec![1u8; 2 * BLOCK_GROESSE]).unwrap();
        fs.rename("/ordner/drin.txt", "/opfer.txt").unwrap();
        assert_eq!(fs.lesen("/opfer.txt").unwrap(), b"inhalt");
        // Bilanz: Die Opfer-Blöcke sind zurück, UND /ordner ist jetzt
        // leer — seine Eintragsliste (1 Block) wurde auch frei:
        assert_eq!(fs.freie_bloecke().unwrap(), frei_vorher + 1);

        // Ziel-Verzeichnis ist Fehler; Teilbaum-Schutz greift.
        assert_eq!(
            fs.rename("/opfer.txt", "/ordner"),
            Err(FsFehler::ExistiertBereits)
        );
        assert_eq!(
            fs.rename("/ordner", "/ordner/kind"),
            Err(FsFehler::UngueltigerPfad)
        );
    }

    /// Mount-Fehlerpfade: jeder Fehler kommt SAUBER (kein Panic) und
    /// gibt das Gerät im Fehler-Tupel zurück, damit der Aufrufer es
    /// z. B. formatieren kann (die Naht, die platte_automounten nutzt).
    #[test_case]
    fn test_speedfs_mount_fehlerpfade() {
        // (a) Unformatierte (genullte) Platte -> KeinSpeedFs.
        let disk = RamDisk::neu(512, 4096); // 2 MiB, alles Null
        match SpeedFs::mounten(Box::new(disk)) {
            Err((FsFehler::KeinSpeedFs, _zurueck)) => {}
            Err((f, _)) => panic!("unformatiert: erwartete KeinSpeedFs, kam {:?}", f),
            Ok(_) => panic!("eine unformatierte Platte darf nicht mounten"),
        }

        // (b) Zu kleines Gerät (1 Block) -> formatieren gibt Voll,
        //     statt beim Layout zu panicken.
        let mut winzig = RamDisk::neu(512, 8); // genau 1 Block (4 KiB)
        assert_eq!(formatieren(&mut winzig), Err(FsFehler::Voll));

        // (c) Krumme Sektorgröße (BLOCK_GROESSE nicht durch sie teilbar)
        //     -> Io(UngueltigePufferGroesse), noch VOR dem ersten Lesen.
        let krumm = RamDisk::neu(513, 100);
        match SpeedFs::mounten(Box::new(krumm)) {
            Err((FsFehler::Io(crate::fs::block::IoFehler::UngueltigePufferGroesse), _)) => {}
            Err((f, _)) => panic!("krumme Sektorgröße: erwartete Io, kam {:?}", f),
            Ok(_) => panic!("krumme Sektorgröße darf nicht mounten"),
        }

        // (d) Kaputter Superblock (formatiert, dann Magic gekippt) ->
        //     KeinSpeedFs (die Superblock-Validierung greift).
        let mut disk2 = RamDisk::neu(512, 4096);
        formatieren(&mut disk2).unwrap();
        let mut block0 = vec![0u8; 512];
        disk2.lese_sektoren(0, &mut block0).unwrap();
        block0[0] ^= 0xFF; // Magic-Byte zerstören
        disk2.schreibe_sektoren(0, &block0).unwrap();
        match SpeedFs::mounten(Box::new(disk2)) {
            Err((FsFehler::KeinSpeedFs, _)) => {}
            Err((f, _)) => panic!("kaputter Superblock: erwartete KeinSpeedFs, kam {:?}", f),
            Ok(_) => panic!("kaputter Superblock darf nicht mounten"),
        }
    }

    /// Volle Platte: der Schreib-Pfad liefert SAUBER FsFehler::Voll,
    /// korrumpiert NICHTS (vorherige Dateien bleiben Bit-für-Bit
    /// erhalten) und der fsck findet danach keine Defekte. Genau das
    /// garantiert die alles-oder-nichts-Allokation (bloecke_allozieren).
    #[test_case]
    fn test_speedfs_voll_sauber() {
        let mut disk = RamDisk::neu(512, 1024); // 512 KiB
        formatieren(&mut disk).unwrap();
        let mut fs = SpeedFs::mounten(Box::new(disk)).map_err(|(f, _)| f).unwrap();

        // Zwei "unantastbare" Dateien anlegen und Inhalte merken:
        fs.schreiben("/bleibt1.txt", b"heiliger Inhalt").unwrap();
        let gross_inhalt = vec![0xABu8; 5000]; // > 1 Block
        fs.schreiben("/bleibt2.txt", &gross_inhalt).unwrap();
        let ref1 = fs.lesen("/bleibt1.txt").unwrap();

        // EINE wachsende Datei bis zur Datenblock-Erschöpfung (nur EIN
        // Inode, damit wirklich die BLÖCKE ausgehen, nicht die Inodes):
        let block = vec![0x55u8; BLOCK_GROESSE];
        let mut off = 0usize;
        let mut voll = false;
        while off <= MAX_DATEI {
            match fs.write_at("/fueller.bin", off, &block) {
                Ok(n) => {
                    assert!(n > 0, "write_at machte keinen Fortschritt");
                    off += n;
                }
                Err(FsFehler::Voll) => {
                    voll = true;
                    break;
                }
                Err(e) => panic!("unerwarteter Fehler beim Füllen: {:?}", e),
            }
        }
        assert!(voll, "die Datenblöcke wurden nie voll");

        // (1) Eine NEUE Datei auf der vollen Platte -> sauber Voll:
        assert_eq!(
            fs.schreiben("/geht_nicht.txt", b"kein Platz"),
            Err(FsFehler::Voll)
        );
        // (2) Die unantastbaren Dateien sind UNVERÄNDERT:
        assert_eq!(fs.lesen("/bleibt1.txt").unwrap(), ref1);
        assert_eq!(fs.lesen("/bleibt2.txt").unwrap(), gross_inhalt);

        // (3) fsck (write-through -> Platte ist konsistent): keine
        //     Defekte (Lecks wären erlaubt; hier gibt es dank
        //     alles-oder-nichts gar keine).
        let bericht = fs.pruefen(false).unwrap();
        assert!(
            bericht.defekte.is_empty(),
            "volle Platte hinterließ DEFEKTE: {:?}",
            bericht.defekte
        );
    }

    // --- Der Folter-Test: Absturz nach N Schreibvorgängen --------------

    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicI64, Ordering};

    /// Ein BlockDevice, das einen STROMAUSFALL simuliert: Nach dem
    /// Schreib-Budget verschwinden alle weiteren Writes spurlos
    /// (Ok, aber nie auf der "Platte") — der Kernel arbeitet ahnungslos
    /// weiter, genau wie vor einem echten Absturz. Die Platte sieht
    /// damit ein exaktes PRÄFIX der Schreibfolge. Die RamDisk steckt
    /// in einem Arc, damit der Test nach dem "Absturz" (Drop des FS
    /// samt Cache) mit demselben Platten-Zustand neu mounten kann.
    struct AbsturzDisk {
        disk: Arc<spin::Mutex<RamDisk>>,
        budget: Arc<AtomicI64>,
    }

    impl BlockDevice for AbsturzDisk {
        fn sektor_groesse(&self) -> usize {
            512
        }
        fn anzahl_sektoren(&self) -> u64 {
            self.disk.lock().anzahl_sektoren()
        }
        fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
            self.disk.lock().lese_sektoren(start, puffer)
        }
        fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
            if self.budget.fetch_sub(1, Ordering::Relaxed) <= 0 {
                return Ok(()); // verworfen — der "Strom ist weg"
            }
            self.disk.lock().schreibe_sektoren(start, puffer)
        }
        fn sync(&mut self) -> Result<(), IoFehler> {
            Ok(())
        }
    }

    /// Die deterministische Op-Serie: create, write (mehrblöckig +
    /// read-modify-write), rename in allen Spielarten, delete.
    /// Ergebnisse bewusst ignoriert — nach dem Abschneide-Punkt ist
    /// die FS-Sicht Phantom; zählen tut nur, was auf der Platte ist.
    fn folter_serie(fs: &mut SpeedFs) {
        let _ = fs.mkdir("/a");
        let _ = fs.schreiben("/a/eins.txt", b"erste Datei");
        let _ = fs.schreiben("/gross.bin", &vec![7u8; 3 * BLOCK_GROESSE + 100]);
        let _ = fs.write_at("/gross.bin", BLOCK_GROESSE - 10, b"mittendrin");
        let _ = fs.rename("/a/eins.txt", "/a/zwei.txt"); // gleicher Ordner
        let _ = fs.mkdir("/b");
        let _ = fs.rename("/a/zwei.txt", "/b/drei.txt"); // ordnerübergreifend
        let _ = fs.schreiben("/opfer.txt", b"wird ersetzt");
        let _ = fs.rename("/b/drei.txt", "/opfer.txt"); // Ziel ersetzen
        let _ = fs.loeschen("/gross.bin");
        let _ = fs.schreiben("/b/vier.txt", b"noch eine");
        let _ = fs.loeschen("/a"); // leer -> weg
    }

    /// Ein Folterlauf: frisches FS, Serie mit Schreib-Budget,
    /// "Absturz" (Drop), Wiedermount, pruefen. Liefert (verbrauchte
    /// Schreibvorgänge des Laufs, Bericht VOR der Reparatur).
    fn folter_lauf(budget_start: i64) -> (i64, PruefBericht) {
        let disk = Arc::new(spin::Mutex::new(RamDisk::neu(512, 4096))); // 2 MiB
        // mkfs VOR dem Budget: Ein Absturz mittendrin ist per
        // "Superblock zuletzt" ohnehin unsichtbar (§7).
        formatieren(&mut *disk.lock()).expect("mkfs fehlgeschlagen");
        let budget = Arc::new(AtomicI64::new(budget_start));
        {
            let wrapper = AbsturzDisk {
                disk: disk.clone(),
                budget: budget.clone(),
            };
            let mut fs = SpeedFs::mounten(Box::new(wrapper))
                .map_err(|(f, _)| f)
                .expect("Mount fehlgeschlagen");
            folter_serie(&mut fs);
            // Drop des FS = der Absturz: Cache und RAM-Sicht sind weg.
        }
        let verbraucht = budget_start - budget.load(Ordering::Relaxed);

        // Wiedermount vom rohen Platten-Stand, dann der Check:
        let frisch = AbsturzDisk {
            disk,
            budget: Arc::new(AtomicI64::new(i64::MAX)),
        };
        let fs = SpeedFs::mounten(Box::new(frisch))
            .map_err(|(f, _)| f)
            .expect("Wiedermount nach Absturz fehlgeschlagen");
        let bericht = fs.pruefen(false).expect("pruefen fehlgeschlagen");

        // Wenn die Metadaten heil sind: Reparatur muss alle Lecks
        // tilgen und danach sauber sein.
        if bericht.defekte.is_empty() {
            fs.pruefen(true).expect("Reparatur fehlgeschlagen");
            let danach = fs.pruefen(false).expect("Kontroll-Lauf fehlgeschlagen");
            assert!(
                danach.defekte.is_empty() && !danach.hat_lecks(),
                "Nach der Reparatur muss das FS sauber sein"
            );
        }
        (verbraucht, bericht)
    }

    /// DER Folter-Test (§7-Beweis): Die Schreibfolge an JEDER Stelle
    /// abschneiden — Lecks sind erlaubt, kaputte Metadaten NIE.
    #[test_case]
    fn test_speedfs_folter_absturz() {
        // Referenzlauf ohne Absturz: zählt die Schreibvorgänge und
        // muss völlig sauber sein (keine Lecks im Normalbetrieb!).
        let (gesamt, sauber) = folter_lauf(1_000_000);
        assert!(sauber.defekte.is_empty(), "Referenzlauf defekt: {:?}", sauber.defekte);
        assert!(!sauber.hat_lecks(), "Referenzlauf leckt: {:?}", sauber.block_lecks);
        assert!(sauber.doppel_eintraege.is_empty());
        assert!(gesamt > 50, "Serie zu kurz fuer einen ernsten Foltertest");

        // Jetzt die Folter: jeden Abschneide-Punkt durchprobieren.
        let mut laeufe_mit_lecks = 0;
        for n in 0..gesamt {
            let (_, bericht) = folter_lauf(n);
            assert!(
                bericht.defekte.is_empty(),
                "Absturz nach {} Schreibvorgaengen hinterlaesst DEFEKTE: {:?}",
                n,
                bericht.defekte
            );
            if bericht.hat_lecks() {
                laeufe_mit_lecks += 1;
            }
        }
        // Plausibilität: Irgendwo MUSS es Lecks gegeben haben (sonst
        // testet der Test nichts) — die Ordering-Disziplin erzeugt
        // sie ja absichtlich statt kaputter Metadaten.
        assert!(laeufe_mit_lecks > 0, "Kein einziger Lauf leckte — Test wirkungslos?");
        crate::serial_println!(
            "[FOLTER] {} Abschneide-Punkte geprueft, {} mit (reparierten) Lecks, 0 Defekte.",
            gesamt,
            laeufe_mit_lecks
        );
    }

    /// Wie folter_lauf, aber die Platte ist schon FAST VOLL, bevor die
    /// Op-Serie startet: erst große Füller schreiben (persistent, außer-
    /// halb des Budgets), bis Voll, dann EINEN wieder löschen — so
    /// bleiben nur wenige Blöcke frei (aber reichlich Inodes). Die
    /// Op-Serie trifft dann unterwegs FsFehler::Voll. Liefert den
    /// Bericht vor jeder Reparatur.
    fn folter_lauf_fast_voll(budget_start: i64) -> PruefBericht {
        let disk = Arc::new(spin::Mutex::new(RamDisk::neu(512, 512))); // 256 KiB
        formatieren(&mut *disk.lock()).expect("mkfs fehlgeschlagen");

        // Fast voll machen (persistent, außerhalb des Budgets):
        {
            let wrapper = AbsturzDisk {
                disk: disk.clone(),
                budget: Arc::new(AtomicI64::new(i64::MAX)),
            };
            let mut fs = SpeedFs::mounten(Box::new(wrapper)).map_err(|(f, _)| f).unwrap();
            let brocken = vec![0x33u8; 4 * BLOCK_GROESSE]; // 4 Blöcke je Datei
            let mut i = 0;
            while i < 30 && fs.schreiben(&alloc::format!("/f{}", i), &brocken).is_ok() {
                i += 1;
            }
            assert!(i >= 2, "Vorfüllen hat zu wenig geschrieben (Platte zu klein?)");
            fs.loeschen("/f0").unwrap(); // 4 Blöcke wieder frei
            fs.sync().unwrap();
        }

        // Budgetierter Lauf mit "Absturz" (Drop des FS):
        {
            let wrapper = AbsturzDisk {
                disk: disk.clone(),
                budget: Arc::new(AtomicI64::new(budget_start)),
            };
            let mut fs = SpeedFs::mounten(Box::new(wrapper)).map_err(|(f, _)| f).unwrap();
            folter_serie(&mut fs);
        }

        // Wiedermount vom rohen Platten-Stand, dann prüfen:
        let frisch = AbsturzDisk {
            disk,
            budget: Arc::new(AtomicI64::new(i64::MAX)),
        };
        let fs = SpeedFs::mounten(Box::new(frisch)).map_err(|(f, _)| f).unwrap();
        fs.pruefen(false).expect("pruefen fehlgeschlagen")
    }

    /// Folter-Variante auf FAST VOLLER Platte: die Op-Serie läuft
    /// unterwegs in FsFehler::Voll UND der Absturz schneidet an jeder
    /// Stelle. Invariante wie beim großen Folter-Test: Lecks erlaubt,
    /// Defekte NIE — die §7-Ordering-Disziplin gilt auch unter
    /// Platzmangel (eine gescheiterte Allokation ändert nichts).
    #[test_case]
    fn test_speedfs_folter_fast_voll() {
        // Referenzlauf ohne Absturz: keine Defekte (Lecks hier möglich,
        // weil Ops an Voll scheitern dürfen).
        let sauber = folter_lauf_fast_voll(1_000_000);
        assert!(
            sauber.defekte.is_empty(),
            "Referenz (fast voll) defekt: {:?}",
            sauber.defekte
        );
        // Jeden Abschneide-Punkt der Op-Serie durchprobieren:
        for n in 0..80 {
            let bericht = folter_lauf_fast_voll(n);
            assert!(
                bericht.defekte.is_empty(),
                "Fast-voll-Absturz nach {} Writes hinterlaesst DEFEKTE: {:?}",
                n,
                bericht.defekte
            );
        }
        crate::serial_println!(
            "[FOLTER-VOLL] 80 Abschneide-Punkte auf fast voller Platte, 0 Defekte."
        );
    }

    /// Großer End-to-End-Test auf der RamDisk: mkfs -> Dateien ->
    /// Editor-Roundtrip -> rename-Orgie -> ABSTURZ (Drop) -> Wiedermount
    /// -> pruefen (0 Defekte, 0 Lecks) -> alles noch da. Dieselbe
    /// Sequenz (e2e_ops) läuft gegen IDE und virtio in
    /// tests/e2e_speedfs.rs.
    #[test_case]
    fn test_speedfs_e2e_ramdisk() {
        let disk = Arc::new(spin::Mutex::new(RamDisk::neu(512, 16384))); // 8 MiB
        formatieren(&mut *disk.lock()).unwrap();
        {
            let wrapper = AbsturzDisk {
                disk: disk.clone(),
                budget: Arc::new(AtomicI64::new(i64::MAX)),
            };
            let mut fs = SpeedFs::mounten(Box::new(wrapper)).map_err(|(f, _)| f).unwrap();
            e2e_ops(&mut fs, "/e2e");
            // Drop = Absturz nach getaner Arbeit (Cache weg; write-through
            // -> die RamDisk trägt bereits alles).
        }
        let frisch = AbsturzDisk {
            disk,
            budget: Arc::new(AtomicI64::new(i64::MAX)),
        };
        let mut fs = SpeedFs::mounten(Box::new(frisch)).map_err(|(f, _)| f).unwrap();
        let bericht = fs.pruefen(false).unwrap();
        assert!(
            bericht.defekte.is_empty() && !bericht.hat_lecks(),
            "E2E nach Absturz nicht sauber: Defekte {:?}, Lecks {:?}",
            bericht.defekte,
            bericht.block_lecks
        );
        e2e_verifizieren(&mut fs, "/e2e");
    }

    /// Einstellungen überleben einen SIMULIERTEN Neustart
    /// (umount + mount): Der Wert wird auf die "Platte" (RamDisk am
    /// globalen /platte-Mount) geschrieben, der RAM-Zustand radiert,
    /// neu gemountet und geladen — der Wert MUSS zurückkommen.
    /// Nebenbei der Fallback-Beweis: Ohne Mount zeigt
    /// einstellungen::pfad() auf das RAM-VFS, mit Mount auf die
    /// Platte (fs::persistenter_pfad — die EINE Abstraktion).
    #[test_case]
    fn test_speedfs_einstellungen_roundtrip_neustart() {
        use crate::einstellungen;

        // Ohne Platte: der RAM-Fallback-Pfad.
        assert!(!crate::fs::ist_gemountet(crate::fs::PLATTE));
        assert_eq!(einstellungen::pfad(), "/system/einstellungen.txt");

        // "Platte" bauen und am ECHTEN globalen VFS mounten:
        let disk = Arc::new(spin::Mutex::new(RamDisk::neu(512, 4096)));
        formatieren(&mut *disk.lock()).unwrap();
        let wrapper = AbsturzDisk {
            disk: disk.clone(),
            budget: Arc::new(AtomicI64::new(i64::MAX)),
        };
        let fs1 = SpeedFs::mounten(Box::new(wrapper)).map_err(|(f, _)| f).unwrap();
        crate::fs::mounten(crate::fs::PLATTE, Box::new(fs1)).unwrap();
        crate::fs::mit_fs(|f| f.mkdir("/platte/system")).unwrap();
        assert_eq!(einstellungen::pfad(), "/platte/system/einstellungen.txt");

        // Wert setzen — setze_zahl speichert SOFORT (auf die Platte):
        einstellungen::setze_zahl("test.neustart", 4711);
        assert!(crate::fs::mit_fs(|f| f.stat("/platte/system/einstellungen.txt")).is_ok());

        // "Neustart": aushängen (FS + Cache weg), RAM-Werte radieren,
        // vom selben Platten-Zustand neu mounten und laden.
        crate::fs::unmounten(crate::fs::PLATTE).unwrap();
        einstellungen::mit_werten(|werte| werte.clear());
        assert_eq!(einstellungen::hole_zahl("test.neustart", 0), 0); // wirklich weg
        let wrapper2 = AbsturzDisk {
            disk,
            budget: Arc::new(AtomicI64::new(i64::MAX)),
        };
        let fs2 = SpeedFs::mounten(Box::new(wrapper2)).map_err(|(f, _)| f).unwrap();
        crate::fs::mounten(crate::fs::PLATTE, Box::new(fs2)).unwrap();
        einstellungen::laden();
        assert_eq!(
            einstellungen::hole_zahl("test.neustart", 0),
            4711,
            "Der Wert muss den simulierten Neustart ueberleben"
        );

        // Aufräumen: aushängen, RAM-Zustand neutralisieren (laden()
        // vom RAM-Pfad = Defaults; die Skala bleibt so auf 1.0 —
        // die bekannte Test-Falle aus CLAUDE.md).
        crate::fs::unmounten(crate::fs::PLATTE).unwrap();
        einstellungen::mit_werten(|werte| werte.clear());
        einstellungen::laden();
    }

    /// Wiedermount: aushängen, neu mounten — alles noch da (die
    /// eigentliche Daseinsberechtigung eines Disk-Dateisystems).
    #[test_case]
    fn test_speedfs_wiedermount_findet_alles() {
        let mut fs = test_fs();
        fs.mkdir("/projekte").unwrap();
        fs.schreiben("/projekte/plan.txt", b"1. Dateisystem bauen").unwrap();
        fs.schreiben("/notiz.txt", b"nicht vergessen").unwrap();
        fs.sync().unwrap();

        // Aushängen und mit DEMSELBEN Gerät neu mounten:
        let geraet = fs.aushaengen();
        let fs2 = SpeedFs::mounten(geraet).map_err(|(f, _)| f).unwrap();
        assert_eq!(
            fs2.lesen("/projekte/plan.txt").unwrap(),
            b"1. Dateisystem bauen"
        );
        assert_eq!(fs2.lesen("/notiz.txt").unwrap(), b"nicht vergessen");
        let namen: Vec<String> = fs2.liste("/").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(namen, vec![String::from("notiz.txt"), String::from("projekte")]);
    }
}
