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
// Ringpuffer — der Byte-Ring für Stream-Puffer (TCP Sende-/Empfangspuffer)
// ---------------------------------------------------------------------------
//
// Ein Ringpuffer (zirkulärer Puffer) fester Kapazität: Bytes werden hinten
// angehängt (`schreiben`) und vorne entnommen (`lesen`), die Indizes laufen
// am Ende auf 0 zurück ("wickeln"). Das ist die natürliche Datenstruktur für
// einen BYTESTROM mit begrenztem Fenster — genau, was TCP für seine Sende-
// und Empfangspuffer braucht:
//   * SENDEPUFFER: Die App schreibt Bytes hinein; sie bleiben, bis der Peer
//     sie bestätigt (ACK). `spitzen` liest sie zum (Neu-)Senden OHNE sie zu
//     entfernen, `verwerfen` gibt die bestätigten vorne frei.
//   * EMPFANGSPUFFER: Ankommende, in-Order-Daten landen hier; die App holt
//     sie mit `lesen` ab. Der freie Platz (`frei`) ist unser Empfangsfenster.
//
// PUFFER-OWNERSHIP: Der Ringpuffer BESITZT seinen Speicher (ein Vec fester
// Länge). Er kopiert bei jeder Operation (kein Aliasing nach außen) — die
// Grenze ist bewusst copy-in/copy-out, damit später eine Kernel/User-Grenze
// (Serie 6) sauber dazwischen passt.

/// Ein Byte-Ringpuffer fester Kapazität.
pub struct Ringpuffer {
    daten: Vec<u8>,
    /// Index des ersten belegten Bytes (Lese-Position).
    kopf: usize,
    /// Anzahl belegter Bytes.
    fuell: usize,
}

impl Ringpuffer {
    /// Legt einen Ringpuffer mit fester Kapazität an (Kapazität >= 1).
    pub fn neu(kapazitaet: usize) -> Ringpuffer {
        Ringpuffer {
            daten: alloc::vec![0u8; kapazitaet.max(1)],
            kopf: 0,
            fuell: 0,
        }
    }

    /// Die feste Gesamtkapazität.
    pub fn kapazitaet(&self) -> usize {
        self.daten.len()
    }

    /// Wie viele Bytes gerade belegt sind.
    pub fn len(&self) -> usize {
        self.fuell
    }

    /// Ist der Puffer leer?
    pub fn is_empty(&self) -> bool {
        self.fuell == 0
    }

    /// Wie viel freier Platz übrig ist (= das Empfangsfenster).
    pub fn frei(&self) -> usize {
        self.daten.len() - self.fuell
    }

    /// Hängt so viele Bytes wie möglich hinten an (bis `frei()`). Liefert
    /// die tatsächlich geschriebene Anzahl.
    pub fn schreiben(&mut self, daten: &[u8]) -> usize {
        let n = daten.len().min(self.frei());
        let kap = self.daten.len();
        let mut schreib = (self.kopf + self.fuell) % kap; // Schreib-Position
        for &b in &daten[..n] {
            self.daten[schreib] = b;
            schreib = (schreib + 1) % kap;
        }
        self.fuell += n;
        n
    }

    /// Kopiert bis zu `ziel.len()` Bytes ab `offset` (relativ zum Kopf) OHNE
    /// sie zu entfernen. Für das (Neu-)Senden unbestätigter Sende-Daten.
    /// Liefert die kopierte Anzahl.
    pub fn spitzen(&self, offset: usize, ziel: &mut [u8]) -> usize {
        if offset >= self.fuell {
            return 0;
        }
        let verfuegbar = self.fuell - offset;
        let n = ziel.len().min(verfuegbar);
        let kap = self.daten.len();
        let mut lese = (self.kopf + offset) % kap;
        for byte in ziel.iter_mut().take(n) {
            *byte = self.daten[lese];
            lese = (lese + 1) % kap;
        }
        n
    }

    /// Entnimmt bis zu `ziel.len()` Bytes vorne (kopieren + entfernen).
    /// Liefert die gelesene Anzahl.
    pub fn lesen(&mut self, ziel: &mut [u8]) -> usize {
        let n = self.spitzen(0, ziel);
        self.verwerfen(n);
        n
    }

    /// Verwirft `n` Bytes vorne (z. B. nach einer Bestätigung). Liefert die
    /// tatsächlich verworfene Anzahl.
    pub fn verwerfen(&mut self, n: usize) -> usize {
        let n = n.min(self.fuell);
        self.kopf = (self.kopf + n) % self.daten.len();
        self.fuell -= n;
        n
    }

    /// Leert den Puffer vollständig.
    pub fn leeren(&mut self) {
        self.kopf = 0;
        self.fuell = 0;
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

    /// Der Ringpuffer: schreiben/lesen, freier Platz, spitzen (ohne
    /// Entnehmen) und verwerfen — inklusive WRAPAROUND über die
    /// Kapazitätsgrenze.
    #[test_case]
    fn test_ringpuffer() {
        let mut r = Ringpuffer::neu(8);
        assert_eq!(r.kapazitaet(), 8);
        assert_eq!(r.frei(), 8);
        assert!(r.is_empty());

        // Mehr schreiben als Platz: nur `frei()` viele werden angenommen.
        assert_eq!(r.schreiben(b"ABCDEFGHIJ"), 8);
        assert_eq!(r.len(), 8);
        assert_eq!(r.frei(), 0);
        assert_eq!(r.schreiben(b"X"), 0); // voll

        // spitzen liest OHNE zu entfernen (für Retransmit).
        let mut sicht = [0u8; 3];
        assert_eq!(r.spitzen(0, &mut sicht), 3);
        assert_eq!(&sicht, b"ABC");
        assert_eq!(r.spitzen(5, &mut sicht), 3);
        assert_eq!(&sicht, b"FGH");
        assert_eq!(r.len(), 8, "spitzen entfernt nichts");

        // verwerfen gibt vorne frei (wie ein ACK).
        assert_eq!(r.verwerfen(5), 5);
        assert_eq!(r.len(), 3); // "FGH" bleibt

        // Jetzt über die Grenze schreiben (Wraparound): Platz ist wieder 5.
        assert_eq!(r.frei(), 5);
        assert_eq!(r.schreiben(b"12345"), 5);
        // Auslesen muss die richtige Reihenfolge über den Wrap liefern.
        let mut raus = [0u8; 8];
        assert_eq!(r.lesen(&mut raus), 8);
        assert_eq!(&raus, b"FGH12345");
        assert!(r.is_empty());
    }
}
