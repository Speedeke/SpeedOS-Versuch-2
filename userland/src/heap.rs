// heap.rs — Der Heap eines SpeedOS-Programms (Serie 7, Teil 3)
//
// ==========================================================================
// WARUM ES DEN BIS JETZT NICHT GAB
//
// `libspeed` war bis hierher ALLOKATIONSFREI, und das war kein Mangel,
// sondern eine Haltung: feste Puffer, `.bss`, klare Obergrenzen. `hallo`,
// `kopiere`, `netzhole`, `zertifikate` — alle kommen ohne aus, und sie sind
// dadurch berechenbar.
//
// `rustls` kommt nicht ohne aus. Ein TLS-Handshake baut Zertifikatsketten,
// Puffer und Zustandsobjekte auf, deren Groesse erst zur Laufzeit feststeht;
// die Bibliothek verlangt `alloc` ohne Alternative. Also gibt es jetzt einen
// Heap — aber einen, der die Haltung mitnimmt: feste Obergrenze, waechst nur
// auf Anforderung, und wenn er voll ist, ist er voll (kein Auslagern, kein
// Ueberbuchen).
//
// ==========================================================================
// WIE ER WAECHST
//
//   alloc() findet keinen Platz
//        |
//        v
//   SYS_SPEICHER(bytes)  ---> der Kernel mappt Seiten HINTER dem bisherigen
//        |                     Heap-Ende (prozess::HEAP_START + heap_bytes)
//        v
//   `extend` des Allocators  ---> der neue Bereich ist LUECKENLOS
//                                 anschliessend, also ein Bereich, nicht zwei
//
// Dass der Kernel immer direkt hinter dem bisherigen Ende mappt, ist der
// Grund, warum ein einziger `linked_list_allocator` reicht: Es gibt genau
// EINEN zusammenhaengenden Heap, der nach oben waechst — dasselbe Modell wie
// `brk` unter Unix, und dasselbe wie `allocator::heap_erweitern` im Kernel.
//
// FREIGEGEBEN WIRD NIE an den Kernel zurueck. Ein Prozess gibt Seiten nicht
// einzeln her; sein Adressraum faellt beim Prozess-Ende als Ganzes (Serie 6,
// Teil 3). Der Allocator verwaltet innerhalb dessen, was er bekommen hat.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};
use linked_list_allocator::Heap;
use spin::Mutex;

/// Wie viel beim ERSTEN Mal geholt wird. 64 KiB decken alles ab, was unsere
/// bisherigen Programme brauchen wuerden; rustls waechst dann von selbst.
const ERSTE_ANFORDERUNG: usize = 64 * 1024;
/// Mindest-Zuwachs je Erweiterung. Einzelne Seiten nachzufordern waere ein
/// Syscall je 4 KiB — bei einem Handshake sind das hunderte.
const MIN_ZUWACHS: usize = 64 * 1024;

/// Der Allocator. `spin::Mutex`, weil `GlobalAlloc` `Sync` verlangt — ein
/// SpeedOS-Prozess hat nur einen Ausfuehrungsstrang, der Lock ist also nie
/// umkaempft und kostet ein `lock cmpxchg`.
pub struct SpeedHeap {
    innen: Mutex<Heap>,
}

impl SpeedHeap {
    pub const fn neu() -> SpeedHeap {
        SpeedHeap {
            innen: Mutex::new(Heap::empty()),
        }
    }

    /// Holt `bytes` beim Kernel und haengt sie an den Heap an.
    ///
    /// Liefert `false`, wenn der Kernel ablehnt (Obergrenze erreicht oder
    /// kein Speicher) — dann liefert `alloc` einen Nullzeiger, und Rust ruft
    /// den `alloc_error_handler`. Das ist der ehrliche Weg: Ein Programm,
    /// dem der Speicher ausgeht, soll das merken.
    fn nachfordern(&self, bytes: usize) -> bool {
        let bytes = bytes.max(MIN_ZUWACHS);
        let basis = match crate::speicher_anfordern(bytes as u64) {
            Ok(basis) => basis,
            Err(_) => return false,
        };
        let mut heap = self.innen.lock();
        if heap.size() == 0 {
            // Der erste Bereich: initialisieren.
            // Sicherheit: Der Kernel hat genau diesen Bereich soeben fuer
            // UNS gemappt (`SYS_SPEICHER`), er ist beschreibbar, genullt und
            // gehoert niemandem sonst.
            unsafe { heap.init(basis as *mut u8, bytes) };
        } else {
            // Sicherheit: `SYS_SPEICHER` mappt IMMER direkt hinter dem
            // bisherigen Heap-Ende — der neue Bereich schliesst also
            // lueckenlos an, und genau das verlangt `extend`.
            unsafe { heap.extend(bytes) };
        }
        true
    }
}

// Sicherheit: Alle Zugriffe laufen ueber den Mutex; die Zeiger stammen aus
// dem Heap, den der Kernel uns gemappt hat.
unsafe impl GlobalAlloc for SpeedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // ==================================================================
        // ACHTUNG, HIER LAG EIN DEADLOCK — und zwar ein lehrreicher:
        //
        //     if let Ok(z) = self.innen.lock().allocate_first_fit(layout) {
        //         hoechststand_merken(&self.innen);   // <- lockt ERNEUT
        //
        // Der `MutexGuard` aus der `if let`-Bedingung ist ein TEMPORARY, und
        // Temporaries einer `if let`-Bedingung leben bis zum ENDE DES
        // BLOCKS — nicht bis zum Ende der Bedingung. Der Lock war also noch
        // gehalten, und ein Spinlock, der auf sich selbst wartet, dreht sich
        // fuer immer. Der Spike blieb nach der ersten Zeile stehen.
        //
        // Deshalb wird der Lock jetzt GENAU EINMAL genommen und der
        // Hoechststand aus demselben Guard gelesen.
        // ==================================================================
        {
            let mut heap = self.innen.lock();
            if let Ok(zeiger) = heap.allocate_first_fit(layout) {
                HOECHSTSTAND.fetch_max(heap.used(), Ordering::Relaxed);
                return zeiger.as_ptr();
            }
        }
        let gebraucht = layout.size() + layout.align();
        if !self.nachfordern(gebraucht.max(ERSTE_ANFORDERUNG)) {
            return null_mut();
        }
        let mut heap = self.innen.lock();
        match heap.allocate_first_fit(layout) {
            Ok(zeiger) => {
                HOECHSTSTAND.fetch_max(heap.used(), Ordering::Relaxed);
                zeiger.as_ptr()
            }
            Err(_) => null_mut(),
        }
    }

    unsafe fn dealloc(&self, zeiger: *mut u8, layout: Layout) {
        if let Some(nn) = core::ptr::NonNull::new(zeiger) {
            // Sicherheit: `zeiger`/`layout` stammen laut GlobalAlloc-Vertrag
            // aus einem frueheren `alloc` desselben Allocators.
            unsafe { self.innen.lock().deallocate(nn, layout) };
        }
    }
}

/// Der HOECHSTSTAND der Belegung seit Programmstart.
static HOECHSTSTAND: AtomicUsize = AtomicUsize::new(0);

// WARUM DER HOECHSTSTAND UND NICHT DER ENDSTAND: Der Endstand einer Messung
// ist fast immer klein — der Code hat seine Zwischenergebnisse laengst
// freigegeben. Wer wissen will, wie viel Heap ein Programm BRAUCHT, muss die
// SPITZE kennen; sie bestimmt, ob es in die Obergrenze passt. Genau diese
// Zahl hat die erste Fassung des Spikes verschwiegen (sie meldete 16 Byte,
// obwohl 128 KiB gemappt waren). Fortgeschrieben wird sie in `alloc`, unter
// dem Lock, den wir ohnehin schon halten.

/// Wie viel Heap ist belegt, wie viel gemappt, und was war die SPITZE?
pub fn heap_stand() -> (usize, usize, usize) {
    let heap = ALLOCATOR.innen.lock();
    (
        heap.used(),
        heap.size(),
        HOECHSTSTAND.load(Ordering::Relaxed),
    )
}

#[global_allocator]
pub static ALLOCATOR: SpeedHeap = SpeedHeap::neu();

/// Was passiert, wenn der Speicher ausgeht.
///
/// KEIN stiller Nullzeiger und kein Weiterlaufen: Das Programm meldet es auf
/// den Diagnose-Kanal und beendet sich mit einem eigenen Code. Ein Programm,
/// das nach einer fehlgeschlagenen Allokation weitermacht, produziert
/// Folgefehler, die niemand mehr auf die Ursache zurueckfuehrt.
#[alloc_error_handler]
fn speicher_alle(layout: Layout) -> ! {
    let (belegt, gesamt, spitze) = heap_stand();
    crate::diagnoseln!(
        "[libspeed] SPEICHER ALLE: {} Byte (Ausrichtung {}) nicht bekommen; \
         Heap {} von {} Byte belegt, Spitze {} Byte.",
        layout.size(),
        layout.align(),
        belegt,
        gesamt,
        spitze
    );
    crate::beenden(102)
}
