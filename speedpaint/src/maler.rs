// speedpaint::maler — Befehle in eine Flaeche malen
//
// ===========================================================================
// DER GANZE MALER IST EINE SCHLEIFE UEBER EINE LISTE
//
// Genau das war der Sinn der Entscheidung aus Teil 6, das Layout in
// BEFEHLE muenden zu lassen: Der Renderer hat kein Layout-Wissen, keinen
// Zustand ueber den Aufruf hinaus und keine Rekursion. Er nimmt einen
// Befehl, rechnet den Scroll-Versatz drauf und ruft eine Zeichenfunktion.
//
// Wer hier etwas ueber Kaesten, Zeilen oder Vererbung liest, hat einen
// Fehler gefunden: Das gehoert eine Schicht tiefer.
//
// ===========================================================================
// ZWEI DINGE, DIE DER MALER NICHT SELBST MACHT
//
//  1. **Bilder holen.** `Befehl::Bild` traegt eine QUELLE, keine Pixel.
//     Der Maler fragt eine `Bildquelle`; ist das Bild noch nicht da, malt
//     er den Platzhalter und ZAEHLT es. Ein Renderer, der auf ein Bild
//     wartet, haelt die ganze Seite an.
//  2. **Clippen.** Das kann die Leinwand besser (beide Wirte haben dafuer
//     Schnellpfade). Der Maler SETZT das Clip und stellt es hinterher
//     wieder her — er verlaesst sich aber nicht darauf: Was sicher
//     ausserhalb liegt, wird gar nicht erst uebergeben. Das ist derselbe
//     4K-Schnellpfad, den der Text-Editor in Serie 3 bekommen hat.

use crate::sicht::Sicht;
use crate::{farbe_nach_ui, rechteck_nach_ui};
use speedlayout::{Anzeigeliste, Befehl};
use speedui::{Farbe, Leinwand, Rechteck, Stil};

// ---------------------------------------------------------------------------
// BILDER
// ---------------------------------------------------------------------------

/// Ein dekodiertes Bild: RGBA, 4 Byte je Pixel.
///
/// Dasselbe Format, das `libspeed::bild` liefert (Serie 8, Teil 3) — und
/// zwar als `&[u8]` und nicht als `&[u32]`, damit hier nichts umgedeutet
/// werden muss. Diese Kiste hat NULL unsafe-Bloecke, und das soll so
/// bleiben.
#[derive(Debug, Clone, Copy)]
pub struct Bild<'a> {
    pub breite: i32,
    pub hoehe: i32,
    pub rgba: &'a [u8],
}

/// Woher der Maler die Pixel eines `<img>` bekommt.
///
/// Ein Trait und kein `HashMap`-Argument: Der Browser haelt seine Bilder,
/// wie er will (Liste, Cache mit Verfallsdatum, gar nicht), und der Maler
/// stellt genau eine Frage.
pub trait Bildquelle {
    /// Die Pixel zu dieser Quelle — oder `None`, wenn (noch) nicht da.
    fn bild(&self, quelle: &str) -> Option<Bild<'_>>;
}

/// Eine Bildquelle, die nie ein Bild hat.
///
/// Fuer Tests und fuer den Fall „Bilder abgeschaltet". Dass es sie gibt,
/// erspart jedem Aufrufer, der keine Bilder will, eine eigene Attrappe.
pub struct OhneBilder;

impl Bildquelle for OhneBilder {
    fn bild(&self, _quelle: &str) -> Option<Bild<'_>> {
        None
    }
}

// ---------------------------------------------------------------------------
// DER AUFTRAG
// ---------------------------------------------------------------------------

/// Was gemalt werden soll.
///
/// Ein Struct statt sechs Argumenten — und zwar eines mit `&`-Referenz
/// auf die Anzeigeliste. Damit ist im TYP festgehalten, dass Malen die
/// Liste nicht anfassen kann; „Scrollen layoutet nicht neu" ist keine
/// Absichtserklaerung, sondern eine Signatur.
pub struct Auftrag<'a> {
    pub liste: &'a Anzeigeliste,
    pub sicht: &'a Sicht,
    /// Der Ausschnitt der Leinwand, der neu gemalt wird. Beim Scrollen
    /// ist das nur ein Streifen (`Sicht::scrollen` liefert ihn).
    pub streifen: Rechteck,
    /// Die Seitenfarbe. Wird VOR den Befehlen in den Streifen gefuellt —
    /// ohne das stuenden beim Scrollen die alten Pixel unter dem neuen
    /// Text.
    pub hintergrund: Farbe,
}

/// Was beim Malen anfiel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MalBefund {
    /// Befehle, die wirklich gezeichnet wurden.
    pub gemalt: u32,
    /// Befehle, die ausserhalb des Streifens lagen.
    pub uebersprungen: u32,
    /// `<img>`, deren Pixel noch nicht da waren (Platzhalter gemalt).
    pub bilder_fehlend: u32,
    /// Bilder, die gezeichnet wurden.
    pub bilder_gemalt: u32,
}

// ---------------------------------------------------------------------------
// MALEN
// ---------------------------------------------------------------------------

/// Die Befehle eines Auftrags auf die Leinwand bringen.
///
/// **PANICKT NIE** — wie alles in diesem Fundament. Ein Befehl mit
/// unsinnigen Koordinaten wird uebersprungen, ein Bild mit zu kurzem
/// Puffer als Platzhalter gemalt.
pub fn malen(auftrag: &Auftrag, flaeche: &mut dyn Leinwand, bilder: &dyn Bildquelle) -> MalBefund {
    let mut befund = MalBefund::default();

    // Der Streifen kann nur innerhalb des Inhaltsbereichs liegen — sonst
    // malte ein Browser ueber seine eigene Statuszeile.
    let Some(ziel) = auftrag.streifen.schneiden(&auftrag.sicht.bereich) else {
        return befund;
    };

    // Das alte Clip merken und WIEDERHERSTELLEN: Die Leinwand gehoert dem
    // Aufrufer, nicht uns. Wer das vergisst, malt beim naechsten Aufruf
    // eines anderen Widgets in einen Ausschnitt, den niemand gesetzt hat.
    let vorheriges_clip = flaeche.clip();
    let clip = match vorheriges_clip {
        Some(alt) => match alt.schneiden(&ziel) {
            Some(geschnitten) => geschnitten,
            None => return befund,
        },
        None => ziel,
    };
    flaeche.clip_setzen(Some(clip));

    // Der Hintergrund zuerst — er ist der Grund, warum ein Streifen
    // ueberhaupt genuegt.
    flaeche.fuellen(clip, auftrag.hintergrund);

    // Der sichtbare Streifen in DOKUMENT-Koordinaten. Die Auswahl laeuft
    // ueber diese Spanne und nicht ueber das Clip: ein Vergleich zweier
    // Zahlen je Befehl, ohne Umrechnung.
    let oben = auftrag.sicht.nach_dokument_y(clip.y);
    let unten = oben + clip.hoehe;

    for befehl in &auftrag.liste.befehle {
        let bereich = befehl.bereich();
        // NUR SENKRECHT PRUEFEN. Waagerecht waere falsch: Ein Textbefehl
        // meldet seine Breite als 0 (die kennt nur die Schrift des
        // Wirts), und er wuerde weggeworfen. Das Clip der Leinwand
        // erledigt die zweite Achse genau.
        if bereich.unten() < oben || bereich.y > unten {
            befund.uebersprungen += 1;
            continue;
        }
        befehl_malen(befehl, auftrag.sicht, flaeche, bilder, &mut befund);
    }

    flaeche.clip_setzen(vorheriges_clip);
    befund
}

fn befehl_malen(
    befehl: &Befehl,
    sicht: &Sicht,
    flaeche: &mut dyn Leinwand,
    bilder: &dyn Bildquelle,
    befund: &mut MalBefund,
) {
    match befehl {
        Befehl::Rechteck { rechteck, farbe } => {
            // Vollstaendig durchsichtig = nichts zu tun. Der Aufruf waere
            // nicht falsch, nur Arbeit ohne Wirkung — und in einem
            // Mal-Protokoll waere er Laerm.
            if farbe.ist_durchsichtig() || rechteck.ist_leer() {
                befund.uebersprungen += 1;
                return;
            }
            flaeche.fuellen(versetzt(*rechteck, sicht), farbe_nach_ui(*farbe));
            befund.gemalt += 1;
        }
        Befehl::Text {
            x,
            y,
            text,
            groesse,
            fett,
            kursiv,
            farbe,
            ..
        } => {
            if farbe.ist_durchsichtig() || text.is_empty() {
                befund.uebersprungen += 1;
                return;
            }
            flaeche.text_stil(
                x + sicht.bereich.x,
                sicht.nach_leinwand_y(*y),
                text,
                *groesse,
                Stil {
                    fett: *fett,
                    kursiv: *kursiv,
                },
                farbe_nach_ui(*farbe),
            );
            befund.gemalt += 1;
        }
        Befehl::Linie {
            x0,
            y0,
            x1,
            y1,
            dicke,
            farbe,
        } => {
            if farbe.ist_durchsichtig() {
                befund.uebersprungen += 1;
                return;
            }
            linie_malen((*x0, *y0), (*x1, *y1), *dicke, *farbe, sicht, flaeche);
            befund.gemalt += 1;
        }
        Befehl::Bild {
            rechteck,
            quelle,
            alt,
            ..
        } => {
            bild_malen(*rechteck, quelle, alt, sicht, flaeche, bilder, befund);
        }
    }
}

/// Ein Layout-Rechteck an seinen Platz auf der Leinwand.
#[inline]
fn versetzt(rechteck: speedlayout::Rechteck, sicht: &Sicht) -> Rechteck {
    let mut ui = rechteck_nach_ui(rechteck);
    ui.x += sicht.bereich.x;
    ui.y = sicht.nach_leinwand_y(rechteck.y);
    ui
}

/// Eine Linie mit Dicke.
///
/// WAAGERECHT UND SENKRECHT WERDEN ZU RECHTECKEN, und das ist kein
/// Trick: `Leinwand::linie` kennt keine Dicke (kein Widget braucht eine),
/// und alles, was das Layout an Linien erzeugt — Unterstreichung,
/// Durchstreichung, `<hr>` —, ist achsenparallel. Eine schraege Linie
/// kann bei uns gar nicht entstehen; kaeme doch eine, wird sie duenn
/// gezeichnet statt gar nicht.
fn linie_malen(
    von: (i32, i32),
    bis: (i32, i32),
    dicke: i32,
    farbe: speedcss::Farbe,
    sicht: &Sicht,
    flaeche: &mut dyn Leinwand,
) {
    let ((x0, y0), (x1, y1)) = (von, bis);
    let dicke = dicke.max(1);
    let ui_farbe = farbe_nach_ui(farbe);
    let vx = sicht.bereich.x;
    if y0 == y1 {
        let x = x0.min(x1) + vx;
        let breite = (x1 - x0).abs().max(1);
        flaeche.fuellen(
            Rechteck::neu(x, sicht.nach_leinwand_y(y0), breite, dicke),
            ui_farbe,
        );
    } else if x0 == x1 {
        let y = y0.min(y1);
        let hoehe = (y1 - y0).abs().max(1);
        flaeche.fuellen(
            Rechteck::neu(x0 + vx, sicht.nach_leinwand_y(y), dicke, hoehe),
            ui_farbe,
        );
    } else {
        flaeche.linie(
            x0 + vx,
            sicht.nach_leinwand_y(y0),
            x1 + vx,
            sicht.nach_leinwand_y(y1),
            ui_farbe,
        );
    }
}

/// Die Farben des Platzhalters — ein Rahmen, damit ein fehlendes Bild
/// SICHTBAR fehlt.
///
/// Dieselbe Haltung wie beim Magenta der Kernel-Icons und beim vollen
/// Kaesten des 5x7-Rasters: Was nicht da ist, soll man sehen. Ein
/// unsichtbar fehlendes Bild ist eine Seite, die stillschweigend anders
/// aussieht als gemeint.
pub const PLATZHALTER_RAHMEN: Farbe = Farbe::neu(150, 150, 150);
pub const PLATZHALTER_TEXT: Farbe = Farbe::neu(110, 110, 110);
/// Schriftgroesse des Alternativtexts im Platzhalter.
pub const PLATZHALTER_SCHRIFT: i32 = 12;

fn bild_malen(
    rechteck: speedlayout::Rechteck,
    quelle: &str,
    alt: &str,
    sicht: &Sicht,
    flaeche: &mut dyn Leinwand,
    bilder: &dyn Bildquelle,
    befund: &mut MalBefund,
) {
    let ziel = versetzt(rechteck, sicht);
    if ziel.breite <= 0 || ziel.hoehe <= 0 {
        befund.uebersprungen += 1;
        return;
    }
    if let Some(bild) = bilder.bild(quelle) {
        // Die Laenge PRUEFEN und nicht vertrauen: Die Bildquelle ist
        // Fremdcode aus Sicht dieser Kiste, und ein zu kurzer Puffer
        // waere sonst ein Absturz mitten im Malen.
        let gebraucht = (bild.breite as i64) * (bild.hoehe as i64) * 4;
        if bild.breite > 0 && bild.hoehe > 0 && bild.rgba.len() as i64 >= gebraucht {
            flaeche.bild(ziel, bild.breite, bild.hoehe, bild.rgba);
            befund.bilder_gemalt += 1;
            befund.gemalt += 1;
            return;
        }
    }
    // Platzhalter: Rahmen plus Alternativtext.
    flaeche.rahmen(ziel, PLATZHALTER_RAHMEN);
    if !alt.is_empty() {
        flaeche.text(
            ziel.x + 4,
            ziel.y + 4,
            alt,
            PLATZHALTER_SCHRIFT,
            false,
            PLATZHALTER_TEXT,
        );
    }
    befund.bilder_fehlend += 1;
    befund.gemalt += 1;
}
