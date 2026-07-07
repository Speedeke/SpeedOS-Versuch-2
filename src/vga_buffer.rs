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
            // Backspace (0x08): einen Schritt zurück und das Zeichen
            // dort löschen. Am Zeilenanfang passiert nichts — wir
            // springen (noch) nicht in die vorherige Zeile zurück.
            0x08 => self.backspace(),
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

    /// Schreibt einen ganzen String auf den Bildschirm.
    ///
    /// Rust-Strings sind UTF-8, der VGA-Textmodus versteht aber nur
    /// Codepage 437 (den alten IBM-PC-Zeichensatz). Deshalb gehen wir
    /// hier Zeichen für Zeichen (nicht Byte für Byte!) durch den String
    /// und übersetzen jedes Zeichen mit `char_zu_cp437`.
    pub fn write_string(&mut self, s: &str) {
        for c in s.chars() {
            self.write_byte(char_zu_cp437(c));
        }
    }

    /// Ändert die Farbe für alle FOLGENDEN Ausgaben.
    /// Bereits geschriebener Text behält seine Farbe.
    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color_code = ColorCode::new(foreground, background);
    }

    /// Leert den kompletten Bildschirm und setzt den Cursor an den
    /// Anfang der (untersten) Schreibzeile.
    pub fn clear_screen(&mut self) {
        for row in 0..BUFFER_HEIGHT {
            self.clear_row(row);
        }
        self.column_position = 0;
        self.update_cursor();
    }

    /// Schiebt den blinkenden Hardware-Cursor der VGA-Karte an die
    /// aktuelle Schreibposition. Die Karte wird über zwei I/O-Ports
    /// gesteuert: In 0x3D4 sagt man, WELCHES Register man meint,
    /// in 0x3D5 schreibt man den Wert. Das Blinken macht die
    /// Hardware dann ganz von selbst.
    fn update_cursor(&self) {
        use x86_64::instructions::port::Port;

        let pos = ((BUFFER_HEIGHT - 1) * BUFFER_WIDTH + self.column_position) as u16;
        let mut index_port: Port<u8> = Port::new(0x3D4);
        let mut data_port: Port<u8> = Port::new(0x3D5);
        // unsafe: Standard-VGA-Ports, auf jedem PC/QEMU vorhanden.
        unsafe {
            index_port.write(0x0F); // Register: Cursorposition, Low-Byte
            data_port.write((pos & 0xFF) as u8);
            index_port.write(0x0E); // Register: Cursorposition, High-Byte
            data_port.write((pos >> 8) as u8);
        }
    }

    /// Löscht das Zeichen links von der aktuellen Position
    /// (klassisches Backspace-Verhalten).
    fn backspace(&mut self) {
        if self.column_position > 0 {
            self.column_position -= 1;
            let row = BUFFER_HEIGHT - 1;
            let blank = ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            };
            self.buffer.chars[row][self.column_position].write(blank);
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
    /// Bewusst immer in der Standardfarbe (Hellgrau auf Schwarz),
    /// nicht in der aktuellen Schreibfarbe — sonst hinterlässt das
    /// Scrolling farbige Streifen, wenn gerade z. B. mit blauem
    /// Hintergrund geschrieben wird.
    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: ColorCode::new(Color::LightGray, Color::Black),
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[row][col].write(blank);
        }
    }
}

/// Übersetzt ein Unicode-Zeichen in den VGA-Zeichensatz (Codepage 437).
///
/// Codepage 437 ist der Zeichensatz des Ur-IBM-PCs von 1981 — die
/// VGA-Hardware hat für jeden der 256 Byte-Werte ein festes Zeichenbild.
/// Zum Glück enthält er die deutschen Umlaute und das ß, nur an ganz
/// anderen Positionen als in Unicode/ASCII. Alles, was CP437 nicht
/// kennt (z. B. € oder Emoji), wird zum Ersatzzeichen ■ (0xFE).
fn char_zu_cp437(c: char) -> u8 {
    match c {
        // Druckbares ASCII (Leerzeichen bis '~') und Zeilenumbruch:
        // identisch in Unicode und CP437, einfach durchreichen.
        ' '..='~' | '\n' => c as u8,
        // Steuerzeichen fürs Löschen: Backspace (0x08) direkt, und
        // DEL (0x7F, von der Entf-Taste) behandeln wir genauso.
        '\u{8}' | '\u{7f}' => 0x08,
        // Deutsche Umlaute und ß an ihren CP437-Positionen:
        'ä' => 0x84,
        'ö' => 0x94,
        'ü' => 0x81,
        'Ä' => 0x8E,
        'Ö' => 0x99,
        'Ü' => 0x9A,
        'ß' => 0xE1,
        // Ein paar weitere nützliche CP437-Zeichen:
        '§' => 0x15,
        '°' => 0xF8,
        '²' => 0xFD,
        'é' => 0x82,
        'è' => 0x8A,
        '█' => 0xDB, // Vollblock — praktisch für Farb-Demos
        // Linien-Zeichen für den tree-Befehl:
        '│' => 0xB3,
        '├' => 0xC3,
        '└' => 0xC0,
        '─' => 0xC4,
        // Alles andere kann VGA nicht darstellen: Ersatzzeichen ■
        _ => 0xFE,
    }
}

/// Damit wir `write!`/`writeln!` mit Formatierung ({}) benutzen können,
/// implementieren wir das Standard-Trait `core::fmt::Write`.
impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        // Nach jeder Ausgabe den blinkenden Cursor nachziehen.
        self.update_cursor();
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

/// Ändert die Ausgabefarbe des globalen Writers (bequemer Kurzweg,
/// damit der Rest des Kernels nicht selbst WRITER.lock() rufen muss).
pub fn set_color(foreground: Color, background: Color) {
    WRITER.lock().set_color(foreground, background);
}

/// Leert den kompletten Bildschirm (mit Interrupt- und Lock-Schutz
/// wie bei _print).
pub fn clear_screen() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        WRITER.lock().clear_screen();
    });
}

/// Schaltet den blinkenden Hardware-Cursor ein (Unterstrich-Form,
/// Scanlines 14-15 der 16 Pixel hohen Textzelle) und setzt ihn an
/// die aktuelle Schreibposition.
pub fn cursor_aktivieren() {
    use x86_64::instructions::{interrupts, port::Port};
    interrupts::without_interrupts(|| {
        let writer = WRITER.lock();
        let mut index_port: Port<u8> = Port::new(0x3D4);
        let mut data_port: Port<u8> = Port::new(0x3D5);
        unsafe {
            index_port.write(0x0A); // Register: Cursor-Start-Scanline
            data_port.write(14); //   (Bit 5 = 0 heißt: Cursor sichtbar)
            index_port.write(0x0B); // Register: Cursor-End-Scanline
            data_port.write(15);
        }
        writer.update_cursor();
    });
}

/// Interne Hilfsfunktion, die NUR auf VGA schreibt.
/// Die print!/println!-Makros (in lib.rs) rufen sie zusammen mit der
/// seriellen Ausgabe auf — bitte nicht direkt benutzen.
///
/// Deadlock-Schutz: Während wir den WRITER-Lock halten, schalten wir
/// Interrupts kurz ab. Sonst könnte mitten im Schreiben ein Tastatur-
/// Interrupt zuschlagen, dessen Handler auch drucken will — er würde
/// ewig auf den Lock warten, den wir nie freigeben können (Deadlock!).
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        WRITER.lock().write_fmt(args).unwrap();
    });
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::println;

    /// println! darf niemals abstürzen — auch nicht bei vielen Aufrufen.
    #[test_case]
    fn test_println_ohne_panic() {
        for i in 0..10 {
            println!("println-Test ohne Panic, Durchlauf {}", i);
        }
    }

    /// Druckt mehr Zeilen, als auf den Bildschirm passen (25), und prüft
    /// danach, dass die letzte Zeile korrekt sichtbar ist. Das beweist,
    /// dass das Scrolling funktioniert und nichts durcheinanderkommt.
    #[test_case]
    fn test_scrolling_viele_zeilen() {
        // Bildschirm mehrfach komplett füllen -> erzwingt Scrolling.
        for i in 0..60 {
            println!("Scroll-Zeile {}", i);
        }
        let s = "Diese Zeile muss nach dem Scrollen sichtbar sein";
        println!("{}", s);
        // Nach dem abschließenden \n von println! steht der Text in der
        // vorletzten Zeile (BUFFER_HEIGHT - 2). Zeichen für Zeichen prüfen:
        for (i, c) in s.chars().enumerate() {
            let screen_char = WRITER.lock().buffer.chars[BUFFER_HEIGHT - 2][i].read();
            assert_eq!(char::from(screen_char.ascii_character), c);
        }
    }

    /// Prüft das Backspace-Verhalten: "ab" tippen, einmal löschen —
    /// übrig bleiben muss "a", und die Schreibposition steht auf 1.
    /// Auch am Zeilenanfang darf Backspace nicht abstürzen.
    #[test_case]
    fn test_backspace_loescht_letztes_zeichen() {
        use x86_64::instructions::interrupts;

        // Interrupts aus + Lock direkt halten, damit uns während der
        // Prüfung niemand (z. B. der Tastatur-Handler) dazwischenschreibt.
        interrupts::without_interrupts(|| {
            let mut writer = WRITER.lock();
            let row = BUFFER_HEIGHT - 1;
            // "\n" bringt uns sauber an einen Zeilenanfang.
            writer.write_string("\nab\u{8}");
            assert_eq!(writer.column_position, 1);
            assert_eq!(writer.buffer.chars[row][0].read().ascii_character, b'a');
            assert_eq!(writer.buffer.chars[row][1].read().ascii_character, b' ');
            // Entf-Variante (DEL, 0x7F) löscht genauso:
            writer.write_string("\u{7f}");
            assert_eq!(writer.column_position, 0);
            assert_eq!(writer.buffer.chars[row][0].read().ascii_character, b' ');
            // Backspace am Zeilenanfang: darf einfach nichts tun.
            writer.write_string("\u{8}");
            assert_eq!(writer.column_position, 0);
        });
    }

    /// Prüft, dass Umlaute an den richtigen CP437-Positionen landen
    /// und nicht darstellbare Zeichen zum Ersatzzeichen ■ werden.
    #[test_case]
    fn test_umlaute_und_ersatzzeichen() {
        println!("äöüÄÖÜß€");
        let erwartet: [u8; 8] = [0x84, 0x94, 0x81, 0x8E, 0x99, 0x9A, 0xE1, 0xFE];
        for (i, &code) in erwartet.iter().enumerate() {
            let screen_char = WRITER.lock().buffer.chars[BUFFER_HEIGHT - 2][i].read();
            assert_eq!(screen_char.ascii_character, code);
        }
    }
}
