// news <url> [--breite=N] [--roh] — eine Webseite als Text im Terminal
//
// ==========================================================================
// WAS DIESES PROGRAMM IST — UND WAS AUSDRUECKLICH NICHT
//
// Es ist der BEWEIS, dass `libspeed::netz` traegt. Der ganze Netz-Teil sind
// drei Zeilen:
//
//     let mut klient = Klient::neu();
//     klient.max_bytes = ...;
//     let abruf = klient.holen(url)?;
//
// Alles andere in dieser Datei ist Textaufbereitung. Genau so soll eine
// Abrufschicht sich anfuehlen: Wer eine Seite braucht, schreibt drei Zeilen
// und kuemmert sich um seine eigene Aufgabe.
//
// ES IST KEIN HTML-RENDERER. Es entfernt Tags, loest ein paar Entities auf
// und bricht Zeilen um — mehr nicht. Kein DOM, kein CSS, keine Tabellen,
// kein JavaScript. Ein Browser (Serie 8) ist etwas anderes; dies ist der
// Vorgeschmack darauf, wie sich „eine Seite lesen" anfuehlt, wenn der
// Unterbau steht.
//
// DIE ENTSCHEIDUNG, die den Unterschied macht: `<script>`- und
// `<style>`-BLOECKE werden mitsamt Inhalt geworfen, nicht nur ihre Tags.
// Wer nur Tags entfernt, bekommt bei jeder modernen Seite seitenweise
// JavaScript zu lesen — und haelt das Ergebnis fuer kaputt.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libspeed::netz::Klient;
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

libspeed::zufall_als_getrandom!();

const OK: i32 = 0;
const FEHLER_BEDIENUNG: i32 = 2;
const FEHLER_ABRUF: i32 = 3;

/// Voreingestellte Zeilenbreite. Die FramebufferKonsole ist bei 720p rund
/// 100 Zeichen breit; 78 laesst Rand und liest sich angenehm.
const BREITE: usize = 78;
/// Mehr als das holen wir nicht — eine Textseite, kein Download.
const MAX_BYTES: usize = 512 * 1024;

fn haupt(argumente: &Argumente) -> i32 {
    let mut url = None;
    let mut breite = BREITE;
    let mut roh = false;
    for i in 1..argumente.anzahl() {
        let Some(wort) = argumente.get(i) else {
            return FEHLER_BEDIENUNG;
        };
        if let Some(wert) = wort.strip_prefix("--breite=") {
            match wert.parse::<usize>() {
                Ok(n) if (20..=400).contains(&n) => breite = n,
                _ => {
                    println!("--breite= braucht eine Zahl zwischen 20 und 400.");
                    return FEHLER_BEDIENUNG;
                }
            }
        } else if wort == "--roh" {
            roh = true;
        } else if wort.starts_with("--") {
            println!("Unbekannter Schalter: {}", wort);
            return FEHLER_BEDIENUNG;
        } else if url.is_none() {
            url = Some(wort);
        }
    }
    let Some(url) = url else {
        println!("Benutzung: {} <url> [--breite=N] [--roh]", argumente.programm());
        println!("  {} https://example.com", argumente.programm());
        println!();
        println!("Holt eine Seite und zeigt sie als Text: Tags raus, Entities");
        println!("aufgeloest, Zeilen umgebrochen. Es ist KEIN HTML-Renderer.");
        println!("  --roh  den unveraenderten Rumpf ausgeben");
        return FEHLER_BEDIENUNG;
    };

    // ---- DER GANZE NETZ-TEIL ----
    let mut klient = Klient::neu();
    klient.max_bytes = MAX_BYTES;
    let abruf = match klient.holen(url) {
        Ok(abruf) => abruf,
        Err(fehler) => {
            println!("{}", fehler.text());
            return FEHLER_ABRUF;
        }
    };
    // ---- ab hier geht es nur noch um Text ----

    let schloss = if abruf.info.tls { "[verschluesselt]" } else { "[KLARTEXT]" };
    println!("=== {} {} ===", abruf.ziel.als_text(), schloss);
    println!(
        // Bindestrich, kein Gedankenstrich: Die FramebufferKonsole ist
        // Latin-1 und macht aus "—" ein "?" (CLAUDE.md, Serie-4-Abschluss).
        "HTTP {} {} - {} Byte, {} ms{}",
        abruf.antwort.status,
        abruf.antwort.grund,
        abruf.antwort.rumpf.len(),
        abruf.dauer_ms,
        if abruf.weiterleitungen > 0 {
            ", nach Weiterleitung"
        } else {
            ""
        }
    );
    println!();

    if roh {
        let _ = libspeed::schreibe(libspeed::AUSGABE, &abruf.antwort.rumpf);
        println!();
        return OK;
    }

    let quelle = String::from_utf8_lossy(&abruf.antwort.rumpf);
    let text = if sieht_nach_html_aus(&abruf, &quelle) {
        html_zu_text(&quelle)
    } else {
        // Kein HTML (Klartext, JSON, ...): nur Zeilen normalisieren.
        quelle.replace('\r', "")
    };

    if let Some(titel) = titel_finden(&quelle) {
        println!("### {}", titel);
        println!();
    }
    for zeile in text.lines() {
        for stueck in umbrechen(zeile, breite) {
            println!("{}", stueck);
        }
    }
    OK
}

/// Ist das HTML? Erst den Content-Type fragen, dann den Inhalt ansehen.
fn sieht_nach_html_aus(abruf: &libspeed::netz::Abruf, quelle: &str) -> bool {
    if let Some(typ) = abruf.antwort.header_wert("content-type") {
        let typ = typ.to_ascii_lowercase();
        if typ.contains("html") {
            return true;
        }
        if typ.starts_with("text/plain") || typ.contains("json") {
            return false;
        }
    }
    // Ohne brauchbare Angabe: nachsehen. `<` in den ersten Zeichen ist ein
    // gutes Indiz, aber kein Beweis — und mehr braucht es hier nicht.
    let anfang = &quelle[..quelle.len().min(200)].to_ascii_lowercase();
    anfang.contains("<html") || anfang.contains("<!doctype html")
}

/// Der Inhalt von `<title>…</title>`, falls vorhanden.
fn titel_finden(quelle: &str) -> Option<String> {
    let klein = quelle.to_ascii_lowercase();
    let start = klein.find("<title")?;
    let inhalt_ab = start + klein[start..].find('>')? + 1;
    let ende = inhalt_ab + klein[inhalt_ab..].find("</title>")?;
    let roh = entities_aufloesen(&quelle[inhalt_ab..ende]);
    let sauber = roh.split_whitespace().collect::<Vec<_>>().join(" ");
    if sauber.is_empty() {
        None
    } else {
        Some(sauber)
    }
}

// ===========================================================================
// HTML -> Text
// ===========================================================================

/// Tags, deren INHALT weggeworfen wird (nicht nur das Tag selbst).
const STUMME_BLOECKE: &[&str] = &["script", "style", "head", "noscript", "svg", "template"];

/// Tags, die einen Zeilenumbruch bedeuten.
const UMBRUCH_TAGS: &[&str] = &[
    "p", "br", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
    "header", "footer", "nav", "blockquote", "pre", "ul", "ol", "table", "hr",
];

/// Wandelt HTML in etwas Lesbares.
///
/// Der Laeufer ist bewusst ein Zeichen-Automat und kein Parser: Er kennt
/// „ausserhalb eines Tags", „in einem Tag" und „in einem stummen Block".
/// Damit kommt er auch mit kaputtem HTML zurecht — und kaputtes HTML ist
/// die Regel, nicht die Ausnahme. Er kann dafuer nichts VERSTEHEN; genau
/// deshalb ist das hier kein Renderer.
fn html_zu_text(quelle: &str) -> String {
    let bytes = quelle.as_bytes();
    let mut aus = String::with_capacity(quelle.len() / 2);
    let mut i = 0usize;
    // Wenn wir in einem stummen Block sind: auf welches `</name>` warten wir?
    let mut stumm_bis: Option<&'static str> = None;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            if stumm_bis.is_none() {
                // Ein gewoehnliches Zeichen. Mehrfachen Weissraum sofort
                // zusammenfassen — sonst besteht die halbe Ausgabe aus der
                // Einrueckung des Quelltexts.
                let zeichen = bytes[i] as char;
                if zeichen.is_ascii_whitespace() {
                    if !aus.ends_with(' ') && !aus.ends_with('\n') {
                        aus.push(' ');
                    }
                } else {
                    // Nicht-ASCII sauber uebernehmen (UTF-8 mehrbytig).
                    let rest = &quelle[i..];
                    if let Some(z) = rest.chars().next() {
                        aus.push(z);
                        i += z.len_utf8();
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }

        // Ab hier: ein Tag (oder etwas, das so aussieht).
        let Some(tag_ende) = quelle[i..].find('>').map(|p| i + p) else {
            break; // '<' ohne '>' — der Rest ist unbrauchbar
        };
        let inneres = &quelle[i + 1..tag_ende];
        let schliessend = inneres.starts_with('/');
        let name = tag_name(inneres);

        if let Some(warte_auf) = stumm_bis {
            if schliessend && name == warte_auf {
                stumm_bis = None;
            }
        } else if !schliessend && STUMME_BLOECKE.contains(&name.as_str()) {
            // Selbstschliessend (`<svg …/>`) faengt keinen Block an.
            if !inneres.ends_with('/') {
                stumm_bis = STUMME_BLOECKE.iter().find(|t| **t == name).copied();
            }
        } else if UMBRUCH_TAGS.contains(&name.as_str()) {
            zeilenumbruch(&mut aus);
        }
        i = tag_ende + 1;
    }

    entities_aufloesen(aus.trim())
}

/// Der Name eines Tags, klein geschrieben ("/p" -> "p", "a href=…" -> "a").
fn tag_name(inneres: &str) -> String {
    let ohne_slash = inneres.trim_start_matches('/').trim_start();
    ohne_slash
        .split(|z: char| z.is_ascii_whitespace() || z == '/' || z == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Fuegt einen Umbruch an, aber nie zwei leere Zeilen hintereinander.
fn zeilenumbruch(aus: &mut String) {
    while aus.ends_with(' ') {
        aus.pop();
    }
    if aus.is_empty() || aus.ends_with("\n\n") {
        return;
    }
    aus.push('\n');
}

/// Loest die Entities auf, die in echtem Text wirklich vorkommen.
///
/// BEWUSST EINE KURZE LISTE plus die numerischen Formen. Die vollstaendige
/// HTML-Entity-Tabelle hat ueber 2000 Eintraege; sie hier einzubauen waere
/// viel Tabelle fuer wenig Gewinn. Was NICHT erkannt wird, bleibt einfach
/// stehen — sichtbar und harmlos.
fn entities_aufloesen(text: &str) -> String {
    const BEKANNT: &[(&str, &str)] = &[
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", " "),
        ("mdash", "-"),
        ("ndash", "-"),
        ("hellip", "..."),
        ("laquo", "\""),
        ("raquo", "\""),
        ("bdquo", "\""),
        ("ldquo", "\""),
        ("rdquo", "\""),
        ("szlig", "ß"),
        ("auml", "ä"),
        ("ouml", "ö"),
        ("uuml", "ü"),
        ("Auml", "Ä"),
        ("Ouml", "Ö"),
        ("Uuml", "Ü"),
        ("euro", "EUR"),
        ("copy", "(c)"),
    ];

    let mut aus = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        aus.push_str(&rest[..start]);
        let nach = &rest[start + 1..];
        // Eine Entity ist kurz; laenger als 10 Zeichen ist keine.
        let ende = match nach.find(';') {
            Some(ende) if ende <= 10 => ende,
            _ => {
                aus.push('&');
                rest = nach;
                continue;
            }
        };
        let name = &nach[..ende];
        let ersetzt = if let Some(zahl) = name.strip_prefix("#") {
            // Numerisch: &#228; oder &#xE4;
            let wert = if let Some(hex) = zahl.strip_prefix('x').or_else(|| zahl.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                zahl.parse::<u32>().ok()
            };
            wert.and_then(char::from_u32).map(String::from)
        } else {
            BEKANNT
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, wert)| String::from(*wert))
        };
        match ersetzt {
            Some(wert) => aus.push_str(&wert),
            // Unbekannt: unveraendert stehen lassen.
            None => {
                aus.push('&');
                aus.push_str(name);
                aus.push(';');
            }
        }
        rest = &nach[ende + 1..];
    }
    aus.push_str(rest);
    aus
}

/// Bricht eine Zeile an Wortgrenzen auf `breite` Zeichen um.
///
/// Zaehlt ZEICHEN, nicht Bytes — sonst zerlegt ein Umlaut die Zeile mitten
/// im UTF-8-Codepunkt.
fn umbrechen(zeile: &str, breite: usize) -> Vec<String> {
    if zeile.chars().count() <= breite {
        return alloc::vec![String::from(zeile)];
    }
    let mut zeilen = Vec::new();
    let mut aktuell = String::new();
    let mut laenge = 0usize;
    for wort in zeile.split(' ') {
        let wortlaenge = wort.chars().count();
        if laenge > 0 && laenge + 1 + wortlaenge > breite {
            zeilen.push(core::mem::take(&mut aktuell));
            laenge = 0;
        }
        if wortlaenge > breite {
            // Ein einzelnes Wort, das laenger ist als die Breite (URLs!):
            // hart durchschneiden, statt die Zeile ueberlaufen zu lassen.
            for zeichen in wort.chars() {
                if laenge == breite {
                    zeilen.push(core::mem::take(&mut aktuell));
                    laenge = 0;
                }
                aktuell.push(zeichen);
                laenge += 1;
            }
            continue;
        }
        if laenge > 0 {
            aktuell.push(' ');
            laenge += 1;
        }
        aktuell.push_str(wort);
        laenge += wortlaenge;
    }
    if !aktuell.is_empty() {
        zeilen.push(aktuell);
    }
    zeilen
}
