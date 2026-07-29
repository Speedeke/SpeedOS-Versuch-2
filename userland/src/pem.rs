// pem.rs — PEM-Blöcke zu DER, und ein Minimal-Blick in X.509
//          (Serie 7, Teil 2)
//
// ==========================================================================
// WARUM DAS HIER IM USER-SPACE LIEGT
//
// Dieser Code liest eine Datei, die von aussen kommt — im Zweifel eine, die
// jemand ersetzt hat. Ein Fehler darin soll einen PROZESS treffen und nicht
// den Kernel; genau dafuer gibt es seit Serie 6 Ring 3. Der Kernel kennt das
// CA-Buendel nur als Bytes: Er schreibt es beim Boot hin und liest es nie.
//
// Die Haltung ist dieselbe wie beim ELF-Lader (src/elf.rs):
// **JEDE ZAHL IN DER DATEI IST DIE BEHAUPTUNG EINES FREMDEN.** Es wird
// nichts geglaubt, nichts geraten, und es wird NIE gepanickt.
//
// ==========================================================================
// WAS DIESER PARSER IST — UND WAS NICHT
//
// PEM-Teil: Er sucht `-----BEGIN CERTIFICATE-----`, sammelt bis
// `-----END CERTIFICATE-----`, dekodiert Base64. Mehr nicht. Alles
// ausserhalb der Bloecke ist Kommentar (in einem CA-Buendel stehen dort die
// Namen der Stellen).
//
// DIE WICHTIGSTE ENTSCHEIDUNG: **Ein kaputter Block macht NUR DIESEN Block
// ungueltig, nicht die Datei.** Ein Vertrauensanker mit 145 von 146 lesbaren
// Wurzeln ist brauchbar; einer, der bei einem Zeilenumbruch-Fehler auf 0
// faellt, ist eine Ausfallquelle.
//
// X.509-Teil: ein DER-Laeufer, der GENAU DREI Dinge herausholt — den Common
// Name des Subjects und die beiden Zeitangaben aus `Validity`. Das ist
// ausdruecklich **kein X.509-Parser**: Es wird nichts geprueft, nichts
// validiert und nichts geglaubt. Es ist eine ANZEIGE-HILFE, damit ein Mensch
// sieht, was da liegt. Die echte Zerlegung macht spaeter `rustls-webpki`
// (docs/tls-vertrauen.md §4).

/// Hoechstzahl Zertifikate, die wir aus einer Datei nehmen. Ein CA-Buendel
/// hat ~150; 512 ist reichlich Luft und trotzdem eine Grenze.
pub const MAX_ZERTIFIKATE: usize = 512;
/// Hoechstgroesse EINES DER-Blocks. Zertifikate sind wenige KiB.
pub const MAX_DER_BYTES: usize = 16 * 1024;

const BEGIN_MARKE: &[u8] = b"-----BEGIN ";
const END_MARKE: &[u8] = b"-----END ";
const ZERT_TYP: &[u8] = b"CERTIFICATE-----";

/// Warum ein einzelner Block nicht gelesen werden konnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PemFehler {
    /// Zu `BEGIN` fehlt das `END`.
    KeinEnde,
    /// Ein Zeichen, das in Base64 nicht vorkommt.
    UngueltigesZeichen,
    /// Die Base64-Laenge geht nicht auf (Anzahl % 4 == 1 ist unmoeglich).
    UngueltigeLaenge,
    /// Der Block ist leer.
    Leer,
    /// Ueber `MAX_DER_BYTES`.
    ZuGross,
}

impl PemFehler {
    pub fn meldung(self) -> &'static str {
        match self {
            PemFehler::KeinEnde => "END-Marke fehlt",
            PemFehler::UngueltigesZeichen => "ungueltiges Base64-Zeichen",
            PemFehler::UngueltigeLaenge => "Base64-Laenge geht nicht auf",
            PemFehler::Leer => "leerer Block",
            PemFehler::ZuGross => "Block zu gross",
        }
    }
}

/// Ein gefundener Block: der DER-Inhalt plus, wo er in der Datei stand.
pub struct Block<'a> {
    /// Die dekodierten DER-Bytes.
    pub der: &'a [u8],
    /// Der wievielte BEGIN-CERTIFICATE-Block der Datei war das (ab 0)?
    pub nummer: usize,
}

/// Das Ergebnis eines Datei-Durchgangs.
pub struct Bestand {
    /// Wie viele Bloecke sauber gelesen wurden.
    pub gelesen: usize,
    /// Wie viele Bloecke kaputt waren (uebersprungen).
    pub kaputt: usize,
    /// Wie viele Bloecke wegen `MAX_ZERTIFIKATE` gar nicht mehr angesehen
    /// wurden.
    pub uebrig: usize,
    /// Der erste aufgetretene Fehler (fuer die Anzeige).
    pub erster_fehler: Option<PemFehler>,
}

/// Sucht Base64-Zeichen: A-Z a-z 0-9 + / und '=' als Fuellzeichen.
fn base64_wert(zeichen: u8) -> Option<u8> {
    match zeichen {
        b'A'..=b'Z' => Some(zeichen - b'A'),
        b'a'..=b'z' => Some(zeichen - b'a' + 26),
        b'0'..=b'9' => Some(zeichen - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Dekodiert Base64 in `ziel` und liefert die Byte-Zahl.
///
/// Ueberspringt Zeilenumbrueche und Leerzeichen (PEM bricht alle 64
/// Zeichen um) und lehnt alles andere ab. `=` beendet die Eingabe — was
/// danach kommt, wird ignoriert, wie es die Base64-Regel vorsieht.
pub fn base64_dekodieren(eingabe: &[u8], ziel: &mut [u8]) -> Result<usize, PemFehler> {
    let mut geschrieben = 0usize;
    // Sammelt bis zu 4 Base64-Zeichen (je 6 Bit) zu 3 Bytes.
    let mut puffer: u32 = 0;
    let mut bits = 0u32;
    let mut gesehen = 0usize;

    for &zeichen in eingabe {
        // Zeilenumbrueche und Weissraum sind in PEM normal.
        if zeichen == b'\n' || zeichen == b'\r' || zeichen == b' ' || zeichen == b'\t' {
            continue;
        }
        if zeichen == b'=' {
            break; // Fuellzeichen: ab hier kommen keine Daten mehr.
        }
        let wert = base64_wert(zeichen).ok_or(PemFehler::UngueltigesZeichen)?;
        puffer = (puffer << 6) | wert as u32;
        bits += 6;
        gesehen += 1;
        if bits >= 8 {
            bits -= 8;
            let byte = ((puffer >> bits) & 0xff) as u8;
            if geschrieben >= ziel.len() {
                return Err(PemFehler::ZuGross);
            }
            ziel[geschrieben] = byte;
            geschrieben += 1;
        }
    }

    // Anzahl % 4 == 1 kann es in Base64 nicht geben: Ein einzelnes Zeichen
    // traegt 6 Bit und damit kein volles Byte.
    if gesehen % 4 == 1 {
        return Err(PemFehler::UngueltigeLaenge);
    }
    if geschrieben == 0 {
        return Err(PemFehler::Leer);
    }
    Ok(geschrieben)
}

/// Sucht `nadel` in `heu` ab `ab` und liefert den Index.
fn suchen(heu: &[u8], nadel: &[u8], ab: usize) -> Option<usize> {
    if nadel.is_empty() || heu.len() < nadel.len() {
        return None;
    }
    let mut i = ab;
    while i + nadel.len() <= heu.len() {
        if &heu[i..i + nadel.len()] == nadel {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// GEHT DIE PEM-DATEI DURCH und ruft `je_block` fuer jeden lesbaren
/// CERTIFICATE-Block.
///
/// `arbeitspuffer` nimmt die dekodierten DER-Bytes auf (muss mindestens
/// so gross sein, wie das groesste erwartete Zertifikat — `MAX_DER_BYTES`).
///
/// Warum ein Rueckruf und keine Liste: Ein User-Programm hat 64 KiB Stack
/// und einen kleinen Heap. 150 Zertifikate am Stueck zu halten waere
/// Verschwendung; sie werden einzeln angesehen und wieder vergessen.
pub fn bloecke_durchgehen(
    pem: &[u8],
    arbeitspuffer: &mut [u8],
    mut je_block: impl FnMut(Block<'_>),
) -> Bestand {
    let mut bestand = Bestand {
        gelesen: 0,
        kaputt: 0,
        uebrig: 0,
        erster_fehler: None,
    };
    let mut pos = 0usize;
    let mut nummer = 0usize;

    while let Some(begin) = suchen(pem, BEGIN_MARKE, pos) {
        let typ_start = begin + BEGIN_MARKE.len();
        // NUR Zertifikate. Andere Block-Typen (BEGIN PRIVATE KEY ...)
        // werden UEBERSPRUNGEN und nicht als Fehler gezaehlt: In einem
        // CA-Buendel haben sie nichts zu suchen, aber sie sind auch kein
        // Grund, die restlichen 140 Zertifikate wegzuwerfen.
        let ist_zertifikat =
            typ_start + ZERT_TYP.len() <= pem.len() && &pem[typ_start..typ_start + ZERT_TYP.len()] == ZERT_TYP;
        if !ist_zertifikat {
            pos = typ_start;
            continue;
        }
        let daten_start = typ_start + ZERT_TYP.len();

        let ende = match suchen(pem, END_MARKE, daten_start) {
            Some(ende) => ende,
            None => {
                // BEGIN ohne END: Der Rest der Datei ist unbrauchbar, aber
                // alles davor bleibt gueltig.
                bestand.kaputt += 1;
                bestand.erster_fehler.get_or_insert(PemFehler::KeinEnde);
                break;
            }
        };

        if nummer >= MAX_ZERTIFIKATE {
            bestand.uebrig += 1;
            pos = ende + END_MARKE.len();
            nummer += 1;
            continue;
        }

        match base64_dekodieren(&pem[daten_start..ende], arbeitspuffer) {
            Ok(laenge) => {
                bestand.gelesen += 1;
                je_block(Block {
                    der: &arbeitspuffer[..laenge],
                    nummer,
                });
            }
            Err(fehler) => {
                // NUR DIESER BLOCK ist hin — weiter mit dem naechsten.
                bestand.kaputt += 1;
                bestand.erster_fehler.get_or_insert(fehler);
            }
        }
        nummer += 1;
        pos = ende + END_MARKE.len();
    }
    bestand
}

// ===========================================================================
// DER MINIMAL-BLICK IN X.509 (nur zum ANZEIGEN)
// ===========================================================================
//
// DER ist Tag-Laenge-Wert. Ein Zertifikat sieht so aus:
//
//   SEQUENCE (Certificate)
//     SEQUENCE (TBSCertificate)
//       [0] version            (optional)
//       INTEGER serialNumber
//       SEQUENCE signature
//       SEQUENCE issuer        <- Name
//       SEQUENCE validity      <- notBefore, notAfter
//       SEQUENCE subject       <- Name
//       ...
//
// Wir laufen genau so weit hinein, wie es fuer die Anzeige noetig ist, und
// geben bei jeder Ueberraschung auf (`None`) statt zu raten.

/// Ein DER-Element: Tag, Inhalt, und wo es aufhoert.
struct Element<'a> {
    tag: u8,
    inhalt: &'a [u8],
    /// Index HINTER diesem Element (im uebergeordneten Puffer).
    ende: usize,
}

/// Liest EIN DER-Element ab `pos`. `None` bei jeder Unstimmigkeit.
///
/// Die Laengen-Kodierung ist die einzige knifflige Stelle: Bit 7 des ersten
/// Laengen-Bytes sagt „lang". Dann nennen die unteren 7 Bit, wie viele
/// Folgebytes die Laenge bilden. Laengen ueber 4 Bytes lehnen wir ab — ein
/// Zertifikat mit ueber 4 GiB gibt es nicht, und ein solcher Wert ist
/// entweder Unsinn oder ein Angriff.
fn element_lesen(daten: &[u8], pos: usize) -> Option<Element<'_>> {
    let tag = *daten.get(pos)?;
    let laengen_byte = *daten.get(pos + 1)?;
    let (laenge, kopf) = if laengen_byte & 0x80 == 0 {
        (laengen_byte as usize, 2usize)
    } else {
        let bytes = (laengen_byte & 0x7f) as usize;
        if bytes == 0 || bytes > 4 {
            return None; // unbestimmte Laenge (BER) oder absurd — nein
        }
        let mut laenge = 0usize;
        for i in 0..bytes {
            laenge = (laenge << 8) | *daten.get(pos + 2 + i)? as usize;
        }
        (laenge, 2 + bytes)
    };
    let start = pos.checked_add(kopf)?;
    let ende = start.checked_add(laenge)?;
    if ende > daten.len() {
        return None;
    }
    Some(Element {
        tag,
        inhalt: &daten[start..ende],
        ende,
    })
}

/// Was wir aus einem Zertifikat fuer die Anzeige herausholen.
#[derive(Debug, Clone, Copy, Default)]
pub struct Kurzinfo<'a> {
    /// Der Common Name des Subjects (leer = nicht gefunden).
    pub name: &'a [u8],
    /// notBefore/notAfter als UNIX-Sekunden (0 = nicht lesbar).
    pub gueltig_ab: u64,
    pub gueltig_bis: u64,
}

/// DER-Tags, die wir kennen.
const TAG_SEQUENCE: u8 = 0x30;
const TAG_SET: u8 = 0x31;
const TAG_OID: u8 = 0x06;
const TAG_UTC_TIME: u8 = 0x17;
const TAG_GENERALIZED_TIME: u8 = 0x18;

/// OID 2.5.4.3 = commonName, in DER: 55 04 03.
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];

/// Holt Name und Gueltigkeit aus einem DER-Zertifikat.
///
/// Liefert IMMER etwas (notfalls leere Felder) — diese Funktion darf nicht
/// scheitern, sie zeigt nur an.
pub fn kurzinfo(der: &[u8]) -> Kurzinfo<'_> {
    let mut info = Kurzinfo::default();
    // Certificate ::= SEQUENCE { tbsCertificate, ... }
    let zertifikat = match element_lesen(der, 0) {
        Some(e) if e.tag == TAG_SEQUENCE => e,
        _ => return info,
    };
    let tbs = match element_lesen(zertifikat.inhalt, 0) {
        Some(e) if e.tag == TAG_SEQUENCE => e,
        _ => return info,
    };

    // Durch die Felder des TBSCertificate laufen. Wir suchen (a) die
    // Validity-SEQUENCE (erkennbar an zwei Zeit-Elementen darin) und
    // (b) die LETZTE Name-SEQUENCE davor bzw. die erste danach — das
    // Subject steht direkt HINTER der Validity.
    let mut pos = 0usize;
    let mut validity_gesehen = false;
    while let Some(feld) = element_lesen(tbs.inhalt, pos) {
        if feld.ende <= pos {
            break; // Fortschritts-Garantie: nie in einer Schleife haengen
        }
        if feld.tag == TAG_SEQUENCE {
            if !validity_gesehen {
                // Ist das die Validity? Sie besteht aus GENAU zwei
                // Zeit-Elementen — daran ist sie sicher zu erkennen, ohne
                // die Felder zu zaehlen (die Version ist optional).
                if let Some((ab, bis)) = validity_lesen(feld.inhalt) {
                    info.gueltig_ab = ab;
                    info.gueltig_bis = bis;
                    validity_gesehen = true;
                    pos = feld.ende;
                    continue;
                }
            } else if info.name.is_empty() {
                // Die erste Name-SEQUENCE NACH der Validity ist das Subject.
                if let Some(name) = common_name_suchen(feld.inhalt) {
                    info.name = name;
                    break;
                }
            }
        }
        pos = feld.ende;
    }
    info
}

/// Validity ::= SEQUENCE { notBefore Time, notAfter Time }
fn validity_lesen(inhalt: &[u8]) -> Option<(u64, u64)> {
    let erst = element_lesen(inhalt, 0)?;
    let zweit = element_lesen(inhalt, erst.ende)?;
    // Genau zwei Elemente, und beide sind Zeiten.
    if zweit.ende != inhalt.len() {
        return None;
    }
    let ab = zeit_lesen(erst.tag, erst.inhalt)?;
    let bis = zeit_lesen(zweit.tag, zweit.inhalt)?;
    Some((ab, bis))
}

/// Sucht in einem Name (SEQUENCE OF SET OF AttributeTypeAndValue) den
/// commonName.
fn common_name_suchen(inhalt: &[u8]) -> Option<&[u8]> {
    let mut pos = 0usize;
    while let Some(rdn) = element_lesen(inhalt, pos) {
        if rdn.ende <= pos {
            return None;
        }
        if rdn.tag == TAG_SET {
            let mut innen = 0usize;
            while let Some(paar) = element_lesen(rdn.inhalt, innen) {
                if paar.ende <= innen {
                    break;
                }
                if paar.tag == TAG_SEQUENCE {
                    let oid = element_lesen(paar.inhalt, 0)?;
                    if oid.tag == TAG_OID && oid.inhalt == OID_COMMON_NAME {
                        let wert = element_lesen(paar.inhalt, oid.ende)?;
                        return Some(wert.inhalt);
                    }
                }
                innen = paar.ende;
            }
        }
        pos = rdn.ende;
    }
    None
}

/// Wandelt eine DER-Zeit in UNIX-Sekunden.
///
/// UTCTime ist `YYMMDDHHMMSSZ` — mit der beruehmten Zweistelligkeit:
/// 50..99 heisst 19xx, 00..49 heisst 20xx (RFC 5280 §4.1.2.5.1).
/// GeneralizedTime ist `YYYYMMDDHHMMSSZ`, also vierstellig.
fn zeit_lesen(tag: u8, inhalt: &[u8]) -> Option<u64> {
    let ziffer = |b: u8| -> Option<u64> {
        if b.is_ascii_digit() {
            Some((b - b'0') as u64)
        } else {
            None
        }
    };
    let zahl = |bytes: &[u8]| -> Option<u64> {
        let mut wert = 0u64;
        for &b in bytes {
            wert = wert * 10 + ziffer(b)?;
        }
        Some(wert)
    };

    let (jahr, rest) = match tag {
        TAG_UTC_TIME if inhalt.len() >= 12 => {
            let jj = zahl(&inhalt[0..2])?;
            let jahr = if jj >= 50 { 1900 + jj } else { 2000 + jj };
            (jahr, &inhalt[2..])
        }
        TAG_GENERALIZED_TIME if inhalt.len() >= 14 => (zahl(&inhalt[0..4])?, &inhalt[4..]),
        _ => return None,
    };
    let monat = zahl(&rest[0..2])?;
    let tag_im_monat = zahl(&rest[2..4])?;
    let stunde = zahl(&rest[4..6])?;
    let minute = zahl(&rest[6..8])?;
    let sekunde = zahl(&rest[8..10])?;
    if !(1..=12).contains(&monat) || !(1..=31).contains(&tag_im_monat) {
        return None;
    }
    Some(unix_aus_datum(jahr, monat, tag_im_monat, stunde, minute, sekunde))
}

/// Tage seit 1970 -> UNIX-Sekunden (reine Kalender-Arithmetik, wie in
/// `zeit::sekunden_seit_2000` im Kernel, nur mit anderer Epoche).
pub fn unix_aus_datum(
    jahr: u64,
    monat: u64,
    tag: u64,
    stunde: u64,
    minute: u64,
    sekunde: u64,
) -> u64 {
    let schaltjahr =
        |j: u64| j.is_multiple_of(4) && (!j.is_multiple_of(100) || j.is_multiple_of(400));
    let mut tage = 0u64;
    for j in 1970..jahr {
        tage += if schaltjahr(j) { 366 } else { 365 };
    }
    const MONATSTAGE: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..monat.min(13) {
        tage += MONATSTAGE[(m - 1) as usize];
        if m == 2 && schaltjahr(jahr) {
            tage += 1;
        }
    }
    tage += tag.saturating_sub(1);
    tage * 86_400 + stunde * 3600 + minute * 60 + sekunde
}
