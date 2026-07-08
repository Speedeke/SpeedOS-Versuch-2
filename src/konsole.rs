// konsole.rs — Übergangs-Konsole: Farben & Co. über die serielle Leitung
//
// ACHTUNG, ÜBERGANGSZUSTAND (seit der bootloader-0.11-Migration):
// Der VGA-Textmodus (0xb8000) existiert nicht mehr — wir booten in
// einen Grafikmodus mit linearem Framebuffer. Bis der Framebuffer-
// Text-Renderer gebaut ist (nächster Meilenstein!), ist die serielle
// Schnittstelle unsere einzige Text-Ausgabe.
//
// Dieses Modul behält die gewohnte API (Color, set_color, clear_screen,
// cursor_aktivieren), setzt sie aber als ANSI-Escape-Codes um, die
// jedes Terminal versteht. Die SpeedShell bleibt dadurch voll benutzbar
// — inklusive Farben — nur eben im Terminal statt im QEMU-Fenster.
// Getippt wird weiterhin im QEMU-Fenster (PS/2-Tastatur)!

use crate::serial_print;

/// Die 16 klassischen Konsolen-Farben (Namen wie zu VGA-Zeiten,
/// damit der restliche Code unverändert bleibt).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Blue,
    Green,
    Cyan,
    Red,
    Magenta,
    Brown,
    LightGray,
    DarkGray,
    LightBlue,
    LightGreen,
    LightCyan,
    LightRed,
    Pink,
    Yellow,
    White,
}

impl Color {
    /// Der ANSI-Farbcode für den Vordergrund (Hintergrund = +10).
    fn ansi_code(self) -> u8 {
        match self {
            Color::Black => 30,
            Color::Red => 31,
            Color::Green => 32,
            Color::Brown => 33, // ANSI nennt es "yellow", dunkel = braun
            Color::Blue => 34,
            Color::Magenta => 35,
            Color::Cyan => 36,
            Color::LightGray => 37,
            Color::DarkGray => 90,
            Color::LightRed => 91,
            Color::LightGreen => 92,
            Color::Yellow => 93,
            Color::LightBlue => 94,
            Color::Pink => 95,
            Color::LightCyan => 96,
            Color::White => 97,
        }
    }
}

/// Setzt Vorder- und Hintergrundfarbe für alle folgenden Ausgaben
/// (als ANSI-SGR-Sequenz, z. B. "\x1b[93;44m" = Gelb auf Blau).
pub fn set_color(foreground: Color, background: Color) {
    serial_print!(
        "\x1b[{};{}m",
        foreground.ansi_code(),
        background.ansi_code() + 10
    );
}

/// Leert den Bildschirm (Terminal) und setzt den Cursor nach oben links.
pub fn clear_screen() {
    serial_print!("\x1b[2J\x1b[H");
}

/// Der Terminal-Cursor blinkt von selbst — nichts zu tun.
/// (Kommt mit dem Framebuffer-Renderer als Software-Cursor zurück.)
pub fn cursor_aktivieren() {}
