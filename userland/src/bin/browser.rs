// browser <datei|url> — SpeedOS zeigt eine Webseite an (Serie 8, Teil 7)
//
// ===========================================================================
// DER MOMENT, AUF DEN SERIE 8 ZULAEUFT
//
// Bis hierher war jede Stufe fuer sich pruefbar: `htmldump` zeigt den
// Baum, `cssdump` die berechneten Werte, `cssdump --layout` die
// Anzeige-Befehle. Hier werden aus den Befehlen Pixel — und zwar in
// einem UNPRIVILEGIERTEN PROZESS mit eigenem Adressraum, der das
// Betriebssystem nur durch die 28 Zahlen aus docs/syscalls.md kennt.
//
// ===========================================================================
// WIE WENIG HIER STEHT, IST DER PUNKT
//
// Der Browser parst nicht, kaskadiert nicht, layoutet nicht und malt
// nicht. Er VERDRAHTET:
//
//     speedhttp/netz  holt die Bytes
//     speedhtml       macht daraus einen Baum
//     speedcss        rechnet die Stile aus
//     speedlayout     macht daraus Kaesten und Anzeige-Befehle
//     speedpaint      malt die Befehle und entscheidet, wann was noetig ist
//     libspeed        Fenster, Ereignisse, Leinwand, Bilder
//
// Was hier eigen ist, sind die Ereignisschleife und die Frage, WELCHE
// Arbeit ein Ereignis ausloest. Genau darum geht es in diesem Teil.
//
// ===========================================================================
// BEDIENUNG
//
//     starte browser /platte/seiten/cern.html &     (das `&` ist PFLICHT)
//     starte browser https://example.com &
//
//     Bild auf/ab, Pfeile, Pos1/Ende, Mausrad   scrollen
//     Balken rechts                             ziehen oder anklicken
//
//     --breite=N     feste Layout-Breite statt der Fensterbreite
//     --messen=N     MESSMODUS: N Scroll-Frames, Zahlen statt Bedienung
//                    (docs/browser-rendern.md — das Umstiegskriterium)
//
// DAS `&` IST PFLICHT: Solange ein Shell-Befehl synchron laeuft, kommt
// kein anderer Kernel-Task dran — auch der Compositor nicht. Ohne `&`
// zeichnet der Browser brav, und niemand sieht es (Serie 8, Teil 1).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libspeed::fenster::{Ereignis, Fenster};
use libspeed::leinwand::{FensterLeinwand, RasterMetrik};
use libspeed::netz::Klient;
use libspeed::{println, Argumente};
use speedcss::{Herkunft, StilBaum, Stylesheet, Zustand};
use speedhtml::Dokument;
use speedlayout::{Anzeigeliste, Metrik};
use speedpaint::maler::{Auftrag, Bild, Bildquelle};
use speedpaint::{malen, Anlass, Massnahme, Scrollschritt, Sicht};
use speedui::{Farbe, Rechteck};

libspeed::hauptprogramm!(haupt);
libspeed::zufall_als_getrandom!();

const OK: i32 = 0;
const FEHLER_BEDIENUNG: i32 = 2;
const FEHLER_LESEN: i32 = 3;
const FEHLER_ABRUF: i32 = 4;

const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Startgroesse des Fensters. 720p-Klasse minus Deko — auf einem
/// groesseren Schirm zieht man es auf, und das loest ein Neu-Layout aus
/// (Regel 1).
const START_BREITE: usize = 900;
const START_HOEHE: usize = 620;

/// Breite des Scrollbalkens. Er liegt UEBER dem Inhalt (wie ein
/// Overlay-Balken); die Layout-Breite wird dafuer um denselben Betrag
/// verringert, damit kein Text unter ihm verschwindet.
const BALKEN_BREITE: i32 = 12;

const SEITE_HINTERGRUND: Farbe = Farbe::neu(255, 255, 255);
const BALKEN_SPUR: Farbe = Farbe::mit_alpha(0, 0, 0, 30);
const BALKEN_GREIFER: Farbe = Farbe::neu(140, 140, 150);

// ===========================================================================
// DIE BILDER
// ===========================================================================

/// Die schon dekodierten Bilder der Seite.
///
/// ===================================================================
/// WAS HIER GEHT UND WAS NICHT
///
/// Geladen werden **Bilder von der Platte** — also alles, worauf eine
/// lokale Seite mit `src="bild.png"` zeigt. Bilder aus dem NETZ werden
/// (noch) nicht geholt: Das braeuchte je Bild einen Abruf mit eigener
/// Frist, und der gehoert in eine Ereignisschleife, die nebenher laedt,
/// statt in einen Renderer. In docs/grenzen.md eingetragen.
///
/// DEKODIERT WIRD IN RING 3, wie immer (Serie 8, Teil 3): Ein
/// Bilddekoder ist ein Parser fuer fremde Daten. Schlaegt er fehl, gibt
/// es den Platzhalter — der Maler zaehlt es, und die Seite bleibt
/// stehen.
struct Bilder {
    /// Was schon versucht wurde. Ein gescheiterter Versuch bleibt als
    /// Eintrag stehen — damit wird eine kaputte Datei nicht bei jedem
    /// Frame erneut dekodiert.
    geladen: Vec<Eintrag>,
    /// Der Ordner der Seite, gegen den relative Quellen aufgeloest
    /// werden. Leer, wenn die Seite aus dem Netz kam.
    ordner: String,
}

/// Ein Bild-Versuch: die Quelle und, wenn es geklappt hat, die Pixel.
struct Eintrag {
    quelle: String,
    /// `None` = versucht und gescheitert.
    bild: Option<GeladenesBild>,
}

struct GeladenesBild {
    breite: i32,
    hoehe: i32,
    rgba: Vec<u8>,
}

impl Bilder {
    fn neu(ordner: String) -> Bilder {
        Bilder {
            geladen: Vec::new(),
            ordner,
        }
    }

    fn kennt(&self, quelle: &str) -> bool {
        self.geladen.iter().any(|e| e.quelle == quelle)
    }

    /// Ein Bild laden. Liefert `true`, wenn es jetzt zu sehen ist.
    fn laden(&mut self, quelle: &str) -> bool {
        if self.kennt(quelle) {
            return false;
        }
        let bild = self.versuchen(quelle);
        let gelungen = bild.is_some();
        self.geladen.push(Eintrag {
            quelle: String::from(quelle),
            bild,
        });
        gelungen
    }

    fn versuchen(&self, quelle: &str) -> Option<GeladenesBild> {
        if self.ordner.is_empty() || quelle.starts_with("http") {
            return None;
        }
        let pfad = if quelle.starts_with('/') {
            String::from(quelle)
        } else {
            let mut p = self.ordner.clone();
            p.push('/');
            p.push_str(quelle);
            p
        };
        let bytes = libspeed::netz::datei_lesen(&pfad).ok()?;
        let bild = libspeed::bild::dekodieren(&bytes).ok()?;
        Some(GeladenesBild {
            breite: bild.breite() as i32,
            hoehe: bild.hoehe() as i32,
            rgba: Vec::from(bild.rgba()),
        })
    }
}

impl Bildquelle for Bilder {
    fn bild(&self, quelle: &str) -> Option<Bild<'_>> {
        let geladen = self
            .geladen
            .iter()
            .find(|e| e.quelle == quelle)?
            .bild
            .as_ref()?;
        Some(Bild {
            breite: geladen.breite,
            hoehe: geladen.hoehe,
            rgba: &geladen.rgba,
        })
    }
}

// ===========================================================================
// DIE SEITE
// ===========================================================================

/// Alles, was zwischen zwei Layouts gleich bleibt.
///
/// Baum und Stile werden EINMAL gerechnet und ueberleben jede
/// Groessenaenderung — nur das Layout haengt an der Breite. Das ist der
/// Grund, warum ein Neu-Layout ueberhaupt bezahlbar ist: Parsen und
/// Kaskade sind der teurere Teil, und die fallen nicht noch einmal an.
struct Seite {
    dokument: Dokument,
    stile: StilBaum,
    liste: Anzeigeliste,
    hoehe: i32,
    /// Nur fuer die Ausgabe im Messmodus.
    befehle: usize,
    layout_ms: u64,
}

impl Seite {
    fn laden(bytes: &[u8]) -> Seite {
        let html = String::from_utf8_lossy(bytes);
        let dokument = speedhtml::parsen(&html);
        let standard = speedcss::standard_stylesheet();
        let autor = speedcss::autor_stylesheet(&dokument);
        let blaetter: Vec<(&Stylesheet, Herkunft)> = alloc::vec![
            (&standard, Herkunft::Standard),
            (&autor, Herkunft::Autor),
        ];
        let stile = speedcss::kaskade::berechnen(&dokument, &blaetter, Zustand::default());
        Seite {
            dokument,
            stile,
            liste: Anzeigeliste::default(),
            hoehe: 0,
            befehle: 0,
            layout_ms: 0,
        }
    }

    /// Das Dokument neu setzen. **Die einzige Stelle, an der das
    /// passiert** — wer sie im Scroll-Pfad aufruft, hat den Fehler
    /// gemacht, den `speedpaint::invalidierung` verhindern soll.
    fn setzen(&mut self, breite: i32) {
        let start = libspeed::zeit_jetzt();
        let ergebnis = speedlayout::setzen(&self.dokument, &self.stile, breite, &RasterMetrik);
        self.liste = speedlayout::anzeigeliste(&ergebnis);
        self.hoehe = ergebnis.hoehe;
        self.befehle = self.liste.len();
        self.layout_ms = libspeed::zeit_jetzt().saturating_sub(start);
    }

    /// Alle Bildquellen der Seite (fuer das Nachladen).
    fn bildquellen(&self) -> Vec<(String, Rechteck)> {
        let mut aus = Vec::new();
        for befehl in &self.liste.befehle {
            if let speedlayout::Befehl::Bild {
                quelle, rechteck, ..
            } = befehl
            {
                if !quelle.is_empty() {
                    aus.push((
                        quelle.clone(),
                        Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe),
                    ));
                }
            }
        }
        aus
    }
}

// ===========================================================================
// DAS PROGRAMM
// ===========================================================================

fn haupt(argumente: &Argumente) -> i32 {
    let mut quelle: Option<&str> = None;
    let mut feste_breite: Option<i32> = None;
    let mut messen: Option<u32> = None;
    let mut fenstergroesse = (START_BREITE, START_HOEHE);

    for i in 1..argumente.anzahl() {
        let Some(wort) = argumente.get(i) else {
            return FEHLER_BEDIENUNG;
        };
        match wort {
            _ if wort.starts_with("--breite=") => match wort["--breite=".len()..].parse::<i32>() {
                Ok(n) if (100..=20_000).contains(&n) => feste_breite = Some(n),
                _ => {
                    println!("--breite= braucht eine Zahl zwischen 100 und 20000.");
                    return FEHLER_BEDIENUNG;
                }
            },
            // `--fenster=BxH`: Die MESSUNG braucht ein Fenster in
            // Bildschirmgroesse (der Scroll-Frame haengt an der Flaeche),
            // und ein Ring-3-Programm kennt die Bildschirmgroesse nicht —
            // es gibt keinen Syscall dafuer. Der Test weiss sie und sagt
            // sie hier.
            _ if wort.starts_with("--fenster=") => {
                match masse_lesen(&wort["--fenster=".len()..]) {
                    Some(masse) => fenstergroesse = masse,
                    None => {
                        println!("--fenster= braucht BREITExHOEHE, z. B. 1280x700.");
                        return FEHLER_BEDIENUNG;
                    }
                }
            }
            _ if wort.starts_with("--messen=") => match wort["--messen=".len()..].parse::<u32>() {
                Ok(n) if (1..=10_000).contains(&n) => messen = Some(n),
                _ => {
                    println!("--messen= braucht eine Zahl zwischen 1 und 10000.");
                    return FEHLER_BEDIENUNG;
                }
            },
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

    // --- Die Bytes besorgen (wie htmldump und cssdump: Pfad oder Netz) ---
    let (bytes, herkunft, ordner) = if quelle.starts_with('/') {
        match libspeed::netz::datei_lesen(quelle) {
            Ok(b) => (b, String::from(quelle), ordner_von(quelle)),
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
                (abruf.antwort.rumpf, ziel, String::new())
            }
            Err(f) => {
                println!("Abruf fehlgeschlagen ({}): {}", f.kurz(), f.text());
                return FEHLER_ABRUF;
            }
        }
    };

    let mut seite = Seite::laden(&bytes);
    let mut bilder = Bilder::neu(ordner);

    let mut fenster = match Fenster::oeffnen(&titel_fuer(&herkunft), fenstergroesse.0, fenstergroesse.1)
    {
        Ok(f) => f,
        Err(f) => {
            println!("Fenster liess sich nicht oeffnen: {:?}", f);
            return FEHLER_BEDIENUNG;
        }
    };

    // Erstes Layout und erster Anstrich.
    let mut sicht = Sicht::neu(inhalt_bereich(&fenster), 0)
        .mit_zeilenhoehe(RasterMetrik.zeilen_hoehe(16).max(1));
    let mut layout_breite = feste_breite.unwrap_or_else(|| layout_breite_von(&fenster));
    seite.setzen(layout_breite);
    sicht.anpassen(inhalt_bereich(&fenster), seite.hoehe);
    bilder_nachladen(&seite, &mut bilder);
    voll_zeichnen(&mut fenster, &seite, &sicht, &bilder);

    if let Some(runden) = messen {
        return messmodus(
            &mut fenster,
            &mut seite,
            &mut sicht,
            &bilder,
            &herkunft,
            bytes.len(),
            runden,
        );
    }

    println!(
        "{} — {} Anzeige-Befehle, {} px hoch (Layout {} ms)",
        herkunft, seite.befehle, seite.hoehe, seite.layout_ms
    );

    // --- Die Ereignisschleife ---
    let mut balken_gefasst = false;
    // Ein Fehler aus `naechstes_ereignis` heisst: Das Fenster gibt es
    // nicht mehr. Dann ist die Schleife zu Ende — deshalb `while let`
    // und kein `loop` mit `match`.
    while let Ok(ereignis) = fenster.naechstes_ereignis(50) {
        // JEDES Ereignis wird in einen ANLASS uebersetzt, und die
        // Massnahme kommt aus `speedpaint::invalidierung`. Die
        // Ereignisschleife entscheidet NICHT selbst, was neu zu malen
        // ist — sonst stuenden die Regeln wieder verstreut in einer
        // if-Kette, und die Tests aus Aufgabe 3 prueften etwas anderes
        // als das, was hier laeuft.
        let anlass = match ereignis {
            Ereignis::Schliessen => break,
            Ereignis::Keins | Ereignis::Fokus(_) => continue,

            Ereignis::Groesse { breite, hoehe } => {
                let alt = sicht.bereich;
                fenster.groesse_uebernehmen(breite, hoehe);
                Anlass::FensterGroesse {
                    alte_breite: alt.breite,
                    alte_hoehe: alt.hoehe,
                    neue_breite: breite as i32,
                    neue_hoehe: hoehe as i32,
                }
            }

            Ereignis::MausRad { delta, .. } => scroll_anlass(&mut sicht, Scrollschritt::Rad(delta)),

            Ereignis::Sondertaste(taste) => match schritt_von_taste(taste) {
                Some(schritt) => scroll_anlass(&mut sicht, schritt),
                None => continue,
            },

            Ereignis::Taste(zeichen) => match zeichen {
                ' ' => scroll_anlass(&mut sicht, Scrollschritt::SeiteRunter),
                'q' => break,
                _ => continue,
            },

            Ereignis::MausAb { x, y, .. } => {
                // Auf dem Balken? Dann fassen und springen.
                match sicht.balken(BALKEN_BREITE) {
                    Some(balken) if x >= balken.spur.x => {
                        balken_gefasst = true;
                        let ziel = sicht.versatz_aus_balken(y, &balken);
                        scroll_anlass(&mut sicht, Scrollschritt::Nach(ziel))
                    }
                    _ => continue,
                }
            }
            Ereignis::MausBewegt { y, .. } if balken_gefasst => {
                match sicht.balken(BALKEN_BREITE) {
                    Some(balken) => {
                        let ziel = sicht.versatz_aus_balken(y, &balken);
                        scroll_anlass(&mut sicht, Scrollschritt::Nach(ziel))
                    }
                    None => continue,
                }
            }
            Ereignis::MausAuf { .. } => {
                balken_gefasst = false;
                continue;
            }
            Ereignis::MausBewegt { .. } => continue,
        };

        let massnahme = speedpaint::invalidierung::entscheiden(anlass);
        ausfuehren(
            massnahme,
            anlass,
            &mut fenster,
            &mut seite,
            &mut sicht,
            &mut bilder,
            &mut layout_breite,
            feste_breite,
        );
    }

    let _ = fenster.schliessen();
    OK
}

/// Einen Scroll-Schritt ausfuehren und daraus einen Anlass machen.
fn scroll_anlass(sicht: &mut Sicht, schritt: Scrollschritt) -> Anlass {
    let folge = sicht.scrollen(schritt);
    Anlass::Scrollen {
        streifen: folge.streifen,
        alles: folge.alles,
    }
}

/// Die Massnahme umsetzen.
///
/// Hier steht die einzige Stelle, an der die VERSCHIEBUNG passiert: Beim
/// Scrollen wird der schon gemalte Inhalt im eigenen Puffer bewegt, und
/// nur der neue Rand wird gemalt.
#[allow(clippy::too_many_arguments)]
fn ausfuehren(
    massnahme: Massnahme,
    anlass: Anlass,
    fenster: &mut Fenster,
    seite: &mut Seite,
    sicht: &mut Sicht,
    bilder: &mut Bilder,
    layout_breite: &mut i32,
    feste_breite: Option<i32>,
) {
    if !massnahme.malt() {
        return;
    }
    if massnahme.layoutet() {
        *layout_breite = feste_breite.unwrap_or_else(|| layout_breite_von(fenster));
        seite.setzen(*layout_breite);
        sicht.anpassen(inhalt_bereich(fenster), seite.hoehe);
        bilder_nachladen(seite, bilder);
        voll_zeichnen(fenster, seite, sicht, bilder);
        return;
    }
    // Nach einer Groessenaenderung stimmt der Bereich noch nicht.
    if matches!(anlass, Anlass::FensterGroesse { .. }) {
        sicht.anpassen(inhalt_bereich(fenster), seite.hoehe);
        voll_zeichnen(fenster, seite, sicht, bilder);
        return;
    }

    match (massnahme, anlass) {
        // DER SCHNELLPFAD: verschieben statt neu malen.
        (
            Massnahme::Teil(streifen),
            Anlass::Scrollen {
                alles: false,
                streifen: Some(_),
            },
        ) => {
            // Wie weit? Der Streifen liegt oben oder unten und ist so
            // hoch wie die Verschiebung.
            let dy = if streifen.y > sicht.bereich.y {
                streifen.hoehe
            } else {
                -streifen.hoehe
            };
            fenster.senkrecht_verschieben(dy);
            streifen_zeichnen(fenster, seite, sicht, bilder, streifen);
            balken_zeichnen(fenster, sicht);
            // Uebertragen werden MUSS trotzdem alles: Der Kernel hat
            // eine eigene Kopie und weiss von der Verschiebung nichts.
            let _ = fenster.zeigen();
        }
        (Massnahme::Teil(bereich), _) => {
            streifen_zeichnen(fenster, seite, sicht, bilder, bereich);
            balken_zeichnen(fenster, sicht);
            let _ = fenster.zeigen();
        }
        _ => voll_zeichnen(fenster, seite, sicht, bilder),
    }
}

// ===========================================================================
// ZEICHNEN
// ===========================================================================

fn voll_zeichnen(fenster: &mut Fenster, seite: &Seite, sicht: &Sicht, bilder: &Bilder) {
    streifen_zeichnen(fenster, seite, sicht, bilder, sicht.bereich);
    balken_zeichnen(fenster, sicht);
    let _ = fenster.zeigen();
}

fn streifen_zeichnen(
    fenster: &mut Fenster,
    seite: &Seite,
    sicht: &Sicht,
    bilder: &Bilder,
    streifen: Rechteck,
) {
    let mut leinwand = FensterLeinwand::neu(fenster);
    malen(
        &Auftrag {
            liste: &seite.liste,
            sicht,
            streifen,
            hintergrund: SEITE_HINTERGRUND,
        },
        &mut leinwand,
        bilder,
    );
}

fn balken_zeichnen(fenster: &mut Fenster, sicht: &Sicht) {
    let Some(balken) = sicht.balken(BALKEN_BREITE) else {
        return;
    };
    let mut leinwand = FensterLeinwand::neu(fenster);
    use speedui::Leinwand;
    leinwand.fuellen(balken.spur, BALKEN_SPUR);
    leinwand.fuellen(balken.greifer, BALKEN_GREIFER);
}

/// Alle Bilder der Seite laden, die noch fehlen.
fn bilder_nachladen(seite: &Seite, bilder: &mut Bilder) {
    for (quelle, _) in seite.bildquellen() {
        bilder.laden(&quelle);
    }
}

// ===========================================================================
// DER MESSMODUS
// ===========================================================================

/// Scroll-Frames messen und die Zahlen ausgeben.
///
/// ===================================================================
/// DIE METHODE (damit die Zahlen nachpruefbar sind)
///
/// Gemessen wird, was ein Scroll-Frame WIRKLICH kostet, aufgeteilt in
/// die zwei Posten, nach denen das Umstiegskriterium fragt
/// (docs/fenster-syscalls.md §5):
///
///   MALEN   — verschieben + den neuen Streifen malen (kein Syscall)
///   KOPIE   — `fenster_zeichnen`: der Puffer in den Kernel
///
/// UND ZUSAETZLICH der Vergleichsfall VOLL_MALEN (die ganze Flaeche
/// malen, ohne Verschiebe-Trick) — sonst waere nicht zu sehen, wie viel
/// der Streifen ueberhaupt bringt.
///
/// GEMITTELT UEBER VIELE SCHRITTE, weil `zeit_jetzt` nur Millisekunden
/// kann: Bei 40 Schritten a ~200 us sind das 8 ms je Durchgang.
///
/// UND DER BESTE VON MEHREREN DURCHGAENGEN, nicht der Mittelwert aller —
/// dieselbe Methodik wie in `messung` Modus 1, und hier aus demselben
/// Grund NOETIG: Der Scheduler nimmt uns alle 20 ms die CPU weg, und bei
/// 4K arbeitet der Compositor nebenher an derselben Flaeche. Diese
/// Fremdzeit gehoert nicht zum Scroll-Frame.
///
/// **Ohne diese Vorsichtsmassnahme ist die 4K-Messung nicht
/// entscheidungsfaehig**, und das ist keine Vermutung: Zwei Laeufe
/// derselben Fassung ergaben 7300 us und 9150 us — einmal unter, einmal
/// ueber der 8-ms-Schwelle des Umstiegskriteriums. Ein Kriterium, das je
/// nach Lauf anders ausfaellt, entscheidet nichts.
const DURCHGAENGE: u32 = 5;
#[allow(clippy::too_many_arguments)]
fn messmodus(
    fenster: &mut Fenster,
    seite: &mut Seite,
    sicht: &mut Sicht,
    bilder: &Bilder,
    herkunft: &str,
    bytes: usize,
    runden: u32,
) -> i32 {
    println!("QUELLE={}", herkunft);
    println!("BYTES={}", bytes);
    println!("FENSTER_BREITE={}", fenster.breite());
    println!("FENSTER_HOEHE={}", fenster.hoehe());
    println!("BEFEHLE={}", seite.befehle);
    println!("DOKUMENT_HOEHE={}", seite.hoehe);
    println!("LAYOUT_MS={}", seite.layout_ms);
    println!("RUNDEN={}", runden);

    // --- (1) Der Scroll-Frame: verschieben + Streifen + Kopie ---
    let je_durchgang = (runden / DURCHGAENGE).max(4);
    let mut malen_us = u64::MAX;
    let mut kopie_us = u64::MAX;
    let mut streifen_pixel = 0u64;
    let mut schritte = 0u64;
    let mut runter = true;

    for _ in 0..DURCHGAENGE {
        let mut malen_ms = 0u64;
        let mut kopie_ms = 0u64;
        let mut im_durchgang = 0u64;

        for _ in 0..je_durchgang {
            // Am Ende umkehren, damit wirklich gescrollt wird und nicht
            // gegen die Klemmung gelaufen — sonst maesse man ab der
            // Haelfte „nichts tun".
            if runter && sicht.versatz() >= sicht.max_versatz() {
                runter = false;
            } else if !runter && sicht.versatz() == 0 {
                runter = true;
            }
            // NEGATIV = nach unten (Rad-Konvention, siehe Scrollschritt).
            let folge = sicht.scrollen(Scrollschritt::Rad(if runter { -1 } else { 1 }));
            let Some(streifen) = folge.streifen else {
                continue;
            };
            im_durchgang += 1;
            streifen_pixel += (streifen.breite as u64) * (streifen.hoehe as u64);

            let t0 = libspeed::zeit_jetzt();
            if folge.verschieben_lohnt() {
                let dy = if streifen.y > sicht.bereich.y {
                    streifen.hoehe
                } else {
                    -streifen.hoehe
                };
                fenster.senkrecht_verschieben(dy);
            }
            streifen_zeichnen(fenster, seite, sicht, bilder, streifen);
            balken_zeichnen(fenster, sicht);
            let t1 = libspeed::zeit_jetzt();
            let _ = fenster.zeigen();
            let t2 = libspeed::zeit_jetzt();

            malen_ms += t1.saturating_sub(t0);
            kopie_ms += t2.saturating_sub(t1);
        }
        if im_durchgang == 0 {
            continue;
        }
        schritte += im_durchgang;
        // DER BESTE DURCHGANG, nicht der Mittelwert aller — die
        // Begruendung steht im Kopfkommentar dieser Funktion.
        malen_us = malen_us.min(malen_ms.saturating_mul(1000) / im_durchgang);
        kopie_us = kopie_us.min(kopie_ms.saturating_mul(1000) / im_durchgang);
    }

    // --- (2) Der Vergleichsfall: die ganze Flaeche malen ---
    let voll_je_durchgang = (je_durchgang / 4).max(4);
    let mut voll_us = u64::MAX;
    for _ in 0..DURCHGAENGE {
        let t0 = libspeed::zeit_jetzt();
        for _ in 0..voll_je_durchgang {
            streifen_zeichnen(fenster, seite, sicht, bilder, sicht.bereich);
        }
        let voll_ms = libspeed::zeit_jetzt().saturating_sub(t0);
        voll_us = voll_us.min(voll_ms.saturating_mul(1000) / voll_je_durchgang as u64);
    }

    let schritte = schritte.max(1);
    // `u64::MAX` heisst „kein einziger Durchgang hat gemessen" (ein
    // Dokument, das nicht scrollbar ist). Dann ist 0 die ehrliche Zahl.
    let malen_us = if malen_us == u64::MAX { 0 } else { malen_us };
    let kopie_us = if kopie_us == u64::MAX { 0 } else { kopie_us };
    let voll_us = if voll_us == u64::MAX { 0 } else { voll_us };

    // Die HEAP-SPITZE, nicht der Endstand — die Messfalle aus Serie 7,
    // Teil 3 (dort war der Endstand 16 Byte und die Spitze 65 KiB).
    let (belegt, gesamt, spitze) = libspeed::heap::heap_stand();
    println!("HEAP_BELEGT={}", belegt);
    println!("HEAP_GESAMT={}", gesamt);
    println!("HEAP_SPITZE={}", spitze);
    println!("DURCHGAENGE={}", DURCHGAENGE);
    println!("SCHRITTE={}", schritte);
    println!("STREIFEN_PIXEL={}", streifen_pixel / schritte);
    println!("MALEN_US={}", malen_us);
    println!("KOPIE_US={}", kopie_us);
    println!("FRAME_US={}", malen_us + kopie_us);
    println!("VOLL_MALEN_US={}", voll_us);
    let frame = (malen_us + kopie_us).max(1);
    println!("KOPIE_ANTEIL_PROZENT={}", kopie_us * 100 / frame);
    OK
}

// ===========================================================================
// KLEINKRAM
// ===========================================================================

/// Der Inhaltsbereich = das ganze Fenster. Der Scrollbalken liegt
/// DARUEBER, deshalb wird nur die LAYOUT-Breite um ihn verringert.
fn inhalt_bereich(fenster: &Fenster) -> Rechteck {
    Rechteck::neu(0, 0, fenster.breite() as i32, fenster.hoehe() as i32)
}

/// Die Breite, mit der gesetzt wird: Fensterbreite minus Balken.
fn layout_breite_von(fenster: &Fenster) -> i32 {
    (fenster.breite() as i32 - BALKEN_BREITE).max(50)
}

fn schritt_von_taste(taste: i32) -> Option<Scrollschritt> {
    use libspeed::fenster::*;
    match taste {
        SONDER_HOCH => Some(Scrollschritt::ZeileHoch),
        SONDER_RUNTER => Some(Scrollschritt::ZeileRunter),
        SONDER_BILD_HOCH => Some(Scrollschritt::SeiteHoch),
        SONDER_BILD_RUNTER => Some(Scrollschritt::SeiteRunter),
        SONDER_POS1 => Some(Scrollschritt::Anfang),
        SONDER_ENDE => Some(Scrollschritt::Ende),
        _ => None,
    }
}

/// „1280x700" -> (1280, 700). Unsinn wird abgelehnt, nicht geraten.
fn masse_lesen(text: &str) -> Option<(usize, usize)> {
    let (b, h) = text.split_once('x')?;
    let breite = b.trim().parse::<usize>().ok()?;
    let hoehe = h.trim().parse::<usize>().ok()?;
    if (50..=20_000).contains(&breite) && (50..=20_000).contains(&hoehe) {
        Some((breite, hoehe))
    } else {
        None
    }
}

/// Der Ordner eines Pfades — fuer relative Bildquellen.
fn ordner_von(pfad: &str) -> String {
    match pfad.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&pfad[..i]),
    }
}

/// Ein Fenstertitel aus der Herkunft: nur der letzte Teil, sonst passt
/// er nicht in die Titelleiste.
fn titel_fuer(herkunft: &str) -> String {
    let kurz = match herkunft.rfind('/') {
        Some(i) if i + 1 < herkunft.len() => &herkunft[i + 1..],
        _ => herkunft,
    };
    // EIN SCHLICHTER BINDESTRICH, kein Gedankenstrich: Die Titelleiste
    // zeichnet der Kernel mit seiner Latin-1-Schrift, und aus einem „—"
    // wird dort ein „?" (docs/grenzen.md, dieselbe Falle wie beim
    // Diagnose-Schirm).
    let mut aus = String::from("browser - ");
    aus.push_str(kurz);
    aus
}

fn hilfe(programm: &str) {
    println!("Aufruf: {} <datei|url> [--breite=N] [--messen=N]", programm);
    println!();
    println!("  starte browser /platte/seiten/cern.html &");
    println!("  starte browser https://example.com &");
    println!();
    println!("  Das & ist PFLICHT — sonst kommt der Compositor nicht dran.");
    println!("  Scrollen: Mausrad, Pfeile, Bild auf/ab, Pos1/Ende, Leertaste.");
    println!("  Beenden:  q oder der Schliessen-Knopf.");
}
