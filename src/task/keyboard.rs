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

use crate::{print, println};
use conquer_once::spin::OnceCell;
use core::{
    pin::Pin,
    task::{Context, Poll},
};
use crossbeam_queue::ArrayQueue;
use futures_util::{stream::Stream, stream::StreamExt, task::AtomicWaker};

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

/// Der Tastatur-Task: läuft "ewig" im Executor und verarbeitet
/// jeden Scancode — Dekodierung wie gehabt mit deutschem QWERTZ.
pub async fn print_keypresses() {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};

    let mut scancodes = ScancodeStream::new();
    let mut keyboard = Keyboard::new(
        ScancodeSet1::new(),
        layouts::De105Key,
        HandleControl::Ignore,
    );

    // `while let + .next().await`: schläft bei leerer Queue,
    // ohne einen einzigen CPU-Zyklus zu verschwenden.
    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(key_event) {
                match key {
                    // Backspace/Entf: löschen (siehe interrupts.rs früher)
                    DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                        print!("\u{8} \u{8}")
                    }
                    DecodedKey::Unicode(zeichen) => print!("{}", zeichen),
                    DecodedKey::RawKey(pc_keyboard::KeyCode::Delete) => {
                        print!("\u{8} \u{8}")
                    }
                    DecodedKey::RawKey(taste) => print!("{:?}", taste),
                }
            }
        }
    }
}
