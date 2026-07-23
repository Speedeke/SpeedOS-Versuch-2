// netz/mod.rs — Der Netzwerk-Stack von SpeedOS (Serie 5)
//
// Serie 4 hat SpeedOS PERSISTENT gemacht; Serie 5 bringt es ans NETZ.
// Schritt 1 (virtio/net.rs) war der reine interrupt-getriebene Empfang
// (Hexdump, kein Stack). HIER beginnt der Stack — mit klaren Schicht-
// Grenzen, wie die Bestandsaufnahme (docs/serie5-netzwerk.md) sie fordert:
//
//   netz::geraet    — die geräteunabhängige NIC-Naht (Trait `NetzGeraet`),
//                     analog zu `BlockDevice`. Der Stack redet NUR hiermit.
//   netz::puffer    — die Byte-Puffer-Abstraktion (Leser/Schreiber) für
//                     RX-Parsing und TX-Bau, wiederverwendbar.
//   netz::ethernet  — Schicht 2: Ethernet-Frames parsen/bauen.
//   netz::arp       — Adressauflösung IP <-> MAC (Cache, Request/Reply).
//   (ip/udp/tcp folgen in weiteren Stufen.)
//
// DER DREH- UND ANGELPUNKT ist der async `netz_task`: Er wird vom
// Geräte-IRQ geweckt, holt die empfangenen Frames vom `NetzGeraet` und
// DISPATCHT sie nach EtherType an die passende obere Schicht (ARP heute,
// IPv4 folgt). Der IRQ-Handler bleibt minimal (nur wecken) — genau das
// Tastatur-/Maus-Muster.
//
// STATISCHE IP-KONFIGURATION: Erstmal setzt der Nutzer IP/Maske/Gateway
// selbst (`netz-ip` in der Shell); DHCP kommt in einer späteren Stufe.

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod ethernet;
pub mod geraet;
pub mod icmp;
pub mod http;
pub mod ipv4;
pub mod puffer;
pub mod socket;
pub mod tcp;
pub mod udp;

pub use ethernet::Mac;
pub use geraet::{NetzFehler, NetzGeraet};

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

// ---------------------------------------------------------------------------
// Ipv4 — eine 4-Byte-IP-Adresse mit Anzeige und Parser
// ---------------------------------------------------------------------------

/// Eine IPv4-Adresse (vier Oktette, z. B. 10.0.2.15). Bewusst ein
/// Newtype über `[u8; 4]` — so ist sie typsicher von einer MAC oder
/// beliebigen Bytes zu unterscheiden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Ipv4(pub [u8; 4]);

impl Ipv4 {
    /// Die Adresse 0.0.0.0 (unspezifiziert / „noch keine").
    pub const NULL: Ipv4 = Ipv4([0, 0, 0, 0]);

    /// Die vier Oktette als Array.
    pub fn oktette(&self) -> [u8; 4] {
        self.0
    }

    /// Zerlegt eine Punkt-Notation ("10.0.2.15") in eine Ipv4. None bei
    /// falschem Format (nicht genau vier Oktette 0..=255).
    pub fn parse(text: &str) -> Option<Ipv4> {
        let mut oktette = [0u8; 4];
        let mut anzahl = 0usize;
        for teil in text.trim().split('.') {
            if anzahl >= 4 {
                return None; // mehr als vier Teile
            }
            oktette[anzahl] = teil.parse::<u8>().ok()?;
            anzahl += 1;
        }
        if anzahl == 4 {
            Some(Ipv4(oktette))
        } else {
            None
        }
    }
}

impl core::fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

// ---------------------------------------------------------------------------
// Statische Netz-Konfiguration (IP / Maske / Gateway)
// ---------------------------------------------------------------------------

/// Woher die IP-Konfiguration stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quelle {
    /// Noch nichts gesetzt.
    Keine,
    /// Von Hand gesetzt (`netz-ip`).
    Statisch,
    /// Per DHCP bezogen.
    Dhcp,
}

/// Die IP-Konfiguration: Adresse, Maske, Gateway, DNS-Server und (bei DHCP)
/// die Lease-Dauer. `gesetzt` unterscheidet „bewusst konfiguriert" von der
/// Null-Vorgabe.
#[derive(Debug, Clone, Copy)]
pub struct NetzKonfig {
    pub ip: Ipv4,
    pub maske: Ipv4,
    pub gateway: Ipv4,
    /// Primärer DNS-Server (0.0.0.0 = keiner bekannt).
    pub dns: Ipv4,
    pub gesetzt: bool,
    pub quelle: Quelle,
    /// Lease-Dauer in Sekunden (nur bei DHCP; 0 sonst).
    pub lease_sekunden: u32,
}

/// Die aktuelle Konfiguration (Blatt-Lock, nur aus Task-Kontext). Beginnt
/// leer — bis `netz-ip` sie setzt oder DHCP eine Lease bezieht.
static KONFIG: Mutex<NetzKonfig> = Mutex::new(NetzKonfig {
    ip: Ipv4::NULL,
    maske: Ipv4::NULL,
    gateway: Ipv4::NULL,
    dns: Ipv4::NULL,
    gesetzt: false,
    quelle: Quelle::Keine,
    lease_sekunden: 0,
});

/// Liest die aktuelle Konfiguration (Kopie).
pub fn konfig() -> NetzKonfig {
    without_interrupts(|| *KONFIG.lock())
}

/// Setzt die STATISCHE IP-Konfiguration (Shell-Befehl `netz-ip`). DNS bleibt
/// leer (per DHCP oder später separat).
pub fn konfig_setzen(ip: Ipv4, maske: Ipv4, gateway: Ipv4) {
    without_interrupts(|| {
        *KONFIG.lock() = NetzKonfig {
            ip,
            maske,
            gateway,
            dns: Ipv4::NULL,
            gesetzt: true,
            quelle: Quelle::Statisch,
            lease_sekunden: 0,
        };
    });
}

/// Übernimmt eine per DHCP bezogene Konfiguration (inkl. DNS + Lease).
pub fn konfig_setzen_dhcp(ip: Ipv4, maske: Ipv4, gateway: Ipv4, dns: Ipv4, lease_sekunden: u32) {
    without_interrupts(|| {
        *KONFIG.lock() = NetzKonfig {
            ip,
            maske,
            gateway,
            dns,
            gesetzt: true,
            quelle: Quelle::Dhcp,
            lease_sekunden,
        };
    });
}

/// Unsere konfigurierte IP (None, solange nichts gesetzt ist).
pub fn unsere_ip() -> Option<Ipv4> {
    let k = konfig();
    if k.gesetzt {
        Some(k.ip)
    } else {
        None
    }
}

/// Der bekannte DNS-Server (None, wenn keiner konfiguriert ist).
pub fn dns_server() -> Option<Ipv4> {
    let k = konfig();
    if k.dns != Ipv4::NULL {
        Some(k.dns)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Bequeme Durchreichen an die Geräte-Naht (der Stack ruft `netz::…`)
// ---------------------------------------------------------------------------

/// Ist eine Netzwerkkarte registriert?
pub fn vorhanden() -> bool {
    geraet::vorhanden()
}

/// Unsere MAC-Adresse (None ohne NIC).
pub fn mac() -> Option<Mac> {
    geraet::mac()
}

/// Sendet ein rohes Ethernet-Frame über die registrierte NIC.
pub fn sende_frame(frame: &[u8]) -> Result<(), NetzFehler> {
    geraet::sende_frame(frame)
}

// ---------------------------------------------------------------------------
// „netz-lausch": rohe Frames hexdumpen (an/aus)
// ---------------------------------------------------------------------------

/// Schaltet den Roh-Hexdump empfangener Frames an/aus.
static LAUSCH_AKTIV: AtomicBool = AtomicBool::new(false);

/// Schaltet den Hexdump um und liefert den neuen Zustand.
pub fn lausch_umschalten() -> bool {
    let neu = !LAUSCH_AKTIV.load(Ordering::Relaxed);
    LAUSCH_AKTIV.store(neu, Ordering::Relaxed);
    neu
}

// ---------------------------------------------------------------------------
// Der Dispatch: empfangene Frames nach EtherType verteilen
// ---------------------------------------------------------------------------

/// Verarbeitet ALLE gerade bereitliegenden Frames: einsammeln (Geräte-Lock
/// wird dabei losgelassen), optional hexdumpen, dann nach EtherType
/// verteilen. Synchron — deshalb auch von einem Shell-Befehl aufrufbar,
/// der den Empfang „pumpen" will (arp-ping), ohne dass der `netz_task`
/// läuft (der kooperative Executor gibt während eines synchronen Befehls
/// keinem anderen Task Zeit).
pub fn rx_verarbeiten() {
    let frames = geraet::frames_einsammeln();
    if frames.is_empty() {
        return;
    }
    let lauschen = LAUSCH_AKTIV.load(Ordering::Relaxed);
    for frame in &frames {
        if lauschen {
            ethernet::hexdump(frame);
        }
        dispatch(frame);
    }
    // Nach dem Dispatch: zurückgestellte IP-Pakete ausliefern, deren
    // Next-Hop-MAC gerade per ARP-Antwort bekannt geworden ist.
    ipv4::ausstehend_ausliefern();
}

/// Verteilt EIN Frame an die passende obere Schicht (nach EtherType).
fn dispatch(frame: &[u8]) {
    let (kopf, nutzlast) = match ethernet::parse(frame) {
        Some(paar) => paar,
        None => return, // zu kurz / kaputt
    };
    match kopf.ethertype {
        ethernet::ETHERTYPE_ARP => arp::verarbeiten(nutzlast),
        ethernet::ETHERTYPE_IPV4 => ipv4::verarbeiten(nutzlast),
        _ => {}
    }
}

/// Der async netz_task (DER Dreh- und Angelpunkt): vom Geräte-IRQ geweckt,
/// holt die empfangenen Frames und dispatcht sie. Läuft im Executor als
/// eigener Task (main.rs), solange SpeedOS läuft.
pub async fn netz_task() {
    loop {
        geraet::rx_warten().await;
        pumpen();
    }
}

/// EIN vollständiger Netz-Schritt: Empfangenes verarbeiten UND die Sockets
/// bedienen (Timer ticken, erzeugte Segmente wirklich senden). Das ist der
/// Takt, den sowohl der `netz_task` als auch jede synchrone Pump-Schleife
/// (HTTP-Client, Shell-Befehle) benutzt.
pub fn pumpen() {
    rx_verarbeiten();
    socket::bedienen();
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Ipv4::parse akzeptiert gültige Adressen und weist Unsinn ab; die
    /// Anzeige ist die Umkehrung.
    #[test_case]
    fn test_ipv4_parse_und_anzeige() {
        let ip = Ipv4::parse("10.0.2.15").unwrap();
        assert_eq!(ip.oktette(), [10, 0, 2, 15]);
        assert_eq!(alloc::format!("{}", ip), "10.0.2.15");

        // Randwert 255 geht, 256 nicht (u8-Überlauf):
        assert_eq!(Ipv4::parse("255.255.255.0"), Some(Ipv4([255, 255, 255, 0])));
        assert!(Ipv4::parse("256.0.0.1").is_none());
        // Falsche Anzahl Oktette:
        assert!(Ipv4::parse("1.2.3").is_none());
        assert!(Ipv4::parse("1.2.3.4.5").is_none());
        assert!(Ipv4::parse("").is_none());
        assert!(Ipv4::parse("abc").is_none());
    }

    /// konfig_setzen/unsere_ip spielen zusammen; vor dem Setzen ist keine
    /// IP da. (Läuft nach test_ipv4_parse; setzt globale Konfig — harmlos.)
    #[test_case]
    fn test_konfig() {
        konfig_setzen(Ipv4([192, 168, 1, 50]), Ipv4([255, 255, 255, 0]), Ipv4([192, 168, 1, 1]));
        let k = konfig();
        assert!(k.gesetzt);
        assert_eq!(k.ip, Ipv4([192, 168, 1, 50]));
        assert_eq!(unsere_ip(), Some(Ipv4([192, 168, 1, 50])));
    }

    // --- Der Meilenstein "SpeedOS antwortet auf ARP", geräteunabhängig ---
    // Eine Mock-NIC (erfüllt NetzGeraet) fängt gesendete Frames ab und
    // liefert vorbereitete Empfangs-Frames. So lässt sich der ganze
    // Dispatch->ARP->Antwort-Pfad OHNE echte Hardware beweisen.

    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use spin::Mutex;

    /// Von der Mock-NIC gesendete Frames (der Test liest sie danach aus).
    static MOCK_GESENDET: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
    /// Frames, die die Mock-NIC beim nächsten Empfang herausgibt (FIFO).
    static MOCK_RX: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

    struct MockNic {
        mac: Mac,
    }

    impl NetzGeraet for MockNic {
        fn mac(&self) -> Mac {
            self.mac
        }
        fn sende_frame(&mut self, frame: &[u8]) -> Result<(), NetzFehler> {
            MOCK_GESENDET.lock().push(frame.to_vec());
            Ok(())
        }
        fn empfange_frame(&mut self) -> Option<Vec<u8>> {
            let mut q = MOCK_RX.lock();
            if q.is_empty() {
                None
            } else {
                Some(q.remove(0))
            }
        }
    }

    /// Kommt ein ARP-Request nach UNSERER IP herein, muss SpeedOS mit
    /// einer korrekten ARP-Antwort (unsere MAC) reagieren — und dabei den
    /// Frager lernen. Das ist der Serie-5-Meilenstein, hier ohne slirp.
    #[test_case]
    fn test_arp_antwort_meilenstein() {
        MOCK_GESENDET.lock().clear();
        MOCK_RX.lock().clear();

        let unsere_mac = [0x52, 0x54, 0x00, 0x11, 0x22, 0x33];
        let unsere_ip = Ipv4([10, 0, 2, 15]);
        konfig_setzen(unsere_ip, Ipv4([255, 255, 255, 0]), Ipv4([10, 0, 2, 2]));
        geraet::geraet_registrieren(Box::new(MockNic { mac: unsere_mac }));

        // Ein ARP-REQUEST "wer hat 10.0.2.15?" von 10.0.2.2 (aa:bb:cc:...).
        let frager_mac = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
        let frager_ip = Ipv4([10, 0, 2, 2]);
        let anfrage = arp::ArpPaket::anfrage(frager_mac, frager_ip, unsere_ip);
        let request_frame = ethernet::rahmen_bauen(
            ethernet::BROADCAST,
            frager_mac,
            ethernet::ETHERTYPE_ARP,
            &anfrage.bauen(),
        );
        MOCK_RX.lock().push(request_frame);

        // Dispatch genau wie der netz_task: einsammeln + verteilen.
        rx_verarbeiten();

        // GENAU EINE Antwort, und zwar eine korrekte ARP-Reply.
        let gesendet = MOCK_GESENDET.lock().clone();
        assert_eq!(gesendet.len(), 1, "genau eine ARP-Antwort erwartet");
        let (kopf, nutzlast) =
            ethernet::parse(&gesendet[0]).expect("Antwort muss ein Ethernet-Frame sein");
        assert_eq!(kopf.ziel, frager_mac, "Antwort als Unicast an den Frager");
        assert_eq!(kopf.quelle, unsere_mac, "Antwort von unserer MAC");
        assert_eq!(kopf.ethertype, ethernet::ETHERTYPE_ARP);
        let antwort = arp::ArpPaket::parse(nutzlast).expect("ARP-Nutzlast muss parsen");
        assert_eq!(antwort.operation, arp::OP_REPLY);
        assert_eq!(antwort.absender_mac, unsere_mac, "unsere MAC in der Antwort");
        assert_eq!(antwort.absender_ip, unsere_ip);
        assert_eq!(antwort.ziel_ip, frager_ip);

        // Der Frager wurde gelernt (jede ARP-Nachricht lehrt sha -> spa).
        assert_eq!(arp::cache_suchen(frager_ip), Some(frager_mac));

        // Aufräumen: Mock-NIC entfernen (spätere Tests: "keine NIC").
        geraet::geraet_zuruecksetzen();
        MOCK_GESENDET.lock().clear();
        MOCK_RX.lock().clear();
    }

    /// Der Ping-Meilenstein, geräteunabhängig: kommt ein ICMP-Echo-Request
    /// an UNSERE IP herein, muss SpeedOS mit einem korrekten Echo-Reply
    /// antworten (Identifier/Sequenz/Daten gespiegelt, gültige Prüfsummen).
    #[test_case]
    fn test_icmp_echo_antwort_meilenstein() {
        MOCK_GESENDET.lock().clear();
        MOCK_RX.lock().clear();

        let unsere_mac = [0x52, 0x54, 0x00, 0x11, 0x22, 0x33];
        let unsere_ip = Ipv4([10, 0, 2, 15]);
        konfig_setzen(unsere_ip, Ipv4([255, 255, 255, 0]), Ipv4([10, 0, 2, 2]));
        geraet::geraet_registrieren(Box::new(MockNic { mac: unsere_mac }));

        // Damit die Antwort SOFORT rausgeht (nicht auf ARP wartet), die
        // MAC des Pingers vorab in den Cache legen.
        let pinger_mac = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
        let pinger_ip = Ipv4([10, 0, 2, 2]);
        arp::cache_einfuegen(pinger_ip, pinger_mac);

        // Einen Echo-Request "an uns" bauen: Ethernet -> IPv4 -> ICMP.
        let daten = [1, 2, 3, 4, 5, 6, 7, 8];
        let echo = icmp::echo_bauen(icmp::TYP_ECHO_REQUEST, 0x1234, 7, &daten);
        let ip = ipv4::bauen(pinger_ip, unsere_ip, ipv4::PROTO_ICMP, &echo);
        let frame =
            ethernet::rahmen_bauen(unsere_mac, pinger_mac, ethernet::ETHERTYPE_IPV4, &ip);
        MOCK_RX.lock().push(frame);

        // Dispatch wie der netz_task.
        rx_verarbeiten();

        // GENAU EINE Antwort: ein Echo-Reply zurück an den Pinger.
        let gesendet = MOCK_GESENDET.lock().clone();
        assert_eq!(gesendet.len(), 1, "genau ein Echo-Reply erwartet");
        let (ekopf, enutz) = ethernet::parse(&gesendet[0]).expect("Ethernet");
        assert_eq!(ekopf.ethertype, ethernet::ETHERTYPE_IPV4);
        assert_eq!(ekopf.ziel, pinger_mac);
        assert_eq!(ekopf.quelle, unsere_mac);
        let (ipkopf, ipnutz) = ipv4::parse(enutz).expect("IPv4 (gueltige Pruefsumme)");
        assert_eq!(ipkopf.protokoll, ipv4::PROTO_ICMP);
        assert_eq!(ipkopf.quelle, unsere_ip, "Antwort von unserer IP");
        assert_eq!(ipkopf.ziel, pinger_ip);
        let (ckopf, cdaten) = icmp::echo_parse(ipnutz).expect("ICMP-Echo");
        assert_eq!(ckopf.typ, icmp::TYP_ECHO_REPLY);
        assert_eq!(ckopf.identifier, 0x1234, "Identifier gespiegelt");
        assert_eq!(ckopf.sequenz, 7, "Sequenz gespiegelt");
        assert_eq!(cdaten, &daten, "Daten gespiegelt");

        // Aufräumen.
        geraet::geraet_zuruecksetzen();
        MOCK_GESENDET.lock().clear();
        MOCK_RX.lock().clear();
    }
}
