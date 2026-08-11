// libspeed::leinwand — der Fensterpuffer als Zeichenflaeche
//
// ===========================================================================
// DIE ZWEI NAEHTE EINES RENDERERS, AN EINER STELLE
//
// Ein Programm, das HTML anzeigen will, braucht genau zwei Dinge von
// seinem Wirt, und beide stehen hier:
//
//   `RasterMetrik`    — wie BREIT ist dieser Text? (fuer `speedlayout`)
//   `FensterLeinwand` — MAL ihn hin.               (fuer `speedui`/`speedpaint`)
//
// Beide beschreiben dieselbe Schrift, und genau darin liegt der Grund,
// sie nebeneinanderzulegen: Laufen sie auseinander — misst die Metrik
// 6 Pixel je Zeichen und zeichnet die Leinwand 8 —, bricht das Layout an
// Stellen um, an denen der Text gar nicht endet. Das ist der klassische
// Renderer-Fehler, und er ist von aussen nicht zu sehen, sondern nur „das
// sieht komisch aus".
//
// ===========================================================================
// WARUM `uidemo` DAS NICHT BENUTZT
//
// `uidemo` bringt seine eigene Leinwand mit, und das bleibt so. Sein
// Zweck ist der BEWEIS, dass ein fremder Wirt die Traits aus einer
// Beschreibung bedienen kann (Serie 8, Teil 2) — teilte es sich den Code
// mit dem Browser, gaebe es wieder nur einen Wirt, und der Beweis waere
// keiner mehr.
//
// ===========================================================================
// WAS EIN PROZESS AN SCHRIFT HAT — und was nicht
//
// Die vorgerasterten Kernel-Schriften sind Kernel-Daten; es gibt keinen
// Schrift-Syscall (docs/grenzen.md). Ein Prozess bringt seine eigene
// mit — hier das 5x7-Raster aus `libspeed::fenster`, ganzzahlig
// vergroessert. Das ist grob, aber es ist EHRLICH grob: `exakt_moeglich`
// im Kernel-Toolkit macht dieselbe Aussage fuer die dortigen vier Raster.

use crate::fenster::Fenster;
use alloc::string::String;
use speedlayout::Metrik;
use speedui::{Farbe, Icon, Leinwand, Rechteck, Stil};

/// Breite und Hoehe des eingebauten 5x7-Rasters bei Skalierung 1.
pub const RASTER_BREITE: i32 = 6;
pub const RASTER_HOEHE: i32 = 7;
/// Groesste sinnvolle Vergroesserung — darueber wird das Raster klobig.
pub const MAX_SKALA: i32 = 4;

// ---------------------------------------------------------------------------
// (1) DIE METRIK
// ---------------------------------------------------------------------------

/// Was `speedlayout` ueber unsere Schrift wissen muss.
///
/// **Das ist die ganze Verdrahtung** — `speedlayout::Metrik` hat vier
/// Methoden, drei davon mit Voreinstellung. Genau dafuer ist das Trait so
/// schmal: Ein Programm, das layouten will, muss kein Toolkit einbinden.
#[derive(Debug, Clone, Copy, Default)]
pub struct RasterMetrik;

impl RasterMetrik {
    /// Welche ganzzahlige Vergroesserung kommt dieser Wunschgroesse am
    /// naechsten? (1..MAX_SKALA)
    ///
    /// GERUNDET WIRD ZUR NAECHSTEN, nicht abgerundet: Der Renderer will
    /// aus 19 px eine 20, nicht eine 16 — dieselbe Unterscheidung wie
    /// zwischen `groesse_waehlen` und `raster_hoehe` im Kernel-Toolkit
    /// (docs/schrift-groessen.md). Bei Gleichstand gewinnt die kleinere.
    pub fn skala(groesse: i32) -> i32 {
        ((groesse + RASTER_HOEHE / 2) / RASTER_HOEHE).clamp(1, MAX_SKALA)
    }
}

impl Metrik for RasterMetrik {
    /// **`chars().count()` und niemals `len()`.**
    ///
    /// „Grüße" hat 5 Zeichen und 7 Bytes. Wer Bytes zaehlt, rechnet je
    /// Umlaut eine Zeichenbreite zu viel und bricht JEDE deutsche Zeile
    /// zu frueh um.
    fn text_breite(&self, text: &str, groesse: i32, _fett: bool, _kursiv: bool) -> i32 {
        text.chars().count() as i32 * RASTER_BREITE * Self::skala(groesse)
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        (RASTER_HOEHE + 3) * Self::skala(groesse)
    }
    fn grundlinie(&self, groesse: i32) -> i32 {
        RASTER_HOEHE * Self::skala(groesse)
    }
    fn groesse_waehlen(&self, wunsch: i32) -> i32 {
        // Das Raster kann nur GANZE Vielfache. Das Layout soll mit der
        // Groesse rechnen, die WIRKLICH gezeichnet wird — sonst laufen
        // Zeilenhoehe und Textbreite auseinander.
        (RASTER_HOEHE * Self::skala(wunsch)).max(RASTER_HOEHE)
    }
}

// ---------------------------------------------------------------------------
// (2) DIE LEINWAND
// ---------------------------------------------------------------------------

/// Der Pixelpuffer eines Fensters als `speedui::Leinwand`.
pub struct FensterLeinwand<'a> {
    f: &'a mut Fenster,
    clip: Option<Rechteck>,
}

impl<'a> FensterLeinwand<'a> {
    pub fn neu(f: &'a mut Fenster) -> FensterLeinwand<'a> {
        FensterLeinwand { f, clip: None }
    }

    /// Ein Rechteck auf das Clip schneiden (None = nichts sichtbar).
    #[inline]
    fn sichtbar(&self, r: Rechteck) -> Option<Rechteck> {
        match self.clip {
            Some(c) => r.schneiden(&c),
            None => Some(r),
        }
    }

    #[inline]
    fn farbwert(farbe: Farbe) -> u32 {
        Fenster::farbe(farbe.r, farbe.g, farbe.b)
    }
}

impl Leinwand for FensterLeinwand<'_> {
    fn masse(&self) -> (i32, i32) {
        (self.f.breite() as i32, self.f.hoehe() as i32)
    }
    fn clip(&self) -> Option<Rechteck> {
        self.clip
    }
    fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.clip = clip;
    }

    fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        // VORAB rechteckig clippen und dann ohne Pro-Pixel-Pruefung
        // fuellen — derselbe Schnellpfad wie im Kernel-Zeichner
        // (Serie 3). Bei einem Seitenhintergrund in 4K ist das der
        // Unterschied zwischen einem memset und acht Millionen `if`.
        if let Some(r) = self.sichtbar(rechteck) {
            self.f
                .rechteck(r.x, r.y, r.breite, r.hoehe, Self::farbwert(farbe));
        }
    }

    fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe) {
        let r = radius.min(rechteck.breite / 2).min(rechteck.hoehe / 2);
        self.fuellen(
            Rechteck::neu(rechteck.x + r, rechteck.y, rechteck.breite - 2 * r, rechteck.hoehe),
            farbe,
        );
        self.fuellen(
            Rechteck::neu(rechteck.x, rechteck.y + r, rechteck.breite, rechteck.hoehe - 2 * r),
            farbe,
        );
    }

    fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.fuellen(Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, 1), farbe);
        self.fuellen(
            Rechteck::neu(rechteck.x, rechteck.y + rechteck.hoehe - 1, rechteck.breite, 1),
            farbe,
        );
        self.fuellen(Rechteck::neu(rechteck.x, rechteck.y, 1, rechteck.hoehe), farbe);
        self.fuellen(
            Rechteck::neu(rechteck.x + rechteck.breite - 1, rechteck.y, 1, rechteck.hoehe),
            farbe,
        );
    }

    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe) {
        // Nur waagerecht/senkrecht. Der Maler schickt nichts anderes
        // (alle Layout-Linien sind achsenparallel), und eine schraege
        // Linie hier nachzubauen waere Bresenham fuer null Aufrufer.
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        self.fuellen(
            Rechteck::neu(x0, y0, (x1 - x0 + 1).max(1), (y1 - y0 + 1).max(1)),
            farbe,
        );
    }

    fn text(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe) {
        self.text_stil(x, y, text, groesse, Stil { fett, kursiv: false }, farbe);
    }

    /// Text in der Groesse, die das Layout gerechnet hat.
    ///
    /// **FETT WIRD ANGEDEUTET, NICHT GESETZT**, und das steht hier statt
    /// in einer Fussnote: Ein Prozess hat keinen Fettschnitt (der Kernel
    /// hat einen, aber er gibt ihn nicht heraus). Zweimal mit einem Pixel
    /// Versatz zu zeichnen macht den Text dicker — es ist ein
    /// Fettdruck-EFFEKT, keine Fettschrift. Wichtig fuers Layout: Die
    /// BREITE aendert sich dadurch nicht, und `RasterMetrik::text_breite`
    /// rechnet fett und mager deshalb gleich. Genau so bleiben Messung
    /// und Zeichnung beieinander.
    ///
    /// KURSIV KANN DIESER WIRT NICHT. Der Kernel schert seine Glyphen um
    /// 14 Grad; das 5x7-Raster wird davon unleserlich. Kursiver Text wird
    /// aufrecht gezeichnet — sichtbar falsch waere schlechter als
    /// unsichtbar gleich.
    fn text_stil(&mut self, x: i32, y: i32, text: &str, groesse: i32, stil: Stil, farbe: Farbe) {
        let skala = RasterMetrik::skala(groesse);
        let breite = RASTER_BREITE * skala;
        let hoehe = RASTER_HOEHE * skala;
        let farbwert = Self::farbwert(farbe);
        // ZEICHENWEISE clippen: `Fenster::text` kann kein Clip, und ohne
        // diese Pruefung zeichnete eine lange Zeile ueber den Streifen
        // hinaus. Zugleich der Schnellpfad — was links oder rechts
        // herausragt, kostet nur einen Vergleich.
        let mut eins = String::new();
        for (i, zeichen) in text.chars().enumerate() {
            let zx = x + i as i32 * breite;
            if self.sichtbar(Rechteck::neu(zx, y, breite, hoehe)).is_none() {
                continue;
            }
            eins.clear();
            eins.push(zeichen);
            self.f.text(zx, y, &eins, farbwert, skala);
            if stil.fett {
                self.f.text(zx + 1, y, &eins, farbwert, skala);
            }
        }
    }

    fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32) {
        for (zeile, muster) in icon.zeilen.iter().enumerate() {
            for (spalte, zeichen) in muster.chars().enumerate() {
                let Some(farbe) = speedui::icon_farbe(zeichen) else {
                    continue;
                };
                self.fuellen(
                    Rechteck::neu(
                        x + spalte as i32 * skalierung,
                        y + zeile as i32 * skalierung,
                        skalierung,
                        skalierung,
                    ),
                    farbe,
                );
            }
        }
    }

    /// Ein RGBA-Bild ins Zielrechteck malen.
    ///
    /// PUNKTABTASTUNG, ganzzahlig — es gibt kein Fliesskomma
    /// (`-sse,+soft-float`), und Interpolation braeuchte fuer jeden
    /// Zielpixel vier Quellpixel. Ein Foto sieht dadurch beim
    /// Verkleinern kantig aus; es ist an der richtigen Stelle und in der
    /// richtigen Farbe, und das ist die Zusage.
    ///
    /// ALPHA WIRD GEMISCHT, nicht ignoriert: Ein PNG mit durchsichtigem
    /// Rand saehe sonst aus, als haette es einen schwarzen Rahmen.
    fn bild(&mut self, ziel: Rechteck, quell_breite: i32, quell_hoehe: i32, rgba: &[u8]) {
        let Some(sichtbar) = self.sichtbar(ziel) else {
            return;
        };
        if quell_breite <= 0 || quell_hoehe <= 0 || ziel.breite <= 0 || ziel.hoehe <= 0 {
            return;
        }
        let gebraucht = (quell_breite as i64) * (quell_hoehe as i64) * 4;
        if (rgba.len() as i64) < gebraucht {
            return;
        }
        for y in sichtbar.y..sichtbar.y + sichtbar.hoehe {
            // Quellzeile aus der ZIEL-Geometrie (nicht aus der
            // sichtbaren) — sonst verrutschte das Bild, sobald es am
            // Rand angeschnitten wird.
            let qy = ((y - ziel.y) as i64 * quell_hoehe as i64 / ziel.hoehe as i64) as i32;
            let qy = qy.clamp(0, quell_hoehe - 1);
            for x in sichtbar.x..sichtbar.x + sichtbar.breite {
                let qx = ((x - ziel.x) as i64 * quell_breite as i64 / ziel.breite as i64) as i32;
                let qx = qx.clamp(0, quell_breite - 1);
                let ab = ((qy as usize) * (quell_breite as usize) + qx as usize) * 4;
                let (r, g, b, a) = (rgba[ab], rgba[ab + 1], rgba[ab + 2], rgba[ab + 3]);
                match a {
                    0 => continue,
                    255 => self.f.punkt(x, y, Fenster::farbe(r, g, b)),
                    _ => {
                        // Ueber Weiss mischen. Den Untergrund zu lesen
                        // waere genauer; `Fenster` gibt ihn nicht heraus,
                        // und eine Seite ist fast immer hell.
                        let misch = |kanal: u8| {
                            ((kanal as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8
                        };
                        self.f
                            .punkt(x, y, Fenster::farbe(misch(r), misch(g), misch(b)));
                    }
                }
            }
        }
    }
}
