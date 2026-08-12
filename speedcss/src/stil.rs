// speedcss::stil — der BERECHNETE Stil eines Elements
//
// ===========================================================================
// EIN STRUCT UND KEINE TABELLE
//
// Der naheliegende Weg waere eine Zuordnung `Eigenschaftsname ->
// Wert-String` je Knoten. Der ist flexibel und falsch:
//
//   * Das Layout muesste jeden Wert bei JEDEM Zugriff neu deuten
//     („was heisst `display` hier?") — und ein Layout greift oft zu.
//   * Ein Tippfehler im Namen faellt nie auf.
//   * Bei 20 000 Knoten waeren es 20 000 Zuordnungen mit je einem Dutzend
//     Strings. Auf 12 MiB Prozess-Heap ist das keine Kleinigkeit.
//
// Also feste Felder. Der berechnete Stil ist damit das, was die
// CSS-Spezifikation „computed value" nennt: alles gedeutet, `em`
// aufgeloest, Vererbung angewandt — und genau die Form, die das Layout
// will.
//
// **Die Liste der Felder IST die Liste der unterstuetzten Eigenschaften**
// (docs/browser-v1.md §2.3). Wer eine ergaenzt, tut es hier sichtbar und
// muss sie in `anwenden` deuten und in `ERBT` einordnen.
//
// ===========================================================================
// KEIN FLIESSKOMMA
//
// Alle Laengen stehen in TAUSENDSTELN (siehe `werte.rs`). Das gilt auch
// fuer `schrift_px` — `font-size: 62.5%` von 16 px sind 10 000, nicht 10.

use crate::werte::{farbe_parsen, kleinschreiben, laenge_parsen, zahl_tausendstel, Farbe, Laenge, TAUSEND};
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// AUFZAEHLUNGEN
// ---------------------------------------------------------------------------

/// `display` — die Teilmenge aus docs/browser-v1.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// `none` — das Element und sein Teilbaum erscheinen nicht.
    Keine,
    Block,
    Inline,
    InlineBlock,
    /// `list-item` — Block mit Aufzaehlungszeichen.
    Listenpunkt,
    Tabelle,
    /// `table-row-group` / `thead` / `tbody` / `tfoot`
    TabellenGruppe,
    TabellenZeile,
    TabellenZelle,
}

impl Display {
    /// Nimmt dieses Element am Blockfluss teil?
    pub fn ist_block_artig(self) -> bool {
        matches!(
            self,
            Display::Block
                | Display::Listenpunkt
                | Display::Tabelle
                | Display::TabellenGruppe
                | Display::TabellenZeile
                | Display::TabellenZelle
        )
    }
}

/// `text-align`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ausrichtung {
    Links,
    Mitte,
    Rechts,
    /// `justify` — wir setzen NICHT im Blocksatz (das braucht
    /// Wortabstands-Verteilung). Wird wie `Links` gerendert; der Wert
    /// bleibt trotzdem erhalten, damit `cssdump` die Wahrheit sagt.
    Blocksatz,
}

/// `white-space` — wie mit Leerraum und Umbruch umgegangen wird.
///
/// ===================================================================
/// WARUM DIESE EIGENSCHAFT IN SERIE 9, TEIL 2 DAZUKAM
///
/// Bis hierher entschied der TAG-NAME darueber, ob Leerraum erhalten
/// bleibt (`kasten::ist_vorformatiert` lief den Baum hoch und suchte
/// `<pre>`). Der Kommentar dort nannte den Grund ehrlich: `white-space`
/// nur fuer diesen einen Fall aufzunehmen waere eine halbe Eigenschaft.
///
/// **Die Messung hat das widerlegt** (docs/browser-realitaet.md, dritte
/// Messung): `white-space` steht auf 4 der 10 Seiten und insgesamt
/// 110x. Es ist kein Einzelfall, sondern gehoert zum taeglichen CSS —
/// vor allem `nowrap` auf Navigationsleisten und Tabellenzellen.
///
/// Damit faellt zugleich ein Hack weg: Der Tag-Name entscheidet nicht
/// mehr, das Stylesheet tut es (`pre, textarea { white-space: pre }`
/// steht jetzt im Standard-Blatt). Das ist dieselbe Bewegung wie beim
/// Standard-Stylesheet ueberhaupt — `<h1>` ist nicht gross, WEIL es h1
/// heisst, sondern weil eine Regel es sagt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leerraum {
    /// `normal` — Leerraum wird gefaltet, es wird umgebrochen.
    Normal,
    /// `nowrap` — gefaltet, aber KEIN automatischer Umbruch.
    KeinUmbruch,
    /// `pre` — jedes Zeichen zaehlt, kein automatischer Umbruch.
    Vor,
    /// `pre-wrap` — jedes Zeichen zaehlt, Umbruch erlaubt.
    VorUmbruch,
    /// `pre-line` — Leerraum gefaltet, aber `\n` bleibt ein Umbruch.
    VorZeile,
}

impl Leerraum {
    /// Bleibt der Leerraum, wie er im Dokument steht?
    pub fn erhaelt_leerraum(self) -> bool {
        matches!(self, Leerraum::Vor | Leerraum::VorUmbruch)
    }
    /// Darf am Zeilenende automatisch umgebrochen werden?
    pub fn bricht_um(self) -> bool {
        !matches!(self, Leerraum::KeinUmbruch | Leerraum::Vor)
    }
    /// Ist ein `\n` im Text ein erzwungener Umbruch?
    pub fn zeilenumbruch_zaehlt(self) -> bool {
        matches!(
            self,
            Leerraum::Vor | Leerraum::VorUmbruch | Leerraum::VorZeile
        )
    }
}

/// `font-family`, auf das abgebildet, was wir HABEN.
///
/// EHRLICHE LAGE: SpeedOS hat genau EINE Schriftfamilie
/// (`noto-sans-mono-bitmap`, docs/schrift-groessen.md) — eine Monospace.
/// Beide Werte hier rendern deshalb heute gleich. Sie werden trotzdem
/// unterschieden, weil das LAYOUT den Unterschied schon kennen soll: Ein
/// `<pre>` in Monospace umbricht anders als Fliesstext, und wenn eine
/// Proportionalschrift dazukommt, aendert sich hier nichts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Familie {
    Proportional,
    Monospace,
}

/// `line-height`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zeilenhoehe {
    /// `normal` — der Renderer nimmt seinen Standard-Durchschuss.
    Normal,
    /// Eine nackte Zahl (`1.5`) in Tausendsteln. **Vererbt wird der
    /// FAKTOR, nicht das Ergebnis** — das ist der Unterschied zwischen
    /// `line-height: 1.5` und `line-height: 150%`, und er ist der Grund,
    /// warum es diese Variante ueberhaupt gibt.
    Faktor(i32),
    Laenge(Laenge),
}

/// `text-decoration`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dekoration {
    pub unterstrichen: bool,
    pub durchgestrichen: bool,
    pub ueberstrichen: bool,
}

/// `list-style-type`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listenzeichen {
    Keins,
    Punkt,
    Kreis,
    Quadrat,
    Dezimal,
    LateinKlein,
    LateinGross,
    RoemischKlein,
    RoemischGross,
}

/// `vertical-align` — rudimentaer, wie im Zuschnitt angekuendigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertikal {
    Grundlinie,
    Oben,
    Mitte,
    Unten,
    /// `sub`/`super` — der Versatz, den ein Renderer anwenden kann.
    Tiefgestellt,
    Hochgestellt,
}

/// `border-style` — von den zehn CSS-Stilen koennen wir zwei.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RahmenStil {
    Keiner,
    /// Alles, was eine durchgezogene Linie ist. `dashed`, `dotted`,
    /// `double` usw. werden hierauf abgebildet — eine gestrichelte Linie
    /// zu malen ist Renderer-Arbeit, und sie wegzulassen waere schlechter
    /// als sie durchgezogen zu malen (der Kasten waere sonst unsichtbar).
    Durchgezogen,
}

/// Die vier Seiten eines Kastens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kanten<T> {
    pub oben: T,
    pub rechts: T,
    pub unten: T,
    pub links: T,
}

impl<T: Copy> Kanten<T> {
    pub const fn alle(wert: T) -> Kanten<T> {
        Kanten {
            oben: wert,
            rechts: wert,
            unten: wert,
            links: wert,
        }
    }
}

// ---------------------------------------------------------------------------
// DER BERECHNETE STIL
// ---------------------------------------------------------------------------

/// Alles, was ein Element an Formatierung hat — fertig gedeutet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stil {
    pub display: Display,
    // --- geerbt ---
    pub farbe: Farbe,
    /// Schriftgroesse in 1/1000 px, IMMER absolut (computed value).
    pub schrift_px: i32,
    pub fett: bool,
    pub kursiv: bool,
    pub familie: Familie,
    pub zeilenhoehe: Zeilenhoehe,
    pub ausrichtung: Ausrichtung,
    pub listenzeichen: Listenzeichen,
    pub dekoration: Dekoration,
    pub leerraum: Leerraum,
    // --- nicht geerbt ---
    pub hintergrund: Farbe,
    pub margin: Kanten<Laenge>,
    pub padding: Kanten<Laenge>,
    pub rahmen_breite: Kanten<Laenge>,
    pub rahmen_stil: Kanten<RahmenStil>,
    pub rahmen_farbe: Kanten<Farbe>,
    pub breite: Laenge,
    pub hoehe: Laenge,
    pub max_breite: Laenge,
    pub vertikal: Vertikal,
}

/// Die Anfangswerte (`initial values`) der CSS-Spezifikation.
///
/// **NICHT die Browser-Optik** — die kommt aus dem Standard-Stylesheet
/// (`standard.rs`). Der Unterschied ist der Kern von Aufgabe 3: `<h1>` ist
/// nicht deshalb gross und fett, weil `h1` ein besonderes Element waere,
/// sondern weil ein Stylesheet es so sagt. Ohne dieses Stylesheet sieht
/// HTML aus wie unformatierter Text — und genau das ist der Beweis, dass
/// die Trennung stimmt.
pub const ANFANG: Stil = Stil {
    display: Display::Inline,
    farbe: Farbe::SCHWARZ,
    schrift_px: 16 * TAUSEND,
    fett: false,
    kursiv: false,
    familie: Familie::Proportional,
    zeilenhoehe: Zeilenhoehe::Normal,
    ausrichtung: Ausrichtung::Links,
    listenzeichen: Listenzeichen::Punkt,
    dekoration: Dekoration {
        unterstrichen: false,
        durchgestrichen: false,
        ueberstrichen: false,
    },
    leerraum: Leerraum::Normal,
    hintergrund: Farbe::DURCHSICHTIG,
    margin: Kanten::alle(Laenge::Px(0)),
    padding: Kanten::alle(Laenge::Px(0)),
    rahmen_breite: Kanten::alle(Laenge::Px(0)),
    rahmen_stil: Kanten::alle(RahmenStil::Keiner),
    rahmen_farbe: Kanten::alle(Farbe::SCHWARZ),
    breite: Laenge::Auto,
    hoehe: Laenge::Auto,
    max_breite: Laenge::Auto,
    vertikal: Vertikal::Grundlinie,
};

impl Default for Stil {
    fn default() -> Self {
        ANFANG
    }
}

impl Stil {
    /// Einen Stil fuer ein Kind vorbereiten: geerbte Felder uebernehmen,
    /// alle anderen auf den Anfangswert.
    ///
    /// ===================================================================
    /// DIE VERERBUNGSTABELLE STEHT HIER UND NUR HIER
    ///
    /// Welche Eigenschaft erbt, ist in CSS je Eigenschaft festgelegt und
    /// NICHT zu erraten. Die Faustregel („was den Text betrifft, erbt")
    /// stimmt fast immer und wird deshalb gern statt der Tabelle benutzt —
    /// bis jemand sich wundert, warum sein `margin` nicht durchschlaegt.
    ///
    /// GEERBT: color, font-*, line-height, text-align, list-style-type.
    /// NICHT GEERBT: display, background-color, margin, padding, border,
    /// width, height, max-width, vertical-align.
    ///
    /// SONDERFALL `text-decoration`: In CSS erbt sie NICHT, wird aber auf
    /// Nachfahren MITGEZEICHNET (ein `<a>` unterstreicht auch das `<b>`
    /// darin). Der Unterschied ist nur sichtbar, wenn ein Nachfahre die
    /// Dekoration selbst setzt. Wir behandeln sie als geerbt — das ist
    /// dieselbe Optik mit weniger Maschinerie, und die Abweichung steht
    /// hier.
    pub fn geerbt_von(eltern: &Stil) -> Stil {
        Stil {
            farbe: eltern.farbe,
            schrift_px: eltern.schrift_px,
            fett: eltern.fett,
            kursiv: eltern.kursiv,
            familie: eltern.familie,
            zeilenhoehe: eltern.zeilenhoehe,
            ausrichtung: eltern.ausrichtung,
            listenzeichen: eltern.listenzeichen,
            dekoration: eltern.dekoration,
            // ACHTUNG BEIM ERGAENZEN EINER GEERBTEN EIGENSCHAFT: Sie
            // gehoert an DREI Stellen — in `erbt()` (das entscheidet nur
            // ueber `unset`), in `global_setzen` (fuer `inherit`) und
            // HIER. Nur diese Liste macht die Vererbung wirklich; die
            // beiden anderen sahen bei `white-space` schon richtig aus,
            // waehrend der Wert trotzdem nicht ankam
            // (`test_white_space_wird_vererbt` hat es gefunden).
            leerraum: eltern.leerraum,
            ..ANFANG
        }
    }

    /// Die Zeilenhoehe in 1/1000 px.
    pub fn zeilenhoehe_px(&self) -> i32 {
        match self.zeilenhoehe {
            // 1,2 ist der uebliche Standard-Durchschuss.
            Zeilenhoehe::Normal => ((self.schrift_px as i64 * 1200) / 1000) as i32,
            Zeilenhoehe::Faktor(f) => ((self.schrift_px as i64 * f as i64) / TAUSEND as i64) as i32,
            Zeilenhoehe::Laenge(l) => match l.em_aufloesen(self.schrift_px) {
                Laenge::Px(p) => p,
                // Prozent bezieht sich bei line-height auf die
                // Schriftgroesse — das ist hier schon bekannt.
                Laenge::Prozent(p) => ((self.schrift_px as i64 * p as i64) / (100 * TAUSEND as i64)) as i32,
                _ => ((self.schrift_px as i64 * 1200) / 1000) as i32,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// DEKLARATIONEN ANWENDEN
// ---------------------------------------------------------------------------

/// Ob eine Eigenschaft ueberhaupt bekannt ist.
///
/// Getrennt von `anwenden`, damit `cssdump` „unbekannte Eigenschaft"
/// von „unlesbarer Wert" unterscheiden kann — beim Debuggen ist das der
/// Unterschied zwischen „koennen wir nicht" und „steht falsch da".
pub fn bekannt(name: &str) -> bool {
    matches!(
        name,
        "display"
            | "color"
            | "background-color"
            | "background"
            | "font-size"
            | "font-weight"
            | "font-style"
            | "font-family"
            | "line-height"
            | "text-align"
            | "text-decoration"
            | "text-decoration-line"
            | "white-space"
            | "list-style-type"
            | "list-style"
            | "vertical-align"
            | "width"
            | "height"
            | "max-width"
            | "margin"
            | "margin-top"
            | "margin-right"
            | "margin-bottom"
            | "margin-left"
            | "padding"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "border"
            | "border-width"
            | "border-style"
            | "border-color"
            | "border-top"
            | "border-right"
            | "border-bottom"
            | "border-left"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
    )
}

/// Erbt diese Eigenschaft? (Fuer `inherit`/`initial` und fuer `cssdump`.)
pub fn erbt(name: &str) -> bool {
    matches!(
        name,
        "color"
            | "font-size"
            | "font-weight"
            | "font-style"
            | "font-family"
            | "line-height"
            | "text-align"
            | "list-style-type"
            | "list-style"
            | "text-decoration"
            | "text-decoration-line"
            | "white-space"
    )
}

/// Wie eine Deklaration ausgegangen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ergebnis {
    /// Übernommen.
    Gesetzt,
    /// Eigenschaft kennen wir nicht.
    UnbekannteEigenschaft,
    /// Eigenschaft bekannt, Wert unlesbar — die Deklaration faellt weg.
    UnlesbarerWert,
}

/// Eine Deklaration auf einen Stil anwenden.
///
/// `eltern` wird fuer `inherit` und fuer die `em`-Aufloesung von
/// `font-size` gebraucht.
///
/// ===================================================================
/// DIE REIHENFOLGE, DIE NICHT EGAL IST
///
/// `font-size` MUSS vor allen anderen Laengen angewandt werden, denn
/// deren `em` bezieht sich auf die Schriftgroesse DIESES Elements. Bei
///
/// ```text
/// p { margin: 1em; font-size: 2em }
/// ```
///
/// ist der Rand 2 em vom ELTERN-Wert, nicht 1 em — weil `font-size`
/// zuerst gilt, egal wo es steht. Darum kuemmert sich `kaskade.rs`; hier
/// steht nur die Regel, damit sie nicht verlorengeht.
pub fn anwenden(stil: &mut Stil, name: &str, wert_text: &str, eltern: &Stil) -> Ergebnis {
    if !bekannt(name) {
        return Ergebnis::UnbekannteEigenschaft;
    }
    let wert = kleinschreiben(wert_text.trim());

    // --- Die drei globalen Schluesselwoerter ---
    //
    // `unset` gibt es auch: Es ist `inherit` fuer erbende und `initial`
    // fuer nicht erbende Eigenschaften — also genau die Fallunterscheidung,
    // die `erbt()` schon kennt.
    match wert.as_str() {
        "inherit" => return global_setzen(stil, name, eltern),
        "initial" => return global_setzen(stil, name, &ANFANG),
        "unset" => {
            let quelle = if erbt(name) { eltern } else { &ANFANG };
            return global_setzen(stil, name, quelle);
        }
        _ => {}
    }

    let gesetzt = match name {
        "display" => match wert.as_str() {
            "none" => setze(&mut stil.display, Display::Keine),
            "block" => setze(&mut stil.display, Display::Block),
            "inline" => setze(&mut stil.display, Display::Inline),
            "inline-block" => setze(&mut stil.display, Display::InlineBlock),
            "list-item" => setze(&mut stil.display, Display::Listenpunkt),
            "table" | "inline-table" => setze(&mut stil.display, Display::Tabelle),
            "table-row-group" | "table-header-group" | "table-footer-group" => {
                setze(&mut stil.display, Display::TabellenGruppe)
            }
            "table-row" => setze(&mut stil.display, Display::TabellenZeile),
            "table-cell" => setze(&mut stil.display, Display::TabellenZelle),
            // `flex`, `grid`, `contents`, … koennen wir nicht. ALS BLOCK
            // ZU BEHANDELN ist das ehrlichere Scheitern: Der Inhalt steht
            // untereinander, statt zu verschwinden (docs/browser-v1.md §3).
            "flex" | "grid" | "inline-flex" | "inline-grid" | "flow-root" => {
                setze(&mut stil.display, Display::Block)
            }
            _ => false,
        },
        "color" => match farbe_parsen(wert_text) {
            Some(f) => setze(&mut stil.farbe, f),
            None => false,
        },
        // `background` ist die Kurzform; wir lesen daraus NUR die Farbe.
        // Ein Verlauf oder ein Bild wird damit ignoriert — sichtbar
        // schlichter, nicht falsch.
        "background-color" | "background" => match farbe_parsen(wert_text) {
            Some(f) => setze(&mut stil.hintergrund, f),
            None => false,
        },
        "font-size" => schriftgroesse(stil, &wert, wert_text, eltern),
        "font-weight" => match wert.as_str() {
            "bold" | "bolder" => setze(&mut stil.fett, true),
            "normal" | "lighter" => setze(&mut stil.fett, false),
            // Zahlen: ab 600 gilt als fett (uebliche Schwelle).
            zahl => match zahl.parse::<u32>() {
                Ok(n) => setze(&mut stil.fett, n >= 600),
                Err(_) => false,
            },
        },
        "font-style" => match wert.as_str() {
            "italic" | "oblique" => setze(&mut stil.kursiv, true),
            "normal" => setze(&mut stil.kursiv, false),
            _ => false,
        },
        "font-family" => setze(&mut stil.familie, familie_waehlen(&wert)),
        "line-height" => zeilenhoehe_setzen(stil, &wert),
        "white-space" => match wert.as_str() {
            "normal" => setze(&mut stil.leerraum, Leerraum::Normal),
            "nowrap" => setze(&mut stil.leerraum, Leerraum::KeinUmbruch),
            "pre" => setze(&mut stil.leerraum, Leerraum::Vor),
            "pre-wrap" | "break-spaces" => setze(&mut stil.leerraum, Leerraum::VorUmbruch),
            "pre-line" => setze(&mut stil.leerraum, Leerraum::VorZeile),
            _ => false,
        },
        "text-align" => match wert.as_str() {
            "left" | "start" => setze(&mut stil.ausrichtung, Ausrichtung::Links),
            "center" => setze(&mut stil.ausrichtung, Ausrichtung::Mitte),
            "right" | "end" => setze(&mut stil.ausrichtung, Ausrichtung::Rechts),
            "justify" => setze(&mut stil.ausrichtung, Ausrichtung::Blocksatz),
            _ => false,
        },
        "text-decoration" | "text-decoration-line" => {
            let mut d = Dekoration::default();
            let mut erkannt = false;
            for teil in wert.split_whitespace() {
                match teil {
                    "underline" => {
                        d.unterstrichen = true;
                        erkannt = true;
                    }
                    "line-through" => {
                        d.durchgestrichen = true;
                        erkannt = true;
                    }
                    "overline" => {
                        d.ueberstrichen = true;
                        erkannt = true;
                    }
                    "none" => erkannt = true,
                    // Farbe und Stil der Linie ignorieren wir still.
                    _ => {}
                }
            }
            if erkannt {
                stil.dekoration = d;
            }
            erkannt
        }
        "list-style-type" | "list-style" => {
            let mut erkannt = false;
            for teil in wert.split_whitespace() {
                if let Some(z) = listenzeichen(teil) {
                    stil.listenzeichen = z;
                    erkannt = true;
                }
            }
            erkannt
        }
        "vertical-align" => match wert.as_str() {
            "baseline" => setze(&mut stil.vertikal, Vertikal::Grundlinie),
            "top" | "text-top" => setze(&mut stil.vertikal, Vertikal::Oben),
            "middle" => setze(&mut stil.vertikal, Vertikal::Mitte),
            "bottom" | "text-bottom" => setze(&mut stil.vertikal, Vertikal::Unten),
            "sub" => setze(&mut stil.vertikal, Vertikal::Tiefgestellt),
            "super" => setze(&mut stil.vertikal, Vertikal::Hochgestellt),
            _ => false,
        },
        "width" => laenge_feld(&mut stil.breite, wert_text),
        "height" => laenge_feld(&mut stil.hoehe, wert_text),
        "max-width" => laenge_feld(&mut stil.max_breite, wert_text),

        "margin" => kanten_kurzform(&mut stil.margin, wert_text),
        "margin-top" => laenge_feld(&mut stil.margin.oben, wert_text),
        "margin-right" => laenge_feld(&mut stil.margin.rechts, wert_text),
        "margin-bottom" => laenge_feld(&mut stil.margin.unten, wert_text),
        "margin-left" => laenge_feld(&mut stil.margin.links, wert_text),

        "padding" => kanten_kurzform(&mut stil.padding, wert_text),
        "padding-top" => laenge_feld(&mut stil.padding.oben, wert_text),
        "padding-right" => laenge_feld(&mut stil.padding.rechts, wert_text),
        "padding-bottom" => laenge_feld(&mut stil.padding.unten, wert_text),
        "padding-left" => laenge_feld(&mut stil.padding.links, wert_text),

        "border" => rahmen_kurzform(stil, wert_text, None),
        "border-top" => rahmen_kurzform(stil, wert_text, Some(0)),
        "border-right" => rahmen_kurzform(stil, wert_text, Some(1)),
        "border-bottom" => rahmen_kurzform(stil, wert_text, Some(2)),
        "border-left" => rahmen_kurzform(stil, wert_text, Some(3)),
        "border-width" => kanten_kurzform(&mut stil.rahmen_breite, wert_text),
        "border-style" => {
            let stil_wert = rahmen_stil(&wert);
            stil.rahmen_stil = Kanten::alle(stil_wert);
            true
        }
        "border-color" => match farbe_parsen(wert_text) {
            Some(f) => {
                stil.rahmen_farbe = Kanten::alle(f);
                true
            }
            None => false,
        },
        "border-top-width" => laenge_feld(&mut stil.rahmen_breite.oben, wert_text),
        "border-right-width" => laenge_feld(&mut stil.rahmen_breite.rechts, wert_text),
        "border-bottom-width" => laenge_feld(&mut stil.rahmen_breite.unten, wert_text),
        "border-left-width" => laenge_feld(&mut stil.rahmen_breite.links, wert_text),
        _ => false,
    };

    if gesetzt {
        Ergebnis::Gesetzt
    } else {
        Ergebnis::UnlesbarerWert
    }
}

/// Hilfsfunktion: setzen und `true` liefern.
#[inline]
fn setze<T>(ziel: &mut T, wert: T) -> bool {
    *ziel = wert;
    true
}

/// `inherit` / `initial` / `unset` — den Wert aus `quelle` uebernehmen.
fn global_setzen(stil: &mut Stil, name: &str, quelle: &Stil) -> Ergebnis {
    match name {
        "display" => stil.display = quelle.display,
        "color" => stil.farbe = quelle.farbe,
        "background-color" | "background" => stil.hintergrund = quelle.hintergrund,
        "font-size" => stil.schrift_px = quelle.schrift_px,
        "font-weight" => stil.fett = quelle.fett,
        "font-style" => stil.kursiv = quelle.kursiv,
        "font-family" => stil.familie = quelle.familie,
        "line-height" => stil.zeilenhoehe = quelle.zeilenhoehe,
        "text-align" => stil.ausrichtung = quelle.ausrichtung,
        "white-space" => stil.leerraum = quelle.leerraum,
        "text-decoration" | "text-decoration-line" => stil.dekoration = quelle.dekoration,
        "list-style-type" | "list-style" => stil.listenzeichen = quelle.listenzeichen,
        "vertical-align" => stil.vertikal = quelle.vertikal,
        "width" => stil.breite = quelle.breite,
        "height" => stil.hoehe = quelle.hoehe,
        "max-width" => stil.max_breite = quelle.max_breite,
        "margin" => stil.margin = quelle.margin,
        "margin-top" => stil.margin.oben = quelle.margin.oben,
        "margin-right" => stil.margin.rechts = quelle.margin.rechts,
        "margin-bottom" => stil.margin.unten = quelle.margin.unten,
        "margin-left" => stil.margin.links = quelle.margin.links,
        "padding" => stil.padding = quelle.padding,
        "padding-top" => stil.padding.oben = quelle.padding.oben,
        "padding-right" => stil.padding.rechts = quelle.padding.rechts,
        "padding-bottom" => stil.padding.unten = quelle.padding.unten,
        "padding-left" => stil.padding.links = quelle.padding.links,
        "border" | "border-width" => stil.rahmen_breite = quelle.rahmen_breite,
        "border-style" => stil.rahmen_stil = quelle.rahmen_stil,
        "border-color" => stil.rahmen_farbe = quelle.rahmen_farbe,
        _ => return Ergebnis::UnbekannteEigenschaft,
    }
    Ergebnis::Gesetzt
}

/// `font-size` — der Sonderfall, bei dem `em` und `%` sich auf den
/// ELTERNTEIL beziehen und sofort aufgeloest werden.
fn schriftgroesse(stil: &mut Stil, wert: &str, wert_text: &str, eltern: &Stil) -> bool {
    // Die absoluten Schluesselwoerter, bezogen auf 16 px.
    let benannt = match wert {
        "xx-small" => Some(9 * TAUSEND),
        "x-small" => Some(10 * TAUSEND),
        "small" => Some(13 * TAUSEND),
        "medium" => Some(16 * TAUSEND),
        "large" => Some(18 * TAUSEND),
        "x-large" => Some(24 * TAUSEND),
        "xx-large" => Some(32 * TAUSEND),
        // Relativ zum Elternteil.
        "larger" => Some((eltern.schrift_px as i64 * 1200 / 1000) as i32),
        "smaller" => Some((eltern.schrift_px as i64 * 1000 / 1200) as i32),
        _ => None,
    };
    if let Some(px) = benannt {
        stil.schrift_px = px;
        return true;
    }
    match laenge_parsen(wert_text) {
        Some(Laenge::Px(p)) => {
            stil.schrift_px = p;
            true
        }
        // `em` und `%` beziehen sich hier auf die ELTERN-Groesse — das ist
        // die eine Stelle, an der das gilt.
        Some(Laenge::Em(e)) => {
            stil.schrift_px = ((e as i64 * eltern.schrift_px as i64) / TAUSEND as i64) as i32;
            true
        }
        Some(Laenge::Prozent(p)) => {
            stil.schrift_px =
                ((p as i64 * eltern.schrift_px as i64) / (100 * TAUSEND as i64)) as i32;
            true
        }
        _ => false,
    }
}

fn zeilenhoehe_setzen(stil: &mut Stil, wert: &str) -> bool {
    if wert == "normal" {
        stil.zeilenhoehe = Zeilenhoehe::Normal;
        return true;
    }
    // Eine NACKTE Zahl ist ein Faktor und wird als solcher vererbt —
    // deshalb muss sie VOR `laenge_parsen` geprueft werden (das eine
    // nackte Zahl nur als 0 durchliesse).
    if !wert.ends_with('%')
        && !wert.ends_with("px")
        && !wert.ends_with("em")
        && !wert.ends_with("pt")
    {
        if let Some(faktor) = zahl_tausendstel(wert) {
            stil.zeilenhoehe = Zeilenhoehe::Faktor(faktor);
            return true;
        }
    }
    match laenge_parsen(wert) {
        Some(l) => {
            stil.zeilenhoehe = Zeilenhoehe::Laenge(l);
            true
        }
        None => false,
    }
}

/// `font-family: Georgia, "Times New Roman", serif` — auf unsere EINE
/// Familie abbilden.
///
/// Gewaehlt wird die ERSTE Angabe, die wir einordnen koennen; wird keine
/// erkannt, gilt Proportional. Ein Verlass auf den letzten (generischen)
/// Eintrag waere falsch: `font-family: "Courier New", sans-serif` meint
/// Monospace.
fn familie_waehlen(wert: &str) -> Familie {
    for teil in wert.split(',') {
        let name = teil.trim().trim_matches(['"', '\'']);
        match name {
            "monospace" | "courier" | "courier new" | "consolas" | "menlo" | "monaco"
            | "ui-monospace" => return Familie::Monospace,
            "serif" | "sans-serif" | "cursive" | "fantasy" | "system-ui" | "ui-sans-serif"
            | "arial" | "helvetica" | "georgia" | "times" | "times new roman" | "verdana" => {
                return Familie::Proportional
            }
            _ => {}
        }
    }
    Familie::Proportional
}

fn listenzeichen(wert: &str) -> Option<Listenzeichen> {
    Some(match wert {
        "none" => Listenzeichen::Keins,
        "disc" => Listenzeichen::Punkt,
        "circle" => Listenzeichen::Kreis,
        "square" => Listenzeichen::Quadrat,
        "decimal" => Listenzeichen::Dezimal,
        "lower-alpha" | "lower-latin" => Listenzeichen::LateinKlein,
        "upper-alpha" | "upper-latin" => Listenzeichen::LateinGross,
        "lower-roman" => Listenzeichen::RoemischKlein,
        "upper-roman" => Listenzeichen::RoemischGross,
        _ => return None,
    })
}

fn rahmen_stil(wert: &str) -> RahmenStil {
    match wert {
        "none" | "hidden" => RahmenStil::Keiner,
        _ => RahmenStil::Durchgezogen,
    }
}

fn laenge_feld(ziel: &mut Laenge, wert_text: &str) -> bool {
    match laenge_parsen(wert_text) {
        Some(l) => {
            *ziel = l;
            true
        }
        None => false,
    }
}

/// `margin: 1px`, `1px 2px`, `1px 2px 3px`, `1px 2px 3px 4px` — die
/// CSS-Kurzform mit ihrer Uhrzeiger-Regel.
fn kanten_kurzform(ziel: &mut Kanten<Laenge>, wert_text: &str) -> bool {
    let teile: Vec<Laenge> = wert_text
        .split_whitespace()
        .filter_map(laenge_parsen)
        .collect();
    match teile.len() {
        1 => *ziel = Kanten::alle(teile[0]),
        2 => {
            *ziel = Kanten {
                oben: teile[0],
                unten: teile[0],
                rechts: teile[1],
                links: teile[1],
            }
        }
        3 => {
            *ziel = Kanten {
                oben: teile[0],
                rechts: teile[1],
                links: teile[1],
                unten: teile[2],
            }
        }
        4 => {
            *ziel = Kanten {
                oben: teile[0],
                rechts: teile[1],
                unten: teile[2],
                links: teile[3],
            }
        }
        _ => return false,
    }
    true
}

/// `border: 1px solid red` — in beliebiger Reihenfolge, alle Teile
/// optional. `seite` = None heisst alle vier.
fn rahmen_kurzform(stil: &mut Stil, wert_text: &str, seite: Option<usize>) -> bool {
    let mut breite = None;
    let mut linien_stil = None;
    let mut farbe = None;

    for teil in wert_text.split_whitespace() {
        let klein = kleinschreiben(teil);
        match klein.as_str() {
            "none" | "hidden" => linien_stil = Some(RahmenStil::Keiner),
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset"
            | "outset" => linien_stil = Some(RahmenStil::Durchgezogen),
            "thin" => breite = Some(Laenge::px(1)),
            "medium" => breite = Some(Laenge::px(3)),
            "thick" => breite = Some(Laenge::px(5)),
            _ => {
                if let Some(l) = laenge_parsen(teil) {
                    breite = Some(l);
                } else if let Some(f) = farbe_parsen(teil) {
                    farbe = Some(f);
                }
            }
        }
    }
    if breite.is_none() && linien_stil.is_none() && farbe.is_none() {
        return false;
    }

    // DIE FALLE, DIE JEDER EINMAL BAUT: `border: solid red` ohne Breite
    // bedeutet `medium`, also 3 px — NICHT 0. Ein Rahmen, der stillschweigend
    // 0 breit ist, ist unsichtbar, und man sucht ihn im Renderer.
    if linien_stil.is_some() && breite.is_none() {
        breite = Some(Laenge::px(3));
    }
    // Umgekehrt: Eine Breite ohne Stil bleibt unsichtbar (so die
    // Spezifikation — `border-style` ist standardmaessig `none`).

    let mut setzen = |i: usize| {
        if let Some(b) = breite {
            match i {
                0 => stil.rahmen_breite.oben = b,
                1 => stil.rahmen_breite.rechts = b,
                2 => stil.rahmen_breite.unten = b,
                _ => stil.rahmen_breite.links = b,
            }
        }
        if let Some(s) = linien_stil {
            match i {
                0 => stil.rahmen_stil.oben = s,
                1 => stil.rahmen_stil.rechts = s,
                2 => stil.rahmen_stil.unten = s,
                _ => stil.rahmen_stil.links = s,
            }
        }
        if let Some(f) = farbe {
            match i {
                0 => stil.rahmen_farbe.oben = f,
                1 => stil.rahmen_farbe.rechts = f,
                2 => stil.rahmen_farbe.unten = f,
                _ => stil.rahmen_farbe.links = f,
            }
        }
    };
    match seite {
        Some(i) => setzen(i),
        None => {
            for i in 0..4 {
                setzen(i);
            }
        }
    }
    true
}

/// Einen berechneten Wert als Text — fuer `cssdump`.
pub fn wert_als_text(stil: &Stil, name: &str) -> String {
    use alloc::format;
    let laenge = |l: Laenge| -> String {
        match l {
            Laenge::Auto => String::from("auto"),
            Laenge::Px(t) => format!("{}px", tausendstel_text(t)),
            Laenge::Em(t) => format!("{}em", tausendstel_text(t)),
            Laenge::Prozent(t) => format!("{}%", tausendstel_text(t)),
        }
    };
    let farbe = |f: Farbe| -> String {
        if f.a == 0 {
            String::from("transparent")
        } else if f.a == 255 {
            format!("#{:02x}{:02x}{:02x}", f.r, f.g, f.b)
        } else {
            format!("rgba({}, {}, {}, {})", f.r, f.g, f.b, f.a)
        }
    };
    let kanten = |k: Kanten<Laenge>| -> String {
        format!(
            "{} {} {} {}",
            laenge(k.oben),
            laenge(k.rechts),
            laenge(k.unten),
            laenge(k.links)
        )
    };

    match name {
        "display" => format!("{:?}", stil.display),
        "color" => farbe(stil.farbe),
        "background-color" => farbe(stil.hintergrund),
        "font-size" => format!("{}px", tausendstel_text(stil.schrift_px)),
        "font-weight" => String::from(if stil.fett { "bold" } else { "normal" }),
        "font-style" => String::from(if stil.kursiv { "italic" } else { "normal" }),
        "font-family" => format!("{:?}", stil.familie),
        "line-height" => match stil.zeilenhoehe {
            Zeilenhoehe::Normal => String::from("normal"),
            Zeilenhoehe::Faktor(f) => tausendstel_text(f),
            Zeilenhoehe::Laenge(l) => laenge(l),
        },
        "text-align" => format!("{:?}", stil.ausrichtung),
        "white-space" => format!("{:?}", stil.leerraum),
        "text-decoration" => format!("{:?}", stil.dekoration),
        "list-style-type" => format!("{:?}", stil.listenzeichen),
        "vertical-align" => format!("{:?}", stil.vertikal),
        "width" => laenge(stil.breite),
        "height" => laenge(stil.hoehe),
        "max-width" => laenge(stil.max_breite),
        "margin" => kanten(stil.margin),
        "padding" => kanten(stil.padding),
        "border-width" => kanten(stil.rahmen_breite),
        "border-style" => format!("{:?}", stil.rahmen_stil.oben),
        "border-color" => farbe(stil.rahmen_farbe.oben),
        _ => String::from("?"),
    }
}

/// Tausendstel lesbar machen: 16000 -> "16", 1500 -> "1.5".
pub fn tausendstel_text(t: i32) -> String {
    use alloc::format;
    let ganz = t / TAUSEND;
    let rest = (t % TAUSEND).abs();
    if rest == 0 {
        return format!("{}", ganz);
    }
    // Nachkommastellen ohne Nullen am Ende.
    let mut bruch = format!("{:03}", rest);
    while bruch.ends_with('0') {
        bruch.pop();
    }
    let vorzeichen = if t < 0 && ganz == 0 { "-" } else { "" };
    format!("{}{}.{}", vorzeichen, ganz, bruch)
}

/// Die Eigenschaften, die `cssdump` anzeigt — in sinnvoller Reihenfolge.
pub static ANZEIGE_REIHENFOLGE: &[&str] = &[
    "display",
    "color",
    "background-color",
    "font-size",
    "font-weight",
    "font-style",
    "font-family",
    "line-height",
    "text-align",
    "text-decoration",
    "list-style-type",
    "vertical-align",
    "width",
    "height",
    "max-width",
    "margin",
    "padding",
    "border-width",
    "border-style",
    "border-color",
];
