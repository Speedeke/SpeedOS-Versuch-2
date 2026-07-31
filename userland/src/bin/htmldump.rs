// htmldump <datei|url> — den geparsten HTML-Baum anzeigen
//
// ===========================================================================
// WOZU ES DIESES WERKZEUG GIBT — UND WARUM JETZT UND NICHT SPAETER
//
// Sobald der Renderer da ist, wird es Seiten geben, die falsch aussehen.
// Die erste Frage lautet dann IMMER: **Parser oder Layout?**
//
// Ohne dieses Werkzeug ist das eine halbe Stunde Rätselraten mit
// eingestreuten Debug-Ausgaben. Mit ihm ist es eine Sekunde:
//
//     starte htmldump /platte/seite.html | filter table
//
// Deshalb gehoert es in denselben Schritt wie der Parser und nicht in
// einen spaeteren — ein Werkzeug, das man erst baut, wenn man es braucht,
// baut man unter Druck und deshalb schlecht.
//
// ===========================================================================
// BEDIENUNG
//
//     starte htmldump /platte/seite.html      Datei parsen
//     starte htmldump https://example.com     Seite holen und parsen
//
//     --befund     nur die Zusammenfassung (was zurechtgebogen wurde)
//     --text       nur den sichtbaren Text (ohne script/style)
//     --tags       nur eine Haeufigkeitsliste der Tags
//     --tiefe=N    nur bis Tiefe N ausgeben
//
// Ohne Schalter: der ganze Baum, eingerueckt, plus Befund.
//
// ===========================================================================
// WARUM DIE AUSGABE UEBER EINE PIPE GEHT
//
// Ein Wikipedia-Artikel ergibt rund 20 000 Zeilen Baum. Die schiebt man
// nicht durchs Terminal, sondern durch `filter`:
//
//     starte htmldump <url> | filter "<table"
//
// Genau dafuer gibt es die Pipes aus Serie 6, Teil 6 — und dies ist ihr
// erster Nutzer, der nicht selbst dafuer gebaut wurde.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libspeed::netz::Klient;
use libspeed::{println, Argumente};
use speedhtml::{befund_text, Art, Dokument, Grenzen, KnotenId};

libspeed::hauptprogramm!(haupt);
libspeed::zufall_als_getrandom!();

const OK: i32 = 0;
const FEHLER_BEDIENUNG: i32 = 2;
const FEHLER_LESEN: i32 = 3;
const FEHLER_ABRUF: i32 = 4;

/// Mehr als das holen wir nicht. Passt zu `Grenzen::max_knoten` — eine
/// Seite, die groesser ist, wuerde ohnehin abgeschnitten.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Was ausgegeben werden soll.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Modus {
    Baum,
    Befund,
    Text,
    Tags,
}

fn haupt(argumente: &Argumente) -> i32 {
    let mut quelle = None;
    let mut modus = Modus::Baum;
    let mut max_tiefe = usize::MAX;

    for i in 1..argumente.anzahl() {
        let Some(wort) = argumente.get(i) else {
            return FEHLER_BEDIENUNG;
        };
        match wort {
            "--befund" => modus = Modus::Befund,
            "--text" => modus = Modus::Text,
            "--tags" => modus = Modus::Tags,
            _ if wort.starts_with("--tiefe=") => {
                match wort["--tiefe=".len()..].parse::<usize>() {
                    Ok(n) => max_tiefe = n,
                    Err(_) => {
                        println!("--tiefe= braucht eine Zahl.");
                        return FEHLER_BEDIENUNG;
                    }
                }
            }
            _ if wort.starts_with("--") => {
                println!("Unbekannter Schalter: {}", wort);
                return FEHLER_BEDIENUNG;
            }
            _ if quelle.is_none() => quelle = Some(wort),
            _ => {}
        }
    }

    let Some(quelle) = quelle else {
        hilfe(argumente.programm());
        return FEHLER_BEDIENUNG;
    };

    // --- Die Bytes besorgen ---
    //
    // Datei oder Netz — entschieden am PRAEFIX, nicht geraten. Ein Pfad
    // beginnt bei uns immer mit `/`.
    let (bytes, herkunft) = if quelle.starts_with('/') {
        match libspeed::netz::datei_lesen(quelle) {
            Ok(b) => (b, String::from(quelle)),
            Err(f) => {
                println!("{}: {}", quelle, f.text());
                return FEHLER_LESEN;
            }
        }
    } else {
        let mut klient = Klient::neu();
        klient.max_bytes = MAX_BYTES;
        match klient.holen(quelle) {
            Ok(abruf) => {
                let ziel = abruf.ziel.als_text();
                (abruf.antwort.rumpf, ziel)
            }
            Err(f) => {
                println!("Abruf fehlgeschlagen ({}): {}", f.kurz(), f.text());
                return FEHLER_ABRUF;
            }
        }
    };

    // --- Parsen ---
    //
    // `from_utf8_lossy`, nicht `from_utf8`: Eine Seite in Latin-1 oder mit
    // einem kaputten Byte soll ANGEZEIGT werden, nicht abgelehnt. Die
    // kaputten Stellen werden zum Ersatzzeichen — sichtbar falsch statt
    // gar nichts. (Eine richtige Zeichensatz-Erkennung ueber
    // Content-Type und `<meta charset>` fehlt noch und steht in
    // docs/browser-v1.md.)
    let html = String::from_utf8_lossy(&bytes);
    let dokument = speedhtml::parsen_mit(&html, Grenzen::standard());

    match modus {
        Modus::Befund => {
            println!("{} ({} Byte)", herkunft, bytes.len());
            print_mehrzeilig(&befund_text(&dokument));
        }
        Modus::Text => {
            print_mehrzeilig(&dokument.text_von(Dokument::WURZEL));
        }
        Modus::Tags => tags_zaehlen(&dokument),
        Modus::Baum => {
            println!("{} ({} Byte)", herkunft, bytes.len());
            println!("{}", "-".repeat(60));
            baum_ausgeben(&dokument, max_tiefe);
            println!("{}", "-".repeat(60));
            print_mehrzeilig(&befund_text(&dokument));
        }
    }
    OK
}

fn hilfe(programm: &str) {
    println!("Benutzung: {} <datei|url> [Schalter]", programm);
    println!();
    println!("  {} /platte/seite.html", programm);
    println!("  {} https://example.com", programm);
    println!();
    println!("  --befund     nur die Zusammenfassung (was zurechtgebogen wurde)");
    println!("  --text       nur den sichtbaren Text (ohne script/style)");
    println!("  --tags       Haeufigkeitsliste der Tags");
    println!("  --tiefe=N    nur bis Tiefe N");
    println!();
    println!("  Grosse Seiten durch eine Pipe schicken:");
    println!("    starte {} <url> | filter \"<table\"", programm);
}

/// Einen mehrzeiligen String zeilenweise ausgeben.
///
/// `println!("{}", text)` mit 20 000 Zeilen auf einmal wuerde den
/// Ausgabepuffer von libspeed sprengen (er schreibt in Haeppchen von
/// MAX_PUFFER). Zeilenweise ist ausserdem das, was eine Pipe erwartet.
fn print_mehrzeilig(text: &str) {
    for zeile in text.lines() {
        println!("{}", zeile);
    }
}

/// Den Baum ausgeben — mit eigenem Stapel, nicht rekursiv.
///
/// `speedhtml::baum_text` baut den GANZEN Baum als einen String; bei einem
/// Wikipedia-Artikel sind das mehrere MiB auf einem 12-MiB-Heap. Hier wird
/// deshalb Zeile fuer Zeile ausgegeben und nichts gesammelt — dasselbe
/// Argument wie beim Tokenizer, der ein Iterator ist.
fn baum_ausgeben(dokument: &Dokument, max_tiefe: usize) {
    let mut stapel: Vec<(KnotenId, usize)> = alloc::vec![(Dokument::WURZEL, 0)];
    let mut zeile = String::new();

    while let Some((id, tiefe)) = stapel.pop() {
        let Some(knoten) = dokument.knoten(id) else {
            continue;
        };
        if id != Dokument::WURZEL && tiefe <= max_tiefe {
            zeile.clear();
            for _ in 0..tiefe.min(20) {
                zeile.push_str("  ");
            }
            knoten_beschreiben(&mut zeile, knoten);
            println!("{}", zeile);
        }
        if tiefe < max_tiefe {
            for kind in knoten.kinder.iter().rev() {
                stapel.push((*kind, tiefe + 1));
            }
        }
    }
}

/// Eine Zeile fuer einen Knoten.
///
/// Bewusst dieselbe Form wie `speedhtml::baum_text` (das die Host-Tests
/// benutzen) — wer die Ausgabe hier liest, sieht dasselbe wie ein Test.
fn knoten_beschreiben(aus: &mut String, knoten: &speedhtml::dom::Knoten) {
    match &knoten.art {
        Art::Wurzel => aus.push_str("#dokument"),
        Art::Doctype(n) => {
            aus.push_str("<!DOCTYPE ");
            aus.push_str(n);
            aus.push('>');
        }
        Art::Kommentar(_) => aus.push_str("<!-- ... -->"),
        Art::Text(t) => {
            if t.trim().is_empty() {
                aus.push_str("\"\" (Leerraum)");
            } else {
                aus.push('"');
                kurz_anhaengen(aus, t.trim(), 60);
                aus.push('"');
            }
        }
        Art::Element { name, attribute } => {
            aus.push('<');
            aus.push_str(name);
            for (n, w) in attribute.iter().take(6) {
                aus.push(' ');
                aus.push_str(n);
                if !w.is_empty() {
                    aus.push_str("=\"");
                    kurz_anhaengen(aus, w, 40);
                    aus.push('"');
                }
            }
            if attribute.len() > 6 {
                aus.push_str(" …");
            }
            aus.push('>');
            if speedhtml::dom::ist_void(name) {
                aus.push_str("  (void)");
            }
        }
    }
}

/// Text gekuerzt und einzeilig anhaengen.
fn kurz_anhaengen(aus: &mut String, text: &str, max: usize) {
    for (i, c) in text.chars().enumerate() {
        if i >= max {
            aus.push('…');
            return;
        }
        match c {
            '\n' | '\r' | '\t' => aus.push(' '),
            _ => aus.push(c),
        }
    }
}

/// Wie oft kommt welcher Tag vor?
///
/// Die schnellste Antwort auf „warum fehlt die Tabelle?" — steht dort
/// `table: 0`, hat der Parser keine gefunden, und die Suche geht in die
/// Seite statt ins Layout.
fn tags_zaehlen(dokument: &Dokument) {
    // Eine einfache Liste statt einer Hashmap: Ein Dokument hat selten
    // mehr als 60 verschiedene Tag-Namen, und `alloc` hat keine HashMap
    // ohne Hasher.
    let mut zaehler: Vec<(String, usize)> = Vec::new();
    for (_, knoten) in dokument.alle() {
        let Some(name) = knoten.name() else { continue };
        match zaehler.iter_mut().find(|(n, _)| n == name) {
            Some((_, anzahl)) => *anzahl += 1,
            None => zaehler.push((String::from(name), 1)),
        }
    }
    // Absteigend nach Haeufigkeit.
    zaehler.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    println!("{} verschiedene Tags, {} Knoten insgesamt:", zaehler.len(), dokument.anzahl() - 1);
    for (name, anzahl) in &zaehler {
        println!("  {:>6}  {}", anzahl, name);
    }
}
