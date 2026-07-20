// ata.rs — Der erste echte Massenspeicher-Treiber von SpeedOS
//
// ATA im PIO-Modus ("Programmed I/O"): Die CPU schiebt jedes Wort
// selbst über die klassischen IDE-Ports (Primary-Kanal 0x1F0-0x1F7,
// Control 0x3F6). Das ist die EINFACHSTE Art, mit einer Platte zu
// reden — kein DMA, keine Interrupts, keine PCI-Enumeration: Die
// Legacy-Ports sind seit Jahrzehnten fest verdrahtet, und QEMUs
// PIIX3-IDE-Controller bedient sie direkt. Später (AHCI/NVMe) wird
// das ersetzt; die BlockDevice-Naht bleibt gleich.
//
// GEPOLLT mit TIMEOUT: Wir warten aktiv auf die Status-Bits (BSY/DRQ)
// und geben nach einer Frist mit IoFehler::Zeitueberschreitung auf —
// ein Treiber darf NIE endlos auf Hardware warten (fehlende Platte,
// kaputtes Gerät). Interrupts des Kanals schalten wir ab (nIEN).
//
// GRENZEN (bewusst, dokumentiert):
//   * LBA28: 28-Bit-Sektornummern -> max. 2^28 Sektoren zu 512 Bytes
//     = 128 GiB. Für die 64-MiB-Daten-Platte reichlich; LBA48 ist
//     eine spätere, rein additive Erweiterung (andere Kommandos).
//   * Max. 256 Sektoren pro Kommando (Sektorzahl-Register: 0 = 256);
//     größere Aufträge zerlegt der Treiber selbst in Häppchen.
//
// SICHERHEITSREGEL (siehe CLAUDE.md): Die BOOT-Platte (Primary
// Master) ist PER KONSTRUKTION schreibgeschützt. Das Feld
// `beschreibbar` ist privat, und einzig init() erzeugt Laufwerke —
// nur das konfigurierte DATEN-Laufwerk (Primary Slave) bekommt
// Schreibrechte. Es gibt keinen API-Weg, das zu ändern: Ein Bug im
// Dateisystem-Code kann das Boot-Medium damit nicht zerstören.
//
// LOCK-REGEL: LAUFWERKE ist ein BLATT-Lock (wie Ablage/Einstellungen):
// Er wird nur aus Task-Kontext genommen (Shell-Befehle, Tests), nie
// im Interrupt-Handler, und unter ihm wird kein weiterer Lock geholt.

use crate::fs::block::{BlockDevice, IoFehler};
use crate::{serial_println, zeit};
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::port::Port;

// --- Register-Offsets relativ zur I/O-Basis des Kanals ------------------
// (Primary-Kanal: Basis 0x1F0, Control 0x3F6; Secondary: 0x170/0x376)
const REG_DATEN: u16 = 0; // 16-Bit-Datenfenster (Sektor-Inhalt)
const REG_SEKTORZAHL: u16 = 2; // Wie viele Sektoren (0 = 256)
const REG_LBA_NIEDRIG: u16 = 3; // LBA Bits 0-7
const REG_LBA_MITTE: u16 = 4; // LBA Bits 8-15
const REG_LBA_HOCH: u16 = 5; // LBA Bits 16-23
const REG_LAUFWERK: u16 = 6; // Laufwerkswahl + LBA Bits 24-27
const REG_STATUS_KOMMANDO: u16 = 7; // Lesen: Status, Schreiben: Kommando

// --- Status-Bits ---------------------------------------------------------
const STATUS_BSY: u8 = 0x80; // Gerät arbeitet — andere Bits ungültig
const STATUS_DF: u8 = 0x20; // Device Fault — schwerer Gerätefehler
const STATUS_DRQ: u8 = 0x08; // Datenfenster bereit (lesen/schreiben)
const STATUS_ERR: u8 = 0x01; // Kommando fehlgeschlagen

// --- Kommandos -----------------------------------------------------------
const KOMMANDO_LESEN: u8 = 0x20; // READ SECTORS (PIO, LBA28)
const KOMMANDO_SCHREIBEN: u8 = 0x30; // WRITE SECTORS (PIO, LBA28)
const KOMMANDO_FLUSH: u8 = 0xE7; // FLUSH CACHE — Schreib-Cache aufs Medium
const KOMMANDO_IDENTIFY: u8 = 0xEC; // IDENTIFY DEVICE — 256 Info-Worte

/// Sektorgröße klassischer ATA-Platten. LBA28 adressiert damit
/// maximal 2^28 * 512 Bytes = 128 GiB.
pub const SEKTOR_GROESSE: usize = 512;
/// Höchste mit LBA28 adressierbare Sektor-ANZAHL (2^28).
const LBA28_MAX_SEKTOREN: u64 = 1 << 28;

/// Polling-Frist für normale Statuswechsel (BSY weg, DRQ da). In QEMU
/// antwortet die Platte in Mikrosekunden — 1 s ist also schon extrem
/// großzügig und schlägt nur bei wirklich toter Hardware an.
const TIMEOUT_US: u64 = 1_000_000;
/// FLUSH CACHE darf länger dauern (echte Platten müssen ihren
/// Schreib-Cache mechanisch wegschreiben) — 5 s Frist.
const TIMEOUT_FLUSH_US: u64 = 5_000_000;

/// Ein erkanntes ATA-Laufwerk. Entsteht NUR in init() — deshalb kann
/// niemand nachträglich ein beschreibbares Boot-Laufwerk bauen.
pub struct AtaLaufwerk {
    /// I/O-Basisport des Kanals (Primary 0x1F0).
    io_basis: u16,
    /// Control-Port des Kanals (Primary 0x3F6) — Alt-Status + nIEN.
    control: u16,
    /// false = Master, true = Slave (Bit 4 der Laufwerkswahl).
    slave: bool,
    /// Schreibrechte — nur das Daten-Laufwerk bekommt sie (privat!).
    beschreibbar: bool,
    /// Modellname aus IDENTIFY (z. B. "QEMU HARDDISK").
    modell: String,
    /// Kapazität in Sektoren aus IDENTIFY (Worte 60/61).
    sektoren: u64,
    /// Anzeigename für Mensch und Shell ("Boot" / "Daten").
    rolle: &'static str,
}

impl AtaLaufwerk {
    pub fn modell(&self) -> &str {
        &self.modell
    }
    pub fn rolle(&self) -> &'static str {
        self.rolle
    }
    pub fn ist_beschreibbar(&self) -> bool {
        self.beschreibbar
    }

    /// Liest den Status vom ALT-Status-Port (Control-Basis). Der
    /// normale Status-Port würde als Nebenwirkung anstehende
    /// Interrupts quittieren — der Alt-Port ist nebenwirkungsfrei.
    fn status(&self) -> u8 {
        // unsafe (Port-I/O): Alt-Status ist ein reines Lese-Register
        // des Standard-IDE-Kanals, Lesen hat keine Nebenwirkungen.
        unsafe { Port::<u8>::new(self.control).read() }
    }

    /// Die 400-ns-Pause nach einer Laufwerkswahl (ATA-Spec): Das
    /// Gerät braucht einen Moment, bis seine Status-Bits gelten.
    /// Vier Alt-Status-Reads dauern garantiert lange genug.
    fn kurz_warten(&self) {
        for _ in 0..4 {
            self.status();
        }
    }

    /// Pollt, bis BSY gelöscht ist UND die gewünschten Bits gesetzt
    /// sind (`gewuenscht` = 0: nur auf BSY-Ende warten). Prüft dabei
    /// ERR/DF und bricht nach `frist_us` Mikrosekunden ab — die
    /// TSC-Zeit läuft unabhängig von Interrupts, darf also hier
    /// in der engen Schleife benutzt werden.
    fn warten_auf(&self, gewuenscht: u8, frist_us: u64) -> Result<u8, IoFehler> {
        let start = zeit::us_seit_boot();
        loop {
            let status = self.status();
            if status & STATUS_BSY == 0 {
                if status & (STATUS_ERR | STATUS_DF) != 0 {
                    return Err(IoFehler::Geraetefehler);
                }
                if gewuenscht == 0 || status & gewuenscht != 0 {
                    return Ok(status);
                }
            }
            if zeit::us_seit_boot() - start > frist_us {
                return Err(IoFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }
    }

    /// Wählt dieses Laufwerk aus und setzt die LBA28-Adresse plus
    /// Sektorzahl in die Kommando-Register (anzahl 256 -> Register 0).
    fn adressieren(&mut self, lba: u64, anzahl: u16) {
        debug_assert!((1..=256).contains(&anzahl));
        debug_assert!(lba < LBA28_MAX_SEKTOREN);
        // unsafe (Port-I/O): Standard-Kommandoregister des Kanals;
        // die Werte sind durch die Asserts im gültigen Bereich.
        unsafe {
            Port::<u8>::new(self.io_basis + REG_LAUFWERK)
                .write(laufwerk_byte(self.slave, lba));
            Port::<u8>::new(self.io_basis + REG_SEKTORZAHL).write(anzahl as u8); // 256 wird 0
            Port::<u8>::new(self.io_basis + REG_LBA_NIEDRIG).write(lba as u8);
            Port::<u8>::new(self.io_basis + REG_LBA_MITTE).write((lba >> 8) as u8);
            Port::<u8>::new(self.io_basis + REG_LBA_HOCH).write((lba >> 16) as u8);
        }
        self.kurz_warten();
    }

    /// Schickt ein Kommando ab.
    fn kommando(&mut self, kommando: u8) {
        // unsafe (Port-I/O): Standard-Kommandoport des Kanals.
        unsafe { Port::<u8>::new(self.io_basis + REG_STATUS_KOMMANDO).write(kommando) };
    }

    /// Gemeinsame Bereichs-Validierung für Lesen und Schreiben:
    /// Puffer = Vielfaches der Sektorgröße, Bereich auf dem Gerät.
    fn pruefen(&self, start: u64, puffer_laenge: usize) -> Result<u64, IoFehler> {
        if puffer_laenge == 0 || !puffer_laenge.is_multiple_of(SEKTOR_GROESSE) {
            return Err(IoFehler::UngueltigePufferGroesse);
        }
        let sektoren = (puffer_laenge / SEKTOR_GROESSE) as u64;
        match start.checked_add(sektoren) {
            Some(ende) if ende <= self.sektoren => Ok(sektoren),
            _ => Err(IoFehler::AusserhalbDesGeraets),
        }
    }
}

/// Baut das Laufwerkswahl-Byte: 0xE0 = LBA-Modus + Pflicht-Bits,
/// Bit 4 wählt den Slave, Bits 0-3 tragen LBA-Bits 24-27.
/// Reine Funktion — unten unit-getestet.
fn laufwerk_byte(slave: bool, lba: u64) -> u8 {
    0xE0 | ((slave as u8) << 4) | (((lba >> 24) & 0x0F) as u8)
}

/// Dekodiert den Modellnamen aus den IDENTIFY-Worten 27-46: Die
/// Bytes stecken PAARWEISE VERTAUSCHT in den 16-Bit-Worten (ATA-
/// Altlast: High-Byte zuerst). Reine Funktion — unit-getestet.
fn modell_dekodieren(worte: &[u16]) -> String {
    let mut name = String::new();
    for wort in &worte[27..47] {
        name.push((wort >> 8) as u8 as char);
        name.push((wort & 0xFF) as u8 as char);
    }
    String::from(name.trim())
}

/// Liest die LBA28-Kapazität aus den IDENTIFY-Worten 60/61
/// (Little-Endian-Doppelwort). Reine Funktion — unit-getestet.
fn kapazitaet_dekodieren(worte: &[u16]) -> u64 {
    (worte[60] as u64) | ((worte[61] as u64) << 16)
}

impl BlockDevice for AtaLaufwerk {
    fn sektor_groesse(&self) -> usize {
        SEKTOR_GROESSE
    }

    fn anzahl_sektoren(&self) -> u64 {
        self.sektoren
    }

    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
        let gesamt = self.pruefen(start, puffer.len())?;
        // In Häppchen zu max. 256 Sektoren (Register-Grenze):
        let mut lba = start;
        let mut rest = gesamt;
        let mut ab = 0usize;
        while rest > 0 {
            let anzahl = rest.min(256) as u16;
            self.adressieren(lba, anzahl);
            self.kommando(KOMMANDO_LESEN);
            for sektor in 0..anzahl as usize {
                // Vor JEDEM Sektor meldet das Gerät per DRQ, dass
                // sein Puffer gefüllt ist:
                self.warten_auf(STATUS_DRQ, TIMEOUT_US)?;
                let mut daten = Port::<u16>::new(self.io_basis + REG_DATEN);
                let von = ab + sektor * SEKTOR_GROESSE;
                for paar in puffer[von..von + SEKTOR_GROESSE].as_chunks_mut::<2>().0 {
                    // unsafe (Port-I/O): Das Datenfenster liefert nach
                    // DRQ genau 256 Worte Sektorinhalt (Little-Endian).
                    let wort = unsafe { daten.read() };
                    paar[0] = wort as u8;
                    paar[1] = (wort >> 8) as u8;
                }
            }
            ab += anzahl as usize * SEKTOR_GROESSE;
            lba += anzahl as u64;
            rest -= anzahl as u64;
        }
        Ok(())
    }

    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
        // DIE Sicherheitsregel: Boot-Laufwerk schreibt sich nicht.
        if !self.beschreibbar {
            return Err(IoFehler::Schreibgeschuetzt);
        }
        let gesamt = self.pruefen(start, puffer.len())?;
        let mut lba = start;
        let mut rest = gesamt;
        let mut ab = 0usize;
        while rest > 0 {
            let anzahl = rest.min(256) as u16;
            self.adressieren(lba, anzahl);
            self.kommando(KOMMANDO_SCHREIBEN);
            for sektor in 0..anzahl as usize {
                self.warten_auf(STATUS_DRQ, TIMEOUT_US)?;
                let mut daten = Port::<u16>::new(self.io_basis + REG_DATEN);
                let von = ab + sektor * SEKTOR_GROESSE;
                for paar in puffer[von..von + SEKTOR_GROESSE].as_chunks::<2>().0 {
                    let wort = (paar[0] as u16) | ((paar[1] as u16) << 8);
                    // unsafe (Port-I/O): Nach DRQ erwartet das Gerät
                    // genau 256 Worte für den angekündigten Sektor.
                    unsafe { daten.write(wort) };
                }
            }
            // Warten, bis das Gerät den letzten Sektor verdaut hat
            // (BSY weg, kein ERR/DF) — erst dann das nächste Kommando:
            self.warten_auf(0, TIMEOUT_US)?;
            ab += anzahl as usize * SEKTOR_GROESSE;
            lba += anzahl as u64;
            rest -= anzahl as u64;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), IoFehler> {
        // FLUSH CACHE: Das Gerät schreibt seinen internen Schreib-
        // Cache aufs Medium. Auch fürs (read-only) Boot-Laufwerk
        // harmlos — es gibt dort nichts zu flushen.
        // Laufwerk wählen (ohne Adresse — Flush betrifft alles):
        // unsafe (Port-I/O): Standard-Laufwerkswahlregister.
        unsafe {
            Port::<u8>::new(self.io_basis + REG_LAUFWERK)
                .write(0xE0 | ((self.slave as u8) << 4))
        };
        self.kurz_warten();
        self.kommando(KOMMANDO_FLUSH);
        self.warten_auf(0, TIMEOUT_FLUSH_US)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Erkennung (IDENTIFY) und globale Laufwerks-Registry
// ---------------------------------------------------------------------------

/// Alle beim Boot erkannten Laufwerke. BLATT-Lock (siehe Kopf).
static LAUFWERKE: Mutex<Vec<AtaLaufwerk>> = Mutex::new(Vec::new());

/// Prüft per IDENTIFY, ob am Kanal (io_basis/control) das gewählte
/// Laufwerk existiert, und liest Modell + Kapazität aus. Ein fehlendes
/// Laufwerk ist KEIN Fehler des Systems — es kommt als Err zurück und
/// der Aufrufer entscheidet (init loggt es nur).
fn identifizieren(
    io_basis: u16,
    control: u16,
    slave: bool,
) -> Result<(String, u64), IoFehler> {
    // Interrupts dieses Kanals stumm schalten (nIEN=1): Wir pollen —
    // ein unerwarteter IRQ14/15 hätte keinen Handler.
    // unsafe (Port-I/O): Device-Control-Register des Standard-Kanals.
    unsafe { Port::<u8>::new(control).write(0x02) };

    let probe = AtaLaufwerk {
        io_basis,
        control,
        slave,
        beschreibbar: false,
        modell: String::new(),
        sektoren: 0,
        rolle: "",
    };

    // Laufwerk wählen, dann die Spec-Pause:
    // unsafe (Port-I/O): 0xA0/0xB0 = klassische Laufwerkswahl.
    unsafe { Port::<u8>::new(io_basis + REG_LAUFWERK).write(0xA0 | ((slave as u8) << 4)) };
    probe.kurz_warten();

    // Ein leerer Bus liest 0xFF ("floating bus"), ein leerer Steckplatz
    // oft 0x00 — beides heißt sofort: hier ist niemand. Das ist der
    // SCHNELLE Pfad; das Timeout unten ist nur das Sicherheitsnetz.
    let status_roh = probe.status();
    if status_roh == 0xFF {
        return Err(IoFehler::NichtBereit);
    }

    // IDENTIFY erwartet genullte Adressregister:
    // unsafe (Port-I/O): Standard-Kommandoregister, Werte fest 0.
    unsafe {
        Port::<u8>::new(io_basis + REG_SEKTORZAHL).write(0);
        Port::<u8>::new(io_basis + REG_LBA_NIEDRIG).write(0);
        Port::<u8>::new(io_basis + REG_LBA_MITTE).write(0);
        Port::<u8>::new(io_basis + REG_LBA_HOCH).write(0);
        Port::<u8>::new(io_basis + REG_STATUS_KOMMANDO).write(KOMMANDO_IDENTIFY);
    }
    if probe.status() == 0 {
        // Status 0 nach IDENTIFY = kein Gerät am Steckplatz.
        return Err(IoFehler::NichtBereit);
    }

    // Auf das Ende von BSY warten, dann muss DRQ kommen. ATAPI-Geräte
    // (CD-Laufwerke) brechen IDENTIFY mit ERR ab und tragen sich in
    // LBA-Mitte/Hoch ein — die behandeln wir schlicht als "nicht da".
    probe.warten_auf(STATUS_DRQ, TIMEOUT_US).map_err(|fehler| {
        if fehler == IoFehler::Geraetefehler {
            IoFehler::NichtBereit // ATAPI/kein ATA — kein Hardware-Defekt
        } else {
            fehler
        }
    })?;

    // Die 256 IDENTIFY-Worte einlesen:
    let mut worte = [0u16; 256];
    let mut daten = Port::<u16>::new(io_basis + REG_DATEN);
    for wort in worte.iter_mut() {
        // unsafe (Port-I/O): Nach DRQ liefert das Datenfenster genau
        // 256 Worte IDENTIFY-Struktur.
        *wort = unsafe { daten.read() };
    }

    let modell = modell_dekodieren(&worte);
    let sektoren = kapazitaet_dekodieren(&worte).min(LBA28_MAX_SEKTOREN);
    if sektoren == 0 {
        // Gerät ohne LBA28-Kapazität (uralt/exotisch): nicht nutzbar.
        return Err(IoFehler::NichtBereit);
    }
    Ok((modell, sektoren))
}

/// Erkennt die angeschlossenen Laufwerke und füllt die Registry:
///   * Primary MASTER   = Boot-Platte  -> NUR LESEN (Sicherheitsregel),
///   * Primary SLAVE    = Daten-Platte -> beschreibbar,
///   * Secondary MASTER = FAT-Platte   -> NUR LESEN ("der USB-Stick").
///
/// Läuft beim Boot NACH zeit::init() (die Polling-Fristen brauchen
/// die TSC-Zeit). Fehlende Laufwerke sind kein Boot-Hindernis.
pub fn init() {
    let mut laufwerke = LAUFWERKE.lock();
    laufwerke.clear(); // idempotent — Tests dürfen erneut initialisieren

    // (io_basis, control, slave, rolle, beschreibbar, kanal-name)
    for (io_basis, control, slave, rolle, beschreibbar, kanal) in [
        (0x1F0u16, 0x3F6u16, false, "Boot", false, "Primary Master"),
        (0x1F0, 0x3F6, true, "Daten", true, "Primary Slave"),
        (0x170, 0x376, false, "FAT", false, "Secondary Master"),
    ] {
        match identifizieren(io_basis, control, slave) {
            Ok((modell, sektoren)) => {
                serial_println!(
                    "[ATA] {}-Laufwerk ({}): '{}', {} Sektoren = {} MiB{}",
                    rolle,
                    kanal,
                    modell,
                    sektoren,
                    sektoren * SEKTOR_GROESSE as u64 / 1024 / 1024,
                    if beschreibbar { "" } else { " [schreibgeschuetzt]" }
                );
                laufwerke.push(AtaLaufwerk {
                    io_basis,
                    control,
                    slave,
                    beschreibbar,
                    modell,
                    sektoren,
                    rolle,
                });
            }
            Err(fehler) => serial_println!(
                "[ATA] {}: kein Laufwerk ({})",
                kanal,
                fehler.meldung()
            ),
        }
    }
}

/// Führt `f` mit der Laufwerks-Liste aus (für den platten-Befehl).
pub fn mit_laufwerken<R>(f: impl FnOnce(&mut [AtaLaufwerk]) -> R) -> R {
    f(&mut LAUFWERKE.lock())
}

/// Führt `f` mit dem DATEN-Laufwerk aus — dem einzigen beschreibbaren.
/// Kein Daten-Laufwerk erkannt -> IoFehler::NichtBereit.
pub fn mit_datenlaufwerk<R>(
    f: impl FnOnce(&mut AtaLaufwerk) -> Result<R, IoFehler>,
) -> Result<R, IoFehler> {
    mit_rollenlaufwerk("Daten", f)
}

/// Führt `f` mit dem Laufwerk der gegebenen Rolle aus. Die Rolle ist
/// eindeutig (jede kommt in init() genau einmal vor).
fn mit_rollenlaufwerk<R>(
    rolle: &str,
    f: impl FnOnce(&mut AtaLaufwerk) -> Result<R, IoFehler>,
) -> Result<R, IoFehler> {
    let mut laufwerke = LAUFWERKE.lock();
    match laufwerke.iter_mut().find(|l| l.rolle == rolle) {
        Some(laufwerk) => f(laufwerk),
        None => Err(IoFehler::NichtBereit),
    }
}

/// Probe eines Steckplatzes für Tests: liefert nur Ok/Fehler, ohne
/// die Registry anzufassen (z. B. Secondary Slave = garantiert leer).
pub fn probe(io_basis: u16, control: u16, slave: bool) -> Result<(), IoFehler> {
    identifizieren(io_basis, control, slave).map(|_| ())
}

// ---------------------------------------------------------------------------
// DatenPlatte — ein besitzbares BlockDevice-Handle auf die Daten-Platte
// ---------------------------------------------------------------------------

/// Ein eigenständiges BlockDevice über die DATEN-Platte: delegiert
/// jeden Zugriff an die Registry (mit_datenlaufwerk) und cached nur
/// die Geometrie. Damit kann das VFS (SpeedFS-Mount) das Laufwerk
/// "besitzen", während platten/blocktest weiter über die Registry
/// laufen. LOCK-ORDNUNG: Das VFS hält seinen Lock, wenn es hier
/// landet — LAUFWERKE bleibt ein BLATT darunter (VFS -> LAUFWERKE;
/// niemand nimmt das VFS unter LAUFWERKE).
pub struct DatenPlatte {
    sektoren: u64,
}

/// Liefert das Handle, wenn eine Daten-Platte erkannt wurde.
pub fn daten_platte() -> Option<DatenPlatte> {
    let laufwerke = LAUFWERKE.lock();
    laufwerke
        .iter()
        .find(|l| l.rolle == "Daten")
        .map(|l| DatenPlatte { sektoren: l.sektoren })
}

impl BlockDevice for DatenPlatte {
    fn sektor_groesse(&self) -> usize {
        SEKTOR_GROESSE
    }
    fn anzahl_sektoren(&self) -> u64 {
        self.sektoren
    }
    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
        mit_datenlaufwerk(|laufwerk| laufwerk.lese_sektoren(start, puffer))
    }
    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
        mit_datenlaufwerk(|laufwerk| laufwerk.schreibe_sektoren(start, puffer))
    }
    fn sync(&mut self) -> Result<(), IoFehler> {
        mit_datenlaufwerk(|laufwerk| laufwerk.sync())
    }
}

/// Ein besitzbares BlockDevice-Handle auf die FAT-Platte (Secondary
/// Master). Wie DatenPlatte, aber delegiert an das FAT-Laufwerk —
/// und lehnt Schreibversuche schon in der Registry ab (das FAT-
/// Laufwerk trägt beschreibbar=false, siehe AtaLaufwerk::
/// schreibe_sektoren -> IoFehler::Schreibgeschuetzt).
pub struct FatPlatte {
    sektoren: u64,
}

/// Liefert das Handle, wenn eine FAT-Platte erkannt wurde.
pub fn fat_platte() -> Option<FatPlatte> {
    let laufwerke = LAUFWERKE.lock();
    laufwerke
        .iter()
        .find(|l| l.rolle == "FAT")
        .map(|l| FatPlatte { sektoren: l.sektoren })
}

impl BlockDevice for FatPlatte {
    fn sektor_groesse(&self) -> usize {
        SEKTOR_GROESSE
    }
    fn anzahl_sektoren(&self) -> u64 {
        self.sektoren
    }
    fn lese_sektoren(&mut self, start: u64, puffer: &mut [u8]) -> Result<(), IoFehler> {
        mit_rollenlaufwerk("FAT", |laufwerk| laufwerk.lese_sektoren(start, puffer))
    }
    fn schreibe_sektoren(&mut self, start: u64, puffer: &[u8]) -> Result<(), IoFehler> {
        mit_rollenlaufwerk("FAT", |laufwerk| laufwerk.schreibe_sektoren(start, puffer))
    }
    fn sync(&mut self) -> Result<(), IoFehler> {
        mit_rollenlaufwerk("FAT", |laufwerk| laufwerk.sync())
    }
}

// ---------------------------------------------------------------------------
// Unit-Tests der reinen Funktionen (laufen in QEMU via cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Das Laufwerkswahl-Byte trägt Slave-Bit und LBA-Bits 24-27.
    #[test_case]
    fn test_ata_laufwerk_byte() {
        assert_eq!(laufwerk_byte(false, 0), 0xE0);
        assert_eq!(laufwerk_byte(true, 0), 0xF0);
        // LBA 0x0A00_0000: Bits 24-27 = 0xA landen unten im Byte:
        assert_eq!(laufwerk_byte(false, 0x0A00_0000), 0xEA);
        // Höhere Bits als 27 dürfen NICHT durchsickern:
        assert_eq!(laufwerk_byte(false, 0xF000_0000), 0xE0);
    }

    /// IDENTIFY-Modellname: Byte-Paare je Wort vertauscht, Leerraum
    /// am Rand entfernt.
    #[test_case]
    fn test_ata_modell_dekodieren() {
        let mut worte = [0x2020u16; 256]; // alles Leerzeichen
        // "QEMU" ab Wort 27: 'Q','E' -> 0x5145, 'M','U' -> 0x4D55
        worte[27] = 0x5145;
        worte[28] = 0x4D55;
        assert_eq!(modell_dekodieren(&worte), "QEMU");
    }

    /// Kapazität = Little-Endian-Doppelwort aus Wort 60/61.
    #[test_case]
    fn test_ata_kapazitaet_dekodieren() {
        let mut worte = [0u16; 256];
        worte[60] = 0x0000;
        worte[61] = 0x0002; // 0x0002_0000 Sektoren = 131072 = 64 MiB
        assert_eq!(kapazitaet_dekodieren(&worte), 131072);
    }
}
