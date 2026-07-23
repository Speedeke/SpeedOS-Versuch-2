// netz/tcp.rs — Minimal-Viable-TCP (bewusst ein LERN-ARTEFAKT)
//
// TCP macht aus dem unzuverlässigen IP-Paketdienst einen VERLÄSSLICHEN,
// geordneten Bytestrom: Handshake, Sequenznummern, Bestätigungen (ACK),
// Retransmit bei Verlust und ein sauberer Verbindungsabbau. Das ist der
// lehrreichste — und riskanteste — Teil des Stacks. WAS wir bauen und was
// bewusst FEHLT, ist in docs/tcp-scope.md festgelegt (inkl. Reißleine).
//
// AUFBAU dieses Moduls, damit es OHNE echten Peer testbar ist: Die
// `Verbindung` (der TCB, "Transmission Control Block") ist eine REINE
// Zustandsmaschine. Sie ruft NICHT selbst ins Netz — sie sammelt die zu
// sendenden TCP-Segmente in einem AUSGANG. Ein Treiber (weiter unten) leert
// den Ausgang und schickt die Segmente per IPv4; im Loopback-TEST reicht man
// den Ausgang der einen Verbindung direkt in `segment_empfangen` der anderen
// (durch einen simulierten Kanal mit Paketverlust). So spielt derselbe Code
// gegen echte Hardware UND gegen sich selbst.
//
// PUFFER-OWNERSHIP (explizit): Jede Verbindung BESITZT zwei Byte-Ringpuffer
// (netz::puffer::Ringpuffer):
//   * `sende_puffer`  — Bytes, die die App geschrieben hat, aber der Peer
//     noch NICHT bestätigt hat. Sie bleiben, bis ein ACK sie freigibt
//     (`verwerfen`); zum (Neu-)Senden werden sie mit `spitzen` OHNE Entnahme
//     gelesen. Der freie Platz ist implizit das, was die App noch schreiben
//     darf.
//   * `empfangs_puffer` — in-Order angekommene Bytes, die die App noch nicht
//     abgeholt hat. Sein freier Platz ist unser ANGEKÜNDIGTES FENSTER.
// Kopiert wird an den Rändern (copy-in/copy-out) — die Grenze ist bewusst so
// gelegt, dass später eine Kernel/User-Trennung (Serie 6) dazwischen passt.

use super::ipv4::{self, PROTO_TCP};
use super::puffer::{Ringpuffer, Schreiber};
use super::Ipv4;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

// --- TCP-Flags (im 14. Byte des Kopfes) ---------------------------------
const FLAG_FIN: u8 = 0x01;
const FLAG_SYN: u8 = 0x02;
const FLAG_RST: u8 = 0x04;
const FLAG_PSH: u8 = 0x08;
const FLAG_ACK: u8 = 0x10;

/// Kopflänge ohne Optionen.
const KOPF_LEN: usize = 20;
/// Höchstens so viele Datenbytes pro Segment (klein, LAN — kein PMTU).
const SEGMENT_DATEN_MAX: usize = 1024;
/// Kapazität der Sende-/Empfangspuffer = das FESTE Fenster (kein Scaling).
pub const FENSTER_KAP: usize = 8192;

/// Start-RTO (Retransmission Timeout) in ms — fester Startwert.
const RTO_START_MS: u64 = 500;
/// Obergrenze der RTO nach exponentiellem Backoff.
const RTO_MAX_MS: u64 = 8000;
/// So oft senden wir erneut, bevor wir aufgeben (dann RST + CLOSED).
const MAX_RETRANSMITS: u32 = 10;
/// TIME_WAIT-Dauer (2·MSL) — BEWUSST auf 2 s verkürzt (siehe tcp-scope.md).
const TIME_WAIT_MS: u64 = 2000;

// ---------------------------------------------------------------------------
// Sequenznummern-Arithmetik (u32 mit WRAPAROUND) — reine, getestete Logik
// ---------------------------------------------------------------------------
//
// TCP-Sequenznummern sind 32-Bit und laufen über (nach 0xFFFFFFFF kommt 0).
// "Kleiner/größer" ist deshalb ZYKLISCH definiert: a < b, wenn die
// vorzeichenbehaftete Differenz (a - b) negativ ist. So funktioniert der
// Vergleich auch über die Wickel-Grenze hinweg (solange die Nummern nicht
// mehr als 2^31 auseinanderliegen — das tun sie im Betrieb nie).

fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}
fn seq_gt(a: u32, b: u32) -> bool {
    seq_lt(b, a)
}
fn seq_leq(a: u32, b: u32) -> bool {
    a == b || seq_lt(a, b)
}
fn seq_geq(a: u32, b: u32) -> bool {
    a == b || seq_gt(a, b)
}

// ---------------------------------------------------------------------------
// Segment: parsen, bauen, Prüfsumme (Pseudo-Header wie UDP, Protokoll 6)
// ---------------------------------------------------------------------------

/// Ein geparstes TCP-Segment (die Daten als Slice in den Originalbytes).
struct Segment<'a> {
    quell_port: u16,
    ziel_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    fenster: u16,
    daten: &'a [u8],
}

/// Zerlegt ein TCP-Segment. None bei zu kurz / unplausibler Kopflänge.
fn segment_parse(seg: &[u8]) -> Option<Segment<'_>> {
    if seg.len() < KOPF_LEN {
        return None;
    }
    let daten_offset = ((seg[12] >> 4) as usize) * 4;
    if daten_offset < KOPF_LEN || daten_offset > seg.len() {
        return None;
    }
    Some(Segment {
        quell_port: u16::from_be_bytes([seg[0], seg[1]]),
        ziel_port: u16::from_be_bytes([seg[2], seg[3]]),
        seq: u32::from_be_bytes([seg[4], seg[5], seg[6], seg[7]]),
        ack: u32::from_be_bytes([seg[8], seg[9], seg[10], seg[11]]),
        flags: seg[13] & 0x3F,
        fenster: u16::from_be_bytes([seg[14], seg[15]]),
        daten: &seg[daten_offset..],
    })
}

/// Die TCP-Prüfsumme über den PSEUDO-HEADER (src/dst-IP, Proto 6, TCP-Länge)
/// + das Segment — dieselbe Internet-Checksumme wie bei UDP.
fn checksumme(quell_ip: Ipv4, ziel_ip: Ipv4, seg: &[u8]) -> u16 {
    let mut puffer = Vec::with_capacity(12 + seg.len());
    puffer.extend_from_slice(&quell_ip.oktette());
    puffer.extend_from_slice(&ziel_ip.oktette());
    puffer.push(0);
    puffer.push(PROTO_TCP);
    puffer.extend_from_slice(&(seg.len() as u16).to_be_bytes());
    puffer.extend_from_slice(seg);
    ipv4::internet_checksumme(&puffer)
}

/// Baut ein TCP-Segment (20-Byte-Kopf ohne Optionen) mit korrekter Prüfsumme.
#[allow(clippy::too_many_arguments)]
fn segment_bauen(
    quell_ip: Ipv4,
    ziel_ip: Ipv4,
    quell_port: u16,
    ziel_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    fenster: u16,
    daten: &[u8],
) -> Vec<u8> {
    let mut s = Schreiber::mit_kapazitaet(KOPF_LEN + daten.len());
    s.u16_be(quell_port);
    s.u16_be(ziel_port);
    s.u32_be(seq);
    s.u32_be(ack);
    s.u8(5 << 4); // Data Offset 5 (20 Byte), reservierte Bits 0
    s.u8(flags);
    s.u16_be(fenster);
    s.u16_be(0); // Prüfsummen-Platzhalter (Bytes 16..18)
    s.u16_be(0); // Urgent Pointer
    s.bytes(daten);
    let mut seg = s.fertig();
    let pruef = checksumme(quell_ip, ziel_ip, &seg);
    seg[16..18].copy_from_slice(&pruef.to_be_bytes());
    seg
}

// ---------------------------------------------------------------------------
// Der Zustandsautomat
// ---------------------------------------------------------------------------

/// Die elf TCP-Zustände (RFC 793). Den ganzen Automaten bauen wir — er ist
/// überschaubar; die Kunst steckt in den Übergängen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zustand {
    Closed,
    Listen,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    Closing,
    TimeWait,
    CloseWait,
    LastAck,
}

/// Eine TCP-Verbindung (der TCB). Reine Zustandsmaschine: Eingaben sind
/// `segment_empfangen`, `senden`, `schliessen`, `tick`; Ausgaben sind die
/// gesammelten Segmente im `ausgang`.
pub struct Verbindung {
    lokale_ip: Ipv4,
    lokaler_port: u16,
    ferne_ip: Ipv4,
    ferner_port: u16,
    zustand: Zustand,

    // Sende-Sequenzraum:
    iss: u32,     // initiale Sende-Sequenznummer
    snd_una: u32, // älteste unbestätigte Sequenznummer
    snd_nxt: u32, // nächste neu zu sendende Sequenznummer
    ferne_fenster: u16,

    // Empfangs-Sequenzraum:
    rcv_nxt: u32, // nächste erwartete Sequenznummer

    // Puffer (Ownership: die Verbindung besitzt sie — siehe Modul-Kopf):
    sende_puffer: Ringpuffer,
    empfangs_puffer: Ringpuffer,

    // Verbindungsabbau:
    schliessen_gewuenscht: bool,
    fin_gesendet: bool,
    fin_seq: u32,

    // Retransmit:
    rto_ms: u64,
    retransmit_frist: Option<u64>,
    versuche: u32,
    time_wait_frist: Option<u64>,
    abgebrochen: bool,

    // Ausgang: fertig gebaute Segmente, die der Treiber/Test verschickt.
    ausgang: Vec<Vec<u8>>,
}

impl Verbindung {
    /// Gemeinsames Grundgerüst (im Zustand CLOSED).
    fn leer(lokale_ip: Ipv4, lokaler_port: u16, iss: u32) -> Verbindung {
        Verbindung {
            lokale_ip,
            lokaler_port,
            ferne_ip: Ipv4::NULL,
            ferner_port: 0,
            zustand: Zustand::Closed,
            iss,
            snd_una: iss,
            snd_nxt: iss,
            ferne_fenster: 0,
            rcv_nxt: 0,
            sende_puffer: Ringpuffer::neu(FENSTER_KAP),
            empfangs_puffer: Ringpuffer::neu(FENSTER_KAP),
            schliessen_gewuenscht: false,
            fin_gesendet: false,
            fin_seq: 0,
            rto_ms: RTO_START_MS,
            retransmit_frist: None,
            versuche: 0,
            time_wait_frist: None,
            abgebrochen: false,
            ausgang: Vec::new(),
        }
    }

    /// AKTIVER Verbindungsaufbau (connect): sendet den SYN, geht SYN_SENT.
    pub fn verbinden_aktiv(
        lokale_ip: Ipv4,
        lokaler_port: u16,
        ferne_ip: Ipv4,
        ferner_port: u16,
        iss: u32,
        jetzt_ms: u64,
    ) -> Verbindung {
        let mut v = Verbindung::leer(lokale_ip, lokaler_port, iss);
        v.ferne_ip = ferne_ip;
        v.ferner_port = ferner_port;
        v.snd_una = iss;
        v.snd_nxt = iss.wrapping_add(1); // der SYN belegt eine Sequenznummer
        v.zustand = Zustand::SynSent;
        v.syn_senden();
        v.retransmit_arm(jetzt_ms);
        v
    }

    /// PASSIVER Verbindungsaufbau (listen): wartet auf ein eingehendes SYN.
    pub fn lauschen(lokale_ip: Ipv4, lokaler_port: u16, iss: u32) -> Verbindung {
        let mut v = Verbindung::leer(lokale_ip, lokaler_port, iss);
        v.zustand = Zustand::Listen;
        v
    }

    // --- Abfragen ---------------------------------------------------------

    pub fn zustand(&self) -> Zustand {
        self.zustand
    }
    pub fn ist_verbunden(&self) -> bool {
        self.zustand == Zustand::Established
    }
    pub fn ist_geschlossen(&self) -> bool {
        matches!(self.zustand, Zustand::Closed)
    }
    pub fn abgebrochen(&self) -> bool {
        self.abgebrochen
    }
    /// Wie viele empfangene Bytes die App abholen kann.
    pub fn empfangen_verfuegbar(&self) -> usize {
        self.empfangs_puffer.len()
    }
    pub fn ferner_port(&self) -> u16 {
        self.ferner_port
    }

    /// Nimmt die gebauten Ausgangs-Segmente heraus (Treiber/Test senden sie).
    pub fn ausgang_abholen(&mut self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.ausgang)
    }

    // --- App-API ----------------------------------------------------------

    /// Schreibt App-Daten in den Sendepuffer und versucht sofort zu senden.
    /// Liefert die tatsächlich übernommene Anzahl (durch den Puffer begrenzt).
    pub fn senden(&mut self, daten: &[u8], jetzt_ms: u64) -> usize {
        let n = self.sende_puffer.schreiben(daten);
        self.senden_versuchen(jetzt_ms);
        n
    }

    /// Holt empfangene Bytes ab; schickt bei Bedarf ein Fenster-Update-ACK
    /// (damit ein voll gelaufenes Fenster den Peer nicht blockiert).
    pub fn empfangen(&mut self, ziel: &mut [u8]) -> usize {
        let n = self.empfangs_puffer.lesen(ziel);
        if n > 0 && matches!(self.zustand, Zustand::Established | Zustand::FinWait1 | Zustand::FinWait2) {
            self.ack_senden();
        }
        n
    }

    /// Leitet den Verbindungsabbau ein (sendet FIN, sobald alle Daten
    /// bestätigt sind).
    pub fn schliessen(&mut self, jetzt_ms: u64) {
        self.schliessen_gewuenscht = true;
        match self.zustand {
            Zustand::Listen | Zustand::SynSent => self.zustand = Zustand::Closed,
            _ => self.senden_versuchen(jetzt_ms),
        }
    }

    // --- Eingang: ein Segment verarbeiten ---------------------------------

    /// Verarbeitet ein empfangenes TCP-Segment. `quell_ip` liefert die
    /// IP-Schicht (für die Prüfsumme und — im LISTEN — die Gegenstelle).
    pub fn segment_empfangen(&mut self, quell_ip: Ipv4, seg: &[u8], jetzt_ms: u64) {
        // Prüfsumme (Pseudo-Header aus Absender-IP -> unsere IP).
        if checksumme(quell_ip, self.lokale_ip, seg) != 0 {
            return;
        }
        let s = match segment_parse(seg) {
            Some(s) => s,
            None => return,
        };
        if s.ziel_port != self.lokaler_port {
            return;
        }
        // Außer im LISTEN muss der Absender zur bekannten Gegenstelle passen.
        if self.zustand != Zustand::Listen
            && (quell_ip != self.ferne_ip || s.quell_port != self.ferner_port)
        {
            return;
        }
        // RST bricht ab (im LISTEN ignoriert).
        if s.flags & FLAG_RST != 0 {
            if self.zustand != Zustand::Listen {
                self.zustand = Zustand::Closed;
                self.abgebrochen = true;
                self.retransmit_frist = None;
            }
            return;
        }

        match self.zustand {
            Zustand::Listen => self.empfang_listen(quell_ip, &s, jetzt_ms),
            Zustand::SynSent => self.empfang_syn_sent(&s, jetzt_ms),
            Zustand::SynRcvd => self.empfang_syn_rcvd(&s, jetzt_ms),
            Zustand::Established
            | Zustand::FinWait1
            | Zustand::FinWait2
            | Zustand::CloseWait
            | Zustand::Closing
            | Zustand::LastAck => self.empfang_verbunden(&s, jetzt_ms),
            Zustand::TimeWait => {
                // Ein erneuter FIN (unser letztes ACK ging verloren) -> ACK
                // wiederholen und TIME_WAIT verlängern.
                if s.flags & FLAG_FIN != 0 {
                    self.ack_senden();
                    self.time_wait_frist = Some(jetzt_ms + TIME_WAIT_MS);
                }
            }
            Zustand::Closed => {}
        }
    }

    /// LISTEN: ein SYN macht uns zum passiven Öffner (SYN_RCVD).
    fn empfang_listen(&mut self, quell_ip: Ipv4, s: &Segment, jetzt_ms: u64) {
        if s.flags & FLAG_SYN == 0 {
            return;
        }
        self.ferne_ip = quell_ip;
        self.ferner_port = s.quell_port;
        self.rcv_nxt = s.seq.wrapping_add(1);
        self.ferne_fenster = s.fenster;
        self.snd_una = self.iss;
        self.snd_nxt = self.iss.wrapping_add(1);
        self.zustand = Zustand::SynRcvd;
        self.syn_ack_senden();
        self.retransmit_arm(jetzt_ms);
    }

    /// SYN_SENT: wir warten auf SYN+ACK (normal) oder SYN (simultanes Öffnen).
    fn empfang_syn_sent(&mut self, s: &Segment, jetzt_ms: u64) {
        let hat_ack = s.flags & FLAG_ACK != 0;
        let hat_syn = s.flags & FLAG_SYN != 0;
        if hat_ack && s.ack != self.iss.wrapping_add(1) {
            return; // bestätigt unseren SYN nicht
        }
        if !hat_syn {
            return;
        }
        self.rcv_nxt = s.seq.wrapping_add(1);
        self.ferne_fenster = s.fenster;
        if hat_ack {
            self.snd_una = s.ack; // = iss + 1
            self.zustand = Zustand::Established;
            self.retransmit_stop();
            self.ack_senden(); // das dritte Segment des Handshakes
            self.senden_versuchen(jetzt_ms); // ggf. schon gepufferte Daten
        } else {
            // Simultanes Öffnen: nur SYN erhalten -> SYN_RCVD.
            self.zustand = Zustand::SynRcvd;
            self.syn_ack_senden();
            self.retransmit_arm(jetzt_ms);
        }
    }

    /// SYN_RCVD: das ACK auf unser SYN+ACK vollendet den Handshake.
    fn empfang_syn_rcvd(&mut self, s: &Segment, jetzt_ms: u64) {
        if s.flags & FLAG_ACK == 0 || s.ack != self.snd_nxt {
            return;
        }
        self.snd_una = s.ack;
        self.ferne_fenster = s.fenster;
        self.zustand = Zustand::Established;
        self.retransmit_stop();
        // Das vollendende ACK kann schon Daten/FIN tragen.
        self.daten_und_fin(s, jetzt_ms);
        self.senden_versuchen(jetzt_ms);
    }

    /// Die verbundenen Zustände: ACK verarbeiten, Daten/FIN aufnehmen,
    /// Close-Übergänge prüfen, ggf. neue Daten senden.
    fn empfang_verbunden(&mut self, s: &Segment, jetzt_ms: u64) {
        if s.flags & FLAG_ACK != 0 {
            self.ack_verarbeiten(s.ack, jetzt_ms);
            self.ferne_fenster = s.fenster;
        }
        self.close_ack_pruefen(jetzt_ms);
        self.daten_und_fin(s, jetzt_ms);
        self.senden_versuchen(jetzt_ms);
    }

    /// Bestätigte Sende-Daten freigeben und den Retransmit-Timer nachführen.
    fn ack_verarbeiten(&mut self, ack: u32, jetzt_ms: u64) {
        // Nur ACKs, die etwas Neues bestätigen: snd_una < ack <= snd_nxt.
        if !(seq_gt(ack, self.snd_una) && seq_leq(ack, self.snd_nxt)) {
            return;
        }
        let bestaetigt = ack.wrapping_sub(self.snd_una);
        // Bestätigte DATEN aus dem Sendepuffer entfernen. SYN/FIN sind keine
        // Puffer-Bytes; da wir FIN erst nach leerem Puffer senden, entfernt
        // `verwerfen` (das intern klemmt) hier nie zu viel.
        self.sende_puffer.verwerfen(bestaetigt as usize);
        self.snd_una = ack;
        if seq_lt(self.snd_una, self.snd_nxt) {
            self.retransmit_neustart(jetzt_ms); // noch etwas offen
        } else {
            self.retransmit_stop();
        }
    }

    /// In-Order-Daten aufnehmen (Out-of-Order wird VERWORFEN) und einen
    /// eingehenden FIN behandeln.
    fn daten_und_fin(&mut self, s: &Segment, jetzt_ms: u64) {
        let mut ack_noetig = false;
        let in_order = s.seq == self.rcv_nxt;

        if !s.daten.is_empty() {
            if in_order {
                let geschrieben = self.empfangs_puffer.schreiben(s.daten);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(geschrieben as u32);
            }
            // Ob in-Order (Daten aufgenommen) oder nicht (Out-of-Order/alt):
            // IMMER ein ACK schicken. Bei einer Lücke ist es ein Dup-ACK, das
            // dem Peer sagt "ich erwarte noch rcv_nxt" -> er sendet neu.
            ack_noetig = true;
        }

        // FIN nur akzeptieren, wenn er in-Order ist UND alle seine Daten
        // aufgenommen wurden (rcv_nxt hat bis genau vor den FIN aufgeholt).
        if s.flags & FLAG_FIN != 0
            && in_order
            && self.rcv_nxt == s.seq.wrapping_add(s.daten.len() as u32)
        {
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1); // der FIN belegt 1 seq
            self.fin_empfangen(jetzt_ms);
            ack_noetig = true;
        }

        if ack_noetig {
            self.ack_senden();
        }
    }

    /// Zustandsübergang beim EMPFANG eines FIN.
    fn fin_empfangen(&mut self, jetzt_ms: u64) {
        match self.zustand {
            Zustand::Established => self.zustand = Zustand::CloseWait,
            Zustand::FinWait1 => self.zustand = Zustand::Closing, // unser FIN noch offen
            Zustand::FinWait2 => {
                self.zustand = Zustand::TimeWait;
                self.time_wait_frist = Some(jetzt_ms + TIME_WAIT_MS);
                self.retransmit_stop();
            }
            _ => {}
        }
    }

    /// Zustandsübergang, wenn UNSER FIN bestätigt wurde.
    fn close_ack_pruefen(&mut self, jetzt_ms: u64) {
        let fin_bestaetigt =
            self.fin_gesendet && seq_geq(self.snd_una, self.fin_seq.wrapping_add(1));
        if !fin_bestaetigt {
            return;
        }
        match self.zustand {
            Zustand::FinWait1 => {
                self.zustand = Zustand::FinWait2;
                self.retransmit_stop();
            }
            Zustand::Closing => {
                self.zustand = Zustand::TimeWait;
                self.time_wait_frist = Some(jetzt_ms + TIME_WAIT_MS);
                self.retransmit_stop();
            }
            Zustand::LastAck => {
                self.zustand = Zustand::Closed;
                self.retransmit_stop();
            }
            _ => {}
        }
    }

    // --- Senden -----------------------------------------------------------

    /// Sendet so viel neue Daten wie Fenster und Puffer erlauben; schickt den
    /// FIN, sobald Close gewünscht ist und alle Daten bestätigt sind.
    fn senden_versuchen(&mut self, jetzt_ms: u64) {
        if matches!(self.zustand, Zustand::Established | Zustand::CloseWait) {
            loop {
                let in_flight = self.snd_nxt.wrapping_sub(self.snd_una);
                let fenster = self.ferne_fenster as u32;
                if in_flight >= fenster {
                    break; // Fenster ausgeschöpft
                }
                let gesendet_offset = in_flight as usize;
                let ungesendet = self.sende_puffer.len().saturating_sub(gesendet_offset);
                let erlaubt = (fenster - in_flight) as usize;
                let n = ungesendet.min(SEGMENT_DATEN_MAX).min(erlaubt);
                if n == 0 {
                    break;
                }
                let mut daten = alloc::vec![0u8; n];
                self.sende_puffer.spitzen(gesendet_offset, &mut daten);
                let seq = self.snd_nxt;
                self.daten_segment_senden(seq, &daten);
                self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
                self.retransmit_arm(jetzt_ms);
            }
        }
        // FIN senden, wenn gewünscht und alle Daten gesendet + bestätigt.
        if self.schliessen_gewuenscht
            && !self.fin_gesendet
            && self.sende_puffer.is_empty()
            && self.snd_una == self.snd_nxt
            && matches!(self.zustand, Zustand::Established | Zustand::CloseWait)
        {
            self.fin_senden(jetzt_ms);
        }
    }

    fn fin_senden(&mut self, jetzt_ms: u64) {
        self.fin_seq = self.snd_nxt;
        self.segment_senden(self.snd_nxt, self.rcv_nxt, FLAG_FIN | FLAG_ACK, &[]);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
        self.fin_gesendet = true;
        match self.zustand {
            Zustand::Established => self.zustand = Zustand::FinWait1,
            Zustand::CloseWait => self.zustand = Zustand::LastAck,
            _ => {}
        }
        self.retransmit_arm(jetzt_ms);
    }

    // --- Segment-Emitter --------------------------------------------------

    fn segment_senden(&mut self, seq: u32, ack: u32, flags: u8, daten: &[u8]) {
        // Angekündigtes Fenster = freier Empfangspuffer (kein Scaling).
        let fenster = self.empfangs_puffer.frei().min(0xFFFF) as u16;
        let seg = segment_bauen(
            self.lokale_ip,
            self.ferne_ip,
            self.lokaler_port,
            self.ferner_port,
            seq,
            ack,
            flags,
            fenster,
            daten,
        );
        self.ausgang.push(seg);
    }
    fn syn_senden(&mut self) {
        self.segment_senden(self.iss, 0, FLAG_SYN, &[]);
    }
    fn syn_ack_senden(&mut self) {
        self.segment_senden(self.iss, self.rcv_nxt, FLAG_SYN | FLAG_ACK, &[]);
    }
    fn ack_senden(&mut self) {
        self.segment_senden(self.snd_nxt, self.rcv_nxt, FLAG_ACK, &[]);
    }
    fn daten_segment_senden(&mut self, seq: u32, daten: &[u8]) {
        self.segment_senden(seq, self.rcv_nxt, FLAG_ACK | FLAG_PSH, daten);
    }
    fn fin_erneut_senden(&mut self) {
        self.segment_senden(self.fin_seq, self.rcv_nxt, FLAG_FIN | FLAG_ACK, &[]);
    }
    fn rst_senden(&mut self) {
        self.segment_senden(self.snd_nxt, self.rcv_nxt, FLAG_RST | FLAG_ACK, &[]);
    }

    // --- Retransmit-Timer + tick -----------------------------------------

    fn retransmit_arm(&mut self, jetzt_ms: u64) {
        if self.retransmit_frist.is_none() {
            self.retransmit_frist = Some(jetzt_ms + self.rto_ms);
        }
    }
    fn retransmit_neustart(&mut self, jetzt_ms: u64) {
        self.rto_ms = RTO_START_MS;
        self.versuche = 0;
        self.retransmit_frist = Some(jetzt_ms + self.rto_ms);
    }
    fn retransmit_stop(&mut self) {
        self.retransmit_frist = None;
        self.versuche = 0;
        self.rto_ms = RTO_START_MS;
    }

    /// Der Zeittakt: TIME_WAIT ablaufen lassen und fällige Retransmits
    /// auslösen (mit exponentiellem Backoff, Aufgabe nach MAX_RETRANSMITS).
    pub fn tick(&mut self, jetzt_ms: u64) {
        if self.zustand == Zustand::TimeWait {
            if let Some(f) = self.time_wait_frist {
                if jetzt_ms >= f {
                    self.zustand = Zustand::Closed;
                    self.time_wait_frist = None;
                }
            }
            return;
        }
        if let Some(frist) = self.retransmit_frist {
            if jetzt_ms >= frist {
                self.versuche += 1;
                if self.versuche > MAX_RETRANSMITS {
                    self.rst_senden();
                    self.zustand = Zustand::Closed;
                    self.abgebrochen = true;
                    self.retransmit_frist = None;
                    return;
                }
                self.rto_ms = (self.rto_ms * 2).min(RTO_MAX_MS);
                self.retransmit_ausfuehren(jetzt_ms);
                self.retransmit_frist = Some(jetzt_ms + self.rto_ms);
            }
        }
    }

    /// Sendet die älteste unbestätigte Einheit erneut — je nach Zustand SYN,
    /// SYN+ACK, die unbestätigten Daten (Go-Back-N) oder den FIN.
    fn retransmit_ausfuehren(&mut self, jetzt_ms: u64) {
        match self.zustand {
            Zustand::SynSent => self.syn_senden(),
            Zustand::SynRcvd => self.syn_ack_senden(),
            Zustand::Established | Zustand::CloseWait => {
                // Go-Back-N: ab snd_una alles Unbestätigte erneut senden.
                self.snd_nxt = self.snd_una;
                self.senden_versuchen(jetzt_ms);
            }
            Zustand::FinWait1 | Zustand::Closing | Zustand::LastAck => self.fin_erneut_senden(),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Treiber: EINE aktive Verbindung über IPv4 (für den HTTP-Abruf)
// ---------------------------------------------------------------------------
//
// Bewusst MINIMAL: genau eine Verbindung zur Zeit (kein Verbindungs-Tisch —
// den bringt die Socket-API in einer späteren Stufe). Der Treiber verbindet
// die reine Zustandsmaschine mit dem echten Netz:
//   * `verarbeiten` (aus dem IPv4-Dispatch) reicht eingehende Segmente an
//     die Verbindung,
//   * `hole` treibt den Ablauf synchron (wie ping/nslookup): Segmente aus
//     dem AUSGANG per IPv4 senden, den Empfang pumpen, Timer ticken.

/// Die eine aktive Verbindung (Blatt-Lock, nur aus Task-Kontext).
static VERBINDUNG: Mutex<Option<Verbindung>> = Mutex::new(None);

/// Fehler des TCP-Treibers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpFehler {
    /// Keine IP konfiguriert (erst DHCP/netz-ip).
    NichtKonfiguriert,
    /// Keine Netzwerkkarte.
    KeinGeraet,
    /// Verbindungsaufbau/Transfer lief in den Timeout.
    Zeitueberschreitung,
    /// Die Gegenstelle hat abgelehnt (RST) oder wir haben aufgegeben.
    Abgebrochen,
}

impl TcpFehler {
    pub fn meldung(&self) -> &'static str {
        match self {
            TcpFehler::NichtKonfiguriert => "keine IP konfiguriert (erst dhcp oder netz-ip)",
            TcpFehler::KeinGeraet => "keine Netzwerkkarte vorhanden",
            TcpFehler::Zeitueberschreitung => "Zeitueberschreitung (Handshake/Transfer)",
            TcpFehler::Abgebrochen => "Verbindung abgebrochen (RST oder zu viele Retransmits)",
        }
    }
}

/// Eine initiale Sequenznummer aus der TSC-Uhr (muss nur schwer vorhersagbar
/// sein — für unseren Lernzweck genügt die Uhr).
fn isn() -> u32 {
    crate::zeit::us_seit_boot() as u32
}

/// Fortlaufender ephemerer Quell-Port (49152..).
static EPH_PORT: AtomicU16 = AtomicU16::new(49152);
fn ephemerer_port() -> u16 {
    let p = EPH_PORT.fetch_add(1, Ordering::Relaxed);
    if p >= 60000 {
        EPH_PORT.store(49152, Ordering::Relaxed);
    }
    p
}

/// Aus dem IPv4-Dispatch (Protokoll 6): reicht das Segment an die aktive
/// Verbindung. Sendet NICHT selbst — der `hole`-Ablauf leert danach den
/// Ausgang (so bleibt der Lock-Pfad einfach).
pub fn verarbeiten(quell_ip: Ipv4, _ziel_ip: Ipv4, segment: &[u8]) {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        if let Some(v) = VERBINDUNG.lock().as_mut() {
            v.segment_empfangen(quell_ip, segment, jetzt);
        }
    });
}

/// Leert den Ausgang der Verbindung und schickt jedes Segment per IPv4 an
/// `ferne_ip` (der Lock wird NICHT über das Senden gehalten).
fn ausgang_senden(ferne_ip: Ipv4) {
    let segmente = without_interrupts(|| {
        VERBINDUNG
            .lock()
            .as_mut()
            .map(|v| v.ausgang_abholen())
            .unwrap_or_default()
    });
    for seg in segmente {
        let _ = ipv4::senden(ferne_ip, PROTO_TCP, &seg);
    }
}

/// Ein Pump-Schritt: Ausstehendes senden, Empfang verarbeiten, Timer ticken,
/// Antworten senden.
fn pump_schritt(ferne_ip: Ipv4) {
    ausgang_senden(ferne_ip);
    super::rx_verarbeiten();
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        if let Some(v) = VERBINDUNG.lock().as_mut() {
            v.tick(jetzt);
        }
    });
    ausgang_senden(ferne_ip);
}

/// Der aktuelle Zustand der Verbindung (None, wenn keine da ist).
fn zustand_lesen() -> Option<Zustand> {
    without_interrupts(|| VERBINDUNG.lock().as_ref().map(|v| v.zustand()))
}

/// Schreibt Anfrage-Bytes in den Sendepuffer.
fn verbindung_senden(daten: &[u8]) {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        if let Some(v) = VERBINDUNG.lock().as_mut() {
            v.senden(daten, jetzt);
        }
    });
}

/// Holt alle verfügbaren Empfangsbytes und hängt sie an `ziel` an.
fn empfang_anhaengen(ziel: &mut Vec<u8>) {
    without_interrupts(|| {
        if let Some(v) = VERBINDUNG.lock().as_mut() {
            let mut buf = [0u8; 1024];
            loop {
                let n = v.empfangen(&mut buf);
                if n == 0 {
                    break;
                }
                ziel.extend_from_slice(&buf[..n]);
            }
        }
    });
}

/// Leitet den Verbindungsabbau ein.
fn verbindung_schliessen() {
    let jetzt = crate::zeit::ms_seit_boot();
    without_interrupts(|| {
        if let Some(v) = VERBINDUNG.lock().as_mut() {
            v.schliessen(jetzt);
        }
    });
}

/// Entfernt die Verbindung (gibt ihre Puffer frei).
fn verbindung_raeumen() {
    without_interrupts(|| *VERBINDUNG.lock() = None);
}

/// Baut eine TCP-Verbindung zu `ferne_ip:port` auf, sendet `anfrage`, liest
/// die Antwort bis der Peer schließt (oder Timeout) und baut sauber ab.
/// DER end-to-end-Weg — zugleich die Messung für die Reißleine
/// (docs/tcp-scope.md).
pub fn hole(
    ferne_ip: Ipv4,
    port: u16,
    anfrage: &[u8],
    timeout_ms: u64,
) -> Result<Vec<u8>, TcpFehler> {
    let unsere_ip = super::unsere_ip().ok_or(TcpFehler::NichtKonfiguriert)?;
    super::mac().ok_or(TcpFehler::KeinGeraet)?;

    let jetzt = crate::zeit::ms_seit_boot();
    let v = Verbindung::verbinden_aktiv(unsere_ip, ephemerer_port(), ferne_ip, port, isn(), jetzt);
    without_interrupts(|| *VERBINDUNG.lock() = Some(v));
    let deadline = jetzt + timeout_ms;

    // 1. Handshake abwarten.
    loop {
        pump_schritt(ferne_ip);
        match zustand_lesen() {
            Some(Zustand::Established) => break,
            None | Some(Zustand::Closed) => {
                verbindung_raeumen();
                return Err(TcpFehler::Abgebrochen);
            }
            _ => {}
        }
        if crate::zeit::ms_seit_boot() >= deadline {
            verbindung_raeumen();
            return Err(TcpFehler::Zeitueberschreitung);
        }
        x86_64::instructions::hlt();
    }

    // 2. Anfrage senden.
    verbindung_senden(anfrage);

    // 3. Antwort lesen, bis der Peer schließt (FIN -> CLOSE_WAIT) oder Timeout.
    let mut antwort = Vec::new();
    loop {
        pump_schritt(ferne_ip);
        empfang_anhaengen(&mut antwort);
        let z = zustand_lesen();
        let peer_fertig = matches!(
            z,
            Some(Zustand::CloseWait) | Some(Zustand::LastAck) | Some(Zustand::Closed) | None
        );
        if peer_fertig {
            break;
        }
        if crate::zeit::ms_seit_boot() >= deadline {
            break; // partielle Antwort ist besser als nichts
        }
        x86_64::instructions::hlt();
    }

    // 4. Sauber schließen (unser FIN, auf das ACK des Peers warten).
    verbindung_schliessen();
    let close_deadline = crate::zeit::ms_seit_boot() + 3000;
    loop {
        pump_schritt(ferne_ip);
        empfang_anhaengen(&mut antwort);
        if matches!(zustand_lesen(), Some(Zustand::Closed) | None) {
            break;
        }
        if crate::zeit::ms_seit_boot() >= close_deadline {
            break;
        }
        x86_64::instructions::hlt();
    }
    verbindung_raeumen();
    Ok(antwort)
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const IP_A: Ipv4 = Ipv4([10, 0, 0, 1]);
    const IP_B: Ipv4 = Ipv4([10, 0, 0, 2]);

    /// Sequenznummern-Arithmetik inkl. u32-Wraparound.
    #[test_case]
    fn test_tcp_seq_arithmetik() {
        assert!(seq_lt(1, 2));
        assert!(!seq_lt(2, 1));
        assert!(seq_leq(5, 5));
        assert!(seq_geq(5, 5));
        assert!(seq_gt(2, 1));
        // Über die Wickel-Grenze: 0xFFFFFFFF liegt VOR 0.
        assert!(seq_lt(0xFFFF_FFFF, 0));
        assert!(seq_gt(0, 0xFFFF_FFFF));
        assert!(seq_lt(0xFFFF_FFF0, 5)); // -16 vor +5
        assert!(!seq_lt(5, 0xFFFF_FFF0));
        // Ein ACK, das über den Wickel hinweg bestätigt.
        assert!(seq_gt(3u32, 0xFFFF_FFFEu32));
    }

    /// Der lossless Loopback-Kanal (für die Zustands-Tabelle unten).
    fn austauschen(a: &mut Verbindung, b: &mut Verbindung, t: u64) {
        for seg in a.ausgang_abholen() {
            b.segment_empfangen(IP_A, &seg, t);
        }
        for seg in b.ausgang_abholen() {
            a.segment_empfangen(IP_B, &seg, t);
        }
    }

    /// Die Zustandsübergänge als nachvollziehbare Tabelle: Handshake (aktiv
    /// UND passiv), Datenphase, geordneter Abbau bis CLOSED/TIME_WAIT.
    #[test_case]
    fn test_tcp_zustandsuebergaenge() {
        let mut t = 1000;
        // CLOSED -> SYN_SENT (aktiv) bzw. LISTEN (passiv).
        let mut a = Verbindung::verbinden_aktiv(IP_A, 40000, IP_B, 80, 1000, t);
        let mut b = Verbindung::lauschen(IP_B, 80, 5000);
        assert_eq!(a.zustand(), Zustand::SynSent);
        assert_eq!(b.zustand(), Zustand::Listen);

        // SYN -> b wird SYN_RCVD; SYN+ACK -> a wird ESTABLISHED; ACK -> b
        // wird ESTABLISHED. Ein Austausch pro Handshake-Segment.
        austauschen(&mut a, &mut b, t); // a's SYN -> b
        assert_eq!(b.zustand(), Zustand::SynRcvd);
        austauschen(&mut a, &mut b, t); // b's SYN+ACK -> a, a's ACK -> b
        assert_eq!(a.zustand(), Zustand::Established);
        assert_eq!(b.zustand(), Zustand::Established);

        // Datenphase: a -> b und zurück.
        a.senden(b"hallo", t);
        austauschen(&mut a, &mut b, t);
        let mut buf = [0u8; 16];
        let n = b.empfangen(&mut buf);
        assert_eq!(&buf[..n], b"hallo");
        austauschen(&mut a, &mut b, t); // b's ACK (+ Fenster-Update) -> a

        // Geordneter Abbau: a schließt aktiv.
        a.schliessen(t); // ESTABLISHED -> FIN_WAIT_1 (FIN geht raus)
        assert_eq!(a.zustand(), Zustand::FinWait1);
        austauschen(&mut a, &mut b, t); // a's FIN -> b (ESTABLISHED->CLOSE_WAIT), b's ACK -> a
        assert_eq!(b.zustand(), Zustand::CloseWait);
        assert_eq!(a.zustand(), Zustand::FinWait2);

        b.schliessen(t); // CLOSE_WAIT -> LAST_ACK (b's FIN geht raus)
        assert_eq!(b.zustand(), Zustand::LastAck);
        austauschen(&mut a, &mut b, t); // b's FIN -> a (FIN_WAIT_2->TIME_WAIT), a's ACK -> b
        assert_eq!(a.zustand(), Zustand::TimeWait);
        austauschen(&mut a, &mut b, t); // a's ACK -> b (LAST_ACK->CLOSED)
        assert_eq!(b.zustand(), Zustand::Closed);

        // TIME_WAIT läuft nach 2·MSL ab -> CLOSED.
        t += TIME_WAIT_MS + 1;
        a.tick(t);
        assert_eq!(a.zustand(), Zustand::Closed);
    }

    /// Retransmit-Auslösung: geht der SYN "verloren" (nichts wird
    /// zugestellt), muss der Timer ihn nach der RTO erneut senden.
    #[test_case]
    fn test_tcp_retransmit() {
        let mut t = 0;
        let mut a = Verbindung::verbinden_aktiv(IP_A, 40000, IP_B, 80, 1000, t);
        // Der erste SYN liegt im Ausgang — wir "verlieren" ihn (abholen +
        // wegwerfen), stellen also nichts zu.
        assert_eq!(a.ausgang_abholen().len(), 1);

        // Vor der RTO passiert nichts.
        t += RTO_START_MS - 1;
        a.tick(t);
        assert!(a.ausgang_abholen().is_empty(), "vor der RTO kein Retransmit");

        // Nach der RTO kommt ein zweiter SYN.
        t += 2;
        a.tick(t);
        let erneut = a.ausgang_abholen();
        assert_eq!(erneut.len(), 1, "Retransmit muss den SYN erneut senden");
        let s = segment_parse(&erneut[0]).unwrap();
        assert_eq!(s.flags & FLAG_SYN, FLAG_SYN);
        assert_eq!(s.seq, 1000, "SYN traegt die initiale Sequenznummer");

        // Backoff: der nächste Retransmit kommt erst nach ~2·RTO.
        t += RTO_START_MS; // = RTO_START, aber die neue Frist ist jetzt 2·RTO
        a.tick(t);
        assert!(a.ausgang_abholen().is_empty(), "Backoff: noch kein dritter SYN");
        t += RTO_START_MS + 1;
        a.tick(t);
        assert_eq!(a.ausgang_abholen().len(), 1, "nach 2·RTO der dritte SYN");
    }

    /// Ein simulierter Kanal mit einstellbarem Paketverlust (deterministischer
    /// LCG-"Zufall", damit der Test reproduzierbar ist).
    struct Kanal {
        rng: u64,
        verlust_prozent: u64,
    }
    impl Kanal {
        fn verloren(&mut self) -> bool {
            self.rng = self
                .rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.rng >> 33) % 100 < self.verlust_prozent
        }
    }

    /// Treibt beide Verbindungen über den verlustbehafteten Kanal, rückt die
    /// Zeit vor (löst Retransmits aus) und tickt beide.
    fn pumpe(a: &mut Verbindung, b: &mut Verbindung, kanal: &mut Kanal, t: &mut u64, runden: usize) {
        for _ in 0..runden {
            for seg in a.ausgang_abholen() {
                if !kanal.verloren() {
                    b.segment_empfangen(IP_A, &seg, *t);
                }
            }
            for seg in b.ausgang_abholen() {
                if !kanal.verloren() {
                    a.segment_empfangen(IP_B, &seg, *t);
                }
            }
            *t += 300; // Zeit vorrücken -> fällige Retransmits
            a.tick(*t);
            b.tick(*t);
        }
    }

    /// Der Loopback-Test: Handshake + Daten (beide Richtungen) + Close müssen
    /// auch bei 20 % Paketverlust sauber durchkommen — der Beweis, dass
    /// Zustandsautomat und Retransmit zusammenspielen.
    #[test_case]
    fn test_tcp_loopback_mit_verlust() {
        let mut t = 0u64;
        let mut a = Verbindung::verbinden_aktiv(IP_A, 40000, IP_B, 80, 1_000_000, t);
        let mut b = Verbindung::lauschen(IP_B, 80, 3_000_000);
        let mut kanal = Kanal { rng: 0x1234_5678_9abc_def0, verlust_prozent: 20 };

        // Handshake unter Verlust.
        pumpe(&mut a, &mut b, &mut kanal, &mut t, 200);
        assert!(a.ist_verbunden(), "a nicht verbunden (Zustand {:?})", a.zustand());
        assert!(b.ist_verbunden(), "b nicht verbunden (Zustand {:?})", b.zustand());

        // Eine größere Nachricht a -> b (mehrere Segmente).
        let nachricht: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(a.senden(&nachricht, t), nachricht.len());
        pumpe(&mut a, &mut b, &mut kanal, &mut t, 600);
        // b liest die komplette Nachricht zusammen.
        let mut empfangen = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = b.empfangen(&mut buf);
            if n == 0 {
                break;
            }
            empfangen.extend_from_slice(&buf[..n]);
        }
        assert_eq!(empfangen, nachricht, "Daten kamen nicht vollstaendig/geordnet an");

        // Antwort b -> a.
        b.senden(b"HTTP/1.0 200 OK", t);
        pumpe(&mut a, &mut b, &mut kanal, &mut t, 400);
        let mut abuf = [0u8; 64];
        let n = a.empfangen(&mut abuf);
        assert_eq!(&abuf[..n], b"HTTP/1.0 200 OK");

        // Sauberer Abbau von beiden Seiten.
        a.schliessen(t);
        pumpe(&mut a, &mut b, &mut kanal, &mut t, 300);
        b.schliessen(t);
        pumpe(&mut a, &mut b, &mut kanal, &mut t, 300);
        // Genug Zeit für TIME_WAIT.
        t += TIME_WAIT_MS + 1;
        a.tick(t);
        b.tick(t);
        assert!(a.ist_geschlossen(), "a nicht geschlossen (Zustand {:?})", a.zustand());
        assert!(b.ist_geschlossen(), "b nicht geschlossen (Zustand {:?})", b.zustand());
    }
}
