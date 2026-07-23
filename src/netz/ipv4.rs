// netz/ipv4.rs — Die IPv4-Schicht (Schicht 3, Vermittlungsschicht)
//
// IP (Internet Protocol) ist die Schicht, die Pakete über Netzgrenzen
// hinweg adressiert — mit den vertrauten 4-Byte-Adressen (10.0.2.15). Ein
// IPv4-Paket beginnt mit einem 20-Byte-Kopf (ohne Optionen):
//
//   Version/IHL(1) DSCP/ECN(1) Gesamtlaenge(2) Ident(2) Flags/Fragment(2)
//   TTL(1) Protokoll(1) Kopf-Pruefsumme(2) Quell-IP(4) Ziel-IP(4)
//
// Wichtige Felder:
//   * Version = 4, IHL = Kopflänge in 32-Bit-Worten (5 = 20 Byte).
//   * TTL (Time To Live) zählt jeder Router herunter; bei 0 wird verworfen.
//   * Protokoll sagt, was in der Nutzlast steckt: 1 = ICMP, 6 = TCP,
//     17 = UDP — danach DISPATCHEN wir an die obere Schicht.
//   * Die KOPF-PRÜFSUMME schützt NUR den Kopf (die Internet-Checksumme,
//     RFC 1071 — unten als reine, getestete Funktion).
//
// FRAGMENTIERUNG: Ist ein Paket größer als die maximale Rahmengröße, zerlegt
// IP es in Fragmente (MF-Bit gesetzt oder Fragment-Offset != 0). Das
// Wieder-Zusammensetzen (Reassemblierung) ist echter Aufwand — und für
// unseren Zweck (ICMP/kleine UDP/DNS-Pakete) UNNÖTIG: Wir ERKENNEN
// Fragmente sauber und VERWERFEN sie (mit Log), statt sie halb zu
// verarbeiten. Ausgehend fragmentieren wir nie (unsere Pakete sind klein).

use super::ethernet::{self, Mac, ETHERTYPE_IPV4};
use super::geraet::NetzFehler;
use super::{arp, Ipv4};
use crate::serial_println;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// Protokoll-Nummer: ICMP (Ping, Fehlermeldungen).
pub const PROTO_ICMP: u8 = 1;
/// Protokoll-Nummer: TCP (kommt später).
pub const PROTO_TCP: u8 = 6;
/// Protokoll-Nummer: UDP (kommt später).
pub const PROTO_UDP: u8 = 17;

/// Version 4, IHL 5 (20-Byte-Kopf ohne Optionen) — das erste Byte.
const VERSION_IHL: u8 = 0x45;
/// Standard-TTL für ausgehende Pakete (wie Linux).
const STANDARD_TTL: u8 = 64;
/// Kopflänge ohne Optionen.
pub const KOPF_LEN: usize = 20;

// ---------------------------------------------------------------------------
// Die Internet-Checksumme (RFC 1071) — reine, unit-getestete Funktion
// ---------------------------------------------------------------------------

/// Berechnet die Internet-Checksumme über `daten`: die 16-Bit-Worte
/// (Big-Endian) aufsummieren, die Überträge in die unteren 16 Bit
/// zurückfalten und das Einer-Komplement bilden. Bei UNGERADER Länge zählt
/// das letzte Byte als High-Byte eines mit 0 aufgefüllten Wortes.
///
/// Zwei Eigenschaften machen sie so praktisch:
///   * Über einen Kopf MIT korrekt eingesetzter Prüfsumme ergibt sie 0 —
///     so PRÜFT man ein empfangenes Paket.
///   * Mit Prüfsummen-Feld = 0 liefert sie den einzusetzenden Wert.
pub fn internet_checksumme(daten: &[u8]) -> u16 {
    let mut summe: u32 = 0;
    let mut i = 0;
    while i + 1 < daten.len() {
        summe += u16::from_be_bytes([daten[i], daten[i + 1]]) as u32;
        i += 2;
    }
    // Ungerade Länge: letztes Byte als High-Byte.
    if i < daten.len() {
        summe += (daten[i] as u32) << 8;
    }
    // Überträge zurückfalten, bis nur noch 16 Bit übrig sind.
    while (summe >> 16) != 0 {
        summe = (summe & 0xFFFF) + (summe >> 16);
    }
    !(summe as u16)
}

// ---------------------------------------------------------------------------
// Kopf: parsen und bauen
// ---------------------------------------------------------------------------

/// Der geparste IPv4-Kopf (die Nutzlast reicht `parse` separat zurück).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Kopf {
    pub protokoll: u8,
    pub quelle: Ipv4,
    pub ziel: Ipv4,
    pub ttl: u8,
    /// MF-Flag: "es folgen weitere Fragmente".
    pub mehr_fragmente: bool,
    /// Fragment-Offset (in 8-Byte-Einheiten); != 0 heißt "kein erstes
    /// Fragment".
    pub fragment_offset: u16,
}

impl Ipv4Kopf {
    /// Ist dieses Paket ein Fragment? (MF gesetzt ODER Offset != 0.)
    pub fn ist_fragment(&self) -> bool {
        self.mehr_fragmente || self.fragment_offset != 0
    }
}

/// Zerlegt ein IPv4-Paket in (Kopf, Nutzlast). None bei: zu kurz, falsche
/// Version, IHL < 5, oder FALSCHER Kopf-Prüfsumme (kaputte Pakete werden
/// verworfen, nie verarbeitet). Fragmente werden hier NICHT abgewiesen —
/// das entscheidet `verarbeiten` (damit es den Verwurf loggen kann).
pub fn parse(paket: &[u8]) -> Option<(Ipv4Kopf, &[u8])> {
    if paket.len() < KOPF_LEN {
        return None;
    }
    let version = paket[0] >> 4;
    let ihl = (paket[0] & 0x0F) as usize;
    if version != 4 || ihl < 5 {
        return None;
    }
    let kopf_len = ihl * 4;
    if paket.len() < kopf_len {
        return None;
    }
    // Die Kopf-Prüfsumme muss stimmen (über den GANZEN Kopf inkl. Optionen).
    if internet_checksumme(&paket[..kopf_len]) != 0 {
        return None;
    }
    let gesamt_laenge = u16::from_be_bytes([paket[2], paket[3]]) as usize;
    let flags_frag = u16::from_be_bytes([paket[6], paket[7]]);
    let ttl = paket[8];
    let protokoll = paket[9];
    let quelle = Ipv4([paket[12], paket[13], paket[14], paket[15]]);
    let ziel = Ipv4([paket[16], paket[17], paket[18], paket[19]]);
    let mehr_fragmente = flags_frag & 0x2000 != 0;
    let fragment_offset = flags_frag & 0x1FFF;

    // Nutzlast: von kopf_len bis Gesamtlänge — aber nie über das reale
    // Frame-Ende hinaus (Ethernet kann kurze Frames auffüllen -> die
    // Gesamtlänge ist maßgeblich, wenn sie kleiner ist).
    let ende = gesamt_laenge.clamp(kopf_len, paket.len());
    let nutzlast = &paket[kopf_len..ende];
    Some((
        Ipv4Kopf {
            protokoll,
            quelle,
            ziel,
            ttl,
            mehr_fragmente,
            fragment_offset,
        },
        nutzlast,
    ))
}

/// Baut ein IPv4-Paket (20-Byte-Kopf, keine Optionen, DF gesetzt = wir
/// fragmentieren nicht) mit korrekt berechneter Kopf-Prüfsumme.
pub fn bauen(quelle: Ipv4, ziel: Ipv4, protokoll: u8, nutzlast: &[u8]) -> Vec<u8> {
    use super::puffer::Schreiber;
    let gesamt = (KOPF_LEN + nutzlast.len()) as u16;
    let mut s = Schreiber::mit_kapazitaet(KOPF_LEN + nutzlast.len());
    s.u8(VERSION_IHL);
    s.u8(0); // DSCP/ECN
    s.u16_be(gesamt);
    s.u16_be(0); // Identification (0 — wir fragmentieren nie)
    s.u16_be(0x4000); // Flags: DF (Don't Fragment), Offset 0
    s.u8(STANDARD_TTL);
    s.u8(protokoll);
    s.u16_be(0); // Prüfsummen-Platzhalter (Bytes 10..12)
    s.bytes(&quelle.oktette());
    s.bytes(&ziel.oktette());
    s.bytes(nutzlast);
    let mut paket = s.fertig();
    // Prüfsumme über den 20-Byte-Kopf berechnen und einsetzen.
    let pruef = internet_checksumme(&paket[..KOPF_LEN]);
    paket[10..12].copy_from_slice(&pruef.to_be_bytes());
    paket
}

// ---------------------------------------------------------------------------
// Empfang: dispatchen nach Protokoll
// ---------------------------------------------------------------------------

/// Verarbeitet ein empfangenes IPv4-Paket (aus der Ethernet-Nutzlast). Wird
/// vom netz_task-Dispatch für EtherType 0x0800 gerufen.
pub fn verarbeiten(paket: &[u8]) {
    let (kopf, nutzlast) = match parse(paket) {
        Some(x) => x,
        None => return, // zu kurz / falsche Version / Prüfsumme kaputt
    };
    // Fragmente werden SAUBER erkannt und verworfen (keine Reassemblierung).
    if kopf.ist_fragment() {
        serial_println!(
            "[ipv4] Fragment von {} an {} verworfen (keine Reassemblierung)",
            kopf.quelle,
            kopf.ziel
        );
        return;
    }
    // Ist das Paket an UNS gerichtet? (Broadcast/Multicast später.)
    match super::unsere_ip() {
        Some(unsere_ip) if kopf.ziel == unsere_ip => {}
        _ => return,
    }
    // Nach Protokoll an die obere Schicht dispatchen (UDP/TCP folgen).
    if kopf.protokoll == PROTO_ICMP {
        super::icmp::verarbeiten(kopf.quelle, kopf.ttl, nutzlast);
    }
}

// ---------------------------------------------------------------------------
// Senden: Ziel-MAC per ARP auflösen (bei Miss zurückstellen)
// ---------------------------------------------------------------------------

/// Bestimmt den Next-Hop für ein Ziel: liegt es im EIGENEN Subnetz, geht
/// es direkt dorthin; sonst über das Gateway. None, wenn keine IP
/// konfiguriert ist.
pub fn next_hop(ziel: Ipv4) -> Option<Ipv4> {
    let k = super::konfig();
    if !k.gesetzt {
        return None;
    }
    if im_selben_subnetz(k.ip, k.maske, ziel) {
        Some(ziel)
    } else {
        Some(k.gateway)
    }
}

/// Liegen `a` und `b` im selben Subnetz (nach `maske`)?
fn im_selben_subnetz(a: Ipv4, maske: Ipv4, b: Ipv4) -> bool {
    (0..4).all(|i| (a.0[i] & maske.0[i]) == (b.0[i] & maske.0[i]))
}

/// Sendet ein IPv4-Paket an `ziel`. Löst die Next-Hop-MAC über den
/// ARP-Cache auf; bei einem MISS wird das Paket kurz ZURÜCKGESTELLT und ein
/// ARP-Request geschickt — trifft die Antwort ein (nächster
/// `rx_verarbeiten`-Durchlauf), liefert `ausstehend_ausliefern` es aus.
pub fn senden(ziel: Ipv4, protokoll: u8, nutzlast: &[u8]) -> Result<(), NetzFehler> {
    let k = super::konfig();
    if !k.gesetzt {
        return Err(NetzFehler::NichtKonfiguriert);
    }
    let unsere_mac = super::mac().ok_or(NetzFehler::KeinGeraet)?;
    let next_hop = if im_selben_subnetz(k.ip, k.maske, ziel) {
        ziel
    } else {
        k.gateway
    };
    let ip_paket = bauen(k.ip, ziel, protokoll, nutzlast);

    match arp::cache_suchen(next_hop) {
        Some(mac) => {
            let frame = ethernet::rahmen_bauen(mac, unsere_mac, ETHERTYPE_IPV4, &ip_paket);
            super::sende_frame(&frame)
        }
        None => {
            // ARP-Miss: Paket zurückstellen und die MAC anfragen.
            ausstehend_einreihen(next_hop, ip_paket);
            let _ = arp::anfrage_senden(next_hop); // Best-Effort
            Ok(())
        }
    }
}

/// Ein Paket, das auf seine Next-Hop-MAC (ARP) wartet.
struct Ausstehend {
    next_hop: Ipv4,
    ip_paket: Vec<u8>,
    zeit_ms: u64,
}

/// Wie lange ein zurückgestelltes Paket höchstens auf ARP wartet.
const AUSSTEHEND_TTL_MS: u64 = 3000;
/// So viele Pakete stellen wir höchstens zurück (dann fällt das älteste weg).
const AUSSTEHEND_MAX: usize = 16;

static AUSSTEHEND: Mutex<Vec<Ausstehend>> = Mutex::new(Vec::new());

/// Reiht ein Paket in die Warteschlange ein (ältestes verdrängen, wenn voll).
fn ausstehend_einreihen(next_hop: Ipv4, ip_paket: Vec<u8>) {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        let mut q = AUSSTEHEND.lock();
        if q.len() >= AUSSTEHEND_MAX {
            q.remove(0);
        }
        q.push(Ausstehend {
            next_hop,
            ip_paket,
            zeit_ms: jetzt,
        });
    });
}

/// Liefert zurückgestellte Pakete aus, deren Next-Hop-MAC jetzt bekannt ist,
/// und verwirft abgelaufene. Der netz_task ruft es nach jedem Dispatch (so
/// geht ein Paket raus, sobald seine ARP-Antwort da ist).
pub fn ausstehend_ausliefern() {
    let unsere_mac = match super::mac() {
        Some(m) => m,
        None => return,
    };
    let jetzt = crate::zeit::ms_seit_boot();
    // Erst UNTER dem Lock entscheiden, was rausgeht; danach OHNE Lock senden.
    let zu_senden: Vec<(Mac, Vec<u8>)> = without_interrupts(|| {
        let mut q = AUSSTEHEND.lock();
        let mut raus = Vec::new();
        q.retain(|a| {
            if jetzt.saturating_sub(a.zeit_ms) > AUSSTEHEND_TTL_MS {
                return false; // abgelaufen: verwerfen
            }
            match arp::cache_suchen(a.next_hop) {
                Some(mac) => {
                    raus.push((mac, a.ip_paket.clone()));
                    false // ausliefern -> aus der Queue nehmen
                }
                None => true, // weiter warten
            }
        });
        raus
    });
    for (mac, ip_paket) in zu_senden {
        let frame = ethernet::rahmen_bauen(mac, unsere_mac, ETHERTYPE_IPV4, &ip_paket);
        let _ = super::sende_frame(&frame);
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Internet-Checksumme gegen einen BEKANNTEN Vektor (der klassische
    /// IPv4-Kopf aus der Wikipedia-Erklärung): mit Prüfsummen-Feld = 0 muss
    /// 0xB861 herauskommen, und mit eingesetzter Prüfsumme wieder 0.
    #[test_case]
    fn test_internet_checksumme() {
        let kopf_ohne_pruef: [u8; 20] = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(internet_checksumme(&kopf_ohne_pruef), 0xB861);

        // Prüfsumme einsetzen -> die Summe über den ganzen Kopf ergibt 0.
        let mut kopf_mit_pruef = kopf_ohne_pruef;
        kopf_mit_pruef[10..12].copy_from_slice(&0xB861u16.to_be_bytes());
        assert_eq!(internet_checksumme(&kopf_mit_pruef), 0x0000);

        // Ungerade Länge darf nicht panicken (letztes Byte als High-Byte).
        let _ = internet_checksumme(&[0x01, 0x02, 0x03]);
    }

    /// Bauen und Parsen sind invers; das gebaute Paket hat eine GÜLTIGE
    /// Prüfsumme (parse würde sie sonst abweisen).
    #[test_case]
    fn test_ipv4_bau_und_parse() {
        let quelle = Ipv4([10, 0, 2, 15]);
        let ziel = Ipv4([10, 0, 2, 2]);
        let nutzlast = [0xDE, 0xAD, 0xBE, 0xEF, 0x11, 0x22];

        let paket = bauen(quelle, ziel, PROTO_ICMP, &nutzlast);
        assert_eq!(paket.len(), KOPF_LEN + nutzlast.len());
        assert_eq!(paket[0], VERSION_IHL);

        let (kopf, rest) = parse(&paket).expect("gebautes Paket muss parsen");
        assert_eq!(kopf.quelle, quelle);
        assert_eq!(kopf.ziel, ziel);
        assert_eq!(kopf.protokoll, PROTO_ICMP);
        assert_eq!(kopf.ttl, STANDARD_TTL);
        assert!(!kopf.ist_fragment());
        assert_eq!(rest, &nutzlast);

        // Ein verbogenes Prüfsummen-Byte lässt parse scheitern.
        let mut kaputt = paket.clone();
        kaputt[10] ^= 0xFF;
        assert!(parse(&kaputt).is_none(), "falsche Pruefsumme muss abgewiesen werden");
    }

    /// Ein Fragment (MF-Bit gesetzt) wird als solches ERKANNT.
    #[test_case]
    fn test_ipv4_fragment_erkennung() {
        let mut paket = bauen(Ipv4([10, 0, 0, 1]), Ipv4([10, 0, 0, 2]), PROTO_UDP, &[0u8; 8]);
        // Flags/Fragment auf MF (0x2000) setzen und Prüfsumme neu berechnen.
        paket[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        paket[10..12].copy_from_slice(&[0, 0]);
        let pruef = internet_checksumme(&paket[..KOPF_LEN]);
        paket[10..12].copy_from_slice(&pruef.to_be_bytes());

        let (kopf, _) = parse(&paket).expect("gueltiger Kopf trotz Fragment");
        assert!(kopf.ist_fragment(), "MF-Bit -> Fragment");
        assert!(kopf.mehr_fragmente);

        // Ein Nicht-Fragment ist keins.
        let ganz = bauen(Ipv4([10, 0, 0, 1]), Ipv4([10, 0, 0, 2]), PROTO_UDP, &[0u8; 8]);
        assert!(!parse(&ganz).unwrap().0.ist_fragment());
    }

    /// Next-Hop: eigenes Subnetz -> direkt, fremdes -> Gateway.
    #[test_case]
    fn test_ipv4_next_hop() {
        // im selben /24 wie 10.0.2.x?
        assert!(im_selben_subnetz(
            Ipv4([10, 0, 2, 15]),
            Ipv4([255, 255, 255, 0]),
            Ipv4([10, 0, 2, 99])
        ));
        assert!(!im_selben_subnetz(
            Ipv4([10, 0, 2, 15]),
            Ipv4([255, 255, 255, 0]),
            Ipv4([8, 8, 8, 8])
        ));
    }
}
