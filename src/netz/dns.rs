// netz/dns.rs — DNS-Resolver: Namen zu IP-Adressen auflösen
//
// DNS (Domain Name System) übersetzt Namen ("example.com") in IP-Adressen.
// Wir fragen den per DHCP gelernten DNS-Server über UDP (Port 53) und lesen
// den A-Record (die IPv4-Adresse) aus der Antwort.
//
// Eine DNS-Nachricht: 12-Byte-Kopf (ID, Flags, Anzahlen), dann Fragen und
// Antworten. Ein NAME ist eine Folge von Labels (Längenbyte + Bytes),
// abgeschlossen mit 0. Der Clou ist die KOMPRESSION: statt einen Namen
// mehrfach auszuschreiben, verweist ein Label mit gesetzten oberen zwei
// Bits (0xC0) auf einen früheren Offset in derselben Nachricht. Der Parser
// MUSS solchen Zeigern folgen (mit Schleifen-Schutz!) — sonst findet er die
// Felder hinter dem Namen nicht.
//
// Ein kleiner CACHE (Name -> IP, mit TTL) spart wiederholte Anfragen.

use super::geraet::NetzFehler;
use super::puffer::Schreiber;
use super::{udp, Ipv4};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

const DNS_PORT: u16 = 53;
const TYP_A: u16 = 1; // A-Record (IPv4)
const KLASSE_IN: u16 = 1; // Internet
/// Wie lange wir je VERSUCH auf eine Antwort warten, bevor wir die Anfrage
/// ERNEUT senden (UDP ist unzuverlässig — Anfrage oder Antwort kann verloren
/// gehen, dann hilft nur ein zweiter Versuch).
const VERSUCH_MS: u64 = 1200;
/// So oft senden wir die Anfrage höchstens (Gesamtfrist = MAX·VERSUCH_MS).
const MAX_VERSUCHE: u32 = 3;

/// Fehler bei der Namensauflösung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsFehler {
    /// Kein DNS-Server bekannt (keine DHCP-Lease / keine Konfiguration).
    KeinDnsServer,
    /// Keine Antwort in der Frist.
    Zeitueberschreitung,
    /// Der Server hat geantwortet, aber ohne A-Record (Name unbekannt).
    NichtGefunden,
    /// Fehler beim Senden (Netz-Schicht).
    Netz(NetzFehler),
}

impl DnsFehler {
    pub fn meldung(&self) -> &'static str {
        match self {
            DnsFehler::KeinDnsServer => "kein DNS-Server bekannt (erst DHCP oder netz-ip)",
            DnsFehler::Zeitueberschreitung => "keine Antwort vom DNS-Server (Timeout)",
            DnsFehler::NichtGefunden => "Name nicht gefunden (kein A-Record)",
            DnsFehler::Netz(_) => "Fehler beim Senden der DNS-Anfrage",
        }
    }
}

// ---------------------------------------------------------------------------
// Namen kodieren und lesen (Kompression!) — der reine, testbare Kern
// ---------------------------------------------------------------------------

/// Kodiert einen Namen als DNS-Label-Folge: je Teil ein Längenbyte + die
/// Bytes, abgeschlossen mit einer 0. ("a.bc" -> 1 'a' 2 'b' 'c' 0)
fn name_kodieren(s: &mut Schreiber, name: &str) {
    for teil in name.split('.') {
        if teil.is_empty() {
            continue; // führende/abschließende Punkte überspringen
        }
        let bytes = teil.as_bytes();
        // Labels sind höchstens 63 Byte (obere zwei Bits sind für Zeiger).
        let laenge = bytes.len().min(63);
        s.u8(laenge as u8);
        s.bytes(&bytes[..laenge]);
    }
    s.u8(0); // Wurzel-Label (Ende)
}

/// Liest einen (evtl. komprimierten) Namen ab `start` und liefert
/// (Name, Offset DIREKT hinter dem Namen im Original). Folgt
/// Kompressions-Zeigern (0xC0), mit Schleifen-Schutz. None bei kaputten
/// Daten.
pub fn name_lesen(nachricht: &[u8], start: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut offset = start;
    // Der zurückzugebende Offset ist der hinter dem ERSTEN Zeiger (oder,
    // ohne Zeiger, hinter dem abschließenden 0-Byte).
    let mut ende_nach_zeiger: Option<usize> = None;
    let mut spruenge = 0;

    loop {
        let laenge = *nachricht.get(offset)?;
        if laenge & 0xC0 == 0xC0 {
            // Kompressions-Zeiger: 14-Bit-Offset aus diesem + nächstem Byte.
            let zweites = *nachricht.get(offset + 1)? as usize;
            let ziel = (((laenge & 0x3F) as usize) << 8) | zweites;
            if ende_nach_zeiger.is_none() {
                ende_nach_zeiger = Some(offset + 2);
            }
            spruenge += 1;
            if spruenge > 64 {
                return None; // Zeiger-Schleife -> abbrechen
            }
            offset = ziel;
            continue;
        }
        if laenge == 0 {
            offset += 1; // abschließendes Wurzel-Label
            break;
        }
        // Normales Label.
        let laenge = laenge as usize;
        let label = nachricht.get(offset + 1..offset + 1 + laenge)?;
        if !name.is_empty() {
            name.push('.');
        }
        for &b in label {
            name.push(b as char);
        }
        offset += 1 + laenge;
    }
    Some((name, ende_nach_zeiger.unwrap_or(offset)))
}

// ---------------------------------------------------------------------------
// Anfrage bauen, Antwort parsen
// ---------------------------------------------------------------------------

/// Baut eine A-Record-Anfrage für `name` mit Transaktions-ID `id`.
pub fn abfrage_bauen(id: u16, name: &str) -> Vec<u8> {
    let mut s = Schreiber::mit_kapazitaet(32 + name.len());
    // Kopf: ID, Flags (RD = Recursion Desired), QDCOUNT=1, Rest 0.
    s.u16_be(id);
    s.u16_be(0x0100); // Recursion Desired
    s.u16_be(1); // QDCOUNT
    s.u16_be(0); // ANCOUNT
    s.u16_be(0); // NSCOUNT
    s.u16_be(0); // ARCOUNT
    // Frage: Name, QTYPE=A, QCLASS=IN.
    name_kodieren(&mut s, name);
    s.u16_be(TYP_A);
    s.u16_be(KLASSE_IN);
    s.fertig()
}

/// Parst eine DNS-Antwort und liefert (erste A-IP, TTL). None bei falscher
/// ID, Fehler-RCODE oder wenn kein A-Record dabei ist. Beachtet die
/// Namens-KOMPRESSION beim Überspringen von Frage- und Antwort-Namen.
pub fn antwort_parsen(nachricht: &[u8], erwartete_id: u16) -> Option<(Ipv4, u32)> {
    if nachricht.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([nachricht[0], nachricht[1]]);
    if id != erwartete_id {
        return None;
    }
    let flags = u16::from_be_bytes([nachricht[2], nachricht[3]]);
    // QR-Bit (Antwort) muss gesetzt sein, RCODE (untere 4 Bit) = 0.
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return None;
    }
    let qd = u16::from_be_bytes([nachricht[4], nachricht[5]]);
    let an = u16::from_be_bytes([nachricht[6], nachricht[7]]);

    let mut offset = 12;
    // Fragen überspringen: Name (evtl. komprimiert) + QTYPE(2) + QCLASS(2).
    for _ in 0..qd {
        let (_, naechster) = name_lesen(nachricht, offset)?;
        offset = naechster + 4;
    }
    // Antworten durchgehen, den ersten A-Record zurückgeben.
    for _ in 0..an {
        let (_, naechster) = name_lesen(nachricht, offset)?;
        offset = naechster;
        // TYPE(2) CLASS(2) TTL(4) RDLENGTH(2) RDATA(RDLENGTH)
        let typ = u16::from_be_bytes([*nachricht.get(offset)?, *nachricht.get(offset + 1)?]);
        let ttl = u32::from_be_bytes([
            *nachricht.get(offset + 4)?,
            *nachricht.get(offset + 5)?,
            *nachricht.get(offset + 6)?,
            *nachricht.get(offset + 7)?,
        ]);
        let rdlength =
            u16::from_be_bytes([*nachricht.get(offset + 8)?, *nachricht.get(offset + 9)?]) as usize;
        let rdata_start = offset + 10;
        if typ == TYP_A && rdlength == 4 {
            let a = nachricht.get(rdata_start..rdata_start + 4)?;
            return Some((Ipv4([a[0], a[1], a[2], a[3]]), ttl));
        }
        offset = rdata_start + rdlength;
    }
    None
}

// ---------------------------------------------------------------------------
// Cache (Name -> IP, mit TTL)
// ---------------------------------------------------------------------------

static CACHE: Mutex<BTreeMap<String, (Ipv4, u64)>> = Mutex::new(BTreeMap::new());

/// Legt einen Eintrag mit Ablaufzeit an (TTL in Sekunden, mind. 10 s
/// gecacht, damit ein TTL-0-Server uns nicht bei jedem Aufruf schickt).
fn cache_einfuegen(name: &str, ip: Ipv4, ttl_sekunden: u32) {
    let ablauf = crate::zeit::ms_seit_boot() + (ttl_sekunden.max(10) as u64) * 1000;
    without_interrupts(|| {
        CACHE.lock().insert(name.to_ascii_lowercase(), (ip, ablauf));
    });
}

/// Sucht einen noch gültigen Cache-Eintrag.
fn cache_suchen(name: &str) -> Option<Ipv4> {
    let jetzt = crate::zeit::ms_seit_boot();
    let schluessel = name.to_ascii_lowercase();
    without_interrupts(|| {
        let cache = CACHE.lock();
        let (ip, ablauf) = cache.get(&schluessel)?;
        if *ablauf > jetzt {
            Some(*ip)
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Der Resolver
// ---------------------------------------------------------------------------

/// Fortlaufender ephemerer Quell-Port für DNS-Anfragen (49152..).
static QUELL_PORT: AtomicU16 = AtomicU16::new(49152);

fn naechster_port() -> u16 {
    // Zwischen 49152 und 60000 rotieren.
    let p = QUELL_PORT.fetch_add(1, Ordering::Relaxed);
    if p >= 60000 {
        QUELL_PORT.store(49152, Ordering::Relaxed);
    }
    p
}

/// Löst einen Namen zu einer IPv4-Adresse auf: erst Cache, sonst eine
/// A-Record-Anfrage an den bekannten DNS-Server (den Empfang synchron
/// pumpend). Ist der Name schon eine IP-Adresse, wird sie direkt geliefert.
pub fn aufloesen(name: &str) -> Result<Ipv4, DnsFehler> {
    // Ist es bereits eine IP? Dann keine Anfrage nötig.
    if let Some(ip) = Ipv4::parse(name) {
        return Ok(ip);
    }
    if let Some(ip) = cache_suchen(name) {
        return Ok(ip);
    }
    let server = super::dns_server().ok_or(DnsFehler::KeinDnsServer)?;

    let id = (crate::zeit::us_seit_boot() as u16) ^ 0x4453;
    let quell_port = naechster_port();
    let abfrage = abfrage_bauen(id, name);

    udp::binden(quell_port);
    while udp::empfangen(quell_port).is_some() {} // alte Datagramme weg

    // MEHRERE VERSUCHE mit Neu-Senden: Geht die Anfrage oder die Antwort
    // verloren, feuern wir nach VERSUCH_MS erneut — sonst würde ein einziger
    // verlorener DNS-Datagramm die ganze Auflösung scheitern lassen.
    let ergebnis = 'aufloesen: {
        for _ in 0..MAX_VERSUCHE {
            if let Err(fehler) = udp::senden(server, quell_port, DNS_PORT, &abfrage) {
                break 'aufloesen Err(DnsFehler::Netz(fehler));
            }
            let versuch_frist = crate::zeit::ms_seit_boot() + VERSUCH_MS;
            loop {
                super::rx_verarbeiten();
                while let Some(datagramm) = udp::empfangen(quell_port) {
                    if let Some((ip, ttl)) = antwort_parsen(&datagramm.daten, id) {
                        break 'aufloesen Ok((ip, ttl));
                    }
                    // Passende ID, aber kein A-Record -> Name nicht gefunden.
                    if datagramm.daten.len() >= 2
                        && u16::from_be_bytes([datagramm.daten[0], datagramm.daten[1]]) == id
                    {
                        break 'aufloesen Err(DnsFehler::NichtGefunden);
                    }
                }
                if crate::zeit::ms_seit_boot() >= versuch_frist {
                    break; // dieser Versuch ist um -> erneut senden
                }
                x86_64::instructions::hlt();
            }
        }
        Err(DnsFehler::Zeitueberschreitung)
    };

    udp::freigeben(quell_port);
    let (ip, ttl) = ergebnis?;
    cache_einfuegen(name, ip, ttl);
    Ok(ip)
}

/// NUR FÜR TESTS: leert den DNS-Cache.
#[cfg(test)]
pub fn cache_leeren() {
    without_interrupts(|| CACHE.lock().clear());
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// name_lesen folgt einem Kompressions-Zeiger und liefert den richtigen
    /// Namen UND den Offset direkt hinter dem Zeiger.
    #[test_case]
    fn test_dns_name_kompression() {
        // Nachricht: an Offset 0 der Name "example.com", ab Offset 13 ein
        // Zeiger (0xC0 0x00) zurück auf Offset 0.
        let mut nachricht = Vec::new();
        // "example" (7) "com" (3) 0  -> 13 Byte (0..=12)
        nachricht.push(7);
        nachricht.extend_from_slice(b"example");
        nachricht.push(3);
        nachricht.extend_from_slice(b"com");
        nachricht.push(0);
        // Ab Offset 13: Zeiger auf Offset 0.
        nachricht.push(0xC0);
        nachricht.push(0x00);

        // Unkomprimiert ab 0:
        let (name, ende) = name_lesen(&nachricht, 0).unwrap();
        assert_eq!(name, "example.com");
        assert_eq!(ende, 13);

        // Der Zeiger ab 13 ergibt denselben Namen; Offset danach = 15.
        let (name2, ende2) = name_lesen(&nachricht, 13).unwrap();
        assert_eq!(name2, "example.com");
        assert_eq!(ende2, 15);
    }

    /// Anfrage bauen + Antwort mit KOMPRIMIERTEM Antwort-Namen parsen: der
    /// A-Record wird korrekt herausgelesen.
    #[test_case]
    fn test_dns_abfrage_und_antwort() {
        let id = 0x1234;
        let abfrage = abfrage_bauen(id, "example.com");
        // Der Name beginnt bei Offset 12; QTYPE=A, QCLASS=IN.
        assert_eq!(u16::from_be_bytes([abfrage[0], abfrage[1]]), id);
        let (frage_name, _) = name_lesen(&abfrage, 12).unwrap();
        assert_eq!(frage_name, "example.com");

        // Eine Antwort bauen: Kopf (Antwort-Flag, qd=1, an=1), die Frage
        // gespiegelt, dann ein A-Record mit komprimiertem Namen (Zeiger auf
        // die Frage bei Offset 12).
        let mut antwort = Vec::new();
        antwort.extend_from_slice(&id.to_be_bytes());
        antwort.extend_from_slice(&0x8180u16.to_be_bytes()); // Antwort, RD, RA
        antwort.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        antwort.extend_from_slice(&1u16.to_be_bytes()); // ANCOUNT
        antwort.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        antwort.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        // Frage (Name ab Offset 12):
        let mut frage = Schreiber::neu();
        name_kodieren(&mut frage, "example.com");
        antwort.extend_from_slice(&frage.fertig());
        antwort.extend_from_slice(&TYP_A.to_be_bytes());
        antwort.extend_from_slice(&KLASSE_IN.to_be_bytes());
        // Antwort-Record: Name als Zeiger auf Offset 12 (die Frage).
        antwort.push(0xC0);
        antwort.push(12);
        antwort.extend_from_slice(&TYP_A.to_be_bytes());
        antwort.extend_from_slice(&KLASSE_IN.to_be_bytes());
        antwort.extend_from_slice(&300u32.to_be_bytes()); // TTL 300 s
        antwort.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        antwort.extend_from_slice(&[93, 184, 216, 34]); // 93.184.216.34

        let (ip, ttl) = antwort_parsen(&antwort, id).expect("A-Record");
        assert_eq!(ip, Ipv4([93, 184, 216, 34]));
        assert_eq!(ttl, 300);

        // Falsche ID -> None.
        assert!(antwort_parsen(&antwort, 0x9999).is_none());
    }

    /// Ein Name, der schon eine IP ist, wird direkt geliefert (keine Anfrage).
    #[test_case]
    fn test_dns_ip_direkt() {
        cache_leeren();
        assert_eq!(aufloesen("10.0.2.2"), Ok(Ipv4([10, 0, 2, 2])));
    }
}

