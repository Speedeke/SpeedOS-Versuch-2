// serial.rs — Treiber für die serielle Schnittstelle (COM1, Port 0x3F8)
//
// Die serielle Schnittstelle ist unser wichtigster Debug-Kanal:
// QEMU leitet alles, was der Kernel hier hineinschreibt, direkt in
// unser Terminal um (Option "-serial stdio"). So sehen wir Ausgaben
// auch dann, wenn der Bildschirm nichts anzeigt — und Testergebnisse
// landen automatisch in der Konsole.
//
// Projektregel: ALLE Debug-Ausgaben laufen über diesen Kanal,
// zusätzlich zur VGA-Ausgabe. Niemals nur VGA!
//
// Auch dieser Treiber ist als eigenes Modul isoliert (Mikrokernel-Prinzip).

use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

lazy_static! {
    /// Der globale serielle Port (COM1).
    /// 0x3F8 ist die Standard-I/O-Portadresse für COM1 auf jedem PC.
    /// `unsafe`: Wir versprechen, dass an diesem Port wirklich ein
    /// serieller UART-Chip hängt (bei QEMU und echten PCs garantiert).
    pub static ref SERIAL1: Mutex<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3F8) };
        serial_port.init();
        Mutex::new(serial_port)
    };
}

/// Interne Hilfsfunktion für die Makros — bitte nicht direkt aufrufen.
///
/// Deadlock-Schutz wie beim VGA-Writer: keine Interrupts, solange wir
/// den Lock auf den seriellen Port halten (siehe vga_buffer::_print).
#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .write_fmt(args)
            .expect("Ausgabe an seriellen Port fehlgeschlagen");
    });
}

/// Gibt formatierten Text über die serielle Schnittstelle aus.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
    };
}

/// Wie serial_print!, aber mit Zeilenumbruch am Ende.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
