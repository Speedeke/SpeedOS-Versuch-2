// shell/befehle.rs — Die Befehle der SpeedShell
//
// Jeder Befehl ist eine eigene Struct, die das Befehl-Trait
// implementiert. Alle zusammen bilden die Registry (alle_befehle()).
// Einen neuen Befehl ergänzen heißt nur:
//   1. Struct anlegen + Trait implementieren,
//   2. in alle_befehle() eintragen — fertig.
// help, Dispatcher und Fehlermeldungen funktionieren dann automatisch.

use crate::vga_buffer::{self, Color};
use crate::{allocator, interrupts, print, println};
use alloc::{boxed::Box, vec, vec::Vec};

/// Das Interface, das jeder Shell-Befehl erfüllen muss.
pub trait Befehl {
    /// Der Name, unter dem der Befehl aufgerufen wird (ein Wort).
    fn name(&self) -> &'static str;
    /// Einzeiler für die help-Ausgabe.
    fn beschreibung(&self) -> &'static str;
    /// Führt den Befehl aus. `argumente` = alles hinter dem Namen,
    /// `registry` = alle Befehle (braucht z. B. help für seine Liste).
    fn ausfuehren(&self, argumente: &str, registry: &[Box<dyn Befehl>]);
}

/// Baut die Registry mit allen verfügbaren Befehlen auf.
pub fn alle_befehle() -> Vec<Box<dyn Befehl>> {
    vec![
        Box::new(Help),
        Box::new(Echo),
        Box::new(Clear),
        Box::new(Ticks),
        Box::new(MemInfo),
        Box::new(Version),
        Box::new(Farbtest),
        Box::new(Neustart),
    ]
}

// ---------------------------------------------------------------------------

/// help — listet alle Befehle mit Beschreibung auf.
struct Help;

impl Befehl for Help {
    fn name(&self) -> &'static str {
        "help"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt diese Liste aller Befehle"
    }
    fn ausfuehren(&self, _argumente: &str, registry: &[Box<dyn Befehl>]) {
        println!("Verfuegbare Befehle:");
        for befehl in registry {
            vga_buffer::set_color(Color::LightCyan, Color::Black);
            print!("  {:<10}", befehl.name());
            vga_buffer::set_color(Color::LightGray, Color::Black);
            println!("{}", befehl.beschreibung());
        }
    }
}

/// echo — gibt die Argumente unverändert wieder aus.
struct Echo;

impl Befehl for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn beschreibung(&self) -> &'static str {
        "Gibt den Text dahinter aus: echo <text>"
    }
    fn ausfuehren(&self, argumente: &str, _registry: &[Box<dyn Befehl>]) {
        println!("{}", argumente);
    }
}

/// clear — leert den Bildschirm.
struct Clear;

impl Befehl for Clear {
    fn name(&self) -> &'static str {
        "clear"
    }
    fn beschreibung(&self) -> &'static str {
        "Leert den Bildschirm"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        vga_buffer::clear_screen();
    }
}

/// ticks — zeigt den Timer-Zähler (und die Uptime daraus).
struct Ticks;

impl Befehl for Ticks {
    fn name(&self) -> &'static str {
        "ticks"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt Timer-Ticks seit dem Start (~18,2/Sekunde)"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        let ticks = interrupts::timer_ticks();
        // Der PIT-Timer tickt mit ~18,2 Hz -> Sekunden = Ticks / 18,2.
        // Ohne Fließkomma rechnen wir in Zehnteln: Ticks * 10 / 182.
        let zehntel = ticks * 10 / 182;
        println!(
            "Timer-Ticks: {} (das sind ca. {},{} Sekunden Uptime)",
            ticks,
            zehntel / 10,
            zehntel % 10
        );
    }
}

/// meminfo — Statistik über den Kernel-Heap.
struct MemInfo;

impl Befehl for MemInfo {
    fn name(&self) -> &'static str {
        "meminfo"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die Heap-Statistik (belegt/frei)"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        println!(
            "Kernel-Heap: {} KiB gesamt (ab Adresse {:#x})",
            allocator::HEAP_SIZE / 1024,
            allocator::HEAP_START
        );
        match allocator::heap_statistik() {
            Some((belegt, frei)) => {
                println!("  belegt: {:>6} Bytes", belegt);
                println!("  frei:   {:>6} Bytes", frei);
            }
            None => {
                println!("  (Keine Statistik verfuegbar — der aktive");
                println!("   Lern-Allocator fuehrt nicht Buch.)");
            }
        }
    }
}

/// version — Infos über diese SpeedOS-Version.
struct Version;

impl Befehl for Version {
    fn name(&self) -> &'static str {
        "version"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die SpeedOS-Version"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        vga_buffer::set_color(Color::Yellow, Color::Black);
        println!("SpeedOS v{}", env!("CARGO_PKG_VERSION"));
        vga_buffer::set_color(Color::LightGray, Color::Black);
        println!("  Ein Betriebssystem from scratch in Rust (nightly, no_std)");
        println!("  Architektur: x86_64  |  Bootloader: bootloader 0.9");
        println!("  Multitasking: kooperativ (async/await)");
    }
}

/// farbtest — zeigt alle 16 VGA-Farben.
struct Farbtest;

impl Befehl for Farbtest {
    fn name(&self) -> &'static str {
        "farbtest"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt alle 16 VGA-Farben"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        // Alle 16 Farben mit deutschen Namen, 4 pro Zeile.
        let farben: [(Color, &str); 16] = [
            (Color::Black, "Schwarz"),
            (Color::Blue, "Blau"),
            (Color::Green, "Gruen"),
            (Color::Cyan, "Tuerkis"),
            (Color::Red, "Rot"),
            (Color::Magenta, "Magenta"),
            (Color::Brown, "Braun"),
            (Color::LightGray, "Hellgrau"),
            (Color::DarkGray, "Dunkelgrau"),
            (Color::LightBlue, "Hellblau"),
            (Color::LightGreen, "Hellgruen"),
            (Color::LightCyan, "Helltuerkis"),
            (Color::LightRed, "Hellrot"),
            (Color::Pink, "Rosa"),
            (Color::Yellow, "Gelb"),
            (Color::White, "Weiss"),
        ];
        for (nr, (farbe, name)) in farben.iter().enumerate() {
            // Farbblock in der Farbe selbst, Name daneben in Grau
            // (sonst wäre "Schwarz" komplett unsichtbar).
            vga_buffer::set_color(*farbe, Color::Black);
            print!("██");
            vga_buffer::set_color(Color::LightGray, Color::Black);
            print!(" {:<13}", name);
            if (nr + 1) % 4 == 0 {
                println!();
            }
        }
    }
}

/// neustart — startet die Maschine neu.
struct Neustart;

impl Befehl for Neustart {
    fn name(&self) -> &'static str {
        "neustart"
    }
    fn beschreibung(&self) -> &'static str {
        "Startet SpeedOS neu (Reset ueber den Tastatur-Controller)"
    }
    fn ausfuehren(&self, _argumente: &str, _registry: &[Box<dyn Befehl>]) {
        use x86_64::instructions::port::Port;

        println!("SpeedOS startet neu ...");
        // Der klassische PC-Reset-Trick: Der 8042-Tastatur-Controller
        // (Port 0x64) hat eine Leitung zur Reset-Pin der CPU. Das
        // Kommando 0xFE zieht sie — die Maschine bootet sofort neu.
        let mut port: Port<u8> = Port::new(0x64);
        unsafe {
            port.write(0xFE);
        }
        // Falls der Reset einen Wimpernschlag braucht: CPU anhalten.
        crate::hlt_loop();
    }
}
