// rtc.rs — Die Echtzeituhr (RTC) im CMOS-Baustein
//
// Der Ur-Chip MC146818 (heute Teil des Chipsatzes) führt Datum und
// Uhrzeit batteriegepuffert weiter, auch wenn der PC aus ist. QEMU
// speist ihn aus der Host-Uhr (unser Runner: -rtc base=localtime).
//
// SpeedOS liest die RTC GENAU EINMAL beim Boot (zeit::init) und
// zählt danach mit dem TSC weiter — deshalb braucht es hier weder
// Interrupts noch einen Treiber-Task, nur ein sauberes Einmal-Lesen.
//
// Zwei klassische Fallen, beide behandelt:
//   1. Update-in-Progress: Während die Uhr intern weiterschaltet
//      (Bit 7 in Status-Register A), liefern die Register Müll —
//      warten, und zusätzlich so lange doppelt lesen, bis zwei
//      Lesungen identisch sind.
//   2. BCD-Format: Standardmäßig kommen die Werte binär-codiert
//      dezimal (0x59 = 59) — Status-Register B, Bit 2 verrät es.
//      Bit 1 verrät den 12-Stunden-Modus (PM = Bit 7 der Stunde).

use crate::zeit::DatumUhrzeit;
use x86_64::instructions::port::Port;

/// Die rohen CMOS-Register EINER Lesung (plus Status B).
#[derive(Clone, Copy, PartialEq, Eq)]
struct RohZeit {
    sekunde: u8,
    minute: u8,
    stunde: u8,
    tag: u8,
    monat: u8,
    jahr: u8,
    status_b: u8,
}

/// Liest ein CMOS-Register über das Index/Daten-Portpaar 0x70/0x71.
fn register_lesen(index: u8) -> u8 {
    let mut index_port: Port<u8> = Port::new(0x70);
    let mut daten_port: Port<u8> = Port::new(0x71);
    // unsafe (Port-I/O): 0x70 wählt das CMOS-Register, 0x71 liest es.
    // Bit 7 von 0x70 steuert die NMI-Maske — wir schreiben es als 0
    // und lassen NMIs damit an (SpeedOS nutzt keine NMIs, der
    // Standard-Zustand bleibt erhalten). Nur Lesezugriffe: Die Uhr
    // und das CMOS bleiben unverändert.
    unsafe {
        index_port.write(index & 0x7f);
        daten_port.read()
    }
}

/// Läuft gerade das interne Uhr-Update? (Status A, Bit 7)
fn update_laeuft() -> bool {
    register_lesen(0x0a) & 0x80 != 0
}

fn roh_lesen() -> RohZeit {
    RohZeit {
        sekunde: register_lesen(0x00),
        minute: register_lesen(0x02),
        stunde: register_lesen(0x04),
        tag: register_lesen(0x07),
        monat: register_lesen(0x08),
        jahr: register_lesen(0x09),
        status_b: register_lesen(0x0b),
    }
}

/// BCD -> binär: 0x59 -> 59. Reine Funktion, unit-getestet.
pub fn bcd_nach_binaer(wert: u8) -> u8 {
    (wert >> 4) * 10 + (wert & 0x0f)
}

/// Wandelt eine stabile Roh-Lesung in ein Kalenderdatum um —
/// reine Funktion (BCD, 12h-Modus), unit-getestet.
fn konvertieren(roh: RohZeit) -> DatumUhrzeit {
    let bcd = roh.status_b & 0b0000_0100 == 0; // Bit 2 = "binär"
    let umwandeln = |wert: u8| -> u64 {
        if bcd {
            bcd_nach_binaer(wert) as u64
        } else {
            wert as u64
        }
    };

    // Stunde: Im 12-Stunden-Modus (Status B, Bit 1 = 0) markiert
    // Bit 7 den Nachmittag; 12 AM ist Mitternacht (0), 12 PM Mittag.
    let pm = roh.stunde & 0x80 != 0;
    let mut stunde = umwandeln(roh.stunde & 0x7f);
    if roh.status_b & 0b0000_0010 == 0 {
        stunde %= 12;
        if pm {
            stunde += 12;
        }
    }

    DatumUhrzeit {
        // Das Jahrhundert-Register (0x32) ist nicht überall verlässlich —
        // wir nehmen schlicht die 2000er an (gültig bis 2099).
        jahr: 2000 + umwandeln(roh.jahr),
        monat: umwandeln(roh.monat),
        tag: umwandeln(roh.tag),
        stunde,
        minute: umwandeln(roh.minute),
        sekunde: umwandeln(roh.sekunde),
    }
}

/// Liest die RTC einmal sauber aus: erst das Update-Flag abwarten
/// (mit Timeout — fehlende/kaputte RTC hängt den Boot nicht), dann
/// doppelt lesen, bis zwei Lesungen übereinstimmen.
pub fn lesen() -> Option<DatumUhrzeit> {
    for _ in 0..100_000 {
        if !update_laeuft() {
            // Stabil lesen: Ändert sich zwischen zwei Lesungen etwas
            // (Sekundenwechsel mitten im Lesen), nochmal.
            let mut vorher = roh_lesen();
            for _ in 0..10 {
                let nochmal = roh_lesen();
                if nochmal == vorher && !update_laeuft() {
                    let datum = konvertieren(nochmal);
                    // Plausibilität statt blindem Vertrauen:
                    if (1..=12).contains(&datum.monat) && (1..=31).contains(&datum.tag) {
                        return Some(datum);
                    }
                    return None;
                }
                vorher = nochmal;
            }
            return None;
        }
        core::hint::spin_loop();
    }
    None
}

// ---------------------------------------------------------------------------
// Tests — reine Konvertierungs-Logik, ohne Hardware
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_bcd_nach_binaer() {
        assert_eq!(bcd_nach_binaer(0x00), 0);
        assert_eq!(bcd_nach_binaer(0x09), 9);
        assert_eq!(bcd_nach_binaer(0x10), 10);
        assert_eq!(bcd_nach_binaer(0x59), 59);
        assert_eq!(bcd_nach_binaer(0x31), 31);
    }

    /// BCD-Lesung (Status B: 24h-Modus, BCD) wird korrekt gewandelt.
    #[test_case]
    fn test_konvertieren_bcd_24h() {
        let roh = RohZeit {
            sekunde: 0x42,
            minute: 0x07,
            stunde: 0x23,
            tag: 0x11,
            monat: 0x07,
            jahr: 0x26,
            status_b: 0b0000_0010, // 24h, BCD
        };
        let datum = konvertieren(roh);
        assert_eq!((datum.jahr, datum.monat, datum.tag), (2026, 7, 11));
        assert_eq!((datum.stunde, datum.minute, datum.sekunde), (23, 7, 42));
    }

    /// Binär-Modus und 12-Stunden-Modus: 12 AM = 0 Uhr, PM-Bit addiert.
    #[test_case]
    fn test_konvertieren_binaer_12h() {
        let mitternacht = RohZeit {
            sekunde: 5, minute: 30, stunde: 12, // 12 AM = Mitternacht
            tag: 1, monat: 1, jahr: 30,
            status_b: 0b0000_0100, // 12h-Modus, binär
        };
        assert_eq!(konvertieren(mitternacht).stunde, 0);

        let nachmittag = RohZeit {
            sekunde: 0, minute: 0, stunde: 0x80 | 3, // 3 PM
            tag: 1, monat: 1, jahr: 30,
            status_b: 0b0000_0100,
        };
        assert_eq!(konvertieren(nachmittag).stunde, 15);
    }
}
