// speedhtml::tokenizer — Bytes zu Token, nach dem Vorbild der
//                        HTML5-Zustandsmaschine
//
// ===========================================================================
// WARUM DIE STRUKTUR DER SPEZIFIKATION UND NICHT ETWAS EIGENES
//
// Ein naiver HTML-Zerleger sucht `<`, dann `>`, und nimmt an, was
// dazwischen steht, sei ein Tag. Er funktioniert auf jeder Seite, die
// jemand von Hand geschrieben hat, und auf keiner echten.
//
// Die HTML5-Zustandsmaschine ist etwas anderes: Sie ist **rueckwirkend
// aufgeschrieben worden, um zu beschreiben, was Browser mit KAPUTTEM HTML
// tun** — und deshalb hat sie fuer jeden unmoeglichen Zustand einen
// definierten Ausgang. `<` mitten im Text, ein Tag ohne `>`, ein
// Attributwert ohne Anfuehrungszeichen, ein `<` als letztes Zeichen der
// Datei: Fuer all das gibt es eine Regel, und keine davon lautet „Fehler".
//
// Wir setzen sie NICHT vollstaendig um (die Spezifikation hat ueber 80
// Zustaende). Wir uebernehmen ihre STRUKTUR und die Zustaende, die fuer
// gewoehnliche Dokumente zaehlen — rund zwanzig. Welche fehlen und was das
// kostet, steht unten in §3.
//
// ===========================================================================
// §1 DIE ZUSTAENDE, DIE ES GIBT
//
//   Daten                 gewoehnlicher Text; `<` fuehrt heraus
//   TagAuf                gerade `<` gelesen
//   EndTagAuf             gerade `</` gelesen
//   TagName               im Namen eines Tags
//   VorAttributName       zwischen Attributen
//   AttributName          im Namen eines Attributs
//   NachAttributName      hinter einem Namen (kommt `=`?)
//   VorAttributWert       hinter dem `=`
//   AttributWertDoppelt   in "..."
//   AttributWertEinfach   in '...'
//   AttributWertNackt     ohne Anfuehrungszeichen
//   NachAttributWert      hinter einem beendeten Wert
//   SelbstSchliessend     gerade `/` in einem Tag gelesen
//   BogusKommentar        `<!` ohne `--`, `<?`, `</` ohne Namen
//   MarkupDeklaration     hinter `<!`
//   KommentarStart/-Ende  in `<!-- ... -->`
//   Doctype               in `<!DOCTYPE ...>`
//   RohText               in <script>/<style> — kein Markup
//   RcData                in <title>/<textarea> — kein Markup, ABER Entities
//
// ===========================================================================
// §2 DIE ZWEI ROHTEXT-ZUSTAENDE SIND DIE WICHTIGSTEN
//
// In `<script>` steht Programmtext, und darin steht `if (a < b)`. Ein
// Tokenizer ohne RohText-Zustand findet dort einen Tag-Anfang und
// verschluckt den Rest der Seite bis zum naechsten `>`. Das ist kein
// Randfall — es ist das erste, woran ein selbstgebauter Parser stirbt.
//
// Verlassen wird der Zustand NUR durch das passende Endtag (`</script`),
// und zwar Gross-/Kleinschreibung-egal. Alles andere ist Text.
//
// ===========================================================================
// §3 WAS FEHLT, UND WAS ES KOSTET
//
//   * CDATA-Abschnitte (`<![CDATA[...]]>`) — nur in Fremdinhalten (SVG,
//     MathML) gueltig, die wir ohnehin nicht rendern. Wird als
//     Bogus-Kommentar geschluckt.
//   * Der Unterschied zwischen `script`-Datenzustaenden (`<!--` in
//     Skripten kann dort verschachtelt sein). Bei uns endet RohText am
//     ersten passenden Endtag. Ein Skript, das den String `"</script>"`
//     enthaelt, endet also zu frueh — dasselbe tut jeder Browser, weshalb
//     man in echten Skripten `<\/script>` schreibt.
//   * Zeichenreferenzen in Attributwerten werden am Stueck aufgeloest
//     (`entitaeten::aufloesen`) statt zeichenweise im Automaten. Ergebnis
//     identisch, Code kuerzer.
//
// ===========================================================================
// §4 DIE GRENZEN, DIE ES BRAUCHT
//
// „20 MB Muell duerfen nicht panicken" ist eine Zusage ueber SPEICHER,
// nicht nur ueber Kontrollfluss. Ein Dokument aus lauter `<a b=1 c=2 ...>`
// kann beliebig viele Attribute an einem Tag haben; eines aus lauter `<`
// beliebig viele Token. Der Tokenizer deckelt deshalb die Attributzahl je
// Tag und die Laenge von Namen — der DOM-Aufbau deckelt den Rest
// (`dom::Grenzen`).

use crate::entitaeten;
use alloc::string::String;
use alloc::vec::Vec;

/// Hoechstens so viele Attribute je Tag. Danach wird gelesen und
/// WEGGEWORFEN — der Tag bleibt gueltig, er bekommt nur nicht mehr
/// Attribute. Abbrechen waere schlechter: Ein Tag mit 300 Attributen ist
/// selten boese, meistens ist es generierter Unsinn.
pub const MAX_ATTRIBUTE: usize = 256;
/// Hoechstlaenge eines Tag- oder Attributnamens in Bytes.
pub const MAX_NAME: usize = 128;

/// Ein Token — mehr Sorten gibt es in HTML nicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Zeichendaten. Referenzen sind schon aufgeloest.
    Text(String),
    /// `<div class="a">` — Name IMMER kleingeschrieben.
    StartTag {
        name: String,
        attribute: Vec<(String, String)>,
        /// `<br/>` — in HTML nur bei Void-Elementen bedeutsam, aber der
        /// Baum-Aufbau will es wissen.
        selbst_schliessend: bool,
    },
    /// `</div>` — Name IMMER kleingeschrieben.
    EndTag { name: String },
    /// `<!-- ... -->`
    Kommentar(String),
    /// `<!DOCTYPE html>`
    Doctype(String),
}

impl Token {
    /// Der Tag-Name, falls es ein Tag ist.
    pub fn name(&self) -> Option<&str> {
        match self {
            Token::StartTag { name, .. } | Token::EndTag { name } => Some(name),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zustand {
    Daten,
    TagAuf,
    EndTagAuf,
    TagName,
    VorAttributName,
    AttributName,
    NachAttributName,
    VorAttributWert,
    AttributWertDoppelt,
    AttributWertEinfach,
    AttributWertNackt,
    NachAttributWert,
    SelbstSchliessend,
    BogusKommentar,
    MarkupDeklaration,
    Kommentar,
    Doctype,
    /// `<script>`, `<style>` — Inhalt ist Text, KEINE Referenzen.
    RohText,
    /// `<title>`, `<textarea>` — Text, aber MIT Referenzen.
    RcData,
}

/// Elemente, deren Inhalt nicht als Markup gelesen wird.
///
/// Die Unterscheidung ist keine Feinheit: In `<script>` steht `a < b`, in
/// `<title>` steht `Tom &amp; Jerry`. Ersteres darf nicht als Tag gelesen
/// werden, letzteres muss aufgeloest werden.
fn roh_art(name: &str) -> Option<Zustand> {
    match name {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(Zustand::RohText),
        "title" | "textarea" => Some(Zustand::RcData),
        _ => None,
    }
}

/// Zerlegt HTML in Token.
///
/// Ein `Iterator` — der Aufrufer bekommt Token, sobald sie fertig sind,
/// und der Tokenizer haelt nie das ganze Dokument als Token-Liste im
/// Speicher. Bei 20 MB Eingabe ist das der Unterschied zwischen
/// „funktioniert" und „Heap voll".
pub struct Tokenizer<'a> {
    eingabe: &'a str,
    /// Byte-Position. Zeigt IMMER auf eine Zeichengrenze — jeder
    /// Vorschub geht ueber `len_utf8()`, nie ueber `+1` auf Verdacht.
    pos: usize,
    zustand: Zustand,
    /// Der Name des Elements, dessen Rohtext gerade gelesen wird.
    roh_name: String,
    /// Fertige Token, die noch abgeholt werden muessen (ein Tag kann
    /// zusammen mit vorangehendem Text entstehen).
    warteschlange: Vec<Token>,
    fertig: bool,
}

impl<'a> Tokenizer<'a> {
    pub fn neu(eingabe: &'a str) -> Tokenizer<'a> {
        Tokenizer {
            eingabe,
            pos: 0,
            zustand: Zustand::Daten,
            roh_name: String::new(),
            warteschlange: Vec::new(),
            fertig: false,
        }
    }

    /// Das Zeichen an `pos`, ohne zu verbrauchen.
    #[inline]
    fn schauen(&self) -> Option<char> {
        self.eingabe[self.pos..].chars().next()
    }

    /// Steht ab `pos` (case-insensitiv) dieser ASCII-Text?
    fn passt(&self, muster: &str) -> bool {
        let rest = self.eingabe.as_bytes();
        if self.pos + muster.len() > rest.len() {
            return false;
        }
        rest[self.pos..self.pos + muster.len()]
            .eq_ignore_ascii_case(muster.as_bytes())
    }

    /// Liest einen Namen (Tag oder Attribut) ab `pos`, kleingeschrieben.
    ///
    /// Endet an Leerraum, `/`, `>`, `=` oder Dateiende — und spaetestens
    /// nach `MAX_NAME` Bytes. Ein Name, der laenger ist, wird
    /// ABGESCHNITTEN und nicht verworfen: Der Tag bleibt damit gueltig,
    /// heisst nur anders — bei generiertem Unsinn ist das folgenlos, bei
    /// einem Angriff ebenso.
    fn name_lesen(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.schauen() {
            if c.is_whitespace() || c == '/' || c == '>' || c == '=' || c == '<' {
                break;
            }
            if name.len() < MAX_NAME {
                // ASCII kleinschreiben: HTML-Tagnamen sind ASCII, und
                // `to_lowercase` auf beliebigem Unicode kann aus EINEM
                // Zeichen MEHRERE machen (ß -> ss) — das wollen wir in
                // einem Tagnamen nicht.
                name.push(c.to_ascii_lowercase());
            }
            self.pos += c.len_utf8();
        }
        name
    }

    fn leerraum_ueberspringen(&mut self) {
        while let Some(c) = self.schauen() {
            if !c.is_whitespace() {
                break;
            }
            self.pos += c.len_utf8();
        }
    }
}

/// Der Zustand eines Tags, waehrend er zusammengesetzt wird.
struct TagBau {
    name: String,
    attribute: Vec<(String, String)>,
    ende: bool,
    selbst_schliessend: bool,
    attr_name: String,
    attr_wert: String,
}

impl TagBau {
    fn neu(name: String, ende: bool) -> TagBau {
        TagBau {
            name,
            attribute: Vec::new(),
            ende,
            selbst_schliessend: false,
            attr_name: String::new(),
            attr_wert: String::new(),
        }
    }

    /// Das laufende Attribut ablegen.
    ///
    /// DOPPELTE NAMEN: Das ERSTE gewinnt, spaetere werden verworfen — so
    /// schreibt es die Spezifikation vor, und es ist auch das
    /// Vernuenftige (`<a href=gut href=boese>`).
    fn attribut_ablegen(&mut self) {
        if self.attr_name.is_empty() {
            self.attr_wert.clear();
            return;
        }
        if self.attribute.len() < MAX_ATTRIBUTE
            && !self.attribute.iter().any(|(n, _)| *n == self.attr_name)
        {
            let wert = entitaeten::aufloesen(&self.attr_wert);
            self.attribute.push((core::mem::take(&mut self.attr_name), wert));
        } else {
            self.attr_name.clear();
        }
        self.attr_wert.clear();
    }

    fn fertig(mut self) -> Token {
        self.attribut_ablegen();
        if self.ende {
            // Ein Endtag hat laut Spezifikation keine Attribute — sie
            // werden gelesen (sonst waere `</a href=x>` ein Textbruch)
            // und dann verworfen.
            Token::EndTag { name: self.name }
        } else {
            Token::StartTag {
                name: self.name,
                attribute: self.attribute,
                selbst_schliessend: self.selbst_schliessend,
            }
        }
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            if let Some(t) = self.warteschlange.pop() {
                return Some(t);
            }
            if self.fertig {
                return None;
            }
            if self.pos >= self.eingabe.len() {
                self.fertig = true;
                // Ein angefangener Text am Dateiende geht nicht verloren —
                // darum kuemmert sich der jeweilige Zustand unten.
                return self.warteschlange.pop();
            }
            if let Some(token) = self.schritt() {
                return Some(token);
            }
            // Kein Token in diesem Schritt (z. B. nur Leerraum
            // uebersprungen) — weiterlaufen.
        }
    }
}

impl<'a> Tokenizer<'a> {
    /// Ein Durchlauf des Automaten. Liefert hoechstens ein Token; weitere
    /// landen in der Warteschlange.
    fn schritt(&mut self) -> Option<Token> {
        match self.zustand {
            Zustand::Daten => self.daten(),
            Zustand::RohText | Zustand::RcData => self.rohtext(),
            _ => self.markup(),
        }
    }

    /// Gewoehnlicher Text bis zum naechsten `<`.
    ///
    /// DER `<`-IM-TEXT-FALL: Ein `<`, dem kein Buchstabe, kein `/` und
    /// kein `!` folgt, ist KEIN Tag-Anfang — es ist ein Kleinerzeichen
    /// („5 < 7"). Die Spezifikation sagt das so, und ohne diese Regel
    /// verschwindet bei jeder Mathe-Seite der halbe Text.
    fn daten(&mut self) -> Option<Token> {
        let start = self.pos;
        let mut text = String::new();

        while let Some(c) = self.schauen() {
            if c == '<' {
                // Ist das wirklich ein Tag-Anfang?
                let danach = self.eingabe[self.pos + 1..].chars().next();
                let tag = matches!(danach, Some(d) if d.is_ascii_alphabetic() || d == '/' || d == '!' || d == '?');
                if tag {
                    break;
                }
                // Nein: gewoehnliches Zeichen.
                text.push('<');
                self.pos += 1;
                continue;
            }
            if c == '&' {
                if let Some((zeichen, verbraucht)) = entitaeten::lesen(&self.eingabe[self.pos + 1..])
                {
                    text.push(zeichen);
                    self.pos += 1 + verbraucht;
                    continue;
                }
            }
            text.push(c);
            self.pos += c.len_utf8();
        }

        if self.pos < self.eingabe.len() {
            self.zustand = Zustand::TagAuf;
            self.pos += 1; // das `<`
        }

        if text.is_empty() && self.pos > start {
            return None; // nichts als der Tag-Anfang
        }
        if text.is_empty() {
            return None;
        }
        Some(Token::Text(text))
    }

    /// In `<script>`/`<style>`/`<title>`/`<textarea>`: alles ist Text, bis
    /// das passende Endtag kommt.
    fn rohtext(&mut self) -> Option<Token> {
        let start = self.pos;
        let mut ende_bei = None;

        // Nach `</name` suchen, Gross-/Kleinschreibung egal.
        let mut i = self.pos;
        while i < self.eingabe.len() {
            let rest = self.eingabe.as_bytes();
            if rest[i] == b'<' && i + 1 < rest.len() && rest[i + 1] == b'/' {
                let ab = i + 2;
                let bis = ab + self.roh_name.len();
                if bis <= rest.len() && rest[ab..bis].eq_ignore_ascii_case(self.roh_name.as_bytes())
                {
                    // Danach muss Leerraum, `/` oder `>` kommen — sonst
                    // waere `</scriptfoo>` ein Ende von `<script>`.
                    let folgt = rest.get(bis).copied();
                    if matches!(folgt, None | Some(b'>') | Some(b'/')) || folgt.is_some_and(|c| c.is_ascii_whitespace())
                    {
                        ende_bei = Some(i);
                        break;
                    }
                }
            }
            i += 1;
        }

        let (text_bis, weiter) = match ende_bei {
            Some(e) => (e, e),
            // Kein Endtag mehr — der Rest der Datei ist Inhalt. (Ein
            // abgeschnittenes Dokument mitten im <script>.)
            None => (self.eingabe.len(), self.eingabe.len()),
        };

        let roh = &self.eingabe[start..text_bis];
        self.pos = weiter;
        self.zustand = Zustand::Daten;

        if roh.is_empty() {
            return None;
        }
        // RcData loest Referenzen auf, RohText nicht.
        let text = if self.zustand_war_rcdata() {
            entitaeten::aufloesen(roh)
        } else {
            String::from(roh)
        };
        Some(Token::Text(text))
    }

    /// Hilfsfrage fuer `rohtext` — der Zustand ist dort schon
    /// zurueckgesetzt, die Art haengt am Namen.
    fn zustand_war_rcdata(&self) -> bool {
        matches!(roh_art(&self.roh_name), Some(Zustand::RcData))
    }

    /// Alles zwischen `<` und `>`.
    fn markup(&mut self) -> Option<Token> {
        match self.zustand {
            Zustand::TagAuf => {
                match self.schauen() {
                    Some('/') => {
                        self.pos += 1;
                        self.zustand = Zustand::EndTagAuf;
                    }
                    Some('!') => {
                        self.pos += 1;
                        self.zustand = Zustand::MarkupDeklaration;
                    }
                    Some('?') => {
                        // `<?xml ...>` — laut Spezifikation ein
                        // Bogus-Kommentar, kein Fehler.
                        self.zustand = Zustand::BogusKommentar;
                    }
                    Some(c) if c.is_ascii_alphabetic() => self.zustand = Zustand::TagName,
                    // `<` gefolgt von etwas anderem kam in `daten` gar
                    // nicht hierher; zur Sicherheit trotzdem behandelt.
                    _ => {
                        self.zustand = Zustand::Daten;
                        return Some(Token::Text(String::from("<")));
                    }
                }
                None
            }
            Zustand::EndTagAuf => {
                match self.schauen() {
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.zustand = Zustand::TagName;
                        self.tag_lesen(true)
                    }
                    // `</>` — laut Spezifikation wird es ERSATZLOS
                    // verworfen (kein Text, kein Token).
                    Some('>') => {
                        self.pos += 1;
                        self.zustand = Zustand::Daten;
                        None
                    }
                    _ => {
                        self.zustand = Zustand::BogusKommentar;
                        None
                    }
                }
            }
            Zustand::TagName => self.tag_lesen(false),
            Zustand::MarkupDeklaration => {
                if self.passt("--") {
                    self.pos += 2;
                    self.zustand = Zustand::Kommentar;
                } else if self.passt("doctype") {
                    self.pos += 7;
                    self.zustand = Zustand::Doctype;
                } else if self.passt("[CDATA[") {
                    // Nur in Fremdinhalten gueltig — bei uns ein Kommentar.
                    self.zustand = Zustand::BogusKommentar;
                } else {
                    self.zustand = Zustand::BogusKommentar;
                }
                None
            }
            Zustand::Kommentar => self.kommentar(),
            Zustand::BogusKommentar => self.bogus_kommentar(),
            Zustand::Doctype => self.doctype(),
            // Die Attribut-Zustaende werden innerhalb von `tag_lesen`
            // durchlaufen; hier landet nichts.
            _ => {
                self.zustand = Zustand::Daten;
                None
            }
        }
    }

    /// Einen ganzen Tag lesen — Name, Attribute, Ende.
    ///
    /// Der Automat der Spezifikation wechselt hier zwischen zehn
    /// Zustaenden. Weil ein Tag immer am Stueck gelesen wird (er kann
    /// nicht unterbrochen werden wie Text), stehen sie hier als EINE
    /// Schleife — die Zustaende sind dieselben, nur als lokale Variable
    /// statt als Feld. Das spart die Haelfte des Codes und macht die
    /// Uebergaenge an einer Stelle lesbar.
    fn tag_lesen(&mut self, ende: bool) -> Option<Token> {
        let name = self.name_lesen();
        let mut bau = TagBau::neu(name, ende);
        let mut zustand = Zustand::VorAttributName;

        loop {
            let Some(c) = self.schauen() else {
                // DATEIENDE MITTEN IM TAG. Die Spezifikation wirft den
                // angefangenen Tag weg. Wir auch — ein halber Tag ist
                // keine Auszeichnung, und ihn zu raten waere schlimmer.
                self.zustand = Zustand::Daten;
                self.fertig = true;
                return None;
            };

            match zustand {
                Zustand::VorAttributName => {
                    if c.is_whitespace() {
                        self.pos += c.len_utf8();
                    } else if c == '>' {
                        self.pos += 1;
                        return Some(self.tag_abschliessen(bau));
                    } else if c == '/' {
                        self.pos += 1;
                        zustand = Zustand::SelbstSchliessend;
                    } else if c == '=' {
                        // `<a =b>` — die Spezifikation macht daraus ein
                        // Attribut namens "=". Kurios, aber definiert.
                        self.pos += 1;
                        bau.attr_name.push('=');
                        zustand = Zustand::AttributName;
                    } else {
                        // ==================================================
                        // DIE STELLE, AN DER DER AUTOMAT STEHENBLEIBEN KANN
                        //
                        // `name_lesen` bricht unter anderem bei `<` ab. Steht
                        // hier also ein `<` (`<p>a<b</p>` — ein Tag, der nie
                        // geschlossen wurde, gefolgt vom naechsten), liefert
                        // es den LEEREN Namen, ohne `pos` zu bewegen. Der
                        // Zustandswechsel fuehrt dann ueber
                        // `NachAttributName` zurueck hierher, und die
                        // Schleife dreht sich fuer immer.
                        //
                        // Gefunden hat das der Muellfolgen-Test, nicht das
                        // Nachdenken. Deshalb steht hier jetzt die
                        // INVARIANTE, auf die es ankommt:
                        //
                        //   **JEDER Durchlauf dieser Schleife muss `pos`
                        //   bewegen ODER den Tag beenden.**
                        //
                        // Ein leerer Name heisst: Das Zeichen gehoert
                        // nirgendwohin. Es wird verbraucht und verworfen —
                        // damit ist der Fortschritt garantiert, und `<b`
                        // wird zu einem Tag `b` ohne Attribute.
                        let name = self.name_lesen();
                        if name.is_empty() {
                            self.pos += c.len_utf8();
                        } else {
                            bau.attr_name = name;
                            zustand = Zustand::NachAttributName;
                        }
                    }
                }
                Zustand::AttributName => {
                    // Auch hier gilt die Invariante von oben: Bringt
                    // `name_lesen` nichts, wird das Zeichen verbraucht.
                    let rest = self.name_lesen();
                    if rest.is_empty() {
                        self.pos += c.len_utf8();
                    } else {
                        bau.attr_name.push_str(&rest);
                    }
                    zustand = Zustand::NachAttributName;
                }
                Zustand::NachAttributName => {
                    if c.is_whitespace() {
                        self.leerraum_ueberspringen();
                    } else if c == '=' {
                        self.pos += 1;
                        zustand = Zustand::VorAttributWert;
                    } else {
                        // Attribut OHNE Wert (`<input disabled>`) — der
                        // Wert ist der leere String, nicht „fehlt".
                        bau.attribut_ablegen();
                        zustand = Zustand::VorAttributName;
                    }
                }
                Zustand::VorAttributWert => {
                    if c.is_whitespace() {
                        self.leerraum_ueberspringen();
                    } else if c == '"' {
                        self.pos += 1;
                        zustand = Zustand::AttributWertDoppelt;
                    } else if c == '\'' {
                        self.pos += 1;
                        zustand = Zustand::AttributWertEinfach;
                    } else if c == '>' {
                        // `<a href=>` — leerer Wert.
                        self.pos += 1;
                        return Some(self.tag_abschliessen(bau));
                    } else {
                        zustand = Zustand::AttributWertNackt;
                    }
                }
                Zustand::AttributWertDoppelt | Zustand::AttributWertEinfach => {
                    let schluss = if zustand == Zustand::AttributWertDoppelt { '"' } else { '\'' };
                    // Bis zum schliessenden Anfuehrungszeichen — ODER bis
                    // zum Dateiende. Ein nie geschlossenes
                    // Anfuehrungszeichen frisst sonst den Rest der Datei,
                    // und das tut es hier auch; der Unterschied ist, dass
                    // es definiert endet statt zu haengen.
                    let ab = self.pos;
                    let mut bis = ab;
                    let bytes = self.eingabe.as_bytes();
                    while bis < bytes.len() && bytes[bis] != schluss as u8 {
                        bis += 1;
                    }
                    bau.attr_wert.push_str(&self.eingabe[ab..bis]);
                    self.pos = if bis < bytes.len() { bis + 1 } else { bis };
                    bau.attribut_ablegen();
                    zustand = Zustand::NachAttributWert;
                }
                Zustand::AttributWertNackt => {
                    // Endet an Leerraum oder `>`. DER FALL, DEN MAN
                    // FALSCH MACHT: `<a href=/pfad/>` — der Schraegstrich
                    // vor dem `>` gehoert zum WERT, nicht zum Tag. Deshalb
                    // wird `/` hier NICHT als Ende gewertet.
                    let ab = self.pos;
                    let mut bis = ab;
                    let bytes = self.eingabe.as_bytes();
                    while bis < bytes.len()
                        && !bytes[bis].is_ascii_whitespace()
                        && bytes[bis] != b'>'
                    {
                        bis += 1;
                    }
                    bau.attr_wert.push_str(&self.eingabe[ab..bis]);
                    self.pos = bis;
                    bau.attribut_ablegen();
                    zustand = Zustand::VorAttributName;
                }
                Zustand::NachAttributWert => {
                    if c.is_whitespace() {
                        self.leerraum_ueberspringen();
                        zustand = Zustand::VorAttributName;
                    } else if c == '>' {
                        self.pos += 1;
                        return Some(self.tag_abschliessen(bau));
                    } else if c == '/' {
                        self.pos += 1;
                        zustand = Zustand::SelbstSchliessend;
                    } else {
                        zustand = Zustand::VorAttributName;
                    }
                }
                Zustand::SelbstSchliessend => {
                    if c == '>' {
                        self.pos += 1;
                        bau.selbst_schliessend = true;
                        return Some(self.tag_abschliessen(bau));
                    }
                    // `<a / b>` — der Schraegstrich war Unsinn, weiter.
                    zustand = Zustand::VorAttributName;
                }
                _ => unreachable!("tag_lesen kennt nur Tag-Zustaende"),
            }
        }
    }

    /// Tag fertigstellen und den Folgezustand setzen.
    fn tag_abschliessen(&mut self, bau: TagBau) -> Token {
        let ist_ende = bau.ende;
        let name_kopie = bau.name.clone();
        let selbst = bau.selbst_schliessend;
        let token = bau.fertig();

        // Nach `<script>` beginnt Rohtext — aber nicht nach `<script/>`
        // und nicht nach `</script>`.
        self.zustand = Zustand::Daten;
        if !ist_ende && !selbst {
            if let Some(art) = roh_art(&name_kopie) {
                self.zustand = art;
                self.roh_name = name_kopie;
            }
        }
        token
    }

    /// `<!-- ... -->`
    fn kommentar(&mut self) -> Option<Token> {
        let ab = self.pos;
        let rest = &self.eingabe[ab..];
        let (inhalt, weiter) = match rest.find("-->") {
            Some(i) => (&rest[..i], ab + i + 3),
            // Nie geschlossener Kommentar: bis Dateiende. Kein Fehler —
            // die Spezifikation sagt genau das.
            None => (rest, self.eingabe.len()),
        };
        self.pos = weiter;
        self.zustand = Zustand::Daten;
        Some(Token::Kommentar(String::from(inhalt)))
    }

    /// `<!foo>`, `<?xml?>`, `</ >` — alles, was aussieht wie Markup und
    /// keines ist. Wird als Kommentar geschluckt (so die Spezifikation),
    /// damit der Text nicht sichtbar wird.
    fn bogus_kommentar(&mut self) -> Option<Token> {
        let ab = self.pos;
        let rest = &self.eingabe[ab..];
        let (inhalt, weiter) = match rest.find('>') {
            Some(i) => (&rest[..i], ab + i + 1),
            None => (rest, self.eingabe.len()),
        };
        self.pos = weiter;
        self.zustand = Zustand::Daten;
        Some(Token::Kommentar(String::from(inhalt)))
    }

    /// `<!DOCTYPE html>` — wir merken uns nur den Namen.
    ///
    /// Der Quirks-Modus (verschiedene Layout-Regeln je nach DOCTYPE) ist
    /// bewusst draussen: Er ist eine Ruecksicht auf Seiten aus den 90ern,
    /// und wir haben nur EINEN Layout-Modus.
    fn doctype(&mut self) -> Option<Token> {
        let ab = self.pos;
        let rest = &self.eingabe[ab..];
        let (inhalt, weiter) = match rest.find('>') {
            Some(i) => (&rest[..i], ab + i + 1),
            None => (rest, self.eingabe.len()),
        };
        self.pos = weiter;
        self.zustand = Zustand::Daten;
        let name: String = inhalt.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        Some(Token::Doctype(name))
    }
}
