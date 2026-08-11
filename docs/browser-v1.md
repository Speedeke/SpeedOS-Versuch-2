# Browser V1 — der Zuschnitt

*Serie 8, Teil 4 — Juli 2026. Geschrieben VOR dem Code.*

Dieses Dokument legt fest, **was der Browser können soll und was
ausdrücklich nicht** — und woran gemessen wird, ob er gelungen ist. Es
steht vor der Implementierung, aus demselben Grund wie
`docs/tcp-scope.md` und `docs/scheduler-design.md`: Ein Umfang, den man
nachträglich festlegt, ist immer genau der, den man zufällig erreicht hat.

---

## 0. Die eine Zeile

**V1 macht Webseiten LESBAR und NAVIGIERBAR, nicht pixelgleich.** Was
diesem Ziel nicht dient, ist draußen — auch wenn es leicht wäre.

---

## 1. Warum überhaupt ein Browser

Nicht, weil die Welt einen weiteren braucht. Sondern weil ein Browser die
**ehrlichste Endabnahme** für alles ist, was in Serie 5 bis 8 gebaut
wurde: Er benutzt DNS, TCP, TLS, den HTTP-Parser, die Abrufschicht, den
Prozess-Isolationsmechanismus, den Fenster-Syscall, das Toolkit, den
Bilddekoder und die Schriftmetrik — **gleichzeitig, an einer echten
Adresse, gegen Server, die nichts von SpeedOS wissen.**

Wenn irgendeine dieser Schichten eine bequeme Annahme enthält, fällt sie
hier auf.

---

## 2. Was V1 KANN

### 2.1 Holen

* `https://` und `http://` über `libspeed::netz::Klient` — Weiterleitungen
  (auch über das Schema hinweg), Frist je Versuch, Größenlimit während
  des Lesens, **Zertifikate immer geprüft, ohne Umgehungsschalter**.
* Adresszeile, Verlauf zurück/vorwärts, Neu laden.
* Das **Schloss mit Substanz**: Protokollversion, Ciphersuite und die
  geprüfte Kette stehen schon in `Abruf::Verbindungsinfo`. Anders als bei
  den meisten Browsern lässt sich hier zeigen, *warum* vertraut wird.

### 2.2 HTML lesen — mit Fehlertoleranz als Grundannahme

**Kaputtes HTML ist die Regel, nicht die Ausnahme.** Der Parser folgt
deshalb der Struktur der HTML5-Zustandsmaschine — nicht, weil wir sie
vollständig umsetzen, sondern weil sie **genau dafür entworfen ist**: Sie
hat für jeden unmöglichen Zustand einen definierten Ausgang.

* Tags, Attribute (mit und ohne Anführungszeichen), Zeichendaten,
  Kommentare, DOCTYPE.
* Zeichenreferenzen: `&amp;` `&lt;` `&gt;` `&quot;` `&#123;` `&#x7B;` und
  die gängigen benannten. **Unbekannte werden DURCHGELASSEN**, nicht
  verschluckt — `&foo;` bleibt `&foo;`.
* Implizit geschlossene Tags (`<p>`, `<li>`, `<tr>`, `<td>`, …),
  unerwartete Endtags werden ignoriert, nie geschlossene Tags werden beim
  Dokumentende geschlossen.
* Void-Elemente (`<br>`, `<img>`, `<meta>`, `<hr>`, …) bekommen nie
  Kinder.
* `<script>`, `<style>`, `<title>`, `<textarea>` sind **rohe
  Textbereiche** — ihr Inhalt wird nicht als Markup gelesen. Wer das
  nicht tut, findet in jedem `if (a < b)` einen Tag-Anfang.

### 2.3 CSS in Spurweite — die vollständige Liste

*Gebaut in Serie 8, Teil 5 (`speedcss/`). Die Liste hier IST die Liste;
im Code ist sie die Feldliste von `speedcss::Stil` und die Tabelle in
`speedcss::stil::bekannt`.*

#### Unterstützte Eigenschaften

| Eigenschaft | Werte | erbt |
|---|---|:--:|
| `display` | `block`, `inline`, `inline-block`, `none`, `list-item`, `table`, `table-row-group`, `table-row`, `table-cell` | – |
| `color` | Farbe | **ja** |
| `background-color`, `background` | Farbe (aus `background` **nur** die Farbe) | – |
| `font-size` | Länge, `%`, `xx-small`…`xx-large`, `larger`/`smaller` | **ja** |
| `font-weight` | `normal`, `bold`, `bolder`, `lighter`, 100–900 (≥ 600 = fett) | **ja** |
| `font-style` | `normal`, `italic`, `oblique` | **ja** |
| `font-family` | Liste → auf `Proportional`/`Monospace` abgebildet | **ja** |
| `line-height` | `normal`, Zahl (Faktor), Länge, `%` | **ja** |
| `text-align` | `left`, `center`, `right`, `justify`, `start`, `end` | **ja** |
| `text-decoration`, `-line` | `none`, `underline`, `line-through`, `overline` | **ja** \* |
| `list-style-type`, `list-style` | `none`, `disc`, `circle`, `square`, `decimal`, `lower/upper-alpha`, `lower/upper-roman` | **ja** |
| `vertical-align` | `baseline`, `top`, `middle`, `bottom`, `sub`, `super` | – |
| `width`, `height`, `max-width` | Länge, `%`, `auto` | – |
| `margin` (+ `-top/-right/-bottom/-left`) | Länge, `%`, `auto`, 1–4 Werte | – |
| `padding` (+ vier Seiten) | wie `margin` | – |
| `border` (+ vier Seiten) | Kurzform aus Breite, Stil, Farbe — in beliebiger Reihenfolge | – |
| `border-width`, `-style`, `-color` | 1–4 Werte / `solid`+`none` / Farbe | – |

\* `text-decoration` erbt in CSS **nicht**, wird aber auf Nachfahren
mitgezeichnet. Wir behandeln sie als geerbt: dieselbe Optik mit weniger
Maschinerie. Der Unterschied ist nur sichtbar, wenn ein Nachfahre die
Dekoration selbst setzt.

`border-style` kennt nur **zwei** Zustände: `none` und durchgezogen.
`dashed`, `dotted`, `double` und der Rest werden zu durchgezogen — einen
gestrichelten Rahmen zu malen ist Renderer-Arbeit, ihn wegzulassen wäre
schlechter (der Kasten verschwände).

#### Einheiten

**Dabei:** `px`, `%`, `em`, `rem` (wie `em` — wir haben keine abweichende
Wurzelgröße), `pt` (× 4/3), die nackte `0`, und die Schlüsselwörter
`auto`, `inherit`, `initial`, `unset`.

**Nicht dabei, und alle werden ABGELEHNT statt geraten:** `vw`/`vh`
(brauchen die Fenstergröße schon zur Kaskadenzeit), `ex`/`ch` (brauchen
Schriftmetrik), `cm`/`mm`/`in` (brauchen eine echte Pixeldichte, die wir
nicht kennen), **`calc()`**, **Custom Properties** (`--x` / `var()`).

Eine abgelehnte Deklaration fällt weg; es gilt dann der geerbte oder der
Anfangswert. Das ist *sichtbar* falsch (etwas ist zu klein oder am
falschen Platz) statt unsichtbar falsch.

**Es gibt kein Fließkomma** (`-sse,+soft-float`): Alle Längen stehen in
**Tausendsteln** ihrer Einheit. `1.5em` ist 1500, `62.5%` ist 62500.

#### Was wann aufgelöst wird

Die Unterscheidung, an der eine CSS-Umsetzung steht oder fällt:

* **Kaskadenzeit** (`speedcss`): `font-size` wird zu einer festen
  Pixelzahl; `em` in allen anderen Eigenschaften wird gegen die *eigene*
  Schriftgröße aufgelöst; `line-height` in `%` oder `em` ebenfalls.
* **Layoutzeit** (Prompt 70): `%` und `auto` — erst dann steht fest, wie
  breit der umgebende Kasten ist.

Ein `width: 50%`, das schon die Kaskade zu einer Zahl macht, müsste die
Breite raten und rät falsch, sobald das Fenster seine Größe ändert.

#### Selektoren

**Dabei:** Typ (`p`), Klasse (`.warn`), ID (`#kopf`), Universal (`*`),
Nachfahren (`div p`), Gruppierung (`h1, h2`), und die Pseudoklassen
`:link`, `:visited`, `:hover`.

`:hover` ist **vorbereitet, nicht benutzt**: Die Kaskade wertet es aus,
wenn der Aufrufer den Zustand meldet (`Zustand::unter_maus`) — der
Browser tut das noch nicht. Wenn er es tut, ändert sich an `speedcss`
nichts.

**Nicht dabei:** Kombinatoren `>` `+` `~`, Attributselektoren
(`a[href]`), funktionale Pseudoklassen (`:not()`, `:nth-child()`) und
Pseudo-**Elemente** (`::before`).

**Sie machen den Selektor UNERFÜLLBAR, statt näherungsweise zu passen.**
Das ist die wichtigste Entscheidung dieses Abschnitts: `div > p { display:
none }` als Nachfahren zu deuten würde aus „nur direkte Kinder" ein „alle
Nachfahren" machen — und etwas verstecken, das sichtbar bleiben sollte.
Nicht raten ist hier sicherer als raten.

#### Herkunft und Kaskade

Von schwach nach stark:

1. Standard-Stylesheet (eingebaut, §2.3a)
2. Autor-Stylesheet, gewöhnlich
3. Autor-Stylesheet, `!important`
4. Standard-Stylesheet, `!important` — **bei `!important` dreht sich die
   Herkunfts-Reihenfolge um**

Innerhalb einer Stufe: höhere Spezifität, bei Gleichstand die spätere
Regel. Ein `style`-Attribut schlägt jeden Selektor.

**Spezifität ist lexikografisch** (IDs, Klassen, Typen) und wird **nie**
zu einer Zahl verrechnet — sonst schlagen elf Klassen eine ID, und das
ist falsch.

**Keine Kaskade über externe Stylesheets:** `<link rel=stylesheet>` wird
**nicht geholt**. Das würde aus dem Parsen eine Netz-Operation machen, mit
Frist, Fehlerfall und Größenlimit; `speedcss` kennt kein Netz. Die Folge
ist ehrlich zu benennen: Auf Seiten, die ihr gesamtes Aussehen aus
externen Dateien beziehen — heute die Regel —, wirkt nur das
Standard-Stylesheet. Der Browser kann die Dateien später selbst holen und
hereinreichen; die Kaskade ändert sich dadurch nicht.

**`@media` wird übersprungen** — aber *sauber*, mit balancierter
Klammerung. Wer nur die Zeile überspringt, lässt den Block offen, und die
Regeln darin werden zu Regeln auf oberster Ebene: Eine Druck- oder
Handy-Formatierung schlüge dann auf den Desktop durch. Das ist schlimmer,
als sie wegzulassen.

### 2.3a Das eingebaute Stylesheet

**Der Grund, warum HTML ohne CSS überhaupt aussieht.** Ein `<h1>` ist
nicht groß und fett, weil der Renderer `h1` kennt, sondern weil ein
Stylesheet es sagt — eines, das jeder Browser mitbringt und das man nie
zu Gesicht bekommt.

Bei uns steht es als **echter CSS-Text** in `speedcss::standard` (~90
Regeln, aus dem HTML-Standard, Anhang „Rendering"). Drei Gründe gegen
Rust-Strukturen:

1. Es ist der **Selbsttest des Parsers**: Läuft es durch, kann er
   Selektorlisten, Kurzformen, Einheiten und Pseudoklassen.
2. **Die Kaskade gilt auch für den Standard** — eine Autor-Regel
   `h1 { margin: 0 }` schlägt ihn mit derselben Maschinerie wie jede
   andere Kollision.
3. Man kann es **lesen** und mit dem vergleichen, was Browser tun.

Nachprüfbar ist die Trennung an einem Test: Ohne dieses Stylesheet ist
**alles** `display: inline`, nichts fett, alles gleich groß
(`test_ohne_standard_ist_alles_inline`).

### 2.4 Layout

* **Blockfluss**: `<p>`, `<div>`, `<h1>`–`<h6>`, `<ul>`/`<ol>`/`<li>`,
  `<pre>`, `<blockquote>`, `<hr>`, `<section>`/`<article>`/`<main>` u. ä.
* **Inline-Fluss** mit Zeilenumbruch: Text, `<b>`, `<i>`, `<em>`,
  `<strong>`, `<code>`, `<a>`, `<span>`, `<br>`.
* Der Umbruch läuft über `speedui::text::umbrechen`, also über die
  **echte Textmetrik** — nicht über gezählte Zeichen (Serie 8, Teil 3).
* Schriftgrößen über `speedui::text::Rolle`; die Grenzen dabei sind
  bekannt und dokumentiert (`docs/schrift-groessen.md`): h1..h4 sind
  unterscheidbar, `<small>`/`<h5>`/`<h6>` können nicht kleiner werden als
  Fließtext.

### 2.5 Bilder

* `<img src=…>` wird geholt und dargestellt — **PNG und JPEG**
  (`libspeed::bild`, Serie 8, Teil 3), Ausgabe RGBA, mit Alpha über den
  Seitenhintergrund gemischt.
* `width`/`height`-Attribute werden beachtet, sonst die Eigengröße.
* **Ein Bild, das nicht geht, ist kein Seitenfehler**: Es wird durch
  einen Rahmen mit `alt`-Text ersetzt. Ein zu großes Bild
  (`Grenzen::max_pixel`, eine Heap-Grenze) genauso.

### 2.6 Tabellen — „einfach" heißt hier etwas Bestimmtes

Die Bestandsaufnahme hatte Tabellen noch draußen („V1 rendert Zellen
untereinander"). Sie kommen herein, weil eine Wikipedia-Seite ohne sie
nicht lesbar ist — aber mit einem **scharf umrissenen** „einfach":

**Drin:** `<table>`, `<tr>`, `<td>`, `<th>`, `<thead>`/`<tbody>`/`<tfoot>`.

Fehlt ein `<tbody>` — der Normalfall in handgeschriebenem HTML —, wird
**keines erfunden**. Der Parser bildet ab, was im Dokument steht
(`table > tr`), und das Layout behandelt `table > tr` und
`table > tbody > tr` gleich. Ein synthetischer Knoten wäre bequem für das
Layout und würde `htmldump` zu einer Lüge machen: Man sähe etwas im Baum,
das die Seite nie gesagt hat, und bei der Fehlersuche ist genau das der
teuerste Fehler. (Browser tun es anders — sie synthetisieren, weil das DOM
für JavaScript sichtbar und in der Spezifikation festgeschrieben ist. Wir
haben kein JavaScript.)

Spaltenbreiten in **einem Durchgang**: Für jede Spalte die größte Zellen-Wunschbreite, dann
proportional auf die verfügbare Breite geklemmt. Zeilenhöhe = höchste
Zelle. Rahmen, wenn `border` gesetzt ist.

**Draußen und in dieser Reihenfolge zu haben:** `colspan`/`rowspan`
werden **gelesen und berücksichtigt, aber nicht optimiert** — eine Zelle
mit `colspan=3` bekommt die Summe der drei Spalten, mehr nicht.
`<caption>` wird als Absatz darüber gesetzt. **Verschachtelte Tabellen
werden ab Tiefe 3 als Block gerendert** statt als Tabelle (sonst wird aus
einer Layout-Tabelle ein Rechenproblem). Kein `table-layout: fixed`, kein
Spaltenausgleich über mehrere Durchgänge.

**Der ehrliche Satz dazu:** Layout-Tabellen aus den 2000ern werden damit
*erträglich*, nicht schön.

### 2.7 Formulare — sichtbar, nicht absendbar

`<input>`, `<textarea>`, `<select>`, `<button>` werden **dargestellt**:
Ein Textfeld sieht aus wie ein Textfeld, hat seinen `value`, und man kann
hineinklicken und tippen (es ist ein `speedui`-Widget).

**Abgesendet wird nichts.** Kein GET-mit-Query, kein POST, kein
`multipart`. Ein Klick auf „Absenden" zeigt einen Hinweis.

Warum diese Grenze und nicht „GET-Formulare gehen schon":

1. Ein abgesendetes Formular **schickt Daten des Benutzers an einen
   Fremden**. Das ist eine andere Art von Operation als ein GET auf eine
   Adresse, die der Benutzer selbst eingetippt hat.
2. Ohne JavaScript ist die halbe Formularwelt ohnehin kaputt
   (Validierung, dynamische Felder).
3. Es ist eine Grenze, die man **sieht**. „Manche Formulare gehen" ist
   eine, die man erst bemerkt, wenn eine wichtige nicht geht.

### 2.8 Bedienung

* Links klicken, mit der Tastatur durchsteppen.
* Scrollen mit Rad, Pfeilen, Bild auf/ab, Pos1/Ende.
* Fenster vergrößern → Neu-Layout.
* Abbrechen eines laufenden Abrufs.

---

## 3. Was V1 ausdrücklich NICHT kann

| nicht dabei | warum das eine ENTSCHEIDUNG ist und kein Rückstand |
|---|---|
| **JavaScript** | Eine JS-Engine ist ein eigenes Projekt in der Größenordnung des restlichen Betriebssystems. Kein Interpreter, kein `<script>` — der Inhalt fliegt mitsamt Tag raus (wie in `news`). **Das ist die folgenreichste Grenze von allen** und macht einen wachsenden Teil des Webs unbenutzbar. Sie wird nicht kleingeredet. |
| **Flexbox / Grid** | Setzt einen echten Constraint-Solver voraus. `display: flex` wird zu `block` — das ist das ehrlichere Scheitern: Der Inhalt steht untereinander statt zu verschwinden. |
| **`position: absolute/fixed`** | Bricht den Blockfluss auf. Beide werden **geparst und ignoriert**; das Element läuft im normalen Fluss mit. Ein fixierter Kopfbereich steht dann eben oben im Dokument statt am Bildschirmrand — sichtbar falsch, aber lesbar. **„Außer dem Nötigsten" ist bei uns: nichts**, denn ohne JavaScript gibt es keine Überlagerungen, die man aufklappen könnte. |
| **Floats** | Berüchtigt kompliziert für das, was sie leisten. `float` wird ignoriert; ein umflossenes Bild steht dann in einer eigenen Zeile. |
| **Animationen, Transitions, Transforms** | Kein Zeitbegriff im Layout. |
| **Video, Audio** | Kein Codec, kein Zeitgeber, kein Ton-Treiber. `<video>` zeigt sein `poster`, wenn eins da ist. |
| **Canvas, SVG, WebGL** | Canvas braucht JS. SVG ist eine **zweite** Auszeichnungssprache mit eigenem Layout-Modell — eigenes Vorhaben. |
| **Web-Fonts (`@font-face`)** | Wir haben keinen TrueType-Rasterizer (`docs/schrift-groessen.md`). Vier vorgerasterte Größen, eine Familie. |
| **iframes** | Ein zweiter Dokumentkontext im ersten — mit allem, was daran hängt (eigener Abruf, eigene Herkunft, eigene Größe). |
| **Cookies, LocalStorage, Sessions** | Zustand über Abrufe hinweg ist eine eigene Baustelle **und eine Datenschutzfrage**. Ohne Cookies bleibt man überall abgemeldet — das ist eine Einschränkung, keine Tugend. |
| **Formulare absenden** | Siehe 2.7. |
| **Mehrere Tabs** | Ein Fenster, eine Seite. Mehrere **Fenster** gehen — jedes ist ein eigener Prozess, mit eigenem Adressraum. Das ist mehr Isolation als die meisten Browser zwischen Tabs haben. |
| **Drucken, PDF, Downloads-Verwaltung** | `holes <url> <datei>` gibt es schon. |
| **Zoom der ganzen Seite** | Die UI-Skalierung (1.0/1.5/2.0) gibt es systemweit; ein zweiter Zoom-Begriff darüber wäre Verwirrung. |

---

## 4. Die Zielmarke — messbar, VORHER festgelegt

Die Zielmarke aus dem ursprünglichen Plan gilt weiter: **ein
Wikipedia-Artikel, ein Blog und eine Nachrichtenseite müssen lesbar und
navigierbar sein.** „Lesbar" ist als Wort zu weich, um daran etwas zu
messen — deshalb hier die Kriterien, an denen abgenommen wird.

### 4.1 Die drei Prüfseiten

| # | Seite | wofür sie steht |
|---|---|---|
| **A** | `info.cern.ch/hypertext/WWW/TheProject.html` | Die erste Webseite der Welt. Reines HTML, kein CSS. **Der ehrlichste Maßstab für einen Renderer, der bei Null anfängt** — hier gibt es keine Ausrede. |
| **B** | `de.wikipedia.org/wiki/Betriebssystem` | Viel Text, Listen, Links, **Tabellen**, Bilder, Infobox. Wird *nicht* schön. Muss **lesbar** sein. |
| **C** | ein Blog + eine Nachrichtenseite | Moderne Seiten mit CSS-Layout, Werbung, `<script>`-Blöcken. Hier zeigt sich, was das Fehlen von JS und Flexbox kostet. |

### 4.2 Die Kriterien (jedes einzeln prüfbar)

Für jede Prüfseite gilt:

1. **Kein Absturz, kein Hänger.** Der Prozess endet nie mit Code 101, und
   die Seite steht in **< 10 s** (Abruf + Parsen + Layout, gemessen).
2. **Der Haupttext ist vollständig da.** Stichprobe: Der erste und der
   letzte Absatz des Artikels sind im gerenderten Text auffindbar.
3. **Überschriften sind als solche erkennbar** — größer und/oder fett als
   der Fließtext daneben.
4. **Zeilen brechen im Fenster um.** Kein Text läuft rechts hinaus, kein
   waagerechtes Scrollen nötig.
5. **Links sind sichtbar** (Farbe) **und klickbar**, und ein Klick führt
   auf die richtige Adresse. Zurück kommt man auch.
6. **Bilder erscheinen** — oder ihr `alt`-Text in einem Rahmen. Kein
   leerer Fleck ohne Erklärung.
7. **Listen sind eingerückt** und haben Aufzählungszeichen bzw. Nummern.
8. **Tabellen stehen in Spalten**, nicht als Textwurst.
9. **Kein JavaScript-Quelltext im sichtbaren Text.** (Der Fehler, der
   `news` fast wertlos gemacht hätte.)
10. **Der Speicher kommt zurück:** Nach 10 Seitenwechseln ist die
    Frame-Bilanz innerhalb der bekannten P1-Schranke, und der Prozess
    läuft weiter.

### 4.3 Was NICHT geprüft wird

Kein Pixelvergleich mit Chrome. Kein „sieht gut aus". Keine Abnahme an
einer Seite, die vorher angepasst wurde.

### 4.4 Die Reißleine

**Wenn Prüfseite A (erste Webseite der Welt) die Kriterien 1–7 nicht
erfüllt, ist der Renderer nicht fertig** — dann wird nicht an Wikipedia
weitergebastelt, sondern an A. A hat kein CSS, keine Tabellen, keine
Bilder; wer sie nicht darstellen kann, hat ein Grundlagenproblem.

Für B und C gilt ein weicheres Maß: **Kriterien 1, 2, 4, 5 und 9 sind
hart** (kein Absturz, Text vollständig, Umbruch, Navigation, kein
JS-Quelltext). 3, 6, 7, 8 sind Bericht — sie werden gemessen und
notiert, aber eine Wikipedia-Infobox, die schief steht, ist kein
Grund, V1 zu verwerfen.

Wie bei der TCP-Reißleine gilt: **Die Kriterien werden nachträglich nicht
verschoben.**

---

## 5. Architektur — wo was liegt

Die Aufteilung folgt dem Muster, das sich zweimal bewährt hat
(`speedhttp`, `speedui`): **Was keinen Wirt braucht, bekommt keinen.**

```
speedhtml/            Tokenizer + DOM. Leerer [dependencies]-Block.
                      Kennt kein Netz, kein Fenster, keine Schrift, kein CSS.
                      Tests laufen auf dem HOST in Millisekunden.
speedcss/             Parser + Kaskade + Standard-Stylesheet. Hängt als
                      EINZIGES an speedhtml (CSS ohne Baum ist
                      bedeutungslos) — sonst nichts.
speedui/              Toolkit + Textmetrik + Umbruch (steht)
speedhttp/            HTTP (steht)
libspeed::netz        Abruf mit TLS (steht)
libspeed::bild        PNG/JPEG -> RGBA (steht)
speedlayout/          Box-Modell, Blockfluss, Zeilenbau, Anzeige-Befehle.
                      Hängt an speedhtml + speedcss, NICHT an speedui:
                      Es braucht Textmetrik, kein Toolkit — und die Tests
                      brauchen eine Attrappe mit fester Zeichenbreite.
                      Erzeugt KEINE Pixel, sondern eine Befehlsliste.
userland/browser      Das Programm: Zeichnen, Bedienung, Netz.
                      DAS EINZIGE, das alles zusammen kennt.
userland/htmldump     Werkzeug: Baum ausgeben (dieser Teil)
```

**Warum der Parser eine eigene Kiste ist und nicht einfach ein Modul im
Browser:** Aus demselben Grund wie bei `speedhttp` — er ist *pure*
Byte-Logik, und pure Byte-Logik gehört dorthin, wo man sie **ohne QEMU
testen** kann. Die Tokenizer-Tests dieses Teils laufen auf dem Host in
0,00 s; in einem Browser-Modul bräuchte jeder einzelne einen QEMU-Start.

Der zweite Grund ist die Trennschärfe bei der Fehlersuche: Wenn eine
Seite falsch aussieht, ist die erste Frage „Parser oder Layout?".
`htmldump` beantwortet sie in einer Sekunde — und ist genau deshalb Teil
dieses Schritts und nicht ein späteres Extra.

---

## 6. Die Reihenfolge

1. **Tokenizer + DOM + `htmldump`** (Serie 8, Teil 4 — steht)
2. **CSS-Teilmenge + Kaskade + `cssdump`** (Serie 8, Teil 5 — steht)
3. **Block- und Inline-Layout, Text mit Umbruch** (Serie 8, Teil 6 — steht)
4. **Bilder, Listen, Tabellen, Anzeige-Befehle** (Serie 8, Teil 6 — steht)
5. Fenster, Bedienung, Verlauf — jetzt ist es ein Browser
6. Abnahme gegen §4

Schritte 1 und 2 sind wirtsfrei und host-getestet. Ab 3 wird es ein
Ring-3-Programm.

---

## 7. Was dieser Schritt liefert (Teil 4)

* `speedhtml` — Tokenizer nach dem Vorbild der HTML5-Zustandsmaschine,
  Zeichenreferenzen, DOM-Aufbau mit Fehlererholung.
* `userland/htmldump <datei|url>` — der Baum, eingerückt.
* Tests gegen die fiesen Fälle: nie geschlossene Tags, verschachtelte
  `<p>`, Tabellen ohne `<tbody>`, Attribute ohne Anführungszeichen, `<`
  mitten im Text, abgeschnittene Dokumente, 20 MB Müll — **nichts darf
  panicken** — und gegen eine echte, heruntergeladene Seite.

Was er **nicht** liefert: irgendetwas Sichtbares. Nach diesem Teil kann
SpeedOS HTML *verstehen*, aber nicht *zeigen*. Das ist Absicht — die
Alternative wäre, Parser und Layout gleichzeitig zu debuggen.
