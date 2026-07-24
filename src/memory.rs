// memory.rs — Speicherverwaltung: Paging + physischer Frame-Allocator
//
// Dieses Modul ist DIE zentrale Anlaufstelle für Speicher-Mapping:
// Mapper (Page Tables) und Frame-Allocator leben hier als globale,
// interrupt-sichere Strukturen (Mutex<Option<...>>, wie das VFS) —
// jedes Modul kann damit zur Laufzeit Pages mappen, nicht nur
// kernel_main beim Boot.
//
// Öffentliche API (alles nach memory::init() benutzbar):
//   map_page(page)            eine Page auf einen frischen Frame mappen
//   map_page_zu(page, frame)  eine Page auf einen BESTIMMTEN Frame (MMIO!)
//   unmap_page(page)          Mapping lösen, Frame zurückbekommen
//   allocate_pages(anzahl)    zusammenhängend: virtuell UND physisch
//                             (braucht später der Framebuffer/DMA)
//   frame_allozieren()/frame_freigeben(frame)   rohe Frames
//   uebersetzen(virt)         virtuelle -> physische Adresse
//   frame_statistik()         (freie, gesamte) Frames — für meminfo
//
// Grundlagen-Erklärung zu Paging: siehe Kommentar-Block in der
// Git-Historie (Speicherverwaltung Teil 1) bzw. README.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind, MemoryRegions};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::{
    registers::control::Cr3,
    structures::paging::{
        mapper::{MapToError, UnmapError},
        FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable,
        PageTableFlags, PhysFrame, Size4KiB, Translate,
    },
    PhysAddr, VirtAddr,
};

/// Ab dieser virtuellen Adresse vergibt allocate_pages() Bereiche.
/// Bewusst weit weg vom Heap (0x4444_...) und allen Testadressen.
pub const KERNEL_ALLOC_START: usize = 0x_7000_0000_0000;

// ---------------------------------------------------------------------------
// Globale Strukturen (Muster wie beim VFS: Mutex<Option<...>>)
// ---------------------------------------------------------------------------

/// Der globale Mapper auf die aktiven Page Tables.
static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
/// Der globale physische Frame-Allocator.
static FRAME_ALLOCATOR: Mutex<Option<BitmapFrameAllocator>> = Mutex::new(None);
/// Der Offset des Komplett-Mappings (für phys_offset()).
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);
/// Nächste freie virtuelle Adresse für allocate_pages() —
/// ein simpler Vorwärts-Zähler (virtuellen Adressraum haben wir
/// im Überfluss; Wiederverwenden freigegebener Bereiche kommt,
/// wenn wir es wirklich brauchen).
static NAECHSTE_VIRT_ADRESSE: AtomicUsize = AtomicUsize::new(KERNEL_ALLOC_START);

/// Initialisiert die globale Speicherverwaltung. GENAU EINMAL beim
/// Boot aufrufen, nach speed_os::init() und vor allocator::init_heap().
///
/// # Safety-Anmerkung (Funktion ist trotzdem safe aufrufbar):
/// Die Korrektheit stützt sich darauf, dass boot_info vom Bootloader
/// stammt: physical_memory_offset zeigt wirklich auf das Komplett-
/// Mapping, und die Memory Regions beschreiben den RAM korrekt.
pub fn init(physical_memory_offset: VirtAddr, memory_regions: &'static MemoryRegions) {
    PHYS_OFFSET.store(physical_memory_offset.as_u64(), Ordering::Relaxed);

    // unsafe: Wir lesen CR3 und erzeugen die einzige &mut-Referenz auf
    // die Level-4-Tabelle — sie wandert sofort in den globalen Mutex,
    // weitere Referenzen entstehen nie (init wird nur einmal gerufen).
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    // unsafe: Offset stammt aus der BootInfo, das Mapping existiert.
    let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };

    // unsafe: neu() nimmt die exklusive Referenz auf die statische
    // Bitmap — auch das passiert nur bei diesem einen init-Aufruf.
    let frame_allocator = unsafe { BitmapFrameAllocator::neu(memory_regions) };

    *MAPPER.lock() = Some(mapper);
    *FRAME_ALLOCATOR.lock() = Some(frame_allocator);
}

/// Der Offset, ab dem der komplette physische Speicher gemappt ist.
pub fn phys_offset() -> VirtAddr {
    VirtAddr::new(PHYS_OFFSET.load(Ordering::Relaxed))
}

/// Führt eine Operation mit Mapper + Frame-Allocator aus — kapselt
/// Locking und Lock-REIHENFOLGE (immer Mapper zuerst), damit es nie
/// zwei Pfade mit unterschiedlicher Reihenfolge geben kann (Deadlock!).
/// Interrupts sind währenddessen aus: Kein Interrupt-Handler mappt
/// zwar (Projektregel), aber so ist es auch gegen zukünftige Fehler
/// abgesichert. Panic, falls init() noch nicht lief — Speicher ist
/// boot-kritisch, ein "weiter ohne" gibt es nicht.
fn mit_speicher<T>(
    f: impl FnOnce(&mut OffsetPageTable<'static>, &mut BitmapFrameAllocator) -> T,
) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut mapper = MAPPER.lock();
        let mut frame_allocator = FRAME_ALLOCATOR.lock();
        f(
            mapper.as_mut().expect("memory::init() wurde nie aufgerufen"),
            frame_allocator
                .as_mut()
                .expect("memory::init() wurde nie aufgerufen"),
        )
    })
}

/// Liefert eine Referenz auf die aktive Level-4-Page-Table.
///
/// # Safety
///
/// Der komplette physische Speicher muss bei `physical_memory_offset`
/// gemappt sein, und die Funktion darf nur einmal benutzt werden —
/// sonst entstehen mehrere &mut auf dieselbe Tabelle.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr // unsafe: Zeiger ist dank Bootloader-Mapping gültig
}

// ---------------------------------------------------------------------------
// Öffentliche Mapping-API
// ---------------------------------------------------------------------------

/// Mappt eine Page auf einen frischen physischen Frame
/// (PRESENT + WRITABLE). Der Frame gehört danach dieser Page.
pub fn map_page(page: Page) -> Result<(), MapToError<Size4KiB>> {
    mit_speicher(|mapper, frame_allocator| {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        // unsafe: Der Frame kommt frisch aus dem Allocator, kein
        // Aliasing möglich; die Page prüft map_to selbst auf Konflikte.
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
        Ok(())
    })
}

/// Mappt eine Page auf einen frischen Frame und macht sie RING-3-ZUGÄNGLICH
/// (PRESENT | WRITABLE | USER_ACCESSIBLE). Genau das braucht User-Mode-Code:
/// Ohne das USER_ACCESSIBLE-Flag löst jeder Zugriff aus Ring 3 sofort einen
/// Page Fault aus (Schutzverletzung). Für den ersten Ring-3-Sprung (Serie 6).
///
/// WICHTIG: Die CPU UND-verknüpft das U-Bit über ALLE vier Page-Table-Ebenen
/// (P4→P3→P2→P1) — ein einziges U=0 unterwegs sperrt Ring 3. `map_to_with_
/// table_flags` setzt U nur auf NEU angelegten Zwischentabellen; existierten
/// sie schon (ohne U), müssen wir nachhelfen (`benutzer_pfad_freischalten`).
pub fn map_page_benutzer(page: Page) -> Result<(), MapToError<Size4KiB>> {
    // Expliziter Closure-Rückgabetyp pinnt S = Size4KiB (OffsetPageTable
    // implementiert Mapper für mehrere Seitengrößen — sonst mehrdeutig).
    mit_speicher(|mapper, frame_allocator| -> Result<(), MapToError<Size4KiB>> {
        let frame: PhysFrame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE;
        // unsafe: frischer, exklusiver Frame; parent_table_flags ebenfalls
        // mit U, damit neu angelegte Zwischentabellen ring-3-zugänglich sind.
        let flush = unsafe {
            mapper.map_to_with_table_flags(page, frame, flags, flags, frame_allocator)?
        };
        flush.flush();
        Ok(())
    })?;
    benutzer_pfad_freischalten(page.start_address());
    Ok(())
}

/// Setzt das USER_ACCESSIBLE-Bit (und PRESENT|WRITABLE) auf den P4/P3/P2-
/// Einträgen für `addr` — die drei ZWISCHEN-Ebenen. Nötig, falls sie schon
/// ohne U existierten (map_to lässt vorhandene Tabellen unverändert). Die
/// P1-Leaf-Ebene trägt ihre Flags bereits vom Mapping.
fn benutzer_pfad_freischalten(addr: VirtAddr) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let offset = phys_offset();
        let (p4_frame, _) = Cr3::read();
        let extra =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        // unsafe: Wir lesen/ändern die aktiven Page Tables über das Komplett-
        // Mapping. Single-threaded, keine gleichzeitige Mapper-Nutzung während
        // dieses Aufrufs (nur beim Ring-3-Aufsetzen aufgerufen).
        unsafe {
            let mut table_frame = p4_frame;
            for shift in [39u64, 30, 21] {
                let table: &mut PageTable = &mut *((offset
                    + table_frame.start_address().as_u64())
                .as_mut_ptr::<PageTable>());
                let index = ((addr.as_u64() >> shift) & 0x1ff) as usize;
                let entry = &mut table[index];
                entry.set_flags(entry.flags() | extra);
                table_frame = PhysFrame::containing_address(entry.addr());
            }
        }
    });
    x86_64::instructions::tlb::flush(addr);
}

/// Die Mapping-Flags einer virtuellen Adresse (None = nicht gemappt). Der
/// copy-in-Helfer (ring3.rs) prüft damit, ob eine User-Adresse wirklich zum
/// User-Bereich gehört (USER_ACCESSIBLE gesetzt) — die Kernel/User-Grenze.
pub fn seiten_flags(addr: VirtAddr) -> Option<PageTableFlags> {
    use x86_64::structures::paging::mapper::TranslateResult;
    mit_speicher(|mapper, _| match mapper.translate(addr) {
        TranslateResult::Mapped { flags, .. } => Some(flags),
        _ => None,
    })
}

/// Mappt eine Page auf einen BESTIMMTEN Frame — für Hardware-Speicher
/// wie den VGA-Puffer oder später den Framebuffer (MMIO).
///
/// # Safety
///
/// Der Aufrufer muss sicherstellen, dass dieses Mapping kein
/// verbotenes Aliasing erzeugt: Zeigt der Frame auf normalen RAM, der
/// gleichzeitig woanders gemappt ist, können sich zwei Code-Stellen
/// gegenseitig den Speicher zerschreiben. Für Hardware-Puffer
/// (VGA, Framebuffer) ist genau dieses Mehrfach-Mapping gewollt.
pub unsafe fn map_page_zu(page: Page, frame: PhysFrame) -> Result<(), MapToError<Size4KiB>> {
    mit_speicher(|mapper, frame_allocator| {
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        // unsafe: Verantwortung liegt laut # Safety beim Aufrufer.
        mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        Ok(())
    })
}

/// Löst das Mapping einer Page und gibt den Frame ZURÜCK — der
/// Aufrufer entscheidet, ob er ihn freigibt (frame_freigeben) oder
/// nicht (bei MMIO-Frames: NICHT freigeben, das ist kein RAM!).
pub fn unmap_page(page: Page) -> Result<PhysFrame, UnmapError> {
    mit_speicher(|mapper, _| {
        let (frame, flush) = mapper.unmap(page)?;
        flush.flush();
        Ok(frame)
    })
}

/// Alloziert `anzahl` Pages, die virtuell UND physisch zusammenhängen,
/// und liefert die virtuelle Startadresse. Physische Kontiguität
/// braucht später alles, was Hardware direkt liest (DMA-Puffer etc.).
pub fn allocate_pages(anzahl: usize) -> Result<VirtAddr, MapToError<Size4KiB>> {
    if anzahl == 0 {
        return Err(MapToError::FrameAllocationFailed);
    }
    // Virtuellen Bereich reservieren (atomarer Vorwärts-Zähler):
    let virt_start = NAECHSTE_VIRT_ADRESSE.fetch_add(anzahl * 4096, Ordering::Relaxed);

    mit_speicher(|mapper, frame_allocator| {
        // Physisch zusammenhängenden Block suchen:
        let erster_frame = frame_allocator
            .allozieren_zusammenhaengend(anzahl)
            .ok_or(MapToError::FrameAllocationFailed)?;

        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        for i in 0..anzahl {
            let page =
                Page::containing_address(VirtAddr::new((virt_start + i * 4096) as u64));
            let frame = erster_frame + i as u64;
            // unsafe: Frames kommen frisch und exklusiv aus dem
            // Allocator — kein Aliasing möglich.
            unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
        }
        Ok(VirtAddr::new(virt_start as u64))
    })
}

/// Alloziert einen einzelnen rohen Frame (ohne Mapping).
pub fn frame_allozieren() -> Option<PhysFrame> {
    mit_speicher(|_, frame_allocator| frame_allocator.allocate_frame())
}

/// Gibt einen Frame zur Wiederverwendung frei.
///
/// # Safety
///
/// Der Frame darf nirgendwo mehr gemappt oder in Benutzung sein —
/// sonst vergibt der Allocator ihn erneut und zwei Besitzer
/// zerschreiben sich gegenseitig den Speicher.
pub unsafe fn frame_freigeben(frame: PhysFrame) {
    mit_speicher(|_, frame_allocator| frame_allocator.deallocate_frame(frame))
}

/// Übersetzt eine virtuelle in die physische Adresse (None = ungemappt).
pub fn uebersetzen(adresse: VirtAddr) -> Option<PhysAddr> {
    mit_speicher(|mapper, _| mapper.translate_addr(adresse))
}

/// (freie Frames, Frames gesamt) — für den meminfo-Befehl.
pub fn frame_statistik() -> (usize, usize) {
    mit_speicher(|_, frame_allocator| {
        (frame_allocator.frames_frei, frame_allocator.frames_gesamt)
    })
}

// ---------------------------------------------------------------------------
// Der Bitmap-Frame-Allocator
//
// WARUM eine Bitmap und keine Free-List?
//  - Eine Free-List (freie Frames als verkettete Liste) hätte zwar
//    O(1)-Allokation, aber ZUSAMMENHÄNGENDE Bereiche (Framebuffer,
//    DMA!) findet sie praktisch nicht — die Liste ist ungeordnet,
//    man müsste sie sortieren oder verschmelzen.
//  - Die Bitmap (1 Bit pro 4-KiB-Frame) kann beides: Einzel-Frames
//    fast in O(1) (Next-Fit-Zeiger + 64 Frames pro u64-Vergleich)
//    und zusammenhängende Läufe per simplem Scan.
//  - Freigeben ist O(1) (Bit setzen), Doppel-Freigaben fallen sofort
//    auf (Bit war schon gesetzt -> Panic statt stiller Korruption).
//  - Kosten: 1 Bit pro Frame = 32 KiB Bitmap für 1 GiB RAM. Geschenkt.
// ---------------------------------------------------------------------------

/// Wir verwalten bis zu 1 GiB physischen Speicher (262.144 Frames).
/// RAM oberhalb davon wird schlicht ignoriert (QEMU-Standard: 128 MiB).
const MAX_FRAMES: usize = 262_144;
const BITMAP_WORTE: usize = MAX_FRAMES / 64;

/// Bit = 1: Frame ist FREI. Bit = 0: belegt oder existiert nicht.
pub struct BitmapFrameAllocator {
    bitmap: &'static mut [u64; BITMAP_WORTE],
    /// Next-Fit-Zeiger: Frame-Index, ab dem die nächste Suche startet.
    /// Wird beim Freigeben zurückgesetzt, damit freigegebene Frames
    /// sofort wiederverwendet werden (gut gegen Fragmentierung).
    naechster: usize,
    frames_gesamt: usize,
    frames_frei: usize,
}

impl BitmapFrameAllocator {
    /// Baut den Allocator aus den Memory Regions des Bootloaders:
    /// alle "Usable"-Frames werden als frei markiert, alles andere
    /// (Kernel, Bootloader, Hardware-Löcher) bleibt belegt.
    ///
    /// # Safety
    ///
    /// Nur EINMAL aufrufen: nimmt die exklusive Referenz auf die
    /// statische Bitmap. Und die Memory Regions müssen korrekt sein.
    unsafe fn neu(memory_regions: &'static [MemoryRegion]) -> Self {
        // Die Bitmap lebt als static im BSS-Segment (32 KiB) — sie
        // kann nicht auf dem Heap liegen, denn sie existiert BEVOR
        // es einen Heap gibt (der Heap braucht ja diesen Allocator!).
        static mut BITMAP: [u64; BITMAP_WORTE] = [0; BITMAP_WORTE];
        // Über einen rohen Zeiger statt einer direkten Referenz auf die
        // static mut (das verlangt der Compiler heutzutage so).
        // unsafe: exklusive Referenz, garantiert durch "nur ein Aufruf".
        let bitmap_zeiger: *mut [u64; BITMAP_WORTE] = &raw mut BITMAP;
        let bitmap = &mut *bitmap_zeiger;

        let mut frames_gesamt = 0;
        let mut frames_frei = 0;

        for region in memory_regions.iter() {
            let start_frame = (region.start / 4096) as usize;
            let end_frame = (region.end / 4096) as usize;
            for index in start_frame..end_frame.min(MAX_FRAMES) {
                frames_gesamt = frames_gesamt.max(index + 1);
                if region.kind == MemoryRegionKind::Usable {
                    bitmap[index / 64] |= 1 << (index % 64);
                    frames_frei += 1;
                }
            }
        }

        BitmapFrameAllocator {
            bitmap,
            naechster: 0,
            frames_gesamt,
            frames_frei,
        }
    }

    fn ist_frei(&self, index: usize) -> bool {
        self.bitmap[index / 64] & (1 << (index % 64)) != 0
    }

    fn belegen(&mut self, index: usize) {
        self.bitmap[index / 64] &= !(1 << (index % 64));
    }

    /// Sucht per Next-Fit den nächsten freien Frame: Wort für Wort
    /// (64 Frames pro Vergleich!), beginnend beim naechster-Zeiger.
    fn allozieren(&mut self) -> Option<PhysFrame> {
        if self.frames_frei == 0 {
            return None;
        }
        let mut wort_index = self.naechster / 64;
        // Im Start-Wort nur Bits AB dem Zeiger betrachten:
        let mut maske = !0u64 << (self.naechster % 64);

        // Einmal komplett rundherum (inklusive Wrap-Around).
        for _ in 0..=BITMAP_WORTE {
            let wort = self.bitmap[wort_index] & maske;
            if wort != 0 {
                let bit = wort.trailing_zeros() as usize;
                let index = wort_index * 64 + bit;
                self.belegen(index);
                self.frames_frei -= 1;
                self.naechster = index + 1;
                return Some(PhysFrame::containing_address(PhysAddr::new(
                    (index * 4096) as u64,
                )));
            }
            wort_index = (wort_index + 1) % BITMAP_WORTE;
            maske = !0; // ab dem zweiten Wort zählen alle Bits
        }
        None
    }

    /// Sucht `anzahl` DIREKT AUFEINANDERFOLGENDE freie Frames
    /// (linearer Scan von vorne — Framebuffer/DMA allozieren selten,
    /// da darf die Suche ruhig gründlich statt schnell sein).
    fn allozieren_zusammenhaengend(&mut self, anzahl: usize) -> Option<PhysFrame> {
        if anzahl == 0 || self.frames_frei < anzahl {
            return None;
        }
        let mut lauf_start = 0;
        let mut lauf_laenge = 0;
        for index in 0..self.frames_gesamt {
            if self.ist_frei(index) {
                if lauf_laenge == 0 {
                    lauf_start = index;
                }
                lauf_laenge += 1;
                if lauf_laenge == anzahl {
                    for i in lauf_start..=index {
                        self.belegen(i);
                    }
                    self.frames_frei -= anzahl;
                    return Some(PhysFrame::containing_address(PhysAddr::new(
                        (lauf_start * 4096) as u64,
                    )));
                }
            } else {
                lauf_laenge = 0;
            }
        }
        None
    }

    /// Markiert einen Frame wieder als frei — O(1).
    fn freigeben(&mut self, frame: PhysFrame) {
        let index = (frame.start_address().as_u64() / 4096) as usize;
        assert!(index < self.frames_gesamt, "Frame ausserhalb des RAM");
        // Doppel-Freigabe ist ein Kernel-Bug: sofort laut scheitern,
        // statt still Speicher doppelt zu vergeben.
        assert!(
            !self.ist_frei(index),
            "Doppelte Freigabe von Frame {:?}",
            frame
        );
        self.bitmap[index / 64] |= 1 << (index % 64);
        self.frames_frei += 1;
        // Zeiger zurücksetzen: So kommt ein gerade freigegebener Frame
        // bei der nächsten Allokation sofort wieder dran.
        if index < self.naechster {
            self.naechster = index;
        }
    }
}

// unsafe impl: Vertrag des Traits — wir geben nie denselben Frame
// doppelt heraus. Garantiert durch die Bitmap: allozieren löscht das
// Frei-Bit, und nur freigeben (mit Doppel-Freigabe-Check) setzt es.
unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        self.allozieren()
    }
}

impl FrameDeallocator<Size4KiB> for BitmapFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        self.freigeben(frame);
    }
}
