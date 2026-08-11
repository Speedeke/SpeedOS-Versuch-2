# Fenster-Syscalls: die Naht zwischen Ring 3 und dem Desktop

*Serie 8, Teil 1 — Juli 2026*

Bis hierher lebte die gesamte Fenster- und Widget-Schicht im Kernel. Ein
Programm in Ring 3 konnte rechnen, Dateien lesen und HTTPS sprechen, aber es
konnte **kein Fenster besitzen**. Das war die erste Lücke, die
`docs/serie8-bestandsaufnahme.md` für den Browser zu schliessen empfiehlt.

Dieses Dokument sagt, **was gebaut wurde, warum genau so, was es kostet** —
und unter welcher Bedingung die Entscheidung neu bewertet wird.

---

## 1. Die Entscheidung: Pixelpuffer per Syscall

Die Bestandsaufnahme hatte drei Wege bewertet:

| Weg | Kurz | Bewertung |
|---|---|---|
| **(a) Pixelpuffer per Syscall** | Der Prozess übergibt Bytes, der Kernel kopiert | einfach, kostet eine Kopie |
| (b) Geteilter Speicher | Dieselbe Seite in zwei Adressräumen | schneller, mehr Nähte |
| (c) Zeichenbefehle als Protokoll | „male ein Rechteck" statt Pixel (X11/Wayland) | mächtig, viel Arbeit |

Umgesetzt ist **(a)**. Die Begründung ist nicht „am schnellsten" — sie ist:

**Es kostet keine Sicherheitszusage.** Der Prozess übergibt einen Zeiger und
eine Länge; der Kernel prüft ihn mit demselben `copy_in`-Apparat wie jedes
andere Argument (Dauerregel I) und **kopiert**. Bei geteiltem Speicher läge
dieselbe Seite gleichzeitig in zwei Adressräumen — dann gilt „prüfen, dann
kopieren" nicht mehr, denn der Prozess kann die Daten unter den Händen des
Kernels ändern. Das ist keine theoretische Sorge: Der Compositor liest den
Puffer, während der Prozess läuft.

**Es verbaut (b) nicht.** Die ABI redet über (Zeiger, Länge, Rechteck). Ein
späterer geteilter Puffer wäre ein zweiter Weg, dieselben Pixel zu liefern —
kein anderer Vertrag, keine Änderung an den Programmen, die es nicht nutzen.

**(c) wäre eine eigene Serie.** Ein Zeichenprotokoll heisst: Befehlsformat,
Serialisierung, Fehlerbehandlung je Befehl, und der Kernel bekommt einen
Interpreter für fremde Eingaben. Genau das, was ein Mikrokernel-Entwurf
draussen haben will.

---

## 2. Die fünf Syscalls

Die vollständige Tabelle steht in `docs/syscalls.md`. Kurzfassung:

```
48  fenster_oeffnen(titel_ptr, titel_len, breite, hoehe)   -> Handle
49  fenster_zeichnen(handle, pixel_ptr, len, rechteck)     -> gesetzte Pixel
50  fenster_ereignis(handle, ziel_ptr, frist_ms)           -> Ereignis-Art
51  fenster_titel_setzen(handle, titel_ptr, titel_len)
52  fenster_schliessen(handle)
```

### Das Fenster ist ein Handle

Kein Sonderfall, keine eigene Fenster-Tabelle: `KernelObjekt::Fenster` steht
neben `Socket` und `PipeLesen` in derselben Per-Prozess-Handle-Tabelle aus
Serie 6. Drei Dinge folgen daraus **von selbst**:

* Ein fremdes Fenster ist unerreichbar — es gibt aus einem anderen Prozess
  keine Zahl, die dorthin führt (nachgewiesen: B probiert alle 32 durch).
* `schliesse(handle)` schliesst auch ein Fenster.
* Der `Drop` der Tabelle räumt beim Prozess-Ende alles ab. **Es gibt keinen
  Pfad, der es vergessen könnte** — auch nicht nach einem Absturz.

### Das Rechteck steckt gepackt in einem Register

`fenster_zeichnen` hat vier Argumentregister und braucht fünf Zahlen. Also:

```
rechteck = (x << 48) | (y << 32) | (breite << 16) | hoehe      (je 16 Bit)
```

Die Alternative wäre ein Zeiger auf ein Struct im User-Speicher gewesen — eine
Bereichsprüfung und eine Fehlerquelle mehr für vier kleine Zahlen. Dasselbe
Argument wie bei `pipe()`, das zwei Handles in ein Register packt.

**Warum der Bereich überhaupt im Syscall steht:** damit ein Programm einen
*Streifen* nachzeichnen kann statt immer das ganze Fenster. Der Kernel meldet
genau diesen Streifen als Schaden an den Compositor — die Dirty-Rect-Mechanik
aus Serie 4 zahlt sich hier unmittelbar aus. Wie viel das ausmacht, steht in §4.

### Das Pixelformat

4 Byte je Pixel: **Byte 0 = Blau, 1 = Grün, 2 = Rot, 3 = ungenutzt.** Als
Little-Endian-`u32` gelesen ist das `0x00RRGGBB`, also die Schreibweise aus
HTML.

Umgerechnet und **nicht** gecastet: `Farbe` ist ein gewöhnliches Rust-Struct
ohne `repr(C)`, seine Feldreihenfolge ist nicht zugesichert. Aus User-Bytes
einen `&[Farbe]` zu machen wäre eine Annahme über den Compiler an genau der
Stelle, an der fremde Daten hereinkommen. Die Umrechnung ist zugleich der
Posten, den das Umstiegskriterium misst.

### Geklemmt, nicht abgelehnt

Ein Rechteck, das über den Fensterrand hinausragt, wird **geschnitten**, und
der Syscall liefert die Zahl der wirklich gesetzten Pixel (POSIX-`write`-
Semantik: „so viel habe ich genommen").

Der Grund ist ein unvermeidbares Wettrennen: Zwischen dem Augenblick, in dem
ein Prozess seine Grösse erfährt, und dem, in dem er zeichnet, kann der
Benutzer am Fensterrand gezogen haben. Würde das einen Fehler geben, müsste
jedes Programm den **Normalfall** als Fehler behandeln.

Ein Rechteck *ganz* ausserhalb ist dagegen ein Fehler — das ist ein Bug, kein
Rennen. Und geschrieben wird in keinem Fall über den Puffer hinaus; das ist
die Zusage, auf die es ankommt, und sie ist mit Kanarienvögeln nachgewiesen
(`tests/fenster.rs::test_boese_zeiger_und_rechtecke`).

---

## 3. Eingabe: wie Ereignisse in den Prozess kommen

`fenster_ereignis(handle, ziel_ptr, frist_ms)` schreibt 16 Byte in den
Prozess: `art`, `x`, `y`, `wert` (je 32 Bit). Maus-Koordinaten sind
**fensterlokal** — der Prozess kennt seine Bildschirmposition nicht und soll
sie nicht kennen.

**Blockierend mit Frist**, über den Weck-Pfad aus Serie 7, Teil 0: neuer
Wartegrund `Warteauf::Fenster(id)`, sofortiges Wecken aus dem Eingabe-Pfad,
der Timer als Sicherheitsnetz. Gemessen kommt der Weckruf in **unter 1 ms**
an (`tests/fenster.rs`: 0 ms).

Drei Entscheidungen, die Denkarbeit waren:

**(1) Eine abgelaufene Frist ist kein Fehler.** Sie liefert `Keins`. Ein
Programm, dessen Normalfall ein Fehlercode wäre, schreibt seine Schleife
falsch herum — und in der Frist-Runde animiert eine Oberfläche.

**(2) Die Queue verliert nie Schliessen und Grösse.** Beide sind *Felder*,
keine Queue-Einträge:

* Eine **Grössenänderung ist ein Zustand**, kein Ereignis. Wer dreimal am
  Rand zieht, will nicht drei Meldungen, sondern die letzte Grösse.
* Ein **Schliessen-Wunsch**, der in einer vollen Queue verschwindet, wäre ein
  Fenster, das sich nicht schliessen lässt — für einen Benutzer ein defektes
  System.

Die Eingabe-Queue selbst ist auf 64 gedeckelt, Mausbewegungen verschmelzen am
Ende (eine Maus liefert 200 Pakete je Sekunde), und was verworfen wird, wird
**gezählt** statt heimlich zu verschwinden.

**(3) Der Schliessen-Knopf bittet, er befiehlt nicht.** Der Prozess besitzt
den Puffer und darf aufräumen. Reagiert er nicht, schliesst der **zweite
Klick** — kein Zeitgeber, keine Frist, die man erklären muss. „Nochmal
klicken" ist ohnehin, was ein Mensch tut.

### Die Frist liegt im Fenster, nicht im Syscall

Ein blockierender Syscall wird bei SpeedOS **neu gestartet** (Serie 6,
Teil 6: `rip -= 2`). Eine lokal berechnete Frist begänne bei jedem Durchlauf
von vorn, und `frist_ms = 100` könnte ewig dauern. Sie steht deshalb im
`ProzessFenster` und wird nur gesetzt, wenn noch keine steht — der Neustart
ändert damit nichts, wie die Neustart-Regel es verlangt.

### Die Lock-Falle dieses Teils

`scheduler::wecken` nimmt die Prozess-**Tabelle**, und der Timer nimmt sie
**vor** dem MANAGER (`warter_wecken` sieht im Fenster nach). Aus dem
gehaltenen MANAGER heraus zu wecken wäre also ein ABBA.

Deshalb wird der Weckruf unter dem Lock nur **vorgemerkt** und danach
ausgelöst; `fenster::mit_manager_wecken` ist der Helfer, damit keine
Aufrufstelle es vergessen kann. Dasselbe Muster wie bei den Pipes in
Serie 7, Teil 0.

---

## 4. Was es kostet — die Messung

Gemessen **aus Ring 3**, mit dem echten Programm `messung 7` über die echten
Syscalls (`tests/fenster_messung.rs`). Ein Kernel-seitiger Aufruf wäre
billiger und würde die Zahl schönrechnen — er hätte weder Privilegienwechsel
noch Zeigerprüfung.

QEMU mit WHPX (Hardware-Virtualisierung, also native Ausführung auf dem
Host-Kern). 1000 Runden je Vollbild, 5000 je Streifen; die Uhr hat
Millisekunden-Auflösung, deshalb die hohen Rundenzahlen.

### 720p-Klasse (Bildschirm 1360 × 768, Fenster 1360 × 696)

| Vorgang | Fläche | Zeit |
|---|---:|---:|
| Malen im **eigenen** Puffer (kein Syscall) | 946 560 px | **57 µs** |
| `fenster_zeichnen`, **volles Fenster** | 946 560 px | **128 µs** |
| `fenster_zeichnen`, **Streifen** (volle Breite, 16 Zeilen) | 21 760 px | **3,2 µs** |
| `fenster_zeichnen`, **Block** 32 × 32 (mit Umkopieren) | 1 024 px | **0,4 µs** |

### 4K (Bildschirm 3840 × 2160, Fenster 3840 × 682 — siehe §6)

| Vorgang | Fläche | Zeit |
|---|---:|---:|
| Malen im eigenen Puffer | 2 618 880 px | **183 µs** |
| `fenster_zeichnen`, volles Fenster | 2 618 880 px | **509 µs** |
| `fenster_zeichnen`, Streifen (3840 × 16) | 61 440 px | **11,2 µs** |
| `fenster_zeichnen`, Block 32 × 32 | 1 024 px | **0,6 µs** |

### Was die Zahlen sagen

**Der Durchsatz liegt bei ~5–7 Mio. Pixel je Millisekunde** und skaliert
linear mit der Fläche (bei 10× der Rundenzahl änderte sich das Ergebnis um
2 %). Ein Streifen ist genau um seinen Flächenanteil billiger — die feste
Grundgebühr eines Syscalls (~70 ns Roundtrip aus Serie 7) verschwindet neben
der Kopie, sobald mehr als ein paar hundert Pixel im Spiel sind.

**Der Bereich im Syscall lohnt sich messbar:** Eine nachgezogene Textzeile
kostet bei 4K **11 µs** statt der 509 µs eines Vollbilds — Faktor 45. Das ist
der Grund, warum das Rechteck im Syscall steht, und `fenstertest` sendet
seinen Klick-Punkt entsprechend als 17 × 17-Streifen.

**Ehrliche Einordnung:** Die Puffer sind hier cache-warm, und der Prozess
malt eine einfarbige Fläche. Ein echter Renderer liefert Pixel, die gerade
erst entstanden sind, und die Zahlen werden schlechter. Deshalb ist das
Kriterium unten an einem **Verhältnis** festgemacht und nicht an einer
absoluten Schranke allein.

---

## 5. DAS UMSTIEGSKRITERIUM

Nach dem Muster der TCP-Reissleine (`docs/tcp-scope.md`) — **vorher**
festgelegt, damit es später objektiv prüfbar ist und nicht nachträglich
verschoben werden kann:

> **Geteilter Speicher wird neu bewertet, wenn ein Scroll-Frame über
> ~8 ms braucht UND die Kopie mehr als die Hälfte davon ausmacht.**

### Warum beide Bedingungen

Die erste allein wäre wertlos: Ein langsamer Frame kann genauso gut am
*Malen* liegen — dann würde geteilter Speicher nichts ändern und man hätte
eine Sicherheitszusage für nichts aufgegeben. Erst der Anteil sagt, ob die
**Naht** das Problem ist.

Die 8 ms sind die halbe Frame-Zeit bei 60 Hz. Ein Scroll, der länger
braucht, ist als Ruckeln sichtbar.

### Die Messmethode (damit sie wiederholbar ist)

```bash
cargo test --test fenster_messung                    # 720p-Klasse
```

```bash
SPEEDOS_AUFLOESUNG=4k cargo test --test fenster_messung
```

Der Test rechnet das Kriterium aus und schreibt das Ergebnis ins Protokoll:

```
[MESSUNG-FENSTER] Scroll-Frame (malen + uebertragen) = <F> us,
                  davon Kopie <A> % -> Umstiegskriterium <erfuellt|nicht>
```

* **Scroll-Frame** = `MALEN_US + VOLLBILD_US` — was ein voller Neuaufbau
  kostet, den Malvorgang eingerechnet.
* **Anteil** = `VOLLBILD_US / Scroll-Frame`.

### Stand bei der Festlegung (Serie 8, Teil 1)

Gemessen mit `fenstertest`, also einem Programm, das eine **einfarbige
Fläche** malt:

| Auflösung | Scroll-Frame | Anteil Kopie | Kriterium |
|---|---:|---:|---|
| 720p-Klasse | 190 µs | 68 % | **nicht erfüllt** |
| 4K (Fenster 3840 × 682) | 692 µs | 73 % | **nicht erfüllt** |
| 4K hochgerechnet auf volle Höhe¹ | ~2 100 µs | ~73 % | **nicht erfüllt** |

¹ Hochrechnung, keine Messung: 3840 × 2088 px bei linearem Verhalten. Warum
sie nötig war: §6.

**Der Anteil ist schon hier über 50 %** — die Kopie *ist* der grössere
Posten. Es fehlt allein die absolute Schranke, und zwar um den Faktor vier.

### DIE ENTSCHEIDUNG (Serie 8, Teil 7): der Pixelpuffer bleibt

Mit dem ersten echten Renderer wurde aus der Schätzung eine Messung — an
einem langen Wikipedia-Artikel, aus Ring 3, mit `browser --messen=200`.
**Vollständig samt Methodik und Gegenrechnung in
[`browser-rendern.md`](browser-rendern.md) §4**; die Kurzfassung:

| Auflösung | Scroll-Frame | Anteil Kopie | Kriterium |
|---|---:|---:|---|
| 720p-Klasse (1360 × 696) | 500 µs | 90 % | **nicht erfüllt** |
| 4K (3840 × 2088, echt gemessen) | 7 050–7 725 µs | 74 % | **nicht erfüllt** |

Das Kriterium ist **nicht erfüllt**, geteilter Speicher wird also nicht neu
bewertet. Zwei Dinge gehören dazu, damit das keine bequeme Lesart ist:

1. **Die 4K-Zahl ist knapp** — 88–97 % der Schwelle, nicht „reichlich Luft".
2. **Ohne das Streifen-Zeichnen wäre das Kriterium ERFÜLLT** (9 725–10 150 µs
   bei 52–56 % Kopie-Anteil). Die naheliegende Optimierung ist also nicht
   nachträglich angewandt worden, um ein unbequemes Ergebnis zu drücken —
   sie war Teil desselben Schritts, und die Gegenrechnung steht in der
   Testausgabe jedes Laufs.

Die oben geäusserte Erwartung — „wenn ein HTML-Renderer dazukommt, wird der
Malvorgang teurer und der Anteil *sinkt*" — hat sich **nicht** bestätigt:
Der Anteil blieb bei 74 %, weil das Streifen-Zeichnen den Malvorgang
kleiner gehalten hat, als das Vollbild-Malen ihn gemacht hätte. Bei 720p
stieg er sogar auf 90 %.

---

## 6. Die offene Grenze, die beim Messen auffiel

**Ein Fenster in voller 4K-Grösse passt nicht in den User-Heap.**

Der Pixelpuffer eines Programms liegt auf seinem eigenen Heap, und der ist
auf **12 MiB** gedeckelt (`prozess::HEAP_MAX_BYTES`; er wohnt in der
16-MiB-Lücke zwischen Programm-Image und Stack, siehe `docs/syscalls.md` §9).
Ein Fenster über den ganzen 4K-Schirm wäre

```
3840 × 2088 × 4 Byte = 32,1 MiB
```

Das ist fast das Dreifache. Die Messung kürzt die Höhe deshalb auf das, was
hineinpasst, und meldet es (`HOEHE_GEKUERZT=1`) — statt die Zahl zu umgehen.

**Was das für Serie 8 heisst:** Ein Browser bei 4K braucht entweder einen
grösseren User-Heap oder er muss seinen Inhalt in Streifen halten. Beides ist
machbar, beides ist eine Entscheidung, und sie gehört an den Anfang des
Browsers und nicht mittenhinein:

* **Heap vergrössern** heisst, das Prozess-Layout zu ändern (die Lücke
  zwischen Image und Stack wächst) — eine ABI-Änderung nach `docs/syscalls.md`
  §9, die alle Programme betrifft.
* **In Streifen halten** kostet nichts an der ABI und ist ohnehin das, was ein
  scrollender Renderer tut. Der Nachteil: Das Programm muss seinen sichtbaren
  Ausschnitt selbst verwalten.

Eingetragen in `docs/grenzen.md`.

---

## 6b. `starte … &` — warum ein Fenster-Programm den Vordergrund nicht verträgt

Der erste Versuch im laufenden System sah so aus: `starte fenstertest`, das
Programm meldet über die serielle Schnittstelle „Fenster offen (420×280)" —
und auf dem Bildschirm passiert **nichts**. Sechzig Sekunden lang. Auch die
Uhr in der Taskleiste stand still. Erst als das Programm endete, kam das Bild
zurück.

Kein Deadlock, sondern eine bekannte Eigenschaft, die hier zum ersten Mal
weh tut: **Solange ein Shell-Befehl synchron läuft, kommt kein anderer
Kernel-Task dran** — auch der Compositor nicht. Die Shell-Sitzung ist selbst
ein Task im kooperativen Executor von PID 0; wer ihn nicht verlässt, gibt
niemandem sonst Zeit. Für `hallo` oder `kopiere` ist das gleichgültig. Ein
Programm mit eigenem Fenster zeichnet dagegen brav, und niemand sieht es.

Die Pump-Schleife den Compositor treiben zu lassen ginge nicht: Der
Compositor ist ein `async`-Task, und ein synchroner Befehl kann den Executor
nicht betreten. Also gibt es jetzt den **Hintergrund-Start**:

```
starte fenstertest &
```

Einplanen, PID melden, zurück. Der Executor läuft weiter, der Compositor
komponiert, das Fenster erscheint. Ein Hintergrund-Prozess bekommt **keine
Ausgabe-Pipe**, sondern die Standard-Ausgabe der Shell — eine Pipe ohne
Leser würde nach 64 KiB für immer blockieren. Seine Ausgabe erscheint dafür
mitten im Terminal, wo der Benutzer gerade tippt; das ist unschön und
ehrlich.

Pipelines im Hintergrund gibt es (noch) nicht: Dafür müsste jemand die
Zwischen-Pipes leeren.

---

## 7. Was ausdrücklich (noch) nicht geht

* **Keine Schrift aus dem Kernel.** Die vorgerasterten Fonts sind
  Kernel-Daten; es gibt keinen Syscall, der sie herausgibt. `libspeed::fenster`
  bringt deshalb ein 5 × 7-Raster mit — genug für ein Beweis-Programm, nichts
  für einen Renderer. Was ein HTML/CSS-Renderer wirklich braucht, steht in
  `docs/serie8-bestandsaufnahme.md`.
* **Kein Doppelklick, keine Modifikatortasten** in der Ereignis-ABI.
  Umschalt/Strg/Alt werden nicht zugestellt (der Kernel dekodiert sie schon
  in das Unicode-Zeichen); ein Doppelklick müsste das Programm aus zwei
  Klicks und der Uhr selbst erkennen.
* **Kein Ziehen und Ablegen, keine Zwischenablage** zwischen Prozess-Fenstern
  und Kernel-Apps. Die Ablage (`src/ablage.rs`) ist Kernel-seitig und hat
  keine Syscall-Naht.
* **Der Prozess erfährt seine Bildschirmposition nicht** und kann das Fenster
  nicht selbst verschieben oder maximieren. Das ist Absicht: Die Geometrie
  gehört dem Fenster-Manager.
* **Kein Icon nach Wahl.** Ein Prozess-Fenster trägt das SpeedOS-Logo. Die
  Titelleiste gehört dem Kernel, und ein Programm soll sich dort nicht als
  etwas anderes ausgeben können.

---

## 7b. Die `unsafe`-Fläche dieses Teils

Projektregel: Jeder `unsafe`-Block bekommt eine Begründung. Für Serie 8,
Teil 1 ist die Bilanz kurz:

| Datei | `unsafe` | Anmerkung |
|---|---:|---|
| `src/syscall/fenster.rs` | **0** | reine Prüf- und Rechenlogik |
| `src/fenster/prozessfenster.rs` | **0** | Warteschlange und ABI-Bytes |
| `userland/src/bin/fenstertest.rs` | **0** | |
| `userland/src/fenster.rs` | 5 | ausschliesslich `int 0x80` (der `syscall`-Wrapper), wie in jedem libspeed-Modul |
| `src/ring3.rs` | **+1 neu** | `copy_in_scheibe` |

**Der eine neue Block**, `copy_in_scheibe`:

```rust
user_bereich_pruefen(user_ptr, ziel.len(), false)?;
unsafe { core::ptr::copy_nonoverlapping(user_ptr as *const u8, ziel.as_mut_ptr(), ziel.len()); }
```

*Invariante:* Die Prüfung davor stellt alle drei Anforderungen von
`copy_nonoverlapping` her — der gesamte Bereich liegt im User-Bereich (Stufe
a, mit `checked_add`), jede berührte Seite ist im **aktiven** Adressraum
gemappt und `USER_ACCESSIBLE` (Stufe b), und die Länge ist auf 64 KiB
gedeckelt. Überlappung ist ausgeschlossen: `ziel` ist Kernel-Speicher, die
Quelle liegt nachweislich im User-Bereich. Es ist derselbe Block wie in
`copy_in` — nur das Ziel ist ein vorhandener Puffer statt eines frischen
`Vec`.

Der PIXEL-PFAD selbst ist `unsafe`-frei: `pixel_schreiben` und
`zeile_aus_pixelbytes` rechnen mit Indizes auf `Vec`s, und die Umrechnung
Byte → `Farbe` geht durch den Konstruktor.

---

## 8. Wo der Code steht

| Was | Wo |
|---|---|
| Ereignis-ABI, Warteschlange, Tastatur-Übersetzung | `src/fenster/prozessfenster.rs` |
| Fenster-Typ, Pixel-Übernahme, Ereignis-Zuführung | `src/fenster/mod.rs` |
| Die fünf Syscalls, Rechteck, Prüfungen | `src/syscall/fenster.rs` |
| Handle-Objekt und automatisches Aufräumen | `src/syscall/handle.rs` |
| Wartegrund und Sicherheitsnetz | `src/prozess.rs`, `src/scheduler.rs` |
| User-Space-Seite | `userland/src/fenster.rs` |
| Beweis-Programm | `userland/src/bin/fenstertest.rs` |
| Tests | `tests/fenster.rs`, `tests/fenster_messung.rs` |
