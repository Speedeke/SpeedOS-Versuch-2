// usb::xhci::aufzaehlung — vom Port zum erkannten Geraet
//
// ===========================================================================
// DER ABLAUF, UND WARUM ER SO UNERBITTLICH IST
//
//   1. PORT RESET        — erst danach hat der Port eine Geschwindigkeit
//   2. ENABLE SLOT       — der Controller gibt uns eine Slot-Nummer
//   3. DEVICE CONTEXT    — Speicher, in den der Controller schreibt
//   4. ADDRESS DEVICE    — Controller vergibt die USB-Adresse
//   5. DESKRIPTOREN      — ueber Control-Transfers auf EP0
//   6. SET CONFIGURATION — das Geraet wird benutzbar
//   7. ENDPUNKTE         — Transfer Ring je Interrupt-IN-Endpunkt
//
// Jeder Schritt setzt den vorigen voraus, und keiner meldet von selbst,
// dass er den vorigen vermisst. Wer Schritt 4 vor Schritt 1 macht,
// bekommt einen Fehlercode; wer Schritt 5 mit falscher Paketgroesse
// macht, bekommt gar nichts — die Uebertragung bleibt einfach stehen.
//
// ===========================================================================
// INPUT CONTEXT vs. DEVICE CONTEXT — die Unterscheidung, die man
// zuerst durcheinanderbringt
//
// Es gibt ZWEI Kontext-Strukturen, und sie gehoeren verschiedenen
// Besitzern:
//
//   * DEVICE CONTEXT — gehoert dem CONTROLLER. Er schreibt hinein, wir
//     lesen (Slot-Zustand, USB-Adresse). Seine Adresse steht in der
//     DCBAA.
//   * INPUT CONTEXT — gehoert UNS. Wir schreiben hinein, was wir
//     wollen, und uebergeben ihn EINEM Kommando. Er hat einen
//     zusaetzlichen Kopf (Drop/Add Flags), den der Device Context nicht
//     hat — deshalb ist er um genau einen Kontext-Block groesser.
//
// **Und beide sind 32 ODER 64 Byte je Block**, je nach CSZ-Bit
// (docs/xhci.md, Fallgrube 2). Das ist der Grund, warum hier nirgends
// eine feste 32 steht.

use super::register::*;
use super::{phys_von, seiten_holen, Controller, XhciFehler};
use crate::serial_println;
use crate::usb::deskriptor::{self, Endpunkt, Uebertragung};
use crate::zeit;
use alloc::string::String;
use alloc::vec::Vec;
use x86_64::{PhysAddr, VirtAddr};

// ---------------------------------------------------------------------------
// KONSTANTEN
// ---------------------------------------------------------------------------

/// TRB-Typen, die wir absetzen.
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_SETUP_STAGE: u32 = 2;
const TRB_DATA_STAGE: u32 = 3;
const TRB_STATUS_STAGE: u32 = 4;

/// Wie viele TRBs ein Transfer Ring hat — **einschliesslich des
/// Link-TRBs am Ende**. Nutzbar sind also `TRANSFER_RING_TRBS - 1`.
const TRANSFER_RING_TRBS: u32 = 64;

/// Der TRB-Typ „Link".
const TRB_LINK: u32 = 6;

// ===========================================================================
// DIE FRISTEN — und warum sie GROSSZUEGIG BLEIBEN MUESSEN
//
// Hier standen einmal 1 000 000 us (eine Sekunde) je Kommando. Das war
// „grosszuegig gegen Haenger" gedacht und hat auf dem Laptop das
// GEGENTEIL bewirkt: das Einfrieren des ganzen Systems.
//
// Der Grund ist der AUFRUFPFAD. `usb_task` ruft
// `port_wechsel_behandeln` -> `geraet_aufzaehlen`, und zwar
//
//   * INNERHALB des Controller-Locks,
//   * im kooperativen Executor (PID 0), also OHNE await dazwischen.
//
// Jede Sekunde, die hier gespinnt wird, ist eine Sekunde, in der KEIN
// anderer Kernel-Task laeuft: kein Compositor, kein Eingabe-Router,
// keine Maus. Eine Aufzaehlung setzt ein Dutzend Kommandos ab — bei
// einem Geraet, das nicht antwortet, sind das zwölf Sekunden
// Totalstillstand. In QEMU antwortet immer alles sofort, deshalb war es
// dort unsichtbar; auf echter Hardware mit Hubs und Ports, die
// Ereignisse melden, ohne dass ein brauchbares Geraet dranhaengt, friert
// die Maschine ein.
//
// DER ERSTE VERSUCH, DAS ZU BEHEBEN, WAR FALSCH UND HAT ALLES ZERSTOERT:
// Die Fristen wurden auf 50 ms gekuerzt. Das half dem Executor — und
// liess die Aufzaehlung auf echter Hardware SCHEITERN, weil dieselben
// Fristen auch BEIM BOOTEN gelten. Ohne PS/2 hatte der Rechner danach
// ueberhaupt keine Eingabe mehr. Aus „zaeh" wurde „tot".
//
// DIE EINSICHT, die vorher fehlte: Es gibt ZWEI Aufrufpfade, und sie
// haben GEGENSAETZLICHE Anforderungen.
//
//   * BEIM BOOTEN (`starten`) laeuft noch kein Executor. Blockieren ist
//     dort voellig in Ordnung — es gibt niemanden, den man aufhaelt.
//     Hier zaehlt nur, dass langsame Hardware genug Zeit bekommt.
//   * ZUR LAUFZEIT (`usb_task`) haelt jede Millisekunde alles andere
//     an. Hier zaehlt, dass es NICHT WIEDERHOLT passiert.
//
// Deshalb bleiben die Fristen grosszuegig (Hardware braucht sie), und
// das Laufzeit-Problem wird an seiner Wurzel geloest: Eine Aufzaehlung,
// die gescheitert ist, wird NICHT WIEDERHOLT, solange sich am Port
// nichts aendert (siehe `fehlgeschlagene_ports` in mod.rs). Ein
// haengender Port kostet damit EINE Pause — einmal, nicht alle 8 ms.
const FRIST_KOMMANDO_US: u64 = 500_000;
const FRIST_TRANSFER_US: u64 = 500_000;
const FRIST_PORT_RESET_US: u64 = 500_000;

/// Wie viele Aufzaehlungen HOECHSTENS je Poll-Durchgang laufen.
///
/// Auch mit kurzen Fristen darf ein Schwall von Port-Ereignissen nicht
/// zu einer Kette von Aufzaehlungen in EINEM Durchgang werden — sonst
/// summieren sich die Fristen wieder zu einer spuerbaren Pause. Der
/// Rest wartet auf den naechsten Durchgang (8 ms spaeter).
pub const MAX_AUFZAEHLUNGEN_JE_DURCHGANG: usize = 1;

/// Groesse eines Puffers fuer Deskriptor-Antworten.
///
/// Eine Konfigurations-Kette ist selten ueber 200 Byte. 512 laesst Luft
/// und deckelt zugleich, was ein Geraet uns unterschieben kann — die
/// Laenge steht ohnehin in JEDER Anfrage, das Geraet kann nicht mehr
/// schicken, als wir anfordern.
const ANTWORT_BYTES: usize = 512;

// ---------------------------------------------------------------------------
// EIN GERAET, WIE DER CONTROLLER ES SIEHT
// ---------------------------------------------------------------------------

/// Die Ressourcen EINES Geraets.
///
/// **Alles, was beim Abziehen freigegeben werden muss, steht hier drin.**
/// Das ist Absicht: Ein Leck entsteht dort, wo Besitz verstreut liegt.
pub struct SlotRessourcen {
    pub slot: u8,
    pub port: u8,
    /// Der Device Context (gehoert dem Controller, liegt in unserem RAM).
    pub geraete_kontext: VirtAddr,
    /// Der Input Context (gehoert uns).
    pub eingabe_kontext: VirtAddr,
    /// Der Transfer Ring von EP0 (Control).
    pub ep0_ring: VirtAddr,
    pub ep0_ring_phys: PhysAddr,
    pub ep0_stand: RingStand,
    /// Transfer Ringe weiterer Endpunkte: (DCI, virt, phys, Stand).
    pub endpunkt_ringe: Vec<(u8, VirtAddr, PhysAddr, RingStand)>,
    /// Der Antwortpuffer fuer Control-Transfers.
    pub antwort: VirtAddr,
    pub antwort_phys: PhysAddr,
    /// Alle Seiten, die zu diesem Slot gehoeren — die Freigabeliste.
    pub seiten: Vec<VirtAddr>,

    /// Was fuer ein HID-Geraet das ist (falls es eins ist).
    pub hid_art: Option<crate::usb::hid::HidArt>,
    /// Der DCI des Interrupt-IN-Endpunkts, auf dem die Reports kommen.
    pub hid_dci: u8,
    /// Der Puffer, in den der Controller die Reports schreibt.
    pub hid_puffer: VirtAddr,
    pub hid_puffer_phys: PhysAddr,
    /// Wie viele Byte ein Report hat.
    pub hid_bytes: u16,
    /// Der Tastatur-Zustand (Dauerfeuer, letzte Tasten).
    pub tastatur: crate::usb::hid::TastaturZustand,
}

// ---------------------------------------------------------------------------
// KONTEXT-HILFEN
// ---------------------------------------------------------------------------

impl Controller {
    /// Die Groesse EINES Kontext-Blocks: 32 oder 64 Byte.
    pub(super) fn kontext_bytes(&self) -> u64 {
        if self.kontext_64byte {
            64
        } else {
            32
        }
    }

    /// Ein 32-Bit-Wort in einem Kontext setzen.
    ///
    /// `block` ist der Kontext-Index (0 = Slot bzw. Control-Kopf),
    /// `wort` der Index innerhalb des Blocks.
    ///
    /// # Safety
    /// `basis` muss auf einen von uns allozierten Kontext zeigen, der
    /// gross genug fuer `block` ist.
    unsafe fn kontext_setzen(&self, basis: VirtAddr, block: u64, wort: u64, wert: u32) {
        let adresse = basis.as_u64() + block * self.kontext_bytes() + wort * 4;
        core::ptr::write_volatile(adresse as *mut u32, wert);
    }

    /// Ein 32-Bit-Wort aus einem Kontext lesen.
    ///
    /// # Safety
    /// wie `kontext_setzen`.
    unsafe fn kontext_lesen(&self, basis: VirtAddr, block: u64, wort: u64) -> u32 {
        let adresse = basis.as_u64() + block * self.kontext_bytes() + wort * 4;
        core::ptr::read_volatile(adresse as *const u32)
    }
}

// ---------------------------------------------------------------------------
// DER KOMMANDO-RING
// ---------------------------------------------------------------------------

/// Was ein Kommando ergeben hat.
#[derive(Debug, Clone, Copy)]
pub struct KommandoErgebnis {
    /// Der Completion Code. **1 = Erfolg**, alles andere ist ein Fehler.
    pub code: u8,
    /// Bei Enable Slot: die vergebene Slot-Nummer.
    pub slot: u8,
}

impl Controller {
    /// Ein Kommando absetzen und auf sein Completion-Event warten.
    ///
    /// ===================================================================
    /// DER ABLAUF IST IMMER DERSELBE, UND DIE REIHENFOLGE IST FEST
    ///
    ///   1. TRB an die aktuelle Ringstelle schreiben — **Cycle-Bit
    ///      ZULETZT**. Der Controller darf das TRB erst sehen, wenn es
    ///      VOLLSTAENDIG dasteht; das Cycle-Bit ist die Freigabe.
    ///   2. Doorbell 0 laeuten (der Kommando-Doorbell).
    ///   3. Auf ein Command Completion Event warten — mit Frist.
    ///
    /// Der Ring wird bewusst NICHT als voller Ring benutzt: Wir setzen
    /// immer nur EIN Kommando ab und warten darauf. Mehrere gleichzeitig
    /// waeren schneller und brauchten eine Zuordnung Event->Kommando
    /// ueber die TRB-Adresse. Solange die Aufzaehlung der einzige
    /// Kommando-Nutzer ist, waere das Aufwand ohne Gewinn.
    pub(super) fn kommando(
        &mut self,
        wort0: u32,
        wort1: u32,
        wort2: u32,
        typ: u32,
        zusatz3: u32,
    ) -> Result<KommandoErgebnis, XhciFehler> {
        let index = self.cmd_stand.index;
        let cycle = self.cmd_stand.cycle;
        let trb_phys = self.cmd_phys.as_u64() + self.cmd_stand.versatz();

        // SAFETY: `index` < RING_EINTRAEGE, der Ring ist eine von uns
        // allozierte Seite mit RING_EINTRAEGE * 16 Byte.
        unsafe {
            let z = (self.cmd_virt.as_u64() + index as u64 * TRB_BYTES as u64) as *mut u32;
            core::ptr::write_volatile(z, wort0);
            core::ptr::write_volatile(z.add(1), wort1);
            core::ptr::write_volatile(z.add(2), wort2);
            // DAS CYCLE-BIT ZULETZT — es ist die Freigabe (siehe oben).
            let wort3 = (typ << 10) | zusatz3 | if cycle { 1 } else { 0 };
            core::ptr::write_volatile(z.add(3), wort3);
        }
        self.cmd_stand.weiter();
        // Beim Umlauf braeuchte es ein Link-TRB. Wir setzen so wenige
        // Kommandos ab, dass der Ring nie umlaeuft — aber wenn doch,
        // soll es AUFFALLEN statt still falsch zu werden.
        if self.cmd_stand.index == 0 {
            serial_println!(
                "[xhci] WARNUNG: Kommando-Ring ist umgelaufen — es fehlt ein Link-TRB."
            );
        }

        self.doorbell_laeuten(0, 0);
        self.auf_kommando_warten(trb_phys)
    }

    /// Auf das Command Completion Event zu `trb_phys` warten.
    fn auf_kommando_warten(&mut self, trb_phys: u64) -> Result<KommandoErgebnis, XhciFehler> {
        let start = zeit::us_seit_boot();
        loop {
            if let Some(ergebnis) = self.events_durchsehen(Some(trb_phys))? {
                return Ok(ergebnis);
            }
            if zeit::us_seit_boot().saturating_sub(start) > FRIST_KOMMANDO_US {
                serial_println!("[xhci] FEHLER: Kommando ohne Antwort (Frist abgelaufen).");
                return Err(XhciFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }
    }

    /// Die Doorbell laeuten.
    ///
    /// Slot 0 ist der KOMMANDO-Doorbell; Slot n gehoert dem Geraet in
    /// Slot n, und `ziel` ist dort der DCI des Endpunkts.
    pub(super) fn doorbell_laeuten(&self, slot: u8, ziel: u32) {
        // SAFETY: `doorbell` liegt im gemappten MMIO-Bereich; je Slot
        // ein 32-Bit-Register, slot <= max_slots.
        unsafe {
            self.doorbell.schreibe32(slot as u64 * 4, ziel);
        }
    }
}

// ---------------------------------------------------------------------------
// DIE AUFZAEHLUNG
// ---------------------------------------------------------------------------

impl Controller {
    /// Einen Port zuruecksetzen — **danach erst hat er eine
    /// Geschwindigkeit**.
    ///
    /// Vor dem Reset meldet `PORTSC` zwar „angeschlossen", aber das
    /// Tempo-Feld ist bedeutungslos. Wer ohne Reset weitermacht,
    /// programmiert den Slot mit Tempo 0 — und der Controller lehnt
    /// `Address Device` ab.
    pub(super) fn port_zuruecksetzen(&mut self, port: u8) -> Result<(), XhciFehler> {
        let versatz = super::OP_PORTSC_BASIS + (port as u64 - 1) * super::OP_PORT_ABSTAND;
        // SAFETY: port ist 1..=max_ports, geprueft beim Aufrufer.
        let roh = unsafe { self.op.lese32(versatz) };
        // PR (Bit 4) setzen — und dabei die write-1-to-clear-Bits
        // schonen, sonst quittieren wir versehentlich fremde Meldungen
        // und schalten ueber PED den Port ab.
        let wert = (roh & !PORTSC_NICHT_ANFASSEN) | (1 << 4);
        // SAFETY: wie oben.
        unsafe {
            self.op.schreibe32(versatz, wert);
        }
        let start = zeit::us_seit_boot();
        loop {
            // SAFETY: wie oben.
            let jetzt = unsafe { self.op.lese32(versatz) };
            let z = portsc_lesen(jetzt);
            // Fertig ist der Reset, wenn PR gefallen UND PRC gesetzt ist.
            if !z.reset_laeuft && z.aenderung_reset {
                // PRC quittieren (nur dieses Bit).
                // SAFETY: wie oben.
                unsafe {
                    self.op
                        .schreibe32(versatz, portsc_quittierung(jetzt, 1 << 21));
                }
                if !z.aktiviert {
                    serial_println!("[xhci]   Port {} nach Reset NICHT aktiviert.", port);
                    return Err(XhciFehler::PortNichtBereit);
                }
                serial_println!(
                    "[xhci]   Port {} zurueckgesetzt, aktiviert, Tempo {}",
                    port,
                    z.tempo.text()
                );
                return Ok(());
            }
            if zeit::us_seit_boot().saturating_sub(start) > FRIST_PORT_RESET_US {
                serial_println!("[xhci]   Port {} Reset: Frist abgelaufen.", port);
                return Err(XhciFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }
    }

    /// Ein Geraet an `port` vollstaendig aufzaehlen.
    ///
    /// Liefert den Slot. Bei jedem Fehler werden die schon belegten
    /// Ressourcen wieder freigegeben — **kein halb aufgezaehltes Geraet
    /// bleibt liegen.**
    pub fn geraet_aufzaehlen(&mut self, port: u8) -> Result<u8, XhciFehler> {
        let (frei_vorher, _) = crate::memory::frame_statistik();
        serial_println!(
            "[xhci] Aufzaehlung an Port {} beginnt (Frames frei: {}).",
            port,
            frei_vorher
        );

        // --- 1. Port Reset -------------------------------------------
        self.port_zuruecksetzen(port)?;
        let tempo = {
            let versatz = super::OP_PORTSC_BASIS + (port as u64 - 1) * super::OP_PORT_ABSTAND;
            // SAFETY: port geprueft.
            portsc_lesen(unsafe { self.op.lese32(versatz) }).tempo
        };

        // --- 2. Enable Slot ------------------------------------------
        let ergebnis = self.kommando(0, 0, 0, TRB_ENABLE_SLOT, 0)?;
        if ergebnis.code != 1 {
            serial_println!("[xhci]   Enable Slot fehlgeschlagen (Code {}).", ergebnis.code);
            return Err(XhciFehler::KommandoFehlgeschlagen);
        }
        let slot = ergebnis.slot;
        serial_println!("[xhci]   Slot {} zugeteilt.", slot);

        // --- 3. Kontexte und Ringe anlegen ---------------------------
        let mut res = match self.slot_speicher_anlegen(slot, port) {
            Some(r) => r,
            None => {
                let _ = self.kommando(0, 0, 0, TRB_DISABLE_SLOT, (slot as u32) << 24);
                return Err(XhciFehler::KeinSpeicher);
            }
        };

        // Der eigentliche Ablauf in einer eigenen Funktion, damit ein
        // Fehler an JEDER Stelle durch denselben Aufraeumpfad laeuft.
        match self.aufzaehlung_durchfuehren(&mut res, tempo) {
            Ok(eintrag) => {
                crate::usb::geraet::anmelden(eintrag);
                self.slots.push(res);
                Ok(slot)
            }
            Err(fehler) => {
                serial_println!(
                    "[xhci]   Aufzaehlung fehlgeschlagen ({}) — raeume Slot {} ab.",
                    fehler.text(),
                    slot
                );
                self.slot_freigeben_intern(res);
                let _ = self.kommando(0, 0, 0, TRB_DISABLE_SLOT, (slot as u32) << 24);
                Err(fehler)
            }
        }
    }

    /// Speicher fuer einen Slot: Kontexte, EP0-Ring, Antwortpuffer.
    fn slot_speicher_anlegen(&mut self, slot: u8, port: u8) -> Option<SlotRessourcen> {
        let mut seiten = Vec::new();
        let geraete_kontext = seiten_holen(1)?;
        seiten.push(geraete_kontext);
        let eingabe_kontext = seiten_holen(1)?;
        seiten.push(eingabe_kontext);
        let ep0_ring = seiten_holen(1)?;
        seiten.push(ep0_ring);
        let antwort = seiten_holen(1)?;
        seiten.push(antwort);

        let ep0_ring_phys = phys_von(ep0_ring)?;
        let antwort_phys = phys_von(antwort)?;
        let geraete_phys = phys_von(geraete_kontext)?;

        // Den Device Context in die DCBAA eintragen — Index = Slot.
        // SAFETY: dcbaa_virt ist eine von uns allozierte Seite,
        // slot <= max_slots <= 255, also < 512 Eintraege je Seite.
        unsafe {
            let eintrag = (self.dcbaa_virt.as_u64() as *mut u64).add(slot as usize);
            core::ptr::write_volatile(eintrag, geraete_phys.as_u64());
        }

        Some(SlotRessourcen {
            slot,
            port,
            geraete_kontext,
            eingabe_kontext,
            ep0_ring,
            ep0_ring_phys,
            ep0_stand: RingStand::neu(TRANSFER_RING_TRBS - 1),
            endpunkt_ringe: Vec::new(),
            antwort,
            antwort_phys,
            seiten,
            hid_art: None,
            hid_dci: 0,
            hid_puffer: VirtAddr::new(0),
            hid_puffer_phys: PhysAddr::new(0),
            hid_bytes: 0,
            tastatur: crate::usb::hid::TastaturZustand::default(),
        })
    }

    /// Der Kern: Address Device, Deskriptoren, Set Configuration.
    fn aufzaehlung_durchfuehren(
        &mut self,
        res: &mut SlotRessourcen,
        tempo: Tempo,
    ) -> Result<crate::usb::geraet::GeraeteEintrag, XhciFehler> {
        // --- 4. Address Device ---------------------------------------
        //
        // Der Input Context bekommt zwei Eintraege: den Slot-Kontext
        // (Block 1) und den EP0-Kontext (Block 2). Block 0 ist der
        // Control-Kopf mit den Add-Flags.
        self.eingabe_kontext_leeren(res);
        // Add-Flags: Bit 0 = Slot, Bit 1 = EP0 (DCI 1).
        // SAFETY: eingabe_kontext ist eine von uns allozierte Seite.
        unsafe {
            self.kontext_setzen(res.eingabe_kontext, 0, 1, 0b11);
        }
        // Slot-Kontext (Block 1): Route 0, Tempo, Kontext-Eintraege 1,
        // Root-Hub-Port.
        let tempo_feld = tempo_nummer(tempo) as u32;
        // SAFETY: wie oben.
        unsafe {
            self.kontext_setzen(
                res.eingabe_kontext,
                1,
                0,
                (1u32 << 27) | (tempo_feld << 20),
            );
            self.kontext_setzen(res.eingabe_kontext, 1, 1, (res.port as u32) << 16);
        }
        // EP0-Kontext (Block 2): Control-Endpunkt, 3 Fehlversuche,
        // Paketgroesse nach Tempo, Dequeue-Zeiger auf den EP0-Ring.
        let paket0 = standard_paket0(tempo);
        // SAFETY: wie oben.
        unsafe {
            self.kontext_setzen(res.eingabe_kontext, 2, 1, (4u32 << 3) | (3 << 1) | ((paket0 as u32) << 16));
            self.kontext_setzen(res.eingabe_kontext, 2, 2, (res.ep0_ring_phys.as_u64() as u32) | 1);
            self.kontext_setzen(res.eingabe_kontext, 2, 3, (res.ep0_ring_phys.as_u64() >> 32) as u32);
            // Average TRB Length — der Controller plant damit Bandbreite.
            self.kontext_setzen(res.eingabe_kontext, 2, 4, 8);
        }

        let eingabe_phys = phys_von(res.eingabe_kontext).ok_or(XhciFehler::KeinSpeicher)?;
        let ergebnis = self.kommando(
            eingabe_phys.as_u64() as u32,
            (eingabe_phys.as_u64() >> 32) as u32,
            0,
            TRB_ADDRESS_DEVICE,
            (res.slot as u32) << 24,
        )?;
        if ergebnis.code != 1 {
            serial_println!("[xhci]   Address Device fehlgeschlagen (Code {}).", ergebnis.code);
            return Err(XhciFehler::KommandoFehlgeschlagen);
        }
        // Die vergebene Adresse steht im DEVICE Context (Block 1, Wort 3).
        // SAFETY: geraete_kontext ist eine von uns allozierte Seite.
        // BLOCK 0, NICHT BLOCK 1. Im DEVICE Context ist Block 0 der
        // Slot-Kontext; nur im INPUT Context schiebt der Control-Kopf
        // alles um eins nach hinten. Die erste Fassung las Block 1 und
        // bekam deshalb immer 0 — die Verwechslung, vor der der
        // Kopfkommentar warnt.
        let adresse = unsafe { (self.kontext_lesen(res.geraete_kontext, 0, 3) & 0xFF) as u8 };
        serial_println!("[xhci]   Address Device ok, USB-Adresse {}.", adresse);

        // --- 5. Deskriptoren -----------------------------------------
        let roh = self.control_lesen(res, 0x80, 6, (deskriptor::TYP_DEVICE as u16) << 8, 0, 18)?;
        let geraet = deskriptor::geraet_parsen(&roh).map_err(|f| {
            serial_println!("[xhci]   Device-Deskriptor unbrauchbar: {}", f.text());
            XhciFehler::DeskriptorKaputt
        })?;
        serial_println!(
            "[xhci]   Geraet {:04x}:{:04x}, USB {:x}.{:02x}, Klasse {}, EP0-Paket {}",
            geraet.hersteller_id,
            geraet.produkt_id,
            geraet.usb_version >> 8,
            geraet.usb_version & 0xFF,
            geraet.klasse,
            geraet.max_paket0
        );

        // Die Konfiguration: erst neun Byte holen, um `wTotalLength` zu
        // erfahren, dann die ganze Kette. Ein Geraet, das beim zweiten
        // Mal mehr schickt, bekommt trotzdem nur, was wir angefordert
        // haben — die Laenge steht in der Anfrage.
        let kopf = self.control_lesen(res, 0x80, 6, (deskriptor::TYP_CONFIGURATION as u16) << 8, 0, 9)?;
        if kopf.len() < 4 {
            return Err(XhciFehler::DeskriptorKaputt);
        }
        let gesamt = u16::from_le_bytes([kopf[2], kopf[3]]).min(ANTWORT_BYTES as u16);
        let voll = self.control_lesen(
            res,
            0x80,
            6,
            (deskriptor::TYP_CONFIGURATION as u16) << 8,
            0,
            gesamt,
        )?;
        let konfiguration = deskriptor::konfiguration_parsen(&voll).map_err(|f| {
            serial_println!("[xhci]   Konfiguration unbrauchbar: {}", f.text());
            XhciFehler::DeskriptorKaputt
        })?;
        serial_println!(
            "[xhci]   Konfiguration {}: {} Interface(s), {} Byte, {} uebersprungen{}",
            konfiguration.wert,
            konfiguration.schnittstellen.len(),
            voll.len(),
            konfiguration.befund.uebersprungen,
            if konfiguration.befund.abgebrochen {
                " (ABGEBROCHEN)"
            } else {
                ""
            }
        );

        // Strings — sie sind OPTIONAL. Ein Geraet ohne Strings ist
        // vollkommen gueltig, also darf ein Fehlschlag hier die
        // Aufzaehlung NICHT abbrechen.
        let sprache = match self.control_lesen(res, 0x80, 6, (deskriptor::TYP_STRING as u16) << 8, 0, 4) {
            Ok(d) => deskriptor::erste_sprache(&d),
            Err(_) => 0x0409,
        };
        let hersteller = self.string_holen(res, geraet.index_hersteller, sprache);
        let produkt = self.string_holen(res, geraet.index_produkt, sprache);
        if !hersteller.is_empty() || !produkt.is_empty() {
            serial_println!("[xhci]   \"{}\" / \"{}\"", hersteller, produkt);
        }

        // --- 6. Set Configuration ------------------------------------
        self.control_schreiben(res, 0x00, 9, konfiguration.wert as u16, 0)?;
        serial_println!("[xhci]   Set Configuration {} ok.", konfiguration.wert);

        // --- 7. Endpunkte konfigurieren ------------------------------
        self.endpunkte_konfigurieren(res, &konfiguration)?;

        // --- 8. HID: Boot Protocol setzen und lauschen ---------------
        //
        // ERST JETZT, nachdem die Endpunkte stehen — ein `Set Protocol`
        // vor `Configure Endpoint` waere an einem Geraet gerichtet, das
        // noch keinen Endpunkt hat.
        self.hid_einrichten(res, &konfiguration);

        Ok(crate::usb::geraet::GeraeteEintrag {
            slot: res.slot,
            port: res.port,
            adresse,
            geraet,
            konfiguration,
            hersteller,
            produkt,
            tempo: tempo.text(),
        })
    }

    /// Den Input Context mit Nullen ueberschreiben.
    ///
    /// **Pflicht vor jedem Kommando.** Ein Rest aus dem vorigen
    /// Gebrauch waere ein Add-Flag, das wir nicht gesetzt haben — der
    /// Controller konfiguriert dann einen Endpunkt aus Muell.
    fn eingabe_kontext_leeren(&self, res: &SlotRessourcen) {
        // SAFETY: eingabe_kontext ist eine von uns allozierte 4-KiB-Seite.
        unsafe {
            core::ptr::write_bytes(res.eingabe_kontext.as_u64() as *mut u8, 0, 4096);
        }
    }

    /// Einen String-Deskriptor holen. Leerer String, wenn es ihn nicht
    /// gibt oder er unbrauchbar ist — **nie ein Fehler**.
    fn string_holen(&mut self, res: &mut SlotRessourcen, index: u8, sprache: u16) -> String {
        if index == 0 {
            return String::new();
        }
        match self.control_lesen(
            res,
            0x80,
            6,
            ((deskriptor::TYP_STRING as u16) << 8) | index as u16,
            sprache,
            255,
        ) {
            Ok(daten) => deskriptor::string_parsen(&daten).unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// Die Interrupt-IN-Endpunkte einrichten.
    ///
    /// ===================================================================
    /// WARUM NUR INTERRUPT-IN
    ///
    /// Das ist, was Tastatur und Maus brauchen — und dieser Schritt
    /// endet vor dem Massenspeicher. Bulk-Endpunkte anzulegen waere
    /// Code ohne Benutzer; wenn der Massenspeicher-Treiber kommt,
    /// erweitert er diese eine Stelle.
    fn endpunkte_konfigurieren(
        &mut self,
        res: &mut SlotRessourcen,
        konfiguration: &deskriptor::Konfiguration,
    ) -> Result<(), XhciFehler> {
        let interessant: Vec<Endpunkt> = konfiguration
            .schnittstellen
            .iter()
            .flat_map(|s| s.endpunkte.iter())
            .filter(|e| e.art == Uebertragung::Interrupt && e.ist_eingang())
            .copied()
            .collect();
        if interessant.is_empty() {
            return Ok(());
        }

        self.eingabe_kontext_leeren(res);
        let mut add_flags: u32 = 1; // Bit 0 = Slot-Kontext gehoert immer dazu
        let mut hoechster_dci = 1u8;

        for endpunkt in &interessant {
            let dci = endpunkt.dci();
            if dci as usize >= 32 {
                continue;
            }
            let ring = match seiten_holen(1) {
                Some(r) => r,
                None => return Err(XhciFehler::KeinSpeicher),
            };
            let ring_phys = phys_von(ring).ok_or(XhciFehler::KeinSpeicher)?;
            res.seiten.push(ring);
            res.endpunkt_ringe
                .push((dci, ring, ring_phys, RingStand::neu(TRANSFER_RING_TRBS - 1)));

            add_flags |= 1 << dci;
            hoechster_dci = hoechster_dci.max(dci);

            // Der Endpunkt-Kontext liegt bei Block (DCI + 1) — Block 0
            // ist der Control-Kopf, Block 1 der Slot-Kontext.
            let block = dci as u64 + 1;
            // EP Type 7 = Interrupt IN. Interval als Rohwert.
            // SAFETY: eingabe_kontext ist unsere Seite, block < 33.
            unsafe {
                self.kontext_setzen(res.eingabe_kontext, block, 0, (endpunkt.intervall as u32) << 16);
                self.kontext_setzen(
                    res.eingabe_kontext,
                    block,
                    1,
                    (7u32 << 3) | (3 << 1) | ((endpunkt.max_paket as u32) << 16),
                );
                self.kontext_setzen(res.eingabe_kontext, block, 2, (ring_phys.as_u64() as u32) | 1);
                self.kontext_setzen(res.eingabe_kontext, block, 3, (ring_phys.as_u64() >> 32) as u32);
                self.kontext_setzen(res.eingabe_kontext, block, 4, endpunkt.max_paket as u32);
            }
        }

        // Der Slot-Kontext muss die HOECHSTE benutzte DCI melden —
        // sonst ignoriert der Controller die Endpunkte darueber.
        // SAFETY: wie oben.
        unsafe {
            self.kontext_setzen(res.eingabe_kontext, 0, 1, add_flags);
            let alt = self.kontext_lesen(res.geraete_kontext, 0, 0);
            self.kontext_setzen(
                res.eingabe_kontext,
                1,
                0,
                (alt & 0x07FF_FFFF) | ((hoechster_dci as u32) << 27),
            );
            // Auch hier: Der Slot-Kontext des DEVICE Context liegt bei
            // Block 0, sein Gegenstueck im INPUT Context bei Block 1.
            self.kontext_setzen(
                res.eingabe_kontext,
                1,
                1,
                self.kontext_lesen(res.geraete_kontext, 0, 1),
            );
        }

        let eingabe_phys = phys_von(res.eingabe_kontext).ok_or(XhciFehler::KeinSpeicher)?;
        let ergebnis = self.kommando(
            eingabe_phys.as_u64() as u32,
            (eingabe_phys.as_u64() >> 32) as u32,
            0,
            TRB_CONFIGURE_ENDPOINT,
            (res.slot as u32) << 24,
        )?;
        if ergebnis.code != 1 {
            serial_println!(
                "[xhci]   Configure Endpoint fehlgeschlagen (Code {}).",
                ergebnis.code
            );
            return Err(XhciFehler::KommandoFehlgeschlagen);
        }
        serial_println!(
            "[xhci]   {} Interrupt-IN-Endpunkt(e) konfiguriert (hoechste DCI {}).",
            interessant.len(),
            hoechster_dci
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CONTROL-TRANSFERS
// ---------------------------------------------------------------------------

impl Controller {
    /// Ein Control-Transfer MIT Daten zum Host (IN).
    ///
    /// ===================================================================
    /// DREI TRBs, UND DAS LETZTE TRAEGT DIE MELDUNG
    ///
    /// Ein Control-Transfer besteht aus Setup Stage, (Data Stage) und
    /// Status Stage. Nur das LETZTE bekommt `IOC` (Interrupt On
    /// Completion) — wir wollen genau ein Event, nicht drei.
    ///
    /// Der Setup-TRB traegt die acht Bytes des Setup-Pakets in seinen
    /// ersten beiden Worten; `TRT` (Transfer Type) im vierten sagt, ob
    /// und wohin Daten fliessen. Wer TRT falsch setzt, bekommt einen
    /// Babble- oder Stall-Fehler statt Daten.
    fn control_lesen(
        &mut self,
        res: &mut SlotRessourcen,
        anfrage_typ: u8,
        anfrage: u8,
        wert: u16,
        index: u16,
        laenge: u16,
    ) -> Result<Vec<u8>, XhciFehler> {
        let laenge = laenge.min(ANTWORT_BYTES as u16);
        // Den Antwortpuffer leeren — sonst liest man beim naechsten Mal
        // Reste des vorigen Deskriptors, falls das Geraet weniger
        // schickt als angefordert.
        // SAFETY: `antwort` ist eine von uns allozierte 4-KiB-Seite.
        unsafe {
            core::ptr::write_bytes(res.antwort.as_u64() as *mut u8, 0, ANTWORT_BYTES);
        }

        // Setup Stage: TRT = 3 (IN Data Stage).
        let setup0 = (anfrage_typ as u32) | ((anfrage as u32) << 8) | ((wert as u32) << 16);
        let setup1 = (index as u32) | ((laenge as u32) << 16);
        self.transfer_trb(res, setup0, setup1, 8, TRB_SETUP_STAGE, (3 << 16) | (1 << 6))?;
        // Data Stage: Richtung IN (Bit 16 im vierten Wort).
        self.transfer_trb(
            res,
            res.antwort_phys.as_u64() as u32,
            (res.antwort_phys.as_u64() >> 32) as u32,
            laenge as u32,
            TRB_DATA_STAGE,
            1 << 16,
        )?;
        // Status Stage: Richtung OUT, mit IOC — hierauf warten wir.
        self.transfer_trb(res, 0, 0, 0, TRB_STATUS_STAGE, 1 << 5)?;

        self.doorbell_laeuten(res.slot, 1); // DCI 1 = EP0
        let code = self.auf_transfer_warten()?;
        if code != 1 && code != 13 {
            // 13 = Short Packet. Das ist KEIN Fehler: Das Geraet hat
            // weniger geschickt als angefordert, und genau das ist bei
            // Deskriptoren der Normalfall.
            serial_println!("[xhci]   Control-Transfer Code {}.", code);
            return Err(XhciFehler::TransferFehlgeschlagen);
        }

        // SAFETY: `antwort` ist unsere Seite, `laenge` <= ANTWORT_BYTES.
        let mut aus = Vec::with_capacity(laenge as usize);
        for i in 0..laenge as usize {
            aus.push(unsafe { core::ptr::read_volatile((res.antwort.as_u64() as *const u8).add(i)) });
        }
        Ok(aus)
    }

    /// Ein Control-Transfer OHNE Daten (z. B. Set Configuration).
    fn control_schreiben(
        &mut self,
        res: &mut SlotRessourcen,
        anfrage_typ: u8,
        anfrage: u8,
        wert: u16,
        index: u16,
    ) -> Result<(), XhciFehler> {
        let setup0 = (anfrage_typ as u32) | ((anfrage as u32) << 8) | ((wert as u32) << 16);
        let setup1 = index as u32; // wLength = 0
        // TRT = 0 (kein Data Stage).
        self.transfer_trb(res, setup0, setup1, 8, TRB_SETUP_STAGE, 1 << 6)?;
        // Status Stage: bei einem OUT-Transfer ohne Daten ist die
        // Richtung IN (Bit 16) — das ist die Stelle, an der man sich
        // vertut, weil sie der Anfrage-Richtung WIDERSPRICHT.
        self.transfer_trb(res, 0, 0, 0, TRB_STATUS_STAGE, (1 << 16) | (1 << 5))?;
        self.doorbell_laeuten(res.slot, 1);
        let code = self.auf_transfer_warten()?;
        if code != 1 {
            serial_println!("[xhci]   Control-Schreiben Code {}.", code);
            return Err(XhciFehler::TransferFehlgeschlagen);
        }
        Ok(())
    }

    /// Ein TRB in den EP0-Transfer-Ring legen.
    fn transfer_trb(
        &mut self,
        res: &mut SlotRessourcen,
        wort0: u32,
        wort1: u32,
        wort2: u32,
        typ: u32,
        zusatz3: u32,
    ) -> Result<(), XhciFehler> {
        let index = res.ep0_stand.index;
        let cycle = res.ep0_stand.cycle;
        // SAFETY: index < TRANSFER_RING_TRBS, der Ring ist eine Seite.
        unsafe {
            let z = (res.ep0_ring.as_u64() + index as u64 * TRB_BYTES as u64) as *mut u32;
            core::ptr::write_volatile(z, wort0);
            core::ptr::write_volatile(z.add(1), wort1);
            core::ptr::write_volatile(z.add(2), wort2);
            // Cycle-Bit ZULETZT — die Freigabe.
            core::ptr::write_volatile(z.add(3), (typ << 10) | zusatz3 | if cycle { 1 } else { 0 });
        }
        res.ep0_stand.weiter();
        // ===============================================================
        // DER RINGUMLAUF BRAUCHT EIN LINK-TRB — und die erste Fassung
        // hatte keins.
        //
        // Ein Transfer Ring ist ein Array; der Controller laeuft
        // stumpf vorwaerts. Was ihn an den Anfang zurueckschickt, ist
        // ein LINK-TRB im letzten Eintrag. Ohne eins liest er hinter
        // dem Ring weiter — bei 16 Eintraegen und 3 TRBs je Transfer
        // schon nach fuenf Control-Transfers, und die Aufzaehlung
        // braucht sechs. Symptom: Der Transfer bleibt einfach aus.
        //
        // Das Link-TRB traegt das TOGGLE-CYCLE-Bit (TC): Der Controller
        // kippt daran seinen eigenen Cycle-Zustand, genauso wie
        // `RingStand::weiter` unseren kippt. Damit es beim naechsten
        // Umlauf wieder passt, muss sein Cycle-Bit JEDES MAL neu
        // geschrieben werden — mit dem Zustand, den der Controller
        // erwartet, wenn er dort ankommt (also dem ALTEN).
        if res.ep0_stand.index == 0 {
            let alter_cycle = !res.ep0_stand.cycle;
            let ziel = res.ep0_ring_phys.as_u64();
            // SAFETY: der letzte Eintrag liegt innerhalb der Seite.
            unsafe {
                let z = (res.ep0_ring.as_u64()
                    + (TRANSFER_RING_TRBS as u64 - 1) * TRB_BYTES as u64)
                    as *mut u32;
                core::ptr::write_volatile(z, ziel as u32);
                core::ptr::write_volatile(z.add(1), (ziel >> 32) as u32);
                core::ptr::write_volatile(z.add(2), 0);
                // TC = Bit 1, Cycle = Bit 0.
                core::ptr::write_volatile(
                    z.add(3),
                    (TRB_LINK << 10) | (1 << 1) | if alter_cycle { 1 } else { 0 },
                );
            }
        }
        Ok(())
    }

    /// Auf ein Transfer Event warten. Liefert den Completion Code.
    fn auf_transfer_warten(&mut self) -> Result<u8, XhciFehler> {
        let start = zeit::us_seit_boot();
        loop {
            if let Some(code) = self.transfer_event_suchen()? {
                return Ok(code);
            }
            if zeit::us_seit_boot().saturating_sub(start) > FRIST_TRANSFER_US {
                serial_println!("[xhci]   Transfer ohne Antwort (Frist abgelaufen).");
                return Err(XhciFehler::Zeitueberschreitung);
            }
            core::hint::spin_loop();
        }
    }
}

// ---------------------------------------------------------------------------
// AUFRAEUMEN
// ---------------------------------------------------------------------------

impl Controller {
    /// Alle Ressourcen eines Slots freigeben.
    ///
    /// ===================================================================
    /// DER LECK-PFAD, UND WARUM ER EINE EINZIGE FUNKTION IST
    ///
    /// Beim Abziehen muessen freigegeben werden: der DCBAA-Eintrag, die
    /// beiden Kontexte, der EP0-Ring, jeder Endpunkt-Ring und der
    /// Antwortpuffer. Waeren das mehrere Stellen, wuerde eine davon
    /// vergessen — und ein USB-Leck faellt erst nach dem zwanzigsten
    /// Umstecken auf.
    ///
    /// Deshalb fuehrt `SlotRessourcen::seiten` JEDE Seite, und hier
    /// laeuft genau eine Schleife darueber.
    pub(super) fn slot_freigeben_intern(&mut self, res: SlotRessourcen) {
        // DCBAA-Eintrag nullen, BEVOR der Speicher zurueckgeht — sonst
        // zeigt der Controller kurzzeitig auf fremden Speicher.
        // SAFETY: dcbaa_virt ist unsere Seite, slot < 512.
        unsafe {
            let eintrag = (self.dcbaa_virt.as_u64() as *mut u64).add(res.slot as usize);
            core::ptr::write_volatile(eintrag, 0);
        }
        let anzahl = res.seiten.len();
        for seite in res.seiten {
            crate::memory::seiten_freigeben(seite, 1);
        }
        // DIE FRAME-BILANZ WIRD MITGEDRUCKT — sonst ist „kein Leck"
        // eine Behauptung. Beim wiederholten Ein- und Ausstecken muss
        // die Zahl der freien Frames auf denselben Wert zurueckkehren;
        // wandert sie nach unten, leckt die Aufzaehlung.
        let (frei, gesamt) = crate::memory::frame_statistik();
        serial_println!(
            "[xhci]   Slot {} abgeraeumt ({} Seiten zurueck). Frames frei: {}/{}",
            res.slot,
            anzahl,
            frei,
            gesamt
        );
    }

    /// Ein Geraet ist abgezogen worden: Slot abmelden und freigeben.
    pub fn geraet_entfernen(&mut self, slot: u8) {
        crate::usb::geraet::abmelden(slot);
        if let Some(pos) = self.slots.iter().position(|s| s.slot == slot) {
            let res = self.slots.remove(pos);
            self.slot_freigeben_intern(res);
        }
        let _ = self.kommando(0, 0, 0, TRB_DISABLE_SLOT, (slot as u32) << 24);
    }
}

// ---------------------------------------------------------------------------
// KLEINKRAM
// ---------------------------------------------------------------------------

/// Das Tempo-Feld, wie der Slot-Kontext es will.
fn tempo_nummer(tempo: Tempo) -> u8 {
    match tempo {
        Tempo::Voll => 1,
        Tempo::Niedrig => 2,
        Tempo::Hoch => 3,
        Tempo::Super => 4,
        Tempo::SuperPlus => 5,
        Tempo::Unbekannt => 3,
    }
}

/// Die EP0-Paketgroesse, bevor der Device-Deskriptor gelesen ist.
///
/// **Henne und Ei:** Die echte Groesse steht IM Device-Deskriptor, den
/// man erst lesen kann, wenn EP0 konfiguriert ist. Die Spezifikation
/// loest das mit festen Startwerten je Geschwindigkeit; sie sind fuer
/// die ersten acht Byte immer richtig, und mehr braucht man nicht, um
/// die Wahrheit zu erfahren.
fn standard_paket0(tempo: Tempo) -> u16 {
    match tempo {
        Tempo::Niedrig => 8,
        Tempo::Voll => 64,
        Tempo::Hoch => 64,
        Tempo::Super | Tempo::SuperPlus => 512,
        Tempo::Unbekannt => 64,
    }
}

// ---------------------------------------------------------------------------
// HID: LAUSCHEN AUF DEM INTERRUPT-ENDPUNKT
// ---------------------------------------------------------------------------

/// Der TRB-Typ „Normal" — ein gewoehnlicher Datentransfer.
const TRB_NORMAL: u32 = 1;

impl Controller {
    /// Ein HID-Geraet in Betrieb nehmen: Boot Protocol setzen und den
    /// ersten Transfer einstellen.
    ///
    /// **Ein Fehlschlag ist hier KEIN Grund, die Aufzaehlung
    /// abzubrechen.** Das Geraet ist erkannt und steht im Verzeichnis;
    /// dass wir seine Eingaben nicht lesen koennen, macht es nicht
    /// ungueltig. Ein Massenspeicher zum Beispiel hat gar kein
    /// Boot-Protokoll.
    fn hid_einrichten(
        &mut self,
        res: &mut SlotRessourcen,
        konfiguration: &deskriptor::Konfiguration,
    ) {
        use crate::usb::hid;
        // Das erste Interface, das wir bedienen koennen.
        let Some((schnittstelle, art)) = konfiguration
            .schnittstellen
            .iter()
            .find_map(|s| hid::art_von(s.klasse, s.unterklasse, s.protokoll).map(|a| (s, a)))
        else {
            return;
        };
        let Some(endpunkt) = schnittstelle
            .endpunkte
            .iter()
            .find(|e| e.art == Uebertragung::Interrupt && e.ist_eingang())
        else {
            return;
        };

        // SET PROTOCOL (BOOT). Anfrage 0x0B, Wert 0 = Boot, an das
        // Interface (Empfaenger 1 im bmRequestType).
        //
        // Manche Geraete melden `STALL` darauf und sind trotzdem im
        // Boot-Modus (sie koennen nichts anderes). Deshalb wird der
        // Fehler nur PROTOKOLLIERT und nicht weitergereicht.
        if self
            .control_schreiben(res, 0x21, 0x0B, 0, schnittstelle.nummer as u16)
            .is_err()
        {
            serial_println!(
                "[xhci]   Set Protocol(Boot) abgelehnt — viele Geraete koennen ohnehin nur Boot."
            );
        }

        let puffer = match seiten_holen(1) {
            Some(p) => p,
            None => return,
        };
        let puffer_phys = match phys_von(puffer) {
            Some(p) => p,
            None => return,
        };
        res.seiten.push(puffer);
        res.hid_art = Some(art);
        res.hid_dci = endpunkt.dci();
        res.hid_puffer = puffer;
        res.hid_puffer_phys = puffer_phys;
        res.hid_bytes = endpunkt.max_paket.min(64);

        serial_println!(
            "[xhci]   HID {:?}: lausche auf DCI {} ({} Byte je Report).",
            art,
            res.hid_dci,
            res.hid_bytes
        );
        self.hid_transfer_einstellen(res);
    }

    /// Einen Normal-TRB auf dem Interrupt-Endpunkt einstellen.
    ///
    /// ===================================================================
    /// EIN TRANSFER JE REPORT — UND DER NAECHSTE WIRD SOFORT NACHGELEGT
    ///
    /// Ein Interrupt-Endpunkt liefert nicht von selbst: Der Controller
    /// fragt das Geraet nur ab, solange ein TRB im Ring steht. Ist er
    /// abgearbeitet, ist Ruhe — bis wir den naechsten einstellen.
    ///
    /// Das ist der Unterschied zu einem IRQ-Geraet, und es ist die
    /// Stelle, an der eine Tastatur nach dem ersten Tastendruck
    /// verstummt, wenn man es vergisst.
    pub(super) fn hid_transfer_einstellen(&mut self, res: &mut SlotRessourcen) {
        if res.hid_dci == 0 {
            return;
        }
        let Some((_, ring, ring_phys, stand)) = res
            .endpunkt_ringe
            .iter_mut()
            .find(|(dci, _, _, _)| *dci == res.hid_dci)
        else {
            return;
        };
        let index = stand.index;
        let cycle = stand.cycle;
        let ziel = res.hid_puffer_phys.as_u64();
        // SAFETY: index < nutzbare Ringgroesse, der Ring ist eine Seite.
        unsafe {
            let z = (ring.as_u64() + index as u64 * TRB_BYTES as u64) as *mut u32;
            core::ptr::write_volatile(z, ziel as u32);
            core::ptr::write_volatile(z.add(1), (ziel >> 32) as u32);
            // Wort 2: Transferlaenge.
            core::ptr::write_volatile(z.add(2), res.hid_bytes as u32);
            // IOC (Bit 5) — wir wollen ein Event, wenn der Report da ist.
            core::ptr::write_volatile(
                z.add(3),
                (TRB_NORMAL << 10) | (1 << 5) | if cycle { 1 } else { 0 },
            );
        }
        stand.weiter();
        // Link-TRB beim Umlauf — dieselbe Mechanik wie beim EP0-Ring.
        if stand.index == 0 {
            let alter_cycle = !stand.cycle;
            let ringanfang = ring_phys.as_u64();
            // SAFETY: der letzte Eintrag liegt innerhalb der Seite.
            unsafe {
                let z = (ring.as_u64() + (TRANSFER_RING_TRBS as u64 - 1) * TRB_BYTES as u64)
                    as *mut u32;
                core::ptr::write_volatile(z, ringanfang as u32);
                core::ptr::write_volatile(z.add(1), (ringanfang >> 32) as u32);
                core::ptr::write_volatile(z.add(2), 0);
                core::ptr::write_volatile(
                    z.add(3),
                    (TRB_LINK << 10) | (1 << 1) | if alter_cycle { 1 } else { 0 },
                );
            }
        }
        let slot = res.slot;
        let dci = res.hid_dci as u32;
        self.doorbell_laeuten(slot, dci);
    }

    /// Ein fertiger HID-Transfer: Report auswerten und neu einstellen.
    ///
    /// **HIER ENDET DER USB-WEG UND BEGINNT DER GEWOEHNLICHE.** Die
    /// Bytes gehen in dieselben lock-freien Queues, die der
    /// PS/2-Interrupt speist — danach ist nicht mehr zu erkennen,
    /// woher sie kamen.
    pub fn hid_report_verarbeiten(&mut self, slot: u8, dci: u8) {
        use crate::usb::hid;
        let Some(pos) = self.slots.iter().position(|s| s.slot == slot) else {
            return;
        };
        if self.slots[pos].hid_dci != dci {
            return;
        }
        let jetzt = crate::zeit::ms_seit_boot();
        let bytes = self.slots[pos].hid_bytes as usize;
        let puffer = self.slots[pos].hid_puffer;
        // SAFETY: `puffer` ist eine von uns allozierte Seite,
        // `bytes` <= 64.
        let daten: Vec<u8> = (0..bytes)
            .map(|i| unsafe { core::ptr::read_volatile((puffer.as_u64() as *const u8).add(i)) })
            .collect();

        match self.slots[pos].hid_art {
            Some(hid::HidArt::Tastatur) => {
                let codes = self.slots[pos].tastatur.report(&daten, jetzt);
                for code in codes {
                    scancode_einspeisen(code);
                }
            }
            Some(hid::HidArt::Maus) => {
                if let Some(b) = hid::maus_bytes(&daten) {
                    // Vier Bytes wie ein IntelliMouse-Paket.
                    for byte in b {
                        crate::maus::byte_hinzufuegen(byte);
                    }
                }
            }
            None => {}
        }

        // DEN NAECHSTEN TRANSFER NACHLEGEN — sonst verstummt das Geraet.
        let mut res = self.slots.swap_remove(pos);
        self.hid_transfer_einstellen(&mut res);
        self.slots.push(res);
    }

    /// Faelliges Dauerfeuer aller Tastaturen erzeugen.
    ///
    /// Laeuft im Poll-Task, nicht im Report-Pfad: Solange eine Taste
    /// GEHALTEN wird, meldet das Geraet denselben Zustand — es gibt
    /// also kein Ereignis, an das man die Wiederholung haengen koennte.
    pub fn hid_wiederholungen(&mut self) {
        use crate::usb::hid;
        let jetzt = crate::zeit::ms_seit_boot();
        // DER RUECKSTAU. Steht noch unverarbeitete Eingabe an, wird
        // nichts nachgelegt — siehe `TastaturZustand::wiederholung`.
        let stau = crate::task::keyboard::wartende_scancodes();
        for res in self.slots.iter_mut() {
            if res.hid_art != Some(hid::HidArt::Tastatur) {
                continue;
            }
            if let Some(code) = res.tastatur.wiederholung(jetzt, stau) {
                scancode_einspeisen(code);
            }
        }
    }
}

/// Einen Scancode in die GEMEINSAME Tastatur-Queue legen.
///
/// Dieselbe Funktion, die der PS/2-Interrupt-Handler benutzt. Genau
/// deshalb gibt es hier keine Layout-Logik: Was danach passiert
/// (QWERTZ, Modifier, Strg+C), ist fuer beide Wege identisch.
fn scancode_einspeisen(code: crate::usb::hid::Scancode) {
    let (bytes, anzahl) = code.bytes();
    for byte in bytes.iter().take(anzahl) {
        crate::task::keyboard::add_scancode(*byte);
    }
}
