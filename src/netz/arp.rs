// netz/arp.rs — ARP: das Adressbuch zwischen IP und MAC
//
// IP-Adressen sind eine LOGISCHE Adressierung (10.0.2.15); Ethernet
// dagegen adressiert Geräte über ihre 6-Byte-MAC. Wer ein IP-Paket ins
// lokale Netz schicken will, muss also erst die MAC hinter der Ziel-IP
// kennen. Genau das leistet ARP (Address Resolution Protocol, RFC 826):
//
//   REQUEST  (Broadcast): "Wer hat 10.0.2.2? Sag es 10.0.2.15 (aa:bb:...)."
//   REPLY    (Unicast):   "10.0.2.2 ist bei ff:ee:dd:cc:bb:aa."
//
// Ein ARP-Paket (28 Byte für IPv4-über-Ethernet) sitzt direkt in der
// Ethernet-Nutzlast (EtherType 0x0806):
//   htype(2)=1 ptype(2)=0x0800 hlen(1)=6 plen(1)=4 op(2)
//   sha(6)=Absender-MAC spa(4)=Absender-IP tha(6)=Ziel-MAC tpa(4)=Ziel-IP
//
// Dieses Modul leistet drei Dinge (Aufgabe 3 der Serie 5):
//   1. REQUESTS BEANTWORTEN — fragt jemand nach UNSERER IP, schicken wir
//      unsere MAC zurück. Das ist der Meilenstein: "SpeedOS antwortet auf
//      ARP" (vom Host aus ist unsere IP dann in der ARP-Tabelle sichtbar).
//   2. EIGENE REQUESTS SENDEN — um eine fremde MAC aufzulösen (arp-ping).
//   3. EIN ARP-CACHE (IP -> MAC) mit TIMEOUT — jede gehörte Zuordnung wird
//      gelernt und läuft nach einer Frist ab (Netze ändern sich).
//
// Parsen/Bauen und der Cache sind reine, unit-getestete Logik; nur
// `verarbeiten`/`anfrage_senden` fassen die globale NIC/Konfig an.

use super::ethernet::{self, Mac, BROADCAST, ETHERTYPE_ARP};
use super::geraet::NetzFehler;
use super::puffer::{Leser, Schreiber};
use super::Ipv4;
use crate::serial_println;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

// --- Feste ARP-Felder für IPv4 über Ethernet ---------------------------
const HTYPE_ETHERNET: u16 = 1;
const PTYPE_IPV4: u16 = 0x0800;
const HLEN_MAC: u8 = 6;
const PLEN_IPV4: u8 = 4;

/// ARP-Operation: Anfrage ("wer hat diese IP?").
pub const OP_REQUEST: u16 = 1;
/// ARP-Operation: Antwort ("ich habe sie, hier ist meine MAC").
pub const OP_REPLY: u16 = 2;

/// Länge eines IPv4-über-Ethernet-ARP-Pakets in Byte.
pub const PAKET_LEN: usize = 28;

/// Ein geparstes/zu bauendes ARP-Paket (nur die IPv4-über-Ethernet-Form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArpPaket {
    pub operation: u16,
    /// sha — Absender-MAC.
    pub absender_mac: Mac,
    /// spa — Absender-IP.
    pub absender_ip: Ipv4,
    /// tha — Ziel-MAC (bei Requests 00:00:00:00:00:00, wir suchen sie ja).
    pub ziel_mac: Mac,
    /// tpa — Ziel-IP.
    pub ziel_ip: Ipv4,
}

impl ArpPaket {
    /// Zerlegt die ARP-Nutzlast. None, wenn es KEIN IPv4-über-Ethernet-ARP
    /// ist (fremde Hardware-/Protokolltypen ignorieren wir still — nie
    /// panicken).
    pub fn parse(nutzlast: &[u8]) -> Option<ArpPaket> {
        let mut l = Leser::neu(nutzlast);
        let htype = l.u16_be()?;
        let ptype = l.u16_be()?;
        let hlen = l.u8()?;
        let plen = l.u8()?;
        let operation = l.u16_be()?;
        // Nur die klassische Form (Ethernet + IPv4) verstehen wir.
        if htype != HTYPE_ETHERNET || ptype != PTYPE_IPV4 || hlen != HLEN_MAC || plen != PLEN_IPV4 {
            return None;
        }
        let absender_mac = l.feld::<6>()?;
        let absender_ip = Ipv4(l.feld::<4>()?);
        let ziel_mac = l.feld::<6>()?;
        let ziel_ip = Ipv4(l.feld::<4>()?);
        Some(ArpPaket {
            operation,
            absender_mac,
            absender_ip,
            ziel_mac,
            ziel_ip,
        })
    }

    /// Serialisiert das Paket (28 Byte) für die Ethernet-Nutzlast.
    pub fn bauen(&self) -> Vec<u8> {
        let mut s = Schreiber::mit_kapazitaet(PAKET_LEN);
        s.u16_be(HTYPE_ETHERNET);
        s.u16_be(PTYPE_IPV4);
        s.u8(HLEN_MAC);
        s.u8(PLEN_IPV4);
        s.u16_be(self.operation);
        s.bytes(&self.absender_mac);
        s.bytes(&self.absender_ip.oktette());
        s.bytes(&self.ziel_mac);
        s.bytes(&self.ziel_ip.oktette());
        s.fertig()
    }

    /// Baut einen ARP-REQUEST ("wer hat `ziel_ip`?"). Ziel-MAC ist leer —
    /// die suchen wir ja gerade.
    pub fn anfrage(unsere_mac: Mac, unsere_ip: Ipv4, ziel_ip: Ipv4) -> ArpPaket {
        ArpPaket {
            operation: OP_REQUEST,
            absender_mac: unsere_mac,
            absender_ip: unsere_ip,
            ziel_mac: [0; 6],
            ziel_ip,
        }
    }

    /// Baut eine ARP-ANTWORT auf einen Request (unsere MAC an den Frager).
    pub fn antwort(unsere_mac: Mac, unsere_ip: Ipv4, frager_mac: Mac, frager_ip: Ipv4) -> ArpPaket {
        ArpPaket {
            operation: OP_REPLY,
            absender_mac: unsere_mac,
            absender_ip: unsere_ip,
            ziel_mac: frager_mac,
            ziel_ip: frager_ip,
        }
    }
}

// ---------------------------------------------------------------------------
// Der ARP-Cache (IP -> MAC, mit Timeout) — reine, testbare Logik
// ---------------------------------------------------------------------------

/// Wie lange ein gelernter Eintrag gültig bleibt (2 Minuten). Danach gilt
/// er als abgelaufen und wird bei der nächsten Suche/Anzeige ignoriert —
/// Netze ändern sich, eine alte MAC wäre schlimmer als gar keine.
pub const CACHE_TTL_MS: u64 = 120_000;

/// Der ARP-Cache: für jede IP die zuletzt gehörte MAC samt Lern-Zeitpunkt.
/// BEWUSST ohne eigene Uhr — jede Methode bekommt `jetzt_ms` übergeben, so
/// ist die Timeout-Logik ohne laufende Uhr unit-testbar (das Muster der
/// „reinen Funktion" aus dem ganzen Projekt).
pub struct ArpCache {
    eintraege: BTreeMap<[u8; 4], (Mac, u64)>,
}

impl ArpCache {
    /// Ein leerer Cache (const, damit er in einen `static` passt).
    pub const fn neu() -> ArpCache {
        ArpCache {
            eintraege: BTreeMap::new(),
        }
    }

    /// Lernt/aktualisiert die Zuordnung `ip -> mac` (Zeitstempel = jetzt).
    pub fn einfuegen(&mut self, ip: Ipv4, mac: Mac, jetzt_ms: u64) {
        self.eintraege.insert(ip.oktette(), (mac, jetzt_ms));
    }

    /// Sucht die MAC zu `ip` — aber nur, wenn der Eintrag noch nicht
    /// abgelaufen ist. Abgelaufene Einträge liefern None.
    pub fn suchen(&self, ip: Ipv4, jetzt_ms: u64) -> Option<Mac> {
        let (mac, gelernt) = self.eintraege.get(&ip.oktette())?;
        if jetzt_ms.saturating_sub(*gelernt) <= CACHE_TTL_MS {
            Some(*mac)
        } else {
            None
        }
    }

    /// Alle noch gültigen Einträge als (IP, MAC, Alter-in-ms) — für die
    /// Shell-Anzeige (`arp`).
    pub fn eintraege(&self, jetzt_ms: u64) -> Vec<(Ipv4, Mac, u64)> {
        self.eintraege
            .iter()
            .filter_map(|(oktette, (mac, gelernt))| {
                let alter = jetzt_ms.saturating_sub(*gelernt);
                if alter <= CACHE_TTL_MS {
                    Some((Ipv4(*oktette), *mac, alter))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Entfernt abgelaufene Einträge dauerhaft (hält den Cache klein).
    pub fn aufraeumen(&mut self, jetzt_ms: u64) {
        self.eintraege
            .retain(|_, (_, gelernt)| jetzt_ms.saturating_sub(*gelernt) <= CACHE_TTL_MS);
    }
}

/// Der globale ARP-Cache (Blatt-Lock, nur aus Task-Kontext).
static ARP_CACHE: Mutex<ArpCache> = Mutex::new(ArpCache::neu());

/// Lernt eine IP->MAC-Zuordnung (nutzt die echte Uhr).
pub fn cache_einfuegen(ip: Ipv4, mac: Mac) {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| ARP_CACHE.lock().einfuegen(ip, mac, jetzt));
}

/// Sucht eine MAC im Cache (None, wenn unbekannt oder abgelaufen).
pub fn cache_suchen(ip: Ipv4) -> Option<Mac> {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| ARP_CACHE.lock().suchen(ip, jetzt))
}

/// Alle noch gültigen Cache-Einträge (für den `arp`-Shell-Befehl).
pub fn cache_eintraege() -> Vec<(Ipv4, Mac, u64)> {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| ARP_CACHE.lock().eintraege(jetzt))
}

// ---------------------------------------------------------------------------
// Der aktive Teil: eingehende ARP-Pakete verarbeiten, Requests senden
// ---------------------------------------------------------------------------

/// Verarbeitet ein eingehendes ARP-Paket (aus der Ethernet-Nutzlast). Wird
/// vom `netz_task`-Dispatch für EtherType 0x0806 gerufen.
///
/// 1. LERNEN: Jede ARP-Nachricht verrät uns die Absender-Zuordnung
///    (sha -> spa) — die nehmen wir immer in den Cache (so füllt ein
///    Request-„von 10.0.2.2" den Cache genauso wie eine Antwort).
/// 2. ANTWORTEN: Ist es ein Request nach UNSERER konfigurierten IP,
///    schicken wir eine Antwort mit unserer MAC.
pub fn verarbeiten(nutzlast: &[u8]) {
    let paket = match ArpPaket::parse(nutzlast) {
        Some(p) => p,
        None => return, // kein IPv4/Ethernet-ARP — ignorieren
    };

    // 1. Absender lernen (nicht die 0.0.0.0 eines ARP-Probes eintragen).
    if paket.absender_ip != Ipv4::NULL {
        cache_einfuegen(paket.absender_ip, paket.absender_mac);
    }

    // 2. Fragt jemand nach UNSERER IP? Dann antworten.
    if paket.operation == OP_REQUEST {
        let unsere_ip = match super::unsere_ip() {
            Some(ip) => ip,
            None => return, // keine IP konfiguriert -> wir „sind" niemand
        };
        if paket.ziel_ip != unsere_ip {
            return; // nicht an uns gerichtet
        }
        let unsere_mac = match super::mac() {
            Some(mac) => mac,
            None => return,
        };
        let antwort = ArpPaket::antwort(unsere_mac, unsere_ip, paket.absender_mac, paket.absender_ip);
        // Antwort als Unicast direkt an den Frager.
        let frame = ethernet::rahmen_bauen(paket.absender_mac, unsere_mac, ETHERTYPE_ARP, &antwort.bauen());
        if let Err(fehler) = super::sende_frame(&frame) {
            // Netz ist Best-Effort, aber verschlucken tun wir nichts:
            // ein Sendefehler wird seriell gemeldet (println wäre im
            // Dispatch-Pfad okay, aber seriell reicht für Diagnose).
            serial_println!("[arp] Antwort senden fehlgeschlagen: {}", fehler.meldung());
        }
    }
}

/// Sendet einen ARP-REQUEST als Broadcast, um die MAC hinter `ziel_ip`
/// aufzulösen. Braucht eine konfigurierte IP (als Absender-IP) — sonst
/// `NetzFehler::NichtKonfiguriert`.
pub fn anfrage_senden(ziel_ip: Ipv4) -> Result<(), NetzFehler> {
    let unsere_ip = super::unsere_ip().ok_or(NetzFehler::NichtKonfiguriert)?;
    let unsere_mac = super::mac().ok_or(NetzFehler::KeinGeraet)?;
    let anfrage = ArpPaket::anfrage(unsere_mac, unsere_ip, ziel_ip);
    let frame = ethernet::rahmen_bauen(BROADCAST, unsere_mac, ETHERTYPE_ARP, &anfrage.bauen());
    super::sende_frame(&frame)
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Bauen und Parsen sind invers — ein gebauter Request/Reply parst
    /// wieder zu genau denselben Feldern.
    #[test_case]
    fn test_arp_bau_und_parse() {
        let unsere_mac = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
        let unsere_ip = Ipv4([10, 0, 2, 15]);
        let ziel_ip = Ipv4([10, 0, 2, 2]);

        // Ein Request:
        let anfrage = ArpPaket::anfrage(unsere_mac, unsere_ip, ziel_ip);
        let bytes = anfrage.bauen();
        assert_eq!(bytes.len(), PAKET_LEN);
        // op-Feld (Offset 6/7) = 1 (Request):
        assert_eq!(&bytes[6..8], &[0x00, 0x01]);

        let zurueck = ArpPaket::parse(&bytes).expect("gebautes ARP muss parsen");
        assert_eq!(zurueck, anfrage);
        assert_eq!(zurueck.operation, OP_REQUEST);
        assert_eq!(zurueck.ziel_mac, [0; 6]); // Request: Ziel-MAC leer

        // Eine Antwort:
        let frager_mac = [0x02, 0, 0, 0, 0, 0x01];
        let antwort = ArpPaket::antwort(unsere_mac, unsere_ip, frager_mac, Ipv4([10, 0, 2, 3]));
        let zurueck2 = ArpPaket::parse(&antwort.bauen()).unwrap();
        assert_eq!(zurueck2.operation, OP_REPLY);
        assert_eq!(zurueck2.ziel_mac, frager_mac);
    }

    /// Ein zu kurzes oder fremdes (Nicht-IPv4/Ethernet) ARP-Paket liefert
    /// None statt zu panicken.
    #[test_case]
    fn test_arp_parse_abweisung() {
        // Zu kurz:
        assert!(ArpPaket::parse(&[0u8; 10]).is_none());
        // Richtige Länge, aber htype != Ethernet (hier 0x0006):
        let mut falsch = ArpPaket::anfrage([0; 6], Ipv4::NULL, Ipv4::NULL).bauen();
        falsch[1] = 0x06; // htype-Low-Byte verbiegen
        assert!(ArpPaket::parse(&falsch).is_none());
    }

    /// Der Cache lernt eine Zuordnung, findet sie VOR dem Timeout und
    /// vergisst sie DANACH — der Kern von „Cache mit Timeout".
    #[test_case]
    fn test_arp_cache_timeout() {
        let mut cache = ArpCache::neu();
        let ip = Ipv4([10, 0, 2, 2]);
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

        // Bei t = 1000 ms gelernt:
        cache.einfuegen(ip, mac, 1000);
        // Kurz danach gefunden:
        assert_eq!(cache.suchen(ip, 1000), Some(mac));
        // Genau am Ablauf-Rand (1000 + TTL) noch gültig:
        assert_eq!(cache.suchen(ip, 1000 + CACHE_TTL_MS), Some(mac));
        // Eine Millisekunde später abgelaufen:
        assert_eq!(cache.suchen(ip, 1000 + CACHE_TTL_MS + 1), None);
        // Unbekannte IP ist immer None:
        assert_eq!(cache.suchen(Ipv4([1, 1, 1, 1]), 1000), None);

        // eintraege() filtert Abgelaufene ebenfalls heraus:
        assert_eq!(cache.eintraege(1000).len(), 1);
        assert_eq!(cache.eintraege(1000 + CACHE_TTL_MS + 1).len(), 0);

        // Ein zweiter Eintrag + aufraeumen entfernt nur den alten:
        cache.einfuegen(Ipv4([10, 0, 2, 3]), mac, 200_000);
        cache.aufraeumen(200_000);
        assert_eq!(cache.suchen(ip, 200_000), None); // alter weg
        assert_eq!(cache.suchen(Ipv4([10, 0, 2, 3]), 200_000), Some(mac));
    }
}
