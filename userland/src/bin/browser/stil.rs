// browser::stil — die externen Stylesheets holen
//
// ===========================================================================
// WARUM DIESE DATEI DIE WICHTIGSTE DES SERIE-9-ANFANGS IST
//
// Der Realitaets-Bericht (docs/browser-realitaet.md) hat zehn echte Seiten
// bewertet und dabei eine Reihenfolge umgedreht, die alle erwartet hatten:
// **Nicht JavaScript ist die haeufigste Fehlerursache, sondern das fehlende
// externe Stylesheet.** Acht der zehn Seiten liefern ihren Inhalt brav im
// HTML — sie sehen nur falsch aus. Und schlimmer als „falsch" ist der
// zweite Teil: Was per CSS VERSTECKT sein soll (`display: none`), wird
// sichtbar. Der Screenreader-Text von github und das Tastatur-Overlay des
// Rust-Buchs stehen genau deshalb mitten auf der Seite.
//
// ===========================================================================
// WAS HIER PASSIERT — UND WAS AUSDRUECKLICH WOANDERS
//
//   speedcss::blaetter_einsammeln   WAS das Dokument braucht (Reihenfolge!)
//   Ort::aufloesen                  WOHIN die Adresse zeigt (speedhttp)
//   laden::nebensache_laden         WIE die Bytes kommen (libspeed::netz)
//   speedcss::parsen                WAS drinsteht
//
// Diese Datei VERDRAHTET das und trifft die Entscheidungen, die keine der
// vier Schichten treffen kann: wie viele, wie gross, wie lange, und was
// passiert, wenn es nicht klappt.
//
// ===========================================================================
// SERIELL, NICHT PARALLEL — die Entscheidung mit Begruendung
//
// Fuenf bis zehn Stylesheets je Seite sind der Normalfall, und jeder Abruf
// kostet bei uns einen vollen TLS-Handshake (12–36 ms, Serie 7, Teil 4).
// Seriell sind das also 60–360 ms; parallel waere es einer. Die Versuchung
// ist entsprechend gross. Trotzdem: **seriell.** Drei Gruende, der letzte
// ist der ausschlaggebende.
//
//   (1) `libspeed::netz::Klient` IST BLOCKIEREND GEBAUT (Serie 7, Teil 5).
//       Parallel hiesse: mehrere Sockets, mehrere `TlsStrom`-Zustands-
//       maschinen gleichzeitig treiben, eine eigene Auswahl-Schleife. Das
//       ist eine ZWEITE Abrufschicht neben der bestehenden — genau das,
//       wogegen die Regel „wer DNS, Socket, Handshake selbst schreibt,
//       macht es falsch" steht. Eine Ausnahme fuer Stylesheets waere der
//       Anfang vom Ende dieser Regel.
//
//   (2) DIE OBERFLAECHE GEWINNT NICHTS. Der Abruf blockiert den ganzen
//       Prozess — auch parallel stuende der Browser still, bis das letzte
//       Blatt da ist. Parallel waere die Wartezeit kuerzer, aber genauso
//       tot.
//
//   (3) DER GROESSERE HEBEL LIEGT WOANDERS, UND ER ZEIGT IN DIE ANDERE
//       RICHTUNG. Alle Blaetter einer Seite kommen fast immer vom SELBEN
//       Host. Mit HTTP/1.1-Keep-Alive kosten fuenf Blaetter EINEN
//       Handshake; parallel kosten sie FUENF, auf fuenf Sockets. Wer erst
//       parallelisiert, baut die Struktur, die Keep-Alive danach wieder
//       im Weg steht. Keep-Alive fehlt uns noch (`Connection: close`
//       steht fest in `anfrage_bauen`) — das ist der naechste Schritt,
//       nicht dieser.
//
// Gemessen wird es trotzdem: `browser --pruefen` gibt `STIL_MS` aus, und
// tests/browser.rs schreibt die Zahl mit. Wenn sie weh tut, steht sie da.

use crate::laden::{self, Cache};
use crate::ort::Ort;
use alloc::string::String;
use alloc::vec::Vec;
use libspeed::netz::Klient;
use speedcss::{Blattbedarf, Stylesheet};

// ===========================================================================
// DIE GRENZEN
// ===========================================================================

/// Wie viele EXTERNE Blaetter ein Dokument hoechstens bekommt.
///
/// Nachgezaehlt an den zehn Seiten des Realitaets-Berichts: Wikipedia 3,
/// das Rust-Buch 5, github 6, der Rest 1–2. Zehn laesst also Luft und
/// trifft nur, was ohnehin absurd ist. **Die Grenze schneidet ab, sie
/// lehnt nicht ab** — dieselbe Entscheidung wie bei den Knotengrenzen von
/// speedhtml: Eine Seite mit den ersten zehn Blaettern ist brauchbar, eine
/// Fehlermeldung nicht.
pub const MAX_BLAETTER: usize = 10;

/// Wie gross ein einzelnes Blatt sein darf.
///
/// Grosse Seiten liefern gebuendeltes CSS; githubs Hauptblatt ist rund
/// 300 KiB. 512 KiB laesst Luft, und mehr wuerde am Prozess-Heap (64 MiB)
/// zu knabbern anfangen, sobald mehrere Blaetter zusammenkommen.
pub const MAX_BLATT_BYTES: usize = 512 * 1024;

/// Wie viel CSS insgesamt.
///
/// Die WICHTIGERE der beiden Groessengrenzen, und zwar nicht wegen des
/// Speichers: Die Kaskade ist O(Regeln x Knoten) (docs/browser.md,
/// Serie-8-Abschluss — 51 Regeln x 5 796 Knoten = 11 ms). Zehn grosse
/// Blaetter sind schnell 20 000 Regeln, und das waere auf einem grossen
/// Dokument keine Wartezeit mehr, sondern ein Haenger.
pub const MAX_CSS_BYTES: usize = 1536 * 1024;

/// Frist je Abruf. Kuerzer als die einer Seite (15 s), weil ein Blatt
/// **nicht wichtig genug ist, um die Seite aufzuhalten**.
pub const FRIST_MS: u64 = 8_000;

/// Wie viele `@import`-Ziele insgesamt geholt werden.
pub const MAX_IMPORTE: usize = 4;

// ===========================================================================
// DAS ERGEBNIS
// ===========================================================================

/// Was beim Beschaffen der Stile herauskam.
#[derive(Default)]
pub struct Stilquellen {
    /// Die Autor-Blaetter **in Kaskaden-Reihenfolge**: externe und
    /// `<style>`-Bloecke in Dokumentreihenfolge, jedes `@import` VOR dem
    /// Blatt, das es angefordert hat.
    pub blaetter: Vec<Stylesheet>,
    pub extern_versucht: usize,
    pub extern_geladen: usize,
    pub extern_gescheitert: usize,
    /// Blaetter ueber `MAX_BLAETTER` oder ueber dem Byte-Budget.
    pub extern_uebersprungen: usize,
    pub importe_geladen: usize,
    /// `@import` in einem schon importierten Blatt (zweite Ebene), oder
    /// eines, das nicht ankam.
    pub importe_ignoriert: usize,
    /// Eine Adresse, die auf DIESER Seite schon einmal vorkam — als
    /// zweites `<link>` oder als `@import` auf ein Blatt, das wir haben.
    ///
    /// **Der Schleifenschutz, und er wird GEZAEHLT statt still zu
    /// wirken.** Ein Blatt zweimal zu holen waere nicht falsch (die
    /// Kaskade kaeme aufs selbe heraus), aber es waere ein zweiter
    /// Handshake fuer nichts — und bei sich gegenseitig anfordernden
    /// Blaettern beliebig viele.
    pub doppelt: usize,
    pub bytes: usize,
    pub regeln: usize,
    /// Kurztext fuer die Statuszeile — `None`, wenn alles geklappt hat.
    pub meldung: Option<String>,
}

/// Was beim Beschaffen herauskam — **ohne die Blaetter selbst.**
///
/// ===================================================================
/// WARUM DER TAB DIE BLAETTER NICHT BEHAELT
///
/// Nach der Kaskade sind sie tot: `StilBaum` traegt je Knoten den
/// fertigen Stil, und weder Layout noch Malen noch Scrollen fragen je
/// wieder eine Regel. Sie festzuhalten hiesse, auf jeder grossen Seite
/// ein paar hundert KiB Regeln liegen zu lassen — mal acht Tabs, bei
/// 64 MiB Prozess-Heap.
///
/// Behalten wird nur, was man SPAETER noch braucht: die Zahlen fuer die
/// Statuszeile und fuer `--pruefen`. (Sobald `:hover` wirklich gemeldet
/// wird, braucht es eine zweite Kaskade und damit die Blaetter — dann
/// ist das hier die Stelle, an der es auffaellt.)
#[derive(Default, Clone)]
pub struct StilBefund {
    pub inline_blaetter: usize,
    pub extern_versucht: usize,
    pub extern_geladen: usize,
    pub extern_gescheitert: usize,
    pub extern_uebersprungen: usize,
    pub importe_geladen: usize,
    pub importe_ignoriert: usize,
    pub doppelt: usize,
    pub bytes: usize,
    pub regeln: usize,
    pub meldung: Option<String>,
    /// Welche Eigenschaften die Seite anfordert, die wir nicht koennen —
    /// absteigend nach Haeufigkeit, gekuerzt auf die groessten Posten.
    ///
    /// **Das ist die Datengrundlage von Serie 9, Teil 2.** Sie steht hier
    /// und nicht in einem Extra-Werkzeug, weil sie sonst nur dann
    /// entstuende, wenn jemand eigens danach fragt — und dann misst man
    /// die Seite, die man ohnehin verdaechtigt. So faellt sie bei JEDEM
    /// `--pruefen` mit an, auch bei den zehn Seiten des
    /// Realitaets-Berichts.
    pub unbekannt: Vec<(String, usize)>,
    /// Deklarationen von CSS-Variablen (`--foo: …`). **Eine** Luecke
    /// (`var()`), nicht so viele, wie die Seite Variablen hat.
    pub variablen: usize,
    /// Deklarationen mit Herstellerpraefix — per Definition Dubletten.
    pub praefixe: usize,
}

impl Stilquellen {
    /// Die Zahlen, die den Kaskaden-Lauf ueberleben.
    pub fn befund(&self) -> StilBefund {
        // GEZAEHLT WIRD HIER, weil es der letzte Ort ist, an dem die
        // Blaetter noch leben — direkt danach werden sie fallengelassen.
        let blaetter: Vec<&Stylesheet> = self.blaetter.iter().collect();
        let bilanz = speedcss::unbekannte_eigenschaften(&blaetter);
        // GEKUERZT AUF 40, NICHT AUF 12. Die erste Fassung nahm zwoelf —
        // und schnitt damit auf jeder grossen Seite genau die Haelfte
        // weg, die man fuer eine Rangliste braucht. Eine Messung, die je
        // Seite nur die Spitze zeigt, summiert sich ueber zehn Seiten zu
        // einer Rangliste der SPITZEN, nicht der Haeufigkeiten.
        let mut unbekannt = bilanz.unbekannt;
        unbekannt.truncate(40);
        StilBefund {
            inline_blaetter: self.blaetter.len() - self.extern_geladen - self.importe_geladen,
            extern_versucht: self.extern_versucht,
            extern_geladen: self.extern_geladen,
            extern_gescheitert: self.extern_gescheitert,
            extern_uebersprungen: self.extern_uebersprungen,
            importe_geladen: self.importe_geladen,
            importe_ignoriert: self.importe_ignoriert,
            doppelt: self.doppelt,
            bytes: self.bytes,
            regeln: self.regeln,
            meldung: self.meldung.clone(),
            unbekannt,
            variablen: bilanz.variablen,
            praefixe: bilanz.praefixe,
        }
    }

    /// Die Blaetter in der Form, die `speedcss::kaskade::berechnen` will:
    /// erst das Standard-Blatt, dann die Autor-Blaetter in Reihenfolge.
    ///
    /// ===================================================================
    /// DIE REIHENFOLGE IST DIE HALBE AUFGABE
    ///
    ///     Standard  <  externe + <style> in Dokumentreihenfolge  <  style=""
    ///
    /// Das `style`-Attribut steht nicht in dieser Liste — es kommt in
    /// `kaskade::stil_fuer` mit einer kuenstlich hohen Spezifitaet dazu
    /// und schlaegt damit jedes Blatt, egal an welcher Stelle es steht.
    ///
    /// **Falsch einsortiert sieht es anders falsch aus als vorher, nicht
    /// besser.** Ein Standard-Blatt hinter dem Autor-Blatt hiesse: Jede
    /// Seite bekommt unsere Voreinstellungen aufgezwungen, und das ist
    /// schlimmer als gar kein CSS.
    pub fn kaskaden_blaetter<'a>(
        &'a self,
        standard: &'a Stylesheet,
    ) -> Vec<(&'a Stylesheet, speedcss::Herkunft)> {
        let mut aus: Vec<(&Stylesheet, speedcss::Herkunft)> =
            alloc::vec![(standard, speedcss::Herkunft::Standard)];
        for blatt in &self.blaetter {
            aus.push((blatt, speedcss::Herkunft::Autor));
        }
        aus
    }
}

// ===========================================================================
// BESCHAFFEN
// ===========================================================================

/// Nur die `<style>`-Bloecke — **ohne einen einzigen Abruf.**
///
/// ===================================================================
/// WOFUER ES DEN NETZFREIEN WEG BRAUCHT
///
/// Nicht jede Seite, die der Browser anzeigt, kommt von einem Server:
/// `speedos:info`, die Verlaufs- und die Lesezeichen-Seite, jede
/// Fehlerseite und der JavaScript-Hinweis sind HTML, das der Browser
/// SELBST erzeugt hat. Sie durch `autor_blaetter` zu schicken waere
/// nicht falsch (sie haben kein `<link>`), aber es waere eine
/// Netz-Operation ohne Anlass — und bei einer Fehlerseite ist die
/// Verbindung gerade nachweislich kaputt.
///
/// Deshalb gibt es beide Wege, und der Unterschied steht im NAMEN statt
/// in einem Schalter.
pub fn nur_inline(dokument: &speedhtml::Dokument) -> Stilquellen {
    let mut aus = Stilquellen::default();
    for eintrag in speedcss::blaetter_einsammeln(dokument) {
        if let Blattbedarf::Inline(css) = eintrag {
            if aus.bytes + css.len() > MAX_CSS_BYTES {
                continue;
            }
            aus.bytes += css.len();
            let blatt = speedcss::parsen(&css);
            aus.regeln += blatt.regeln.len();
            aus.blaetter.push(blatt);
        }
    }
    aus
}

/// Alle Autor-Stylesheets eines Dokuments beschaffen.
///
/// **Ein Blatt, das nicht laedt, darf die Seite nicht verhindern.** Es
/// gibt deshalb keinen Fehler-Rueckgabewert: Was ankam, wird benutzt, was
/// fehlt, wird GEZAEHLT und in der Statuszeile genannt. Genau dieselbe
/// Haltung wie im HTML-Parser — ein halbes Dokument ist lesbar, ein Fehler
/// zeigt nichts.
pub fn autor_blaetter(
    dokument: &speedhtml::Dokument,
    basis: &Ort,
    klient: &mut Klient,
    cache: &mut Cache,
) -> Stilquellen {
    let mut aus = Stilquellen::default();
    let bedarf = speedcss::blaetter_einsammeln(dokument);

    // Der Schleifenschutz. Er zaehlt fuer die GANZE Seite, nicht nur je
    // Kette: Zwei `<link>` auf dieselbe Datei sind kein Kreis, aber auch
    // kein Grund, sie zweimal zu holen und zweimal zu kaskadieren.
    let mut gesehen: Vec<String> = Vec::new();

    for eintrag in bedarf {
        match eintrag {
            Blattbedarf::Inline(css) => {
                // Ein `<style>`-Block kostet kein Netz, keine Frist und
                // keinen Cache — er zaehlt aber gegen das CSS-Budget, denn
                // die Kaskade sieht keinen Unterschied.
                if aus.bytes + css.len() > MAX_CSS_BYTES {
                    aus.extern_uebersprungen += 1;
                    continue;
                }
                aus.bytes += css.len();
                let blatt = speedcss::parsen(&css);
                aus.regeln += blatt.regeln.len();
                aus.blaetter.push(blatt);
            }

            Blattbedarf::Extern(href) => {
                // DIE REIHENFOLGE DER DREI PRUEFUNGEN IST NICHT BELIEBIG,
                // weil sie die Zahlen bestimmt, die hinterher jemand liest:
                //
                //   aufloesen  -> danach steht die ADRESSE fest, und ohne
                //                 sie kann man nicht auf „schon gehabt"
                //                 pruefen (zwei verschiedene `href` koennen
                //                 dieselbe Datei meinen).
                //   doppelt    -> VOR der Grenze, sonst frisst ein zehnfach
                //                 verlinktes Blatt das ganze Budget.
                //   Grenze     -> VOR `gesehen.push`, sonst zaehlte ein
                //                 spaeteres Duplikat als „doppelt" statt
                //                 als „uebersprungen".
                //
                // `extern_versucht` zaehlt danach, was WIRKLICH versucht
                // wurde — sonst stuende in der Statuszeile „1 von 3 nicht
                // geladen", wo zwei der drei dieselbe Datei waren.

                // (1) Aufloesen — mit DERSELBEN Funktion wie jeder Verweis
                // (Serie 8, Teil 8). `//cdn.example.com/a.css`, `/a.css`
                // und `../a.css` sind damit erledigt, ohne dass hier eine
                // Zeile URL-Logik steht.
                let Ok(verweis) = basis.aufloesen(&href) else {
                    aus.extern_versucht += 1;
                    aus.extern_gescheitert += 1;
                    continue;
                };
                let adresse = verweis.ort.als_text();
                if gesehen.iter().any(|a| a == &adresse) {
                    aus.doppelt += 1;
                    continue;
                }
                if aus.extern_versucht >= MAX_BLAETTER || aus.bytes >= MAX_CSS_BYTES {
                    aus.extern_uebersprungen += 1;
                    continue;
                }
                gesehen.push(adresse);
                aus.extern_versucht += 1;

                // (2) Holen und parsen.
                let Some(blatt) = blatt_holen(&verweis.ort, klient, cache, &mut aus) else {
                    aus.extern_gescheitert += 1;
                    continue;
                };
                aus.extern_geladen += 1;

                // (3) `@import` — EINE Ebene tief, siehe unten.
                importe_einziehen(&blatt, &verweis.ort, klient, cache, &mut gesehen, &mut aus);

                aus.regeln += blatt.regeln.len();
                aus.blaetter.push(blatt);
            }
        }
    }

    aus.meldung = meldung_bauen(&aus);
    aus
}

/// `@import` in einem geladenen Blatt: **eine Ebene tief, tiefer nicht.**
///
/// ===================================================================
/// WARUM EINE EBENE UND NICHT KEINE — UND WARUM NICHT ZWEI
///
/// **Warum ueberhaupt:** `@import` ist die aeltere der beiden Formen und
/// auf gewachsenen Seiten immer noch da — typischerweise als
/// `@import "reset.css"` ganz oben in der Hauptdatei. Wer sie ignoriert,
/// laedt bei genau diesen Seiten das Hauptblatt und laesst die
/// Grundlagen weg; das Ergebnis sieht kaputter aus als gar kein CSS,
/// weil die Regeln des Hauptblatts auf einem Fundament aufbauen, das
/// fehlt.
///
/// **Warum nicht tiefer:** Jede weitere Ebene ist ein weiterer
/// blockierender Abruf, den die Seite selbst bestimmt — und zwar
/// SERIELL, denn ein `@import` steht erst im Text, nachdem sein
/// Elternblatt da ist. Eine Kette aus zehn Dateien ist damit zehn
/// Handshakes lang, und der Benutzer sieht in dieser Zeit nichts. Eine
/// Ebene deckt den Normalfall ab; alles darunter ist eine Bauweise, die
/// auch echte Browser laengst abraten (deshalb gibt es `<link>`).
///
/// **Der Schleifenschutz ist trotzdem noetig, nicht statt dessen:** Zwei
/// Blaetter koennen einander importieren, und ohne die `gesehen`-Liste
/// waere `a.css` -> `b.css` -> `a.css` schon bei einer Ebene ein zweiter
/// Abruf derselben Datei. Bei mehreren `<link>`-Ketten auf derselben
/// Seite waeren es beliebig viele.
///
/// **Die Reihenfolge:** Ein Import wird VOR sein Elternblatt einsortiert
/// — so steht es in der Spezifikation, und es ist auch das einzig
/// Sinnvolle: `@import "reset.css"` will von der Datei ueberschrieben
/// werden, die ihn schreibt, nicht umgekehrt.
fn importe_einziehen(
    blatt: &Stylesheet,
    blatt_ort: &Ort,
    klient: &mut Klient,
    cache: &mut Cache,
    gesehen: &mut Vec<String>,
    aus: &mut Stilquellen,
) {
    for referenz in &blatt.importe {
        if aus.importe_geladen >= MAX_IMPORTE || aus.bytes >= MAX_CSS_BYTES {
            aus.importe_ignoriert += 1;
            continue;
        }
        // AUFGELOEST WIRD GEGEN DAS BLATT, NICHT GEGEN DIE SEITE. Ein
        // `@import "b.css"` in `/stil/a.css` meint `/stil/b.css` — auch
        // wenn die Seite unter `/artikel/42` liegt. Das ist derselbe
        // Verweis-Begriff wie ueberall, nur mit einer anderen Basis.
        let Ok(verweis) = blatt_ort.aufloesen(referenz) else {
            aus.importe_ignoriert += 1;
            continue;
        };
        let adresse = verweis.ort.als_text();
        if gesehen.iter().any(|a| a == &adresse) {
            aus.doppelt += 1;
            continue;
        }
        gesehen.push(adresse);

        let Some(importiert) = blatt_holen(&verweis.ort, klient, cache, aus) else {
            aus.importe_ignoriert += 1;
            continue;
        };
        // DIE ZWEITE EBENE WIRD GEZAEHLT UND NICHT VERFOLGT — sichtbar
        // in `--pruefen`, damit „warum fehlt da was?" eine Zahl hat.
        aus.importe_ignoriert += importiert.importe.len();
        aus.importe_geladen += 1;
        aus.regeln += importiert.regeln.len();
        aus.blaetter.push(importiert);
    }
}

/// Ein Blatt holen und parsen. `None` = nicht angekommen.
fn blatt_holen(
    ort: &Ort,
    klient: &mut Klient,
    cache: &mut Cache,
    aus: &mut Stilquellen,
) -> Option<Stylesheet> {
    let bytes = laden::nebensache_laden(ort, klient, cache, MAX_BLATT_BYTES, FRIST_MS)?;
    aus.bytes += bytes.len();
    // CSS ist Text. `from_utf8_lossy` statt einer Ablehnung: Ein einzelnes
    // kaputtes Byte in einer 300-KiB-Datei soll nicht das ganze Aussehen
    // kosten — dieselbe Haltung wie beim HTML-Parser.
    let text = String::from_utf8_lossy(&bytes);
    Some(speedcss::parsen(&text))
}

/// Der Satz fuer die Statuszeile — oder `None`, wenn nichts zu sagen ist.
///
/// **Ein Blatt, das fehlt, gehoert gesagt.** Ohne diese Meldung sieht
/// eine Seite mit halbem CSS genauso aus wie eine, die so aussehen soll,
/// und die Fehlersuche faengt bei null an.
fn meldung_bauen(aus: &Stilquellen) -> Option<String> {
    if aus.extern_gescheitert == 0 && aus.extern_uebersprungen == 0 {
        return None;
    }
    let mut text = String::new();
    if aus.extern_gescheitert > 0 {
        text.push_str(&alloc::format!(
            "{} von {} Stylesheets nicht geladen",
            aus.extern_gescheitert,
            aus.extern_versucht
        ));
    }
    if aus.extern_uebersprungen > 0 {
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(&alloc::format!(
            "{} uebersprungen (Grenze)",
            aus.extern_uebersprungen
        ));
    }
    Some(text)
}
