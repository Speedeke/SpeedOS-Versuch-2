// audio::hda — Intel High Definition Audio, bis der Ton kommt
//
// ===========================================================================
// DER ZUSCHNITT STEHT IN docs/audio.md — hier die Kurzfassung
//
// Umgesetzt: Controller finden, ungecacht mappen, Reset, Codecs finden,
// EINEN Ausgabepfad (Pin -> DAC) konfigurieren, BDL + Ringpuffer,
// starten/stoppen, Position lesen.
//
// NICHT umgesetzt: Eingabe, mehrere Streams, Kopfhoerer-Erkennung,
// andere Raten als 48 kHz, CORB/RIRB (wir nehmen das Immediate Command
// Interface — Begruendung unten).
//
// ===========================================================================
// WARUM DAS IMMEDIATE COMMAND INTERFACE STATT CORB/RIRB
//
// HDA hat zwei Wege, ein Kommando („Verb") an einen Codec zu schicken:
// die Ringpuffer CORB/RIRB (fuer Dauerbetrieb) und das IMMEDIATE
// COMMAND INTERFACE (`ICW`/`IRR`/`ICS`) — ein Verb hinein, eine Antwort
// heraus, ohne Ringe.
//
// Wir setzen Verbs nur beim EINRICHTEN ab, ein paar Dutzend insgesamt.
// Dafuer ist das Immediate Interface gedacht, und es spart die halbe
// Komplexitaet des Treibers.
//
// **DER HAKEN, ehrlich notiert:** Nicht jede Hardware implementiert es
// zuverlaessig; manche Chipsaetze lassen es weg. Klemmt es auf dem
// Laptop, ist CORB/RIRB der Nachbau — und das steht dann in
// hardware-log.md, nicht hier als Ueberraschung.

use crate::audio::{AudioFehler, AudioGeraet, Sample, KANAELE};
use crate::{pci, serial_println, zeit};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{Page, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

// ---------------------------------------------------------------------------
// KONSTANTEN
// ---------------------------------------------------------------------------

const KLASSE_MULTIMEDIA: u8 = 0x04;
const UNTERKLASSE_HDA: u8 = 0x03;

const MMIO_BYTES: u64 = 16 * 1024;

// Globale Register
const R_GCAP: u64 = 0x00;
const R_GCTL: u64 = 0x08;
const R_STATESTS: u64 = 0x0E;
const R_INTCTL: u64 = 0x20;
// Immediate Command Interface
const R_ICW: u64 = 0x60;
const R_IRR: u64 = 0x64;
const R_ICS: u64 = 0x68;

/// Der erste Output-Stream-Deskriptor.
///
/// Die Stream-Deskriptoren liegen ab 0x80, je 0x20 Byte. Zuerst kommen
/// die EINGABE-Streams (ISS), dann die AUSGABE-Streams (OSS) — die Zahl
/// steht in GCAP. Wer stumpf 0x80 nimmt, programmiert auf Hardware mit
/// Eingaengen einen EINGABE-Stream und wundert sich, dass nichts kommt.
const SD_BASIS: u64 = 0x80;
const SD_GROESSE: u64 = 0x20;
// Stream-Deskriptor-Register (relativ)
const SD_CTL: u64 = 0x00;
const SD_STS: u64 = 0x03;
const SD_LPIB: u64 = 0x04;
const SD_CBL: u64 = 0x08;
const SD_LVI: u64 = 0x0C;
const SD_FMT: u64 = 0x12;
const SD_BDLPL: u64 = 0x18;
const SD_BDLPU: u64 = 0x1C;

/// Unsere Stream-Nummer (1..15; 0 heisst „unbenutzt").
const STREAM_NR: u8 = 1;

/// Der Ringpuffer: 4 Seiten = 16 KiB = 4096 Stereo-Frames = ~85 ms.
///
/// Gross genug, dass ein Nachfuellen im 8-ms-Takt bequem reicht, und
/// klein genug, dass eine Lautstaerkeaenderung nicht spuerbar spaeter
/// wirkt.
const PUFFER_SEITEN: usize = 4;
const PUFFER_BYTES: usize = PUFFER_SEITEN * 4096;
const PUFFER_FRAMES: usize = PUFFER_BYTES / (KANAELE * 2);
/// Die BDL bekommt ZWEI Eintraege ueber denselben Puffer — die
/// Spezifikation verlangt mindestens zwei.
const BDL_EINTRAEGE: usize = 2;

const FRIST_RESET_US: u64 = 500_000;
const FRIST_VERB_US: u64 = 100_000;

// ---------------------------------------------------------------------------
// VERBS — reine Rechnerei, ohne Hardware testbar
// ---------------------------------------------------------------------------

/// Ein HDA-Verb in seine 32 Bit packen.
///
/// Aufbau: Codec-Adresse (4 Bit, 28..31), Node-ID (8 Bit, 20..27),
/// Verb + Nutzlast (20 Bit).
///
/// **ZWEI FORMEN, und das ist die Falle:** Ein „langes" Verb hat einen
/// 12-Bit-Code und 8 Bit Nutzlast, ein „kurzes" einen 4-Bit-Code und
/// 16 Bit Nutzlast. Wer ein langes Verb mit 16 Bit Nutzlast absetzt,
/// ueberschreibt den Verb-Code — und der Codec fuehrt ein ganz anderes
/// Kommando aus.
pub fn verb_bauen(codec: u8, node: u8, verb: u16, nutzlast: u16) -> u32 {
    let kopf = ((codec as u32 & 0xF) << 28) | ((node as u32) << 20);
    if verb & 0xF00 == 0xF00 || verb & 0xF00 == 0x700 {
        // Langes Verb (12 Bit) mit 8 Bit Nutzlast.
        kopf | ((verb as u32 & 0xFFF) << 8) | (nutzlast as u32 & 0xFF)
    } else {
        // Kurzes Verb (4 Bit) mit 16 Bit Nutzlast.
        kopf | ((verb as u32 & 0xF) << 16) | (nutzlast as u32 & 0xFFFF)
    }
}

// Verb-Codes
const V_GET_PARAMETER: u16 = 0xF00;
const V_GET_CONNECTION: u16 = 0xF02;
const V_SET_STREAM_FORMAT: u16 = 0x2;
const V_SET_AMP: u16 = 0x3;
const V_SET_STREAM_CHANNEL: u16 = 0xF06 - 0xF00 + 0x700; // 0x706
const V_SET_PIN_CTL: u16 = 0x707;
const V_SET_POWER: u16 = 0x705;
const V_GET_CONFIG_DEFAULT: u16 = 0xF1C;

// Parameter-IDs fuer GET_PARAMETER
const P_NODE_COUNT: u16 = 0x04;
const P_FUNCTION_TYPE: u16 = 0x05;
const P_WIDGET_CAP: u16 = 0x09;

/// Der Widget-Typ aus den Capabilities (Bits 20..23).
pub fn widget_typ(caps: u32) -> u8 {
    ((caps >> 20) & 0xF) as u8
}

const W_AUSGANG: u8 = 0x0; // Audio Output (DAC)
const W_PIN: u8 = 0x4; // Pin Complex

/// Aus `SUBORDINATE_NODE_COUNT`: (erster Knoten, Anzahl).
pub fn knoten_bereich(antwort: u32) -> (u8, u8) {
    (((antwort >> 16) & 0xFF) as u8, (antwort & 0xFF) as u8)
}

/// Taugt dieser Pin als Ausgang? Aus der *Configuration Default*.
///
/// Bits 20..23 sind das Geraet (0 = Line Out, 1 = Speaker,
/// 2 = Kopfhoerer), Bits 30..31 die Anschlussart (1 = „No Physical
/// Connection" — ein Pin, der nirgendwo hingeht).
pub fn pin_ist_ausgang(config: u32) -> bool {
    let geraet = ((config >> 20) & 0xF) as u8;
    let verbindung = ((config >> 30) & 0x3) as u8;
    verbindung != 1 && matches!(geraet, 0x0..=0x2)
}

/// Das Stream-Format fuer 48 kHz, 16 Bit, N Kanaele.
///
/// Bits: 14 = Basis (0 = 48 kHz), 11..13 = Multiplikator,
/// 8..10 = Divisor, 4..6 = Bittiefe (1 = 16 Bit), 0..3 = Kanaele - 1.
pub fn format_48k_16bit(kanaele: u8) -> u16 {
    (1 << 4) | ((kanaele.saturating_sub(1)) as u16 & 0xF)
}

// ---------------------------------------------------------------------------
// MMIO
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Mmio {
    basis: VirtAddr,
}

impl Mmio {
    /// # Safety: Versatz muss im gemappten Bereich liegen.
    unsafe fn r8(&self, v: u64) -> u8 {
        core::ptr::read_volatile((self.basis.as_u64() + v) as *const u8)
    }
    /// # Safety: wie `r8`.
    unsafe fn w8(&self, v: u64, w: u8) {
        core::ptr::write_volatile((self.basis.as_u64() + v) as *mut u8, w);
    }
    /// # Safety: wie `r8`.
    unsafe fn r16(&self, v: u64) -> u16 {
        core::ptr::read_volatile((self.basis.as_u64() + v) as *const u16)
    }
    /// # Safety: wie `r8`.
    unsafe fn w16(&self, v: u64, w: u16) {
        core::ptr::write_volatile((self.basis.as_u64() + v) as *mut u16, w);
    }
    /// # Safety: wie `r8`.
    unsafe fn r32(&self, v: u64) -> u32 {
        core::ptr::read_volatile((self.basis.as_u64() + v) as *const u32)
    }
    /// # Safety: wie `r8`.
    unsafe fn w32(&self, v: u64, w: u32) {
        core::ptr::write_volatile((self.basis.as_u64() + v) as *mut u32, w);
    }
}

// ---------------------------------------------------------------------------
// DER CONTROLLER
// ---------------------------------------------------------------------------

pub struct Hda {
    mmio: Mmio,
    /// Versatz unseres Ausgabe-Stream-Deskriptors.
    sd: u64,
    codec: u8,
    dac: u8,
    pin: u8,
    puffer: VirtAddr,
    #[allow(dead_code)]
    bdl: VirtAddr,
    /// Wie weit wir in den Ringpuffer geschrieben haben (in Frames,
    /// monoton wachsend — der Umlauf ergibt sich per Modulo).
    geschrieben: u64,
    /// Die zuletzt gelesene Rohposition (0..PUFFER_FRAMES).
    letzte_position: u64,
    /// Wie viele VOLLE Umlaeufe der Controller schon hinter sich hat.
    umlaeufe: u64,
    /// Wo der Lesezeiger beim Start dieser Wiedergabe stand.
    ///
    /// Er faengt NICHT bei 0 an: Nach dem Anhalten bleibt er stehen, wo
    /// er war. Ohne diesen Bezugspunkt zaehlte jede Wiedergabe die
    /// Position der vorigen mit.
    start_position: u64,
    laeuft: bool,
    seiten: Vec<VirtAddr>,
}

static HDA: Mutex<Option<Hda>> = Mutex::new(None);
static VORHANDEN: AtomicBool = AtomicBool::new(false);

pub fn vorhanden() -> bool {
    VORHANDEN.load(Ordering::Relaxed)
}

pub fn mit_hda<R>(f: impl FnOnce(Option<&mut Hda>) -> R) -> R {
    let mut g = HDA.lock();
    f(g.as_mut())
}

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

pub fn init() {
    match starten() {
        Ok(()) => {
            VORHANDEN.store(true, Ordering::Relaxed);
            serial_println!("[hda] bereit.");
        }
        Err(AudioFehler::KeinGeraet) => {
            serial_println!("[hda] kein HDA-Controller vorhanden.");
        }
        Err(f) => serial_println!("[hda] FEHLGESCHLAGEN: {}", f.text()),
    }
}

fn starten() -> Result<(), AudioFehler> {
    let geraet = pci::finde_klasse(KLASSE_MULTIMEDIA, UNTERKLASSE_HDA, 0)
        .or_else(pci_hda_beliebiges_progif)
        .ok_or(AudioFehler::KeinGeraet)?;
    serial_println!(
        "[hda] gefunden: {:02x}:{:02x}.{} {:04x}:{:04x}",
        geraet.bus,
        geraet.geraet,
        geraet.funktion,
        geraet.vendor_id,
        geraet.device_id
    );
    let pci::Bar::Speicher { basis, .. } = geraet.bars[0] else {
        return Err(AudioFehler::KeinGeraet);
    };
    // Memory Space + Bus Master — ohne Bus Master kein DMA.
    geraet.command_setzen((1 << 1) | (1 << 2));

    let virt = mmio_mappen(basis, MMIO_BYTES).ok_or(AudioFehler::KeinSpeicher)?;
    let mmio = Mmio { basis: virt };
    serial_println!("[hda] MMIO ungecacht gemappt: 0x{:016x}", basis);

    // --- Reset ---
    // SAFETY: `virt` ist gerade gemappt worden.
    unsafe {
        mmio.w32(R_GCTL, 0);
    }
    if !warten_auf(FRIST_RESET_US, || unsafe { mmio.r32(R_GCTL) & 1 == 0 }) {
        return Err(AudioFehler::Zeitueberschreitung);
    }
    // SAFETY: wie oben.
    unsafe {
        mmio.w32(R_GCTL, 1);
    }
    if !warten_auf(FRIST_RESET_US, || unsafe { mmio.r32(R_GCTL) & 1 == 1 }) {
        return Err(AudioFehler::Zeitueberschreitung);
    }
    // DIE 521 MIKROSEKUNDEN. Die Spezifikation verlangt sie
    // ausdruecklich, damit die Codecs sich am Link anmelden koennen.
    // Wer sofort weiterliest, findet KEINE Codecs und haelt den
    // Controller fuer kaputt (docs/audio.md §3, Schritt 2).
    let bis = zeit::us_seit_boot() + 1000;
    while zeit::us_seit_boot() < bis {
        core::hint::spin_loop();
    }

    // SAFETY: wie oben.
    let (gcap, statests) = unsafe { (mmio.r16(R_GCAP), mmio.r16(R_STATESTS)) };
    let eingaenge = ((gcap >> 8) & 0xF) as u64;
    let ausgaenge = ((gcap >> 12) & 0xF) as u64;
    // DIE AUSGABE-STREAMS LIEGEN HINTER DEN EINGABE-STREAMS.
    let sd = SD_BASIS + eingaenge * SD_GROESSE;
    serial_println!(
        "[hda] GCAP 0x{:04x}: {} Eingaenge, {} Ausgaenge -> Stream-Deskriptor +0x{:x}",
        gcap,
        eingaenge,
        ausgaenge,
        sd
    );
    serial_println!("[hda] STATESTS 0x{:04x} (ein Bit je Codec)", statests);
    if statests == 0 {
        serial_println!("[hda] kein Codec gemeldet.");
        return Err(AudioFehler::KeinAusgabepfad);
    }
    // Interrupts aus — wir pollen (docs/audio.md §2).
    // SAFETY: wie oben.
    unsafe {
        mmio.w32(R_INTCTL, 0);
    }

    // --- Ausgabepfad suchen ---
    let mut gefunden = None;
    for codec in 0..15u8 {
        if statests & (1 << codec) == 0 {
            continue;
        }
        serial_println!("[hda] Codec {} gefunden.", codec);
        if let Some((dac, pin)) = ausgabepfad_suchen(&mmio, codec) {
            serial_println!("[hda]   Ausgabepfad: Pin {} <- DAC {}", pin, dac);
            gefunden = Some((codec, dac, pin));
            break;
        }
        serial_println!("[hda]   kein brauchbarer Ausgabepfad an diesem Codec.");
    }
    let (codec, dac, pin) = gefunden.ok_or(AudioFehler::KeinAusgabepfad)?;

    // --- Speicher ---
    let mut seiten = Vec::new();
    let puffer = seiten_holen(PUFFER_SEITEN).ok_or(AudioFehler::KeinSpeicher)?;
    seiten.push(puffer);
    let puffer_phys = phys_von(puffer).ok_or(AudioFehler::KeinSpeicher)?;
    let bdl = seiten_holen(1).ok_or(AudioFehler::KeinSpeicher)?;
    seiten.push(bdl);
    let bdl_phys = phys_von(bdl).ok_or(AudioFehler::KeinSpeicher)?;

    // Die BDL: zwei Eintraege ueber je die Haelfte des Puffers.
    // SAFETY: `bdl` ist eine frisch allozierte, genullte Seite.
    unsafe {
        let haelfte = (PUFFER_BYTES / BDL_EINTRAEGE) as u32;
        for i in 0..BDL_EINTRAEGE {
            let e = (bdl.as_u64() as *mut u32).add(i * 4);
            let adresse = puffer_phys.as_u64() + (i as u64 * haelfte as u64);
            core::ptr::write_volatile(e, adresse as u32);
            core::ptr::write_volatile(e.add(1), (adresse >> 32) as u32);
            core::ptr::write_volatile(e.add(2), haelfte);
            core::ptr::write_volatile(e.add(3), 0); // kein IOC — wir pollen
        }
    }

    // --- Stream-Deskriptor ---
    // SAFETY: `sd` liegt innerhalb der gemappten 16 KiB.
    unsafe {
        // Reset des Streams.
        mmio.w8(sd + SD_CTL, 1);
        let _ = warten_auf(FRIST_RESET_US, || mmio.r8(sd + SD_CTL) & 1 != 0);
        mmio.w8(sd + SD_CTL, 0);
        let _ = warten_auf(FRIST_RESET_US, || mmio.r8(sd + SD_CTL) & 1 == 0);

        mmio.w32(sd + SD_CBL, PUFFER_BYTES as u32);
        mmio.w16(sd + SD_LVI, (BDL_EINTRAEGE - 1) as u16);
        mmio.w16(sd + SD_FMT, format_48k_16bit(KANAELE as u8));
        mmio.w32(sd + SD_BDLPL, bdl_phys.as_u64() as u32);
        mmio.w32(sd + SD_BDLPU, (bdl_phys.as_u64() >> 32) as u32);
        // Stream-Nummer in Bits 20..23 des CTL (als 32-Bit-Zugriff).
        let ctl = mmio.r32(sd + SD_CTL) & !(0xF << 20);
        mmio.w32(sd + SD_CTL, ctl | ((STREAM_NR as u32) << 20));
    }

    // --- Codec scharf schalten ---
    // Strom an (D0), Format, Stream-Zuordnung, Pin freigeben, Amps auf.
    verb(&mmio, codec, dac, V_SET_POWER, 0);
    verb(&mmio, codec, pin, V_SET_POWER, 0);
    verb(
        &mmio,
        codec,
        dac,
        V_SET_STREAM_FORMAT,
        format_48k_16bit(KANAELE as u8),
    );
    // Stream-Nummer in den oberen vier Bit, Kanal 0 in den unteren.
    verb(&mmio, codec, dac, V_SET_STREAM_CHANNEL, (STREAM_NR as u16) << 4);
    // Pin: Output Enable (Bit 6) + Kopfhoerer-Verstaerker (Bit 7).
    verb(&mmio, codec, pin, V_SET_PIN_CTL, 0b1100_0000);
    // DIE AMPS. Der haeufigste Grund, warum alles laeuft und nichts zu
    // hoeren ist (docs/audio.md §6, Befund 2). 0xB000 = Output, beide
    // Kanaele, NICHT stumm; die unteren Bits sind die Verstaerkung.
    verb(&mmio, codec, dac, V_SET_AMP, 0xB000 | 0x3F);
    verb(&mmio, codec, pin, V_SET_AMP, 0xB000 | 0x3F);
    serial_println!("[hda] Codec {}: DAC {} und Pin {} scharf.", codec, dac, pin);

    *HDA.lock() = Some(Hda {
        mmio,
        sd,
        codec,
        dac,
        pin,
        puffer,
        bdl,
        geschrieben: 0,
        letzte_position: 0,
        umlaeufe: 0,
        start_position: 0,
        laeuft: false,
        seiten,
    });
    Ok(())
}

/// Manche Controller melden ein anderes Prog-IF als 0.
fn pci_hda_beliebiges_progif() -> Option<pci::PciGeraet> {
    pci::mit_geraeten(|liste| {
        liste
            .iter()
            .find(|g| g.klasse == KLASSE_MULTIMEDIA && g.unterklasse == UNTERKLASSE_HDA)
            .cloned()
    })
}

/// Ein Verb absetzen und die Antwort holen.
fn verb(mmio: &Mmio, codec: u8, node: u8, v: u16, nutzlast: u16) -> u32 {
    // Warten, bis das Interface frei ist.
    if !warten_auf(FRIST_VERB_US, || unsafe { mmio.r16(R_ICS) & 1 == 0 }) {
        return 0;
    }
    // SAFETY: alle Versaetze liegen im gemappten Bereich.
    unsafe {
        mmio.w32(R_ICW, verb_bauen(codec, node, v, nutzlast));
        // Bit 0 = Busy setzen -> Kommando losschicken.
        mmio.w16(R_ICS, 1);
    }
    if !warten_auf(FRIST_VERB_US, || unsafe { mmio.r16(R_ICS) & 1 == 0 }) {
        return 0;
    }
    // SAFETY: wie oben.
    unsafe { mmio.r32(R_IRR) }
}

/// Einen Ausgabepfad Pin -> DAC suchen.
///
/// **HOECHSTENS ZWEI EBENEN** (docs/audio.md §3, Schritt 5): Pin->DAC
/// und Pin->Mixer->DAC decken den Normalfall ab. Ein vollstaendiger
/// Graph-Durchlauf mit Zyklenschutz ist die richtige Loesung — und ein
/// eigenes Vorhaben.
fn ausgabepfad_suchen(mmio: &Mmio, codec: u8) -> Option<(u8, u8)> {
    // Die Function Groups des Root-Knotens.
    let (erste_fg, anzahl_fg) = knoten_bereich(verb(mmio, codec, 0, V_GET_PARAMETER, P_NODE_COUNT));
    for fg in erste_fg..erste_fg.saturating_add(anzahl_fg) {
        let typ = verb(mmio, codec, fg, V_GET_PARAMETER, P_FUNCTION_TYPE) & 0xFF;
        if typ != 0x01 {
            continue; // keine Audio Function Group
        }
        let (erstes, anzahl) = knoten_bereich(verb(mmio, codec, fg, V_GET_PARAMETER, P_NODE_COUNT));
        let ende = erstes.saturating_add(anzahl);
        for node in erstes..ende {
            let caps = verb(mmio, codec, node, V_GET_PARAMETER, P_WIDGET_CAP);
            if widget_typ(caps) != W_PIN {
                continue;
            }
            let config = verb(mmio, codec, node, V_GET_CONFIG_DEFAULT, 0);
            if !pin_ist_ausgang(config) {
                continue;
            }
            // Ebene 1: die Verbindungsliste des Pins.
            let liste = verb(mmio, codec, node, V_GET_CONNECTION, 0);
            for schritt in 0..4 {
                let ziel = ((liste >> (schritt * 8)) & 0xFF) as u8;
                if ziel == 0 || ziel >= ende {
                    continue;
                }
                let zcaps = verb(mmio, codec, ziel, V_GET_PARAMETER, P_WIDGET_CAP);
                if widget_typ(zcaps) == W_AUSGANG {
                    return Some((ziel, node));
                }
                // Ebene 2: ueber einen Mixer/Selektor hinweg.
                let liste2 = verb(mmio, codec, ziel, V_GET_CONNECTION, 0);
                for s2 in 0..4 {
                    let ziel2 = ((liste2 >> (s2 * 8)) & 0xFF) as u8;
                    if ziel2 == 0 || ziel2 >= ende {
                        continue;
                    }
                    let z2caps = verb(mmio, codec, ziel2, V_GET_PARAMETER, P_WIDGET_CAP);
                    if widget_typ(z2caps) == W_AUSGANG {
                        return Some((ziel2, node));
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// DAS TRAIT
// ---------------------------------------------------------------------------

impl AudioGeraet for Hda {
    fn name(&self) -> String {
        alloc::format!("Intel HDA (Codec {}, DAC {}, Pin {})", self.codec, self.dac, self.pin)
    }

    fn puffer_frames(&self) -> usize {
        PUFFER_FRAMES
    }

    fn gespielte_frames(&mut self) -> u64 {
        // ===============================================================
        // `SD_LPIB` LAEUFT UM — und genau daran ist die erste Fassung
        // gescheitert.
        //
        // Das Register liefert die Position IM PUFFER (0..CBL), nicht
        // die Gesamtzahl gespielter Frames. Unser `geschrieben` waechst
        // dagegen monoton. Nach dem ersten Umlauf wurde
        // `belegt = geschrieben - gespielt` deshalb riesig, `frei` fiel
        // auf 0, und `schreiben` lieferte fuer immer 0.
        //
        // Fehlerbild: Der Ton laeuft an und bleibt nach weniger als
        // einem Puffer stehen — bei jedem Lauf an einer anderen Stelle,
        // weil es davon abhaengt, wo der Lesezeiger beim Start stand.
        // Das sah wie ein Hardware-Problem aus und war Arithmetik.
        //
        // Ein Umlauf ist erkannt, wenn die Position KLEINER wird.
        // SAFETY: `sd` liegt im gemappten Bereich.
        let byte = unsafe { self.mmio.r32(self.sd + SD_LPIB) } as u64;
        let position = byte / (KANAELE as u64 * 2);
        if position < self.letzte_position {
            self.umlaeufe += 1;
        }
        self.letzte_position = position;
        // AB DER STARTPOSITION rechnen, nicht ab 0: Der Lesezeiger steht
        // beim Beginn einer Wiedergabe dort, wo die vorige aufgehoert
        // hat. `saturating_sub`, damit ein Umlauf innerhalb der ersten
        // Runde nicht negativ wird.
        let roh = self.umlaeufe * PUFFER_FRAMES as u64 + position;
        roh.saturating_sub(self.start_position)
    }

    fn freie_frames(&mut self) -> usize {
        let gespielt = self.gespielte_frames();
        let belegt = self.geschrieben.saturating_sub(gespielt) as usize;
        PUFFER_FRAMES.saturating_sub(belegt)
    }

    fn schreiben(&mut self, frames: &[Sample]) -> usize {
        let neue_frames = frames.len() / KANAELE;
        if neue_frames == 0 {
            return 0;
        }
        // WIE VIEL PLATZ IST FREI? Die Hardware liest bei
        // `gespielte_frames`; alles davor ist verbraucht. Wer ohne diese
        // Rechnung schreibt, ueberholt den Lesezeiger und ueberschreibt
        // Ungespieltes — das hoert man als Knacken.
        let gespielt = self.gespielte_frames();
        let belegt = self.geschrieben.saturating_sub(gespielt) as usize;
        let frei = PUFFER_FRAMES.saturating_sub(belegt);
        let nehmen = neue_frames.min(frei);
        if nehmen == 0 {
            return 0;
        }
        // SAFETY: `puffer` ist unser Ringpuffer, die Position wird
        // modulo PUFFER_FRAMES gerechnet.
        unsafe {
            let basis = self.puffer.as_u64() as *mut i16;
            for f in 0..nehmen {
                let ziel = ((self.geschrieben as usize + f) % PUFFER_FRAMES) * KANAELE;
                for k in 0..KANAELE {
                    core::ptr::write_volatile(basis.add(ziel + k), frames[f * KANAELE + k]);
                }
            }
        }
        self.geschrieben += nehmen as u64;
        nehmen
    }

    fn starten(&mut self) -> Result<(), AudioFehler> {
        if self.laeuft {
            return Ok(());
        }
        // SAFETY: `sd` im gemappten Bereich.
        unsafe {
            self.mmio.w8(self.sd + SD_STS, 0x1C); // Statusbits quittieren
            let ctl = self.mmio.r32(self.sd + SD_CTL);
            self.mmio.w32(self.sd + SD_CTL, ctl | 0x2); // RUN
        }
        self.laeuft = true;
        Ok(())
    }

    /// Wiedergabe anhalten.
    ///
    /// ===================================================================
    /// ANHALTEN IST NICHT ZURUECKSETZEN — der Fehler der ersten Fassung
    ///
    /// Hier stand einmal `umlaeufe = 0; letzte_position = 0`. Das sah
    /// aufgeraeumt aus und war falsch: Wer NACH dem Anhalten fragt, wie
    /// viel gespielt wurde, bekam nur noch die rohe Position im
    /// Ringpuffer. Ein 2-Sekunden-Ton (96 000 Frames) meldete „1814" —
    /// naemlich genau da, wo der Lesezeiger im Puffer stehengeblieben
    /// war.
    ///
    /// Der Zaehler ueberlebt das Anhalten jetzt. Zurueckgesetzt wird
    /// ausschliesslich in `leeren()`, und das laeuft vor jeder neuen
    /// Wiedergabe. Damit ist `gespielte_frames()` nach dem Stop
    /// weiterhin eine sinnvolle Antwort — und genau das braucht eine
    /// Fortschrittsanzeige.
    fn stoppen(&mut self) {
        // SAFETY: `sd` im gemappten Bereich.
        unsafe {
            let ctl = self.mmio.r32(self.sd + SD_CTL);
            self.mmio.w32(self.sd + SD_CTL, ctl & !0x2);
        }
        self.laeuft = false;
    }

    fn laeuft(&self) -> bool {
        self.laeuft
    }
}

impl Hda {
    /// Den Ringpuffer mit Stille fuellen.
    pub fn leeren(&mut self) {
        // SAFETY: `puffer` sind PUFFER_SEITEN von uns allozierte Seiten.
        unsafe {
            core::ptr::write_bytes(self.puffer.as_u64() as *mut u8, 0, PUFFER_BYTES);
        }
        // HIER — und nur hier — wird zurueckgesetzt. `leeren()` laeuft
        // vor jeder Wiedergabe; `stoppen()` fasst die Zaehler bewusst
        // nicht mehr an (siehe dort).
        //
        // Die Reihenfolge ist wichtig: erst die Buchhaltung nullen,
        // DANN die Position lesen. Andersherum zaehlte der erste
        // Vergleich einen Umlauf zu viel, wenn der Lesezeiger noch
        // hinten im Puffer stand.
        self.umlaeufe = 0;
        self.letzte_position = 0;
        // SAFETY: `sd` liegt im gemappten Bereich.
        let byte = unsafe { self.mmio.r32(self.sd + SD_LPIB) } as u64;
        self.letzte_position = byte / (KANAELE as u64 * 2);
        self.start_position = self.letzte_position;
        self.geschrieben = 0;
    }

    pub fn codec(&self) -> u8 {
        self.codec
    }
    pub fn dac(&self) -> u8 {
        self.dac
    }
    pub fn pin(&self) -> u8 {
        self.pin
    }
    /// Wie viele Seiten dieser Treiber haelt (fuer `audio`-Anzeige).
    pub fn seiten_anzahl(&self) -> usize {
        self.seiten.len()
    }
}

// ---------------------------------------------------------------------------
// SPEICHER
// ---------------------------------------------------------------------------

fn mmio_mappen(phys_basis: u64, bytes: u64) -> Option<VirtAddr> {
    let start = phys_basis & !0xFFF;
    let seiten = bytes.div_ceil(4096);
    let virt_start = crate::memory::allocate_virt_bereich(seiten as usize)?;
    for i in 0..seiten {
        let page = Page::<Size4KiB>::containing_address(virt_start + i * 4096);
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(start + i * 4096));
        // SAFETY: `frame` zeigt auf einen PCI-MMIO-Bereich (BAR0), nicht
        // auf RAM — genau der Fall, fuer den `map_mmio` da ist.
        unsafe {
            crate::memory::map_mmio(page, frame).ok()?;
        }
    }
    Some(virt_start)
}

fn seiten_holen(anzahl: usize) -> Option<VirtAddr> {
    let virt = crate::memory::allocate_pages(anzahl).ok()?;
    // SAFETY: gerade alloziert, gehoert uns exklusiv.
    unsafe {
        core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, anzahl * 4096);
    }
    Some(virt)
}

fn phys_von(virt: VirtAddr) -> Option<PhysAddr> {
    crate::memory::uebersetzen(virt)
}

// ---------------------------------------------------------------------------
// TESTS — die reine Rechnerei, ohne Hardware
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// **DIE ZWEI VERB-FORMEN.** Ein langes Verb hat 12 Bit Code und
    /// 8 Bit Nutzlast, ein kurzes 4 Bit Code und 16 Bit Nutzlast. Wer
    /// ein langes mit 16 Bit Nutzlast absetzt, ueberschreibt den Code.
    #[test_case]
    fn test_verb_bauen_lang_und_kurz() {
        // Langes Verb: GET_PARAMETER (0xF00) an Codec 0, Node 1.
        let v = verb_bauen(0, 1, V_GET_PARAMETER, P_NODE_COUNT);
        assert_eq!(v, (1 << 20) | (0xF00 << 8) | 0x04);
        // Kurzes Verb: SET_STREAM_FORMAT (0x2) mit 16 Bit Nutzlast.
        let v = verb_bauen(0, 3, V_SET_STREAM_FORMAT, 0x4011);
        assert_eq!(v, (3 << 20) | (0x2 << 16) | 0x4011);
    }

    #[test_case]
    fn test_verb_codec_adresse() {
        let v = verb_bauen(2, 0, V_GET_PARAMETER, 0);
        assert_eq!(v >> 28, 2);
        // Nur vier Bit — eine groessere Adresse darf nicht ueberlaufen.
        let v = verb_bauen(0xFF, 0, V_GET_PARAMETER, 0);
        assert_eq!(v >> 28, 0xF);
    }

    #[test_case]
    fn test_knoten_bereich() {
        // Antwort: erster Knoten 0x02 in Bits 16..23, Anzahl 0x0A.
        let (erster, anzahl) = knoten_bereich((0x02 << 16) | 0x0A);
        assert_eq!(erster, 2);
        assert_eq!(anzahl, 10);
    }

    #[test_case]
    fn test_widget_typ() {
        assert_eq!(widget_typ(0x0 << 20), W_AUSGANG);
        assert_eq!(widget_typ(0x4 << 20), W_PIN);
    }

    /// Ein Pin ohne physische Verbindung taugt nicht — sonst schickt man
    /// den Ton an eine Buchse, die es nicht gibt.
    #[test_case]
    fn test_pin_ohne_verbindung_taugt_nicht() {
        // Geraet 0 (Line Out), Verbindung 1 (No Physical Connection).
        let config = (0x0 << 20) | (1 << 30);
        assert!(!pin_ist_ausgang(config));
        // Dasselbe Geraet MIT Verbindung.
        let config = 0x0 << 20;
        assert!(pin_ist_ausgang(config));
    }

    #[test_case]
    fn test_pin_geraetetypen() {
        assert!(pin_ist_ausgang(0x0 << 20), "Line Out");
        assert!(pin_ist_ausgang(0x1 << 20), "Speaker");
        assert!(pin_ist_ausgang(0x2 << 20), "Kopfhoerer");
        assert!(!pin_ist_ausgang(0x8 << 20), "Line In ist kein Ausgang");
        assert!(!pin_ist_ausgang(0xA << 20), "Mikrofon erst recht nicht");
    }

    #[test_case]
    fn test_stream_format() {
        // 48 kHz, 16 Bit, Stereo: Bittiefe 1 in Bits 4..6, Kanaele-1.
        assert_eq!(format_48k_16bit(2), (1 << 4) | 1);
        assert_eq!(format_48k_16bit(1), (1 << 4), "Mono: Kanalfeld 0");
    }

    /// Der Ringpuffer fasst so viele Frames, wie die Rechnung sagt —
    /// eine falsche Zahl hier heisst Ueberschreiben oder Luecken.
    #[test_case]
    fn test_puffergroesse_stimmt() {
        assert_eq!(PUFFER_BYTES, 16384);
        assert_eq!(PUFFER_FRAMES, 4096, "16384 / (2 Kanaele * 2 Byte)");
        // Bei 48 kHz sind das rund 85 ms.
        let ms = PUFFER_FRAMES as u64 * 1000 / crate::audio::ABTASTRATE as u64;
        assert!((80..=90).contains(&ms), "{} ms", ms);
    }
}
