// speedhtml — HTML zerlegen und zu einem Baum machen. Sonst nichts.
//
// ===========================================================================
// WAS DIESE KISTE IST
//
// Die dritte wirtsfreie Kiste des Projekts, nach `speedhttp` (Serie 7,
// Teil 4) und `speedui` (Serie 8, Teil 2). Sie kennt kein Netz, kein
// Fenster, keine Schrift, kein CSS und kein Layout — sie bekommt einen
// `&str` und liefert einen Baum.
//
// Zwei Teile:
//   * `tokenizer` — Bytes zu Token, nach dem Vorbild der
//     HTML5-Zustandsmaschine (die Begruendung fuer dieses Vorbild steht
//     dort im Kopf).
//   * `dom` — Token zu einem Baum, mit Fehlererholung.
//
// ===========================================================================
// DIE EINE ZUSAGE
//
// **JEDE BYTEFOLGE ERGIBT EINEN BAUM.** Es gibt keinen Fehlerfall, keinen
// `Result`, keine Panik. Kaputtes HTML ist der Normalfall im Web, nicht die
// Ausnahme; ein Parser, der dabei aufgibt, zeigt nie eine Seite an.
//
// Was zurechtgebogen werden musste, wird GEZAEHLT (`dom::Befund`) statt
// verschwiegen — sonst waere der Parser eine Blackbox, und bei jeder
// schiefen Seite bliebe die Frage offen, ob das Dokument kaputt war oder
// der Parser.
//
// ===========================================================================
// WOZU DAS GUT IST
//
// Der Zuschnitt des Browsers, zu dem diese Kiste gehoert, steht in
// `docs/browser-v1.md`. Der naechste Schritt danach ist CSS, dann Layout.
//
// Fuer die Fehlersuche gibt es `userland/htmldump`: Es gibt den Baum
// eingerueckt aus und beantwortet damit bei jeder Layout-Merkwuerdigkeit
// die erste Frage — **Parser oder Layout?**

// `no_std` — AUSSER beim Testen. Dieselbe Loesung wie in `speedui`: Der
// Test-Harness von Rust braucht `std`, und die Tests dieser Kiste sollen
// auf dem HOST laufen (in Millisekunden, ohne QEMU-Start). Am
// ausgelieferten Code aendert das nichts.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dom;
pub mod entitaeten;
pub mod tokenizer;

pub use dom::{parsen, parsen_mit, Art, Befund, Dokument, Grenzen, KnotenId};
pub use tokenizer::{Token, Tokenizer};

use alloc::string::String;

/// Wie tief `baum_text` hoechstens einrueckt, bevor es nur noch die Tiefe
/// als Zahl schreibt. Ohne die Deckelung waere eine Zeile bei Tiefe 100
/// zweihundert Zeichen breit, bevor der Inhalt anfaengt.
const MAX_EINRUECKUNG: usize = 20;

/// Wie viele Zeichen eines Textknotens gezeigt werden.
const TEXT_VORSCHAU: usize = 60;

/// Den Baum eingerueckt ausgeben — das Werkzeug hinter `htmldump`.
///
/// Die Ausgabe ist bewusst ZEILENWEISE und maschinenlesbar-nah: ein
/// Knoten je Zeile, Einrueckung = Tiefe. Damit laesst sie sich in einem
/// Terminal lesen UND in einem Test mit `contains` pruefen.
///
/// ITERATIV mit eigenem Stapel, nicht rekursiv: Bei `Grenzen::max_tiefe`
/// = 100 waere Rekursion zwar sicher, aber diese Funktion soll auch einen
/// Baum ueberstehen, den jemand mit anderen Grenzen gebaut hat. Der
/// User-Stack ist 64 KiB.
pub fn baum_text(dokument: &Dokument) -> String {
    let mut aus = String::new();
    // (Knoten, Tiefe) — rueckwaerts auf den Stapel, damit die Kinder in
    // Dokumentreihenfolge herauskommen.
    let mut stapel = alloc::vec![(Dokument::WURZEL, 0usize)];

    while let Some((id, tiefe)) = stapel.pop() {
        let Some(knoten) = dokument.knoten(id) else {
            continue;
        };

        if id != Dokument::WURZEL {
            for _ in 0..tiefe.min(MAX_EINRUECKUNG) {
                aus.push_str("  ");
            }
            if tiefe > MAX_EINRUECKUNG {
                aus.push_str(&alloc::format!("[+{}] ", tiefe - MAX_EINRUECKUNG));
            }
            knoten_zeile(&mut aus, knoten);
            aus.push('\n');
        }

        for kind in knoten.kinder.iter().rev() {
            stapel.push((*kind, tiefe + 1));
        }
    }
    aus
}

/// Eine Zeile fuer einen Knoten.
fn knoten_zeile(aus: &mut String, knoten: &dom::Knoten) {
    match &knoten.art {
        Art::Wurzel => aus.push_str("#dokument"),
        Art::Doctype(name) => {
            aus.push_str("<!DOCTYPE ");
            aus.push_str(name);
            aus.push('>');
        }
        Art::Kommentar(inhalt) => {
            aus.push_str("<!-- ");
            text_vorschau(aus, inhalt);
            aus.push_str(" -->");
        }
        Art::Text(inhalt) => {
            // Reiner Leerraum ist im Baum echt vorhanden (er zaehlt fuers
            // Layout), aber als Zeile waere er unlesbar — deshalb wird er
            // BENANNT statt gezeigt.
            if inhalt.trim().is_empty() {
                aus.push_str(&alloc::format!("\"\" (Leerraum, {} Zeichen)", inhalt.chars().count()));
            } else {
                aus.push('"');
                text_vorschau(aus, inhalt.trim());
                aus.push('"');
            }
        }
        Art::Element { name, attribute } => {
            aus.push('<');
            aus.push_str(name);
            for (n, w) in attribute {
                aus.push(' ');
                aus.push_str(n);
                if !w.is_empty() {
                    aus.push_str("=\"");
                    text_vorschau(aus, w);
                    aus.push('"');
                }
            }
            aus.push('>');
            if dom::ist_void(name) {
                aus.push_str("  (void)");
            }
        }
    }
}

/// Text gekuerzt und ohne Zeilenumbrueche anhaengen.
///
/// Zeilenumbrueche werden zu `\n` als ZWEI ZEICHEN — eine Baumausgabe, in
/// der ein Textknoten ueber drei Zeilen geht, ist keine Baumausgabe mehr.
fn text_vorschau(aus: &mut String, text: &str) {
    for (gezeigt, c) in text.chars().enumerate() {
        if gezeigt >= TEXT_VORSCHAU {
            aus.push('…');
            return;
        }
        match c {
            '\n' => aus.push_str("\\n"),
            '\r' => aus.push_str("\\r"),
            '\t' => aus.push_str("\\t"),
            _ => aus.push(c),
        }
    }
}

/// Eine Zusammenfassung des Befunds, eine Zeile je Auffaelligkeit.
///
/// Getrennt von `baum_text`, weil man sie oft OHNE den Baum will (bei
/// einem Wikipedia-Artikel sind es 20 000 Zeilen Baum und 5 Zeilen
/// Befund).
pub fn befund_text(dokument: &Dokument) -> String {
    let b = &dokument.befund;
    let mut aus = String::new();
    aus.push_str(&alloc::format!(
        "{} Knoten, groesste Tiefe {}\n",
        b.knoten,
        b.tiefe
    ));
    if b.sauber() {
        aus.push_str("sauber — nichts musste zurechtgebogen werden\n");
        return aus;
    }
    if b.implizit_geschlossen > 0 {
        aus.push_str(&alloc::format!(
            "{} Tag(s) implizit geschlossen (z. B. <p> vor einem Block)\n",
            b.implizit_geschlossen
        ));
    }
    if b.unerwartete_endtags > 0 {
        aus.push_str(&alloc::format!(
            "{} unerwartete(s) Endtag(s) ignoriert\n",
            b.unerwartete_endtags
        ));
    }
    if b.uebersprungene_ebenen > 0 {
        aus.push_str(&alloc::format!(
            "{} Ebene(n) mitgeschlossen (Ueberkreuzung wie <b><i></b>)\n",
            b.uebersprungene_ebenen
        ));
    }
    if b.am_ende_geschlossen > 0 {
        aus.push_str(&alloc::format!(
            "{} Element(e) waren am Dokumentende noch offen\n",
            b.am_ende_geschlossen
        ));
    }
    if b.abgeschnitten {
        aus.push_str("ABGESCHNITTEN — eine Grenze hat gegriffen, der Baum ist unvollstaendig\n");
    }
    aus
}

#[cfg(test)]
mod tests;
