// speedcss::standard — DAS EINGEBAUTE STYLESHEET
//
// ===========================================================================
// DER TRICK, WARUM HTML OHNE CSS ÜBERHAUPT AUSSIEHT
//
// Ein `<h1>` ist nicht deshalb gross und fett, weil der Renderer `h1`
// kennen wuerde. Es ist gross und fett, weil ein STYLESHEET es so sagt —
// eines, das jeder Browser mitbringt und das man nie zu Gesicht bekommt.
//
// Das ist keine Kleinigkeit, sondern die architektonische Entscheidung
// dieses Teils: **Der Renderer enthaelt KEIN Wissen ueber HTML-Elemente.**
// Er kennt `display: block`, Schriftgroessen und Raender — was ein `<h1>`
// ist, steht hier drin und nirgends sonst.
//
// Nachpruefbar: Nimmt man dieses Stylesheet weg, sieht eine Seite aus wie
// unformatierter Fliesstext. Genau das prueft
// `tests::test_ohne_standard_ist_alles_inline`.
//
// ===========================================================================
// WARUM ALS CSS-TEXT UND NICHT ALS RUST-CODE
//
// Man koennte die Regeln als `Stil`-Strukturen hinschreiben und sich das
// Parsen sparen. Drei Gruende dagegen:
//
//   1. **Es ist der Selbsttest des Parsers.** Wenn das Standard-Stylesheet
//      durchlaeuft, kann der Parser Selektorlisten, Kurzformen, Einheiten
//      und Vererbung — bewiesen an ~90 Regeln, bei jedem Start.
//   2. **Die Kaskade gilt auch fuer den Standard.** Eine Autor-Regel
//      `h1 { margin: 0 }` muss die Standard-Regel schlagen, und zwar mit
//      derselben Maschinerie wie jede andere Kollision. Als Rust-Struktur
//      waere es ein zweiter Weg mit eigenen Fehlern.
//   3. Man kann es LESEN und mit dem vergleichen, was Browser tun.
//
// Die Kosten sind ein Parserlauf beim Start (gemessen: unter einer
// Millisekunde) — dafuer wird es einmal geparst und wiederverwendet.
//
// ===========================================================================
// WOHER DIE ZAHLEN STAMMEN
//
// Aus dem HTML-Standard, Anhang „Rendering" — dort steht das
// Standard-Stylesheet, auf das sich alle Browser geeinigt haben.
// Uebernommen ist, was unsere Teilmenge ausdruecken kann; die
// Abweichungen stehen unten als Kommentar an der jeweiligen Regel.

/// Das eingebaute Stylesheet.
///
/// `em`-Werte statt Pixel, wo es geht: So skaliert die Optik mit der
/// UI-Groesse mit (SpeedOS kann 1.0/1.5/2.0 — `theme.rs`), ohne dass hier
/// etwas gerechnet werden muss.
pub const STANDARD_CSS: &str = r#"
/* ---------------------------------------------------------------
   Der Rahmen
   --------------------------------------------------------------- */
html, body { display: block }
body {
    margin: 8px;
    color: #101010;
    background-color: #ffffff;
    line-height: 1.4;
}

/* Was NICHT erscheint. `display: none` ist hier wichtiger als es
   aussieht: Ohne diese Regel stuende der Inhalt von <head> — Titel,
   Skripte, Stile — als Text oben auf der Seite. */
head, title, meta, link, style, script, base, noscript { display: none }

/* ---------------------------------------------------------------
   Block-Elemente
   --------------------------------------------------------------- */
div, p, section, article, aside, header, footer, nav, main, figure,
figcaption, address, blockquote, form, fieldset, hgroup, dl, dd, dt,
details, summary, center, dir, menu { display: block }

p { margin-top: 1em; margin-bottom: 1em }

blockquote { margin: 1em 40px }

figure { margin: 1em 40px }

address { font-style: italic }

hr {
    display: block;
    margin-top: 0.5em;
    margin-bottom: 0.5em;
    border-top: 1px solid #808080;
}

/* ---------------------------------------------------------------
   Ueberschriften

   Die Groessen sind die des HTML-Standards. Was daraus WIRKLICH wird,
   entscheidet die Schrift: SpeedOS hat vier Rastergroessen, und
   unterhalb der Fliesstextgroesse gibt es nichts (docs/schrift-groessen.md).
   h5 und h6 bekommen ihre Groesse also nicht — sie bleiben durch das
   Fettgewicht unterscheidbar. Genau dafuer gibt es
   speedui::text::exakt_moeglich.
   --------------------------------------------------------------- */
h1, h2, h3, h4, h5, h6 { display: block; font-weight: bold }
h1 { font-size: 2em;    margin-top: 0.67em; margin-bottom: 0.67em }
h2 { font-size: 1.5em;  margin-top: 0.83em; margin-bottom: 0.83em }
h3 { font-size: 1.17em; margin-top: 1em;    margin-bottom: 1em }
h4 { font-size: 1em;    margin-top: 1.33em; margin-bottom: 1.33em }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em }

/* ---------------------------------------------------------------
   Listen
   --------------------------------------------------------------- */
ul, ol { display: block; margin-top: 1em; margin-bottom: 1em; padding-left: 40px }
li { display: list-item }
ul { list-style-type: disc }
ol { list-style-type: decimal }
/* Verschachtelte Listen bekommen keinen zusaetzlichen Aussenabstand —
   sonst waechst der Abstand mit jeder Ebene. */
ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0 }
/* Die zweite Ebene wechselt das Zeichen (wie in jedem Browser). */
ul ul { list-style-type: circle }
ul ul ul { list-style-type: square }

dl { margin-top: 1em; margin-bottom: 1em }
dd { margin-left: 40px }
dt { font-weight: bold }

/* ---------------------------------------------------------------
   Inline
   --------------------------------------------------------------- */
span, a, b, i, em, strong, code, kbd, samp, small, sub, sup, u, s,
strike, big, tt, abbr, cite, dfn, q, var, mark, time, label, br, img,
button, input, select, textarea { display: inline }

b, strong { font-weight: bold }
i, em, cite, dfn, var { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small { font-size: 0.83em }
big { font-size: 1.17em }
sub { vertical-align: sub;   font-size: 0.83em }
sup { vertical-align: super; font-size: 0.83em }
mark { background-color: #ffff00; color: #000000 }

/* Links. `:link` passt nur auf <a> MIT href — ein Anker ohne Ziel
   soll nicht blau sein. Das ist der einzige Ort, an dem V1 eine
   Pseudoklasse wirklich braucht. */
a:link    { color: #0000ee; text-decoration: underline }
a:visited { color: #551a8b; text-decoration: underline }

/* ---------------------------------------------------------------
   Monospace

   `white-space: pre` koennen wir nicht (es gehoert dem Layout, und
   das gibt es noch nicht) — <pre> bekommt hier nur Schrift und
   Abstaende. Der Zeilenumbruch von <pre> ist Sache des Renderers.
   --------------------------------------------------------------- */
pre { display: block; font-family: monospace; margin-top: 1em; margin-bottom: 1em }
code, kbd, samp, tt { font-family: monospace }

/* ---------------------------------------------------------------
   Tabellen

   Was hier steht, ist die OPTIK. Das Layout einer Tabelle (Spalten
   ausmessen, Zeilen ausrichten) ist ein eigener Algorithmus und
   kommt mit dem Renderer — docs/browser-v1.md 2.6.
   --------------------------------------------------------------- */
table { display: table; border-color: #808080 }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px }
th { font-weight: bold; text-align: center }
caption { display: block; text-align: center }

/* ---------------------------------------------------------------
   Formulare — dargestellt, nicht absendbar (docs/browser-v1.md 2.7)
   --------------------------------------------------------------- */
input, textarea, select, button {
    border: 1px solid #767676;
    padding: 1px;
    background-color: #ffffff;
    color: #000000;
}
button { background-color: #efefef; text-align: center; padding: 1px 6px }
fieldset { border: 1px solid #c0c0c0; margin: 0 2px; padding: 0.35em 0.75em }
legend { display: block; padding: 0 2px }
"#;
