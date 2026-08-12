// usb::hid — Tastatur und Maus ueber das Boot Protocol
//
// ===========================================================================
// BOOT PROTOCOL UND NICHT DER REPORT-DESCRIPTOR-PARSER
//
// Ein HID-Geraet beschreibt das FORMAT seiner Reports selbst, in einem
// eigenen „Report Descriptor": eine kleine Stack-Sprache mit Usage
// Pages, Logical Minimum/Maximum, Report Size, Report Count und
// verschachtelten Collections. Sie vollstaendig zu lesen heisst, einen
// zweiten Parser fuer fremde, feindliche Daten zu bauen — mehr Arbeit
// als dieser ganze Treiber.
//
// **Das Boot Protocol ist die Abkuerzung, und sie ist legitim.** Die
// HID-Spezifikation definiert fuer Tastatur und Maus ein FESTES
// Report-Format, das jedes Geraet mit `bInterfaceSubClass == 1`
// beherrschen MUSS — genau damit ein BIOS sie ohne Parser benutzen
// kann. Ein `Set Protocol (Boot)` genuegt, und danach ist das Format
// bekannt:
//
//   Tastatur: 8 Byte — [Modifier][reserviert][6 Keycodes]
//   Maus:     3+ Byte — [Tasten][dX][dY]([Rad])
//
// ===========================================================================
// DIE GRENZE, DIE DAMIT ENTSTEHT — ausdruecklich notiert
//
//   * Geraete OHNE Boot-Subclass (viele Gaming-Tastaturen, Grafik-
//     tabletts, Multimedia-Tasten, Joysticks) liefern nichts. Sie
//     brauchen den Report-Descriptor-Parser.
//   * Im Boot Protocol gibt es hoechstens SECHS gleichzeitig gedrueckte
//     Tasten (plus Modifier). Mehr meldet das Geraet als „Rollover".
//   * Die Maus liefert nur drei Tasten und zwei Achsen; Zusatztasten
//     und horizontales Rad fallen weg.
//
// Steht so in docs/grenzen.md. Fuer das Ziel dieser Serie — den Laptop
// bedienbar machen — reicht es: Eine eingebaute Notebook-Tastatur ist
// immer boot-faehig, weil das BIOS sie sonst nicht benutzen koennte.
//
// ===========================================================================
// DIE LAYOUT-LOGIK WIRD GETEILT, NICHT DUPLIZIERT
//
// Das ist die wichtigste Entscheidung dieser Datei. Die naheliegende
// Loesung waere, HID-Keycodes direkt auf Zeichen abzubilden — und damit
// haette SpeedOS ZWEI Tastaturlayouts: eines in `pc_keyboard` fuer
// PS/2 und ein zweites hier. Sie liefen bei der ersten Abweichung
// auseinander, und niemand wuesste, welches gilt.
//
// Statt dessen uebersetzt dieser Treiber HID-Keycodes in **PS/2-Set-1-
// Scancodes** und legt sie in DIESELBE Queue (`task::keyboard::
// add_scancode`). Danach laeuft alles unveraendert weiter: dieselbe
// QWERTZ-Tabelle, dieselben Modifier, dieselbe Strg+C-Dekodierung,
// derselbe Eingabe-Router.
//
// **Der Rest des Systems merkt nicht, woher die Eingabe kommt** — und
// das ist der Beweis, dass die Naht sitzt.
//
// Dasselbe fuer die Maus: Aus einem HID-Report werden PS/2-Maus-Bytes,
// die in `maus::byte_hinzufuegen` gehen. `maus::paket_parsen` bleibt
// unangetastet.

use crate::usb::deskriptor::KLASSE_HID;
use alloc::vec::Vec;

/// `bInterfaceProtocol` im Boot-Interface.
pub const PROTOKOLL_TASTATUR: u8 = 1;
pub const PROTOKOLL_MAUS: u8 = 2;
/// `bInterfaceSubClass` — 1 = Boot Interface.
pub const SUBKLASSE_BOOT: u8 = 1;

// ===========================================================================
// TASTATUR: HID-USAGE -> PS/2-SET-1-SCANCODE
// ===========================================================================

/// Die Uebersetzungstabelle HID-Usage-ID -> Set-1-Scancode.
///
/// ===================================================================
/// WARUM EINE TABELLE UND KEINE RECHNUNG
///
/// Die beiden Nummerierungen haben keinen Zusammenhang: HID zaehlt
/// alphabetisch (A = 0x04, B = 0x05, …), Set 1 zaehlt nach der
/// PHYSISCHEN LAGE auf der Tastatur (Q = 0x10, W = 0x11, …). Jede
/// Formel waere eine Erfindung.
///
/// Der Index ist die HID-Usage-ID, der Wert der Set-1-Scancode.
/// **0 heisst „kennen wir nicht"** und wird verworfen — eine falsche
/// Taste zu erzeugen waere schlimmer, als keine zu erzeugen.
///
/// Abgedeckt ist der Boot-Bereich 0x04..0x65: Buchstaben, Ziffern,
/// Steuertasten, Satzzeichen, F1–F12, Cursorblock, Ziffernblock.
static HID_ZU_SET1: [u8; 0x66] = {
    let mut t = [0u8; 0x66];
    // Buchstaben A..Z (HID 0x04..0x1D) — nach PHYSISCHER Lage.
    t[0x04] = 0x1E; // A
    t[0x05] = 0x30; // B
    t[0x06] = 0x2E; // C
    t[0x07] = 0x20; // D
    t[0x08] = 0x12; // E
    t[0x09] = 0x21; // F
    t[0x0A] = 0x22; // G
    t[0x0B] = 0x23; // H
    t[0x0C] = 0x17; // I
    t[0x0D] = 0x24; // J
    t[0x0E] = 0x25; // K
    t[0x0F] = 0x26; // L
    t[0x10] = 0x32; // M
    t[0x11] = 0x31; // N
    t[0x12] = 0x18; // O
    t[0x13] = 0x19; // P
    t[0x14] = 0x10; // Q
    t[0x15] = 0x13; // R
    t[0x16] = 0x1F; // S
    t[0x17] = 0x14; // T
    t[0x18] = 0x16; // U
    t[0x19] = 0x2F; // V
    t[0x1A] = 0x11; // W
    t[0x1B] = 0x2D; // X
    // ACHTUNG: HID 0x1C ist die Taste an der Stelle, die auf einer
    // US-Tastatur „Y" heisst. Auf QWERTZ sitzt dort das Z — aber das
    // entscheidet die LAYOUT-Schicht, nicht wir. Wir liefern die
    // PHYSISCHE Lage (Set-1 0x15), und `pc_keyboard` macht daraus mit
    // dem deutschen Layout ein Z. Genau deshalb wird hier nicht auf
    // Zeichen abgebildet.
    t[0x1C] = 0x15; // (US-Y / DE-Z)
    t[0x1D] = 0x2C; // (US-Z / DE-Y)
    // Ziffernreihe 1..0
    t[0x1E] = 0x02;
    t[0x1F] = 0x03;
    t[0x20] = 0x04;
    t[0x21] = 0x05;
    t[0x22] = 0x06;
    t[0x23] = 0x07;
    t[0x24] = 0x08;
    t[0x25] = 0x09;
    t[0x26] = 0x0A;
    t[0x27] = 0x0B;
    // Steuertasten
    t[0x28] = 0x1C; // Enter
    t[0x29] = 0x01; // Esc
    t[0x2A] = 0x0E; // Backspace
    t[0x2B] = 0x0F; // Tab
    t[0x2C] = 0x39; // Leertaste
    t[0x2D] = 0x0C; // - / ss
    t[0x2E] = 0x0D; // = / Akzent
    t[0x2F] = 0x1A; // [ / ue
    t[0x30] = 0x1B; // ] / +
    t[0x31] = 0x2B; // Backslash / #
    t[0x32] = 0x2B; // Non-US #
    t[0x33] = 0x27; // ; / oe
    t[0x34] = 0x28; // ' / ae
    t[0x35] = 0x29; // ` / ^
    t[0x36] = 0x33; // Komma
    t[0x37] = 0x34; // Punkt
    t[0x38] = 0x35; // / / -
    t[0x39] = 0x3A; // Feststell
    // F1..F12
    t[0x3A] = 0x3B;
    t[0x3B] = 0x3C;
    t[0x3C] = 0x3D;
    t[0x3D] = 0x3E;
    t[0x3E] = 0x3F;
    t[0x3F] = 0x40;
    t[0x40] = 0x41;
    t[0x41] = 0x42;
    t[0x42] = 0x43;
    t[0x43] = 0x44;
    t[0x44] = 0x57;
    t[0x45] = 0x58;
    // Ziffernblock und Rest
    t[0x47] = 0x46; // Rollen
    t[0x53] = 0x45; // Num
    t[0x54] = 0x35; // Numpad /
    t[0x55] = 0x37; // Numpad *
    t[0x56] = 0x4A; // Numpad -
    t[0x57] = 0x4E; // Numpad +
    t[0x59] = 0x4F;
    t[0x5A] = 0x50;
    t[0x5B] = 0x51;
    t[0x5C] = 0x4B;
    t[0x5D] = 0x4C;
    t[0x5E] = 0x4D;
    t[0x5F] = 0x47;
    t[0x60] = 0x48;
    t[0x61] = 0x49;
    t[0x62] = 0x52;
    t[0x63] = 0x53;
    t[0x64] = 0x56; // Non-US Backslash (auf DE die Taste links neben Y)
    t
};

/// Die Tasten des Cursorblocks brauchen ein **E0-Praefix**.
///
/// In Set 1 unterscheiden sich der Cursorblock und der Ziffernblock nur
/// durch dieses Praefix: 0x48 allein ist „Numpad 8", `E0 48` ist „Pfeil
/// hoch". Wer es weglaesst, bekommt beim Druck auf die Pfeiltaste eine
/// Ziffer — und das faellt erst auf, wenn jemand in einem Textfeld
/// navigiert.
fn braucht_e0(hid: u8) -> bool {
    matches!(
        hid,
        0x46 // Druck
            | 0x49 // Einfg
            | 0x4A // Pos1
            | 0x4B // Bild hoch
            | 0x4C // Entf
            | 0x4D // Ende
            | 0x4E // Bild runter
            | 0x4F // Rechts
            | 0x50 // Links
            | 0x51 // Runter
            | 0x52 // Hoch
            | 0x58 // Numpad Enter
            | 0x65 // Menue
    )
}

/// Der Set-1-Code fuer Cursorblock und Konsorten (mit E0 davor).
fn e0_code(hid: u8) -> u8 {
    match hid {
        0x46 => 0x37,
        0x49 => 0x52,
        0x4A => 0x47,
        0x4B => 0x49,
        0x4C => 0x53,
        0x4D => 0x4F,
        0x4E => 0x51,
        0x4F => 0x4D,
        0x50 => 0x4B,
        0x51 => 0x50,
        0x52 => 0x48,
        0x58 => 0x1C,
        0x65 => 0x5D,
        _ => 0,
    }
}

/// Die acht Modifier-Bits des Boot-Reports, in Set-1-Codes.
///
/// Bit 0 LStrg, 1 LShift, 2 LAlt, 3 LSuper, 4 RStrg, 5 RShift,
/// 6 RAlt (AltGr!), 7 RSuper. Die rechten Varianten brauchen E0.
const MODIFIER: [(u8, bool); 8] = [
    (0x1D, false), // LStrg
    (0x2A, false), // LShift
    (0x38, false), // LAlt
    (0x5B, true),  // LSuper (E0)
    (0x1D, true),  // RStrg (E0)
    (0x36, false), // RShift
    (0x38, true),  // RAlt = AltGr (E0)
    (0x5C, true),  // RSuper (E0)
];

// ===========================================================================
// DER ZUSTAND EINER TASTATUR
// ===========================================================================

/// Wie oft eine gehaltene Taste wiederholt wird (Millisekunden).
///
/// ===================================================================
/// DAUERFEUER MUSS SELBST ERZEUGT WERDEN
///
/// **USB liefert ZUSTAENDE, keine Ereignisse.** Ein Boot-Report sagt
/// „diese sechs Tasten sind gerade gedrueckt" — und solange man eine
/// haelt, kommt derselbe Report immer wieder. Ein Treiber, der einfach
/// jeden Report weiterreicht, erzeugt Dauerfeuer mit der
/// Endpunkt-Rate (bei QEMU 100/s); einer, der nur Aenderungen meldet,
/// erzeugt gar keines.
///
/// PS/2 macht das in der Hardware (Verzoegerung, dann Rate). Damit sich
/// USB genauso anfuehlt, wird es hier nachgebaut: erst
/// `WIEDERHOLUNG_START_MS` warten, dann alle `WIEDERHOLUNG_MS` einen
/// weiteren Tastendruck erzeugen.
const WIEDERHOLUNG_START_MS: u64 = 400;
const WIEDERHOLUNG_MS: u64 = 40;

/// Der Zustand einer Tastatur zwischen zwei Reports.
#[derive(Default)]
pub struct TastaturZustand {
    /// Die zuletzt gemeldeten Keycodes (bis zu sechs).
    letzte: [u8; 6],
    /// Die zuletzt gemeldeten Modifier-Bits.
    letzte_modifier: u8,
    /// Welche Taste gerade fuer die Wiederholung laeuft, und wann sie
    /// das naechste Mal feuert.
    wiederhol_taste: u8,
    wiederhol_faellig_ms: u64,
}

/// Ein Scancode, wie er in die Queue geht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scancode {
    pub e0: bool,
    pub code: u8,
    /// true = Loslassen (Set 1 setzt dafuer Bit 7).
    pub los: bool,
}

impl Scancode {
    /// Die Bytes, die in die Queue gehen.
    pub fn bytes(&self) -> ([u8; 2], usize) {
        let code = if self.los { self.code | 0x80 } else { self.code };
        if self.e0 {
            ([0xE0, code], 2)
        } else {
            ([code, 0], 1)
        }
    }
}

impl TastaturZustand {
    /// Einen 8-Byte-Boot-Report verarbeiten.
    ///
    /// Liefert die Scancodes, die daraus folgen — **als Liste, nicht als
    /// Seiteneffekt**. Damit ist die ganze Logik eine reine Funktion auf
    /// Zahlen und ohne Hardware testbar; wer sie einspeist, entscheidet
    /// der Aufrufer.
    pub fn report(&mut self, daten: &[u8], jetzt_ms: u64) -> Vec<Scancode> {
        let mut aus = Vec::new();
        if daten.len() < 8 {
            return aus;
        }
        let modifier = daten[0];
        let tasten = [daten[2], daten[3], daten[4], daten[5], daten[6], daten[7]];

        // --- Modifier: jedes Bit einzeln vergleichen ---
        for (bit, (code, e0)) in MODIFIER.iter().enumerate() {
            let jetzt_gedrueckt = modifier & (1 << bit) != 0;
            let vorher = self.letzte_modifier & (1 << bit) != 0;
            if jetzt_gedrueckt != vorher {
                aus.push(Scancode {
                    e0: *e0,
                    code: *code,
                    los: !jetzt_gedrueckt,
                });
            }
        }

        // --- ROLLOVER: das Geraet meldet „zu viele Tasten" ---
        //
        // Keycode 1 in ALLEN sechs Feldern heisst „ErrorRollOver". Das
        // ist keine Taste, sondern eine Entschuldigung. Wer sie als
        // Keycode behandelt, erzeugt Geistertasten.
        if tasten[0] == 1 {
            self.letzte_modifier = modifier;
            return aus;
        }

        // --- Losgelassene Tasten: in `letzte`, aber nicht mehr jetzt ---
        for alt in self.letzte {
            if alt >= 4 && !tasten.contains(&alt) {
                if let Some(s) = scancode_von(alt, true) {
                    aus.push(s);
                }
                if self.wiederhol_taste == alt {
                    self.wiederhol_taste = 0;
                }
            }
        }

        // --- Neu gedrueckte Tasten ---
        for neu in tasten {
            if neu >= 4 && !self.letzte.contains(&neu) {
                if let Some(s) = scancode_von(neu, false) {
                    aus.push(s);
                }
                // Die ZULETZT gedrueckte Taste bekommt das Dauerfeuer —
                // so verhaelt sich jede Tastatur.
                self.wiederhol_taste = neu;
                self.wiederhol_faellig_ms = jetzt_ms + WIEDERHOLUNG_START_MS;
            }
        }

        self.letzte = tasten;
        self.letzte_modifier = modifier;
        aus
    }

    /// Faelliges Dauerfeuer erzeugen. Wird unabhaengig von Reports
    /// gerufen — sonst wiederholte sich nichts, solange das Geraet
    /// (korrekt) denselben Zustand meldet.
    /// Faelliges Dauerfeuer erzeugen — **nur, wenn der Verbraucher
    /// hinterherkommt**.
    ///
    /// ===================================================================
    /// DIE RUECKKOPPLUNG, DIE DEN LAPTOP AUFGEHAENGT HAT
    ///
    /// Die erste Fassung schaute nur auf die Uhr. Das erzeugte eine
    /// Schleife, die sich selbst am Leben hielt:
    ///
    ///   1. Taste gedrueckt -> Wiederholung scharf.
    ///   2. Die Shell verarbeitet den Anschlag SYNCHRON — der
    ///      kooperative Executor ist blockiert.
    ///   3. Der USB-Task laeuft nicht, also wird das LOSLASS-Signal
    ///      nicht verarbeitet; `wiederhol_taste` bleibt stehen.
    ///   4. Kommt der Task endlich dran, ist die Frist laengst
    ///      ueberschritten -> er erzeugt sofort eine Wiederholung.
    ///   5. Die Shell verarbeitet sie -> zurueck zu 3.
    ///
    /// **Jeder erzeugte Anschlag garantierte den naechsten.** Die Maus
    /// war nicht betroffen, weil sie keine Wiederholung hat — genau
    /// deshalb lief sie, waehrend Tippen den Rechner anhielt.
    ///
    /// Der Fehler war nicht die Frist, sondern dass ein Erzeuger
    /// Eingaben nachlegt, ohne zu pruefen, ob der Verbraucher sie
    /// ueberhaupt abholt. Deshalb steht hier jetzt GEGENDRUCK: Liegt
    /// noch unverarbeitete Eingabe in der Queue, wird NICHTS
    /// nachgelegt. Wiederholung ist eine Bequemlichkeit fuer jemanden,
    /// der mitkommt; wer im Rueckstand ist, braucht sie per Definition
    /// nicht.
    ///
    /// `stau` ist die Zahl der wartenden Scancodes
    /// (`task::keyboard::wartende_scancodes`). Sie wird uebergeben und
    /// nicht hier geholt, damit diese Funktion rein bleibt und
    /// testbar ist.
    pub fn wiederholung(&mut self, jetzt_ms: u64, stau: usize) -> Option<Scancode> {
        if self.wiederhol_taste < 4 || jetzt_ms < self.wiederhol_faellig_ms {
            return None;
        }
        // GEGENDRUCK: Der Verbraucher haengt noch. Nichts nachlegen —
        // und die Frist NICHT verschieben, damit es sofort weitergeht,
        // sobald er aufgeholt hat.
        if stau > 0 {
            return None;
        }
        self.wiederhol_faellig_ms = jetzt_ms + WIEDERHOLUNG_MS;
        scancode_von(self.wiederhol_taste, false)
    }
}

/// Einen HID-Keycode in einen Set-1-Scancode uebersetzen.
pub fn scancode_von(hid: u8, los: bool) -> Option<Scancode> {
    if braucht_e0(hid) {
        let code = e0_code(hid);
        if code == 0 {
            return None;
        }
        return Some(Scancode { e0: true, code, los });
    }
    let code = *HID_ZU_SET1.get(hid as usize)?;
    if code == 0 {
        return None; // unbekannt — lieber nichts als die falsche Taste
    }
    Some(Scancode { e0: false, code, los })
}

// ===========================================================================
// MAUS
// ===========================================================================

/// Einen HID-Maus-Boot-Report in PS/2-Maus-Bytes uebersetzen.
///
/// ===================================================================
/// WARUM DER UMWEG UEBER PS/2-BYTES
///
/// Dieselbe Ueberlegung wie bei der Tastatur: `maus::paket_parsen`,
/// die Beschleunigung, die Cursor-Verwaltung und der Ereignis-Weg sind
/// da und geprueft. Ein zweiter Weg daneben waere eine zweite Stelle,
/// an der die Maus ruckelt.
///
/// DIE VORZEICHEN SIND DER UNTERSCHIED, UND ZWAR ZWEIMAL: Auf der
/// LEITUNG zaehlt HID dY nach unten, PS/2 nach oben — deshalb wird hier
/// invertiert. `maus::paket_parsen` dreht das Hardware-Vorzeichen dann
/// aber SELBST noch einmal um und liefert Bildschirm-Koordinaten. Die
/// beiden Umkehrungen heben sich auf, und genau das ist gewollt:
/// HID-„unten" bleibt Bildschirm-„unten".
///
/// Wer nur die erste Umkehrung sieht, laesst die Inversion weg und
/// bekommt eine spiegelverkehrte Maus; wer nur die zweite sieht,
/// invertiert zweimal. Siehe `test_maus_dreht_die_y_achse`.
///
/// Der PS/2-Kopf traegt in Bit 3 ein festes Sync-Bit; ohne das verwirft
/// `paket_parsen` das Paket (zu Recht, es ist seine Resynchronisation).
pub fn maus_bytes(daten: &[u8]) -> Option<[u8; 4]> {
    if daten.len() < 3 {
        return None;
    }
    let tasten = daten[0] & 0b0000_0111;
    let dx = daten[1] as i8;
    // DIE ACHSENUMKEHR — siehe oben.
    let dy = (daten[2] as i8).saturating_neg();
    let rad = if daten.len() >= 4 { daten[3] as i8 } else { 0 };

    // Kopf: Tasten, Sync-Bit (immer 1), Vorzeichenbits von X und Y.
    let mut kopf = tasten | 0b0000_1000;
    if dx < 0 {
        kopf |= 0b0001_0000;
    }
    if dy < 0 {
        kopf |= 0b0010_0000;
    }
    Some([kopf, dx as u8, dy as u8, (rad as u8) & 0x0F])
}

// ===========================================================================
// WELCHE GERAETE WIR BEDIENEN
// ===========================================================================

/// Was fuer ein HID-Geraet das ist — oder keins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidArt {
    Tastatur,
    Maus,
}

/// Erkennt dieser Treiber das Geraet?
///
/// Verlangt wird das BOOT-Interface (`bInterfaceSubClass == 1`) — ohne
/// es ist das Report-Format unbekannt, und wir haben keinen Parser
/// dafuer. Ein Geraet abzulehnen, dessen Format man nicht kennt, ist
/// die einzige ehrliche Antwort; es zu raten erzeugt Geistertasten.
pub fn art_von(klasse: u8, unterklasse: u8, protokoll: u8) -> Option<HidArt> {
    if klasse != KLASSE_HID || unterklasse != SUBKLASSE_BOOT {
        return None;
    }
    match protokoll {
        PROTOKOLL_TASTATUR => Some(HidArt::Tastatur),
        PROTOKOLL_MAUS => Some(HidArt::Maus),
        _ => None,
    }
}

// ===========================================================================
// TESTS — reine Zahlenlogik, ohne Hardware
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_buchstabe_wird_zu_set1() {
        // HID 0x04 = A -> Set-1 0x1E, kein E0.
        let s = scancode_von(0x04, false).unwrap();
        assert_eq!(s.code, 0x1E);
        assert!(!s.e0);
        assert!(!s.los);
        // Loslassen setzt Bit 7.
        let (bytes, n) = scancode_von(0x04, true).unwrap().bytes();
        assert_eq!(n, 1);
        assert_eq!(bytes[0], 0x9E);
    }

    /// **DIE QWERTZ-STELLE.** Wir liefern die PHYSISCHE Lage; dass
    /// daraus auf einem deutschen Layout ein Z wird, entscheidet
    /// `pc_keyboard` — dieselbe Tabelle wie bei PS/2.
    #[test_case]
    fn test_physische_lage_statt_zeichen() {
        // HID 0x1C sitzt dort, wo US „Y" hat -> Set-1 0x15.
        assert_eq!(scancode_von(0x1C, false).unwrap().code, 0x15);
        // HID 0x1D („Z" auf US) -> Set-1 0x2C.
        assert_eq!(scancode_von(0x1D, false).unwrap().code, 0x2C);
    }

    /// Cursortasten brauchen E0 — sonst kommt eine Ziffer heraus.
    #[test_case]
    fn test_cursortasten_bekommen_e0() {
        let hoch = scancode_von(0x52, false).unwrap();
        assert!(hoch.e0);
        assert_eq!(hoch.code, 0x48);
        let (bytes, n) = hoch.bytes();
        assert_eq!(n, 2);
        assert_eq!(bytes, [0xE0, 0x48]);
        // Und beim Loslassen bleibt das Praefix.
        let (bytes, n) = scancode_von(0x52, true).unwrap().bytes();
        assert_eq!(n, 2);
        assert_eq!(bytes, [0xE0, 0xC8]);
    }

    #[test_case]
    fn test_unbekannter_keycode_wird_verworfen() {
        // 0x00 = keine Taste, 0x03 = reserviert, 0xFF = Muell.
        assert!(scancode_von(0x00, false).is_none());
        assert!(scancode_von(0x03, false).is_none());
        assert!(scancode_von(0xFF, false).is_none());
    }

    #[test_case]
    fn test_druck_und_loslassen() {
        let mut z = TastaturZustand::default();
        // A gedrueckt.
        let s = z.report(&[0, 0, 0x04, 0, 0, 0, 0, 0], 0);
        assert_eq!(s.len(), 1);
        assert!(!s[0].los);
        // Derselbe Report noch einmal: KEIN neuer Druck (USB liefert
        // Zustaende, nicht Ereignisse).
        let s = z.report(&[0, 0, 0x04, 0, 0, 0, 0, 0], 10);
        assert!(s.is_empty(), "derselbe Zustand ist kein neues Ereignis");
        // Losgelassen.
        let s = z.report(&[0, 0, 0, 0, 0, 0, 0, 0], 20);
        assert_eq!(s.len(), 1);
        assert!(s[0].los);
    }

    #[test_case]
    fn test_modifier_einzeln() {
        let mut z = TastaturZustand::default();
        // LShift (Bit 1) gedrueckt.
        let s = z.report(&[0b10, 0, 0, 0, 0, 0, 0, 0], 0);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].code, 0x2A);
        assert!(!s[0].los);
        // AltGr (Bit 6) dazu — nur DIESE Aenderung wird gemeldet.
        let s = z.report(&[0b100_0010, 0, 0, 0, 0, 0, 0, 0], 10);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].code, 0x38);
        assert!(s[0].e0, "AltGr ist E0 38");
        // Beide los.
        let s = z.report(&[0, 0, 0, 0, 0, 0, 0, 0], 20);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|x| x.los));
    }

    /// **ROLLOVER IST KEINE TASTE.** Keycode 1 heisst „zu viele
    /// gedrueckt" — wer ihn uebersetzt, erzeugt Geistertasten.
    #[test_case]
    fn test_rollover_erzeugt_keine_taste() {
        let mut z = TastaturZustand::default();
        let s = z.report(&[0, 0, 1, 1, 1, 1, 1, 1], 0);
        assert!(s.is_empty());
    }

    #[test_case]
    fn test_kurzer_report_wird_verworfen() {
        let mut z = TastaturZustand::default();
        assert!(z.report(&[0, 0, 0x04], 0).is_empty());
        assert!(z.report(&[], 0).is_empty());
    }

    /// **DAUERFEUER.** Erst nach der Startverzoegerung, dann im Takt.
    #[test_case]
    fn test_wiederholung_erst_nach_verzoegerung() {
        let mut z = TastaturZustand::default();
        z.report(&[0, 0, 0x04, 0, 0, 0, 0, 0], 1000);
        // Zu frueh.
        assert!(z.wiederholung(1100, 0).is_none());
        assert!(z.wiederholung(1399, 0).is_none());
        // Faellig.
        let w = z.wiederholung(1400, 0).expect("jetzt muss es feuern");
        assert_eq!(w.code, 0x1E);
        assert!(!w.los, "Dauerfeuer sind DRUECKE, keine Loslass-Codes");
        // Danach im kurzen Takt.
        assert!(z.wiederholung(1420, 0).is_none());
        assert!(z.wiederholung(1440, 0).is_some());
    }

    /// **DER GEGENDRUCK — der Fehler, der den Laptop aufhaengte.**
    ///
    /// Liegt noch unverarbeitete Eingabe in der Queue, darf KEINE
    /// Wiederholung dazukommen. Sonst erzeugt jeder Anschlag Arbeit,
    /// die den naechsten Anschlag ausloest.
    #[test_case]
    fn test_wiederholung_haelt_bei_rueckstau_an() {
        let mut z = TastaturZustand::default();
        z.report(&[0, 0, 0x04, 0, 0, 0, 0, 0], 1000);
        // Faellig waere es — aber es stauen sich noch 5 Scancodes.
        assert!(
            z.wiederholung(1400, 5).is_none(),
            "bei Rueckstau darf nichts nachgelegt werden"
        );
        // Und die Frist wurde NICHT verschoben: Sobald der Verbraucher
        // aufgeholt hat, geht es sofort weiter.
        assert!(
            z.wiederholung(1400, 0).is_some(),
            "ohne Stau muss es sofort feuern, nicht erst 40 ms spaeter"
        );
    }

    #[test_case]
    fn test_wiederholung_endet_beim_loslassen() {
        let mut z = TastaturZustand::default();
        z.report(&[0, 0, 0x04, 0, 0, 0, 0, 0], 0);
        z.report(&[0, 0, 0, 0, 0, 0, 0, 0], 10);
        assert!(z.wiederholung(10_000, 0).is_none(), "losgelassen = kein Feuer");
    }

    // -------------------------------------------------------------------
    // MAUS
    // -------------------------------------------------------------------

    /// **DIE ACHSENUMKEHR — und wo sie WIRKLICH sitzt.**
    ///
    /// Die erste Fassung dieses Tests erwartete -10 und war falsch. Der
    /// Grund ist lehrreich, weil er ZWEI Umkehrungen betrifft:
    ///
    ///   * Auf der LEITUNG zaehlt HID dY nach unten, PS/2 nach oben.
    ///   * `maus::paket_parsen` dreht das Hardware-Vorzeichen aber
    ///     SCHON SELBST um (`dy: -dy_hardware`) und liefert
    ///     Bildschirm-Koordinaten.
    ///
    /// `Paket.dy` und der HID-Wert haben damit DIESELBE Bedeutung:
    /// positiv = nach unten. Damit das nach dem Umweg durch das
    /// PS/2-Format wieder herauskommt, muss `maus_bytes` invertieren —
    /// die beiden Umkehrungen heben sich auf.
    ///
    /// Wer nur die erste Umkehrung sieht, laesst die Inversion weg und
    /// bekommt eine senkrecht spiegelverkehrte Maus; wer nur die zweite
    /// sieht, dreht im Test das Vorzeichen. Beides ist mir passiert.
    #[test_case]
    fn test_maus_dreht_die_y_achse() {
        // HID: 10 nach unten -> auf dem Bildschirm ebenfalls nach unten.
        let b = maus_bytes(&[0, 0, 10]).unwrap();
        let p = crate::maus::paket_parsen(b[0], b[1], b[2], Some(b[3])).unwrap();
        assert_eq!(p.dy, 10, "HID unten muss Bildschirm unten bleiben");
        // HID: 10 nach oben.
        let b = maus_bytes(&[0, 0, (-10i8) as u8]).unwrap();
        let p = crate::maus::paket_parsen(b[0], b[1], b[2], Some(b[3])).unwrap();
        assert_eq!(p.dy, -10);
    }

    #[test_case]
    fn test_maus_x_und_tasten() {
        // Linke Taste (Bit 0), 5 nach rechts.
        let b = maus_bytes(&[0b001, 5, 0]).unwrap();
        let p = crate::maus::paket_parsen(b[0], b[1], b[2], Some(b[3])).unwrap();
        assert_eq!(p.dx, 5);
        assert!(p.links);
        assert!(!p.rechts);
        // Nach links.
        let b = maus_bytes(&[0, (-7i8) as u8, 0]).unwrap();
        let p = crate::maus::paket_parsen(b[0], b[1], b[2], Some(b[3])).unwrap();
        assert_eq!(p.dx, -7);
    }

    /// Das Sync-Bit muss gesetzt sein, sonst verwirft `paket_parsen`
    /// zu Recht.
    #[test_case]
    fn test_maus_kopf_hat_sync_bit() {
        let b = maus_bytes(&[0, 0, 0]).unwrap();
        assert_ne!(b[0] & 0b0000_1000, 0);
    }

    #[test_case]
    fn test_maus_kurzer_report() {
        assert!(maus_bytes(&[0, 0]).is_none());
        assert!(maus_bytes(&[]).is_none());
        // Drei Byte reichen (ohne Rad).
        assert!(maus_bytes(&[0, 0, 0]).is_some());
    }

    // -------------------------------------------------------------------
    // GERAETE-ERKENNUNG
    // -------------------------------------------------------------------

    #[test_case]
    fn test_art_erkennen() {
        assert_eq!(art_von(3, 1, 1), Some(HidArt::Tastatur));
        assert_eq!(art_von(3, 1, 2), Some(HidArt::Maus));
        // OHNE Boot-Subclass lehnen wir ab — das Format waere unbekannt.
        assert_eq!(art_von(3, 0, 1), None);
        // Andere Klassen gehen uns nichts an.
        assert_eq!(art_von(8, 1, 1), None);
    }
}
