// netz/icmp.rs — ICMP: der Ping (Schicht über IPv4)
//
// ICMP (Internet Control Message Protocol) transportiert Steuer- und
// Fehlermeldungen von IP. Sein bekanntester Vertreter ist der PING:
//
//   ECHO REQUEST (Typ 8): "Bist du da? Schick mir diese Bytes zurück."
//   ECHO REPLY   (Typ 0): "Ja — hier sind deine Bytes."
//
// Eine ICMP-Echo-Nachricht (direkt in der IPv4-Nutzlast, Protokoll 1):
//   Typ(1) Code(1) Pruefsumme(2) Identifier(2) Sequenz(2) [Daten ...]
//
// Der Identifier ordnet Antworten einem Absender zu, die Sequenz zählt die
// Pings durch — beim Beantworten SPIEGELN wir beide (und die Daten), damit
// der Frager „seine" Antwort wiedererkennt. Die ICMP-PRÜFSUMME ist dieselbe
// Internet-Checksumme wie bei IPv4, aber über die GANZE ICMP-Nachricht.
//
// Zwei Rollen:
//   1. EMPFANGEN: Ein Echo-Request an uns -> wir antworten (Meilenstein:
//      "der Host kann SpeedOS anpingen").
//   2. SENDEN: Der Shell-Befehl `ping` schickt Echo-Requests und misst die
//      Round-Trip-Zeit über die TSC-Mikrosekunden-Uhr (Meilenstein: "SpeedOS
//      kann das Gateway anpingen").

use super::geraet::NetzFehler;
use super::ipv4::{self, internet_checksumme, PROTO_ICMP};
use super::puffer::{Leser, Schreiber};
use super::Ipv4;
use crate::serial_println;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

/// ICMP-Typ: Echo Reply (Antwort auf einen Ping).
pub const TYP_ECHO_REPLY: u8 = 0;
/// ICMP-Typ: Echo Request (der Ping selbst).
pub const TYP_ECHO_REQUEST: u8 = 8;

/// Der geparste Kopf einer ICMP-Echo-Nachricht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoKopf {
    pub typ: u8,
    pub code: u8,
    pub identifier: u16,
    pub sequenz: u16,
}

/// Zerlegt eine ICMP-Echo-Nachricht in (Kopf, Daten). None, wenn sie zu
/// kurz für den 8-Byte-Echo-Kopf ist.
pub fn echo_parse(nachricht: &[u8]) -> Option<(EchoKopf, &[u8])> {
    let mut l = Leser::neu(nachricht);
    let typ = l.u8()?;
    let code = l.u8()?;
    let _pruef = l.u16_be()?;
    let identifier = l.u16_be()?;
    let sequenz = l.u16_be()?;
    let daten = l.bytes(l.rest())?;
    Some((
        EchoKopf {
            typ,
            code,
            identifier,
            sequenz,
        },
        daten,
    ))
}

/// Baut eine ICMP-Echo-Nachricht (Request oder Reply) mit korrekt
/// berechneter Prüfsumme über die GANZE Nachricht.
pub fn echo_bauen(typ: u8, identifier: u16, sequenz: u16, daten: &[u8]) -> Vec<u8> {
    let mut s = Schreiber::mit_kapazitaet(8 + daten.len());
    s.u8(typ);
    s.u8(0); // Code 0 (Echo)
    s.u16_be(0); // Prüfsummen-Platzhalter (Bytes 2..4)
    s.u16_be(identifier);
    s.u16_be(sequenz);
    s.bytes(daten);
    let mut nachricht = s.fertig();
    let pruef = internet_checksumme(&nachricht);
    nachricht[2..4].copy_from_slice(&pruef.to_be_bytes());
    nachricht
}

/// Verarbeitet eine empfangene ICMP-Nachricht (aus der IPv4-Nutzlast). Wird
/// von `ipv4::verarbeiten` für Protokoll 1 gerufen. `quelle`/`ttl` stammen
/// aus dem IP-Kopf.
pub fn verarbeiten(quelle: Ipv4, ttl: u8, nachricht: &[u8]) {
    // Die Prüfsumme über die ganze ICMP-Nachricht muss 0 ergeben.
    if internet_checksumme(nachricht) != 0 {
        serial_println!("[icmp] Nachricht mit falscher Pruefsumme verworfen");
        return;
    }
    let (kopf, daten) = match echo_parse(nachricht) {
        Some(x) => x,
        None => return,
    };
    match kopf.typ {
        TYP_ECHO_REQUEST => {
            // PING-REPLY: Identifier/Sequenz spiegeln, Daten zurückgeben.
            let antwort = echo_bauen(TYP_ECHO_REPLY, kopf.identifier, kopf.sequenz, daten);
            if let Err(fehler) = ipv4::senden(quelle, PROTO_ICMP, &antwort) {
                serial_println!("[icmp] Echo-Reply senden fehlgeschlagen: {}", fehler.meldung());
            }
        }
        TYP_ECHO_REPLY => {
            // Eine Antwort auf UNSEREN Ping — für den `ping`-Befehl vermerken.
            antwort_vermerken(kopf.identifier, kopf.sequenz, ttl);
        }
        _ => {} // andere ICMP-Typen ignorieren wir vorerst
    }
}

/// Sendet einen Echo-Request an `ziel` (für den `ping`-Befehl).
pub fn echo_senden(
    ziel: Ipv4,
    identifier: u16,
    sequenz: u16,
    daten: &[u8],
) -> Result<(), NetzFehler> {
    let request = echo_bauen(TYP_ECHO_REQUEST, identifier, sequenz, daten);
    ipv4::senden(ziel, PROTO_ICMP, &request)
}

// ---------------------------------------------------------------------------
// Empfangene Echo-Antworten vermerken (der ping-Befehl fragt sie ab)
// ---------------------------------------------------------------------------

/// (Identifier, Sequenz, TTL) empfangener Echo-Antworten. Der ping-Befehl
/// leert die Liste zu Beginn, sendet Echos und holt die passenden Antworten
/// ab. Klein gehalten (ein Ping läuft synchron, wenige offene Sequenzen).
static ANTWORTEN: Mutex<Vec<(u16, u16, u8)>> = Mutex::new(Vec::new());

fn antwort_vermerken(identifier: u16, sequenz: u16, ttl: u8) {
    without_interrupts(|| {
        let mut a = ANTWORTEN.lock();
        if a.len() < 64 {
            a.push((identifier, sequenz, ttl));
        }
    });
}

/// Leert die Antwort-Liste (der ping-Befehl ruft es vor der ersten Runde).
pub fn antworten_leeren() {
    without_interrupts(|| ANTWORTEN.lock().clear());
}

/// Prüft (und entnimmt) eine empfangene Echo-Antwort für (identifier,
/// sequenz). Liefert die TTL des Antwort-Pakets zurück, None wenn (noch)
/// keine da ist.
pub fn antwort_empfangen(identifier: u16, sequenz: u16) -> Option<u8> {
    without_interrupts(|| {
        let mut a = ANTWORTEN.lock();
        let pos = a.iter().position(|&(i, s, _)| i == identifier && s == sequenz)?;
        Some(a.remove(pos).2)
    })
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die ICMP-Echo-Reply-Konstruktion: Typ 0, Identifier/Sequenz und
    /// Daten wie im Request, und eine GÜLTIGE Prüfsumme (Summe über die
    /// ganze Nachricht = 0).
    #[test_case]
    fn test_icmp_echo_reply_konstruktion() {
        let daten = [0x61, 0x62, 0x63, 0x64]; // "abcd"
        let reply = echo_bauen(TYP_ECHO_REPLY, 0xABCD, 42, &daten);

        // Prüfsumme über die ganze Nachricht ergibt 0 (gültig).
        assert_eq!(internet_checksumme(&reply), 0x0000);

        let (kopf, zurueck) = echo_parse(&reply).expect("Echo muss parsen");
        assert_eq!(kopf.typ, TYP_ECHO_REPLY);
        assert_eq!(kopf.code, 0);
        assert_eq!(kopf.identifier, 0xABCD);
        assert_eq!(kopf.sequenz, 42);
        assert_eq!(zurueck, &daten);
    }

    /// Ein zu kurzer ICMP-Puffer liefert None statt zu panicken.
    #[test_case]
    fn test_icmp_echo_parse_zu_kurz() {
        assert!(echo_parse(&[8, 0, 0, 0]).is_none()); // nur 4 Byte
        // Genau 8 Byte: Kopf komplett, Daten leer.
        let (kopf, daten) = echo_parse(&[8, 0, 0, 0, 0x12, 0x34, 0x00, 0x01]).unwrap();
        assert_eq!(kopf.identifier, 0x1234);
        assert_eq!(kopf.sequenz, 1);
        assert!(daten.is_empty());
    }
}
