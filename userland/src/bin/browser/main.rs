// browser <adresse> — der SpeedOS-Browser (Serie 8, Teil 8)
//
// ===========================================================================
// WAS HIER STEHT, UND WAS NICHT
//
// Diese Datei ist die EREIGNISSCHLEIFE und sonst nichts. Sie uebersetzt
// Fenster-Ereignisse in Absichten, fragt `speedpaint::invalidierung`, was
// daraufhin zu tun ist, und tut es. Wer hier Parser-, Layout-, Netz- oder
// URL-Logik findet, hat einen Fehler gefunden — die liegt in:
//
//     speedhtml    Baum            speedcss     Stile
//     speedlayout  Kaesten/Befehle speedpaint   Malen/Scrollen/Invalidieren
//     speedhttp    URL-Aufloesung  libspeed     Fenster, Netz, Bilder, Heap
//     browser::*   Tab, Laden, Chrome, Orte, erzeugte Seiten
//
// ===========================================================================
// BEDIENUNG
//
//     starte browser &                          Startseite
//     starte browser https://example.com &      Adresse
//     starte browser /platte/seiten/cern.html & lokale Datei
//
//     Strg+T neuer Tab      Strg+W schliessen    Strg+L Adressleiste
//     Strg+H Verlauf        Strg+D Lesezeichen   F5 neu laden
//     Alt+Links/Rechts      zurueck/vor
//
// DAS `&` IST PFLICHT: Solange ein Shell-Befehl synchron laeuft, kommt kein
// anderer Kernel-Task dran — auch der Compositor nicht (Serie 8, Teil 1).

#![no_std]
#![no_main]

extern crate alloc;

mod chrome;
mod laden;
mod merkliste;
mod ort;
mod seiten;
mod tab;

use alloc::string::String;
use alloc::vec::Vec;
use chrome::{Aktion, Chrome, BrowserSchrift, BrowserThema, BrowserUhr};
use laden::Cache;
use libspeed::fenster::{Ereignis, Fenster};
use libspeed::leinwand::FensterLeinwand;
use libspeed::netz::Klient;
use libspeed::{println, Argumente};
use ort::{Intern, Ort};
use speedpaint::maler::Auftrag;
use speedpaint::{malen, Anlass, Massnahme, Scrollschritt, Sicht};
use speedui::{Farbe, Rechteck, Taste, UiKontext};
use tab::{Tab, TabZustand};

libspeed::hauptprogramm!(haupt);
libspeed::zufall_als_getrandom!();

const OK: i32 = 0;
const FEHLER_FENSTER: i32 = 3;

const START_BREITE: usize = 1000;
const START_HOEHE: usize = 680;
const BALKEN_BREITE: i32 = 12;
const SEITE_HINTERGRUND: Farbe = Farbe::neu(255, 255, 255);
const BALKEN_SPUR: Farbe = Farbe::mit_alpha(0, 0, 0, 30);
const BALKEN_GREIFER: Farbe = Farbe::neu(150, 150, 160);
/// Wie viel der Cache einer Sitzung halten darf.
const CACHE_BYTES: usize = 8 * 1024 * 1024;
/// Hoechstzahl der Tabs — mehr passen nicht in die Leiste.
const MAX_TABS: usize = 8;

// ===========================================================================
// DER BROWSER
// ===========================================================================

struct Browser {
    fenster: Fenster,
    chrome: Chrome,
    tabs: Vec<Tab>,
    aktiv: usize,
    klient: Klient,
    cache: Cache,
    merkliste: merkliste::Merkliste,
    /// Der Verweis unter dem Cursor (fuer die Statuszeile).
    unter_cursor: Option<String>,
    balken_gefasst: bool,
}

impl Browser {
    fn tab(&self) -> &Tab {
        &self.tabs[self.aktiv]
    }
    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.aktiv]
    }

    /// Der Bereich, in dem die SEITE liegt (unter dem Chrome).
    fn seiten_bereich(&self) -> Rechteck {
        Rechteck::neu(
            0,
            chrome::OBEN,
            self.fenster.breite() as i32,
            (self.fenster.hoehe() as i32 - chrome::OBEN).max(1),
        )
    }

    /// Die Breite, mit der gesetzt wird (Balken abgezogen).
    fn layout_breite(&self) -> i32 {
        (self.fenster.breite() as i32 - BALKEN_BREITE).max(50)
    }

    // -----------------------------------------------------------------
    // NAVIGATION
    // -----------------------------------------------------------------

    /// Eine Seite laden und anzeigen.
    ///
    /// `in_verlauf` ist `false`, wenn der Ort AUS dem Verlauf kommt —
    /// sonst wuerde jedes Zurueckgehen einen neuen Eintrag erzeugen und
    /// man kaeme nie irgendwo an.
    fn navigieren(&mut self, ziel: Ort, in_verlauf: bool, fragment: Option<String>) {
        // ERST die Ladeanzeige zeigen, DANN blockierend holen. Der Abruf
        // haelt den Prozess an (der Klient ist synchron), also muss das
        // Bild vorher stehen — sonst sieht der Benutzer die alte Seite
        // und haelt den Browser fuer eingefroren.
        self.tab_mut().zustand = TabZustand::Laedt;
        self.chrome.adresse_setzen(&ziel.als_text());
        self.chrome.meldung = Some(String::from("Laedt ..."));
        self.alles_zeichnen();

        let geladen = match ziel {
            // Verlauf und Lesezeichen brauchen Daten, die nur der Browser
            // hat — sie entstehen hier statt in `laden`.
            Ort::Intern(Intern::Verlauf) => {
                let liste = self.tab().verlauf_liste();
                let stelle = self.tab().stelle;
                laden::Geladen {
                    bytes: seiten::verlauf(&liste, stelle).into_bytes(),
                    ort: Ort::Intern(Intern::Verlauf),
                    fehler: None,
                    unsicher: false,
                }
            }
            Ort::Intern(Intern::Lesezeichen) => {
                let pfad = self.merkliste.pfad.clone();
                laden::Geladen {
                    bytes: seiten::lesezeichen(&self.merkliste.eintraege, &pfad).into_bytes(),
                    ort: Ort::Intern(Intern::Lesezeichen),
                    fehler: None,
                    unsicher: false,
                }
            }
            anderer => laden::seite_laden(&anderer, &mut self.klient, &mut self.cache),
        };

        let bereich = self.seiten_bereich();
        let breite = self.layout_breite();
        let endgueltig = geladen.ort.clone();
        {
            let tab = &mut self.tabs[self.aktiv];
            tab.inhalt_setzen(&geladen.bytes, endgueltig.clone(), geladen.fehler, geladen.unsicher);
            tab.setzen(breite, bereich);
        }

        // DER JAVASCRIPT-BEFUND: Wenn nichts dasteht und Skripte da sind,
        // sagen wir das — statt eine weisse Flaeche zu zeigen.
        if let Some(skripte) = self.tabs[self.aktiv].braucht_javascript() {
            let hinweis = seiten::braucht_javascript(&endgueltig.als_text(), skripte);
            let tab = &mut self.tabs[self.aktiv];
            // Der ORT bleibt der der echten Seite: Neu laden und
            // Lesezeichen sollen auf sie zeigen, nicht auf den Hinweis.
            tab.erzeugte_seite_setzen(&hinweis, endgueltig.clone());
            tab.setzen(breite, bereich);
            tab.titel = String::from("Braucht JavaScript");
            tab.js_hinweis = true;
        }

        if in_verlauf {
            let titel = self.tabs[self.aktiv].titel.clone();
            self.tabs[self.aktiv].verlauf_eintragen(endgueltig.clone(), &titel);
        }

        // Zum Anker springen, falls einer angegeben war.
        if let Some(fragment) = fragment {
            if let Some(y) = self.tabs[self.aktiv].fragment_y(&fragment) {
                self.tabs[self.aktiv].sicht.versatz_setzen(y);
            }
        }

        self.tabs[self.aktiv].bilder_einsammeln();
        self.chrome.adresse_setzen(&endgueltig.als_text());
        self.chrome.meldung = None;
        self.alles_zeichnen();
    }

    /// Einen Verweis verfolgen.
    fn verweis_folgen(&mut self, href: &str) {
        let basis = self.tab().ort.clone();
        match basis.aufloesen(href) {
            Ok(verweis) => {
                // NUR EIN ANKER: nicht laden, nur springen. Der haeufigste
                // Verweis auf einer langen Seite.
                if verweis.gleiche_seite {
                    if let Some(fragment) = verweis.fragment {
                        if let Some(y) = self.tabs[self.aktiv].fragment_y(&fragment) {
                            self.tabs[self.aktiv].sicht.versatz_setzen(y);
                            self.alles_zeichnen();
                        }
                    }
                    return;
                }
                self.navigieren(verweis.ort, true, verweis.fragment);
            }
            Err(fehler) => {
                // Kein Ladeversuch, keine Fehlerseite — nur ein Wort in
                // der Statuszeile. Ein `mailto:`-Link soll die Seite
                // nicht ersetzen.
                self.chrome.meldung = Some(String::from(fehler.text()));
                self.alles_zeichnen();
            }
        }
    }

    // -----------------------------------------------------------------
    // TABS
    // -----------------------------------------------------------------

    fn tab_neu(&mut self, ziel: Ort) {
        if self.tabs.len() >= MAX_TABS {
            self.chrome.meldung = Some(String::from("Mehr Tabs passen nicht in die Leiste."));
            self.alles_zeichnen();
            return;
        }
        self.tabs.push(Tab::neu());
        self.aktiv = self.tabs.len() - 1;
        self.tabs[self.aktiv].sicht = Sicht::neu(self.seiten_bereich(), 0);
        self.navigieren(ziel, true, None);
    }

    fn tab_schliessen(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return true;
        }
        // Der LETZTE Tab schliesst das Fenster — so verhaelt sich jeder
        // Browser, und ein Fenster ohne Tab waere ein Zustand ohne Sinn.
        if self.tabs.len() == 1 {
            return false;
        }
        self.tabs.remove(index);
        if self.aktiv >= self.tabs.len() {
            self.aktiv = self.tabs.len() - 1;
        } else if index < self.aktiv {
            self.aktiv -= 1;
        }
        self.nach_tab_wechsel();
        true
    }

    fn tab_waehlen(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.aktiv {
            self.aktiv = index;
            self.nach_tab_wechsel();
        }
    }

    /// Nach jedem Tab-Wechsel: Adressleiste und Sicht nachziehen.
    ///
    /// Die Sicht muss angepasst werden, weil das Fenster inzwischen eine
    /// andere Groesse haben kann als beim letzten Blick auf diesen Tab.
    fn nach_tab_wechsel(&mut self) {
        let bereich = self.seiten_bereich();
        let breite = self.layout_breite();
        let adresse = self.tab().ort.als_text();
        {
            let tab = &mut self.tabs[self.aktiv];
            let hoehe = tab.hoehe;
            tab.sicht.anpassen(bereich, hoehe);
            // Die Fensterbreite kann sich geaendert haben, waehrend
            // dieser Tab im Hintergrund lag.
            if tab.layout_breite != breite {
                tab.setzen(breite, bereich);
            }
        }
        self.chrome.adresse_setzen(&adresse);
        self.chrome.fokus_loesen();
        self.alles_zeichnen();
    }

    // -----------------------------------------------------------------
    // BILDER — asynchron, zwischen den Ereignissen
    // -----------------------------------------------------------------

    /// EIN offenes Bild holen. Liefert `true`, wenn etwas passiert ist.
    ///
    /// ===================================================================
    /// WARUM EINES UND NICHT ALLE
    ///
    /// Der Abruf blockiert. Wer in einer Runde alle Bilder holt, friert
    /// die Oberflaeche fuer die Summe aller Abrufe ein — bei zehn Bildern
    /// sind das Sekunden ohne Reaktion. Eines je Durchgang heisst:
    /// zwischen zwei Bildern ist der Browser bedienbar, und die Bilder
    /// erscheinen NACHEINANDER auf der Seite. Das ist genau das
    /// Verhalten, das man von einem Browser kennt.
    fn ein_bild_laden(&mut self) -> bool {
        let Some(quelle) = self.tabs[self.aktiv].offene_bilder.first().cloned() else {
            return false;
        };
        self.tabs[self.aktiv].offene_bilder.remove(0);

        let basis = self.tab().ort.clone();
        let bild_ort = match basis.aufloesen(&quelle) {
            Ok(verweis) => verweis.ort,
            Err(_) => {
                // Nicht aufloesbar: als gescheitert vermerken, damit es
                // nicht erneut versucht wird.
                self.tabs[self.aktiv].bilder.bilder.push(tab::Bild {
                    quelle,
                    daten: None,
                });
                return true;
            }
        };

        let bytes = laden::nebensache_laden(&bild_ort, &mut self.klient, &mut self.cache);
        let daten = bytes.and_then(|b| libspeed::bild::dekodieren(&b).ok()).map(|bild| {
            tab::BildDaten {
                breite: bild.breite() as i32,
                hoehe: bild.hoehe() as i32,
                rgba: Vec::from(bild.rgba()),
            }
        });
        let hat_geklappt = daten.is_some();
        let aendert = hat_geklappt && self.tabs[self.aktiv].bild_aendert_masse(&quelle);
        let bereich = self.tabs[self.aktiv]
            .bild_bereich(&quelle)
            .unwrap_or(Rechteck::neu(0, 0, 0, 0));
        self.tabs[self.aktiv].bilder.bilder.push(tab::Bild {
            quelle,
            daten,
        });

        if !hat_geklappt {
            return true;
        }

        // DIE INVALIDIERUNGS-REGEL ENTSCHEIDET, nicht diese Funktion.
        let massnahme = speedpaint::invalidierung::entscheiden(Anlass::BildGeladen {
            bereich,
            aendert_masse: aendert,
        });
        self.massnahme_ausfuehren(massnahme);
        true
    }

    // -----------------------------------------------------------------
    // ZEICHNEN
    // -----------------------------------------------------------------

    fn kontext<'a>(
        thema: &'a BrowserThema,
        schrift: &'a BrowserSchrift,
        uhr: &'a BrowserUhr,
    ) -> UiKontext<'a> {
        UiKontext::neu(thema, schrift, uhr)
    }

    fn alles_zeichnen(&mut self) {
        self.seite_zeichnen(self.seiten_bereich());
        self.chrome_zeichnen();
        let _ = self.fenster.zeigen();
    }

    fn seite_zeichnen(&mut self, streifen: Rechteck) {
        let sicht = self.tabs[self.aktiv].sicht;
        let liste = &self.tabs[self.aktiv].liste;
        let bilder = &self.tabs[self.aktiv].bilder;
        {
            let mut leinwand = FensterLeinwand::neu(&mut self.fenster);
            malen(
                &Auftrag {
                    liste,
                    sicht: &sicht,
                    streifen,
                    hintergrund: SEITE_HINTERGRUND,
                },
                &mut leinwand,
                bilder,
            );
        }
        self.balken_zeichnen();
    }

    fn balken_zeichnen(&mut self) {
        let Some(balken) = self.tabs[self.aktiv].sicht.balken(BALKEN_BREITE) else {
            return;
        };
        use speedui::Leinwand;
        let mut leinwand = FensterLeinwand::neu(&mut self.fenster);
        leinwand.fuellen(balken.spur, BALKEN_SPUR);
        leinwand.fuellen(balken.greifer, BALKEN_GREIFER);
    }

    fn chrome_zeichnen(&mut self) {
        let thema = BrowserThema;
        let schrift = BrowserSchrift;
        let uhr = BrowserUhr;
        let k = Self::kontext(&thema, &schrift, &uhr);
        let aktiv = self.aktiv;
        let kann_zurueck = self.tab().kann_zurueck();
        let kann_vor = self.tab().kann_vor();
        let gemerkt = self.merkliste.kennt(&self.tab().ort.als_text());
        let hoehe = self.fenster.hoehe() as i32;
        // Der Chrome braucht `&self.tabs` UND `&mut self.fenster` — die
        // Leinwand wird deshalb in einem eigenen Block gehalten.
        let chrome = &self.chrome;
        let tabs = &self.tabs;
        let mut leinwand = FensterLeinwand::neu(&mut self.fenster);
        chrome.zeichnen(&mut leinwand, &k, tabs, aktiv, kann_zurueck, kann_vor, gemerkt);
        chrome.status_zeichnen(&mut leinwand, &k, hoehe);
    }

    /// Eine Massnahme aus `speedpaint::invalidierung` umsetzen.
    fn massnahme_ausfuehren(&mut self, massnahme: Massnahme) {
        if !massnahme.malt() {
            return;
        }
        if massnahme.layoutet() {
            let bereich = self.seiten_bereich();
            let breite = self.layout_breite();
            self.tabs[self.aktiv].setzen(breite, bereich);
            self.tabs[self.aktiv].bilder_einsammeln();
            self.alles_zeichnen();
            return;
        }
        match massnahme.ausschnitt() {
            Some(teil) => {
                // Der Ausschnitt darf nie in den Chrome hineinreichen.
                let Some(sicher) = teil.schneiden(&self.seiten_bereich()) else {
                    return;
                };
                self.seite_zeichnen(sicher);
                let _ = self.fenster.zeigen();
            }
            None => self.alles_zeichnen(),
        }
    }

    /// Nach einem Scroll-Schritt: verschieben und den Streifen malen.
    fn scrollen(&mut self, schritt: Scrollschritt) {
        let folge = self.tabs[self.aktiv].sicht.scrollen(schritt);
        let anlass = Anlass::Scrollen {
            streifen: folge.streifen,
            alles: folge.alles,
        };
        if !folge.geaendert() {
            return;
        }
        if folge.verschieben_lohnt() {
            let bereich = self.seiten_bereich();
            let dy = folge.verschiebung;
            // NUR DAS SEITENBAND verschieben. Der Chrome steht still und
            // wird deshalb auch nicht neu gezeichnet — das spart bei 4K
            // gemessen 2 200 us JE FRAME (die Begruendung steht bei
            // `Fenster::senkrecht_verschieben_bereich`).
            self.fenster.senkrecht_verschieben_bereich(
                bereich.y.max(0) as usize,
                bereich.hoehe.max(0) as usize,
                dy,
            );
            let streifen = folge.streifen.unwrap_or(bereich);
            if let Some(sicher) = streifen.schneiden(&bereich) {
                self.seite_zeichnen(sicher);
            }
            // NUR DAS SEITENBAND uebertragen: Der Chrome hat sich nicht
            // geaendert, also muss der Kernel ihn auch nicht erneut
            // bekommen.
            let _ = self.fenster.zeigen_bereich(
                0,
                bereich.y.max(0) as usize,
                bereich.breite.max(0) as usize,
                bereich.hoehe.max(0) as usize,
            );
            return;
        }
        let massnahme = speedpaint::invalidierung::entscheiden(anlass);
        self.massnahme_ausfuehren(massnahme);
    }
}

// ===========================================================================
// DAS PROGRAMM
// ===========================================================================

fn haupt(argumente: &Argumente) -> i32 {
    let mut start: Option<String> = None;
    let mut messen: Option<u32> = None;
    let mut pruefen = false;
    let mut zyklus = false;
    let mut phasen = false;
    let mut fenstergroesse = (START_BREITE, START_HOEHE);
    for i in 1..argumente.anzahl() {
        let Some(wort) = argumente.get(i) else { continue };
        if wort.starts_with("--") {
            if wort == "--hilfe" {
                hilfe(argumente.programm());
                return OK;
            }
            if let Some(zahl) = wort.strip_prefix("--messen=") {
                messen = zahl.parse::<u32>().ok().filter(|n| (1..=10_000).contains(n));
            }
            if wort == "--pruefen" {
                pruefen = true;
            }
            if wort == "--zyklus" {
                zyklus = true;
            }
            if wort == "--phasen" {
                phasen = true;
            }
            if let Some(masse) = wort.strip_prefix("--fenster=") {
                if let Some(gelesen) = masse_lesen(masse) {
                    fenstergroesse = gelesen;
                }
            }
            continue;
        }
        if start.is_none() {
            start = Some(String::from(wort));
        }
    }

    let fenster = match Fenster::oeffnen("browser", fenstergroesse.0, fenstergroesse.1) {
        Ok(f) => f,
        Err(fehler) => {
            println!("Fenster liess sich nicht oeffnen: {}", fehler.text());
            if fehler == libspeed::Fehler::NICHT_KONFIGURIERT {
                println!("Es laeuft kein Desktop — mit dem Befehl 'desktop' starten.");
            }
            return FEHLER_FENSTER;
        }
    };

    let merkliste = merkliste::Merkliste::laden();
    let breite = fenster.breite() as i32;
    let mut browser = Browser {
        fenster,
        chrome: Chrome::neu(breite),
        tabs: alloc::vec![Tab::neu()],
        aktiv: 0,
        klient: Klient::neu(),
        cache: Cache::neu(CACHE_BYTES),
        merkliste,
        unter_cursor: None,
        balken_gefasst: false,
    };
    browser.tabs[0].sicht = Sicht::neu(browser.seiten_bereich(), 0);

    // Startziel: Argument, sonst die konfigurierte Startseite.
    let ziel_text = start.unwrap_or_else(|| browser.merkliste.startseite.clone());
    let ziel = ort::Ort::parsen(&ziel_text).unwrap_or(Ort::Intern(Intern::Info));
    browser.navigieren(ziel, true, None);

    if phasen {
        phasenmodus(&mut browser, &ziel_text);
        let _ = browser.fenster.schliessen();
        return OK;
    }

    if zyklus {
        zyklusmodus(&mut browser);
        let _ = browser.fenster.schliessen();
        return OK;
    }

    if pruefen {
        pruefmodus(&browser);
        let _ = browser.fenster.schliessen();
        return OK;
    }

    if let Some(runden) = messen {
        messmodus(&mut browser, &ziel_text, runden);
        let _ = browser.fenster.schliessen();
        return OK;
    }

    ereignisschleife(&mut browser);
    let _ = browser.fenster.schliessen();
    OK
}

// ===========================================================================
// DER PHASEN-MODUS — wohin die Ladezeit geht
// ===========================================================================

/// `--phasen`: eine Seite laden und die Zeit auf die Stufen aufteilen.
///
/// ===================================================================
/// WARUM DIE STUFEN EINZELN GEMESSEN WERDEN
///
/// „Die Seite braucht 300 ms" ist keine Auskunft, sondern der Anfang
/// einer Suche. Erst die Aufteilung sagt, WO die Zeit bleibt — und ob
/// sich Optimieren ueberhaupt lohnt. Bei einem Browser sind die
/// Kandidaten sehr verschieden teuer:
///
///   NETZ    DNS + TCP + TLS-Handshake + Uebertragung (bei uns aus
///           Serie 7 bekannt: Handshake 11-36 ms)
///   HTML    Tokenizer + Baum
///   CSS     Parsen + Kaskade (jede Regel gegen jeden Knoten)
///   LAYOUT  Kaesten, Zeilenumbruch, Anzeige-Befehle
///   MALEN   Befehle -> Pixel
///   KOPIE   `fenster_zeichnen` (die Naht zum Kernel)
///
/// GEMESSEN WIRD MEHRFACH UND DER BESTWERT GENOMMEN — dieselbe Methodik
/// wie in `messung` Modus 1 und im Scroll-Frame: Der Scheduler nimmt
/// alle 20 ms die CPU weg, und diese Fremdzeit gehoert in keine Stufe.
/// Die Uhr kann nur Millisekunden; die kleinen Stufen werden deshalb
/// WIEDERHOLT und geteilt.
fn phasenmodus(b: &mut Browser, quelle: &str) {
    const RUNDEN: u32 = 5;

    println!("QUELLE={}", quelle);
    println!("RUNDEN={}", RUNDEN);

    let ort = match ort::Ort::parsen(quelle) {
        Ok(o) => o,
        Err(_) => {
            println!("FEHLER=adresse");
            return;
        }
    };

    // --- (1) NETZ: holen. Beim ERSTEN Mal wirklich, danach aus dem
    // Cache — deshalb zaehlt hier nur der erste Durchgang.
    let t0 = libspeed::zeit_jetzt();
    let geladen = laden::seite_laden(&ort, &mut b.klient, &mut b.cache);
    let netz_ms = libspeed::zeit_jetzt().saturating_sub(t0);
    let bytes = geladen.bytes.len();
    let html = String::from_utf8_lossy(&geladen.bytes);

    // --- (2) HTML parsen ---
    let mut html_ms = u64::MAX;
    let mut dokument = speedhtml::parsen("");
    for _ in 0..RUNDEN {
        let t = libspeed::zeit_jetzt();
        dokument = speedhtml::parsen(&html);
        html_ms = html_ms.min(libspeed::zeit_jetzt().saturating_sub(t));
    }
    let knoten = dokument.anzahl();

    // --- (3) CSS. AUFGETEILT, weil es zwei verschiedene Probleme sind:
    // Stylesheets PARSEN (Textarbeit, haengt an der CSS-Menge) und
    // KASKADIEREN (jede Regel gegen jeden Knoten, haengt am Produkt).
    // Wer nur die Summe kennt, optimiert die falsche Haelfte.
    let mut css_standard_ms = u64::MAX;
    let mut css_autor_ms = u64::MAX;
    let mut css_kaskade_ms = u64::MAX;
    let mut stile = tab::kaskadieren(&dokument);
    let mut regeln_standard = 0usize;
    let mut regeln_autor = 0usize;
    for _ in 0..RUNDEN {
        let t = libspeed::zeit_jetzt();
        let standard = speedcss::standard_stylesheet();
        css_standard_ms = css_standard_ms.min(libspeed::zeit_jetzt().saturating_sub(t));

        let t = libspeed::zeit_jetzt();
        let autor = speedcss::autor_stylesheet(&dokument);
        css_autor_ms = css_autor_ms.min(libspeed::zeit_jetzt().saturating_sub(t));

        regeln_standard = standard.regeln.len();
        regeln_autor = autor.regeln.len();
        let blaetter: Vec<(&speedcss::Stylesheet, speedcss::Herkunft)> = alloc::vec![
            (&standard, speedcss::Herkunft::Standard),
            (&autor, speedcss::Herkunft::Autor),
        ];
        let t = libspeed::zeit_jetzt();
        stile = speedcss::kaskade::berechnen(&dokument, &blaetter, speedcss::Zustand::default());
        css_kaskade_ms = css_kaskade_ms.min(libspeed::zeit_jetzt().saturating_sub(t));
    }
    let css_ms = wert(css_standard_ms) + wert(css_autor_ms) + wert(css_kaskade_ms);

    // --- (4) LAYOUT ---
    let bereich = b.seiten_bereich();
    let breite = b.layout_breite();
    let leere_bilder = tab::BildSammlung::default();
    let metrik = tab::SeitenMetrik {
        bilder: &leere_bilder,
    };
    let mut layout_ms = u64::MAX;
    let mut befehle = 0usize;
    let mut hoehe = 0i32;
    for _ in 0..RUNDEN {
        let t = libspeed::zeit_jetzt();
        let ergebnis = speedlayout::setzen(&dokument, &stile, breite, &metrik);
        let liste = speedlayout::anzeigeliste(&ergebnis);
        layout_ms = layout_ms.min(libspeed::zeit_jetzt().saturating_sub(t));
        befehle = liste.len();
        hoehe = ergebnis.hoehe;
    }

    // --- (5) MALEN und (6) KOPIE, am fertigen Tab ---
    {
        let tab = &mut b.tabs[b.aktiv];
        tab.inhalt_setzen(&geladen.bytes, ort.clone(), geladen.fehler, geladen.unsicher);
        tab.setzen(breite, bereich);
    }
    let mut malen_ms = u64::MAX;
    let mut kopie_ms = u64::MAX;
    for _ in 0..RUNDEN {
        let t = libspeed::zeit_jetzt();
        b.seite_zeichnen(bereich);
        malen_ms = malen_ms.min(libspeed::zeit_jetzt().saturating_sub(t));
        let t = libspeed::zeit_jetzt();
        let _ = b.fenster.zeigen();
        kopie_ms = kopie_ms.min(libspeed::zeit_jetzt().saturating_sub(t));
    }

    let (belegt, _, spitze) = libspeed::heap::heap_stand();
    println!("BYTES={}", bytes);
    println!("KNOTEN={}", knoten);
    println!("BEFEHLE={}", befehle);
    println!("DOKUMENT_HOEHE={}", hoehe);
    println!("SEITE_BREITE={}", bereich.breite);
    println!("SEITE_HOEHE={}", bereich.hoehe);
    println!("NETZ_MS={}", netz_ms);
    println!("HTML_MS={}", wert(html_ms));
    println!("CSS_MS={}", css_ms);
    println!("CSS_STANDARD_MS={}", wert(css_standard_ms));
    println!("CSS_AUTOR_MS={}", wert(css_autor_ms));
    println!("CSS_KASKADE_MS={}", wert(css_kaskade_ms));
    println!("REGELN_STANDARD={}", regeln_standard);
    println!("REGELN_AUTOR={}", regeln_autor);
    println!("LAYOUT_MS={}", wert(layout_ms));
    println!("MALEN_MS={}", wert(malen_ms));
    println!("KOPIE_MS={}", wert(kopie_ms));
    println!("HEAP_BELEGT={}", belegt);
    println!("HEAP_SPITZE={}", spitze);
}

fn wert(x: u64) -> u64 {
    if x == u64::MAX {
        0
    } else {
        x
    }
}

// ===========================================================================
// DER ZYKLUS-MODUS — fuer den Speicher-Pass
// ===========================================================================

/// `--zyklus`: einmal alles tun, was ein Benutzer in einer Sitzung tut,
/// und dann sauber enden.
///
/// ===================================================================
/// WOZU EIN EIGENER MODUS UND NICHT EINFACH `--pruefen` MEHRMALS
///
/// Der Speicher-Pass fragt nicht „leckt EIN Ladevorgang?", sondern
/// „leckt eine SITZUNG?". Das ist etwas anderes: Tabs entstehen und
/// vergehen, der Verlauf waechst, der Cache fuellt sich, Bilder werden
/// geladen, Fenster-Handles kommen und gehen. Ein Modus, der nur eine
/// Seite laedt, wuerde genau die Dinge nicht anfassen, an denen ein
/// Browser leckt.
///
/// Was hier passiert (in dieser Reihenfolge, weil sie sich gegenseitig
/// stoert): fuenf Seiten laden, drei Tabs oeffnen, in ihnen laden,
/// zwischen ihnen wechseln, zwei wieder schliessen, zurueck- und
/// vorgehen. Danach endet der Prozess — und der Kernel muss ALLES
/// zurueckbekommen.
fn zyklusmodus(b: &mut Browser) {
    let seiten = [
        "speedos:info",
        "speedos:lesezeichen",
        "/platte/seiten/cern.html",
        "/seiten/cern.html",
        "/gibt-es-nicht-4711.html",
    ];

    // (1) Fuenf Seiten im ersten Tab — inklusive einer, die es nicht gibt
    // (die Fehlerseite ist auch ein Dokument und alloziert auch).
    for adresse in seiten {
        if let Ok(ziel) = ort::Ort::parsen(adresse) {
            b.navigieren(ziel, true, None);
        }
    }

    // (2) Drei Tabs auf, in jedem laden.
    for adresse in ["speedos:info", "speedos:verlauf", "speedos:lesezeichen"] {
        if let Ok(ziel) = ort::Ort::parsen(adresse) {
            b.tab_neu(ziel);
        }
    }

    // (3) Zwischen ihnen wechseln (das laeuft ueber `nach_tab_wechsel`,
    // das bei geaenderter Breite neu setzt).
    for i in 0..b.tabs.len() {
        b.tab_waehlen(i);
    }

    // (4) Zurueck und vor — der Verlauf wird wirklich benutzt.
    for _ in 0..3 {
        if let Some(ziel) = b.tabs[b.aktiv].zurueck() {
            b.navigieren(ziel, false, None);
        }
    }
    for _ in 0..2 {
        if let Some(ziel) = b.tabs[b.aktiv].vor() {
            b.navigieren(ziel, false, None);
        }
    }

    // (5) Zwei Tabs wieder zu.
    for _ in 0..2 {
        if b.tabs.len() > 1 {
            let letzter = b.tabs.len() - 1;
            b.tab_schliessen(letzter);
        }
    }

    let (belegt, gesamt, spitze) = libspeed::heap::heap_stand();
    println!("ZYKLUS=fertig");
    println!("TABS={}", b.tabs.len());
    println!("VERLAUF={}", b.tabs[b.aktiv].verlauf.len());
    println!("HEAP_BELEGT={}", belegt);
    println!("HEAP_GESAMT={}", gesamt);
    println!("HEAP_SPITZE={}", spitze);
}

// ===========================================================================
// DER PRUEFMODUS — der vierte Debug-Blick
// ===========================================================================

/// `--pruefen`: laedt eine Seite und gibt maschinenlesbar aus, was dabei
/// herauskam.
///
/// ===================================================================
/// WOZU, NEBEN htmldump UND cssdump
///
/// `htmldump` beantwortet „Parser oder Layout?", `cssdump` „welche Regel
/// hat das gesetzt?", `cssdump --layout` „was kommt heraus?". Diese
/// Ausgabe beantwortet die vierte Frage, die erst mit einem BROWSER
/// entsteht: **„Was haette der Benutzer gesehen?"** — also Titel,
/// Fehlerzustand, Sicherheitsbefund, JavaScript-Diagnose und die
/// aufgeloesten Verweise.
///
/// Genau diese Dinge lassen sich sonst nur fotografieren. Als Zahlen
/// sind sie ein Testfall (tests/browser.rs).
fn pruefmodus(b: &Browser) {
    let tab = &b.tabs[b.aktiv];
    println!("ORT={}", tab.ort.als_text());
    println!("TITEL={}", tab.titel);
    println!("BEFEHLE={}", tab.liste.len());
    println!("HOEHE={}", tab.hoehe);
    println!("ZUSTAND={}", match tab.zustand {
        TabZustand::Leer => "leer",
        TabZustand::Laedt => "laedt",
        TabZustand::Fertig => "fertig",
        TabZustand::Fehler => "fehler",
    });
    println!("UNSICHER={}", if tab.unsicher { 1 } else { 0 });
    println!(
        "FEHLER={}",
        tab.fehler.as_deref().unwrap_or("-")
    );
    println!("JS_HINWEIS={}", if tab.js_hinweis { 1 } else { 0 });
    println!("VERLAUF={}", tab.verlauf.len());
    println!("LESEZEICHEN={}", b.merkliste.eintraege.len());
    println!("LESEZEICHEN_PFAD={}", b.merkliste.pfad);
    println!("STARTSEITE={}", b.merkliste.startseite);

    // Die Verweise der Seite — AUFGELOEST. Das ist die Auskunft, die
    // sonst nur ein Klick gibt.
    let mut gezeigt = 0usize;
    for (_, knoten) in tab.dokument.alle() {
        if knoten.name() != Some("a") {
            continue;
        }
        let Some(href) = knoten.attribut("href") else { continue };
        if href.trim().is_empty() {
            continue;
        }
        match tab.ort.aufloesen(href) {
            Ok(v) => println!("LINK={} -> {}", href, v.ort.als_text()),
            Err(f) => println!("LINK={} -> FEHLER:{}", href, kurzfehler(f)),
        }
        gezeigt += 1;
        if gezeigt >= 12 {
            break;
        }
    }
    println!("LINKS={}", gezeigt);
    let (_, _, spitze) = libspeed::heap::heap_stand();
    println!("HEAP_SPITZE={}", spitze);
}

fn kurzfehler(fehler: ort::OrtFehler) -> &'static str {
    match fehler {
        ort::OrtFehler::Ungueltig => "ungueltig",
        ort::OrtFehler::NichtNavigierbar => "nicht-navigierbar",
        ort::OrtFehler::UnbekannteSeite => "unbekannte-seite",
    }
}

/// „1280x700" -> (1280, 700).
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

// ===========================================================================
// DER MESSMODUS (Serie 8, Teil 7 — hier UNVERAENDERT weitergefuehrt)
// ===========================================================================

/// Wie viele Durchgaenge gemessen werden; gewertet wird der BESTE.
///
/// Die Begruendung steht in docs/browser-rendern.md §4 und ist die Lehre
/// aus Teil 7: Ein gemittelter Wert schwankte bei 4K zwischen 7300 und
/// 9150 us — einmal unter, einmal ueber der 8-ms-Schwelle des
/// Umstiegskriteriums. Der Scheduler nimmt alle 20 ms die CPU weg, und
/// der Compositor arbeitet nebenher; diese Fremdzeit gehoert nicht zum
/// Scroll-Frame.
const DURCHGAENGE: u32 = 5;

/// Scroll-Frames messen und die Zahlen ausgeben.
///
/// DER MODUS BLEIBT, obwohl der Browser in Teil 8 ein anderer geworden
/// ist: Er ist der Beleg fuer die Entscheidung zum Umstiegskriterium,
/// und eine Messung, die man nach einem Umbau nicht mehr wiederholen
/// kann, ist keine.
///
/// EINE ZAHL AENDERT SICH DABEI EHRLICHERWEISE: Die Seitenflaeche ist
/// jetzt um den Chrome (Tab-Leiste + Werkzeugleiste) kleiner. Gemessen
/// wird, was wirklich gescrollt wird — also die kleinere Flaeche.
fn messmodus(b: &mut Browser, quelle: &str, runden: u32) {
    let bereich = b.seiten_bereich();
    println!("QUELLE={}", quelle);
    println!("FENSTER_BREITE={}", b.fenster.breite());
    println!("FENSTER_HOEHE={}", b.fenster.hoehe());
    println!("SEITE_BREITE={}", bereich.breite);
    println!("SEITE_HOEHE={}", bereich.hoehe);
    println!("BEFEHLE={}", b.tabs[b.aktiv].liste.len());
    println!("DOKUMENT_HOEHE={}", b.tabs[b.aktiv].hoehe);
    println!("RUNDEN={}", runden);

    let je_durchgang = (runden / DURCHGAENGE).max(4);
    let mut malen_us = u64::MAX;
    let mut kopie_us = u64::MAX;
    let mut schritte = 0u64;
    let mut runter = true;

    for _ in 0..DURCHGAENGE {
        let mut malen_ms = 0u64;
        let mut kopie_ms = 0u64;
        let mut im_durchgang = 0u64;
        for _ in 0..je_durchgang {
            let sicht = &b.tabs[b.aktiv].sicht;
            if runter && sicht.versatz() >= sicht.max_versatz() {
                runter = false;
            } else if !runter && sicht.versatz() == 0 {
                runter = true;
            }
            // NEGATIV = nach unten (Rad-Konvention aus speedpaint::sicht).
            let folge = b.tabs[b.aktiv]
                .sicht
                .scrollen(Scrollschritt::Rad(if runter { -1 } else { 1 }));
            let Some(streifen) = folge.streifen else { continue };
            im_durchgang += 1;

            let t0 = libspeed::zeit_jetzt();
            if folge.verschieben_lohnt() {
                b.fenster.senkrecht_verschieben_bereich(
                    bereich.y.max(0) as usize,
                    bereich.hoehe.max(0) as usize,
                    folge.verschiebung,
                );
            }
            if let Some(sicher) = streifen.schneiden(&bereich) {
                b.seite_zeichnen(sicher);
            }
            let t1 = libspeed::zeit_jetzt();
            // NUR das Seitenband uebertragen — der Chrome hat sich nicht
            // geaendert. Genau so laeuft es auch im Betrieb.
            let _ = b.fenster.zeigen_bereich(
                0,
                bereich.y.max(0) as usize,
                bereich.breite.max(0) as usize,
                bereich.hoehe.max(0) as usize,
            );
            let t2 = libspeed::zeit_jetzt();
            malen_ms += t1.saturating_sub(t0);
            kopie_ms += t2.saturating_sub(t1);
        }
        if im_durchgang == 0 {
            continue;
        }
        schritte += im_durchgang;
        malen_us = malen_us.min(malen_ms.saturating_mul(1000) / im_durchgang);
        kopie_us = kopie_us.min(kopie_ms.saturating_mul(1000) / im_durchgang);
    }

    // Der Vergleichsfall: die ganze Flaeche malen.
    let voll_je_durchgang = (je_durchgang / 4).max(4);
    let mut voll_us = u64::MAX;
    for _ in 0..DURCHGAENGE {
        let t0 = libspeed::zeit_jetzt();
        for _ in 0..voll_je_durchgang {
            b.seite_zeichnen(bereich);
        }
        let ms = libspeed::zeit_jetzt().saturating_sub(t0);
        voll_us = voll_us.min(ms.saturating_mul(1000) / voll_je_durchgang as u64);
    }

    let (belegt, gesamt, spitze) = libspeed::heap::heap_stand();
    println!("HEAP_BELEGT={}", belegt);
    println!("HEAP_GESAMT={}", gesamt);
    println!("HEAP_SPITZE={}", spitze);
    println!("DURCHGAENGE={}", DURCHGAENGE);
    println!("SCHRITTE={}", schritte.max(1));
    let malen_us = if malen_us == u64::MAX { 0 } else { malen_us };
    let kopie_us = if kopie_us == u64::MAX { 0 } else { kopie_us };
    let voll_us = if voll_us == u64::MAX { 0 } else { voll_us };
    println!("MALEN_US={}", malen_us);
    println!("KOPIE_US={}", kopie_us);
    println!("FRAME_US={}", malen_us + kopie_us);
    println!("VOLL_MALEN_US={}", voll_us);
    println!(
        "KOPIE_ANTEIL_PROZENT={}",
        kopie_us * 100 / (malen_us + kopie_us).max(1)
    );
}

fn ereignisschleife(b: &mut Browser) {
    let thema = BrowserThema;
    let schrift = BrowserSchrift;
    let uhr = BrowserUhr;

    loop {
        // Offene Bilder haben Vorrang vor dem Warten: Solange welche da
        // sind, wird nicht blockiert — die Seite soll sich fuellen.
        let frist = if b.tabs[b.aktiv].offene_bilder.is_empty() {
            80
        } else {
            0
        };
        let ereignis = match b.fenster.naechstes_ereignis(frist) {
            Ok(e) => e,
            Err(_) => return,
        };

        let k = Browser::kontext(&thema, &schrift, &uhr);

        match ereignis {
            Ereignis::Schliessen => return,

            Ereignis::Keins => {
                if b.ein_bild_laden() {
                    continue;
                }
            }

            Ereignis::Fokus(_) => {}

            Ereignis::Groesse { breite, hoehe } => {
                let alt = b.tabs[b.aktiv].sicht.bereich;
                b.fenster.groesse_uebernehmen(breite, hoehe);
                b.chrome.breite = breite as i32;
                let anlass = Anlass::FensterGroesse {
                    alte_breite: alt.breite,
                    alte_hoehe: alt.hoehe,
                    neue_breite: breite as i32,
                    neue_hoehe: (hoehe as i32 - chrome::OBEN).max(1),
                };
                let bereich = b.seiten_bereich();
                let neue_hoehe = b.tabs[b.aktiv].hoehe;
                b.tabs[b.aktiv].sicht.anpassen(bereich, neue_hoehe);
                let massnahme = speedpaint::invalidierung::entscheiden(anlass);
                b.massnahme_ausfuehren(massnahme);
            }

            Ereignis::MausRad { x, y, delta } => {
                if y >= chrome::OBEN {
                    let _ = x;
                    b.scrollen(Scrollschritt::Rad(delta));
                }
            }

            Ereignis::MausAb { x, y, .. } => {
                if y < chrome::OBEN {
                    let anzahl = b.tabs.len();
                    let aktion = b.chrome.klick(x, y, anzahl, &k);
                    if !aktion_ausfuehren(b, aktion) {
                        return;
                    }
                    continue;
                }
                b.chrome.fokus_loesen();
                // Der Scrollbalken?
                if let Some(balken) = b.tabs[b.aktiv].sicht.balken(BALKEN_BREITE) {
                    if x >= balken.spur.x {
                        b.balken_gefasst = true;
                        let ziel = b.tabs[b.aktiv].sicht.versatz_aus_balken(y, &balken);
                        b.scrollen(Scrollschritt::Nach(ziel));
                        continue;
                    }
                }
                // Ein Verweis?
                if let Some((href, _)) = b.tabs[b.aktiv].verweis_bei(x, y) {
                    b.verweis_folgen(&href);
                }
            }

            Ereignis::MausAuf { .. } => b.balken_gefasst = false,

            Ereignis::MausBewegt { x, y } => {
                if b.balken_gefasst {
                    if let Some(balken) = b.tabs[b.aktiv].sicht.balken(BALKEN_BREITE) {
                        let ziel = b.tabs[b.aktiv].sicht.versatz_aus_balken(y, &balken);
                        b.scrollen(Scrollschritt::Nach(ziel));
                    }
                    continue;
                }
                // DIE STATUSZEILE: Was liegt unter dem Cursor?
                let ziel = if y >= chrome::OBEN {
                    b.tabs[b.aktiv]
                        .verweis_bei(x, y)
                        .and_then(|(href, _)| b.tabs[b.aktiv].ort.aufloesen(&href).ok())
                        .map(|v| v.ort.als_text())
                } else {
                    None
                };
                // NUR bei ECHTER Aenderung neu zeichnen — sonst kostet
                // jede Mausbewegung ein Vollbild.
                if ziel != b.unter_cursor {
                    b.unter_cursor = ziel.clone();
                    b.chrome.status = ziel;
                    b.chrome.meldung = None;
                    b.alles_zeichnen();
                }
            }

            Ereignis::Sondertaste(code) => {
                if !sondertaste(b, code, &k) {
                    return;
                }
            }

            Ereignis::Taste(zeichen) => {
                if !zeichen_taste(b, zeichen, &k) {
                    return;
                }
            }
        }
    }
}

/// Eine Chrome-Aktion umsetzen. `false` = Fenster schliessen.
fn aktion_ausfuehren(b: &mut Browser, aktion: Aktion) -> bool {
    match aktion {
        Aktion::Keine => {}
        Aktion::NurZeichnen => b.alles_zeichnen(),
        Aktion::Zurueck => {
            if let Some(ziel) = b.tabs[b.aktiv].zurueck() {
                b.navigieren(ziel, false, None);
            }
        }
        Aktion::Vor => {
            if let Some(ziel) = b.tabs[b.aktiv].vor() {
                b.navigieren(ziel, false, None);
            }
        }
        Aktion::NeuLaden => {
            let ziel = b.tabs[b.aktiv].ort.clone();
            b.navigieren(ziel, false, None);
        }
        Aktion::Gehen(text) => match ort::Ort::parsen(&text) {
            Ok(ziel) => {
                b.chrome.fokus_loesen();
                b.navigieren(ziel, true, None);
            }
            Err(fehler) => {
                b.chrome.meldung = Some(String::from(fehler.text()));
                b.alles_zeichnen();
            }
        },
        Aktion::Merken => {
            let adresse = b.tabs[b.aktiv].ort.als_text();
            let titel = b.tabs[b.aktiv].titel.clone();
            let gesetzt = b.merkliste.umschalten(&adresse, &titel);
            b.chrome.meldung = Some(String::from(if gesetzt {
                "Lesezeichen gesetzt."
            } else {
                "Lesezeichen entfernt."
            }));
            b.alles_zeichnen();
        }
        Aktion::TabWaehlen(i) => b.tab_waehlen(i),
        Aktion::TabSchliessen(i) => {
            if !b.tab_schliessen(i) {
                return false;
            }
        }
        Aktion::TabNeu => b.tab_neu(Ort::Intern(Intern::Info)),
    }
    true
}

/// Sondertasten. `false` = beenden.
fn sondertaste(b: &mut Browser, code: i32, k: &UiKontext) -> bool {
    use libspeed::fenster::*;
    // Im Adressfeld gehen die Tasten ans Feld.
    if b.chrome.adresse_hat_fokus() {
        if let Some(taste) = taste_von(code) {
            let aktion = b.chrome.taste(taste, k);
            return aktion_ausfuehren(b, aktion);
        }
    }
    let schritt = match code {
        SONDER_HOCH => Some(Scrollschritt::ZeileHoch),
        SONDER_RUNTER => Some(Scrollschritt::ZeileRunter),
        SONDER_BILD_HOCH => Some(Scrollschritt::SeiteHoch),
        SONDER_BILD_RUNTER => Some(Scrollschritt::SeiteRunter),
        SONDER_POS1 => Some(Scrollschritt::Anfang),
        SONDER_ENDE => Some(Scrollschritt::Ende),
        _ => None,
    };
    if let Some(schritt) = schritt {
        b.scrollen(schritt);
        return true;
    }
    if code == SONDER_F1 {
        b.navigieren(Ort::Intern(Intern::Info), true, None);
    }
    true
}

/// Zeichen-Tasten (inklusive der Strg-Kuerzel). `false` = beenden.
///
/// ===================================================================
/// STRG KOMMT ALS STEUERZEICHEN
///
/// Die Fenster-ABI hat keine Modifikatortasten (docs/grenzen.md) — der
/// Kernel dekodiert Strg+T aber zu U+0014, weil der KeyStream mit
/// `MapLettersToUnicode` arbeitet (dieselbe Mechanik, ueber die Strg+C
/// in die Ablage kam, Serie 3). Ein Steuerzeichen ist also ein Kuerzel.
fn zeichen_taste(b: &mut Browser, zeichen: char, k: &UiKontext) -> bool {
    let code = zeichen as u32;

    // ===================================================================
    // DIE KOLLISION, DIE ES WIRKLICH GIBT: STRG+H **IST** BACKSPACE
    //
    // Beide sind U+0008. Das ist keine Eigenart von SpeedOS, sondern
    // stammt aus der Fernschreiber-Zeit und gilt in jedem Terminal —
    // und weil unsere Fenster-ABI keine Modifikatortasten hat
    // (docs/grenzen.md), kann der Browser die beiden NICHT
    // unterscheiden.
    //
    // Aufgeloest wird es am KONTEXT, und zwar so, wie es jeder Browser
    // ohnehin tut: Steht der Cursor in der Adressleiste, ist es
    // Backspace (loeschen). Steht er nicht dort, ist es Strg+H
    // (Verlauf). Ohne diese Reihenfolge oeffnet JEDER Backspace beim
    // Tippen einer Adresse den Verlauf — genau das ist beim ersten
    // Probelauf passiert.
    //
    // Dieselbe Ueberlegung gilt fuer 9 (Tab), 13 (Enter) und 27 (Esc):
    // Sie sind als Kuerzel unbrauchbar und werden deshalb keins.
    // (Backspace ist fuer das Toolkit ein ZEICHEN: U+0008.)
    if code == 8 {
        if b.chrome.adresse_hat_fokus() {
            let aktion = b.chrome.taste(Taste::Zeichen(zeichen), k);
            return aktion_ausfuehren(b, aktion);
        }
        b.navigieren(Ort::Intern(Intern::Verlauf), true, None);
        return true;
    }

    match code {
        // Strg+T = 20, Strg+W = 23, Strg+D = 4, Strg+L = 12,
        // Strg+R = 18, Strg+B = 2. (Buchstabe - 'a' + 1)
        20 => {
            b.tab_neu(Ort::Intern(Intern::Info));
            return true;
        }
        23 => {
            let aktiv = b.aktiv;
            return b.tab_schliessen(aktiv);
        }
        4 => return aktion_ausfuehren(b, Aktion::Merken),
        12 => {
            b.chrome.adresse_fokussieren();
            b.alles_zeichnen();
            return true;
        }
        18 => return aktion_ausfuehren(b, Aktion::NeuLaden),
        2 => {
            // Strg+B = Lesezeichen-Seite.
            b.navigieren(Ort::Intern(Intern::Lesezeichen), true, None);
            return true;
        }
        _ => {}
    }

    if b.chrome.adresse_hat_fokus() {
        let aktion = b.chrome.taste(Taste::Zeichen(zeichen), k);
        return aktion_ausfuehren(b, aktion);
    }
    match zeichen {
        ' ' => b.scrollen(Scrollschritt::SeiteRunter),
        'q' => return false,
        _ => {}
    }
    true
}

/// ABI-Sondertaste -> `speedui::Taste`.
fn taste_von(code: i32) -> Option<Taste> {
    use libspeed::fenster::*;
    match code {
        SONDER_LINKS => Some(Taste::Links),
        SONDER_RECHTS => Some(Taste::Rechts),
        SONDER_HOCH => Some(Taste::Hoch),
        SONDER_RUNTER => Some(Taste::Runter),
        SONDER_POS1 => Some(Taste::Pos1),
        SONDER_ENDE => Some(Taste::Ende),
        SONDER_ENTF => Some(Taste::Entf),
        _ => None,
    }
}

fn hilfe(programm: &str) {
    println!("Aufruf: {} [adresse]", programm);
    println!();
    println!("  starte browser &                          Startseite");
    println!("  starte browser https://example.com &      Adresse");
    println!("  starte browser /platte/seiten/cern.html & lokale Datei");
    println!();
    println!("  Das & ist PFLICHT — sonst kommt der Compositor nicht dran.");
    println!("  Strg+T Tab   Strg+W zu   Strg+L Adresse   Strg+H Verlauf");
    println!("  Strg+D Lesezeichen   Strg+B Liste   Strg+R neu laden");
    println!("  speedos:info zeigt, was dieser Browser kann und was nicht.");
}
