# Serie 9: die große Weiche

*Bestandsaufnahme nach dem Serie-8-Abschluss — August 2026*

Serie 8 hat einen Browser gebaut. Der
[Realitäts-Bericht](browser-realitaet.md) sagt, was er kann: fünf von
zehn echten Seiten sind wirklich benutzbar, drei teilweise, zwei nicht.

> **Nachtrag, Serie 9, Teil 1:** Die unten empfohlene erste Maßnahme
> (externe Stylesheets) ist umgesetzt. Die
> [zweite Messung](browser-realitaet.md#zweite-messung-mit-externen-stylesheets)
> steht bei **7 lesbar / 2 teilweise / 1 unbrauchbar**. Die Bewertung
> der drei großen Wege darunter ändert das nicht — sie stützt sie: Was
> billig war, ist getan, und die Frage lautet jetzt unverändert
> JavaScript, Anwendungen oder Fundament.

Daraus folgt die Frage dieser Serie: **Wohin geht die Kraft?** Drei Wege
stehen offen, und sie schließen sich in den nächsten Monaten praktisch
aus. Dieses Dokument bewertet alle drei ehrlich und gibt am Ende eine
Empfehlung — **die Entscheidung gehört dem Projektbesitzer.**

---

## (a) JavaScript

### Was es wirklich kostet

Eine JS-Engine ist nicht ein Ding, sondern vier — und die Reihenfolge
der Kosten ist nicht die, die man erwartet:

| Teil | Aufwand | Anmerkung |
|---|---|---|
| **Lexer + Parser** (ES5-Teilmenge → AST) | 2–4 Wochen | Der *einfachste* Teil. ASI (automatic semicolon insertion) und Regex-vs-Division sind die Fallen. |
| **Interpreter** (Baum- oder Bytecode) | 4–8 Wochen | Prototypen, Closures, `this`, Scope-Ketten. Ein Baum-Interpreter reicht — JIT ist ausgeschlossen (W^X, und wir wollen kein ausführbares Datensegment). |
| **Speicherverwaltung** | 2–4 Wochen | JS braucht **GC mit Zyklen** (`Rc` genügt nicht: DOM-Knoten und Closures zeigen im Kreis). Das ist echte Arbeit, kein Detail. |
| **DOM-Bindings** | 4–8 Wochen | `document.getElementById`, `querySelector`, `createElement`, `innerHTML`, `addEventListener`, `style.*` … Jede Methode braucht Semantik *und* muss den Baum invalidieren. |
| **Event-Loop + Timer** | 1–2 Wochen | Haben wir strukturell schon (Executor, `warte_ms`) — hier ist der Unterbau billig. |
| **Web-APIs** | ∞ | `fetch`, `XMLHttpRequest`, `localStorage`, `history`, `IntersectionObserver`, `requestAnimationFrame`, `Promise`, `MutationObserver`, Web Components, … |

**Realistische Schätzung für eine ES5-Engine mit brauchbaren
DOM-Bindings: vier bis sechs Monate.** Das ist länger als Serie 5, 6
und 7 zusammen.

### Was sie bringt — der unbequeme Teil

Für github.com bräuchte es nicht „JavaScript", sondern: ES2020+, Module,
`fetch`, Promises, Web Components, `IntersectionObserver`, CSS-in-JS und
ein React-kompatibles Reconciling-Verhalten. **Eine ES5-Engine mit
DOM-Bindings zeigt github.com genauso wenig wie gar keine.**

Der Realitäts-Bericht sagt es deutlicher als jede Schätzung: **Von zehn
Seiten scheitert genau eine an fehlendem JavaScript.** Acht liefern
ihren Inhalt im HTML und sehen nur falsch aus, weil das **CSS** fehlt.

> Vier bis sechs Monate für die zweithäufigste Fehlerursache — und ein
> Tag für die häufigste.

### Eigenbau oder eine bestehende Engine?

Das Krypto-Argument aus Serie 7 gilt hier ausdrücklich **umgekehrt**:

* Bei TLS hieß es *nicht selbst bauen* — ein Protokollfehler ist still,
  und der Test wäre ein Angreifer, den wir nicht haben.
* Bei einer JS-Engine ist ein Fehler **laut**: Die Seite tut nicht, was
  sie soll. Und sie läuft seit Serie 6 dort, wo großer Fremdcode
  hingehört — **in Ring 3**, mit eigenem Adressraum und harter
  Heap-Grenze.

Damit wäre eine fremde Engine tragbar. Nur: es gibt keine passende.
`boa` und `rquickjs` brauchen `std` bzw. C; `quickjs` ist C und
bräuchte eine libc-Portierung. Realistisch bliebe **Eigenbau** — mit
allen oben genannten Monaten.

### Ehrliches Fazit zu (a)

Eine JS-Engine ist das **lehrreichste** Projekt der drei und das mit dem
**schlechtesten Verhältnis von Aufwand zu sichtbarem Gewinn**. Wer sie
baut, sollte das tun, weil er eine Sprache implementieren will — nicht,
weil er das Web erreichen will. Das Web erreicht man damit nämlich
nicht.

---

## (b) Native Anwendungen statt Browser-Perfektion

Der ursprüngliche Plan sah „Web-Apps light" vor: native Programme, die
HTTPS-APIs sprechen, statt Webseiten zu rendern.

**Was dafür schon steht** — und das ist erstaunlich viel:

* HTTPS mit Zertifikatsprüfung (Serie 7), aus Ring 3
* JSON? *Fehlt* — aber ein Parser dafür ist zwei Tage (dieselbe Sorte
  Arbeit wie `speedhtml`, nur viel kleiner)
* Fenster, Widgets, Bilder, Schriften (Serie 8)
* Prozesse, Pipes, Dateisystem, Einstellungen

**Wetter, News, ein Reader, ein Fahrplan, ein Mail-Client (IMAP), ein
Chat (IRC/XMPP): jedes davon ist ein bis zwei Wochen** und liefert eine
Anwendung, die *vollständig* funktioniert statt einer Seite, die zu 80 %
aussieht.

### YouTube — die ehrliche Rechnung

Der genannte Fall zeigt die Grenze am schärfsten. Es fehlt nicht ein
Stück, sondern eine Kette:

| Nötig | Stand | Aufwand |
|---|---|---|
| HTTPS-API + JSON | fast fertig | Tage |
| **Video-Dekodierung (H.264/VP9/AV1)** | fehlt | **Monate** |
| Audio-Dekodierung (AAC/Opus) | fehlt | Wochen |
| **Audio-Ausgabe** (HDA/AC'97-Treiber) | fehlt | 2–4 Wochen |
| Zeitsteuerung A/V-Synchronisation | fehlt | Wochen |
| Durchsatz | ~6,7 MiB/s HTTPS — reicht für 480p | ok |

Zur Dekodierung: In `no_std` gibt es praktisch nichts. `rav1e` ist ein
*Encoder*, `dav1d` ist C mit Assembler. Eine eigene H.264-Baseline in
Rust ist ein Projekt für sich — und ohne SIMD (wir haben SSE
abgeschaltet, `-sse,+soft-float`, wegen des Kontext-Wechsels) läuft sie
in Software auf einem Kern. **Realistisch: Standbilder und Audio ja,
Video nein.**

Der ehrliche Weg zu „YouTube" wäre also nicht YouTube, sondern
**Podcasts** (Opus/MP3 dekodieren ist überschaubar, sobald es
Audio-Ausgabe gibt).

### Ehrliches Fazit zu (b)

Bester **Nutzen je Woche** von allen dreien. Jede Anwendung ist für sich
fertig, prüfbar und benutzbar — und sie zeigt das System von seiner
starken Seite (Prozesse, Netz, TLS, Fenster) statt von seiner schwachen
(fremdes CSS).

---

## (c) Das Fundament

Was hier offen ist, betrifft nicht die Programme, sondern die Frage, ob
SpeedOS **auf echter Hardware** ein benutzbares System ist.

| Vorhaben | Aufwand | Effekt |
|---|---|---|
| **USB (xHCI)** | 4–8 Wochen | **Der größte Einzelposten.** Ohne ihn gibt es auf moderner Hardware *keine Tastatur und keine Maus* — PS/2 existiert dort nicht mehr. Der USB-Stick, von dem gebootet wird, ist danach auch lesbar. |
| **e1000/RTL8169** | 1–2 Wochen | Netz auf echter Hardware. Heute nur virtio-net, also nur in VMs. |
| **SMP + APIC** | 4–6 Wochen | Alle Kerne. Schön, aber: Ein Browser, der 250 ms braucht, wird dadurch nicht *benutzbarer*. |
| **Audio (HDA)** | 2–4 Wochen | Voraussetzung für alles Klingende. |
| **AHCI/NVMe** | 2–3 Wochen | Echte Platten statt IDE/virtio. |

### Die unbequeme Wahrheit zum Fundament

SpeedOS bootet auf echter Hardware (Acer Aspire A515-51, verifiziert,
`hardware-log.md`) — **und man kann es dort nicht bedienen**, weil das
Notebook keine PS/2-Tastatur hat. Ein Browser, den man auf dem Blech
nicht bedienen kann, ist ein QEMU-Programm.

**USB ist damit der einzige Posten auf dieser Liste, der über „schön"
hinausgeht.** Er entscheidet, ob das ganze Projekt auf einem echten
Rechner benutzbar ist oder eine Demonstration in einer VM bleibt.

---

## Empfehlung

**In dieser Reihenfolge:**

### 1. Zuerst die zwei Tage, die den Browser wirklich besser machen

Bevor irgendeine große Entscheidung fällt:

* ~~**Externe Stylesheets holen** (~1 Tag).~~ **ERLEDIGT** (Serie 9,
  Teil 1). Bilanz nach dem Augenschein: *5/3/2 → 7/2/1*. Zwei Dinge sind
  dabei anders gekommen als hier vorhergesagt, und beide stehen in der
  [zweiten Messung](browser-realitaet.md#zweite-messung-mit-externen-stylesheets):
  **(a)** Es waren nicht acht von zehn Seiten, sondern vier — fünf
  liefern ihr CSS inline. **(b)** `display:none` allein reichte für
  github *nicht*; seine Screenreader-Meldungen hängen am HTML-Attribut
  `hidden`, das erst danach dazukam.
* **Jetzt der größte Einzelposten am Browser: `float` und `position`**
  (~1–2 Tage für den einfachen Fall). Seit das CSS da ist, ist das die
  **häufigste** verbliebene Ursache dafür, dass eine Seite falsch
  aussieht: Wikipedias Seitenleiste steht als lange Liste über dem
  Artikel, und das ändert kein Stylesheet der Welt.
* Danach lohnenswert und billig: **`@media` wenigstens für `screen` und
  feste `min-width`/`max-width` auswerten**. lite.cnn.com liefert 199 KB
  CSS, und davon bleiben 16 Regeln — der Rest steckt in Media-Queries.

Das ist kein Serien-Thema, sondern der Rest von Serie 8. Es wäre
unklug, mit einer Monats-Entscheidung anzufangen, solange zwei Tage so
viel bewegen.

### 2. Dann: **USB (xHCI)** — Empfehlung für Serie 9

Begründung in einem Satz: **Es ist das Einzige auf allen drei Listen,
das darüber entscheidet, ob SpeedOS ein System ist, das man benutzen
kann, oder ein System, das man vorführt.**

Dazu kommt: Es ist ein *klassisches OS-Thema* (DMA, Ringpuffer,
Deskriptoren, Hotplug), passt zum Lernziel des Projekts, und es hat eine
harte, prüfbare Zielmarke — „auf dem Acer tippen und klicken". Der
e1000-Treiber ist eine sinnvolle Zugabe in derselben Serie (1–2 Wochen,
dieselbe PCI-Mechanik).

### 3. Danach: native Anwendungen (b)

Wenn das System auf echter Hardware bedienbar ist, zahlt jede Anwendung
doppelt. JSON-Parser, Wetter, News-Reader, IRC — jede ein bis zwei
Wochen, jede fertig.

### 4. JavaScript: **nicht jetzt**

Nicht, weil es uninteressant wäre — es ist das interessanteste Projekt
der Liste. Sondern weil es vier bis sechs Monate kostet, um die
*zweithäufigste* Fehlerursache zu beheben, und weil es die Seiten, für
die man es baut (github & Co.), am Ende trotzdem nicht zeigt.

Wenn es doch gebaut wird, dann als **Sprachprojekt mit eigenem Wert**
und mit einer Reißleine wie bei TCP: eine vorher festgelegte Zielmarke
(„diese fünf Seiten müssen danach funktionieren"), gemessen, bevor der
zweite Monat beginnt.

---

## Was in Serie 8 offen geblieben ist

Kleinigkeiten, die in keine der drei Richtungen fallen, aber notiert
gehören (vollständig in [`grenzen.md`](grenzen.md)):

* ~~Externe Stylesheets~~ — erledigt (Serie 9, Teil 1). Neu offen daraus:
  **HTTP/1.1-Keep-Alive**. Fünf Blätter vom selben Host kosten heute fünf
  TLS-Handshakes; mit Keep-Alive wäre es einer. Das ist der Grund, warum
  seriell geholt wird und nicht parallel — Parallelität stünde ihm
  hinterher im Weg.
* Der Browser blockiert beim Laden; nur Bilder laden nebenher.
  Nicht-blockierendes Laden bräuchte entweder Threads im Prozess oder
  einen nicht-blockierenden Klienten.
* Die Schrift des Browsers ist das 5×7-Raster (nur ASCII, alles groß).
  Ein Schrift-Syscall oder eine mitgelieferte Bitmap-Schrift im
  User-Space würde das beheben.
* Kaskade und Layout sind mit je ~11 ms die größten Posten einer
  Ladung — **nicht optimiert, weil 29 ms für eine 300-KiB-Seite kein
  Problem sind.** Der Hebel wäre ein Regel-Index nach Tag/Klasse.
* Ein `<img>` ohne Maßangabe löst nach dem Laden ein Neu-Layout aus
  (korrekt), aber es gibt keine Zusammenfassung mehrerer Bilder — bei
  zwanzig Bildern sind das zwanzig Layouts.
