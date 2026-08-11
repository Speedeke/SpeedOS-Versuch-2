// cssdump <datei|url> [pfad] — die berechneten Stile eines Knotens
//
// ===========================================================================
// WOZU
//
// `htmldump` beantwortet „Parser oder Layout?". `cssdump` beantwortet die
// naechste Frage, und die stellt man beim Bauen eines Renderers noch
// oefter: **„Warum ist das Ding blau?"**
//
// Es zeigt fuer EINEN Knoten alle berechneten Werte UND die Regel, die
// jeden gesetzt hat — mit Herkunft, Selektor und Spezifitaet. Und es zeigt
// die Regeln, die verloren haben; das ist beim Debuggen meistens die
// eigentliche Auskunft („meine Regel ist da, sie wird nur ueberstimmt").
//
// ===========================================================================
// BEDIENUNG
//
//     starte cssdump /platte/seiten/cern.html          Uebersicht
//     starte cssdump /platte/seiten/cern.html h1       das erste <h1>
//     starte cssdump seite.html "body/p[2]"            der zweite <p> in body
//     starte cssdump seite.html "#kopf"                per Id
//     starte cssdump seite.html ".warnung"             per Klasse
//     starte cssdump https://example.com p
//
//     --regeln     die geparsten Regeln statt der Knoten-Stile
//     --befund     nur, was beim Parsen uebersprungen wurde
//     --alle       ALLE Elemente mit ihren wichtigsten Werten
//     --layout     die ANZEIGE-BEFEHLE (Serie 8, Teil 6)
//     --breite=N   Seitenbreite fuer --layout (Standard 600)
//
// Die PFADANGABE ist bewusst simpel gehalten (kein zweiter Selektor-
// Dialekt): Tag-Namen, durch `/` getrennt, mit `[n]` fuer das n-te
// Vorkommen. `#id` und `.klasse` als Abkuerzung.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libspeed::netz::Klient;
use libspeed::{println, Argumente};
use speedcss::kaskade::{self, Herkunft, Quelle, Zustand};
use speedcss::{parser, stil, Stylesheet};
use speedhtml::{Dokument, KnotenId};
use speedlayout::{Befehl, Metrik};

libspeed::hauptprogramm!(haupt);
libspeed::zufall_als_getrandom!();

const OK: i32 = 0;
const FEHLER_BEDIENUNG: i32 = 2;
const FEHLER_LESEN: i32 = 3;
const FEHLER_ABRUF: i32 = 4;
const FEHLER_NICHT_GEFUNDEN: i32 = 5;

const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Modus {
    Knoten,
    Regeln,
    Befund,
    Alle,
    Layout,
}

// ---------------------------------------------------------------------------
// DIE METRIK DES BROWSERS
// ---------------------------------------------------------------------------

/// Die Naht zwischen `speedlayout` und dem, was ein Ring-3-Programm an
/// Schrift hat.
///
/// **Das ist die ganze Verdrahtung** — `speedlayout::Metrik` hat vier
/// Methoden, und drei davon haben eine Voreinstellung. Genau dafuer ist
/// das Trait so schmal geschnitten: Ein Programm, das layouten will,
/// muss kein Toolkit einbinden.
///
/// Die Zahlen sind die des 5x7-Rasters aus `libspeed::fenster` (6 Pixel
/// je Zeichen, 7 hoch) — dasselbe, das `uidemo` benutzt. Ein Prozess
/// bekommt die vorgerasterten Kernel-Schriften nicht (es gibt keinen
/// Schrift-Syscall, docs/grenzen.md), er bringt seine eigene mit.
struct RasterMetrik;

/// Breite und Hoehe des eingebauten 5x7-Rasters bei Skalierung 1.
const RASTER_BREITE: i32 = 6;
const RASTER_HOEHE: i32 = 7;

impl Metrik for RasterMetrik {
    fn text_breite(&self, text: &str, groesse: i32, _fett: bool, _kursiv: bool) -> i32 {
        // ZEICHEN zaehlen, nicht Bytes — sonst bricht jede deutsche
        // Zeile zu frueh um.
        let skala = self.skala(groesse);
        text.chars().count() as i32 * RASTER_BREITE * skala
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        (RASTER_HOEHE + 3) * self.skala(groesse)
    }
    fn grundlinie(&self, groesse: i32) -> i32 {
        RASTER_HOEHE * self.skala(groesse)
    }
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        // Das Raster kann nur GANZE Vielfache — eine Zwischengroesse
        // gibt es nicht, und das Layout soll mit der rechnen, die
        // WIRKLICH gezeichnet wird.
        (RASTER_HOEHE * self.skala(wunsch)).max(RASTER_HOEHE)
    }
}

impl RasterMetrik {
    /// Welche ganzzahlige Vergroesserung kommt dieser Wunschgroesse am
    /// naechsten? (1..4 — darueber wird das Raster klobig.)
    fn skala(&self, groesse: i32) -> i32 {
        ((groesse + RASTER_HOEHE / 2) / RASTER_HOEHE).clamp(1, 4)
    }
}

fn haupt(argumente: &Argumente) -> i32 {
    let mut quelle = None;
    let mut pfad = None;
    let mut modus = Modus::Knoten;
    let mut seitenbreite = 600i32;

    for i in 1..argumente.anzahl() {
        let Some(wort) = argumente.get(i) else {
            return FEHLER_BEDIENUNG;
        };
        match wort {
            "--regeln" => modus = Modus::Regeln,
            "--befund" => modus = Modus::Befund,
            "--alle" => modus = Modus::Alle,
            "--layout" => modus = Modus::Layout,
            _ if wort.starts_with("--breite=") => {
                match wort["--breite=".len()..].parse::<i32>() {
                    Ok(n) if (1..=100_000).contains(&n) => seitenbreite = n,
                    _ => {
                        println!("--breite= braucht eine Zahl zwischen 1 und 100000.");
                        return FEHLER_BEDIENUNG;
                    }
                }
            }
            _ if wort.starts_with("--") => {
                println!("Unbekannter Schalter: {}", wort);
                return FEHLER_BEDIENUNG;
            }
            _ if quelle.is_none() => quelle = Some(wort),
            _ if pfad.is_none() => pfad = Some(wort),
            _ => {}
        }
    }

    let Some(quelle) = quelle else {
        hilfe(argumente.programm());
        return FEHLER_BEDIENUNG;
    };

    // --- Bytes besorgen (wie htmldump: Pfad oder Netz) ---
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

    let html = String::from_utf8_lossy(&bytes);
    let dokument = speedhtml::parsen(&html);

    // --- Die Stylesheets ---
    let standard = speedcss::standard_stylesheet();
    let autor = speedcss::autor_stylesheet(&dokument);
    let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
        (&standard, Herkunft::Standard),
        (&autor, Herkunft::Autor),
    ];

    match modus {
        Modus::Befund => {
            println!("{} ({} Byte)", herkunft, bytes.len());
            println!();
            befund_zeigen("Standard-Stylesheet", &standard);
            befund_zeigen("Autor-Stylesheet (<style>-Bloecke)", &autor);
            return OK;
        }
        Modus::Regeln => {
            println!("{}: {} Autor-Regeln", herkunft, autor.regeln.len());
            println!("{}", "-".repeat(64));
            for regel in &autor.regeln {
                regel_zeigen(regel);
            }
            return OK;
        }
        _ => {}
    }

    let baum = kaskade::berechnen(&dokument, &blaetter, Zustand::default());

    if modus == Modus::Layout {
        layout_zeigen(&dokument, &baum, seitenbreite, &herkunft);
        return OK;
    }

    if modus == Modus::Alle {
        alle_zeigen(&dokument, &baum);
        return OK;
    }

    // --- Einen Knoten waehlen ---
    let Some(gesucht) = pfad else {
        uebersicht(&dokument, &autor, &herkunft, bytes.len());
        return OK;
    };
    let Some(id) = knoten_finden(&dokument, gesucht) else {
        println!("Nicht gefunden: {}", gesucht);
        println!();
        println!("Pfadangaben: tag | tag[n] | a/b/c | #id | .klasse");
        return FEHLER_NICHT_GEFUNDEN;
    };

    knoten_zeigen(&dokument, &baum, &blaetter, id, gesucht);
    OK
}

/// Das Layout durchrechnen und die Anzeige-Befehle ausgeben.
///
/// ===================================================================
/// WOZU DAS GUT IST
///
/// `htmldump` beantwortet „Parser oder Layout?", die Knoten-Ansicht von
/// `cssdump` „welche Regel hat das gesetzt?" — und das hier beantwortet
/// die dritte Frage: **„Was kommt am Ende wirklich heraus?"**
///
/// Weil das Layout eine Liste von BEFEHLEN liefert und kein Bild, ist
/// diese Ausgabe der vollstaendige Zustand vor dem Zeichnen. Wer eine
/// Merkwuerdigkeit sieht, findet sie hier als Zahl statt als Pixel.
fn layout_zeigen(dokument: &Dokument, baum: &speedcss::StilBaum, breite: i32, herkunft: &str) {
    let metrik = RasterMetrik;
    let ergebnis = speedlayout::setzen(dokument, baum, breite, &metrik);
    let liste = speedlayout::anzeigeliste(&ergebnis);

    println!("{} — Breite {} px", herkunft, breite);
    println!(
        "  {} Kaesten, {} Zeilen, Gesamthoehe {} px",
        ergebnis.befund.kaesten, ergebnis.befund.zeilen, ergebnis.hoehe
    );
    if ergebnis.befund.ueberlaeufe > 0 {
        println!("  {} Ueberlauf/Ueberlaeufe (Inhalt breiter als sein Kasten)", ergebnis.befund.ueberlaeufe);
    }
    if !ergebnis.befund.sauber() {
        println!("  ABGESCHNITTEN: {} Teilbaum/Teilbaeume zu tief", ergebnis.befund.zu_tief);
    }
    println!("  {} Anzeige-Befehle", liste.len());
    println!("{}", "-".repeat(64));

    for b in &liste.befehle {
        match b {
            Befehl::Text { x, y, text, groesse, fett, kursiv, .. } => {
                println!(
                    "TEXT   {:>5},{:<5} {:>3}px{}{}  {:?}",
                    x,
                    y,
                    groesse,
                    if *fett { " fett" } else { "" },
                    if *kursiv { " kursiv" } else { "" },
                    kuerzen(text, 48)
                );
            }
            Befehl::Rechteck { rechteck, farbe } => {
                println!(
                    "FLAECHE{:>5},{:<5} {}x{}  #{:02x}{:02x}{:02x}",
                    rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe,
                    farbe.r, farbe.g, farbe.b
                );
            }
            Befehl::Bild { rechteck, quelle, .. } => {
                println!(
                    "BILD   {:>5},{:<5} {}x{}  {:?}",
                    rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe,
                    kuerzen(quelle, 40)
                );
            }
            Befehl::Linie { x0, y0, x1, y1, dicke, .. } => {
                println!("LINIE  {:>5},{:<5} bis {},{}  {}px", x0, y0, x1, y1, dicke);
            }
        }
    }
}

/// Text fuer die Anzeige kuerzen — auf ZEICHENGRENZEN.
fn kuerzen(text: &str, max: usize) -> String {
    let mut aus = String::new();
    for (i, c) in text.chars().enumerate() {
        if i >= max {
            aus.push('~');
            break;
        }
        aus.push(if c == '\n' { ' ' } else { c });
    }
    aus
}

fn hilfe(programm: &str) {
    println!("Benutzung: {} <datei|url> [pfad] [Schalter]", programm);
    println!();
    println!("  {} /platte/seiten/cern.html h1", programm);
    println!("  {} seite.html \"body/p[2]\"", programm);
    println!("  {} seite.html \"#kopf\"", programm);
    println!();
    println!("  --regeln   die geparsten Autor-Regeln zeigen");
    println!("  --befund   was beim Parsen uebersprungen wurde");
    println!("  --alle     alle Elemente mit ihren wichtigsten Werten");
    println!("  --layout   die Anzeige-Befehle (Position, Groesse, Farbe)");
    println!("  --breite=N Seitenbreite fuer --layout (Standard 600)");
}

// ---------------------------------------------------------------------------
// Knoten suchen
// ---------------------------------------------------------------------------

/// Eine Pfadangabe aufloesen.
///
/// Bewusst KEIN zweiter Selektor-Dialekt: `tag`, `tag[n]`, `a/b/c`, `#id`,
/// `.klasse`. Wer CSS-Selektoren will, hat sie im Stylesheet.
fn knoten_finden(dokument: &Dokument, pfad: &str) -> Option<KnotenId> {
    if let Some(id) = pfad.strip_prefix('#') {
        return dokument
            .alle()
            .find(|(_, k)| k.attribut("id") == Some(id))
            .map(|(i, _)| i);
    }
    if let Some(klasse) = pfad.strip_prefix('.') {
        return dokument
            .alle()
            .find(|(_, k)| {
                k.attribut("class")
                    .unwrap_or("")
                    .split_whitespace()
                    .any(|c| c == klasse)
            })
            .map(|(i, _)| i);
    }

    // `a/b/c` — jeder Schritt sucht UNTERHALB des vorigen.
    let mut aktuell = Dokument::WURZEL;
    for schritt in pfad.split('/').filter(|s| !s.is_empty()) {
        let (tag, nummer) = match schritt.split_once('[') {
            Some((t, rest)) => {
                let n: usize = rest.trim_end_matches(']').parse().ok()?;
                (t, n.max(1))
            }
            None => (schritt, 1),
        };
        aktuell = nachkomme_mit_tag(dokument, aktuell, tag, nummer)?;
    }
    Some(aktuell)
}

/// Den n-ten Nachkommen mit diesem Tag finden (1-basiert).
fn nachkomme_mit_tag(
    dokument: &Dokument,
    ab: KnotenId,
    tag: &str,
    nummer: usize,
) -> Option<KnotenId> {
    let mut gefunden = 0usize;
    let mut stapel = alloc::vec![ab];
    let mut treffer = None;
    // Vorbestellung, damit „der zweite <p>" die Dokumentreihenfolge meint.
    let mut reihe: Vec<KnotenId> = Vec::new();
    while let Some(id) = stapel.pop() {
        let Some(knoten) = dokument.knoten(id) else {
            continue;
        };
        if id != ab {
            reihe.push(id);
        }
        for kind in knoten.kinder.iter().rev() {
            stapel.push(*kind);
        }
    }
    for id in reihe {
        if dokument.knoten(id).and_then(|k| k.name()) == Some(tag) {
            gefunden += 1;
            if gefunden == nummer {
                treffer = Some(id);
                break;
            }
        }
    }
    treffer
}

// ---------------------------------------------------------------------------
// Anzeigen
// ---------------------------------------------------------------------------

fn befund_zeigen(name: &str, blatt: &Stylesheet) {
    let b = &blatt.befund;
    println!("{}: {} Regeln", name, blatt.regeln.len());
    if b.sauber() {
        println!("  sauber — nichts musste uebersprungen werden");
    } else {
        if b.regeln_uebersprungen > 0 {
            println!("  {} Regel(n) uebersprungen", b.regeln_uebersprungen);
        }
        if b.deklarationen_uebersprungen > 0 {
            println!(
                "  {} Deklaration(en) uebersprungen",
                b.deklarationen_uebersprungen
            );
        }
        if b.selektoren_unerfuellbar > 0 {
            println!(
                "  {} Selektor(en) koennen wir nicht (>, [attr], :not, ::vor)",
                b.selektoren_unerfuellbar
            );
        }
        if b.abgeschnitten {
            println!("  ABGESCHNITTEN — eine Grenze hat gegriffen");
        }
    }
    if b.at_regeln_uebersprungen > 0 {
        println!(
            "  {} At-Regel(n) uebersprungen (@media, @import, ...)",
            b.at_regeln_uebersprungen
        );
    }
    println!();
}

fn regel_zeigen(regel: &speedcss::Regel) {
    let selektoren: Vec<&str> = regel.selektoren.iter().map(|s| s.text.as_str()).collect();
    let spez = regel
        .selektoren
        .iter()
        .map(|s| s.spezifitaet())
        .max()
        .unwrap_or_default();
    println!(
        "{}  ({},{},{})",
        selektoren.join(", "),
        spez.ids,
        spez.klassen,
        spez.typen
    );
    for d in &regel.deklarationen {
        let bekannt = if stil::bekannt(&d.name) { " " } else { "?" };
        println!(
            "  {}{}: {}{}",
            bekannt,
            d.name,
            d.wert,
            if d.wichtig { " !important" } else { "" }
        );
    }
}

fn uebersicht(dokument: &Dokument, autor: &Stylesheet, herkunft: &str, bytes: usize) {
    println!("{} ({} Byte)", herkunft, bytes);
    println!("  {} Knoten im Baum", dokument.anzahl() - 1);
    println!("  {} Autor-Regeln aus <style>-Bloecken", autor.regeln.len());
    println!();
    println!("Ohne Pfadangabe zeigt cssdump nur diese Uebersicht.");
    println!("Beispiele:");
    // Ein paar Vorschlaege aus dem, was WIRKLICH im Dokument steht — das
    // ist nuetzlicher als eine erfundene Beispielzeile.
    let mut gezeigt = 0;
    for tag in ["h1", "h2", "p", "a", "li", "td", "div", "body"] {
        if dokument.erstes(tag).is_some() {
            println!("  cssdump <datei> {}", tag);
            gezeigt += 1;
            if gezeigt >= 4 {
                break;
            }
        }
    }
}

/// Alle Elemente mit den wichtigsten Werten — die Vogelperspektive.
fn alle_zeigen(dokument: &Dokument, baum: &kaskade::StilBaum) {
    println!(
        "{:<20} {:<12} {:>8}  {:<9} {}",
        "Element", "display", "Schrift", "Farbe", "Rand oben"
    );
    println!("{}", "-".repeat(64));
    for (id, knoten) in dokument.alle() {
        let Some(name) = knoten.name() else { continue };
        let s = baum.stil(id);
        println!(
            "{:<20} {:<12} {:>8} {:<10} {}",
            name,
            alloc::format!("{:?}", s.display),
            alloc::format!("{}px", stil::tausendstel_text(s.schrift_px)),
            stil::wert_als_text(s, "color"),
            stil::wert_als_text(s, "margin"),
        );
    }
}

/// DIE HAUPTANSICHT: ein Knoten, alle Werte, jede mit ihrer Quelle.
fn knoten_zeigen(
    dokument: &Dokument,
    baum: &kaskade::StilBaum,
    blaetter: &[(&Stylesheet, Herkunft)],
    id: KnotenId,
    pfad: &str,
) {
    let Some(knoten) = dokument.knoten(id) else {
        return;
    };

    // Kopfzeile: das Element mit seinen Attributen.
    let mut kopf = String::new();
    kopf.push('<');
    kopf.push_str(knoten.name().unwrap_or("?"));
    if let Some(id_attr) = knoten.attribut("id") {
        kopf.push_str(" id=\"");
        kopf.push_str(id_attr);
        kopf.push('"');
    }
    if let Some(klasse) = knoten.attribut("class") {
        kopf.push_str(" class=\"");
        kopf.push_str(klasse);
        kopf.push('"');
    }
    if knoten.attribut("style").is_some() {
        kopf.push_str(" style=\"...\"");
    }
    kopf.push('>');

    println!("{}   (Pfad: {})", kopf, pfad);

    // Die Kette der Vorfahren — hilft beim Verstehen von Vererbung.
    let mut kette: Vec<&str> = Vec::new();
    let mut aktuell = knoten.eltern;
    while let Some(eid) = aktuell {
        let Some(e) = dokument.knoten(eid) else { break };
        if let Some(n) = e.name() {
            kette.push(n);
        }
        aktuell = e.eltern;
    }
    kette.reverse();
    if !kette.is_empty() {
        println!("  in: {}", kette.join(" > "));
    }
    println!("{}", "-".repeat(64));

    let eltern_stil = knoten
        .eltern
        .map(|e| *baum.stil(e))
        .unwrap_or(speedcss::ANFANG);
    let erklaerungen =
        kaskade::erklaeren(dokument, id, blaetter, &eltern_stil, Zustand::default());

    for e in &erklaerungen {
        println!("{:<18} {}", e.eigenschaft, e.wert);
        println!("{}", quelle_text(&e.quelle));
        for ueberstimmt in &e.ueberstimmt {
            println!("      ueberstimmt: {}", quelle_kurz(ueberstimmt));
        }
    }

    println!("{}", "-".repeat(64));
    println!("Legende: die Zeile unter jedem Wert sagt, WER ihn gesetzt hat.");
    println!("  (a,b,c) = Spezifitaet: Ids, Klassen, Typen — lexikografisch.");
}

fn quelle_text(quelle: &Quelle) -> String {
    match quelle {
        Quelle::Regel {
            herkunft,
            selektor,
            spezifitaet,
            wichtig,
            wert,
        } => alloc::format!(
            "      <- {} `{}` ({},{},{}){}  [{}]",
            match herkunft {
                Herkunft::Standard => "Standard",
                Herkunft::Autor => "Autor   ",
            },
            selektor,
            spezifitaet.ids,
            spezifitaet.klassen,
            spezifitaet.typen,
            if *wichtig { " !important" } else { "" },
            wert
        ),
        Quelle::Vererbt => String::from("      <- geerbt vom Elternteil"),
        Quelle::Anfangswert => String::from("      <- Anfangswert (niemand hat etwas gesagt)"),
    }
}

fn quelle_kurz(quelle: &Quelle) -> String {
    match quelle {
        Quelle::Regel {
            herkunft,
            selektor,
            spezifitaet,
            wichtig,
            wert,
        } => alloc::format!(
            "{} `{}` ({},{},{}){} [{}]",
            match herkunft {
                Herkunft::Standard => "Standard",
                Herkunft::Autor => "Autor",
            },
            selektor,
            spezifitaet.ids,
            spezifitaet.klassen,
            spezifitaet.typen,
            if *wichtig { " !important" } else { "" },
            wert
        ),
        Quelle::Vererbt => String::from("geerbt"),
        Quelle::Anfangswert => String::from("Anfangswert"),
    }
}

// `parser` wird fuer den Typ `Stylesheet` gebraucht; ohne diese Zeile
// meldet der Compiler einen ungenutzten Import.
#[allow(unused_imports)]
use parser as _parser_genutzt;
