# ÜBERGABE — Stand der Hardware-Fehlersuche (13. August 2026)

> Dieses Dokument ist für die Fortsetzung an einem anderen Gerät
> geschrieben. Es enthält **alles**, was man braucht, um ohne den
> bisherigen Chatverlauf weiterzuarbeiten: das Problem, was schon
> gefunden und behoben wurde, was der nächste Schritt ist, und wie man
> ihn ausführt.

---

## 1. Das Problem in einem Absatz

SpeedOS läuft in QEMU einwandfrei, auf echter Hardware nicht. Testgerät
ist ein **Acer Aspire A515-51 (1080p)**, gebootet vom USB-Stick über
`speedos-live.img`. Symptome: alles ist zäh, der Mauszeiger ruckelt, und
beim Tippen friert das System ein.

**Wichtige Gerätefakten** (mehrfach falsch angenommen, deshalb hier
zuoberst):

* Die **Tastatur ist die eingebaute** und hängt über **PS/2** am
  Embedded Controller — es ist **keine USB-Tastatur** angeschlossen.
* Die **Maus ist eine USB-Maus**.
* Die Maus hat **noch nie** richtig funktioniert. Die Tastatur ging
  **früher** und wurde später kaputt.

---

## 2. Was bereits gefunden und behoben wurde

Alles davon ist committet, getestet (611 Tests grün) und im Image.

### 2.1 Der Framebuffer war ungecacht — MTRR statt nur PAT
*(Commit „Der Bildschirm war die Ursache", `src/mtrr.rs`)*

Der effektive Speichertyp einer Seite ergibt sich aus **MTRR und PAT
zusammen**, und dabei gewinnt der **restriktivere**. Stand der
Framebuffer im MTRR auf UC, konnte die Seitentabelle ihn nicht auf
Write-Combining heben — der PAT-Eintrag war gesetzt und wirkungslos.
Jeder Schreibzugriff war dann eine eigene Bus-Transaktion.

`framebuffer::init` ruft jetzt **erst** `mtrr::framebuffer_beschleunigen`
(physikalische Adresse!), **dann** `memory::bereich_write_combining`.
Reihenfolge ist Pflicht.

**Sicherheitsschranke:** Eingegriffen wird **nur bei UC**. Bei WB/WT
passiert nichts — MTRRs können nur ausgerichtete Zweierpotenzen, wir
überdecken also nach oben, und auf gecachtem Speicher erwischte das
benachbarten RAM und nähme ihm Cache-Kohärenz. Bestehende Register
werden nie überschrieben, höchstens zwei je Bereich.

### 2.2 Der Mauszeiger schrieb byteweise
`pixel_setzen_vorne` schrieb drei einzelne Bytes je Pixel in den echten
Framebuffer. Der Zeiger sind ~1000 Pixel bei bis zu 200 Hz — 600 000
Bus-Transaktionen je Sekunde. Jetzt **ein 32-Bit-Zugriff** bei 4 Byte je
Pixel.

### 2.3 Die USB-Maus lief dauerhaft aus dem Takt — DER Mausfehler
*(Commit „Drei Annahmen, die nur QEMU erfüllt hat")*

Der USB-HID-Treiber baute seine Bewegungsdaten in **PS/2-Bytes** um und
schob sie durch dieselbe Warteschlange wie eine PS/2-Maus. Wie lang ein
PS/2-Paket ist, entscheidet aber `RAD_MODUS` — gesetzt von der
**PS/2-Initialisierung**. Ohne PS/2-Maus schlägt die fehl, `RAD_MODUS`
bleibt `false`, und der Maus-Task las den **4-Byte-Strom in
3er-Schritten**.

In QEMU *gibt* es eine PS/2-Maus, die Erkennung gelingt, beide Längen
passen zufällig zusammen — deshalb fiel es nie auf.

Behoben: `maus::paket_einspeisen(Paket)`; `usb::hid::maus_paket` liefert
ein fertiges Paket statt Bytes. **Regel: Ein Datenformat darf nie von
einem Zustand abhängen, den eine fremde Quelle gesetzt hat.**

### 2.4 Die 8042-Interrupts lasen blind — DER Tastaturfehler
Tastatur und Maus teilen sich **einen Datenport** (0x60). Wem ein Byte
gehört, steht **nicht im Byte**, sondern im Statusregister 0x64, **Bit 5
(AUX)**. Beide Handler lasen blind und benutzten die IRQ-Nummer als
Herkunftsbeweis. Ein Embedded Controller bedient beide Geräte
verschachtelt — dann landet ein Maus-Byte im Scancode-Strom, und im
schlimmsten Fall bleibt ein Byte liegen, das niemand abholt: Danach
schickt der 8042 **gar keinen Interrupt mehr**.

Behoben: `interrupts::ps2_bytes_verteilen` — beide Handler benutzen
denselben Pfad, lesen **immer erst den Status** und leeren in einer
gedeckelten Schleife (`PS2_MAX_JE_IRQ = 16`, damit ein defekter
Controller den Handler nicht endlos drehen lässt). Die Weiche ist eine
reine Funktion `ps2_ziel(status)` mit zwei Regressionstests.

### 2.5 Die Boot-Meldung log
Ein gerade gestarteter xHCI-Controller weiß noch nicht, was an ihm
hängt. Wir sahen sofort nach und meldeten „keine Eingabe gefunden",
obwohl eine USB-Maus steckte. **Diese falsche Meldung hat die Suche
zwischenzeitlich in die komplett falsche Richtung geschickt.** Jetzt
wird bis 200 ms gewartet, mit Abbruch beim ersten belegten Port.

### 2.6 Eigener Fehler, zurückgenommen
Ein Versuch, Zwischenpositionen des Mauszeigers auszulassen, hinterließ
**Pfade aus stehengebliebenen Pfeilen** (im Foto des Projektbesitzers
sichtbar). Grund: Es gibt **zwei** Stellen, die den Zeiger malen — der
Compositor an `position()`, der Maus-Task an seiner gemerkten Stelle.
**Merksatz: Es darf nur eine Stelle geben, die weiß, wo der Zeiger
steht.** Entfernt.

---

## 3. Die Werkzeuge, die dabei entstanden sind

Sie sind der eigentliche Fortschritt: Ohne sie war jede Vermutung ein
kompletter Zyklus aus Bauen, Stick schreiben, Booten, Fotografieren.

### 3.1 Der Befund-Schirm (`framebuffer::befund_zeigen`)
Erscheint bei **jedem Boot** für 5 Sekunden — nicht hinter Taste D. Eine
Messung, die man nur mit dem kaputten Gerät abrufen kann, ist keine.
Zeigt: Auflösung, `present`-Zeit in µs, MTRR-Befund samt Typ *vorher*,
PAT-Status, erkannte Eingabegeräte, eine Bewertung in Worten und die
Wachhund-Legende.

**Bewertung der present-Zeit:** unter 15 ms in Ordnung, über 30 ms =
ungecacht und die Ursache des Einfrierens.

### 3.2 Der Wachhund (`src/wacht.rs`)
Läuft im Timer-Interrupt, prüft den Herzschlag des Executors und malt
bei **3 s Stillstand** Balken an den oberen Bildschirmrand:

* **Rote Reihe** = grober Programmpunkt, als Anzahl weißer Kästchen:
  `1 Executor · 2 Compositor · 3 Bildschirm · 4 Konsole · 5 Tastatur ·
  6 Maus · 7 Shell · 8 USB · 9 Audio · 10 Dateisystem · 11 Netz`
* **Blaue Reihe** = laufende Nummer des Kernel-Tasks, in dem es hängt
  (neu, siehe Abschnitt 4).

Er nimmt **keinen Lock** (in genau der Lage könnte ein Lock die Ursache
sein) und meldet **einmal**, nicht im Sekundentakt. In QEMU steht der
Name des Tasks zusätzlich auf der seriellen Ausgabe.

**Grenze:** Er hängt am Timer. Bei ausgeschalteten Interrupts, Triple
Fault oder angehaltener CPU bleibt er stumm.

---

## 4. WO ES GERADE STEHT — der nächste Schritt

**Letzter Stand vom Gerät: der Wachhund zeigte EIN Kästchen.**

Das heißt: Programmpunkt 1 = **Executor**. Übersetzt: Das System stand
länger als 3 Sekunden in **irgendeinem Kernel-Task, der keine eigene
Wegmarke gesetzt hat**. Das grenzt ein, benennt aber nicht.

### Was daraufhin gebaut wurde (fertig, committet, gebaut)

Der Wachhund nennt jetzt den **Task**:

* `wacht::task_setzen(nummer, name)` — der Executor meldet vor **jedem**
  Poll, welchen Task er anfasst (`src/task/executor.rs`,
  `task_pollen`).
* Der Name wird in einen **festen 24-Byte-Puffer kopiert**, nicht per
  Zeiger gemerkt: Task-Namen sind `String`s und ein Task kann enden —
  der Wachhund läuft im Interrupt und läse sonst freigegebenen Speicher.
* Ausgabe: serieller Name + **zweite (blaue) Kästchenreihe** mit der
  Task-Nummer.

### Der nächste konkrete Handgriff

1. Image bauen und auf den Stick schreiben:
   ```
   cargo image
   powershell -ExecutionPolicy Bypass -File tools/usb_schreiben.ps1
   ```
2. Auf dem Laptop booten, den **Befund-Schirm fotografieren** (5 s, ganz
   am Anfang) — daraus kommt die `present`-Zeit und der MTRR-Befund.
3. Das Einfrieren provozieren (tippen) und den **Balken fotografieren**:
   rote Kästchen zählen **und** blaue Kästchen zählen.
4. Die blaue Zahl ist die Task-Nummer. Die Spawn-Reihenfolge steht in
   `src/main.rs` ab ca. Zeile 322:
   `Eingabe-Router, …, Konsolen-Cursor, Log-Schreiber, PS/2-Maus,
   Netz-Dispatch, USB-Events, Audio-Mixer, …`
   (Task-IDs werden fortlaufend vergeben; die Nummer ist die TaskId.)

### Die Verdachtsliste, in dieser Reihenfolge

1. **USB-Events (`usb::xhci::usb_task`)** — der stärkste Verdacht. Er
   arbeitet **synchron im Executor** mit Fristen von 500 ms je
   Control-Transfer. Eine fehlschlagende Aufzählung summiert mehrere
   davon zu einer mehrsekündigen Pause, in der **nichts** anderes läuft
   — Maus und Tastatur eingeschlossen. Das passt exakt auf „ein
   Kästchen" (der Task hat keine eigene Wegmarke).
   * **ACHTUNG, schon einmal falsch gemacht:** Die Fristen wurden
     einmal pauschal auf 50 ms gesenkt — dadurch schlug die Aufzählung
     beim Boot fehl und es gab **gar keine Eingabe mehr**. Der
     Port-RESET braucht legitim ~100 ms und mehr. Falls hier angesetzt
     wird: Reset und Datentransfers **getrennt** behandeln, nicht
     pauschal kürzen.
   * Der saubere Weg wäre, die Aufzählung **asynchron** zu machen
     (Zustandsautomat mit `await` zwischen den Schritten), statt sie in
     `mit_controller(...)` synchron durchlaufen zu lassen.
2. **Netz-Dispatch** — auf echter Hardware gibt es kein virtio-net; zu
   prüfen, ob der Task dann trotzdem etwas mit Fristen tut.
3. **Log-Schreiber** — schreibt sekündlich nach `/platte`. Auf dem
   Live-Stick gibt es keine Datenplatte; zu prüfen, ob Fehlerpfade dort
   in Timeouts laufen.

### Falls der Balken NICHT erscheint

Dann läuft der Executor weiter (Herzschlag tickt), und es ist kein
Stillstand, sondern eine **Überlastung** — dann ist die `present`-Zeit
aus dem Befund-Schirm die entscheidende Zahl.

---

## 5. Arbeitsregeln, die sich in dieser Sitzung bewährt haben

* **QEMU beweist, dass der Code *eine* Umgebung bedient — nicht, dass er
  richtig ist.** Wo eine Annahme über Hardware im Code steckt
  (Paketlänge, Herkunft eines Bytes, Zeitpunkt einer Meldung), prüft
  QEMU sie nicht, weil es sie erfüllt.
* **Wer ein Hardware-Problem sucht, prüft zuerst, ob der verdächtigte
  Treiber auf der Maschine überhaupt läuft.**
* **Nicht raten, messen.** Jede Vermutung kostet auf echter Hardware
  einen kompletten Zyklus. Erst das Werkzeug bauen, das die Antwort
  liefert.
* **Wer ein Kriterium reißen sieht, sucht zuerst den eigenen Fehler.**
* Änderungen immer **direkt** mit Edit/Write, nie über Python-Skripte
  (Quoting bricht, und man sieht nicht, was sich wirklich ändert).

---

## 6. Zustand des Repositories

* Branch `serie6-teil3-scheduler`, alles committet.
* `cargo build`, `cargo clippy --all-targets` sauber.
* **611 Tests grün** (`cargo test`; braucht laufenden
  `python tools/tls_testserver.py` für `netz_https`/`netz_klient`,
  sonst schlagen die fehl — das ist kein Regressionsfehler).
* `speedos-live.img` ist gebaut.
* Die vollständige Befund-Historie steht in `docs/hardware-log.md`
  (Befunde 1–6), die bekannten Grenzen in `docs/grenzen.md`.
