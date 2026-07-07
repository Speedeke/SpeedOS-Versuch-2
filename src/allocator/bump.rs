// allocator/bump.rs — Der Bump-Allocator: der einfachste Allocator der Welt
//
// Funktionsweise: Wir merken uns nur EINEN Zeiger (`next`), der am
// Heap-Anfang startet. Jede Allokation "stößt" (bump) ihn um die
// angeforderte Größe nach vorne:
//
//   Heap:  [####|####|##......................]
//                     ^ next — hier gibt's das nächste Stück
//
// Vorteile:
//   + Extrem schnell: eine Addition, fertig. Kein Suchen.
//   + Winzig wenig Verwaltungsdaten (2 Zahlen).
//
// Der große Nachteil:
//   - deallocate() kann einzelne Blöcke NICHT wiederverwenden!
//     Wir wissen ja nur, wo der Heap endet — nicht, wo Löcher sind.
//     Erst wenn ALLE Allokationen freigegeben wurden (Zähler = 0),
//     springt `next` wieder an den Anfang zurück.
//
// Deshalb taugt ein Bump-Allocator nur für kurzlebige Spezialfälle
// (z. B. "alles pro Frame allokieren, am Frame-Ende alles wegwerfen"),
// nicht als allgemeiner Heap — genau das zeigt unser Wiederverwendungs-
// Test: Mit --features bump-allocator läuft ihm der Speicher aus.

use super::{align_up, Locked};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

pub struct BumpAllocator {
    heap_start: usize,
    heap_end: usize,
    /// Die nächste freie Adresse — wandert bei jeder Allokation vorwärts.
    next: usize,
    /// Wie viele Allokationen gerade "leben". Nur wenn der Zähler auf 0
    /// fällt, dürfen wir den ganzen Heap auf einmal zurücksetzen.
    allocations: usize,
}

impl BumpAllocator {
    /// Erzeugt einen leeren Bump-Allocator (const, damit er in einem
    /// `static` stehen kann — zu dem Zeitpunkt gibt es noch keinen Heap).
    pub const fn new() -> Self {
        BumpAllocator {
            heap_start: 0,
            heap_end: 0,
            next: 0,
            allocations: 0,
        }
    }

    /// Weist dem Allocator seinen Speicherbereich zu.
    /// `unsafe`: Der Aufrufer garantiert, dass [heap_start, heap_start
    /// + heap_size) gültig gemappt ist und exklusiv uns gehört.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;
        self.next = heap_start;
    }
}

unsafe impl GlobalAlloc for Locked<BumpAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut bump = self.lock();

        // Startadresse auf die geforderte Ausrichtung aufrunden
        // (ein u64 will z. B. an durch 8 teilbaren Adressen liegen).
        let alloc_start = align_up(bump.next, layout.align());
        // checked_add: Überlauf abfangen statt falsch weiterzurechnen.
        let alloc_end = match alloc_start.checked_add(layout.size()) {
            Some(end) => end,
            None => return ptr::null_mut(),
        };

        if alloc_end > bump.heap_end {
            // Heap voll — Null zurückgeben löst den alloc_error_handler aus.
            ptr::null_mut()
        } else {
            bump.next = alloc_end;
            bump.allocations += 1;
            alloc_start as *mut u8
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        let mut bump = self.lock();

        // Einzelne Blöcke können wir nicht zurücknehmen — nur zählen.
        bump.allocations -= 1;
        // Alles freigegeben? Dann den ganzen Heap auf Anfang.
        if bump.allocations == 0 {
            bump.next = bump.heap_start;
        }
    }
}
