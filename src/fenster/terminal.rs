// fenster/terminal.rs — Das Text-Raster des Terminal-Fensters
//
// Die SpeedShell läuft im Desktop als FENSTER: konsole::_print leitet
// die Ausgabe hierher um, statt in den Bildschirm-Back-Buffer zu
// malen. Dieses Modul ist NUR das Datenmodell — ein Raster aus
// Zellen (Zeichen + Farben) mit Cursor, Zeilenumbruch und Scrolling,
// als reine, unit-getestete Logik ganz ohne Bildschirm.
// Das Rendern ins Fenster-Pixel-Puffer macht fenster/mod.rs
// (gebündelt pro Compositor-Frame, nicht bei jedem print!).

use crate::framebuffer::Farbe;
use alloc::vec;
use alloc::vec::Vec;

/// Eine Zelle des Terminal-Rasters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zelle {
    pub zeichen: char,
    pub vg: Farbe,
    pub hg: Farbe,
}

pub struct Terminal {
    spalten: usize,
    zeilen: usize,
    /// Zeilenweise abgelegt: zellen[zeile * spalten + spalte].
    zellen: Vec<Zelle>,
    cursor_spalte: usize,
    cursor_zeile: usize,
    /// Farbe leerer Zellen (der Terminal-Hintergrund des Themes).
    standard_hg: Farbe,
    /// Geänderte RASTERZEILEN [von, bis) seit dem letzten Rendern —
    /// der Serie-3-Performance-Pass: Eine Prompt-Zeile soll nicht
    /// das ganze 80x24-Raster neu zeichnen (und den Compositor nicht
    /// die ganze Fensterfläche komponieren lassen). Scrollen und
    /// Resize markieren ALLES.
    dirty_von: usize,
    dirty_bis: usize,
}

impl Terminal {
    pub fn neu(spalten: usize, zeilen: usize, standard_hg: Farbe) -> Self {
        let (spalten, zeilen) = (spalten.max(1), zeilen.max(1));
        Terminal {
            spalten,
            zeilen,
            zellen: vec![Zelle::leer(standard_hg); spalten * zeilen],
            cursor_spalte: 0,
            cursor_zeile: 0,
            standard_hg,
            dirty_von: 0,
            dirty_bis: zeilen, // frisch: alles rendern
        }
    }

    /// Markiert eine Rasterzeile als geändert.
    fn zeile_markieren(&mut self, zeile: usize) {
        self.dirty_von = self.dirty_von.min(zeile);
        self.dirty_bis = self.dirty_bis.max(zeile + 1);
    }

    /// Markiert das GANZE Raster (Scroll, Resize, Theme-Wechsel).
    pub fn alles_markieren(&mut self) {
        self.dirty_von = 0;
        self.dirty_bis = self.zeilen;
    }

    /// Der aktuelle Dirty-Bereich [von, bis) — None = nichts zu tun.
    /// (Nur lesen; abholen setzt zurück.)
    pub fn dirty_zeilen(&self) -> Option<(usize, usize)> {
        if self.dirty_von >= self.dirty_bis {
            None
        } else {
            Some((self.dirty_von, self.dirty_bis.min(self.zeilen)))
        }
    }

    /// Holt den Dirty-Bereich ab und setzt ihn zurück (der Renderer).
    pub fn dirty_abholen(&mut self) -> Option<(usize, usize)> {
        let bereich = self.dirty_zeilen();
        self.dirty_von = usize::MAX;
        self.dirty_bis = 0;
        bereich
    }

    pub fn spalten(&self) -> usize {
        self.spalten
    }
    pub fn zeilen(&self) -> usize {
        self.zeilen
    }
    /// Cursor-Position als (spalte, zeile).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_spalte, self.cursor_zeile)
    }
    pub fn zelle(&self, spalte: usize, zeile: usize) -> Zelle {
        self.zellen[zeile * self.spalten + spalte]
    }

    /// Leert das komplette Raster (für den clear-Befehl).
    pub fn leeren(&mut self) {
        self.zellen.fill(Zelle::leer(self.standard_hg));
        self.cursor_spalte = 0;
        self.cursor_zeile = 0;
        self.alles_markieren();
    }

    /// Schreibt EIN Zeichen an die Cursor-Position — mit denselben
    /// Regeln wie die FramebufferKonsole: \n bricht um, Backspace geht
    /// nur zurück (die Shell radiert per "\b \b"), am Zeilenende wird
    /// automatisch umgebrochen, unten wird gescrollt.
    /// Dirty-Buchhaltung: Cursor-Zeile VOR und NACH der Operation
    /// markieren (der Cursor-Unterstrich wandert mit!).
    pub fn schreiben(&mut self, zeichen: char, vg: Farbe, hg: Farbe) {
        self.zeile_markieren(self.cursor_zeile);
        match zeichen {
            '\n' => self.neue_zeile(),
            '\r' => self.cursor_spalte = 0,
            '\u{8}' => self.cursor_spalte = self.cursor_spalte.saturating_sub(1),
            zeichen => {
                if self.cursor_spalte >= self.spalten {
                    self.neue_zeile();
                }
                self.zellen[self.cursor_zeile * self.spalten + self.cursor_spalte] =
                    Zelle { zeichen, vg, hg };
                self.cursor_spalte += 1;
            }
        }
        self.zeile_markieren(self.cursor_zeile);
    }

    fn neue_zeile(&mut self) {
        self.cursor_spalte = 0;
        if self.cursor_zeile + 1 < self.zeilen {
            self.cursor_zeile += 1;
        } else {
            // Scrollen: alle Zeilen eine hoch (memmove), unterste leeren.
            // Danach hat sich JEDE Zeile verschoben:
            self.zellen.copy_within(self.spalten.., 0);
            let ab = (self.zeilen - 1) * self.spalten;
            self.zellen[ab..].fill(Zelle::leer(self.standard_hg));
            self.alles_markieren();
        }
    }

    /// Passt das Raster an eine neue Fenstergröße an. Die UNTEREN
    /// Zeilen bleiben erhalten — dort stehen Prompt und die jüngste
    /// Ausgabe (oben ist nur Historie).
    pub fn groesse_setzen(&mut self, spalten: usize, zeilen: usize) {
        let (spalten, zeilen) = (spalten.max(1), zeilen.max(1));
        if spalten == self.spalten && zeilen == self.zeilen {
            return;
        }
        let mut neu = vec![Zelle::leer(self.standard_hg); spalten * zeilen];
        let zeilen_kopieren = self.zeilen.min(zeilen);
        // Bei Verkleinerung: die obersten Zeilen fallen weg.
        let quell_ab = self.zeilen - zeilen_kopieren;
        for zeile in 0..zeilen_kopieren {
            for spalte in 0..self.spalten.min(spalten) {
                neu[zeile * spalten + spalte] =
                    self.zellen[(quell_ab + zeile) * self.spalten + spalte];
            }
        }
        self.zellen = neu;
        self.cursor_zeile = self.cursor_zeile.saturating_sub(quell_ab).min(zeilen - 1);
        self.cursor_spalte = self.cursor_spalte.min(spalten - 1);
        self.spalten = spalten;
        self.zeilen = zeilen;
        self.alles_markieren();
    }
}

impl Zelle {
    fn leer(hg: Farbe) -> Self {
        Zelle { zeichen: ' ', vg: hg, hg }
    }
}

// ---------------------------------------------------------------------------
// Tests — reine Raster-Logik, kein Bildschirm nötig
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VG: Farbe = Farbe::neu(200, 200, 200);
    const HG: Farbe = Farbe::neu(10, 10, 10);

    fn zeile_als_text(term: &Terminal, zeile: usize) -> alloc::string::String {
        (0..term.spalten()).map(|s| term.zelle(s, zeile).zeichen).collect()
    }

    fn schreiben(term: &mut Terminal, text: &str) {
        for zeichen in text.chars() {
            term.schreiben(zeichen, VG, HG);
        }
    }

    /// Schreiben, Zeilenumbruch am Rand und \n.
    #[test_case]
    fn test_terminal_schreiben_und_umbruch() {
        let mut term = Terminal::neu(5, 3, HG);
        schreiben(&mut term, "abcdefg\nhi");
        assert_eq!(zeile_als_text(&term, 0), "abcde"); // automatisch umgebrochen
        assert_eq!(zeile_als_text(&term, 1), "fg   ");
        assert_eq!(zeile_als_text(&term, 2), "hi   ");
        assert_eq!(term.cursor(), (2, 2));
    }

    /// Am unteren Rand scrollt der Inhalt eine Zeile hoch.
    #[test_case]
    fn test_terminal_scrollt() {
        let mut term = Terminal::neu(5, 2, HG);
        schreiben(&mut term, "eins\nzwei\ndrei");
        assert_eq!(zeile_als_text(&term, 0), "zwei ");
        assert_eq!(zeile_als_text(&term, 1), "drei ");
    }

    /// Backspace + Leerzeichen radiert wie in der Konsole ("\b \b").
    #[test_case]
    fn test_terminal_backspace() {
        let mut term = Terminal::neu(10, 2, HG);
        schreiben(&mut term, "hallo\u{8} \u{8}");
        assert_eq!(zeile_als_text(&term, 0), "hall      ");
        assert_eq!(term.cursor(), (4, 0));
    }

    /// Resize behält die UNTEREN Zeilen (Prompt!) und klemmt den Cursor.
    #[test_case]
    fn test_terminal_groesse_setzen() {
        let mut term = Terminal::neu(6, 3, HG);
        schreiben(&mut term, "eins\nzwei\ndrei");
        // Auf 2 Zeilen schrumpfen: "eins" (oberste) fällt weg.
        term.groesse_setzen(6, 2);
        assert_eq!(zeile_als_text(&term, 0), "zwei  ");
        assert_eq!(zeile_als_text(&term, 1), "drei  ");
        assert_eq!(term.cursor(), (4, 1));
        // Wieder wachsen: Inhalt bleibt, unten kommt Platz dazu.
        term.groesse_setzen(8, 4);
        assert_eq!(zeile_als_text(&term, 0), "zwei    ");
        assert_eq!(&zeile_als_text(&term, 1)[..4], "drei");
        // clear leert alles:
        term.leeren();
        assert_eq!(term.cursor(), (0, 0));
        assert_eq!(zeile_als_text(&term, 0), "        ");
    }
}
