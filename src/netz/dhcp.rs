// netz/dhcp.rs — DHCP-Client: SpeedOS holt sich selbst eine IP
//
// DHCP (Dynamic Host Configuration Protocol) verteilt IP-Adressen im Netz.
// Der Client hat noch KEINE IP — also läuft alles per BROADCAST über UDP
// (Client-Port 68, Server-Port 67). Der klassische Vier-Wege-Tanz:
//
//   DISCOVER (Client -> Broadcast): "Ist ein DHCP-Server da?"
//   OFFER    (Server -> Client):    "Ja, nimm 10.0.2.15 (+ Maske/Router/DNS)."
//   REQUEST  (Client -> Broadcast): "Ich HÄTTE gern 10.0.2.15 von DIR (Server X)."
//   ACK      (Server -> Client):    "Bestätigt, sie gehört dir (Lease N s)."
//
// Das Paket ist ein altes BOOTP-Format (236 feste Byte) plus ein "magic
// cookie" (0x63825363) und dann OPTIONEN als TLV (Typ/Länge/Wert):
//   53 = Nachrichtentyp (1=DISCOVER, 2=OFFER, 3=REQUEST, 5=ACK)
//    1 = Subnetzmaske   3 = Router/Gateway   6 = DNS-Server
//   51 = Lease-Dauer   54 = Server-Identifier   50 = angeforderte IP
//   55 = Parameter-Request-List (was der Client wissen will)  255 = Ende
//
// WICHTIG (Broadcast-Flag): Weil wir Antworten empfangen müssen, BEVOR wir
// eine IP haben, setzen wir das Broadcast-Flag (0x8000) — der Server schickt
// OFFER/ACK dann an 255.255.255.255, und unser IPv4-Empfang akzeptiert das.

use super::ipv4::{self, BROADCAST_IP, PROTO_UDP};
use super::puffer::Schreiber;
use super::{udp, Ipv4, Mac};
use crate::serial_println;
use alloc::vec::Vec;

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;

const OP_REQUEST: u8 = 1; // BOOTREQUEST (Client -> Server)
const OP_REPLY: u8 = 2; // BOOTREPLY (Server -> Client)
const HTYPE_ETHERNET: u8 = 1;
const HLEN_MAC: u8 = 6;
const MAGIC_COOKIE: u32 = 0x6382_5363;

// DHCP-Nachrichtentypen (Option 53).
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;

// Option-Codes.
const OPT_SUBNETZMASKE: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_ANGEFORDERTE_IP: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_NACHRICHTENTYP: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAMETER_LISTE: u8 = 55;
const OPT_ENDE: u8 = 255;

/// Das aus einer DHCP-Antwort (OFFER/ACK) herausgelesene Wichtige.
#[derive(Debug, Clone, Copy, Default)]
pub struct DhcpNachricht {
    pub typ: u8,
    pub xid: u32,
    /// "your IP" — die dem Client angebotene/zugewiesene Adresse.
    pub yiaddr: Ipv4,
    pub maske: Ipv4,
    pub router: Ipv4,
    pub dns: Ipv4,
    pub server_id: Ipv4,
    pub lease_sekunden: u32,
}

/// Das Endergebnis eines erfolgreichen DHCP-Laufs.
#[derive(Debug, Clone, Copy)]
pub struct DhcpErgebnis {
    pub ip: Ipv4,
    pub maske: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
    pub lease_sekunden: u32,
}

// ---------------------------------------------------------------------------
// Optionen parsen (der reine, testbare Kern)
// ---------------------------------------------------------------------------

/// Liest die DHCP-Optionen (TLV-Liste hinter dem magic cookie) in eine
/// `DhcpNachricht`. Reine Funktion (kein Netz) — der Testkern für
/// "DHCP-Optionen-Parsing". Unbekannte Optionen werden übersprungen; 255
/// (Ende) bzw. 0 (Padding) sauber behandelt.
pub fn optionen_parsen(optionen: &[u8], nachricht: &mut DhcpNachricht) {
    let mut i = 0;
    while i < optionen.len() {
        let code = optionen[i];
        if code == OPT_ENDE {
            break;
        }
        if code == 0 {
            i += 1; // Padding
            continue;
        }
        // Ab hier: code, dann Längenbyte, dann Wert.
        if i + 1 >= optionen.len() {
            break;
        }
        let laenge = optionen[i + 1] as usize;
        let wert_start = i + 2;
        let wert_ende = wert_start + laenge;
        if wert_ende > optionen.len() {
            break; // abgeschnitten
        }
        let wert = &optionen[wert_start..wert_ende];
        match code {
            OPT_NACHRICHTENTYP if laenge >= 1 => nachricht.typ = wert[0],
            OPT_SUBNETZMASKE if laenge == 4 => nachricht.maske = ipv4_aus(wert),
            OPT_ROUTER if laenge >= 4 => nachricht.router = ipv4_aus(&wert[0..4]),
            OPT_DNS if laenge >= 4 => nachricht.dns = ipv4_aus(&wert[0..4]),
            OPT_SERVER_ID if laenge == 4 => nachricht.server_id = ipv4_aus(wert),
            OPT_LEASE if laenge == 4 => {
                nachricht.lease_sekunden = u32::from_be_bytes([wert[0], wert[1], wert[2], wert[3]])
            }
            _ => {}
        }
        i = wert_ende;
    }
}

/// Liest 4 Bytes als Ipv4.
fn ipv4_aus(bytes: &[u8]) -> Ipv4 {
    Ipv4([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Zerlegt eine komplette DHCP-Nachricht (BOOTP + Optionen). None, wenn es
/// keine gültige BOOTREPLY mit magic cookie ist.
pub fn parse(paket: &[u8]) -> Option<DhcpNachricht> {
    // Fester Teil: op(1) htype(1) hlen(1) hops(1) xid(4) ... yiaddr@16
    // ... chaddr@28 ... sname@44 ... file@108 ... cookie@236 ... options@240
    if paket.len() < 240 {
        return None;
    }
    if paket[0] != OP_REPLY {
        return None;
    }
    let cookie = u32::from_be_bytes([paket[236], paket[237], paket[238], paket[239]]);
    if cookie != MAGIC_COOKIE {
        return None;
    }
    let xid = u32::from_be_bytes([paket[4], paket[5], paket[6], paket[7]]);
    let yiaddr = Ipv4([paket[16], paket[17], paket[18], paket[19]]);
    let mut nachricht = DhcpNachricht {
        xid,
        yiaddr,
        ..Default::default()
    };
    optionen_parsen(&paket[240..], &mut nachricht);
    Some(nachricht)
}

// ---------------------------------------------------------------------------
// Nachrichten bauen
// ---------------------------------------------------------------------------

/// Baut den festen BOOTP-Teil + magic cookie; die Optionen hängt der
/// Aufrufer an. `xid` = Transaktions-ID, `mac` = unsere Hardware-Adresse.
fn bootp_grundgeruest(mac: Mac, xid: u32) -> Schreiber {
    let mut s = Schreiber::mit_kapazitaet(300);
    s.u8(OP_REQUEST); // op = BOOTREQUEST
    s.u8(HTYPE_ETHERNET); // htype
    s.u8(HLEN_MAC); // hlen
    s.u8(0); // hops
    s.u32_be(xid); // xid
    s.u16_be(0); // secs
    s.u16_be(0x8000); // flags: BROADCAST (Antworten an 255.255.255.255)
    s.u32_be(0); // ciaddr
    s.u32_be(0); // yiaddr
    s.u32_be(0); // siaddr
    s.u32_be(0); // giaddr
    // chaddr (16 Byte): MAC + 10 Byte Null-Padding.
    s.bytes(&mac);
    s.bytes(&[0u8; 10]);
    // sname (64) + file (128) = 192 Byte Null.
    s.bytes(&[0u8; 192]);
    // magic cookie.
    s.u32_be(MAGIC_COOKIE);
    s
}

/// Hängt die Parameter-Request-List an (was wir vom Server wissen wollen).
fn parameter_liste(s: &mut Schreiber) {
    s.u8(OPT_PARAMETER_LISTE);
    s.u8(3);
    s.u8(OPT_SUBNETZMASKE);
    s.u8(OPT_ROUTER);
    s.u8(OPT_DNS);
}

/// Baut eine DISCOVER-Nachricht.
pub fn discover_bauen(mac: Mac, xid: u32) -> Vec<u8> {
    let mut s = bootp_grundgeruest(mac, xid);
    s.u8(OPT_NACHRICHTENTYP);
    s.u8(1);
    s.u8(DHCP_DISCOVER);
    parameter_liste(&mut s);
    s.u8(OPT_ENDE);
    s.fertig()
}

/// Baut eine REQUEST-Nachricht (angeforderte IP + gewählter Server).
pub fn request_bauen(mac: Mac, xid: u32, angefordert: Ipv4, server_id: Ipv4) -> Vec<u8> {
    let mut s = bootp_grundgeruest(mac, xid);
    s.u8(OPT_NACHRICHTENTYP);
    s.u8(1);
    s.u8(DHCP_REQUEST);
    s.u8(OPT_ANGEFORDERTE_IP);
    s.u8(4);
    s.bytes(&angefordert.oktette());
    s.u8(OPT_SERVER_ID);
    s.u8(4);
    s.bytes(&server_id.oktette());
    parameter_liste(&mut s);
    s.u8(OPT_ENDE);
    s.fertig()
}

// ---------------------------------------------------------------------------
// Der Ablauf: eine IP beziehen (synchron, den Empfang pumpend)
// ---------------------------------------------------------------------------

/// Sendet eine DHCP-Nachricht als Broadcast (0.0.0.0 -> 255.255.255.255,
/// UDP 68 -> 67, an die Broadcast-MAC).
fn senden(payload: &[u8]) -> Result<(), super::NetzFehler> {
    let segment = udp::bauen(CLIENT_PORT, SERVER_PORT, Ipv4::NULL, BROADCAST_IP, payload);
    ipv4::senden_an_mac(
        Ipv4::NULL,
        BROADCAST_IP,
        super::ethernet::BROADCAST,
        PROTO_UDP,
        &segment,
    )
}

/// Wartet (den Empfang pumpend) bis `deadline_ms` auf eine DHCP-Antwort mit
/// passender `xid` und `typ`. None bei Timeout.
fn warte_auf(xid: u32, typ: u8, deadline_ms: u64) -> Option<DhcpNachricht> {
    loop {
        super::rx_verarbeiten();
        while let Some(datagramm) = udp::empfangen(CLIENT_PORT) {
            if let Some(nachricht) = parse(&datagramm.daten) {
                if nachricht.xid == xid && nachricht.typ == typ {
                    return Some(nachricht);
                }
            }
        }
        if crate::zeit::ms_seit_boot() >= deadline_ms {
            return None;
        }
        crate::zeit::warte_auf_interrupt();
    }
}

/// Führt den vollen DHCP-Ablauf aus (DISCOVER -> OFFER -> REQUEST -> ACK) und
/// liefert die bezogene Konfiguration. Pumpt den Empfang synchron; bricht
/// nach `timeout_ms` insgesamt ab (Fallback auf statische Config).
pub fn beziehen(timeout_ms: u64) -> Option<DhcpErgebnis> {
    let mac = super::mac()?;
    // Transaktions-ID aus der TSC-Uhr (muss nur einigermaßen einzigartig sein).
    let xid = (crate::zeit::us_seit_boot() as u32) ^ 0x5350_4544;
    let gesamt_deadline = crate::zeit::ms_seit_boot() + timeout_ms;

    udp::binden(CLIENT_PORT);
    // Etwaige alte Datagramme verwerfen.
    while udp::empfangen(CLIENT_PORT).is_some() {}

    // Phase 1: DISCOVER -> OFFER (bis zu einige Male, jeweils ~1 s warten).
    let offer = loop {
        if let Err(fehler) = senden(&discover_bauen(mac, xid)) {
            serial_println!("[dhcp] DISCOVER senden fehlgeschlagen: {}", fehler.meldung());
            udp::freigeben(CLIENT_PORT);
            return None;
        }
        let phase = (crate::zeit::ms_seit_boot() + 1000).min(gesamt_deadline);
        if let Some(offer) = warte_auf(xid, DHCP_OFFER, phase) {
            break offer;
        }
        if crate::zeit::ms_seit_boot() >= gesamt_deadline {
            udp::freigeben(CLIENT_PORT);
            return None;
        }
    };

    // Phase 2: REQUEST -> ACK.
    let ack = loop {
        if let Err(fehler) = senden(&request_bauen(mac, xid, offer.yiaddr, offer.server_id)) {
            serial_println!("[dhcp] REQUEST senden fehlgeschlagen: {}", fehler.meldung());
            udp::freigeben(CLIENT_PORT);
            return None;
        }
        let phase = (crate::zeit::ms_seit_boot() + 1000).min(gesamt_deadline);
        if let Some(ack) = warte_auf(xid, DHCP_ACK, phase) {
            break ack;
        }
        if crate::zeit::ms_seit_boot() >= gesamt_deadline {
            udp::freigeben(CLIENT_PORT);
            return None;
        }
    };

    udp::freigeben(CLIENT_PORT);
    Some(DhcpErgebnis {
        ip: ack.yiaddr,
        maske: ack.maske,
        gateway: ack.router,
        dns: ack.dns,
        lease_sekunden: ack.lease_sekunden,
    })
}

/// Versucht beim Boot, per DHCP eine IP zu beziehen, und übernimmt sie in die
/// Netz-Konfiguration. Kein Erfolg -> nur eine Meldung (Fallback: `netz-ip`).
pub fn autokonfig(timeout_ms: u64) {
    if !super::vorhanden() {
        return; // keine NIC
    }
    match beziehen(timeout_ms) {
        Some(e) => {
            super::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);
            serial_println!(
                "[dhcp] Lease bezogen: IP {}, Maske {}, Gateway {}, DNS {}, {} s.",
                e.ip,
                e.maske,
                e.gateway,
                e.dns,
                e.lease_sekunden
            );
        }
        None => {
            serial_println!("[dhcp] Keine Antwort — statische Konfiguration mit 'netz-ip' noetig.");
        }
    }
}

// ---------------------------------------------------------------------------
// Lease-Erneuerung — reine Zeit-Logik (testbar) + der Erneuerungs-Task
// ---------------------------------------------------------------------------
//
// Eine DHCP-Lease gilt nur `lease_sekunden` lang. Nach der Hälfte (T1, RFC
// 2131) SOLL der Client sie erneuern, damit sie nicht abläuft und ein anderer
// die IP bekommt. Die Entscheidung „jetzt erneuern?" ist reine Zeit-Rechnung
// — deshalb als eigene, unit-getestete Funktion (die echte Uhr wird nur im
// Task übergeben).

/// Ist die Lease-Erneuerung fällig? T1 = 50 % der Lease-Dauer. `lease_sekunden
/// == 0` (keine DHCP-Lease) heißt „nie".
pub fn erneuerung_faellig(jetzt_ms: u64, lease_start_ms: u64, lease_sekunden: u32) -> bool {
    if lease_sekunden == 0 {
        return false;
    }
    let t1_ms = lease_start_ms + (lease_sekunden as u64) * 1000 / 2;
    jetzt_ms >= t1_ms
}

/// Ist die Lease bereits ABGELAUFEN (100 % verstrichen)?
pub fn abgelaufen(jetzt_ms: u64, lease_start_ms: u64, lease_sekunden: u32) -> bool {
    if lease_sekunden == 0 {
        return false;
    }
    jetzt_ms >= lease_start_ms + (lease_sekunden as u64) * 1000
}

/// Der Lease-Erneuerungs-Task: prüft regelmäßig, ob die Lease-Hälfte (T1)
/// erreicht ist, und bezieht dann eine frische Lease. In QEMU (Lease
/// 86400 s) feuert das praktisch nie — aber die Mechanik steht und ist
/// getestet. Läuft ruhig (alle 30 s ein Blick auf die Uhr).
pub async fn erneuerung_task() {
    loop {
        crate::zeit::warte_ms(30_000).await;
        let k = super::konfig();
        if k.quelle != super::Quelle::Dhcp {
            continue;
        }
        let jetzt = crate::zeit::ms_seit_boot();
        if erneuerung_faellig(jetzt, k.lease_start_ms, k.lease_sekunden) {
            serial_println!("[dhcp] Lease-Haelfte erreicht — erneuere ...");
            if let Some(e) = beziehen(4000) {
                super::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);
                serial_println!("[dhcp] Lease erneuert: IP {}, {} s.", e.ip, e.lease_sekunden);
            } else {
                serial_println!("[dhcp] Erneuerung fehlgeschlagen — behalte alte Lease vorerst.");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// DHCP-Optionen-Parsing: aus einer TLV-Liste die Felder herauslesen
    /// (Nachrichtentyp, Maske, Router, DNS, Lease, Server-ID), Padding und
    /// Ende sauber behandeln.
    #[test_case]
    fn test_dhcp_optionen_parsen() {
        // Ein OFFER: Typ=2, Maske, Router, DNS, Lease=86400, Server-ID, Ende.
        let optionen = [
            OPT_NACHRICHTENTYP, 1, DHCP_OFFER,
            OPT_SUBNETZMASKE, 4, 255, 255, 255, 0,
            OPT_ROUTER, 4, 10, 0, 2, 2,
            OPT_DNS, 4, 10, 0, 2, 3,
            OPT_LEASE, 4, 0, 1, 0x51, 0x80, // 86400 = 0x00015180
            OPT_SERVER_ID, 4, 10, 0, 2, 2,
            0, 0, // Padding
            OPT_ENDE,
            42, 42, // Müll hinter dem Ende: wird ignoriert
        ];
        let mut n = DhcpNachricht::default();
        optionen_parsen(&optionen, &mut n);
        assert_eq!(n.typ, DHCP_OFFER);
        assert_eq!(n.maske, Ipv4([255, 255, 255, 0]));
        assert_eq!(n.router, Ipv4([10, 0, 2, 2]));
        assert_eq!(n.dns, Ipv4([10, 0, 2, 3]));
        assert_eq!(n.lease_sekunden, 86400);
        assert_eq!(n.server_id, Ipv4([10, 0, 2, 2]));
    }

    /// Eine abgeschnittene Option darf nicht panicken (Länge zeigt über das
    /// Ende hinaus -> Abbruch).
    #[test_case]
    fn test_dhcp_optionen_abgeschnitten() {
        let optionen = [OPT_ROUTER, 4, 10, 0]; // behauptet 4 Byte, hat nur 2
        let mut n = DhcpNachricht::default();
        optionen_parsen(&optionen, &mut n);
        assert_eq!(n.router, Ipv4::NULL); // nichts übernommen, kein Absturz
    }

    /// discover_bauen erzeugt eine gültige, wieder parsbare Anfrage-Struktur
    /// (op=REQUEST, magic cookie, Nachrichtentyp DISCOVER).
    #[test_case]
    fn test_dhcp_discover_bauen() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let paket = discover_bauen(mac, 0xDEADBEEF);
        assert_eq!(paket[0], OP_REQUEST);
        assert!(paket.len() >= 240);
        // magic cookie an Offset 236:
        assert_eq!(
            u32::from_be_bytes([paket[236], paket[237], paket[238], paket[239]]),
            MAGIC_COOKIE
        );
        // chaddr trägt unsere MAC (Offset 28):
        assert_eq!(&paket[28..34], &mac);
        // Der Nachrichtentyp DISCOVER steht in den Optionen.
        let mut n = DhcpNachricht::default();
        optionen_parsen(&paket[240..], &mut n);
        assert_eq!(n.typ, DHCP_DISCOVER);
    }

    /// Die Lease-Erneuerungs-Logik: vor T1 (50 %) nichts, ab T1 fällig, ab
    /// 100 % abgelaufen; ohne Lease (0 s) nie.
    #[test_case]
    fn test_dhcp_lease_erneuerung() {
        // Lease über 100 s, bezogen bei t = 1000 ms.
        let start = 1000;
        let dauer = 100; // Sekunden -> T1 bei 50 000 ms nach Start
        assert!(!erneuerung_faellig(start + 49_999, start, dauer), "vor T1 nicht");
        assert!(erneuerung_faellig(start + 50_000, start, dauer), "ab T1 faellig");
        assert!(erneuerung_faellig(start + 80_000, start, dauer));

        // Ablauf erst bei 100 %.
        assert!(!abgelaufen(start + 99_999, start, dauer));
        assert!(abgelaufen(start + 100_000, start, dauer));

        // Keine Lease (0 s) -> nie fällig, nie abgelaufen.
        assert!(!erneuerung_faellig(u64::MAX, 0, 0));
        assert!(!abgelaufen(u64::MAX, 0, 0));
    }
}
