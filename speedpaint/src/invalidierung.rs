// speedpaint::invalidierung — was muss nach einem Ereignis wirklich getan
// werden?
//
// ===========================================================================
// DIE TEUERSTE FRAGE EINES BROWSERS
//
// Ein Layout ueber einen langen Artikel kostet zweistellige
// Millisekunden, ein Vollbild-Malen bei 4K ebenfalls, ein Streifen ein
// paar Dutzend Mikrosekunden. Zwischen der bequemen Antwort („nach jedem
// Ereignis alles neu") und der richtigen liegen zwei Groessenordnungen —
// und zwar genau die zwei, die ueber fluessiges Scrollen entscheiden.
//
// Deshalb steht die Entscheidung hier als REINE FUNKTION auf einem Enum
// und nicht als verstreute `if`-Kette in der Ereignisschleife: So ist
// jede einzelne Regel ein Testfall, und eine neue Regel kann nicht
// versehentlich eine alte ueberschreiben.
//
// ===========================================================================
// DIE REGELN, UND WARUM SIE SO LAUTEN
//
// (1) FENSTERBREITE GEAENDERT -> NEU LAYOUTEN.
//     Das Layout haengt an genau einer Zahl von aussen: der verfuegbaren
//     Breite (`speedlayout::setzen(.., breite, ..)`). Aendert sie sich,
//     brechen alle Zeilen anders um, und die Dokumenthoehe ist eine
//     andere. Daran fuehrt nichts vorbei.
//
// (2) NUR DIE FENSTERHOEHE GEAENDERT -> NICHT layouten, aber voll malen.
//     Das ist die Regel, die man uebersieht. Die Hoehe geht in kein
//     Layout ein — ein Dokument, das 4000 px hoch ist, ist das in einem
//     300 px und in einem 900 px hohen Fenster. Neu zu layouten waere
//     reine Rechenzeit fuer ein identisches Ergebnis. Was NICHT entfaellt,
//     ist das Malen: Nach einer Groessenaenderung ist der Fensterpuffer
//     des Kernels NEU UND LEER (docs/fenster-syscalls.md) — wer nicht
//     malt, sieht Schwarz.
//
// (3) SCROLLEN -> NIEMALS layouten, nur den neuen Streifen malen.
//     Der Kern von Aufgabe 2. Der Versatz ist eine Anzeige-Groesse; kein
//     Kasten aendert dabei seine Koordinaten.
//
// (4) BILD FERTIG GELADEN -> nur sein Rechteck neu malen, NIE layouten.
//     Das gilt bei uns AUSNAHMSLOS, und der Grund ist nachpruefbar:
//     `speedlayout` fragt ein Bild nie nach seiner Groesse. Der Kasten
//     entsteht aus `width`/`height` (Stil schlaegt Attribut) oder — wenn
//     beides fehlt — aus einem festen Platzhalter von 32x32
//     (`inline.rs`). Ein ankommendes Bild kann die Geometrie also gar
//     nicht veraendern.
//     DER PREIS, ehrlich benannt und in docs/grenzen.md eingetragen: Ein
//     `<img>` ohne Massangabe wird in 32x32 gequetscht. Echte Browser
//     layouten nach dem Laden neu; das kostet den beruechtigten
//     Seitensprung und braucht eine Layout-Runde je Bild.
//
// (5) NEUE SEITE / NEUES STYLESHEET -> alles von vorn.
//
// (6) THEMA GEAENDERT -> voll malen, nicht layouten. Farben aendern keine
//     Masse; unser Layout kennt das Thema gar nicht.

use speedui::Rechteck;

/// Was passiert ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anlass {
    /// Das Fenster hat eine neue Inhaltsgroesse bekommen.
    FensterGroesse {
        alte_breite: i32,
        alte_hoehe: i32,
        neue_breite: i32,
        neue_hoehe: i32,
    },
    /// Es wurde gescrollt. `streifen` ist der neu sichtbare Rand
    /// (`Sicht::scrollen` liefert ihn), `alles` heisst „Sprung zu gross".
    Scrollen {
        streifen: Option<Rechteck>,
        alles: bool,
    },
    /// Ein Bild ist eingetroffen. `bereich` ist sein Rechteck auf der
    /// Leinwand.
    BildGeladen { bereich: Rechteck },
    /// Eine andere Seite wurde geladen.
    NeueSeite,
    /// Das Erscheinungsbild hat sich geaendert.
    ThemaGeaendert,
    /// Das Fenster hat den Fokus gewechselt o. Ae. — nichts Sichtbares.
    Nichts,
}

/// Was zu tun ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Massnahme {
    /// Gar nichts.
    Nichts,
    /// Nur diesen Ausschnitt neu malen (Leinwand-Koordinaten).
    Teil(Rechteck),
    /// Die ganze Sichtflaeche neu malen. Layout bleibt gueltig.
    Alles,
    /// Erst neu layouten, dann alles neu malen.
    NeuLayouten,
}

impl Massnahme {
    /// Der Rang einer Massnahme — je hoeher, desto teurer.
    ///
    /// EINE EIGENE FUNKTION UND KEIN `#[derive(Ord)]`: Ein abgeleitetes
    /// `Ord` haette die Rangfolge an die Reihenfolge der Varianten
    /// geknuepft, und wer eine neue dazwischenschreibt, aenderte die
    /// Bedeutung von `verstaerken`, ohne es zu merken. (Ausserdem traegt
    /// `Teil` ein `Rechteck`, und Rechtecke haben keine sinnvolle
    /// Ordnung — der Compiler hat hier also nur denselben Einwand
    /// gehabt.)
    fn rang(&self) -> u8 {
        match self {
            Massnahme::Nichts => 0,
            Massnahme::Teil(_) => 1,
            Massnahme::Alles => 2,
            Massnahme::NeuLayouten => 3,
        }
    }

    /// Muss das Dokument neu gesetzt werden?
    pub fn layoutet(&self) -> bool {
        matches!(self, Massnahme::NeuLayouten)
    }

    /// Muss ueberhaupt gemalt werden?
    pub fn malt(&self) -> bool {
        !matches!(self, Massnahme::Nichts)
    }

    /// Der zu malende Ausschnitt — `None` heisst „die ganze Sicht".
    pub fn ausschnitt(&self) -> Option<Rechteck> {
        match self {
            Massnahme::Teil(rechteck) => Some(*rechteck),
            _ => None,
        }
    }

    /// Zwei Massnahmen zusammenfassen: die teurere gewinnt.
    ///
    /// Zwei Teil-Ausschnitte werden zu ihrer Bounding-Box vereinigt
    /// (KORREKTHEIT VOR OPTIMUM — dieselbe Entscheidung wie bei der
    /// Widget-Schadensmeldung in Serie 3). Zu viel zu malen ist eine
    /// Verschwendung, zu wenig ist ein Anzeigefehler.
    pub fn verstaerken(self, andere: Massnahme) -> Massnahme {
        match (self, andere) {
            (Massnahme::Teil(a), Massnahme::Teil(b)) => Massnahme::Teil(a.umschliessen(&b)),
            (a, b) => {
                if a.rang() >= b.rang() {
                    a
                } else {
                    b
                }
            }
        }
    }
}

/// Die Regeln. Eine reine Funktion — deshalb ist jede Regel ein Testfall.
pub fn entscheiden(anlass: Anlass) -> Massnahme {
    match anlass {
        Anlass::FensterGroesse {
            alte_breite,
            neue_breite,
            alte_hoehe,
            neue_hoehe,
        } => {
            if neue_breite != alte_breite {
                // Regel (1)
                Massnahme::NeuLayouten
            } else if neue_hoehe != alte_hoehe {
                // Regel (2): kein Layout, aber der Puffer ist leer.
                Massnahme::Alles
            } else {
                // Ein „Groesse"-Ereignis ohne Groessenaenderung kommt vor
                // (der Fenstermanager schickt es beim Wiederherstellen).
                // Der Puffer ist trotzdem neu — also malen.
                Massnahme::Alles
            }
        }
        // Regel (3)
        Anlass::Scrollen { streifen, alles } => match (alles, streifen) {
            (true, _) => Massnahme::Alles,
            (false, Some(streifen)) => Massnahme::Teil(streifen),
            (false, None) => Massnahme::Nichts,
        },
        // Regel (4)
        Anlass::BildGeladen { bereich } => {
            if bereich.breite <= 0 || bereich.hoehe <= 0 {
                Massnahme::Nichts
            } else {
                Massnahme::Teil(bereich)
            }
        }
        // Regel (5)
        Anlass::NeueSeite => Massnahme::NeuLayouten,
        // Regel (6)
        Anlass::ThemaGeaendert => Massnahme::Alles,
        Anlass::Nichts => Massnahme::Nichts,
    }
}
