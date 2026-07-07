// vga_buffer.rs — Treiber für den VGA-Textmodus (Bildschirmausgabe)
//
// Der VGA-Textmodus ist die einfachste Art, auf einem x86-PC etwas auf den
// Bildschirm zu schreiben: An der physischen Speicheradresse 0xb8000 liegt
// ein Puffer von 25 Zeilen x 80 Spalten. Jede Bildschirmzelle besteht aus
// 2 Bytes: das erste ist das ASCII-Zeichen, das zweite die Farbe
// (4 Bit Vordergrund, 4 Bit Hintergrund).
//
// Dieser Treiber ist bewusst als eigenständiges Modul isoliert
// (Mikrokernel-Prinzip): Der Rest des Kernels benutzt nur die
// print!/println!-Makros und weiß nichts über VGA-Interna.

use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;
use volatile::Volatile;

/// Die 16 Standard-Farben des VGA-Textmodus.
/// `repr(u8)` sorgt dafür, dass jede Variante genau als das Byte
/// gespeichert wird, das die VGA-Hardware erwartet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// Kombiniert Vorder- und Hintergrundfarbe in einem Byte,
/// genau so, wie die VGA-Hardware es erwartet:
/// obere 4 Bit = Hintergrund, untere 4 Bit = Vordergrund.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

/// Eine einzelne Bildschirmzelle: Zeichen + Farbe (zusammen 2 Bytes).
/// `repr(C)` garantiert die Reihenfolge der Felder im Speicher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

/// Größe des VGA-Textpuffers: 25 Zeilen x 80 Spalten.
const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

/// Der VGA-Puffer selbst — ein 2D-Array von Bildschirmzellen.
/// `Volatile` verhindert, dass der Compiler Schreibzugriffe wegoptimiert:
/// Er sieht ja nicht, dass die Grafikkarte den Speicher mitliest!
#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

/// Der Writer kümmert sich um das eigentliche Schreiben:
/// Er merkt sich die aktuelle Spalte, schreibt immer in die unterste
/// Zeile und schiebt bei einem Zeilenumbruch alles nach oben.
pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
}

impl Writer {
    /// Schreibt ein einzelnes Byte auf den Bildschirm.
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                // Zeile voll? Dann erst umbrechen.
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;

                let color_code = self.color_code;
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
            }
        }
    }

    /// Schreibt einen ganzen String. Der VGA-Textmodus kann kein
    /// Unicode — nicht darstellbare Zeichen werden als ■ (0xfe) gezeigt.
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // Druckbares ASCII-Zeichen oder Zeilenumbruch
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // Alles andere: Platzhalter-Block
                _ => self.write_byte(0xfe),
            }
        }
    }

    /// Zeilenumbruch: Alle Zeilen eins nach oben schieben,
    /// die unterste Zeile leeren, Cursor an den Zeilenanfang.
    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
    }

    /// Überschreibt eine Zeile komplett mit Leerzeichen.
    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }
}

/// Damit wir `write!`/`writeln!` mit Formatierung ({}) benutzen können,
/// implementieren wir das Standard-Trait `core::fmt::Write`.
impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

lazy_static! {
    /// Der globale Writer, den die print!-Makros benutzen.
    /// Ein Spinlock (Mutex) schützt ihn vor gleichzeitigem Zugriff.
    /// `unsafe`: Wir versprechen dem Compiler, dass an Adresse 0xb8000
    /// wirklich der VGA-Puffer liegt (garantiert die PC-Hardware).
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::LightGreen, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    });
}

/// Gibt formatierten Text auf dem VGA-Bildschirm aus (wie print! in normalem Rust).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

/// Wie print!, aber mit Zeilenumbruch am Ende.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Interne Hilfsfunktion für die Makros — bitte nicht direkt aufrufen.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    WRITER.lock().write_fmt(args).unwrap();
}
