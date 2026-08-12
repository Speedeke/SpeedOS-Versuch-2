// usb::deskriptor — was ein Geraet ueber sich selbst behauptet
//
// ===========================================================================
// JEDES BYTE HIER IST DIE BEHAUPTUNG EINES FREMDEN
//
// Ein USB-Deskriptor kommt vom Geraet. Er ist damit dieselbe Sorte Daten
// wie ein ELF-Header (Serie 6, Teil 5), ein PNG (Serie 8, Teil 3) oder
// eine HTTP-Antwort: **fremd und potenziell feindlich.** Ein Geraet, das
// sich falsch meldet, muss zu einem Fehlerwert fuehren und niemals zu
// einer Panik, einem Ueberlauf oder einer Endlosschleife.
//
// Das ist keine graue Theorie: USB-Geraete sind physisch steckbar. Wer
// einen Stick in einen fremden Rechner steckt, kann jedes Byte dieser
// Struktur frei waehlen — das ist die klassische „BadUSB"-Lage. Der
// Parser hier ist die erste Verteidigungslinie, und er ist bewusst so
// gebaut wie `elf::pruefen`: eine REINE FUNKTION auf `&[u8]`, ohne
// Hardware, ohne Locks, ohne unsafe, testbar bis in jede Ecke.
//
// ===========================================================================
// DIE DREI REGELN, DIE JEDER PRUEFUNG HIER ZUGRUNDE LIEGEN
//
//   (1) DAS LAENGENFELD IST EINE BEHAUPTUNG, KEINE TATSACHE. Jeder
//       Deskriptor traegt sein `bLength` selbst. Wer ihm glaubt, liest
//       ueber das Ende des Puffers hinaus (oder dreht sich im Kreis,
//       wenn es 0 ist). Es wird deshalb IMMER gegen die tatsaechlich
//       empfangene Menge geprueft.
//
//   (2) KEINE SCHLEIFE OHNE OBERGRENZE. Die Deskriptor-Kette einer
//       Konfiguration ist eine Liste variabler Laenge. Ein `bLength` von
//       0 waere eine Endlosschleife; deshalb gibt es sowohl die
//       Null-Pruefung ALS AUCH einen harten Zaehler. Zwei Riegel, weil
//       ein einzelner uebersehen werden kann.
//
//   (3) ABGESCHNITTEN IST BESSER ALS ABGELEHNT. Findet sich mehr, als
//       wir verwalten koennen (zu viele Endpunkte, zu viele
//       Interfaces), wird GEKUERZT und GEZAEHLT — dieselbe Haltung wie
//       bei speedhtml. Eine Maus mit einem brauchbaren Endpunkt ist
//       besser als eine abgelehnte Maus.

use alloc::string::String;
use alloc::vec::Vec;

// ===========================================================================
// GRENZEN
// ===========================================================================

/// Wie viele Endpunkte wir je Interface fuehren.
///
/// Die USB-Spezifikation erlaubt 15 IN + 15 OUT. Eine Tastatur hat
/// einen, eine Maus einen, ein Massenspeicher zwei. 16 ist reichlich
/// und deckelt zugleich, was ein boesartiges Geraet behaupten kann.
pub const MAX_ENDPUNKTE: usize = 16;

/// Wie viele Interfaces je Konfiguration.
pub const MAX_INTERFACES: usize = 8;

/// Wie viele Deskriptoren eine Kette haben darf, bevor abgebrochen wird.
/// Der zweite Riegel gegen Endlosschleifen (Regel 2).
pub const MAX_KETTE: usize = 256;

/// Laengste Zeichenkette, die wir aus einem String-Deskriptor nehmen.
pub const MAX_STRING_ZEICHEN: usize = 126;

// ===========================================================================
// DESKRIPTOR-TYPEN
// ===========================================================================

pub const TYP_DEVICE: u8 = 1;
pub const TYP_CONFIGURATION: u8 = 2;
pub const TYP_STRING: u8 = 3;
pub const TYP_INTERFACE: u8 = 4;
pub const TYP_ENDPOINT: u8 = 5;

/// Die USB-Geraeteklassen, die uns interessieren.
pub const KLASSE_HID: u8 = 0x03;
pub const KLASSE_MASSENSPEICHER: u8 = 0x08;
pub const KLASSE_HUB: u8 = 0x09;

// ===========================================================================
// FEHLER
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeskriptorFehler {
    /// Weniger Bytes empfangen, als ein Deskriptor mindestens braucht.
    ZuKurz,
    /// `bLength` behauptet mehr, als tatsaechlich da ist.
    LaengeLuegt,
    /// `bLength` ist 0 — waere eine Endlosschleife.
    LaengeNull,
    /// `bDescriptorType` passt nicht zu dem, was angefordert wurde.
    FalscherTyp,
}

impl DeskriptorFehler {
    pub fn text(self) -> &'static str {
        match self {
            DeskriptorFehler::ZuKurz => "Deskriptor zu kurz",
            DeskriptorFehler::LaengeLuegt => "bLength groesser als die Antwort",
            DeskriptorFehler::LaengeNull => "bLength ist 0",
            DeskriptorFehler::FalscherTyp => "unerwarteter Deskriptor-Typ",
        }
    }
}

// ===========================================================================
// DEVICE-DESKRIPTOR
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GeraeteDeskriptor {
    pub usb_version: u16,
    pub klasse: u8,
    pub unterklasse: u8,
    pub protokoll: u8,
    /// Maximale Paketgroesse von Endpunkt 0.
    pub max_paket0: u16,
    pub hersteller_id: u16,
    pub produkt_id: u16,
    pub geraete_version: u16,
    /// String-Indizes — 0 heisst „gibt es nicht".
    pub index_hersteller: u8,
    pub index_produkt: u8,
    pub index_seriennummer: u8,
    pub konfigurationen: u8,
}

/// Der Device-Deskriptor ist immer 18 Byte lang.
pub const DEVICE_DESKRIPTOR_BYTES: usize = 18;

/// Den Device-Deskriptor lesen.
///
/// `daten` ist, was WIRKLICH angekommen ist — nicht, was das Geraet
/// behauptet.
pub fn geraet_parsen(daten: &[u8]) -> Result<GeraeteDeskriptor, DeskriptorFehler> {
    if daten.len() < DEVICE_DESKRIPTOR_BYTES {
        return Err(DeskriptorFehler::ZuKurz);
    }
    let laenge = daten[0];
    if laenge == 0 {
        return Err(DeskriptorFehler::LaengeNull);
    }
    if daten[1] != TYP_DEVICE {
        return Err(DeskriptorFehler::FalscherTyp);
    }
    // Das Laengenfeld MUSS zu dem passen, was ein Device-Deskriptor ist.
    // Ein Geraet, das hier 200 behauptet, bekommt keinen Vertrauensbonus.
    if (laenge as usize) < DEVICE_DESKRIPTOR_BYTES {
        return Err(DeskriptorFehler::LaengeLuegt);
    }

    // `max_paket0` ist bei USB 3 ein EXPONENT (2^wert), bei USB 2 die
    // Zahl selbst. Wer das verwechselt, programmiert dem Controller eine
    // Paketgroesse von 9 statt 512 — und jede Uebertragung bricht ab.
    let usb_version = u16le(daten, 2);
    let roh_paket = daten[7];
    let max_paket0 = if usb_version >= 0x0300 {
        // Exponent, vernuenftig gedeckelt: 2^15 = 32768 ist weit ueber
        // allem Erlaubten, aber ein Shift um 200 waere ein Ueberlauf.
        1u16.checked_shl(roh_paket.min(15) as u32).unwrap_or(64)
    } else {
        roh_paket as u16
    };

    Ok(GeraeteDeskriptor {
        usb_version,
        klasse: daten[4],
        unterklasse: daten[5],
        protokoll: daten[6],
        max_paket0,
        hersteller_id: u16le(daten, 8),
        produkt_id: u16le(daten, 10),
        geraete_version: u16le(daten, 12),
        index_hersteller: daten[14],
        index_produkt: daten[15],
        index_seriennummer: daten[16],
        konfigurationen: daten[17],
    })
}

// ===========================================================================
// CONFIGURATION, INTERFACE, ENDPUNKT
// ===========================================================================

/// Die Uebertragungsart eines Endpunkts (`bmAttributes`, Bits 0..1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uebertragung {
    Control,
    Isochron,
    Bulk,
    /// **Das ist, was Tastatur und Maus benutzen.**
    Interrupt,
}

impl Uebertragung {
    pub fn text(self) -> &'static str {
        match self {
            Uebertragung::Control => "control",
            Uebertragung::Isochron => "isochron",
            Uebertragung::Bulk => "bulk",
            Uebertragung::Interrupt => "interrupt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpunkt {
    /// `bEndpointAddress` roh — Bit 7 = Richtung, Bits 0..3 = Nummer.
    pub adresse: u8,
    pub art: Uebertragung,
    pub max_paket: u16,
    /// `bInterval` — bei Interrupt-Endpunkten das Abfrageintervall.
    pub intervall: u8,
}

impl Endpunkt {
    /// Nummer des Endpunkts (1..15).
    pub fn nummer(&self) -> u8 {
        self.adresse & 0x0F
    }
    /// Zeigt er zum HOST (IN)? Das ist die Richtung, aus der eine
    /// Tastatur ihre Tastendruecke liefert.
    pub fn ist_eingang(&self) -> bool {
        self.adresse & 0x80 != 0
    }
    /// Die xHCI-Endpunkt-Nummer („Device Context Index").
    ///
    /// ===================================================================
    /// DIE UMRECHNUNG, DIE MAN FALSCH MACHT
    ///
    /// xHCI nummeriert Endpunkte NICHT wie USB. Im Device Context ist
    /// Index 1 der Control-Endpunkt (EP0), und danach wechseln sich OUT
    /// und IN ab:
    ///
    ///     DCI = (Endpunktnummer * 2) + (IN ? 1 : 0)
    ///
    /// Endpunkt 1 IN wird also zu DCI 3, Endpunkt 1 OUT zu DCI 2. Wer
    /// stattdessen die USB-Nummer nimmt, konfiguriert einen ganz anderen
    /// Endpunkt — und der Controller meldet keinen Fehler, es kommen
    /// nur nie Daten.
    pub fn dci(&self) -> u8 {
        self.nummer() * 2 + if self.ist_eingang() { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schnittstelle {
    pub nummer: u8,
    pub alternativ: u8,
    pub klasse: u8,
    pub unterklasse: u8,
    pub protokoll: u8,
    pub index_name: u8,
    pub endpunkte: Vec<Endpunkt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Konfiguration {
    /// `bConfigurationValue` — der Wert, den `Set Configuration` will.
    pub wert: u8,
    pub index_name: u8,
    /// `bmAttributes`: Bit 6 = selbstversorgt, Bit 5 = Remote-Wakeup.
    pub attribute: u8,
    /// In 2-mA-Schritten.
    pub max_strom: u8,
    pub schnittstellen: Vec<Schnittstelle>,
    /// Was beim Parsen gekuerzt oder uebersprungen wurde.
    pub befund: KonfigBefund,
}

/// Die Buchhaltung ueber das, was zurechtgebogen wurde.
///
/// Ohne sie waere die Fehlertoleranz eine Blackbox — dieselbe
/// Ueberlegung wie bei `speedhtml::Befund`. Bei einem Geraet, das nicht
/// tut, was es soll, ist „hat der Parser etwas weggeworfen?" die erste
/// Frage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KonfigBefund {
    /// Endpunkte, die ueber `MAX_ENDPUNKTE` hinausgingen.
    pub endpunkte_gekuerzt: usize,
    /// Interfaces ueber `MAX_INTERFACES`.
    pub interfaces_gekuerzt: usize,
    /// Deskriptoren, die wir nicht kennen (HID, Klassen-spezifisch …).
    pub uebersprungen: usize,
    /// Die Kette war laenger als `MAX_KETTE` oder ein `bLength` war 0.
    pub abgebrochen: bool,
    /// `wTotalLength` behauptete mehr, als angekommen ist.
    pub laenge_gekuerzt: bool,
}

/// Die Konfigurations-Kette parsen.
///
/// ===================================================================
/// DER AUFBAU, DEN MAN KENNEN MUSS
///
/// Ein Configuration-Deskriptor kommt NICHT allein: Dahinter haengen in
/// EINEM Puffer alle Interface- und Endpunkt-Deskriptoren, dazwischen
/// beliebige klassenspezifische (z. B. HID). Es ist eine flache Liste,
/// die man von vorn nach hinten abgeht; die Zugehoerigkeit ergibt sich
/// aus der REIHENFOLGE — jeder Endpunkt gehoert zum zuletzt gesehenen
/// Interface.
///
/// Genau deshalb ist ein `bLength` von 0 hier so gefaehrlich: Es gibt
/// keinen anderen Weg vorwaerts als „Position + bLength".
pub fn konfiguration_parsen(daten: &[u8]) -> Result<Konfiguration, DeskriptorFehler> {
    if daten.len() < 9 {
        return Err(DeskriptorFehler::ZuKurz);
    }
    if daten[0] == 0 {
        return Err(DeskriptorFehler::LaengeNull);
    }
    if daten[1] != TYP_CONFIGURATION {
        return Err(DeskriptorFehler::FalscherTyp);
    }

    let mut befund = KonfigBefund::default();

    // `wTotalLength` sagt, wie lang die GANZE Kette ist. Es ist eine
    // Behauptung: Ist es groesser als das, was angekommen ist, wird auf
    // die tatsaechliche Menge GEKUERZT (und das vermerkt) — niemals
    // darueber hinaus gelesen.
    let behauptet = u16le(daten, 2) as usize;
    let ende = if behauptet > daten.len() {
        befund.laenge_gekuerzt = true;
        daten.len()
    } else if behauptet < 9 {
        // Auch nach UNTEN luegen ist moeglich. Dann gilt der Kopf.
        befund.laenge_gekuerzt = true;
        daten.len()
    } else {
        behauptet
    };

    let mut konfiguration = Konfiguration {
        wert: daten[5],
        index_name: daten[6],
        attribute: daten[7],
        max_strom: daten[8],
        schnittstellen: Vec::new(),
        befund,
    };

    // Die Kette abgehen. Startpunkt ist hinter dem Kopf — und zwar
    // hinter SEINEM bLength, nicht hinter den festen 9 Byte: Manche
    // Geraete haengen dem Kopf etwas an.
    let mut pos = (daten[0] as usize).max(9);
    let mut runden = 0usize;

    while pos + 2 <= ende {
        runden += 1;
        // RIEGEL 2: der harte Zaehler. Auch wenn `bLength` immer > 0
        // ist, koennte eine absurd lange Kette uns hier festhalten.
        if runden > MAX_KETTE {
            konfiguration.befund.abgebrochen = true;
            break;
        }
        let laenge = daten[pos] as usize;
        let typ = daten[pos + 1];

        // RIEGEL 1: bLength == 0 waere eine Endlosschleife, denn die
        // Position kaeme nicht voran. Das ist die Invariante dieser
        // Schleife, und sie steht hier, damit sie nicht verlorengeht:
        // **Jeder Durchlauf MUSS `pos` vergroessern.**
        if laenge == 0 {
            konfiguration.befund.abgebrochen = true;
            break;
        }
        // Ein Deskriptor, der ueber das Ende hinausragt, wird nicht
        // halb gelesen — er wird verworfen.
        if pos + laenge > ende {
            konfiguration.befund.abgebrochen = true;
            break;
        }
        let stueck = &daten[pos..pos + laenge];

        match typ {
            TYP_INTERFACE if laenge >= 9 => {
                if konfiguration.schnittstellen.len() >= MAX_INTERFACES {
                    konfiguration.befund.interfaces_gekuerzt += 1;
                } else {
                    konfiguration.schnittstellen.push(Schnittstelle {
                        nummer: stueck[2],
                        alternativ: stueck[3],
                        // stueck[4] = bNumEndpoints — eine BEHAUPTUNG,
                        // die wir bewusst NICHT benutzen. Gezaehlt wird,
                        // was wirklich kommt; ein Geraet, das 200
                        // Endpunkte behauptet und zwei liefert, bekommt
                        // zwei.
                        klasse: stueck[5],
                        unterklasse: stueck[6],
                        protokoll: stueck[7],
                        index_name: stueck[8],
                        endpunkte: Vec::new(),
                    });
                }
            }
            TYP_ENDPOINT if laenge >= 7 => {
                let endpunkt = Endpunkt {
                    adresse: stueck[2],
                    art: match stueck[3] & 0x03 {
                        0 => Uebertragung::Control,
                        1 => Uebertragung::Isochron,
                        2 => Uebertragung::Bulk,
                        _ => Uebertragung::Interrupt,
                    },
                    // Die oberen Bits von wMaxPacketSize tragen bei
                    // High-Speed die Zahl der Zusatz-Transaktionen —
                    // fuer die Paketgroesse zaehlen nur Bits 0..10.
                    max_paket: u16le(stueck, 4) & 0x07FF,
                    intervall: stueck[6],
                };
                // Ein Endpunkt OHNE vorangehendes Interface ist kaputtes
                // Deskriptor-Layout. Er wird verworfen statt geraten —
                // ihn dem naechsten Interface zuzuschlagen waere eine
                // Erfindung.
                match konfiguration.schnittstellen.last_mut() {
                    Some(schnittstelle) => {
                        if schnittstelle.endpunkte.len() >= MAX_ENDPUNKTE {
                            konfiguration.befund.endpunkte_gekuerzt += 1;
                        } else {
                            schnittstelle.endpunkte.push(endpunkt);
                        }
                    }
                    None => konfiguration.befund.uebersprungen += 1,
                }
            }
            _ => {
                // HID-Deskriptoren und anderes Klassenspezifisches.
                // Uebersprungen, nicht abgelehnt — sie stehen bei jeder
                // Tastatur mitten in der Kette.
                konfiguration.befund.uebersprungen += 1;
            }
        }
        pos += laenge;
    }

    Ok(konfiguration)
}

// ===========================================================================
// STRING-DESKRIPTOREN
// ===========================================================================

/// Einen String-Deskriptor in einen Rust-String verwandeln.
///
/// ===================================================================
/// UTF-16LE — UND UNSERE UMLAUT-REGEL GILT AUCH HIER
///
/// USB-Strings sind UTF-16 Little Endian, nicht ASCII. Ein Hersteller
/// heisst „Müller GmbH", und wer byteweise liest, bekommt „M\0ü..."
/// oder Muell.
///
/// Gelesen wird deshalb mit `char::decode_utf16` — genau wie die
/// VFAT-Langnamen im FAT32-Treiber (Serie 4). Ungueltige Paare werden
/// zu U+FFFD, nicht verworfen: Ein Name mit einem Ersatzzeichen ist
/// lesbar, ein leerer Name ist eine verlorene Information.
///
/// **DIE LAENGE IST WIEDER EINE BEHAUPTUNG.** `bLength` schliesst die
/// zwei Kopfbytes ein; ist es groesser als die Antwort, wird gekuerzt.
/// Eine ungerade Restlaenge (halbes UTF-16-Zeichen) wird abgerundet.
pub fn string_parsen(daten: &[u8]) -> Result<String, DeskriptorFehler> {
    if daten.len() < 2 {
        return Err(DeskriptorFehler::ZuKurz);
    }
    if daten[0] == 0 {
        return Err(DeskriptorFehler::LaengeNull);
    }
    if daten[1] != TYP_STRING {
        return Err(DeskriptorFehler::FalscherTyp);
    }
    // Auf das kuerzere von „behauptet" und „angekommen" klemmen.
    let laenge = (daten[0] as usize).min(daten.len());
    if laenge <= 2 {
        return Ok(String::new()); // leerer, aber gueltiger String
    }
    let nutz = &daten[2..laenge];
    // Ungerade Restlaenge: das letzte halbe Zeichen faellt weg.
    let paare = nutz.len() / 2;
    let mut worte: Vec<u16> = Vec::with_capacity(paare.min(MAX_STRING_ZEICHEN));
    for i in 0..paare.min(MAX_STRING_ZEICHEN) {
        worte.push(u16::from_le_bytes([nutz[i * 2], nutz[i * 2 + 1]]));
    }
    Ok(char::decode_utf16(worte)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

/// Die erste Sprach-ID aus String-Deskriptor 0.
///
/// Deskriptor 0 ist ein Sonderfall: Er traegt keine Zeichen, sondern
/// eine Liste von Sprach-IDs. Ohne eine davon kann man keinen anderen
/// String anfordern. Fehlt er oder ist er kaputt, nehmen wir 0x0409
/// (US-Englisch) — die Sprache, die praktisch jedes Geraet kann.
pub fn erste_sprache(daten: &[u8]) -> u16 {
    if daten.len() >= 4 && daten[1] == TYP_STRING && daten[0] >= 4 {
        u16le(daten, 2)
    } else {
        0x0409
    }
}

// ===========================================================================
// KLASSEN-TEXT (fuer die Anzeige)
// ===========================================================================

pub fn klasse_text(klasse: u8, unterklasse: u8, protokoll: u8) -> &'static str {
    match (klasse, unterklasse, protokoll) {
        (KLASSE_HID, 1, 1) => "HID Tastatur (Boot)",
        (KLASSE_HID, 1, 2) => "HID Maus (Boot)",
        (KLASSE_HID, _, _) => "HID",
        (KLASSE_MASSENSPEICHER, _, 0x50) => "Massenspeicher (Bulk-Only)",
        (KLASSE_MASSENSPEICHER, _, _) => "Massenspeicher",
        (KLASSE_HUB, _, _) => "Hub",
        (0x01, _, _) => "Audio",
        (0x02, _, _) => "Kommunikation",
        (0x06, _, _) => "Bild",
        (0x07, _, _) => "Drucker",
        (0x0E, _, _) => "Video",
        (0xFF, _, _) => "herstellerspezifisch",
        (0x00, _, _) => "(im Interface angegeben)",
        _ => "unbekannt",
    }
}

// ===========================================================================
// KLEINKRAM
// ===========================================================================

/// Zwei Bytes Little Endian. Der Aufrufer hat die Laenge geprueft.
fn u16le(daten: &[u8], versatz: usize) -> u16 {
    u16::from_le_bytes([daten[versatz], daten[versatz + 1]])
}

#[cfg(test)]
mod tests {
    // usb::deskriptor::tests — echte und boesartige Deskriptoren
    //
    // ===========================================================================
    // ZWEI SORTEN, UND DIE ZWEITE IST DIE WICHTIGERE
    //
    //   (1) ECHTE Deskriptoren, Byte fuer Byte von QEMUs `usb-kbd`. Sie
    //       beweisen, dass der Parser das RICHTIGE liest — ein Parser, der
    //       nur kaputte Eingaben uebersteht, koennte auch alles verwerfen.
    //
    //   (2) KAPUTTE und BOESARTIGE. Sie beweisen, dass er nie panickt, nie
    //       ueberlaeuft und nie haengt. Das ist die Zusage, auf die es bei
    //       fremden, physisch steckbaren Geraeten ankommt (BadUSB).

    use super::*;

    /// Der echte Device-Deskriptor von QEMUs `usb-kbd`: 18 Byte, USB 2.0,
    /// Klasse 0 (steht im Interface), 0x0627:0x0001.
    const KBD_DEVICE: [u8; 18] = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x08, 0x27, 0x06, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04,
        0x03, 0x01,
    ];

    /// Die echte Konfigurations-Kette einer USB-Boot-Tastatur:
    /// Config (9) + Interface (9) + HID (9) + Endpoint (7) = 34 Byte.
    const KBD_CONFIG: [u8; 34] = [
        // Configuration: wTotalLength 34, 1 Interface, Wert 1, 50 mA
        0x09, 0x02, 0x22, 0x00, 0x01, 0x01, 0x07, 0xA0, 0x32,
        // Interface 0: Klasse 3 (HID), Sub 1 (Boot), Protokoll 1 (Tastatur)
        0x09, 0x04, 0x00, 0x00, 0x01, 0x03, 0x01, 0x01, 0x00,
        // HID-Deskriptor (Typ 0x21) — muss UEBERSPRUNGEN werden
        0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, 0x3F, 0x00,
        // Endpoint 0x81 (IN, Nr. 1), Interrupt, 8 Byte, Intervall 10
        0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x0A,
    ];

    // ---------------------------------------------------------------------------
    // (1) ECHTE DESKRIPTOREN
    // ---------------------------------------------------------------------------

    #[test_case]
    fn test_echter_geraetedeskriptor() {
        let d = geraet_parsen(&KBD_DEVICE).expect("gueltig");
        assert_eq!(d.usb_version, 0x0200);
        assert_eq!(d.klasse, 0, "Klasse 0 = steht im Interface");
        assert_eq!(d.max_paket0, 8);
        assert_eq!(d.hersteller_id, 0x0627);
        assert_eq!(d.produkt_id, 0x0001);
        assert_eq!(d.index_hersteller, 1);
        assert_eq!(d.index_produkt, 4);
        assert_eq!(d.konfigurationen, 1);
    }

    #[test_case]
    fn test_echte_konfiguration_mit_hid_deskriptor() {
        let k = konfiguration_parsen(&KBD_CONFIG).expect("gueltig");
        assert_eq!(k.wert, 1);
        assert_eq!(k.max_strom, 50);
        assert_eq!(k.schnittstellen.len(), 1);
        let s = &k.schnittstellen[0];
        assert_eq!(s.klasse, KLASSE_HID);
        assert_eq!(s.unterklasse, 1, "Boot-Interface");
        assert_eq!(s.protokoll, 1, "Tastatur");
        assert_eq!(s.endpunkte.len(), 1);
        let e = &s.endpunkte[0];
        assert_eq!(e.art, Uebertragung::Interrupt);
        assert_eq!(e.max_paket, 8);
        assert!(e.ist_eingang(), "eine Tastatur liefert per IN");
        assert_eq!(e.nummer(), 1);
        // Der HID-Deskriptor wurde uebersprungen und GEZAEHLT.
        assert_eq!(k.befund.uebersprungen, 1);
        assert!(!k.befund.abgebrochen);
    }

    /// **DIE DCI-UMRECHNUNG.** Endpunkt 1 IN ist xHCI-Index 3, nicht 1.
    #[test_case]
    fn test_dci_umrechnung() {
        let ein = Endpunkt {
            adresse: 0x81,
            art: Uebertragung::Interrupt,
            max_paket: 8,
            intervall: 10,
        };
        assert_eq!(ein.dci(), 3, "EP1 IN -> DCI 3");
        let aus = Endpunkt {
            adresse: 0x01,
            art: Uebertragung::Bulk,
            max_paket: 64,
            intervall: 0,
        };
        assert_eq!(aus.dci(), 2, "EP1 OUT -> DCI 2");
        let ep2ein = Endpunkt {
            adresse: 0x82,
            art: Uebertragung::Bulk,
            max_paket: 512,
            intervall: 0,
        };
        assert_eq!(ep2ein.dci(), 5, "EP2 IN -> DCI 5");
    }

    #[test_case]
    fn test_usb3_max_paket_ist_ein_exponent() {
        // USB 3: bMaxPacketSize0 = 9 bedeutet 2^9 = 512, nicht 9.
        let mut d = KBD_DEVICE;
        d[2] = 0x00;
        d[3] = 0x03;
        d[7] = 9;
        let g = geraet_parsen(&d).expect("gueltig");
        assert_eq!(g.max_paket0, 512);
    }

    #[test_case]
    fn test_usb3_absurder_exponent_laeuft_nicht_ueber() {
        let mut d = KBD_DEVICE;
        d[2] = 0x00;
        d[3] = 0x03;
        d[7] = 200; // 2^200 gibt es nicht
        let g = geraet_parsen(&d).expect("darf nicht panicken");
        assert!(g.max_paket0 > 0, "brauchbarer Wert statt Absturz");
    }

    // ---------------------------------------------------------------------------
    // (2) KAPUTT UND BOESARTIG
    // ---------------------------------------------------------------------------

    #[test_case]
    fn test_abgeschnittener_geraetedeskriptor() {
        for laenge in 0..DEVICE_DESKRIPTOR_BYTES {
            assert_eq!(
                geraet_parsen(&KBD_DEVICE[..laenge]),
                Err(DeskriptorFehler::ZuKurz),
                "bei {} Byte muss es ZuKurz sein",
                laenge
            );
        }
    }

    #[test_case]
    fn test_laenge_null_ist_kein_haenger() {
        let mut d = KBD_DEVICE;
        d[0] = 0;
        assert_eq!(geraet_parsen(&d), Err(DeskriptorFehler::LaengeNull));
    }

    #[test_case]
    fn test_geraet_laengenfeld_luegt_nach_unten() {
        let mut d = KBD_DEVICE;
        d[0] = 5; // behauptet 5, ein Device-Deskriptor hat 18
        assert_eq!(geraet_parsen(&d), Err(DeskriptorFehler::LaengeLuegt));
    }

    #[test_case]
    fn test_falscher_typ_wird_abgelehnt() {
        let mut d = KBD_DEVICE;
        d[1] = TYP_STRING;
        assert_eq!(geraet_parsen(&d), Err(DeskriptorFehler::FalscherTyp));
    }

    /// **`wTotalLength` luegt nach OBEN.** Es darf nur so weit gelesen
    /// werden, wie wirklich Daten da sind.
    #[test_case]
    fn test_wtotallength_luegt_nach_oben() {
        let mut c = KBD_CONFIG;
        c[2] = 0xFF;
        c[3] = 0xFF; // behauptet 65535 Byte
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert!(k.befund.laenge_gekuerzt, "die Luege muss vermerkt sein");
        assert_eq!(k.schnittstellen.len(), 1, "was da war, wurde geparst");
        assert_eq!(k.schnittstellen[0].endpunkte.len(), 1);
    }

    /// **`bLength` = 0 mitten in der Kette.** Ohne Riegel eine
    /// Endlosschleife — dieser Test muss also ueberhaupt ZURUECKKOMMEN.
    #[test_case]
    fn test_laenge_null_in_der_kette_haengt_nicht() {
        let mut c = KBD_CONFIG;
        c[9] = 0; // das Interface behauptet Laenge 0
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert!(k.befund.abgebrochen, "muss als Abbruch vermerkt sein");
    }

    /// Ein Deskriptor, der ueber das Ende hinausragt, wird nicht halb
    /// gelesen.
    #[test_case]
    fn test_deskriptor_ragt_ueber_das_ende() {
        let mut c = KBD_CONFIG;
        c[9] = 200; // Interface behauptet 200 Byte in 34 Byte Puffer
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert!(k.befund.abgebrochen);
        assert_eq!(k.schnittstellen.len(), 0, "nichts halb Gelesenes");
    }

    /// **Absurde Endpunkt-Zahl.** 200 behauptet, MAX_ENDPUNKTE gefuehrt.
    #[test_case]
    fn test_absurd_viele_endpunkte_werden_gekuerzt() {
        let mut c: Vec<u8> = alloc::vec![
            0x09, 0x02, 0x00, 0x00, 0x01, 0x01, 0x00, 0xA0, 0x32, // Config
            0x09, 0x04, 0x00, 0x00, 0xC8, 0x03, 0x01, 0x01, 0x00, // Interface: 200 EPs
        ];
        for i in 0..200u8 {
            c.extend_from_slice(&[0x07, 0x05, 0x81 | (i & 0x0F), 0x03, 0x08, 0x00, 0x0A]);
        }
        let gesamt = c.len() as u16;
        c[2] = gesamt as u8;
        c[3] = (gesamt >> 8) as u8;
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert_eq!(k.schnittstellen[0].endpunkte.len(), MAX_ENDPUNKTE);
        assert_eq!(k.befund.endpunkte_gekuerzt, 200 - MAX_ENDPUNKTE);
    }

    #[test_case]
    fn test_absurd_viele_interfaces_werden_gekuerzt() {
        let mut c: Vec<u8> = alloc::vec![0x09, 0x02, 0x00, 0x00, 0xFF, 0x01, 0x00, 0xA0, 0x32];
        for i in 0..100u8 {
            c.extend_from_slice(&[0x09, 0x04, i, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00]);
        }
        let gesamt = c.len() as u16;
        c[2] = gesamt as u8;
        c[3] = (gesamt >> 8) as u8;
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert_eq!(k.schnittstellen.len(), MAX_INTERFACES);
        assert!(k.befund.interfaces_gekuerzt > 0);
    }

    /// Ein Endpunkt OHNE Interface davor wird verworfen, nicht geraten.
    #[test_case]
    fn test_endpunkt_ohne_interface_wird_verworfen() {
        let c: [u8; 16] = [
            0x09, 0x02, 0x10, 0x00, 0x00, 0x01, 0x00, 0xA0, 0x32, // Config
            0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0x0A, // Endpoint ohne Interface
        ];
        let k = konfiguration_parsen(&c).expect("darf nicht panicken");
        assert_eq!(k.schnittstellen.len(), 0);
        assert_eq!(k.befund.uebersprungen, 1, "verworfen und gezaehlt");
    }

    /// **MUELL.** Jede Bytefolge ergibt ein Ergebnis — nie eine Panik, nie
    /// einen Haenger.
    #[test_case]
    fn test_muell_panickt_nicht() {
        // Ein reproduzierbarer LCG — eine TESTHILFE, kein Zufall
        // (RNG-Dauerregel I: was Zufall sein soll, heisst `zufall`).
        let mut wert: u32 = 0x1234_5678;
        for laenge in [0usize, 1, 2, 9, 33, 64, 255] {
            let mut muell = Vec::with_capacity(laenge);
            for _ in 0..laenge {
                wert = wert.wrapping_mul(1103515245).wrapping_add(12345);
                muell.push((wert >> 16) as u8);
            }
            let _ = geraet_parsen(&muell);
            let _ = konfiguration_parsen(&muell);
            let _ = string_parsen(&muell);
            let _ = erste_sprache(&muell);
        }
    }

    /// Alle 65536 Kopf-Kombinationen — die erschoepfende Variante.
    #[test_case]
    fn test_alle_kopf_kombinationen() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let daten = [a, b, 0, 0, 0, 0, 0, 0, 0, 0];
                let _ = konfiguration_parsen(&daten);
                let _ = string_parsen(&daten);
                let _ = geraet_parsen(&daten);
            }
        }
    }

    // ---------------------------------------------------------------------------
    // STRINGS
    // ---------------------------------------------------------------------------

    #[test_case]
    fn test_string_utf16() {
        let d: [u8; 10] = [10, 3, b'Q', 0, b'E', 0, b'M', 0, b'U', 0];
        assert_eq!(string_parsen(&d).unwrap(), "QEMU");
    }

    /// **UMLAUTE.** Ein byteweiser Leser macht daraus Muell — dieselbe
    /// Regel wie bei den VFAT-Langnamen (Serie 4).
    #[test_case]
    fn test_string_mit_umlauten() {
        // "Müller" — ü ist U+00FC.
        let d: [u8; 14] = [
            14, 3, b'M', 0, 0xFC, 0x00, b'l', 0, b'l', 0, b'e', 0, b'r', 0,
        ];
        assert_eq!(string_parsen(&d).unwrap(), "Müller");
    }

    #[test_case]
    fn test_string_laenge_luegt_und_ungerade() {
        // bLength behauptet 200, da sind aber 6 Byte.
        let d: [u8; 6] = [200, 3, b'A', 0, b'B', 0];
        assert_eq!(string_parsen(&d).unwrap(), "AB");
        // Ungerade Restlaenge: das halbe Zeichen faellt weg, ohne Panik.
        let u: [u8; 5] = [5, 3, b'A', 0, b'B'];
        assert_eq!(string_parsen(&u).unwrap(), "A");
    }

    #[test_case]
    fn test_string_leer_und_kaputt() {
        assert_eq!(string_parsen(&[2, 3]).unwrap(), "", "gueltig, aber leer");
        assert_eq!(string_parsen(&[0, 3]), Err(DeskriptorFehler::LaengeNull));
        assert_eq!(
            string_parsen(&[4, 1, 0, 0]),
            Err(DeskriptorFehler::FalscherTyp)
        );
        assert_eq!(string_parsen(&[1]), Err(DeskriptorFehler::ZuKurz));
    }

    /// Ein halbes Surrogat-Paar wird zu U+FFFD, nicht verworfen.
    #[test_case]
    fn test_string_kaputtes_surrogat() {
        let d: [u8; 6] = [6, 3, 0x00, 0xD8, b'A', 0];
        let s = string_parsen(&d).unwrap();
        assert!(
            s.contains('\u{FFFD}'),
            "Ersatzzeichen statt Verlust: {:?}",
            s
        );
        assert!(s.contains('A'), "der Rest bleibt lesbar");
    }

    #[test_case]
    fn test_sprache_fallback() {
        assert_eq!(erste_sprache(&[4, 3, 0x07, 0x04]), 0x0407, "Deutsch");
        // Kaputt -> US-Englisch, damit ueberhaupt etwas geht.
        assert_eq!(erste_sprache(&[]), 0x0409);
        assert_eq!(erste_sprache(&[0, 0, 0, 0]), 0x0409);
    }

}
