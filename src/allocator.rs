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

use crate::memory;
use core::sync::atomic::{AtomicUsize, Ordering};
use x86_64::{
    structures::paging::{mapper::MapToError, Page, Size4KiB},
    VirtAddr,
};

/// Startadresse des Heaps im virtuellen Adressraum. Die Adresse ist
/// frei gewählt — wichtig ist nur, dass dort noch nichts anderes liegt.
pub const HEAP_START: usize = 0x_4444_4444_0000;
/// ANFANGS-Größe des Heaps: 100 KiB (= 25 Pages à 4 KiB).
/// Der Heap kann danach mit heap_erweitern() wachsen.
pub const HEAP_SIZE: usize = 100 * 1024;

/// Die aktuelle Heap-Größe in Bytes (wächst durch heap_erweitern).
static HEAP_GROESSE: AtomicUsize = AtomicUsize::new(0);

/// Die aktuelle Größe des Heaps in Bytes.
pub fn heap_groesse() -> usize {
    HEAP_GROESSE.load(Ordering::Relaxed)
}

/// Mappt die Heap-Pages über die globale Speicher-API und übergibt
/// den Bereich an den Allocator. Ab dann funktionieren Box & Co.
/// Voraussetzung: memory::init() ist bereits gelaufen.
pub fn init_heap() -> Result<(), MapToError<Size4KiB>> {
    heap_pages_mappen(HEAP_START, HEAP_SIZE)?;

    // Dem Allocator seinen Arbeitsbereich geben.
    // unsafe: Der Bereich ist frisch gemappt und gehört exklusiv
    // dem Allocator — genau die init-Bedingung.
    unsafe {
        ALLOCATOR.lock().init(HEAP_START, HEAP_SIZE);
    }
    HEAP_GROESSE.store(HEAP_SIZE, Ordering::Relaxed);

    Ok(())
}

/// Erweitert den Heap zur Laufzeit um `zusaetzliche_pages` Pages und
/// gibt die neue Gesamtgröße zurück. Die neuen Pages schließen
/// virtuell nahtlos an den bisherigen Heap an — nötig, damit der
/// Allocator seinen Bereich einfach verlängern kann.
/// (Automatisches Wachsen bei Heap-Voll kommt später; im Moment
/// ruft man die Funktion bewusst auf, z. B. bevor große Puffer
/// gebraucht werden.)
pub fn heap_erweitern(zusaetzliche_pages: usize) -> Result<usize, MapToError<Size4KiB>> {
    let alte_groesse = HEAP_GROESSE.load(Ordering::Relaxed);
    let zusatz = zusaetzliche_pages * 4096;

    heap_pages_mappen(HEAP_START + alte_groesse, zusatz)?;

    // unsafe: Der neue Bereich ist gemappt, exklusiv und schließt
    // direkt an das bisherige Heap-Ende an.
    unsafe {
        ALLOCATOR.lock().extend(zusatz);
    }
    let neue_groesse = alte_groesse + zusatz;
    HEAP_GROESSE.store(neue_groesse, Ordering::Relaxed);

    Ok(neue_groesse)
}

/// Mappt den Bereich [start, start+groesse) Page für Page auf
/// frische Frames (über die globale Speicher-API).
fn heap_pages_mappen(start: usize, groesse: usize) -> Result<(), MapToError<Size4KiB>> {
    let start_page = Page::containing_address(VirtAddr::new(start as u64));
    let end_page = Page::containing_address(VirtAddr::new((start + groesse - 1) as u64));
    for page in Page::range_inclusive(start_page, end_page) {
        memory::map_page(page)?;
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
