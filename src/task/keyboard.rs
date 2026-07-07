// task/keyboard.rs — Die Tastatur als async Task
//
// Neue Arbeitsteilung (vorher machte der Interrupt-Handler alles):
//
//   Interrupt-Handler (interrupts.rs):  Scancode lesen -> in die
//       lock-freie Queue werfen -> Waker anstoßen -> FERTIG.
//       So bleibt der Handler winzig kurz — wichtig, denn solange er
//       läuft, ist das ganze System blockiert.
//
//   Async Task (print_keypresses, läuft im Executor): holt Scancodes
//       aus der Queue, dekodiert sie in Ruhe (QWERTZ, Umlaute) und
//       gibt sie aus. Ist die Queue leer, schläft der Task, bis der
//       Waker ihn weckt — kostet dann NULL Rechenzeit.

use crate::println;
use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{stream::Stream, task::AtomicWaker};
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

/// Die Scancode-Warteschlange. OnceCell statt lazy_static, damit die
/// Initialisierung (die allokiert!) GARANTIERT nicht heimlich beim
/// ersten Zugriff im Interrupt-Handler passiert, sondern explizit
/// in ScancodeStream::new().
static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();

/// Der Aufbewahrungsort für den Waker des Tastatur-Tasks.
/// AtomicWaker ist die interrupt-sichere Variante: registrieren und
/// wecken dürfen von verschiedenen "Seiten" gleichzeitig passieren.
static WAKER: AtomicWaker = AtomicWaker::new();

/// Wird vom Tastatur-Interrupt-Handler aufgerufen (interrupts.rs).
///
/// MUSS interrupt-tauglich sein, deshalb: kein Blockieren, kein Locken,
/// kein Allozieren. Queue voll oder nicht initialisiert? Dann geht der
/// Scancode eben verloren — schlimmstenfalls fehlt ein Tastendruck,
/// aber das System bleibt stabil.
pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            println!("WARNUNG: Scancode-Queue voll, Taste verloren");
        } else {
            // Dem Tastatur-Task Bescheid geben: Es gibt was zu tun!
            WAKER.wake();
        }
    } else {
        println!("WARNUNG: Scancode-Queue noch nicht initialisiert");
    }
}

/// Ein Stream (asynchroner Iterator) über die ankommenden Scancodes.
pub struct ScancodeStream {
    /// Privates Feld verhindert, dass man den Stream ohne new() baut.
    _private: (),
}

// Kein Default für ScancodeStream/KeyStream: new() hat einen
// einmaligen Seiteneffekt (Queue-Initialisierung, zweiter Aufruf
// panict absichtlich) — ein "harmlos" aussehendes Default würde
// das verschleiern.
#[allow(clippy::new_without_default)]
impl ScancodeStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE
            .try_init_once(|| ArrayQueue::new(100))
            .expect("ScancodeStream::new darf nur einmal aufgerufen werden");
        ScancodeStream { _private: () }
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE.try_get().expect("Queue nicht initialisiert");

        // Schneller Weg: Es liegt schon etwas bereit.
        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        // Queue leer: Waker deponieren, dann NOCHMAL prüfen.
        // Das zweite pop() schließt eine Race Condition: Kam der
        // Interrupt genau zwischen erstem pop() und register(), hätte
        // sein wake() ins Leere gezielt — wir würden ewig schlafen,
        // obwohl ein Scancode wartet.
        WAKER.register(cx.waker());
        match queue.pop() {
            Some(scancode) => {
                WAKER.take(); // doch Arbeit da -> Waker wird nicht gebraucht
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending, // wirklich leer -> bis zum wake() schlafen
        }
    }
}

/// Ein Stream fertig dekodierter Tasten: Scancodes aus der Queue
/// werden mit dem deutschen QWERTZ-Layout (De105Key) übersetzt.
/// Der Konsument (z. B. die SpeedShell) bekommt entweder ein
/// Unicode-Zeichen (auch ä ö ü ß!) oder eine Roh-Taste (Pfeile, F1...).
pub struct KeyStream {
    scancodes: ScancodeStream,
    keyboard: Keyboard<layouts::De105Key, ScancodeSet1>,
}

#[allow(clippy::new_without_default)] // siehe ScancodeStream
impl KeyStream {
    pub fn new() -> Self {
        KeyStream {
            scancodes: ScancodeStream::new(),
            keyboard: Keyboard::new(
                ScancodeSet1::new(),
                layouts::De105Key,
                HandleControl::Ignore,
            ),
        }
    }
}

impl Stream for KeyStream {
    type Item = DecodedKey;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<DecodedKey>> {
        let this = self.get_mut();
        // Solange Scancodes da sind: dekodieren. Nicht jeder Scancode
        // ergibt eine Taste (z. B. "Taste losgelassen" oder der erste
        // Teil einer E0-Sequenz bei Pfeiltasten) — dann weiterlesen.
        while let Poll::Ready(Some(scancode)) = Pin::new(&mut this.scancodes).poll_next(cx) {
            if let Ok(Some(key_event)) = this.keyboard.add_byte(scancode) {
                if let Some(key) = this.keyboard.process_keyevent(key_event) {
                    return Poll::Ready(Some(key));
                }
            }
        }
        // Queue leer: Der Waker ist durch das poll_next des inneren
        // Streams schon registriert — wir schlafen bis zur Taste.
        Poll::Pending
    }
}
