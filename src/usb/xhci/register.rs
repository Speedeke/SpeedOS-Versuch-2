// usb::xhci::register — die Register-Dekoder und die Ring-Arithmetik
//
// ===========================================================================
// WARUM DIESE DATEI KEINE HARDWARE ANFASST
//
// Bei xHCI ist fast alles, was man falsch machen kann, REINE RECHNEREI:
// ein Feld aus den falschen Bits gelesen, eine Zahl aus zwei Feldern
// nur halb zusammengesetzt, ein Cycle-Bit beim Ringumlauf nicht
// gekippt. Genau diese Fehler sind am schwersten zu finden, wenn sie
// erst in der laufenden Maschine auffallen — der Controller sagt nicht
// „dein HCSPARAMS2-Dekoder ist falsch", er tut einfach nichts.
//
// Deshalb steht hier alles, was ohne Controller entschieden werden
// kann, als reine Funktion — und wird auf dem Host getestet. Der
// Treiber daneben (`mod.rs`) macht dann nur noch Register-Zugriffe und
// Warteschleifen.
//
// Dieselbe Aufteilung wie bei PCI (`bar_dekodieren`), ATA (die
// IDENTIFY-Dekoder) und der Internet-Pruefsumme: Der schwierige Teil
// ist eine Funktion auf Zahlen, der Rest ist Ein- und Ausgabe.

// ===========================================================================
// DIE CAPABILITY-REGISTER
// ===========================================================================

/// Was in `HCSPARAMS1` steht (Offset 0x04 der Capability-Register).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params1 {
    /// Wie viele Geraete-Slots der Controller kann (1..=255).
    pub max_slots: u8,
    /// Wie viele Interrupter es gibt (mindestens 1).
    pub max_interrupter: u16,
    /// Wie viele Wurzel-Ports (= physische Anschluesse).
    pub max_ports: u8,
}

/// `HCSPARAMS1` zerlegen.
///
/// Die Felder liegen so: Slots 0..7, Interrupter 8..18, Ports 24..31.
/// Bits 19..23 sind reserviert — wer die Interrupter-Zahl mit einer
/// 16-Bit-Maske herausschneidet, liest sie versehentlich mit.
pub fn params1_lesen(roh: u32) -> Params1 {
    Params1 {
        max_slots: (roh & 0xFF) as u8,
        max_interrupter: ((roh >> 8) & 0x7FF) as u16,
        max_ports: ((roh >> 24) & 0xFF) as u8,
    }
}

/// Was aus `HCCPARAMS1` gebraucht wird (Offset 0x10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamsCapabilities {
    /// `AC64` — kann der Controller 64-Bit-Adressen?
    pub adressen_64bit: bool,
    /// **`CSZ` — sind die Kontext-Strukturen 64 statt 32 Byte gross?**
    pub kontext_64byte: bool,
    /// `xECP` — Offset der Extended Capabilities, **in 32-Bit-WORTEN**
    /// ab dem Anfang des BAR. 0 = es gibt keine.
    pub xecp_worte: u16,
}

/// `HCCPARAMS1` zerlegen.
///
/// ===================================================================
/// DAS `CSZ`-BIT IST DIE WICHTIGSTE EINZELNE INFORMATION HIER
///
/// Ist es gesetzt, sind ALLE Kontext-Strukturen 64 Byte gross statt 32.
/// Rechnet man mit 32, wo 64 gilt, zeigt jeder Kontext-Zeiger ab dem
/// zweiten Eintrag auf die falsche Stelle — und das faellt erst auf,
/// wenn das erste Geraet angeschlossen wird, also weit entfernt von
/// seiner Ursache.
///
/// Es wird deshalb schon in diesem Schritt gelesen und protokolliert,
/// obwohl noch kein Geraetekontext angelegt wird.
///
/// `xECP` STEHT IN WORTEN, NICHT IN BYTES. Wer den Wert als
/// Byte-Offset benutzt, landet bei einem Viertel der richtigen Adresse
/// — meistens mitten in den Operational-Registern, was beim Lesen
/// nicht auffaellt und beim Schreiben den Controller zerlegt.
pub fn capabilities_lesen(roh: u32) -> ParamsCapabilities {
    ParamsCapabilities {
        adressen_64bit: (roh & 1) != 0,
        kontext_64byte: (roh & (1 << 2)) != 0,
        xecp_worte: ((roh >> 16) & 0xFFFF) as u16,
    }
}

/// Wie viele Scratchpad-Puffer der Controller verlangt (`HCSPARAMS2`).
///
/// ===================================================================
/// DIE ZAHL STEHT IN ZWEI FELDERN, UND DAS IST DIE FALLE
///
/// `Max Scratchpad Buffers` ist auf zwei Bitgruppen verteilt:
/// die oberen fuenf Bits liegen bei 21..25, die unteren fuenf bei
/// 27..31. Wer nur das untere Feld liest, bekommt bei jedem Controller
/// mit mehr als 31 Puffern zu wenige — und legt dann zu wenig Speicher
/// an, was der Controller nicht meldet, sondern womit er einfach
/// abstuerzt.
///
/// QEMU verlangt 0, echte Controller oft 4 bis 32. Der Fehler waere
/// also im Testaufbau unsichtbar.
pub fn scratchpad_anzahl(hcsparams2: u32) -> u32 {
    let hoch = (hcsparams2 >> 21) & 0x1F;
    let niedrig = (hcsparams2 >> 27) & 0x1F;
    (hoch << 5) | niedrig
}

// ===========================================================================
// DIE PORT-REGISTER
// ===========================================================================

/// Die USB-Geschwindigkeit, wie `PORTSC` sie meldet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tempo {
    Unbekannt,
    /// USB 1.1, 1,5 Mbit/s
    Niedrig,
    /// USB 1.1, 12 Mbit/s
    Voll,
    /// USB 2.0, 480 Mbit/s
    Hoch,
    /// USB 3.0, 5 Gbit/s
    Super,
    /// USB 3.1, 10 Gbit/s
    SuperPlus,
}

impl Tempo {
    pub fn text(self) -> &'static str {
        match self {
            Tempo::Unbekannt => "?",
            Tempo::Niedrig => "low (1,5 Mbit/s)",
            Tempo::Voll => "full (12 Mbit/s)",
            Tempo::Hoch => "high (480 Mbit/s)",
            Tempo::Super => "super (5 Gbit/s)",
            Tempo::SuperPlus => "super+ (10 Gbit/s)",
        }
    }
}

/// Der Zustand eines Wurzel-Ports, aus `PORTSC` gelesen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortZustand {
    /// `CCS` — haengt etwas dran?
    pub angeschlossen: bool,
    /// `PED` — ist der Port aktiviert (nach erfolgreichem Reset)?
    pub aktiviert: bool,
    /// `PR` — laeuft gerade ein Reset?
    pub reset_laeuft: bool,
    pub tempo: Tempo,
    /// `CSC` — hat sich der Anschlusszustand geaendert? **Das ist das
    /// Bit, das ein Ein- oder Ausstecken meldet.**
    pub aenderung_angeschlossen: bool,
    /// `PEC` — hat sich der Aktiviert-Zustand geaendert?
    pub aenderung_aktiviert: bool,
    /// `PRC` — ist ein Reset fertig geworden?
    pub aenderung_reset: bool,
}

impl PortZustand {
    /// Gibt es ueberhaupt eine Aenderung zu quittieren?
    pub fn hat_aenderung(self) -> bool {
        self.aenderung_angeschlossen || self.aenderung_aktiviert || self.aenderung_reset
    }
}

/// `PORTSC` zerlegen.
pub fn portsc_lesen(roh: u32) -> PortZustand {
    PortZustand {
        angeschlossen: (roh & 1) != 0,
        aktiviert: (roh & (1 << 1)) != 0,
        reset_laeuft: (roh & (1 << 4)) != 0,
        tempo: tempo_von((roh >> 10) & 0xF),
        aenderung_angeschlossen: (roh & (1 << 17)) != 0,
        aenderung_aktiviert: (roh & (1 << 18)) != 0,
        aenderung_reset: (roh & (1 << 21)) != 0,
    }
}

fn tempo_von(feld: u32) -> Tempo {
    match feld {
        1 => Tempo::Voll,
        2 => Tempo::Niedrig,
        3 => Tempo::Hoch,
        4 => Tempo::Super,
        5 => Tempo::SuperPlus,
        _ => Tempo::Unbekannt,
    }
}

/// Die Bits in `PORTSC`, die man beim Schreiben NICHT versehentlich
/// setzen darf.
///
/// ===================================================================
/// PORTSC IST EIN REGISTER MIT „WRITE-1-TO-CLEAR"-BITS
///
/// Die Aenderungs-Bits (CSC, PEC, PRC …) werden geloescht, indem man
/// eine EINS hineinschreibt. Wer also `PORTSC` liest, ein Bit setzt und
/// alles zurueckschreibt, loescht dabei versehentlich JEDE anstehende
/// Aenderungsmeldung — die Steckvorgaenge, auf die man wartet,
/// verschwinden spurlos.
///
/// Schlimmer noch: `PED` (Bit 1) ist ebenfalls write-1-to-clear und
/// DEAKTIVIERT den Port, wenn man es zurueckschreibt.
///
/// Deshalb gibt es diese Maske. Sie wird beim Zurueckschreiben
/// ABGEZOGEN, und nur die Bits, die man wirklich quittieren will,
/// kommen wieder dazu.
pub const PORTSC_NICHT_ANFASSEN: u32 = (1 << 1)      // PED — wuerde den Port abschalten
    | (1 << 17)  // CSC
    | (1 << 18)  // PEC
    | (1 << 19)  // WRC
    | (1 << 20)  // OCC
    | (1 << 21)  // PRC
    | (1 << 22)  // PLC
    | (1 << 23); // CEC

/// Einen `PORTSC`-Wert so vorbereiten, dass das Zurueckschreiben nur
/// die AUSDRUECKLICH genannten Aenderungs-Bits quittiert.
pub fn portsc_quittierung(roh: u32, zu_quittieren: u32) -> u32 {
    (roh & !PORTSC_NICHT_ANFASSEN) | (zu_quittieren & PORTSC_NICHT_ANFASSEN)
}

// ===========================================================================
// DIE RING-ARITHMETIK
// ===========================================================================

/// Wo ein Ring gerade steht: Index und erwarteter Cycle-Zustand.
///
/// ===================================================================
/// DAS CYCLE-BIT IST DER GANZE TRICK
///
/// Ein TRB-Ring hat KEINEN Zaehler „wie viele sind drin". Produzent und
/// Konsument laufen beide im Kreis, und woran der Konsument erkennt, ob
/// ein Eintrag neu ist, ist ein einzelnes Bit:
///
///   * Jeder Ring hat einen aktuellen Cycle-Zustand, Startwert `true`.
///   * Der Produzent schreibt jedes TRB mit dem aktuellen Zustand.
///   * Der Konsument liest ein TRB als „fuer mich", wenn dessen
///     Cycle-Bit seinem eigenen Zustand ENTSPRICHT.
///   * **Beim Umlauf kippt der Zustand.**
///
/// Ohne das Kippen liefe der Konsument beim zweiten Umlauf ueber TRBs,
/// die er schon gelesen hat, und hielte sie fuer neu — der Event Ring
/// lieferte dieselben Ereignisse endlos.
///
/// Das ist der Kern des Treibers, und er haengt an keiner Hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingStand {
    /// Der naechste Eintrag, der gelesen/geschrieben wird.
    pub index: u32,
    /// Der Cycle-Zustand, der gerade als „gueltig" gilt.
    pub cycle: bool,
    /// Wie viele Eintraege der Ring hat.
    pub groesse: u32,
}

impl RingStand {
    /// Ein frischer Ring: Anfang, Cycle-Zustand 1.
    ///
    /// Startwert `true` ist nicht beliebig — der Controller erwartet
    /// laut Spezifikation, dass frisch angelegter Speicher (also
    /// Nullen, Cycle-Bit 0) als „noch nicht beschrieben" gilt. Ein
    /// Konsument mit Startzustand `true` sieht eine genullte Seite
    /// damit korrekt als LEER.
    pub const fn neu(groesse: u32) -> RingStand {
        RingStand {
            index: 0,
            cycle: true,
            groesse,
        }
    }

    /// Einen Schritt weiter — **mit Kippen beim Umlauf**.
    pub fn weiter(&mut self) {
        self.index += 1;
        if self.index >= self.groesse {
            self.index = 0;
            self.cycle = !self.cycle;
        }
    }

    /// Gehoert ein TRB mit diesem Cycle-Bit uns?
    ///
    /// Fuer den Event Ring heisst „nein": Der Ring ist LEER. Das ist
    /// kein Fehlerfall, sondern der Normalzustand.
    pub fn gehoert_uns(&self, trb_cycle: bool) -> bool {
        trb_cycle == self.cycle
    }

    /// Der Byte-Versatz des aktuellen Eintrags (ein TRB = 16 Byte).
    pub fn versatz(&self) -> u64 {
        self.index as u64 * TRB_BYTES as u64
    }
}

/// Ein TRB ist immer 16 Byte gross.
pub const TRB_BYTES: usize = 16;

/// Der TRB-Typ steht in den Bits 10..15 des vierten Wortes.
pub fn trb_typ(wort3: u32) -> u8 {
    ((wort3 >> 10) & 0x3F) as u8
}

/// Das Cycle-Bit ist Bit 0 des vierten Wortes.
pub fn trb_cycle(wort3: u32) -> bool {
    (wort3 & 1) != 0
}

/// Die TRB-Typen, die dieser Schritt kennen muss.
pub const TRB_TYP_PORT_STATUS_CHANGE: u8 = 34;
pub const TRB_TYP_COMMAND_COMPLETION: u8 = 33;
pub const TRB_TYP_TRANSFER_EVENT: u8 = 32;

/// Bei einem Port-Status-Change-Event steht die Port-Nummer im ersten
/// Wort, Bits 24..31. **Sie ist EINSBASIERT** — Port 1 ist der erste.
pub fn port_aus_event(wort0: u32) -> u8 {
    ((wort0 >> 24) & 0xFF) as u8
}

/// Ein menschlicher Name fuer einen TRB-Typ (nur fuer das Protokoll).
pub fn trb_typ_text(typ: u8) -> &'static str {
    match typ {
        32 => "Transfer Event",
        33 => "Command Completion",
        34 => "Port Status Change",
        35 => "Bandwidth Request",
        36 => "Doorbell",
        37 => "Host Controller Event",
        38 => "Device Notification",
        39 => "MFINDEX Wrap",
        _ => "unbekannt",
    }
}
