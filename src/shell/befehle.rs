// shell/befehle.rs — Die Befehle der SpeedShell
//
// Jeder Befehl ist eine eigene Struct, die das Befehl-Trait
// implementiert. Alle zusammen bilden die Registry (alle_befehle()).
// Einen neuen Befehl ergänzen heißt nur:
//   1. Struct anlegen + Trait implementieren,
//   2. in alle_befehle() eintragen — fertig.
// help, Dispatcher und Fehlermeldungen funktionieren dann automatisch.

use crate::fs::{self, FsFehler, NodeTyp};
use crate::konsole::{self, Color};
use crate::{allocator, print, println};
use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

/// Gemeinsamer Zustand der Shell, den Befehle lesen und ändern dürfen —
/// im Moment nur das aktuelle Verzeichnis (für cd, dir, relative Pfade).
pub struct ShellKontext {
    pub aktuelles_verzeichnis: String,
}

impl ShellKontext {
    pub fn neu() -> Self {
        ShellKontext {
            aktuelles_verzeichnis: String::from("/"),
        }
    }

    /// Löst eine Pfad-Eingabe relativ zum aktuellen Verzeichnis auf.
    pub fn aufloesen(&self, eingabe: &str) -> String {
        fs::pfad_aufloesen(&self.aktuelles_verzeichnis, eingabe)
    }
}

/// Das Interface, das jeder Shell-Befehl erfüllen muss.
/// `Send + Sync`: Die Registry lebt im Shell-Task, und Tasks müssen
/// Send sein (globale Spawn-Queue) — unsere Befehle sind alle
/// zustandslose Structs, das erfüllen sie automatisch.
pub trait Befehl: Send + Sync {
    /// Der Name, unter dem der Befehl aufgerufen wird (ein Wort).
    fn name(&self) -> &'static str;
    /// Einzeiler für die help-Ausgabe.
    fn beschreibung(&self) -> &'static str;
    /// Führt den Befehl aus. `argumente` = alles hinter dem Namen,
    /// `kontext` = Shell-Zustand (aktuelles Verzeichnis),
    /// `registry` = alle Befehle (braucht z. B. help für seine Liste).
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, registry: &[Box<dyn Befehl>]);
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
        Box::new(Grafiktest),
        Box::new(Desktop),
        Box::new(Neustart),
        // Dateisystem-Befehle:
        Box::new(Dir),
        Box::new(Cd),
        Box::new(MkDir),
        Box::new(Type),
        Box::new(Write),
        Box::new(Del),
        Box::new(Copy),
        Box::new(Move),
        Box::new(Tree),
        // Hardware-Befehle:
        Box::new(Pci),
        // Massenspeicher-Befehle (ATA-Treiber):
        Box::new(Platten),
        Box::new(Blocktest),
        // SpeedFS auf der Daten-Platte:
        Box::new(MkfsSpeedfs),
        Box::new(Mount),
        Box::new(Umount),
        Box::new(SyncBefehl),
        Box::new(PruefeSpeedfs),
        Box::new(Plattentest),
    ]
}

/// Gibt einen Dateisystem-Fehler rot und auf Deutsch aus.
fn fs_fehler_ausgeben(fehler: FsFehler) {
    konsole::set_color(Color::LightRed, Color::Black);
    println!("Fehler: {}", fehler.meldung());
    konsole::set_color(Color::LightGray, Color::Black);
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, registry: &[Box<dyn Befehl>]) {
        println!("Verfuegbare Befehle:");
        for befehl in registry {
            konsole::set_color(Color::LightCyan, Color::Black);
            // 12 Zeichen: auch "grafiktest" (10) bekommt noch Abstand.
            print!("  {:<12}", befehl.name());
            konsole::set_color(Color::LightGray, Color::Black);
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
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        konsole::clear_screen();
    }
}

/// ticks — zeigt den Timer-Zähler (und die Uptime daraus).
struct Ticks;

impl Befehl for Ticks {
    fn name(&self) -> &'static str {
        "ticks"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt Timer-Ticks und Uptime (~250 Ticks/Sekunde)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        // Alles über die zentrale Zeit-API (zeit.rs) — wenn die
        // Zeitquelle mal präziser wird, stimmt dieser Befehl einfach mit.
        let ticks = crate::zeit::ticks();
        let ms = crate::zeit::ms_seit_boot();
        println!(
            "Timer-Ticks: {}  |  Uptime: {},{:03} Sekunden",
            ticks,
            ms / 1000,
            ms % 1000
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        println!(
            "Kernel-Heap: {} KiB aktuell (ab Adresse {:#x}, wachstumsfaehig)",
            allocator::heap_groesse() / 1024,
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
        // Physischer Speicher (aus dem Bitmap-Frame-Allocator):
        let (frames_frei, frames_gesamt) = crate::memory::frame_statistik();
        println!(
            "Physischer Speicher: {} von {} Frames frei ({} von {} MiB)",
            frames_frei,
            frames_gesamt,
            frames_frei * 4 / 1024,
            frames_gesamt * 4 / 1024
        );
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        konsole::set_color(Color::Yellow, Color::Black);
        println!("SpeedOS v{}", env!("CARGO_PKG_VERSION"));
        konsole::set_color(Color::LightGray, Color::Black);
        println!("  Ein Betriebssystem from scratch in Rust (nightly, no_std)");
        println!("  Architektur: x86_64  |  Bootloader: bootloader 0.11 (Framebuffer!)");
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
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
            // Farbfeld als eingefärbter HINTERGRUND zweier Leerzeichen —
            // das klappt mit jedem Font (der Noto-Font hat keinen
            // Vollblock), der Name daneben in Grau.
            konsole::set_color(Color::LightGray, *farbe);
            print!("  ");
            konsole::set_color(Color::LightGray, Color::Black);
            print!(" {:<13}", name);
            if (nr + 1) % 4 == 0 {
                println!();
            }
        }
    }
}

/// grafiktest — zeigt alle Zeichen-Primitive des grafik-Moduls.
struct Grafiktest;

impl Befehl for Grafiktest {
    fn name(&self) -> &'static str {
        "grafiktest"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die Grafik-Primitive (beliebige Taste beendet)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if !crate::framebuffer::ist_initialisiert() {
            println!("Kein Framebuffer verfuegbar.");
            return;
        }
        // Im Desktop-Modus gehört der Bildschirm dem Compositor —
        // die Demo würde sofort wieder übermalt.
        if crate::fenster::desktop_aktiv() {
            println!("Bitte erst den Desktop mit ESC verlassen.");
            return;
        }
        // Zeichnet die Demo und setzt den Demo-Modus: Die Shell
        // fängt die nächste Taste ab und kehrt zur Konsole zurück.
        crate::grafik::demo_zeichnen();
    }
}

/// desktop — startet den Fenster-Desktop.
struct Desktop;

impl Befehl for Desktop {
    fn name(&self) -> &'static str {
        "desktop"
    }
    fn beschreibung(&self) -> &'static str {
        "Startet den Fenster-Desktop (ESC kehrt zurueck)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if !crate::framebuffer::ist_initialisiert() {
            println!("Kein Framebuffer verfuegbar.");
            return;
        }
        if crate::fenster::desktop_aktiv() {
            println!("Der Desktop laeuft bereits.");
            return;
        }
        crate::fenster::desktop_starten();
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
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        println!("SpeedOS startet neu ...");
        // Der eigentliche Reset lebt in lib.rs (crate::neustart) —
        // dieselbe Funktion nutzt auch die Startmenü-App.
        crate::neustart();
    }
}

// ---------------------------------------------------------------------------
// Dateisystem-Befehle (arbeiten alle NUR über das VFS-Trait!)
// ---------------------------------------------------------------------------

/// dir — zeigt den Inhalt eines Verzeichnisses (wie in cmd).
struct Dir;

impl Befehl for Dir {
    fn name(&self) -> &'static str {
        "dir"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt den Verzeichnisinhalt: dir [pfad]"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let pfad = if argumente.is_empty() {
            kontext.aktuelles_verzeichnis.clone()
        } else {
            kontext.aufloesen(argumente)
        };
        match fs::mit_fs(|f| f.liste(&pfad)) {
            Ok(eintraege) => {
                println!(" Verzeichnis von {}", pfad);
                println!();
                let mut dateien = 0;
                let mut verzeichnisse = 0;
                let mut bytes = 0;
                for e in &eintraege {
                    // Echter Änderungs-Zeitstempel aus dem VFS —
                    // angezeigt mit demselben Offset wie die
                    // Systray-Uhr (einstellungen::stempel_text).
                    let stempel = crate::einstellungen::stempel_text(e.geaendert);
                    match e.typ {
                        NodeTyp::Verzeichnis => {
                            verzeichnisse += 1;
                            konsole::set_color(Color::LightCyan, Color::Black);
                            println!("    {}  <DIR>           {}", stempel, e.name);
                        }
                        NodeTyp::Datei => {
                            dateien += 1;
                            bytes += e.groesse;
                            konsole::set_color(Color::LightGray, Color::Black);
                            println!("    {}  {:>9} Bytes  {}", stempel, e.groesse, e.name);
                        }
                    }
                }
                konsole::set_color(Color::LightGray, Color::Black);
                println!();
                println!(
                    "    {} Datei(en), {} Bytes  |  {} Verzeichnis(se)",
                    dateien, bytes, verzeichnisse
                );
            }
            Err(f) => fs_fehler_ausgeben(f),
        }
    }
}

/// cd — wechselt das Verzeichnis (ohne Argument: zeigt das aktuelle).
struct Cd;

impl Befehl for Cd {
    fn name(&self) -> &'static str {
        "cd"
    }
    fn beschreibung(&self) -> &'static str {
        "Wechselt das Verzeichnis: cd <pfad> (auch .. und /)"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if argumente.is_empty() {
            // Wie in cmd: cd ohne Argument zeigt, wo man ist.
            println!("{}", kontext.aktuelles_verzeichnis);
            return;
        }
        let ziel = kontext.aufloesen(argumente);
        match fs::mit_fs(|f| f.node_typ(&ziel)) {
            Ok(NodeTyp::Verzeichnis) => kontext.aktuelles_verzeichnis = ziel,
            Ok(NodeTyp::Datei) => fs_fehler_ausgeben(FsFehler::KeinVerzeichnis),
            Err(f) => fs_fehler_ausgeben(f),
        }
    }
}

/// mkdir — legt ein neues Verzeichnis an.
struct MkDir;

impl Befehl for MkDir {
    fn name(&self) -> &'static str {
        "mkdir"
    }
    fn beschreibung(&self) -> &'static str {
        "Legt ein Verzeichnis an: mkdir <pfad>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if argumente.is_empty() {
            println!("Benutzung: mkdir <pfad>");
            return;
        }
        let pfad = kontext.aufloesen(argumente);
        if let Err(f) = fs::mit_fs(|fs| fs.mkdir(&pfad)) {
            fs_fehler_ausgeben(f);
        }
    }
}

/// type — zeigt den Inhalt einer Datei (wie in cmd).
struct Type;

impl Befehl for Type {
    fn name(&self) -> &'static str {
        "type"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt den Inhalt einer Datei: type <datei>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if argumente.is_empty() {
            println!("Benutzung: type <datei>");
            return;
        }
        let pfad = kontext.aufloesen(argumente);
        match fs::mit_fs(|f| f.lesen(&pfad)) {
            // from_utf8_lossy: kaputte Bytes werden zu Ersatzzeichen,
            // statt die Ausgabe ganz zu verweigern.
            Ok(inhalt) => print!("{}", String::from_utf8_lossy(&inhalt)),
            Err(f) => fs_fehler_ausgeben(f),
        }
    }
}

/// write — schreibt Text in eine Datei (anlegen oder überschreiben).
struct Write;

impl Befehl for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn beschreibung(&self) -> &'static str {
        "Schreibt Text in eine Datei: write <datei> <text>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let (datei, text) = match argumente.split_once(' ') {
            Some((d, t)) if !d.is_empty() => (d, t),
            _ => {
                println!("Benutzung: write <datei> <text>");
                return;
            }
        };
        let pfad = kontext.aufloesen(datei);
        // Text als Zeile speichern (mit Zeilenumbruch am Ende).
        let inhalt = format!("{}\n", text);
        if let Err(f) = fs::mit_fs(|fs| fs.schreiben(&pfad, inhalt.as_bytes())) {
            fs_fehler_ausgeben(f);
        }
    }
}

/// del — löscht eine Datei oder ein leeres Verzeichnis.
struct Del;

impl Befehl for Del {
    fn name(&self) -> &'static str {
        "del"
    }
    fn beschreibung(&self) -> &'static str {
        "Loescht eine Datei oder ein leeres Verzeichnis: del <pfad>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if argumente.is_empty() {
            println!("Benutzung: del <pfad>");
            return;
        }
        let pfad = kontext.aufloesen(argumente);
        if let Err(f) = fs::mit_fs(|fs| fs.loeschen(&pfad)) {
            fs_fehler_ausgeben(f);
        }
    }
}

/// copy — kopiert eine Datei.
struct Copy;

impl Befehl for Copy {
    fn name(&self) -> &'static str {
        "copy"
    }
    fn beschreibung(&self) -> &'static str {
        "Kopiert eine Datei: copy <quelle> <ziel>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let (quelle, ziel) = match argumente.split_once(' ') {
            Some((q, z)) if !q.is_empty() && !z.trim().is_empty() => (q, z.trim()),
            _ => {
                println!("Benutzung: copy <quelle> <ziel>");
                return;
            }
        };
        let quelle = kontext.aufloesen(quelle);
        let ziel = kontext.aufloesen(ziel);
        if let Err(f) = fs::kopieren(&quelle, &ziel) {
            fs_fehler_ausgeben(f);
        }
    }
}

/// move — verschiebt eine Datei (oder benennt sie um).
struct Move;

impl Befehl for Move {
    fn name(&self) -> &'static str {
        "move"
    }
    fn beschreibung(&self) -> &'static str {
        "Verschiebt/benennt um: move <quelle> <ziel>"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let (quelle, ziel) = match argumente.split_once(' ') {
            Some((q, z)) if !q.is_empty() && !z.trim().is_empty() => (q, z.trim()),
            _ => {
                println!("Benutzung: move <quelle> <ziel>");
                return;
            }
        };
        let quelle = kontext.aufloesen(quelle);
        let ziel = kontext.aufloesen(ziel);
        if let Err(f) = fs::verschieben(&quelle, &ziel) {
            fs_fehler_ausgeben(f);
        }
    }
}

/// tree — zeichnet den Verzeichnisbaum mit Linien (CP437-Grafik).
struct Tree;

impl Befehl for Tree {
    fn name(&self) -> &'static str {
        "tree"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt den Verzeichnisbaum: tree [pfad]"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let pfad = if argumente.is_empty() {
            kontext.aktuelles_verzeichnis.clone()
        } else {
            kontext.aufloesen(argumente)
        };
        println!("{}", pfad);
        baum_zeichnen(&pfad, "");
    }
}

/// Rekursiver Teil von tree. Wichtig: liste() wird ZUERST komplett
/// eingesammelt (mit_fs gibt den Lock danach frei), erst DANN steigen
/// wir rekursiv ab — sonst Deadlock durch verschachteltes mit_fs!
fn baum_zeichnen(pfad: &str, einrueckung: &str) {
    let eintraege = match fs::mit_fs(|f| f.liste(pfad)) {
        Ok(e) => e,
        Err(f) => {
            fs_fehler_ausgeben(f);
            return;
        }
    };
    let anzahl = eintraege.len();
    for (i, eintrag) in eintraege.iter().enumerate() {
        let letzter = i + 1 == anzahl;
        // ASCII-Äste: Die Box-Zeichen (└─ etc.) fehlen im Noto-Font.
        let ast = if letzter { "`--" } else { "|--" };
        match eintrag.typ {
            NodeTyp::Verzeichnis => {
                konsole::set_color(Color::LightCyan, Color::Black);
                println!("{}{}{}", einrueckung, ast, eintrag.name);
                konsole::set_color(Color::LightGray, Color::Black);
                let kind_pfad = if pfad == "/" {
                    format!("/{}", eintrag.name)
                } else {
                    format!("{}/{}", pfad, eintrag.name)
                };
                let kind_einrueckung =
                    format!("{}{}", einrueckung, if letzter { "    " } else { "|   " });
                baum_zeichnen(&kind_pfad, &kind_einrueckung);
            }
            NodeTyp::Datei => {
                println!("{}{}{}", einrueckung, ast, eintrag.name);
            }
        }
    }
}

// ---------------------------------------------------------------------------

/// pci — listet alle beim Boot enumerierten PCI-Geraete.
struct Pci;

impl Befehl for Pci {
    fn name(&self) -> &'static str {
        "pci"
    }
    fn beschreibung(&self) -> &'static str {
        "Listet die PCI-Geraete (Vendor/Device, Klasse, BARs)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        crate::pci::mit_geraeten(|geraete| {
            if geraete.is_empty() {
                println!("Keine PCI-Geraete gefunden.");
                return;
            }
            println!("PCI-Geraete:");
            for g in geraete {
                konsole::set_color(Color::LightCyan, Color::Black);
                print!("  {:02x}:{:02x}.{}  ", g.bus, g.geraet, g.funktion);
                konsole::set_color(Color::LightGray, Color::Black);
                println!(
                    "{:04x}:{:04x}  {}",
                    g.vendor_id,
                    g.device_id,
                    g.klasse_text()
                );
                // BARs, die belegt sind, eingerückt darunter:
                for (i, bar) in g.bars.iter().enumerate() {
                    match bar {
                        crate::pci::Bar::Port(p) => {
                            konsole::set_color(Color::DarkGray, Color::Black);
                            println!("            BAR{}: I/O-Port 0x{:04x}", i, p);
                        }
                        crate::pci::Bar::Speicher { basis, bit64 } => {
                            konsole::set_color(Color::DarkGray, Color::Black);
                            println!(
                                "            BAR{}: MMIO 0x{:x}{}",
                                i,
                                basis,
                                if *bit64 { " (64-Bit)" } else { "" }
                            );
                        }
                        crate::pci::Bar::Leer => {}
                    }
                }
                konsole::set_color(Color::LightGray, Color::Black);
            }
        });
    }
}

/// platten — listet die beim Boot erkannten ATA-Laufwerke auf.
struct Platten;

impl Befehl for Platten {
    fn name(&self) -> &'static str {
        "platten"
    }
    fn beschreibung(&self) -> &'static str {
        "Listet die erkannten Laufwerke (Modell, Groesse)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        crate::ata::mit_laufwerken(|laufwerke| {
            if laufwerke.is_empty() {
                println!("Keine ATA-Laufwerke erkannt.");
                return;
            }
            println!("Erkannte ATA-Laufwerke:");
            println!();
            for laufwerk in laufwerke.iter_mut() {
                use crate::fs::block::BlockDevice;
                let sektoren = laufwerk.anzahl_sektoren();
                let mib = sektoren * laufwerk.sektor_groesse() as u64 / 1024 / 1024;
                konsole::set_color(Color::LightCyan, Color::Black);
                print!("  {:<6}", laufwerk.rolle());
                konsole::set_color(Color::LightGray, Color::Black);
                println!(
                    "{:<20}  {:>9} Sektoren = {:>5} MiB  {}",
                    laufwerk.modell(),
                    sektoren,
                    mib,
                    if laufwerk.ist_beschreibbar() {
                        "beschreibbar"
                    } else {
                        "schreibgeschuetzt"
                    }
                );
            }
        });

        // Die gemounteten Dateisysteme (Typ, Zugriff, Belegung):
        let mounts = crate::fs::mount_uebersicht();
        if !mounts.is_empty() {
            println!();
            println!("Gemountete Dateisysteme:");
            for m in &mounts {
                konsole::set_color(Color::LightCyan, Color::Black);
                print!("  {:<8}", m.praefix);
                konsole::set_color(Color::LightGray, Color::Black);
                let belegung = match m.belegung {
                    Some((frei, gesamt)) => format!(
                        "{} frei / {}",
                        crate::explorer::groesse_formatieren(frei as usize),
                        crate::explorer::groesse_formatieren(gesamt as usize)
                    ),
                    None => String::from("-"),
                };
                println!(
                    "{:<8}  {:<17}  {}",
                    m.typ,
                    if m.beschreibbar { "lesen+schreiben" } else { "nur lesen" },
                    belegung
                );
            }
        }
    }
}

/// blocktest — liest einen Sektor der DATEN-Platte und zeigt ihn als
/// klassischen Hexdump (16 Bytes pro Zeile, Offset + Hex + ASCII).
struct Blocktest;

impl Befehl for Blocktest {
    fn name(&self) -> &'static str {
        "blocktest"
    }
    fn beschreibung(&self) -> &'static str {
        "Hexdump eines Sektors der Daten-Platte: blocktest <lba>"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let lba = match argumente.trim().parse::<u64>() {
            Ok(lba) => lba,
            Err(_) => {
                println!("Aufruf: blocktest <lba>   (Sektornummer, z. B. blocktest 0)");
                return;
            }
        };
        let mut sektor = [0u8; crate::ata::SEKTOR_GROESSE];
        let ergebnis = crate::ata::mit_datenlaufwerk(|laufwerk| {
            use crate::fs::block::BlockDevice;
            laufwerk.lese_sektoren(lba, &mut sektor)
        });
        match ergebnis {
            Ok(()) => {
                println!("Daten-Platte, Sektor {} ({} Bytes):", lba, sektor.len());
                hexdump_ausgeben(&sektor);
            }
            Err(fehler) => fs_fehler_ausgeben(FsFehler::Io(fehler)),
        }
    }
}

/// Der klassische Hexdump: Offset (hex), 16 Byte-Werte, ASCII-Spalte
/// (nur druckbare Zeichen 0x20-0x7E, sonst '.').
fn hexdump_ausgeben(daten: &[u8]) {
    for (zeile, bytes) in daten.chunks(16).enumerate() {
        konsole::set_color(Color::DarkGray, Color::Black);
        print!("  {:04x}  ", zeile * 16);
        konsole::set_color(Color::LightGray, Color::Black);
        for (i, byte) in bytes.iter().enumerate() {
            // Nach 8 Bytes eine kleine Luecke — so zaehlt das Auge leichter:
            print!("{:02x}{}", byte, if i == 7 { "  " } else { " " });
        }
        konsole::set_color(Color::LightCyan, Color::Black);
        print!(" |");
        for byte in bytes {
            let zeichen = if (0x20..0x7F).contains(byte) {
                *byte as char
            } else {
                '.'
            };
            print!("{}", zeichen);
        }
        println!("|");
    }
}

// ---------------------------------------------------------------------------

/// Der Mount-Punkt der Daten-Platte — EIN Ort für alle drei Befehle.
const PLATTE_MOUNT: &str = "/platte";

/// mkfs.speedfs — formatiert die DATEN-Platte mit SpeedFS v1.
/// Destruktiv! Deshalb die Sicherheitsabfrage: Erst der Aufruf mit
/// dem Argument JA fuehrt wirklich aus.
struct MkfsSpeedfs;

impl Befehl for MkfsSpeedfs {
    fn name(&self) -> &'static str {
        "mkfs.speedfs"
    }
    fn beschreibung(&self) -> &'static str {
        "Formatiert die Daten-Platte mit SpeedFS: mkfs.speedfs JA"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if crate::fs::ist_gemountet(PLATTE_MOUNT) {
            konsole::set_color(Color::LightRed, Color::Black);
            println!("Die Platte ist unter {} gemountet — erst 'umount'.", PLATTE_MOUNT);
            konsole::set_color(Color::LightGray, Color::Black);
            return;
        }
        // Die Sicherheitsabfrage: ohne das explizite JA nur warnen.
        if argumente.trim() != "JA" {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("ACHTUNG: Formatiert die DATEN-Platte mit SpeedFS —");
            println!("ALLE Daten darauf gehen verloren!");
            konsole::set_color(Color::LightGray, Color::Black);
            println!("Wirklich formatieren: mkfs.speedfs JA");
            return;
        }
        let mut platte = match crate::fs::daten_geraet() {
            Some(platte) => platte,
            None => {
                fs_fehler_ausgeben(FsFehler::Io(crate::fs::IoFehler::NichtBereit));
                return;
            }
        };
        match crate::fs::speedfs::formatieren(platte.as_mut()) {
            Ok(()) => {
                let mib = platte.anzahl_sektoren() * platte.sektor_groesse() as u64 / 1024 / 1024;
                println!("SpeedFS angelegt ({} MiB). Einhaengen mit: mount", mib);
            }
            Err(fehler) => fs_fehler_ausgeben(fehler),
        }
    }
}

/// mount — haengt das SpeedFS der Daten-Platte unter /platte ein.
/// Danach arbeiten dir, type, write, copy, tree ... dort ganz normal.
struct Mount;

impl Befehl for Mount {
    fn name(&self) -> &'static str {
        "mount"
    }
    fn beschreibung(&self) -> &'static str {
        "Haengt die Daten-Platte (SpeedFS) unter /platte ein"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if crate::fs::ist_gemountet(PLATTE_MOUNT) {
            println!("{} ist bereits gemountet.", PLATTE_MOUNT);
            return;
        }
        let platte = match crate::fs::daten_geraet() {
            Some(platte) => platte,
            None => {
                fs_fehler_ausgeben(FsFehler::Io(crate::fs::IoFehler::NichtBereit));
                return;
            }
        };
        let speedfs = match crate::fs::speedfs::SpeedFs::mounten(platte) {
            Ok(speedfs) => speedfs,
            Err((fehler, _geraet)) => {
                fs_fehler_ausgeben(fehler);
                return;
            }
        };
        match crate::fs::mounten(PLATTE_MOUNT, Box::new(speedfs)) {
            Ok(()) => println!("Daten-Platte eingehaengt: {}", PLATTE_MOUNT),
            Err(fehler) => fs_fehler_ausgeben(fehler),
        }
    }
}

/// umount — synct und haengt /platte wieder aus.
struct Umount;

impl Befehl for Umount {
    fn name(&self) -> &'static str {
        "umount"
    }
    fn beschreibung(&self) -> &'static str {
        "Synct und haengt /platte aus"
    }
    fn ausfuehren(&self, _argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        match crate::fs::unmounten(PLATTE_MOUNT) {
            Ok(()) => {
                // Steht die Shell gerade IN /platte, zurueck zur Wurzel —
                // sonst zeigt der Prompt auf einen leeren Mount-Punkt.
                if kontext.aktuelles_verzeichnis.starts_with(PLATTE_MOUNT) {
                    kontext.aktuelles_verzeichnis = String::from("/");
                }
                println!("{} ausgehaengt (alles auf der Platte).", PLATTE_MOUNT);
            }
            Err(fehler) => fs_fehler_ausgeben(fehler),
        }
    }
}

/// sync — die komplette Kette: VFS -> alle Dateisysteme (Write-
/// Through-Caches sind schon unten, SpeedFS reicht durch) ->
/// BlockDevice (ATA FLUSH CACHE aufs Medium).
struct SyncBefehl;

impl Befehl for SyncBefehl {
    fn name(&self) -> &'static str {
        "sync"
    }
    fn beschreibung(&self) -> &'static str {
        "Schreibt alle Puffer aufs Medium (VFS -> FS -> Platte)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        match crate::fs::sync() {
            Ok(()) => println!("Alle Puffer auf dem Medium."),
            Err(fehler) => fs_fehler_ausgeben(fehler),
        }
    }
}

/// pruefe.speedfs — unser fsck (docs/speedfs-format.md §10).
/// Laeuft nur auf der UNGEMOUNTETEN Platte; --repariere gibt
/// gefundene Lecks (Absturz-Rueckstaende) wieder frei.
struct PruefeSpeedfs;

impl Befehl for PruefeSpeedfs {
    fn name(&self) -> &'static str {
        "pruefe.speedfs"
    }
    fn beschreibung(&self) -> &'static str {
        "Prueft das SpeedFS der Daten-Platte: pruefe.speedfs [--repariere]"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if crate::fs::ist_gemountet(PLATTE_MOUNT) {
            konsole::set_color(Color::LightRed, Color::Black);
            println!("Die Platte ist unter {} gemountet — erst 'umount'.", PLATTE_MOUNT);
            konsole::set_color(Color::LightGray, Color::Black);
            return;
        }
        let reparieren = argumente.trim() == "--repariere";
        let platte = match crate::fs::daten_geraet() {
            Some(platte) => platte,
            None => {
                fs_fehler_ausgeben(FsFehler::Io(crate::fs::IoFehler::NichtBereit));
                return;
            }
        };
        let speedfs = match crate::fs::speedfs::SpeedFs::mounten(platte) {
            Ok(speedfs) => speedfs,
            Err((fehler, _)) => {
                fs_fehler_ausgeben(fehler);
                return;
            }
        };
        let bericht = match speedfs.pruefen(reparieren) {
            Ok(bericht) => bericht,
            Err(fehler) => {
                fs_fehler_ausgeben(fehler);
                return;
            }
        };

        println!(
            "SpeedFS geprueft: {} Inodes erreichbar, {} Bloecke referenziert.",
            bericht.inodes_erreichbar, bericht.bloecke_referenziert
        );
        for eintrag in &bericht.doppel_eintraege {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("Befund: Doppel-Eintrag {} (rename-Absturz, harmlos)", eintrag);
        }
        if bericht.hat_lecks() {
            konsole::set_color(Color::Yellow, Color::Black);
            println!(
                "Lecks: {} Block/Bloecke, {} Inode(s) belegt aber unreferenziert.",
                bericht.block_lecks.len(),
                bericht.inode_lecks.len()
            );
            konsole::set_color(Color::LightGray, Color::Black);
            if bericht.repariert {
                println!("Repariert: Lecks wieder freigegeben.");
            } else {
                println!("Reparieren mit: pruefe.speedfs --repariere");
            }
        }
        if bericht.defekte.is_empty() {
            if !bericht.hat_lecks() && bericht.doppel_eintraege.is_empty() {
                println!("Keine Befunde — das Dateisystem ist sauber.");
            }
        } else {
            konsole::set_color(Color::LightRed, Color::Black);
            println!("DEFEKTE ({}) — werden NICHT automatisch repariert:", bericht.defekte.len());
            for defekt in &bericht.defekte {
                println!("  {}", defekt);
            }
            konsole::set_color(Color::LightGray, Color::Black);
        }
    }
}

/// plattentest — Benchmark der Daten-Platte (sequenziell + zufaellig,
/// lesen + schreiben, MiB/s). Braucht die Platte AUSGEHAENGT (die
/// Schreib-Tests wuerden ein gemountetes SpeedFS zerstoeren) und misst
/// das ROHE BlockDevice — so vergleichbar zwischen IDE und virtio.
struct Plattentest;

impl Plattentest {
    /// Wandelt (Bytes, Mikrosekunden) in "X,YZ MiB/s"-Text.
    fn mibs(bytes: u64, us: u64) -> String {
        if us == 0 {
            return String::from("(zu schnell zu messen)");
        }
        // bytes/us * 1e6 / 2^20  -> mit *100 fuer zwei Nachkommastellen:
        let hundertstel = bytes.saturating_mul(1_000_000) * 100 / (us * 1024 * 1024);
        format!("{},{:02} MiB/s", hundertstel / 100, hundertstel % 100)
    }
}

impl Befehl for Plattentest {
    fn name(&self) -> &'static str {
        "plattentest"
    }
    fn beschreibung(&self) -> &'static str {
        "Benchmark der Daten-Platte (seq+zufaellig, lesen/schreiben)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::fs::block::IoFehler;
        if crate::fs::ist_gemountet(PLATTE_MOUNT) {
            konsole::set_color(Color::LightRed, Color::Black);
            println!("Die Platte ist gemountet — erst 'umount' (der Schreibtest wuerde");
            println!("das Dateisystem ueberschreiben!).");
            konsole::set_color(Color::LightGray, Color::Black);
            return;
        }
        let mut platte = match crate::fs::daten_geraet() {
            Some(platte) => platte,
            None => {
                fs_fehler_ausgeben(FsFehler::Io(IoFehler::NichtBereit));
                return;
            }
        };
        let sektor = platte.sektor_groesse();
        let anzahl_sektoren = platte.anzahl_sektoren();

        // Parameter: 2 MiB sequenziell in 64-KiB-Bloecken, 100
        // Zufalls-Zugriffe zu 4 KiB. Alles bleibt in der ersten Haelfte
        // der Platte (Reserve fuer die Geometrie). BEWUSST klein: der
        // gepollte IDE-PIO-Pfad schafft nur ~0,2 MiB/s (Port-I/O-VM-
        // Exits pro 16-Bit-Wort) — mehr waere quaelend langsam. Die
        // MiB/s-RATE ist von der Groesse unabhaengig, also fair.
        let block = 64 * 1024; // 64 KiB je Transfer
        let bloecke = 32; // 32 * 64 KiB = 2 MiB
        let gesamt = (block * bloecke) as u64;
        let block_sektoren = (block / sektor) as u64;
        let zufall_bytes = 4096;
        let zufall_sektoren = (zufall_bytes / sektor) as u64;
        let zufall_anzahl = 100u64;
        let max_lba = anzahl_sektoren / 2;

        if max_lba < block_sektoren * bloecke as u64 {
            println!("Platte zu klein fuer den Benchmark.");
            return;
        }
        let mut puffer = vec![0u8; block];
        // Erkennbares Muster fuer den Schreibtest:
        for (i, b) in puffer.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }

        println!("Plattentest ({} MiB seq, {} x 4 KiB zufaellig):", gesamt / 1024 / 1024, zufall_anzahl);
        let messen = |name: &str, f: &mut dyn FnMut() -> Result<u64, IoFehler>| {
            let start = crate::zeit::us_seit_boot();
            match f() {
                Ok(bytes) => {
                    let us = crate::zeit::us_seit_boot() - start;
                    konsole::set_color(Color::LightCyan, Color::Black);
                    print!("  {:<22}", name);
                    konsole::set_color(Color::LightGray, Color::Black);
                    println!("{}", Plattentest::mibs(bytes, us));
                }
                Err(fehler) => fs_fehler_ausgeben(FsFehler::Io(fehler)),
            }
        };

        // 1. Sequenziell schreiben:
        messen("seq. schreiben:", &mut || {
            let mut lba = 0u64;
            for _ in 0..bloecke {
                platte.schreibe_sektoren(lba, &puffer)?;
                lba += block_sektoren;
            }
            platte.sync()?;
            Ok(gesamt)
        });
        // 2. Sequenziell lesen:
        messen("seq. lesen:", &mut || {
            let mut lba = 0u64;
            for _ in 0..bloecke {
                platte.lese_sektoren(lba, &mut puffer)?;
                lba += block_sektoren;
            }
            Ok(gesamt)
        });
        // 3. Zufaellig schreiben (LCG-"Zufall", 4-KiB-Haeppchen):
        let mut zbuf = vec![0u8; zufall_bytes];
        let mut rng = 0x1234_5678u64;
        let naechster = |rng: &mut u64| {
            *rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*rng >> 33) % (max_lba - zufall_sektoren)
        };
        messen("zufaellig schreiben:", &mut || {
            for _ in 0..zufall_anzahl {
                let lba = naechster(&mut rng);
                platte.schreibe_sektoren(lba, &zbuf)?;
            }
            platte.sync()?;
            Ok(zufall_anzahl * zufall_bytes as u64)
        });
        // 4. Zufaellig lesen:
        messen("zufaellig lesen:", &mut || {
            for _ in 0..zufall_anzahl {
                let lba = naechster(&mut rng);
                platte.lese_sektoren(lba, &mut zbuf)?;
            }
            Ok(zufall_anzahl * zufall_bytes as u64)
        });
        println!("Fertig. (Backend: siehe 'platten' — IDE oder virtio.)");
    }
}
