// netz/udp.rs — UDP: Datagramme ohne Verbindung (Schicht 4)
//
// UDP (User Datagram Protocol) ist der einfache Transport: keine Verbindung,
// keine Bestätigung, keine Reihenfolge — nur ein 8-Byte-Kopf über IP:
//
//   Quell-Port(2) Ziel-Port(2) Laenge(2) Pruefsumme(2) [Daten ...]
//
// Die PORTS unterscheiden mehrere Dienste auf derselben IP: DHCP hört auf
// 67/68, DNS auf 53. Die PRÜFSUMME ist optional (0 = "nicht berechnet"),
// wird aber — anders als bei IPv4 — über einen PSEUDO-HEADER gebildet: die
// Quell-/Ziel-IP, das Protokoll und die UDP-Länge fließen mit ein (so
// erkennt der Empfänger fehlgeleitete Pakete). Dieselbe Internet-Checksumme
// wie bei IPv4, nur über Pseudo-Header + UDP-Segment.
//
// PORT-DEMUX: Wer auf einem Port lauschen will, `binden`-t ihn; ankommende
// Datagramme landen in seiner Empfangs-Queue, aus der er sie `empfangen`-t.
// Das ist bewusst die VORÜBUNG für die spätere Socket-API — Handles (Ports)
// statt roher Zeiger, explizite Puffer-Ownership (jedes Datagramm ein Vec).

use super::ipv4::{self, PROTO_UDP};
use super::puffer::{Leser, Schreiber};
use super::Ipv4;
use crate::serial_println;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// Länge des UDP-Kopfes in Byte.
pub const KOPF_LEN: usize = 8;

/// Der geparste UDP-Kopf (die Nutzlast reicht `parse` separat zurück).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpKopf {
    pub quell_port: u16,
    pub ziel_port: u16,
}

/// Zerlegt ein UDP-Segment in (Kopf, Nutzlast). None bei zu kurz oder wenn
/// das Längenfeld nicht zum Segment passt.
pub fn parse(segment: &[u8]) -> Option<(UdpKopf, &[u8])> {
    let mut l = Leser::neu(segment);
    let quell_port = l.u16_be()?;
    let ziel_port = l.u16_be()?;
    let laenge = l.u16_be()? as usize;
    let _pruef = l.u16_be()?;
    // Das Längenfeld umfasst Kopf + Daten und darf das Segment nicht
    // überschreiten (Ethernet kann kurze Frames auffüllen).
    if laenge < KOPF_LEN {
        return None;
    }
    let ende = laenge.min(segment.len());
    let nutzlast = segment.get(KOPF_LEN..ende)?;
    Some((UdpKopf { quell_port, ziel_port }, nutzlast))
}

/// Berechnet die UDP-Prüfsumme über den PSEUDO-HEADER (Quell-IP, Ziel-IP,
/// 0, Protokoll, UDP-Länge) + das UDP-Segment. Reine Funktion (nutzt die
/// getestete Internet-Checksumme):
///   * über ein Segment mit Prüfsummen-Feld = 0 -> der einzusetzende Wert,
///   * über ein Segment MIT korrekter Prüfsumme -> 0 (so prüft man RX).
pub fn checksumme(quell_ip: Ipv4, ziel_ip: Ipv4, segment: &[u8]) -> u16 {
    let mut puffer = Vec::with_capacity(12 + segment.len());
    puffer.extend_from_slice(&quell_ip.oktette());
    puffer.extend_from_slice(&ziel_ip.oktette());
    puffer.push(0);
    puffer.push(PROTO_UDP);
    puffer.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    puffer.extend_from_slice(segment);
    ipv4::internet_checksumme(&puffer)
}

/// Baut ein UDP-Segment mit korrekt berechneter Pseudo-Header-Prüfsumme.
/// (Ergibt die Prüfsumme rechnerisch 0, wird 0xFFFF gesendet — 0 bedeutet
/// bei UDP "keine Prüfsumme", das wollen wir nicht.)
pub fn bauen(quell_port: u16, ziel_port: u16, quell_ip: Ipv4, ziel_ip: Ipv4, nutzlast: &[u8]) -> Vec<u8> {
    let laenge = (KOPF_LEN + nutzlast.len()) as u16;
    let mut s = Schreiber::mit_kapazitaet(KOPF_LEN + nutzlast.len());
    s.u16_be(quell_port);
    s.u16_be(ziel_port);
    s.u16_be(laenge);
    s.u16_be(0); // Prüfsummen-Platzhalter (Bytes 6..8)
    s.bytes(nutzlast);
    let mut segment = s.fertig();
    let pruef = checksumme(quell_ip, ziel_ip, &segment);
    let pruef = if pruef == 0 { 0xFFFF } else { pruef };
    segment[6..8].copy_from_slice(&pruef.to_be_bytes());
    segment
}

/// Sendet ein UDP-Datagramm an `ziel_ip:ziel_port` von `quell_port`. Nutzt
/// unsere konfigurierte IP als Quelle (also erst nach IP-Konfiguration);
/// DHCP (noch ohne IP) sendet über `ipv4::senden_an_mac` direkt.
pub fn senden(ziel_ip: Ipv4, quell_port: u16, ziel_port: u16, nutzlast: &[u8]) -> Result<(), super::NetzFehler> {
    let k = super::konfig();
    if !k.gesetzt {
        return Err(super::NetzFehler::NichtKonfiguriert);
    }
    let segment = bauen(quell_port, ziel_port, k.ip, ziel_ip, nutzlast);
    ipv4::senden(ziel_ip, PROTO_UDP, &segment)
}

// ---------------------------------------------------------------------------
// Port-Demux: gebundene Ports mit Empfangs-Queue
// ---------------------------------------------------------------------------

/// Ein empfangenes Datagramm (Absender + Nutzdaten) — der Aufrufer besitzt
/// den Vec (klare Puffer-Ownership, Vorbild für die Socket-API).
pub struct Datagramm {
    pub quell_ip: Ipv4,
    pub quell_port: u16,
    pub daten: Vec<u8>,
}

/// Ein gebundener Port samt seiner Empfangs-Queue.
struct Gebunden {
    port: u16,
    empfangen: Vec<Datagramm>,
}

/// So viele Datagramme puffern wir pro Port (dann fällt das älteste weg).
const QUEUE_MAX: usize = 8;

/// Die gebundenen Ports (Blatt-Lock, nur aus Task-Kontext).
static PORTS: Mutex<Vec<Gebunden>> = Mutex::new(Vec::new());

/// Bindet einen Port zum Empfang (idempotent). Danach landen Datagramme an
/// diesen Port in seiner Queue.
pub fn binden(port: u16) {
    without_interrupts(|| {
        let mut ports = PORTS.lock();
        if !ports.iter().any(|g| g.port == port) {
            ports.push(Gebunden {
                port,
                empfangen: Vec::new(),
            });
        }
    });
}

/// Gibt einen Port wieder frei (verwirft seine gepufferten Datagramme).
pub fn freigeben(port: u16) {
    without_interrupts(|| PORTS.lock().retain(|g| g.port != port));
}

/// Holt das nächste empfangene Datagramm für `port` (FIFO), None wenn leer.
pub fn empfangen(port: u16) -> Option<Datagramm> {
    without_interrupts(|| {
        let mut ports = PORTS.lock();
        let g = ports.iter_mut().find(|g| g.port == port)?;
        if g.empfangen.is_empty() {
            None
        } else {
            Some(g.empfangen.remove(0))
        }
    })
}

/// Verarbeitet ein empfangenes UDP-Segment (aus der IPv4-Nutzlast). Wird von
/// `ipv4::verarbeiten` für Protokoll 17 gerufen; `quell_ip`/`ziel_ip`
/// stammen aus dem IP-Kopf (für die Pseudo-Header-Prüfung).
pub fn verarbeiten(quell_ip: Ipv4, ziel_ip: Ipv4, segment: &[u8]) {
    if segment.len() < KOPF_LEN {
        return;
    }
    // Prüfsumme 0 heißt "nicht berechnet" — nur prüfen, wenn gesetzt.
    let pruef_feld = u16::from_be_bytes([segment[6], segment[7]]);
    if pruef_feld != 0 && checksumme(quell_ip, ziel_ip, segment) != 0 {
        serial_println!("[udp] Segment mit falscher Pruefsumme verworfen");
        return;
    }
    let (kopf, nutzlast) = match parse(segment) {
        Some(x) => x,
        None => return,
    };
    // An den gebundenen Ziel-Port zustellen (unbekannter Port -> verwerfen).
    without_interrupts(|| {
        let mut ports = PORTS.lock();
        if let Some(g) = ports.iter_mut().find(|g| g.port == kopf.ziel_port) {
            if g.empfangen.len() >= QUEUE_MAX {
                g.empfangen.remove(0);
            }
            g.empfangen.push(Datagramm {
                quell_ip,
                quell_port: kopf.quell_port,
                daten: nutzlast.to_vec(),
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Bauen und Parsen sind invers; das gebaute Segment hat eine GÜLTIGE
    /// Pseudo-Header-Prüfsumme (checksumme über das fertige Segment = 0).
    #[test_case]
    fn test_udp_bau_und_parse() {
        let quelle = Ipv4([10, 0, 2, 15]);
        let ziel = Ipv4([10, 0, 2, 3]);
        let nutzlast = [0x12, 0x34, 0x56, 0x78, 0x9A];

        let segment = bauen(4000, 53, quelle, ziel, &nutzlast);
        assert_eq!(segment.len(), KOPF_LEN + nutzlast.len());
        // Längenfeld = 8 + 5 = 13:
        assert_eq!(u16::from_be_bytes([segment[4], segment[5]]), 13);

        let (kopf, rest) = parse(&segment).expect("gebautes Segment muss parsen");
        assert_eq!(kopf.quell_port, 4000);
        assert_eq!(kopf.ziel_port, 53);
        assert_eq!(rest, &nutzlast);

        // Die Pseudo-Header-Prüfsumme über das fertige Segment ergibt 0.
        assert_eq!(checksumme(quelle, ziel, &segment), 0);
        // Ein verbogenes Byte macht die Prüfsumme ungültig (!= 0).
        let mut kaputt = segment.clone();
        kaputt[8] ^= 0xFF;
        assert_ne!(checksumme(quelle, ziel, &kaputt), 0);
    }

    /// Der Port-Demux: an einen gebundenen Port zugestelltes Datagramm ist
    /// abholbar, an einen ungebundenen nicht.
    #[test_case]
    fn test_udp_port_demux() {
        let quelle = Ipv4([10, 0, 2, 3]);
        let ziel = Ipv4([10, 0, 2, 15]);
        freigeben(6789); // sauberer Start
        binden(6789);

        // Ein Segment an Port 6789 bauen und "empfangen".
        let segment = bauen(53, 6789, quelle, ziel, &[0xAA, 0xBB]);
        verarbeiten(quelle, ziel, &segment);
        let d = empfangen(6789).expect("Datagramm an gebundenem Port");
        assert_eq!(d.quell_ip, quelle);
        assert_eq!(d.quell_port, 53);
        assert_eq!(d.daten, alloc::vec![0xAA, 0xBB]);
        assert!(empfangen(6789).is_none(), "nur eines war da");

        // An einen NICHT gebundenen Port stellt nichts zu.
        let segment2 = bauen(53, 9999, quelle, ziel, &[1, 2, 3]);
        verarbeiten(quelle, ziel, &segment2);
        assert!(empfangen(9999).is_none());

        freigeben(6789);
    }
}
