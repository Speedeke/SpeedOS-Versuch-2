// syscall/netz.rs — Die Netz-Syscalls über die Socket-API (Serie 6, Teil 4)
//
// ==========================================================================
// DIE PROBE AUFS EXEMPEL: HÄLT DIE SOCKET-NAHT AUS SERIE 5?
//
// `src/netz/socket.rs` wurde in Serie 5 ausdrücklich als „die Naht für
// Serie 6" gebaut: HANDLES statt Zeiger, klare Fehler-Enums, explizite
// Puffer-Ownership (senden = copy-in, empfangen = copy-out in Aufrufer-
// Slices), TLS-agnostisch. Die Behauptung war: Daraus werden Syscalls, OHNE
// die API umzubauen.
//
// ERGEBNIS — die Behauptung hält, mit EINER Nachschärfung:
//
//  * `oeffnen`, `senden`, `empfangen`, `schliessen`, `zustand`: 1:1
//    durchgereicht. Die Signaturen passten exakt; aus „Slice, den die
//    Kernel-Shell stellt" wurde „Slice, den der Kernel aus User-Speicher
//    kopiert" — genau wie vorhergesagt. Kein Zeichen Änderung in socket.rs.
//
//  * `aufloesen` (DNS): 1:1 durchgereicht.
//
//  * NACHGESCHÄRFT WERDEN MUSSTE `verbinden`. Nicht die Signatur — die
//    SEMANTIK. `socket::verbinden` ist NICHT-BLOCKIEREND: Es startet den
//    TCP-Handshake und kehrt sofort zurück; wer wissen will, ob es geklappt
//    hat, muss `netz::pumpen()` + `zustand()` in einer Schleife abfragen.
//    Genau das tun `hole` und `ping` im Kernel. Ein Ring-3-Programm KANN das
//    aber nicht: Es hat keinen Zugriff auf `pumpen()` (das ist Kernel-
//    Innenleben und wird nie ein Syscall). Ohne einen Kernel, der für ihn
//    pumpt, würde ein Prozess ewig auf einen Handshake warten, der nie
//    vorankommt.
//    Deshalb ist der SYSCALL `verbinde` blockierend (mit Frist): Er pumpt
//    selbst, bis die Verbindung steht, abgelehnt wird oder die Frist abläuft.
//    Das ist keine Änderung AN der Socket-API, sondern eine Entscheidung
//    DARÜBER — sie steht in docs/syscalls.md, damit klar ist, dass dieser
//    eine Syscall Sekunden dauern kann.
//
//  * `empfange` bleibt dagegen NICHT-blockierend (0 = noch nichts da), pumpt
//    aber einmal. Grund: Ein blockierendes `empfange` bräuchte ein
//    Warte-Modell mit Weck-Bedingung (`Zustand::Wartend` auf ein
//    Socket-Ereignis) — das ist Teil 5. Bis dahin ist Pollen ehrlicher als
//    ein verstecktes Timeout.
// ==========================================================================
//
// LOCK-DISZIPLIN (siehe syscall/mod.rs): Alle Locks der Netz-Schicht werden
// im Kernel AUSSCHLIESSLICH mit ausgeschalteten Interrupts gehalten
// (`without_interrupts` in socket.rs, geraet.rs, udp.rs, arp.rs). Wenn ein
// Syscall läuft, hält sie also niemand — wir dürfen sie gefahrlos nehmen.
// Nur die WARTESCHLEIFEN brauchen `warte_fenster()`: Dort halten wir nichts,
// lassen Interrupts zu (der RX-IRQ muss ja feuern können) und dürfen
// verdrängt werden.

use super::handle::{self, KernelObjekt};
use super::{laenge_pruefen, puffer_lesen, puffer_schreiben, warte_fenster, Fehler, SysErgebnis};
use crate::netz::socket::{self, SocketTyp, Verbindungszustand};
use crate::netz::{dns, Ipv4};

/// Socket-Typ-Zahlen (ABI).
pub const TYP_TCP: u64 = 0;
pub const TYP_UDP: u64 = 1;

/// Frist für `verbinde` in Millisekunden.
pub const VERBINDE_FRIST_MS: u64 = 8_000;

/// Zustands-Zahlen für `socket_zustand` (ABI).
pub const ZUSTAND_NEU: u64 = 0;
pub const ZUSTAND_VERBINDET: u64 = 1;
pub const ZUSTAND_VERBUNDEN: u64 = 2;
pub const ZUSTAND_PEER_HAT_GESCHLOSSEN: u64 = 3;
pub const ZUSTAND_GESCHLOSSEN: u64 = 4;
pub const ZUSTAND_LAUSCHT: u64 = 5;

/// Bildet den Kernel-Zustand auf seine ABI-Zahl ab (kein Kernel-Typ nach
/// außen — auch kein Enum).
fn zustand_zahl(zustand: Verbindungszustand) -> u64 {
    match zustand {
        Verbindungszustand::Neu => ZUSTAND_NEU,
        Verbindungszustand::Verbindet => ZUSTAND_VERBINDET,
        Verbindungszustand::Verbunden => ZUSTAND_VERBUNDEN,
        Verbindungszustand::PeerHatGeschlossen => ZUSTAND_PEER_HAT_GESCHLOSSEN,
        Verbindungszustand::Geschlossen => ZUSTAND_GESCHLOSSEN,
        Verbindungszustand::Lauscht => ZUSTAND_LAUSCHT,
    }
}

/// Wandelt die ABI-Darstellung einer IPv4 (u32, a.b.c.d in den Bytes 3..0)
/// in die Kernel-Struktur. Reine Funktion, deshalb unit-getestet.
pub fn ip_aus_zahl(zahl: u64) -> Result<Ipv4, Fehler> {
    if zahl > u32::MAX as u64 {
        return Err(Fehler::UngueltigesArgument);
    }
    let wert = zahl as u32;
    Ok(Ipv4([
        (wert >> 24) as u8,
        (wert >> 16) as u8,
        (wert >> 8) as u8,
        wert as u8,
    ]))
}

/// Die Rückrichtung (für `aufloesen`).
pub fn ip_zu_zahl(ip: Ipv4) -> u64 {
    ((ip.0[0] as u64) << 24) | ((ip.0[1] as u64) << 16) | ((ip.0[2] as u64) << 8) | ip.0[3] as u64
}

/// Prüft eine Port-Nummer aus dem User-Space.
fn port_aus(zahl: u64) -> Result<u16, Fehler> {
    if zahl == 0 || zahl > u16::MAX as u64 {
        return Err(Fehler::UngueltigesArgument);
    }
    Ok(zahl as u16)
}

// ---------------------------------------------------------------------------
// Die Syscalls
// ---------------------------------------------------------------------------

/// `socket(typ)` -> Handle. (typ: 0 = TCP, 1 = UDP)
pub fn sys_socket(typ: u64) -> SysErgebnis {
    let socket_typ = match typ {
        TYP_TCP => SocketTyp::Tcp,
        TYP_UDP => SocketTyp::Udp,
        _ => return Err(Fehler::UngueltigesArgument),
    };
    let kern_handle = socket::oeffnen(socket_typ).map_err(Fehler::von_socket)?;
    // Das globale Handle wandert in die PROZESS-Tabelle; der Prozess sieht nur
    // seine kleine Zahl. Schlägt das Einfügen fehl (Tabelle voll), muss der
    // Socket sofort wieder zu — sonst hätten wir ein Kernel-Objekt ohne
    // Besitzer, also genau das Leck, das die Handle-Tabelle verhindern soll.
    match handle::einfuegen_aktuell(KernelObjekt::Socket(kern_handle)) {
        Ok(prozess_handle) => Ok(prozess_handle),
        Err(fehler) => {
            let _ = socket::schliessen(kern_handle);
            Err(fehler)
        }
    }
}

/// `verbinde(handle, ip, port)`.
///
/// BLOCKIEREND mit Frist (siehe Kopfkommentar): Der Syscall pumpt den Stack
/// selbst, bis die Verbindung steht. Für UDP kehrt er sofort zurück (dort
/// merkt sich der Socket nur das Ziel — es gibt keinen Handshake).
pub fn sys_verbinde(h: u64, ip: u64, port: u64) -> SysErgebnis {
    let kern_handle = handle::socket_handle(h)?;
    let ziel_ip = ip_aus_zahl(ip)?;
    let ziel_port = port_aus(port)?;

    socket::verbinden(kern_handle, ziel_ip, ziel_port).map_err(Fehler::von_socket)?;

    // UDP: nichts zu warten.
    let zustand = socket::zustand(kern_handle).map_err(Fehler::von_socket)?;
    if zustand == Verbindungszustand::Neu {
        return Ok(ZUSTAND_VERBUNDEN);
    }

    let frist = crate::zeit::ms_seit_boot() + VERBINDE_FRIST_MS;
    loop {
        // Ein Pump-Durchgang: kurz, nimmt Locks nur intern und gibt sie wieder
        // frei. Interrupts sind hier aus — niemand hält die Netz-Locks.
        crate::netz::pumpen();
        match socket::zustand(kern_handle).map_err(Fehler::von_socket)? {
            Verbindungszustand::Verbunden => return Ok(ZUSTAND_VERBUNDEN),
            Verbindungszustand::Geschlossen => return Err(Fehler::Abgebrochen),
            _ => {}
        }
        if crate::zeit::ms_seit_boot() >= frist {
            return Err(Fehler::Zeitueberschreitung);
        }
        // WARTEFENSTER: Hier halten wir nichts. Interrupts an, damit der
        // RX-IRQ feuern kann und andere Prozesse laufen dürfen.
        warte_fenster();
    }
}

/// `sende(handle, ptr, len)` -> übernommene Bytes.
///
/// Die Byte-Zahl kann KLEINER als `len` sein (der TCP-Sendepuffer ist
/// endlich) — das ist die Semantik von `socket::senden` und wird ehrlich
/// durchgereicht, statt intern zu schleifen. Der Aufrufer ruft nochmal.
pub fn sys_sende(h: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let kern_handle = handle::socket_handle(h)?;
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(0);
    }
    let daten = puffer_lesen(ptr, laenge as u64)?;
    let uebernommen = socket::senden(kern_handle, &daten).map_err(Fehler::von_socket)?;
    // Einmal pumpen, damit die Segmente auch wirklich rausgehen (sonst
    // passierte das erst beim nächsten Socket-Takt in 100 ms).
    crate::netz::pumpen();
    Ok(uebernommen as u64)
}

/// `empfange(handle, ptr, len)` -> Bytes (0 = noch nichts da).
///
/// NICHT-BLOCKIEREND (siehe Kopfkommentar). Es wird einmal gepumpt, damit
/// eingetroffene Frames verarbeitet sind, dann abgeholt, was da ist.
pub fn sys_empfange(h: u64, ptr: u64, laenge: u64) -> SysErgebnis {
    let kern_handle = handle::socket_handle(h)?;
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(0);
    }
    // Zielbereich VOR dem Empfangen prüfen: Sonst hätten wir Daten aus dem
    // Socket geholt (sie sind dann WEG) und könnten sie nicht abliefern.
    crate::ring3::user_bereich_pruefen(ptr, laenge, true).map_err(Fehler::von_copy)?;

    crate::netz::pumpen();
    let mut puffer = alloc::vec![0u8; laenge];
    let empfangen = socket::empfangen(kern_handle, &mut puffer).map_err(Fehler::von_socket)?;
    if empfangen > 0 {
        puffer_schreiben(ptr, &puffer[..empfangen])?;
    }
    Ok(empfangen as u64)
}

/// `aufloesen(name_ptr, name_len)` -> IPv4 als u32.
///
/// Blockierend mit der Frist des DNS-Resolvers (bis 3 Versuche à 1,2 s, siehe
/// `netz::dns`). `dns::aufloesen` pumpt selbst — deshalb braucht es hier
/// keine eigene Schleife; aber der Aufruf kann Sekunden dauern.
pub fn sys_aufloesen(name_ptr: u64, name_len: u64) -> SysErgebnis {
    if name_len == 0 {
        return Err(Fehler::UngueltigesArgument);
    }
    if name_len as usize > super::MAX_NAME {
        return Err(Fehler::ZuGross);
    }
    let bytes = crate::ring3::copy_in(name_ptr, name_len as usize).map_err(Fehler::von_copy)?;
    let name = alloc::string::String::from_utf8(bytes).map_err(|_| Fehler::UngueltigesArgument)?;
    let ip = dns::aufloesen(&name).map_err(Fehler::von_dns)?;
    Ok(ip_zu_zahl(ip))
}

/// `socket_zustand(handle)` -> Zustands-Zahl.
pub fn sys_socket_zustand(h: u64) -> SysErgebnis {
    let kern_handle = handle::socket_handle(h)?;
    // Einmal pumpen, damit der gemeldete Zustand aktuell ist.
    crate::netz::pumpen();
    let zustand = socket::zustand(kern_handle).map_err(Fehler::von_socket)?;
    Ok(zustand_zahl(zustand))
}

// ---------------------------------------------------------------------------
// Tests der reinen Umrechnungen und der Argument-Prüfung
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// IP-Umrechnung in beide Richtungen (ABI-Reihenfolge a.b.c.d).
    #[test_case]
    fn test_ip_umrechnung() {
        assert_eq!(ip_aus_zahl(0x0A00_020F), Ok(Ipv4([10, 0, 2, 15])));
        assert_eq!(ip_zu_zahl(Ipv4([10, 0, 2, 15])), 0x0A00_020F);
        assert_eq!(ip_aus_zahl(0), Ok(Ipv4([0, 0, 0, 0])));
        assert_eq!(ip_aus_zahl(0xFFFF_FFFF), Ok(Ipv4([255, 255, 255, 255])));
        // Rundreise über alle Byte-Positionen:
        for ip in [
            Ipv4([1, 2, 3, 4]),
            Ipv4([192, 168, 178, 1]),
            Ipv4([255, 0, 0, 255]),
        ] {
            assert_eq!(ip_aus_zahl(ip_zu_zahl(ip)), Ok(ip));
        }
        // Über 32 Bit hinaus: Fehler, keine stille Kürzung.
        assert_eq!(ip_aus_zahl(0x1_0000_0000), Err(Fehler::UngueltigesArgument));
        assert_eq!(ip_aus_zahl(u64::MAX), Err(Fehler::UngueltigesArgument));
    }

    /// Port-Prüfung: 0 und alles über 65535 sind ungültig.
    #[test_case]
    fn test_port_pruefung() {
        assert_eq!(port_aus(80), Ok(80));
        assert_eq!(port_aus(65535), Ok(65535));
        assert_eq!(port_aus(0), Err(Fehler::UngueltigesArgument));
        assert_eq!(port_aus(65536), Err(Fehler::UngueltigesArgument));
        assert_eq!(port_aus(u64::MAX), Err(Fehler::UngueltigesArgument));
    }

    /// Die Zustands-Zahlen sind ABI und müssen stabil bleiben.
    #[test_case]
    fn test_zustand_zahlen_stabil() {
        assert_eq!(zustand_zahl(Verbindungszustand::Neu), 0);
        assert_eq!(zustand_zahl(Verbindungszustand::Verbindet), 1);
        assert_eq!(zustand_zahl(Verbindungszustand::Verbunden), 2);
        assert_eq!(zustand_zahl(Verbindungszustand::PeerHatGeschlossen), 3);
        assert_eq!(zustand_zahl(Verbindungszustand::Geschlossen), 4);
        assert_eq!(zustand_zahl(Verbindungszustand::Lauscht), 5);
    }

    /// Argument-Prüfung VOR jeder Netz-Operation — ohne laufenden Prozess
    /// scheitern alle Socket-Syscalls sauber am Handle, nie mit einer Panik.
    #[test_case]
    fn test_netz_syscalls_boesartig() {
        // Ungültiger Socket-Typ (die Prüfung kommt vor allem anderen):
        assert_eq!(sys_socket(2), Err(Fehler::UngueltigesArgument));
        assert_eq!(sys_socket(u64::MAX), Err(Fehler::UngueltigesArgument));
        // Fremde/ungültige Handles:
        for boese in [0u64, 1, 2, 3, 99, u64::MAX] {
            assert!(sys_verbinde(boese, 0x0A00_0202, 80).is_err());
            assert!(sys_sende(boese, 0, 8).is_err());
            assert!(sys_empfange(boese, 0, 8).is_err());
            assert!(sys_socket_zustand(boese).is_err());
        }
        // aufloesen: Länge 0 und über dem Deckel.
        assert_eq!(sys_aufloesen(0, 0), Err(Fehler::UngueltigesArgument));
        assert_eq!(
            sys_aufloesen(0, super::super::MAX_NAME as u64 + 1),
            Err(Fehler::ZuGross)
        );
        // Kernel-Adresse als Namens-Zeiger:
        assert_eq!(
            sys_aufloesen(crate::allocator::HEAP_START as u64, 8),
            Err(Fehler::UngueltigerZeiger)
        );
    }
}
