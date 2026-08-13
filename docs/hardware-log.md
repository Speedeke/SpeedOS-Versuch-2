# SpeedOS — Hardware-Log (Boot-Tests auf echten Geräten)

Hier wird festgehalten, auf welcher echten Hardware SpeedOS als
Live-System (siehe [`usb-boot.md`](usb-boot.md)) getestet wurde und was
funktioniert hat. **Ehrlich dokumentieren** — ein toter Mauszeiger ist
ein erwartetes Ergebnis, kein Scheitern (USB-Legacy-Emulation ist
Glückssache; echte USB-Eingabe kommt erst später).

Getestetes Image: `speedos-live.img` (`cargo image`).

---

## Übersichts-Tabelle

| Datum | Gerät | Bootet? | Desktop? | Tastatur | Maus | Auflösung | Notiz |
|-------|-------|:-------:|:--------:|:--------:|:----:|-----------|-------|
| 2026-07-22 | Acer Aspire A515-51 (N17C4) | ✅ | ✅ | ✅ | ⚠️ | 1920×1080 | Touchpad als PS/2 erkannt, bewegt aber den Cursor nicht |

Legende: ✅ ja · ❌ nein · ⚠️ teilweise · ⬜ noch offen

---

## Vorlage pro Gerät (kopieren und ausfüllen)

> Diesen Block für jedes getestete Gerät duplizieren.

### Gerät: _Marke + Modell_

- **Datum:** _JJJJ-MM-TT_
- **Typ:** _Laptop / Desktop / Mini-PC / …_
- **Firmware:** _UEFI-Hersteller/Version, Secure Boot aus? CSM aus?_
- **Bildschirm-Auflösung (laut Diagnose):** _z. B. 1920×1080_

| Prüfpunkt | Ergebnis | Bemerkung |
|---|:---:|---|
| Firmware bootet vom Stick (UEFI) | ⬜ | |
| Aurora-Bootscreen erscheint | ⬜ | |
| Desktop startet | ⬜ | |
| PS/2-/Legacy-**Tastatur** funktioniert | ⬜ | |
| **Maus** funktioniert | ⬜ | _USB-Legacy? oft ❌_ |
| Automatische HiDPI-Skalierung passt | ⬜ | _bei hoher Auflösung_ |
| „Keine PS/2-Eingabe"-Meldung (falls zutreffend) | ⬜ | |
| Diagnose-Modus (Taste D) zeigt Hardware | ⬜ | |

**Was ging:**
_… _

**Was nicht ging:**
_… _

**Diagnose-Ausgabe (Taste D) — was wurde erkannt?**
_Bildschirm / Tastatur / Maus / Laufwerke laut Diagnose-Schirm:_
_… _

**Fotos:**
_Bildschirmfotos vom Gerät hier verlinken (in `docs/screenshots/` ablegen):_

<!-- Beispiel:
![Boot auf Laptop XY](screenshots/hw-laptopxy-desktop.jpg)
![Diagnose auf Laptop XY](screenshots/hw-laptopxy-diagnose.jpg)
-->

---

## Bisherige Ergebnisse

### Gerät: Acer Aspire A515-51 (Modell N17C4)

- **Datum:** 2026-07-22 — **erster erfolgreicher Boot auf echter Hardware** 🎉
- **Typ:** Laptop (Baujahr 2018-06)
- **Firmware:** UEFI (Acer InsydeH2O). Secure Boot AUS — dazu erst ein
  Supervisor-Passwort gesetzt (Acer-Eigenheit); F12-Boot-Menü aktiviert.
- **Bildschirm-Auflösung (laut Diagnose):** 1920×1080 (natives Panel,
  korrekt erkannt; Skala 1.0, keine HiDPI-Skalierung nötig)

| Prüfpunkt | Ergebnis | Bemerkung |
|---|:---:|---|
| Firmware bootet vom Stick (UEFI) | ✅ | |
| Aurora-Bootscreen erscheint | ✅ | |
| Desktop startet | ✅ | |
| PS/2-/Legacy-**Tastatur** funktioniert | ✅ | als PS/2 erkannt, tippt normal |
| **Maus** funktioniert | ⚠️ | als PS/2 **erkannt**, aber **kein Cursor-Movement** |
| Automatische HiDPI-Skalierung passt | n/a | 1080p = Skala 1.0 |
| „Keine PS/2-Eingabe"-Meldung | n/a | Eingabe war ja vorhanden |
| Diagnose-Modus (Taste D) zeigt Hardware | ✅ | Ausgabe unten |

**Was ging:** Boot vom Stick, Aurora-Bootscreen, Desktop, **Tastatur**,
korrekte native Auflösung **1920×1080**, Diagnose-Modus (Taste D).

**Was nicht ging:** Das **Touchpad** bewegt den Cursor nicht — obwohl es
als PS/2-Maus erkannt wird (die Init-Handshakes werden beantwortet).

**Diagnose-Ausgabe (Taste D):**
```
Bildschirm : 1920x1080 Pixel, Bgr, 4 B/Pixel
Tastatur   : PS/2 erkannt
Maus       : PS/2 erkannt
ATA        : keine Laufwerke
Dateisystem: nur RAM (keine Platte gemountet)
```

**Lead für später (Touchpad-Init):** Das Acer-Touchpad (Synaptics/ELAN)
hängt am 8042 und ACKt unsere Standard-PS/2-Maus-Init (0xF6 / IntelliMouse-
Sequenz / 0xF4), streamt danach aber KEINE Bewegungspakete. Kein
„USB-Glücksspiel", sondern ein konkreter Punkt: vermutlich braucht das
Touchpad eine andere Aktivierung, oder die IntelliMouse-Ratensequenz
bringt es aus dem Standard-Stream. Zu prüfen, wenn PS/2-Touchpads dran
sind (schwer aus der Ferne — braucht Test-Iterationen auf genau diesem
Gerät). ATA „keine Laufwerke" ist erwartbar: der interne Speicher hängt
an NVMe/AHCI, nicht am Legacy-IDE-Port, den unser ATA-Treiber abfragt.

_Foto vom Diagnose-Schirm liegt vor; als `docs/screenshots/hw-acer-a515-diagnose.jpg` ablegen (optional)._

---

## OFFEN: Die RTC-Zone auf echter Hardware (Serie 7, Teil 2)

**Status: NICHT VERIFIZIERT.** Diese Prüfung braucht einen Menschen vor dem
Gerät und ist von der Entwicklungsmaschine aus nicht durchführbar — sie steht
hier als Auftrag, nicht als Ergebnis.

### Warum es geprüft werden muss

Seit Serie 7, Teil 2 liefert `zeit::jetzt()` **UTC**, und
Zertifikats-Gültigkeitszeiträume hängen daran. Was die CMOS-Uhr einer
konkreten Maschine liefert, ist aber **nicht festgelegt**:

* Ein Windows-PC führt die RTC üblicherweise in **Lokalzeit**.
* Ein Linux-System führt sie in **UTC**.
* Der Acer A515-51 kam mit Windows — die Erwartung ist also **Lokalzeit**,
  in Deutschland also UTC+1 bzw. UTC+2 (Sommerzeit).

SpeedOS' Voreinstellung ist `zeit.rtc_zone_min = 0`, also „die RTC läuft in
UTC" (das ist auch das, was unser QEMU-Runner mit `-rtc base=utc`
herstellt). Trifft das auf einer Maschine nicht zu, geht die Uhr um den
Zonenversatz falsch — bei Zertifikaten mit Monats-Laufzeiten harmlos, aber
es ist eine Unwahrheit im Kern, und sie gehört gemessen statt vermutet.

### Wie es geprüft wird

1. Live-Stick bauen und schreiben (`cargo image`, `tools/usb_schreiben.ps1`).
2. Beim Bootscreen **D** drücken (Diagnose-Modus).
3. Im Abschnitt „Erkannte Hardware" stehen jetzt drei neue Zeilen:
   ```
   RTC roh    : TT.MM.JJJJ HH:MM:SS  (RTC-Zone +0 min)
   UTC        : TT.MM.JJJJ HH:MM:SS  (plausibel)
   Kernel-Bau : TT.MM.JJJJ
   ```
4. **„RTC roh" mit der Uhr an der Wand vergleichen:**
   * Stimmt sie mit der **Ortszeit** überein → die RTC läuft in Lokalzeit.
     Dann in *Einstellungen → Zeit* die **Zone der HARDWARE-Uhr** auf den
     eigenen Versatz setzen (Sommer: UTC+02:00). Danach muss „UTC" die
     Weltzeit zeigen.
   * Stimmt sie mit der **UTC** überein → alles richtig, nichts zu tun.
5. Das Ergebnis hier eintragen (Datum, Gerät, welcher Fall).

### Was dabei ausserdem zu sehen ist

* `Kernel-Bau` — die Grenze der Plausibilitätsprüfung. Zeigt „UTC" ein Datum
  **davor**, meldet SpeedOS `UNPLAUSIBEL!` und verweigert die
  Zertifikatsprüfung. Genau das ist auf einem Gerät mit leerer
  Pufferbatterie zu erwarten und der eigentliche Zweck der Prüfung.
* `CA-Buendel` — ob ein Vertrauensanker im Image steckt.

### Ergebnis

| Datum | Gerät | RTC läuft in | RTC-Zone gesetzt auf | Bemerkung |
|---|---|---|---|---|
| — | Acer Aspire A515-51 | *offen* | — | zu prüfen |

---

> Als Referenz, wie ein voller Erfolg in der Emulation aussieht, dienen
> die QEMU-Screendumps der Generalprobe:
> [Desktop](screenshots/live-desktop.png) ·
> [Diagnose](screenshots/live-diagnose.png) ·
> [keine PS/2-Eingabe](screenshots/live-keine-ps2.png).

---

## Serie 9, Teil 5 — der Hardware-Tag: NOCH NICHT DURCHGEFÜHRT

**Stand: offen.** Der USB-Eingabepfad ist gebaut und in QEMU bewiesen
(mit `i8042=off` ist der Desktop rein über USB bedienbar), aber auf
echter Hardware ist er **ungetestet**. Dieser Abschnitt ist die
vorbereitete Befundliste, nicht ihr Ergebnis.

Ehrliche Erwartung, damit sie nicht als Scheitern verbucht wird: Der
erste Anlauf auf echtem Blech findet typischerweise Dinge, die in QEMU
nie auftreten — Timing, mehrere Controller, **Hubs** (interne
Notebook-Geräte hängen oft an einem internen Hub, und Hubs können wir
noch nicht), Firmware-Handoff. Das ist der Normalfall.

### Vorgehen

```bash
cargo image
```

Dann `tools/usb_schreiben.ps1` als Administrator (schreibt
`speedos-live.img` roh auf den Stick — der Stick ist danach im
Explorer unsichtbar, das ist normal).

Beim Bootscreen **Taste D** drücken: Der Diagnose-Modus ist auf dem
Blech die einzige Informationsquelle, weil es dort keine serielle
Ausgabe gibt. Er zeigt seit diesem Teil **ganz oben** die USB-Lage und
unterscheidet drei Fälle, die von außen gleich aussehen.

### Befundliste (auszufüllen)

| Frage | Befund |
|---|---|
| Bootet das Image? | |
| Zeigt die Diagnose einen xHCI-Controller? | |
| Wie viele USB-Geräte werden gelistet? | |
| Steht dort „USB-Eingabe AKTIV"? | |
| Funktioniert die **eingebaute Tastatur**? | |
| Funktioniert das **Touchpad**? | |
| Funktioniert eine **externe** USB-Tastatur? | |
| Ist der Desktop bedienbar (tippen, klicken, ziehen)? | |
| Läuft der Browser? | |

### Wenn nichts kommt — in dieser Reihenfolge nachsehen

1. **„KEIN Controller gefunden"** → Der xHCI liegt nicht auf Bus 0.
   Wir rekursieren nicht über PCI-Bridges (CLAUDE.md).
2. **Controller läuft, 0 Geräte** → Wahrscheinlich ein interner **Hub**.
   Das ist die wahrscheinlichste Ursache überhaupt.
3. **Geräte da, aber „KEIN HID-Boot-Geraet"** → Das Gerät hat keine
   Boot-Subclass; es bräuchte den Report-Descriptor-Parser.
4. **Geräte da, „AKTIV", aber nichts passiert** → Transfer-Problem
   (Stall, falsches Intervall). Kein `Reset Endpoint` vorhanden.
5. **Hängt beim Booten** → BIOS-Handoff (USBLEGSUP). Er ist gebaut,
   aber von QEMU nie angefordert und damit ungetestet.

### Fotos

(hierher: Bootscreen, Diagnose-Schirm, Desktop)

### Befund 1 (behoben): zu wenige Tick-Waker-Slots — DIE Ursache der Zähigkeit

`zeit::MAX_TICK_WARTER` stand auf **8**. Wer keinen Slot bekam, lief in
diesen Zweig:

```rust
None => {
    cx.waker().wake_by_ref();   // sofort wieder einreihen
    Poll::Pending
}
```

Das ist **kein Warten, sondern ein Busy-Spin mit voller
Executor-Geschwindigkeit**. Ein einziger solcher Task frisst die CPU
und lässt alle anderen verhungern — Maus, Tastatur und Compositor
werden zäh, und das System wirkt aufgehängt.

Bis Serie 8 warteten ungefähr sechs Tasks gleichzeitig auf Ticks, also
blieb es unbemerkt. **Serie 9 und 10 haben zwei weitere hinzugefügt**
(USB-Events alle 8 ms, Audio-Mixer alle 4 ms) — damit lief die Liste
über. In QEMU fiel es nicht auf, weil dort selten alle Tasks
gleichzeitig warten; auf dem Laptop sofort.

Behoben: 32 Slots, ein Überlauf-Zähler (`tick_warter_ueberlauf()`, im
Betrieb 0) und eine einmalige serielle Meldung. **Der Spin bleibt** —
ohne Slot gibt es niemanden, der weckt, und ewig zu schlafen wäre
schlimmer. Was sich ändert: Er fällt auf.

Vier Regressionstests in `zeit::slot_tests`, darunter zwölf
gleichzeitige Warter (mit den alten 8 Slots hätten vier gespinnt).
Gegengeprüft: Auf 8 zurückgestellt wird der Test rot.

### Befund 2 (behoben): Mauszeiger ohne Maus

Ein Pfeil mitten im Bild, obwohl weder PS/2- noch USB-Maus da war —
unbeweglich, und damit vom Aussehen her nicht von einem abgestürzten
System zu unterscheiden. `ZEIGER_DA` wird jetzt an einer Stelle
gesetzt, die beide Wege abdeckt.

### Offen

Ob die Zähigkeit damit ganz weg ist, muss der nächste Lauf auf dem
Laptop zeigen. Der Slot-Überlauf war messbar die größte Einzelursache,
aber ob er die EINZIGE war, ist damit nicht bewiesen.

### Befund 3 (behoben): die Aufzählung fror den ganzen Rechner ein

**Das war die Ursache für „alles friert ein".**

`usb_task` ruft `port_wechsel_behandeln` → `geraet_aufzaehlen`, und zwar

* **innerhalb** des Controller-Locks,
* im kooperativen Executor (PID 0), also **ohne `await` dazwischen**.

Die Fristen dort standen auf **einer Sekunde je Kommando** — „großzügig
gegen Hänger" gedacht, und genau deshalb tödlich: Jede Sekunde, die
hier gespinnt wird, ist eine Sekunde, in der **kein** anderer
Kernel-Task läuft. Kein Compositor, kein Eingabe-Router, keine Maus.

Eine Aufzählung setzt rund ein Dutzend Kommandos ab. Bei einem Gerät,
das nicht antwortet, sind das **zwölf Sekunden Totalstillstand** — und
danach, beim nächsten Port-Ereignis, wieder. Das erklärt das Muster
genau: kurz bedienbar, dann zäh, dann eingefroren.

In QEMU antwortet alles sofort, deshalb war es dort unsichtbar. Auf
echter Hardware gibt es Hubs und Ports, die Ereignisse melden, ohne
dass ein brauchbares Gerät dranhängt.

**Der erste Behebungsversuch war falsch und machte alles schlimmer.**
Er kürzte die Fristen auf 50 ms. Das half dem Executor — und liess die
Aufzählung auf echter Hardware **scheitern**, weil dieselben Fristen
auch beim Booten gelten. Ohne PS/2 hatte der Rechner danach überhaupt
keine Eingabe mehr. Aus „zäh" wurde „tot".

Die Einsicht, die vorher fehlte: **Es gibt zwei Aufrufpfade mit
gegensätzlichen Anforderungen.**

* **Beim Booten** läuft noch kein Executor. Blockieren ist dort völlig
  in Ordnung; es zählt nur, dass langsame Hardware genug Zeit bekommt.
* **Zur Laufzeit** hält jede Millisekunde alles andere an. Dort zählt,
  dass es *nicht wiederholt* passiert.

Richtig behoben:

* **Fristen bleiben großzügig** (500 ms) — Hardware braucht sie.
* **Höchstens eine Aufzählung je Poll-Durchgang**; der Rest wird
  zurückgestellt, nicht verworfen.
* **Ein Port, an dem die Aufzählung scheitert, wird nicht erneut
  versucht**, bis er wirklich getrennt und neu angesteckt wurde. Damit
  kostet ein hängender Port *eine* Pause — einmal, nicht alle 8 ms.

Das ist der Punkt, an dem das Problem gar nicht erst entsteht: nicht
eine kürzere Pause, sondern keine Wiederholung.

### Befund 4: der Framebuffer war ungecacht (Write-Combining)

**Das ist der Unterschied zwischen QEMU und echtem Blech.**

In QEMU ist der „Framebuffer" ganz normaler Host-Arbeitsspeicher — ihn
zu beschreiben kostet nichts Besonderes. Auf einem echten Laptop ist er
**VRAM auf der anderen Seite des PCIe-Busses**, und die Firmware mappt
ihn üblicherweise ungecacht (UC). Dort wird *jeder einzelne*
Schreibzugriff zu einer eigenen Bus-Transaktion.

Bei 1080p sind das 8,3 MB je Vollbild. Uncached kostet das leicht 50 ms
— bei **jedem** `present()`, also bei jeder Konsolenzeile, die scrollt.
Genau das passt auf den Befund: In QEMU läuft alles, auf dem Blech
friert es beim Tippen ein. Es war nie zu wenig Rechenleistung; es war
der Weg zum Bildschirm.

Behoben: `memory::write_combining_einrichten()` stellt PAT-Eintrag 1
(bisher WT, erreichbar über PWT) auf **Write-Combining**, und
`framebuffer::init` schaltet die Seiten des vorderen Puffers darauf um
(`update_flags`, kein zweites Mapping — Aliasing mit verschiedenen
Speichertypen ist verboten). Die CPU sammelt benachbarte Schreibzugriffe
dann in 64-Byte-Bursts.

Der Back-Buffer bleibt gecachtes RAM: Dort wird *gezeichnet*, und Lesen
aus WC-Speicher wäre langsam.

`map_mmio` ist nicht betroffen — es setzt PCD **und** PWT, landet also
bei Eintrag 3 (UC). Geräteregister müssen ungecacht bleiben.

**Ehrliche Grenze:** Die PAT kann einen Bereich nicht besser machen, als
die MTRRs erlauben. Steht er dort auf UC, bleibt es bei UC. Ob es auf
dieser Maschine greift, sagt die Messung.

**Die Messung steht jetzt im Diagnose-Schirm** (Taste D):

```
Bildschirm: Vollbild-present 168 us (WC verfuegbar)
```

In QEMU 168 µs — dort war es erwartungsgemäß nie das Problem. Auf dem
Laptop ist diese Zahl die Antwort: unter 2 ms = in Ordnung, über 30 ms =
ungecacht und die Ursache des Einfrierens.

---

## Befund 5 — die MTRRs waren die andere Hälfte (August 2026)

**Die ehrliche Grenze von Befund 4 war die eigentliche Ursache.** Dort
stand: *„Die PAT kann einen Bereich nicht besser machen, als die MTRRs
erlauben. Steht er dort auf UC, bleibt es bei UC."* Genau so ist es —
und damit war die PAT-Umstellung allein wirkungslos.

### Die Regel, um die es geht

Der effektive Speichertyp einer Seite ergibt sich aus **MTRR und PAT
zusammen**, und dabei gewinnt der **restriktivere** (Intel SDM Vol. 3,
Tabelle *„Effective Memory Type Depending on MTRR and PAT"*). UC ist die
stärkste Aussage. Eine Seitentabelle, die WC sagt, während der MTRR UC
sagt, ergibt UC — der PAT-Eintrag ist gesetzt und bewirkt nichts.

Auf echter Hardware lässt die Firmware den Framebuffer-Bereich häufig
auf UC stehen. In QEMU fällt es nicht auf, weil dort ohnehin alles
schnell ist. Das ist der Grund, warum Befund 4 in QEMU 168 µs maß und
sich auf dem Laptop nichts änderte.

### Was jetzt passiert

`src/mtrr.rs` programmiert einen **variablen MTRR auf WC** über den
Framebuffer — dasselbe, was Linux früher mit `mtrr_add()` für
Grafikkarten tat. `framebuffer::init` ruft es **vor** dem PAT-Eintrag;
wer die Reihenfolge dreht, setzt wieder ein Flag ohne Wirkung.

**Die Sicherheitsschranke ist der wichtigste Teil der Datei:** Wir
greifen **nur ein, wenn der Bereich wirklich UC ist**. Sagt der MTRR WB,
ist es gewöhnlicher gecachter Speicher — genau der Fall in QEMU, wo der
„Framebuffer" schlicht Hauptspeicher ist. Ihn auf WC zu stellen machte
ihn *langsamer*, und weil MTRRs nur ausgerichtete Zweierpotenzen können
und wir deshalb nach oben überdecken, erwischte die Überdeckung
benachbarten RAM — der verlöre Cache-Kohärenz und Schreib-Reihenfolge.
Das wäre ein Fehler, den man erst Wochen später als unerklärliche
Datenverfälschung bemerkt. Wir reparieren den kaputten Fall und lassen
den gesunden in Ruhe.

Weiter gilt: bestehende Einträge werden **nie** überschrieben (nur
Register mit gelöschtem Gültig-Bit), höchstens zwei Register je Bereich,
und bei jeder unerfüllten Bedingung passiert schlicht nichts.

In QEMU meldet der Bereich tatsächlich UC (die VGA-Apertur ist MMIO,
kein RAM) — die Schranke greift also am richtigen Fall, und beide
Register werden gesetzt.

### Der zweite Fund: der Mauszeiger schrieb byteweise

`pixel_setzen_vorne` schrieb **drei einzelne Bytes je Pixel** in den
echten Framebuffer. Auf Gerätespeicher ist jeder davon eine eigene
Bus-Transaktion. Der Zeiger sind rund 1000 Pixel und wird bis zu
200-mal je Sekunde neu gezeichnet — das sind 600 000 Transaktionen pro
Sekunde für einen Mauszeiger. Jetzt ist es **ein 32-Bit-Zugriff**, wenn
ein Pixel 4 Byte breit ist. Faktor 3, an genau der Stelle, die der
Benutzer als „nicht smooth" wahrnimmt.

### Und das Werkzeug: der Wachhund (`src/wacht.rs`)

Der eigentliche Grund, warum diese Suche so lange dauerte: **Auf echter
Hardware gibt es keine serielle Ausgabe.** Ein eingefrorenes SpeedOS
zeigte das Bild von vorher — daran lässt sich nicht ablesen, woran es
lag. Jede Vermutung kostete einen kompletten Zyklus aus Bauen, Stick
schreiben, Booten, Fotografieren.

Der Wachhund läuft im Timer-Interrupt, prüft den Fortschritt des
Executors und malt bei Stillstand (3 s) einen **roten Balken** an den
oberen Bildschirmrand — mit so vielen weißen Kästchen, wie der zuletzt
erreichte Programmpunkt groß ist. Kästchen statt Schrift, weil sich das
auf einem Handyfoto abzählen lässt und keine Zeichensatz-Tabellen
braucht. Er nimmt **keinen Lock** (in genau der Lage könnte ein Lock die
Ursache sein) und meldet **einmal**, nicht im Sekundentakt.

Die Zuordnung steht seit diesem Befund auch im Boot-Schirm, damit man
sie nicht nachschlagen muss:

```
1 Executor  2 Compositor  3 Bildschirm  4 Konsole
5 Tastatur  6 Maus  7 Shell  8 USB  9 Audio
```

**Was er nicht kann, und das gehört dazu:** Er hängt am Timer. Steht die
Maschine mit ausgeschalteten Interrupts (Endlosschleife unter
`without_interrupts`, Triple Fault, CPU angehalten), läuft auch er nicht
mehr. Er fängt die häufigere Sorte: eine Schleife oder ein Warten, das
nie endet, während die Interrupts weiterlaufen.

### Der Befund-Schirm erscheint jetzt bei JEDEM Boot

Die Messung stand bisher hinter Taste D — und ausgerechnet die Tastatur
war auf dieser Maschine das Problem. **Eine Messung, die man nur mit dem
kaputten Gerät abrufen kann, ist keine.** Sie erscheint deshalb von
selbst für fünf Sekunden, mit Auflösung, `present`-Zeit, MTRR-Befund
(inklusive des Typs *vorher*), PAT-Status, den erkannten Eingabegeräten
und einer Bewertung in Worten.

### Nebenbefund, der eine Annahme widerlegt hat

Das Foto vom Laptop zeigte „keine Eingabe gefunden". Die xHCI-Aufzählung
läuft **synchron** in `usb::xhci::init()` — die Meldung stimmt also:
**auf dem Laptop hängt nichts am USB.** Eingabe kommt dort über PS/2,
und `diagnose::tastatur_vorhanden()` meldet sie fälschlich als abwesend
(der 8042-Port-Test 0xAB antwortet auf diesem EC nicht wie erwartet).

Folge: **Die gesamte USB-Arbeit aus Serie 9 ist auf dieser Maschine
wirkungslos** — sie war nie an der Zähigkeit beteiligt. Das ist der
Grund, warum die Verbesserungen an xHCI und HID dort nichts brachten,
obwohl sie in QEMU messbar waren. Wer künftig ein Hardware-Problem
sucht, prüft **zuerst**, ob der verdächtigte Treiber auf der Maschine
überhaupt läuft.

---

## Befund 6 — warum es in QEMU läuft und auf dem Blech nicht (August 2026)

Der Projektbesitzer stellte die entscheidende Frage: *„Warum klappt es in
QEMU so gut?"* Bei allen drei Fehlern dieses Tages lautet die Antwort
gleich — **QEMU erfüllt eine Annahme, die echte Hardware nicht macht.**
Ein Emulator ist ein freundlicher Gesprächspartner; er tut, was man
erwartet. Ein echter Chip tut, was in seinem Datenblatt steht.

Wichtige Korrektur der Ausgangslage: Der Laptop hat **keine
USB-Tastatur** (die eingebaute hängt über PS/2 am Embedded Controller),
aber **eine USB-Maus**. Der Befund 5 („auf dem Laptop hängt nichts am
USB") war deshalb falsch — siehe Fehler 3.

### Fehler 1 — die Maus hat NIE funktioniert

Der USB-HID-Treiber baute seine Bewegungsdaten in **PS/2-Bytes** um und
schob sie durch dieselbe Warteschlange wie eine echte PS/2-Maus. Wie
lang ein solches Paket ist, entscheidet aber `RAD_MODUS` — und den setzt
die **PS/2-Initialisierung**.

Auf einem Laptop ohne PS/2-Maus schlägt die fehl, `RAD_MODUS` bleibt
`false`, und der Maus-Task liest den **4-Byte-Strom der USB-Maus in
3er-Schritten**. Dauerhaft aus dem Takt: Der Zeiger springt, die
Resynchronisation über das Sync-Bit rastet an falschen Stellen ein.

**In QEMU gibt es eine PS/2-Maus**, ihre Erkennung gelingt, `RAD_MODUS`
wird `true` — und beide Längen passen zufällig zusammen.

Behoben durch `maus::paket_einspeisen(Paket)`: Der USB-Treiber liefert
ein **fertiges Paket** statt eines Byte-Stroms. Ein Paket trägt seine
Bedeutung selbst; es gibt keine Länge mehr, über die sich zwei Geräte
uneinig sein könnten.

> **Die Lehre, allgemein:** Ein Datenformat darf nie von einem Zustand
> abhängen, den eine *fremde* Quelle gesetzt hat. Die Wiederverwendung
> des PS/2-Pfades klang sparsam („kein zweiter Weg, an dem die Maus
> ruckeln kann") und war in Wahrheit eine versteckte Kopplung.

### Fehler 2 — die Tastatur starb, obwohl sie „ganz früher" ging

Tastatur und Maus hängen am **selben Chip** und teilen sich **einen
Datenport** (0x60). Welches Gerät ein Byte geschickt hat, steht **nicht
im Byte** — es steht im Statusregister 0x64, Bit 5 (AUX).

Beide Interrupt-Handler lasen den Datenport **blind** und schoben das
Byte in die Queue ihres eigenen Geräts; die IRQ-Nummer galt als Beweis
der Herkunft. Auf echter Hardware ist sie das nicht: Ein Embedded
Controller bedient Tastatur und Touchpad verschachtelt, und ein
Tastatur-Interrupt trifft dort regelmäßig auf ein wartendes Maus-Byte.
Dann wandert ein Maus-Byte in den Scancode-Strom, und der Scancode, der
gemeint war, verschwindet. Im schlimmsten Fall bleibt ein Byte liegen,
das niemand abholt — dann schickt der 8042 **gar keinen Interrupt mehr**
und die Eingabe ist tot, während der Rest weiterläuft.

**In QEMU gehört zu jedem Interrupt genau ein passendes Byte.** Die
Verschachtelung tritt nie auf.

Behoben durch `interrupts::ps2_bytes_verteilen`: Beide Handler benutzen
denselben Pfad, lesen **immer erst den Status** und geben das Byte
dorthin, wohin der Controller es adressiert hat — geleert wird in einer
gedeckelten Schleife, damit nie eines liegenbleibt. Die Weiche selbst
ist eine reine Funktion (`ps2_ziel`) mit zwei Regressionstests.

### Fehler 3 — die Boot-Meldung log

Ein gerade gestarteter xHCI-Controller weiß noch nicht, was an ihm
hängt; die Wurzel-Hubs brauchen einen Moment. Wir sahen sofort nach,
fanden leere Ports und meldeten „keine Eingabe gefunden" — die USB-Maus
wurde erst später durch einen Port-Wechsel gefunden. **Die Meldung war
falsch, nicht der Treiber.** Jetzt wird bis zu 200 ms gewartet, mit
Abbruch beim ersten belegten Port.

Diese falsche Meldung hatte mich in Befund 5 zu der Fehlannahme geführt,
auf dem Laptop hänge nichts am USB — und damit die Suche in die falsche
Richtung geschickt.

### Fehler 4 — meine eigene Optimierung malte Zeigerspuren

Der Versuch, bei schneller Bewegung Zwischenpositionen des Mauszeigers
auszulassen, hinterließ **Pfade aus stehengebliebenen Pfeilen**. Grund:
Es gibt **zwei** Stellen, die den Zeiger malen — der Compositor
zeichnet ihn nach jedem Frame an `position()` neu, der Maus-Task löscht
ihn an der Stelle, die *er* sich gemerkt hat. Beide stimmen nur überein,
solange jede Position gezeichnet wird.

> **Merksatz:** Es darf nur eine Stelle geben, die weiß, wo der Zeiger
> steht. Solange es zwei sind, ist Auslassen kein Optimieren, sondern
> ein Fehler.

Die Optimierung ist entfernt.

### Was daraus als Arbeitsregel bleibt

**QEMU beweist, dass der Code *eine* Umgebung bedient — nicht, dass er
richtig ist.** Wo eine Annahme über Hardware im Code steckt (Paketlänge,
Herkunft eines Bytes, Zeitpunkt einer Meldung), prüft QEMU sie nicht,
weil es sie erfüllt. Solche Stellen gehören ausdrücklich benannt und mit
reinen Funktionen abgesichert, die man ohne Hardware testen kann — genau
das ist mit `ps2_ziel` und `maus_paket` jetzt geschehen.
