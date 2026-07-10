// zeit.rs — Die Zeit-API von SpeedOS
//
// Alle Stellen im Kernel, die Zeit brauchen (der künftige blinkende
// Software-Cursor, Timeouts, Uptime-Anzeigen), fragen NUR dieses
// Modul — niemals direkt den Tick-Zähler des Interrupt-Handlers.
// Diese API-Naht ist der Punkt: Wenn wir später vom groben PIT auf
// den präzisen APIC-Timer (oder TSC) umsteigen, ändert sich nur die
// Implementierung hier drin, kein einziger Aufrufer.
//
// Aktuelle Zeitquelle: der PIT (Programmable Interval Timer), wie ihn
// das BIOS konfiguriert. Er läuft mit 1.193.182 Hz und teilt durch
// 65.536 -> ~18,2065 Interrupts pro Sekunde, also ~54,93 ms pro Tick.
// Das ist grob (Auflösung ~55 ms!), aber ehrlich dokumentiert und
// für Uptime/Cursor-Blinken völlig ausreichend.

/// Die Basisfrequenz des PIT-Chips in Hz (Quarz seit dem Ur-PC 1981).
const PIT_BASIS_HZ: u64 = 1_193_182;
/// Der Teiler, mit dem das BIOS den PIT konfiguriert (Maximum).
const PIT_TEILER: u64 = 65_536;

/// Timer-Ticks seit dem Boot (~18,2 pro Sekunde).
pub fn ticks() -> u64 {
    crate::interrupts::timer_ticks()
}

/// Millisekunden seit dem Boot — in ~55-ms-Schritten (PIT-Auflösung).
pub fn ms_seit_boot() -> u64 {
    ms_von_ticks(ticks())
}

/// Rechnet Ticks in Millisekunden um (reine Funktion, gut testbar):
/// ms = ticks * Teiler * 1000 / Basisfrequenz  (~54,93 ms pro Tick).
pub fn ms_von_ticks(ticks: u64) -> u64 {
    ticks * (PIT_TEILER * 1000) / PIT_BASIS_HZ
}

// ---------------------------------------------------------------------------
// Async-Warten auf Timer-Ticks (fürs Cursor-Blinken u. Ä.)
//
// Ein async Task darf NICHT in einer Schleife pollen ("ist es schon
// soweit?") — mit yield_now wäre er immer "bereit", der Executor käme
// nie zum Schlafen, die CPU liefe auf 100 %. Stattdessen: Der Task
// deponiert seinen Waker hier, der Timer-Interrupt weckt ihn beim
// nächsten Tick. Zwischen den Ticks schläft die CPU per hlt.
// ---------------------------------------------------------------------------

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use futures_util::task::AtomicWaker;

/// Der Waker des (einen) wartenden Tasks. Ein AtomicWaker reicht:
/// Aktuell wartet nur der Cursor-Blink-Task auf Ticks. (Mehrere
/// Warter bräuchten eine Liste — bauen wir, wenn es soweit ist.)
static TICK_WAKER: AtomicWaker = AtomicWaker::new();

/// Wird vom Timer-Interrupt-Handler gerufen (interrupt-sicher:
/// AtomicWaker::wake ist lock-frei).
pub(crate) fn tick_waker_wecken() {
    TICK_WAKER.wake();
}

/// Future, die beim NÄCHSTEN Timer-Tick fertig wird.
struct NaechsterTick {
    start_ticks: u64,
}

impl Future for NaechsterTick {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if ticks() > self.start_ticks {
            return Poll::Ready(());
        }
        // Waker registrieren, dann NOCHMAL prüfen — schließt die
        // Race Condition, falls der Tick genau dazwischen kam
        // (gleiches Muster wie beim Tastatur-Stream).
        TICK_WAKER.register(cx.waker());
        if ticks() > self.start_ticks {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Wartet asynchron ungefähr `ms` Millisekunden (Auflösung: ~55 ms,
/// die PIT-Tick-Länge — für Cursor-Blinken völlig ausreichend).
pub async fn warte_ms(ms: u64) {
    let ziel = ms_seit_boot() + ms;
    while ms_seit_boot() < ziel {
        NaechsterTick {
            start_ticks: ticks(),
        }
        .await;
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Umrechnung stimmt (Werte von Hand nachgerechnet:
    /// 65.536.000 / 1.193.182 = 54,93 ms pro Tick).
    #[test_case]
    fn test_ms_von_ticks() {
        assert_eq!(ms_von_ticks(0), 0);
        assert_eq!(ms_von_ticks(1), 54);
        assert_eq!(ms_von_ticks(100), 5492);
        // ~18,2 Ticks sollten fast genau 1 Sekunde sein:
        assert_eq!(ms_von_ticks(18), 988);
        assert_eq!(ms_von_ticks(19), 1043);
    }

    /// Die Uhr läuft vorwärts: Nach ein paar hlt-Schlafrunden ist
    /// ms_seit_boot größer als vorher.
    #[test_case]
    fn test_zeit_laeuft_vorwaerts() {
        let vorher = ms_seit_boot();
        for _ in 0..3 {
            x86_64::instructions::hlt();
        }
        assert!(ms_seit_boot() > vorher);
    }
}
