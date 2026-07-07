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

use crate::fs::{self, DirEintrag, NodeTyp};
use crate::task::keyboard::KeyStream;
use crate::vga_buffer::{self, Color};
use crate::{print, println};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use befehle::{Befehl, ShellKontext};
use futures_util::StreamExt;
use pc_keyboard::{DecodedKey, KeyCode};

/// Wie viele Befehle sich der Verlauf merkt (Pfeil hoch/runter).
const MAX_VERLAUF: usize = 10;

/// Der Shell-Task: läuft "ewig" im Executor.
pub async fn run() {
    banner();
    // Blinkenden Hardware-Cursor einschalten — ab jetzt sieht man,
    // wo die nächste Eingabe landet.
    vga_buffer::cursor_aktivieren();

    let registry = befehle::alle_befehle();
    let mut keys = KeyStream::new();
    // Der Shell-Zustand: aktuelles Verzeichnis (Befehle ändern ihn).
    let mut kontext = ShellKontext::neu();
    // Die aktuelle Eingabezeile (das, was der Nutzer gerade tippt).
    let mut zeile = String::new();
    // Der Befehlsverlauf: vorne = neuester Befehl.
    let mut verlauf: VecDeque<String> = VecDeque::new();
    // Wo wir gerade im Verlauf blättern (None = nicht am Blättern).
    let mut verlauf_index: Option<usize> = None;

    prompt(&kontext);
    while let Some(key) = keys.next().await {
        match key {
            // Enter: Zeile abschließen und Befehl ausführen.
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                println!();
                let eingabe = zeile.trim();
                if !eingabe.is_empty() {
                    // In den Verlauf (aber nicht doppelt hintereinander):
                    if verlauf.front().map(|s| s.as_str()) != Some(eingabe) {
                        verlauf.push_front(String::from(eingabe));
                        verlauf.truncate(MAX_VERLAUF);
                    }
                    befehl_ausfuehren(&registry, &mut kontext, eingabe);
                }
                zeile.clear();
                verlauf_index = None;
                prompt(&kontext);
            }
            // Tab: Datei-/Ordnernamen vervollständigen.
            DecodedKey::Unicode('\t') => {
                tab_vervollstaendigen(&mut zeile, &kontext);
            }
            // Backspace oder Entf: letztes Zeichen der Eingabe löschen.
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                if zeile.pop().is_some() {
                    print!("\u{8} \u{8}");
                }
            }
            DecodedKey::RawKey(KeyCode::Delete) => {
                if zeile.pop().is_some() {
                    print!("\u{8} \u{8}");
                }
            }
            // Pfeil hoch: einen Schritt zurück im Verlauf.
            DecodedKey::RawKey(KeyCode::ArrowUp) => {
                let neuer_index = match verlauf_index {
                    None if !verlauf.is_empty() => Some(0),
                    Some(i) if i + 1 < verlauf.len() => Some(i + 1),
                    unveraendert => unveraendert,
                };
                if neuer_index != verlauf_index {
                    if let Some(i) = neuer_index {
                        let eintrag = verlauf[i].clone();
                        zeile_ersetzen(&mut zeile, &eintrag);
                        verlauf_index = neuer_index;
                    }
                }
            }
            // Pfeil runter: wieder Richtung Gegenwart blättern.
            DecodedKey::RawKey(KeyCode::ArrowDown) => match verlauf_index {
                Some(0) => {
                    // Unten angekommen: leere Eingabezeile.
                    zeile_ersetzen(&mut zeile, "");
                    verlauf_index = None;
                }
                Some(i) => {
                    let eintrag = verlauf[i - 1].clone();
                    zeile_ersetzen(&mut zeile, &eintrag);
                    verlauf_index = Some(i - 1);
                }
                None => {}
            },
            // Normales druckbares Zeichen: anhängen und anzeigen.
            DecodedKey::Unicode(c) if c >= ' ' => {
                zeile.push(c);
                print!("{}", c);
            }
            // Alles andere (F-Tasten, Pfeile links/rechts, ...): ignorieren.
            _ => {}
        }
    }
}

/// Führt eine Eingabezeile aus: erstes Wort = Befehlsname, Rest =
/// Argumente. Öffentlich, damit die Tests sie direkt aufrufen können.
pub fn befehl_ausfuehren(registry: &[Box<dyn Befehl>], kontext: &mut ShellKontext, eingabe: &str) {
    let (name, argumente) = eingabe.split_once(' ').unwrap_or((eingabe, ""));

    match registry.iter().find(|b| b.name() == name) {
        Some(befehl) => befehl.ausfuehren(argumente.trim(), kontext, registry),
        None => {
            vga_buffer::set_color(Color::LightRed, Color::Black);
            println!("Unbekannter Befehl: '{}'", name);
            vga_buffer::set_color(Color::LightGray, Color::Black);
            println!("Tippe 'help', um alle verfuegbaren Befehle zu sehen.");
        }
    }
}

/// Gibt den Eingabe-Prompt aus — mit dem aktuellen Verzeichnis,
/// wie man es von cmd kennt (C:\> ... bei uns: SpeedOS:/system>).
fn prompt(kontext: &ShellKontext) {
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    print!("SpeedOS:{}> ", kontext.aktuelles_verzeichnis);
    vga_buffer::set_color(Color::LightGray, Color::Black);
}

/// Tab-Vervollständigung für Datei- und Ordnernamen.
///
/// Nimmt das letzte Wort der Eingabezeile, zerlegt es in Verzeichnis-
/// Teil und Namensanfang, und schaut im VFS nach passenden Einträgen:
///   - genau 1 Treffer: wird vervollständigt (Ordner bekommen ein /)
///   - mehrere: erst den gemeinsamen Anfang ergänzen; geht das nicht,
///     alle Kandidaten anzeigen und die Eingabezeile neu zeichnen
fn tab_vervollstaendigen(zeile: &mut String, kontext: &ShellKontext) {
    // Das letzte Wort der Zeile ist das zu vervollständigende Token.
    let token_start = zeile.rfind(' ').map(|i| i + 1).unwrap_or(0);
    let token = String::from(&zeile[token_start..]);

    // Token = optionaler Verzeichnis-Teil + Namensanfang.
    // "system/in" -> Verzeichnis "system/", Anfang "in"
    let (verz_teil, anfang) = match token.rfind('/') {
        Some(i) => (&token[..=i], &token[i + 1..]),
        None => ("", &token[..]),
    };
    let verz_pfad = if verz_teil.is_empty() {
        kontext.aktuelles_verzeichnis.clone()
    } else {
        kontext.aufloesen(verz_teil)
    };

    let eintraege = match fs::mit_fs(|f| f.liste(&verz_pfad)) {
        Ok(e) => e,
        Err(_) => return, // Kein gültiges Verzeichnis: nichts tun.
    };
    let passende: Vec<&DirEintrag> = eintraege
        .iter()
        .filter(|e| e.name.starts_with(anfang))
        .collect();

    match passende.len() {
        0 => {} // Nichts passt: nichts tun (wie in cmd).
        1 => {
            // Eindeutig: den Rest des Namens ergänzen.
            let eintrag = passende[0];
            let mut rest = String::from(&eintrag.name[anfang.len()..]);
            if eintrag.typ == NodeTyp::Verzeichnis {
                rest.push('/'); // Ordner: gleich weitertippen können
            }
            print!("{}", rest);
            zeile.push_str(&rest);
        }
        _ => {
            // Mehrdeutig: so weit ergänzen, wie alle übereinstimmen.
            let gemeinsam = gemeinsamer_anfang(&passende);
            if gemeinsam.chars().count() > anfang.chars().count() {
                let rest = String::from(&gemeinsam[anfang.len()..]);
                print!("{}", rest);
                zeile.push_str(&rest);
            } else {
                // Alle Kandidaten zeigen, dann Prompt + Zeile neu.
                println!();
                for eintrag in &passende {
                    if eintrag.typ == NodeTyp::Verzeichnis {
                        vga_buffer::set_color(Color::LightCyan, Color::Black);
                        print!("{}/  ", eintrag.name);
                        vga_buffer::set_color(Color::LightGray, Color::Black);
                    } else {
                        print!("{}  ", eintrag.name);
                    }
                }
                println!();
                prompt(kontext);
                print!("{}", zeile);
            }
        }
    }
}

/// Der längste gemeinsame Namensanfang aller Kandidaten
/// (zeichenweise verglichen, damit Umlaute nicht zerteilt werden).
fn gemeinsamer_anfang(eintraege: &[&DirEintrag]) -> String {
    let erster = &eintraege[0].name;
    let mut gemeinsam = String::new();
    'aussen: for (i, zeichen) in erster.char_indices() {
        for eintrag in eintraege.iter().skip(1) {
            if !eintrag.name[i..].starts_with(zeichen) {
                break 'aussen;
            }
        }
        gemeinsam.push(zeichen);
    }
    gemeinsam
}

/// Ersetzt die sichtbare Eingabezeile durch `neu` (fürs Blättern im
/// Verlauf): alte Zeichen rückwärts wegradieren, neuen Text tippen.
fn zeile_ersetzen(zeile: &mut String, neu: &str) {
    for _ in 0..zeile.chars().count() {
        print!("\u{8} \u{8}");
    }
    zeile.clear();
    zeile.push_str(neu);
    print!("{}", neu);
}

/// Das farbige SpeedOS-Banner beim Start der Shell.
fn banner() {
    vga_buffer::clear_screen();
    vga_buffer::set_color(Color::Yellow, Color::Blue);
    println!("{:<60}", "");
    println!("{:<60}", "    ____                      _  ___  ____");
    println!("{:<60}", "   / ___| _ __   ___  ___  __| |/ _ \\/ ___|");
    println!("{:<60}", "   \\___ \\| '_ \\ / _ \\/ _ \\/ _` | | | \\___ \\");
    println!("{:<60}", "    ___) | |_) |  __/  __/ (_| | |_| |___) |");
    println!("{:<60}", "   |____/| .__/ \\___|\\___|\\__,_|\\___/|____/");
    println!("{:<60}", "         |_|      v0.1  -  ein OS in Rust");
    println!("{:<60}", "");
    vga_buffer::set_color(Color::LightCyan, Color::Black);
    println!();
    println!("Willkommen in der SpeedShell!");
    println!("Tippe 'help' fuer alle Befehle. Pfeil hoch/runter = Verlauf.");
    println!();
    vga_buffer::set_color(Color::LightGray, Color::Black);
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
