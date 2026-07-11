// shell/mod.rs — Die SpeedShell: die interaktive Kommandozeile von SpeedOS
//
// Die Shell ist ein ganz normaler async Task im Executor. Sie liest
// dekodierte Tasten aus dem KeyStream (task/keyboard.rs), baut daraus
// eine Eingabezeile (mit Backspace und Befehlsverlauf) und führt beim
// Drücken von Enter den passenden Befehl aus.
//
// Die Befehle selbst leben in shell/befehle.rs hinter dem Befehl-Trait —
// neue Befehle (dir, cd, ... sobald es ein Dateisystem gibt) brauchen
// nur eine neue Struct + einen Eintrag in alle_befehle().

pub mod befehle;
pub mod editor;

use crate::fs::{self, NodeTyp};
use crate::task::keyboard::KeyStream;
use crate::konsole::{self, Color};
use crate::{print, println};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use befehle::{Befehl, ShellKontext};
use editor::{Reaktion, Taste, Vervollstaendiger, ZeilenEditor};
use futures_util::StreamExt;
use pc_keyboard::{DecodedKey, KeyCode};

/// Wie viele Befehle sich der Verlauf merkt (Pfeil hoch/runter).
const MAX_VERLAUF: usize = 10;

/// Der Shell-Task: läuft "ewig" im Executor.
///
/// Die Arbeitsteilung nach dem Editor-Refactoring:
///   1. Tastatur-Event in ein abstraktes Taste-Event übersetzen,
///   2. dem ZeilenEditor geben (der macht die ganze Eingabelogik),
///   3. seine Reaktion auf den Bildschirm bringen,
///   4. fertige Zeilen an die Befehls-Registry weiterreichen.
pub async fn run() {
    banner();
    // Blinkenden Hardware-Cursor einschalten — ab jetzt sieht man,
    // wo die nächste Eingabe landet.
    konsole::cursor_aktivieren();

    let registry = befehle::alle_befehle();
    let mut keys = KeyStream::new();
    // Der Shell-Zustand: aktuelles Verzeichnis (Befehle ändern ihn).
    let mut kontext = ShellKontext::neu();
    // Der Editor übernimmt Eingabezeile, Verlauf und Tab-Logik.
    let mut editor = ZeilenEditor::neu(MAX_VERLAUF);
    let vervollstaendiger = FsVervollstaendiger;

    prompt(&kontext);
    while let Some(key) = keys.next().await {
        // Desktop-Modus? ESC schließt erst das Startmenü, dann den
        // Desktop; Tasten gehen ins offene Startmenü oder ans
        // fokussierte Fenster (Event-Routing im FensterManager).
        if crate::fenster::desktop_aktiv() {
            if key == DecodedKey::Unicode('\u{1b}') {
                if crate::fenster::startmenue_offen() {
                    crate::fenster::startmenue_schliessen();
                } else {
                    crate::fenster::desktop_beenden();
                    konsole::cursor_aktivieren();
                    konsole::clear_screen();
                    prompt(&kontext);
                }
            } else if crate::fenster::startmenue_offen() {
                crate::fenster::startmenue_taste(key);
            } else {
                crate::fenster::taste_event(key);
            }
            continue;
        }

        // Grafik-Demo aktiv? Dann beendet JEDE Taste sie und wir
        // kehren zur Konsole zurück (Taste wird verschluckt).
        if crate::grafik::demo_aktiv() {
            crate::grafik::demo_beenden();
            konsole::cursor_aktivieren();
            konsole::clear_screen();
            prompt(&kontext);
            continue;
        }

        // Schritt 1: pc_keyboard-Event -> abstraktes Taste-Event.
        let taste = match key {
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => Taste::Enter,
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => Taste::Backspace,
            DecodedKey::RawKey(KeyCode::Delete) => Taste::Backspace,
            DecodedKey::Unicode('\t') => Taste::Tab,
            DecodedKey::RawKey(KeyCode::ArrowUp) => Taste::HochPfeil,
            DecodedKey::RawKey(KeyCode::ArrowDown) => Taste::RunterPfeil,
            DecodedKey::Unicode(c) if c >= ' ' => Taste::Zeichen(c),
            // Alles andere (F-Tasten, Pfeile links/rechts, ...): ignorieren.
            _ => continue,
        };

        // Schritt 2 + 3: Editor fragen, Reaktion anzeigen.
        match editor.taste(taste, &kontext.aktuelles_verzeichnis, &vervollstaendiger) {
            Reaktion::Keine => {}
            Reaktion::Anhaengen(text) => print!("{}", text),
            Reaktion::Loeschen(anzahl) => {
                for _ in 0..anzahl {
                    print!("\u{8} \u{8}");
                }
            }
            Reaktion::Ersetzen { geloescht, neu } => {
                for _ in 0..geloescht {
                    print!("\u{8} \u{8}");
                }
                print!("{}", neu);
            }
            // Schritt 4: fertige Zeile ausführen.
            Reaktion::Fertig(eingabe) => {
                println!();
                if !eingabe.is_empty() {
                    befehl_ausfuehren(&registry, &mut kontext, &eingabe);
                }
                // Kein Prompt, wenn gerade Grafik-Demo oder Desktop
                // laufen — der würde mitten ins Bild malen.
                if !crate::grafik::demo_aktiv() && !crate::fenster::desktop_aktiv() {
                    prompt(&kontext);
                }
            }
            Reaktion::KandidatenZeigen(kandidaten) => {
                println!();
                for kandidat in &kandidaten {
                    print!("{}  ", kandidat);
                }
                println!();
                prompt(&kontext);
                print!("{}", editor.zeile());
            }
        }
    }
}

/// Die VFS-Anbindung der Tab-Vervollständigung: Der Editor kennt nur
/// das Trait, die Shell liefert die echten Verzeichnis-Einträge.
struct FsVervollstaendiger;

impl Vervollstaendiger for FsVervollstaendiger {
    fn eintraege(&self, pfad: &str) -> Vec<(String, bool)> {
        fs::mit_fs(|f| f.liste(pfad))
            .map(|eintraege| {
                eintraege
                    .into_iter()
                    .map(|e| (e.name, e.typ == NodeTyp::Verzeichnis))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Führt eine Eingabezeile aus: erstes Wort = Befehlsname, Rest =
/// Argumente. Öffentlich, damit die Tests sie direkt aufrufen können.
pub fn befehl_ausfuehren(registry: &[Box<dyn Befehl>], kontext: &mut ShellKontext, eingabe: &str) {
    let (name, argumente) = eingabe.split_once(' ').unwrap_or((eingabe, ""));

    match registry.iter().find(|b| b.name() == name) {
        Some(befehl) => befehl.ausfuehren(argumente.trim(), kontext, registry),
        None => {
            konsole::set_color(Color::LightRed, Color::Black);
            println!("Unbekannter Befehl: '{}'", name);
            konsole::set_color(Color::LightGray, Color::Black);
            println!("Tippe 'help', um alle verfuegbaren Befehle zu sehen.");
        }
    }
}

/// Gibt den Eingabe-Prompt aus — mit dem aktuellen Verzeichnis,
/// wie man es von cmd kennt (C:\> ... bei uns: SpeedOS:/system>).
fn prompt(kontext: &ShellKontext) {
    konsole::set_color(Color::LightGreen, Color::Black);
    print!("SpeedOS:{}> ", kontext.aktuelles_verzeichnis);
    konsole::set_color(Color::LightGray, Color::Black);
}

/// Das farbige SpeedOS-Banner beim Start der Shell.
fn banner() {
    konsole::clear_screen();
    konsole::set_color(Color::Yellow, Color::Blue);
    println!("{:<60}", "");
    println!("{:<60}", "    ____                      _  ___  ____");
    println!("{:<60}", "   / ___| _ __   ___  ___  __| |/ _ \\/ ___|");
    println!("{:<60}", "   \\___ \\| '_ \\ / _ \\/ _ \\/ _` | | | \\___ \\");
    println!("{:<60}", "    ___) | |_) |  __/  __/ (_| | |_| |___) |");
    println!("{:<60}", "   |____/| .__/ \\___|\\___|\\__,_|\\___/|____/");
    println!("{:<60}", "         |_|      v0.1  -  ein OS in Rust");
    println!("{:<60}", "");
    konsole::set_color(Color::LightCyan, Color::Black);
    println!();
    println!("Willkommen in der SpeedShell!");
    println!("Tippe 'help' fuer alle Befehle. Pfeil hoch/runter = Verlauf.");
    println!();
    konsole::set_color(Color::LightGray, Color::Black);
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Jeder registrierte Befehl muss ohne Panic ausführbar sein
    /// (mit leeren Argumenten). "neustart" lassen wir aus — der würde
    /// QEMU mitten im Test neu booten!
    #[test_case]
    fn test_alle_befehle_ohne_panic() {
        let registry = befehle::alle_befehle();
        let mut kontext = ShellKontext::neu();
        for befehl in registry.iter() {
            if befehl.name() == "neustart" {
                continue;
            }
            befehl.ausfuehren("", &mut kontext, &registry);
        }
        // grafiktest hat den Demo-Modus gesetzt (und dabei alle
        // Primitive wirklich gezeichnet — guter Test!): aufräumen.
        crate::grafik::demo_beenden();
        crate::konsole::cursor_aktivieren();
    }

    /// Ein unbekannter Befehl gibt die Fehlermeldung aus, ohne
    /// abzustürzen; echo mit Argumenten funktioniert.
    #[test_case]
    fn test_dispatcher() {
        let registry = befehle::alle_befehle();
        let mut kontext = ShellKontext::neu();
        befehl_ausfuehren(&registry, &mut kontext, "gibtsnicht");
        befehl_ausfuehren(&registry, &mut kontext, "echo Hallo aus dem Test");
    }

    /// Die Dateisystem-Befehle über den Dispatcher, wie sie ein
    /// Nutzer tippen würde — inklusive cd mit relativen Pfaden.
    #[test_case]
    fn test_dateisystem_befehle() {
        let registry = befehle::alle_befehle();
        let mut kontext = ShellKontext::neu();

        befehl_ausfuehren(&registry, &mut kontext, "mkdir shelltest");
        befehl_ausfuehren(&registry, &mut kontext, "cd shelltest");
        assert_eq!(kontext.aktuelles_verzeichnis, "/shelltest");

        befehl_ausfuehren(&registry, &mut kontext, "write notiz.txt Hallo Welt");
        assert_eq!(
            fs::mit_fs(|f| f.lesen("/shelltest/notiz.txt")).unwrap(),
            b"Hallo Welt\n"
        );

        befehl_ausfuehren(&registry, &mut kontext, "copy notiz.txt kopie.txt");
        assert!(fs::mit_fs(|f| f.lesen("/shelltest/kopie.txt")).is_ok());

        befehl_ausfuehren(&registry, &mut kontext, "cd ..");
        assert_eq!(kontext.aktuelles_verzeichnis, "/");

        // cd in eine Datei muss scheitern (cwd bleibt unverändert).
        befehl_ausfuehren(&registry, &mut kontext, "cd shelltest/notiz.txt");
        assert_eq!(kontext.aktuelles_verzeichnis, "/");

        // Aufräumen.
        befehl_ausfuehren(&registry, &mut kontext, "del shelltest/notiz.txt");
        befehl_ausfuehren(&registry, &mut kontext, "del shelltest/kopie.txt");
        befehl_ausfuehren(&registry, &mut kontext, "del shelltest");
        assert_eq!(
            fs::mit_fs(|f| f.node_typ("/shelltest")),
            Err(fs::FsFehler::NichtGefunden)
        );
    }
}
