// elf.rs — Der ELF64-Lader (Serie 6, Teil 5, Aufgabe 1)
//
// DER Moment, in dem SpeedOS fremde Programme ausführen kann. Bis hierhin lag
// jedes Stück User-Code IM KERNEL-IMAGE: hand-assemblierte Byte-Folgen in
// `prozess.rs` und `ring3.rs`, mit fest eingesetzten Adressen. Ab jetzt liest
// der Kernel eine DATEI von der Platte, versteht ihr Format und macht daraus
// einen laufenden Prozess. Von hier an ist SpeedOS keine geschlossene
// Veranstaltung mehr.
//
// ==========================================================================
// WAS EIN ELF IST — in drei Sätzen
//
// Eine ELF-Datei beginnt mit einem 64-Byte-HEADER (Magie, Architektur,
// Einsprungadresse, wo die Tabellen liegen). Für das LADEN zählt genau eine
// dieser Tabellen: die PROGRAM-HEADER-Tabelle, `e_phnum` Einträge zu je 56
// Byte. Jeder Eintrag beschreibt ein SEGMENT — "nimm `p_filesz` Bytes ab
// Dateioffset `p_offset`, lege sie an die virtuelle Adresse `p_vaddr`, mach
// den Bereich `p_memsz` gross und gib ihm die Rechte `p_flags`".
//
//   Datei                        Adressraum des Prozesses
//   +------------------+         +---------------------------+
//   | ELF-Header (64)  |         |                           |
//   | Program Headers  |         | 0x80_0000_0000 .text  R-X |<-+
//   |   [0] LOAD R-X --|-------->|                .rodata R-- |  |
//   |   [1] LOAD R--   |         |                .data  RW-  |  |
//   |   [2] LOAD RW-   |         |                .bss   RW-  |  | e_entry
//   | .text .rodata .. |         |                (genullt)   |  |
//   +------------------+         +---------------------------+  |
//                                                               |
//   e_entry ----------------------------------------------------+
//
// WAS WIR BEWUSST NICHT UNTERSTÜTZEN: dynamisches Linken. Nur `ET_EXEC`
// (statisch gelinkt, feste Adressen) wird geladen — kein `ET_DYN`/PIE, keine
// Relokationen, kein Interpreter (`PT_INTERP`), keine Shared Objects. Das ist
// keine Faulheit, sondern eine Grenze mit Begründung: Ein dynamischer Linker
// ist ein eigenes Teilprojekt (Symbol-Auflösung, GOT/PLT, Lade-Reihenfolge),
// und für ein Betriebssystem, das seine Programme selbst mitbringt, kauft er
// nichts. Die `.bss` dagegen ist Pflicht — ohne sie hätte kein Programm
// statische Variablen.
//
// ==========================================================================
// DIE HALTUNG DIESER DATEI: JEDE ZAHL IN DER DATEI IST EINE BEHAUPTUNG
//
// Eine ELF-Datei kommt von aussen. Sie kann abgeschnitten sein, sie kann
// absichtlich bösartig gebaut sein. Jedes Feld darin ist deshalb eine
// BEHAUPTUNG eines Fremden, keine Tatsache — und der Kernel glaubt keine
// davon ungeprüft:
//
//   * `p_offset + p_filesz` könnte hinter das Dateiende zeigen  -> geprüft
//     (mit checked_add, denn beide sind u64 und dürfen nicht überlaufen).
//   * `p_vaddr` könnte auf KERNEL-Speicher zeigen               -> geprüft
//     gegen `adressraum::USER_START..USER_ENDE`; ein Segment, das den Kernel
//     überschreiben will, wird abgelehnt, nicht gemappt.
//   * `p_memsz` könnte absurd gross sein (4 GiB Nullen)         -> gedeckelt.
//   * Zwei Segmente könnten sich überlappen                     -> geprüft;
//     sonst wäre die spätere Reihenfolge entscheidend, und ein Segment
//     könnte ein anderes nachträglich überschreiben.
//   * Ein Segment könnte schreibbar UND ausführbar sein         -> abgelehnt
//     (W^X, siehe `adressraum::Rechte`).
//   * `e_entry` könnte irgendwohin zeigen                       -> muss in
//     einem ausführbaren Segment liegen, sonst startet der Prozess in Daten.
//
// Und: DIESE DATEI PANICKT NIE. Jeder dieser Fälle ist ein `ElfFehler`, kein
// Absturz. `pruefen()` ist eine REINE Funktion auf einem `&[u8]` — sie fasst
// keinen Adressraum an, nimmt keinen Lock und lässt sich deshalb mit
// beliebigem Müll füttern. Genau das tut `tests/elf.rs`.
//
// Erst `laden()` fasst Speicher an, und zwar in dieser Reihenfolge:
// VOLLSTÄNDIG PRÜFEN, dann mappen. Schlägt das Mappen mittendrin fehl,
// gehören die schon gemappten Seiten dem Adressraum und gehen bei seinem
// Abriss zurück — es leckt nichts.
// ==========================================================================

use crate::adressraum::{self, AdressRaum, Rechte};
use alloc::vec::Vec;
use x86_64::VirtAddr;

// ---------------------------------------------------------------------------
// Die Konstanten des Formats (ELF64, System V ABI)
// ---------------------------------------------------------------------------

/// Die vier Magie-Bytes am Dateianfang: 0x7F 'E' 'L' 'F'.
pub const MAGIE: [u8; 4] = [0x7F, b'E', b'L', b'F'];
/// Grösse des ELF64-Headers.
pub const HEADER_GROESSE: usize = 64;
/// Grösse EINES Program-Header-Eintrags (ELF64).
pub const PH_GROESSE: usize = 56;

/// `e_ident[4]`: 2 = 64 Bit (1 wäre 32 Bit).
const KLASSE_64: u8 = 2;
/// `e_ident[5]`: 1 = little endian.
const DATEN_LE: u8 = 1;
/// `e_ident[6]` und `e_version`: 1 = die einzige Version, die es gibt.
const VERSION_1: u8 = 1;
/// `e_type`: 2 = ET_EXEC (statisch gelinktes Programm mit festen Adressen).
const TYP_EXEC: u16 = 2;
/// `e_type`: 3 = ET_DYN — PIE/Shared Object. Wird ABGELEHNT (kein dynamisches
/// Linken), aber mit einem EIGENEN Fehler, damit die Meldung hilfreich ist.
const TYP_DYN: u16 = 3;
/// `e_machine`: 0x3E = x86-64.
const MASCHINE_X86_64: u16 = 0x3E;

/// `p_type`: das einzige Segment, das geladen wird.
pub const PT_LOAD: u32 = 1;
/// `p_type`: PT_INTERP — verlangt einen dynamischen Linker. Abgelehnt.
pub const PT_INTERP: u32 = 3;

/// `p_flags`-Bits.
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

// ---------------------------------------------------------------------------
// Unsere Grenzen (nicht die des Formats — unsere eigenen, bewussten)
// ---------------------------------------------------------------------------

/// Höchstzahl von PT_LOAD-Segmenten, die wir laden. Ein statisch gelinktes
/// Programm hat typisch drei (R-X, R--, RW-); 16 ist reichlich Luft und
/// deckelt zugleich, wie viel Arbeit eine bösartige Datei auslösen kann.
pub const MAX_SEGMENTE: usize = 16;

/// Höchstzahl von Program-Header-Einträgen, die wir überhaupt ansehen
/// (inklusive der nicht-ladbaren wie PT_GNU_STACK).
pub const MAX_PROGRAM_HEADER: usize = 64;

/// Grösste Spanne, die das Programm-Image im Adressraum einnehmen darf:
/// von `adressraum::USER_START` bis 16 MiB darüber. Alles dahinter gehört
/// dem Stack (siehe `prozess::ELF_STACK_OBEN`), und der Abstand dazwischen
/// bleibt bewusst ungemappt.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Grösste Datei, die wir als Programm annehmen (der Kernel liest sie ganz
/// in den Heap — 8 MiB sind für unsere Programme absurd viel).
pub const MAX_DATEI_BYTES: usize = 8 * 1024 * 1024;

/// Untere Grenze des Programm-Images = Anfang des privaten User-Slots.
pub const IMAGE_START: u64 = adressraum::USER_START;
/// Obere Grenze (exklusiv) des Programm-Images.
pub const IMAGE_ENDE: u64 = IMAGE_START + MAX_IMAGE_BYTES;

// ---------------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------------

/// Warum eine Datei kein ladbares SpeedOS-Programm ist.
///
/// Bewusst FEINGLIEDRIG: Anders als bei den Syscall-Fehlern (wo Grobheit
/// Kernel-Zustand verbirgt) hilft hier jede Unterscheidung — die Meldung
/// landet vor einem Menschen, der wissen will, WAS an seinem Programm nicht
/// stimmt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfFehler {
    /// Die Datei ist kürzer als der ELF-Header.
    ZuKurz,
    /// Die Datei ist grösser als `MAX_DATEI_BYTES`.
    ZuGross,
    /// Die vier Magie-Bytes fehlen — das ist überhaupt kein ELF.
    KeinElf,
    /// 32-Bit-ELF (`e_ident[4] != 2`).
    Kein64Bit,
    /// Big-Endian-ELF (`e_ident[5] != 1`).
    FalscheBytereihenfolge,
    /// Unbekannte Format-Version.
    FalscheVersion,
    /// Nicht x86-64 (`e_machine`).
    FalscheArchitektur,
    /// PIE/Shared Object (`ET_DYN`) — wir linken nicht dynamisch.
    DynamischGelinkt,
    /// Weder ET_EXEC noch ET_DYN (Objektdatei, Core-Dump, ...).
    KeinProgramm,
    /// Braucht einen Interpreter (`PT_INTERP`) — also dynamisches Linken.
    BrauchtInterpreter,
    /// Header-Felder passen nicht zusammen (`e_ehsize`, `e_phentsize`,
    /// `e_phoff`, `e_phnum`).
    KaputterHeader,
    /// Kein einziges PT_LOAD-Segment — es gäbe nichts zu laden.
    KeineSegmente,
    /// Mehr Segmente/Program-Header, als wir verarbeiten.
    ZuVieleSegmente,
    /// Ein Segment liest über das Dateiende hinaus.
    SegmentAusserhalbDerDatei,
    /// `p_memsz < p_filesz` — der Speicherbereich wäre kleiner als die Daten.
    SegmentKleinerAlsInhalt,
    /// Ein Segment ist grösser als erlaubt.
    SegmentZuGross,
    /// Ein Segment läge (ganz oder teilweise) ausserhalb des Programm-
    /// Bereichs — insbesondere in KERNEL-Speicher.
    SegmentAusserhalbUserBereich,
    /// Zwei Segmente beanspruchen dieselbe Seite.
    SegmenteUeberlappen,
    /// Ein Segment wäre schreibbar UND ausführbar (W^X verletzt).
    SegmentSchreibbarUndAusfuehrbar,
    /// Ein Segment wäre nicht einmal lesbar.
    SegmentNichtLesbar,
    /// `p_align` ist keine Zweierpotenz oder `p_vaddr`/`p_offset` passen
    /// nicht dazu.
    FalscheAusrichtung,
    /// Der Einsprungpunkt liegt in keinem ausführbaren Segment.
    EinsprungNichtAusfuehrbar,
    /// Beim Laden ist der Speicher ausgegangen.
    KeinSpeicher,
}

impl ElfFehler {
    /// Deutsche Meldung für Shell und Explorer.
    pub fn meldung(self) -> &'static str {
        match self {
            ElfFehler::ZuKurz => "Datei ist kuerzer als ein ELF-Header",
            ElfFehler::ZuGross => "Datei ist zu gross fuer ein Programm",
            ElfFehler::KeinElf => "keine ELF-Datei (Magie 7F 45 4C 46 fehlt)",
            ElfFehler::Kein64Bit => "32-Bit-ELF — SpeedOS ist 64-Bit",
            ElfFehler::FalscheBytereihenfolge => "Big-Endian-ELF — x86 ist Little-Endian",
            ElfFehler::FalscheVersion => "unbekannte ELF-Version",
            ElfFehler::FalscheArchitektur => "nicht fuer x86-64 uebersetzt",
            ElfFehler::DynamischGelinkt => {
                "dynamisch gelinkt (ET_DYN/PIE) — SpeedOS laedt nur statische Programme"
            }
            ElfFehler::KeinProgramm => "kein ausfuehrbares Programm (ET_EXEC fehlt)",
            ElfFehler::BrauchtInterpreter => "braucht einen dynamischen Linker (PT_INTERP)",
            ElfFehler::KaputterHeader => "ELF-Header ist in sich widerspruechlich",
            ElfFehler::KeineSegmente => "kein einziges ladbares Segment (PT_LOAD)",
            ElfFehler::ZuVieleSegmente => "zu viele Segmente",
            ElfFehler::SegmentAusserhalbDerDatei => "ein Segment liegt hinter dem Dateiende",
            ElfFehler::SegmentKleinerAlsInhalt => "Segment-Speicher kleiner als sein Inhalt",
            ElfFehler::SegmentZuGross => "ein Segment ist zu gross",
            ElfFehler::SegmentAusserhalbUserBereich => {
                "ein Segment zeigt ausserhalb des Programm-Bereichs (Kernel-Adresse?)"
            }
            ElfFehler::SegmenteUeberlappen => "zwei Segmente ueberlappen sich",
            ElfFehler::SegmentSchreibbarUndAusfuehrbar => {
                "ein Segment waere schreibbar UND ausfuehrbar (W^X verletzt)"
            }
            ElfFehler::SegmentNichtLesbar => "ein Segment ist nicht einmal lesbar",
            ElfFehler::FalscheAusrichtung => "Segment-Ausrichtung passt nicht zur Seitengroesse",
            ElfFehler::EinsprungNichtAusfuehrbar => {
                "der Einsprungpunkt liegt in keinem ausfuehrbaren Segment"
            }
            ElfFehler::KeinSpeicher => "kein Speicher mehr zum Laden",
        }
    }
}

pub type ElfErgebnis<T> = Result<T, ElfFehler>;

// ---------------------------------------------------------------------------
// Bytes lesen — grenzgeprüft, nie mit unsafe
// ---------------------------------------------------------------------------
//
// Der naheliegende Weg wäre, den Puffer per `transmute` als Header-Struktur
// zu lesen. Wir tun das NICHT: Das setzt Ausrichtung und Mindestlänge
// voraus, die eine fremde Datei nicht schuldet. Stattdessen drei winzige
// Funktionen, die jeden Zugriff selbst prüfen. Sie sind der Grund, warum
// dieses Modul ohne ein einziges `unsafe` auskommt.

fn u16_bei(bytes: &[u8], offset: usize) -> ElfErgebnis<u16> {
    let teil = bytes
        .get(offset..offset + 2)
        .ok_or(ElfFehler::ZuKurz)?;
    Ok(u16::from_le_bytes([teil[0], teil[1]]))
}

fn u32_bei(bytes: &[u8], offset: usize) -> ElfErgebnis<u32> {
    let teil = bytes
        .get(offset..offset + 4)
        .ok_or(ElfFehler::ZuKurz)?;
    Ok(u32::from_le_bytes([teil[0], teil[1], teil[2], teil[3]]))
}

fn u64_bei(bytes: &[u8], offset: usize) -> ElfErgebnis<u64> {
    let teil = bytes
        .get(offset..offset + 8)
        .ok_or(ElfFehler::ZuKurz)?;
    let mut wert = [0u8; 8];
    wert.copy_from_slice(teil);
    Ok(u64::from_le_bytes(wert))
}

// ---------------------------------------------------------------------------
// Das geprüfte Ergebnis
// ---------------------------------------------------------------------------

/// Ein ladbares Segment — schon vollständig geprüft und in unsere Begriffe
/// übersetzt (`Rechte` statt `p_flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Offset der Daten IN DER DATEI.
    pub datei_offset: usize,
    /// Wie viele Bytes aus der Datei kommen.
    pub datei_bytes: usize,
    /// Wohin im Adressraum des Prozesses.
    pub virt_adresse: u64,
    /// Wie gross der Bereich im Speicher wird (`>= datei_bytes`; die
    /// Differenz ist `.bss` und muss genullt sein).
    pub speicher_bytes: u64,
    /// Die Rechte der Seiten dieses Segments.
    pub rechte: Rechte,
}

impl Segment {
    /// Erste Seite, die dieses Segment berührt (abgerundet).
    pub fn erste_seite(&self) -> u64 {
        self.virt_adresse & !0xfff
    }

    /// Erste Seite HINTER dem Segment (aufgerundet). Nur gültig, wenn die
    /// Prüfung schon lief — dann kann hier nichts überlaufen.
    pub fn seite_dahinter(&self) -> u64 {
        (self.virt_adresse + self.speicher_bytes).div_ceil(4096) * 4096
    }

    /// Wie viele Bytes sind `.bss` (im Speicher, aber nicht in der Datei)?
    pub fn bss_bytes(&self) -> u64 {
        self.speicher_bytes - self.datei_bytes as u64
    }
}

/// Eine geprüfte ELF-Datei: alles, was zum Laden nötig ist — und nichts,
/// was noch geglaubt werden müsste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfProgramm {
    /// Adresse des ersten Befehls.
    pub einsprung: u64,
    /// Die ladbaren Segmente, nach Adresse sortiert.
    pub segmente: Vec<Segment>,
}

impl ElfProgramm {
    /// Niedrigste virtuelle Adresse des Images.
    pub fn image_start(&self) -> u64 {
        self.segmente
            .iter()
            .map(|s| s.erste_seite())
            .min()
            .unwrap_or(IMAGE_START)
    }

    /// Erste Seite hinter dem Image.
    pub fn image_ende(&self) -> u64 {
        self.segmente
            .iter()
            .map(|s| s.seite_dahinter())
            .max()
            .unwrap_or(IMAGE_START)
    }

    /// Wie viele Seiten belegt das Image insgesamt?
    pub fn seiten(&self) -> u64 {
        self.segmente
            .iter()
            .map(|s| (s.seite_dahinter() - s.erste_seite()) / 4096)
            .sum()
    }
}

// ---------------------------------------------------------------------------
// (1) PRÜFEN — die reine Funktion, das Herz dieser Datei
// ---------------------------------------------------------------------------

/// Prüft eine ELF-Datei vollständig und liefert das, was zum Laden nötig ist.
///
/// REIN: kein Adressraum, kein Lock, kein `unsafe`, keine Panik. Man darf
/// diese Funktion mit beliebigem Müll füttern — sie antwortet immer mit
/// `Ok` oder einem `ElfFehler`, nie mit einem Absturz. Genau deshalb kann
/// `tests/elf.rs` sie mit abgeschnittenen, verdrehten und bösartig
/// konstruierten Dateien bewerfen.
pub fn pruefen(bytes: &[u8]) -> ElfErgebnis<ElfProgramm> {
    // --- Der ELF-Header ---
    if bytes.len() > MAX_DATEI_BYTES {
        return Err(ElfFehler::ZuGross);
    }
    if bytes.len() < HEADER_GROESSE {
        return Err(ElfFehler::ZuKurz);
    }
    if bytes[0..4] != MAGIE {
        return Err(ElfFehler::KeinElf);
    }
    if bytes[4] != KLASSE_64 {
        return Err(ElfFehler::Kein64Bit);
    }
    if bytes[5] != DATEN_LE {
        return Err(ElfFehler::FalscheBytereihenfolge);
    }
    if bytes[6] != VERSION_1 {
        return Err(ElfFehler::FalscheVersion);
    }

    let e_type = u16_bei(bytes, 16)?;
    match e_type {
        TYP_EXEC => {}
        // ET_DYN bekommt einen eigenen Fehler: Das ist der Fall, in den man
        // versehentlich läuft, wenn der Linker PIE erzeugt — und dann will
        // man wissen, dass die Datei nicht kaputt ist, sondern falsch
        // gelinkt.
        TYP_DYN => return Err(ElfFehler::DynamischGelinkt),
        _ => return Err(ElfFehler::KeinProgramm),
    }
    if u16_bei(bytes, 18)? != MASCHINE_X86_64 {
        return Err(ElfFehler::FalscheArchitektur);
    }
    if u32_bei(bytes, 20)? != VERSION_1 as u32 {
        return Err(ElfFehler::FalscheVersion);
    }

    let e_entry = u64_bei(bytes, 24)?;
    let e_phoff = u64_bei(bytes, 32)?;
    let e_ehsize = u16_bei(bytes, 52)?;
    let e_phentsize = u16_bei(bytes, 54)?;
    let e_phnum = u16_bei(bytes, 56)?;

    // Die Selbstbeschreibung des Headers muss zum Format passen. Ein ELF64
    // mit e_phentsize != 56 ist entweder kaputt oder will uns dazu bringen,
    // die Tabelle mit falscher Schrittweite zu lesen.
    if e_ehsize as usize != HEADER_GROESSE || e_phentsize as usize != PH_GROESSE {
        return Err(ElfFehler::KaputterHeader);
    }
    if e_phnum == 0 {
        return Err(ElfFehler::KeineSegmente);
    }
    if e_phnum as usize > MAX_PROGRAM_HEADER {
        return Err(ElfFehler::ZuVieleSegmente);
    }
    // Liegt die GANZE Program-Header-Tabelle in der Datei? checked_*, weil
    // e_phoff ein u64 aus einer fremden Datei ist und mühelos überläuft.
    let ph_bytes = (e_phnum as u64)
        .checked_mul(PH_GROESSE as u64)
        .ok_or(ElfFehler::KaputterHeader)?;
    let ph_ende = e_phoff
        .checked_add(ph_bytes)
        .ok_or(ElfFehler::KaputterHeader)?;
    if ph_ende > bytes.len() as u64 {
        return Err(ElfFehler::KaputterHeader);
    }

    // --- Die Program-Header ---
    let mut segmente: Vec<Segment> = Vec::new();
    for index in 0..e_phnum as usize {
        let basis = (e_phoff as usize) + index * PH_GROESSE;
        let p_type = u32_bei(bytes, basis)?;

        // Ein Interpreter-Eintrag heisst: Diese Datei erwartet einen
        // dynamischen Linker, der Bibliotheken nachlädt. Den haben wir nicht
        // und wollen wir nicht — ehrlich ablehnen statt halb laden.
        if p_type == PT_INTERP {
            return Err(ElfFehler::BrauchtInterpreter);
        }
        // Alles andere (PT_GNU_STACK, PT_NOTE, PT_PHDR ...) ist für uns
        // Information, kein Auftrag: übergehen.
        if p_type != PT_LOAD {
            continue;
        }

        let p_flags = u32_bei(bytes, basis + 4)?;
        let p_offset = u64_bei(bytes, basis + 8)?;
        let p_vaddr = u64_bei(bytes, basis + 16)?;
        // p_paddr (basis+24) ignorieren wir bewusst: Physische Adressen sind
        // Sache des Kernels, nicht der Datei.
        let p_filesz = u64_bei(bytes, basis + 32)?;
        let p_memsz = u64_bei(bytes, basis + 40)?;
        let p_align = u64_bei(bytes, basis + 48)?;

        // Leere Segmente gibt es; sie kosten nichts und tragen nichts bei.
        if p_memsz == 0 {
            continue;
        }
        if segmente.len() >= MAX_SEGMENTE {
            return Err(ElfFehler::ZuVieleSegmente);
        }

        // (a) GRÖSSEN. Der Speicherbereich kann grösser sein als die Daten
        //     (das ist .bss) — aber niemals kleiner.
        if p_memsz < p_filesz {
            return Err(ElfFehler::SegmentKleinerAlsInhalt);
        }
        if p_memsz > MAX_IMAGE_BYTES {
            return Err(ElfFehler::SegmentZuGross);
        }

        // (b) DIE DATEI-SEITE. p_offset + p_filesz darf nicht überlaufen und
        //     nicht hinter das Dateiende zeigen — sonst würden wir Bytes
        //     lesen, die es nicht gibt (bzw. fremden Heap).
        let datei_ende = p_offset
            .checked_add(p_filesz)
            .ok_or(ElfFehler::SegmentAusserhalbDerDatei)?;
        if datei_ende > bytes.len() as u64 {
            return Err(ElfFehler::SegmentAusserhalbDerDatei);
        }

        // (c) DIE SPEICHER-SEITE — die sicherheitskritische Prüfung. Das
        //     Segment muss VOLLSTÄNDIG im Programm-Bereich liegen. Damit
        //     sind Kernel-Adressen, die Nullseite und alles jenseits des
        //     User-Slots erschlagen, bevor irgendetwas gemappt wird.
        let speicher_ende = p_vaddr
            .checked_add(p_memsz)
            .ok_or(ElfFehler::SegmentAusserhalbUserBereich)?;
        if p_vaddr < IMAGE_START || speicher_ende > IMAGE_ENDE {
            return Err(ElfFehler::SegmentAusserhalbUserBereich);
        }

        // (d) AUSRICHTUNG. p_align muss eine Zweierpotenz sein, und der
        //     Versatz in der Datei muss zum Versatz in der Adresse passen
        //     (das verlangt das ABI, und es ist die Bedingung dafür, dass
        //     ein echtes Datei-Mapping überhaupt möglich WÄRE).
        if p_align > 1 {
            if !p_align.is_power_of_two() {
                return Err(ElfFehler::FalscheAusrichtung);
            }
            if p_vaddr % p_align != p_offset % p_align {
                return Err(ElfFehler::FalscheAusrichtung);
            }
        }

        // (e) RECHTE — hier wird W^X durchgesetzt.
        if p_flags & PF_R == 0 {
            return Err(ElfFehler::SegmentNichtLesbar);
        }
        let schreiben = p_flags & PF_W != 0;
        let ausfuehren = p_flags & PF_X != 0;
        if schreiben && ausfuehren {
            return Err(ElfFehler::SegmentSchreibbarUndAusfuehrbar);
        }

        segmente.push(Segment {
            datei_offset: p_offset as usize,
            datei_bytes: p_filesz as usize,
            virt_adresse: p_vaddr,
            speicher_bytes: p_memsz,
            rechte: Rechte {
                schreiben,
                ausfuehren,
            },
        });
    }

    if segmente.is_empty() {
        return Err(ElfFehler::KeineSegmente);
    }

    // (f) ÜBERLAPPUNG — auf SEITEN-Ebene, nicht auf Byte-Ebene.
    //
    // Warum seitenweise? Weil eine Seite nur EINEN Satz Rechte haben kann.
    // Zwei Segmente, die sich eine Seite teilen, müssten sich also auch die
    // Rechte teilen — und dann wäre W^X aushebelbar: ein RW-Segment und ein
    // R-X-Segment in derselben Seite ergäben faktisch RWX. Wir lehnen das
    // ab, statt still das schwächere Recht zu gewinnen.
    segmente.sort_unstable_by_key(|s| s.virt_adresse);
    for paar in segmente.windows(2) {
        if paar[0].seite_dahinter() > paar[1].erste_seite() {
            return Err(ElfFehler::SegmenteUeberlappen);
        }
    }

    // Und die Gesamt-Spanne bleibt im Rahmen (Segmente können einzeln klein
    // und trotzdem weit auseinander sein).
    let start = segmente[0].erste_seite();
    let ende = segmente
        .iter()
        .map(|s| s.seite_dahinter())
        .max()
        .unwrap_or(start);
    if ende - start > MAX_IMAGE_BYTES {
        return Err(ElfFehler::SegmentZuGross);
    }

    // (g) DER EINSPRUNGPUNKT. Er muss in einem AUSFÜHRBAREN Segment liegen
    //     und in dessen aus der Datei geladenem Teil — ein Einsprung in
    //     .bss wäre ein Sprung in genullten Speicher.
    let einsprung_gueltig = segmente.iter().any(|s| {
        s.rechte.ausfuehren
            && e_entry >= s.virt_adresse
            && e_entry < s.virt_adresse + s.datei_bytes as u64
    });
    if !einsprung_gueltig {
        return Err(ElfFehler::EinsprungNichtAusfuehrbar);
    }

    Ok(ElfProgramm {
        einsprung: e_entry,
        segmente,
    })
}

// ---------------------------------------------------------------------------
// (2) LADEN — jetzt darf Speicher angefasst werden
// ---------------------------------------------------------------------------

/// Lädt ein geprüftes ELF in einen FRISCHEN Adressraum.
///
/// Reihenfolge je Segment: **mappen, dann füllen.** Und zwar mit den
/// ENDGÜLTIGEN Rechten — auch bei einem nur lesbaren Code-Segment. Das geht,
/// weil der Kernel den Inhalt nicht über die User-Adresse schreibt, sondern
/// über `AdressRaum::schreiben`, also über das Physik-Komplettmapping. Aus
/// Kernel-Sicht ist die Seite ganz normal beschreibbar; für Ring 3 ist sie es
/// nie gewesen. Es gibt also kein Zeitfenster, in dem Code-Seiten
/// beschreibbar wären — der Klassiker "erst RW mappen, füllen, dann auf RX
/// umstellen" wird hier gar nicht erst gebraucht.
///
/// `.bss` (der Teil hinter `datei_bytes`) wird NICHT eigens genullt: Jeder
/// frisch gemappte Frame ist schon genullt (`AdressRaum::map_benutzer`
/// nullt ihn, damit kein Byte des Vorbesitzers nach Ring 3 leckt). Wir
/// schreiben nur `datei_bytes` hinein, der Rest bleibt 0 — die
/// `.bss`-Garantie fällt aus einer Sicherheitsmassnahme heraus ab.
/// `test_bss_ist_genullt` misst das trotzdem nach, statt es zu glauben.
pub fn laden(raum: &mut AdressRaum, bytes: &[u8]) -> ElfErgebnis<ElfProgramm> {
    let programm = pruefen(bytes)?;

    for segment in &programm.segmente {
        let erste = segment.erste_seite();
        let seiten_bytes = (segment.seite_dahinter() - erste) as usize;

        raum.bereich_mappen_mit_rechten(VirtAddr::new(erste), seiten_bytes, segment.rechte)
            .map_err(|_| ElfFehler::KeinSpeicher)?;

        if segment.datei_bytes > 0 {
            // Der Bereich ist von `pruefen` als vollständig in der Datei
            // liegend bestätigt — dieser Slice kann nicht danebengreifen.
            let inhalt = &bytes[segment.datei_offset..segment.datei_offset + segment.datei_bytes];
            raum.schreiben(VirtAddr::new(segment.virt_adresse), inhalt)
                .map_err(|_| ElfFehler::KeinSpeicher)?;
        }
    }

    Ok(programm)
}

/// Ist das plausibel eine ausführbare Datei? Reine Anschauung der ersten
/// Bytes — für den Explorer (Doppelklick) und die Shell, die es wissen
/// wollen, ohne die ganze Datei zu prüfen.
pub fn sieht_ausfuehrbar_aus(kopf: &[u8]) -> bool {
    kopf.len() >= 20
        && kopf[0..4] == MAGIE
        && kopf[4] == KLASSE_64
        && kopf[5] == DATEN_LE
        && u16_bei(kopf, 16) == Ok(TYP_EXEC)
        && u16_bei(kopf, 18) == Ok(MASCHINE_X86_64)
}

// ---------------------------------------------------------------------------
// Tests der reinen Prüf-Logik
//
// Die Angriffs-Tests (abgeschnitten, Kernel-Adressen, überlappend, ...) und
// der echte Lade-Beweis liegen in tests/elf.rs — dort gibt es einen
// Adressraum. Hier nur das, was ohne Hardware auskommt.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut ein MINIMALES, gültiges ELF64 mit einem Code-Segment. Die
    /// Grundlage aller Angriffs-Tests: Wir bauen etwas Gültiges und drehen
    /// dann genau EINE Schraube kaputt — so ist immer klar, welche Prüfung
    /// zugeschlagen hat.
    pub(crate) fn minimal_elf() -> Vec<u8> {
        bauen(&[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0x1000)], IMAGE_START)
    }

    /// Baut ein ELF aus (typ, flags, offset, vaddr, filesz, memsz, align)
    /// und einem Einsprungpunkt. Die Datei wird gross genug für alle
    /// Segment-Inhalte gemacht.
    pub(crate) fn bauen(
        header: &[(u32, u32, u64, u64, u64, u64, u64)],
        einsprung: u64,
    ) -> Vec<u8> {
        let ph_offset = HEADER_GROESSE as u64;
        let noetig = header
            .iter()
            .map(|h| h.2.saturating_add(h.4))
            .max()
            .unwrap_or(0)
            .max(ph_offset + (header.len() * PH_GROESSE) as u64);
        let mut datei = alloc::vec![0u8; noetig as usize];

        datei[0..4].copy_from_slice(&MAGIE);
        datei[4] = KLASSE_64;
        datei[5] = DATEN_LE;
        datei[6] = VERSION_1;
        datei[16..18].copy_from_slice(&TYP_EXEC.to_le_bytes());
        datei[18..20].copy_from_slice(&MASCHINE_X86_64.to_le_bytes());
        datei[20..24].copy_from_slice(&1u32.to_le_bytes());
        datei[24..32].copy_from_slice(&einsprung.to_le_bytes());
        datei[32..40].copy_from_slice(&ph_offset.to_le_bytes());
        datei[52..54].copy_from_slice(&(HEADER_GROESSE as u16).to_le_bytes());
        datei[54..56].copy_from_slice(&(PH_GROESSE as u16).to_le_bytes());
        datei[56..58].copy_from_slice(&(header.len() as u16).to_le_bytes());

        for (i, (typ, flags, offset, vaddr, filesz, memsz, align)) in header.iter().enumerate() {
            let b = ph_offset as usize + i * PH_GROESSE;
            datei[b..b + 4].copy_from_slice(&typ.to_le_bytes());
            datei[b + 4..b + 8].copy_from_slice(&flags.to_le_bytes());
            datei[b + 8..b + 16].copy_from_slice(&offset.to_le_bytes());
            datei[b + 16..b + 24].copy_from_slice(&vaddr.to_le_bytes());
            datei[b + 24..b + 32].copy_from_slice(&vaddr.to_le_bytes()); // p_paddr
            datei[b + 32..b + 40].copy_from_slice(&filesz.to_le_bytes());
            datei[b + 40..b + 48].copy_from_slice(&memsz.to_le_bytes());
            datei[b + 48..b + 56].copy_from_slice(&align.to_le_bytes());
        }
        datei
    }

    /// Das gültige Minimal-ELF wird angenommen — sonst wären alle
    /// Negativ-Tests wertlos (sie könnten aus dem falschen Grund scheitern).
    #[test_case]
    fn test_gueltiges_elf_wird_angenommen() {
        let programm = pruefen(&minimal_elf()).expect("Minimal-ELF muss gueltig sein");
        assert_eq!(programm.einsprung, IMAGE_START);
        assert_eq!(programm.segmente.len(), 1);
        assert_eq!(programm.segmente[0].virt_adresse, IMAGE_START);
        assert_eq!(programm.segmente[0].rechte, Rechte::AUSFUEHREN);
        assert_eq!(programm.segmente[0].bss_bytes(), 0);
        assert!(sieht_ausfuehrbar_aus(&minimal_elf()));
    }

    /// ABGESCHNITTEN: Bei JEDER Länge < der vollen Datei muss ein Fehler
    /// kommen und nie eine Panik. Das ist der Test, der Slice-Fehler in den
    /// Lese-Helfern zuverlässig findet.
    #[test_case]
    fn test_abgeschnitten_an_jeder_stelle() {
        let voll = minimal_elf();
        for laenge in 0..voll.len() {
            let ergebnis = pruefen(&voll[..laenge]);
            assert!(
                ergebnis.is_err(),
                "Abgeschnitten auf {} Byte muesste abgelehnt werden",
                laenge
            );
        }
        // Und ganz ohne Inhalt:
        assert_eq!(pruefen(&[]), Err(ElfFehler::ZuKurz));
        assert!(!sieht_ausfuehrbar_aus(&[]));
    }

    /// Die Header-Prüfungen einzeln — jede dreht genau EIN Byte/Feld.
    #[test_case]
    fn test_header_pruefungen() {
        // Magie kaputt:
        let mut datei = minimal_elf();
        datei[1] = b'X';
        assert_eq!(pruefen(&datei), Err(ElfFehler::KeinElf));
        // 32 Bit:
        let mut datei = minimal_elf();
        datei[4] = 1;
        assert_eq!(pruefen(&datei), Err(ElfFehler::Kein64Bit));
        // Big Endian:
        let mut datei = minimal_elf();
        datei[5] = 2;
        assert_eq!(pruefen(&datei), Err(ElfFehler::FalscheBytereihenfolge));
        // Version:
        let mut datei = minimal_elf();
        datei[6] = 7;
        assert_eq!(pruefen(&datei), Err(ElfFehler::FalscheVersion));
        // PIE/ET_DYN bekommt seinen EIGENEN Fehler (haeufigster Bau-Fehler):
        let mut datei = minimal_elf();
        datei[16..18].copy_from_slice(&TYP_DYN.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::DynamischGelinkt));
        // Objektdatei (ET_REL):
        let mut datei = minimal_elf();
        datei[16..18].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::KeinProgramm));
        // Falsche Architektur (ARM64):
        let mut datei = minimal_elf();
        datei[18..20].copy_from_slice(&0xB7u16.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::FalscheArchitektur));
        // e_phentsize verdreht -> wir wuerden mit falscher Schrittweite lesen:
        let mut datei = minimal_elf();
        datei[54..56].copy_from_slice(&32u16.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::KaputterHeader));
        // Keine Program-Header:
        let mut datei = minimal_elf();
        datei[56..58].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::KeineSegmente));
        // e_phoff absurd (Ueberlauf-Kandidat):
        let mut datei = minimal_elf();
        datei[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::KaputterHeader));
        // e_phnum absurd hoch:
        let mut datei = minimal_elf();
        datei[56..58].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::ZuVieleSegmente));
    }

    /// DIE SICHERHEITSKRITISCHE PRÜFUNG: Ein Segment darf niemals ausserhalb
    /// des Programm-Bereichs liegen. Jede dieser Dateien ist ein Angriff.
    #[test_case]
    fn test_segment_adressen_angriffe() {
        // (1) Segment auf den KERNEL-HEAP: Der Lader wuerde Kernel-Speicher
        //     ueberschreiben, wenn er das glaubte.
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, crate::allocator::HEAP_START as u64, 0x40, 0x40, 0)],
            crate::allocator::HEAP_START as u64,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbUserBereich));

        // (2) Segment auf die NULLSEITE.
        let datei = bauen(&[(PT_LOAD, PF_R | PF_X, 0x1000, 0, 0x40, 0x40, 0)], 0);
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbUserBereich));

        // (3) Segment in die obere Haelfte (klassische Kernel-Adresse).
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, 0xffff_8000_0000_0000, 0x40, 0x40, 0)],
            0xffff_8000_0000_0000,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbUserBereich));

        // (4) INTEGER-ÜBERLAUF: p_vaddr dicht unter u64::MAX plus grosse
        //     p_memsz — das Ende darf nicht "hinten wieder rauskommen" und
        //     dabei den ganzen Kernel umschliessen.
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, u64::MAX - 0x100, 0x40, 0x1000, 0)],
            u64::MAX - 0x100,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbUserBereich));

        // (5) Segment beginnt gueltig, ragt aber ueber das Image-Ende hinaus
        //     (in Richtung Stack) — der Teil-Treffer, den eine reine
        //     Anfangs-Pruefung durchliesse.
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_ENDE - 0x1000, 0x40, 0x4000, 0)],
            IMAGE_ENDE - 0x1000,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbUserBereich));

        // (6) GENAU bis an die Obergrenze ist dagegen erlaubt (Ende exklusiv).
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_ENDE - 0x1000, 0x40, 0x1000, 0)],
            IMAGE_ENDE - 0x1000,
        );
        assert!(pruefen(&datei).is_ok(), "Segment bis exakt IMAGE_ENDE ist gueltig");
    }

    /// Datei-Offsets: absurde Werte und Überläufe.
    #[test_case]
    fn test_segment_datei_angriffe() {
        // p_offset + p_filesz hinter dem Dateiende:
        let mut datei = minimal_elf();
        let b = HEADER_GROESSE;
        datei[b + 32..b + 40].copy_from_slice(&0xFFFF_0000u64.to_le_bytes()); // p_filesz
        datei[b + 40..b + 48].copy_from_slice(&0xFFFF_0000u64.to_le_bytes()); // p_memsz
        assert!(matches!(
            pruefen(&datei),
            Err(ElfFehler::SegmentAusserhalbDerDatei) | Err(ElfFehler::SegmentZuGross)
        ));
        // p_offset dicht unter u64::MAX (Ueberlauf in der Addition):
        let mut datei = minimal_elf();
        datei[b + 8..b + 16].copy_from_slice(&(u64::MAX - 4).to_le_bytes());
        datei[b + 48..b + 56].copy_from_slice(&0u64.to_le_bytes()); // align aus
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentAusserhalbDerDatei));
        // p_memsz < p_filesz:
        let mut datei = minimal_elf();
        datei[b + 32..b + 40].copy_from_slice(&0x40u64.to_le_bytes());
        datei[b + 40..b + 48].copy_from_slice(&0x10u64.to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentKleinerAlsInhalt));
        // p_memsz absurd gross:
        let mut datei = minimal_elf();
        datei[b + 40..b + 48].copy_from_slice(&(4u64 * 1024 * 1024 * 1024).to_le_bytes());
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentZuGross));
    }

    /// W^X und die Lesbarkeit.
    #[test_case]
    fn test_rechte_pruefungen() {
        // Schreibbar UND ausfuehrbar -> abgelehnt.
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_W | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0)],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentSchreibbarUndAusfuehrbar));
        // Gar nicht lesbar -> abgelehnt.
        let datei = bauen(
            &[(PT_LOAD, PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0)],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmentNichtLesbar));
        // Die drei erlaubten Kombinationen ergeben die richtigen Rechte.
        for (flags, erwartet) in [
            (PF_R, Rechte::NUR_LESEN),
            (PF_R | PF_W, Rechte::SCHREIBEN),
            (PF_R | PF_X, Rechte::AUSFUEHREN),
        ] {
            // Der Einsprung muss in einem ausfuehrbaren Segment liegen —
            // deshalb bekommt jede Datei zusaetzlich ein Code-Segment.
            let datei = bauen(
                &[
                    (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                    (PT_LOAD, flags, 0x2000, IMAGE_START + 0x2000, 0x40, 0x40, 0),
                ],
                IMAGE_START,
            );
            let programm = pruefen(&datei).expect("gueltige Rechte-Kombination");
            assert_eq!(programm.segmente[1].rechte, erwartet);
        }
    }

    /// ÜBERLAPPUNG auf Seiten-Ebene — der Angriff, mit dem man W^X aushebeln
    /// wollte (RW- und R-X in derselben Seite).
    #[test_case]
    fn test_ueberlappende_segmente() {
        // Zwei Segmente in DERSELBEN Seite, mit gegensaetzlichen Rechten.
        let datei = bauen(
            &[
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                (PT_LOAD, PF_R | PF_W, 0x2000, IMAGE_START + 0x100, 0x40, 0x40, 0),
            ],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmenteUeberlappen));
        // Auch die umgekehrte Reihenfolge in der Tabelle (wir sortieren
        // vorher — der Angriff darf nicht durch Umsortieren durchrutschen).
        let datei = bauen(
            &[
                (PT_LOAD, PF_R | PF_W, 0x2000, IMAGE_START + 0x100, 0x40, 0x40, 0),
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
            ],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::SegmenteUeberlappen));
        // Direkt benachbarte SEITEN sind dagegen in Ordnung.
        let datei = bauen(
            &[
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                (PT_LOAD, PF_R | PF_W, 0x2000, IMAGE_START + 0x1000, 0x40, 0x40, 0),
            ],
            IMAGE_START,
        );
        assert!(pruefen(&datei).is_ok(), "benachbarte Seiten sind kein Ueberlapp");
    }

    /// Der Einsprungpunkt muss in ausführbarem, aus der DATEI geladenem Code
    /// liegen.
    #[test_case]
    fn test_einsprung_pruefung() {
        // Einsprung ausserhalb jedes Segments:
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0)],
            IMAGE_START + 0x8000,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::EinsprungNichtAusfuehrbar));
        // Einsprung in ein DATEN-Segment (nicht ausfuehrbar):
        let datei = bauen(
            &[
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                (PT_LOAD, PF_R | PF_W, 0x2000, IMAGE_START + 0x2000, 0x40, 0x40, 0),
            ],
            IMAGE_START + 0x2000,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::EinsprungNichtAusfuehrbar));
        // Einsprung in den .bss-TEIL eines Code-Segments (waere genullter
        // Speicher, also kein Code):
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x400, 0)],
            IMAGE_START + 0x200,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::EinsprungNichtAusfuehrbar));
    }

    /// Ausrichtung und dynamisches Linken.
    #[test_case]
    fn test_ausrichtung_und_interpreter() {
        // p_align keine Zweierpotenz:
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 3000)],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::FalscheAusrichtung));
        // Versatz in Datei und Adresse passen nicht zusammen:
        let datei = bauen(
            &[(PT_LOAD, PF_R | PF_X, 0x1008, IMAGE_START, 0x40, 0x40, 0x1000)],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::FalscheAusrichtung));
        // PT_INTERP -> braucht einen dynamischen Linker:
        let datei = bauen(
            &[
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                (PT_INTERP, PF_R, 0x2000, 0, 0x10, 0x10, 0),
            ],
            IMAGE_START,
        );
        assert_eq!(pruefen(&datei), Err(ElfFehler::BrauchtInterpreter));
    }

    /// Nicht-ladbare Program-Header (PT_NOTE, PT_GNU_STACK, ...) werden
    /// stillschweigend übergangen — ein echtes ELF hat immer welche.
    #[test_case]
    fn test_unbekannte_segmenttypen_werden_uebergangen() {
        let datei = bauen(
            &[
                (4, PF_R, 0x1000, 0, 0x10, 0x10, 0),           // PT_NOTE
                (PT_LOAD, PF_R | PF_X, 0x1000, IMAGE_START, 0x40, 0x40, 0),
                (0x6474e551, PF_R | PF_W, 0, 0, 0, 0, 0),       // PT_GNU_STACK
            ],
            IMAGE_START,
        );
        let programm = pruefen(&datei).expect("Nur PT_LOAD zaehlt");
        assert_eq!(programm.segmente.len(), 1);
    }

    /// `sieht_ausfuehrbar_aus` ist bewusst OBERFLÄCHLICH — es darf nie
    /// panicken und muss offensichtliche Nicht-Programme abweisen.
    #[test_case]
    fn test_schnellpruefung() {
        assert!(sieht_ausfuehrbar_aus(&minimal_elf()));
        assert!(!sieht_ausfuehrbar_aus(b"Hallo, ich bin eine Textdatei."));
        assert!(!sieht_ausfuehrbar_aus(&[0x7F, b'E']));
        assert!(!sieht_ausfuehrbar_aus(&[]));
        // Ein 32-Bit-ELF sieht am Anfang gleich aus, ist aber keins fuer uns.
        let mut datei = minimal_elf();
        datei[4] = 1;
        assert!(!sieht_ausfuehrbar_aus(&datei));
    }

    /// Alle Fehler haben eine nicht-leere Meldung (die sieht ein Mensch).
    #[test_case]
    fn test_fehler_meldungen() {
        for fehler in [
            ElfFehler::ZuKurz,
            ElfFehler::KeinElf,
            ElfFehler::DynamischGelinkt,
            ElfFehler::SegmentAusserhalbUserBereich,
            ElfFehler::SegmentSchreibbarUndAusfuehrbar,
            ElfFehler::EinsprungNichtAusfuehrbar,
            ElfFehler::KeinSpeicher,
        ] {
            assert!(!fehler.meldung().is_empty());
        }
    }
}
