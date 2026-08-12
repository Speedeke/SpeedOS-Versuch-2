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
use core::sync::atomic::{AtomicU64, Ordering};

static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();

/// Wie viele Tasten verlorengingen, weil die Queue voll war.
///
/// Wird NUR gezaehlt, nie gedruckt — siehe `add_scancode`.
static VERLORENE: AtomicU64 = AtomicU64::new(0);

/// Wie viele Scancodes noch unverarbeitet warten.
///
/// ===================================================================
/// DIE ZAHL, DIE RUECKSTAU SICHTBAR MACHT
///
/// Sie ist die Grundlage fuer den Gegendruck bei der Tastatur-
/// wiederholung (`usb::xhci::hid_wiederholungen`): Wer noch nicht
/// verarbeitet hat, was er schon bekommen hat, braucht nicht mehr.
pub fn wartende_scancodes() -> usize {
    SCANCODE_QUEUE.try_get().map(|q| q.len()).unwrap_or(0)
}

/// Wie viele Tasten wegen voller Queue verlorengingen (Diagnose).
pub fn verlorene_tasten() -> u64 {
    VERLORENE.load(Ordering::Relaxed)
}

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
            // KEIN `println!` HIER. Es war eines von zwei Dingen, die
            // auf dem Laptop eine Rueckkopplung erzeugt haben: Die
            // Queue laeuft genau dann ueber, wenn das System schon
            // ueberlastet ist — und `println!` rendert auf den
            // Framebuffer, kostet also ausgerechnet in diesem Moment am
            // meisten. Jede verlorene Taste erzeugte so noch mehr Last,
            // wodurch die naechste erst recht verlorenging.
            //
            // Gezaehlt wird trotzdem: `verlorene_tasten()` sagt es dem,
            // der danach fragt (Diagnose), ohne selbst Last zu
            // erzeugen.
            VERLORENE.fetch_add(1, Ordering::Relaxed);
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
        // Idempotent über queue_bereitstellen(): Die Queue wird schon
        // FRÜH im Boot angelegt (für die D-Taste des Diagnose-Modus),
        // also darf der spätere Aufruf aus dem Tastatur-Task sie
        // einfach vorfinden.
        queue_bereitstellen();
        ScancodeStream { _private: () }
    }
}

/// Legt die Scancode-Queue an, falls das noch nicht geschehen ist.
/// IDEMPOTENT — darf mehrfach gerufen werden: einmal ganz früh in
/// kernel_main (damit die D-Taste auf dem Bootscreen nicht verloren
/// geht), später erneut aus ScancodeStream::new(). Nur aus Task-
/// Kontext aufrufen (alloziert!), NIE aus einem Interrupt-Handler.
pub fn queue_bereitstellen() {
    let _ = SCANCODE_QUEUE.try_init_once(|| ArrayQueue::new(100));
}

/// Prüft (und leert dabei die Queue), ob beim Boot die Taste D
/// gedrückt wurde — der Auslöser für den Diagnose-Modus auf echter
/// Hardware. 'D' hat im Scancode-Set 1 den Make-Code 0x20 (ein
/// PHYSISCHER Code, layout-unabhängig — die D-Taste liegt bei QWERTZ
/// wie bei QWERTY an derselben Stelle). Am Boot verworfene Scancodes
/// sind egal; der eigentliche Eingabe-Betrieb beginnt erst später.
pub fn diagnose_taste_beim_boot() -> bool {
    let queue = match SCANCODE_QUEUE.try_get() {
        Ok(q) => q,
        Err(_) => return false,
    };
    let mut gefunden = false;
    while let Some(scancode) = queue.pop() {
        if scancode == 0x20 {
            gefunden = true;
        }
    }
    gefunden
}

/// Wurde IRGENDEINE Taste gedrückt (Make-Code)? Leert dabei die Queue.
/// Für den „Taste zum Fortfahren"-Moment am Ende des Diagnose-Schirms.
/// Make-Codes sind im Set 1 < 0x80 (Break-Codes haben Bit 7 gesetzt).
pub fn boot_taste_vorhanden() -> bool {
    let queue = match SCANCODE_QUEUE.try_get() {
        Ok(q) => q,
        Err(_) => return false,
    };
    let mut gedrueckt = false;
    while let Some(scancode) = queue.pop() {
        if scancode < 0x80 {
            gedrueckt = true;
        }
    }
    gedrueckt
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
    /// Ist gerade eine Alt-Taste gedrückt? Für Alt+Tab.
    alt_gedrueckt: bool,
}

#[allow(clippy::new_without_default)] // siehe ScancodeStream
impl KeyStream {
    pub fn new() -> Self {
        KeyStream {
            scancodes: ScancodeStream::new(),
            // MapLettersToUnicode: Strg+Buchstabe wird zum Steuer-
            // zeichen (Strg+C = U+0003, X = U+0018, V = U+0016) —
            // so erkennen Apps die Kürzel; normale Eingaben (>= ' ')
            // sind davon unberührt.
            keyboard: Keyboard::new(
                ScancodeSet1::new(),
                layouts::De105Key,
                HandleControl::MapLettersToUnicode,
            ),
            alt_gedrueckt: false,
        }
    }
}

impl Stream for KeyStream {
    type Item = DecodedKey;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<DecodedKey>> {
        use pc_keyboard::{KeyCode, KeyState};

        let this = self.get_mut();
        // Solange Scancodes da sind: dekodieren. Nicht jeder Scancode
        // ergibt eine Taste (z. B. "Taste losgelassen" oder der erste
        // Teil einer E0-Sequenz bei Pfeiltasten) — dann weiterlesen.
        while let Poll::Ready(Some(scancode)) = Pin::new(&mut this.scancodes).poll_next(cx) {
            if let Ok(Some(key_event)) = this.keyboard.add_byte(scancode) {
                // ALT+TAB-Abgriff VOR dem Dekodieren: Der Fenster-
                // Switcher braucht Tastendruck UND -loslassen sowie den
                // Alt-Status — das gibt eine dekodierte Taste nicht her.
                let code = key_event.code;
                let herunter = key_event.state == KeyState::Down;
                if matches!(code, KeyCode::LAlt | KeyCode::RAltGr) {
                    this.alt_gedrueckt = herunter;
                    // Alt losgelassen: den Fensterwechsel bestätigen.
                    if !herunter {
                        crate::fenster::switcher_bestaetigen();
                    }
                    // Nicht als normale Taste weiterreichen.
                    let _ = this.keyboard.process_keyevent(key_event);
                    continue;
                }
                if this.alt_gedrueckt
                    && code == KeyCode::Tab
                    && herunter
                    && crate::fenster::desktop_aktiv()
                {
                    crate::fenster::switcher_weiter();
                    let _ = this.keyboard.process_keyevent(key_event);
                    continue;
                }
                // Super-/Windows-Taste: öffnet/schließt das Startmenü
                // (nur im Desktop-Modus; in der Konsole ignoriert).
                if matches!(code, KeyCode::LWin | KeyCode::RWin) {
                    if herunter && crate::fenster::desktop_aktiv() {
                        crate::fenster::startmenue_umschalten();
                    }
                    let _ = this.keyboard.process_keyevent(key_event);
                    continue;
                }

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
