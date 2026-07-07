// allocator.rs — Der Kernel-Heap: dynamischer Speicher für Box, Vec & Co.
//
// Bisher konnte SpeedOS nur mit Speicher arbeiten, dessen Größe zur
// Compile-Zeit feststeht (Statics, Stack). Ein Heap erlaubt Allokationen
// zur Laufzeit: "gib mir jetzt 253 Bytes". Dafür brauchen wir zwei Dinge:
//
//  1. Einen Speicherbereich: Wir reservieren 100 KiB an einer festen
//     virtuellen Adresse und mappen die Pages mit unserem Mapper und
//     FrameAllocator aus Teil 1 auf echte physische Frames.
//  2. Einen Allocator: Der verwaltet diesen Bereich und entscheidet,
//     wer welches Stück bekommt. Rust verlangt dafür das
//     GlobalAlloc-Trait und das #[global_allocator]-Attribut — danach
//     funktionieren Box, Vec, String, BTreeMap (aus dem alloc-Crate)
//     wie gewohnt.
//
// SpeedOS hat DREI Allocator-Implementierungen zum Vergleichen:
//   - linked_list_allocator (Crate, Standard): verkette Liste freier
//     Blöcke — flexibel, wiederverwendet Speicher, guter Allrounder.
//   - BumpAllocator (--features bump-allocator): nur ein Zeiger, der
//     vorwärts wandert — rasend schnell, kann aber einzelne
//     Freigaben nicht wiederverwenden. Siehe allocator/bump.rs.
//   - FixedSizeBlockAllocator (--features fixed-block-allocator):
//     vorsortierte Blockgrößen-Listen — sehr schnelle Allokation,
//     dafür Speicher-Verschnitt. Siehe allocator/fixed_size_block.rs.

pub mod bump;
pub mod fixed_size_block;

use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

/// Startadresse des Heaps im virtuellen Adressraum. Die Adresse ist
/// frei gewählt — wichtig ist nur, dass dort noch nichts anderes liegt.
pub const HEAP_START: usize = 0x_4444_4444_0000;
/// Größe des Heaps: 100 KiB (= 25 Pages à 4 KiB).
pub const HEAP_SIZE: usize = 100 * 1024;

/// Mappt die Heap-Pages auf frische physische Frames und übergibt den
/// Bereich danach an den Allocator. Ab dann funktionieren Box & Co.
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    // Alle Pages von HEAP_START bis HEAP_START + HEAP_SIZE - 1:
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    // Für jede Page einen freien Frame besorgen und mappen:
    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe {
            // unsafe: siehe create_example_mapping — hier unbedenklich,
            // die Frames sind frisch und gehören niemand anderem.
            mapper.map_to(page, frame, flags, frame_allocator)?.flush()
        };
    }

    // Dem Allocator seinen Arbeitsbereich geben.
    // unsafe: Der Bereich muss gültig gemappt und exklusiv sein —
    // haben wir gerade beides sichergestellt.
    unsafe {
        ALLOCATOR.lock().init(HEAP_START, HEAP_SIZE);
    }

    Ok(())
}

/// Liefert (belegt, frei) in Bytes — für den meminfo-Shell-Befehl.
/// Nur der Standard-Allocator führt diese Statistik; die beiden
/// Lern-Allocatoren geben None zurück.
#[cfg(not(any(feature = "bump-allocator", feature = "fixed-block-allocator")))]
pub fn heap_statistik() -> Option<(usize, usize)> {
    let heap = ALLOCATOR.lock();
    Some((heap.used(), heap.free()))
}

#[cfg(any(feature = "bump-allocator", feature = "fixed-block-allocator"))]
pub fn heap_statistik() -> Option<(usize, usize)> {
    None
}

// ---------------------------------------------------------------------------
// Auswahl des globalen Allocators (per Cargo-Feature)
// ---------------------------------------------------------------------------

/// Standard: der bewährte linked_list_allocator aus der Community.
#[cfg(not(any(feature = "bump-allocator", feature = "fixed-block-allocator")))]
#[global_allocator]
static ALLOCATOR: linked_list_allocator::LockedHeap = linked_list_allocator::LockedHeap::empty();

/// Alternative 1: unser eigener Bump-Allocator (zum Lernen).
#[cfg(feature = "bump-allocator")]
#[global_allocator]
static ALLOCATOR: Locked<bump::BumpAllocator> = Locked::new(bump::BumpAllocator::new());

/// Alternative 2: unser eigener Fixed-Size-Block-Allocator (zum Lernen).
#[cfg(feature = "fixed-block-allocator")]
#[global_allocator]
static ALLOCATOR: Locked<fixed_size_block::FixedSizeBlockAllocator> =
    Locked::new(fixed_size_block::FixedSizeBlockAllocator::new());

// ---------------------------------------------------------------------------
// Hilfskonstruktionen für unsere eigenen Allocatoren
// ---------------------------------------------------------------------------

/// Wrapper um spin::Mutex. Nötig, weil GlobalAlloc-Methoden nur &self
/// bekommen, wir aber &mut auf den Allocator brauchen — und weil wir
/// Traits nicht direkt für fremde Typen wie spin::Mutex implementieren
/// dürfen (Orphan Rule).
pub struct Locked<A> {
    inner: spin::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked {
            inner: spin::Mutex::new(inner),
        }
    }

    /// (`'_`: Der Guard borgt sich den Mutex aus &self.)
    pub fn lock(&self) -> spin::MutexGuard<'_, A> {
        self.inner.lock()
    }
}

/// Rundet `addr` auf die nächste `align`-Grenze auf.
/// Trick: funktioniert nur, weil `align` immer eine Zweierpotenz ist
/// (das garantiert Rusts Layout-Typ). Beispiel: align_up(13, 8) = 16.
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
