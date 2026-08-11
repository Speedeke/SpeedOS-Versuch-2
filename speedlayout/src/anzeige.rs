// speedlayout::anzeige — das Ergebnis: eine Liste von Befehlen
//
// ===========================================================================
// WARUM EINE LISTE UND KEIN BILD
//
// Das Layout koennte auch direkt zeichnen. Es tut es nicht, und das ist
// die wichtigste Entscheidung dieses Teils:
//
//   * **Testbarkeit.** „Rechteck bei (10,20), 100x30, blau" ist ein
//     Datenwert. Ihn zu pruefen kostet ein `assert_eq!`. Ein Layout, das
//     Pixel malt, laesst sich nur mit Bildschirmfotos pruefen — und was
//     man nur fotografieren kann, wird nicht geprueft. Die 40 Tests
//     dieser Kiste gaebe es sonst nicht.
//   * **Der Renderer wird trivial.** Er laeuft die Liste durch und ruft
//     je Befehl eine Zeichenfunktion. Kein Layout-Wissen, kein Zustand.
//   * **Die Liste ist wiederverwendbar.** Beim Scrollen aendert sich
//     nichts an ihr — nur der Ausschnitt.
//
// ===========================================================================
// DIE REIHENFOLGE IST DIE MALREIHENFOLGE
//
// Erst der Hintergrund eines Kastens, dann sein Rahmen, dann seine
// Kinder. Wer das umdreht, uebermalt den Inhalt mit dem Hintergrund des
// naechsten Kastens.
//
// Eine echte Malreihenfolge (`z-index`, Stapelkontexte) gibt es nicht —
// sie braeuchte `position`, und das steht nicht im Zuschnitt.

use crate::kasten::{Kasten, KastenArt, Rechteck};
use alloc::string::String;
use alloc::vec::Vec;
use speedcss::stil::RahmenStil;
use speedcss::{Farbe, Stil};
use speedhtml::KnotenId;

/// Ein Anzeige-Befehl mit ABSOLUTEN Koordinaten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Befehl {
    /// Eine gefuellte Flaeche.
    Rechteck { rechteck: Rechteck, farbe: Farbe },
    /// Ein Textstueck. `x`/`y` ist die linke OBERE Ecke — das ist, was
    /// unser Zeichner erwartet (er setzt Glyphen von links oben).
    Text {
        x: i32,
        y: i32,
        text: String,
        groesse: i32,
        fett: bool,
        kursiv: bool,
        farbe: Farbe,
        /// Fuer Klick-Ziele und Debugging.
        knoten: Option<KnotenId>,
    },
    /// Ein Bild. Die BYTES holt der Aufrufer — diese Kiste kennt keine
    /// Bilder, nur ihre Quelle und ihr Rechteck.
    Bild {
        rechteck: Rechteck,
        quelle: String,
        /// Anzuzeigen, wenn das Bild nicht geladen werden kann.
        alt: String,
        knoten: Option<KnotenId>,
    },
    /// Eine waagerechte oder senkrechte Linie (Rahmen, `<hr>`,
    /// Unterstreichung).
    Linie {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dicke: i32,
        farbe: Farbe,
    },
}

impl Befehl {
    /// Das Rechteck, das dieser Befehl beruehrt — fuer Scroll-Auswahl
    /// und Schadensmeldungen.
    pub fn bereich(&self) -> Rechteck {
        match self {
            Befehl::Rechteck { rechteck, .. } | Befehl::Bild { rechteck, .. } => *rechteck,
            Befehl::Text {
                x, y, groesse, ..
            } => Rechteck::neu(*x, *y, 0, *groesse),
            Befehl::Linie {
                x0, y0, x1, y1, dicke, ..
            } => Rechteck::neu(
                *x0.min(x1),
                *y0.min(y1),
                (x1 - x0).abs().max(*dicke),
                (y1 - y0).abs().max(*dicke),
            ),
        }
    }
}

/// Die fertige Befehlsliste.
#[derive(Debug, Clone, Default)]
pub struct Anzeigeliste {
    pub befehle: Vec<Befehl>,
}

impl Anzeigeliste {
    pub fn len(&self) -> usize {
        self.befehle.len()
    }
    pub fn is_empty(&self) -> bool {
        self.befehle.is_empty()
    }

    /// Alle Textbefehle als ein String — die bequemste Pruefung in
    /// Tests („steht der Text ueberhaupt drin?").
    pub fn text(&self) -> String {
        let mut aus = String::new();
        for b in &self.befehle {
            if let Befehl::Text { text, .. } = b {
                if !aus.is_empty() {
                    aus.push(' ');
                }
                aus.push_str(text);
            }
        }
        aus
    }

    /// Die Textbefehle, in Reihenfolge — fuer Zeilen-Tests.
    pub fn texte(&self) -> Vec<&Befehl> {
        self.befehle
            .iter()
            .filter(|b| matches!(b, Befehl::Text { .. }))
            .collect()
    }

    /// Nur die Befehle, die dieses Rechteck beruehren.
    ///
    /// Der Browser nimmt das beim Scrollen: Von 20 000 Befehlen sind
    /// hundert sichtbar.
    pub fn im_bereich(&self, sicht: Rechteck) -> Vec<&Befehl> {
        self.befehle
            .iter()
            .filter(|b| {
                let r = b.bereich();
                r.y <= sicht.unten() && r.unten() >= sicht.y
            })
            .collect()
    }
}

/// Aus einem gesetzten Kastenbaum die Befehlsliste machen.
pub fn erzeugen(wurzel: &Kasten) -> Anzeigeliste {
    let mut liste = Anzeigeliste::default();
    // ITERATIV mit eigenem Stapel — der Baum kann tief sein, und diese
    // Funktion soll auch einen ueberstehen, den jemand anders gebaut hat.
    // Die Reihenfolge muss dabei erhalten bleiben (Malreihenfolge!),
    // deshalb werden die Kinder RUECKWAERTS auf den Stapel gelegt.
    let mut stapel: Vec<&Kasten> = alloc::vec![wurzel];
    while let Some(kasten) = stapel.pop() {
        kasten_malen(kasten, &mut liste);
        for kind in kasten.kinder.iter().rev() {
            stapel.push(kind);
        }
    }
    liste
}

/// Die Befehle EINES Kastens (ohne seine Kinder).
fn kasten_malen(kasten: &Kasten, liste: &mut Anzeigeliste) {
    match &kasten.art {
        KastenArt::Text(text) | KastenArt::Marke(text) => {
            if text.trim().is_empty() {
                return;
            }
            let r = kasten.masse.inhalt;
            liste.befehle.push(Befehl::Text {
                x: r.x,
                y: r.y,
                text: String::from(text.as_str()),
                groesse: r.hoehe.max(1),
                fett: kasten.stil.fett,
                kursiv: kasten.stil.kursiv,
                farbe: kasten.stil.farbe,
                knoten: kasten.knoten,
            });
            // Unterstreichung und Durchstreichung sind LINIEN, keine
            // Schrifteigenschaft — unsere Schriften koennen das nicht,
            // und ein Renderer, der Linien ziehen kann, schon.
            dekoration_malen(kasten, liste);
        }
        KastenArt::Bild { quelle, alt, .. } => {
            liste.befehle.push(Befehl::Bild {
                rechteck: kasten.masse.inhalt,
                quelle: String::from(quelle.as_str()),
                alt: String::from(alt.as_str()),
                knoten: kasten.knoten,
            });
        }
        // Zeilen sind reine Behaelter — sie malen nichts.
        KastenArt::Zeile | KastenArt::Umbruch | KastenArt::Inline => {}
        _ => {
            hintergrund_malen(kasten, liste);
            rahmen_malen(kasten, liste);
        }
    }
}

fn hintergrund_malen(kasten: &Kasten, liste: &mut Anzeigeliste) {
    if kasten.stil.hintergrund.ist_durchsichtig() {
        return;
    }
    // Der Hintergrund geht bis zum RAHMEN (padding-Box plus Rahmen), so
    // schreibt es die Spezifikation.
    let r = kasten.masse.rahmen_box();
    if r.ist_leer() {
        return;
    }
    liste.befehle.push(Befehl::Rechteck {
        rechteck: r,
        farbe: kasten.stil.hintergrund,
    });
}

fn rahmen_malen(kasten: &Kasten, liste: &mut Anzeigeliste) {
    let m = &kasten.masse;
    let aussen = m.rahmen_box();
    if aussen.ist_leer() {
        return;
    }
    let s = &kasten.stil;

    // Jede Kante als eigenes Rechteck. Die Ecken werden dabei doppelt
    // gemalt (bei durchgezogenen Rahmen unsichtbar); ein sauberer
    // Gehrungsschnitt braeuchte Dreiecke, und die kann unser Zeichner
    // nicht.
    let kanten: [(i32, RahmenStil, Farbe, Rechteck); 4] = [
        (
            m.rahmen.oben,
            s.rahmen_stil.oben,
            s.rahmen_farbe.oben,
            Rechteck::neu(aussen.x, aussen.y, aussen.breite, m.rahmen.oben),
        ),
        (
            m.rahmen.unten,
            s.rahmen_stil.unten,
            s.rahmen_farbe.unten,
            Rechteck::neu(
                aussen.x,
                aussen.unten() - m.rahmen.unten,
                aussen.breite,
                m.rahmen.unten,
            ),
        ),
        (
            m.rahmen.links,
            s.rahmen_stil.links,
            s.rahmen_farbe.links,
            Rechteck::neu(aussen.x, aussen.y, m.rahmen.links, aussen.hoehe),
        ),
        (
            m.rahmen.rechts,
            s.rahmen_stil.rechts,
            s.rahmen_farbe.rechts,
            Rechteck::neu(
                aussen.rechts() - m.rahmen.rechts,
                aussen.y,
                m.rahmen.rechts,
                aussen.hoehe,
            ),
        ),
    ];
    for (breite, stil, farbe, rechteck) in kanten {
        if breite > 0 && stil != RahmenStil::Keiner && !rechteck.ist_leer() {
            liste.befehle.push(Befehl::Rechteck { rechteck, farbe });
        }
    }
}

fn dekoration_malen(kasten: &Kasten, liste: &mut Anzeigeliste) {
    let d = kasten.stil.dekoration;
    if !d.unterstrichen && !d.durchgestrichen && !d.ueberstrichen {
        return;
    }
    let r = kasten.masse.inhalt;
    let dicke = (r.hoehe / 14).max(1);
    let mut linie = |y: i32| {
        liste.befehle.push(Befehl::Linie {
            x0: r.x,
            y0: y,
            x1: r.rechts(),
            y1: y,
            dicke,
            farbe: kasten.stil.farbe,
        });
    };
    if d.unterstrichen {
        // Knapp unter der Grundlinie — sonst schneidet sie durch die
        // Unterlaengen von `g` und `p`.
        linie(r.y + (r.hoehe * 9) / 10);
    }
    if d.durchgestrichen {
        linie(r.y + r.hoehe / 2);
    }
    if d.ueberstrichen {
        linie(r.y);
    }
}

/// Der Stil eines Kastens, ohne ihn zu klonen — Hilfe fuer Tests.
pub fn stil_von(kasten: &Kasten) -> &Stil {
    &kasten.stil
}
