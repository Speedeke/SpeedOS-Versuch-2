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

/// Wie viele herausgescrollte Zeilen aufgehoben werden.
///
/// 1000 Zeilen à 200 Spalten sind bei 12 Byte je Zelle rund 2,4 MiB — das
/// ist der Preis dafür, dass eine lange Ausgabe nicht unwiederbringlich
/// oben herausläuft. Der Puffer wird ERST BEIM ERSTEN SCROLLEN angelegt;
/// ein Terminal, das nie überläuft, kostet nichts.
pub const MAX_HISTORIE: usize = 1000;

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

    // ----- Der Rückblick (Scrollback) -----
    //
    // RINGPUFFER, kein Vec mit `remove(0)`: Bei jedem Scrollen die
    // älteste Zeile vorne herauszunehmen würde den ganzen Puffer
    // verschieben — bei 1000 Zeilen à 200 Zellen wären das 2,4 MiB
    // memmove je Ausgabezeile. Der Ring schreibt an eine Stelle.
    /// Herausgescrollte Zeilen, je `spalten` Zellen. Leer, solange nie
    /// gescrollt wurde.
    historie: Vec<Zelle>,
    /// Wie viele Zeilen des Rings gültig sind (wächst bis `MAX_HISTORIE`).
    historie_zeilen: usize,
    /// Wohin die NÄCHSTE herausgescrollte Zeile geschrieben wird.
    historie_kopf: usize,
    /// Wie weit der Blick nach OBEN verschoben ist. 0 = live am Ende.
    ///
    /// Er wird beim Herausscrollen MITGEZOGEN: Wer zurückgeblättert hat und
    /// dann kommt neue Ausgabe, soll weiter dieselbe Stelle sehen und nicht
    /// mitwandern. Genau das erwartet man von einem Terminal.
    blick_ab: usize,
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
            historie: Vec::new(),
            historie_zeilen: 0,
            historie_kopf: 0,
            blick_ab: 0,
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
    /// Cursor-Position im LIVE-Raster als (spalte, zeile).
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_spalte, self.cursor_zeile)
    }

    /// Wo der Cursor auf dem BILDSCHIRM steht — `None`, wenn er gerade
    /// nicht zu sehen ist, weil zurückgeblättert wurde.
    ///
    /// Ein Cursor, der beim Zurückblättern mitten in alter Ausgabe klebt,
    /// wäre eine Lüge: Dort wird nicht getippt.
    pub fn cursor_bildschirm(&self) -> Option<(usize, usize)> {
        let zeile = self.cursor_zeile + self.blick_ab;
        if zeile >= self.zeilen {
            None
        } else {
            Some((self.cursor_spalte, zeile))
        }
    }
    /// Die Zelle, die an dieser BILDSCHIRM-Position zu sehen ist.
    ///
    /// Ist zurückgeblättert (`blick_ab > 0`), liefert sie für die oberen
    /// Zeilen den Inhalt aus der Historie. Der Renderer merkt davon nichts —
    /// er fragt weiter nach (Spalte, Zeile) und bekommt, was dort steht.
    pub fn zelle(&self, spalte: usize, zeile: usize) -> Zelle {
        if self.blick_ab == 0 || zeile >= self.blick_ab {
            let live_zeile = zeile - self.blick_ab;
            return self.zellen[live_zeile * self.spalten + spalte];
        }
        // Diese Bildschirmzeile liegt OBERHALB des Live-Rasters:
        // `blick_ab - zeile` Zeilen über dessen erster Zeile.
        match self.historie_zelle(self.blick_ab - zeile, spalte) {
            Some(zelle) => zelle,
            None => Zelle::leer(self.standard_hg),
        }
    }

    /// Die Zelle aus der Historie, `hinauf` Zeilen über dem Live-Raster
    /// (`hinauf == 1` ist die zuletzt herausgescrollte Zeile).
    fn historie_zelle(&self, hinauf: usize, spalte: usize) -> Option<Zelle> {
        if hinauf == 0 || hinauf > self.historie_zeilen || self.historie.is_empty() {
            return None;
        }
        // Rückwärts vom Schreibkopf, modulo Ringgröße. `hinauf == MAX`
        // ergibt dabei genau den Kopf — und der zeigt bei vollem Ring auf
        // die ÄLTESTE Zeile. Das stimmt also.
        let index = (self.historie_kopf + MAX_HISTORIE - hinauf % MAX_HISTORIE) % MAX_HISTORIE;
        self.historie.get(index * self.spalten + spalte).copied()
    }

    /// Wie viele Zeilen liegen im Rückblick?
    pub fn historie_zeilen(&self) -> usize {
        self.historie_zeilen
    }

    /// Wie weit ist zurückgeblättert? 0 = live am Ende.
    pub fn blick_ab(&self) -> usize {
        self.blick_ab
    }

    /// BLÄTTERT im Rückblick. Positiv = nach oben (in die Vergangenheit).
    /// Liefert `true`, wenn sich etwas geändert hat.
    pub fn scrollen(&mut self, zeilen: isize) -> bool {
        let vorher = self.blick_ab;
        let ziel = self.blick_ab as isize + zeilen;
        self.blick_ab = ziel.clamp(0, self.historie_zeilen as isize) as usize;
        if self.blick_ab != vorher {
            self.alles_markieren();
            true
        } else {
            false
        }
    }

    /// Springt ans ENDE (zum Live-Bild). Liefert `true`, wenn gesprungen
    /// wurde — der Aufrufer weiß dann, dass neu gerendert werden muss.
    ///
    /// Das ruft, wer TIPPT: Wer eine Taste drückt, will sehen, was er
    /// schreibt, und nicht in der Vergangenheit stehen bleiben.
    pub fn zum_ende(&mut self) -> bool {
        if self.blick_ab == 0 {
            return false;
        }
        self.blick_ab = 0;
        self.alles_markieren();
        true
    }

    /// Leert das komplette Raster (für den clear-Befehl) — MITSAMT
    /// Rückblick.
    ///
    /// Eine Entscheidung, die man auch anders treffen könnte: Viele
    /// Terminals behalten die Historie über `clear` hinweg. Hier wirft
    /// `clear` alles weg, weil das die Bedeutung ist, die man in SpeedOS
    /// erwartet — „mach sauber", nicht „schieb es nur aus dem Blick".
    pub fn leeren(&mut self) {
        self.zellen.fill(Zelle::leer(self.standard_hg));
        self.historie = Vec::new();
        self.historie_zeilen = 0;
        self.historie_kopf = 0;
        self.blick_ab = 0;
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
            // Die oberste Zeile verlässt den Bildschirm — sie wandert in
            // den Rückblick, BEVOR sie überschrieben wird.
            self.oberste_zeile_aufheben();
            // Scrollen: alle Zeilen eine hoch (memmove), unterste leeren.
            // Danach hat sich JEDE Zeile verschoben:
            self.zellen.copy_within(self.spalten.., 0);
            let ab = (self.zeilen - 1) * self.spalten;
            self.zellen[ab..].fill(Zelle::leer(self.standard_hg));
            self.alles_markieren();
        }
    }

    /// Legt die oberste Rasterzeile im Rückblick ab.
    ///
    /// Der Ringpuffer wird beim ERSTEN Mal angelegt — ein Terminal, das nie
    /// überläuft (der Normalfall bei kurzen Befehlen), zahlt nichts dafür.
    fn oberste_zeile_aufheben(&mut self) {
        self.zeile_in_historie(0);
    }

    /// Legt EINE bestimmte Rasterzeile im Rückblick ab.
    ///
    /// Schiebt das Raster NICHT — das tut der Aufrufer (`neue_zeile` per
    /// `copy_within`, `groesse_setzen` beim Neuaufbau).
    fn zeile_in_historie(&mut self, zeile: usize) {
        if zeile >= self.zeilen {
            return;
        }
        if self.historie.is_empty() {
            self.historie = vec![Zelle::leer(self.standard_hg); MAX_HISTORIE * self.spalten];
            self.historie_kopf = 0;
            self.historie_zeilen = 0;
        }
        // `zellen` und `historie` sind verschiedene Felder — die Ausleihen
        // sind disjunkt, es braucht keinen Zwischenpuffer.
        let breite = self.spalten;
        let quelle = zeile * breite;
        let ziel = self.historie_kopf * breite;
        self.historie[ziel..ziel + breite]
            .copy_from_slice(&self.zellen[quelle..quelle + breite]);
        self.historie_kopf = (self.historie_kopf + 1) % MAX_HISTORIE;
        self.historie_zeilen = (self.historie_zeilen + 1).min(MAX_HISTORIE);

        // DEN BLICK MITZIEHEN: Wer zurückgeblättert hat, soll dieselbe
        // Stelle weiter sehen, statt von neuer Ausgabe nach unten
        // geschoben zu werden.
        if self.blick_ab > 0 {
            self.blick_ab = (self.blick_ab + 1).min(self.historie_zeilen);
        }
    }

    /// LEGT DEN RÜCKBLICK AUF EINE NEUE BREITE UM — ohne ihn wegzuwerfen.
    ///
    /// ==================================================================
    /// WARUM DAS SEIN MUSS (und die erste Fassung falsch war)
    ///
    /// Der Ring liegt zeilenweise auf `spalten`; ändert sich die Breite,
    /// stimmt der Zeilenabstand nicht mehr. Die erste Fassung hat den
    /// Rückblick deshalb einfach VERWORFEN und das als „bekannte Grenze"
    /// dokumentiert.
    ///
    /// Das war in der Praxis unbrauchbar: Ein Terminal-Fenster zu
    /// MAXIMIEREN ändert die Spaltenzahl — und Maximieren ist genau die
    /// Geste, mit der man mehr sehen will. Der Rückblick war also immer
    /// dann weg, wenn man ihn am ehesten braucht, und nach dem
    /// Wiederherstellen gleich noch einmal.
    ///
    /// Jetzt werden die Zeilen umkopiert. Wird es SCHMALER, verlieren sie
    /// rechts etwas — dasselbe tut das sichtbare Raster auch. Umbrechen
    /// wäre die Kür (und echte Terminals tun sich damit schwer); erhalten
    /// zu bleiben ist die Pflicht.
    /// ==================================================================
    fn historie_umlegen(&mut self, neue_spalten: usize) {
        if self.historie.is_empty() || neue_spalten == self.spalten {
            return;
        }
        let alt_stride = self.spalten;
        let breite = alt_stride.min(neue_spalten);
        let anzahl = self.historie_zeilen;
        let mut neu = vec![Zelle::leer(self.standard_hg); MAX_HISTORIE * neue_spalten];
        // In LOGISCHER Reihenfolge kopieren (älteste zuerst) — danach liegt
        // der Ring wieder von vorne, und der Kopf zeigt hinter die jüngste.
        for i in 0..anzahl {
            let hinauf = anzahl - i;
            let alt = (self.historie_kopf + MAX_HISTORIE - hinauf % MAX_HISTORIE) % MAX_HISTORIE;
            let von = alt * alt_stride;
            let nach = i * neue_spalten;
            neu[nach..nach + breite].copy_from_slice(&self.historie[von..von + breite]);
        }
        self.historie = neu;
        self.historie_kopf = anzahl % MAX_HISTORIE;
    }

    /// Passt das Raster an eine neue Fenstergröße an. Die UNTEREN
    /// Zeilen bleiben erhalten — dort stehen Prompt und die jüngste
    /// Ausgabe.
    pub fn groesse_setzen(&mut self, spalten: usize, zeilen: usize) {
        let (spalten, zeilen) = (spalten.max(1), zeilen.max(1));
        if spalten == self.spalten && zeilen == self.zeilen {
            return;
        }
        // (1) WIRD ES NIEDRIGER, fallen oben Zeilen weg — die gehören in den
        //     Rückblick, nicht in den Müll. Noch in der ALTEN Breite, damit
        //     das Umlegen gleich alles auf einmal erwischt.
        //
        //     ACHTUNG: `zeile_in_historie` schiebt das Raster NICHT (das tut
        //     `neue_zeile` selbst). Hier wird deshalb jede wegfallende Zeile
        //     einzeln benannt — die erste Fassung rief in einer Schleife
        //     immer wieder dieselbe Zeile 0 auf und legte sie mehrfach ab.
        let zeilen_kopieren = self.zeilen.min(zeilen);
        let quell_ab = self.zeilen - zeilen_kopieren;
        for zeile in 0..quell_ab {
            self.zeile_in_historie(zeile);
        }

        // (2) Den Rückblick auf die neue Breite umlegen (siehe dort).
        self.historie_umlegen(spalten);

        // (3) Das sichtbare Raster neu aufbauen — die UNTEREN Zeilen bleiben.
        self.blick_ab = 0;
        let mut neu = vec![Zelle::leer(self.standard_hg); spalten * zeilen];
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

    /// DER RÜCKBLICK: Was oben herauslief, ist wiederzufinden.
    #[test_case]
    fn test_terminal_rueckblick_holt_zurueck() {
        // 3 Zeilen sichtbar, 6 Zeilen Ausgabe -> 3 wandern in die Historie.
        let mut term = Terminal::neu(10, 3, HG);
        for nummer in 1..=6 {
            schreiben(&mut term, &alloc::format!("Zeile{}\n", nummer));
        }
        // Jedes der sechs "\n" hinter der dritten Zeile scrollt — also
        // liegen VIER Zeilen im Rueckblick, und unten steht die leere
        // Zeile hinter Zeile6 (dort blinkt der Cursor).
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile5");
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "Zeile6");
        assert_eq!(zeile_als_text(&term, 2).trim_end(), "");
        assert_eq!(term.historie_zeilen(), 4, "vier Zeilen muessen aufgehoben sein");
        assert_eq!(term.blick_ab(), 0, "frisch ist der Blick live");

        // EINE Zeile zurueck: oben erscheint Zeile4.
        assert!(term.scrollen(1));
        assert_eq!(term.blick_ab(), 1);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile4");
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "Zeile5");
        assert_eq!(zeile_als_text(&term, 2).trim_end(), "Zeile6");

        // Ganz nach oben — weiter als die Historie geht es nicht.
        assert!(term.scrollen(99));
        assert_eq!(term.blick_ab(), 4, "am oberen Ende ist Schluss");
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile1");
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "Zeile2");
        assert_eq!(zeile_als_text(&term, 2).trim_end(), "Zeile3");
        assert!(!term.scrollen(5), "am Anschlag darf nichts passieren");

        // Und zurueck ans Ende.
        assert!(term.zum_ende());
        assert_eq!(term.blick_ab(), 0);
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "Zeile6");
        assert!(!term.zum_ende(), "zweimal ans Ende ist folgenlos");
    }

    /// NEUE AUSGABE VERSCHIEBT DEN BLICK NICHT.
    ///
    /// Wer zurueckblaettert, um etwas zu lesen, soll nicht von
    /// nachlaufender Ausgabe weggeschoben werden — der Blick haengt an der
    /// STELLE, nicht am Abstand zum Ende.
    #[test_case]
    fn test_terminal_rueckblick_bleibt_stehen() {
        let mut term = Terminal::neu(10, 3, HG);
        for nummer in 1..=6 {
            schreiben(&mut term, &alloc::format!("Zeile{}\n", nummer));
        }
        term.scrollen(2);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile3");

        // Jetzt kommt Ausgabe nach — die Sicht muss dieselbe bleiben,
        // und `blick_ab` muss dafuer MITWACHSEN (2 -> 4).
        schreiben(&mut term, "Zeile7\n");
        schreiben(&mut term, "Zeile8\n");
        assert_eq!(
            zeile_als_text(&term, 0).trim_end(),
            "Zeile3",
            "der Blick ist mitgewandert, statt stehen zu bleiben"
        );
        assert_eq!(term.blick_ab(), 4, "der Blick muss mitgezogen worden sein");

        // Am Ende ist alles da, auch das Nachgelaufene.
        term.zum_ende();
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "Zeile8");
    }

    /// `clear` wirft auch den Rueckblick — und der Ring haelt seine Grenze.
    #[test_case]
    fn test_terminal_rueckblick_grenzen() {
        let mut term = Terminal::neu(8, 2, HG);
        // Deutlich mehr Zeilen als der Ring fasst.
        for nummer in 0..(MAX_HISTORIE + 50) {
            schreiben(&mut term, &alloc::format!("{}\n", nummer % 10));
        }
        assert_eq!(
            term.historie_zeilen(),
            MAX_HISTORIE,
            "der Ring darf nicht ueber seine Grenze wachsen"
        );
        // Ganz nach oben blaettern darf nicht panicken und nicht daneben
        // greifen (der Ring ist inzwischen mehrfach umgelaufen).
        term.scrollen(MAX_HISTORIE as isize);
        assert_eq!(term.blick_ab(), MAX_HISTORIE);
        let _ = zeile_als_text(&term, 0);

        term.leeren();
        assert_eq!(term.historie_zeilen(), 0, "clear muss den Rueckblick werfen");
        assert_eq!(term.blick_ab(), 0);
    }

    /// Zurueckgeblaettert gibt es keinen Cursor — dort wird nicht getippt.
    #[test_case]
    fn test_terminal_cursor_beim_blaettern() {
        let mut term = Terminal::neu(10, 3, HG);
        for nummer in 1..=6 {
            schreiben(&mut term, &alloc::format!("Zeile{}\n", nummer));
        }
        assert!(term.cursor_bildschirm().is_some(), "live gibt es einen Cursor");
        term.scrollen(3);
        assert!(
            term.cursor_bildschirm().is_none(),
            "beim Zurueckblaettern darf kein Cursor stehen"
        );
        term.zum_ende();
        assert!(term.cursor_bildschirm().is_some());
    }

    /// DER RÜCKBLICK ÜBERLEBT EINE GRÖSSENÄNDERUNG.
    ///
    /// Der Fehler, den dieser Test festhält: Die erste Fassung warf den
    /// Rückblick bei jeder Breitenänderung weg — und ein Terminal-Fenster
    /// zu MAXIMIEREN ändert die Breite. Also war er immer dann verloren,
    /// wenn man ihn am dringendsten wollte, und nach dem Wiederherstellen
    /// gleich noch einmal.
    #[test_case]
    fn test_terminal_rueckblick_ueberlebt_resize() {
        let mut term = Terminal::neu(10, 3, HG);
        for nummer in 1..=6 {
            schreiben(&mut term, &alloc::format!("Zeile{}\n", nummer));
        }
        assert_eq!(term.historie_zeilen(), 4);

        // BREITER (wie beim Maximieren) — der Rückblick muss bleiben.
        term.groesse_setzen(20, 3);
        assert_eq!(
            term.historie_zeilen(),
            4,
            "der Rueckblick wurde beim Verbreitern weggeworfen"
        );
        term.scrollen(4);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile1");
        term.zum_ende();

        // Und wieder SCHMALER (Wiederherstellen) — ebenfalls.
        term.groesse_setzen(10, 3);
        assert_eq!(
            term.historie_zeilen(),
            4,
            "der Rueckblick wurde beim Verschmaelern weggeworfen"
        );
        term.scrollen(4);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "Zeile1");
    }

    /// Wird das Fenster NIEDRIGER, wandern die oberen Zeilen in den
    /// Rückblick, statt verloren zu gehen.
    #[test_case]
    fn test_terminal_niedriger_rettet_zeilen() {
        let mut term = Terminal::neu(10, 4, HG);
        schreiben(&mut term, "eins\nzwei\ndrei\nvier");
        assert_eq!(term.historie_zeilen(), 0, "noch nichts herausgescrollt");

        // Auf 2 Zeilen schrumpfen: "eins" und "zwei" fallen oben weg.
        term.groesse_setzen(10, 2);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "drei");
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "vier");
        assert_eq!(
            term.historie_zeilen(),
            2,
            "die weggefallenen Zeilen muessen im Rueckblick landen"
        );
        term.scrollen(2);
        assert_eq!(zeile_als_text(&term, 0).trim_end(), "eins");
        assert_eq!(zeile_als_text(&term, 1).trim_end(), "zwei");
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
