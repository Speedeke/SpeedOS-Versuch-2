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
