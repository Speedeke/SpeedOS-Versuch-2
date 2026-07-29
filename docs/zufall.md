# Zufall in SpeedOS — Entwurf

Stand: Juli 2026, Serie 7, Teil 1. **Dieses Dokument entstand vor dem Code**
(Projektregel wie bei `docs/speedfs-format.md`, `docs/scheduler-design.md`,
`docs/syscalls.md`). Die Messwerte in §2 sind nachträglich eingetragen — sie
sind Beobachtungen, keine Annahmen, und als solche gekennzeichnet.

---

## 0. Warum überhaupt, und warum jetzt

Heute gibt es in SpeedOS **keinen Zufallsgenerator**. Die zwei Stellen, die
„Zufall" im Namen tragen (Plattentest, TCP-Verlusttest), sind lineare
Kongruenzgeneratoren — absichtlich reproduzierbar und für Kryptographie
vollkommen untauglich. TCP-Anfangssequenznummern und ephemere DNS-Ports
stammen aus der TSC: für ihren Zweck ausreichend, aber vorhersagbar.

Das ist der **Blocker vor TLS** (`docs/serie7-bestandsaufnahme.md` §b). Ein
TLS-Client, dessen Zufall geraten werden kann, ist nicht „etwas schwächer" —
er ist **wertlos**: Client-Random, die Schlüsselanteile des Handshakes und
alle Nonces hängen daran. Deshalb steht Zufall vor TLS und nicht daneben.

### Die eine Sache, die dieses Dokument nicht verspricht

Ein Zufallsgenerator lässt sich **nicht durch Testen als sicher nachweisen**.
Das ist dieselbe Asymmetrie, die in der Serie-7-Bestandsaufnahme gegen
Eigenbau-TLS entschieden hat: Ein kaputter Generator sieht genauso zufällig
aus wie ein guter. Was hier nachweisbar ist, ist eng begrenzt:

* dass die **DRBG-Konstruktion** rechnet, was sie soll (Testvektoren — §7),
* dass **grobe Fehler** ausgeschlossen sind (Nullen, konstante Werte, ein
  hochzählender Zähler statt Zufall — §7),
* und dass die **Buchführung ehrlich** ist (was zählen wir als Entropie, und
  wie kommen wir auf die Zahl — §3).

Alles Übrige ist Argumentation, nicht Beweis. Diese Trennung wird hier und in
den Testkommentaren durchgehalten.

### Warum ChaCha20 selbst schreiben kein Widerspruch zur TLS-Absage ist

Die Serie-7-Bestandsaufnahme hat Eigenbau-Krypto abgelehnt. ChaCha20 als
DRBG-Kern ist trotzdem selbst geschrieben, und der Unterschied ist konkret:

| | TLS 1.3 | ChaCha20-Blockfunktion |
|---|---|---|
| Umfang | ~30.000 Zeilen, Zustandsautomat + Krypto + X.509 | 40 Zeilen, eine Permutation |
| Vollständig spezifiziert? | in Dutzenden RFCs, mit Optionen | RFC 8439 §2.3, eine Seite |
| Prüfbar? | nein — der Test wäre ein Angreifer | **ja — Testvektoren, bitgenau** |
| Seitenkanäle | Padding, Zeitverhalten, Fehlermeldungen | **keine**: nur Add/XOR/Rotate auf festen Indizes, kein Zweig und kein Tabellenzugriff hängt vom Schlüssel ab |
| Fehler bleibt unbemerkt? | ja | **nein** — der Testvektor schlägt fehl |

Genau die Eigenschaften, die TLS unprüfbar machen, fehlen hier. Ein Fehler in
unserer Blockfunktion fällt beim ersten Testlauf auf; ein Fehler in einem
selbstgebauten TLS fällt nie auf. Deshalb: Permutation selbst, Protokoll
niemals.

---

## 1. Welche Quellen hat SpeedOS wirklich?

Drei Klassen, und sie sind **nicht gleichwertig**:

```
   (a) HARDWARE            (b) INTERRUPT-JITTER          (c) SALZ
   RDSEED / RDRAND          TSC-Deltas von IRQs           RTC, Boot-Layout
   viel Entropie,           wenig Entropie je Probe,      KEINE Entropie
   nicht auditierbar        aber unabhängig von der       (vorhersagbar)
                            CPU-Firmware
        │                          │                           │
        └──────────────┬───────────┴───────────────────────────┘
                       ▼
                 ENTROPIE-POOL  ──►  ChaCha20-DRBG  ──►  zufall(ptr, len)
```

### (a) RDSEED und RDRAND

* **RDSEED** (CPUID.07H:EBX[18]) ist die rohe Quelle des On-Die-Rauschens.
* **RDRAND** (CPUID.01H:ECX[30]) ist ein von RDSEED gesäter CSPRNG in
  Hardware.

Beide setzen **CF=0**, wenn gerade kein Wert bereitsteht — der Rückgabewert
*muss* geprüft und begrenzt wiederholt werden. Wer das übersieht, liest im
Fehlerfall den unveränderten Registerinhalt und hält ihn für Zufall.

**Warum niemals als einzige Quelle** — drei Gründe, jeder für sich
ausreichend:

1. **Nicht auditierbar.** Was der Rauschgenerator tut, steht in keinem
   Schaltplan, den wir lesen können. Eine Hintertür wäre von außen nicht
   unterscheidbar von gutem Zufall — genau die Sorte Fehler, die dieses
   Dokument sonst überall ausschließen will.
2. **Reale Errata.** Es gab AMD-CPUs, die nach dem Aufwachen aus S3 dauerhaft
   `0xFFFFFFFF` lieferten — mit gesetztem Carry-Flag, also als „gültig"
   gemeldet. Ohne Gesundheitsprüfung hätte ein System das als Zufall
   verarbeitet.
3. **Kostet nichts.** Immer zu mischen ist ein XOR. Es gibt keinen Grund,
   diese Absicherung wegzulassen.

Die Umkehrung gilt genauso: Ein defektes RDSEED darf den Pool **nicht
verschlechtern**. Weil eingemischt wird (XOR in einen Pool, der danach durch
die Permutation geht), kann eine schlechte Quelle die anderen nicht
beschädigen — sie trägt nur nichts bei.

### (b) Interrupt-Jitter — die klassische Hobby-OS-Quelle

SpeedOS hat eine invariante, per PIT kalibrierte TSC (`zeit::us_seit_boot`)
und mehrere unabhängige Interrupt-Quellen. Der Zeitpunkt, zu dem ein
Interrupt eintrifft, gemessen in TSC-Zyklen, hat in den **unteren Bits**
echte Unvorhersagbarkeit: Buslaufzeiten, Cache-Zustand, DRAM-Refresh,
Takt-Domänen, die nicht phasenstarr sind — und bei Mensch und Netz die
Quelle selbst.

| Quelle | IRQ | Woher die Unvorhersagbarkeit kommt | Güte |
|---|---|---|---|
| Tastatur | 1 | menschliches Tippen, ~10 ms Jitter | **gut** |
| Maus | 12 | menschliche Bewegung, 200 Proben/s | **gut** |
| Netz-RX | 11 | fremde Gegenstelle, Netzlaufzeit | **gut** |
| Platte | — | Warteschlangen, Wirt-Dateisystem (virtio) | mittel |
| PIT | 0 | **nur** Interrupt-Latenz-Jitter | **schwach** |

Der PIT ist bewusst als *schwach* eingestuft, und das ist die wichtigste
Zeile der Tabelle: Ein 250-Hz-Timer tickt **regelmäßig**. Die TSC-Differenz
zwischen zwei Ticks ist fast konstant; unvorhersagbar sind nur die
untersten Bits der Latenz. Ihn wie die anderen zu bewerten wäre die
bequemste Art, sich selbst zu betrügen — siehe §3.

### (c) Boot-Zeit und RTC — **Salz, nicht Entropie**

RTC-Uhrzeit, TSC-Stand beim Boot und das Speicher-Layout werden
eingemischt, aber mit **null angerechneten Bits**. Die Begründung ist kurz
und lässt keinen Spielraum:

> **Ein Angreifer kennt sie.** Die Uhrzeit eines Bootvorgangs lässt sich auf
> Sekunden schätzen (oft besser: aus einem Zertifikat, einer Logzeile, einer
> HTTP-`Date`-Kopfzeile). Das Speicher-Layout ist bei gleicher Firmware und
> gleicher Auflösung identisch. Beides ist **reproduzierbar**, und
> Reproduzierbarkeit ist das genaue Gegenteil von Entropie.

Wozu dann überhaupt einmischen? Aus einem Grund, der nichts mit Entropie zu
tun hat: **Trennung gleicher Systeme.** Zwei identische Rechner mit
identischer Firmware, die zur selben Millisekunde booten, hätten sonst
denselben Startzustand. Salz macht ihre Zustände verschieden, ohne einen
einzigen Angreifer zu behindern. Genau das ist die Definition von Salz — und
genau deshalb darf es nie als Entropie gezählt werden.

---

## 2. Was ist tatsächlich verfügbar? (gemessen)

Erhoben mit `zufall::status()` bzw. dem Shell-Befehl `zufall`, jeweils direkt
nach dem Boot.

| Umgebung | RDSEED | RDRAND | Beleg |
|---|---|---|---|
| **QEMU + WHPX, Standard-Runner** (kein `-cpu`) | **ja** | **ja** | gemessen, `tests/zufall.rs` |
| **QEMU + WHPX, `SPEEDOS_CPU=qemu64`** | **ja** | **ja** | gemessen — siehe unten |
| **QEMU + TCG** (ohne Hardware-Virtualisierung) | *nicht gemessen* | *nicht gemessen* | erwartet: nein, weil TCG das CPU-Modell wirklich emuliert. Der Runner probiert immer erst WHPX; um TCG zu erzwingen, müsste man die `-accel`-Liste ändern. |
| **Acer Aspire A515-51** (Kaby Lake, verifizierte Zielhardware) | *erwartet ja* | *erwartet ja* | aus der CPU-Generation; per Live-USB mit `zufall` zu bestätigen (`docs/hardware-log.md`) |

### Eine Erwartung, die sich als falsch herausgestellt hat

Der ursprüngliche Entwurf ging davon aus, dass QEMU ohne `-cpu` das Modell
`qemu64` benutzt und die Bits damit **maskiert** — die Standard-Testumgebung
hätte also keine Hardwarequelle gehabt. **Das ist nachgemessen widerlegt:**
Unter WHPX sind RDSEED und RDRAND vorhanden, und zwar auch mit einem
ausdrücklichen `-cpu qemu64`. Der Hypervisor reicht die CPUID-Bits der
Host-CPU durch; das Modell filtert sie nicht.

Die Annahme steht hier stehen gelassen statt stillschweigend korrigiert, weil
sie zeigt, wozu §2 überhaupt da ist: Eine plausible Erwartung über fremde
Hardware ist keine Tatsache, solange sie nicht nachgesehen wurde.

*(Der Runner nimmt `SPEEDOS_CPU` jetzt als Schalter entgegen — er war für
genau diese Messung nötig und bleibt für weitere.)*

### Warum der unangenehme Fall trotzdem bei jedem Testlauf durchlaufen wird

Weil die Hardwarequelle **gedeckelt** ist (§3): Angerechnet wird höchstens
die halbe Schwelle, also 128 der 256 Bit. Genau das zeigt der Bootlog:

```
[ZUFALL] RDSEED: ja, RDRAND: ja — Start mit 134 von 256 Bit.
[ZUFALL] Noch nicht gesaet — es fehlen 122 Bit aus Interrupt-Jitter.
```

Der Generator ist beim Boot **nicht** gesät, obwohl eine Hardwarequelle
vorhanden ist, und muss auf Interrupt-Jitter warten. Die „nie aus einer
Quelle allein"-Regel ist damit kein Kommentar, sondern etwas, das man im
Bootlog sieht — und der Wartepfad aus §4 kann nicht unbemerkt verrotten.

---

## 3. Wie viel Entropie nehmen wir an? (die unbequeme Rechnung)

Entropieschätzung ist die Stelle, an der ein RNG-Entwurf ehrlich oder
gefällig ist. Die Regel hier lautet: **im Zweifel weniger anrechnen.** Zu
wenig anrechnen kostet Wartezeit beim Boot; zu viel anrechnen kostet
Sicherheit, ohne dass es jemand merkt.

### Was angerechnet wird

| Quelle | Bits je Probe | Begründung |
|---|---|---|
| Tastatur | 4 | Tippabstände schwanken um zehntel Sekunden; das sind bei 4,2 GHz Millionen TSC-Zyklen Streuung. 4 Bit sind eine grobe Untertreibung — bewusst. |
| Maus | 3 | Wie Tastatur, aber 200 Proben/s: Aufeinanderfolgende Proben sind **korreliert** (eine Bewegung ist glatt), also weniger je Probe. |
| Netz-RX | 2 | Ankunftszeit hängt von einer fremden Gegenstelle ab — aber ein Angreifer im selben Netz kann sie mitbestimmen. Deshalb niedrig. |
| Platte | 2 | Antwortzeiten schwanken, sind aber bei virtio vom Wirt bestimmt. |
| PIT | 1 je **8.** Tick | Siehe unten. |
| RDSEED / RDRAND | 64 je 64-Bit-Wert, gedeckelt auf die Hälfte der Schwelle | Volle Anrechnung wäre üblich — der Deckel erzwingt, dass **nie** allein aus der Hardware gesät wird. |
| Salz (RTC, Layout) | **0** | §1(c). |

### Warum der PIT so hart abgewertet wird

Ein 250-Hz-Timer liefert 250 Proben/s. Mit 1 Bit je Probe wäre die
256-Bit-Schwelle nach **einer Sekunde** erreicht — ausgerechnet aus der
regelmäßigsten Quelle im System. Das wäre eine Zahl, die gut aussieht und
nichts bedeutet.

Mit „1 Bit je 8. Tick" sind es **31 Bit/s**, die Schwelle also nach gut
**8 Sekunden** reiner PIT-Zeit. Und selbst diese Zahl ist eine **Annahme, keine
Messung**: Wir behaupten, dass in der Interrupt-Latenz eines Ticks
mindestens ein Bit steckt, das ein Angreifer nicht vorhersagen kann.
Plausibel ist das (Cache, DRAM-Refresh, unter WHPX zusätzlich die
Host-Planung); bewiesen ist es nicht. Wer eine belastbare Zahl will, muss
die Deltas aufzeichnen und eine Min-Entropie-Schätzung nach NIST SP 800-90B
rechnen — das ist eine eigene Aufgabe und steht in §8.

### Zusätzliche Filter, die vor Selbstbetrug schützen

1. **Gleiche Differenz zweimal ⇒ null Bits.** Liefert eine Quelle zweimal
   hintereinander dasselbe TSC-Delta, ist sie in diesem Moment ein Zähler und
   kein Rauschen. Die Probe wird eingemischt, aber **nicht angerechnet**.
   (Der einfachste denkbare Wiederholungstest, angelehnt an den *Repetition
   Count Test* aus SP 800-90B.)
2. **Deckel.** Der Zähler wird bei 4096 Bit gekappt — ein Mausbewegungs-Sturm
   soll nicht den Eindruck erwecken, wir hätten Kilobit an Entropie.
3. **Gesundheitsprüfung der Hardwarequelle.** Beim Start werden 8 Werte
   gezogen; sind alle gleich, oder ist einer davon `0` oder `u64::MAX` und
   wiederholt sich, gilt die Quelle als **defekt** und wird dauerhaft
   abgeschaltet (das AMD-Erratum aus §1a).

### Die Schwelle

**256 Bit**, weil der DRBG-Schlüssel 256 Bit hat. Mehr anzusammeln, bevor der
Generator als gesät gilt, bringt nichts; weniger wäre eine kürzere Schlüssel
als beworben.

---

## 4. Der unangenehme Fall: frisch gebootet, keine Eingabe, kein RDRAND

Das ist die Situation, an der sich ein RNG-Entwurf entscheidet — und es ist
keine Randbedingung, sondern der Normalfall: **Ein Programm, das beim Boot
startet und sofort TLS spricht, trifft genau darauf.**

Die drei möglichen Antworten:

### (i) Schwachen Zufall liefern — **abgelehnt**

„Irgendwas ist besser als nichts" ist hier falsch herum gedacht. Der Aufrufer
bekommt Bytes, die aussehen wie Zufall, prüft nichts (er *kann* nichts
prüfen) und baut daraus einen Sitzungsschlüssel. Der Fehler ist **still und
dauerhaft**: Die Verbindung funktioniert, das Schloss-Symbol erscheint, und
die Vertraulichkeit ist weg. Das ist exakt das Fehlerbild, wegen dem dieses
Projekt kein eigenes TLS baut. Ein Fallback wäre hier die schlimmste
Lösung — nicht die bequemste.

### (ii) Sofort einen Fehler liefern — ehrlich, aber unbrauchbar

Klar und ohne Risiko, aber es verschiebt das Problem nach oben: Jeder
Aufrufer müsste selbst eine Warteschleife bauen, und der erste, der es
vergisst, hat einen Absturz beim Boot statt einer kurzen Verzögerung.

### (iii) Blockieren, bis der Pool bereit ist — **gewählt**

Der Syscall wartet, bis 256 Bit beisammen sind. Das ist die Entscheidung, die
Linux 2020 mit `getrandom(2)` getroffen hat, nachdem der frühere Weg
(`/dev/urandom` liefert immer etwas) reihenweise Systeme mit vorhersagbaren
Schlüsseln erzeugt hatte.

**Aber mit Frist, und das ist der Unterschied zu Linux.** Ewiges Blockieren
ist auf einem Einzelnutzer-System ohne Netz und ohne Tastatur ein Hänger
ohne Meldung — und *„keine Meldung"* verstößt gegen die Daten-Integritäts-
Regel dieses Projekts genauso wie stiller Datenverlust. Deshalb:

> **`zufall(ptr, len)` blockiert bis zu `ZUFALL_FRIST_MS` (10 s). Ist der Pool
> dann immer noch nicht bereit, liefert er `Fehler::NichtGesaet`.**
>
> Nie schwache Bytes. Nie ein Hänger ohne Ende. Immer eine Antwort, die der
> Aufrufer unterscheiden kann.

Warum 10 Sekunden: PIT allein braucht rechnerisch ~8 s (§3). Die Frist ist
also so gewählt, dass die schwächste vorstellbare Konfiguration es **gerade
noch** schafft — und alles darunter ein echter Befund ist, kein
Ungeduldsfehler.

**Was der Nutzer dabei sieht:** Der Shell-Befehl `zufall` zeigt den
Pool-Status (Bits, Quellen, gesät ja/nein). Wer wartet, kann nachsehen,
worauf — statt zu raten.

**Und was das für TLS heißt, ausgesprochen:** Auf einem Rechner ohne RDSEED
und ohne jede Eingabe wird der erste TLS-Handshake nach dem Boot ein paar
Sekunden später stattfinden. Das ist der Preis, und er ist richtig
eingepreist.

---

## 5. Aufbau

```
   IRQ-Handler (Timer, Tastatur, Maus, Netz, Platte)
        │  zufall::einspeisen(Quelle)   ← nur Atomics, kein Lock, keine Allokation
        ▼
   ENTROPIE-POOL   [AtomicU64; 32]  (256 Byte, fetch_xor, Ringindex)
        │  pool_falten()  — XOR-Faltung auf 48 Byte, dann EINE ChaCha20-Runde
        ▼
   DRBG   Schlüssel [u8;32] + Zähler u64        ← spin::Mutex, BLATT-Lock
        │  fast key erasure: erste 32 Byte jedes Aufrufs = NEUER Schlüssel
        ▼
   fuellen(&mut [u8])  →  Syscall zufall(ptr,len)  /  Shell `zufall`
```

### Warum ChaCha20 als DRBG-Kern

* **`no_std`, keine Abhängigkeit** — reine 32-Bit-Ganzzahlarithmetik. Kein
  Fließkomma (unser Target ist `-sse/+soft-float`), keine Tabellen.
* **Konstante Laufzeit von selbst.** Add/XOR/Rotate auf festen Indizes; kein
  Zweig und kein Speicherzugriff hängt vom Schlüssel ab. Bei einer
  AES-Software-Implementierung mit S-Box-Tabellen wäre genau das die
  klassische Cache-Timing-Lücke.
* **Prüfbar** (§7) — der eigentliche Grund.
* Es ist die Konstruktion hinter `arc4random` (BSD) und dem heutigen
  Linux-`get_random_bytes`. Wir bauen nichts Neues, wir bauen etwas
  Bekanntes nach.

### Fast key erasure — warum der Schlüssel bei jedem Aufruf stirbt

Jeder `fuellen`-Aufruf erzeugt einen Keystream aus dem aktuellen Schlüssel;
dessen **erste 32 Byte werden der neue Schlüssel**, der Rest ist Ausgabe. Der
alte Schlüssel wird überschrieben (`write_volatile`, damit der Optimierer ihn
nicht stehen lässt).

Der Gewinn ist **Vorwärts-Sicherheit**: Wer den Kernel-Speicher später liest,
kann frühere Ausgaben nicht rekonstruieren — die dafür nötigen Schlüssel
existieren nicht mehr. Ohne diesen Schritt könnte ein einziger späterer
Speicherabzug rückwirkend jeden je erzeugten Sitzungsschlüssel offenlegen.

### Nachsäen

* **Beim Boot**, sobald die Schwelle erreicht ist (Zustandswechsel auf
  „gesät").
* **Periodisch** alle 5 Sekunden aus einem Kernel-Task, wenn seither neue
  Entropie eingetroffen ist.
* **Vor jedem `fuellen`**, wenn seit dem letzten Nachsäen mindestens 64 neue
  Bits angefallen sind.

Nachsäen ist `schluessel ^= pool_falten()` gefolgt von einem
Key-Erasure-Schritt. XOR, damit eine **schlechte neue Quelle den bestehenden
Zustand nicht verschlechtern kann** — das ist die Eigenschaft, auf der die
gesamte „nie nur eine Quelle"-Regel steht.

---

## 6. Die ABI

```
zufall(ptr, len)  →  Nummer 12,  Ergebnis = gefüllte Bytes
```

* `len` ≤ `MAX_PUFFER` (64 KiB), wie jeder Puffer der ABI.
* `len == 0` → `Ok(0)`, kein Fehler.
* copy-out über `ring3::copy_out` (Dauerregel I) — der Kernel füllt einen
  **Kernel**-Puffer und kopiert geprüft hinaus. Nie direkt in User-Speicher
  schreiben: Schlägt die Prüfung fehl, hat der Prozess **nichts** bekommen,
  statt eines halb gefüllten Puffers.
* Fehler:
  * `NichtGesaet` (**neu, Code 25**) — Pool nicht bereit, auch nach der Frist.
  * `UngueltigerZeiger`, `ZuGross` — wie überall.

Der Fehlercode 25 ist eine **Erweiterung**, keine Änderung: Bestehende Zahlen
bleiben, wie sie sind (`docs/syscalls.md` §0).

---

## 7. Was geprüft wird — und was die Prüfung wert ist

### Belastbar: Testvektoren

`test_chacha20_rfc8439_vektoren` prüft die Blockfunktion gegen **RFC 8439
§2.1.1** (Quarter Round) und **§2.3.2** (Blockfunktion, voller Zustand *und*
serialisierte 64 Byte). Das ist der Teil, der wirklich etwas beweist: Wäre
auch nur eine Rotation, eine Addition oder die Little-Endian-Serialisierung
falsch, stimmte kein einziges Byte.

Zusätzlich wurden die Vektoren **unabhängig gegengeprüft**: mit einer aus der
Spezifikation heraus geschriebenen Python-Referenz, die dieselben Werte
liefert. Zwei unabhängige Herleitungen, ein Ergebnis.

Dazu: der DRBG selbst gegen sich (`nachsaeen` mit bekanntem Material erzeugt
reproduzierbare Folgen), und die Key-Erasure-Eigenschaft (der Schlüssel ist
nach jedem `fuellen` ein anderer).

### Nicht belastbar: Statistik

Byteverteilung, keine Wiederholung über N MiB, unterschiedliche Werte nach
Neustart — diese Tests laufen, **und sie beweisen keine kryptographische
Qualität**. Ein Zähler, durch AES geschickt, besteht sie alle. Was sie
finden, ist die Klasse grober Fehler:

* Puffer bleibt genullt (Generator lief gar nicht),
* konstanter Wert (Schlüssel/Zähler bewegen sich nicht),
* erkennbare Struktur (Zähler statt Zufall),
* identische Folge nach Neustart (Salz/Pool wirken nicht).

Genau das steht als Kommentar an den Tests, damit niemand später ein grünes
Häkchen für mehr hält, als es ist.

---

## 8. Bewusst NICHT dabei

* **Min-Entropie-Schätzung nach NIST SP 800-90B.** Unsere Bit-Werte in §3
  sind begründete Untertreibungen, keine Messungen. Eine echte Schätzung
  bräuchte aufgezeichnete Deltas und die Testbatterie des Standards. Der
  Weg dahin: Deltas in einen Ring schreiben, per Shell ausleiten, offline
  auswerten.
* **Getrennte Pools je Quelle** (Linux' Eingangspools). Bei fünf Quellen und
  einem Einprozessor-System ist ein Pool ausreichend.
* **Blockierendes `zufall` mit Weckruf statt Frist.** Die Weck-Maschinerie aus
  Serie 7 Teil 0 könnte das (`scheduler::wecken`), und es wäre eleganter als
  das Wartefenster. Es wäre aber auch ein neuer `Warteauf`-Zustand für einen
  Fall, der nach 10 Sekunden ohnehin entschieden ist — der Aufwand lohnt
  erst, wenn `zufall` ein heißer Pfad wird.
* **Zufall für TCP-Sequenznummern und ephemere Ports.** Der Nebengewinn aus
  der Bestandsaufnahme; eine eigene, kleine Aufgabe, sobald der Generator
  steht.
* **Wiederherstellbarer Startwert über Neustarts** (Linux' `random-seed`-
  Datei). Würde den Bootfall in §4 entschärfen, verlangt aber eine Datei, die
  beim Herunterfahren geschrieben und beim Start **sofort ungültig gemacht**
  werden muss — sonst säen zwei Starts aus demselben Wert. Lohnt sich, hat
  aber eine scharfe Kante und gehört deshalb in einen eigenen Schritt.
