// speedcss::parser — aus CSS-Text Regeln machen, komme was wolle
//
// ===========================================================================
// DIESELBE HALTUNG WIE BEI HTML
//
// **Kaputtes CSS ist der Normalfall.** Jede echte Seite benutzt
// Eigenschaften, die es 2015 noch nicht gab, Praefixe fuer Browser, die es
// nicht mehr gibt, und mindestens eine Regel, in der eine Klammer fehlt.
//
// Die CSS-Spezifikation hat dafuer — anders als HTML — eine ausdrueckliche,
// kurze und sehr gute Regel, die **Error Handling**-Sektion:
//
//   * Eine Deklaration, die man nicht versteht, wird BIS ZUM `;`
//     uebersprungen.
//   * Eine Regel, deren Selektor man nicht versteht, wird MITSAMT ihrem
//     Block uebersprungen.
//   * Eine At-Regel, die man nicht kennt, wird bis zum `;` oder bis zum
//     Ende ihres Blocks uebersprungen.
//
// Das Entscheidende dabei ist die BALANCIERTE Klammerung: Wer beim
// Ueberspringen nur das naechste `}` sucht, endet mitten in einer
// verschachtelten Regel und interpretiert danach Deklarationen als
// Selektoren. Genau das ist der Grund, warum `@media` in dieser Kiste
// sauber uebersprungen werden MUSS und nicht einfach ignoriert werden
// kann — der Block dahinter enthaelt weitere Bloecke.
//
// ===========================================================================
// TOKENIZER UND PARSER IN EINER DATEI
//
// Bei HTML sind es zwei (der Tokenizer hat dort echte Zustaende, die man
// einzeln testen will). CSS ist flacher: Der Parser braucht vom Tokenizer
// nur „das naechste Zeichen, Kommentare und Leerraum uebersprungen". Ein
// eigener Token-Typ waere hier eine Schicht ohne Ertrag.

use crate::werte::kleinschreiben;
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// GRENZEN
// ---------------------------------------------------------------------------

/// Was ein Stylesheet hoechstens kosten darf.
///
/// Dieselbe Ueberlegung wie bei `speedhtml::Grenzen` und
/// `libspeed::bild::Grenzen`: Eine Zusage ueber Speicher braucht Zahlen.
/// Ein Wikipedia-Stylesheet hat rund 20 000 Regeln; die Vorgaben lassen
/// also Faktor fuenf Luft und treffen nur, was ohnehin kaputt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grenzen {
    pub max_regeln: usize,
    pub max_selektoren_je_regel: usize,
    pub max_deklarationen_je_regel: usize,
    /// Wie tief `@media` und Konsorten verschachtelt sein duerfen.
    pub max_block_tiefe: usize,
}

impl Grenzen {
    pub const fn standard() -> Grenzen {
        Grenzen {
            max_regeln: 100_000,
            max_selektoren_je_regel: 256,
            max_deklarationen_je_regel: 256,
            max_block_tiefe: 16,
        }
    }
}

impl Default for Grenzen {
    fn default() -> Self {
        Grenzen::standard()
    }
}

// ---------------------------------------------------------------------------
// SELEKTOREN
// ---------------------------------------------------------------------------

/// Ein einzelner Selektor-Bestandteil: `div.warn#haupt`.
///
/// Kein `Vec<Bestandteil>`, sondern feste Felder — die Teilmenge aus
/// docs/browser-v1.md kennt genau diese vier Sorten, und ein Vec waere
/// eine Allokation je Verbund fuer nichts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Verbund {
    /// Tag-Name, kleingeschrieben. `None` = `*` oder keiner.
    pub tag: Option<String>,
    /// `#id` — hoechstens einer ist sinnvoll, mehrere machen den Selektor
    /// unerfuellbar (das ist erlaubt und wird einfach nie passen).
    pub id: Option<String>,
    /// `.klasse` — Klassennamen sind gross-/kleinschreibungsempfindlich.
    pub klassen: Vec<String>,
    /// `:hover`, `:link`, … — siehe `Pseudo`.
    pub pseudo: Vec<Pseudo>,
}

/// Die Pseudoklassen der V1-Teilmenge.
///
/// **`:hover` ist VORBEREITET, nicht umgesetzt** (docs/browser-v1.md):
/// Der Selektor wird geparst und passt, wenn der Aufrufer den Zustand
/// meldet — der Browser tut das noch nicht. So kann die Kaskade
/// unveraendert bleiben, wenn Hover dazukommt.
///
/// Unbekannte Pseudoklassen machen den Selektor UNERFUELLBAR statt ihn zu
/// verwerfen. Der Unterschied ist wichtig: `a:not(.x) { color: red }` soll
/// NICHT alle `a` rot machen, nur weil wir `:not` nicht koennen — es soll
/// gar nichts machen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pseudo {
    Link,
    Besucht,
    Hover,
    /// Alles, was wir nicht kennen — passt NIE.
    Unbekannt(String),
}

/// Ein vollstaendiger Selektor: Verbuende, durch Nachkommen-Kombinatoren
/// verbunden. `div p span` sind drei Verbuende.
///
/// Der LETZTE ist der, auf den die Regel zutrifft; die davor sind
/// Bedingungen an die Vorfahren. Deshalb wird beim Vergleichen von hinten
/// nach vorn gegangen (siehe `kaskade::passt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selektor {
    pub verbuende: Vec<Verbund>,
    /// Der Text, wie er im Stylesheet stand — fuer `cssdump`.
    pub text: String,
}

/// Die Spezifitaet eines Selektors: (Ids, Klassen, Typen).
///
/// Die drei Zahlen werden NIE zu einer verrechnet (etwa `a*100 + b*10 +
/// c`). Das ist der klassische Fehler: Elf Klassen schlagen dann eine Id,
/// und das ist falsch — die Ordnung ist LEXIKOGRAFISCH. `Ord` auf einem
/// Tupel macht genau das richtige.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Spezifitaet {
    pub ids: u32,
    pub klassen: u32,
    pub typen: u32,
}

impl Selektor {
    /// Die Spezifitaet ausrechnen — reine Funktion, gut testbar.
    pub fn spezifitaet(&self) -> Spezifitaet {
        let mut s = Spezifitaet::default();
        for verbund in &self.verbuende {
            if verbund.id.is_some() {
                s.ids += 1;
            }
            s.klassen += verbund.klassen.len() as u32;
            // Pseudoklassen zaehlen wie Klassen (CSS-Spezifikation).
            s.klassen += verbund.pseudo.len() as u32;
            if verbund.tag.is_some() {
                s.typen += 1;
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// DEKLARATIONEN UND REGELN
// ---------------------------------------------------------------------------

/// `color: red !important`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deklaration {
    /// Eigenschaftsname, kleingeschrieben.
    pub name: String,
    /// Der Wert als roher Text (getrimmt). Gedeutet wird er erst in
    /// `stil.rs` — so bleibt der Parser frei von Eigenschaftswissen.
    pub wert: String,
    pub wichtig: bool,
}

/// Eine Regel: Selektoren + Deklarationen.
#[derive(Debug, Clone)]
pub struct Regel {
    pub selektoren: Vec<Selektor>,
    pub deklarationen: Vec<Deklaration>,
    /// Position im Stylesheet — bei gleicher Spezifitaet gewinnt die
    /// spaetere Regel. Ohne diese Zahl waere die Kaskade nicht
    /// deterministisch.
    pub reihenfolge: usize,
}

/// Was beim Parsen zurechtgebogen oder uebersprungen wurde.
///
/// Dieselbe Buchhaltung wie `speedhtml::Befund`, aus demselben Grund: Ohne
/// sie ist die Fehlertoleranz eine Blackbox. `cssdump` gibt die Zahlen aus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Befund {
    /// Regeln, deren Selektor unlesbar war.
    pub regeln_uebersprungen: u32,
    /// Deklarationen ohne `:` oder mit leerem Namen.
    pub deklarationen_uebersprungen: u32,
    /// `@media`, `@import`, … — uebersprungen, aber sauber.
    pub at_regeln_uebersprungen: u32,
    /// Selektoren mit Konstrukten, die wir nicht koennen (Attribut-
    /// Selektoren, `>`), die den Selektor unerfuellbar machen.
    pub selektoren_unerfuellbar: u32,
    /// Eine Grenze hat gegriffen.
    pub abgeschnitten: bool,
    pub regeln: usize,
}

impl Befund {
    pub fn sauber(&self) -> bool {
        self.regeln_uebersprungen == 0
            && self.deklarationen_uebersprungen == 0
            && self.selektoren_unerfuellbar == 0
            && !self.abgeschnitten
    }
}

/// Ein geparstes Stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub regeln: Vec<Regel>,
    pub befund: Befund,
}

impl Stylesheet {
    pub fn leer() -> Stylesheet {
        Stylesheet::default()
    }
}

// ---------------------------------------------------------------------------
// DER PARSER
// ---------------------------------------------------------------------------

struct Leser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Leser<'a> {
    fn neu(text: &'a str) -> Leser<'a> {
        Leser { text, pos: 0 }
    }

    fn fertig(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn schauen(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn vor(&mut self) {
        if let Some(c) = self.schauen() {
            self.pos += c.len_utf8();
        }
    }

    /// Leerraum UND Kommentare ueberspringen.
    ///
    /// Beides zusammen, weil ein Kommentar ueberall stehen darf, wo
    /// Leerraum darf — auch mitten in einem Selektor (`div/**/p`). Wer
    /// Kommentare nur am Regelanfang entfernt, stolpert genau darueber.
    fn leerraum(&mut self) {
        loop {
            let vorher = self.pos;
            while let Some(c) = self.schauen() {
                if c.is_whitespace() {
                    self.vor();
                } else {
                    break;
                }
            }
            if self.text[self.pos..].starts_with("/*") {
                match self.text[self.pos + 2..].find("*/") {
                    Some(i) => self.pos += 2 + i + 2,
                    // Nie geschlossener Kommentar: der Rest ist Kommentar.
                    None => self.pos = self.text.len(),
                }
            }
            if self.pos == vorher {
                return;
            }
        }
    }

    /// Einen Block `{ ... }` BALANCIERT ueberspringen.
    ///
    /// DIE WICHTIGSTE FUNKTION DIESER DATEI. Sie zaehlt geschweifte
    /// Klammern mit und beachtet dabei Zeichenketten — sonst beendet ein
    /// `content: "}"` den Block zu frueh, und ab da wird alles Folgende
    /// falsch gedeutet.
    ///
    /// Erwartet, dass `pos` auf dem `{` steht oder davor.
    fn block_ueberspringen(&mut self) {
        // Bis zur oeffnenden Klammer.
        while let Some(c) = self.schauen() {
            if c == '{' {
                break;
            }
            // Eine At-Regel kann auch mit `;` enden (`@import ...;`).
            if c == ';' {
                self.vor();
                return;
            }
            self.vor();
        }
        if self.fertig() {
            return;
        }
        let mut tiefe = 0usize;
        while let Some(c) = self.schauen() {
            match c {
                '{' => {
                    tiefe += 1;
                    self.vor();
                }
                '}' => {
                    tiefe -= 1;
                    self.vor();
                    if tiefe == 0 {
                        return;
                    }
                }
                '"' | '\'' => self.zeichenkette_ueberspringen(c),
                _ => self.vor(),
            }
        }
    }

    /// Eine Zeichenkette ueberspringen, `\`-Escapes beachtend.
    fn zeichenkette_ueberspringen(&mut self, anfuehrung: char) {
        self.vor(); // das oeffnende Zeichen
        while let Some(c) = self.schauen() {
            if c == '\\' {
                self.vor();
                self.vor();
                continue;
            }
            self.vor();
            if c == anfuehrung {
                return;
            }
        }
    }

    /// Alles bis zu einem der Zeichen in `bis` lesen (nicht verbrauchen),
    /// Zeichenketten und Klammern beachtend.
    fn bis_zu(&mut self, bis: &[char]) -> &'a str {
        let start = self.pos;
        let mut runde_klammern = 0usize;
        while let Some(c) = self.schauen() {
            match c {
                '"' | '\'' => {
                    self.zeichenkette_ueberspringen(c);
                    continue;
                }
                '(' => runde_klammern += 1,
                ')' => runde_klammern = runde_klammern.saturating_sub(1),
                _ => {}
            }
            // Innerhalb von `url(...)` oder `rgb(...)` gelten die
            // Trennzeichen nicht.
            if runde_klammern == 0 && bis.contains(&c) {
                break;
            }
            self.vor();
        }
        &self.text[start..self.pos]
    }
}

/// Ein Stylesheet parsen, mit den Standard-Grenzen.
pub fn parsen(css: &str) -> Stylesheet {
    parsen_mit(css, Grenzen::standard())
}

/// Ein Stylesheet parsen.
///
/// **PANICKT NIE, GIBT NIE AUF.** Wie `speedhtml::parsen` gibt es kein
/// `Result`: Jede Eingabe ergibt ein Stylesheet, notfalls ein leeres. Was
/// uebersprungen wurde, steht im `Befund`.
pub fn parsen_mit(css: &str, grenzen: Grenzen) -> Stylesheet {
    let mut blatt = Stylesheet::leer();
    let mut leser = Leser::neu(css);
    let mut naechste_nummer = 0usize;

    loop {
        leser.leerraum();
        if leser.fertig() {
            break;
        }
        if blatt.regeln.len() >= grenzen.max_regeln {
            blatt.befund.abgeschnitten = true;
            break;
        }

        // --- At-Regeln ---
        if leser.schauen() == Some('@') {
            at_regel(&mut leser, &mut blatt, &mut naechste_nummer, grenzen);
            continue;
        }

        // --- Ein verirrtes `}` (ein Block zu viel geschlossen) ---
        if leser.schauen() == Some('}') {
            leser.vor();
            blatt.befund.regeln_uebersprungen += 1;
            continue;
        }

        match qualifizierte_regel(&mut leser, &mut blatt.befund, grenzen, naechste_nummer) {
            Some(regel) => {
                naechste_nummer += 1;
                blatt.regeln.push(regel);
            }
            None => {
                // `qualifizierte_regel` hat schon aufgeraeumt; wenn sie
                // nichts geliefert hat und die Position steht, sind wir am
                // Ende.
                if leser.fertig() {
                    break;
                }
            }
        }
    }

    blatt.befund.regeln = blatt.regeln.len();
    blatt
}

/// `@media ... { ... }`, `@import ...;`, `@font-face { ... }`
///
/// ===================================================================
/// WARUM `@media` UEBERSPRUNGEN UND NICHT AUSGEWERTET WIRD
///
/// Eine Media-Query auszuwerten hiesse, `min-width`, `max-width`,
/// `prefers-color-scheme`, `print` und die Verknuepfungen `and`/`or`/`not`
/// zu verstehen — und dann bei jeder Fenster-Groessenaenderung die ganze
/// Kaskade neu zu rechnen. Das ist ein eigener Schritt.
///
/// **Wichtig ist nur, dass die Regeln DAHINTER sauber verschwinden.** Wer
/// `@media` ignoriert, indem er nur die Zeile ueberspringt, laesst den
/// Block offen — und die Regeln darin werden dann als Regeln auf oberster
/// Ebene gelesen. Das ist schlimmer als sie wegzulassen: Eine
/// Druck-Formatierung oder ein Handy-Layout schlagen dann auf den Desktop
/// durch.
///
/// FOLGE, ehrlich benannt: Auf Seiten, die ihr gesamtes Layout in
/// `@media`-Bloecken haben (das ist bei „mobile first" die Regel), sieht
/// V1 nur die Grundregeln.
fn at_regel(leser: &mut Leser, blatt: &mut Stylesheet, _nummer: &mut usize, _grenzen: Grenzen) {
    blatt.befund.at_regeln_uebersprungen += 1;
    // `block_ueberspringen` behandelt beide Formen: mit Block und mit `;`.
    leser.block_ueberspringen();
}

/// `selektor, selektor { deklarationen }`
fn qualifizierte_regel(
    leser: &mut Leser,
    befund: &mut Befund,
    grenzen: Grenzen,
    nummer: usize,
) -> Option<Regel> {
    let selektor_text = leser.bis_zu(&['{', '}']);

    // Kein `{` mehr — abgeschnittenes Stylesheet.
    if leser.schauen() != Some('{') {
        if !selektor_text.trim().is_empty() {
            befund.regeln_uebersprungen += 1;
        }
        // Bis ans Ende, damit die Schleife terminiert.
        leser.pos = leser.text.len();
        return None;
    }

    let selektoren = selektoren_parsen(selektor_text, befund, grenzen);

    // Den Block lesen — auch wenn die Selektoren unbrauchbar waren, denn
    // er muss in jedem Fall uebersprungen werden.
    leser.vor(); // das `{`
    let block_start = leser.pos;
    let mut leser_kopie = Leser {
        text: leser.text,
        pos: block_start - 1,
    };
    leser_kopie.block_ueberspringen();
    let block_ende = leser_kopie.pos;
    // Der Inhalt ohne die schliessende Klammer.
    let inhalt_ende = block_ende.saturating_sub(1).max(block_start);
    let inhalt = &leser.text[block_start..inhalt_ende];
    leser.pos = block_ende;

    if selektoren.is_empty() {
        befund.regeln_uebersprungen += 1;
        return None;
    }

    let deklarationen = deklarationen_parsen(inhalt, befund, grenzen);
    Some(Regel {
        selektoren,
        deklarationen,
        reihenfolge: nummer,
    })
}

/// Kommentare aus einem Stueck CSS entfernen.
///
/// ===================================================================
/// WARUM ES DIESE FUNKTION ZUSAETZLICH ZU `Leser::leerraum` GIBT
///
/// `leerraum` ueberspringt Kommentare ZWISCHEN den Regeln. CSS erlaubt
/// sie aber ueberall, wo Leerraum erlaubt ist — auch MITTEN in einem
/// Selektor (`div/**/p`) und mitten in einem Wert (`color: /*x*/ red`).
/// Wer die stehen laesst, lehnt gueltige Regeln ab und bekommt Werte, die
/// sich nicht deuten lassen.
///
/// EIN KOMMENTAR WIRD ZU EINEM LEERZEICHEN, nicht zu nichts: `a/**/b`
/// sind ZWEI Verbuende, nicht einer.
///
/// ZEICHENKETTEN WERDEN GESCHONT. `content: "/*"` ist kein Kommentar-
/// anfang, und `url(/*.png)` auch nicht — wer hier stumpf sucht,
/// verschluckt den Rest der Regel.
fn kommentare_entfernen(text: &str) -> String {
    if !text.contains("/*") {
        return String::from(text);
    }
    let mut aus = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let anfuehrung = bytes[i];
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != anfuehrung {
                    // Ein Rueckstrich schuetzt das naechste Zeichen.
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                aus.push_str(&text[start..i]);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                aus.push(' ');
                match text[i + 2..].find("*/") {
                    Some(j) => i = i + 2 + j + 2,
                    // Nie geschlossen: der Rest ist Kommentar.
                    None => return aus,
                }
            }
            _ => {
                // ZEICHENWEISE, nicht byteweise — sonst schneidet ein
                // Umlaut im Klassennamen mitten durch.
                let c = text[i..].chars().next().unwrap_or(' ');
                aus.push(c);
                i += c.len_utf8();
            }
        }
    }
    aus
}

/// `h1, h2 > x, .warn p` — die Liste zerlegen.
///
/// Ein unbrauchbarer Selektor in der Liste macht NUR IHN unbrauchbar,
/// nicht die ganze Regel. (Die CSS-Spezifikation sieht es strenger vor —
/// dort faellt die ganze Regel. Wir sind hier absichtlich milder: Bei
/// `h1, h2:not(.x) { color: red }` ist es nuetzlicher, wenigstens `h1`
/// einzufaerben. Der Preis ist, dass eine Regel wirkt, die ein strenger
/// Browser verwerfen wuerde.)
pub fn selektoren_parsen(text: &str, befund: &mut Befund, grenzen: Grenzen) -> Vec<Selektor> {
    let mut aus = Vec::new();
    let ohne_kommentare = kommentare_entfernen(text);
    for teil in ohne_kommentare.split(',') {
        if aus.len() >= grenzen.max_selektoren_je_regel {
            befund.abgeschnitten = true;
            break;
        }
        let getrimmt = teil.trim();
        if getrimmt.is_empty() {
            continue;
        }
        match selektor_parsen(getrimmt) {
            Some(selektor) => aus.push(selektor),
            None => befund.selektoren_unerfuellbar += 1,
        }
    }
    aus
}

/// EINEN Selektor zerlegen: `div.warn p#x`.
///
/// Liefert `None`, wenn der Selektor Konstrukte enthaelt, die wir nicht
/// koennen — dann passt er GAR NICHT, statt zu breit zu passen.
///
/// DIE ENTSCHEIDUNG, die dahintersteht: `div > p { display: none }` ist
/// haeufig, und ein Kind-Kombinator, den man als Nachkommen deutet, macht
/// aus „nur direkte Kinder" ein „alle Nachkommen" — sichtbar mehr, als
/// gemeint war. Etwas zu verstecken, das sichtbar bleiben sollte, ist der
/// schlimmere Fehler; deshalb: nicht raten, sondern nicht anwenden.
pub fn selektor_parsen(text: &str) -> Option<Selektor> {
    let mut verbuende = Vec::new();
    let mut aktuell = Verbund::default();
    let mut hat_etwas = false;

    let mut zeichen = text.char_indices().peekable();
    while let Some((i, c)) = zeichen.next() {
        match c {
            // --- Kombinatoren, die wir NICHT koennen ---
            '>' | '+' | '~' => return None,
            // --- Attribut-Selektoren: nicht in V1 ---
            '[' => return None,
            // --- Nachkommen-Kombinator ---
            c if c.is_whitespace() => {
                if hat_etwas {
                    verbuende.push(core::mem::take(&mut aktuell));
                    hat_etwas = false;
                }
            }
            '.' => {
                let name = wort_lesen(text, i + 1);
                if name.is_empty() {
                    return None;
                }
                for _ in 0..name.chars().count() {
                    zeichen.next();
                }
                aktuell.klassen.push(String::from(name));
                hat_etwas = true;
            }
            '#' => {
                let name = wort_lesen(text, i + 1);
                if name.is_empty() {
                    return None;
                }
                for _ in 0..name.chars().count() {
                    zeichen.next();
                }
                aktuell.id = Some(String::from(name));
                hat_etwas = true;
            }
            ':' => {
                // `::before` ist ein Pseudo-ELEMENT und etwas anderes als
                // eine Pseudo-KLASSE: Es erzeugt Inhalt, den es im
                // Dokument nicht gibt. Das koennen wir nicht, und es als
                // Pseudoklasse zu deuten waere falsch — also ist der
                // ganze Selektor unerfuellbar (wie bei `:not(...)`).
                let ab = i + 1;
                if text[ab..].starts_with(':') {
                    return None;
                }
                let name = wort_lesen(text, ab);
                if name.is_empty() {
                    return None;
                }
                for _ in 0..name.chars().count() {
                    zeichen.next();
                }
                // Eine funktionale Pseudoklasse (`:not(...)`) hat noch
                // Klammern — die machen den Selektor unerfuellbar.
                if text[ab + name.len()..].starts_with('(') {
                    return None;
                }
                aktuell.pseudo.push(match kleinschreiben(name).as_str() {
                    "link" => Pseudo::Link,
                    "visited" => Pseudo::Besucht,
                    "hover" => Pseudo::Hover,
                    anderes => Pseudo::Unbekannt(String::from(anderes)),
                });
                hat_etwas = true;
            }
            '*' => {
                // Der Universalselektor setzt keinen Tag-Namen (und zaehlt
                // laut Spezifikation nicht zur Spezifitaet).
                hat_etwas = true;
            }
            c if c.is_alphanumeric() || c == '-' || c == '_' => {
                let name = wort_lesen(text, i);
                for _ in 1..name.chars().count() {
                    zeichen.next();
                }
                aktuell.tag = Some(kleinschreiben(name));
                hat_etwas = true;
            }
            // Alles andere ist etwas, das wir nicht kennen.
            _ => return None,
        }
    }
    if hat_etwas {
        verbuende.push(aktuell);
    }
    if verbuende.is_empty() {
        return None;
    }
    Some(Selektor {
        verbuende,
        text: String::from(text),
    })
}

/// Ein Bezeichnerwort ab `ab` lesen (Buchstaben, Ziffern, `-`, `_`).
fn wort_lesen(text: &str, ab: usize) -> &str {
    let rest = &text[ab..];
    let ende = rest
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '-' || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    &rest[..ende]
}

/// `color: red; margin: 0 !important;` — den Blockinhalt zerlegen.
pub fn deklarationen_parsen(
    inhalt: &str,
    befund: &mut Befund,
    grenzen: Grenzen,
) -> Vec<Deklaration> {
    let mut aus: Vec<Deklaration> = Vec::new();
    // ZUERST die Kommentare weg, DANN zerlegen: Ein Kommentar darf ein
    // `;` oder `:` enthalten, und dann trennt er an der falschen Stelle.
    let ohne_kommentare = kommentare_entfernen(inhalt);
    let mut leser = Leser::neu(&ohne_kommentare);

    loop {
        leser.leerraum();
        if leser.fertig() {
            break;
        }
        if aus.len() >= grenzen.max_deklarationen_je_regel {
            befund.abgeschnitten = true;
            break;
        }

        let stueck = leser.bis_zu(&[';']);
        if !leser.fertig() {
            leser.vor(); // das `;`
        }
        let stueck = stueck.trim();
        if stueck.is_empty() {
            continue;
        }

        // Ohne `:` ist es keine Deklaration.
        let Some((name, wert)) = stueck.split_once(':') else {
            befund.deklarationen_uebersprungen += 1;
            continue;
        };
        let name = kleinschreiben(name.trim());
        let mut wert = wert.trim();
        if name.is_empty() || wert.is_empty() {
            befund.deklarationen_uebersprungen += 1;
            continue;
        }

        // `!important` abschneiden — case-insensitiv, Leerraum egal.
        let mut wichtig = false;
        if let Some(rumpf) = wert.rfind('!').map(|i| (&wert[..i], &wert[i + 1..])) {
            if kleinschreiben(rumpf.1.trim()) == "important" {
                wichtig = true;
                wert = rumpf.0.trim();
            }
        }
        if wert.is_empty() {
            befund.deklarationen_uebersprungen += 1;
            continue;
        }

        // DOPPELTE EIGENSCHAFTEN IN EINEM BLOCK: die SPAETERE gewinnt
        // (anders als bei HTML-Attributen, wo die erste gewinnt). So
        // schreibt es die CSS-Spezifikation, und darauf beruht das
        // uebliche Muster „erst ein Rueckfallwert, dann der richtige".
        if let Some(vorhanden) = aus.iter_mut().find(|d| d.name == name) {
            // Ein `!important` laesst sich nicht von einem gewoehnlichen
            // Wert im selben Block ueberschreiben.
            if vorhanden.wichtig && !wichtig {
                continue;
            }
            vorhanden.wert = String::from(wert);
            vorhanden.wichtig = wichtig;
            continue;
        }
        aus.push(Deklaration {
            name,
            wert: String::from(wert),
            wichtig,
        });
    }
    aus
}
