// netz/ethernet.rs — Die Ethernet-Schicht (Schicht 2, Sicherungsschicht)
//
// Jedes Paket, das über die NIC geht, ist in einen ETHERNET-RAHMEN
// gepackt. Der Rahmen-Kopf ist denkbar einfach — 14 Byte:
//
//   [ Ziel-MAC (6) | Quell-MAC (6) | EtherType (2, Big-Endian) | Nutzlast ]
//
// Die MAC-Adresse (Media Access Control) ist die 6-Byte-Hardware-Adresse
// der Netzwerkkarte; sie adressiert Geräte im LOKALEN Netz (ein Switch
// leitet danach weiter). Der EtherType sagt, WAS in der Nutzlast steckt:
//   * 0x0806 = ARP  (Adressauflösung — dieses Modul-Nachbar arp.rs)
//   * 0x0800 = IPv4 (kommt in einer späteren Serie-5-Stufe)
//   * 0x86DD = IPv6
//
// Dieses Modul ist BEWUSST reine Byte-Logik ohne Geräte-Bezug: `parse`
// und `rahmen_bauen` sind unit-testbar ohne jede Hardware. Der `netz_task`
// (mod.rs) ruft `parse` auf jedes empfangene Frame und verzweigt nach dem
// EtherType an die passende obere Schicht.

use super::puffer::{Leser, Schreiber};
use crate::println;
use alloc::string::String;
use alloc::vec::Vec;

/// Eine MAC-Adresse — 6 Byte. (Bewusst ein Alias auf `[u8; 6]` statt ein
/// eigener Typ: So bleibt sie zu `crate::virtio::net` kompatibel, das die
/// MAC ebenfalls als `[u8; 6]` führt, und wir sparen Konvertierungen.)
pub type Mac = [u8; 6];

/// Die Broadcast-MAC (an ALLE im lokalen Netz) — z. B. das Ziel eines
/// ARP-Requests, weil wir die Ziel-MAC ja gerade erst herausfinden wollen.
pub const BROADCAST: Mac = [0xff; 6];

/// Länge des Ethernet-Kopfes in Byte (Ziel + Quelle + EtherType).
pub const KOPF_LEN: usize = 14;

/// EtherType: ARP (Adressauflösung IP -> MAC).
pub const ETHERTYPE_ARP: u16 = 0x0806;
/// EtherType: IPv4.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// EtherType: IPv6.
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

/// Der geparste Ethernet-Kopf (die Nutzlast reicht `parse` separat zurück).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetKopf {
    pub ziel: Mac,
    pub quelle: Mac,
    pub ethertype: u16,
}

/// Zerlegt ein rohes Ethernet-Frame in (Kopf, Nutzlast). None, wenn das
/// Frame kürzer als der 14-Byte-Kopf ist (kaputtes/zu kurzes Frame —
/// niemals panicken).
pub fn parse(frame: &[u8]) -> Option<(EthernetKopf, &[u8])> {
    let mut l = Leser::neu(frame);
    let ziel = l.feld::<6>()?;
    let quelle = l.feld::<6>()?;
    let ethertype = l.u16_be()?;
    // Der Rest hinter dem Kopf ist die Nutzlast.
    let nutzlast = l.bytes(l.rest())?;
    Some((EthernetKopf { ziel, quelle, ethertype }, nutzlast))
}

/// Baut ein Ethernet-Frame aus Ziel, Quelle, EtherType und Nutzlast.
/// (Wir polstern NICHT auf die Mindestrahmengröße von 60 Byte — virtio-net
/// und QEMUs slirp akzeptieren kurze Frames und polstern selbst; auf echter
/// Hardware übernimmt das die NIC.)
pub fn rahmen_bauen(ziel: Mac, quelle: Mac, ethertype: u16, nutzlast: &[u8]) -> Vec<u8> {
    let mut s = Schreiber::mit_kapazitaet(KOPF_LEN + nutzlast.len());
    s.bytes(&ziel);
    s.bytes(&quelle);
    s.u16_be(ethertype);
    s.bytes(nutzlast);
    s.fertig()
}

/// Formatiert eine MAC-Adresse als `aa:bb:cc:dd:ee:ff`.
pub fn mac_text(m: &Mac) -> String {
    alloc::format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

/// Menschlicher Name eines EtherType (für Hexdump/Diagnose).
pub fn typ_name(ethertype: u16) -> &'static str {
    match ethertype {
        ETHERTYPE_ARP => "ARP",
        ETHERTYPE_IPV4 => "IPv4",
        ETHERTYPE_IPV6 => "IPv6",
        0x8100 => "VLAN",
        _ => "?",
    }
}

/// Gibt ein rohes Ethernet-Frame lesbar aus: Ziel-/Quell-MAC, EtherType
/// (annotiert) und einen Hexdump der ersten Bytes. Für den Shell-Befehl
/// `netz-lausch` — geräteunabhängig, deshalb hier statt im Treiber.
pub fn hexdump(frame: &[u8]) {
    if frame.len() < KOPF_LEN {
        println!("[netz] Frame zu kurz ({} Byte)", frame.len());
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    println!(
        "[netz] Frame {} Byte | Ziel {} | Quelle {} | EtherType 0x{:04x} ({})",
        frame.len(),
        mac_text(&[frame[0], frame[1], frame[2], frame[3], frame[4], frame[5]]),
        mac_text(&[frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]]),
        ethertype,
        typ_name(ethertype),
    );
    // Roh-Hexdump, gedeckelt (16 Byte je Zeile, höchstens 64 Byte):
    let max = frame.len().min(64);
    let mut i = 0;
    while i < max {
        let ende = (i + 16).min(max);
        let mut hex = String::new();
        let mut asc = String::new();
        for &b in &frame[i..ende] {
            hex.push_str(&alloc::format!("{:02x} ", b));
            asc.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        println!("  {:04x}  {:<48}{}", i, hex, asc);
        i += 16;
    }
    if frame.len() > max {
        println!("  ... ({} weitere Byte)", frame.len() - max);
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Bauen und Parsen sind zueinander invers: ein gebautes Frame parst
    /// wieder zu genau denselben Feldern (Kopf + Nutzlast).
    #[test_case]
    fn test_ethernet_bau_und_parse() {
        let ziel = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let quelle = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let nutzlast = [0xDE, 0xAD, 0xBE, 0xEF];

        let frame = rahmen_bauen(ziel, quelle, ETHERTYPE_ARP, &nutzlast);
        // 14 Byte Kopf + 4 Byte Nutzlast:
        assert_eq!(frame.len(), KOPF_LEN + 4);
        // EtherType steht Big-Endian an Position 12/13:
        assert_eq!(&frame[12..14], &[0x08, 0x06]);

        let (kopf, rest) = parse(&frame).expect("gebautes Frame muss parsen");
        assert_eq!(kopf.ziel, ziel);
        assert_eq!(kopf.quelle, quelle);
        assert_eq!(kopf.ethertype, ETHERTYPE_ARP);
        assert_eq!(rest, &nutzlast);
    }

    /// Ein Frame, das kürzer als der 14-Byte-Kopf ist, liefert None.
    #[test_case]
    fn test_ethernet_zu_kurz() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 13]).is_none());
        // Genau 14 Byte: Kopf komplett, Nutzlast leer — das ist gültig.
        let (kopf, rest) = parse(&[0u8; 14]).unwrap();
        assert_eq!(kopf.ethertype, 0x0000);
        assert_eq!(rest.len(), 0);
    }
}
