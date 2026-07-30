# Bestandsaufnahme für Serie 8: der Browser

Serie 7 hat SpeedOS ins verschlüsselte Netz gebracht. Der Browser ist der
erste große Kunde dieser Arbeit — und er bringt eine Frage mit, die das
Projekt bisher umgangen hat:

> **Wie zeichnet ein Ring-3-Prozess in ein Fenster?**

Heute kann er es nicht. Die Fenster- und Widget-Schicht (Serie 3) lebt
vollständig im Kernel; Apps sind Rust-Typen, die `ui::App` implementieren und
im Kernel-Adressraum laufen. Der Browser wäre die erste Anwendung, für die
das nicht mehr taugt: Er lädt fremde Daten, parst fremdes HTML und wird von
fremdem CSS gesteuert. Genau die Sorte Code, die in Serie 6 aus gutem Grund
nach Ring 3 gewandert ist.

Dieses Dokument entscheidet nichts — es legt die Optionen so hin, dass die
Entscheidung begründet fallen kann.

---

## (a) Fenster-Syscalls: wie kommen Pixel aus einem Prozess auf den Schirm?

### Was heute da ist

| Baustein | Wo | Was er kann |
|---|---|---|
| `FensterPuffer` | `src/fenster/mod.rs` | `Vec<Farbe>`, ein Puffer je Fenster |
| `Zeichenflaeche` (Trait) | `src/grafik.rs` | `setzen`/`lesen` + zwei Zeilen-Schnellpfade |
| `Zeichner<'_, F>` | `src/grafik.rs` | generisch über die Fläche, Clip + Alpha |
| Compositor | `src/fenster/mod.rs` | Dirty-Rects, Schatten, Titelleiste, Taskleiste |

**Die gute Nachricht:** Der Compositor komponiert schon heute aus
*Pixelpuffern*. Er weiß nichts darüber, wer sie gefüllt hat. Ein Fenster, das
einem Prozess gehört, ist für ihn kein neuer Fall — nur eine neue Quelle.

**Was fehlt**, ist ausschließlich der Weg vom Prozess zum Puffer.

### Option 1: Pixelpuffer per Syscall übergeben

```
fenster_neu(breite, hoehe) -> handle
fenster_zeichnen(handle, ptr, len, x, y, breite, hoehe)   // copy-in
```

Der Prozess malt in seinen eigenen Speicher und schiebt das Ergebnis (oder
ein Rechteck davon) per `copy_in` hinüber.

**Dafür:** Es passt zu allem, was schon da ist. `copy_in` existiert und ist
auditiert; der Kernel prüft jeden Zeiger, kopiert und folgt nie blind
(Dauerregel I). Der Prozess braucht keinerlei neue Rechte. Die ABI ist zwei
Zeilen groß. **Und es ist der einzige Weg, bei dem ein bösartiger Prozess
nichts anderes kaputtmachen kann als sein eigenes Bild.**

**Dagegen:** Es kopiert. Ein Vollbild bei 1080p sind 1920·1080·4 = **8,3 MB**
je Frame. Bei 30 fps wären das 250 MB/s allein für `copy_nonoverlapping` —
das ist machbar (der Compositor schiebt heute schon Millionen Pixel), aber es
ist reine Verschwendung.

**Wie schlimm wirklich?** Der Dirty-Rect-Mechanismus rettet das: Ein
tippender Cursor meldet einen Streifen, kein Vollbild. Gemessen wurde in
Serie 3 ein Uhr-Tick bei 4K mit 0,31 ms statt 9,3 ms. Ein Browser, der
scrollt, ist allerdings genau der Fall, bei dem *doch* alles neu wird.

### Option 2: Geteilter Speicher (shared memory)

Der Kernel mappt den `FensterPuffer` **zusätzlich** in den Adressraum des
Prozesses. Der Prozess schreibt direkt hinein, ein `fenster_fertig(handle,
rechteck)`-Syscall meldet nur noch den Schaden.

**Dafür:** Null Kopien. Das ist der Weg, den ernsthafte Systeme gehen.

**Dagegen — und das wiegt hier schwerer als anderswo:** Es durchbricht die
Zusage, die Serie 6 aufgebaut hat. Heute ist P4-Slot 1 der *einzige*
gemeinsame Nenner zwischen Kernel und Prozess, und alles, was hinüberwandert,
geht durch `copy_in`/`copy_out`. Ein geteilter Puffer heißt: Der Prozess
schreibt in Speicher, den der Kernel gleich darauf liest — **während** der
Prozess weiterschreiben kann. Jede Annahme des Compositors über den Inhalt
(Größe, Ausrichtung, „ist fertig") wird zu einer Annahme über einen
unkooperativen Fremden.

Konkret zu klären wäre: Wer besitzt die Frames? Was passiert beim Ende des
Prozesses, während der Compositor gerade komponiert? Was, wenn er die Größe
ändert? Das ist lösbar (Refcount auf den Frames, Doppelpuffer mit Umschalten
per Syscall), aber es sind **echte neue Nähte**, und jede davon ist eine
Stelle, an der eine Sicherheitszusage verloren gehen kann.

Der `BuchAllocator` aus `adressraum.rs` führt schon Buch über alle Frames
eines Prozesses — ein geteiltes Frame müsste er explizit *ausnehmen*, sonst
gibt der `Drop` Speicher frei, den der Kernel noch benutzt. Genau die Sorte
Fehler, die erst unter Last auffällt.

### Option 3: Zeichenbefehle als Protokoll (X11/Wayland-artig)

Der Prozess schickt keine Pixel, sondern Befehle: „Rechteck füllen", „Text
zeichnen", „Puffer blitten". Der Kernel führt sie auf dem `FensterPuffer`
aus.

**Dafür:** Wenig Daten. Der `Zeichner` existiert bereits und kann genau diese
Operationen — das Protokoll wäre in weiten Teilen eine Serialisierung
vorhandener Methoden. Und die Schriften blieben im Kernel, wo sie schon
liegen (vorgerastert, `noto-sans-mono-bitmap`).

**Dagegen:** Es ist die meiste Arbeit von den dreien — und für einen Browser
die **falsche Abstraktion**. Ein HTML/CSS-Renderer produziert keine
Rechtecke und Texte in einer sinnvoll serialisierbaren Reihenfolge; er
rastert eine Seite. Man baute ein Protokoll, dessen einziger großer Nutzer es
sofort umgeht, indem er ein Vollbild-Blit schickt. Dazu kommt: Jeder
Zeichenbefehl ist ein Syscall oder muss gepuffert werden — und der Kernel
müsste jeden Parameter prüfen, weil er von einem Fremden kommt.

### Empfehlung: Option 1, und zwar bewusst

**Pixelpuffer per Syscall, mit Dirty-Rect.** Begründung in der Reihenfolge
ihres Gewichts:

1. **Sie kostet keine Sicherheitszusage.** Alles läuft über `copy_in`, das
   auditiert ist und im Sicherheits-Pass beschossen wurde. Bei Option 2
   müsste die Prozess-Isolation für die Grafik teilweise aufgemacht werden —
   und das wäre der erste Rückschritt der ganzen Serie.
2. **Sie ist in einem Nachmittag gebaut.** Der Compositor braucht keine
   Änderung. Damit bleibt Serie 8 für das übrig, worum es geht: HTML, CSS,
   Layout.
3. **Die Kosten sind messbar und begrenzt.** Ein Fenster ist typisch
   800×600 = 1,9 MB; ein Scroll-Frame kostet also ~2 MB Kopie. Bei 4,2 GHz
   und einfachem `rep movsb` ist das unter einer Millisekunde. Für einen
   Browser, der ohnehin Millisekunden mit Layout verbringt, ist das nicht der
   Engpass.
4. **Sie verbaut Option 2 nicht.** Die ABI (`fenster_zeichnen(handle, ptr,
   rechteck)`) bleibt gültig, wenn der Puffer später geteilt wird — dann wird
   der Kopiervorgang zum No-Op. Man kann also zuerst *messen*, ob es
   überhaupt weh tut.

**Der Auslöser für Option 2 sollte eine Zahl sein, kein Gefühl:** Wenn ein
Scroll-Frame messbar über ~8 ms liegt und die Kopie mehr als die Hälfte davon
ausmacht, lohnt sich geteilter Speicher. Vorher nicht. Das ist dieselbe
Methodik wie die TCP-Reißleine (`docs/tcp-scope.md`): Kriterium vorher
festlegen, dann messen.

### Skizze der ABI

```
40  fenster_neu(breite, hoehe, titel_ptr, titel_len)  -> handle
41  fenster_zeichnen(handle, ptr, len, x, y, b, h)    -> 0     (copy-in + dirty)
42  fenster_titel(handle, ptr, len)                   -> 0
43  fenster_ereignis(handle, ziel_ptr)                -> 0/1   (nicht blockierend)
44  fenster_warten(handle, ziel_ptr, frist_ms)        -> 0/1   (blockierend)
    schliesse(handle)                                          (vorhanden!)
```

`schliesse` gibt es schon, und die Handle-Tabelle im PCB schließt beim
Prozess-Ende automatisch alles — ein Fenster verschwindet also mit seinem
Prozess, ohne dass jemand daran denken muss. Das ist geschenkt.

---

## (b) Wie kommen Eingabe-Events in den Prozess?

Der Kernel hat die Ereignisse bereits: `UiEreignis` (Klick, Doppelklick,
Losgelassen, Bewegt, Scroll, Taste, MausRein/Raus, FokusRein/Raus), und der
Manager routet sie schon heute an das richtige Fenster in Fenster-Koordinaten.

Was fehlt, ist der Weg hinaus. Zwei Varianten:

**Abholen (`fenster_ereignis`, nicht blockierend)** passt zu einem Programm,
das ohnehin eine Schleife dreht. Es passt aber schlecht zu einem Browser, der
zwischen zwei Klicks nichts zu tun hat — er würde entweder pollen (CPU
verbrennen) oder schlafen (träge reagieren).

**Warten (`fenster_warten`, blockierend mit Frist)** ist das Richtige. Die
Mechanik dafür ist **vollständig vorhanden**: `prozess::Warteauf` kennt
bereits `Zeit`, `Kind`, `PipeLesen`, `PipeSchreiben`; ein `FensterEreignis`
wäre eine weitere Variante, und `scheduler::wecken` macht den Wartenden
sofort lauffähig (Serie 7, Teil 0 — Weck-Latenz **5 µs**). Der Eingabe-Router
weckt beim Zustellen, das Sicherheitsnetz im Timer bleibt.

**Das Ereignis-Format ist ABI** und gehört deshalb in `docs/syscalls.md`
festgenagelt: fester `#[repr(C)]`-Datensatz, `(Zeiger, Länge)` wie überall,
keine Nullterminierung. Ein Vorschlag:

```
typ: u32        // 0 Klick, 1 Losgelassen, 2 Bewegt, 3 Scroll,
                // 4 Taste, 5 FokusRein, 6 FokusRaus, 7 Groesse, 8 Schliessen
x, y: i32       // Fenster-Koordinaten
knopf: u32      // Maustaste bzw. Tastencode (Unicode)
modifikatoren: u32
zeit_ms: u64
```

**Eine Entscheidung, die man hier trifft, ohne es zu merken:**
`UiEreignis::Taste` liefert heute schon dekodiertes Unicode (der KeyStream
macht das, inklusive QWERTZ-Umlauten). Das sollte so bleiben — ein Browser
will Zeichen, keine Scancodes. Wer Scancodes braucht (Spiele), bekommt sie
später als eigener Ereignistyp.

**Und die Fokus-Frage ist schon beantwortet:** Der Manager weiß, welches
Fenster den Fokus hat, und schickt Tasten nur dorthin. Ein Prozess bekommt
also nie Eingaben für ein fremdes Fenster — das ist dieselbe Isolation wie
bei Handles, nur für Ereignisse.

---

## (c) DIE Architekturfrage: wandert das Widget-Toolkit nach Ring 3?

Das Toolkit aus Serie 3 (`src/ui/`) ist substantiell: `Widget`-Trait, Layout
(`laengen_verteilen`, VBox/HBox/Füller), Button, Checkbox, Textfeld,
ScrollListe, Fokus-Kette, Dialoge, Kontextmenüs. Vier Apps benutzen es
(Explorer, SpeedText, Einstellungen, Task-Manager).

### Die drei Wege

**(1) Toolkit bleibt im Kernel, der Browser malt selbst.**
Der Browser bekommt ein Fenster und rastert hinein. Er braucht Knöpfe für
„Zurück" und eine Adresszeile — die malt er sich selbst.

*Dafür:* Nichts muss umziehen. Die vier vorhandenen Apps laufen weiter. Der
Browser braucht sowieso einen eigenen Renderer — ein Toolkit-Button und ein
CSS-gerendeter Button haben nichts miteinander zu tun.
*Dagegen:* SpeedOS hätte dauerhaft **zwei** Widget-Welten. Die zweite (im
Browser) wäre anfangs winzig und würde wachsen.

**(2) Toolkit wandert vollständig nach `libspeed`, alle Apps werden Prozesse.**
*Dafür:* Die saubere Lösung. Der Kernel verlöre `src/ui/`, `src/explorer.rs`,
`src/speedtext.rs`, `src/einstellungen.rs` (App-Teil), `src/taskmanager.rs` —
das sind mehrere tausend Zeilen, die dann nicht mehr im Kernel *sein müssten*.
*Dagegen:* Das ist **Serie 8 komplett**, und der Browser käme in Serie 9.
Außerdem hängen die Apps an Kernel-Innereien (`fs::mit_fs`, `theme::aktuell`,
`einstellungen::`), die alle zu Syscalls werden müssten.

**(3) Das Toolkit wird GETEILT — als eigene Kiste, wie `speedhttp`.**

Das ist die Empfehlung, und es ist genau das Muster, das Serie 7 erfolgreich
vorgemacht hat: `speedhttp` ist der Parser aus Serie 5, herausgelöst, ohne
Abhängigkeiten, benutzt vom Kernel **und** von Ring 3.

Das Toolkit ist dafür fast schon vorbereitet. `Widget::zeichnen` nimmt einen
`Zeichner<'_, FensterPuffer>` — und `Zeichner` ist **bereits generisch** über
`Zeichenflaeche`. Was fehlt, ist wenig:

* `grafik.rs` (Zeichner, Farbe, Rechteck, Clip, Alpha) + `ui/` in eine Kiste
  `speedui`, die nur `alloc` braucht.
* `Widget::zeichnen` generisch über `Zeichenflaeche` statt fest auf
  `FensterPuffer`.
* Die Font-Frage (siehe (d)) — heute kommt die Rasterung aus einer
  Kernel-Kiste.
* `theme::aktuell()` ist ein lockfreies Atomic im Kernel; im User-Space wäre
  es ein Wert, den das Fenster beim Öffnen mitbekommt.

**Der Gewinn ist derselbe wie bei `speedhttp`:** eine Implementierung, zwei
Kunden. Die vorhandenen Apps bleiben, wo sie sind (kein Umbau, kein Risiko),
und der Browser bekommt dieselben Knöpfe, ohne sie nachzubauen. Wenn später
eine App nach Ring 3 wandern soll, ist der Weg schon frei.

**Der ehrliche Vorbehalt:** Bei `speedhttp` war die Trennung sauber, weil ein
HTTP-Parser nichts kennt außer Bytes. Das Toolkit kennt Schriften, Themes und
Zeit (Cursor-Blinken). Jede dieser drei Abhängigkeiten muss zu einem Argument
werden, sonst zieht die Kiste den Kernel hinter sich her. Das ist die
eigentliche Arbeit — und der Punkt, an dem sich entscheidet, ob Weg (3)
funktioniert oder in Weg (1) zurückfällt.

---

## (d) Was ein HTML/CSS-Renderer von der Plattform braucht

### Schriften in beliebigen Größen — die größte echte Lücke

Heute: **vorgerasterte Bitmap-Fonts** in genau drei Größen (16/24/32,
`noto-sans-mono-bitmap`), monospace, Latin-1.

Ein Browser braucht: proportionale Schrift, beliebige Größen (`font-size:
13px`), fett/kursiv, und Textmessung *vor* dem Zeichnen (ohne
`text_breite(&str) -> i32` gibt es kein Zeilenumbruch-Layout).

Drei Wege:
* **Mehr Größen vorrastern** — billig, aber `font-size: 13px` bleibt
  unmöglich, und jede Größe kostet Platz im Image.
* **Einen TrueType-Rasterizer einbauen** (`ab_glyph`, `fontdue` — beide
  `no_std`-tauglich). Das ist der richtige Weg und ein eigenes Vorhaben:
  Glyph-Cache, Hinting-Verzicht, Subpixel ja/nein.
* **Skalierung der Bitmaps** — sieht schlecht aus, macht aber ein V1 möglich.

**Empfehlung:** Für V1 mit vorgerasterten Größen anfangen und CSS-Größen auf
die nächstliegende runden. Ehrlich dokumentieren. Der Rasterizer ist Serie 9.

### Bild-Dekodierung — fehlt vollständig

Kein PNG, kein JPEG, kein GIF. Für V1: Bilder als Platzhalter-Rechteck mit
`alt`-Text. PNG wäre der erste Schritt (`miniz_oxide` + eigener
PNG-Chunk-Leser); JPEG ist deutlich mehr Arbeit.

### Scroll-Leistung

Der Dirty-Rect-Mechanismus hilft beim Scrollen **nicht** — da ändert sich
alles. Was hilft:
* Der Browser rendert in einen Puffer, der **höher** ist als das Fenster, und
  verschiebt beim Scrollen nur den Ausschnitt (memmove im eigenen Puffer,
  dann ein Blit). `DoppelPuffer::hochscrollen` macht im Kernel genau das
  schon.
* Gemessen werden muss, was ein Vollbild-Blit über die Syscall-Grenze
  kostet — siehe (a), das ist die Zahl, an der Option 2 hängt.

### Was schon da ist und nicht auffällt

* **Der Abruf.** `libspeed::netz::Klient` liefert eine URL mit
  Weiterleitungen, Frist, Größenlimit und geprüften Zertifikaten. Der Browser
  schreibt drei Zeilen Netz-Code — `news` ist der Beweis.
* **Heap.** 12 MiB je Prozess, `brk`-Modell, gemessen.
* **`speedhttp`.** Content-Type, Header, chunked — alles vorhanden.
* **UTF-8.** Rust-`&str` überall; die Framebuffer-Konsole ist Latin-1, ein
  eigener Renderer wäre es nicht.

### Was fehlt und teuer ist

* Ein **HTML-Parser**, der kaputtes HTML übersteht (also die Regel, nicht die
  Ausnahme). `news` hat einen Zeichen-Automaten mit drei Zuständen — das ist
  bewusst kein Parser. Ein echter braucht einen Tokenizer plus
  Baum-Konstruktion.
* **CSS**: Parser, Kaskade, Spezifität, Vererbung.
* **Layout**: Block- und Inline-Fluss. Das ist der schwierigste Teil und der,
  den man am leichtesten unterschätzt.

---

## (e) Realistischer Zuschnitt für Browser V1

### Was er kann

* `https://` und `http://` über `libspeed::netz` (steht, gemessen)
* HTML **lesen**: Tokenizer + DOM-Baum, tolerant gegen Kaputtes
* **Block-Layout**: `<p>`, `<div>`, `<h1>`–`<h6>`, `<ul>/<ol>/<li>`, `<pre>`,
  `<blockquote>`, `<hr>`, `<br>`
* **Inline**: Text mit Umbruch, `<b>`, `<i>`, `<code>`, `<a>`
* **CSS in Spurweite**: `color`, `background-color`, `font-size`,
  `font-weight`, `margin`, `padding`, `text-align` — aus `style`-Attributen
  und einfachen Selektoren (Typ, Klasse, ID). Keine Kaskade über mehrere
  Blätter.
* **Links klicken**, Verlauf zurück/vorwärts, Adresszeile, Neu laden
* Scrollen mit Rad und Tastatur
* **Das Schloss-Symbol mit Substanz:** Protokollversion, Ciphersuite und die
  geprüfte Kette stehen schon im `Abruf` (`Verbindungsinfo::kette`) — anders
  als bei den meisten Browsern kann man hier zeigen, *warum* vertraut wird.

### Was er ausdrücklich NICHT kann

| nicht dabei | warum |
|---|---|
| **JavaScript** | Eine JS-Engine ist ein eigenes Projekt in der Größenordnung des restlichen Betriebssystems. Kein Interpreter, kein `<script>` — der Inhalt fliegt raus (wie in `news`). |
| Tabellen-Layout | `<table>` ist ein eigener Layout-Algorithmus. V1 rendert Zellen untereinander. |
| Flexbox / Grid | Setzt einen echten Constraint-Solver voraus. |
| `position: absolute/fixed` | Bricht den Blockfluss auf. |
| Floats | Berüchtigt kompliziert für das, was sie leisten. |
| Bilder | Keine PNG/JPEG-Dekodierung (siehe (d)) — Platzhalter mit `alt`-Text. |
| Formulare absenden | Kein POST, kein `multipart`. Anzeigen ja, absenden nein. |
| Cookies, LocalStorage | Zustand über Abrufe hinweg ist eine eigene Baustelle (und eine Datenschutzfrage). |
| Video, Audio, Canvas, WebGL | — |
| mehrere Tabs | Ein Fenster, eine Seite. Mehrere Fenster gehen (jedes ist ein Prozess). |

### Der Prüfstein

Ein Browser-V1 ist gelungen, wenn **`https://example.com` und
`https://info.cern.ch/hypertext/WWW/TheProject.html`** ordentlich aussehen —
die erste Webseite der Welt ist reines HTML ohne CSS und damit der ehrlichste
Maßstab für einen Renderer, der bei Null anfängt.

Ein guter zweiter Prüfstein wäre eine Wikipedia-Seite: viel Text, Listen,
Links, Tabellen. Sie wird *nicht* gut aussehen — aber sie muss **lesbar**
sein, und der Browser darf nicht abstürzen.

### Die Reihenfolge, die ich vorschlagen würde

1. **Fenster-Syscalls** (a) + Ereignisse (b) — ohne die geht nichts. Ein
   Prozess, der ein leeres Fenster öffnet und auf Klicks reagiert, ist der
   Meilenstein von Teil 1.
2. **`speedui`** herauslösen (c), mit dem Fenster-Syscall als erstem Kunden.
3. **HTML-Tokenizer + DOM** — noch ohne Layout, Ausgabe als Baum im Terminal.
4. **Block-Layout + Text-Umbruch**, monospace, eine Schriftgröße.
5. **CSS-Spurweite** und mehrere Schriftgrößen.
6. **Links, Verlauf, Adresszeile** — jetzt ist es ein Browser.

Schritt 1 und 2 sind Betriebssystem-Arbeit, 3 bis 6 sind Anwendungs-Arbeit.
Die Trennung ist wichtig: Der interessante Teil für ein OS-Projekt ist der
erste, und er ist auch der, der schiefgehen kann.
