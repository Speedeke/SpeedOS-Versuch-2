// usb::xhci — der USB-3-Hostcontroller, bis er laeuft
//
// ===========================================================================
// WAS DIESER TREIBER TUT UND WO ER AUFHOERT
//
// Er findet den Controller, mappt seine Register UNGECACHT, holt ihn
// notfalls der Firmware ab (BIOS-Handoff), setzt ihn zurueck, legt die
// vier Datenstrukturen an (DCBAA, Scratchpad, Command Ring, Event Ring
// mit ERST) und laesst ihn laufen. Danach liest er den Event Ring aus
// und protokolliert Port-Status-Aenderungen.
//
// **HIER IST BEWUSST SCHLUSS.** Kein Slot wird aktiviert, keine Adresse
// vergeben, kein Deskriptor gelesen, keine Uebertragung gemacht. Der
// vollstaendige Zuschnitt mit Begruendung steht in docs/xhci.md; das
// Ziel dieses Schrittes ist: Der Controller laeuft, und ein Ein- oder
// Ausstecken erzeugt ein Event, das ankommt.
//
// ===========================================================================
// WARUM DAS PROTOKOLL HIER NICHT SPARSAM IST
//
// Bei xHCI gibt es keinen Zwischenzustand, den man sich ansehen kann:
// Entweder der Controller laeuft, oder er tut nichts — und beide Faelle
// sehen von aussen gleich aus. Auf fremder Hardware ist die letzte
// gedruckte Zeile die EINZIGE Information darueber, wo es
// stehengeblieben ist.
//
// Deshalb meldet jeder Schritt seinen Namen, die gelesenen Rohwerte und
// sein Ergebnis — auch im Erfolgsfall. Ein Protokoll, das nur bei
// Fehlern spricht, hilft genau dann nicht, wenn man es braucht.
//
// ===========================================================================
// KEINE SCHLEIFE OHNE FRIST
//
// Jeder Warteschritt laeuft ueber `warten_auf`. Auf echter Hardware ist
// „haengt beim Booten" der teuerste aller Fehler, weil er keine Meldung
// hinterlaesst — dieselbe Regel wie im ATA-Treiber.

pub mod register;

use crate::{pci, serial_println, zeit};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use register::*;
use spin::Mutex;
use x86_64::structures::paging::{Page, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

// ---------------------------------------------------------------------------
// KONSTANTEN
// ---------------------------------------------------------------------------

/// PCI-Klassenkennung eines xHCI-Controllers.
const KLASSE_SERIELL: u8 = 0x0C;
const UNTERKLASSE_USB: u8 = 0x03;
const PROGIF_XHCI: u8 = 0x30;

/// Wie viel MMIO wir mappen. Grosszuegig statt genau — der Bereich ist
/// klein, und eine zu knappe Rechnung ist ein Page Fault beim ersten
/// Doorbell (docs/xhci.md §3, Schritt 2).
const MMIO_BYTES: u64 = 64 * 1024;

/// Eintraege im Command Ring bzw. Event Ring. 64 ist reichlich fuer
/// diesen Schritt (wir setzen keine Kommandos ab) und haelt beide Ringe
/// mit 1 KiB weit unter der 64-KiB-Grenze, die ein Ring nicht
/// ueberschreiten darf.
const RING_EINTRAEGE: u32 = 64;

/// Fristen. Die Werte stammen aus der Spezifikation (§4.2) bzw. sind
/// grosszuegig darueber gewaehlt — sie sollen einen HAENGER verhindern,
/// nicht knapp sein.
const FRIST_HALT_US: u64 = 50_000;
const FRIST_RESET_US: u64 = 1_000_000;
const FRIST_HANDOFF_US: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// FEHLER
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciFehler {
    /// Kein Controller mit der Klassenkennung 0x0C/0x03/0x30 gefunden.
    NichtGefunden,
    /// BAR0 ist kein Speicher-BAR (oder leer) — dann ist es kein xHCI.
    KeinMmioBar,
    /// Die MMIO-Seiten liessen sich nicht mappen.
    MappingFehlgeschlagen,
    /// Der Controller wurde nicht rechtzeitig „halted".
    ZeitueberschreitungHalt,
    /// Der Reset wurde nicht rechtzeitig fertig (oder CNR blieb stehen).
    ZeitueberschreitungReset,
    /// Der Controller lief nach `RS` nicht an (`HCH` blieb 1).
    LaeuftNicht,
    /// Speicher fuer die Ringe/Tabellen liess sich nicht anlegen.
    KeinSpeicher,
}

impl XhciFehler {
    pub fn text(self) -> &'static str {
        match self {
            XhciFehler::NichtGefunden => "kein xHCI-Controller gefunden",
            XhciFehler::KeinMmioBar => "BAR0 ist kein Speicher-BAR",
            XhciFehler::MappingFehlgeschlagen => "MMIO liess sich nicht mappen",
            XhciFehler::ZeitueberschreitungHalt => "Controller wurde nicht halted",
            XhciFehler::ZeitueberschreitungReset => "Reset nicht fertig geworden",
            XhciFehler::LaeuftNicht => "Controller laeuft nach RS nicht",
            XhciFehler::KeinSpeicher => "kein Speicher fuer Ringe/Tabellen",
        }
    }
}

// ---------------------------------------------------------------------------
// REGISTER-ZUGRIFF
// ---------------------------------------------------------------------------

/// Ein gemappter MMIO-Bereich.
///
/// JEDER Zugriff laeuft ueber `read_volatile`/`write_volatile`. Das
/// ungecachte Mapping (`memory::map_mmio`) schuetzt vor dem CACHE,
/// `volatile` vor dem OPTIMIERER — das sind zwei verschiedene Gegner,
/// und man braucht beide (docs/xhci.md §2.1).
#[derive(Debug, Clone, Copy)]
struct Mmio {
    basis: VirtAddr,
}

impl Mmio {
    /// # Safety
    /// `basis + versatz` muss im gemappten MMIO-Bereich liegen.
    unsafe fn lese32(&self, versatz: u64) -> u32 {
        let zeiger = (self.basis.as_u64() + versatz) as *const u32;
        core::ptr::read_volatile(zeiger)
    }
    /// # Safety
    /// wie `lese32`.
    unsafe fn schreibe32(&self, versatz: u64, wert: u32) {
        let zeiger = (self.basis.as_u64() + versatz) as *mut u32;
        core::ptr::write_volatile(zeiger, wert);
    }
    /// Ein 64-Bit-Register — **als zwei 32-Bit-Zugriffe, unteres Wort
    /// zuerst**.
    ///
    /// Manche Controller (und manche Chipsaetze) vertragen keinen
    /// 64-Bit-Zugriff auf ihre Register. Die Spezifikation erlaubt
    /// beides; zwei 32-Bit-Zugriffe gehen IMMER, ein 64-Bit-Zugriff
    /// nicht. Die Reihenfolge (unten vor oben) ist vorgeschrieben:
    /// Beim Schreiben von `CRCR` und `ERDP` loest das obere Wort die
    /// Wirkung aus.
    /// # Safety
    /// wie `lese32`.
    unsafe fn schreibe64(&self, versatz: u64, wert: u64) {
        self.schreibe32(versatz, wert as u32);
        self.schreibe32(versatz + 4, (wert >> 32) as u32);
    }
}

// Capability-Register (ab BAR0 + 0)
const CAP_CAPLENGTH: u64 = 0x00; // Byte 0, HCIVERSION in Byte 2..3
const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;

// Operational-Register (ab BAR0 + CAPLENGTH)
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_PAGESIZE: u64 = 0x08;
const OP_DNCTRL: u64 = 0x14;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC_BASIS: u64 = 0x400;
const OP_PORT_ABSTAND: u64 = 0x10;

// USBCMD-Bits
const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;

// USBSTS-Bits
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;

// Runtime: Interrupter 0 liegt bei RTSOFF + 0x20
const RT_IR0: u64 = 0x20;
const IR_IMAN: u64 = 0x00;
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10;
const IR_ERDP: u64 = 0x18;

// ---------------------------------------------------------------------------
// DER CONTROLLER
// ---------------------------------------------------------------------------

/// Alles, was der Treiber ueber den laufenden Controller weiss.
pub struct Controller {
    cap: Mmio,
    op: Mmio,
    laufzeit: Mmio,
    #[allow(dead_code)]
    doorbell: Mmio,

    pub max_slots: u8,
    pub max_ports: u8,
    pub max_interrupter: u16,
    pub kontext_64byte: bool,
    pub adressen_64bit: bool,
    pub hci_version: u16,
    pub scratchpad_puffer: u32,

    /// Wo wir im Event Ring stehen (Index + Cycle-Zustand).
    event_stand: RingStand,
    event_virt: VirtAddr,
    event_phys: PhysAddr,

    /// Die zuletzt gesehenen Port-Zustaende — fuer `usb` und um
    /// Aenderungen zu erkennen.
    pub ports: Vec<PortZustand>,

    /// Damit der DMA-Speicher am Leben bleibt, solange der Controller
    /// laeuft. Wird nie gelesen; der Besitz ist der Zweck.
    #[allow(dead_code)]
    speicher: Vec<VirtAddr>,
}

/// Der eine Controller. Mehrere xHCI in einem Rechner gibt es, aber
/// dieser Schritt nimmt den ersten — mehr waere eine Liste ohne Nutzen,
/// solange kein Geraet bedient wird.
static CONTROLLER: Mutex<Option<Controller>> = Mutex::new(None);
static VORHANDEN: AtomicBool = AtomicBool::new(false);

/// Laeuft ein xHCI-Controller?
pub fn vorhanden() -> bool {
    VORHANDEN.load(Ordering::Relaxed)
}

/// Zugriff auf den Controller (fuer den `usb`-Befehl und den Task).
pub fn mit_controller<R>(f: impl FnOnce(Option<&mut Controller>) -> R) -> R {
    let mut gesperrt = CONTROLLER.lock();
    f(gesperrt.as_mut())
}

// ---------------------------------------------------------------------------
// WARTEN
// ---------------------------------------------------------------------------

/// Auf eine Bedingung warten — **niemals endlos**.
///
/// Liefert `true`, wenn die Bedingung eintrat, `false` bei Fristablauf.
/// Zwischen den Versuchen wird `spin_loop` gerufen; ein `hlt` waere
/// hier falsch, weil `init` frueh laeuft und wir keine Interrupts
/// erwarten (docs/xhci.md §7: gepollt, nicht per IRQ).
fn warten_auf(frist_us: u64, mut bedingung: impl FnMut() -> bool) -> bool {
    let start = zeit::us_seit_boot();
    loop {
        if bedingung() {
            return true;
        }
        if zeit::us_seit_boot().saturating_sub(start) > frist_us {
            return false;
        }
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// INITIALISIERUNG
// ---------------------------------------------------------------------------

/// Den xHCI-Controller suchen und hochfahren.
///
/// Wird aus `main.rs` gerufen, NACH `pci::init` (der Controller muss
/// enumeriert sein) und NACH der Heap-Erweiterung (die Ringe
/// allozieren).
pub fn init() {
    match starten() {
        Ok(()) => {
            VORHANDEN.store(true, Ordering::Relaxed);
            serial_println!("[xhci] Controller laeuft.");
        }
        Err(XhciFehler::NichtGefunden) => {
            // Kein Fehler, sondern der Normalfall auf einer Maschine
            // ohne xHCI. Nur eine Zeile, keine Aufregung.
            serial_println!("[xhci] kein xHCI-Controller vorhanden.");
        }
        Err(fehler) => {
            serial_println!("[xhci] FEHLGESCHLAGEN: {}", fehler.text());
        }
    }
}

fn starten() -> Result<(), XhciFehler> {
    // --- Schritt 1: finden -------------------------------------------
    let geraet = pci::finde_klasse(KLASSE_SERIELL, UNTERKLASSE_USB, PROGIF_XHCI)
        .ok_or(XhciFehler::NichtGefunden)?;
    serial_println!(
        "[xhci] gefunden: {:02x}:{:02x}.{} {:04x}:{:04x}",
        geraet.bus,
        geraet.geraet,
        geraet.funktion,
        geraet.vendor_id,
        geraet.device_id
    );

    let pci::Bar::Speicher { basis, bit64 } = geraet.bars[0] else {
        return Err(XhciFehler::KeinMmioBar);
    };
    serial_println!(
        "[xhci] BAR0 = 0x{:016x} ({}-Bit)",
        basis,
        if bit64 { 64 } else { 32 }
    );

    // MEMORY SPACE (Bit 1) + BUS MASTER (Bit 2). Ohne Bus Master laeuft
    // der Controller scheinbar an und greift NIE auf unsere Ringe zu —
    // Fallgrube 5 in docs/xhci.md.
    geraet.command_setzen((1 << 1) | (1 << 2));
    serial_println!("[xhci] PCI: Memory Space + Bus Master gesetzt.");

    // --- Schritt 2: MMIO mappen --------------------------------------
    let cap_basis = mmio_mappen(basis, MMIO_BYTES).ok_or(XhciFehler::MappingFehlgeschlagen)?;
    let cap = Mmio { basis: cap_basis };
    serial_println!(
        "[xhci] MMIO ungecacht gemappt: 0x{:016x} -> {:?} ({} KiB)",
        basis,
        cap_basis,
        MMIO_BYTES / 1024
    );

    // --- Schritt 5 (vorgezogen): Capability-Register -----------------
    // Sie muessen VOR allem anderen gelesen werden, weil sie sagen, WO
    // die anderen Registerbloecke liegen.
    // SAFETY: `cap_basis` ist gerade gemappt worden, die Versaetze
    // liegen alle in den ersten 32 Byte.
    let (caplength, hci_version, p1, params2, hcc, dboff, rtsoff) = unsafe {
        let wort0 = cap.lese32(CAP_CAPLENGTH);
        let caplength = (wort0 & 0xFF) as u64;
        let hci_version = (wort0 >> 16) as u16;
        let p1 = params1_lesen(cap.lese32(CAP_HCSPARAMS1));
        let params2 = cap.lese32(CAP_HCSPARAMS2);
        let hcc = capabilities_lesen(cap.lese32(CAP_HCCPARAMS1));
        let dboff = (cap.lese32(CAP_DBOFF) & !0x3) as u64;
        let rtsoff = (cap.lese32(CAP_RTSOFF) & !0x1F) as u64;
        (caplength, hci_version, p1, params2, hcc, dboff, rtsoff)
    };
    let scratchpad = scratchpad_anzahl(params2);

    serial_println!(
        "[xhci] HCIVERSION {:x}.{:02x}, CAPLENGTH {}, RTSOFF 0x{:x}, DBOFF 0x{:x}",
        hci_version >> 8,
        hci_version & 0xFF,
        caplength,
        rtsoff,
        dboff
    );
    serial_println!(
        "[xhci] Slots {}, Ports {}, Interrupter {}",
        p1.max_slots,
        p1.max_ports,
        p1.max_interrupter
    );
    // DAS CSZ-BIT WIRD PROTOKOLLIERT, obwohl dieser Schritt noch keinen
    // Geraetekontext anlegt — es ist die Information, deren Fehlen sich
    // erst beim ersten Geraet raecht (Fallgrube 2).
    serial_println!(
        "[xhci] Kontextgroesse {} Byte (CSZ={}), 64-Bit-Adressen: {}, Scratchpad: {}",
        if hcc.kontext_64byte { 64 } else { 32 },
        hcc.kontext_64byte as u8,
        hcc.adressen_64bit,
        scratchpad
    );

    let op = Mmio {
        basis: cap_basis + caplength,
    };
    let laufzeit = Mmio {
        basis: cap_basis + rtsoff,
    };
    let doorbell = Mmio {
        basis: cap_basis + dboff,
    };

    // --- Schritt 3: BIOS-Handoff -------------------------------------
    bios_handoff(&cap, hcc.xecp_worte);

    // --- Schritt 4: anhalten und zuruecksetzen -----------------------
    // SAFETY: `op` liegt im gemappten Bereich (caplength < MMIO_BYTES).
    unsafe {
        let cmd = op.lese32(OP_USBCMD);
        op.schreibe32(OP_USBCMD, cmd & !USBCMD_RS);
    }
    if !warten_auf(FRIST_HALT_US, || unsafe {
        op.lese32(OP_USBSTS) & USBSTS_HCH != 0
    }) {
        serial_println!("[xhci] FEHLER: HCH kam nicht — Controller haelt nicht an.");
        return Err(XhciFehler::ZeitueberschreitungHalt);
    }
    serial_println!("[xhci] angehalten (HCH gesetzt).");

    unsafe {
        op.schreibe32(OP_USBCMD, USBCMD_HCRST);
    }
    // ZWEI BEDINGUNGEN, und die zweite ist die, die man vergisst:
    // HCRST muss zurueckfallen UND CNR muss 0 sein. Schreibt man
    // waehrend CNR=1 in ein Register, wird der Zugriff VERWORFEN — ohne
    // Fehler (Fallgrube 3).
    if !warten_auf(FRIST_RESET_US, || unsafe {
        op.lese32(OP_USBCMD) & USBCMD_HCRST == 0 && op.lese32(OP_USBSTS) & USBSTS_CNR == 0
    }) {
        serial_println!("[xhci] FEHLER: Reset nicht fertig (HCRST oder CNR haengt).");
        return Err(XhciFehler::ZeitueberschreitungReset);
    }
    serial_println!("[xhci] zurueckgesetzt (HCRST zurueck, CNR frei).");

    // Nach dem Reset ist PAGESIZE lesbar. Wir unterstuetzen nur 4 KiB —
    // alles andere kommt in der Praxis nicht vor, aber es wird
    // GEMELDET statt stillschweigend angenommen.
    // SAFETY: siehe oben.
    let pagesize = unsafe { op.lese32(OP_PAGESIZE) };
    serial_println!(
        "[xhci] PAGESIZE-Register 0x{:04x} (Bit 0 = 4 KiB){}",
        pagesize,
        if pagesize & 1 == 0 {
            " — ACHTUNG: 4 KiB nicht angeboten!"
        } else {
            ""
        }
    );

    // --- Schritte 6-9: die Datenstrukturen ---------------------------
    let mut speicher: Vec<VirtAddr> = Vec::new();

    // (6) DCBAA — (max_slots + 1) Zeiger, 64-Byte-ausgerichtet. Eine
    // ganze Seite ist mehr als genug (512 Zeiger) und automatisch
    // ausgerichtet.
    let dcbaa_virt = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
    let dcbaa_phys = phys_von(dcbaa_virt).ok_or(XhciFehler::KeinSpeicher)?;
    speicher.push(dcbaa_virt);

    // (7) Scratchpad — echte Seiten fuer den Controller selbst.
    // QEMU verlangt 0; echte Controller oft 4..32. Wir bauen es
    // trotzdem, weil der Fehler sonst erst auf der Hardware auffiele,
    // auf der man ihn am schlechtesten sucht (Fallgrube 10).
    if scratchpad > 0 {
        let tabelle_virt = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
        let tabelle_phys = phys_von(tabelle_virt).ok_or(XhciFehler::KeinSpeicher)?;
        speicher.push(tabelle_virt);
        for i in 0..scratchpad {
            let puffer = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
            let puffer_phys = phys_von(puffer).ok_or(XhciFehler::KeinSpeicher)?;
            speicher.push(puffer);
            // SAFETY: `tabelle_virt` ist eine frisch allozierte, genullte
            // Seite; `i` < scratchpad <= 1023, also < 512 Eintraege je
            // Seite … bei mehr als 512 Puffern braeuchte es eine zweite
            // Seite. Das wird geprueft.
            if i >= 512 {
                serial_println!("[xhci] WARNUNG: mehr als 512 Scratchpad-Puffer — gekuerzt.");
                break;
            }
            unsafe {
                let eintrag = (tabelle_virt.as_u64() as *mut u64).add(i as usize);
                core::ptr::write_volatile(eintrag, puffer_phys.as_u64());
            }
        }
        // DCBAA[0] zeigt auf die Scratchpad-Tabelle — Eintrag 0 ist
        // dafuer reserviert und NICHT fuer Slot 0.
        // SAFETY: dcbaa_virt ist eine gemappte, genullte Seite.
        unsafe {
            core::ptr::write_volatile(dcbaa_virt.as_u64() as *mut u64, tabelle_phys.as_u64());
        }
        serial_println!(
            "[xhci] Scratchpad: {} Puffer, Tabelle bei 0x{:016x}.",
            scratchpad,
            tabelle_phys.as_u64()
        );
    }

    // SAFETY: op liegt im gemappten Bereich.
    unsafe {
        op.schreibe64(OP_DCBAAP, dcbaa_phys.as_u64());
    }
    serial_println!("[xhci] DCBAA bei 0x{:016x}.", dcbaa_phys.as_u64());

    // (8) Command Ring. In diesem Schritt wird er nur ANGELEGT — es
    // wird kein Kommando abgesetzt.
    let cmd_virt = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
    let cmd_phys = phys_von(cmd_virt).ok_or(XhciFehler::KeinSpeicher)?;
    speicher.push(cmd_virt);
    // Das RCS-Bit (Bit 0) muss zum Cycle-Zustand unseres Rings passen:
    // frischer Ring = 1.
    unsafe {
        op.schreibe64(OP_CRCR, cmd_phys.as_u64() | 1);
    }
    serial_println!(
        "[xhci] Command Ring bei 0x{:016x} ({} Eintraege, RCS=1).",
        cmd_phys.as_u64(),
        RING_EINTRAEGE
    );

    // (9) Event Ring + ERST.
    let event_virt = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
    let event_phys = phys_von(event_virt).ok_or(XhciFehler::KeinSpeicher)?;
    speicher.push(event_virt);
    let erst_virt = seiten_holen(1).ok_or(XhciFehler::KeinSpeicher)?;
    let erst_phys = phys_von(erst_virt).ok_or(XhciFehler::KeinSpeicher)?;
    speicher.push(erst_virt);

    // Der EINE ERST-Eintrag: Adresse (64 Bit) + Groesse (16 Bit).
    // SAFETY: frisch allozierte, genullte Seite.
    unsafe {
        let e = erst_virt.as_u64() as *mut u64;
        core::ptr::write_volatile(e, event_phys.as_u64());
        core::ptr::write_volatile(e.add(1), RING_EINTRAEGE as u64);
    }

    // DIE REIHENFOLGE IST VORGESCHRIEBEN: erst ERSTSZ, dann ERDP, und
    // ERSTBA ZULETZT — dieses Schreiben aktiviert den Interrupter
    // (Fallgrube 7).
    let ir0 = RT_IR0;
    // SAFETY: laufzeit liegt bei cap_basis + rtsoff, innerhalb MMIO.
    unsafe {
        // ERSTSZ IST DIE ZAHL DER SEGMENTE, NICHT DER TRBs.
        //
        // Der Fehler, den dieser Treiber wirklich gemacht hat: Hier
        // stand `RING_EINTRAEGE` (64). Damit liest der Controller 64
        // ERST-EINTRAEGE, obwohl nur EINER gueltig ist — die uebrigen
        // 63 sind Nullen, also 63 Segmente der Groesse 0 an Adresse 0.
        //
        // Symptom: Der Controller laeuft an, meldet keinen Fehler, und
        // es kommt schlicht NIE ein Event. Genau die Sorte Fehler, vor
        // der docs/xhci.md warnt — dort steht „ERSTSZ = 1 (ein
        // Segment)", und die Umsetzung wich davon ab.
        //
        // Die GROESSE des Segments (64 TRBs) steht im ERST-Eintrag
        // selbst, nicht hier.
        laufzeit.schreibe32(ir0 + IR_ERSTSZ, 1);
        laufzeit.schreibe64(ir0 + IR_ERDP, event_phys.as_u64());
        laufzeit.schreibe64(ir0 + IR_ERSTBA, erst_phys.as_u64());
    }
    serial_println!(
        "[xhci] Event Ring bei 0x{:016x}, ERST bei 0x{:016x} ({} Eintraege).",
        event_phys.as_u64(),
        erst_phys.as_u64(),
        RING_EINTRAEGE
    );

    // --- Schritt 10: laufen lassen -----------------------------------
    // SAFETY: op im gemappten Bereich.
    unsafe {
        op.schreibe32(OP_CONFIG, p1.max_slots as u32);
        op.schreibe32(OP_DNCTRL, 0);
        let cmd = op.lese32(OP_USBCMD);
        // INTE bleibt AUS: Wir pollen (docs/xhci.md §7). Das Bit wird
        // trotzdem bewusst genannt statt vergessen.
        let _ = USBCMD_INTE;
        op.schreibe32(OP_USBCMD, cmd | USBCMD_RS);
    }
    if !warten_auf(FRIST_HALT_US, || unsafe {
        op.lese32(OP_USBSTS) & USBSTS_HCH == 0
    }) {
        serial_println!("[xhci] FEHLER: HCH blieb gesetzt — Controller laeuft nicht.");
        return Err(XhciFehler::LaeuftNicht);
    }
    serial_println!("[xhci] RS gesetzt, HCH gefallen — der Controller LAEUFT.");

    // Die Ports einmal einlesen — und PROTOKOLLIEREN.
    //
    // Das ist nicht nur fuer `usb` da: Geraete, die beim Start schon
    // stecken, erzeugen KEIN Port-Status-Change-Event (ihr CSC-Bit
    // stand schon, bevor wir liefen). Ohne diese Zeilen saehe ein
    // Protokoll ohne Events genauso aus wie ein kaputter Event Ring —
    // und man suchte den Fehler an der falschen Stelle.
    let mut ports = Vec::new();
    let mut angeschlossen_beim_start = 0usize;
    for i in 0..p1.max_ports {
        // SAFETY: PORTSC-Bereich liegt bei op + 0x400 + i*0x10, mit
        // max_ports <= 255 also unter 0x1000 — innerhalb MMIO_BYTES.
        let roh = unsafe { op.lese32(OP_PORTSC_BASIS + i as u64 * OP_PORT_ABSTAND) };
        let z = portsc_lesen(roh);
        if z.angeschlossen {
            angeschlossen_beim_start += 1;
            serial_println!(
                "[xhci] Port {} beim Start belegt: Tempo {} (PORTSC=0x{:08x})",
                i + 1,
                z.tempo.text(),
                roh
            );
        }
        ports.push(z);
    }
    serial_println!(
        "[xhci] {} von {} Ports beim Start belegt.          (Schon steckende Geraete erzeugen kein Event —          zum Pruefen des Event Rings ein Geraet NEU einstecken.)",
        angeschlossen_beim_start,
        p1.max_ports
    );

    let controller = Controller {
        cap,
        op,
        laufzeit,
        doorbell,
        max_slots: p1.max_slots,
        max_ports: p1.max_ports,
        max_interrupter: p1.max_interrupter,
        kontext_64byte: hcc.kontext_64byte,
        adressen_64bit: hcc.adressen_64bit,
        hci_version,
        scratchpad_puffer: scratchpad,
        event_stand: RingStand::neu(RING_EINTRAEGE),
        event_virt,
        event_phys,
        ports,
        speicher,
    };
    *CONTROLLER.lock() = Some(controller);
    Ok(())
}

// ---------------------------------------------------------------------------
// BIOS-HANDOFF
// ---------------------------------------------------------------------------

/// Den Controller der Firmware abnehmen (USBLEGSUP, Capability-ID 1).
///
/// ===================================================================
/// DIE FALLE, DIE ES NUR AUF ECHTER HARDWARE GIBT
///
/// Die Firmware benutzt USB selbst (Boot-Tastatur) und uebergibt den
/// Controller in einem Zustand, in dem sie ihn noch besitzt. Schreibt
/// man dann in die Register, kaempfen zwei Treiber um dasselbe Geraet:
/// sporadische Resets, verschwindende Ports, ein SMI-Sturm.
///
/// **In QEMU gibt es diese Capability nicht** — der Code laeuft dort
/// also durch, ohne etwas zu tun. Genau deshalb muss er sorgfaeltig
/// sein: Er wird zuerst auf echter Hardware wirksam, wo man ihn nicht
/// schrittweise debuggen kann. Er protokolliert deshalb auch, wenn er
/// NICHTS gefunden hat.
fn bios_handoff(cap: &Mmio, xecp_worte: u16) {
    if xecp_worte == 0 {
        serial_println!("[xhci] keine Extended Capabilities (xECP=0) — kein BIOS-Handoff noetig.");
        return;
    }
    // xECP steht in 32-Bit-WORTEN ab BAR0, nicht in Bytes (Fallgrube:
    // ein Viertel der richtigen Adresse).
    let mut versatz = xecp_worte as u64 * 4;
    // Die Kette ist begrenzt durchlaufen — eine kaputte Firmware koennte
    // einen Ring bauen, und ein Treiber, der beim Booten endlos kreist,
    // ist genau der Fehler, den wir ueberall sonst vermeiden.
    for _ in 0..64 {
        if versatz == 0 || versatz + 4 > MMIO_BYTES {
            break;
        }
        // SAFETY: `versatz` ist gegen MMIO_BYTES geprueft.
        let kopf = unsafe { cap.lese32(versatz) };
        let id = (kopf & 0xFF) as u8;
        let naechster = ((kopf >> 8) & 0xFF) as u64 * 4;

        if id == 1 {
            serial_println!("[xhci] USBLEGSUP bei +0x{:x} gefunden.", versatz);
            // Bit 24 = OS Owned setzen.
            // SAFETY: wie oben.
            unsafe {
                cap.schreibe32(versatz, kopf | (1 << 24));
            }
            let frei = warten_auf(FRIST_HANDOFF_US, || {
                // SAFETY: wie oben.
                unsafe { cap.lese32(versatz) & (1 << 16) == 0 }
            });
            if frei {
                serial_println!("[xhci] BIOS hat losgelassen.");
            } else {
                // MANCHE FIRMWARE SETZT DAS BIT NIE ZURUECK. Aufgeben
                // waere schlechter als es selbst zu loeschen — dann
                // haetten wir einen Controller, den niemand benutzt.
                serial_println!(
                    "[xhci] WARNUNG: BIOS-Semaphore blieb stehen — wird selbst geloescht."
                );
                // SAFETY: wie oben.
                unsafe {
                    let jetzt = cap.lese32(versatz);
                    cap.schreibe32(versatz, jetzt & !(1 << 16));
                }
            }
            // USBLEGCTLSTS liegt direkt dahinter: alle SMI-Freigaben
            // abschalten, sonst loest jeder Portwechsel weiter einen
            // SMI aus (und der laeuft in die Firmware, nicht zu uns).
            // SAFETY: wie oben.
            unsafe {
                let ctl = cap.lese32(versatz + 4);
                // Die SMI-Freigaben liegen in den Bits 0..15; die
                // oberen Bits sind write-1-to-clear-Statusbits, die wir
                // dabei gleich quittieren.
                cap.schreibe32(versatz + 4, (ctl & 0xFFFF_0000) & !0x0000_FFFF);
            }
            serial_println!("[xhci] SMI-Freigaben abgeschaltet.");
            return;
        }

        if naechster == 0 {
            break;
        }
        versatz += naechster;
    }
    serial_println!("[xhci] kein USBLEGSUP in den Extended Capabilities.");
}

// ---------------------------------------------------------------------------
// DER EVENT RING
// ---------------------------------------------------------------------------

impl Controller {
    /// Ein TRB aus dem Event Ring lesen (vier 32-Bit-Worte).
    ///
    /// # Safety
    /// `index` muss kleiner als die Ringgroesse sein.
    unsafe fn event_trb(&self, index: u32) -> [u32; 4] {
        let zeiger = (self.event_virt.as_u64() + index as u64 * TRB_BYTES as u64) as *const u32;
        [
            core::ptr::read_volatile(zeiger),
            core::ptr::read_volatile(zeiger.add(1)),
            core::ptr::read_volatile(zeiger.add(2)),
            core::ptr::read_volatile(zeiger.add(3)),
        ]
    }

    /// Alle anstehenden Events abholen und protokollieren.
    ///
    /// Liefert die Zahl der verarbeiteten Events. Wird vom
    /// `usb_task` gepollt (docs/xhci.md §7).
    pub fn events_abholen(&mut self) -> usize {
        let mut anzahl = 0usize;
        loop {
            // SAFETY: event_stand.index ist immer < RING_EINTRAEGE
            // (RingStand::weiter klemmt).
            let trb = unsafe { self.event_trb(self.event_stand.index) };
            let cycle = trb_cycle(trb[3]);
            // DAS IST DER GANZE TEST: Stimmt das Cycle-Bit nicht mit
            // unserem Zustand ueberein, ist der Ring LEER — kein
            // Fehlerfall, sondern der Normalzustand.
            if !self.event_stand.gehoert_uns(cycle) {
                break;
            }
            let typ = trb_typ(trb[3]);
            match typ {
                TRB_TYP_PORT_STATUS_CHANGE => {
                    let port = port_aus_event(trb[0]);
                    serial_println!("[xhci] EVENT: Port-Status-Aenderung an Port {}", port);
                    self.port_behandeln(port);
                }
                _ => {
                    serial_println!(
                        "[xhci] EVENT: {} (Typ {}) — in diesem Schritt nicht behandelt.",
                        trb_typ_text(typ),
                        typ
                    );
                }
            }
            self.event_stand.weiter();
            anzahl += 1;
            // Schutz gegen einen Ring, der aus irgendeinem Grund nur
            // noch gueltige TRBs liefert: Wir verarbeiten hoechstens
            // einen ganzen Umlauf je Durchgang.
            if anzahl > RING_EINTRAEGE as usize {
                serial_println!("[xhci] WARNUNG: Event Ring liefert ohne Ende — abgebrochen.");
                break;
            }
        }
        if anzahl > 0 {
            self.erdp_schreiben();
        }
        anzahl
    }

    /// Dem Controller sagen, wie weit wir gelesen haben.
    ///
    /// Bit 3 (`EHB`, Event Handler Busy) wird mitgeschrieben — es ist
    /// write-1-to-clear und muss quittiert werden, sonst meldet der
    /// Controller keine weiteren Interrupts. Auch beim Pollen wird es
    /// gesetzt; es stehenzulassen ist ein Fehler, der erst auffaellt,
    /// wenn spaeter auf Interrupts umgestellt wird.
    fn erdp_schreiben(&self) {
        let adresse = self.event_phys.as_u64() + self.event_stand.versatz();
        // SAFETY: laufzeit liegt im gemappten MMIO-Bereich.
        unsafe {
            self.laufzeit.schreibe64(RT_IR0 + IR_ERDP, adresse | (1 << 3));
        }
    }

    /// Einen Port nach einer gemeldeten Aenderung neu einlesen,
    /// protokollieren und die Aenderungs-Bits quittieren.
    fn port_behandeln(&mut self, port_eins_basiert: u8) {
        if port_eins_basiert == 0 || port_eins_basiert > self.max_ports {
            serial_println!(
                "[xhci] EVENT nennt Port {}, es gibt aber nur {} — ignoriert.",
                port_eins_basiert,
                self.max_ports
            );
            return;
        }
        let index = (port_eins_basiert - 1) as u64;
        let versatz = OP_PORTSC_BASIS + index * OP_PORT_ABSTAND;
        // SAFETY: index < max_ports, siehe Pruefung oben.
        let roh = unsafe { self.op.lese32(versatz) };
        let zustand = portsc_lesen(roh);
        serial_println!(
            "[xhci]   Port {}: {}{}, Tempo {} (PORTSC=0x{:08x})",
            port_eins_basiert,
            if zustand.angeschlossen {
                "angeschlossen"
            } else {
                "frei"
            },
            if zustand.aktiviert { ", aktiviert" } else { "" },
            zustand.tempo.text(),
            roh
        );
        // QUITTIEREN — aber NUR die Aenderungs-Bits. Wer den ganzen
        // gelesenen Wert zurueckschreibt, loescht dabei jede andere
        // anstehende Meldung UND deaktiviert ueber PED den Port
        // (siehe `portsc_quittierung`).
        let zu_quittieren = roh & PORTSC_NICHT_ANFASSEN & !(1 << 1);
        if zu_quittieren != 0 {
            // SAFETY: wie oben.
            unsafe {
                self.op
                    .schreibe32(versatz, portsc_quittierung(roh, zu_quittieren));
            }
        }
        if let Some(eintrag) = self.ports.get_mut(index as usize) {
            *eintrag = zustand;
        }
    }

    /// Alle Ports frisch einlesen (fuer den `usb`-Befehl).
    pub fn ports_lesen(&mut self) {
        for i in 0..self.max_ports {
            // SAFETY: i < max_ports, Versatz unter 0x1000.
            let roh = unsafe { self.op.lese32(OP_PORTSC_BASIS + i as u64 * OP_PORT_ABSTAND) };
            if let Some(eintrag) = self.ports.get_mut(i as usize) {
                *eintrag = portsc_lesen(roh);
            }
        }
    }

    /// Der Rohwert eines PORTSC (fuer die ausfuehrliche Anzeige).
    pub fn portsc_roh(&self, index: u8) -> u32 {
        if index >= self.max_ports {
            return 0;
        }
        // SAFETY: index < max_ports.
        unsafe { self.op.lese32(OP_PORTSC_BASIS + index as u64 * OP_PORT_ABSTAND) }
    }

    /// `USBSTS` — fuer die Anzeige „laeuft er noch?".
    pub fn usbsts(&self) -> u32 {
        // SAFETY: op im gemappten Bereich.
        unsafe { self.op.lese32(OP_USBSTS) }
    }

    pub fn laeuft(&self) -> bool {
        self.usbsts() & USBSTS_HCH == 0
    }

    /// Die Interrupter-Register und die ersten TRBs im Klartext.
    ///
    /// ===================================================================
    /// WARUM DAS FEST EINGEBAUT IST UND KEIN WEGGEWORFENES DEBUG
    ///
    /// Als der Event Ring beim ersten Anlauf schwieg, gab es genau
    /// keine Moeglichkeit, von aussen zu sehen, WO es klemmte: Der
    /// Controller lief, die Ports waren richtig gelesen, und der Ring
    /// blieb leer. Erst dieser Auszug zeigte, dass der Controller gar
    /// nichts geschrieben hatte — und damit, dass der Fehler in der
    /// EINRICHTUNG lag und nicht im Auslesen.
    ///
    /// Genau die Unterscheidung, die man ohne Auszug nicht treffen
    /// kann. Sie wird beim naechsten Schritt (Slots, Uebertragungen)
    /// wieder gebraucht.
    ///
    /// SIE LAEUFT NICHT MEHR IM TAKT MIT: Als Dauerausgabe alle zwei
    /// Sekunden hat sie das Protokoll so vollgeschrieben, dass die
    /// Zeilen, auf die es ankam, untergingen — ein Protokoll, das alles
    /// sagt, sagt nichts. Sie haengt jetzt an `usb --roh`.
    pub fn diagnose(&self) {
        // SAFETY: alle Versaetze liegen im gemappten MMIO-Bereich.
        let (iman, erstsz, erstba, erdp) = unsafe {
            (
                self.laufzeit.lese32(RT_IR0 + IR_IMAN),
                self.laufzeit.lese32(RT_IR0 + IR_ERSTSZ),
                self.laufzeit.lese32(RT_IR0 + IR_ERSTBA),
                self.laufzeit.lese32(RT_IR0 + IR_ERDP),
            )
        };
        serial_println!(
            "[xhci] DIAG USBSTS=0x{:08x} IMAN=0x{:08x} ERSTSZ={} ERSTBA=0x{:08x} ERDP=0x{:08x}              (Stand: Index {}, Cycle {})",
            self.usbsts(),
            iman,
            erstsz,
            erstba,
            erdp,
            self.event_stand.index,
            self.event_stand.cycle as u8
        );
        for i in 0..2u32 {
            // SAFETY: i < 2 < RING_EINTRAEGE.
            let t = unsafe { self.event_trb(i) };
            serial_println!(
                "[xhci] DIAG   TRB[{}] {:08x} {:08x} {:08x} {:08x} (Typ {}, Cycle {})",
                i,
                t[0],
                t[1],
                t[2],
                t[3],
                trb_typ(t[3]),
                trb_cycle(t[3]) as u8
            );
        }
    }

    /// Nur damit `cap` nicht als ungenutzt gilt — die
    /// Capability-Register werden nach der Initialisierung nicht mehr
    /// gebraucht, der Bereich bleibt aber der Anker des Mappings.
    pub fn cap_basis(&self) -> VirtAddr {
        self.cap.basis
    }
}

// ---------------------------------------------------------------------------
// SPEICHER-HILFEN
// ---------------------------------------------------------------------------

/// Einen MMIO-Bereich ungecacht mappen und die virtuelle Basis liefern.
fn mmio_mappen(phys_basis: u64, bytes: u64) -> Option<VirtAddr> {
    // Der BAR ist seitenausgerichtet (das verlangt PCI), aber wir
    // runden trotzdem ab — eine falsche Annahme waere hier ein
    // verschobenes Registerfenster.
    let start = phys_basis & !0xFFF;
    let seiten = bytes.div_ceil(4096);
    let virt_start = crate::memory::allocate_virt_bereich(seiten as usize)?;
    for i in 0..seiten {
        let page = Page::<Size4KiB>::containing_address(virt_start + i * 4096);
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(start + i * 4096));
        // SAFETY: `frame` zeigt auf einen PCI-MMIO-Bereich (aus BAR0),
        // nicht auf RAM — genau der Fall, fuer den `map_mmio` da ist.
        // Aliasing mit normalem Speicher ist damit ausgeschlossen.
        unsafe {
            crate::memory::map_mmio(page, frame).ok()?;
        }
    }
    Some(virt_start)
}

/// Physisch zusammenhaengende, GENULLTE Seiten fuer DMA.
///
/// Genullt, weil der Controller den Inhalt als gueltig deutet: Ein
/// ungenullter Ring enthaelt zufaellige Cycle-Bits, und der Controller
/// haelt zufaellige Bytes fuer TRBs.
fn seiten_holen(anzahl: usize) -> Option<VirtAddr> {
    let virt = crate::memory::allocate_pages(anzahl).ok()?;
    // SAFETY: `allocate_pages` hat gerade `anzahl` Seiten ab `virt`
    // gemappt; sie gehoeren uns exklusiv.
    unsafe {
        core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, anzahl * 4096);
    }
    Some(virt)
}

fn phys_von(virt: VirtAddr) -> Option<PhysAddr> {
    crate::memory::uebersetzen(virt)
}

// ---------------------------------------------------------------------------
// TESTS — die reinen Funktionen, ohne jede Hardware
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::register::*;

    #[test_case]
    fn test_params1_dekodieren() {
        // Slots 32, Interrupter 8, Ports 4 — so meldet sich qemu-xhci.
        let roh = (4u32 << 24) | (8 << 8) | 32;
        let p = params1_lesen(roh);
        assert_eq!(p.max_slots, 32);
        assert_eq!(p.max_interrupter, 8);
        assert_eq!(p.max_ports, 4);
    }

    #[test_case]
    fn test_params1_reservierte_bits_stoeren_nicht() {
        // Bits 19..23 sind reserviert. Wer die Interrupter-Zahl mit
        // einer 16-Bit-Maske herausschneidet, liest sie mit.
        let roh = (4u32 << 24) | (0x1F << 19) | (8 << 8) | 32;
        let p = params1_lesen(roh);
        assert_eq!(p.max_interrupter, 8, "reservierte Bits gehoeren nicht dazu");
    }

    #[test_case]
    fn test_capabilities_csz() {
        // CSZ gesetzt = 64-Byte-Kontexte. Die wichtigste Einzelinfo.
        let p = capabilities_lesen(0b100);
        assert!(p.kontext_64byte);
        let p = capabilities_lesen(0b000);
        assert!(!p.kontext_64byte, "ohne CSZ sind es 32 Byte");
    }

    #[test_case]
    fn test_capabilities_xecp_und_ac64() {
        let p = capabilities_lesen((0x20 << 16) | 1);
        assert!(p.adressen_64bit);
        assert_eq!(p.xecp_worte, 0x20, "xECP steht in WORTEN");
    }

    #[test_case]
    fn test_scratchpad_aus_zwei_feldern() {
        // Nur das untere Feld: 5 Puffer.
        assert_eq!(scratchpad_anzahl(5 << 27), 5);
        // NUR das obere Feld: 1 << 5 = 32 Puffer. Wer es weglaesst,
        // bekommt hier 0 — und legt keinen Speicher an.
        assert_eq!(scratchpad_anzahl(1 << 21), 32);
        // Beide zusammen: (1 << 5) | 3 = 35.
        assert_eq!(scratchpad_anzahl((1 << 21) | (3 << 27)), 35);
        assert_eq!(scratchpad_anzahl(0), 0);
    }

    #[test_case]
    fn test_portsc_dekodieren() {
        // Angeschlossen + aktiviert + high speed (3) + CSC.
        let roh = 1 | (1 << 1) | (3 << 10) | (1 << 17);
        let z = portsc_lesen(roh);
        assert!(z.angeschlossen);
        assert!(z.aktiviert);
        assert_eq!(z.tempo, Tempo::Hoch);
        assert!(z.aenderung_angeschlossen);
        assert!(z.hat_aenderung());

        let leer = portsc_lesen(0);
        assert!(!leer.angeschlossen);
        assert!(!leer.hat_aenderung());
    }

    #[test_case]
    fn test_portsc_quittierung_schont_ped() {
        // PED (Bit 1) ist write-1-to-clear und wuerde den Port
        // ABSCHALTEN. Es darf beim Quittieren nie mitgeschrieben werden.
        let roh = 1 | (1 << 1) | (1 << 17); // angeschlossen, aktiviert, CSC
        let wert = portsc_quittierung(roh, 1 << 17);
        assert_eq!(wert & (1 << 1), 0, "PED darf NICHT zurueckgeschrieben werden");
        assert_ne!(wert & (1 << 17), 0, "CSC soll quittiert werden");
    }

    #[test_case]
    fn test_portsc_quittierung_loescht_fremde_aenderungen_nicht() {
        // Zwei Aenderungen stehen an (CSC und PRC), quittiert wird nur
        // CSC. PRC muss stehenbleiben, sonst geht die Meldung verloren.
        let roh = 1 | (1 << 17) | (1 << 21);
        let wert = portsc_quittierung(roh, 1 << 17);
        assert_ne!(wert & (1 << 17), 0);
        assert_eq!(wert & (1 << 21), 0, "PRC nicht mitquittieren");
    }

    // -----------------------------------------------------------------
    // DIE RING-ARITHMETIK — der Kern des Treibers
    // -----------------------------------------------------------------

    #[test_case]
    fn test_ring_faengt_bei_null_mit_cycle_eins_an() {
        let r = RingStand::neu(4);
        assert_eq!(r.index, 0);
        assert!(r.cycle);
        // Eine genullte Seite (Cycle-Bit 0) gilt damit als LEER.
        assert!(!r.gehoert_uns(false));
        assert!(r.gehoert_uns(true));
    }

    #[test_case]
    fn test_ring_laeuft_um_und_kippt_das_cycle_bit() {
        let mut r = RingStand::neu(4);
        for erwartet in 1..4 {
            r.weiter();
            assert_eq!(r.index, erwartet);
            assert!(r.cycle, "innerhalb eines Umlaufs bleibt das Bit stehen");
        }
        // Der Umlauf.
        r.weiter();
        assert_eq!(r.index, 0);
        assert!(!r.cycle, "beim Umlauf MUSS das Cycle-Bit kippen");
    }

    #[test_case]
    fn test_ring_kippt_bei_jedem_umlauf_erneut() {
        // Ohne das zweite Kippen liefe der Konsument ab dem dritten
        // Umlauf ueber alte TRBs und hielte sie fuer neu.
        let mut r = RingStand::neu(2);
        let mut gesehen = alloc::vec::Vec::new();
        for _ in 0..8 {
            gesehen.push((r.index, r.cycle));
            r.weiter();
        }
        assert_eq!(
            gesehen,
            alloc::vec![
                (0, true),
                (1, true),
                (0, false),
                (1, false),
                (0, true),
                (1, true),
                (0, false),
                (1, false),
            ]
        );
    }

    #[test_case]
    fn test_ring_versatz_ist_sechzehn_byte_je_eintrag() {
        let mut r = RingStand::neu(4);
        assert_eq!(r.versatz(), 0);
        r.weiter();
        assert_eq!(r.versatz(), 16);
        r.weiter();
        assert_eq!(r.versatz(), 32);
    }

    #[test_case]
    fn test_trb_typ_und_cycle() {
        // Port Status Change (34) mit gesetztem Cycle-Bit.
        let wort3 = (34u32 << 10) | 1;
        assert_eq!(trb_typ(wort3), TRB_TYP_PORT_STATUS_CHANGE);
        assert!(trb_cycle(wort3));
        // Ohne Cycle-Bit.
        assert!(!trb_cycle(34u32 << 10));
    }

    #[test_case]
    fn test_port_aus_event_ist_einsbasiert() {
        // Port 1 steht als 1 in den Bits 24..31 — nicht als 0.
        assert_eq!(port_aus_event(1 << 24), 1);
        assert_eq!(port_aus_event(4 << 24), 4);
    }
}

// ---------------------------------------------------------------------------
// DER POLL-TASK
// ---------------------------------------------------------------------------

/// Sieht regelmaessig im Event Ring nach.
///
/// ===================================================================
/// GEPOLLT UND NICHT PER INTERRUPT — mit Ablaufdatum
///
/// xHCI benutzt normalerweise MSI-X, und das ist ein eigenes Vorhaben
/// (Tabelle im BAR finden, Vektoren zuordnen, APIC programmieren).
/// Fuer den Zuschnitt dieses Schrittes — Ports beobachten — reicht
/// Pollen vollkommen: Ein Steckvorgang ist ein MENSCHLICHES Ereignis,
/// 100 ms Latenz merkt niemand.
///
/// **Fuer eine Tastatur wird das anders**, und dann ist es der
/// richtige Zeitpunkt fuer die Interrupt-Frage. Dieselbe
/// Unterscheidung wie bei virtio-blk (gepollt) gegen virtio-net
/// (Interrupts): Der Unterschied ist nicht die Bequemlichkeit,
/// sondern ob unaufgefordert etwas ankommt.
pub async fn usb_task() {
    if !vorhanden() {
        return;
    }
    serial_println!("[xhci] Event-Task laeuft (gepollt, 100 ms).");
    loop {
        mit_controller(|c| {
            if let Some(controller) = c {
                controller.events_abholen();
            }
        });
        zeit::warte_ms(100).await;
    }
}
