// browser::suche — im Dokument suchen (Strg+F)
//
// ===========================================================================
// WAS HIER STEHT UND WAS AUSDRUECKLICH WOANDERS
//
// Die eigentliche Sucharbeit — Text aus der Anzeigeliste einsammeln,
// Gross/Klein falten, Treffer finden, Rechtecke ausrechnen — liegt in
// `speedlayout::textkarte`. Sie ist reine Geometrie und dort auf dem
// HOST getestet (Umlaute, Wortgrenzen, Inline-Grenzen).
//
// Diese Datei haelt nur den ZUSTAND einer laufenden Suche: Was wurde
// getippt, welche Treffer gibt es, auf welchem stehen wir. Wer hier
// Zeichenarithmetik findet, hat einen Fehler gefunden.
//
// ===========================================================================
// DIE TREFFER WERDEN GEMERKT, NICHT BEI JEDEM BILD NEU GESUCHT
//
// Ein Suchlauf ueber ein grosses Dokument kostet einen Durchlauf durch
// den gesamten Text (Wikipedia: rund 300 KiB). Beim Scrollen jedes Mal
// neu zu suchen hiesse, das je Bild zu tun — und Scrollen soll gerade
// NICHT rechnen (Serie 8, Teil 7).
//
// Ungueltig wird die Trefferliste genau dann, wenn sich die
// ANZEIGELISTE aendert: neue Seite, neues Layout (Breite geaendert),
// nachgeladenes Bild. Deshalb gibt es `verwerfen()`, und der Browser
// ruft es an denselben Stellen, an denen er neu layoutet.

use alloc::string::String;
use alloc::vec::Vec;
use speedlayout::textkarte::{Textkarte, Treffer};
use speedlayout::Rechteck;

/// Der Zustand der Suchleiste.
#[derive(Default)]
pub struct Suche {
    /// Ist die Leiste offen? Nur dann gehen Tastendruecke an sie.
    pub offen: bool,
    /// Was der Benutzer getippt hat.
    pub begriff: String,
    /// Die gefundenen Stellen — leer, solange nichts gesucht wurde.
    treffer: Vec<Treffer>,
    /// Der Treffer, auf dem wir gerade stehen (Index in `treffer`).
    aktuell: usize,
    /// Sind `treffer` noch gueltig? Siehe Kopfkommentar.
    gueltig: bool,
}

impl Suche {
    /// Die Leiste oeffnen. Der bisherige Begriff bleibt stehen — wer
    /// Strg+F zweimal drueckt, will meistens denselben Begriff noch
    /// einmal, nicht ein leeres Feld.
    pub fn oeffnen(&mut self) {
        self.offen = true;
    }

    /// Die Leiste schliessen und die Hervorhebung loeschen.
    pub fn schliessen(&mut self) {
        self.offen = false;
        self.treffer.clear();
        self.gueltig = false;
    }

    /// Die Trefferliste verwerfen, ohne die Leiste zu schliessen.
    ///
    /// Nach einem neuen Layout stimmen die Positionen nicht mehr. Der
    /// BEGRIFF bleibt — er wird beim naechsten Zeichnen neu gesucht.
    pub fn verwerfen(&mut self) {
        self.treffer.clear();
        self.gueltig = false;
    }

    /// Ein Zeichen anfuegen.
    pub fn tippen(&mut self, zeichen: char) {
        self.begriff.push(zeichen);
        self.verwerfen();
    }

    /// Das letzte Zeichen loeschen.
    pub fn ruecktaste(&mut self) {
        self.begriff.pop();
        self.verwerfen();
    }

    /// Sicherstellen, dass die Trefferliste zum aktuellen Begriff und
    /// zur aktuellen Anzeigeliste passt.
    ///
    /// **DER AKTUELLE TREFFER WIRD GEKLEMMT UND NICHT ZURUECKGESETZT.**
    /// Wer beim vierten Treffer steht und das Fenster schmaler zieht,
    /// soll nicht wieder beim ersten anfangen. Nur wenn es so viele
    /// Treffer gar nicht mehr gibt, rutscht er auf den letzten.
    pub fn auffrischen(&mut self, karte: &Textkarte) {
        if self.gueltig {
            return;
        }
        self.treffer = if self.begriff.is_empty() {
            Vec::new()
        } else {
            karte.suchen(&self.begriff)
        };
        self.gueltig = true;
        if self.aktuell >= self.treffer.len() {
            self.aktuell = self.treffer.len().saturating_sub(1);
        }
    }

    pub fn anzahl(&self) -> usize {
        self.treffer.len()
    }

    /// Die Nummer des aktuellen Treffers, EINSBASIERT fuer die Anzeige
    /// („3 von 12"). 0, wenn es keinen gibt.
    pub fn nummer(&self) -> usize {
        if self.treffer.is_empty() {
            0
        } else {
            self.aktuell + 1
        }
    }

    /// Zum naechsten Treffer. **Laeuft im Kreis** — nach dem letzten
    /// kommt der erste. Ein Suchfeld, das am Ende einfach stehenbleibt,
    /// sieht kaputt aus.
    pub fn weiter(&mut self) {
        if self.treffer.is_empty() {
            return;
        }
        self.aktuell = (self.aktuell + 1) % self.treffer.len();
    }

    /// Zum vorigen Treffer, ebenfalls im Kreis.
    pub fn zurueck(&mut self) {
        if self.treffer.is_empty() {
            return;
        }
        self.aktuell = if self.aktuell == 0 {
            self.treffer.len() - 1
        } else {
            self.aktuell - 1
        };
    }

    /// Der aktuelle Treffer.
    pub fn aktueller(&self) -> Option<Treffer> {
        self.treffer.get(self.aktuell).copied()
    }

    /// Alle Treffer-Rechtecke in SEITEN-Koordinaten, mit der Angabe, ob
    /// es der aktuelle ist.
    ///
    /// Der aktuelle wird anders eingefaerbt — ohne das sieht man bei
    /// zwanzig gelben Kaesten nicht, wohin „weiter" gesprungen ist.
    pub fn rechtecke(&self, karte: &Textkarte, metrik: &dyn speedlayout::Metrik) -> Vec<(Rechteck, bool)> {
        let mut aus = Vec::new();
        for (index, treffer) in self.treffer.iter().enumerate() {
            let ist_aktuell = index == self.aktuell;
            for r in karte.rechtecke(treffer.von, treffer.bis, metrik) {
                aus.push((r, ist_aktuell));
            }
        }
        aus
    }

    /// Die Zeile fuer die Statusanzeige.
    pub fn beschriftung(&self) -> String {
        let mut text = String::from("Suchen: ");
        text.push_str(&self.begriff);
        if self.begriff.is_empty() {
            return text;
        }
        if self.treffer.is_empty() {
            text.push_str("   (nichts gefunden)");
        } else {
            text.push_str(&alloc::format!(
                "   ({} von {})",
                self.nummer(),
                self.anzahl()
            ));
        }
        text
    }
}
