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
    /// Die Terminal-Sitzung, zu der diese Shell gehört (0 = keine).
    ///
    /// Gebraucht für Strg+C: Der Abbruch-Wunsch liegt PRO SITZUNG, damit
    /// zwei Terminal-Fenster sich nicht gegenseitig die Programme
    /// abschiessen (siehe `sitzung::abbruch_anfordern`).
    pub sitzung: u64,
}

impl ShellKontext {
    pub fn neu() -> Self {
        ShellKontext {
            aktuelles_verzeichnis: String::from("/"),
            sitzung: 0,
        }
    }

    /// Verbindet den Kontext mit seiner Terminal-Sitzung.
    pub fn mit_sitzung(mut self, sitzung: u64) -> Self {
        self.sitzung = sitzung;
        self
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
        Box::new(Schriftprobe),
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
        // Netzwerk-Befehle (Serie 5):
        Box::new(Netz),
        Box::new(NetzIp),
        Box::new(NetzLausch),
        Box::new(NetzStatus),
        Box::new(Dhcp),
        Box::new(Arp),
        Box::new(ArpPing),
        Box::new(Ping),
        Box::new(Nslookup),
        Box::new(Hole),
        Box::new(BrowserBefehl),
        // Serie 6: der erste Sprung nach Ring 3 (User-Mode).
        Box::new(Ring3Test),
        Box::new(AdressraumTest),
        Box::new(Prozesse),
        Box::new(ProzessStart),
        Box::new(ProzessStop),
        Box::new(PraemptionsTest),
        // Serie 6, Teil 5: echte Programme von der Platte.
        Box::new(Starte),
        Box::new(Programme),
        Box::new(ElfInfo),
        // Serie 7, Teil 1: der Zufallsgenerator.
        Box::new(ZufallBefehl),
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
        // Der Hinweis gehört ans ENDE: Wer `help` tippt, hat gerade eine
        // Ausgabe erzeugt, die länger ist als der Bildschirm — das ist
        // genau der Moment, in dem man vom Zurückblättern erfahren will.
        println!();
        konsole::set_color(Color::DarkGray, Color::Black);
        println!("Bild auf/ab blaettert zurueck (im Fenster auch das Mausrad).");
        konsole::set_color(Color::LightGray, Color::Black);
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
            Ok(()) => {
                println!("Daten-Platte eingehaengt: {}", PLATTE_MOUNT);
                // Die mitgelieferten Programme gehoeren jetzt auf die Platte
                // (bis eben lagen sie im RAM-VFS). Ohne das zeigte
                // `programme::verzeichnis()` auf einen leeren Ordner.
                let geschrieben = crate::programme::nach_mount_wechsel();
                if geschrieben > 0 {
                    println!(
                        "{} Programm(e) nach {} uebernommen.",
                        geschrieben,
                        crate::programme::verzeichnis()
                    );
                }
            }
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
                // Ohne Platte gilt wieder der RAM-Ort — dorthin gehoeren die
                // Programme jetzt, sonst laesst sich keines mehr starten.
                crate::programme::nach_mount_wechsel();
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

/// Gibt "keine NIC"-Hinweis aus; liefert true, wenn KEINE NIC da ist
/// (die Netz-Befehle brechen dann ab). Ein Ort für die Meldung.
fn keine_nic() -> bool {
    if crate::netz::vorhanden() {
        return false;
    }
    konsole::set_color(Color::Yellow, Color::Black);
    println!("Keine Netzwerkkarte vorhanden.");
    konsole::set_color(Color::LightGray, Color::Black);
    true
}

/// Gibt einen Netz-Fehler rot und auf Deutsch aus.
fn netz_fehler_ausgeben(fehler: crate::netz::NetzFehler) {
    konsole::set_color(Color::LightRed, Color::Black);
    println!("Fehler: {}", fehler.meldung());
    konsole::set_color(Color::LightGray, Color::Black);
}

/// netz — zeigt den Status der Netzwerkkarte (MAC) und die IP-Konfiguration.
struct Netz;

impl Befehl for Netz {
    fn name(&self) -> &'static str {
        "netz"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt NIC-Status (MAC) und IP-Konfiguration"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if keine_nic() {
            return;
        }
        if let Some(mac) = crate::netz::mac() {
            konsole::set_color(Color::LightCyan, Color::Black);
            print!("  MAC      ");
            konsole::set_color(Color::LightGray, Color::Black);
            println!("{}", crate::netz::ethernet::mac_text(&mac));
        }
        let k = crate::netz::konfig();
        if k.gesetzt {
            let zeilen = [
                ("IP", k.ip),
                ("Maske", k.maske),
                ("Gateway", k.gateway),
            ];
            for (name, wert) in zeilen {
                konsole::set_color(Color::LightCyan, Color::Black);
                print!("  {:<8} ", name);
                konsole::set_color(Color::LightGray, Color::Black);
                println!("{}", wert);
            }
        } else {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("  Keine IP konfiguriert.");
            konsole::set_color(Color::LightGray, Color::Black);
            println!("  Setzen mit: netz-ip <ip> <maske> <gateway>");
        }
    }
}

/// netz-ip — setzt die statische IP-Konfiguration (DHCP kommt später).
struct NetzIp;

impl Befehl for NetzIp {
    fn name(&self) -> &'static str {
        "netz-ip"
    }
    fn beschreibung(&self) -> &'static str {
        "Setzt die statische IP: netz-ip <ip> <maske> <gateway>"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::netz::Ipv4;
        let teile: Vec<&str> = argumente.split_whitespace().collect();
        if teile.len() != 3 {
            println!("Benutzung: netz-ip <ip> <maske> <gateway>");
            println!("  Beispiel (QEMU-slirp): netz-ip 10.0.2.15 255.255.255.0 10.0.2.2");
            return;
        }
        let ip = Ipv4::parse(teile[0]);
        let maske = Ipv4::parse(teile[1]);
        let gateway = Ipv4::parse(teile[2]);
        match (ip, maske, gateway) {
            (Some(ip), Some(maske), Some(gateway)) => {
                crate::netz::konfig_setzen(ip, maske, gateway);
                println!("IP-Konfiguration gesetzt: {} / {} / Gateway {}", ip, maske, gateway);
            }
            _ => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("Ungueltige Adresse — jede muss vier Oktette 0..255 haben (a.b.c.d).");
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

/// netz-lausch — schaltet den Hexdump empfangener Ethernet-Frames an/aus.
/// Der `netz_task` dumpt dann jedes ankommende Frame; Verkehr erzeugt man
/// z. B. mit `arp-ping <ip>`.
struct NetzLausch;

impl Befehl for NetzLausch {
    fn name(&self) -> &'static str {
        "netz-lausch"
    }
    fn beschreibung(&self) -> &'static str {
        "Empfangene Ethernet-Frames hexdumpen (an/aus)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if keine_nic() {
            return;
        }
        if crate::netz::lausch_umschalten() {
            println!("netz-lausch AN - empfangene Frames werden gehexdumpt.");
            println!("Verkehr erzeugen z. B. mit: arp-ping <ip>");
        } else {
            println!("netz-lausch AUS.");
        }
    }
}

/// arp — zeigt den ARP-Cache (gelernte IP -> MAC, mit Alter).
struct Arp;

impl Befehl for Arp {
    fn name(&self) -> &'static str {
        "arp"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt den ARP-Cache (IP -> MAC)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let eintraege = crate::netz::arp::cache_eintraege();
        if eintraege.is_empty() {
            println!("ARP-Cache leer. (Aufloesen mit: arp-ping <ip>)");
            return;
        }
        println!("ARP-Cache:");
        for (ip, mac, alter_ms) in &eintraege {
            konsole::set_color(Color::LightCyan, Color::Black);
            print!("  {:<15} ", format!("{}", ip));
            konsole::set_color(Color::LightGray, Color::Black);
            println!("{}  (vor {} s gelernt)", crate::netz::ethernet::mac_text(mac), alter_ms / 1000);
        }
    }
}

/// arp-ping — löst die MAC hinter einer IP auf: schickt einen ARP-Request
/// und PUMPT den Empfang synchron (der kooperative Executor lässt während
/// eines Befehls keinen anderen Task laufen), bis die Antwort da ist oder
/// ein Timeout greift.
struct ArpPing;

impl Befehl for ArpPing {
    fn name(&self) -> &'static str {
        "arp-ping"
    }
    fn beschreibung(&self) -> &'static str {
        "Loest die MAC hinter einer IP auf: arp-ping <ip>"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::netz::Ipv4;
        if keine_nic() {
            return;
        }
        let ziel = match Ipv4::parse(argumente.trim()) {
            Some(ip) => ip,
            None => {
                println!("Benutzung: arp-ping <ip>   (z. B. arp-ping 10.0.2.2)");
                return;
            }
        };
        // Request senden (braucht eine konfigurierte Absender-IP).
        if let Err(fehler) = crate::netz::arp::anfrage_senden(ziel) {
            netz_fehler_ausgeben(fehler);
            return;
        }
        println!("ARP-Request an {} gesendet, warte auf Antwort ...", ziel);
        // Synchron pumpen: bis zu 2 Sekunden RX verarbeiten + Cache prüfen.
        let deadline = crate::zeit::ms_seit_boot() + 2000;
        loop {
            crate::netz::rx_verarbeiten();
            if let Some(mac) = crate::netz::arp::cache_suchen(ziel) {
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("{} ist bei {}", ziel, crate::netz::ethernet::mac_text(&mac));
                konsole::set_color(Color::LightGray, Color::Black);
                return;
            }
            if crate::zeit::ms_seit_boot() >= deadline {
                konsole::set_color(Color::Yellow, Color::Black);
                println!("Keine Antwort von {} (Timeout).", ziel);
                konsole::set_color(Color::LightGray, Color::Black);
                return;
            }
            x86_64::instructions::hlt();
        }
    }
}

/// Formatiert Mikrosekunden als "X,YZ ms" (zwei Nachkommastellen).
fn ms_text(us: u64) -> String {
    format!("{},{:02} ms", us / 1000, (us % 1000) / 10)
}

/// ping — der klassische Netzwerk-Meilenstein: schickt ICMP-Echo-Requests
/// an eine IP und misst die Round-Trip-Zeit über die TSC-Mikrosekunden-Uhr,
/// wie das echte ping. PUMPT den Empfang synchron (kooperativer Executor).
struct Ping;

impl Ping {
    /// Anzahl der Echos, die wir schicken.
    const ANZAHL: u16 = 4;
    /// Nutzlast-Größe (wie das klassische ping: 56 Datenbytes -> 64-Byte-ICMP).
    const DATEN_LEN: usize = 56;
    /// Unser ICMP-Identifier (fest — es läuft immer nur ein ping zugleich).
    const IDENT: u16 = 0x5057; // "PW" ~ SpeedOS-Ping
    /// Wartezeit pro Echo auf die Antwort.
    const TIMEOUT_MS: u64 = 1000;
    /// Abstand zwischen zwei Echos (wie das echte ping ~1 s, hier kürzer).
    const INTERVALL_MS: u64 = 500;

    /// Pumpt den Empfang bis `deadline_ms` und wartet dabei auf die Antwort
    /// für `sequenz`. Liefert die RTT (µs) + TTL, wenn sie eintrifft.
    fn auf_antwort_warten(sequenz: u16, start_us: u64, deadline_ms: u64) -> Option<(u64, u8)> {
        loop {
            crate::netz::rx_verarbeiten();
            if let Some(ttl) = crate::netz::icmp::antwort_empfangen(Ping::IDENT, sequenz) {
                return Some((crate::zeit::us_seit_boot() - start_us, ttl));
            }
            if crate::zeit::ms_seit_boot() >= deadline_ms {
                return None;
            }
            x86_64::instructions::hlt();
        }
    }
}

impl Befehl for Ping {
    fn name(&self) -> &'static str {
        "ping"
    }
    fn beschreibung(&self) -> &'static str {
        "Sendet ICMP-Echos und misst die RTT: ping <ip>"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::netz::Ipv4;
        if keine_nic() {
            return;
        }
        let ziel = match Ipv4::parse(argumente.trim()) {
            Some(ip) => ip,
            None => {
                println!("Benutzung: ping <ip>   (z. B. ping 10.0.2.2)");
                return;
            }
        };
        if crate::netz::unsere_ip().is_none() {
            netz_fehler_ausgeben(crate::netz::NetzFehler::NichtKonfiguriert);
            return;
        }

        // Eine erkennbare Nutzlast (stabiles Muster, wird zurückgespiegelt).
        let mut daten = vec![0u8; Ping::DATEN_LEN];
        for (i, b) in daten.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(0x10);
        }

        println!("PING {}: {} Datenbytes", ziel, Ping::DATEN_LEN);
        crate::netz::icmp::antworten_leeren();

        let mut gesendet = 0u32;
        let mut empfangen = 0u32;
        let mut summe_us = 0u64;
        let mut min_us = u64::MAX;
        let mut max_us = 0u64;

        for sequenz in 0..Ping::ANZAHL {
            let start_us = crate::zeit::us_seit_boot();
            if let Err(fehler) = crate::netz::icmp::echo_senden(ziel, Ping::IDENT, sequenz, &daten) {
                netz_fehler_ausgeben(fehler);
                return;
            }
            gesendet += 1;
            let deadline = crate::zeit::ms_seit_boot() + Ping::TIMEOUT_MS;
            match Ping::auf_antwort_warten(sequenz, start_us, deadline) {
                Some((rtt_us, ttl)) => {
                    empfangen += 1;
                    summe_us += rtt_us;
                    min_us = min_us.min(rtt_us);
                    max_us = max_us.max(rtt_us);
                    konsole::set_color(Color::LightGreen, Color::Black);
                    println!(
                        "{} Bytes von {}: seq={} ttl={} zeit={}",
                        Ping::DATEN_LEN + 8,
                        ziel,
                        sequenz,
                        ttl,
                        ms_text(rtt_us)
                    );
                    konsole::set_color(Color::LightGray, Color::Black);
                }
                None => {
                    konsole::set_color(Color::Yellow, Color::Black);
                    println!("Zeitueberschreitung fuer seq={}", sequenz);
                    konsole::set_color(Color::LightGray, Color::Black);
                }
            }
            // Bis zum nächsten Echo warten — und dabei weiter RX pumpen,
            // damit eingehende Pings an UNS trotzdem beantwortet werden.
            if sequenz + 1 < Ping::ANZAHL {
                let bis = crate::zeit::ms_seit_boot() + Ping::INTERVALL_MS;
                while crate::zeit::ms_seit_boot() < bis {
                    crate::netz::rx_verarbeiten();
                    x86_64::instructions::hlt();
                }
            }
        }

        // Statistik wie das echte ping.
        // gesendet ist hier immer >= 1 (bei Fehler kehren wir vorher zurück);
        // max(1) hält die Division sicher und clippy zufrieden.
        let verlust = (gesendet - empfangen) * 100 / gesendet.max(1);
        println!("--- {} Ping-Statistik ---", ziel);
        println!(
            "{} gesendet, {} empfangen, {}% Verlust",
            gesendet, empfangen, verlust
        );
        if empfangen > 0 {
            println!(
                "RTT min/schnitt/max = {} / {} / {}",
                ms_text(min_us),
                ms_text(summe_us / empfangen as u64),
                ms_text(max_us)
            );
        }
    }
}

/// Gibt eine Konfigurations-Zeile "  Name   Wert" aus.
fn status_zeile(name: &str, wert: &str) {
    konsole::set_color(Color::LightCyan, Color::Black);
    print!("  {:<9}", name);
    konsole::set_color(Color::LightGray, Color::Black);
    println!("{}", wert);
}

/// netz-status — zeigt die volle Netz-Konfiguration: IP, Maske, Gateway,
/// DNS, Lease und die Quelle (DHCP oder statisch).
struct NetzStatus;

impl Befehl for NetzStatus {
    fn name(&self) -> &'static str {
        "netz-status"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt IP, Maske, Gateway, DNS, Lease und Quelle"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::netz::Quelle;
        if keine_nic() {
            return;
        }
        println!("Netz-Status:");
        if let Some(mac) = crate::netz::mac() {
            status_zeile("MAC", &crate::netz::ethernet::mac_text(&mac));
        }
        let k = crate::netz::konfig();
        let quelle = match k.quelle {
            Quelle::Keine => "keine",
            Quelle::Statisch => "statisch (netz-ip)",
            Quelle::Dhcp => "DHCP",
        };
        status_zeile("Quelle", quelle);
        if !k.gesetzt {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("  Keine IP — 'dhcp' versuchen oder 'netz-ip <ip> <maske> <gateway>'.");
            konsole::set_color(Color::LightGray, Color::Black);
            return;
        }
        status_zeile("IP", &format!("{}", k.ip));
        status_zeile("Maske", &format!("{}", k.maske));
        status_zeile("Gateway", &format!("{}", k.gateway));
        status_zeile(
            "DNS",
            &if k.dns == crate::netz::Ipv4::NULL {
                String::from("-")
            } else {
                format!("{}", k.dns)
            },
        );
        if k.quelle == Quelle::Dhcp {
            status_zeile("Lease", &format!("{} s", k.lease_sekunden));
        }
    }
}

/// dhcp — bezieht (erneut) eine IP per DHCP und übernimmt sie.
struct Dhcp;

impl Befehl for Dhcp {
    fn name(&self) -> &'static str {
        "dhcp"
    }
    fn beschreibung(&self) -> &'static str {
        "Bezieht eine IP per DHCP (IP/Maske/Gateway/DNS)"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if keine_nic() {
            return;
        }
        println!("DHCP: sende DISCOVER, warte auf Angebot ...");
        match crate::netz::dhcp::beziehen(4000) {
            Some(e) => {
                crate::netz::konfig_setzen_dhcp(e.ip, e.maske, e.gateway, e.dns, e.lease_sekunden);
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("Lease bezogen: {} (Maske {}, Gateway {})", e.ip, e.maske, e.gateway);
                konsole::set_color(Color::LightGray, Color::Black);
                println!("DNS {}, Lease {} s. Details: netz-status", e.dns, e.lease_sekunden);
            }
            None => {
                konsole::set_color(Color::Yellow, Color::Black);
                println!("Keine DHCP-Antwort (Timeout). Alternativ: netz-ip <ip> <maske> <gateway>");
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

/// nslookup — löst einen Namen über den DNS-Server auf (A-Record).
struct Nslookup;

impl Befehl for Nslookup {
    fn name(&self) -> &'static str {
        "nslookup"
    }
    fn beschreibung(&self) -> &'static str {
        "Loest einen Namen zu einer IP auf: nslookup <name>"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let name = argumente.trim();
        if name.is_empty() {
            println!("Benutzung: nslookup <name>   (z. B. nslookup example.com)");
            return;
        }
        if keine_nic() {
            return;
        }
        if let Some(server) = crate::netz::dns_server() {
            status_zeile("Server", &format!("{}", server));
        }
        match crate::netz::dns::aufloesen(name) {
            Ok(ip) => {
                status_zeile("Name", name);
                konsole::set_color(Color::LightGreen, Color::Black);
                status_zeile("Adresse", &format!("{}", ip));
                konsole::set_color(Color::LightGray, Color::Black);
            }
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("Fehler: {}", fehler.meldung());
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

/// hole — DIE EINE ADRESSE FÜR „lade mir das".
///
/// ==========================================================================
/// EIN BEFEHL, ZWEI WEGE — und der Benutzer muss nicht wissen, welcher
///
/// SpeedOS kann http im KERNEL (Serie 5) und https in RING 3 (Serie 7). Das
/// ist eine Architektur-Entscheidung und keine Laune: Ein Fehler in 30k
/// Zeilen fremdem TLS-Code soll einen Prozess treffen, nicht den Kernel
/// (docs/tls-entscheidung.md). Nur — den Benutzer geht das nichts an. Er
/// tippt eine Adresse.
///
/// Also entscheidet dieser Befehl:
///   * `http://…`  -> der Kernel-Klient. Kein Prozess, kein TLS, schnell.
///   * `https://…` -> das Ring-3-Programm `holes`, Ausgabe durchgereicht.
///   * schemalos   -> https (2026 ist Klartext die Ausnahme).
///   * **http, das auf https weiterleitet** -> der Kernel-Klient meldet das
///     ausgerechnete Ziel (`KlientFehler::BrauchtTls`), und wir übergeben
///     mitten im Lauf an Ring 3. Dieser Fall ist im heutigen Web der
///     Normalfall, nicht die Ausnahme.
///
/// ZIELDATEI: Ein Name ohne `/` landet im Zuhause (`/platte/heim`), alles
/// andere wird wie gewohnt gegen das aktuelle Verzeichnis aufgelöst.
/// Löst eine Zieldatei auf: ein blosser Name landet im ZUHAUSE, alles mit
/// `/` wie gewohnt gegen das aktuelle Verzeichnis.
///
/// `hole example.com seite.html` soll nicht davon abhängen, wo man gerade
/// steht — heruntergeladene Dateien gehören ins Zuhause. Wer es anders will,
/// schreibt einen Pfad hin, und dann gilt der.
fn ziel_im_zuhause(kontext: &ShellKontext, wunsch: &str) -> String {
    if wunsch.contains('/') {
        kontext.aufloesen(wunsch)
    } else {
        fs::pfad_anhaengen(crate::explorer::start_ordner(), wunsch)
    }
}

/// Übergibt den Abruf an das Ring-3-Programm `holes`.
///
/// Warum ein PROZESS und nicht eine Kernel-Funktion: Weil TLS im User-Space
/// lebt und dort bleiben soll. Die Shell startet dafür kein Sonderkonstrukt,
/// sondern benutzt dieselbe Pipeline-Maschinerie wie `starte` — inklusive
/// Ausgabe-Durchreichung, Strg+C und Exit-Code.
fn an_ring3_uebergeben(
    kontext: &mut ShellKontext,
    url: &str,
    zieldatei: Option<&str>,
    von_http: Option<&str>,
) {
    if let Some(vorher) = von_http {
        konsole::set_color(Color::Yellow, Color::Black);
        println!("{} leitet auf https weiter — uebernommen von `holes`:", vorher);
        konsole::set_color(Color::LightGray, Color::Black);
    }
    if !crate::scheduler::aktiv() {
        println!("Fuer https braucht es einen Prozess, und der Scheduler ist nicht aktiv.");
        return;
    }
    // Der Abruf läuft im Programm `holes` — die Adresse steht dabei in der
    // Kommandozeile, also darf sie keine Leerzeichen enthalten. URLs haben
    // keine (und wenn doch, wäre die Adresse ohnehin kaputt).
    if url.split_whitespace().count() != 1 {
        println!("Die Adresse darf keine Leerzeichen enthalten.");
        return;
    }
    let mut zeile = alloc::format!("holes {}", url);
    if let Some(ziel) = zieldatei {
        if ziel.split_whitespace().count() != 1 {
            println!("Der Zielpfad darf keine Leerzeichen enthalten.");
            return;
        }
        zeile.push(' ');
        zeile.push_str(ziel);
    }
    pipeline_ausfuehren(kontext, &zeile);
}

/// `browser [adresse]` — die Seite ANZEIGEN statt sie zu holen.
///
/// ===================================================================
/// DER UNTERSCHIED ZU `hole`
///
/// `hole` besorgt Bytes und legt sie ab oder gibt sie aus — ein
/// Werkzeug fuer die Kommandozeile. `browser` ZEIGT die Seite, mit
/// Layout, Bildern und Links. Beide bleiben, weil beide gebraucht
/// werden: `hole` laesst sich in eine Pipe stecken, der Browser nicht.
///
/// ES BRAUCHT KEIN `&`: Der Befehl startet den Prozess selbst im
/// Hintergrund. Genau das ist der Sinn — `starte browser … &` ist der
/// allgemeine Weg, und wer nur eine Seite ansehen will, soll sich die
/// Regel mit dem Kaufmanns-Und nicht merken muessen.
struct BrowserBefehl;

impl Befehl for BrowserBefehl {
    fn name(&self) -> &'static str {
        "browser"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt eine Seite an: browser [url|datei]  (oeffnet ein Fenster)"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        let adresse = argumente.trim();
        let adresse = if adresse.is_empty() { None } else { Some(adresse) };
        if let Some(a) = adresse {
            if a.split_whitespace().count() != 1 {
                println!("Die Adresse darf keine Leerzeichen enthalten.");
                return;
            }
        }
        match crate::programme::browser_oeffnen(adresse) {
            Ok(pid) => {
                konsole::set_color(Color::DarkGray, Color::Black);
                println!("[PID {} im Hintergrund: browser]", pid);
                konsole::set_color(Color::LightGray, Color::Black);
            }
            Err(meldung) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("{}", meldung);
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

struct Hole;

impl Befehl for Hole {
    fn name(&self) -> &'static str {
        "hole"
    }
    fn beschreibung(&self) -> &'static str {
        "Laedt eine Seite (http UND https): hole <url> [zieldatei]"
    }
    fn ausfuehren(&self, argumente: &str, kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        if keine_nic() {
            return;
        }
        let mut teile = argumente.split_whitespace();
        let url_text = match teile.next() {
            Some(u) => u,
            None => {
                println!("Benutzung: hole <url> [zieldatei]");
                println!("  hole example.com                       (ohne Schema: https)");
                println!("  hole https://example.com seite.html    (-> /platte/heim/seite.html)");
                println!("  hole http://10.0.2.2:8000/datei.txt /platte/heim/datei.txt");
                return;
            }
        };
        let zieldatei = teile.next().map(|wunsch| ziel_im_zuhause(kontext, wunsch));

        // --- Schema erkennen (EINE getestete Stelle: speedhttp) ---
        let ziel = match crate::netz::http::ziel_parsen(url_text) {
            Ok(ziel) => ziel,
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("Fehler: {}", fehler.meldung());
                konsole::set_color(Color::LightGray, Color::Black);
                return;
            }
        };
        if ziel.tls {
            // Direkt der Ring-3-Weg.
            an_ring3_uebergeben(kontext, &ziel.als_text(), zieldatei.as_deref(), None);
            return;
        }

        println!("Hole {} ...", url_text);
        let (end_url, antwort) = match crate::netz::http::holen(url_text) {
            Ok(paar) => paar,
            // DIE ÜBERGABE: Der Server hat auf https weitergeleitet.
            Err(fehler) if fehler.tls_ziel().is_some() => {
                let neu = String::from(fehler.tls_ziel().expect("gerade geprüft"));
                an_ring3_uebergeben(kontext, &neu, zieldatei.as_deref(), Some(url_text));
                return;
            }
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("Fehler: {}", fehler.meldung());
                konsole::set_color(Color::LightGray, Color::Black);
                return;
            }
        };

        // Statuszeile (gruen bei 2xx, gelb sonst) + Kopfzeilen anzeigen.
        let farbe = if (200..300).contains(&antwort.status) {
            Color::LightGreen
        } else {
            Color::Yellow
        };
        konsole::set_color(farbe, Color::Black);
        println!("HTTP {} {}", antwort.status, antwort.grund);
        konsole::set_color(Color::LightGray, Color::Black);
        if end_url.als_text() != url_text {
            println!("(weitergeleitet nach {})", end_url.als_text());
        }
        for (name, wert) in &antwort.header {
            konsole::set_color(Color::LightCyan, Color::Black);
            print!("  {}: ", name);
            konsole::set_color(Color::LightGray, Color::Black);
            println!("{}", wert);
        }
        println!("Rumpf: {} Byte", antwort.rumpf.len());

        match zieldatei {
            // Speichern (Netz + Persistenz zusammen). Der Pfad ist schon
            // aufgeloest — `ziel_im_zuhause` hat das erledigt.
            Some(pfad) => {
                match fs::mit_fs(|f| f.schreiben(&pfad, &antwort.rumpf)) {
                    Ok(()) => {
                        // "Gespeichert" heisst "auf dem Medium" -> sync.
                        if let Err(fehler) = fs::sync() {
                            fs_fehler_ausgeben(fehler);
                            return;
                        }
                        konsole::set_color(Color::LightGreen, Color::Black);
                        println!("{} Byte nach {} gespeichert.", antwort.rumpf.len(), pfad);
                        konsole::set_color(Color::LightGray, Color::Black);
                    }
                    Err(fehler) => fs_fehler_ausgeben(fehler),
                }
            }
            // Anzeigen (nur bei Text-Inhalten, gedeckelt).
            None => {
                if !antwort.ist_text() {
                    println!("(kein Text-Inhalt — mit Zieldatei speichern: hole <url> <datei>)");
                    return;
                }
                let text = String::from_utf8_lossy(&antwort.rumpf);
                let zeigen = text.char_indices().nth(800).map(|(i, _)| i).unwrap_or(text.len());
                println!("--- Rumpf ---");
                print!("{}", &text[..zeigen]);
                if zeigen < text.len() {
                    println!();
                    println!("... (gekuerzt; ganz speichern: hole <url> <datei>)");
                } else {
                    println!();
                }
            }
        }
    }
}
/// ring3test — DER historische Beweis (Serie 6): CPU-Code laeuft in Ring 3
/// (User-Mode) und kehrt sauber zurueck; ein Absturz im User-Mode reisst den
/// Kernel NICHT mit.
struct Ring3Test;

impl Befehl for Ring3Test {
    fn name(&self) -> &'static str {
        "ring3test"
    }
    fn beschreibung(&self) -> &'static str {
        "Fuehrt Code in Ring 3 aus (User-Mode-Beweis + Absturz-Auffang)"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        konsole::set_color(Color::LightCyan, Color::Black);
        println!("=== Ring-3-Test (User-Mode) ===");
        konsole::set_color(Color::LightGray, Color::Black);

        // Teil 1: Erfolgs-Lauf (immer).
        println!("[1] Ring-3-Code druckt eine Nachricht per Syscall:");
        crate::ring3::ring3_erfolg();

        // Teil 2: Absturz-Lauf — nur mit Argument 'absturz' (er erzeugt
        // absichtlich einen Page Fault; die Meldung ist gewollt).
        match argumente.trim() {
            "absturz" => {
                println!();
                println!("[2] Ring-3-Code greift verboten auf Kernel-Speicher zu:");
                konsole::set_color(Color::Yellow, Color::Black);
                crate::ring3::ring3_absturz();
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("Der Kernel hat den Absturz ueberlebt.");
                konsole::set_color(Color::LightGray, Color::Black);
            }
            "stack" => {
                println!();
                println!("[2] Ring-3-Code pusht unter seinen Stack (Guard-Page):");
                konsole::set_color(Color::Yellow, Color::Black);
                crate::ring3::ring3_stack_ueberlauf();
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("Die Guard-Page hat den Stack-Ueberlauf gefangen.");
                konsole::set_color(Color::LightGray, Color::Black);
            }
            _ => {
                println!("(Absturz-Beweis:      'ring3test absturz' - erzeugt einen Page Fault)");
                println!("(Guard-Page-Beweis:   'ring3test stack'   - Stack-Ueberlauf in Ring 3)");
            }
        }
    }
}

/// adressraum — DER Isolations-Beweis (Serie 6, Teil 2): zwei Adressraeume,
/// dieselbe virtuelle Adresse, unterschiedlicher Inhalt. Und: Abreissen gibt
/// alle Frames zurueck.
struct AdressraumTest;

impl Befehl for AdressraumTest {
    fn name(&self) -> &'static str {
        "adressraum"
    }
    fn beschreibung(&self) -> &'static str {
        "Beweist Prozess-Isolation: gleiche Adresse, zwei Adressraeume"
    }
    fn ausfuehren(
        &self,
        _argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        use crate::adressraum::{self, AdressRaum};
        use x86_64::structures::paging::Page;
        use x86_64::VirtAddr;

        konsole::set_color(Color::LightCyan, Color::Black);
        println!("=== Adressraum-Test (Prozess-Isolation) ===");
        konsole::set_color(Color::LightGray, Color::Black);

        let probe = adressraum::USER_START + 0x10_0000;
        let (frei_vorher, _) = crate::memory::frame_statistik();

        let mut a = match AdressRaum::neu() {
            Ok(r) => r,
            Err(f) => {
                println!("Adressraum A anlegen fehlgeschlagen: {:?}", f);
                return;
            }
        };
        let mut b = match AdressRaum::neu() {
            Ok(r) => r,
            Err(f) => {
                println!("Adressraum B anlegen fehlgeschlagen: {:?}", f);
                return;
            }
        };
        let page = Page::containing_address(VirtAddr::new(probe));
        if a.map_benutzer(page).is_err() || b.map_benutzer(page).is_err() {
            println!("Mapping fehlgeschlagen.");
            return;
        }
        let _ = a.schreiben(VirtAddr::new(probe), b"Ich bin Prozess A");
        let _ = b.schreiben(VirtAddr::new(probe), b"Ich bin Prozess B");

        println!("Virtuelle Adresse:  {:#x} (in BEIDEN Adressraeumen gemappt)", probe);
        println!("Physisch dahinter:  A = P4 {:#x} / B = P4 {:#x}",
            a.p4_frame().start_address().as_u64(),
            b.p4_frame().start_address().as_u64());
        println!();

        // Derselbe Lesevorgang, zweimal — nur CR3 unterscheidet sich.
        //
        // PLANUNGS-SPERRE (Serie 6, Teil 3): Zwischen `aktivieren()` und dem
        // Lesen darf KEIN Kontext-Wechsel liegen. Sonst käme der Kernel-Prozess
        // mit Kernel-CR3 zurück, und die User-Adresse waere ploetzlich
        // ungemappt — ein Page Fault in Ring 0, also ein Kernel-Halt. Dieselbe
        // Begruendung wie bei ring3::nach_ring3 (docs/scheduler-design.md §6).
        crate::scheduler::sperre_erhoehen();
        let mut puffer = [0u8; 17];
        for (name, raum) in [("A", &mut a), ("B", &mut b)] {
            raum.aktivieren();
            // unsafe: Der Adressraum ist aktiv und die Seite ist gemappt;
            // Ring 0 darf User-Seiten lesen.
            unsafe {
                core::ptr::copy_nonoverlapping(probe as *const u8, puffer.as_mut_ptr(), 17);
            }
            adressraum::kernel_aktivieren();
            konsole::set_color(Color::LightGreen, Color::Black);
            println!(
                "  Nach dem Wechsel zu {}: \"{}\"",
                name,
                core::str::from_utf8(&puffer).unwrap_or("?")
            );
            konsole::set_color(Color::LightGray, Color::Black);
        }
        crate::scheduler::sperre_senken();

        let besitz = a.frames_besitz() + b.frames_besitz();
        a.abreissen();
        b.abreissen();
        let (frei_nachher, _) = crate::memory::frame_statistik();
        println!();
        println!(
            "Abgerissen: {} Frames zurueckgegeben. Frei vorher {} / nachher {} -> {}",
            besitz,
            frei_vorher,
            frei_nachher,
            if frei_vorher == frei_nachher {
                "kein Leck"
            } else {
                "LECK!"
            }
        );
    }
}

/// prozesse — die Prozess-Tabelle des praeemptiven Schedulers.
struct Prozesse;

impl Befehl for Prozesse {
    fn name(&self) -> &'static str {
        "prozesse"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die Prozess-Tabelle (PID, Name, Zustand, CPU-Zeit)"
    }
    fn ausfuehren(
        &self,
        _argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        if !crate::scheduler::aktiv() {
            println!("Der Scheduler ist nicht aktiv (kein scheduler::init()).");
            return;
        }
        konsole::set_color(Color::LightCyan, Color::Black);
        println!(
            "{:>4}  {:<26} {:<11} {:>10} {:>8} {:>7} {:>8}",
            "PID", "Name", "Zustand", "CPU-Zeit", "Praeem.", "Abgab.", "Syscalls"
        );
        konsole::set_color(Color::LightGray, Color::Black);
        for zeile in crate::scheduler::momentaufnahme() {
            println!(
                "{:>4}  {:<26} {:<11} {:>10} {:>8} {:>7} {:>8}",
                zeile.pid,
                zeile.name,
                zeile.zustand.text(),
                crate::taskmanager::cpu_zeit_text(zeile.cpu_us),
                zeile.praemptionen,
                zeile.abgaben,
                zeile.syscalls
            );
        }
        println!();
        println!(
            "Zeitscheibe {} Ticks (~{} ms), {} Kontext-Wechsel seit dem Boot.",
            crate::scheduler::SCHEIBE_TICKS,
            crate::zeit::ms_von_ticks(crate::scheduler::SCHEIBE_TICKS as u64),
            crate::scheduler::wechsel_gesamt()
        );
        println!("PID 0 ist der Kernel-Prozess: in IHM laufen alle Kernel-Tasks kooperativ.");
    }
}

/// prozess-start — plant einen Demo-Prozess ein (zaehler | schlaefer | absturz).
struct ProzessStart;

impl Befehl for ProzessStart {
    fn name(&self) -> &'static str {
        "prozess-start"
    }
    fn beschreibung(&self) -> &'static str {
        "Plant einen Ring-3-Prozess ein (zaehler <A-Z> | schlaefer | absturz)"
    }
    fn ausfuehren(
        &self,
        argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        let mut teile = argumente.split_whitespace();
        let art = teile.next().unwrap_or("zaehler");
        let prozess = match art {
            "zaehler" => {
                // Kennung: erstes Zeichen des zweiten Arguments, sonst 'A'.
                let kennung = teile
                    .next()
                    .and_then(|s| s.bytes().next())
                    .unwrap_or(b'A');
                crate::prozess::zaehler_prozess(kennung)
            }
            "schlaefer" => {
                let ms = teile.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(200);
                crate::prozess::schlaefer_prozess(ms)
            }
            "absturz" => crate::prozess::absturz_prozess(),
            _ => {
                println!("Benutzung: prozess-start [zaehler <Kennung> | schlaefer <ms> | absturz]");
                return;
            }
        };
        match prozess.and_then(crate::scheduler::einplanen) {
            Some(pid) => {
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("Prozess mit PID {} eingeplant — er laeuft ab dem naechsten Tick.", pid);
                konsole::set_color(Color::LightGray, Color::Black);
                println!("(Ausgaben laufen SERIELL — ein Syscall darf den MANAGER-Lock nicht");
                println!(" anfassen. 'prozesse' zeigt die CPU-Zeit, 'prozess-stop {}' beendet.)", pid);
            }
            None => println!("Prozess konnte nicht eingeplant werden (Tabelle voll oder kein Speicher)."),
        }
    }
}

/// prozess-stop — beendet einen Prozess.
struct ProzessStop;

impl Befehl for ProzessStop {
    fn name(&self) -> &'static str {
        "prozess-stop"
    }
    fn beschreibung(&self) -> &'static str {
        "Beendet einen Prozess (prozess-stop <pid>, 'alle' fuer alle)"
    }
    fn ausfuehren(
        &self,
        argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        let argument = argumente.trim();
        if argument.is_empty() {
            println!("Benutzung: prozess-stop <pid>   (oder 'prozess-stop alle')");
            return;
        }
        let mut beendet = 0usize;
        if argument == "alle" {
            for zeile in crate::scheduler::momentaufnahme() {
                if zeile.ist_user && crate::scheduler::beenden(zeile.pid) {
                    beendet += 1;
                }
            }
        } else {
            match argument.parse::<crate::prozess::Pid>() {
                Ok(pid) if crate::scheduler::beenden(pid) => beendet = 1,
                Ok(pid) => println!("PID {} gibt es nicht (oder sie ist schon beendet).", pid),
                Err(_) => println!("'{}' ist keine Prozess-Nummer.", argument),
            }
        }
        if beendet > 0 {
            println!(
                "{} Prozess(e) beendet. Der Aufraeum-Task gibt Adressraum und",
                beendet
            );
            println!("Kernel-Stack in Kuerze zurueck (nie im Interrupt — dort ist Freigeben verboten).");
        }
    }
}

/// praemptionstest — DER PRAEMPTIONS-BEWEIS als Shell-Befehl (Serie 6, Teil 3):
/// zwei Ring-3-Prozesse, die in Endlosschleifen zaehlen und NIE freiwillig
/// abgeben. Verschraenkt sich ihre Ausgabe, wurde ihnen die CPU WEGGENOMMEN.
struct PraemptionsTest;

impl Befehl for PraemptionsTest {
    fn name(&self) -> &'static str {
        "praemptionstest"
    }
    fn beschreibung(&self) -> &'static str {
        "Beweist Praemption: zwei Zaehler-Prozesse ohne freiwillige Abgabe"
    }
    fn ausfuehren(
        &self,
        argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        use crate::prozess::Pid;

        if !crate::scheduler::aktiv() {
            println!("Der Scheduler ist nicht aktiv.");
            return;
        }
        let sekunden = argumente
            .trim()
            .parse::<u64>()
            .unwrap_or(2)
            .clamp(1, 10);

        konsole::set_color(Color::LightCyan, Color::Black);
        println!("=== Praemptionstest (Serie 6, Teil 3) ===");
        konsole::set_color(Color::LightGray, Color::Black);
        println!("Zwei Ring-3-Prozesse zaehlen in Endlosschleifen und drucken per");
        println!("Syscall. Ihr Maschinencode enthaelt KEINE freiwillige Abgabe.");
        println!("Laufzeit: {} s. (Die Zaehler-Ausgabe laeuft seriell.)", sekunden);
        println!();

        crate::scheduler::spur_loeschen();
        let mut pids: Vec<Pid> = Vec::new();
        for kennung in *b"AB" {
            match crate::prozess::zaehler_prozess(kennung).and_then(crate::scheduler::einplanen) {
                Some(pid) => pids.push(pid),
                None => {
                    println!("Prozess '{}' konnte nicht eingeplant werden.", kennung as char);
                    for pid in &pids {
                        crate::scheduler::beenden(*pid);
                    }
                    return;
                }
            }
        }

        // Warten — und dabei selbst verdraengt werden. Der Shell-Task laeuft im
        // Kernel-Prozess; hlt gibt die CPU frei, bis der Timer tickt.
        let ziel = crate::zeit::ms_seit_boot() + sekunden * 1000;
        while crate::zeit::ms_seit_boot() < ziel {
            x86_64::instructions::hlt();
        }

        // Auswertung: erst die Zahlen aus der Tabelle, dann die Ausgabe-Spur.
        let moment = crate::scheduler::momentaufnahme();
        konsole::set_color(Color::LightCyan, Color::Black);
        println!("{:>4}  {:<12} {:>10} {:>8} {:>7}", "PID", "Name", "CPU-Zeit", "Praeem.", "Abgab.");
        konsole::set_color(Color::LightGray, Color::Black);
        let mut praemptionen_min = u64::MAX;
        let mut abgaben_summe = 0u64;
        for zeile in moment.iter().filter(|z| pids.contains(&z.pid)) {
            println!(
                "{:>4}  {:<12} {:>10} {:>8} {:>7}",
                zeile.pid,
                zeile.name,
                crate::taskmanager::cpu_zeit_text(zeile.cpu_us),
                zeile.praemptionen,
                zeile.abgaben
            );
            praemptionen_min = praemptionen_min.min(zeile.praemptionen);
            abgaben_summe += zeile.abgaben;
        }

        let spur: Vec<Pid> = crate::scheduler::spur_lesen().iter().map(|(p, _)| *p).collect();
        let befund = crate::scheduler::spur_auswerten(&spur);
        println!();
        println!("Ausgabe-Reihenfolge der ERSTEN {} Ausgaben (| = Prozess-Wechsel):", crate::scheduler::SPUR_LAENGE);
        // Die Spur als Blockfolge, damit die Verschraenkung ins Auge springt.
        let mut zeile = String::new();
        let mut letzte: Option<Pid> = None;
        for (pid, zeichen) in crate::scheduler::spur_lesen().iter().take(120) {
            if letzte != Some(*pid) {
                zeile.push(' ');
                zeile.push('|');
                zeile.push(' ');
                letzte = Some(*pid);
            }
            zeile.push(*zeichen as char);
        }
        println!(" {}", zeile.trim());
        println!();
        println!(
            "{} Ausgaben von {} Prozessen, {} Wechsel in der Spur.",
            befund.gesamt, befund.beteiligte, befund.wechsel
        );

        // Das Urteil — genau die drei Aussagen aus tests/scheduler.rs.
        let bewiesen = befund.beteiligte >= 2 && befund.wechsel >= 2
            && praemptionen_min > 0 && abgaben_summe == 0;
        if bewiesen {
            konsole::set_color(Color::LightGreen, Color::Black);
            println!("BEWIESEN: Beide kamen voran, beide wurden aus Ring 3 verdraengt,");
            println!("und KEINER hat freiwillig abgegeben. Die CPU wurde weggenommen.");
        } else {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("Kein vollstaendiger Beweis in diesem Lauf — laenger laufen lassen:");
            println!("praemptionstest {}", (sekunden * 2).min(10));
        }
        konsole::set_color(Color::LightGray, Color::Black);

        for pid in &pids {
            crate::scheduler::beenden(*pid);
        }
        println!("Beide Prozesse beendet.");
    }
}

// ---------------------------------------------------------------------------
// Serie 6, Teil 5: echte Programme starten
// ---------------------------------------------------------------------------

/// Wie lange `starte` auf das Ende eines Programms wartet, bevor es
/// aufgibt und den Prozess im Hintergrund weiterlaufen laesst.
///
/// Grosszuegig, weil `netzhole` DNS (bis 3 Versuche) plus TCP-Handshake
/// plus Uebertragung braucht — das koennen ueber eine langsame Leitung
/// mehrere Sekunden sein.
const STARTE_FRIST_MS: u64 = 120_000;

/// Höchstzahl von Stufen in einer Pipeline. Mehr als das würde die
/// Prozess-Tabelle (`MAX_PROZESSE`) sprengen, in der auch der Kernel-Prozess
/// und alles andere Platz braucht.
const MAX_PIPELINE: usize = 4;

/// starte — DIE SHELL WIRD ZUR SHELL (Serie 6, Teil 6).
///
/// ==========================================================================
/// WAS SICH GEGENÜBER TEIL 5 GEÄNDERT HAT — UND WARUM
///
/// In Teil 5 schrieb ein Programm auf Handle 1, und das landete über
/// `konsole::_print` direkt im Terminal. Das war der KERNEL-AUSGABEPFAD: Der
/// Kernel hat für den Prozess gedruckt.
///
/// Jetzt legt die Shell eine PIPE an und gibt deren Schreib-Ende dem Kind
/// als Handle 1. Das Kind schreibt in eine Pipe; die Shell liest heraus und
/// druckt. Für das Programm ändert sich nichts (es schreibt auf Handle 1
/// wie immer) — aber für das SYSTEM ändert sich alles:
///
///   * Die Ausgabe ist jetzt ein DATENSTROM, kein Seiteneffekt. Und was ein
///     Strom ist, kann man umleiten — genau daraus wird `a | b`.
///   * Das Kind BLOCKIERT, wenn niemand liest (die Pipe läuft voll), statt
///     unbegrenzt in den Kernel zu drucken.
///   * Der Kernel druckt nicht mehr für fremden Code.
///
/// Der Preis ist eine Umdrehung mehr, und der Gewinn ist eine Pipeline.
/// ==========================================================================
struct Starte;

impl Befehl for Starte {
    fn name(&self) -> &'static str {
        "starte"
    }
    fn beschreibung(&self) -> &'static str {
        "Startet Programme, auch als Pipeline (starte a [args] | b [args])"
    }
    fn ausfuehren(
        &self,
        argumente: &str,
        kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        if argumente.trim().is_empty() {
            println!("Benutzung: starte <pfad|name> [argumente ...]");
            println!("           starte <a> [args] | <b> [args]     (Pipeline)");
            println!("           starte <name> &                    (im Hintergrund)");
            println!();
            println!("Mitgelieferte Programme (auch ohne Pfad aufrufbar):");
            for zeile in crate::programme::uebersicht() {
                println!("  {}", zeile);
            }
            println!();
            println!("Strg+C beendet das laufende Programm.");
            println!("Ein '&' am Ende startet, ohne zu warten — noetig fuer Programme");
            println!("mit eigenem FENSTER (siehe unten).");
            return;
        }
        if !crate::scheduler::aktiv() {
            println!("Der Scheduler ist nicht aktiv — ohne ihn kann kein Prozess laufen.");
            return;
        }
        pipeline_ausfuehren(kontext, argumente);
    }
}

/// Eine Stufe der Pipeline: ein Programm mit seinen Argumenten.
struct Stufe {
    pfad: String,
    /// argv, inklusive argv[0].
    argumente: Vec<String>,
}

/// Führt eine (ein- oder mehrstufige) Pipeline aus und wartet auf sie.
///
/// DER AUFBAU einer Pipeline `a | b`:
///
/// ```text
///                    Pipe 1                    Pipe 2
///   [ a ] --Handle 1--> [====] --Handle 0--> [ b ] --Handle 1--> [====] --> Shell
/// ```
///
/// Die LETZTE Pipe gehört der Shell: Sie liest daraus und druckt ins
/// Terminal. Dadurch ist der Fall „ein Programm" kein Sonderfall — er ist
/// einfach eine Pipeline der Länge 1.
///
/// DER ENTSCHEIDENDE SCHRITT ZUM SCHLUSS: Die Shell schliesst ihre EIGENEN
/// Kopien aller weitergegebenen Enden. Täte sie das nicht, bliebe sie selbst
/// als Schreiber von Pipe 1 eingetragen — `b` bekäme nie ein Dateiende und
/// wartete für immer. Das ist der Klassiker bei Pipes, und die Besitz-Zähler
/// in pipe.rs sind genau dafür da.
fn pipeline_ausfuehren(kontext: &mut ShellKontext, eingabe: &str) {
    use crate::pipe::{self, Ende};
    use crate::syscall::handle::KernelObjekt;

    // ======================================================================
    // DER HINTERGRUND-START (Serie 8, Teil 1) — und warum es ihn braucht
    //
    // Ein `&` am Zeilenende heisst: einplanen, PID melden, ZURUECKKEHREN.
    // Ohne Warten, ohne Ausgabe-Pumpe.
    //
    // Bis Serie 7 war das nur Bequemlichkeit. Mit Fenstern ist es eine
    // NOTWENDIGKEIT, und der Grund ist die kooperative Natur von PID 0:
    // Solange ein Shell-Befehl synchron laeuft, kommt KEIN anderer
    // Kernel-Task dran — auch der COMPOSITOR nicht. Ein Programm mit
    // eigenem Fenster wuerde also brav zeichnen, und niemand wuerde es je
    // sehen; die Uhr in der Taskleiste bliebe stehen, bis das Programm
    // fertig ist. (Genau so ist es beim ersten Versuch passiert.)
    //
    // Die Alternative waere gewesen, die Pump-Schleife den Compositor
    // treiben zu lassen. Das ginge nicht: Der Compositor ist ein
    // async-Task im Executor, und ein synchroner Befehl kann den Executor
    // nicht betreten — deshalb steht das seit Serie 6 so in CLAUDE.md.
    //
    // Ein Hintergrund-Prozess bekommt KEINE Ausgabe-Pipe, sondern die
    // Standard-Ausgabe der Shell. Sonst muesste jemand seine Pipe leeren,
    // und genau den gibt es dann nicht mehr — nach 64 KiB Ausgabe wuerde
    // er fuer immer blockieren.
    // ======================================================================
    let (eingabe, hintergrund) = match eingabe.trim().strip_suffix('&') {
        Some(rest) => (rest.trim(), true),
        None => (eingabe, false),
    };
    if hintergrund && eingabe.is_empty() {
        println!("Was soll im Hintergrund starten? (starte <name> &)");
        return;
    }

    // --- 1. Die Eingabe in Stufen zerlegen ---
    let mut stufen: Vec<Stufe> = Vec::new();
    for abschnitt in eingabe.split('|') {
        let mut teile = abschnitt.split_whitespace();
        let wunsch = match teile.next() {
            Some(wunsch) => wunsch,
            None => {
                println!("Leere Stufe in der Pipeline (ein '|' zu viel?).");
                return;
            }
        };
        let pfad = pfad_fuer_programm(kontext, wunsch);
        // argv[0] ist per Konvention der Programmname — so, wie es jedes
        // Unix seit 1971 macht.
        let mut argumente: Vec<String> = vec![String::from(wunsch)];
        argumente.extend(teile.map(String::from));
        stufen.push(Stufe { pfad, argumente });
    }
    if stufen.len() > MAX_PIPELINE {
        println!("Höchstens {} Stufen je Pipeline.", MAX_PIPELINE);
        return;
    }
    if hintergrund {
        if stufen.len() > 1 {
            println!("Eine Pipeline laesst sich (noch) nicht in den Hintergrund schicken —");
            println!("dafuer muesste jemand die Zwischen-Pipes leeren.");
            return;
        }
        hintergrund_starten(&stufen[0]);
        return;
    }

    // --- 2. Für jede Stufe eine Ausgabe-Pipe anlegen ---
    let mut pipes: Vec<pipe::PipeId> = Vec::new();
    for _ in 0..stufen.len() {
        match pipe::anlegen() {
            Some(id) => pipes.push(id),
            None => {
                println!("Keine Pipe mehr frei (höchstens {}).", pipe::MAX_PIPES);
                for id in &pipes {
                    pipe::ende_schliessen(*id, Ende::Lesen);
                    pipe::ende_schliessen(*id, Ende::Schreiben);
                }
                return;
            }
        }
    }

    // --- 3. Die Prozesse starten und verdrahten ---
    let mut pids: Vec<crate::prozess::Pid> = Vec::new();
    let mut fehlgeschlagen = false;
    for (index, stufe) in stufen.iter().enumerate() {
        // Eingabe: das Lese-Ende der VORHERIGEN Pipe (die erste Stufe erbt
        // nichts — sie hat keine Eingabe).
        let erbe_eingabe = if index == 0 {
            None
        } else {
            pipe::ende_uebernehmen(pipes[index - 1], Ende::Lesen);
            Some(KernelObjekt::PipeLesen(pipes[index - 1]))
        };
        // Ausgabe: das Schreib-Ende der EIGENEN Pipe.
        pipe::ende_uebernehmen(pipes[index], Ende::Schreiben);
        let erbe_ausgabe = Some(KernelObjekt::PipeSchreiben(pipes[index]));

        let argumente: Vec<&str> = stufe.argumente.iter().map(|s| s.as_str()).collect();
        match crate::prozess::prozess_starten_mit(
            &stufe.pfad,
            &argumente,
            None, // Elternteil ist die SHELL (Kernel) — kein User-Prozess
            erbe_eingabe,
            erbe_ausgabe,
            false,
        ) {
            Ok(pid) => pids.push(pid),
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!(
                    "'{}' konnte nicht gestartet werden: {}",
                    stufe.pfad,
                    fehler.meldung()
                );
                konsole::set_color(Color::LightGray, Color::Black);
                // Die schon übernommenen Enden dieser Stufe wieder abgeben
                // (die Handles sind nie in einem Prozess gelandet).
                if index > 0 {
                    pipe::ende_schliessen(pipes[index - 1], Ende::Lesen);
                }
                pipe::ende_schliessen(pipes[index], Ende::Schreiben);
                fehlgeschlagen = true;
                break;
            }
        }
    }

    // --- 4. DIE EIGENEN KOPIEN SCHLIESSEN (siehe Funktions-Doku) ---
    // Behalten wird NUR das Lese-Ende der letzten Pipe — daraus liest die
    // Shell gleich die Ausgabe.
    let ausgabe_pipe = *pipes.last().expect("mindestens eine Stufe");
    for (index, id) in pipes.iter().enumerate() {
        pipe::ende_schliessen(*id, Ende::Schreiben);
        if index + 1 != pipes.len() {
            pipe::ende_schliessen(*id, Ende::Lesen);
        }
    }

    if !fehlgeschlagen {
        konsole::set_color(Color::DarkGray, Color::Black);
        if pids.len() == 1 {
            println!("[PID {} gestartet: {}]", pids[0], stufen[0].pfad);
        } else {
            let namen: Vec<String> = stufen
                .iter()
                .zip(&pids)
                .map(|(stufe, pid)| alloc::format!("{} (PID {})", stufe.argumente[0], pid))
                .collect();
            println!("[Pipeline: {}]", namen.join(" | "));
        }
        konsole::set_color(Color::LightGray, Color::Black);
    }

    // --- 5. Ausgabe durchreichen und auf das Ende warten ---
    let enden = if pids.is_empty() {
        Vec::new()
    } else {
        ausgabe_pumpen_und_warten(kontext, ausgabe_pipe, &pids)
    };
    pipe::ende_schliessen(ausgabe_pipe, Ende::Lesen);

    // --- 6. Die Exit-Codes zeigen ---
    for (index, pid) in pids.iter().enumerate() {
        let name = &stufen[index].argumente[0];
        // Erst das in der Pump-Schleife GEERNTETE Ergebnis, sonst noch
        // einmal kurz warten (falls einer die Frist knapp verpasst hat).
        let ende = enden
            .get(index)
            .copied()
            .flatten()
            .or_else(|| crate::scheduler::warten_auf(*pid, 2_000));
        match ende {
            Some(ende) => {
                let code = ende.code();
                konsole::set_color(if code == 0 { Color::LightGreen } else { Color::Yellow },
                                   Color::Black);
                println!("[{} (PID {}) {} — Exit-Code {}]", name, pid, ende.text(), code);
                konsole::set_color(Color::LightGray, Color::Black);
            }
            None => {
                konsole::set_color(Color::Yellow, Color::Black);
                println!("[{} (PID {}) laeuft noch — 'prozess-stop {}' beendet ihn.]",
                         name, pid, pid);
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

/// Startet EIN Programm im Hintergrund: einplanen, PID melden, zurück.
///
/// Keine Pipe, keine Pump-Schleife, kein Warten — die Shell ist sofort
/// wieder da, und damit läuft der Executor weiter (Compositor, Uhr,
/// Netz-Task). Der Prozess erbt die Standard-Ausgabe der Shell; seine
/// Ausgabe erscheint also mitten im Terminal, wo der Benutzer gerade
/// tippt. Das ist unschön und ehrlich — die Alternative wäre eine Pipe
/// ohne Leser, und die würde nach 64 KiB für immer blockieren.
fn hintergrund_starten(stufe: &Stufe) {
    let argumente: Vec<&str> = stufe.argumente.iter().map(|s| s.as_str()).collect();
    match crate::prozess::prozess_starten_mit(
        &stufe.pfad,
        &argumente,
        None,
        None,
        // `None` = nicht umleiten: Das Kind bekommt den Kernel-Standard,
        // also Bildschirm UND seriell.
        None,
        false,
    ) {
        Ok(pid) => {
            konsole::set_color(Color::DarkGray, Color::Black);
            println!("[PID {} im Hintergrund: {}]", pid, stufe.pfad);
            println!("[Beenden mit: prozess-stop {}]", pid);
            konsole::set_color(Color::LightGray, Color::Black);
        }
        Err(fehler) => {
            konsole::set_color(Color::LightRed, Color::Black);
            println!(
                "'{}' konnte nicht gestartet werden: {}",
                stufe.pfad,
                fehler.meldung()
            );
            konsole::set_color(Color::LightGray, Color::Black);
        }
    }
}

/// Liest die Ausgabe-Pipe leer und druckt sie ins Terminal — bis Dateiende,
/// Strg+C oder Fristablauf.
///
/// DIE SCHLEIFE IST DER GRUND, WARUM ES NICHT KLEMMT: Eine Pipe fasst 4 KiB.
/// Ein Programm, das mehr ausgibt, blockiert beim Schreiben, bis jemand
/// liest — also muss die Shell WÄHREND der Laufzeit lesen, nicht danach.
/// „Erst warten, dann Ausgabe abholen" wäre ein Deadlock, sobald ein
/// Programm mehr als 4 KiB produziert.
///
/// Solange wir hier `hlt`-en, laufen die PROZESSE weiter (der Timer nimmt
/// uns die CPU weg). Was steht, sind die kooperativen Kernel-Tasks —
/// derselbe bewusste Preis wie bei `praemptionstest`.
fn ausgabe_pumpen_und_warten(
    kontext: &ShellKontext,
    ausgabe_pipe: crate::pipe::PipeId,
    pids: &[crate::prozess::Pid],
) -> Vec<Option<crate::prozess::ProzessEnde>> {
    use crate::pipe::{self, PipeErgebnis};

    // Die geernteten Ergebnisse — eingesammelt, BEVOR `aufraeumen` die
    // Tabelleneinträge löscht. Danach wäre der Exit-Code unwiederbringlich
    // weg (genau der Fehler, den die erste Fassung hatte: sie meldete
    // „laeuft noch" für längst beendete Prozesse).
    let mut enden: Vec<Option<crate::prozess::ProzessEnde>> = alloc::vec![None; pids.len()];

    // Ein verspätetes Strg+C von vorhin darf dieses Programm nicht treffen.
    crate::shell::sitzung::abbruch_loeschen(kontext.sitzung);

    let frist = crate::zeit::ms_seit_boot() + STARTE_FRIST_MS;
    let mut puffer = [0u8; 512];
    loop {
        match pipe::lesen(ausgabe_pipe, &mut puffer) {
            // Dateiende: Alle Schreiber sind weg, die Pipeline ist durch.
            PipeErgebnis::Bytes(0) => break,
            PipeErgebnis::Bytes(n) => {
                // Als BYTES ausgeben: Die Ausgabe eines Programms muss kein
                // gültiges UTF-8 sein.
                match core::str::from_utf8(&puffer[..n]) {
                    Ok(text) => print!("{}", text),
                    Err(_) => {
                        for byte in &puffer[..n] {
                            print!("{}", *byte as char);
                        }
                    }
                }
                continue; // sofort weiterlesen, solange etwas da ist
            }
            // Leer, aber es gibt noch Schreiber -> warten.
            PipeErgebnis::Blockiert => {}
            PipeErgebnis::Abgebrochen | PipeErgebnis::Ungueltig => break,
        }

        // Ergebnisse einsammeln, SOLANGE die Einträge noch da sind.
        for (index, pid) in pids.iter().enumerate() {
            if enden[index].is_none() {
                enden[index] = crate::scheduler::ende_abfragen(*pid);
            }
        }

        // STRG+C: den ganzen Vordergrund beenden.
        if crate::shell::sitzung::abbruch_abholen(kontext.sitzung) {
            konsole::set_color(Color::Yellow, Color::Black);
            println!();
            println!("^C — Vordergrund-Prozess(e) werden beendet.");
            konsole::set_color(Color::LightGray, Color::Black);
            for pid in pids {
                crate::scheduler::beenden(*pid);
            }
            // NICHT sofort abbrechen: Noch gepufferte Ausgabe soll heraus,
            // und das Dateiende kommt, sobald die Handle-Tabellen der
            // beendeten Prozesse abgeräumt sind.
        }
        if crate::zeit::ms_seit_boot() >= frist {
            konsole::set_color(Color::Yellow, Color::Black);
            println!();
            println!("[Frist von {} s abgelaufen.]", STARTE_FRIST_MS / 1000);
            konsole::set_color(Color::LightGray, Color::Black);
            break;
        }
        // Beendete Prozesse abräumen — erst dadurch fallen ihre
        // Handle-Tabellen und damit ihre Pipe-Enden.
        crate::scheduler::aufraeumen();
        crate::zeit::warte_auf_interrupt();
    }

    // Zum Schluss noch einmal: Wer zwischen dem letzten Durchgang und dem
    // Dateiende fertig wurde, wird hier eingesammelt.
    for (index, pid) in pids.iter().enumerate() {
        if enden[index].is_none() {
            enden[index] = crate::scheduler::ende_abfragen(*pid);
        }
    }
    enden
}

/// Bestimmt den Pfad zu einem Programm: erst als (ggf. relative) Pfad-
/// Angabe, sonst als Kurzname im Programm-Verzeichnis.
fn pfad_fuer_programm(kontext: &ShellKontext, wunsch: &str) -> String {
    let direkt = kontext.aufloesen(wunsch);
    if fs::mit_fs(|dateisystem| dateisystem.node_typ(&direkt)) == Ok(NodeTyp::Datei) {
        return direkt;
    }
    // Kein direkter Treffer: Kurzname im Programm-Verzeichnis probieren.
    let im_ordner = crate::programme::pfad(wunsch);
    if fs::mit_fs(|dateisystem| dateisystem.node_typ(&im_ordner)) == Ok(NodeTyp::Datei) {
        return im_ordner;
    }
    // Nichts gefunden — den direkten Pfad zurueckgeben, damit die
    // Fehlermeldung den nennt, den der Benutzer gemeint hat.
    direkt
}

/// programme — zeigt die mitgelieferten User-Programme.
struct Programme;

impl Befehl for Programme {
    fn name(&self) -> &'static str {
        "programme"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die mitgelieferten User-Space-Programme"
    }
    fn ausfuehren(
        &self,
        _argumente: &str,
        _kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        let ordner = crate::programme::verzeichnis();
        konsole::set_color(Color::LightCyan, Color::Black);
        println!("Mitgelieferte Programme in {}:", ordner);
        konsole::set_color(Color::LightGray, Color::Black);
        for zeile in crate::programme::uebersicht() {
            println!("  {}", zeile);
        }
        println!();
        println!("Diese Programme sind KEIN Kernel-Code: Sie liegen als eigene");
        println!("ELF-Dateien auf der Platte, laufen in Ring 3 in einem eigenen");
        println!("Adressraum und erreichen SpeedOS nur ueber int 0x80.");
        println!();
        println!("Starten mit:  starte hallo");
        println!("              starte kopiere /platte/heim/a.txt /platte/heim/b.txt");
        println!("              starte netzhole http://example.com");

        // Und was liegt WIRKLICH im Ordner? (Der Benutzer darf dort eigene
        // Programme ablegen — sie sind nichts Besonderes.)
        if let Ok(eintraege) = fs::mit_fs(|dateisystem| dateisystem.liste(ordner)) {
            let fremde: Vec<&crate::fs::DirEintrag> = eintraege
                .iter()
                .filter(|eintrag| {
                    !crate::programme::PROGRAMME
                        .iter()
                        .any(|programm| programm.name == eintrag.name)
                })
                .collect();
            if !fremde.is_empty() {
                println!();
                println!("Ausserdem im Ordner (selbst abgelegt):");
                for eintrag in fremde {
                    println!("  {:<10} {:>7} B", eintrag.name, eintrag.groesse);
                }
            }
        }
    }
}

/// elfinfo — zeigt, was der Lader in einer Programmdatei sieht.
///
/// Ein Diagnose-Werkzeug, aber auch ein LEHR-Werkzeug: Es macht sichtbar,
/// woraus ein Programm besteht (Segmente, Rechte, Einsprung) und warum eine
/// kaputte Datei abgelehnt wird.
struct ElfInfo;

impl Befehl for ElfInfo {
    fn name(&self) -> &'static str {
        "elfinfo"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt die Segmente einer Programmdatei (elfinfo <pfad|name>)"
    }
    fn ausfuehren(
        &self,
        argumente: &str,
        kontext: &mut ShellKontext,
        _registry: &[Box<dyn Befehl>],
    ) {
        let wunsch = argumente.trim();
        if wunsch.is_empty() {
            println!("Benutzung: elfinfo <pfad|name>");
            return;
        }
        let pfad = pfad_fuer_programm(kontext, wunsch);
        let bytes = match fs::mit_fs(|dateisystem| dateisystem.lesen(&pfad)) {
            Ok(bytes) => bytes,
            Err(fehler) => {
                println!("{}:", pfad);
                fs_fehler_ausgeben(fehler);
                return;
            }
        };

        println!("{} ({} Byte)", pfad, bytes.len());
        match crate::elf::pruefen(&bytes) {
            Ok(programm) => {
                konsole::set_color(Color::LightGreen, Color::Black);
                println!("Gueltiges SpeedOS-Programm (ET_EXEC, x86-64, statisch gelinkt).");
                konsole::set_color(Color::LightGray, Color::Black);
                println!("Einsprung: {:#x}", programm.einsprung);
                println!();
                konsole::set_color(Color::LightCyan, Color::Black);
                println!(
                    "{:<8} {:>14} {:>10} {:>10} {:>8}",
                    "Rechte", "Adresse", "Datei", "Speicher", "Seiten"
                );
                konsole::set_color(Color::LightGray, Color::Black);
                for segment in &programm.segmente {
                    let rechte = alloc::format!(
                        "r{}{}",
                        if segment.rechte.schreiben { "w" } else { "-" },
                        if segment.rechte.ausfuehren { "x" } else { "-" }
                    );
                    println!(
                        "{:<8} {:>14x} {:>10} {:>10} {:>8}",
                        rechte,
                        segment.virt_adresse,
                        segment.datei_bytes,
                        segment.speicher_bytes,
                        (segment.seite_dahinter() - segment.erste_seite()) / 4096
                    );
                }
                let bss: u64 = programm.segmente.iter().map(|s| s.bss_bytes()).sum();
                println!();
                println!(
                    "{} Segment(e), {} Seiten, davon {} Byte .bss (genullt, nicht in der Datei).",
                    programm.segmente.len(),
                    programm.seiten(),
                    bss
                );
            }
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("KEIN ladbares Programm: {}", fehler.meldung());
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Serie 7, Teil 1: der Zufallsgenerator
// ---------------------------------------------------------------------------

/// zufall [bytes] — zeigt Zufallsbytes als Hex UND den Zustand des
/// Entropie-Pools.
///
/// Der Status ist hier nicht Beiwerk, sondern der eigentliche Punkt: Wenn
/// `zufall` nichts liefert, soll der Nutzer SEHEN, warum — wie viele Bit
/// fehlen, welche Quellen ueberhaupt Proben liefern, ob es eine
/// Hardware-Quelle gibt. Ein Generator, der nur „geht nicht" sagt, waere
/// nicht nachvollziehbar (docs/zufall.md §4).
struct ZufallBefehl;

impl Befehl for ZufallBefehl {
    fn name(&self) -> &'static str {
        "zufall"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt Zufallsbytes (Hex) und den Zustand des Entropie-Pools"
    }
    fn ausfuehren(&self, argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::zufall::{self, Quelle};

        let anzahl: usize = match argumente.trim() {
            "" => 32,
            text => match text.parse::<usize>() {
                // 1 KiB ist genug zum Ansehen; wer mehr will, nimmt den
                // Syscall. Die Shell soll das Terminal nicht zumuellen.
                Ok(n) if n > 0 && n <= 1024 => n,
                _ => {
                    konsole::set_color(Color::LightRed, Color::Black);
                    println!("Benutzung: zufall [1..1024]");
                    konsole::set_color(Color::LightGray, Color::Black);
                    return;
                }
            },
        };

        let s = zufall::status();

        // --- Der Pool-Status, immer zuerst ---
        konsole::set_color(Color::LightCyan, Color::Black);
        println!("Entropie-Pool:");
        konsole::set_color(Color::LightGray, Color::Black);
        print!("  Zustand:   ");
        if s.gesaet {
            konsole::set_color(Color::LightGreen, Color::Black);
            println!("GESAET (einsatzbereit)");
        } else {
            konsole::set_color(Color::Yellow, Color::Black);
            println!("NICHT GESAET — es gibt keine Bytes, siehe unten");
        }
        konsole::set_color(Color::LightGray, Color::Black);
        println!(
            "  Entropie:  {} von {} Bit (geschaetzt, bewusst untertrieben)",
            s.entropie_bits, s.schwelle_bits
        );
        print!("  Hardware:  RDSEED {}", if s.rdseed { "ja" } else { "nein" });
        print!(", RDRAND {}", if s.rdrand { "ja" } else { "nein" });
        if s.hardware_defekt {
            konsole::set_color(Color::LightRed, Color::Black);
            print!("  [Gesundheitspruefung fehlgeschlagen — abgeschaltet]");
            konsole::set_color(Color::LightGray, Color::Black);
        }
        println!();
        println!("  Nachsaat:  {}x, ausgegeben: {} Byte", s.nachsaaten, s.ausgegebene_bytes);

        println!("  Quellen (Proben):");
        for quelle in Quelle::alle() {
            let proben = s.proben[quelle.index()];
            let aktiv = proben > 0;
            if aktiv {
                konsole::set_color(Color::LightGray, Color::Black);
            } else {
                konsole::set_color(Color::DarkGray, Color::Black);
            }
            println!(
                "    {:<16} {:>8}   {}",
                quelle.name(),
                proben,
                match quelle.bits_je_probe() {
                    // Salz wird ausdruecklich als solches ausgewiesen — wer
                    // die Zahl sieht, soll nicht denken, sie zaehle mit.
                    0 => alloc::string::String::from("0 Bit/Probe (SALZ, keine Entropie)"),
                    bits => alloc::format!("{} Bit/Probe", bits),
                }
            );
        }
        konsole::set_color(Color::LightGray, Color::Black);

        // --- Die Bytes ---
        println!();
        let mut puffer = alloc::vec![0u8; anzahl];
        match zufall::fuellen(&mut puffer) {
            Ok(()) => {
                println!("{} Zufallsbytes:", anzahl);
                for (i, byte) in puffer.iter().enumerate() {
                    if i % 16 == 0 {
                        if i > 0 {
                            println!();
                        }
                        konsole::set_color(Color::DarkGray, Color::Black);
                        print!("  {:04x}  ", i);
                        konsole::set_color(Color::LightGray, Color::Black);
                    }
                    print!("{:02x} ", byte);
                }
                println!();
            }
            Err(fehler) => {
                konsole::set_color(Color::LightRed, Color::Black);
                println!("Keine Bytes: {}", fehler.meldung());
                konsole::set_color(Color::LightGray, Color::Black);
                println!(
                    "Es fehlen {} Bit. SpeedOS liefert in diesem Zustand KEINEN",
                    s.schwelle_bits.saturating_sub(s.entropie_bits)
                );
                println!(
                    "schwachen Zufall — lieber warten als etwas ausgeben, das"
                );
                println!("wie Zufall aussieht und keiner ist (docs/zufall.md §4).");
                println!("Tipp: Tasten druecken oder die Maus bewegen fuellt den Pool.");
            }
        }
    }
}

// ===========================================================================
// schrift — WAS DER FONT-BESTAND WIRKLICH HERGIBT (Serie 8, Teil 3)
// ===========================================================================

/// schrift — zeigt die Schriftgroessen und die Rollen-Abbildung.
///
/// WOZU EIN BEFEHL DAFUER: Die Abbildung „h1..h6/p/small -> Pixelhoehe"
/// ist die Grundlage jedes Renderer-Layouts, und sie haengt an zwei
/// Dingen, die sich AENDERN — den eingebundenen Rastern (Cargo-Features)
/// und der UI-Skalierung (zur Laufzeit umschaltbar). Eine Tabelle in der
/// Doku waere ab der ersten Aenderung eine Behauptung; dieser Befehl
/// fragt den WIRT.
///
/// Die Spalte „exakt" ist die wichtigste: Sie zeigt, wo gerundet werden
/// MUSSTE — und damit genau die Luecke, die docs/schrift-groessen.md
/// beschreibt.
struct Schriftprobe;

impl Befehl for Schriftprobe {
    fn name(&self) -> &'static str {
        "schrift"
    }
    fn beschreibung(&self) -> &'static str {
        "Zeigt Schriftgroessen, Rollen-Abbildung (h1..small) und Fett/Kursiv"
    }
    fn ausfuehren(&self, _argumente: &str, _kontext: &mut ShellKontext, _registry: &[Box<dyn Befehl>]) {
        use crate::ui::wirt::KernelSchrift;
        use speedui::text::{self, Rolle};
        use speedui::Schrift;

        let schrift = KernelSchrift;
        let basis = crate::theme::metrik().schrift_ui as i32;

        konsole::set_color(Color::Yellow, Color::Black);
        println!("Schrift-Bestand");
        konsole::set_color(Color::LightGray, Color::Black);

        print!("  Vorgerasterte Groessen:");
        for g in schrift.groessen() {
            print!(" {}", g);
        }
        println!(" (Pixel)");
        println!("  Fliesstext-Groesse:    {} px", basis);
        println!(
            "  Fett:   {}",
            if schrift.fett_echt() {
                "ECHT (eigener Schnitt FontWeight::Bold)"
            } else {
                "simuliert"
            }
        );
        println!(
            "  Kursiv: {}",
            if schrift.kursiv_echt() {
                "ECHT (eigener Schnitt)"
            } else {
                "SIMULIERT (Scherung um ~14 Grad, keine Kursiv-Formen)"
            }
        );
        println!();

        konsole::set_color(Color::Yellow, Color::Black);
        println!("  Rolle   Wunsch  ->  bekommt   exakt  fett");
        konsole::set_color(Color::LightGray, Color::Black);
        for rolle in Rolle::ALLE {
            let wunsch = text::wunschgroesse(rolle, basis);
            let echt = text::groesse_fuer(rolle, basis, &schrift);
            let exakt = text::exakt_moeglich(rolle, basis, &schrift);
            if !exakt {
                konsole::set_color(Color::LightRed, Color::Black);
            }
            println!(
                "  {:<6}  {:>4}    ->  {:>4}      {:<5}  {}",
                rolle.name(),
                wunsch,
                echt,
                if exakt { "ja" } else { "NEIN" },
                if rolle.fett() { "ja" } else { "-" }
            );
            if !exakt {
                konsole::set_color(Color::LightGray, Color::Black);
            }
        }
        println!();
        // ASCII-Bindestrich statt Gedankenstrich: Die FramebufferKonsole
        // ist Latin-1, ein '—' wird dort zu '?' (docs/usb-boot.md).
        println!("  Rot = musste gerundet werden. Unter der Fliesstextgroesse");
        println!("  gibt es NICHTS - small/h5/h6 koennen nicht kleiner werden.");
        println!("  Begruendung und Ausweg: docs/schrift-groessen.md");
    }
}
