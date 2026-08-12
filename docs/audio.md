# Audio — Treiberwahl, Umfang, Naht

*Serie 10, Teil 1 — Entwurf. Geschrieben VOR dem Code.*

---

## 0. Warum Audio und nicht Video

Aus der Serie-9-Bestandsaufnahme: **Video scheitert an der
Dekodierung.** Ein H.264- oder VP9-Dekoder ist Monatsarbeit, es gibt
keinen brauchbaren `no_std`-Weg, und SIMD ist bei uns abgeschaltet
(`-sse,+soft-float`, wegen des Kontext-Wechsels). Ein Software-Dekoder
ohne SIMD schafft kein Video in Echtzeit — das wäre ein Projekt, das
am Ende nicht funktioniert.

**Audio ist erreichbar**, weil unkomprimiertes PCM keine Dekodierung
braucht. Eine WAV-Datei ist ein Header und dahinter die Samples; der
„Dekoder" sind zwei Zeilen. Der Aufwand liegt vollständig im Treiber
und in der Zeitsteuerung — beides Dinge, die dieses Projekt kann.

---

## 1. Die Treiberwahl: **Intel HDA**

**Empfehlung: HDA. Deutlich, nicht knapp.**

### Die Alternativen

| | AC97 | Intel HDA |
|---|---|---|
| Auf echter Hardware | **tot** (letzte Chipsätze ~2006) | **Standard**, seit ~2004 überall |
| In QEMU | `-device AC97` | `-device intel-hda` + `-device hda-duplex` |
| Registerfläche | klein, Port-I/O | größer, MMIO |
| Kommandoweg | direkte Register | **CORB/RIRB-Ringe** |
| Ausgabepfad finden | fest verdrahtet | **Widget-Graph durchlaufen** |
| DMA | BDL | BDL (praktisch gleich) |
| Aufwand | ~1 Tag | ~3–4 Tage |

### Warum trotzdem HDA

Der Aufwandsunterschied ist real, und er sitzt an genau zwei Stellen
(CORB/RIRB und der Widget-Graph). Alles andere — BDL, Streams,
Position lesen — ist bei beiden praktisch identisch.

Dagegen steht das Entscheidende: **Dieses Projekt hat gerade drei
Prompts in USB investiert, und zwar ausdrücklich, weil es auf echter
Hardware zählt** (docs/xhci.md §0: „Auf echter Hardware gibt es keine
PS/2-Tastatur"). Mit derselben Begründung fällt die Audio-Entscheidung:
Ein AC97-Treiber wäre auf dem Acer Aspire A515-51 **niemals** aktiv. Er
wäre ein Treiber, der nur in QEMU existiert — genau die Sorte Arbeit,
die die Bestandsaufnahme als Sackgasse benannt hat.

Zweitens ist der Mehraufwand **zuschneidbar**, und zwar ehrlich:

* **CORB/RIRB** kann man umgehen. HDA hat zusätzlich das
  *Immediate Command Interface* (`ICW`/`IRR`/`ICS`, Register 0x60–0x68):
  ein Verb hinein, eine Antwort heraus, ohne Ringe. Es ist für genau
  diesen Fall gedacht — wenige Kommandos beim Initialisieren — und
  spart die halbe Komplexität. **Der Haken, ehrlich notiert:** Nicht
  jede Hardware implementiert es zuverlässig; wenn es auf dem Laptop
  klemmt, ist CORB/RIRB der Nachbau, und das steht dann in
  hardware-log.md.
* **Der Widget-Graph** muss nicht vollständig durchlaufen werden. Wir
  suchen *einen* Weg von einem Ausgabe-Pin zu einem DAC — nicht alle,
  nicht den besten. Siehe §3.

### Was das kostet, wenn es schiefgeht

Auf echter Hardware ist HDA deutlich zickiger als in QEMU: mehrere
Codecs, stumme Pins, fehlende Amp-Freischaltung, Kopfhörer-Erkennung.
Das ist eingeplant (§6) und wird protokolliert, nicht wegdiskutiert.

---

## 2. Der Umfang dieses Schrittes

Umgesetzt wird:

* Controller finden (PCI-Klasse `0x04` / Unterklasse `0x03`)
* MMIO **ungecacht** mappen (`memory::map_mmio`, seit Serie 9 da)
* Controller-Reset, Interrupts aus (wir pollen)
* Codecs finden (`STATESTS`)
* **Einen** Ausgabepfad konfigurieren: Pin → DAC
* BDL + Ringpuffer im DMA-Speicher
* Stream starten/stoppen, Position lesen
* Shell: `ton [hz] [ms]` — **der Meilenstein**

**Nicht** umgesetzt:

* Eingabe (Mikrofon), Multi-Stream, S/PDIF, HDMI-Audio
* Kopfhörer-Erkennung (Jack Presence) und Umschalten
* Sample-Raten außer 48 kHz, Formate außer 16 Bit Stereo
* Power-Management der Widgets über das Nötigste hinaus
* MSI-Interrupts — gepollt, wie bei xHCI und aus demselben Grund

---

## 3. Der Ablauf

### Schritt 1 — Controller

PCI-Klasse `0x04` (Multimedia), Unterklasse `0x03` (HDA). Wieder über
`pci::finde_klasse` — kein Vendor-Rätselraten, dieselbe Regel wie bei
xHCI. Bus Master **muss** an, sonst gibt es keinen DMA.

### Schritt 2 — Reset

`GCTL.CRST` (Bit 0) auf 0, warten bis es 0 ist, dann auf 1, warten bis
es 1 ist. **Danach mindestens 521 µs warten** — die Spezifikation
verlangt das ausdrücklich, damit die Codecs sich am Link anmelden
können. Wer sofort weiterliest, findet keine Codecs und hält den
Controller für kaputt.

### Schritt 3 — Codecs finden

`STATESTS` (Register 0x0E) hat ein Bit je Link-Adresse (0..14). Jedes
gesetzte Bit ist ein Codec. In QEMU ist es genau einer; auf echter
Hardware sind zwei normal (Analog + HDMI), und **der erste ist nicht
immer der richtige**.

### Schritt 4 — Verbs absetzen

Ein HDA-Kommando („Verb") ist 32 Bit: Codec-Adresse (4), Node-ID (8),
Verb (12 oder 4) und Nutzlast. Über das Immediate Command Interface:
`ICW` schreiben, auf `ICS.Busy` warten, `IRR` lesen — mit Frist, wie
überall.

### Schritt 5 — Den Ausgabepfad finden

Das ist der Teil, der HDA von AC97 unterscheidet, und hier wird
**bewusst zugeschnitten**:

1. Vom Root-Node (0) die Function Groups holen
   (`GET_PARAMETER SUBORDINATE_NODE_COUNT`).
2. Die erste **Audio Function Group** nehmen (Typ 0x01).
3. Deren Kinder durchgehen und die **Pin Complexes** (Typ 0x04) finden.
4. Für jeden Pin die *Configuration Default* lesen: Wir wollen einen
   mit „Jack" oder „Fixed" und Device `Line Out` (0x0) oder
   `Speaker` (0x1) — und **nicht** „No Physical Connection".
5. Über die *Connection List* des Pins rückwärts zu einem **Audio
   Output (DAC)** (Typ 0x00) laufen — höchstens zwei Ebenen tief.

**Zwei Ebenen und nicht beliebig viele**, weil ein echter Graph
Mixer, Selektoren und Schleifen enthält. Ein vollständiger
Graph-Durchlauf mit Zyklenschutz ist die richtige Lösung — und ein
eigenes Vorhaben. Zwei Ebenen decken Pin→DAC und Pin→Mixer→DAC ab, und
das ist der Normalfall.

Wenn nichts gefunden wird, wird das **gemeldet und nicht geraten**.

### Schritt 6 — Pin und DAC scharf schalten

* Pin: `SET_PIN_WIDGET_CONTROL` mit Output-Enable (Bit 6)
* Pin + DAC: `SET_AMPLIFIER_GAIN_MUTE` — **Stummschaltung lösen und
  Verstärkung setzen**. Das ist der häufigste Grund, warum auf echter
  Hardware alles läuft und nichts zu hören ist.
* DAC: `SET_CONVERTER_FORMAT` (48 kHz, 16 Bit, 2 Kanäle) und
  `SET_CONVERTER_STREAM_CHANNEL` (unsere Stream-Nummer, Kanal 0)

### Schritt 7 — BDL und Ringpuffer

Die **Buffer Descriptor List** ist ein Array aus Einträgen à 16 Byte
(Adresse 64 Bit, Länge 32 Bit, Flags 32 Bit). Sie muss
**128-Byte-ausgerichtet** sein, mindestens zwei Einträge haben, und die
Gesamtlänge kommt nach `CBL`.

**Der Ringpuffer läuft von selbst im Kreis** — der Controller springt
nach dem letzten BDL-Eintrag zum ersten zurück. Das ist der Unterschied
zu den xHCI-Ringen: Es gibt kein Link-TRB und kein Cycle-Bit, die
Hardware wiederholt einfach.

### Schritt 8 — Starten und Position lesen

`SDCTL.RUN` setzen. `SDLPIB` (Link Position in Buffer) sagt, wo der
Controller gerade liest — **das ist die einzige Uhr, die zählt**: Wer
nachfüllt, ohne sie zu lesen, überschreibt entweder Daten, die noch
nicht gespielt sind, oder lässt Lücken.

---

## 4. Die Naht (Aufgabe 3)

Wie bei `BlockDevice`, `NetzGeraet` und `usb::geraet`:

```
  hda  --[implementiert]-->  AudioGeraet  <--[benutzt]--  Mixer
                                                            ^
                                          Syscall / ton / spielen
```

Der **Mixer** rechnet mehrere Quellen zusammen (Lautstärke je Quelle +
Gesamt). Die wichtigste Regel dort ist die Übersteuerung: Zwei Quellen
mit je 80 % Pegel ergeben 160 % — und ein `i16`, der überläuft, klingt
nicht leise, sondern wie ein Knacken bei voller Lautstärke. **Es wird
geklemmt, nicht gewrappt**, und genau das wird getestet.

---

## 5. Was ohne Hardware getestet wird

* **WAV-Parser** gegen kaputte Dateien (abgeschnitten, Länge lügt,
  fehlende Chunks, absurde Kanalzahl) — dieselbe Haltung wie beim
  USB-Deskriptor: fremde Datei, feindlich behandeln.
* **Mixer-Mathematik**: Klemmen statt Überlaufen, Lautstärke ganzzahlig
  (kein Fließkomma — soft-float).
* **Ringpuffer-Positionslogik**: Wie viel Platz ist frei, wo wird
  geschrieben, was passiert am Umlauf.
* Die **Verb-Kodierung** (Codec/Node/Verb/Nutzlast in 32 Bit).

Was nur in QEMU geht: dass wirklich ein Ton herauskommt.
Was nur auf echter Hardware geht: siehe §6.

---

## 6. Ehrliche Erwartung für den Hardware-Tag

HDA ist auf echtem Blech **deutlich zickiger** als in QEMU. Wahr-
scheinliche Befunde, nach Häufigkeit:

1. **Mehrere Codecs** — der erste ist der HDMI-Codec, der Analog-Codec
   ist der zweite. Ton geht dann „irgendwohin".
2. **Amp stumm** — Pin oder DAC haben eine eigene Stummschaltung, die
   gelöst werden muss. Alles läuft, nichts ist zu hören.
3. **Immediate Command Interface klemmt** — dann muss CORB/RIRB nach.
4. **Der gefundene Pin ist der Kopfhörerausgang**, und es steckt nichts
   drin. Ohne Jack-Erkennung merkt man das nicht.
5. **Position steht still** — BDL falsch ausgerichtet oder Bus Master
   nicht gesetzt.

Das ist die Befundliste, nicht die Fehlerliste. Sie wird abgearbeitet,
nicht als Scheitern verbucht.
