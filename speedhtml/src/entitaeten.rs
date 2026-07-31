// speedhtml::entitaeten — Zeichenreferenzen aufloesen
//
// ===========================================================================
// DREI SORTEN, UND EINE REGEL FUER DEN REST
//
//   &amp;      benannt      -> aus einer Tabelle
//   &#123;     dezimal      -> Codepunkt
//   &#x7B;     hexadezimal  -> Codepunkt
//
// **UNBEKANNTES WIRD DURCHGELASSEN, NICHT VERSCHLUCKT.** `&foo;` bleibt
// `&foo;`, und ein einzelnes `&` bleibt ein `&`. Das ist keine Faulheit,
// sondern das Verhalten, das jeder Browser zeigt — und das einzige, das
// keinen Text verliert. Ein Parser, der Unbekanntes wegwirft, macht aus
// „Tom & Jerry" ein „Tom Jerry" und aus einer Preisangabe Unsinn.
//
// ===========================================================================
// DIE TABELLE IST BEWUSST KLEIN
//
// Die HTML5-Spezifikation kennt **2231** benannte Referenzen. Die
// vollstaendige Tabelle waere ~60 KiB Daten fuer einen Ertrag, der nach den
// ersten hundert Eintraegen gegen null geht: Was auf echten Seiten
// vorkommt, sind Umlaute, Anfuehrungszeichen, Gedankenstriche, `&nbsp;` und
// eine Handvoll Symbole.
//
// Hier stehen ~120 Eintraege — die gaengigen plus der komplette
// Latin-1-Block, weil deutsche Seiten voll davon sind. Alles andere geht
// unveraendert durch und ist damit im schlimmsten Fall SICHTBAR falsch
// statt unsichtbar falsch. Wer eine vermisst, traegt sie hier ein; die
// Liste ist sortiert und `bekannt()` macht daraus eine binaere Suche.

use alloc::string::String;

/// Die benannten Referenzen, die wir kennen — **aufsteigend sortiert**.
///
/// Die Sortierung ist keine Kosmetik: `aufloesen_benannt` sucht binaer.
/// Ein neuer Eintrag an der falschen Stelle wird nicht gefunden — deshalb
/// prueft `test_tabelle_ist_sortiert` die Ordnung.
static BENANNT: &[(&str, char)] = &[
    ("AElig", 'Æ'),
    ("Aacute", 'Á'),
    ("Acirc", 'Â'),
    ("Agrave", 'À'),
    ("Aring", 'Å'),
    ("Atilde", 'Ã'),
    ("Auml", 'Ä'),
    ("Ccedil", 'Ç'),
    ("ETH", 'Ð'),
    ("Eacute", 'É'),
    ("Ecirc", 'Ê'),
    ("Egrave", 'È'),
    ("Euml", 'Ë'),
    ("Iacute", 'Í'),
    ("Icirc", 'Î'),
    ("Igrave", 'Ì'),
    ("Iuml", 'Ï'),
    ("Ntilde", 'Ñ'),
    ("Oacute", 'Ó'),
    ("Ocirc", 'Ô'),
    ("Ograve", 'Ò'),
    ("Oslash", 'Ø'),
    ("Otilde", 'Õ'),
    ("Ouml", 'Ö'),
    ("THORN", 'Þ'),
    ("Uacute", 'Ú'),
    ("Ucirc", 'Û'),
    ("Ugrave", 'Ù'),
    ("Uuml", 'Ü'),
    ("Yacute", 'Ý'),
    ("aacute", 'á'),
    ("acirc", 'â'),
    ("acute", '´'),
    ("aelig", 'æ'),
    ("agrave", 'à'),
    ("amp", '&'),
    ("apos", '\''),
    ("aring", 'å'),
    ("atilde", 'ã'),
    ("auml", 'ä'),
    ("bdquo", '„'),
    ("brvbar", '¦'),
    ("bull", '•'),
    ("ccedil", 'ç'),
    ("cedil", '¸'),
    ("cent", '¢'),
    ("copy", '©'),
    ("curren", '¤'),
    ("dagger", '†'),
    ("darr", '↓'),
    ("deg", '°'),
    ("divide", '÷'),
    ("eacute", 'é'),
    ("ecirc", 'ê'),
    ("egrave", 'è'),
    ("emsp", '\u{2003}'),
    ("ensp", '\u{2002}'),
    ("euml", 'ë'),
    ("euro", '€'),
    ("frac12", '½'),
    ("frac14", '¼'),
    ("frac34", '¾'),
    ("ge", '≥'),
    ("gt", '>'),
    ("harr", '↔'),
    ("hellip", '…'),
    ("iacute", 'í'),
    ("icirc", 'î'),
    ("iexcl", '¡'),
    ("igrave", 'ì'),
    ("infin", '∞'),
    ("iquest", '¿'),
    ("iuml", 'ï'),
    ("laquo", '«'),
    ("larr", '←'),
    ("ldquo", '“'),
    ("le", '≤'),
    ("lsaquo", '‹'),
    ("lsquo", '‘'),
    ("lt", '<'),
    ("macr", '¯'),
    ("mdash", '—'),
    ("micro", 'µ'),
    ("middot", '·'),
    ("minus", '−'),
    ("nbsp", '\u{00A0}'),
    ("ndash", '–'),
    ("ne", '≠'),
    ("not", '¬'),
    ("ntilde", 'ñ'),
    ("oacute", 'ó'),
    ("ocirc", 'ô'),
    ("ograve", 'ò'),
    ("ordf", 'ª'),
    ("ordm", 'º'),
    ("oslash", 'ø'),
    ("otilde", 'õ'),
    ("ouml", 'ö'),
    ("para", '¶'),
    ("permil", '‰'),
    ("plusmn", '±'),
    ("pound", '£'),
    ("quot", '"'),
    ("raquo", '»'),
    ("rarr", '→'),
    ("rdquo", '”'),
    ("reg", '®'),
    ("rsaquo", '›'),
    ("rsquo", '’'),
    ("sbquo", '‚'),
    ("sect", '§'),
    ("shy", '\u{00AD}'),
    ("sup1", '¹'),
    ("sup2", '²'),
    ("sup3", '³'),
    ("szlig", 'ß'),
    ("thinsp", '\u{2009}'),
    ("thorn", 'þ'),
    ("tilde", '˜'),
    ("times", '×'),
    ("trade", '™'),
    ("uacute", 'ú'),
    ("uarr", '↑'),
    ("ucirc", 'û'),
    ("ugrave", 'ù'),
    ("uml", '¨'),
    ("uuml", 'ü'),
    ("yacute", 'ý'),
    ("yen", '¥'),
    ("yuml", 'ÿ'),
];

/// Die laengste Referenz in der Tabelle (`frac12`, `permil`, … = 6) plus
/// Reserve. Begrenzt, wie weit hinter einem `&` ueberhaupt gesucht wird —
/// ohne diese Grenze wuerde ein `&` gefolgt von 20 MB Buchstaben die ganze
/// Datei absuchen, und zwar bei JEDEM `&`.
const MAX_NAME: usize = 8;

/// So viele Ziffern einer numerischen Referenz werden mitgerechnet.
/// Der groesste gueltige Codepunkt (0x10FFFF) hat sechs Hexziffern und
/// sieben Dezimalziffern — alles darueber ist ohnehin ungueltig.
const MAX_ZIFFERN: usize = 8;

/// DIE WINDOWS-1252-AUSNAHME, die die HTML5-Spezifikation ausdruecklich
/// vorschreibt (§13.2.5.80).
///
/// Codepunkte 0x80..0x9F sind in Unicode Steuerzeichen und in
/// Windows-1252 die typografischen Zeichen. Unzaehlige Seiten schreiben
/// `&#151;` und meinen einen Gedankenstrich — weil ihr Redaktionssystem
/// Windows-1252 fuer Unicode hielt. Wer das wortwoertlich nimmt, setzt ein
/// unsichtbares Steuerzeichen und der Text hat ein Loch.
///
/// Das ist kein Entgegenkommen an schlechte Seiten, sondern die Regel:
/// Jeder Browser tut es, und die Spezifikation schreibt es vor.
static WIN1252: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Was aus einem Codepunkt wird — mit allen Sonderfaellen der Spezifikation.
///
/// PANICKT NIE. `char::from_u32` liefert bei Surrogaten und Werten ueber
/// 0x10FFFF `None`; daraus wird das Ersatzzeichen U+FFFD, nicht ein
/// Absturz und nicht ein stilles Nichts.
fn codepunkt(wert: u32) -> char {
    const ERSATZ: char = '\u{FFFD}';
    match wert {
        // 0 waere ein Nullbyte mitten im Text.
        0 => ERSATZ,
        0x80..=0x9F => WIN1252[(wert - 0x80) as usize],
        // Surrogate sind in UTF-8 nicht darstellbar.
        0xD800..=0xDFFF => ERSATZ,
        _ => char::from_u32(wert).unwrap_or(ERSATZ),
    }
}

/// Eine benannte Referenz nachschlagen (binaere Suche).
fn benannt(name: &str) -> Option<char> {
    BENANNT
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| BENANNT[i].1)
}

/// Versucht, an `rest` (beginnt HINTER dem `&`) eine Referenz zu lesen.
///
/// Liefert `(zeichen, verbrauchte_bytes)` — oder `None`, wenn dort keine
/// gueltige Referenz steht. Der Aufrufer schreibt dann einfach das `&`.
///
/// ===================================================================
/// DER SEMIKOLON-FALL, den man leicht falsch macht
///
/// `&amp` ohne Semikolon ist im Fliesstext **gueltig** (die Spezifikation
/// erlaubt es fuer eine Liste von Alt-Referenzen), in einem
/// ATTRIBUTWERT aber nicht, wenn danach `=` oder ein Buchstabe folgt —
/// sonst wuerde aus `?x=1&amp=2` etwas anderes als gemeint.
///
/// Wir machen es einfacher und strenger: **Ohne Semikolon wird nur
/// aufgeloest, wenn danach kein Buchstabe, keine Ziffer und kein `=`
/// steht.** Das trifft den Fliesstext-Fall („Tom &amp Jerry") und laesst
/// Query-Strings in Ruhe. Der Unterschied zur Spezifikation ist eine
/// Handvoll historischer Namen; der Preis waere eine zweite Tabelle.
pub fn lesen(rest: &str) -> Option<(char, usize)> {
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    // --- Numerisch: &#123; oder &#x7B; ---
    if bytes[0] == b'#' {
        let (hex, ab) = match bytes.get(1) {
            Some(b'x') | Some(b'X') => (true, 2),
            _ => (false, 1),
        };
        let mut wert: u32 = 0;
        let mut ziffern = 0usize;
        let mut i = ab;
        while i < bytes.len() {
            let c = bytes[i];
            let d = if hex {
                match c {
                    b'0'..=b'9' => (c - b'0') as u32,
                    b'a'..=b'f' => (c - b'a' + 10) as u32,
                    b'A'..=b'F' => (c - b'A' + 10) as u32,
                    _ => break,
                }
            } else {
                match c {
                    b'0'..=b'9' => (c - b'0') as u32,
                    _ => break,
                }
            };
            // SAETTIGEN STATT UEBERLAUFEN: `&#99999999999999;` ist eine
            // Zahl, die in kein u32 passt. Ein `wrapping_mul` lieferte
            // dabei irgendein gueltiges Zeichen — hier wird daraus ein
            // Wert, den `codepunkt` sicher als Ersatzzeichen abweist.
            // NUR SO LANGE MITRECHNEN, WIE ES SINN HAT — aber IMMER
            // weiterlesen. Der Unterschied ist wichtig: Wer bei der achten
            // Ziffer aufhoert zu LESEN, laesst den Rest der Zahl und das
            // Semikolon als Text stehen (`&#99999999999999;` wuerde zu
            // „<Ersatzzeichen>999999;"). Wer weiterliest und nur das
            // Rechnen saettigt, bekommt EIN Ersatzzeichen — so wie jeder
            // Browser.
            if ziffern <= MAX_ZIFFERN {
                wert = wert.saturating_mul(if hex { 16 } else { 10 }).saturating_add(d);
            } else {
                // Ueber der Grenze steht ohnehin kein gueltiger Codepunkt
                // mehr; der Wert wird so festgenagelt, dass `codepunkt`
                // ihn sicher abweist.
                wert = u32::MAX;
            }
            ziffern += 1;
            i += 1;
        }
        if ziffern == 0 {
            return None; // `&#;` oder `&#x;` — kein Zeichen, nur Text
        }
        // Das Semikolon ist optional (viele Seiten lassen es weg).
        let verbraucht = if bytes.get(i) == Some(&b';') { i + 1 } else { i };
        return Some((codepunkt(wert), verbraucht));
    }

    // --- Benannt: &amp; ---
    let mut ende = 0usize;
    while ende < bytes.len() && ende < MAX_NAME && bytes[ende].is_ascii_alphanumeric() {
        ende += 1;
    }
    if ende == 0 {
        return None;
    }
    let name = &rest[..ende];

    if bytes.get(ende) == Some(&b';') {
        return benannt(name).map(|c| (c, ende + 1));
    }
    // Ohne Semikolon: nur, wenn danach nichts Namensartiges folgt.
    match bytes.get(ende) {
        Some(c) if c.is_ascii_alphanumeric() || *c == b'=' => None,
        _ => benannt(name).map(|c| (c, ende)),
    }
}

/// Loest alle Referenzen in `eingabe` auf.
///
/// Die bequeme Fassung fuer Attributwerte und rohe Textbereiche; der
/// Tokenizer selbst loest zeichenweise auf, waehrend er ohnehin laeuft.
pub fn aufloesen(eingabe: &str) -> String {
    // Kein `&` — dann ist die Kopie schon das Ergebnis. Der haeufigste
    // Fall, und er soll nicht zeichenweise durchlaufen werden.
    if !eingabe.contains('&') {
        return String::from(eingabe);
    }
    let mut aus = String::with_capacity(eingabe.len());
    let mut i = 0usize;
    let bytes = eingabe.as_bytes();
    while i < eingabe.len() {
        if bytes[i] == b'&' {
            if let Some((zeichen, verbraucht)) = lesen(&eingabe[i + 1..]) {
                aus.push(zeichen);
                i += 1 + verbraucht;
                continue;
            }
        }
        // Kein Treffer: dieses ZEICHEN (nicht Byte!) uebernehmen.
        let c = eingabe[i..].chars().next().unwrap_or('\u{FFFD}');
        aus.push(c);
        i += c.len_utf8();
    }
    aus
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die binaere Suche verlaesst sich auf die Sortierung. Ein Eintrag an
    /// der falschen Stelle wird nicht gefunden — und zwar STILL.
    #[test]
    fn test_tabelle_ist_sortiert() {
        for paar in BENANNT.windows(2) {
            assert!(
                paar[0].0 < paar[1].0,
                "'{}' steht vor '{}' — die Tabelle muss sortiert sein",
                paar[0].0,
                paar[1].0
            );
        }
        // Und keine Referenz ist laenger als MAX_NAME, sonst wird sie nie
        // gefunden.
        for (name, _) in BENANNT {
            assert!(name.len() <= MAX_NAME, "'{name}' ist laenger als MAX_NAME");
        }
    }

    #[test]
    fn test_die_fuenf_wichtigen() {
        assert_eq!(aufloesen("&amp;"), "&");
        assert_eq!(aufloesen("&lt;"), "<");
        assert_eq!(aufloesen("&gt;"), ">");
        assert_eq!(aufloesen("&quot;"), "\"");
        assert_eq!(aufloesen("&apos;"), "'");
    }

    #[test]
    fn test_numerisch() {
        assert_eq!(aufloesen("&#123;"), "{");
        assert_eq!(aufloesen("&#x7B;"), "{");
        assert_eq!(aufloesen("&#X7b;"), "{");
        assert_eq!(aufloesen("&#65;&#66;"), "AB");
        // Ohne Semikolon geht auch.
        assert_eq!(aufloesen("&#65 B"), "A B");
        // Mehrbyte-Zeichen.
        assert_eq!(aufloesen("&#8364;"), "€");
        assert_eq!(aufloesen("&#x20AC;"), "€");
    }

    /// DIE WINDOWS-1252-AUSNAHME: `&#151;` ist laut Unicode ein
    /// Steuerzeichen und laut jedem Browser ein Gedankenstrich.
    #[test]
    fn test_windows1252_ausnahme() {
        assert_eq!(aufloesen("&#151;"), "—");
        assert_eq!(aufloesen("&#147;Hallo&#148;"), "“Hallo”");
        assert_eq!(aufloesen("&#128;"), "€");
    }

    /// KEIN ABSTURZ und KEIN Unsinn bei unmoeglichen Codepunkten.
    #[test]
    fn test_unmoegliche_codepunkte_werden_zum_ersatzzeichen() {
        assert_eq!(aufloesen("&#0;"), "\u{FFFD}");
        assert_eq!(aufloesen("&#xD800;"), "\u{FFFD}"); // Surrogat
        assert_eq!(aufloesen("&#x110000;"), "\u{FFFD}"); // ueber dem Maximum
        // Eine Zahl, die kein u32 fassen kann — darf nicht ueberlaufen.
        assert_eq!(aufloesen("&#99999999999999999999;"), "\u{FFFD}");
        assert_eq!(aufloesen("&#xFFFFFFFFFFFF;"), "\u{FFFD}");
    }

    /// UNBEKANNTES WIRD DURCHGELASSEN — die wichtigste Zusage dieser Datei.
    #[test]
    fn test_unbekanntes_bleibt_stehen() {
        assert_eq!(aufloesen("&foo;"), "&foo;");
        assert_eq!(aufloesen("&;"), "&;");
        assert_eq!(aufloesen("&"), "&");
        assert_eq!(aufloesen("&#;"), "&#;");
        assert_eq!(aufloesen("&#x;"), "&#x;");
        assert_eq!(aufloesen("Tom & Jerry"), "Tom & Jerry");
        // Ein sehr langer Name laeuft in MAX_NAME und bleibt stehen.
        assert_eq!(aufloesen("&abcdefghijklmnop;"), "&abcdefghijklmnop;");
    }

    /// Der Query-String-Fall: `&amp=` darf NICHT aufgeloest werden, sonst
    /// zerlegt es Adressen.
    #[test]
    fn test_ohne_semikolon_nur_wenn_eindeutig() {
        assert_eq!(aufloesen("Tom &amp Jerry"), "Tom & Jerry");
        assert_eq!(aufloesen("?a=1&amp=2"), "?a=1&amp=2");
        assert_eq!(aufloesen("&ampere"), "&ampere");
    }

    #[test]
    fn test_deutsche_umlaute() {
        assert_eq!(aufloesen("Gr&uuml;&szlig;e"), "Grüße");
        assert_eq!(aufloesen("&Auml;&Ouml;&Uuml;&auml;&ouml;&uuml;&szlig;"), "ÄÖÜäöüß");
        assert_eq!(aufloesen("&nbsp;"), "\u{00A0}");
    }

    #[test]
    fn test_gemischter_text() {
        assert_eq!(
            aufloesen("&lt;p&gt; ist kein Absatz &amp; &#x2014; wirklich nicht"),
            "<p> ist kein Absatz & — wirklich nicht"
        );
    }

    /// Text ohne `&` wird unveraendert durchgereicht (der Schnellpfad).
    #[test]
    fn test_ohne_referenzen_unveraendert() {
        let t = "Ein ganz gewoehnlicher Satz mit Umlauten: äöü.";
        assert_eq!(aufloesen(t), t);
    }
}
