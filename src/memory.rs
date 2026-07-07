// memory.rs — Speicherverwaltung, Teil 1: Paging
//
// Grundidee von Paging: Programme arbeiten NIE direkt mit echten
// RAM-Adressen (physisch), sondern mit virtuellen Adressen. Die CPU
// übersetzt jeden einzelnen Speicherzugriff über die Page Tables —
// eine 4-stufige Tabellen-Hierarchie (Level 4 bis Level 1), deren
// Wurzel im CPU-Register CR3 steht. Übersetzt wird in Blöcken von
// 4096 Bytes: virtuelle Blöcke heißen "Pages", physische "Frames".
//
// Henne-Ei-Problem: Die Page Tables selbst liegen im physischen
// Speicher — aber wir können ja nur noch über virtuelle Adressen
// zugreifen! Die Lösung liefert der Bootloader (Feature
// "map_physical_memory"): Er mappt den KOMPLETTEN physischen Speicher
// zusätzlich an eine hohe virtuelle Adresse. Physische Adresse X ist
// damit immer unter der virtuellen Adresse X + offset erreichbar.

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PhysFrame, Size4KiB,
    },
    PhysAddr, VirtAddr,
};

/// Initialisiert eine OffsetPageTable — unsere Schnittstelle zum
/// Lesen und Verändern der aktiven Page Tables. Sie kann u. a.
/// virtuelle in physische Adressen übersetzen (translate_addr aus
/// dem Translate-Trait) und neue Mappings anlegen (map_to).
///
/// `unsafe`: Der Aufrufer muss garantieren, dass der komplette
/// physische Speicher wirklich bei `physical_memory_offset` gemappt
/// ist (macht unser Bootloader) — und die Funktion darf nur EINMAL
/// aufgerufen werden, sonst gäbe es mehrere &mut auf dieselbe Tabelle.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

/// Liefert eine Referenz auf die aktive Level-4-Page-Table.
///
/// Die CPU verrät uns in CR3, in welchem physischen Frame die Wurzel
/// der Tabellen-Hierarchie liegt. Dank des Komplett-Mappings können
/// wir sie unter (physische Adresse + Offset) direkt anfassen.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    &mut *page_table_ptr // unsafe: Zeiger ist dank Bootloader-Mapping gültig
}

/// Demo-Funktion: Mappt die übergebene virtuelle Page auf den
/// VGA-Frame (physisch 0xb8000). Danach kann man über diese Page
/// auf den Bildschirm schreiben — der Beweis, dass unser Mapping
/// funktioniert und mehrere virtuelle Adressen auf denselben
/// physischen Speicher zeigen können.
pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    use x86_64::structures::paging::PageTableFlags as Flags;

    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = Flags::PRESENT | Flags::WRITABLE;

    // `unsafe`: map_to kann bei falscher Benutzung Aliasing-Chaos
    // anrichten (z. B. denselben Frame doppelt als normalen Speicher
    // mappen). Für den VGA-Frame in dieser Demo ist das unbedenklich.
    // Der frame_allocator wird gebraucht, falls für das Mapping neue
    // Tabellen der Level 3-1 angelegt werden müssen.
    let map_to_result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    // flush(): den TLB-Cache der CPU für diese Page leeren, damit die
    // Änderung sofort gilt und nicht ein alter Eintrag gewinnt.
    map_to_result.expect("map_to fehlgeschlagen").flush();
}

/// Ein FrameAllocator, der freie physische Frames aus der Memory Map
/// des Bootloaders vergibt.
///
/// Die Memory Map ist die Landkarte des RAM, die der Bootloader vom
/// BIOS erfragt hat: welche Bereiche nutzbar sind und welche belegt
/// (Kernel, Bootloader, Hardware-Löcher, ...). Wir vergeben der Reihe
/// nach die als "Usable" markierten Frames und merken uns nur, wie
/// viele schon weg sind (`next`) — simpel, aber völlig ausreichend,
/// bis wir einen richtigen Allocator bauen.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// `unsafe`: Der Aufrufer garantiert, dass die Memory Map stimmt
    /// und die "Usable"-Frames wirklich niemand anderes benutzt.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }

    /// Alle freien Frames als Iterator: nutzbare Regionen heraussuchen,
    /// in 4096-Byte-Schritte zerlegen, Startadressen zu Frames machen.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions.filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable_regions.map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));
        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

// `unsafe impl`: Wir versprechen, dass allocate_frame nie denselben
// Frame zweimal vergibt — garantiert durch den hochzählenden Index.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}
