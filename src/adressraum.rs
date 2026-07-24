// adressraum.rs — Pro-Prozess-Adressräume (Serie 6, Teil 2)
//
// DER zweite historische Schritt nach Ring 3: Bis hierhin lief User-Code zwar
// unprivilegiert, aber in DENSELBEN Page Tables wie der Kernel — er hätte jede
// Kernel-Adresse wenigstens ANSPRECHEN können (nur das U-Bit hat ihn gestoppt),
// und zwei User-Programme hätten sich gegenseitig gesehen. Ab jetzt bekommt
// jeder Prozess eine EIGENE Level-4-Tabelle: Was er nicht in SEINEN Tabellen
// stehen hat, EXISTIERT für ihn schlicht nicht. Das ist echte Isolation.
//
// ==========================================================================
// DAS GRUNDPRINZIP: "Kernel spiegeln, User privat"
//
// Ein Adressraum ist eine Level-4-Tabelle mit 512 Einträgen; jeder Eintrag
// deckt 512 GiB virtuellen Adressraum ab (Slot i => VA-Basis i << 39).
// Beim Anlegen eines neuen Adressraums KOPIEREN wir die Kernel-Einträge
// hinein (nicht die Tabellen selbst — nur die 8-Byte-Zeiger auf sie!).
// Folge: Kernel-Code, Kernel-Stack, Heap und das Physik-Komplettmapping
// liegen in JEDEM Adressraum an derselben Adresse. Genau das MUSS so sein:
// Ein Interrupt kann jederzeit zuschlagen, während User-Code läuft — die CPU
// springt dann in den Kernel-Handler, OHNE CR3 zu wechseln. Fehlte das
// Kernel-Mapping, gäbe es sofort einen Triple Fault.
//
// EHRLICHE ABWEICHUNG VOM LEHRBUCH ("die obere Hälfte spiegeln"):
// Lehrbuch-Kernel liegen in der oberen Adressraumhälfte (Slots 256..511),
// dann heisst Spiegeln schlicht "Einträge 256..511 kopieren". UNSER Kernel
// tut das NICHT: bootloader_api 0.11 legt mit `Mapping::Dynamic` alles in
// die UNTERE Hälfte. Nachgemessen (Slot -> Inhalt):
//     P4[0]   Bootloader-/Frühmappings      P4[5]   Physik-Komplettmapping
//     P4[2,3] Kernel-Image                  P4[6,7] Stack, BootInfo, Framebuffer
//     P4[4]   ...                           P4[136] Kernel-Heap (0x4444_4444_0000)
// Würden wir nur die obere Hälfte spiegeln, wäre der neue Adressraum LEER
// und der erste Befehl nach dem CR3-Wechsel ein Triple Fault. Deshalb gilt
// bei uns: gespiegelt wird JEDER vom Kernel benutzte P4-Slot, privat ist
// genau EIN reservierter Slot — Nummer 1 (VA 512 GiB .. 1 TiB), der einzige
// freie Slot unterhalb des Heaps. Dort und NUR dort lebt User-Speicher.
//
// DIE FEINHEIT, DIE DAS ROBUST MACHT: Wir kopieren P4-EINTRÄGE, also Zeiger
// auf gemeinsame P3-Tabellen. Mappt der Kernel später eine neue Page (etwa
// heap_erweitern), landet sie in einer dieser GETEILTEN Tabellen und ist
// damit sofort in allen Adressräumen sichtbar. Nur ein KOMPLETT NEUER
// P4-Slot wäre nicht sichtbar — deshalb frischt `aktivieren()` den Spiegel
// jedes Mal auf (511 Einträge kopieren, ein Wimpernschlag).
//
// BESITZ UND ABRISS: Der Adressraum führt Buch über JEDEN Frame, den er
// selbst alloziert hat — die P4, alle Zwischentabellen und alle Datenseiten
// (`eigene`). Beim Abreissen gehen exakt diese zurück in den Frame-
// Allocator, kein einziger Kernel-Frame (der ist nur GESPIEGELT, nicht
// besessen). Der Beweis dafür ist byte-exakt: frame_statistik() vorher und
// nachher muss identisch sein.
// ==========================================================================

use crate::memory;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

/// Der EINE P4-Slot, der dem User-Space gehört. Slot 1 ist der einzige
/// unbenutzte Slot unterhalb des Kernel-Heaps (siehe Messung im Kopf-
/// kommentar) — er deckt 512 GiB ab, mehr als genug für unsere Prozesse.
pub const USER_P4_SLOT: usize = 1;
/// Untere Grenze des User-Adressbereichs: 512 GiB (= Slot 1 << 39).
/// Alles darunter (insbesondere die Nullseite) ist NIE User-Speicher —
/// ein Nullzeiger aus Ring 3 ist damit garantiert ein Page Fault.
pub const USER_START: u64 = (USER_P4_SLOT as u64) << 39;
/// Obere Grenze (exklusiv): 1 TiB.
pub const USER_ENDE: u64 = ((USER_P4_SLOT as u64) + 1) << 39;

/// Der gerade AKTIVE User-Adressraum (physische Adresse seiner P4);
/// 0 = der Kernel-Adressraum ist aktiv. Lock-frei, weil der Page-Fault-
/// Handler es lesen darf.
static AKTIVER_USER_P4: AtomicU64 = AtomicU64::new(0);

/// Fehler beim Umgang mit einem Adressraum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdressRaumFehler {
    /// Der Frame-Allocator hat keinen physischen Speicher mehr.
    KeinFrame,
    /// Die Adresse liegt nicht im User-Bereich (nur der ist pro Prozess
    /// privat — alles andere gehört dem gespiegelten Kernel).
    AusserhalbUserBereich,
    /// Der reservierte User-Slot wird vom Kernel benutzt (Struktur-Fehler:
    /// die Slot-Wahl im Kopfkommentar stimmt nicht mehr).
    UserSlotBelegt,
    /// Die Adresse ist in diesem Adressraum nicht gemappt.
    NichtGemappt,
    /// Die Seite ist bereits gemappt (doppeltes Mapping wäre ein Leck).
    SchonGemappt,
    /// Länge/Adresse laufen über den Adressraum hinaus.
    Ueberlauf,
}

/// Liegt [addr, addr+laenge) VOLLSTÄNDIG im User-Bereich? Reine Funktion —
/// die erste der drei Prüfungen jedes copy-in/copy-out (Dauerregel I).
pub fn im_user_bereich(addr: u64, laenge: usize) -> bool {
    if laenge == 0 {
        return (USER_START..USER_ENDE).contains(&addr);
    }
    match addr.checked_add(laenge as u64) {
        // Ende ist exklusiv, darf also GENAU auf USER_ENDE zeigen.
        Some(ende) => addr >= USER_START && ende <= USER_ENDE,
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Der Page-Table-Läufer (ohne Mapper, ohne Lock)
// ---------------------------------------------------------------------------

/// Läuft die vier Page-Table-Ebenen von `p4` aus für `addr` ab und liefert
/// die Flags der zuständigen Seite (None = nicht gemappt).
///
/// WARUM von Hand statt über `Mapper::translate`? Erstens brauchen wir das
/// für Tabellen, die NICHT in CR3 stehen (Prüfen eines fremden Adressraums),
/// zweitens nimmt der globale Mapper einen Lock — und diese Prüfung läuft im
/// Syscall-Pfad, also möglichst ohne Lock. HUGE_PAGE wird korrekt behandelt:
/// Das Physik-Komplettmapping des Bootloaders benutzt 2-MiB-/1-GiB-Seiten,
/// da endet der Abstieg vorzeitig.
fn flags_in(p4: PhysFrame, addr: VirtAddr) -> Option<PageTableFlags> {
    let offset = memory::phys_offset();
    let mut frame = p4;
    let indizes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    for (stufe, index) in indizes.iter().enumerate() {
        // unsafe: Der komplette physische Speicher ist ab `offset` gemappt,
        // `frame` stammt aus einem gültigen Tabelleneintrag — der Zeiger ist
        // also gültig und ausgerichtet. Nur Lesezugriff.
        let tabelle: &PageTable =
            unsafe { &*((offset + frame.start_address().as_u64()).as_ptr::<PageTable>()) };
        let eintrag = &tabelle[*index];
        let flags = eintrag.flags();
        if !flags.contains(PageTableFlags::PRESENT) {
            return None;
        }
        // Letzte Ebene erreicht ODER eine Huge Page: Hier stehen die Flags,
        // die für den Zugriff gelten.
        if stufe == 3 || flags.contains(PageTableFlags::HUGE_PAGE) {
            return Some(flags);
        }
        frame = PhysFrame::containing_address(eintrag.addr());
    }
    None
}

/// Übersetzt eine virtuelle Adresse in `p4` in die physische (nur 4-KiB-
/// Seiten — genau das, was wir selbst in User-Adressräume mappen).
fn uebersetzen_in(p4: PhysFrame, addr: VirtAddr) -> Option<PhysAddr> {
    let offset = memory::phys_offset();
    let mut frame = p4;
    let indizes = [
        addr.p4_index(),
        addr.p3_index(),
        addr.p2_index(),
        addr.p1_index(),
    ];
    for (stufe, index) in indizes.iter().enumerate() {
        // unsafe: siehe flags_in — gültiges Physik-Mapping, nur Lesezugriff.
        let tabelle: &PageTable =
            unsafe { &*((offset + frame.start_address().as_u64()).as_ptr::<PageTable>()) };
        let eintrag = &tabelle[*index];
        if !eintrag.flags().contains(PageTableFlags::PRESENT) {
            return None;
        }
        if eintrag.flags().contains(PageTableFlags::HUGE_PAGE) {
            return None; // Huge Pages mappen wir selbst nie an
        }
        if stufe == 3 {
            return Some(eintrag.addr() + (addr.as_u64() & 0xfff));
        }
        frame = PhysFrame::containing_address(eintrag.addr());
    }
    None
}

/// Die Seiten-Flags im AKTUELL AKTIVEN Adressraum (das, was CR3 sagt).
/// DAS ist die Prüf-Grundlage der copy-Helfer: Beim Syscall steht in CR3 der
/// Adressraum des AUFRUFENDEN Prozesses — wir prüfen also automatisch gegen
/// den richtigen, ohne ihn durchreichen zu müssen.
pub fn aktive_seiten_flags(addr: VirtAddr) -> Option<PageTableFlags> {
    flags_in(Cr3::read().0, addr)
}

/// Ist gerade ein User-Adressraum aktiv? (None = Kernel-Adressraum.)
pub fn aktiver_user_raum() -> Option<PhysFrame> {
    let adresse = AKTIVER_USER_P4.load(Ordering::SeqCst);
    if adresse == 0 {
        None
    } else {
        Some(PhysFrame::containing_address(PhysAddr::new(adresse)))
    }
}

/// Schaltet CR3 zurück auf den KERNEL-Adressraum. Immer erlaubt und immer
/// sicher: Der Kernel-Adressraum enthält alles, was der Kernel braucht.
pub fn kernel_aktivieren() {
    let kernel = memory::kernel_p4_frame();
    let (aktuell, flags) = Cr3::read();
    if aktuell != kernel {
        // unsafe: Wir laden eine Level-4-Tabelle, die den kompletten Kernel
        // (Code, Stack, Heap, Physik-Mapping) enthält — genau die, in der
        // SpeedOS gebootet ist. Der Befehl danach läuft garantiert weiter.
        unsafe { Cr3::write(kernel, flags) };
    }
    AKTIVER_USER_P4.store(0, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Der buchführende Frame-Allocator
// ---------------------------------------------------------------------------

/// Reicht Frames vom globalen Bitmap-Allocator durch und NOTIERT jeden —
/// so weiss der Adressraum am Ende exakt, was ihm gehört (auch die von
/// `map_to` im Verborgenen angelegten Zwischentabellen!).
struct BuchAllocator<'a> {
    eigene: &'a mut Vec<PhysFrame>,
}

// unsafe impl: Vertrag des Traits ist "nie denselben Frame zweimal" — das
// garantiert der globale Bitmap-Allocator, wir reichen nur durch.
unsafe impl FrameAllocator<Size4KiB> for BuchAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = memory::frame_allozieren()?;
        self.eigene.push(frame);
        Some(frame)
    }
}

/// Nullt einen frischen Frame über das Physik-Komplettmapping.
///
/// SICHERHEIT, nicht Kosmetik: Ein Frame aus dem Allocator trägt noch die
/// Bytes seines Vorbesitzers — womöglich Kernel-Daten oder Passwörter eines
/// anderen Prozesses. Er wird gleich für Ring 3 lesbar gemappt, also MUSS er
/// vorher gelöscht werden. (Die Zwischentabellen nullt `map_to` selbst.)
fn frame_nullen(frame: PhysFrame) {
    let virt = memory::phys_offset() + frame.start_address().as_u64();
    // unsafe: gültiges Physik-Mapping, exklusiv frischer Frame, 4 KiB gross.
    unsafe {
        core::ptr::write_bytes(virt.as_mut_ptr::<u8>(), 0, 4096);
    }
}

// ---------------------------------------------------------------------------
// AdressRaum
// ---------------------------------------------------------------------------

/// Ein privater Adressraum: eigene Level-4-Tabelle, gespiegelter Kernel,
/// privater User-Bereich — und eine Besitzliste für den vollständigen Abriss.
pub struct AdressRaum {
    /// Unsere Level-4-Tabelle (steht bei `aktivieren()` in CR3).
    p4: PhysFrame,
    /// ALLE Frames, die uns gehören: P4, Zwischentabellen, Datenseiten.
    /// Genau diese — und nur diese — gehen beim Abriss zurück.
    eigene: Vec<PhysFrame>,
}

impl AdressRaum {
    /// Legt einen neuen Adressraum an: frische P4, Kernel-Bereich
    /// hineingespiegelt, User-Slot leer.
    pub fn neu() -> Result<Self, AdressRaumFehler> {
        let p4 = memory::frame_allozieren().ok_or(AdressRaumFehler::KeinFrame)?;
        frame_nullen(p4); // 512 leere Einträge — nichts ist gemappt

        let mut raum = AdressRaum {
            p4,
            eigene: alloc::vec![p4],
        };
        // Bevor wir spiegeln: Der User-Slot MUSS im Kernel frei sein, sonst
        // würden wir Kernel-Mappings mit User-Speicher überbauen.
        if !raum.user_slot_frei_im_kernel() {
            // Den eben allozierten Frame nicht verlieren: Drop räumt auf.
            return Err(AdressRaumFehler::UserSlotBelegt);
        }
        raum.kernel_spiegeln();
        Ok(raum)
    }

    /// Der P4-Frame dieses Adressraums (für Diagnose/Tests).
    pub fn p4_frame(&self) -> PhysFrame {
        self.p4
    }

    /// Wie viele physische Frames besitzt dieser Adressraum gerade?
    /// (P4 + Zwischentabellen + Datenseiten — die Zahl, die beim Abriss
    /// vollständig zurückfliesst.)
    pub fn frames_besitz(&self) -> usize {
        self.eigene.len()
    }

    /// Prüft, ob der Kernel den reservierten User-Slot benutzt.
    fn user_slot_frei_im_kernel(&self) -> bool {
        let kernel = memory::kernel_p4_frame();
        let virt = memory::phys_offset() + kernel.start_address().as_u64();
        // unsafe: gültiges Physik-Mapping, nur Lesezugriff auf die Kernel-P4.
        let tabelle: &PageTable = unsafe { &*virt.as_ptr::<PageTable>() };
        tabelle[USER_P4_SLOT].is_unused()
    }

    /// Kopiert alle Kernel-P4-Einträge in unsere P4 (der User-Slot bleibt
    /// unangetastet). Läuft beim Anlegen UND bei jedem `aktivieren()`, damit
    /// ein zwischenzeitlich neu belegter Kernel-Slot nicht fehlt.
    fn kernel_spiegeln(&mut self) {
        let kernel = memory::kernel_p4_frame();
        let offset = memory::phys_offset();
        // unsafe: Beides sind gültige, verschiedene Page-Table-Frames im
        // Physik-Mapping. Wir kopieren nur 8-Byte-Einträge (Zeiger auf
        // GETEILTE P3-Tabellen — die Tabellen selbst gehören dem Kernel).
        unsafe {
            let quelle: &PageTable = &*((offset + kernel.start_address().as_u64())
                .as_ptr::<PageTable>());
            let ziel: &mut PageTable =
                &mut *((offset + self.p4.start_address().as_u64()).as_mut_ptr::<PageTable>());
            for slot in 0..512 {
                if slot == USER_P4_SLOT {
                    continue; // unser privater Bereich — nie überschreiben
                }
                ziel[slot] = quelle[slot].clone();
            }
        }
    }

    /// Führt eine Mapping-Operation auf UNSEREN Tabellen aus (auch wenn sie
    /// nicht in CR3 stehen — wir erreichen sie über das Physik-Mapping).
    fn mit_mapper<T>(
        &mut self,
        f: impl FnOnce(&mut OffsetPageTable<'_>, &mut BuchAllocator<'_>) -> T,
    ) -> T {
        // (Der Parameter heisst bewusst `zuteiler` und nicht `alloc` — sonst
        // verdeckt er im Rumpf den Crate-Namen `alloc`.)
        let offset = memory::phys_offset();
        let p4 = self.p4;
        // unsafe: Exklusive Referenz auf UNSERE P4. Der globale MAPPER hält
        // die KERNEL-P4 (anderer Frame) — es entsteht kein Aliasing. Und wir
        // mappen ausschliesslich in den User-Slot, dessen Unterbau uns allein
        // gehört; die gespiegelten Kernel-Slots fasst niemand hier an.
        let tabelle: &mut PageTable =
            unsafe { &mut *((offset + p4.start_address().as_u64()).as_mut_ptr::<PageTable>()) };
        // unsafe: Der Offset stimmt (Bootloader-Komplettmapping).
        let mut mapper = unsafe { OffsetPageTable::new(tabelle, offset) };
        let mut zuteiler = BuchAllocator {
            eigene: &mut self.eigene,
        };
        f(&mut mapper, &mut zuteiler)
    }

    /// Mappt EINE Page in den User-Bereich dieses Adressraums:
    /// PRESENT | WRITABLE | USER_ACCESSIBLE, frischer und GENULLTER Frame.
    ///
    /// Die Zwischentabellen bekommen dieselben Flags — die CPU UND-verknüpft
    /// das U-Bit über alle vier Ebenen, ein einziges U=0 unterwegs sperrt
    /// Ring 3 aus. (Nachträgliches Freischalten wie im Kernel-Adressraum
    /// brauchen wir hier NIE: Der ganze User-Slot ist frisch und gehört uns.)
    pub fn map_benutzer(&mut self, page: Page) -> Result<(), AdressRaumFehler> {
        let addr = page.start_address().as_u64();
        if !im_user_bereich(addr, 4096) {
            return Err(AdressRaumFehler::AusserhalbUserBereich);
        }
        if flags_in(self.p4, page.start_address()).is_some() {
            return Err(AdressRaumFehler::SchonGemappt);
        }
        self.mit_mapper(|mapper, zuteiler| -> Result<(), AdressRaumFehler> {
            let frame: PhysFrame = zuteiler
                .allocate_frame()
                .ok_or(AdressRaumFehler::KeinFrame)?;
            frame_nullen(frame);
            let flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE;
            // unsafe: frischer, exklusiver Frame; die Page ist geprüft frei.
            let flush = unsafe {
                mapper
                    .map_to_with_table_flags(page, frame, flags, flags, zuteiler)
                    .map_err(|_| AdressRaumFehler::KeinFrame)?
            };
            // Kein flush nötig, solange dieser Adressraum nicht aktiv ist —
            // aber ignorieren ist gefährlicher als einmal zu viel zu leeren.
            flush.ignore();
            Ok(())
        })?;
        if self.ist_aktiv() {
            x86_64::instructions::tlb::flush(page.start_address());
        }
        Ok(())
    }

    /// Mappt einen ganzen BEREICH [start, start+laenge) seitenweise in den
    /// User-Bereich (start wird abgerundet, das Ende aufgerundet).
    /// Schlägt eine Seite mittendrin fehl, bleiben die bereits gemappten
    /// stehen — sie gehören dem Adressraum und gehen beim Abriss zurück,
    /// es leckt also nichts.
    pub fn bereich_mappen(
        &mut self,
        start: VirtAddr,
        laenge: usize,
    ) -> Result<(), AdressRaumFehler> {
        if laenge == 0 {
            return Ok(());
        }
        let erste = start.as_u64() & !0xfff;
        let ende = start
            .as_u64()
            .checked_add(laenge as u64)
            .ok_or(AdressRaumFehler::Ueberlauf)?;
        let letzte = (ende - 1) & !0xfff;
        let mut va = erste;
        loop {
            self.map_benutzer(Page::containing_address(VirtAddr::new(va)))?;
            if va == letzte {
                return Ok(());
            }
            va += 4096;
        }
    }

    /// Legt einen USER-STACK an: `seiten` Seiten unterhalb von `top`, und
    /// darunter eine GUARD-PAGE, die BEWUSST NICHT gemappt wird.
    ///
    /// Warum das wichtig ist: Ein Stack wächst nach unten. Ohne Guard-Page
    /// liefe er bei einem Überlauf einfach in die nächste gemappte Seite
    /// weiter und würde STILL fremde Daten zerschreiben — der übelste
    /// Fehlerfall überhaupt, weil er erst viel später und ganz woanders
    /// auffällt. Mit Guard-Page gibt es sofort einen Page Fault, und der
    /// wird (Dauerregel II) sauber als User-Fehler aufgefangen.
    ///
    /// Liefert das obere Stack-Ende (den Startwert für RSP).
    pub fn stack_anlegen(
        &mut self,
        top: VirtAddr,
        seiten: usize,
    ) -> Result<VirtAddr, AdressRaumFehler> {
        assert!(seiten > 0, "Ein Stack braucht mindestens eine Seite");
        assert!(
            top.as_u64().is_multiple_of(4096),
            "Das Stack-Ende muss seiten-ausgerichtet sein"
        );
        let unterkante = top
            .as_u64()
            .checked_sub((seiten as u64) * 4096)
            .ok_or(AdressRaumFehler::Ueberlauf)?;
        // Die Guard-Page muss ÜBERHAUPT existieren können, sonst wäre der
        // Stack am unteren Rand des User-Bereichs ungeschützt.
        let guard = unterkante
            .checked_sub(4096)
            .ok_or(AdressRaumFehler::Ueberlauf)?;
        if guard < USER_START {
            return Err(AdressRaumFehler::AusserhalbUserBereich);
        }
        self.bereich_mappen(VirtAddr::new(unterkante), seiten * 4096)?;
        // Die Guard-Page wird NICHT gemappt — das IST der Schutz.
        Ok(top)
    }

    /// Ist die Adresse in DIESEM Adressraum gemappt, und mit welchen Flags?
    /// (Funktioniert auch, wenn der Adressraum gerade nicht aktiv ist —
    /// so prüfen die Tests den "fremden Adressraum".)
    pub fn seiten_flags(&self, addr: VirtAddr) -> Option<PageTableFlags> {
        flags_in(self.p4, addr)
    }

    /// Schreibt Bytes an eine virtuelle Adresse DIESES Adressraums, OHNE ihn
    /// zu aktivieren — der Kernel geht über das Physik-Komplettmapping. Genau
    /// so lädt man ein Programm in einen Prozess, bevor er das erste Mal
    /// läuft (und genau so wird es der ELF-Loader tun).
    pub fn schreiben(&mut self, addr: VirtAddr, daten: &[u8]) -> Result<(), AdressRaumFehler> {
        transfer_schleife(self.p4, addr, daten.len(), |phys_ptr, index, teil| {
            // unsafe: `phys_ptr` zeigt über das Physik-Mapping in unsere
            // eigene, geprüft gemappte Datenseite.
            unsafe { core::ptr::copy_nonoverlapping(daten[index..].as_ptr(), phys_ptr, teil) }
        })
    }

    /// Liest Bytes aus diesem Adressraum (Gegenstück zu `schreiben`).
    pub fn lesen(&self, addr: VirtAddr, ziel: &mut [u8]) -> Result<(), AdressRaumFehler> {
        // Erst prüfen, dann kopieren — dieselbe Reihenfolge wie überall.
        let laenge = ziel.len();
        let p4 = self.p4;
        transfer_schleife(p4, addr, laenge, |phys_ptr, index, teil| {
            // unsafe: geprüft gemappte Seite dieses Adressraums.
            unsafe { core::ptr::copy_nonoverlapping(phys_ptr, ziel[index..].as_mut_ptr(), teil) }
        })
    }

    /// Steht dieser Adressraum gerade in CR3?
    pub fn ist_aktiv(&self) -> bool {
        Cr3::read().0 == self.p4
    }

    /// Aktiviert diesen Adressraum (CR3-Wechsel). Ab dem nächsten Befehl
    /// sieht die CPU den privaten User-Bereich dieses Prozesses — und
    /// weiterhin den kompletten Kernel (gespiegelt).
    ///
    /// Der Spiegel wird vorher aufgefrischt: Hat der Kernel seit dem Anlegen
    /// einen ganz neuen P4-Slot belegt (z. B. beim ersten allocate_pages),
    /// kommt er damit noch mit. Mappings INNERHALB schon gespiegelter Slots
    /// sind automatisch sichtbar, weil wir uns die P3-Tabellen teilen.
    pub fn aktivieren(&mut self) {
        self.kernel_spiegeln();
        let (_, flags) = Cr3::read();
        AKTIVER_USER_P4.store(self.p4.start_address().as_u64(), Ordering::SeqCst);
        // unsafe: Unsere P4 enthält den vollständig gespiegelten Kernel —
        // der Befehlsstrom, der Stack und alle Daten des Kernels bleiben
        // nach dem Wechsel an derselben virtuellen Adresse erreichbar.
        unsafe { Cr3::write(self.p4, flags) };
    }

    /// Reisst den Adressraum ab und gibt ALLE eigenen Frames zurück.
    /// (Explizite, gut lesbare Form von `drop` — die Arbeit macht `Drop`,
    /// damit auch ein vergessener Adressraum nichts leckt.)
    pub fn abreissen(self) {
        drop(self);
    }
}

/// Die gemeinsame Transfer-Schleife von `schreiben`/`lesen`: geht den Bereich
/// SEITENWEISE durch, übersetzt jede Seite über die Tabellen von `p4` und
/// ruft `f(zeiger_ins_physik_mapping, index_im_puffer, bytes_in_dieser_seite)`.
/// PRÜFT VOLLSTÄNDIG VORHER: Ist auch nur eine Seite ungemappt, wird GAR
/// NICHTS kopiert (kein halb geschriebener Puffer).
fn transfer_schleife(
    p4: PhysFrame,
    addr: VirtAddr,
    laenge: usize,
    mut f: impl FnMut(*mut u8, usize, usize),
) -> Result<(), AdressRaumFehler> {
    if laenge == 0 {
        return Ok(());
    }
    let start = addr.as_u64();
    let ende = start
        .checked_add(laenge as u64)
        .ok_or(AdressRaumFehler::Ueberlauf)?;
    // 1. Durchgang: alles prüfen.
    let mut seite = start & !0xfff;
    while seite < ende {
        if uebersetzen_in(p4, VirtAddr::new(seite)).is_none() {
            return Err(AdressRaumFehler::NichtGemappt);
        }
        seite += 4096;
    }
    // 2. Durchgang: kopieren.
    let offset = memory::phys_offset();
    let mut rest = laenge;
    let mut va = start;
    let mut index = 0usize;
    while rest > 0 {
        let phys = uebersetzen_in(p4, VirtAddr::new(va)).ok_or(AdressRaumFehler::NichtGemappt)?;
        let bis_seitenende = 4096 - (va & 0xfff) as usize;
        let teil = rest.min(bis_seitenende);
        let zeiger = (offset + phys.as_u64()).as_mut_ptr::<u8>();
        f(zeiger, index, teil);
        va += teil as u64;
        index += teil;
        rest -= teil;
    }
    Ok(())
}

impl Drop for AdressRaum {
    /// Der vollständige Abriss: Ist der Adressraum noch aktiv, wird ZUERST
    /// auf den Kernel-Adressraum zurückgeschaltet (die Tabellen unter den
    /// eigenen Füssen freizugeben wäre der schnellste Weg zum Triple Fault).
    /// Danach gehen alle eigenen Frames zurück — Datenseiten, Zwischen-
    /// tabellen und die P4. Kernel-Frames sind nur GESPIEGELT (kopierte
    /// Zeiger), stehen nicht in `eigene` und werden deshalb nie angefasst.
    fn drop(&mut self) {
        if self.ist_aktiv() {
            kernel_aktivieren();
        }
        for frame in self.eigene.drain(..) {
            // unsafe: Der Adressraum wird gerade zerstört, seine Tabellen
            // stehen nicht mehr in CR3 und niemand hält Referenzen darauf.
            unsafe { memory::frame_freigeben(frame) };
        }
    }
}

// ---------------------------------------------------------------------------
// Unit-Tests (die grossen Beweise liegen in tests/adressraum.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die reine Bereichs-Prüfung — die erste Verteidigungslinie jedes
    /// copy-in/copy-out, deshalb gegen alle Ränder geprüft.
    #[test_case]
    fn test_im_user_bereich_grenzen() {
        assert!(im_user_bereich(USER_START, 4096));
        assert!(im_user_bereich(USER_ENDE - 1, 1));
        // Genau bis an die Obergrenze ist erlaubt (Ende ist exklusiv) ...
        assert!(im_user_bereich(USER_ENDE - 8, 8));
        // ... ein Byte darüber nicht.
        assert!(!im_user_bereich(USER_ENDE - 8, 9));
        // Nullseite und alles darunter: nie.
        assert!(!im_user_bereich(0, 8));
        assert!(!im_user_bereich(USER_START - 1, 8));
        // Kernel-Heap (P4[136]) liegt zwar in der unteren Hälfte, ist aber
        // ausserhalb des User-Slots — genau darum reicht "untere Hälfte"
        // als Prüfung NICHT (siehe Kopfkommentar).
        assert!(!im_user_bereich(crate::allocator::HEAP_START as u64, 8));
        // Integer-Überlauf bei Zeiger + Länge.
        assert!(!im_user_bereich(u64::MAX - 3, 16));
        assert!(!im_user_bereich(USER_START, usize::MAX));
    }

    /// Anlegen + Abreissen ist frame-neutral, und der Kernel ist gespiegelt.
    #[test_case]
    fn test_adressraum_anlegen_und_abreissen() {
        let (frei_vorher, _) = memory::frame_statistik();
        {
            let mut raum = AdressRaum::neu().expect("Adressraum anlegen");
            // Der Kernel ist da: Der Heap muss in der neuen P4 auffindbar
            // sein (gespiegelter Slot), obwohl wir ihn nie gemappt haben.
            assert!(raum
                .seiten_flags(VirtAddr::new(crate::allocator::HEAP_START as u64))
                .is_some());
            // Der User-Bereich ist dagegen komplett leer.
            assert!(raum.seiten_flags(VirtAddr::new(USER_START)).is_none());
            // Eine User-Page mappen: Flags müssen U tragen.
            let page = Page::containing_address(VirtAddr::new(USER_START));
            raum.map_benutzer(page).expect("User-Page mappen");
            let flags = raum.seiten_flags(VirtAddr::new(USER_START)).unwrap();
            assert!(flags.contains(PageTableFlags::USER_ACCESSIBLE));
            assert!(flags.contains(PageTableFlags::WRITABLE));
            // Im KERNEL-Adressraum ist dieselbe Adresse weiterhin ungemappt —
            // das ist die Isolation, um die es geht.
            assert!(memory::seiten_flags(VirtAddr::new(USER_START)).is_none());
            // Doppeltes Mappen wird abgelehnt (sonst Frame-Leck).
            assert_eq!(
                raum.map_benutzer(page),
                Err(AdressRaumFehler::SchonGemappt)
            );
            // Ausserhalb des User-Bereichs: abgelehnt.
            assert_eq!(
                raum.map_benutzer(Page::containing_address(VirtAddr::new(
                    crate::allocator::HEAP_START as u64
                ))),
                Err(AdressRaumFehler::AusserhalbUserBereich)
            );
        }
        // Abgerissen (Drop) -> byte-exakt derselbe Frame-Stand wie vorher.
        let (frei_nachher, _) = memory::frame_statistik();
        assert_eq!(frei_vorher, frei_nachher, "Adressraum hat Frames geleckt");
    }

    /// Schreiben/Lesen OHNE Aktivierung (der Weg des künftigen ELF-Loaders),
    /// inklusive Seitengrenze.
    #[test_case]
    fn test_schreiben_lesen_ohne_aktivierung() {
        let mut raum = AdressRaum::neu().expect("Adressraum anlegen");
        raum.bereich_mappen(VirtAddr::new(USER_START), 2 * 4096)
            .expect("zwei Seiten mappen");
        // Ein Puffer, der GENAU über die Seitengrenze läuft.
        let daten: [u8; 16] = *b"AAAABBBBCCCCDDDD";
        let ziel_va = VirtAddr::new(USER_START + 4096 - 8);
        raum.schreiben(ziel_va, &daten).expect("schreiben");
        let mut zurueck = [0u8; 16];
        raum.lesen(ziel_va, &mut zurueck).expect("lesen");
        assert_eq!(zurueck, daten);
        // Frisch gemappte Seiten sind GENULLT (kein Datenleck aus fremden
        // Frames in den User-Space).
        let mut null = [0xffu8; 32];
        raum.lesen(VirtAddr::new(USER_START + 0x800), &mut null)
            .expect("lesen");
        assert_eq!(null, [0u8; 32]);
        // In ungemapptes Gebiet: sauberer Fehler, kein halber Schreibvorgang.
        assert_eq!(
            raum.schreiben(VirtAddr::new(USER_START + 2 * 4096), &daten),
            Err(AdressRaumFehler::NichtGemappt)
        );
    }

    /// Die Guard-Page unter dem Stack ist wirklich NICHT gemappt.
    #[test_case]
    fn test_stack_hat_guard_page() {
        let mut raum = AdressRaum::neu().expect("Adressraum anlegen");
        let top = VirtAddr::new(USER_START + 0x10_0000);
        raum.stack_anlegen(top, 4).expect("Stack anlegen");
        // Alle vier Stack-Seiten sind da ...
        for i in 1..=4u64 {
            assert!(
                raum.seiten_flags(top - i * 4096).is_some(),
                "Stack-Seite {} fehlt",
                i
            );
        }
        // ... und direkt darunter ist ein Loch (der Überlauf-Fänger).
        assert!(
            raum.seiten_flags(top - 5u64 * 4096).is_none(),
            "Guard-Page ist gemappt — der Stack-Ueberlauf-Schutz fehlt!"
        );
    }
}
