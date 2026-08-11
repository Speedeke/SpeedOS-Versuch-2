// browser::chrome — die Bedienoberflaeche
//
// ===========================================================================
// WIDGETS AUS `speedui`, ABER OHNE `UiFenster`
//
// Die Adressleiste ist ein echtes `speedui::widgets::Textfeld` — mit dem
// Zeileneditor aus Serie 3 darin, also Tippen, Backspace, Pfeiltasten und
// Eingabe-Verlauf, ohne dass davon hier eine Zeile stuende. Die Knoepfe
// sind echte `Button`s mit Hover-Zustand aus dem Thema.
//
// Was NICHT benutzt wird, ist `UiFenster`: Es legt seinen Baum immer ueber
// die GANZE Leinwand (`zeichnen` nimmt `leinwand.masse()`), und dieses
// Fenster gehoert zur Haelfte dem Renderer. Die Widgets werden deshalb
// EINZELN gehalten, gezeichnet und mit Ereignissen versorgt — das
// `Widget`-Trait kann genau das (`zeichnen(&self, m, bereich)` und
// `ereignis(&mut self, e, bereich, k)`), und mehr braucht eine Leiste
// nicht. Die Alternative waere `speedui::TeilLeinwand` plus ein zweites
// `UiFenster` gewesen; das kostet einen Container fuer fuenf Widgets, die
// ohnehin an festen Plaetzen sitzen.
//
// ===========================================================================
// DIE TAB-LEISTE IST SELBST GEZEICHNET
//
// Ein Tab-Reiter ist kein Widget des Toolkits (es hat keins), und ihn als
// Knopf nachzubauen ginge an dem vorbei, was ihn ausmacht: Er hat einen
// gekuerzten Titel, einen eigenen Schliessen-Knopf und einen Zustand
// (aktiv/inaktiv/laedt). Das sind dreissig Zeilen Zeichnen und eine
// Trefferpruefung — ein Widget dafuer waere mehr Code, nicht weniger.

use crate::tab::{Tab, TabZustand};
use alloc::string::String;
use alloc::vec::Vec;
use libspeed::leinwand::RasterMetrik;
use speedlayout::Metrik;
use speedui::widgets::{Button, Textfeld};
use speedui::{
    Farbe, Farbrolle, Leinwand, Maler, Mass, Rechteck, Schrift, Thema, Uhr, UiEreignis, UiKontext,
    Widget,
};

// --- Masse der Leisten ---
pub const TAB_HOEHE: i32 = 26;
pub const LEISTE_HOEHE: i32 = 32;
pub const STATUS_HOEHE: i32 = 18;
/// Gesamthoehe des Chrome oben.
pub const OBEN: i32 = TAB_HOEHE + LEISTE_HOEHE;

const TAB_BREITE_MAX: i32 = 170;
const TAB_BREITE_MIN: i32 = 60;
const KNOPF_BREITE: i32 = 30;
const RAND: i32 = 4;

// --- Nachrichten der Knoepfe ---
const N_ZURUECK: u32 = 1;
const N_VOR: u32 = 2;
const N_NEU: u32 = 3;
const N_LOS: u32 = 4;
const N_MERKEN: u32 = 5;

/// Was der Benutzer im Chrome ausgeloest hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aktion {
    Keine,
    Zurueck,
    Vor,
    NeuLaden,
    /// Adresse aus der Leiste laden.
    Gehen(String),
    /// Lesezeichen setzen.
    Merken,
    TabWaehlen(usize),
    TabSchliessen(usize),
    TabNeu,
    /// Das Ereignis war fuer den Chrome, hat aber nichts ausgeloest
    /// (z. B. ein Tastendruck im Adressfeld) — nur neu zeichnen.
    NurZeichnen,
}

// ===========================================================================
// DER WIRT: Thema, Schrift, Uhr
// ===========================================================================

pub struct BrowserThema;

impl Thema for BrowserThema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe {
        match rolle {
            Farbrolle::Flaeche => Farbe::neu(238, 238, 242),
            Farbrolle::InhaltHintergrund => Farbe::neu(255, 255, 255),
            Farbrolle::Rahmen => Farbe::neu(196, 196, 204),
            Farbrolle::Akzent => Farbe::neu(64, 110, 200),
            Farbrolle::Auswahl => Farbe::neu(200, 216, 244),
            Farbrolle::Eingabefeld => Farbe::neu(255, 255, 255),
            Farbrolle::KnopfFlaeche => Farbe::neu(226, 226, 232),
            Farbrolle::KnopfAktiv => Farbe::neu(206, 214, 236),
            Farbrolle::TextStark => Farbe::neu(24, 24, 30),
            Farbrolle::TextNormal => Farbe::neu(40, 40, 48),
            Farbrolle::TextSekundaer => Farbe::neu(96, 96, 108),
            Farbrolle::TextGedimmt => Farbe::neu(150, 150, 160),
            Farbrolle::TextAufAkzent => Farbe::neu(255, 255, 255),
        }
    }

    fn mass(&self, mass: Mass) -> i32 {
        match mass {
            Mass::Abstand => 4,
            Mass::UiRand => 4,
            Mass::ElementHoehe => LEISTE_HOEHE - 2 * RAND,
            Mass::ListenEintragHoehe => 20,
            Mass::ScrollbalkenBreite => 12,
            Mass::RadiusKlein => 3,
            Mass::SchriftUi => 14,
            Mass::ZeilenHoehe => 18,
            Mass::CursorBlinkUs => 500_000,
        }
    }
}

/// Die Schrift des Chrome ist DIESELBE wie die der Seite (das 5x7-Raster).
///
/// Nicht aus Bequemlichkeit: Ein Prozess hat nur diese eine, und eine
/// Leiste, die so tut, als haette sie eine zweite, wuerde bei jeder
/// Breitenrechnung danebenliegen.
pub struct BrowserSchrift;

impl Schrift for BrowserSchrift {
    fn zeichen_breite(&self, groesse: i32) -> i32 {
        libspeed::leinwand::RASTER_BREITE * RasterMetrik::skala(groesse)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        RasterMetrik.zeilen_hoehe(groesse)
    }
}

pub struct BrowserUhr;

impl Uhr for BrowserUhr {
    fn us(&self) -> u64 {
        libspeed::zeit_jetzt().saturating_mul(1000)
    }
}

// ===========================================================================
// DER CHROME
// ===========================================================================

pub struct Chrome {
    pub adresse: Textfeld,
    zurueck: Button,
    vor: Button,
    neu_laden: Button,
    los: Button,
    /// Ziel des Verweises unter dem Cursor — die Statuszeile.
    pub status: Option<String>,
    /// Eine Meldung, die statt des Verweis-Ziels erscheint.
    pub meldung: Option<String>,
    pub breite: i32,
}

impl Chrome {
    pub fn neu(breite: i32) -> Chrome {
        Chrome {
            adresse: Textfeld::neu(N_LOS),
            zurueck: Button::neu("<", N_ZURUECK),
            vor: Button::neu(">", N_VOR),
            neu_laden: Button::neu("R", N_NEU),
            los: Button::neu("OK", N_LOS),
            status: None,
            meldung: None,
            breite,
        }
    }

    pub fn adresse_setzen(&mut self, text: &str) {
        self.adresse.text_setzen(text);
    }

    pub fn adresse_fokussieren(&mut self) {
        self.adresse.fokus_setzen(true);
    }

    // -----------------------------------------------------------------
    // GEOMETRIE — an EINER Stelle, damit Zeichnen und Treffer nie
    // auseinanderlaufen.
    // -----------------------------------------------------------------

    fn leiste_y(&self) -> i32 {
        TAB_HOEHE
    }

    /// Die Rechtecke der Werkzeugleiste: (zurueck, vor, neu, adresse, los, merken).
    fn leiste(&self) -> [Rechteck; 6] {
        let y = self.leiste_y() + RAND;
        let h = LEISTE_HOEHE - 2 * RAND;
        let schritt = KNOPF_BREITE + RAND;
        // Von Hand gerechnet statt mit einer Closure: Die wuerde `x`
        // ausleihen und danach waere `x` fuer die Breitenrechnung des
        // Adressfelds gesperrt.
        let zurueck = Rechteck::neu(RAND, y, KNOPF_BREITE, h);
        let vor = Rechteck::neu(RAND + schritt, y, KNOPF_BREITE, h);
        let neu = Rechteck::neu(RAND + 2 * schritt, y, KNOPF_BREITE, h);
        let adresse_x = RAND + 3 * schritt;
        // Das Adressfeld bekommt den Rest, minus den beiden rechten
        // Knoepfen.
        let adresse_breite = (self.breite - adresse_x - 2 * schritt - RAND).max(40);
        let adresse = Rechteck::neu(adresse_x, y, adresse_breite, h);
        let los = Rechteck::neu(adresse_x + adresse_breite + RAND, y, KNOPF_BREITE, h);
        let merken = Rechteck::neu(los.x + schritt, y, KNOPF_BREITE, h);
        [zurueck, vor, neu, adresse, los, merken]
    }

    /// Die Reiter-Rechtecke plus das Plus-Feld.
    fn reiter(&self, anzahl: usize) -> (Vec<Rechteck>, Rechteck) {
        let plus_breite = TAB_HOEHE;
        let platz = (self.breite - plus_breite - RAND).max(0);
        let breite = if anzahl == 0 {
            TAB_BREITE_MAX
        } else {
            (platz / anzahl as i32).clamp(TAB_BREITE_MIN, TAB_BREITE_MAX)
        };
        let mut aus = Vec::with_capacity(anzahl);
        for i in 0..anzahl {
            aus.push(Rechteck::neu(i as i32 * breite, 0, breite - 1, TAB_HOEHE));
        }
        let plus_x = (anzahl as i32 * breite).min(self.breite - plus_breite);
        (aus, Rechteck::neu(plus_x, 0, plus_breite, TAB_HOEHE))
    }

    /// Das Schliessen-Kreuz eines Reiters.
    fn kreuz(reiter: Rechteck) -> Rechteck {
        Rechteck::neu(reiter.x + reiter.breite - 18, reiter.y + 5, 14, 14)
    }

    // -----------------------------------------------------------------
    // ZEICHNEN
    // -----------------------------------------------------------------

    pub fn zeichnen(
        &self,
        leinwand: &mut dyn Leinwand,
        k: &UiKontext,
        tabs: &[Tab],
        aktiv: usize,
        kann_zurueck: bool,
        kann_vor: bool,
        gemerkt: bool,
    ) {
        let mut m = Maler::neu(leinwand, *k);
        // Hintergrund beider Leisten.
        m.fuellen(
            Rechteck::neu(0, 0, self.breite, OBEN),
            k.farbe(Farbrolle::Flaeche),
        );

        self.reiter_zeichnen(&mut m, k, tabs, aktiv);

        let [r_zurueck, r_vor, r_neu, r_adresse, r_los, r_merken] = self.leiste();
        // Die Knoepfe wissen selbst, wie sie aussehen — inklusive
        // deaktiviert.
        Button::neu("<", N_ZURUECK)
            .mit_deaktiviert(!kann_zurueck)
            .zeichnen(&mut m, r_zurueck);
        Button::neu(">", N_VOR)
            .mit_deaktiviert(!kann_vor)
            .zeichnen(&mut m, r_vor);
        self.neu_laden.zeichnen(&mut m, r_neu);
        self.adresse.zeichnen(&mut m, r_adresse);
        self.los.zeichnen(&mut m, r_los);
        // Der Stern zeigt, OB die Seite gemerkt ist — sonst waere der
        // Knopf ein Schalter ohne Zustand, und man muesste ihn druecken,
        // um es herauszufinden.
        Button::neu(if gemerkt { "*" } else { "-" }, N_MERKEN)
            .mit_aktiv(gemerkt)
            .zeichnen(&mut m, r_merken);

        // Eine Trennlinie zum Inhalt.
        m.fuellen(
            Rechteck::neu(0, OBEN - 1, self.breite, 1),
            k.farbe(Farbrolle::Rahmen),
        );
    }

    fn reiter_zeichnen(&self, m: &mut Maler<'_>, k: &UiKontext, tabs: &[Tab], aktiv: usize) {
        let (rechtecke, plus) = self.reiter(tabs.len());
        for (i, r) in rechtecke.iter().enumerate() {
            let ist_aktiv = i == aktiv;
            let flaeche = if ist_aktiv {
                k.farbe(Farbrolle::InhaltHintergrund)
            } else {
                k.farbe(Farbrolle::KnopfFlaeche)
            };
            m.fuellen(*r, flaeche);
            m.fuellen(
                Rechteck::neu(r.x + r.breite, r.y + 3, 1, r.hoehe - 6),
                k.farbe(Farbrolle::Rahmen),
            );
            if ist_aktiv {
                // Ein Akzentstreifen oben — die uebliche Anzeige, welcher
                // Reiter gilt.
                m.fuellen(
                    Rechteck::neu(r.x, r.y, r.breite, 2),
                    k.farbe(Farbrolle::Akzent),
                );
            }
            let tab = &tabs[i];
            let farbe = if ist_aktiv {
                k.farbe(Farbrolle::TextStark)
            } else {
                k.farbe(Farbrolle::TextSekundaer)
            };
            // Platz bis zum Kreuz.
            let platz = r.breite - 26;
            let beschriftung = if tab.zustand == TabZustand::Laedt {
                String::from("laedt ...")
            } else {
                tab.titel.clone()
            };
            let text = kuerzen(&beschriftung, platz, k);
            m.text_mit(r.x + 6, r.y + 8, &text, k.mass(Mass::SchriftUi), false, farbe);

            let kreuz = Self::kreuz(*r);
            m.text_mit(
                kreuz.x + 3,
                kreuz.y + 3,
                "x",
                k.mass(Mass::SchriftUi),
                false,
                k.farbe(Farbrolle::TextGedimmt),
            );
        }
        // Der Plus-Knopf.
        m.fuellen(plus, k.farbe(Farbrolle::KnopfFlaeche));
        m.text_mit(
            plus.x + 9,
            plus.y + 8,
            "+",
            k.mass(Mass::SchriftUi),
            true,
            k.farbe(Farbrolle::TextNormal),
        );
    }

    /// Die Statuszeile unten — nur, wenn es etwas zu sagen gibt.
    ///
    /// SIE LIEGT UEBER DEM INHALT und wird nur gezeichnet, wenn ein
    /// Verweis unter dem Cursor liegt oder eine Meldung ansteht. Eine
    /// Zeile, die immer da ist, kostet bei jeder Seite Platz fuer eine
    /// Auskunft, die man meistens nicht braucht.
    pub fn status_zeichnen(&self, leinwand: &mut dyn Leinwand, k: &UiKontext, hoehe: i32) {
        let Some(text) = self.status_text() else {
            return;
        };
        let mut m = Maler::neu(leinwand, *k);
        let breite = (k.text_breite(&text) + 16).min(self.breite);
        let r = Rechteck::neu(0, hoehe - STATUS_HOEHE, breite, STATUS_HOEHE);
        m.fuellen(r, k.farbe(Farbrolle::Flaeche));
        m.fuellen(
            Rechteck::neu(r.x, r.y, r.breite, 1),
            k.farbe(Farbrolle::Rahmen),
        );
        m.text_mit(
            r.x + 8,
            r.y + 5,
            &text,
            k.mass(Mass::SchriftUi),
            false,
            k.farbe(Farbrolle::TextSekundaer),
        );
    }

    pub fn status_text(&self) -> Option<String> {
        self.meldung.clone().or_else(|| self.status.clone())
    }

    // -----------------------------------------------------------------
    // EREIGNISSE
    // -----------------------------------------------------------------

    /// Ein Klick im Chrome-Bereich.
    pub fn klick(&mut self, x: i32, y: i32, anzahl_tabs: usize, k: &UiKontext) -> Aktion {
        if y < TAB_HOEHE {
            let (rechtecke, plus) = self.reiter(anzahl_tabs);
            if plus.enthaelt(x, y) {
                return Aktion::TabNeu;
            }
            for (i, r) in rechtecke.iter().enumerate() {
                if !r.enthaelt(x, y) {
                    continue;
                }
                // Das Kreuz zuerst: Es liegt IM Reiter, und wer es trifft,
                // meint nicht den Reiter.
                if Self::kreuz(*r).enthaelt(x, y) {
                    return Aktion::TabSchliessen(i);
                }
                return Aktion::TabWaehlen(i);
            }
            return Aktion::Keine;
        }

        let [r_zurueck, r_vor, r_neu, r_adresse, r_los, r_merken] = self.leiste();
        let ereignis = UiEreignis::Klick { x, y };
        // Der Fokus wandert zum Adressfeld, wenn man es trifft — und weg,
        // wenn man daneben trifft.
        self.adresse.fokus_setzen(r_adresse.enthaelt(x, y));

        if r_zurueck.enthaelt(x, y) {
            self.zurueck.ereignis(&ereignis, r_zurueck, k);
            return Aktion::Zurueck;
        }
        if r_vor.enthaelt(x, y) {
            self.vor.ereignis(&ereignis, r_vor, k);
            return Aktion::Vor;
        }
        if r_neu.enthaelt(x, y) {
            self.neu_laden.ereignis(&ereignis, r_neu, k);
            return Aktion::NeuLaden;
        }
        if r_los.enthaelt(x, y) {
            return Aktion::Gehen(String::from(self.adresse.text()));
        }
        if r_merken.enthaelt(x, y) {
            return Aktion::Merken;
        }
        if r_adresse.enthaelt(x, y) {
            self.adresse.ereignis(&ereignis, r_adresse, k);
            return Aktion::NurZeichnen;
        }
        Aktion::Keine
    }

    /// Eine Taste, waehrend das Adressfeld den Fokus hat.
    pub fn taste(&mut self, taste: speedui::Taste, k: &UiKontext) -> Aktion {
        if !self.adresse.hat_fokus() {
            return Aktion::Keine;
        }
        let [.., r_adresse, _, _] = self.leiste();
        // DEN TEXT VORHER LESEN, und zwar zwingend: Das `Textfeld` ist das
        // Eingabefeld der Shell (Serie 3), und dort BEENDET Enter eine
        // Zeile — der `ZeilenEditor` legt sie in seinen Verlauf und
        // LEERT sich. Wer erst das Ereignis schickt und dann `text()`
        // liest, bekommt einen leeren String und laedt nichts. Genau das
        // ist beim ersten Probelauf passiert: Adresse getippt, Enter,
        // Feld leer, Seite unveraendert.
        let vorher = String::from(self.adresse.text());
        let reaktion = self
            .adresse
            .ereignis(&UiEreignis::Taste(taste), r_adresse, k);
        match reaktion.nachricht {
            Some(N_LOS) => Aktion::Gehen(vorher),
            _ => {
                if reaktion.neu_zeichnen {
                    Aktion::NurZeichnen
                } else {
                    Aktion::Keine
                }
            }
        }
    }

    pub fn adresse_hat_fokus(&self) -> bool {
        self.adresse.hat_fokus()
    }

    pub fn fokus_loesen(&mut self) {
        self.adresse.fokus_setzen(false);
    }
}

/// Text so kuerzen, dass er in `platz` Pixel passt (mit „...").
fn kuerzen(text: &str, platz: i32, k: &UiKontext) -> String {
    if k.text_breite(text) <= platz {
        return String::from(text);
    }
    let je_zeichen = k.zeichen_breite().max(1);
    let passt = ((platz / je_zeichen) - 2).max(1) as usize;
    let mut aus: String = text.chars().take(passt).collect();
    aus.push_str("..");
    aus
}
