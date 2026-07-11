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
        "Zeigt Timer-Ticks und Uptime (~18,2 Ticks/Sekunde)"
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
                    match e.typ {
                        NodeTyp::Verzeichnis => {
                            verzeichnisse += 1;
                            konsole::set_color(Color::LightCyan, Color::Black);
                            println!("    <DIR>           {}", e.name);
                        }
                        NodeTyp::Datei => {
                            dateien += 1;
                            bytes += e.groesse;
                            konsole::set_color(Color::LightGray, Color::Black);
                            println!("    {:>9} Bytes  {}", e.groesse, e.name);
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
