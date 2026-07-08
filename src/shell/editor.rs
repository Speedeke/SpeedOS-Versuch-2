// shell/editor.rs — Der ZeilenEditor: die Eingabelogik der Shell,
//                   sauber getrennt von Tastatur und Bildschirm
//
// Warum die Trennung? Vorher steckte alles in einem großen match in
// shell::run() — untestbar, weil untrennbar mit VGA-Ausgabe und
// Key-Stream verwoben. Jetzt gilt:
//
//   Tastatur --Taste--> ZeilenEditor --Reaktion--> Shell zeichnet
//
// Der Editor bekommt abstrakte Tasten (eigenes Taste-Enum, kein
// pc_keyboard-Typ) und gibt zurück, WAS anzuzeigen wäre (Reaktion) —
// er selbst gibt NIE etwas aus. Die Tab-Vervollständigung fragt ein
// Vervollstaendiger-Trait statt direkt das VFS. Ergebnis: Die ganze
// Eingabelogik (Tippen, Backspace, Verlauf, Tab) ist mit einem
// Mock-Vervollständiger als reiner Unit-Test prüfbar — ganz ohne
// Tastatur, Bildschirm oder Dateisystem.
//
// Der Cursor steht in dieser Version immer am Zeilenende (Einfügen
// mitten in der Zeile kommt, wenn wir Pfeil links/rechts brauchen).

use crate::fs;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Die Tasten, die der Editor versteht — bewusst ein eigenes Enum,
/// damit Tests keine pc_keyboard-Typen bauen müssen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taste {
    Zeichen(char),
    Enter,
    Backspace,
    HochPfeil,
    RunterPfeil,
    Tab,
}

/// Was die Shell nach einem Tastendruck anzeigen soll.
/// Der Editor beschreibt die Anzeige nur — gezeichnet wird woanders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaktion {
    /// Nichts zu tun.
    Keine,
    /// Diesen Text ans Zeilenende anhängen (Tippen, Tab-Ergänzung).
    Anhaengen(String),
    /// So viele Zeichen rückwärts löschen (Backspace).
    Loeschen(usize),
    /// `geloescht` Zeichen wegradieren, dann `neu` anzeigen (Verlauf).
    Ersetzen { geloescht: usize, neu: String },
    /// Zeile abgeschlossen (Enter): Inhalt ausführen, neue Zeile beginnen.
    Fertig(String),
    /// Tab war mehrdeutig: Kandidaten anzeigen, dann Prompt + Zeile
    /// neu zeichnen (editor.zeile() liefert den aktuellen Inhalt).
    KandidatenZeigen(Vec<String>),
}

/// Liefert dem Editor die Verzeichnis-Einträge für die Tab-Taste.
/// Die Shell implementiert das über das VFS, Tests über einen Mock.
pub trait Vervollstaendiger {
    /// Einträge im Verzeichnis `pfad`: (Name, ist_verzeichnis).
    fn eintraege(&self, pfad: &str) -> Vec<(String, bool)>;
}

/// Der Editor-Zustand: Eingabezeile + Befehlsverlauf.
pub struct ZeilenEditor {
    /// Die aktuelle Eingabezeile (Cursor steht am Ende).
    zeile: String,
    /// Der Befehlsverlauf: vorne = neuester Befehl.
    verlauf: VecDeque<String>,
    /// Wo wir gerade im Verlauf blättern (None = nicht am Blättern).
    verlauf_index: Option<usize>,
    /// Wie viele Einträge sich der Verlauf merkt.
    max_verlauf: usize,
}

impl ZeilenEditor {
    pub fn neu(max_verlauf: usize) -> Self {
        ZeilenEditor {
            zeile: String::new(),
            verlauf: VecDeque::new(),
            verlauf_index: None,
            max_verlauf,
        }
    }

    /// Der aktuelle Zeileninhalt (z. B. zum Neuzeichnen nach Tab).
    pub fn zeile(&self) -> &str {
        &self.zeile
    }

    /// Verarbeitet einen Tastendruck und sagt, was anzuzeigen ist.
    /// `aktuelles_verzeichnis` braucht nur die Tab-Vervollständigung
    /// (für relative Pfade).
    pub fn taste(
        &mut self,
        taste: Taste,
        aktuelles_verzeichnis: &str,
        vervollstaendiger: &impl Vervollstaendiger,
    ) -> Reaktion {
        match taste {
            Taste::Zeichen(c) => {
                self.zeile.push(c);
                Reaktion::Anhaengen(String::from(c))
            }
            Taste::Backspace => {
                if self.zeile.pop().is_some() {
                    Reaktion::Loeschen(1)
                } else {
                    Reaktion::Keine
                }
            }
            Taste::Enter => {
                let eingabe = String::from(self.zeile.trim());
                // In den Verlauf (aber nicht doppelt hintereinander):
                if !eingabe.is_empty() && self.verlauf.front() != Some(&eingabe) {
                    self.verlauf.push_front(eingabe.clone());
                    self.verlauf.truncate(self.max_verlauf);
                }
                self.zeile.clear();
                self.verlauf_index = None;
                Reaktion::Fertig(eingabe)
            }
            Taste::HochPfeil => {
                // Einen Schritt weiter in die Vergangenheit:
                let neuer_index = match self.verlauf_index {
                    None if !self.verlauf.is_empty() => Some(0),
                    Some(i) if i + 1 < self.verlauf.len() => Some(i + 1),
                    unveraendert => unveraendert,
                };
                if neuer_index != self.verlauf_index {
                    if let Some(i) = neuer_index {
                        self.verlauf_index = neuer_index;
                        return self.zeile_ersetzen_mit(self.verlauf[i].clone());
                    }
                }
                Reaktion::Keine
            }
            Taste::RunterPfeil => match self.verlauf_index {
                // Unten angekommen: zurück zur leeren Eingabe.
                Some(0) => {
                    self.verlauf_index = None;
                    self.zeile_ersetzen_mit(String::new())
                }
                Some(i) => {
                    self.verlauf_index = Some(i - 1);
                    self.zeile_ersetzen_mit(self.verlauf[i - 1].clone())
                }
                None => Reaktion::Keine,
            },
            Taste::Tab => self.tab(aktuelles_verzeichnis, vervollstaendiger),
        }
    }

    /// Tauscht die Zeile aus und beschreibt die nötige Anzeige-Änderung.
    fn zeile_ersetzen_mit(&mut self, neu: String) -> Reaktion {
        let geloescht = self.zeile.chars().count();
        self.zeile = neu.clone();
        Reaktion::Ersetzen { geloescht, neu }
    }

    /// Tab-Vervollständigung fürs letzte Wort der Zeile.
    fn tab(&mut self, cwd: &str, vervollstaendiger: &impl Vervollstaendiger) -> Reaktion {
        // Das letzte Wort ist das zu vervollständigende Token,
        // zerlegt in Verzeichnis-Teil und Namensanfang:
        // "system/in" -> Verzeichnis "system/", Anfang "in"
        let token_start = self.zeile.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let token = String::from(&self.zeile[token_start..]);
        let (verz_teil, anfang) = match token.rfind('/') {
            Some(i) => (&token[..=i], &token[i + 1..]),
            None => ("", &token[..]),
        };
        let verz_pfad = if verz_teil.is_empty() {
            String::from(cwd)
        } else {
            fs::pfad_aufloesen(cwd, verz_teil)
        };

        let eintraege = vervollstaendiger.eintraege(&verz_pfad);
        let passende: Vec<&(String, bool)> = eintraege
            .iter()
            .filter(|(name, _)| name.starts_with(anfang))
            .collect();

        match passende.len() {
            0 => Reaktion::Keine,
            1 => {
                // Eindeutig: Rest des Namens ergänzen (Ordner: + '/').
                let (name, ist_verzeichnis) = passende[0];
                let mut rest = String::from(&name[anfang.len()..]);
                if *ist_verzeichnis {
                    rest.push('/');
                }
                self.zeile.push_str(&rest);
                Reaktion::Anhaengen(rest)
            }
            _ => {
                // Mehrdeutig: so weit ergänzen, wie alle übereinstimmen.
                let gemeinsam = gemeinsamer_anfang(passende.iter().map(|(n, _)| n.as_str()));
                if gemeinsam.chars().count() > anfang.chars().count() {
                    let rest = String::from(&gemeinsam[anfang.len()..]);
                    self.zeile.push_str(&rest);
                    Reaktion::Anhaengen(rest)
                } else {
                    Reaktion::KandidatenZeigen(
                        passende
                            .iter()
                            .map(|(name, ist_verzeichnis)| {
                                if *ist_verzeichnis {
                                    format!("{}/", name)
                                } else {
                                    name.clone()
                                }
                            })
                            .collect(),
                    )
                }
            }
        }
    }
}

/// Der längste gemeinsame Anfang mehrerer Namen (zeichenweise,
/// damit Umlaute nicht zerteilt werden).
fn gemeinsamer_anfang<'a>(mut namen: impl Iterator<Item = &'a str>) -> String {
    let erster = match namen.next() {
        Some(n) => n,
        None => return String::new(),
    };
    let rest: Vec<&str> = namen.collect();
    let mut gemeinsam = String::new();
    for (i, zeichen) in erster.char_indices() {
        if rest.iter().any(|name| !name[i.min(name.len())..].starts_with(zeichen)) {
            break;
        }
        gemeinsam.push(zeichen);
    }
    gemeinsam
}

// ---------------------------------------------------------------------------
// Unit-Tests — reine Logik, kein VGA, keine Tastatur, kein Dateisystem
// (nur ein Mock-Vervollständiger). Sie laufen wie alle Tests im
// QEMU-Test-Kernel, brauchen aber nichts davon.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Fester Datei-Baum für die Tab-Tests.
    struct Mock;

    impl Vervollstaendiger for Mock {
        fn eintraege(&self, pfad: &str) -> Vec<(String, bool)> {
            match pfad {
                "/" => vec![
                    (String::from("system"), true),
                    (String::from("wichtig.txt"), false),
                    (String::from("willkommen.txt"), false),
                ],
                "/system" => vec![(String::from("info.txt"), false)],
                _ => Vec::new(),
            }
        }
    }

    /// Hilfsfunktion: einen ganzen Text "eintippen".
    fn tippen(editor: &mut ZeilenEditor, text: &str) {
        for c in text.chars() {
            editor.taste(Taste::Zeichen(c), "/", &Mock);
        }
    }

    /// Tippen baut die Zeile auf, jede Taste meldet ihr Zeichen.
    #[test_case]
    fn test_tippen() {
        let mut editor = ZeilenEditor::neu(10);
        let r = editor.taste(Taste::Zeichen('h'), "/", &Mock);
        assert_eq!(r, Reaktion::Anhaengen(String::from("h")));
        tippen(&mut editor, "allo");
        assert_eq!(editor.zeile(), "hallo");
    }

    /// Backspace löscht hinten; auf leerer Zeile passiert nichts.
    #[test_case]
    fn test_backspace() {
        let mut editor = ZeilenEditor::neu(10);
        tippen(&mut editor, "ab");
        assert_eq!(editor.taste(Taste::Backspace, "/", &Mock), Reaktion::Loeschen(1));
        assert_eq!(editor.zeile(), "a");
        editor.taste(Taste::Backspace, "/", &Mock);
        // Leere Zeile: Backspace ist ein No-Op.
        assert_eq!(editor.taste(Taste::Backspace, "/", &Mock), Reaktion::Keine);
        assert_eq!(editor.zeile(), "");
    }

    /// Enter liefert die (getrimmte) Zeile und leert den Editor;
    /// der Verlauf übernimmt den Befehl, aber keine Duplikate.
    #[test_case]
    fn test_enter_und_verlauf_aufnahme() {
        let mut editor = ZeilenEditor::neu(10);
        tippen(&mut editor, "  echo hi  ");
        assert_eq!(
            editor.taste(Taste::Enter, "/", &Mock),
            Reaktion::Fertig(String::from("echo hi"))
        );
        assert_eq!(editor.zeile(), "");

        // Gleicher Befehl nochmal -> kein Verlaufs-Duplikat:
        tippen(&mut editor, "echo hi");
        editor.taste(Taste::Enter, "/", &Mock);
        tippen(&mut editor, "dir");
        editor.taste(Taste::Enter, "/", &Mock);

        // Verlauf: [dir, echo hi] — prüfen wir übers Blättern:
        let r = editor.taste(Taste::HochPfeil, "/", &Mock);
        assert_eq!(r, Reaktion::Ersetzen { geloescht: 0, neu: String::from("dir") });
        let r = editor.taste(Taste::HochPfeil, "/", &Mock);
        assert_eq!(r, Reaktion::Ersetzen { geloescht: 3, neu: String::from("echo hi") });
    }

    /// Verlauf: hoch bis zum Anschlag, runter bis zur leeren Zeile.
    #[test_case]
    fn test_verlauf_blaettern() {
        let mut editor = ZeilenEditor::neu(10);
        for befehl in ["eins", "zwei", "drei"] {
            tippen(&mut editor, befehl);
            editor.taste(Taste::Enter, "/", &Mock);
        }

        // Hoch: drei -> zwei -> eins -> (Anschlag: bleibt eins)
        editor.taste(Taste::HochPfeil, "/", &Mock);
        assert_eq!(editor.zeile(), "drei");
        editor.taste(Taste::HochPfeil, "/", &Mock);
        editor.taste(Taste::HochPfeil, "/", &Mock);
        assert_eq!(editor.zeile(), "eins");
        assert_eq!(editor.taste(Taste::HochPfeil, "/", &Mock), Reaktion::Keine);
        assert_eq!(editor.zeile(), "eins");

        // Runter: zwei -> drei -> leer; danach No-Op.
        editor.taste(Taste::RunterPfeil, "/", &Mock);
        assert_eq!(editor.zeile(), "zwei");
        editor.taste(Taste::RunterPfeil, "/", &Mock);
        assert_eq!(editor.zeile(), "drei");
        let r = editor.taste(Taste::RunterPfeil, "/", &Mock);
        assert_eq!(r, Reaktion::Ersetzen { geloescht: 4, neu: String::new() });
        assert_eq!(editor.taste(Taste::RunterPfeil, "/", &Mock), Reaktion::Keine);
    }

    /// Verlauf behält nur max_verlauf Einträge.
    #[test_case]
    fn test_verlauf_begrenzung() {
        let mut editor = ZeilenEditor::neu(2);
        for befehl in ["eins", "zwei", "drei"] {
            tippen(&mut editor, befehl);
            editor.taste(Taste::Enter, "/", &Mock);
        }
        // Nur noch [drei, zwei] — "eins" ist rausgefallen:
        editor.taste(Taste::HochPfeil, "/", &Mock);
        editor.taste(Taste::HochPfeil, "/", &Mock);
        assert_eq!(editor.zeile(), "zwei");
        assert_eq!(editor.taste(Taste::HochPfeil, "/", &Mock), Reaktion::Keine);
    }

    /// Tab mit eindeutigem Treffer: ergänzt den Rest, Ordner mit '/'.
    #[test_case]
    fn test_tab_eindeutig() {
        let mut editor = ZeilenEditor::neu(10);
        tippen(&mut editor, "cd sys");
        let r = editor.taste(Taste::Tab, "/", &Mock);
        assert_eq!(r, Reaktion::Anhaengen(String::from("tem/")));
        assert_eq!(editor.zeile(), "cd system/");

        // Und gleich weiter IN dem Ordner (relativer Pfad!):
        tippen(&mut editor, "in");
        let r = editor.taste(Taste::Tab, "/", &Mock);
        assert_eq!(r, Reaktion::Anhaengen(String::from("fo.txt")));
        assert_eq!(editor.zeile(), "cd system/info.txt");
    }

    /// Tab mit mehreren Kandidaten: erst den gemeinsamen Anfang
    /// ergänzen, beim zweiten Tab die Kandidatenliste zeigen.
    #[test_case]
    fn test_tab_mehrere_kandidaten() {
        let mut editor = ZeilenEditor::neu(10);
        tippen(&mut editor, "type w");
        // "wichtig.txt" und "willkommen.txt" teilen sich "wi":
        let r = editor.taste(Taste::Tab, "/", &Mock);
        assert_eq!(r, Reaktion::Anhaengen(String::from("i")));
        assert_eq!(editor.zeile(), "type wi");

        // Kein weiterer gemeinsamer Fortschritt -> Kandidaten zeigen:
        let r = editor.taste(Taste::Tab, "/", &Mock);
        assert_eq!(
            r,
            Reaktion::KandidatenZeigen(vec![
                String::from("wichtig.txt"),
                String::from("willkommen.txt"),
            ])
        );
        // Die Zeile bleibt dabei unverändert:
        assert_eq!(editor.zeile(), "type wi");
    }

    /// Tab ohne Treffer: nichts passiert.
    #[test_case]
    fn test_tab_ohne_treffer() {
        let mut editor = ZeilenEditor::neu(10);
        tippen(&mut editor, "type xyz");
        assert_eq!(editor.taste(Taste::Tab, "/", &Mock), Reaktion::Keine);
        assert_eq!(editor.zeile(), "type xyz");
    }
}
