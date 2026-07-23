// netz/socket.rs — Die öffentliche Socket-API von SpeedOS
//
// Das ist die FASSADE des Netz-Stacks: Anwendungen (heute Shell-Befehle und
// der HTTP-Client, morgen User-Space-Programme) reden nur noch hierüber —
// nie mit tcp::Verbindung oder udp::binden direkt.
//
// DIE NAHT FÜR SERIE 6 (User-Space), so gelegt, dass später nur noch
// Syscalls davorgesetzt werden müssen:
//   * HANDLES statt Zeiger. Ein `Handle` ist eine undurchsichtige Zahl; nach
//     außen geht NIE ein Kernel-Zeiger. Ein ungültiges/geschlossenes Handle
//     liefert sauber `SocketFehler::UngueltigerHandle` — es kann prinzipiell
//     nichts "erraten" werden, weil die IDs monoton wachsen (kein Recycling).
//   * PUFFER-OWNERSHIP explizit: `senden` KOPIERT die Daten HINEIN (copy-in),
//     `empfangen` KOPIERT sie HERAUS (copy-out) — in vom Aufrufer gestellte
//     Slices. Der Kernel gibt niemals einen Puffer heraus, den der Aufrufer
//     behält. Genau diese Grenze wird später zur Kernel/User-Grenze.
//   * KLARE FEHLER-ENUMS statt Zahlen-Codes; jeder Fehler hat eine deutsche
//     Meldung für die Oberfläche.
//   * TLS-AGNOSTISCH: Die API kennt nur Bytes. TLS wäre später eine Schicht
//     ÜBER dem TCP-Socket (sie umhüllt den Bytestrom) — hier ist dafür
//     bewusst nichts vorgesehen und nichts im Weg.
//
// TCP UND UDP über dieselbe API: `oeffnen(SocketTyp::Tcp|Udp)`, danach
// `binden`/`verbinden`/`senden`/`empfangen`/`schliessen`. Intern trägt ein
// TCP-Socket die Zustandsmaschine (`tcp::Verbindung`), ein UDP-Socket nutzt
// den bewährten Port-Demux aus `udp`.
//
// BEDIENT wird alles von `bedienen()`: Timer ticken und die von den
// Verbindungen erzeugten Segmente wirklich verschicken. Das ruft der
// `netz_task` (bzw. der synchrone Pump-Pfad eines Shell-Befehls).

use super::ipv4::{self, PROTO_TCP};
use super::tcp::{self, Verbindung, Zustand as TcpZustand};
use super::udp;
use super::Ipv4;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// Ein Socket-HANDLE — eine undurchsichtige Zahl, KEIN Kernel-Zeiger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(u32);

/// Welche Transportart ein Socket spricht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketTyp {
    Tcp,
    Udp,
}

/// Alle Fehler der Socket-API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketFehler {
    /// Das Handle gibt es nicht (mehr) — z. B. nach `schliessen`.
    UngueltigerHandle,
    /// Die Operation passt nicht zum Socket-Typ.
    FalscherTyp,
    /// Der Socket ist (noch) nicht verbunden bzw. hat kein Ziel.
    NichtVerbunden,
    /// Der Socket ist bereits verbunden/lauschend.
    BereitsVerbunden,
    /// Für diese Operation muss erst `binden` gerufen werden.
    NichtGebunden,
    /// Es sind zu viele Sockets offen.
    KeinPlatz,
    /// Keine IP konfiguriert (erst DHCP oder netz-ip).
    NichtKonfiguriert,
    /// Keine Netzwerkkarte vorhanden.
    KeinGeraet,
    /// Die Gegenstelle hat abgelehnt / die Verbindung brach ab.
    Abgebrochen,
    /// Die Operation lief in den Timeout.
    Zeitueberschreitung,
}

impl SocketFehler {
    pub fn meldung(&self) -> &'static str {
        match self {
            SocketFehler::UngueltigerHandle => "ungueltiges Socket-Handle (schon geschlossen?)",
            SocketFehler::FalscherTyp => "Operation passt nicht zum Socket-Typ",
            SocketFehler::NichtVerbunden => "der Socket ist nicht verbunden",
            SocketFehler::BereitsVerbunden => "der Socket ist bereits verbunden",
            SocketFehler::NichtGebunden => "der Socket ist an keinen Port gebunden",
            SocketFehler::KeinPlatz => "zu viele offene Sockets",
            SocketFehler::NichtKonfiguriert => "keine IP konfiguriert (erst dhcp oder netz-ip)",
            SocketFehler::KeinGeraet => "keine Netzwerkkarte vorhanden",
            SocketFehler::Abgebrochen => "die Verbindung wurde abgebrochen",
            SocketFehler::Zeitueberschreitung => "Zeitueberschreitung",
        }
    }
}

/// Der Verbindungszustand, wie ihn die Anwendung sieht (die TCP-Interna
/// bleiben drinnen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbindungszustand {
    /// Frisch geöffnet, weder gebunden noch verbunden.
    Neu,
    /// Wartet auf eingehende Verbindungen.
    Lauscht,
    /// Handshake läuft.
    Verbindet,
    /// Verbunden — Daten können fließen.
    Verbunden,
    /// Die Gegenstelle hat ihre Senderichtung beendet (alles empfangen).
    PeerHatGeschlossen,
    /// Endgültig zu.
    Geschlossen,
}

/// Was hinter einem Handle steckt (intern — nie nach außen sichtbar).
enum Inhalt {
    /// TCP vor `verbinden`/`lauschen`: nur ein optionaler lokaler Port.
    TcpLeer { lokaler_port: Option<u16> },
    /// TCP mit laufender Zustandsmaschine.
    Tcp(Verbindung),
    /// UDP: gebundener Port (0 = noch keiner) + optionales Standard-Ziel.
    Udp { port: u16, ziel: Option<(Ipv4, u16)> },
}

struct Eintrag {
    id: u32,
    inhalt: Inhalt,
    /// Handle vom Nutzer freigegeben (`schliessen`): ab jetzt ungültig; der
    /// Eintrag lebt nur noch, bis der geordnete TCP-Abbau fertig ist.
    freigegeben: bool,
}

/// So viele Sockets dürfen gleichzeitig existieren (inkl. der Sockets, die
/// gerade ihren TIME_WAIT absitzen).
const MAX_SOCKETS: usize = 32;

/// Die Socket-Tabelle (Blatt-Lock, nur aus Task-Kontext).
static SOCKETS: Mutex<Vec<Eintrag>> = Mutex::new(Vec::new());
/// Monoton wachsende Handle-IDs — nie recycelt, damit ein altes Handle nie
/// versehentlich einen neuen Socket trifft.
static NAECHSTE_ID: AtomicU32 = AtomicU32::new(1);

/// Sucht den (noch gültigen) Eintrag zu einem Handle.
fn finde(tabelle: &mut [Eintrag], h: Handle) -> Result<&mut Eintrag, SocketFehler> {
    tabelle
        .iter_mut()
        .find(|e| e.id == h.0 && !e.freigegeben)
        .ok_or(SocketFehler::UngueltigerHandle)
}

/// Entfernt fertig abgebaute, freigegebene Sockets.
fn aufraeumen(tabelle: &mut Vec<Eintrag>) {
    tabelle.retain(|e| {
        if !e.freigegeben {
            return true;
        }
        match &e.inhalt {
            // Ein TCP-Socket bleibt, bis der Abbau (inkl. TIME_WAIT) durch ist.
            Inhalt::Tcp(v) => v.zustand() != TcpZustand::Closed,
            _ => false,
        }
    });
}

// ---------------------------------------------------------------------------
// Die API
// ---------------------------------------------------------------------------

/// Öffnet einen neuen Socket und liefert sein Handle.
pub fn oeffnen(typ: SocketTyp) -> Result<Handle, SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        aufraeumen(&mut tabelle);
        if tabelle.len() >= MAX_SOCKETS {
            return Err(SocketFehler::KeinPlatz);
        }
        let id = NAECHSTE_ID.fetch_add(1, Ordering::Relaxed);
        let inhalt = match typ {
            SocketTyp::Tcp => Inhalt::TcpLeer { lokaler_port: None },
            SocketTyp::Udp => Inhalt::Udp { port: 0, ziel: None },
        };
        tabelle.push(Eintrag {
            id,
            inhalt,
            freigegeben: false,
        });
        Ok(Handle(id))
    })
}

/// Bindet den Socket an einen lokalen Port.
pub fn binden(h: Handle, port: u16) -> Result<(), SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::TcpLeer { lokaler_port } => {
                *lokaler_port = Some(port);
                Ok(())
            }
            Inhalt::Udp { port: p, .. } => {
                udp::binden(port); // der Port-Demux aus udp.rs
                *p = port;
                Ok(())
            }
            Inhalt::Tcp(_) => Err(SocketFehler::BereitsVerbunden),
        }
    })
}

/// TCP: macht den Socket zum passiven Öffner (wartet auf eingehende SYNs).
pub fn lauschen(h: Handle) -> Result<(), SocketFehler> {
    let lokale_ip = super::unsere_ip().ok_or(SocketFehler::NichtKonfiguriert)?;
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &e.inhalt {
            Inhalt::TcpLeer {
                lokaler_port: Some(p),
            } => {
                e.inhalt = Inhalt::Tcp(Verbindung::lauschen(lokale_ip, *p, tcp::isn()));
                Ok(())
            }
            Inhalt::TcpLeer { lokaler_port: None } => Err(SocketFehler::NichtGebunden),
            _ => Err(SocketFehler::FalscherTyp),
        }
    })
}

/// Verbindet den Socket mit `ziel_ip:ziel_port`. TCP beginnt den Handshake
/// (nicht blockierend — den Fortschritt zeigt `zustand`); UDP merkt sich das
/// Ziel für `senden`.
pub fn verbinden(h: Handle, ziel_ip: Ipv4, ziel_port: u16) -> Result<(), SocketFehler> {
    // NUR TCP braucht IP+NIC (es beginnt sofort den Handshake). UDP merkt sich
    // bloß das Ziel — dafür muss noch gar nichts konfiguriert sein.
    let lokale_ip_opt = super::unsere_ip();
    let mac_opt = super::mac();
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::TcpLeer { lokaler_port } => {
                let lokale_ip = lokale_ip_opt.ok_or(SocketFehler::NichtKonfiguriert)?;
                mac_opt.ok_or(SocketFehler::KeinGeraet)?;
                let port = lokaler_port.unwrap_or_else(tcp::ephemerer_port);
                e.inhalt = Inhalt::Tcp(Verbindung::verbinden_aktiv(
                    lokale_ip,
                    port,
                    ziel_ip,
                    ziel_port,
                    tcp::isn(),
                    jetzt,
                ));
                Ok(())
            }
            Inhalt::Udp { ziel, port } => {
                if *port == 0 {
                    let p = tcp::ephemerer_port();
                    udp::binden(p);
                    *port = p;
                }
                *ziel = Some((ziel_ip, ziel_port));
                Ok(())
            }
            Inhalt::Tcp(_) => Err(SocketFehler::BereitsVerbunden),
        }
    })
}

/// Sendet Daten. PUFFER-OWNERSHIP: die Bytes werden HINEIN kopiert (copy-in);
/// der Aufrufer behält seinen Puffer. Liefert die übernommene Anzahl (bei TCP
/// durch den Sendepuffer begrenzt).
pub fn senden(h: Handle, daten: &[u8]) -> Result<usize, SocketFehler> {
    let jetzt = crate::zeit::ms_seit_boot();
    /// Was nach dem Loslassen des Locks noch zu tun ist.
    enum Auftrag {
        Fertig(usize),
        UdpSenden(Ipv4, u16, u16),
    }
    let auftrag = without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::Tcp(v) => {
                if !matches!(v.zustand(), TcpZustand::Established | TcpZustand::CloseWait) {
                    return Err(SocketFehler::NichtVerbunden);
                }
                Ok(Auftrag::Fertig(v.senden(daten, jetzt)))
            }
            Inhalt::Udp { port, ziel } => {
                let (zi, zp) = ziel.ok_or(SocketFehler::NichtVerbunden)?;
                Ok(Auftrag::UdpSenden(zi, zp, *port))
            }
            Inhalt::TcpLeer { .. } => Err(SocketFehler::NichtVerbunden),
        }
    })?;
    match auftrag {
        Auftrag::Fertig(n) => Ok(n),
        // UDP AUSSERHALB des Socket-Locks senden (udp::senden nimmt die
        // Geräte-/ARP-Locks — nie verschachteln).
        Auftrag::UdpSenden(zi, zp, qp) => {
            udp::senden(zi, qp, zp, daten).map_err(|_| SocketFehler::Abgebrochen)?;
            Ok(daten.len())
        }
    }
}

/// Holt empfangene Daten ab. PUFFER-OWNERSHIP: die Bytes werden in den vom
/// Aufrufer gestellten Slice HERAUS kopiert (copy-out). 0 = gerade nichts da.
pub fn empfangen(h: Handle, ziel: &mut [u8]) -> Result<usize, SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::Tcp(v) => Ok(v.empfangen(ziel)),
            Inhalt::Udp { port, .. } => match udp::empfangen(*port) {
                Some(d) => {
                    let n = d.daten.len().min(ziel.len());
                    ziel[..n].copy_from_slice(&d.daten[..n]);
                    Ok(n)
                }
                None => Ok(0),
            },
            Inhalt::TcpLeer { .. } => Err(SocketFehler::NichtVerbunden),
        }
    })
}

/// UDP: empfängt ein Datagramm samt Absender (n, Absender-IP, Absender-Port).
pub fn empfangen_von(h: Handle, ziel: &mut [u8]) -> Result<(usize, Ipv4, u16), SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::Udp { port, .. } => match udp::empfangen(*port) {
                Some(d) => {
                    let n = d.daten.len().min(ziel.len());
                    ziel[..n].copy_from_slice(&d.daten[..n]);
                    Ok((n, d.quell_ip, d.quell_port))
                }
                None => Ok((0, Ipv4::NULL, 0)),
            },
            _ => Err(SocketFehler::FalscherTyp),
        }
    })
}

/// Der aktuelle Zustand des Sockets.
pub fn zustand(h: Handle) -> Result<Verbindungszustand, SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        Ok(match &e.inhalt {
            Inhalt::TcpLeer { .. } => Verbindungszustand::Neu,
            Inhalt::Tcp(v) => match v.zustand() {
                TcpZustand::Closed => Verbindungszustand::Geschlossen,
                TcpZustand::Listen => Verbindungszustand::Lauscht,
                TcpZustand::SynSent | TcpZustand::SynRcvd => Verbindungszustand::Verbindet,
                // Wir haben zwar zugemacht, empfangen aber noch:
                TcpZustand::Established | TcpZustand::FinWait1 | TcpZustand::FinWait2 => {
                    Verbindungszustand::Verbunden
                }
                TcpZustand::CloseWait
                | TcpZustand::LastAck
                | TcpZustand::Closing
                | TcpZustand::TimeWait => Verbindungszustand::PeerHatGeschlossen,
            },
            Inhalt::Udp { ziel, .. } => {
                if ziel.is_some() {
                    Verbindungszustand::Verbunden
                } else {
                    Verbindungszustand::Neu
                }
            }
        })
    })
}

/// Wie viele Bytes sofort abholbar sind.
pub fn verfuegbar(h: Handle) -> Result<usize, SocketFehler> {
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        Ok(match &e.inhalt {
            Inhalt::Tcp(v) => v.empfangen_verfuegbar(),
            _ => 0,
        })
    })
}

/// Schließt den Socket: leitet den geordneten Abbau ein (TCP: FIN) und macht
/// das HANDLE sofort UNGÜLTIG. Der Abbau läuft im Hintergrund weiter
/// (`bedienen`), danach verschwindet der Eintrag.
pub fn schliessen(h: Handle) -> Result<(), SocketFehler> {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let e = finde(&mut tabelle, h)?;
        match &mut e.inhalt {
            Inhalt::Tcp(v) => v.schliessen(jetzt),
            Inhalt::Udp { port, .. } => {
                if *port != 0 {
                    udp::freigeben(*port);
                }
            }
            Inhalt::TcpLeer { .. } => {}
        }
        e.freigegeben = true;
        Ok(())
    })
}

/// Wie viele Sockets gerade in der Tabelle stehen (Diagnose/Tests).
pub fn anzahl() -> usize {
    without_interrupts(|| SOCKETS.lock().len())
}

// ---------------------------------------------------------------------------
// Bedienung: Timer ticken und erzeugte Segmente wirklich verschicken
// ---------------------------------------------------------------------------

/// BEDIENT alle Sockets: TCP-Timer ticken (Retransmits!), die von den
/// Zustandsmaschinen erzeugten Segmente einsammeln und per IPv4 senden,
/// fertige Sockets abräumen. Der `netz_task` ruft das nach jedem Empfang;
/// synchrone Pump-Schleifen (Shell) ebenso.
///
/// LOCK-ORDNUNG: Der Socket-Lock wird beim SENDEN NICHT gehalten —
/// `ipv4::senden` nimmt Geräte-/ARP-Locks.
pub fn bedienen() {
    let jetzt = crate::zeit::ms_seit_boot();
    let zu_senden: Vec<(Ipv4, Vec<Vec<u8>>)> = without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        let mut raus = Vec::new();
        for e in tabelle.iter_mut() {
            if let Inhalt::Tcp(v) = &mut e.inhalt {
                v.tick(jetzt);
                let segmente = v.ausgang_abholen();
                if !segmente.is_empty() {
                    raus.push((v.ferne_ip(), segmente));
                }
            }
        }
        aufraeumen(&mut tabelle);
        raus
    });
    for (ziel, segmente) in zu_senden {
        for seg in segmente {
            let _ = ipv4::senden(ziel, PROTO_TCP, &seg);
        }
    }
}

/// Stellt ein eingehendes TCP-Segment dem passenden Socket zu: zuerst per
/// exaktem 4-Tupel (lokaler Port, ferne IP, ferner Port), sonst einem
/// LAUSCHENDEN Socket auf diesem Port. Ruft `tcp::verarbeiten` (Dispatch).
pub(crate) fn tcp_zustellen(quell_ip: Ipv4, segment: &[u8]) {
    if segment.len() < 4 {
        return;
    }
    let quell_port = u16::from_be_bytes([segment[0], segment[1]]);
    let ziel_port = u16::from_be_bytes([segment[2], segment[3]]);
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        let mut tabelle = SOCKETS.lock();
        // 1. Exakter Treffer auf eine bestehende Verbindung.
        for e in tabelle.iter_mut() {
            if let Inhalt::Tcp(v) = &mut e.inhalt {
                if v.zustand() != TcpZustand::Listen
                    && v.lokaler_port() == ziel_port
                    && v.ferne_ip() == quell_ip
                    && v.ferner_port() == quell_port
                {
                    v.segment_empfangen(quell_ip, segment, jetzt);
                    return;
                }
            }
        }
        // 2. Sonst ein lauschender Socket auf diesem Port.
        for e in tabelle.iter_mut() {
            if let Inhalt::Tcp(v) = &mut e.inhalt {
                if v.zustand() == TcpZustand::Listen && v.lokaler_port() == ziel_port {
                    v.segment_empfangen(quell_ip, segment, jetzt);
                    return;
                }
            }
        }
    });
}

/// Der Socket-TAKT: tickt die Sockets auch dann, wenn gerade nichts empfangen
/// wird — sonst würden Retransmits ohne eingehenden Verkehr nie feuern.
pub async fn takt_task() {
    loop {
        crate::zeit::warte_ms(100).await;
        bedienen();
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Der HANDLE-LEBENSZYKLUS: öffnen liefert ein gültiges Handle, die
    /// Operationen greifen, `schliessen` macht es UNGÜLTIG — und ein zweites
    /// Schließen ebenso. IDs werden nie recycelt.
    #[test_case]
    fn test_socket_handle_lebenszyklus() {
        let vorher = anzahl();
        let h = oeffnen(SocketTyp::Udp).expect("oeffnen");
        assert_eq!(anzahl(), vorher + 1);

        // Frisch geöffnet: Zustand "Neu", noch kein Ziel -> senden scheitert.
        assert_eq!(zustand(h), Ok(Verbindungszustand::Neu));
        assert_eq!(senden(h, b"x"), Err(SocketFehler::NichtVerbunden));

        // Binden + Ziel setzen macht ihn benutzbar.
        binden(h, 51234).expect("binden");
        verbinden(h, Ipv4([10, 0, 2, 3]), 53).expect("verbinden (UDP: Ziel merken)");
        assert_eq!(zustand(h), Ok(Verbindungszustand::Verbunden));
        // Ohne eingegangenes Datagramm liefert empfangen 0 (kein Fehler).
        let mut buf = [0u8; 8];
        assert_eq!(empfangen(h, &mut buf), Ok(0));

        // Schließen -> Handle ungültig, jede weitere Operation scheitert.
        schliessen(h).expect("schliessen");
        assert_eq!(zustand(h), Err(SocketFehler::UngueltigerHandle));
        assert_eq!(empfangen(h, &mut buf), Err(SocketFehler::UngueltigerHandle));
        assert_eq!(senden(h, b"x"), Err(SocketFehler::UngueltigerHandle));
        assert_eq!(schliessen(h), Err(SocketFehler::UngueltigerHandle));

        // Der UDP-Eintrag ist sofort weg (kein Abbau nötig).
        bedienen();
        assert_eq!(anzahl(), vorher);

        // Ein neues Handle ist NICHT dasselbe (keine Wiederverwendung).
        let h2 = oeffnen(SocketTyp::Udp).expect("oeffnen 2");
        assert_ne!(h2, h);
        schliessen(h2).unwrap();
        bedienen();
    }

    /// Ein TCP-Socket ohne Konfiguration/Verbindung verhält sich sauber:
    /// lauschen ohne binden -> NichtGebunden, senden ohne verbinden ->
    /// NichtVerbunden.
    #[test_case]
    fn test_socket_tcp_fehlerpfade() {
        let h = oeffnen(SocketTyp::Tcp).expect("oeffnen");
        assert_eq!(zustand(h), Ok(Verbindungszustand::Neu));
        assert_eq!(senden(h, b"x"), Err(SocketFehler::NichtVerbunden));
        // lauschen ohne gebundenen Port: entweder NichtGebunden oder (ohne
        // IP-Konfiguration) NichtKonfiguriert — beides ist ein sauberer Fehler.
        let r = lauschen(h);
        assert!(
            matches!(
                r,
                Err(SocketFehler::NichtGebunden) | Err(SocketFehler::NichtKonfiguriert)
            ),
            "unerwartet: {:?}",
            r
        );
        schliessen(h).expect("schliessen");
        assert_eq!(zustand(h), Err(SocketFehler::UngueltigerHandle));
        bedienen();
    }
}
