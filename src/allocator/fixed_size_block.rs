// allocator/fixed_size_block.rs — Der Fixed-Size-Block-Allocator
//
// Funktionsweise: Statt beliebig große Stücke zu verwalten, gibt es
// nur wenige feste Blockgrößen (8, 16, 32, ... 2048 Bytes). Für jede
// Größe führen wir eine eigene Liste freier Blöcke:
//
//   list_heads[8]:    [Block] -> [Block] -> None
//   list_heads[16]:   [Block] -> None
//   list_heads[32]:   None
//   ...
//
// Eine Anfrage wird auf die nächstpassende Größe AUFGERUNDET
// (13 Bytes -> 16er-Block). Der Clou: Die Verwaltung freier Blöcke
// kostet keinen extra Speicher, denn der Zeiger auf den nächsten
// freien Block wird IN den freien Block selbst geschrieben — der ist
// ja gerade unbenutzt!
//
// Vorteile:
//   + Sehr schnell: Liste vorne kürzen/verlängern, O(1) — kein Suchen
//     wie beim Linked-List-Allocator, dessen Liste bei vielen
//     verschieden großen Löchern lang und langsam werden kann.
//   + Freigegebene Blöcke werden sofort wiederverwendet.
//
// Nachteile:
//   - Interner Verschnitt: Wer 65 Bytes braucht, bekommt 128 —
//     fast die Hälfte verschwendet.
//   - Für Anfragen über 2048 Bytes brauchen wir einen Fallback
//     (hier: der linked_list_allocator).
//
// Echte Systeme (z. B. der Slab-Allocator in Linux) verfeinern genau
// dieses Prinzip.

use super::Locked;
use core::alloc::{GlobalAlloc, Layout};
use core::{mem, ptr, ptr::NonNull};

/// Die verfügbaren Blockgrößen. Alle Zweierpotenzen, damit die
/// Blockgröße gleichzeitig als Alignment taugt. Kein Eintrag unter 8:
/// Jeder freie Block muss einen Zeiger (8 Bytes) aufnehmen können.
const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048];

/// Ein Knoten der Frei-Liste — liegt IM freien Block selbst.
struct ListNode {
    next: Option<&'static mut ListNode>,
}

/// Findet den Index der kleinsten Blockgröße, die für `layout` reicht
/// (Größe UND Alignment), oder None für "zu groß -> Fallback".
fn list_index(layout: &Layout) -> Option<usize> {
    let required_block_size = layout.size().max(layout.align());
    BLOCK_SIZES.iter().position(|&s| s >= required_block_size)
}

pub struct FixedSizeBlockAllocator {
    /// Pro Blockgröße der Kopf der Liste freier Blöcke.
    list_heads: [Option<&'static mut ListNode>; BLOCK_SIZES.len()],
    /// Fallback für Allokationen über 2048 Bytes — und Quelle für
    /// frische Blöcke, wenn eine Frei-Liste leer ist.
    fallback_allocator: linked_list_allocator::Heap,
}

impl FixedSizeBlockAllocator {
    pub const fn new() -> Self {
        const EMPTY: Option<&'static mut ListNode> = None;
        FixedSizeBlockAllocator {
            list_heads: [EMPTY; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// `unsafe`: wie immer — der Bereich muss gültig und exklusiv sein.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.fallback_allocator.init(heap_start, heap_size);
    }

    /// Allokation über den Fallback-Allocator.
    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => ptr::null_mut(),
        }
    }
}

unsafe impl GlobalAlloc for Locked<FixedSizeBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => {
                // Passende Frei-Liste vorhanden?
                match allocator.list_heads[index].take() {
                    Some(node) => {
                        // Ja: ersten Block aus der Liste nehmen (O(1)!).
                        allocator.list_heads[index] = node.next.take();
                        node as *mut ListNode as *mut u8
                    }
                    None => {
                        // Nein: frischen Block der festen Größe vom
                        // Fallback holen. Beim späteren dealloc landet
                        // er dann in unserer Frei-Liste.
                        let block_size = BLOCK_SIZES[index];
                        let block_align = block_size;
                        let layout = Layout::from_size_align(block_size, block_align).unwrap();
                        allocator.fallback_alloc(layout)
                    }
                }
            }
            // Zu groß für unsere Blockgrößen -> direkt zum Fallback.
            None => allocator.fallback_alloc(layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => {
                // Block vorne in die passende Frei-Liste einhängen.
                // Der Zeiger auf den bisherigen Kopf wandert IN den
                // freigegebenen Block (der ist jetzt ja unbenutzt).
                let new_node = ListNode {
                    next: allocator.list_heads[index].take(),
                };
                // Sicherstellen, dass der ListNode in den Block passt:
                assert!(mem::size_of::<ListNode>() <= BLOCK_SIZES[index]);
                assert!(mem::align_of::<ListNode>() <= BLOCK_SIZES[index]);
                let new_node_ptr = ptr as *mut ListNode;
                new_node_ptr.write(new_node);
                allocator.list_heads[index] = Some(&mut *new_node_ptr);
            }
            None => {
                // Kam vom Fallback -> geht zurück an den Fallback.
                let ptr = NonNull::new(ptr).unwrap();
                allocator.fallback_allocator.deallocate(ptr, layout);
            }
        }
    }
}
