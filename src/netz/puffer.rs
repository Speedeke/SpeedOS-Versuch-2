// netz/puffer.rs — Die Byte-Puffer-Abstraktion für RX/TX
//
// Netzwerk-Protokolle sind BYTE-GENAU spezifiziert: Ethernet, ARP, IPv4
// und alle folgenden Schichten lesen und schreiben Felder fester Länge
// an festen Positionen, meist in NETZWERK-Byte-Reihenfolge (Big-Endian,
// "das höchstwertige Byte zuerst"). Statt in jedem Modul mit rohen
// Slice-Indizes (`frame[12]`, `u16::from_be_bytes([frame[12], frame[13]])`)
// zu hantieren — fehleranfällig und ohne Grenzprüfung — kapseln wir das
// hier EINMAL:
//
//   * `Leser`     — liest sequentiell aus einem &[u8] mit GRENZPRÜFUNG.
//                   Jede Lese-Methode liefert `Option`: ist der Puffer zu
//                   kurz, kommt `None` statt einer Panik (ein kaputtes
//                   Frame darf uns NIE zum Absturz bringen).
//   * `Schreiber` — hängt Felder an einen wachsenden `Vec<u8>` an; am Ende
//                   liefert `fertig()` das gebaute Frame.
//
// Beide sind BEWUSST geräte- und protokoll-unabhängig (die Bestandsaufnahme
// in docs/serie5-netzwerk.md nennt diese Puffer-Naht als eigenen Baustein,
// wiederverwendbar für RX-Parsing, TX-Bau und später Socket-Puffer).

use alloc::vec::Vec;

/// Liest Felder sequentiell aus einem Byte-Slice — mit Grenzprüfung.
/// Der Cursor `pos` wandert bei jedem erfolgreichen Lesen vorwärts.
pub struct Leser<'a> {
    daten: &'a [u8],
    pos: usize,
}

impl<'a> Leser<'a> {
    /// Legt einen Leser über die gegebenen Daten (Cursor am Anfang).
    pub fn neu(daten: &'a [u8]) -> Leser<'a> {
        Leser { daten, pos: 0 }
    }

    /// Wie viele Bytes ab dem Cursor noch übrig sind.
    pub fn rest(&self) -> usize {
        self.daten.len() - self.pos
    }

    /// Liest ein einzelnes Byte (u8). None, wenn nichts mehr da ist.
    pub fn u8(&mut self) -> Option<u8> {
        let b = *self.daten.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// Liest ein u16 in Netzwerk-Byte-Reihenfolge (Big-Endian).
    pub fn u16_be(&mut self) -> Option<u16> {
        let feld = self.feld::<2>()?;
        Some(u16::from_be_bytes(feld))
    }

    /// Liest ein u32 in Netzwerk-Byte-Reihenfolge (Big-Endian).
    pub fn u32_be(&mut self) -> Option<u32> {
        let feld = self.feld::<4>()?;
        Some(u32::from_be_bytes(feld))
    }

    /// Liest `n` Bytes als Slice (ohne zu kopieren). None, wenn zu wenige
    /// Bytes übrig sind.
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let ende = self.pos.checked_add(n)?;
        let ausschnitt = self.daten.get(self.pos..ende)?;
        self.pos = ende;
        Some(ausschnitt)
    }

    /// Liest ein Feld FESTER Länge N als kopiertes Array — praktisch für
    /// MAC-Adressen (`feld::<6>()`) und IPv4-Adressen (`feld::<4>()`).
    pub fn feld<const N: usize>(&mut self) -> Option<[u8; N]> {
        let ausschnitt = self.bytes(N)?;
        let mut ergebnis = [0u8; N];
        ergebnis.copy_from_slice(ausschnitt);
        Some(ergebnis)
    }
}

/// Baut ein Frame, indem Felder an einen wachsenden Puffer angehängt
/// werden. Zahlen werden in Netzwerk-Byte-Reihenfolge (Big-Endian)
/// geschrieben — dieselbe Reihenfolge, die `Leser` liest.
pub struct Schreiber {
    puffer: Vec<u8>,
}

impl Schreiber {
    /// Ein leerer Schreiber.
    pub fn neu() -> Schreiber {
        Schreiber { puffer: Vec::new() }
    }

    /// Ein Schreiber mit vorab reservierter Kapazität (spart Umkopieren,
    /// wenn die Endgröße bekannt ist — z. B. 28 Byte für ARP).
    pub fn mit_kapazitaet(bytes: usize) -> Schreiber {
        Schreiber {
            puffer: Vec::with_capacity(bytes),
        }
    }

    /// Hängt ein Byte an.
    pub fn u8(&mut self, wert: u8) {
        self.puffer.push(wert);
    }

    /// Hängt ein u16 in Netzwerk-Byte-Reihenfolge an.
    pub fn u16_be(&mut self, wert: u16) {
        self.puffer.extend_from_slice(&wert.to_be_bytes());
    }

    /// Hängt ein u32 in Netzwerk-Byte-Reihenfolge an.
    pub fn u32_be(&mut self, wert: u32) {
        self.puffer.extend_from_slice(&wert.to_be_bytes());
    }

    /// Hängt rohe Bytes an (MAC, IP, Nutzlast).
    pub fn bytes(&mut self, daten: &[u8]) {
        self.puffer.extend_from_slice(daten);
    }

    /// Aktuelle Länge des gebauten Puffers.
    pub fn laenge(&self) -> usize {
        self.puffer.len()
    }

    /// Gibt das fertig gebaute Frame heraus (verbraucht den Schreiber).
    pub fn fertig(self) -> Vec<u8> {
        self.puffer
    }
}

impl Default for Schreiber {
    fn default() -> Self {
        Schreiber::neu()
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Schreiben und Zurücklesen ergibt exakt dieselben Werte, in der
    /// richtigen Byte-Reihenfolge — der Grundvertrag der beiden Typen.
    #[test_case]
    fn test_puffer_roundtrip() {
        let mut s = Schreiber::neu();
        s.u8(0xAB);
        s.u16_be(0x1234);
        s.u32_be(0xDEAD_BEEF);
        s.bytes(&[1, 2, 3, 4, 5, 6]);
        let daten = s.fertig();

        // Big-Endian: das höchstwertige Byte steht ZUERST im Speicher.
        assert_eq!(daten[0], 0xAB);
        assert_eq!(&daten[1..3], &[0x12, 0x34]);
        assert_eq!(&daten[3..7], &[0xDE, 0xAD, 0xBE, 0xEF]);

        let mut l = Leser::neu(&daten);
        assert_eq!(l.u8(), Some(0xAB));
        assert_eq!(l.u16_be(), Some(0x1234));
        assert_eq!(l.u32_be(), Some(0xDEAD_BEEF));
        assert_eq!(l.feld::<6>(), Some([1, 2, 3, 4, 5, 6]));
        // Alles gelesen — nichts mehr übrig.
        assert_eq!(l.rest(), 0);
        assert_eq!(l.u8(), None);
    }

    /// Ein zu kurzer Puffer liefert None statt zu panicken — genau das
    /// schützt uns vor kaputten/böswilligen Frames.
    #[test_case]
    fn test_leser_grenzen() {
        let daten = [0x00, 0x01, 0x02];
        let mut l = Leser::neu(&daten);
        // u32 braucht 4 Bytes, wir haben nur 3: None, Cursor unbewegt.
        assert_eq!(l.u32_be(), None);
        assert_eq!(l.rest(), 3);
        // feld::<4>() ebenso.
        assert_eq!(l.feld::<4>(), None);
        // bytes(4) auch — checked_add + get schützen vor Überlauf.
        assert_eq!(l.bytes(4), None);
        // Was passt, geht: erst u8, dann u16.
        assert_eq!(l.u8(), Some(0x00));
        assert_eq!(l.u16_be(), Some(0x0102));
        assert_eq!(l.rest(), 0);
    }
}
