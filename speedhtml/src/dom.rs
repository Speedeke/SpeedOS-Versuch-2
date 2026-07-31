// speedhtml::dom — aus Token einen Baum machen, komme was wolle
//
// ===========================================================================
// DIE DAUERREGEL DIESER DATEI
//
// **ES GIBT KEIN UNGUELTIGES HTML.** Jede Bytefolge ergibt einen Baum. Der
// Aufbau hat keinen Fehlerfall, keine Panik und keinen Abbruch — er hat nur
// REGELN, was bei Unerwartetem geschieht. Was er dabei zurechtbiegen
// musste, ZAEHLT er (`Befund`), damit man es nachsehen kann, statt es zu
// vermuten.
//
// Das ist keine Nachsicht gegenueber schlechten Seiten. Es ist die einzige
// Haltung, mit der ein Browser funktioniert: Auf echten Seiten sind nicht
// geschlossene `<p>`, doppelte `</div>` und Tabellen ohne `<tbody>` der
// NORMALFALL. Ein Parser, der dabei „Syntaxfehler" sagt, zeigt nie eine
// Seite an.
//
// ===========================================================================
// WARUM EIN ARENA-BAUM UND KEIN `Rc<RefCell<Knoten>>`
//
// Der naheliegende Baum in Rust ist `Rc<RefCell<..>>` mit `Weak` fuer die
// Eltern. Er hat drei Nachteile, die hier alle wehtun:
//
//   * Eltern-Zeiger als `Weak` bedeutet `upgrade()` bei jedem Schritt nach
//     oben — und ein Layout laeuft dauernd nach oben.
//   * `RefCell` verschiebt Aliasing-Fehler in die LAUFZEIT. Ein
//     `already borrowed` waere eine Panik, und Paniken sind hier verboten.
//   * Referenzzaehler kosten Speicher je Knoten, und ein Dokument hat
//     zehntausende.
//
// Stattdessen: ein `Vec<Knoten>` und `KnotenId` als Index. Eltern und
// Kinder sind Zahlen. Keine Zyklen moeglich, kein `unsafe`, kein
// Borrow-Konflikt, und der ganze Baum ist EIN zusammenhaengender Block —
// bei 12 MiB Prozess-Heap ist auch das ein Argument.
//
// Der Preis: Ein `KnotenId` ist nicht typsicher an sein Dokument gebunden.
// Wer ihn in ein fremdes Dokument steckt, bekommt einen falschen Knoten
// statt eines Fehlers. Bei EINEM Dokument je Browserfenster ist das
// verkraftbar; `knoten()` liefert `Option`, also panickt auch das nicht.

use crate::tokenizer::{Token, Tokenizer};
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// GRENZEN
// ---------------------------------------------------------------------------

/// Was ein Dokument hoechstens kosten darf.
///
/// DIESELBE UEBERLEGUNG WIE BEI `libspeed::bild::Grenzen` (Serie 8, Teil
/// 3): „20 MB Muell duerfen nicht panicken" ist eine Zusage ueber
/// SPEICHER, nicht nur ueber Kontrollfluss. 20 MB aus lauter `<div>` sind
/// 3,3 Millionen Elemente; bei ~120 Byte je Knoten waeren das 400 MiB —
/// mehr als das Dreissigfache des Prozess-Heaps.
///
/// Wird eine Grenze erreicht, wird der Baum ABGESCHNITTEN und
/// `Befund::abgeschnitten` gesetzt. **Kein Fehler:** Ein halbes Dokument
/// ist lesbar, ein Fehler zeigt gar nichts. Dass es unvollstaendig ist,
/// steht im Befund — der Browser kann es anzeigen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grenzen {
    /// Hoechstzahl der Knoten (Elemente + Text + Kommentare).
    pub max_knoten: usize,
    /// Hoechste Schachtelungstiefe.
    ///
    /// Ein Dokument aus 100 000 mal `<div>` ist nicht boesartig gemeint,
    /// es entsteht durch kaputte Generatoren. Wichtiger: **Das spaetere
    /// Layout laeuft rekursiv**, und eine Rekursion von 100 000 Ebenen
    /// sprengt den 64-KiB-User-Stack. Diese Grenze schuetzt also nicht den
    /// Parser, sondern alles, was den Baum danach durchlaeuft.
    pub max_tiefe: usize,
    /// Hoechstlaenge EINES Textknotens in Bytes.
    pub max_text_bytes: usize,
}

impl Grenzen {
    /// Die Vorgabe: 200 000 Knoten, Tiefe 100, 1 MiB je Textknoten.
    ///
    /// Zum Vergleich: Ein grosser Wikipedia-Artikel hat rund 20 000
    /// Knoten und eine Tiefe von etwa 25. Die Vorgabe ist also
    /// zehnfacher Spielraum und trifft nur, was ohnehin kaputt ist.
    pub const fn standard() -> Grenzen {
        Grenzen {
            max_knoten: 200_000,
            max_tiefe: 100,
            max_text_bytes: 1024 * 1024,
        }
    }
}

impl Default for Grenzen {
    fn default() -> Self {
        Grenzen::standard()
    }
}

// ---------------------------------------------------------------------------
// DER BAUM
// ---------------------------------------------------------------------------

/// Ein Verweis auf einen Knoten. Nur innerhalb SEINES Dokuments gueltig.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KnotenId(pub u32);

/// Was ein Knoten ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Art {
    /// Der unsichtbare Wurzelknoten. Ein Dokument kann mehrere Kinder auf
    /// oberster Ebene haben (Kommentar, DOCTYPE, `<html>`), und ein Baum
    /// braucht genau eine Wurzel.
    Wurzel,
    Element {
        /// IMMER kleingeschrieben.
        name: String,
        attribute: Vec<(String, String)>,
    },
    Text(String),
    Kommentar(String),
    Doctype(String),
}

/// Ein Knoten im Baum.
#[derive(Debug, Clone)]
pub struct Knoten {
    pub art: Art,
    pub eltern: Option<KnotenId>,
    pub kinder: Vec<KnotenId>,
}

impl Knoten {
    /// Der Tag-Name, falls es ein Element ist.
    pub fn name(&self) -> Option<&str> {
        match &self.art {
            Art::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Der Wert eines Attributs.
    pub fn attribut(&self, name: &str) -> Option<&str> {
        match &self.art {
            Art::Element { attribute, .. } => attribute
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, w)| w.as_str()),
            _ => None,
        }
    }

    /// Der Text, falls es ein Textknoten ist.
    pub fn text(&self) -> Option<&str> {
        match &self.art {
            Art::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn ist_element(&self) -> bool {
        matches!(self.art, Art::Element { .. })
    }
}

/// Was beim Aufbau zurechtgebogen werden musste.
///
/// ===================================================================
/// WOZU DAS GEZAEHLT WIRD
///
/// Ein Parser mit Fehlererholung tut ununterbrochen Dinge, die im
/// Dokument nicht stehen. Ohne Buchhaltung ist er eine Blackbox: Sieht
/// eine Seite falsch aus, weiss niemand, ob das Dokument kaputt war oder
/// der Parser.
///
/// `htmldump` gibt diese Zahlen aus, und die Tests pruefen sie — „hat er
/// den `<p>` implizit geschlossen?" ist damit eine Frage an eine Zahl
/// statt an eine Vermutung.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Befund {
    /// Wie oft ein Tag implizit geschlossen wurde (`<p>a<p>b`).
    pub implizit_geschlossen: u32,
    /// Wie oft ein Endtag ignoriert wurde, weil nichts dazu offen war.
    pub unerwartete_endtags: u32,
    /// Wie viele Elemente beim Dokumentende noch offen waren.
    pub am_ende_geschlossen: u32,
    /// Wie oft ein Endtag mehrere Ebenen auf einmal geschlossen hat.
    pub uebersprungene_ebenen: u32,
    /// Eine Grenze hat gegriffen — der Baum ist UNVOLLSTAENDIG.
    pub abgeschnitten: bool,
    /// Zahl der Knoten (ohne Wurzel).
    pub knoten: usize,
    /// Groesste erreichte Tiefe.
    pub tiefe: usize,
}

impl Befund {
    /// Musste ueberhaupt etwas zurechtgebogen werden?
    pub fn sauber(&self) -> bool {
        self.implizit_geschlossen == 0
            && self.unerwartete_endtags == 0
            && self.am_ende_geschlossen == 0
            && self.uebersprungene_ebenen == 0
            && !self.abgeschnitten
    }
}

/// Ein geparstes Dokument.
pub struct Dokument {
    knoten: Vec<Knoten>,
    pub befund: Befund,
}

impl Dokument {
    /// Die Wurzel — immer `KnotenId(0)`, immer `Art::Wurzel`.
    pub const WURZEL: KnotenId = KnotenId(0);

    pub fn knoten(&self, id: KnotenId) -> Option<&Knoten> {
        self.knoten.get(id.0 as usize)
    }

    /// Alle Knoten in Dokumentreihenfolge (die Arena IST diese
    /// Reihenfolge — Knoten entstehen, wie sie gelesen werden).
    pub fn alle(&self) -> impl Iterator<Item = (KnotenId, &Knoten)> {
        self.knoten
            .iter()
            .enumerate()
            .map(|(i, k)| (KnotenId(i as u32), k))
    }

    pub fn anzahl(&self) -> usize {
        self.knoten.len()
    }

    /// Das erste Element mit diesem Tag-Namen.
    pub fn erstes(&self, tag: &str) -> Option<KnotenId> {
        self.alle()
            .find(|(_, k)| k.name() == Some(tag))
            .map(|(id, _)| id)
    }

    /// Alle Elemente mit diesem Tag-Namen.
    pub fn alle_mit_tag<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = KnotenId> + 'a {
        self.alle()
            .filter(move |(_, k)| k.name() == Some(tag))
            .map(|(id, _)| id)
    }

    /// Der gesamte Text unterhalb eines Knotens, zusammengesetzt.
    ///
    /// OHNE `<script>` UND `<style>` — deren Inhalt ist Programmtext und
    /// gehoert nie in den sichtbaren Text. Das ist dieselbe Entscheidung
    /// wie in `news` (Serie 7, Teil 5), und sie war dort die wichtigste:
    /// Wer nur Tags entfernt, bekommt seitenweise JavaScript zu lesen.
    ///
    /// ITERATIV, NICHT REKURSIV: Bei `max_tiefe` = 100 waere Rekursion
    /// zwar sicher, aber diese Funktion soll auch auf einem Baum laufen,
    /// den jemand anders gebaut hat.
    pub fn text_von(&self, id: KnotenId) -> String {
        let mut aus = String::new();
        let mut stapel = alloc::vec![id];
        while let Some(aktuell) = stapel.pop() {
            let Some(knoten) = self.knoten(aktuell) else {
                continue;
            };
            match &knoten.art {
                Art::Text(t) => aus.push_str(t),
                Art::Element { name, .. } if name == "script" || name == "style" => continue,
                _ => {}
            }
            // Rueckwaerts auf den Stapel, damit sie vorwaerts abgearbeitet
            // werden.
            for kind in knoten.kinder.iter().rev() {
                stapel.push(*kind);
            }
        }
        aus
    }
}

// ---------------------------------------------------------------------------
// DIE REGELN DER FEHLERERHOLUNG
// ---------------------------------------------------------------------------

/// Void-Elemente: Sie haben NIE Kinder und NIE ein Endtag.
///
/// Ein `<br>` auf den Stapel offener Elemente zu legen ist der Fehler,
/// nach dem der ganze Rest des Dokuments in einem `<br>` landet.
pub fn ist_void(name: &str) -> bool {
    matches!(
        name,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr" | "basefont" | "bgsound" | "frame" | "keygen"
    )
}

/// Elemente, die einen offenen `<p>` beenden.
///
/// `<p>` ist der haeufigste nie geschlossene Tag im Web — er DARF laut
/// Spezifikation ohne `</p>` bleiben. Ohne diese Liste verschachteln sich
/// alle folgenden Absaetze ineinander, und das Layout rueckt jeden weiter
/// ein.
fn beendet_absatz(name: &str) -> bool {
    matches!(
        name,
        "address" | "article" | "aside" | "blockquote" | "details" | "div" | "dl" | "fieldset"
            | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" | "header" | "hgroup" | "hr" | "main" | "menu" | "nav" | "ol" | "p" | "pre"
            | "section" | "summary" | "table" | "ul" | "li" | "dt" | "dd"
    )
}

/// Welche offenen Elemente ein neues Start-Tag implizit schliesst.
///
/// Liefert die Liste der Tag-Namen, die vom Stapel geraeumt werden, wenn
/// `neu` beginnt. Das ist die Tabelle, an der sich entscheidet, ob eine
/// Tabelle eine Tabelle wird oder eine Verschachtelung.
fn schliesst_implizit(neu: &str) -> &'static [&'static str] {
    match neu {
        // Ein Listenpunkt beendet den vorigen.
        "li" => &["li"],
        "dt" | "dd" => &["dt", "dd"],
        // Eine Tabellenzeile beendet die vorige — und die offenen Zellen.
        "tr" => &["td", "th", "tr"],
        "td" | "th" => &["td", "th"],
        "thead" | "tbody" | "tfoot" => &["td", "th", "tr", "thead", "tbody", "tfoot"],
        "option" => &["option"],
        "optgroup" => &["option", "optgroup"],
        // Verschachtelte <a> sind laut Spezifikation verboten und kommen
        // trotzdem vor; sie sind der Klassiker fuer „der ganze Rest der
        // Seite ist ein Link".
        "a" => &["a"],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// DER AUFBAU
// ---------------------------------------------------------------------------

struct Bauer {
    knoten: Vec<Knoten>,
    /// Die offenen Elemente, aeusserstes zuerst.
    offen: Vec<KnotenId>,
    befund: Befund,
    grenzen: Grenzen,
    /// Sobald eine Grenze greift, wird nichts mehr angelegt.
    voll: bool,
}

impl Bauer {
    fn neu(grenzen: Grenzen) -> Bauer {
        Bauer {
            knoten: alloc::vec![Knoten {
                art: Art::Wurzel,
                eltern: None,
                kinder: Vec::new(),
            }],
            offen: Vec::new(),
            befund: Befund::default(),
            grenzen,
            voll: false,
        }
    }

    /// Wohin gerade eingefuegt wird.
    fn aktuell(&self) -> KnotenId {
        *self.offen.last().unwrap_or(&Dokument::WURZEL)
    }

    /// Einen Knoten anlegen und einhaengen. `None`, wenn eine Grenze greift.
    fn anlegen(&mut self, art: Art) -> Option<KnotenId> {
        if self.voll {
            return None;
        }
        if self.knoten.len() >= self.grenzen.max_knoten {
            self.voll = true;
            self.befund.abgeschnitten = true;
            return None;
        }
        let eltern = self.aktuell();
        let id = KnotenId(self.knoten.len() as u32);
        self.knoten.push(Knoten {
            art,
            eltern: Some(eltern),
            kinder: Vec::new(),
        });
        self.knoten[eltern.0 as usize].kinder.push(id);
        self.befund.knoten = self.knoten.len() - 1;
        Some(id)
    }

    /// Steht dieses Element auf dem Stapel?
    fn offen_hat(&self, name: &str) -> bool {
        self.offen
            .iter()
            .any(|id| self.knoten[id.0 as usize].name() == Some(name))
    }

    /// Den obersten Stapeleintrag schliessen.
    fn schliessen(&mut self) {
        self.offen.pop();
    }

    /// Alle offenen Elemente aus `namen` vom Stapel raeumen — aber nur,
    /// solange sie GANZ OBEN liegen.
    ///
    /// Die Einschraenkung ist wichtig: `<li><div>` darf den `<li>` NICHT
    /// schliessen, obwohl ein `<div>` in der Absatzliste steht — der
    /// `<div>` gehoert IN den Listenpunkt. Geschlossen wird nur, was
    /// unmittelbar offen ist.
    fn implizit_schliessen(&mut self, namen: &[&str]) {
        while let Some(oben) = self.offen.last() {
            let Some(name) = self.knoten[oben.0 as usize].name() else {
                break;
            };
            if namen.contains(&name) {
                self.offen.pop();
                self.befund.implizit_geschlossen += 1;
            } else {
                break;
            }
        }
    }

    fn start_tag(&mut self, name: String, attribute: Vec<(String, String)>, selbst: bool) {
        // (1) Implizite Schliessungen VOR dem Einfuegen.
        let regel = schliesst_implizit(&name);
        if !regel.is_empty() {
            self.implizit_schliessen(regel);
        }
        // (2) Ein Blockelement beendet einen offenen Absatz.
        if beendet_absatz(&name) {
            self.implizit_schliessen(&["p"]);
        }

        // (3) Tiefe pruefen — BEVOR angelegt wird.
        if self.offen.len() >= self.grenzen.max_tiefe {
            // Zu tief: Das Element wird angelegt, aber NICHT geoeffnet.
            // Sein Inhalt landet damit beim Elternteil — flacher als
            // gemeint, aber vollstaendig. Das Dokument zu verwerfen waere
            // die schlechtere Wahl.
            self.befund.abgeschnitten = true;
            let void = ist_void(&name);
            self.anlegen(Art::Element { name, attribute });
            let _ = void;
            return;
        }

        let void = ist_void(&name);
        let Some(id) = self.anlegen(Art::Element { name, attribute }) else {
            return;
        };
        // (4) Void-Elemente und selbstschliessende kommen NICHT auf den
        // Stapel — sonst landete der Rest des Dokuments in ihnen.
        if !void && !selbst {
            self.offen.push(id);
            self.befund.tiefe = self.befund.tiefe.max(self.offen.len());
        }
    }

    fn end_tag(&mut self, name: &str) {
        // Ein Endtag fuer ein Void-Element (`</br>`) ist bedeutungslos.
        if ist_void(name) {
            self.befund.unerwartete_endtags += 1;
            return;
        }
        // Nichts Passendes offen -> IGNORIEREN. Der haeufigste Fall ist
        // ein doppeltes `</div>`; es zu befolgen wuerde ein Element
        // schliessen, das jemand anders geoeffnet hat, und ab da ist der
        // Baum falsch.
        if !self.offen_hat(name) {
            self.befund.unerwartete_endtags += 1;
            return;
        }
        // Passendes offen: bis dorthin schliessen. Was dazwischen liegt,
        // war nie geschlossen (`<b><i></b>`) — das wird mitgeschlossen
        // und gezaehlt.
        let mut ebenen = 0u32;
        while let Some(oben) = self.offen.last().copied() {
            let treffer = self.knoten[oben.0 as usize].name() == Some(name);
            self.schliessen();
            if treffer {
                break;
            }
            ebenen += 1;
        }
        if ebenen > 0 {
            self.befund.uebersprungene_ebenen += ebenen;
        }
    }

    fn text(&mut self, mut inhalt: String) {
        if inhalt.is_empty() {
            return;
        }
        if inhalt.len() > self.grenzen.max_text_bytes {
            // Auf einer ZEICHENGRENZE abschneiden, nicht auf einer
            // Bytegrenze — sonst panickt `truncate` mitten in einem
            // Umlaut.
            let mut bis = self.grenzen.max_text_bytes;
            while bis > 0 && !inhalt.is_char_boundary(bis) {
                bis -= 1;
            }
            inhalt.truncate(bis);
            self.befund.abgeschnitten = true;
        }
        self.anlegen(Art::Text(inhalt));
    }

    fn fertig(mut self) -> Dokument {
        self.befund.am_ende_geschlossen = self.offen.len() as u32;
        self.befund.knoten = self.knoten.len() - 1;
        Dokument {
            knoten: self.knoten,
            befund: self.befund,
        }
    }
}

/// HTML parsen, mit den Standard-Grenzen.
pub fn parsen(html: &str) -> Dokument {
    parsen_mit(html, Grenzen::standard())
}

/// HTML parsen, mit ausdruecklichen Grenzen.
///
/// **PANICKT NIE, GIBT NIE AUF.** Es gibt keinen Rueckgabewert `Result`,
/// weil es keinen Fehlerfall gibt: Jede Eingabe ergibt einen Baum. Was
/// dabei zurechtgebogen oder abgeschnitten wurde, steht in
/// `Dokument::befund`.
pub fn parsen_mit(html: &str, grenzen: Grenzen) -> Dokument {
    let mut bauer = Bauer::neu(grenzen);

    for token in Tokenizer::neu(html) {
        match token {
            Token::Text(t) => bauer.text(t),
            Token::StartTag {
                name,
                attribute,
                selbst_schliessend,
            } => bauer.start_tag(name, attribute, selbst_schliessend),
            Token::EndTag { name } => bauer.end_tag(&name),
            Token::Kommentar(k) => {
                bauer.anlegen(Art::Kommentar(k));
            }
            Token::Doctype(d) => {
                bauer.anlegen(Art::Doctype(d));
            }
        }
        // Sobald eine Grenze gegriffen hat, hat Weiterlesen keinen Zweck
        // mehr — der Tokenizer laeuft sonst noch durch 20 MB, um nichts
        // mehr anzulegen.
        if bauer.voll {
            break;
        }
    }

    bauer.fertig()
}
