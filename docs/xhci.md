# xHCI — der USB-3-Hostcontroller

*Serie 9, Teil 3 — Entwurf. Geschrieben VOR dem Code.*

Dieses Dokument beschreibt den Ablauf, der umgesetzt wird, und die
Stellen, an denen man dabei scheitert. Es steht vor dem Treiber aus
demselben Grund wie `docs/tcp-scope.md` und `docs/browser-v1.md`: Bei
einem Vorhaben dieser Größe ist die Frage „was genau bauen wir?"
schwerer als der Code, und eine Antwort, die erst hinterher
aufgeschrieben wird, ist keine Entscheidung, sondern ein Protokoll.

---

## 0. Warum überhaupt, und warum jetzt

Aus der Serie-9-Bestandsaufnahme: **Auf echter Hardware gibt es keine
PS/2-Tastatur.** SpeedOS bootet auf dem Acer Aspire A515-51
(docs/hardware-log.md), zeigt den Desktop — und ist nicht bedienbar.
Der Browser aus Serie 8, der Explorer, die Shell: alles da, alles
unerreichbar.

USB ist damit das Einzige, was darüber entscheidet, ob SpeedOS ein
System ist, das man *benutzen* kann. Das ist ein anderer Rang als
„noch ein Feature".

**xHCI und nicht EHCI/UHCI/OHCI**, obwohl xHCI die größte Spezifikation
der vier ist. Der Grund ist nicht Modernität, sondern Verfügbarkeit:
Auf Rechnern der letzten zehn Jahre ist xHCI der einzige Controller,
der überhaupt vorhanden ist — die alten Controller sind verschwunden,
und die USB-2-Anschlüsse hängen am xHCI mit. Einen UHCI-Treiber zu
bauen hieße, für Hardware zu bauen, die wir nicht testen können.

---

## 1. Der Zuschnitt dieses Schrittes

**Dieser Schritt endet BEWUSST vor dem ersten Gerät.**

Umgesetzt wird:

* Controller finden (PCI-Klasse `0x0C` / Unterklasse `0x03` /
  Prog-IF `0x30`)
* MMIO mappen — **ungecacht**
* Capability-Register lesen und dekodieren
* BIOS-Handoff (USBLEGSUP), falls vorhanden
* Controller-Reset
* DCBAA, Command Ring, Event Ring (mit ERST), Scratchpad
* Run/Stop setzen
* Event Ring auslesen, Port-Status-Änderungen protokollieren
* Shell-Befehl `usb`

**Nicht** umgesetzt (das sind die Prompts danach):

* Slots aktivieren, Adressen vergeben, Deskriptoren lesen
* Übertragungen jeder Art (Control, Interrupt, Bulk)
* HID, also Tastatur und Maus
* MSI/MSI-X-Interrupts (siehe §7)
* USB-Hubs, isochrone Übertragungen, USB-3-Streams

Das Ziel dieses Schrittes in einem Satz: **Der Controller läuft, und
das Ein- und Ausstecken eines Geräts erzeugt ein Event, das wir
protokollieren.**

---

## 2. Was der Treiber vom System braucht — und was ihm fehlt

### 2.1 Die MMIO-Lücke: `NO_CACHE` gibt es nicht

`memory::map_page_zu` mappt heute mit `PRESENT | WRITABLE`. Für den
Framebuffer geht das (er wird nur geschrieben, und ein bisschen
Cache schadet dort nicht). **Für Geräteregister ist es falsch.**

Ein xHCI-Treiber wartet ständig darauf, dass die Hardware ein Bit
umlegt — `USBSTS.CNR`, `USBCMD.HCRST`, `PORTSC.CSC`. Liest die CPU
diese Adresse aus dem Cache, sieht der Treiber den alten Wert für
immer und läuft in jeden Timeout. Das ist kein sporadischer Fehler,
sondern ein sicherer.

Deshalb bekommt `memory` eine zweite Funktion, `map_mmio`, mit
`PRESENT | WRITABLE | NO_CACHE | WRITE_THROUGH`. Sie ist der einzige
Weg, auf dem dieser Treiber Register mappt.

Zusätzlich wird jeder Registerzugriff über `read_volatile` /
`write_volatile` geführt — sonst darf der Compiler eine Schleife über
ein Statusregister zu einem einzigen Lesen zusammenfassen.

### 2.2 DMA-Speicher

Alle Ringe und Tabellen liest die **Hardware**, nicht die CPU. Sie
müssen also physisch zusammenhängend sein und ihre PHYSISCHE Adresse
muss bekannt sein. Beides kann das System schon:
`memory::allocate_pages(n)` liefert virtuell UND physisch
zusammenhängende Seiten (gebaut für genau diesen Fall, Serie 5), und
`memory::uebersetzen` liefert die physische Adresse.

**Der Bitmap-Allocator ist hier keine Bequemlichkeit, sondern die
Voraussetzung** — eine Free-List könnte physische Kontiguität nicht
zusagen (siehe den Eintrag in CLAUDE.md).

### 2.3 Zeit

Jeder Warteschritt bekommt eine Frist über `zeit::us_seit_boot()`.
**Es gibt keine Schleife ohne Ausstieg.** Dieselbe Regel wie beim
ATA-Treiber: Auf echter Hardware ist „hängt beim Booten" der teuerste
aller Fehler, weil er keine Meldung hinterlässt.

---

## 3. Der Ablauf, Schritt für Schritt

Die Spezifikation beschreibt das in Abschnitt 4.2 („Host Controller
Initialization"). Hier steht die Fassung, die wir umsetzen.

### Schritt 1 — Controller finden

PCI-Klasse `0x0C` (Serial Bus), Unterklasse `0x03` (USB), Prog-IF
`0x30` (xHCI). Die bestehende `pci::finde` sucht nach Vendor/Device;
sie bekommt ein Gegenstück `pci::finde_klasse`. Nach Vendor zu suchen
wäre hier falsch — es gibt Dutzende xHCI-Hersteller, und genau das ist
der Sinn einer Klassen-Kennung.

BAR0 ist ein Speicher-BAR und oft 64-bittig (belegt dann BAR0+BAR1).
`pci::Bar::Speicher { basis, bit64 }` liefert das schon dekodiert.

Im PCI-Command-Register müssen **Memory Space** (Bit 1) und **Bus
Master** (Bit 2) gesetzt werden. Ohne Bus Master kann der Controller
nicht per DMA auf unsere Ringe zugreifen — er läuft dann scheinbar an
und tut nie etwas.

### Schritt 2 — MMIO mappen

Der Registerbereich zerfällt in vier Blöcke, deren Lage man erst
kennt, wenn man den ersten gelesen hat:

```
  BAR0 + 0            Capability-Register  (CAPLENGTH … HCCPARAMS2)
  BAR0 + CAPLENGTH    Operational-Register (USBCMD, USBSTS, … PORTSC[])
  BAR0 + RTSOFF       Runtime-Register     (Interrupter 0..n)
  BAR0 + DBOFF        Doorbell-Array
```

`CAPLENGTH`, `RTSOFF` und `DBOFF` stehen in den Capability-Registern.
Wir mappen großzügig (64 KiB) statt genau — der Bereich ist klein, und
eine zu knappe Rechnung ist ein Page Fault beim ersten Doorbell.

### Schritt 3 — BIOS-Handoff (USBLEGSUP)

**Die Falle, die es nur auf echter Hardware gibt.** Die Firmware
benutzt USB selbst (für die Boot-Tastatur) und übergibt den Controller
in einem Zustand, in dem sie ihn noch besitzt. Schreibt man dann in
die Register, kämpfen zwei Treiber um dasselbe Gerät: Symptome sind
sporadische Resets, verschwindende Ports und ein SMI-Sturm.

Der Weg steht in den *Extended Capabilities*, die über
`HCCPARAMS1.xECP` verkettet sind. Gesucht wird Capability-ID 1 (USB
Legacy Support):

1. `HC OS Owned Semaphore` (Bit 24) setzen
2. warten, bis `HC BIOS Owned Semaphore` (Bit 16) fällt — **mit
   Frist**
3. läuft die Frist ab: Bit 16 selbst löschen und weitermachen. Das ist
   grob, aber besser als aufzugeben — manche Firmware setzt das Bit
   nie zurück.
4. Zusätzlich im `USBLEGCTLSTS` alle SMI-Freigaben abschalten, sonst
   löst jeder Portwechsel weiter einen SMI aus.

**In QEMU gibt es diese Capability nicht.** Der Code läuft dort also
durch, ohne etwas zu tun — und genau deshalb muss er sorgfältig sein:
Er wird zuerst auf echter Hardware ausprobiert, wo man ihn nicht
schrittweise debuggen kann.

### Schritt 4 — Controller anhalten und zurücksetzen

1. `USBCMD.RS` (Bit 0) löschen → Controller anhalten
2. warten, bis `USBSTS.HCH` (Halted, Bit 0) gesetzt ist, Frist 20 ms
3. `USBCMD.HCRST` (Bit 1) setzen
4. warten, bis `HCRST` wieder 0 ist **und** `USBSTS.CNR` (Controller
   Not Ready, Bit 11) 0 ist, Frist 1 s

**`CNR` ist die Falle.** Nach dem Reset ist der Controller mehrere
Millisekunden lang nicht ansprechbar; schreibt man in dieser Zeit in
ein Register, wird der Schreibzugriff verworfen — ohne Fehler. Der
Treiber sieht danach eine Konfiguration, die er zu setzen glaubt und
die nie ankam.

### Schritt 5 — Capability-Register auswerten

Aus `HCSPARAMS1`: Anzahl Ports (`MaxPorts`, Bits 24–31), Anzahl Slots
(`MaxSlots`, Bits 0–7), Anzahl Interrupter (`MaxIntrs`, Bits 8–18).

Aus `HCCPARAMS1`:

* **`CSZ` (Bit 2) — die Kontextgröße.** Ist es gesetzt, sind alle
  Kontext-Strukturen **64 Byte** statt 32. Rechnet man mit 32, wo 64
  gilt, zeigt jeder Kontext-Zeiger ab dem zweiten Eintrag auf die
  falsche Stelle — und der Fehler tritt erst auf, wenn das erste
  Gerät angeschlossen wird, also weit weg von seiner Ursache. Das
  Bit wird deshalb ausgelesen, protokolliert und **jetzt schon**
  angewandt, obwohl dieser Schritt noch keine Gerätekontexte anlegt.
* `AC64` (Bit 0): 64-Bit-Adressierung.
* `xECP` (Bits 16–31): Zeiger auf die Extended Capabilities, in
  32-Bit-Worten ab BAR0.

Aus `HCSPARAMS2`: `Max Scratchpad Buffers` (Bits 21–25 hoch, 27–31
niedrig — **die Zahl ist auf zwei Felder verteilt**, und wer nur das
untere liest, bekommt bei Controllern mit vielen Puffern zu wenige).

### Schritt 6 — DCBAA (Device Context Base Address Array)

Ein Array aus 64-Bit-Zeigern, Index = Slot-ID, Eintrag 0 ist für die
Scratchpad-Tabelle reserviert. Größe `(MaxSlots + 1) * 8` Byte,
**64-Byte-ausgerichtet**. Physische Adresse nach `DCBAAP`.

### Schritt 7 — Scratchpad

Verlangt der Controller Scratchpad-Puffer (`Max Scratchpad Buffers >
0`), braucht er **echten Arbeitsspeicher für sich selbst**. Man legt
an:

* ein Array aus 64-Bit-Zeigern (die Scratchpad-Buffer-Array-Tabelle),
  dessen physische Adresse in **DCBAA[0]** kommt,
* je Puffer eine 4-KiB-Seite (PAGESIZE-ausgerichtet).

**Das wird gern übersehen, weil QEMU 0 verlangt.** Echte Controller
verlangen oft 4–32 Puffer, und ohne sie läuft der Controller nicht an
oder stürzt beim ersten Gerät ab. Wir setzen es deshalb um, obwohl
unser Testaufbau es nicht braucht — genau der Fall, in dem man einen
Fehler erst auf der Hardware findet, auf der man ihn am schlechtesten
sucht.

### Schritt 8 — Command Ring

Ein Ring aus TRBs (Transfer Request Blocks, je 16 Byte). Letzter
Eintrag ist ein **Link-TRB**, das auf den Anfang zurückzeigt.

* **64-Byte-ausgerichtet**, und er darf **keine 64-KiB-Grenze
  überschreiten**.
* Die physische Adresse kommt nach `CRCR` — zusammen mit dem
  **RCS-Bit** (Ring Cycle State), das mit unserem Cycle-Bit
  übereinstimmen muss.

In diesem Schritt wird der Ring nur angelegt, nicht benutzt.

### Schritt 9 — Event Ring und ERST

Der Event Ring ist **anders als die anderen Ringe**, und das ist die
Stelle, an der die meiste Verwirrung entsteht:

* Er hat **kein Link-TRB.** Statt dessen gibt es eine Tabelle, die
  **Event Ring Segment Table (ERST)**, die die Segmente aufzählt.
* Der Controller SCHREIBT, wir LESEN. Bei den anderen Ringen ist es
  umgekehrt.
* Wir sagen dem Controller per **ERDP** (Event Ring Dequeue Pointer),
  wie weit wir gelesen haben.

Anzulegen sind also drei Dinge: das Segment (unser eigentlicher Ring),
die ERST mit einem Eintrag (Adresse + Größe des Segments), und dann:

1. `ERSTSZ` = 1 (ein Segment)
2. `ERDP` = physische Adresse des Segmentanfangs
3. `ERSTBA` = physische Adresse der ERST — **zuletzt**, denn dieses
   Schreiben aktiviert den Interrupter.

ERST ist **64-Byte-ausgerichtet**, das Segment **64-Byte**, und die
Segmentgröße muss zwischen 16 und 4096 Einträgen liegen.

### Schritt 10 — Laufen lassen

`CONFIG.MaxSlotsEn` auf die Zahl der Slots setzen, die wir benutzen
wollen, dann `USBCMD.RS` setzen. Danach `USBSTS.HCH` prüfen: Es muss
**0** werden. Bleibt es 1, läuft der Controller nicht, und alles
Weitere wäre Zeitverschwendung.

---

## 4. Die Ring-Arithmetik — das, was man falsch macht

Ein TRB-Ring ist ein Array, das der Produzent und der Konsument
gemeinsam benutzen, **ohne** einen Zähler für „wie viele sind drin".
Statt dessen gibt es das **Cycle-Bit**:

* Jeder Ring hat einen aktuellen Cycle-Zustand (`CCS`), Startwert 1.
* Der Produzent schreibt jedes TRB mit dem aktuellen Zustand im
  Cycle-Bit.
* Der Konsument liest ein TRB als „für mich" genau dann, wenn dessen
  Cycle-Bit seinem eigenen Zustand entspricht.
* **Beim Umlauf kippt der Zustand.** Aus 1 wird 0, aus 0 wird 1.

Ohne das Kippen liefe der Konsument beim zweiten Umlauf über TRBs, die
er schon gelesen hat, und hielte sie für neu. **Das ist die
Kernarithmetik dieses Treibers, und sie hängt an keiner Hardware** —
sie wird deshalb als reine Funktion gebaut und auf dem Host getestet
(Umlauf, Kippen, mehrfacher Umlauf).

Für den Event Ring heißt das konkret: Wir merken uns Index und
erwarteten Cycle-Zustand. Beim Abholen lesen wir das TRB an unserem
Index; stimmt sein Cycle-Bit nicht mit unserem Zustand überein, ist
der Ring LEER (nicht etwa kaputt). Sonst verarbeiten wir es, erhöhen
den Index, und bei Überlauf auf 0 kippen wir den Zustand.

---

## 5. Die Fallgruben, gesammelt

| # | Falle | Folge, wenn man sie übersieht |
|---|---|---|
| 1 | **MMIO gecacht gemappt** | Statusbits ändern sich nie, jeder Warteschritt läuft in den Timeout |
| 2 | **`CSZ` ignoriert (32 statt 64 Byte)** | Läuft bis zum ersten Gerät, dann falsche Kontext-Zeiger |
| 3 | **`CNR` nicht abgewartet** | Registerschreibzugriffe nach dem Reset verpuffen lautlos |
| 4 | **BIOS-Handoff übersprungen** | Nur auf echter Hardware: SMI-Sturm, zwei Besitzer |
| 5 | **Bus Master nicht gesetzt** | Controller läuft an, greift aber nie auf die Ringe zu |
| 6 | **Event Ring mit Link-TRB gebaut** | Er hat keins — der Controller schreibt über das Ende hinaus |
| 7 | **`ERSTBA` vor `ERDP` geschrieben** | Interrupter aktiv, bevor der Lesezeiger stimmt |
| 8 | **Cycle-Bit beim Umlauf nicht gekippt** | Alte Events werden ewig neu verarbeitet |
| 9 | **Ring über eine 64-KiB-Grenze** | Undefiniertes Verhalten, oft still |
| 10 | **Scratchpad nicht angelegt** | QEMU egal, echte Hardware läuft nicht an |
| 11 | **Registerzugriff ohne `volatile`** | Compiler fasst Warteschleifen zusammen |
| 12 | **Scratchpad-Zahl nur aus einem Feld** | Zu wenige Puffer bei großen Controllern |

---

## 6. Protokollierung

**Bei xHCI ist das serielle Protokoll die einzige Chance.** Es gibt
keinen Zwischenzustand, den man sich ansehen kann: Entweder der
Controller läuft, oder er tut nichts, und beide Fälle sehen von außen
gleich aus.

Deshalb protokolliert jeder Schritt seinen Namen, die gelesenen
Rohwerte und das Ergebnis — und zwar **auch im Erfolgsfall**. Ein
Protokoll, das nur bei Fehlern spricht, hilft genau dann nicht, wenn
man es braucht: Auf fremder Hardware ist die letzte gedruckte Zeile
die einzige Information darüber, wo es stehengeblieben ist.

---

## 7. Interrupts: bewusst noch nicht

Der Event Ring wird in diesem Schritt **gepollt**, nicht per Interrupt
abgeholt — ein Kernel-Task sieht regelmäßig nach.

Begründung: xHCI benutzt normalerweise MSI-X, und das ist ein eigenes
Vorhaben (Tabelle im BAR finden, Vektoren zuordnen, den APIC
programmieren — CLAUDE.md hält fest, dass APIC/MSI erst mit SMP
wirklich fällig wird). Der Legacy-PCI-IRQ funktioniert bei xHCI zwar
oft, ist aber auf echter Hardware häufig mit anderen Geräten geteilt.

Für den Zuschnitt dieses Schrittes — Ports beobachten — reicht Polling
vollkommen: Ein Steckvorgang ist ein menschliches Ereignis, 100 ms
Latenz merkt niemand. Für eine Tastatur wird das anders, und dann ist
es der richtige Zeitpunkt für die Interrupt-Frage.

Dieselbe Haltung wie bei virtio-blk (gepollt) gegen virtio-net
(Interrupts): Der Unterschied ist nicht die Bequemlichkeit, sondern
ob unaufgefordert etwas ankommt.

---

## 8. Testbarkeit

**Was ohne Hardware getestet wird** (reine Funktionen, Host):

* die Register-Dekoder: `HCSPARAMS1` → Ports/Slots/Interrupter,
  `HCCPARAMS1` → CSZ/AC64/xECP, `HCSPARAMS2` → Scratchpad-Zahl aus
  **beiden** Feldern
* die Ring-Arithmetik: Vorrücken, Umlauf, Kippen des Cycle-Bits,
  mehrfacher Umlauf
* die Port-Status-Dekodierung: angeschlossen, aktiviert,
  Geschwindigkeit

**Was nur in QEMU geht:** dass der Controller wirklich anläuft und
dass ein `device_add` ein Port-Event erzeugt.

**Was nur auf echter Hardware geht:** der BIOS-Handoff und die
Scratchpad-Puffer. Beides ist deshalb besonders sorgfältig
protokolliert.

---

## 9. Der Testaufbau

Der Runner hängt an:

```
-device qemu-xhci,id=xhci
-device usb-kbd,bus=xhci.0
-device usb-mouse,bus=xhci.0
```

**PS/2 bleibt zusätzlich an**, damit nichts kaputtgeht: Solange der
USB-Pfad keine Eingaben liefert, ist PS/2 die einzige Bedienung, und
ein Testlauf, der das Bedienen unmöglich macht, wäre ein Rückschritt.

`SPEEDOS_OHNE_PS2=1` schaltet PS/2 ab (`-machine ...,i8042=off`) —
**das ist die Situation auf echter Hardware**, und der einzige Weg,
den USB-Pfad isoliert zu prüfen. Solange USB keine Eingaben liefert,
ist ein so gestarteter Rechner nicht bedienbar; das ist beabsichtigt
und steht im Runner als Meldung.

---

## 9a. Was beim Bauen wirklich passiert ist

Zwei Befunde, die im Entwurf oben nicht standen — der erste, weil die
Umsetzung vom eigenen Text abwich, der zweite, weil ihn niemand
vorhersagen konnte.

### `ERSTSZ` ist die Zahl der SEGMENTE, nicht der TRBs

Im Entwurf (§3, Schritt 9) steht „`ERSTSZ` = 1 (ein Segment)". Im Code
stand `RING_EINTRAEGE`, also **64**. Der Controller liest damit 64
ERST-Einträge, obwohl nur einer gültig ist — die übrigen 63 sind
Nullen, also 63 Segmente der Größe 0 an Adresse 0.

**Symptom: nichts.** Der Controller lief an, `HCH` fiel, `USBSTS` war
sauber, die Ports wurden korrekt gelesen — und es kam nie ein Event.
Kein Fehlercode, keine Meldung.

Gefunden hat es nicht das Nachdenken, sondern der Auszug: Erst als
Interrupter-Register **und** die ersten TRBs nebeneinander im Protokoll
standen, war zu sehen, dass der Controller gar nichts geschrieben
hatte — und damit, dass der Fehler in der EINRICHTUNG lag und nicht im
Auslesen. Das ist die Unterscheidung, die man ohne Auszug nicht treffen
kann; deshalb ist er als `usb --roh` fest eingebaut geblieben.

Die Lehre ist unbequemer als der Fehler: **Ein Entwurf, der das
Richtige sagt, schützt nicht davor, es falsch abzuschreiben.**

### `usb-kbd` STIEHLT die PS/2-Tastatur — der Testaufbau war der Rückschritt

Der Plan in §9 lautete: USB-Tastatur und -Maus zusätzlich zu PS/2
anhängen, „damit nichts kaputtgeht". Gemessen wurde das Gegenteil.

**QEMU leitet Tastendrücke an die ZULETZT angemeldete Tastatur.** Mit
`usb-kbd` am xHCI bekommt die PS/2-Tastatur nichts mehr — und weil
SpeedOS USB-HID noch nicht liest, ist die Maschine dann überhaupt nicht
mehr bedienbar. Genau der Rückschritt, den die Vorsichtsmaßnahme
verhindern sollte.

Aufgefallen ist es beim Versuch, den `usb`-Befehl zu tippen: Es kam
kein Echo. Der Gegenbeweis war ein Start ohne die Geräte — dort ging
es sofort.

Deshalb gilt jetzt:

* **`qemu-xhci` hängt IMMER dran.** Der Controller allein stiehlt
  nichts und wird für den Treiber gebraucht.
* **`usb-kbd`/`usb-mouse` nur mit `SPEEDOS_USB_GERAETE=1`**, mit einer
  Warnung im Runner.
* Für den Port-Event-Test braucht man sie ohnehin nicht — ein Gerät,
  das beim Start schon steckt, erzeugt gar kein Event (siehe unten).

### Schon steckende Geräte erzeugen kein Event

Das ist kein Fehler, sondern die Spezifikation: Das `CSC`-Bit stand
schon, bevor der Controller lief. Der Treiber protokolliert deshalb
beim Start, welche Ports belegt sind — sonst sieht ein Protokoll ohne
Events genauso aus wie ein kaputter Event Ring.

Geprüft wird der Event Ring folglich mit einem Gerät, das WÄHREND des
Betriebs dazukommt: `tools/usb_einstecken.py` macht das über QMP
(`device_add` / `device_del`).

**Ergebnis:** Einstecken und Ziehen erzeugen je ein Port Status Change
Event an Port 7, beide kommen an und werden aufgelöst.

---

## 10. Was danach kommt

* **Teil 2:** Slot aktivieren, Adresse vergeben, Deskriptoren lesen —
  das erste Gerät, das antwortet.
* **Teil 3:** HID-Boot-Protokoll, Tastatur und Maus in die
  bestehenden Eingabepfade (`tastatur.rs`, `maus.rs`) einhängen.

Erst danach ist die Aussage aus der Bestandsaufnahme eingelöst.
