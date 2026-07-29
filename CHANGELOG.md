# Changelog

Alle nennenswerten Änderungen an SpeedOS, neueste zuerst.
Format angelehnt an [Keep a Changelog](https://keepachangelog.com/de/).

## [Unveröffentlicht]

### Serie 7, Teil 0: DIE WECK-LATENZ — aus 199 KiB/s werden 202 MiB/s

Die Vorarbeit für TLS. Der Serie-6-Abschluss hatte eine Zahl hinterlassen,
die nicht stehen bleiben durfte: Eine Pipe brachte vom Prozess zum Kernel
**199 KiB/s**, während derselbe Ringpuffer im Kernel allein **241 MiB/s**
schaffte. Der Faktor 1200 war nicht das Kopieren, sondern das **Warten aufs
Geweckt-Werden** — und TLS im User-Space schickt jedes einzelne Byte durch
genau diese Kette.

- **SOFORTIGES WECKEN STATT TIMER-PRÜFUNG (`scheduler::wecken`).** Bis hierhin
  galt „nachsehen statt anstoßen": Der Timer fragte jeden Tick nach, ob ein
  wartender Prozess weiterkann. Jetzt stößt die Pipe selbst an — wer Bytes
  hineinlegt, weckt die Leser; wer welche herausnimmt, weckt die Schreiber;
  wer ein Ende schließt, weckt die Gegenseite (Dateiende bzw. EPIPE). Dasselbe
  gilt für das Ende eines Kindes (`warte`). **Der Timer bleibt als
  SICHERHEITSNETZ**: `wecken()` arbeitet mit `try_lock` und darf folgenlos
  aussetzen, weil `warter_wecken` jede Bedingung weiterhin jeden Tick
  nachprüft. Ein Weckruf, der aussetzt, kostet 4 ms; einer, der als einziger
  Weg gedacht wäre, wäre ein Prozess, der nie wieder aufwacht.
- **DIE ENTSCHEIDUNG — Reschedule-Punkt, aber NIEMALS direkte Übergabe.**
  Drei Wege standen zur Wahl, und die Begründung steht ausführlich in
  `src/scheduler.rs`:
  - *Nur „lauffähig markieren"* löst das Problem **nicht**. Genau das war der
    gemessene Fall: Der Leser hatte eine frische 20-ms-Scheibe; ob der
    Schreiber nach 0 ms oder nach 4 ms lauffähig wird, ändert nichts daran,
    dass er erst in 20 ms drankommt. Aus 199 wären ~240 KiB/s geworden.
  - *Direkte Übergabe an den Geweckten* („handoff") ist die einzige Variante,
    die wirklich **aushungert**: Ein Ping-Pong-Paar A↔B reicht sich die CPU
    gegenseitig, ein dritter Prozess C ist nie „der Geweckte". Verworfen.
  - **Gewählt: ein Reschedule-Punkt über die normale Round-Robin-Wahl.** Der
    Weckruf setzt nur ein Flag; am Rückweg des Syscalls
    (`umplanen_am_syscall_ende`) und in den synchronen Warteschleifen des
    Kernels (`zeit::warte_auf_interrupt`) wird daraufhin `wechsel_entscheiden`
    gefragt — mit `freiwillig = true`, also derselben zyklischen Suche wie bei
    `yield`. Das Ziel ist damit immer der nächste Lauffähige **hinter** dem
    aktuellen, nie „der, den ich gerade geweckt habe".
- **FAIRNESS: der Schutz steckt in der Struktur, die Bremse nur im Aufwand.**
  Weil `naechster_lauffaehig` zyklisch ab `aktuell + 1` sucht, kommt bei
  A(Slot 1)↔B(Slot 2) und C(Slot 3) schon in der zweiten Runde C dran — ohne
  jedes Zutun. Aushungern ist damit strukturell ausgeschlossen (nachgerechnet
  in `test_pingpong_hungert_niemanden_aus`: jeder bekommt exakt seinen
  Viertel-Anteil über 200 Runden). Wogegen dennoch eine Bremse nötig war, ist
  **Verschwendung**: Ein Paar, das sich einzelne Bytes zuwirft, würde je Byte
  umschalten. Deshalb ein **Budget je Timer-Tick** (`SOFORT_MAX_PRO_TICK = 16`);
  danach wirkt der Weckruf nur noch markierend. Die Zahl ist ausgerechnet:
  16 × 450 ns je 4-ms-Tick = **0,18 % CPU-Deckel**, während ein produktiver
  64-KiB-Strom nur einen Weckruf je Füllung braucht (das Budget entspräche
  > 200 MiB/s, mehr als der Ringpuffer schafft). Wer das Budget ausschöpft,
  tauscht keine Daten, sondern schaltet um.
- **DIE hlt-FALLE, jetzt als Regel im Code:** `zeit::warte_auf_interrupt()`
  **gibt ab, statt zu schlafen**, solange ein anderer Prozess laufen kann —
  dieselbe Regel, nach der `Executor::sleep_if_idle` schon seit Serie 6 Teil 3
  verfährt, gilt jetzt für jede synchrone Warteschleife des Kernels. Das war
  nicht nur eine Messfalle: Wer auf Daten eines anderen Prozesses wartet und
  dabei `hlt`-t, **blockiert ihn**, denn seine verschlafene Zeitscheibe läuft
  trotzdem 20 ms.
- **PIPE-PUFFER KONFIGURIERBAR, Standard 4 KiB → 64 KiB**
  (`pipe::kapazitaet_setzen`, `anlegen_mit`; geklemmt auf 512 B … 256 KiB).
  Die Puffergröße *ist* die Stückgröße je Weckruf, der Durchsatz also
  höchstens `Kapazität / Weck-Latenz`. 64 KiB, weil das genau
  `syscall::MAX_PUFFER` ist: Ein einziger `schreibe` füllt eine leere Pipe,
  ein einziger `lese` leert sie — ein Kontext-Wechsel je 64 KiB ist damit
  erreichbar, ohne dass ein Programm etwas anders machen müsste. Speicher
  ehrlich ausgerechnet: 16 Pipes × 64 KiB = **1 MiB Worst Case**, alloziert
  erst beim `anlegen`.

**DIE ZAHLEN (QEMU/WHPX 4,2 GHz, ALT und NEU im SELBEN Lauf —
`tests/wecken.rs`; zwei Zahlen aus zwei QEMU-Starts wären nicht
vergleichbar):**

| Messung | ALT | NEU | Faktor |
|---|---|---|---|
| Weck-Latenz (Mittel aus 20 Runden) | 3558 µs | **17 µs** | 209× |
| Pipe Prozess → Kernel | 203 KiB/s | **202 MiB/s** | 1019× |
| Pipe Prozess → Prozess | 101 KiB/s | **199 MiB/s** | 2022× |
| Ringpuffer allein (Obergrenze) | — | 186 MiB/s | — |
| Socket-Syscall (UDP, 1 KiB/Datagramm) | — | 24 MiB/s (40 µs je `sende`) | — |

Die entscheidende Zeile ist die vierte: **Prozess-Pipe und roher Ringpuffer
liegen jetzt gleichauf.** Die Weck-Latenz ist nicht verkleinert, sondern aus
der Rechnung verschwunden — es begrenzt das Kopieren, und das ist die
richtige Grenze. Der ALT-Wert der Weck-Latenz (3558 µs) ist übrigens genau,
was er sein muss: ein halber bis ganzer Timer-Tick.

- **SOCKETS — ehrliche Einordnung statt Scheinarbeit.** Von diesem Pass sind
  Sockets **nicht** betroffen, und zwar aus einem nachprüfbaren Grund: *kein
  Prozess wartet je auf einen Socket.* `empfange` ist laut ABI
  nicht-blockierend (0 = noch nichts da, docs/syscalls.md), es gibt also
  keinen `Warteauf`-Zustand für Sockets und damit niemanden, den ein
  ankommendes Paket wecken könnte. Die Weck-Maschinerie ist trotzdem
  allgemein gebaut (`scheduler::wecken` nimmt jeden `Warteauf`), sodass ein
  späteres blockierendes `empfange` nur einen Aufruf im Zustell-Pfad braucht.
  Ein blockierendes `empfange` **jetzt** einzuführen wäre eine stillschweigende
  ABI-Änderung gewesen — die gibt es in diesem Projekt nur bewusst und
  dokumentiert. Gemessen wurde deshalb der *Weg* (Ring 3 → `int 0x80` →
  Zeigerprüfung → copy-in → UDP → virtio-net): 40 µs je `sende`, davon der
  Löwenanteil Geräte-Übergabe (ein Syscall allein kostet 60–70 ns).
- **TESTS (`tests/wecken.rs`, 6 Stück, alle grün):** Weck-Latenz ALT/NEU;
  **Fairness unter Ping-Pong-Last** (zwei Prozesse werfen sich einzelne Bytes
  zu, ein dritter rechnet nur — er bekam **99 % CPU**; bei direkter Übergabe
  wären es 0 %); **kein verlorenes Wecken bei gleichzeitigem Schließen**
  (25 Runden „Daten schreiben + Ende schließen ohne Pause dazwischen", jedes
  Mal kommen die Daten an, dann das Dateiende, kein Hänger); dazu die drei
  Durchsatz-Messungen. Unit-Tests für die reine Logik: `weck_passt`
  (Zeit-Warter dürfen von **keinem** Anstoß vorzeitig geweckt werden, fremde
  Pipes und die falsche Richtung wecken nicht, `Kind(0)` = „irgendeines"),
  Pipe-Kapazität (Deckel, und eine bestehende Pipe wächst **nie** nachträglich).
- **Selbst hineingelaufen, deshalb notiert:** Der erste Anlauf legte den
  Weckruf *innerhalb* des `PIPES`-Locks — das wäre PIPES → TABELLE gewesen,
  während der Timer TABELLE → PIPES nimmt: ein lehrbuchreines ABBA. Jetzt
  wird der Weckruf unter dem Lock nur **ermittelt** und danach ausgelöst.
  Und der Test lief prompt in die eigene, in CLAUDE.md notierte Falle:
  `aufraeumen()` löscht den Tabelleneintrag **samt Exit-Code** — wer in einer
  Schleife aufräumt, muss `ende_abfragen` vorher einsammeln.

### Serie-6-ABSCHLUSS: der Kernel unter Angriff, Zahlen, und die Weiche zu Serie 7

- **DER WERTVOLLSTE TEST DES PROJEKTS — und er hat etwas gefunden.**
  `userland/angreifer` ist ein absichtlich **böswilliges Programm im
  Repository**: Es versucht systematisch, aus Ring 3 auszubrechen —
  Kernel-Speicher lesen und schreiben, fremde Handles durchprobieren (alle
  Zahlen 3..64 plus die u64-Extreme), ungültige Syscall-Nummern,
  Zeiger mit Integer-Überlauf, absurde Längen, Stack-Überlauf, privilegierte
  Instruktionen, Endlosschleife ohne Abgabe.
  **Gefunden:** Bis hierhin hatten nur **#PF und #GP** einen IDT-Handler. Ein
  Ring-3-Programm mit `ud2` (#UD) oder einer **Division durch Null** (#DE)
  traf auf einen Vektor **ohne Eintrag** — und das eskaliert zum Double
  Fault, der SpeedOS anhält. **Ein einziges `div rax, 0` in einem
  unprivilegierten Programm hätte den ganzen Kernel gestoppt.**
  Behoben, indem nicht das Loch gestopft, sondern die **Klasse geschlossen**
  wurde: Jede aus Ring 3 erreichbare CPU-Exception hat jetzt einen Handler,
  alle laufen durch dieselbe `user_recovery`. Nachgewiesen im Testlauf:
  19 × Page Fault, 5 × #UD, 5 × #GP, 5 × #DE — **alle aus User-Mode
  aufgefangen, der Kernel lief durchgehend weiter.**
- **Der Verfügbarkeits-Angriff, gemessen:** Ein Prozess, der endlos rechnet
  und **nie** abgibt, wurde in 2 Sekunden **58-mal verdrängt** (bei 0
  freiwilligen Abgaben) — und ein friedlicher Nachbarprozess kam in derselben
  Zeit nachweislich voran. In einem kooperativen System wäre das das Ende
  gewesen.
- **Speicher-Pass:** 100 Zyklen starten/beenden, davon **33 mitten im Lauf
  per `beende(pid)` abgeschossen** (der interessantere Pfad — dort hält der
  Prozess noch alles in der Hand). Ergebnis: **Heap byte-exakt**
  (207 904 → 207 904), **0 Pipe-Lecks**, **0 Handle-Lecks** (20 Runden mit
  geerbten Handles). Dazu 39 Angriffe im Dauerbeschuss: ebenfalls byte-exakt.
- **Die eine Frame-Differenz — ausgerechnet statt weggedrückt.** Nach 100
  Zyklen fehlt **1 Frame**. Kein Prozess-Leck: `memory::allocate_pages`
  vergibt virtuellen Raum mit einem reinen Vorwärts-Zähler, und alle 512
  Seiten braucht `map_to` eine neue P1-Tabelle, die dem Kernel-Adressraum
  verbleibt. Bei 5 Seiten je Prozess sind das ~1 Frame je 100 Prozesse. Der
  Test rechnet die Schranke aus und benennt sie, statt die Bilanz
  aufzuweichen. (Behebung wäre ein Freilisten-Allocator für virtuelle
  Bereiche — notiert für Serie 7.)
- **LEISTUNG — gemessen aus Ring 3, nicht schöngerechnet** (QEMU/WHPX,
  4,2 GHz; Bestwert aus 7 Runden à 100 000 Aufrufen):

  | Was | Wert | Woraus es besteht |
  |---|---:|---|
  | **Syscall-Roundtrip** (`getpid`) | **60–70 ns** | `int 0x80` → Privilegienwechsel → TSS-Stack → 15 Register → Dispatch → `iretq` |
  | **Kontext-Wechsel** (yield-Roundtrip) | **~450 ns** | enthält den yield-Syscall; darin ein CR3-Wechsel, der den TLB leert |
  | **Prozess-Start** | **6–11 µs** | Datei lesen, ELF prüfen, Adressraum, Segmente, Stack, argv |
  | **Pipe, Ringpuffer allein** | **241 MiB/s** | reines Kopieren im Kernel |
  | **Pipe, Prozess → Kernel** | **199 KiB/s** | ⚠ siehe unten |

  **Ehrlich benannt, was langsam ist:** Der Pipe-Durchsatz zwischen Prozessen
  ist **1200-mal** niedriger als der Ringpuffer selbst — und das liegt
  **nicht** am Kopieren, sondern an der **Weck-Latenz**. Die Pipe fasst
  4 KiB; ist sie voll, schläft der Schreiber und wird erst geweckt, wenn der
  Timer die Bedingung nachprüft *und* er wieder an der Reihe ist — also etwa
  einmal je Scheduling-Runde (20 ms). 4 KiB / 20 ms **sind** genau die
  gemessenen 200 KiB/s. Die Hebel wären ein grösserer Puffer oder ein
  sofortiges Wecken durch den Leser statt der Prüfung im Timer.
  **Und der offensichtliche spätere Gewinn beim Syscall: `SYSCALL`/`SYSRET`
  statt `int 0x80`.** Das Interrupt-Gate kostet uns den Umweg über IDT und
  TSS; `SYSCALL` spart beides und wäre auf dieser Hardware etwa die Hälfte.
  Es bräuchte MSR-Einrichtung (STAR/LSTAR/SFMASK) und eine bestimmte
  GDT-Reihenfolge — eine Beschleunigung, keine andere Schnittstelle.
- **unsafe-Audit** (`docs/unsafe-audit-serie6.md`): Serie 6 hat die riskanteste
  `unsafe`-Fläche des Projekts gebracht, und jeder Block ist jetzt mit seiner
  **Invariante** dokumentiert — für `copy_in`/`copy_out` einzeln aufgeschlüsselt,
  welche der vier Anforderungen von `copy_nonoverlapping` durch welche
  Prüfstufe hergestellt wird. **0 `unsafe fn`** in der ganzen Prozess-Schicht.
  Bemerkenswert: Die zwei Module, die am meisten mit **fremden** Daten
  arbeiten — der ELF-Parser und die Pipes — kommen **ohne ein einziges
  `unsafe`** aus (`elf.rs` liest jede Zahl grenzgeprüft statt per
  `transmute`).
- **Bestandsaufnahme Serie 7** (`docs/serie7-bestandsaufnahme.md`): TLS-Strategie
  mit ausdrücklicher Bewertung des Eigenbaus, RNG-Lage, Zertifikate,
  Fenster-Naht für den Browser. Kurzfassung der Entscheidungen weiter unten.
- **Neue Programme:** `angreifer` (der Gegner) und `messung` (Leistung aus
  Ring 3). Neu: `protokoll::puffer_bytes()`, damit Speicher-Bilanzen den
  wachsenden Log-Puffer **benennen** statt ihn mitzumessen.

### Serie 6, Teil 6: PROZESSE ARBEITEN ZUSAMMEN — Pipes, warte, Strg+C
- **DER BEWEIS:** `starte zaehle 20 | filter 7` → `7`, `17`. Zwei
  eigenständige Programme, gleichzeitig in getrennten Adressräumen, verbunden
  durch ein Byte-Rohr im Kernel. **Beide Programme sind bytegleich mit dem,
  was auch allein läuft** — keine Fallunterscheidung, keine Anpassung. Die
  Umleitung passiert vollständig ausserhalb, beim Start.
- **Pipes (`src/pipe.rs`) — der Ringpuffer ist NICHT neu.**
  `netz::puffer::Ringpuffer` gibt es seit Serie 5 (TCP-Sende-/Empfangspuffer,
  unit-getestet). Eine Pipe ist genau das plus zwei Besitz-ZÄHLER. Zwei
  Ringpuffer-Implementierungen im selben Kernel wären zwei Stellen, an denen
  derselbe Off-by-one wohnen kann. Die drei Entscheidungen, die eine Pipe
  ausmachen: **voll → der Schreiber wartet** (Gegendruck, nicht abschneiden),
  **leer + Schreiber da → der Leser wartet**, **leer + kein Schreiber →
  Dateiende**. Und Lese-Ende zu → der Schreiber bekommt `Abgebrochen`
  (POSIX-EPIPE).
  **ZÄHLER STATT FLAGS:** Ein Ende kann mehrere Besitzer haben (die Shell
  hält es kurz selbst, während sie es dem Kind gibt). Ein Flag wäre je nach
  Schliess-Reihenfolge ein Leck oder ein zu früh gemeldetes Dateiende.
- **BLOCKIERENDE SYSCALLS — und wie sie bei uns funktionieren.** Ein Syscall
  hält NICHT mitten drin an: Unser gesicherter Kontext ist der Trap-Rahmen am
  Eingang, beim Umschalten landet die CPU per `iretq` hinter dem `int 0x80` —
  der Rust-Stack des halben Syscalls wäre verloren. Also wird er **von vorn
  wiederholt**: `rip` um zwei Bytes zurück (die Länge von `int 0x80`), Prozess
  schlafen legen, fertig. Daraus folgt die eiserne Regel, die jeder
  blockierende Syscall erfüllt: **bis zum Blockieren darf nichts verändert
  worden sein.**
  **GEWECKT WIRD DURCH NACHSEHEN, NICHT DURCH ANSTOSSEN:** Der Timer prüft je
  Tick die Weck-Bedingung (`Warteauf::{Zeit, Kind, PipeLesen, PipeSchreiben}`).
  Ein Weckruf aus dem schreibenden Prozess wäre schneller — und eine
  Lock-Kette quer durch den Kernel, aus einem Syscall heraus, in dem wir nicht
  warten dürfen. Preis: höchstens 4 ms.
- **ELTERN, KIND — UND KEINE ZOMBIES.** Das Unix-Modell hält den *Kind*-Eintrag
  am Leben, bis jemand `wait` ruft; das ist der Zombie. Wir kehren es um: Beim
  Ende eines Kindes wandert sein Ergebnis in einen kleinen Puffer **im
  Elternteil**, und der Kind-Eintrag verschwindet **sofort vollständig**
  (Adressraum, Kernel-Stack, Handles). Es gibt gar keinen Zustand, in dem ein
  toter Prozess noch Ressourcen hält. Stirbt der Elternteil, verfallen
  ungelesene Ergebnisse mit ihm — kein Waisen-Aufsammler nötig. Der Puffer ist
  ein FESTES Feld, keine Allokation im Syscall-Pfad.
  Neu ist auch **`ende_vermerken` als DIE eine Stelle**, an der ein Prozess
  endet: Vorher stand „Beendet setzen" an drei Stellen (exit, Absturz, Stopp),
  und mit der Eltern-Beziehung kam ein vierter Schritt dazu — drei Kopien
  davon wären drei Gelegenheiten, ihn zu vergessen.
- **Fünf neue Syscalls** (7..11, docs/syscalls.md §8b): `lese` (Strom-Gegenpart
  zu `schreibe`, 0 = Dateiende), `warte`, `beende`, `pipe` (beide Handles in
  einem Register — die ABI hat nur eines), `starte`. **`schreibe` kann seit
  jetzt ebenfalls blockieren** (volle Pipe).
- **HANDLE-WEITERGABE:** `starte` nimmt zwei Handles, die im Kind zu 0 und 1
  werden. `ERBE_KEINS` ist `u64::MAX` und bewusst nicht 0 — **0 ist ein
  gültiges Handle**, und ein Sonderwert mit Doppelbedeutung ist die Sorte
  Falle, die man später teuer bezahlt.
- **DIE SHELL WIRD ZUR SHELL.** Bisher schrieb ein Programm auf Handle 1, und
  der KERNEL druckte für es. Jetzt legt die Shell eine Pipe an, gibt das
  Schreib-Ende als Handle 1 weiter, liest heraus und druckt selbst. Für das
  Programm ändert sich nichts — für das System alles: Die Ausgabe ist ein
  **Datenstrom** statt eines Seiteneffekts, und was ein Strom ist, kann man
  umleiten. Daraus wird `a | b`.
  Dass die Shell **während** der Laufzeit liest (nicht danach), ist kein
  Detail: Eine Pipe fasst 4 KiB, „erst warten, dann Ausgabe abholen" wäre ein
  Deadlock, sobald ein Programm mehr produziert.
- **Strg+C** beendet den Vordergrund. Es geht NICHT in die Tasten-Queue: Die
  Shell steckt beim Warten mitten in einem synchronen Befehl und käme dort gar
  nicht heran — das Signal käme frühestens an, wenn es nichts mehr abzubrechen
  gibt. Der Eingabe-Router setzt deshalb ein Flag **pro Sitzung** (zwei
  Terminals sollen sich nicht gegenseitig abschiessen), das die Pump-Schleife
  abfragt.
- **Task-Manager beendet echte Prozesse** — mit Bestätigungsdialog. Der
  Unterschied steht jetzt auch in der App: „Task beenden" ist eine **Bitte**
  (der Task fällt am nächsten await-Punkt), „Prozess beenden" ist eine
  **Tatsache** (er wird nicht mehr eingeplant, ob er will oder nicht). Genau
  das ist der Gewinn der Präemption.
- **Drei neue Programme:** `zaehle` (die linke Pipe-Hälfte), `filter` (die
  rechte — das erste SpeedOS-Programm, das etwas liest, das keine Datei ist)
  und `elternprobe`, das aus **Ring 3** ein Kind startet und auf es wartet.
- **Beweise (`tests/zusammenspiel.rs`, 13 Tests, echt in QEMU):** beide
  warte/exit-Reihenfolgen (Kind zuerst → Ergebnis gepuffert; Eltern zuerst →
  blockiert), aus Ring 3 UND kernelseitig; zweites `warte` wird abgelehnt;
  `beende` räumt über drei Durchgänge **byte-exakt** ab, auch bei einem
  Prozess, der nie kooperiert; die Pipe blockiert und weckt an **beiden**
  Enden — nachgemessen am Zustand `Wartend` und daran, dass ein wartender
  Prozess **keine CPU verbraucht** (der Unterschied zwischen „blockieren" und
  „in einer Schleife nachsehen"); Handle-Weitergabe; `zaehle 20 | filter 7`
  von Hand und durch die Shell, inklusive dreistufiger Pipeline und
  Fehlerfällen. Dazu 6 Pipe-Unit-Tests.

### Serie 6, Teil 5: ECHTE PROGRAMME — SpeedOS führt fremden Code aus
- **DER MEILENSTEIN.** `starte /platte/programme/netzhole http://example.com`
  holt eine Webseite aus dem Internet. Das klingt banal und ist es nicht: Es
  ist ein **eigenständiges Programm**, getrennt übersetzt und gelinkt, **von
  der eigenen Platte geladen**, in **seinem eigenen Adressraum**, in **Ring 3**
  — und es holt die Seite über **unseren eigenen Netzwerk-Stack**. Vom
  `int 0x80` bis zum Ethernet-Frame ist kein Byte geliehen. Nachgemessen in
  `tests/programme.rs`: 571 Byte von example.com.
- **Bis hierhin lag ALLER User-Code im Kernel-Image** — hand-assemblierte
  Byte-Folgen mit fest eingesetzten Adressen. Ab jetzt liest der Kernel eine
  **Datei**, versteht ihr Format und macht daraus einen laufenden Prozess.
  Damit ist SpeedOS keine geschlossene Veranstaltung mehr.
- **`src/elf.rs` — der ELF64-Lader.** Lädt statisch gelinkte `ET_EXEC` für
  x86-64; dynamisches Linken ist **bewusst draussen** (`ET_DYN`/`PT_INTERP`
  bekommen eigene Fehler, damit man einen versehentlichen PIE-Build sofort
  erkennt). Die Haltung der Datei: **jede Zahl in der Datei ist eine
  Behauptung eines Fremden.** Geprüft werden Dateigrenzen (mit `checked_add`,
  ein Offset nahe `u64::MAX` darf nicht „hinten wieder rauskommen"),
  Adressen (jedes Segment muss vollständig im Programm-Bereich liegen —
  Kernel-Adressen, Nullseite und obere Hälfte fallen raus, **bevor** die erste
  Seite gemappt wird), Grössen, Ausrichtung, Überlappungen und der
  Einsprungpunkt (muss in ausführbarem, aus der Datei geladenem Code liegen).
  `pruefen()` ist eine **reine Funktion auf `&[u8]`** — kein Adressraum, kein
  Lock, kein einziges `unsafe`, und sie **panickt nie**.
- **W^X ist keine Behauptung, sondern steht in den Page Tables.** Jedes
  Segment wird mit genau seinen Rechten gemappt; ein Segment mit `PF_W|PF_X`
  wird abgelehnt. Neu dafür: das **NX-Bit** (`memory::nx_aktivieren` schaltet
  EFER.NXE ein — mit der Falle im Kommentar, dass Bit 63 ohne NXE als
  *reserviert* gilt und jeden Zugriff zum Page Fault machen würde) und
  `adressraum::Rechte` mit getrennten Flags für Blatt und Zwischentabellen.
  **Der Stack ist jetzt ebenfalls NX.** `tests/programme.rs` sieht in den
  Page Tables nach, statt es zu glauben.
  Folgerichtig lehnt der Loader auch **überlappende Segmente auf Seiten-Ebene**
  ab: Zwei Segmente in einer Seite müssten sich die Rechte teilen — und ein
  RW-Segment plus ein R-X-Segment in derselben Seite wären faktisch RWX.
- **`.bss` fällt aus einer Sicherheitsmassnahme ab.** `p_memsz > p_filesz`
  wird nicht eigens genullt: Jeder frisch gemappte Frame ist ohnehin genullt,
  damit kein Byte des Vorbesitzers nach Ring 3 leckt. Der Test misst es
  trotzdem nach (64 KiB in `netzhole`, auch weit hinten).
- **`prozess_starten(pfad, argumente)`** — der ganze Weg in einer Funktion:
  Datei lesen → prüfen → Adressraum → Segmente mappen und füllen → Stack mit
  Guard-Page → **Argumente auf den User-Stack** → PCB → einplanen.
  Argument-Übergabe: `argc` in `rdi`, `argv` in `rsi` als Feld von
  `(Zeiger, Länge)`-Paaren — **nie nullterminiert**, dieselbe Regel wie in der
  ganzen ABI (docs/syscalls.md §9).
- **Exit-Codes.** `ProzessEnde` unterscheidet `Beendet(code)`, `Abgestuerzt`
  (139) und `Gestoppt` (143) — ein Exit-Code allein könnte das nicht.
  `scheduler::warten_auf` holt ihn ab und erntet den Prozess gleich selbst.
- **`userland/` — die andere Seite der Grenze.** Ein eigener Workspace mit
  **libspeed** (Syscall-Wrapper, `print!`, Datei-/Socket-Funktionen,
  Panic-Handler, `_start`-Runtime, `no_std`) und drei Programmen: **hallo**,
  **kopiere** (echtes Datei-Werkzeug über Syscalls) und **netzhole**.
  libspeed hat **keine** Kernel-Abhängigkeit — die ABI-Konstanten stehen dort
  noch einmal, und das ist der Punkt: **Eine ABI ist ein Vertrag, kein
  geteilter Header.**
- **Zwei hart erkämpfte Bau-Lektionen** (beide im Code dokumentiert):
  (1) `relocation-model=static` erzeugt absolute 32-Bit-Adressen — bei einem
  Ladeort von 512 GiB scheitert der Linker mit hunderten
  `R_X86_64_32S out of range`. Die Voreinstellung (`pic`, RIP-relativ) läuft
  an jeder Adresse und ist sogar kürzer. (2) Ohne `--no-pie` entsteht ein
  `ET_DYN` — und schlimmer: Der PIE-Link zieht `.dynsym`/`.rela.dyn`/
  `.dynamic` als **Waisen-Sektionen** direkt hinter `.text` und zerlegt damit
  die sorgfältig ausgerichtete Segment-Folge.
- **DIE FALLE, DIE DEN MEILENSTEIN AUFHIELT — und die einen ganzen Fehler-Typ
  erledigt:** Jede synchrone Warteschleife des Netz-Stacks endete auf `hlt()`.
  Völlig richtig für Kernel-Kontext. Aus einem **Syscall** heraus ist es ein
  Totalausfall: `int 0x80` geht durch ein Interrupt-Gate, IF ist also aus, und
  `hlt` mit ausgeschalteten Interrupts hält die CPU **für immer** an — kein
  Timer, kein Netz, keine Meldung. Genau das passierte, als `netzhole` den
  Syscall `aufloesen` rief. Neu: **`zeit::warte_auf_interrupt()`** sieht nach,
  in welchem Kontext es läuft, und öffnet bei ausgeschalteten Interrupts ein
  Wartefenster. Alle fünf Stellen in `dns`/`dhcp`/`http` benutzen es jetzt.
- **Build-Integration ohne Host-Werkzeug für SpeedFS.** Das `build.rs` des
  Kernels baut `userland/` mit (eigener Ziel-Baum, damit der innere
  cargo-Aufruf nicht auf die Dateisperre des äusseren wartet; geerbte
  `RUSTFLAGS` werden weggeräumt), die ELFs werden per `include_bytes!`
  eingebettet, und `programme::installieren()` schreibt sie beim Boot nach
  `/platte/programme` — byteweise verglichen, also nur bei echter Änderung.
  Dadurch reisen die Programme mit `cargo run`, `cargo test` **und**
  `cargo image` (USB-Stick) mit, ohne eine Zeile im Runner. Der Preis sind
  ~70 KiB im Kernel-Image; ein Host-seitiger SpeedFS-Writer wäre eine
  dauerhafte Doppelpflege gewesen.
- **Shell:** `starte <programm> [args]` (mit Exit-Code-Anzeige und Kurznamen
  ohne Pfad), `programme`, `elfinfo` (zeigt Segmente und Rechte — Diagnose-
  und Lehrwerkzeug). **Explorer:** Doppelklick auf eine ausführbare Datei
  startet sie; entschieden wird an den **ersten Bytes**, nicht am Namen (unser
  VFS kennt keine Endungen, und eine Endung wäre auch nur eine Behauptung).
- **Beweise (`tests/programme.rs`, 14 Tests, echt in QEMU):** Lebenszyklus
  über 5 Läufe mit **byte-exakter Frame-Bilanz**; Exit-Codes 0/1/7/42/255 aus
  Ring 3; argv kommt an; `kopiere` kopiert 10 000 Byte byte-identisch;
  Segment-Rechte und NX in den Page Tables nachgesehen; 15 kaputte/bösartige
  Programmdateien (abgeschnitten an sechs Stellen, falsche Magie, 32 Bit,
  ET_DYN, Einsprung im Kernel, Segment im Kernel, `u64::MAX`-Grösse, W+X) —
  alle abgelehnt, **ohne einen einzigen geleckten Frame**; Absturz und Stopp
  räumen vollständig auf und der Kernel läuft weiter; zwei Programme an
  denselben Adressen in getrennten Welten; die Shell-Befehle end-to-end.
  Der ELF-Parser selbst wird zusätzlich in 12 Unit-Tests zerlegt.

### Serie 6, Teil 4: DIE SYSCALL-ABI — jetzt zahlen die Nähte
- **Der Sprung:** Aus „der Kernel tut alles selbst" wird „der Kernel wird
  **gebeten**". Hinter INT 0x80 steht jetzt eine echte **Syscall-Tabelle** mit
  22 Nummern in drei Gruppen, einem einheitlichen Fehler-Enum und einer
  dokumentierten ABI: **`docs/syscalls.md`**. Ab jetzt gilt: Eine Änderung an
  dieser Tabelle ist eine bewusste **ABI-Änderung** — zwei Tests nageln
  Nummern und Struktur-Layouts fest, wer eine Zahl verschiebt, bricht sie.
- **DIE NÄHTE HABEN GEHALTEN.** Die ehrliche Bilanz dessen, was seit Serie 4
  bewusst gelegt wurde:
  - **Socket-API (Serie 5): 1:1 durchgereicht.** `oeffnen`, `senden`,
    `empfangen`, `schliessen`, `zustand`, `aufloesen` — **kein Zeichen**
    Änderung in `src/netz/socket.rs`. Aus „Slice, den die Kernel-Shell stellt"
    wurde „Slice, den der Kernel aus User-Speicher kopiert", genau wie in der
    Bestandsaufnahme vorhergesagt.
  - **VFS (Serie 3/4): 1:1 durchgereicht.** `read_at`/`write_at` lieferten
    schon Byte-Zahlen statt Panics, `stat` schon reine Daten, `FsFehler` war
    schon vollständig. Es kam nur die Grenze DAVOR.
  - **NACHGESCHÄRFT WERDEN MUSSTE GENAU EINES:** die **Semantik** von
    `verbinde`. `socket::verbinden` ist nicht-blockierend — es startet den
    Handshake, und wer wissen will, ob er klappt, muss den Stack „pumpen". Ein
    Ring-3-Programm **kann** das nicht (Pumpen ist Kernel-Innenleben und wird
    nie ein Syscall). Also pumpt der **Syscall** selbst, mit 8-s-Frist. Keine
    Änderung *an* der API, sondern eine Entscheidung *darüber* — dokumentiert,
    damit klar ist, dass dieser eine Syscall Sekunden dauern kann.
- **Aufruf-Konvention:** Nummer in `rax`, Argumente in `rdi/rsi/rdx/r10`,
  **Fehlercode in `rax`, Ergebnis in `rdx`**. Zwei Rückgabe-Register statt
  Linux' „negativer errno", weil ein Ergebnis jeden u64-Wert annehmen darf
  (Uhrzeit, IP, Dateigröße) — kein Bit muss für „ist das ein Fehler?"
  reserviert werden.
- **Zeiger IMMER als (Zeiger, Länge), NIE nullterminiert.** Der Kernel sucht
  nie ein Terminator-Byte im User-Speicher — das wäre ein Lesevorgang
  unbekannter Länge in fremdem Speicher. Die Fehlerklasse „Pfad ohne
  Nullterminierung" existiert in dieser ABI deshalb gar nicht.
- **`Fehler`-Enum (25 Codes) als einzige Außensicht.** `FsFehler`, `IoFehler`,
  `SocketFehler`, `DnsFehler` und `CopyFehler` werden darauf ABGEBILDET — ein
  Prozess erfährt nie, ob unter ihm SpeedFS, FAT32 oder ein RamFs liegt
  (`KeinSpeedFs`/`KeinFat32` → beide `NichtKonfiguriert`). Zeiger-Fehler
  werden absichtlich GROB abgebildet: Ob eine Adresse gemappt ist, ist eine
  Information über den Kernel-Zustand.
- **PER-PROZESS-HANDLE-TABELLE** (`src/syscall/handle.rs`) — die letzte offene
  Lücke aus der Bestandsaufnahme (b). Ein Handle ist ein INDEX in die eigene
  kleine Tabelle: Dieselbe Zahl bedeutet in jedem Prozess etwas anderes, und
  das GLOBALE Socket-Handle verlässt den Kernel nie. Handle 0/1/2 sind
  reserviert (Eingabe / **Ausgabe: Bildschirm+seriell** / **Diagnose: nur
  seriell**) und gehören dem Kernel. Der getrennte Diagnose-Kanal ist kein
  Luxus: Ein Prozess, der tausende Zeilen protokolliert, würde über Handle 1
  den Compositor überschwemmen.
- **AUTOMATISCHES AUFRÄUMEN.** Die Tabelle steckt IM Prozess-Kontrollblock,
  also schließt ihr `Drop` beim Prozess-Ende alles Offene — inklusive
  geordnetem TCP-Abbau. Kein Pfad kann es vergessen. Bewiesen für den
  regulären `exit` UND für den ABSTURZ.
- **Gruppe 0 (Prozess/Ausgabe):** `exit`, `yield`, `getpid`, `schreibe`,
  `schlafe`, `zeit_jetzt` (monoton), `zeit_epoche` (Wanduhr).
- **Gruppe 1 (Dateien):** `oeffne`, `lese_at`, `schreibe_at`, `schliesse`,
  `stat`, `liste`, `loesche`, `umbenenne`, `mkdir`. Zwei ehrliche Folgen des
  pfadbasierten VFS stehen in der Doku statt in einer Ausrede: Ein Handle
  merkt sich den **Pfad** (nach `umbenenne` liefert es `NichtGefunden` — der
  Test prüft genau das), und es gibt **keine Dateiposition** (deshalb hängt
  `schreibe` auf ein Datei-Handle an).
- **Gruppe 2 (Netz):** `socket`, `verbinde`, `sende`, `empfange`, `aufloesen`,
  `socket_zustand`. `empfange` bleibt bewusst NICHT-blockierend (0 = noch
  nichts) — blockierend bräuchte das Warte-Modell, und Pollen ist ehrlicher
  als ein verstecktes Timeout.
- **DIE NEUE LOCK-DISZIPLIN** (docs/syscalls.md §8) — das war die eigentliche
  Denkarbeit. Ein Syscall läuft mit ausgeschalteten Interrupts, also (a) sind
  Locks, die der Kernel nur mit `without_interrupts` hält, gefahrlos
  benutzbar, aber (b) auf einen Lock WARTEN ist ein Hänger. `fs::mit_fs` ist
  genau so ein Lock. Zwei Bausteine lösen das: **`warte_fenster()`**
  (Interrupts an, `hlt`, aus — nur hier darf gewechselt werden, und nur ohne
  Lock in der Hand) und **`mit_vfs()`** (try_lock + Wartefenster, bis 50
  Versuche, dann `Belegt`). Nebenbei: `hlt` mit ausgeschalteten Interrupts
  wäre ein Stillstand für immer.
- **DER PRÜFSTAND** (`prozess::pruefstand_programm`) — ein 75-Byte-Ring-3-
  Programm als **Fernbedienung**: Es liest Syscall-Nummer und Argumente aus
  seinem eigenen Speicher, löst `int 0x80` aus und legt Fehlercode und
  Ergebnis dort ab; zwischen zwei Aufträgen schläft es per `schlafe(1)`.
  Damit läuft jeder Testfall als gewöhnlicher Rust-Code, während der Aufruf
  ECHT unprivilegiert ist — eigener Adressraum, eigene Handle-Tabelle, echte
  dreistufige Zeigerprüfung. Ein Angriff im Test ist ein echter Angriff.
- **BEWEISE** (`tests/syscalls.rs`, 8 Tests, echt in QEMU):
  - Gruppe 0/1/2 im Erfolgsfall, inklusive **copy-OUT**: Der Kernel schreibt
    `lese_at`- und `stat`-Ergebnisse in den Prozess, der Test liest sie aus
    dessen (inaktivem) Adressraum zurück und vergleicht byteweise.
  - **Angriffe:** unbekannte Nummern (auch `u64::MAX`), Kernel-Zeiger,
    Nullzeiger, ungemappte Adressen, Zeiger über die Seitengrenze, Längen von
    0 bis `u64::MAX`, relative Pfade, kaputtes UTF-8, Pfade über dem Deckel,
    Offsets über 1 GiB, fremde/geschlossene/nie vergebene/„negative" Handles,
    reservierte Handles, falscher Handle-Typ in beide Richtungen,
    Nur-Lesen/Nur-Schreiben-Verstöße. **Jeder** liefert einen Fehlercode,
    keiner eine Panik.
  - **HANDLE-ISOLATION aus Ring 3:** Zwei Prozesse bekommen beide Handle 3 —
    einer eine Datei, einer einen Socket. Prozess B probiert alle 32
    Handle-Zahlen durch und erreicht A's Datei mit keiner.
  - **LECK-TEST:** 5 Sockets öffnen, `exit` ohne ein einziges `schliesse` →
    alle automatisch zu; danach dasselbe mit einem ABGESTÜRZTEN Prozess. Der
    Nachbarprozess macht danach weiter Syscalls. Frame-Bilanz byte-exakt null.
- **Ein eigener Fehler, ehrlich notiert:** Die erste Fassung des Leck-Tests
  hat ein Leck GEMELDET, wo keins war — `socket::schliessen` markiert nur, aus
  der Tabelle fliegen die Einträge erst beim nächsten `aufraeumen` (das in
  `oeffnen`/`bedienen` steckt). Die Messdisziplin steht jetzt als Kommentar im
  Test, damit der nächste Leser nicht dieselbe Falle findet.
- **Zwei ABI-Folgen, die eigene Tests aufgedeckt haben** (statt sie zu
  übersehen): (1) `rax` und `rdx` sind AUSGABE-Register und werden **nicht**
  erhalten — der Kontext-Sicherungs-Test prüft sie jetzt getrennt, und die
  Doku sagt es ausdrücklich. (2) Der Zähler-Demo-Prozess benutzt jetzt
  `SYS_SCHREIBE` (Nummer 3); der „gibt nie ab"-Test prüft die Abgabe-Nummern
  über die ABI-Konstanten, damit er bei künftigen Änderungen mitwandert.
- **Bewusst NICHT dabei** (docs/syscalls.md §9): `fork`/`exec` (ohne
  ELF-Loader gäbe es nichts zu exec-en), blockierendes `lese`/`empfange` und
  `select` (brauchen das Warte-Modell), Arbeitsverzeichnis, **Fenster-
  Syscalls** (ein Zeichenbefehl aus Ring 3 darf den MANAGER-Lock nicht lange
  synchron halten — das braucht eine Kommando-Warteschlange pro Fenster),
  Rechte/Benutzer, `dup`/Handle-Vererbung.

### Serie 6, Teil 3: DER PRÄEMPTIVE SCHEDULER — der PIT wird zum Herz
- **Der Sprung:** Multitasking war in SpeedOS immer KOOPERATIV — ein Task
  lief, bis er `await`-te. Ab jetzt kann der Kernel einem Programm die CPU
  **wegnehmen**. Bewiesen mit zwei Ring-3-Prozessen, die in Endlosschleifen
  zählen und deren Maschinencode **keinen einzigen Abgabe-Syscall** enthält —
  und die trotzdem beide vorankommen.
- **DIE ARCHITEKTUR-ENTSCHEIDUNG, vor dem Code aufgeschrieben**
  (`docs/scheduler-design.md`, wie schon `speedfs-format.md` und
  `tcp-scope.md`): Der kooperative Executor wird NICHT ersetzt, sondern ist
  **selbst ein schedulebarer Kontext — der Kernel-Prozess PID 0**. Er steht
  als normaler Eintrag in der Prozess-Tabelle und bekommt seine Zeitscheibe
  wie jeder User-Prozess; *innerhalb* seiner Scheibe multiplext er weiter
  kooperativ zwischen Compositor, netz_task, Shell-Sitzungen & Co.
  ```
   praeemptiv (PIT, 20 ms):  PID 0 -> PID 1 -> PID 2 -> PID 0 -> ...
                               |
                               +-- kooperativ (await): Compositor, Netz, Shell
  ```
  Drei Dinge fallen dadurch geschenkt an: **(1)** EIN Wechsel-Mechanismus,
  keine Sonderfälle „Kernel vs. Prozess"; **(2)** der **Leerlauf bleibt, wie
  er war** — PID 0 mit leerer Task-Queue schläft per `hlt`, er IST der
  Idle-Prozess, ein separater wäre überflüssig (und „nichts lauffähig" kann
  es nicht geben, denn PID 0 ist immer lauffähig); **(3)** die Oberfläche
  verhungert nicht. Vier Alternativen sind im Entwurf begründet verworfen.
- **DER KONTEXT-WECHSEL** (`src/prozess.rs`, `src/scheduler.rs`) — die
  Kernidee macht ihn erstaunlich klein: **Ein gesicherter Prozess-Kontext ist
  EINE ZAHL.** Jeder Prozess hat einen eigenen Kernel-Stack; beim Trap legt
  die CPU dort RIP/CS/RFLAGS/RSP/SS ab (bei Traps aus Ring 3 dank
  `TSS.RSP0`), unser Assembler-Einstieg die 15 General-Register dahinter.
  Zusammen ist das der vollständige Zustand — und er liegt auf SEINEM Stack,
  wo er beliebig lange liegen bleiben darf. „Umschalten" heißt deshalb: `RSP`
  auf den Rahmen des anderen Prozesses setzen, poppen, `iretq`.
- **DREI EINSTIEGE, EIN AUSSTIEG.** `timer_entry` (Präemption),
  `syscall_entry` (INT 0x80, freiwillige Abgabe) und `prozess_sterben`
  (Ring-0-Stub nach einem Fault) sichern jeweils den Rahmen, rufen ihren
  Rust-Dispatcher — und der **liefert einen Rahmen zurück**. Geladen wird er
  ausschließlich von `schalte_auf_rahmen`, drei Assembler-Zeilen, der einzige
  Kontext-Lade-Punkt des Kernels. Präemption, Yield, Prozess-Start und
  Prozess-Tod laufen damit durch denselben Pfad.
- **Der Timer ist kein `extern "x86-interrupt"`-Handler mehr**, sondern ein
  nackter Assembler-Einstieg — eine gewöhnliche Rust-Funktion kann ihren
  Stack-Pointer nicht umbiegen. Die alte Basisarbeit (Tick-Zähler,
  Weck-Waker, EOI) steht unverändert als `interrupts::timer_basisarbeit()`,
  damit sichtbar bleibt: daran hat sich nichts geändert.
- **Prozess-Start ist kein Sonderfall.** Ein neuer Prozess bekommt von Hand
  einen Trap-Rahmen an das obere Ende seines Kernel-Stacks geschrieben — er
  sieht aus, als wäre er schon gelaufen und gerade verdrängt worden. Daraus
  folgt **Invariante 1**: Der erste Wechsel zu einem Prozess passiert IMMER
  im Timer (oder Syscall), also dort, wo der Kontext von PID 0 ohnehin
  gesichert wird. Es gibt keinen Pfad, auf dem PID 0 ohne gesicherten Kontext
  verlassen wird.
- **`src/prozess.rs`** — PCB (PID, Name, Zustand, Adressraum, Kernel-Stack,
  gesicherter Kontext, Startzeit, CPU-Zeit, Präemptionen, Abgaben, Syscalls)
  und die Tabelle als **festes Array `[Option<Prozess>; 8]`** — kein `Vec`,
  denn der Timer-Interrupt liest sie und darf nicht allozieren. Jeder
  Kernel-Stack hat eine **Guard-Page** (5 Seiten mappen, die unterste sofort
  aushängen).
- **Round-Robin, 5 Ticks = 20 ms** bei 250 Hz. Die Entscheidung ist eine
  **reine Funktion** `wechsel_entscheiden(zustaende, aktuell,
  scheibe_abgelaufen, freiwillig)` mit vier Regeln — Fairness ist dadurch
  über 200 Runden nachgerechnet statt geglaubt (jeder von 4 Lauffähigen
  bekommt exakt 50 Scheiben, in strikter Reihum-Folge).
- **Lock-Disziplin, die neue Gefahrenstelle:** Aus Kernel-Kontext wird die
  Tabelle immer mit `without_interrupts` gesperrt — während der Sperre kann
  der Timer gar nicht feuern. Im Interrupt selbst nur `try_lock`: kein
  Wechsel in diesem Tick ist harmlos, der nächste kommt in 4 ms. Und
  **beendete Prozesse werden NIE im Interrupt abgeräumt** (Freigeben nimmt
  Locks und Heap) — der Timer markiert, ein Kernel-Task („Prozess-Aufräumer")
  räumt auf, und zwar erst außerhalb des Tabellen-Locks.
- **Dauerregel II jetzt PROZESS-WEISE.** Ein Page Fault aus Ring 3 setzt den
  Prozess auf `Beendet` und biegt den Interrupt-Rahmen auf einen
  Ring-0-Sterbe-Stub um (Kernel-Stack des Sterbenden); der schaltet auf den
  nächsten lauffähigen Prozess. Bewiesen: **PID 7 stirbt, PID 6 rechnet
  weiter, der Kernel lebt** — und alle Frames kommen zurück.
- **Neue Syscalls:** 3 = `schlafen(ms)` (der erste BLOCKIERENDE — Zustand
  `Wartend`, der Timer weckt), 4 = `yield`, 5 = `getpid`. `yield` benutzt
  **der Executor selbst**: Findet `sleep_if_idle` keine Arbeit, aber es gibt
  lauffähige Prozesse, gibt er die Scheibe sofort ab statt zu `hlt`-en. Die
  hlt-Messung rechnet die **Fremdzeit** heraus, sonst zeigte der Task-Manager
  0 %, während zwei Prozesse rechnen.
- **Der alte Einzelschuss-Pfad (`ring3test`) bleibt** — er führt Ring-3-Code
  im Kontext von PID 0 mit fremdem CR3 aus, verträgt also keinen Wechsel.
  Also **sperrt** er die Planung (`scheduler::sperre_erhoehen`). Dieselbe
  Falle steckte im `adressraum`-Befehl (CR3 juggeln in der Shell) — auch
  dort jetzt gesperrt. Bewiesen, dass beides koexistiert.
- **Task-Manager zeigt jetzt ZWEI Tabellen** und macht die Architektur
  sichtbar: oben PROZESSE (PID, Name, Zustand, CPU-Zeit, Präemptionen) —
  präemptiv, eigener Adressraum; unten KERNEL-TASKS — kooperativ, alle
  innerhalb von PID 0.
- **Shell:** `prozesse`, `prozess-start [zaehler <Kennung> | schlaefer <ms> |
  absturz]`, `prozess-stop <pid>|alle`, `praemptionstest [sekunden]` (fällt
  am Ende ein maschinelles Urteil).
- **BEWEISE** (`tests/scheduler.rs`, `tests/scheduler_executor.rs`, echt in
  QEMU):
  - **Präemptions-Beweis:** 2 Zähler-Prozesse, 1,5 s. Gemessen PID 2:
    521 352 µs / **26 Präemptionen aus Ring 3** / **0 Abgaben**, PID 3:
    526 047 µs / 26 / 0 (Round-Robin-Fairness bis auf 1 %!), Ausgabe-Spur
    verschränkt. Wer nie abgibt und trotzdem vorankommt, dem wurde die CPU
    weggenommen.
  - **Kontext-Sicherung gegen synthetische Registersätze:** Ein Stub lädt alle
    15 Register mit Magic-Werten, löst `int 0x80` aus; geprüft wird der
    SAVE-Pfad (gesicherter Rahmen) UND der RESTORE-Pfad (Register danach).
    Dazu `TrapFrame`-Layout per `offset_of!` auf alle 20 Offsets festgenagelt
    (`size == 160` — nur so stimmt die C-ABI-Ausrichtung am `call`).
  - **SSE bleibt unberührt:** XMM0-XMM15 mit Mustern füllen, sich über 24
    Kontext-Wechsel verdrängen lassen, nachmessen. Der nackte Einstieg
    sichert nur GP-Register — dass der Kernel wirklich fließkomma-frei
    bleibt, ist damit gemessen statt gehofft.
  - **Koexistenz mit dem ECHTEN Executor:** Kernel-Task schafft 20
    kooperative Runden in 1242 ms (Soll 1000) neben einem endlos rechnenden
    Prozess mit 60 Präemptionen — die Kernel-Welt verhungert nicht.
  - **`Wartend` ist echt:** Schläfer 26 µs CPU gegen Dauerrechner 600 899 µs.
  - **Frame-Bilanz byte-exakt null** nach jedem Test — auch nach Abstürzen.
- **Zwei eigene Fehler, unterwegs gefunden und behoben:** (1) Die statischen
  Kernel-Stacks landeten ohne `mut` in `.rodata` — ein schreibgeschützter
  Stack heißt Page Fault → Double Fault → **Triple Fault ohne jede Meldung**
  (Lektion steht jetzt im Code). (2) `spur_auswerten` zählte Beteiligte über
  `pid as usize` als Array-Index — eine PID ist aber KEIN Tabellen-Index, sie
  wächst monoton weiter; bei PID 12/13 verschluckte das den halben Beweis.
- **Bewusst NICHT dabei** (`docs/scheduler-design.md` §8): Prioritäten (reines
  Round-Robin — 2 rechnende Prozesse drücken den Desktop auf ~1/3 CPU, das
  ist korrekt und nicht versteckt), blockierende VFS-/Socket-Syscalls (dafür
  braucht es erst das Warte-Modell: `fs::mit_fs` sperrt OHNE Interrupts
  auszuschalten und wäre aus einem Syscall ein Deadlock-Risiko), SMP/APIC
  (unverändert vertagt), PCID, Einsammeln leerer Page-Tables.

### Serie 6, Teil 2: EIGENER ADRESSRAUM PRO PROZESS — echte Isolation
- **Der zweite große Schritt in den User-Space.** Bis eben lief Ring-3-Code
  zwar unprivilegiert, aber in DENSELBEN Page Tables wie der Kernel — zwei
  Programme hätten sich gegenseitig gesehen. Jetzt bekommt jeder Prozess
  seine **eigene Level-4-Tabelle**: Was nicht in seinen Tabellen steht,
  EXISTIERT für ihn nicht.
- **`src/adressraum.rs`** — das neue Modul. Grundprinzip **„Kernel spiegeln,
  User privat"**: Beim Anlegen werden die Kernel-P4-EINTRÄGE (8-Byte-Zeiger
  auf GETEILTE P3-Tabellen, nicht die Tabellen selbst!) in die neue P4
  kopiert. Das MUSS sein, weil ein Interrupt jederzeit mitten im User-Code
  zuschlägt und die CPU dabei **nicht** CR3 wechselt — ohne Kernel-Mapping
  wäre das ein Triple Fault.
- **EHRLICHE ABWEICHUNG VOM LEHRBUCH.** Lehrbücher sagen „spiegle die obere
  Adressraumhälfte". Bei uns wäre das fatal: `bootloader_api` 0.11 legt mit
  `Mapping::Dynamic` **alles in die UNTERE Hälfte**. Nachgemessen im
  laufenden System:
  ```
  P4[0]   Bootloader-/Frühmappings   P4[5]   Physik-Komplettmapping
  P4[2,3] Kernel-Image               P4[6,7] Stack, BootInfo, Framebuffer
  P4[4]   ...                        P4[136] Kernel-Heap (0x4444_4444_0000)
  ```
  Die obere Hälfte ist **komplett leer**. Also spiegeln wir jeden belegten
  Kernel-Slot; privat ist genau **P4-Slot 1** (512 GiB .. 1 TiB) — der
  einzige freie Slot. Die Messung stand vor dem Code, nicht die Annahme.
- **Die Feinheit, die es robust macht:** Weil wir P4-EINTRÄGE kopieren
  (Zeiger auf gemeinsame P3-Tabellen), sind spätere Kernel-Mappings
  *innerhalb* schon gespiegelter Slots — etwa `heap_erweitern` — automatisch
  in allen Adressräumen sichtbar. Nur ein komplett NEUER Kernel-Slot wäre es
  nicht, deshalb frischt `aktivieren()` den Spiegel jedes Mal auf (511
  Einträge kopieren, ein Wimpernschlag).
- **Besitz und Abriss.** `eigene: Vec<PhysFrame>` führt Buch über die P4,
  ALLE Zwischentabellen (ein `BuchAllocator` notiert auch die, die `map_to`
  im Verborgenen anlegt) und alle Datenseiten. `Drop` schaltet nötigenfalls
  erst auf den Kernel-Adressraum zurück (Tabellen unter den eigenen Füßen
  freizugeben wäre der schnellste Weg zum Triple Fault) und gibt exakt diese
  Frames frei. Kernel-Frames sind nur *gespiegelt*, stehen nicht in `eigene`
  und werden nie angefasst.
- **API:** `map_benutzer` (PRESENT|WRITABLE|USER_ACCESSIBLE — und der Frame
  wird **vorher genullt**, sonst leckt der Inhalt des Vorbesitzers nach
  Ring 3), `bereich_mappen`, `stack_anlegen`, `schreiben`/`lesen` über das
  Physik-Komplettmapping **ohne Aktivierung** (genau das Muster des künftigen
  ELF-Loaders), `seiten_flags` auch für INAKTIVE Räume, `aktivieren` /
  `kernel_aktivieren` (CR3), `abreissen`.
- **User-Stack mit GUARD-PAGE.** `stack_anlegen(top, seiten)` mappt die
  Stack-Seiten und lässt die Seite darunter **bewusst ungemappt**. Ein
  Stack-Überlauf gibt damit sofort einen Page Fault, statt still den
  darunterliegenden Speicher zu zerschreiben — der übelste Fehlerfall
  überhaupt, weil er erst viel später und ganz woanders auffällt.
- **copy-in/copy-out ausgebaut** (`src/ring3.rs`) — die kritischste
  Sicherheitsfläche des Kernels, jetzt **dreistufig** in dieser Reihenfolge:
  **(a) Bereich** — liegt [ptr, ptr+len) vollständig im User-Bereich? Reine
  Arithmetik mit `checked_add`, erledigt Kernel-Adressen und Nullzeiger, ohne
  auch nur eine Page Table anzufassen. **(b) Mapping** — ist JEDE berührte
  Page im Adressraum des AUFRUFENDEN Prozesses (den Tabellen aus CR3!)
  gemappt und USER_ACCESSIBLE? Eine Page aus einem FREMDEN Adressraum ist
  damit schlicht ungemappt. **(c) Schreibrecht** — beim copy-OUT zusätzlich
  WRITABLE, sonst würde Ring-0-Code in eine Seite schreiben, die der Prozess
  selbst nur lesen darf. Neu: `copy_out`, `user_bereich_pruefen` und
  `copy_in_prozess`/`copy_out_prozess`, die den Adressraum explizit nennen
  und ablehnen, wenn er nicht der aktive ist. Panickt weiterhin **nie**.
- **`memory::map_page_benutzer` ist ersatzlos entfallen** — User-Speicher
  darf es im Kernel-Adressraum nie geben. Neu: `memory::kernel_p4_frame()`
  (der globale MAPPER schreibt IMMER in die Kernel-P4, egal was in CR3 steht,
  damit Kernel-Mappings nie im Adressraum eines Prozesses landen).
- **Neuer Syscall 2 = `zeit_ms(ptr)`** — der erste, der copy-OUT benutzt
  (Kernel schreibt in einen User-Puffer). Ring-3-Programme laufen jetzt in
  `prozess_aufsetzen`-gebauten Adressräumen; `nach_ring3` wechselt CR3 und
  schaltet auf BEIDEN Rückwegen (exit UND Absturz) sauber zurück.
- **Shell:** `adressraum` (Isolations-Beweis zum Zusehen) und
  `ring3test stack` (Guard-Page-Beweis).
- **DIE BEWEISE** (`tests/adressraum.rs`, echt in QEMU):
  ```
  [ADRESSRAUM-MEILENSTEIN] Adresse 0x8000100000: in A = "AAAA-ich-bin-A--",
                                                 in B = "BBBB-ich-bin-B--"
  [ADRESSRAUM-MEILENSTEIN] 5x anlegen/abreissen: 25375 von 32768 Frames frei
    (vorher 25375), Spitzenbedarf 53 Frames — Bilanz exakt null.
  EXCEPTION: PAGE FAULT — aus USER-MODE (Ring 3)
    Zugriff auf Adresse: VirtAddr(0x80000fbff8)   <- die Guard-Page
    Ursache: Seite nicht vorhanden   Zugriff: Schreibzugriff
  [ADRESSRAUM-MEILENSTEIN] Guard-Page hat den Stack-Ueberlauf gefangen.
  [ADRESSRAUM-MEILENSTEIN] Absturz aufgefangen UND der Adressraum
                           vollstaendig abgeraeumt.
  ```
  Dazu die Angriffs-Unit-Tests gegen copy-in/copy-out: Kernel-Adresse,
  Nullzeiger, obere Hälfte, Integer-Überlauf bei Zeiger+Länge, Länge über die
  Seitengrenze in ungemapptes Gebiet, und **fremder Adressraum** (Prozess B
  legt ein Geheimnis an eine Adresse, Prozess A liest dieselbe Adresse und
  bekommt `NichtGemappt`).

### Serie 6 beginnt: RING 3 — SpeedOS führt unprivilegierten Code aus
- **Der Sprung, der SpeedOS zu einem „echten" OS macht.** Bis hierher lief
  JEDER Befehl im Kernel-Privileg (Ring 0). Jetzt läuft Code in **Ring 3**,
  darf NICHT alles — und ein Absturz dort reißt den Kernel NICHT mehr mit.
  Bewusst klein: nur der Privilegienwechsel, noch KEIN ELF-Loader, KEIN
  eigener Adressraum, KEIN präemptiver Scheduler (Fahrplan:
  `docs/serie6-bestandsaufnahme.md`).
- **GDT/TSS** (`src/gdt.rs`): User-Code- und User-Data-Segmente (DPL 3;
  Selektoren mit RPL 3) plus **RSP0** in der `privilege_stack_table` — der
  Kernel-Stack, auf den die CPU bei einem Trap AUS Ring 3 automatisch
  umschaltet (16-ausgerichtet wegen SSE im Dispatcher). Die bestehende
  IST-Nutzung für Double Faults bleibt unangetastet.
- **User-Pages** (`memory::map_page_benutzer`): PRESENT|WRITABLE|
  USER_ACCESSIBLE. Wichtige Lektion: Die CPU **UND-verknüpft das U-Bit über
  alle vier Page-Table-Ebenen** — `benutzer_pfad_freischalten` setzt es
  deshalb auch auf bereits existierenden P4/P3/P2-Einträgen. Dazu
  `memory::seiten_flags` als Prüf-Grundlage für copy-in.
- **Der Übergang** (`ring3::nach_ring3`): per **`iretq`** (begründet im Code:
  braucht keine MSR-Einrichtung und keine bestimmte Segment-Anordnung wie
  `sysretq` — wir bauen einfach den Rahmen, den ein Trap aus Ring 3
  hinterlassen hätte). Der User-Code liegt als hand-assemblierter
  Maschinencode in einer User-Page.
- **Der Rückweg** — **INT 0x80** als Trap-Gate mit **DPL 3** (sonst dürfte
  Ring 3 es gar nicht auslösen; die anderen Gates bleiben DPL 0). Begründung
  im Code: einfacher und lehrreicher als SYSCALL/SYSRET, das kann später
  kommen. Der Einstieg ist nacktes `global_asm`, das **den vollen
  User-Kontext sichert** (alle General-Register als `TrapFrame`), den
  Rust-Dispatcher ruft und alles wiederherstellt. Syscall 0 = `debug_print`
  (Zeiger + Länge), 1 = `exit`.
- **KRITISCHE LEKTION (im Code dokumentiert):** Der Kernel-Kontext wird per
  **setjmp/longjmp-Muster** gesichert (`kern_setjmp` + `kern_ring3_landing`).
  Ein einzelner Inline-asm-Block mit Sprung-Label funktioniert NICHT: Der
  Rückweg kommt über einen Trap-Handler, den der Compiler nicht als
  Kontrollfluss sieht — er verwaltet die Register dann falsch (Korruption,
  #GP beim iretq). Das kostete mehrere Debug-Runden.
- **DIE ZWEI DAUERREGELN** (ab jetzt in CLAUDE.md, gelten überall):
  **(I) Der Kernel folgt niemals blind einem User-Zeiger** — `ring3::copy_in`
  prüft JEDE berührte Page auf „gemappt UND USER_ACCESSIBLE", fängt
  Längen-Überlauf und absurde Größen ab und **panickt nie**; ein User-Zeiger
  auf Kernel-Speicher wird abgelehnt.
  **(II) Ein Fehler im User-Mode reißt den Kernel nie mit** — Page Fault und
  #GP aus Ring 3 werden über `interrupts::user_recovery()` aufgefangen (der
  Interrupt-Rahmen wird auf den Kernel-Landeplatz umgebogen); nur ein Fehler
  im Kernel selbst hält an. Neu: ein **General-Protection-Fault-Handler**.
- **Shell `ring3test`** (und `ring3test absturz`) führt beide Beweise vor.
- **DIE BEWEISE** (`tests/ring3.rs`, echt in QEMU):
  ```
  Hallo aus Ring 3!
  [ring3] Sauber zurueck im Kernel (Ring 0) — System laeuft weiter.
  EXCEPTION: PAGE FAULT — aus USER-MODE (Ring 3)
    Zugriff auf Adresse: VirtAddr(0x444444440000)
    -> Der User-Code wird BEENDET, der Kernel laeuft weiter.
  [RING3-MEILENSTEIN] Kernel lebt nach dem User-Mode-Absturz weiter.
  ```
  Plus: Ring 3 läuft auch NACH dem Absturz fehlerfrei weiter (nichts kaputt
  zurückgelassen).
- Tests: copy-in gegen Kernel-Adresse, ungemappte Adresse, Längen-Überlauf und
  absurde Länge (alle sauber Fehler, nie Panik) sowie gegen eine gültige
  User-Page (inkl. Puffer, der über die Page hinausragt). 192 Lib-Tests grün
  (vorher 190) + `tests/ring3.rs`. `cargo clippy --all-targets` warnungsfrei;
  Desktop bootet unverändert sauber.

### Serie-5-Abschluss: Härtetests, unsafe-Audit, Serie-6-Bestandsaufnahme
- **Feature-Lücken geschlossen (mit Tests):**
  - **DNS-Retry**: `dns::aufloesen` sendet die Anfrage jetzt bis zu 3× erneut
    (1,2 s je Versuch) — ein einzelnes verlorenes UDP-Datagramm scheitert nicht
    mehr die ganze Auflösung.
  - **DHCP-Lease-Erneuerung**: `NetzKonfig` trägt den Lease-Startzeitpunkt;
    reine, getestete Zeit-Logik (`erneuerung_faellig`/`abgelaufen`, T1 = 50 %)
    plus ein Erneuerungs-Task, der die Lease bei T1 neu bezieht.
  - **Socket-Erschöpfung/Leak-Test**: die Tabelle läuft nicht über
    (`KeinPlatz` statt Panik bei MAX_SOCKETS), und Öffnen+Schließen im Kreis
    sammelt sich nicht an.
  - **IRQ-Sturm-Test**: ein Schwung, der ALLE 16 RX-Puffer auf einmal erledigt,
    wird über viele Runden vollständig abgeholt + neu eingestellt — kein
    DMA-Puffer geht verloren oder doppelt.
- **Speicher-Stabilitäts-Pass** (`tests/netz_abschluss.rs`): 150 Zyklen aus
  Socket-auf/zu + Ping + regelmäßigem HTTP-Abruf ergaben **0 Byte Heap-Wachstum,
  0 geleakte Frames, 0 geleakte Sockets** (der Frame-Allocator ist byte-exakt
  stabil, TIME_WAIT wird sauber abgeräumt).
- **Robustheit-Pass** (alles saubere Fehler in begrenzter Zeit, kein Hänger/
  Panik): Kabel weg (100 % Verlust → Fehler + Erholung nach Link-up), Server
  stumm (RST/Timeout), DNS-Server tot (`Zeitueberschreitung` nach ~3,6 s),
  Gateway-MAC-Wechsel (der ARP-Cache übernimmt die neue MAC in beide
  Richtungen). Neues Testwerkzeug bleibt: `netz::geraet::verlust_setzen`.
- **Leistungs-Pass (ehrlich)**: Durchsatz von `hole` über LAN **~0,6 MiB/s**
  (512 KiB), Ping-RTT ~0,2 ms (erster Ping ~4 ms durch ARP). Langsam, klar
  begründet: 8-KiB-Fenster ohne Scaling + synchrones Pumpen pro Segment. Kein
  Fix — nur Transparenz.
- **unsafe-Audit** des Netz-Pfads: `src/netz/` hat **0 unsafe** (reine
  Byte-Logik); die riskante Fläche liegt allein in `virtio/net.rs`
  (Port-I/O + DMA, alle mit `# Safety`-Begründung, 1 `unsafe impl Send`).
  KONKRETE HÄRTUNG: `empfange_frame` **klemmt die vom Gerät gemeldete Länge auf
  PUFFER_BYTES**, bevor daraus ein Slice entsteht — ein buggy/böses Gerät kann
  uns so nie über den DMA-Puffer hinaus lesen lassen.
- **README** mit echter Netz-Beispielsitzung (netz-status/nslookup/ping/hole/
  arp — der serielle Mitschnitt von `tests/netz_shell.rs`) und den gemessenen
  Grenzen; **`docs/serie6-bestandsaufnahme.md`** beantwortet ehrlich, was echte
  User-Space-Prozesse brauchen (Ring 3, Adressraum-Trennung, präemptiver
  Scheduler, ELF-Loader), warum APIC/MSI erst **SMP** erzwingt (nicht User-Space
  an sich), wie aus Socket-/VFS-/Fenster-APIs Syscalls werden (Trap-Gate,
  copy-in/out, Handle-Tabelle pro Prozess), der kleinste erste Ring-3-Prozess,
  und wo **TLS** zum Browser-Blocker wird.
- 190 Lib-Tests grün (vorher 187) + `tests/netz_abschluss.rs`,
  `tests/netz_shell.rs`. `cargo clippy --all-targets` warnungsfrei; Desktop
  bootet sauber.

### Serie 5: Der Reißleinen-Entscheid — das Eigenbau-TCP BLEIBT
- **Der bewusst eingeplante Ingenieur-Entscheid**, durchgeführt statt
  übersprungen — auch nachdem der HTTP-Client funktionierte.
- **Neuer Stresstest `tests/netz_stress.rs`** (viel härter als die bisherigen
  Einzelmessungen): 20 Abrufe gegen **8 verschiedene echte Internet-Server**
  (verschiedene TCP-Stacks, 0–11,5 KB, `Content-Length` und `chunked`, auch
  ein `204`), dazu Läufe mit **künstlichem Paketverlust**.
  Neues Testwerkzeug `netz::geraet::verlust_setzen(prozent)` verwirft Frames
  je Richtung an der Geräte-Naht (auf einem Windows-Host gibt es kein
  tc/netem); zusätzlich im Runner **`SPEEDOS_NET_DELAY=<µs>`** →
  QEMUs eingebauter `filter-buffer` für Verzögerung/Bursts.
- **Messergebnis (protokolliert in docs/tcp-scope.md):**
  - Internet, 3 Läufe à 20 Abrufe: **56/60 sauber = 93 %**. Alle vier
    Fehlschläge entfielen auf **denselben** Server, der auch im Erfolgsfall
    5–15× langsamer ist als die anderen; die übrigen sieben: **100 %**.
  - LAN (kontrolliert, 21 700 Byte): **10/10** und **10/10**.
  - Mit 10 % / 20 % Paketverlust: 4/5 bzw. 2/3 durchgekommen.
- **Fehlerbild ehrlich benannt:** KEINE Deadlocks (jeder Fehlschlag war ein
  Timeout mit teilweise empfangenem Rumpf, der Stack war danach sofort wieder
  benutzbar), KEINE falschen Daten (Content-Length exakt, Anfang und Ende
  geprüft), KEINE Socket-/TIME_WAIT-Lecks (0 Einträge nach allen Phasen).
  **DIE Schwäche: krasse Verlangsamung unter Verlust** (21 KB brauchen bei
  10 % Verlust 0,3–12 s) durch die drei bewusst weggelassenen Mechanismen —
  kein Fast-Retransmit, Out-of-Order wird verworfen, RTO-Backoff bis 8 s.
- **ENTSCHEIDUNG: Das vorher registrierte Kriterium (≥ 9/10) ist erfüllt →
  die smoltcp-Reißleine wird NICHT gezogen, das Eigenbau-TCP bleibt.**
  Ein vorher festgelegtes Kriterium wird hinterher nicht verschoben. Die
  Grenzen stehen jetzt ehrlich in der **README** („Bekannte Grenzen des
  Netzwerk-Stacks") und im Messprotokoll.
- **Cargo-Feature `tcp-eigen`** (Standard an) markiert die Tausch-Stelle
  hinter der Socket-API; ohne das Feature bricht der Bau mit einer
  erklärenden `compile_error!`-Meldung ab (es ist bewusst keine Alternative
  eingebunden). Die unteren Schichten und die Socket-Fassade blieben bei einem
  Tausch in jedem Fall unsere.
- **Testmethodik:** Das HARTE Gate liegt auf dem kontrollierbaren LAN-Server;
  der Internet-Lauf ist Bericht + Grundschwelle, die Verlust-Läufe fordern
  ERHOLUNG (Mehrheit kommt durch) statt Perfektion — eine Testsuite darf
  weder von fremden Servern abhängen noch eine dokumentierte, akzeptierte
  Schwäche als Fehler werten.

### Serie 5: Socket-API + HTTP-Client — die öffentliche Fassade des Stacks
- **`src/netz/socket.rs` — DIE NAHT FÜR SERIE 6 (User-Space).** Anwendungen
  reden nur noch hierüber, nie mit `tcp::Verbindung`/`udp` direkt:
  `oeffnen/binden/lauschen/verbinden/senden/empfangen/schliessen` +
  `zustand`/`verfuegbar`. **HANDLES statt Zeiger** (undurchsichtige,
  monoton wachsende IDs — kein Recycling, kein Kernel-Zeiger nach außen),
  **klare Fehler-Enums** (`SocketFehler` mit deutschen Meldungen), und die
  **Puffer-Ownership explizit**: `senden` kopiert HINEIN, `empfangen` HERAUS,
  in vom Aufrufer gestellte Slices — genau die Grenze, an der später
  copy-in/out zwischen Kernel und User sitzt. TCP UND UDP über dieselbe API
  (TCP trägt die Zustandsmaschine, UDP nutzt den bestehenden Port-Demux).
  **TLS-agnostisch**: die API kennt nur Bytes (TLS wäre später eine Schicht
  darüber).
- **Der `netz_task` bedient die Sockets**: neues `netz::pumpen()` = Empfang
  verarbeiten + `socket::bedienen()` (TCP-Timer ticken, erzeugte Segmente per
  IPv4 senden, fertige Sockets abräumen). Dazu ein **Socket-Takt-Task**
  (100 ms), damit Retransmits auch ohne eingehenden Verkehr feuern. Der alte
  Einzelverbindungs-Treiber in `tcp.rs` ist weg; `tcp::verarbeiten` stellt
  Segmente per 4-Tupel (bzw. lauschendem Port) dem passenden Socket zu.
- **`src/netz/http.rs` — HTTP/1.1-Client**: Anfrage bauen (Host-Header,
  `Connection: close`), Antwort parsen — Statuszeile, Kopfzeilen
  (case-insensitiv, robust gegen krumme Abstände/Zeilen ohne Doppelpunkt/
  bloße LF), Rumpf per **Content-Length UND chunked transfer encoding**
  (inkl. Chunk-Erweiterungen; fehlender 0-Chunk = `UnvollstaendigeAntwort`).
  **Weiterleitungen (3xx)** bis zu einer kleinen Grenze, mit Auflösung
  absoluter/relativer `Location`-Ziele. **NUR http://** — https wird sauber
  mit `TlsNichtUnterstuetzt` abgelehnt statt halbherzig versucht.
- **Shell `hole <url> [zieldatei]`**: zeigt Statuszeile + alle Kopfzeilen;
  ohne Zieldatei wird Text direkt angezeigt, mit Zieldatei wandert der Rumpf
  aufs Dateisystem (inkl. `sync`) — **Netz und Persistenz zusammen**.
- **MEILENSTEIN (Reißleinen-Prüfpunkt, protokolliert in docs/tcp-scope.md):**
  gegen einen LAN-Server (`python -m http.server` auf dem Host, über slirp als
  10.0.2.2) **10/10 Abrufe sauber, je 21 700 Byte** — die Datei ist größer als
  das 8-KiB-Empfangsfenster, läuft also über mehrere Fensterfüllungen; geprüft
  wird exakte Content-Length-Übereinstimmung plus Anfang und Ende. Realtest
  gegen das Internet ebenfalls **10/10**. Kriterium (≥ 9/10) in beiden
  Messungen erfüllt → **Eigenbau-TCP bleibt**.
- Zusätzlich bewiesen: ein über den eigenen Stack geholter Body landet
  **byte-identisch auf der SpeedFS-Platte** (`test_http_auf_platte_speichern`).
- Tests: HTTP-Response-Parsing (Content-Length, chunked, Header-Wirrwarr),
  URL-Zerlegung, Redirect-Logik, Anfrage-Bau, Socket-Handle-Lebenszyklus
  (nach `schliessen` ist jedes weitere Kommando `UngueltigerHandle`, IDs
  werden nie wiederverwendet) und TCP-Socket-Fehlerpfade. 187 Lib-Tests grün
  (vorher 179) + `tests/netz_http.rs`. `cargo clippy --all-targets`
  warnungsfrei; Desktop bootet sauber.

### Serie 5: TCP (Minimal-Viable, selbst gebaut) — SpeedOS lädt HTTP-Seiten
- **Der lehrreichste und riskanteste Teil**, bewusst als LERN-ARTEFAKT scharf
  abgegrenzt. Umfang, bewusste Lücken und — VOR dem Code festgelegt — das
  **Reißleinen-Kriterium** stehen in `docs/tcp-scope.md`.
- **`src/netz/tcp.rs` — der vollständige Zustandsautomat** (CLOSED, LISTEN,
  SYN_SENT, SYN_RCVD, ESTABLISHED, FIN_WAIT_1/2, CLOSING, TIME_WAIT,
  CLOSE_WAIT, LAST_ACK): 3-Wege-Handshake aktiv (`connect`) UND passiv
  (`listen`), In-Order-Datentransfer mit Seq/ACK und **festem** Fenster,
  **Retransmit-on-Timeout** (feste Start-RTO, exponentielles Backoff, Aufgabe
  nach N Versuchen — KEIN Karn/Jacobson), geordneter Abbau inkl. TIME_WAIT
  (2·MSL bewusst auf 2 s verkürzt). Die `Verbindung` (TCB) ist eine REINE
  Zustandsmaschine: sie sammelt Segmente in einem Ausgang, statt selbst ins
  Netz zu rufen — so spielt derselbe Code gegen echte Hardware UND im
  Loopback-Test gegen sich selbst.
- **Bewusst NICHT** (docs/tcp-scope.md): Congestion-Control, Fast-Retransmit,
  SACK, Window-Scaling, Out-of-Order-Reassembly (Out-of-Order → verworfen +
  kumulatives ACK → Retransmit). Ehrlich dokumentiert: bei Verlust langsam
  (Go-Back-N-artig), aber korrekt.
- **Byte-Ring-Abstraktion** `netz::puffer::Ringpuffer` (fester Ring: schreiben/
  lesen/spitzen/verwerfen mit Wraparound) trägt Sende- und Empfangspuffer;
  Puffer-Ownership explizit dokumentiert (copy-in/out — passt für die spätere
  Kernel/User-Grenze).
- **Treiber** (eine aktive Verbindung über IPv4) + IPv4-Dispatch für Protokoll
  6; Shell **`hole <host> [pfad]`** holt eine HTTP/1.0-Seite (DNS-Auflösung →
  TCP-Connect → GET → Antwort → Close).
- **MEILENSTEIN + REISSLEINEN-MESSUNG** (`tests/netz_tcp.rs`, echt über slirp):
  **10/10 HTTP-Abrufe gegen example.com:80 sauber** (`HTTP/1.1 200 OK`,
  828 Byte, sauberer Close). Kriterium ≥ 9/10 erfüllt → **Eigenbau-TCP bleibt**
  (smoltcp-Reißleine NICHT gezogen).
- Tests: Zustandsübergänge (Handshake aktiv+passiv, Datenphase, Abbau),
  Sequenznummern-Arithmetik mit u32-Wraparound, Retransmit-Auslösung +
  Backoff, **Loopback über einen simulierten Kanal mit 20 % Paketverlust**
  (Handshake + 3000 Byte + Close kommen durch), Ringpuffer-Wraparound.
  179 Lib-Tests grün (vorher 174) + `tests/netz_tcp.rs`. `cargo clippy
  --all-targets` warnungsfrei; Desktop bootet sauber.

### Serie 5: UDP + DHCP + DNS — SpeedOS ist im Internet
- **UDP-Schicht** (`src/netz/udp.rs`): Datagramme parsen/bauen (Ports, Länge,
  Prüfsumme). Die **Prüfsumme über den PSEUDO-HEADER** (Quell-/Ziel-IP,
  Protokoll, UDP-Länge + Segment) als reine, getestete Funktion. Ein
  **Port-Demux**: Dienste `binden` einen Port, ankommende Datagramme landen in
  seiner Empfangs-Queue (`empfangen`) — bewusst die VORÜBUNG für die spätere
  Socket-API (Handles statt Zeiger, jedes Datagramm ein eigener Vec).
- **DHCP-Client** (`src/netz/dhcp.rs`): der Vier-Wege-Tanz DISCOVER → OFFER →
  REQUEST → ACK über UDP-Broadcast (68→67). Übernimmt IP, Maske, Gateway,
  DNS-Server und Lease-Dauer aus den Optionen. Broadcast-Flag gesetzt, damit
  der Server antworten kann, BEVOR wir eine IP haben (IPv4 akzeptiert dafür
  jetzt 255.255.255.255). **Beim Boot automatisch** (`autokonfig`, 3 s Timeout
  → Fallback auf statische Config). Neu in IPv4: `senden_an_mac` (explizite
  Quell-IP 0.0.0.0 an die Broadcast-MAC, ohne ARP/Config — genau für DHCP).
- **DNS-Resolver** (`src/netz/dns.rs`): A-Record-Query bauen und Antwort
  parsen — inklusive **Namens-KOMPRESSION** (0xC0-Zeiger mit Schleifen-Schutz),
  gegen den per DHCP gelernten Server (UDP 53). Kleiner **Cache mit TTL**.
- **Shell**: `netz-status` (IP, Maske, Gateway, DNS, Lease, Quelle), `dhcp`
  (erneut beziehen), `nslookup <name>` (Name → IP).
- **MEILENSTEIN „SpeedOS ist im Internet" ECHT bewiesen** (`tests/netz_dhcp_dns.rs`
  gegen slirp): DHCP → `IP 10.0.2.15 / Maske 255.255.255.0 / Gateway 10.0.2.2 /
  DNS 10.0.2.3 / Lease 86400 s`, dann DNS → `example.com -> 172.66.147.243`.
  Beim echten Boot: `[dhcp] Lease bezogen: IP 10.0.2.15 …` automatisch.
- Tests: UDP-Parse/Build + Pseudo-Header-Prüfsumme, Port-Demux, DHCP-Optionen-
  Parsing (inkl. abgeschnitten), DHCP-Discover-Bau, DNS-Namens-Kompression,
  DNS-Query-Build + Antwort-Parsing. 174 Lib-Tests grün (vorher 166) +
  `tests/netz_dhcp_dns.rs`. `cargo clippy --all-targets` warnungsfrei.

### Serie 5: IPv4 + ICMP — SpeedOS ist anpingbar (der klassische Meilenstein)
- **IPv4-Schicht** (`src/netz/ipv4.rs`): Kopf parsen/bauen (Version/IHL,
  Gesamtlänge, TTL, Protokoll, Kopf-Prüfsumme), Dispatch nach Protokollfeld
  (ICMP heute; UDP/TCP folgen). Die **Internet-Checksumme (RFC 1071)** als
  reine, gegen einen bekannten Vektor getestete Funktion (0xB861).
  FRAGMENTIERUNG wird bewusst NICHT unterstützt, aber sauber ERKANNT (MF-Bit
  bzw. Offset != 0) und mit Log verworfen — Reassemblierung wäre unnötiger
  Aufwand für unseren Zweck. Ausgehend: Ziel-MAC per ARP-Cache auflösen;
  bei einem MISS wird das Paket kurz ZURÜCKGESTELLT (`AUSSTEHEND`, TTL 3 s)
  und ein ARP-Request geschickt — trifft die Antwort ein, liefert
  `ausstehend_ausliefern` (nach jedem Dispatch) es aus. Next-Hop-Wahl:
  eigenes Subnetz → direkt, sonst über das Gateway.
- **ICMP-Schicht** (`src/netz/icmp.rs`): Echo-Request BEANTWORTEN (Ping-Reply
  mit gespiegeltem Identifier/Sequenz/Daten und korrekter Prüfsumme über die
  ganze Nachricht) und Echo-Request SENDEN. Empfangene Echo-Antworten werden
  vermerkt (Identifier/Sequenz/TTL), damit der `ping`-Befehl sie zuordnen kann.
- **`ping <ip>`** (Shell): schickt 4 Echos (56 Datenbytes wie das echte ping),
  misst die RTT über die TSC-Mikrosekunden-Uhr, zeigt `N Bytes von <ip>:
  seq=… ttl=… zeit=…ms` und eine min/schnitt/max-Statistik. Pumpt den Empfang
  synchron (kooperativer Executor) und beantwortet dabei auch eingehende Pings.
- **MEILENSTEIN 1 „der Host kann SpeedOS anpingen"** geräteunabhängig bewiesen
  (`test_icmp_echo_antwort_meilenstein`, Mock-NIC): ein Echo-Request an unsere
  IP → genau ein korrekter Echo-Reply (unsere IP, Identifier/Sequenz/Daten
  gespiegelt). (Über slirp-NAT ist der Gast von außen nicht direkt pingbar —
  darum der geräteunabhängige Beweis; ein echter Host-Ping bräuchte ein
  Bridged/TAP-Netz.)
- **MEILENSTEIN 2 „SpeedOS kann das Gateway anpingen"** ECHT gegen slirp
  (`tests/netz_ping.rs`): `ping 10.0.2.2` →
  `[PING-MEILENSTEIN] Antwort von 10.0.2.2 seq=0 ttl=255 zeit=4354us`.
- Tests: IPv4-Parse/Build (inkl. Prüfsummen-Abweisung), Internet-Checksumme
  (bekannter Vektor), ICMP-Echo-Reply-Konstruktion, Fragment-Erkennung,
  ICMP-Parse-Grenzen. 166 Lib-Tests grün (vorher 159) + `tests/netz_ping.rs`.
  `cargo clippy --all-targets` warnungsfrei.

### Serie 5: Der Netz-Stack beginnt — NetzGeraet-Trait, Ethernet, ARP
- **Die Architektur-Naht `netz::NetzGeraet`** (analog zu `BlockDevice`):
  `mac()`, `sende_frame(&[u8])` und `empfange_frame()`. Der Stack redet ab
  jetzt AUSSCHLIESSLICH mit diesem Trait — ein e1000/rtl8139 ließe sich
  später ohne eine Zeile Stack-Änderung ergänzen. virtio-net implementiert
  es und REGISTRIERT sich beim Boot (`geraet_registrieren`); der frühere
  `rx_task` des Treibers ist weg, den RX-Weg treibt jetzt die Netz-Schicht.
- **`src/netz/` mit klaren Schicht-Grenzen:**
  - `puffer.rs` — die Byte-Puffer-Abstraktion: `Leser` (grenzgeprüft, gibt
    `Option` statt zu panicken) und `Schreiber` (Big-Endian-Bau). Von
    Ethernet UND ARP wiederverwendet.
  - `ethernet.rs` — Schicht 2: Frames parsen/bauen (Ziel/Quelle/EtherType),
    plus der Frame-Hexdump (geräteunabhängig, deshalb aus dem Treiber
    hierher).
  - `arp.rs` — Adressauflösung IP↔MAC: Pakete parsen/bauen, Requests
    BEANTWORTEN (wer nach UNSERER IP fragt, bekommt unsere MAC), eigene
    Requests SENDEN, ein ARP-Cache (IP→MAC) mit 2-Minuten-Timeout (reine,
    testbare Logik — `jetzt_ms` wird übergeben).
  - `geraet.rs` — die NIC-Registry + der RX-Weckmechanismus (Waker/Flag,
    vom Geräte-IRQ gesetzt, wie bei Tastatur/Maus).
- **Der async `netz_task` (Dreh- und Angelpunkt):** vom Geräte-IRQ geweckt,
  holt die RX-Frames vom `NetzGeraet` und DISPATCHT sie nach EtherType
  (ARP → arp-Modul; IPv4 folgt). Der IRQ-Handler bleibt minimal (nur ISR
  quittieren + wecken). `rx_verarbeiten()` ist synchron aufrufbar, damit ein
  Shell-Befehl den Empfang selbst „pumpen" kann (der kooperative Executor
  gibt während eines Befehls keinem anderen Task Zeit).
- **Statische IP-Konfiguration** (DHCP kommt später) + Shell-Befehle:
  `netz` (NIC-Status/MAC/IP), `netz-ip <ip> <maske> <gateway>`,
  `netz-lausch` (Hexdump an/aus), `arp` (Cache anzeigen),
  `arp-ping <ip>` (MAC auflösen — sendet Request, pumpt bis Antwort/Timeout).
- **MEILENSTEIN „SpeedOS antwortet auf ARP" doppelt verifiziert:**
  (1) geräteunabhängig per Mock-NIC (`test_arp_antwort_meilenstein`): ein
  ARP-Request nach unserer IP → genau eine korrekte Reply mit unserer MAC,
  Frager gelernt; (2) ECHT gegen QEMUs slirp (`tests/netz_arp.rs`):
  `arp-ping 10.0.2.2` löst das Gateway auf →
  `[ARP-MEILENSTEIN] Gateway 10.0.2.2 ist bei 52:55:0a:00:02:02`.
  Tests: Ethernet-Parse/Build, ARP-Parse/Build, ARP-Cache-Timeout,
  Puffer-Roundtrip/Grenzen, Ipv4-Parse. 159 Lib-Tests grün (vorher 149) +
  der neue Integrationstest `tests/netz_arp.rs`. `cargo clippy
  --all-targets` warnungsfrei.

### Serie 5 beginnt: virtio-net — interrupt-getriebener Empfang (RX, kein Stack)
- **Der erste ASYNCHRONE Hardware-Event jenseits von Tastatur/Maus/
  Timer.** Netzwerk-Pakete kommen unaufgefordert — man kann sie nicht
  wie die Platte pollen, sie müssen INTERRUPTS auslösen. Bewusst klein:
  nur empfangen + hexdumpen, KEIN Stack (ARP/IP/TCP sind der Fahrplan
  aus `docs/serie5-netzwerk.md`).
- **`src/virtio/net.rs`** (Muster: `blk.rs`): findet die NIC per
  `pci::finde` (0x1AF4:0x1000), Legacy-Init wie blk (Reset→ACK→DRIVER→
  Features→Queues→DRIVER_OK), MAC aus der Device-Config. RX-Queue (0)
  mit 16 gerätebeschreibbaren DMA-Puffern (kein Bounce — wir besitzen
  sie); die `RxRing`-Struct führt die Kopf→Puffer-Zuordnung und stellt
  Puffer nach dem Verbrauch wieder ein. Die **Virtqueue wird UNVERÄNDERT
  weiterbenutzt** (die Serie-4-Vorhersage stimmt).
- **IRQ-Pfad** (exakt das Tastatur-/Maus-Muster): IDT-Handler für die
  PCI-Vektoren 41/42/43 (IRQ 9/10/11); der Handler liest das
  ISR-Register (quittiert das Gerät, sagt „waren WIR es?" — Shared
  Interrupts) und weckt per `AtomicWaker` einen async `rx_task` — KEIN
  Lock im Handler. `interrupts::irq_freischalten(irq)` schaltet die zur
  Laufzeit gefundene IRQ am PIC frei. Der gepollte virtio-blk bekommt
  `Virtqueue::interrupts_aus()` (VIRTQ_AVAIL_F_NO_INTERRUPT), damit er
  auf keiner geteilten Leitung stört.
- **`netz-lausch`** (Shell): schaltet den Frame-Hexdump an/aus (Ziel-/
  Quell-MAC, EtherType annotiert). Da QEMUs user-Netz (slirp) reaktiv
  ist, sendet es beim Einschalten EIN statisches Broadcast-ARP-Frame —
  slirp antwortet, der Empfang wird sichtbar (kein Stack, ein
  42-Byte-Frame).
- **Meilenstein verifiziert:** `[virtio-net] Bereit: MAC …, IRQ 11`, dann
  `netz-lausch` → `[netz] Frame 64 Byte | Ziel … | Quelle 52:55:0a:00:
  02:02 | EtherType 0x0806 (ARP)` samt Hexdump des ARP-Reply. Tests:
  Header-Längen-Logik, RX-Ringführung (ohne Gerät), PCI-Fund. `cargo
  clippy --all-targets` warnungsfrei.

### Serie-4-Abschluss: Persistenz-Tests + Performance-Zahlen
- **Neue Tests der Persistenz-Schicht** (die kritischsten Lücken):
  - `test_speedfs_mount_fehlerpfade`: jeder Mount-Fehler kommt sauber
    (unformatiert → KeinSpeedFs, zu klein → formatieren gibt Voll,
    krumme Sektorgröße → Io, kaputter Superblock → KeinSpeedFs).
  - `test_speedfs_voll_sauber`: volle Platte → `FsFehler::Voll` sauber,
    vorherige Dateien Bit-für-Bit intakt, fsck 0 Defekte (die
    alles-oder-nichts-Allokation ändert bei Platzmangel nichts).
    (Es gibt bewusst kein `IoFehler::KeinPlatz` — ein fixes Blockgerät
    ist nie „voll", nur das Dateisystem.)
  - `test_speedfs_folter_fast_voll`: der Folter-Test auf FAST VOLLER
    Platte — die Op-Serie trifft unterwegs Voll UND der Absturz
    schneidet an jeder Stelle: 80 Abschneide-Punkte, 0 Defekte.
- **Großer End-to-End-Test** als automatisierter Ablauf (geteilte
  Sequenz `speedfs::e2e_ops`): mkfs → Dateien → Editor-Roundtrip
  (write_at/read_at + Editieren) → rename-Orgie → Absturz → fsck →
  alles noch da. Läuft als Unit-Test (RamDisk, inkl. Absturz-Simulation)
  UND als `tests/e2e_speedfs.rs` gegen die ECHTE Platte — **IDE und
  virtio** (SPEEDOS_PLATTE), non-destruktiv im Unterbaum /platte/e2e.
  146 Lib-Tests grün (vorher 142) + der neue Integrationstest.
- **Performance-Zahlen (final):**

  *plattentest* (2 MiB sequenziell + 100 × 4 KiB zufällig):

  | Operation           | virtio-blk  | IDE-PIO   | Faktor |
  |---------------------|-------------|-----------|--------|
  | seq. schreiben      | 324,51 MiB/s| 0,21 MiB/s| ~1545× |
  | seq. lesen          | 1808,31 MiB/s| 0,21 MiB/s| ~8600× |
  | zufällig schreiben  | 15,28 MiB/s | 0,21 MiB/s| ~73×   |
  | zufällig lesen      | 39,05 MiB/s | 0,21 MiB/s| ~186×  |

  Bestätigt die Architektur-Wahl: virtio ist DER Standard, IDE bleibt
  nur für die volle ATA-Treiber-Abdeckung (tests/ata_platte.rs) wählbar.

  *Compositing* (Berichts-Tests, aktuelle Messung): Editor-Tippen
  ALT 2156 µs → **NEU 222 µs** (~10×, das Dirty-Rect-Ergebnis hält),
  Terminal-Ausgabe ALT 1651 µs → NEU 912 µs, Uhr-Tick 244 µs,
  Fenster-Blit Pro-Pixel 25295 µs → Zeilenkopie **4424 µs** (~5,7×).
- **Schlimmster verbliebener Fresser: der IDE-PIO-Pfad** (0,21 MiB/s).
  Ursache ist ARCHITEKTONISCH — jedes 16-Bit-Wort kostet einen
  Port-I/O-VM-Exit; das lässt sich mit PIO nicht auf <1 Tag beheben
  (LBA48-DMA/Bus-Master wäre ein eigenes Projekt). Da virtio der
  Standard ist und IDE nur der Test-Abdeckung dient, bleibt es bewusst
  stehen — benannt, nicht „gefixt".
- **unsafe-Audit** der Port-I/O-Treiber: 50 `unsafe {`-Blöcke in
  `pci.rs` (2), `virtio/blk.rs` (18), `virtio/virtqueue.rs` (20) und
  `ata.rs` (10) durchgesehen — **alle** sind Port-I/O (Legacy-Register)
  oder `read_volatile` auf validierten Deskriptor-Indizes, jeder mit
  „warum safe"-Begründung (Gruppen-Kommentar bei wiederholtem Muster);
  **0 `unsafe fn`** (keine fehlenden `# Safety`-Abschnitte). Zwei
  verwaiste Blöcke in blk.rs nachdokumentiert. `cargo clippy
  --all-targets` ist **warnungsfrei**.
- README/CLAUDE.md auf Serie-4-Stand: Persistenz-Stack, Live-USB
  (Acer-Boot verifiziert), aktualisierter Feature-/Roadmap-Stand.

### SpeedOS bootet vom USB-Stick (Live-System) + Diagnose-Modus
- `cargo image` (neuer Alias + Host-Binary `boot/src/bin/live-image.rs`)
  baut **`speedos-live.img`** — ein bootfähiges UEFI-GPT-Image mit
  EFI-System-Partition, OHNE QEMU-Start und OHNE Daten-/FAT-Platte.
  BEWUSST ohne erzwungene Mindestauflösung (Unterschied zum Runner):
  auf echter Hardware nimmt die Firmware ihren nativen GOP-Modus, der
  Kernel ist ohnehin auflösungsunabhängig + skaliert HiDPI automatisch
- Robustheit gegen fehlende Geräte (ohne Übertreibung):
  - **Keine PS/2-Tastatur** wird per nicht-intrusiver Probe erkannt
    (First-Port-Test 0xAB, ändert KEINE Controller-Config — die Regel
    "Tastatur-Bits 0/4/6 nie anfassen" bleibt gewahrt); statt still zu
    hängen erscheint die Bildschirm-Meldung „keine PS/2-Eingabe
    gefunden — USB-Eingabe kommt in einer künftigen Version"
  - **Keine PS/2-Maus** → Desktop läuft per Tastatur (war schon so;
    jetzt als Presence-Flag gemerkt)
  - **Keine Platte** → RAM-VFS-Fallback (war schon so; nun sichtbar)
- **Diagnose-Modus** (`src/diagnose.rs`): auf echter Hardware gibt es
  keine serielle Ausgabe — Taste **D** beim Boot (oder `SPEEDOS_DIAGNOSE=1`
  im Runner via `set_ramdisk`) zeigt die Boot-Schritte [1/4]..[4/4] und
  eine Hardware-Zusammenfassung (Bildschirm, Tastatur/Maus, Laufwerke,
  Mounts) auf dem Bildschirm
- QEMU-Generalprobe (`tools/live_qemu.ps1`, UEFI/OVMF) für alle Fälle
  verifiziert (Screendumps in `docs/screenshots/live-*.png`): Normal-
  Desktop, `i8042=off` → Eingabe-Meldung, Taste D → Diagnose-Schirm,
  2560×1440 → HiDPI-Skalierung. `cargo test` bleibt grün
- Anleitungen: [`docs/usb-boot.md`](docs/usb-boot.md) (auf Stick
  schreiben inkl. Laufwerks-Warnung, BIOS/UEFI-Einstellungen, Erst-Boot)
  und [`docs/hardware-log.md`](docs/hardware-log.md) (Vorlage für echte
  Geräte-Tests mit Fotos)

### Feinkörniges Dirty-Rect: das Voll-Zeichnen pro Taste stirbt
- Widgets melden jetzt SCHADENS-RECHTECKE statt "ganzes Fenster neu":
  `UiReaktion.schaden: Option<Rechteck>` (Fensterinhalt-Koordinaten),
  neu über `neu_zeichnen_bereich()` / `mit_schaden()`. Ohne Rect
  bleibt der ehrliche Vollbild-Fallback (Korrektheit vor Eleganz).
  Fenster sammeln MEHRERE Schadens-Rects (keine Bounding-Box —
  Cursorzeile oben und Statuszeile unten würden sonst fast das ganze
  Fenster umfassen); der Compositor rendert/komponiert je Rect nur den
  gemeldeten Streifen (die Dirty-Rect-Mechanik bekam feinere Meldungen)
- Heiße Pfade umgestellt: SpeedText-Tippen (nur Cursorzeile +
  Statusstreifen; der Editor CULLT Zeilen außerhalb des Clips — ohne
  das würde `text()` bei 4K Millionen Glyph-Pixel gegen den Clip
  prüfen), Textfelder, ScrollListen (Scrollen = Listenfläche),
  Button-/Checkbox-Hover (nur der Button). Der Task-Manager bleibt
  bewusst Vollbild (er tickt nur 1×/s und aktualisiert Zahlen + Graph
  + Liste gemeinsam — kein interaktiver Hot-Path)
- BEWEIS (Berichts-Test messung_serie3, ALT/NEU im selben Lauf —
  immun gegen die WHPX/TCG-Lotterie), großes SpeedText-Fenster:

  | Szenario                | 720p ALT | 720p NEU | 4K ALT   | 4K NEU |
  |-------------------------|----------|----------|----------|--------|
  | Editor-Tippen (µs/Taste)| 2553     | 350      | 15430    | **417** |
  | Terminal-Ausgabe (µs)   | 1713     | 1017     | 1325     | 744    |

  ZIEL ERREICHT: Editor-Tippen bei 4K **417 µs** (< 500 µs) — vorher
  15,4 ms/Taste (unbenutzbar), jetzt ~37× schneller. Der größte
  Rest-Fresser (ein zu großzügiger Statusstreifen) wurde analysiert
  und auf eine Zeilenhöhe geschrumpft (502 µs -> 417 µs)
- Keine sichtbaren Artefakte: per QMP-Screenshots an den heiklen
  Stellen geprüft (überlappende Fenster, Terminal-Streifen-Ausgabe
  unter/über anderem Fenster) — der Compositor komponiert je Rect
  alle Fenster in Z-Ordnung, Überlappung bleibt korrekt
- 142 Lib-Tests (inkl. neuer Schadens-Kombinations- und
  umschliessen-Tests) + alle Integrationstests grün; die
  Routing-Tests unverändert bestanden

### PCI + virtio-blk: die schnelle Platte (Infrastruktur für virtio-net)
- Neues PCI-Modul `src/pci.rs`: Config-Space-Enumeration über die
  Legacy-Ports 0xCF8/0xCFC, Bus/Gerät/Funktion durchgehen, Vendor/
  Device/Klasse und BARs (I/O- und 64-Bit-MMIO) dekodieren.
  Shell-Befehl `pci` listet alles lesbar. Reine Dekoder-Funktionen
  unit-getestet — die Grundlage für jeden künftigen modernen Treiber
- Neues virtqueue-Modul `src/virtio/virtqueue.rs`: die Split-Virtqueue
  (Deskriptor-Tabelle + Available-/Used-Ring in physisch
  zusammenhängendem Speicher) — GERÄTE- UND TRANSPORT-UNABHÄNGIG und
  ausführlich kommentiert als BLAUPAUSE für virtio-net (Serie 5).
  Speicher-Barrieren an den Übergabepunkten, Schleifen-Schutz beim
  Kettenfreigeben, Freiliste-Verwaltung
- Neuer virtio-blk-Treiber `src/virtio/blk.rs`: virtio über den
  PCI-LEGACY-Transport (Port-I/O-BAR — Begründung im Code: QEMUs
  transitional device bietet es an, wir kennen Port-I/O vom
  ATA-Treiber, und die Virtqueue bleibt für Modern/virtio-net gleich).
  Feature-Negotiation (FLUSH), eine Virtqueue, Requests gepollt mit
  Timeout, Bounce-Puffer für DMA. Implementiert BlockDevice inkl.
  sync (FLUSH)
- Runner: `SPEEDOS_PLATTE=ide|virtio` wählt das Backend der
  Daten-Platte; `fs::daten_geraet()` liefert das richtige BlockDevice
  (virtio hat Vorrang), alle Aufrufer (mkfs/mount/pruefe/automount/
  Einstellungen) laufen unverändert darüber. SpeedFS funktioniert auf
  beiden Backends identisch — der Persistenz-Beweis besteht auf beiden
- Neuer Shell-Befehl `plattentest`: Benchmark der rohen Daten-Platte
  (sequenziell + zufällig, lesen + schreiben, MiB/s). GEMESSEN
  (2 MiB seq, 100×4 KiB zufällig, QEMU/WHPX):

  | Zugriff             | IDE (PIO) | virtio-blk | Faktor |
  |---------------------|-----------|------------|--------|
  | seq. schreiben      | 0,70 MiB/s | 1810 MiB/s | ~2600× |
  | seq. lesen          | 0,70 MiB/s | 4819 MiB/s | ~6900× |
  | zufällig schreiben  | 0,69 MiB/s |  144 MiB/s |  ~200× |
  | zufällig lesen      | 0,70 MiB/s |  298 MiB/s |  ~425× |

  Der IDE-PIO-Pfad ist so langsam, weil jedes 16-Bit-Wort einen
  Port-I/O-VM-Exit kostet; virtio nutzt DMA + Virtqueue. virtio
  gewinnt klar -> **Standard jetzt virtio** (IDE bleibt per
  SPEEDOS_PLATTE=ide wählbar, u. a. für die volle ATA-Test-Abdeckung)
- Tests: PCI-Dekoder (config_adresse, BAR, Klasse), virtqueue-Layout
  und Ring-Ablauf ohne Gerät; die Persistenz-Tests laufen über
  fs::daten_geraet und bestehen auf BEIDEN Backends (ata_platte
  überspringt seine ATA-Daten-Tests unter virtio sauber). 140
  Lib-Tests + alle Integrationstests grün

### FAT32 lesen: SpeedOS versteht fremde Medien (nur lesend)
- Neuer FAT32-Treiber `src/fs/fat32.rs` (NUR LESEN) auf dem
  BlockDevice-Trait: Bootsektor/BPB parsen und streng validieren
  (nie panicken -> `FsFehler::KeinFat32`), die FAT einmal in den RAM,
  Cluster-Ketten mit Schleifen-Schutz verfolgen, Verzeichnisse
  inklusive langer Dateinamen (VFAT-LFN, UTF-16-LE -> unsere Strings,
  Umlaute stimmen), Dateien über read_at lesen, FAT-Zeitstempel ->
  zeit-Epoche. Jeder Schreib-Weg lehnt sauber mit
  `IoFehler::NurLesen` ab
- Runner: `tools/fat32_image_erzeugen.py` baut speedos-fat.img mit
  Beispieldateien, Unterordner und Umlaut-Namen — bevorzugt mit den
  Host-mtools (mformat/mcopy), sonst mit eingebautem Python-FAT32-
  Writer. Der Runner hängt es als Secondary Master an ("USB-Stick");
  gitignored
- Mount-Integration: `fs::fat_automounten()` beim Boot mountet das
  FAT-Laufwerk NUR LESEND unter /fat; `platten` zeigt jetzt eine
  Mount-Übersicht mit Dateisystem-Typ und Zugriffsrecht (neue
  Trait-Methoden `FileSystem::ist_beschreibbar`/`typ_name`,
  `fs::mount_uebersicht`/`pfad_beschreibbar`)
- Explorer graut Schreib-Aktionen (+O/+D, Umbenennen, Löschen,
  Ausschneiden, Einfügen) auf Nur-Lese-Mounts aus — Kontextmenü und
  Tastenkürzel gesperrt; Kopieren VON /fat bleibt erlaubt. Der
  Alltag funktioniert: durch /fat navigieren, .txt in SpeedText
  öffnen (Umlaute im Namen UND im Inhalt), Dateien nach /platte
  kopieren
- Tests: BPB-Validierung gegen kaputte Werte (kein Panik),
  LFN-Zusammensetzung inkl. Umlauten, komplettes Mini-FAT32 auf
  einer sparse RamDisk; Integrationstest tests/fat_platte.rs
  vergleicht die gelesenen Inhalte Byte für Byte mit dem
  mtools-Image (Texte, Umlaut-Namen, 100-KiB-Datei über viele
  Cluster, Kopieren /fat -> /platte). 134 Lib-Tests + alle
  Integrationstests grün

### Persistenz wird Standard: SpeedOS überlebt den Neustart
- AUTO-MOUNT beim Boot: fs::platte_automounten() erkennt die
  Daten-Platte, mountet ihr SpeedFS unter /platte (klare serielle
  Meldung mit Füllstand) und legt beim ersten Mal /platte/heim,
  /platte/dokumente und /platte/system an. Eine unformatierte
  Platte bekommt NUR den mkfs-Hinweis in der Shell — nie
  Auto-Format (Formatieren bleibt eine Nutzer-Entscheidung)
- DER Demo-Moment, maschinell bewiesen: Die Einstellungen wohnen
  jetzt auf /platte/system/einstellungen.txt (RAM-Fallback ohne
  Platte) — Theme, Akzent, Skala und Uhrformat überleben den
  QEMU-Neustart. tests/speedfs_platte.rs führt den Beweis mit
  Boot-Zähler und Theme-Testwert über echte Neustarts; dazu ein
  Lib-Roundtrip über simuliertes umount+mount. Die Ortswahl trifft
  zentral fs::persistenter_pfad(platte, ram) — EINE Abstraktion,
  kein if-Wildwuchs
- Umzug auf die Platte: Papierkorb -> /platte/papierkorb,
  Explorer-Startordner und SpeedText-Dialoge -> /platte/heim
  (jeweils mit RAM-Fallback)
- Rotierendes Kernel-Log (src/protokoll.rs): jede println!-Ausgabe
  landet zusätzlich in einem RAM-Puffer (Blatt-Lock, 64-KiB-
  Fenster); der Log-Schreiber-Task flusht sekündlich nach
  /platte/system/log.txt — write_at ans Dateiende, bei 64 KiB
  Rotation per rename nach log.alt.txt. Bewusst Puffer+Task statt
  synchronem Schreiben aus _print (ABBA-Deadlock KONSOLE/VFS)
- Einstellungen-App, neue Seite "Speicher": Laufwerksliste
  (Modell, Größe, Schreibschutz), Mount-Status mit frei/gesamt aus
  der SpeedFS-Bitmap (neue Trait-Methode FileSystem::speicher_info),
  sync-Knopf und pruefe.speedfs-Knopf (hängt kurz aus, prüft,
  hängt wieder ein) mit Ergebnis-Dialog; Startbreite 700
- Runner-Option SPEEDOS_OHNE_DATENPLATTE=1 (Boot ohne Platte —
  der RAM-Fallback-Pfad, manuell verifiziert: sauberer Boot,
  alles im RAM); Deadlock-Lehre dokumentiert: ist_gemountet/
  persistenter_pfad nie in mit_fs-Closures auswerten
- 131 Lib-Tests (u. a. Log-Rotation, Puffer-Fenster, Einstellungs-
  Roundtrip) + alle Integrationstests grün

### SpeedFS wird erwachsen: rename überall, sync-Kette, fsck, Folter-Test
- Explorer-Ausschneiden+Einfügen nutzt jetzt `fs::verschieben_rekursiv`
  (echtes atomares rename) statt kopieren+löschen — auf einer echten
  Platte wäre das Kopieren untragbar gewesen; nur über die
  Mount-Grenze kopiert weiterhin der VFS-Fallback. Format-Doc §7
  präzisiert: Nach einem rename-Absturz existiert die Datei IMMER
  (nie in keinem Ordner), schlimmstenfalls kurz in beiden —
  `pruefe.speedfs` meldet so einen Doppel-Eintrag als Befund
- sync-Kette komplett: `fs::sync()` → alle gemounteten Dateisysteme
  → BlockDevice (ATA FLUSH CACHE). Neuer Shell-Befehl `sync`;
  SpeedText-Speichern ruft sync automatisch (ein sync-Fehler zählt
  wie ein Schreibfehler und erscheint als Dialog), die
  Einstellungen taten es schon
- `pruefe.speedfs [--repariere]` — unser fsck (Format-Doc §10, nur
  ungemountet): Baum-Scan ab der Wurzel prüft Verzeichnis-Einträge
  gegen die Inode-Tabelle, Blockzeiger gegen Datenbereich und
  Doppel-Referenzen, Größen gegen die Zeiger-Belegung; die Bilanz
  gegen die Bitmap findet LECKS (belegt, aber unreferenziert —
  der erlaubte Absturz-Schaden), --repariere gibt sie frei.
  DEFEKTE werden nur gemeldet, nie automatisch repariert
- DER FOLTER-TEST: `AbsturzDisk` verwirft alle Schreibvorgänge nach
  einem Budget N (Präfix-Semantik eines Stromausfalls); eine Serie
  aus create/write/rename/delete wird an JEDEM N abgeschnitten,
  neu gemountet und geprüft — Ergebnis: 72 Abschneide-Punkte,
  58 mit (reparierbaren) Lecks, 0 kaputte Metadaten. Die
  Ordering-Disziplin aus §7 ist damit maschinell belegt
- 128 Lib-Tests + alle Integrationstests grün

### SpeedFS: das eigene Dateisystem-Format (Design zuerst!)
- Neues Design-Dokument `docs/speedfs-format.md` — geschrieben VOR
  dem Code: Superblock ("SPFS", Version 1) | Block-Bitmap |
  Inode-Tabelle | Datenblöcke in 4-KiB-Blöcken, alles Little-Endian.
  Inodes (128 B): Typ, Größe, Zeitstempel, 22 direkte + 1 einfach-
  indirekter Blockzeiger (max. Dateigröße 1046 Blöcke ≈ 4,09 MiB,
  Rechnung im Dokument); Verzeichnisse als Byte-Listen
  [Inode u32 | Länge u8 | Name]. Absturz-Analyse in §7: KEIN
  Journal, stattdessen Ordering-Disziplin (Belegen vor Benutzen,
  Inhalt vor Verweis, Entkoppeln vor Freigeben) — nach einem
  Absturz schlimmstenfalls Block-Lecks, nie falsche Metadaten
- Neue Implementierung `src/fs/speedfs.rs` auf dem BlockDevice-Trait
  (läuft auf RamDisk UND ATA): mkfs (formatieren), mounten/aushängen,
  alle FileSystem-Trait-Methoden (lesen, schreiben, read_at/write_at,
  liste, mkdir, loeschen, stat, rename, sync). Block-Cache mit
  WRITE-THROUGH (bewusst einfach und ehrlich — Entscheidung in
  CLAUDE.md, Write-Back ist Serie-5-Stoff). Neue Fehler: Voll,
  DateiZuGross, KeinSpeedFs
- Aus dem Root-Mount wurde eine MOUNT-TABELLE: Wurzel-RamFs plus
  Präfix-Mounts, selbst ein FileSystem — mit_fs() und alle Befehle
  blieben unverändert; rename über die Mount-Grenze fällt in
  fs::verschieben auf kopieren+löschen zurück (MountGrenze)
- Neue Shell-Befehle: `mkfs.speedfs` (Sicherheitsabfrage: nur mit
  Argument JA, nie bei gemountetem /platte), `mount`, `umount` —
  danach arbeiten dir, type, write, copy, tree, cd, SpeedText …
  transparent auf /platte, ohne einen einzigen Sonderfall
- 9 neue SpeedFS-Lib-Tests auf der RamDisk (Superblock-/Inode-/
  Verzeichnis-Roundtrips, Datei über Blockgrenzen, indirekter
  Block, Verzeichnis-Blocküberlauf, Bitmap-Bilanz beim Löschen,
  rename-Semantik, Wiedermount) — 127 Lib-Tests gesamt, alle grün
- Persistenz-Beweis 2.0 (`tests/speedfs_platte.rs`): Eine ECHTE
  DATEI (/platte/beweis.txt), über das VFS geschrieben, überlebt
  den QEMU-Neustart; die Roh-Sektor-Tests aus ata_platte.rs leben
  jetzt am Platten-ENDE, damit sie das Dateisystem nicht anfassen

### ATA-PIO-Treiber: SpeedOS spricht mit einer echten Platte
- Neues `src/ata.rs`: ATA im PIO-Modus über die Legacy-Ports des
  Primary-Kanals (0x1F0/0x3F6) — gepollt mit TSC-Timeout, keine
  PCI-Enumeration, Kanal-Interrupts aus (nIEN). IDENTIFY liest
  Modell und Kapazität (reine, unit-getestete Dekoder), Lesen/
  Schreiben über LBA28 (max. 128 GiB; 256 Sektoren pro Kommando,
  größere Aufträge zerlegt der Treiber), FLUSH CACHE als sync().
  Implementiert das `BlockDevice`-Trait der Serie-4-Naht
- SICHERHEITSREGEL: Die Boot-Platte (Primary Master) ist PER
  KONSTRUKTION schreibgeschützt — nur die Daten-Platte (Primary
  Slave) bekommt Schreibrechte; Verstöße melden den neuen
  `IoFehler::Schreibgeschuetzt` (dazu neu: `Zeitueberschreitung`)
- Runner: `cargo run` hängt automatisch ein persistentes 64-MiB-Image
  `speedos-daten.img` als zweite IDE-Platte an (beim ersten Start
  angelegt, gitignored); Tests bekommen ein EIGENES
  `speedos-daten-test.img`, damit sie nie Nutzerdaten überschreiben
- Neue Shell-Befehle: `platten` (erkannte Laufwerke mit Modell,
  Größe, Schreibschutz) und `blocktest <lba>` (klassischer Hexdump
  eines Sektors der Daten-Platte)
- ERSTER PERSISTENZ-BEWEIS: tests/ata_platte.rs schreibt ein
  Generationen-Muster in Sektor 1000, und der nächste Testlauf —
  nach QEMU-Neustart! — findet es intakt wieder
  (`[PERSISTENZ-BEWEIS]`-Zeile in der seriellen Ausgabe). Dazu:
  Roundtrip über die 256-Sektoren-Grenze, Boot-Platten-Schreibschutz,
  leerer Steckplatz antwortet in ~130 µs statt zu hängen
- 3 neue Unit-Tests (Laufwerkswahl-Byte, IDENTIFY-Dekoder) und
  5 Integrationstests — 118 Lib-Tests insgesamt, alle grün

### Serie-4-Auftakt: BlockDevice-Naht + VFS-Erweiterung
- Neues `src/fs/block.rs`: das schmale `BlockDevice`-Trait
  (sektor_groesse, anzahl_sektoren, lese_sektoren, schreibe_sektoren,
  sync — alles `Result<_, IoFehler>`, LBA-Adressierung, Puffer =
  Sektor-Vielfaches) plus `RamDisk` als Vec-basierte
  Referenz-Implementierung. Die Naht entsteht BEWUSST vor dem ersten
  echten Treiber — Disk-Dateisysteme reden nur mit diesem Trait
- VFS-Trait erweitert: `read_at`/`write_at` (Offset-basiert,
  POSIX-read-Semantik bzw. Nullbyte-Lücken), `stat` (`Metadaten` mit
  Typ, Größe, erstellt/geaendert aus der RTC/zeit-API), `rename` als
  ATOMARE Primitive (validieren, dann entnehmen+einfügen; Ziel-Datei
  wird ersetzt, Ziel-Verzeichnis/eigener Teilbaum sind Fehler) und
  `sync`; `FsFehler::Io(IoFehler)` transportiert Geräte-Fehler
- RamFs-Knoten tragen jetzt Metadaten (erstellt/geaendert);
  `fs::verschieben`/`verschieben_rekursiv` laufen über rename statt
  kopieren+löschen (atomar, kann jetzt auch ganze Ordner); `dir` und
  die Explorer-Statusleiste zeigen echte Zeitstempel
  (einstellungen::stempel_text, gleicher Anzeige-Offset wie die Uhr)
- Fehler sichtbar: SpeedText meldet Lade-/Speicherfehler als Dialog
  (`ui::dialog::fehler`, Mantel um bestaetigung()); Einstellungen
  rufen nach dem Schreiben fs::sync()
- 8 neue Tests (RamDisk-Roundtrip/Grenzen, read_at/write_at-Grenzen,
  rename-Semantik, stat-Zeitstempel, verschieben+sync,
  SpeedText-Fehlerdialog) — 115 Lib-Tests insgesamt, alle grün

### Serie-3-Abschluss (Qualitäts-, Performance- und Speicher-Pass)
- Neue Tests für die kritischsten App-Schicht-Lücken: Event-Routing
  durch VERSCHACHTELTE Container (Klick/Taste/MausRaus über zwei
  Ebenen), Fokus über Fenster-Wechsel hinweg (Widget-Fokus bleibt
  pro Fenster erhalten), Sitzungs-Zuordnung der Terminals
  (fokus_terminal_sitzung als testbare Manager-Methode)
- Performance-Pass mit neuem Serie-3-Berichts-Test (5 Fenster:
  2 Terminal-Sitzungen, Explorer, Task-Manager mit vollem Graph,
  SpeedText mit 60 Zeilen; A/B alt-gegen-neu im SELBEN Lauf, damit
  die WHPX/TCG-Lotterie die Zahlen nicht verfälscht):

  | Szenario (720p)      | vorher | nachher |
  |----------------------|--------|---------|
  | Terminal-Ausgabe     | 555 us | 275 us  |
  | Editor-Tippen        | 466 us | 403 us  |
  | Vollbild 5 Fenster   | 544 us | (unverändert) |

  Optimierung 1: Das Terminal-Raster führt DIRTY-ZEILEN — eine
  Prompt-Ausgabe rendert nur ihre Rasterzeile in den persistenten
  Fenster-Puffer, und terminal_schreiben meldet dem Compositor nur
  noch den Zeilen-STREIFEN statt der Fensterfläche (2x schneller;
  Scroll/Resize/Theme markieren weiterhin alles).
  Optimierung 2: SpeedText baut beim Tippen den Widget-Baum NICHT
  mehr neu — die neue StatusZeile liest Zeile/Spalte/Zeichen live
  aus dem geteilten Puffer, der Titel wird nur bei echtem Wechsel
  gemeldet. Ehrliche Bilanz: nur -14 %, denn der Fresser ist das
  Voll-Zeichnen+Komponieren der Fensterfläche pro Taste — Teil-
  Rect-Compositing für Widget-Fenster ist als Serie-4-Kandidat
  notiert
- Speicher-Pass: App-Zyklen-Test (Terminal + alle vier Trait-Apps
  20x öffnen/benutzen/schließen) — Heap exakt stabil, KEIN Leck
  gefunden (die Besitz-Ketten Arc-Puffer/Sitzungs-Registry/
  Fenster-Puffer geben vollständig frei)
- unsafe-Audit: Die GESAMTE Serie 3 (Toolkit, vier Apps,
  Terminal-Sitzungen, Task-Übersicht) kommt ohne einen einzigen
  neuen unsafe-Block aus — alle 82 unsafe-Stellen liegen in den
  Hardware-Modulen aus Serie 1/2
- Toolkit-Review nach drei echten Apps: BEWÄHRT haben sich
  Nachricht-IDs mit Basis-Kodierung, Zustand-in-App + aufbau(),
  die NachLock-Disziplin (null Deadlocks in der ganzen Serie) und
  das Arc-Muster für heißen Widget-Zustand. UMSTÄNDLICH waren das
  Box-Cast-Boilerplate (-> neuer Helfer ui::w()), die duplizierte
  Sekunden-Tick-Logik (-> ui::app::SekundenTick, Einstellungen +
  Task-Manager umgestellt) und der Textfeld-Zustandsverlust bei
  Neu-Aufbauten (Muster dokumentiert; echte Lösung = geteilter
  Zustand wie im Editor). Notiert, nicht geändert: Tab ist global
  Fokus-Taste (der Editor kann keine Tabs einfügen), Nachricht-
  Basen bleiben Handarbeit
- docs/serie4-bestandsaufnahme.md: die ehrliche Bestandsaufnahme
  für Serie 4 — VFS-Lücken (read_at/write_at, stat, rename, sync,
  BlockDevice-Trait), Treiber-Empfehlung (ATA PIO zuerst, dann
  virtio-blk; AHCI vertagt), PIC trägt Serie 4 (APIC/MSI als
  eigener Meilenstein), USB-Stick-Boot: Booten ja, PS/2-Emulation
  ist das Risiko, Persistenz auf dem Stick braucht xHCI (Blocker)

### Hinzugefügt (Terminal-Sitzungen + SpeedText — die letzte App der Serie)
- TERMINAL-SITZUNGEN (das Ein-Terminal-Limit fällt): Jedes
  Terminal-Fenster trägt eine Sitzungs-Id, pro Sitzung läuft ein
  EIGENER Shell-Task (shell/sitzung.rs). Der neue Eingabe-Router ist
  der einzige KeyStream-Leser und wirft Tasten in die Queue der
  fokussierten Sitzung (lock-freie Queue + AtomicWaker); die
  _print-Umleitung wurde zum Sitzungs-Konzept: Jede Shell schreibt
  in IHR Fenster (AUSGABE_SITZUNG um die synchrone Verarbeitung —
  race-frei, weil kooperativ ohne await dazwischen), Kernel-Log geht
  ans designierte HAUPT-Terminal und wird GEPUFFERT, wenn keins
  offen ist (Nachlieferung beim nächsten Öffnen). Zwei Terminals
  arbeiten unabhängig (tree hier, tippen dort); Schließen trägt die
  Sitzung aus — der Shell-Task endet sauber am await-Punkt, das
  Haupt-Terminal vererbt seine Rolle
- src/speedtext.rs — SpeedText, der Texteditor:
  * Mehrzeiliges Editor-Widget (ui/texteditor.rs): TextPuffer als
    Zeilen-Vec (im Kommentar begründet: KiB-Dateien brauchen keinen
    Rope), Einfügen/Löschen über Zeilengrenzen, Pfeile/Pos1/Ende/
    Bild-Tasten, Klick setzt den Cursor, vertikales Scrolling mit
    ziehbarem Balken, Zeilennummern-Spalte, blinkender Cursor. Der
    Puffer lebt GETEILT (Arc<Mutex>) zwischen App und Widget — so
    überlebt der Text die Widget-Neu-Aufbauten der Statuszeile
  * Datei öffnen/speichern übers VFS: Strg+S speichert (ohne Pfad:
    Speichern-unter-Dialog), Strg+O öffnet den neuen DATEI-DIALOG
    (ui/dialog.rs, wiederverwendbar: Ordner-ScrollListe +
    Pfad-Eingabe + OK/Abbrechen; Doppelklick navigiert/wählt) —
    der Explorer-Doppelklick auf Dateien öffnet jetzt SpeedText,
    der alte Nur-Lese-Betrachter ist Geschichte
  * Titelleiste zeigt "name.txt * - SpeedText" (Stern = ungespeichert,
    via neuem AppReaktion.titel); Schließen mit Änderungen fragt nach
    (Speichern/Verwerfen/Abbrechen) über den generischen
    BESTÄTIGUNGS-Dialog + neuen App::schliessen_abfragen-Hook und
    AppReaktion.schliessen
  * Statusleiste: Zeile:Spalte, Zeichenzahl, Änderungs-Status
- Unit-Tests: TextPuffer (Einfügen/Löschen über Zeilengrenzen,
  Umlaute, Cursor-Klemmen, Scroll-Folgen, Roundtrip), Datei-Roundtrip
  über das Test-VFS, Datei-Dialog-Zustandsmaschine, Schließen-Dialog-
  Logik, Terminal-Sitzungen (Unabhängigkeit, Haupt-Vererbung,
  Lebenszyklus, Log-Puffer-Deckel)

### Hinzugefügt (Task-Manager: App + Executor-Übersicht)
- Executor-Erweiterung (src/task/uebersicht.rs): Jeder Task bekommt
  beim Spawnen einen NAMEN, seine Id und den Startzeitpunkt; eine
  globale Registry (Blatt-Lock) führt die Schatten-Buchhaltung, die
  heißen Zähler (Polls, Wecken, wach/schläft) sind Atomics in einem
  Arc, das sich Executor, Waker und Registry teilen — der Waker
  feuert aus Interrupt-Handlern und darf keinen Lock nehmen.
  momentaufnahme() liefert die sortierte Liste (Id, Name, Art,
  Laufzeit, Status, Zähler), anzahl() die Gesamtzahl. Alle
  bestehenden spawn-Aufrufe tragen jetzt sinnvolle Namen
  (SpeedShell (Terminal), Compositor, Desktop-Uhr, PS/2-Maus, ...)
- CPU-Metrik: Der Executor misst per TSC die Zeit in
  run_ready_tasks (Arbeit) vs. im hlt-Schlaf (Ruhe) und verbucht
  beides in einem gleitenden Fenster aus 10 Eimern à 100 ms —
  cpu_auslastung_prozent() ist die ehrliche System-Auslastung über
  ~1 s (reine, unit-getestete Fenster-Logik)
- src/taskmanager.rs — die Task-Manager-App: Kopfzeile mit CPU-%,
  Live-Graph der letzten 60 s (Linien-Rendering mit Spalten-MAXIMUM
  beim Downsampling — Spitzen bleiben sichtbar) und Heap-Belegung;
  Tabelle (Id, Name, Art Kernel/Fenster/Demo, Laufzeit, Polls/s,
  wach/schläft) mit sekündlichem tick-Update, Auswahl überlebt als
  Task-ID; "Task beenden" wirkt nur auf beendbare Tasks (Demos),
  bei geschützten Kernel-/Fenster-Tasks ist der Knopf gedimmt
  (neues Button::mit_deaktiviert). EHRLICH dokumentiert (Info-Zeile
  + Code): Kooperative Tasks werden beim nächsten Executor-
  Durchlauf an ihrem await-Punkt FALLEN GELASSEN (Drop), nicht
  abgeschossen — "Demo-Task starten" spawnt einen beendbaren
  Zähler-Task zum gefahrlosen Ausprobieren
- Neues Icon ICON_TASKS (Balkendiagramm); Registry-Eintrag
  "Task-Manager" im Startmenü
- Unit-Tests: Momentaufnahme-Konsistenz (Zähler, Sortierung,
  Beenden-Schutz), Auslastungs-Gleitfenster mit synthetischen
  Zahlen (Rotation, Zeitsprünge, 100-%-Deckel), Graph-Downsampling
  (Spalten-Maximum, Achsen-Abbildung, Grenzfälle), Laufzeit-Format

### Hinzugefügt (Einstellungen: App + persistenter Store)
- src/einstellungen.rs, Teil 1 — der persistente Einstellungs-Store:
  Schlüssel=Wert-Datei /system/einstellungen.txt im VFS (parsen/
  serialisieren als reine, unit-getestete Funktionen; Kommentare und
  kaputte Zeilen werden toleriert), typisierter Zugriff (hole_/
  setze_zahl/bool/text — jedes setze_* speichert SOFORT), Laden +
  Anwenden beim Boot (main.rs nach fs::init). Die API-Naht für
  Serie 4: Kommt das Disk-Dateisystem, wird nur das gemountete VFS
  getauscht — dann überleben die Werte auch echte Neustarts
- Teil 2 — die Einstellungen-App (ui::App, Kategorien-ScrollListe
  links, Inhaltsseiten rechts):
  * Personalisierung: Theme Dunkel/Hell, Akzentfarbe aus 6er-Palette
    (NEU: unabhängig von Hell/Dunkel, mit passender Farbvariante je
    Theme — theme::aktuell() liefert jetzt eine Kopie mit
    eingesetztem Akzent), Desktop-Hintergrund aus 5 Verlauf-Presets
    (theme::hintergrund_verlauf; der Compositor-Hintergrund-CACHE
    wird über hintergrund_neu invalidiert)
  * Anzeige: Auflösung (nur Anzeige, mit SPEEDOS_AUFLOESUNG-Hinweis),
    UI-Skalierung 1.0/1.5/2.0 direkt wählbar, Cursor-Blinktempo
    (wirken live: Textfeld-Cursor + Konsolen-Blink-Task lesen
    einstellungen::cursor_blink_ms)
  * Datum & Uhrzeit: Live-Uhr, UTC-Offset in 30-min-Schritten und
    12/24h-Format für die Systray-Uhr (jetzt_lokal/uhrzeit_text;
    dokumentierte Annahme: die RTC liefert in QEMU die Host-
    LOKALZEIT, der Offset ist eine reine Anzeige-Verschiebung)
  * Info: Logo, Version aus Cargo.toml (Compile-Zeit-env!),
    Auflösung, Speicher frei/gesamt (frame_statistik), TSC-Frequenz,
    Uptime live (tick beim Sekundenwechsel), Task-Anzahl (neues
    Atomic im Executor)
- Alle Optionen wirken SOFORT und überleben Fenster-zu/auf — per
  QMP-Fernsteuerung in QEMU verifiziert (Screenshots: Theme, Akzent
  Grün, Ozean-Hintergrund, 12h-Systray, Skalierung; die Datei per
  `type /system/einstellungen.txt` geprüft). Muster dafür: Atomics
  UNTER dem MANAGER-Lock setzen, Neuzeichnen via AppReaktion.danach
  -> fenster::alles_neu_zeichnen()
- Toolkit-Ausbau: Button::mit_aktiv (markierte Wahl in Options-
  Gruppen), Farbfeld- und Icon-Widgets in der App; Registry-Apps
  "Theme wechseln"/"Skalierung" persistieren ihre Wahl jetzt auch
- Gespeicherte UI-Skala schlägt beim Desktop-Start die Auto-Wahl
  nach Bildschirmbreite

### Hinzugefügt (Explorer Teil 2: Dateioperationen)
- Umbenennen mit F2 (der Auswahl-Eintrag wird zur Eingabezeile, die
  App puffert über den taste-Hook; Enter übernimmt, Esc bricht ab),
  Neu-Ordner/Neu-Datei (Werkzeugleiste + Kontextmenü; legt mit
  eindeutigem Namen an und springt direkt in den Umbenennen-Modus),
  Entf verschiebt in den Papierkorb
- Papierkorb /papierkorb: Der Ursprungs-Ordner wird in einer
  METADATEN-Datei (<name>.herkunft) gemerkt — begründet im Code:
  normale VFS-Zugriffe statt Namens-Parser, echter Anzeigename,
  die Ansicht filtert die Metadaten einfach aus. Papierkorb-Ansicht
  mit Wiederherstellen (Konflikt -> " (2)") und Endgültig-Löschen;
  Entf im Papierkorb löscht endgültig
- Kopieren/Ausschneiden/Einfügen mit Strg+C/X/V — die Ablage ist
  ein GLOBALER Zustand (src/ablage.rs, Grundstein der System-
  Zwischenablage): funktioniert innerhalb eines und zwischen zwei
  Explorer-Fenstern. Rekursives Kopieren/Löschen/Verschieben von
  Ordnern als fs-Helfer (Deadlock-Regel: liste() vor dem Abstieg
  abschließen); Namenskonflikt hängt automatisch " (2)" an.
  Strg-Erkennung: KeyStream dekodiert mit MapLettersToUnicode
  (Strg+C = U+0003 usw.)
- GENERISCHES Kontextmenü-Overlay im Fenster-Manager (Offscreen-
  Puffer + Blit wie Startmenü/Switcher; Empfänger = FensterId,
  Taskleiste/Desktop können es später nutzen): Rechtsklick auf
  Eintrag (Öffnen/Umbenennen/Kopieren/Ausschneiden/Löschen, im
  Papierkorb + Wiederherstellen) und auf freie Fläche (Einfügen/
  Neu/Aktualisieren). Neu dafür: UiEreignis::Rechtsklick,
  ScrollListe::mit_rechtsklick, AppReaktion::menue sowie
  AppReaktion::danach als Box<dyn FnOnce> (Aktionen MIT Daten,
  NachLock::Einmal)
- Aktualisieren-Button (@): Shell und Explorer sehen dasselbe VFS —
  write /test.txt im Terminal, @ im Explorer, Datei ist da
- Doppelklick auf Datei öffnet den minimalen Betrachter
  (BetrachterApp: Pfad, Zeilen-ScrollListe, nur Lesen)
- Unit-Tests: eindeutiger_name, rekursives Kopieren/Löschen (echtes
  Test-VFS), Papierkorb-Roundtrip (Herkunft-Metadaten,
  Wiederherstellen, Endgültig)

### Hinzugefügt (Explorer — die erste echte App auf dem Toolkit)
- src/explorer.rs: ExplorerApp (ui::App) mit Werkzeugleiste
  (Zurück/Vor/Hoch + klickbare Breadcrumbs + KlickFlaeche für den
  Eingabemodus), Ordnerbaum-Spalte (aufklappbar ab /, Klick
  navigiert), Dateiliste (Icons nach Typ, Größen als B/KiB/MiB,
  Ordner-zuerst-Sortierung) und Statusleiste (Eintragszahl,
  Auswahl-Info, Fehlermeldungen)
- Navigation: Doppelklick/Enter öffnet Ordner, Backspace/^-Button =
  hoch, Zurück/Vor-Verlauf wie im Browser (Vorwärts-Historie wird
  beim Abbiegen gekappt), Adressleiste per Klick tippbar (Enter
  navigiert, Esc bricht ab); mehrere Explorer-Fenster laufen als
  eigene App-Instanzen unabhängig — alles per QMP verifiziert
- Toolkit-Ausbau dafür: ScrollListe mit Index-kodierten Nachrichten
  (BASIS+Index — Apps erfahren, WELCHER Eintrag), Fokus +
  Pfeiltasten/Enter-Navigation, Auswahl-Erhalt über Neu-Aufbauten
  (Scroll als Cell, auswahl_sichtbar), konfigurierbarem Layout;
  BoxContainer::mit_flex; UiFenster::fokus_initial;
  App::taste-Hook (App-Shortcuts/Eingabemodi VOR den Widgets)
- 4 neue Unit-Tests: Breadcrumb-Zerlegung + Elternpfad,
  Größen-Formatierung, Ordner-zuerst-Sortierung, Browser-Verlauf

### Hinzugefügt (UI-Skalierung + Dirty-Rect-Compositing — die 4K-Baustellen)
- UI-Skalierung 1.0/1.5/2.0 (in Halben, soft-float-frei): metrik()
  liefert die skalierte Metrik, Schrift mappt auf die vorgerasterten
  Fonts 16/24/32 (neues Cargo-Feature size_24). Boot-Standard nach
  Breite (>=2560 -> 1.5, >=3840 -> 2.0), Laufzeit-Umschaltung über
  die neue Registry-App "Skalierung" (Mechanik wie Theme-Wechsel).
  4K-Screenshot mit Faktor 2.0: docs/screenshots/desktop-4k-skaliert
- Dirty-Rect-Compositing: Änderungen melden ihre Fläche (Drag/Resize
  alte+neue, Uhr-Tick nur den Systray, Menü/Switcher ihre Panels,
  max. 16 Rects mit Vollbild-Fallback); der Compositor komponiert
  je Rect mit Clip (Fenster ohne Schnitt übersprungen) und presentet
  nur diese Bereiche. Desktop-Verlauf als byte-identischer Cache im
  DoppelPuffer (Wiederherstellen = memcpy pro Zeile; die erste
  Fassung als Farbe-Array war LANGSAMER als der Gradient — Lehre:
  Cache immer im Zielformat)
- Messwerte (Berichts-Test, drei Szenarien, warm):

  | Szenario     | 720p vorher | 720p nachher | 4K vorher | 4K nachher |
  |--------------|-------------|--------------|-----------|------------|
  | Vollbild     | ~1,2 ms     | ~1,1 ms      | ~9,3 ms   | ~9,3 ms    |
  | Uhr-Tick     | = Vollbild  | 0,25 ms      | = Vollbild| 0,31 ms    |
  | Fenster-Drag | = Vollbild  | 0,37 ms      | = Vollbild| 0,41 ms    |
  (vorher war JEDER dirty Frame ein Vollbild-Frame; Ziel
  "Uhr-Tick bei 4K unter 1 ms" mit 0,31 ms erreicht)
- Taskleiste skaliert sauber mit (Logo waechst mit der Leiste,
  Uhr/Datum als zentrierter Block statt fester Offsets)

### Geändert (Toolkit-Beweis: App-Trait, Startmenü + Alt+Tab umgezogen)
- trait ui::App (name/icon/aufbau/nachricht/tick) + Inhalt::App als
  Brücke vom Enum zum Trait — jede NEUE App implementiert das Trait
  (Enum bleibt für Terminal/Demos); fenster::app_starten öffnet
  Trait-Apps, die Widget-Galerie ist die erste (Nachrichten laufen
  über App::nachricht statt fn-Handler; Lock-Regel in ui/app.rs)
- Startmenü aufs Toolkit umgezogen: Suchfeld = Textfeld-Widget (neue
  Änderungs-Nachricht für den Live-Filter), App-Liste = ScrollListe;
  Panel zeichnet in einen Offscreen-Puffer, der Compositor blittet
  (Overlay-Muster). Verhalten unverändert: Super-Taste, Klick
  außerhalb schließt, Tippen filtert live, Pfeile + Enter, Klick
  startet — per QMP-Screenshot-Vergleich verifiziert
- Alt+Tab-Switcher ebenso: ScrollListe mit Auswahl-Highlight statt
  handgemalter Liste (auswahl_bewegen mit Wrap-Around)
- Aufräum-Bilanz der Event-Kaskade: maus_gedrueckt 61 -> 55 Zeilen;
  handgestrickter Startmenü-Sondercode (Tasten-Kaskade, Eintrags-
  Geometrie, Hover-Pflege, Panel-Malerei) 161 -> 69 Zeilen
  Widget-Anbindung; switcher_zeichnen (46 Zeilen) ersatzlos
- NachLock::AppStarten -> NachLock::Ausfuehren (traegt jetzt auch
  AppReaktion.danach-Aktionen)

### Hinzugefügt (UI-Widget-Toolkit: das Fundament aller Apps)
- Neues Modul src/ui/: retained Widget-Baum mit `trait Widget`
  (wunschgroesse/zeichnen/ereignis), UiEreignis (Klick, Doppelklick,
  Losgelassen, Bewegt, Scroll, Taste, MausRein/Raus, FokusRein/Raus)
  und UiReaktion (verbraucht + neu_zeichnen + Nachricht kombinierbar);
  App-Nachrichten als u32-ID an fn(u32)-Handler (Begründung im
  Kopfkommentar: keine Closures/Generics im no_std-Kernel)
- Hover-Konzept: das Container-Routing erzeugt MausRein/MausRaus
  (Fenster-Ebene: ui_hover_fenster im Manager, MausRaus beim
  Fensterwechsel); Fokus-Kette pro Fenster mit Tab (Wrap-Around),
  Tasten laufen zum fokussierten Widget
- Layout bewusst primitiv: laengen_verteilen (pure) + VBox/HBox mit
  METRIK-Abständen und Fueller (flex) — kein Constraint-Solver
- Widgets im Aurora-Stil: Label (mehrzeilig), Trennlinie, Button
  (Hover/Pressed, Icon optional, feuert beim Loslassen im Bereich),
  Checkbox, Textfeld (Innenleben = ZeilenEditor der Shell, Cursor
  blinkt über die zeit-API), ScrollListe (Scrollrad, ziehbarer
  Scrollbalken, Auswahl-Highlight, Doppelklick-Nachricht)
- Fenster-Integration: Inhalt::Ui, Maus-Routing inkl. Scroll und
  Losgelassen ins Fenster, NachLock-Enum (Ui-Nachrichten laufen wie
  App-Starts nie unter dem MANAGER-Lock); Panic-Handler meldet
  zuerst roh seriell (Deadlock-Fix bei Panik unter dem Lock)
- Widget-Galerie als Demo-App im Startmenü: zeigt alle Widgets,
  loggt jede Interaktion seriell — in QEMU end-to-end bedient
  (Tab-Fokus, Tippen, Button-Klick per Maus, Scroll, Doppelklick)
- 10 neue Unit-Tests: Layout-Verteilung, Klick-Routing,
  Hover-Enter/Leave, Fokus-Kette mit Tab, Doppelklick-Erkennung,
  Scroll-Klemmen, ScrollListe-Sichtbereich/Auswahl, Button-Zustände,
  Textfeld+Checkbox

### Hinzugefügt (Echte Zeit: TSC-Zeitquelle + RTC-Uhrzeit)
- TSC als monotone Zeitquelle: zeit::init() kalibriert den TSC beim
  Boot gegen den PIT (2 Messungen à ~100 ms, loggt Frequenz,
  Abweichung in Promille, Dauer und den CPUID-Invariant-Status);
  us_seit_boot()/ms_seit_boot() laufen seitdem über den TSC —
  mikrosekundengenau und unabhängig von Interrupts (kein Uhr-
  Stillstand mehr unter without_interrupts). Der PIT ist nur noch
  Weckgeber für warte_ms/Executor und Fallback vor der Kalibrierung
- Neues Modul rtc.rs: liest die CMOS-Echtzeituhr einmal beim Boot
  (Update-in-Progress-Flag, BCD- und 12h-Modus, Doppel-Lesen bis
  stabil, Timeout bei fehlender RTC); zeit::jetzt() liefert daraus
  plus TSC echte Uhrzeit und echtes Datum — die Taskleiste zeigt
  jetzt die WIRKLICHE Zeit (QEMU-RTC via -rtc base=localtime auf
  der Host-Uhr). Datums-Arithmetik auf "Sekunden seit 1.1.2000"
  umgestellt (reine, roundtrip-getestete Funktionen)
- Die Mess-Falle aus dem Qualitäts-Pass ist damit tot: Zeit darf
  überall genommen werden, der Frame-Zeit-Berichts-Test misst in µs
- 6 neue Tests: TSC-Kalibrierung plausibel (100-ms-Messung ±20 %),
  Uhr läuft unter without_interrupts weiter, BCD-Konvertierung,
  RTC-Konvertierung (BCD/24h und binär/12h inkl. Mitternacht),
  Datums-Roundtrip inkl. Schalttag

### Geändert (Qualitäts-Pass Grafik-Schicht)
- Zeilen-Schnellpfade in der Zeichenflaeche: flaeche_zeile_fuellen /
  flaeche_zeile_kopieren (Default korrekt pro Pixel; DoppelPuffer
  per Muster-Verdopplung/Formatwandlung einmal pro Zeile,
  FensterPuffer per slice fill/copy). Der Zeichner clippt Rechtecke
  VORAB (sichtbar = Rechteck ∩ Clip ∩ Fläche) — rechteck_fuellen
  (deckend), verlauf_vertikal und der neue puffer_blit laufen ohne
  Pro-Pixel-Prüfungen; der Compositor blittet Fenster-Inhalte damit
- Gemessen (1360x768, 3 Fenster + Drag, WHPX): 2,30 -> 1,20 ms/Frame
  im Erstlauf, eingeschwungen 0,40 ms/Frame; isolierter Fenster-Blit
  (560x140, 100x): 25,8 -> 4,4 ms (Faktor ~6; präzise nachgemessen
  mit der späteren TSC-µs-Uhr)
- 8 neue Unit-Tests: Koordinaten-Grenzen, Resize-Zonen, Z-Ordnung,
  Dirty-Flag-Logik, Theme-Umschaltung färbt Puffer, Schnellpfad-
  Clipping, Speicherleck-Schleife (Fenster+Terminal öffnen/schließen),
  Frame-Zeit-Messung (Berichts-Test)
- Speicher-Pass: kein Leck gefunden (Heap-Belegung nach 30 Zyklen
  Fenster+Terminal öffnen/schließen unverändert)
- unsafe-Audit: Serie 2 (Theme, Taskleiste, Startmenü, Terminal,
  Schnellpfade) kommt komplett OHNE neue unsafe-Blöcke aus

### Hinzugefügt (Auflösungswahl 720p bis 4K)
- SPEEDOS_AUFLOESUNG wählt den Grafikmodus (720p Standard, 1080p,
  1200p, 2k, 1600p, 4k, 5k, 8k oder frei BREITExHOEHE) — der Runner
  leitet VRAM (vgamem_mb als Modus-Wähler, da OVMF den EDID-Wunsch
  ignoriert und immer den größten passenden Modus nimmt) und
  Arbeitsspeicher (-m, ~20 B/Pixel + Grundbedarf) automatisch ab
- 1080p und 4K treffen exakt; 5K/8K werden ehrlich auf das
  Firmware-Maximum 4096x2160 gedeckelt (5K fehlt in der
  edk2-Modustabelle, 8K lässt die Firmware hängen) — der Kernel
  selbst ist auflösungsunabhängig (in 4K end-to-end geprüft)

### Geändert (Performance gegen Maus-/Desktop-Lag)
- QEMU mit Hardware-Virtualisierung (-accel whpx, TCG-Fallback)
- Kernel baut im dev-Profil mit opt-level 2 (Debug-Checks bleiben)
- Standard-Auflösung 1360x768 statt 2560x1600 (4x weniger Pixel)
- PIT von ~18,2 Hz auf 250 Hz (zeit::PIT_TEILER zentral) — 4-ms-
  statt 55-ms-Auflösung für warte_ms, Compositor erreicht ~33 FPS
- Maus-Abtastrate nach IntelliMouse-Init auf 200/s (statt 80/s)

### Hinzugefügt (Desktop komplett: Theme, Taskleiste, Startmenü, Terminal)
- Theme-System (src/theme.rs): ALLE UI-Farben in einer zentralen
  Struktur, zwei Themes "Aurora Dunkel" (Standard) und "Aurora Hell",
  zur Laufzeit umschaltbar (alle Fenster werden neu gezeichnet);
  METRIK bündelt alle Abstände und Schriftgrößen — keine
  hartcodierten Farben mehr im UI-Code
- Taskleiste (40px, unten, immer im Vordergrund): SpeedOS-Startknopf,
  ein Knopf pro Fenster (Icon + Titel, fokussiert hervorgehoben,
  Klick = Fokus/Minimieren-Toggle, stabile Reihenfolge), Systray mit
  Platzhalter-Icons und Uhrzeit + Datum (aus Ticks abgeleitet;
  RTC-Kalibrierung bleibt bekanntes TODO)
- App-Registry (src/apps.rs): Name + Icon + Start-Funktion — die
  Architektur für alle künftigen Apps; Einträge: Terminal, Uhr,
  Tastatur-Echo, Malkasten, Theme wechseln, Neustart
- Startmenü im Aurora-Stil (Startknopf oder Super-Taste): Suchfeld
  filtert die App-Liste live (Grundlage der späteren Schnellsuche),
  Bedienung per Maus (Klick/Hover) und Tastatur (Pfeile, Enter, ESC)
- SpeedShell als Terminal-FENSTER: konsole::_print leitet im
  Desktop-Modus in ein unit-getestetes Text-Raster um
  (fenster/terminal.rs, Zellen/Cursor/Scrolling/Resize), gerendert
  gebündelt pro Compositor-Frame; serielle Doppel-Ausgabe unberührt
- SpeedOS bootet jetzt DIREKT in den Desktop (Terminal offen);
  ESC wechselt zur Vollbild-Konsole, `desktop` zurück
- Fenster-Icons je nach Inhalt (Terminal/Uhr/Tastatur/Pinsel) in
  Titelleiste, Taskleiste und Alt+Tab; 6 neue 16x16-Icons
- Datum/Uhrzeit-Arithmetik in zeit.rs (Schaltjahre, Monatsübertrag),
  Neustart-Logik nach lib.rs gezogen (Shell-Befehl + App teilen sie)
- 9 neue Unit-Tests (Terminal-Raster, Taskleiste, Startmenü,
  App-Filter, Datum, Theme)

### Hinzugefügt (Fenster-Deko & volle Bedienung)
- Titelleiste pro Fenster: Icon, Titel, 3 Knöpfe (Minimieren,
  Maximieren/Wiederherstellen, Schließen rot); Aurora-Verlauf beim
  fokussierten, gedimmt beim inaktiven Fenster; Schatten (Alpha)
- Verschieben per Titel-Drag, Größe ändern per Rand-/Ecken-Drag
  (Cursor wechselt die Form: horizontal/vertikal/diagonal)
- Minimieren (lebt weiter), Maximieren (Vollbild minus 40px
  Taskleisten-Reserve), Schließen (Fenster-Puffer wird freigegeben)
- Alt+Tab-Fensterwechsler mit zentriertem Overlay (Titelliste +
  Auswahl-Highlight), Loslassen von Alt wechselt; holt auch
  minimierte Fenster zurück
- Snap-Layouts: Ziehen an den linken/rechten Rand = halbe Fläche
  (mit halbtransparenter Vorschau)
- grafik::Zeichner generisch über Zeichenflaeche-Trait (Apps malen
  in ihren Puffer, Compositor auf den Bildschirm — identisch);
  framebuffer::zeile_fuellen (schneller Zeilen-Füller),
  present_bereich, pixel_setzen_vorne
- zeit::warte_ms nutzt eine Slot-Liste von AtomicWakern (mehrere
  gleichzeitige Tick-Warter: Cursor, Compositor, Uhr)
- Heap wächst beim Desktop-Start passend zur Auflösung
- 3 neue Unit-Tests (Minimieren/Schließen, Größe ändern, Snap)

### Hinzugefügt (Fenster-Desktop)
- src/fenster/mod.rs: Fenster mit eigenem Pixel-Puffer + Metadaten,
  FensterManager mit Z-Ordnung und Fokus-Verwaltung
- Compositor-Task mit Dirty-Flags (nur komponieren, wenn nötig):
  Aurora-Hintergrund -> Fenster in Z-Reihenfolge -> present -> Cursor
- Event-Routing: Maus -> oberstes Fenster unter dem Cursor (in
  Fenster-Koordinaten), Klick hebt+fokussiert; Tastatur -> Fokus-
  Fenster; Drag an der Titelzeile verschiebt
- grafik::Zeichner generisch über Zeichenflaeche-Trait (Apps malen
  mit denselben Primitiven in ihren Puffer wie der Compositor auf
  den Bildschirm); schneller Zeilen-Füller (framebuffer::zeile_fuellen)
- zeit::warte_ms: mehrere gleichzeitige Tick-Warter (Slot-Liste
  statt einem AtomicWaker)
- Shell-Befehl `desktop`: 3 Demo-Fenster (Live-Uhr, Tastatur-Echo,
  statische Grafik mit Klick-Markern), ESC kehrt zur Konsole zurück
- 3 Unit-Tests (Fokus/Z-Ordnung, Verschieben, Koordinaten/Routing)

### Hinzugefügt (PS/2-Maus)
- Maus-Treiber am zweiten 8042-Port: vorsichtige Controller-Init
  (Tastatur-Bits unangetastet, alles mit Timeouts), IntelliMouse-
  Scrollrad (200/100/80-Magie, 4-Byte-Pakete)
- Reines, unit-getestetes Paket-Parsing (Sync, 9-Bit-Vorzeichen,
  Overflow, Tasten, 4-Bit-Rad)
- IRQ 12 -> lock-freie Queue -> async maus_task; globaler Zustand
  (geklemmte Position, Tasten) und MausEvent-Typ (Bewegt/Gedrueckt/
  Losgelassen/Gescrollt)
- Pfeil-Cursor als Front-Buffer-Overlay (Back-Buffer bleibt
  cursorfrei; Wiederherstellen = present_bereich)
- grafiktest interaktiv: Klick malt Punkte, Rad wechselt die
  Malfarbe (Anzeige unten rechts)

### Hinzugefügt (Zeichen-Werkzeuge: grafik.rs)
- Zeichner auf dem Back-Buffer: Linien (Bresenham), Rechtecke
  (gefüllt/Rahmen/abgerundet), Kreise (gefüllt/Midpoint-Rahmen),
  vertikale Farbverläufe, Text an Pixelposition (per Intensität
  auf jeden Untergrund geblendet), Bitmap-Blitting mit
  Transparenz-Farbe
- Rgba-Farben mit Alpha-Blending (reine, unit-getestete Formel)
- Clipping: optionales Clip-Rechteck für alle Operationen
  (Schnitt-Mathematik unit-getestet)
- Eingebettetes Icon-Format (16x16-ASCII-Art + Palette) mit vier
  Icons: Ordner, Datei, Zahnrad, SpeedOS-Logo
- Shell-Befehl grafiktest: zeigt alle Primitive, beliebige Taste
  kehrt zur Konsole zurück; framebuffer::pixel_lesen für Alpha

### Hinzugefügt (Framebuffer-Konsole — die Konsole ist zurück!)
- framebuffer.rs: Double Buffering (Back-Buffer aus allocate_pages,
  present() als Block-Kopie), Pixel-/Text-Primitive mit Antialiasing
  (Farbmischung), hochscrollen() als memmove im Back-Buffer
- Font: noto-sans-mono-bitmap (vorgerastert, pure Rust, Latin-1 =
  Umlaute ä ö ü ß; Größe 16 Konsole, 32 fett für den Boot-Screen)
- konsole.rs ist jetzt die FramebufferKonsole: Zeichenraster,
  Zeilenumbruch, Scrolling, Farben pro Zeichen (Obsidian-Aurora-
  Palette) — print!/println! schreiben wieder DOPPELT (Bildschirm +
  seriell mit ANSI), Shell und Makros blieben unberührt (die Naht!)
- Software-Cursor: blinkt über einen async Task; dafür zeit::warte_ms
  mit Tick-Future (Timer-Interrupt weckt per AtomicWaker — kein
  Busy-Polling, die CPU schläft zwischen den Blinks)
- Boot-Screen: Obsidian-Hintergrund, Aurora-Farbverlauf
  (violett-blau-cyan), SpeedOS-Schriftzug in Größe 32 fett
- QEMU-Auflösung per EDID auf 1280x720 gewünscht (Runner)

### Geändert (Migration auf bootloader 0.11 — UEFI + Framebuffer)
- Kernel bootet jetzt per UEFI (edk2/OVMF) und bekommt einen linearen
  Framebuffer vom Bootloader (QEMU: 2560x1600 BGR) — Grundlage für
  alles Grafische; beim Boot wird er mit SpeedOS-Blau gefüllt und
  seine Eckdaten seriell geloggt
- Build-System: eingebautes Target x86_64-unknown-none (Target-JSON,
  build-std, bootimage entfallen); neues boot/-Crate als Runner baut
  das UEFI-Disk-Image und startet QEMU (inkl. Test-Exit-Codes)
- VGA-Textmodus ist Geschichte: vga_buffer.rs gelöscht; konsole.rs
  liefert die gewohnte Farb-API übergangsweise als ANSI-Codes über
  die serielle Leitung (Shell voll benutzbar im Terminal)
- Drei UEFI-Lektionen gefixt und dokumentiert: SS/DS/ES nach
  GDT-Laden neu setzen (sonst #GP bei iretq), PIT selbst
  programmieren, PIC-Masken selbst setzen + LAPIC deaktivieren
- Migrationsplan in docs/migration-011.md

### Geändert (Shell-Editor, Executor, Zeit-API)
- ZeilenEditor aus shell::run() extrahiert (shell/editor.rs):
  Eingabepuffer, Verlauf und Tab-Vervollständigung als reine Logik
  mit Taste-/Reaktion-Enums und Vervollstaendiger-Trait — komplett
  per Unit-Test abgedeckt (8 Tests mit Mock, ohne Tastatur/VGA/VFS)
- Executor: task::spawn() erlaubt Tasks, selbst Tasks zu starten
  (globale Spawn-Queue); volle Warteschlangen panicken nicht mehr
  (Überlauf-Flag -> alle Tasks einmal pollen, kein Wecken geht
  verloren); Queue-Kapazität konfigurierbar (mit_kapazitaet)
- Neue Zeit-API src/zeit.rs: ticks(), ms_seit_boot(), ms_von_ticks()
  — zentrale Naht für den späteren Wechsel auf den APIC-Timer;
  ticks-Befehl zeigt jetzt die Uptime in Sekunden

### Geändert (Speicherverwaltung generalüberholt)
- Mapper + Frame-Allocator sind jetzt global (`Mutex<Option<...>>` in
  memory.rs, Muster wie das VFS) — jedes Modul kann zur Laufzeit Pages
  mappen. Neue API: map_page, map_page_zu (MMIO), unmap_page,
  allocate_pages (virtuell UND physisch zusammenhängend, für
  Framebuffer/DMA), frame_allozieren/frame_freigeben, uebersetzen,
  frame_statistik
- BootInfoFrameAllocator (O(n²), konnte nie freigeben) ersetzt durch
  Bitmap-Frame-Allocator: Next-Fit-Suche, O(1)-Freigabe mit
  Doppel-Freigabe-Erkennung, freigegebene Frames werden sofort
  wiederverwendet, zusammenhängende Allokation möglich
- Heap wächst zur Laufzeit: allocator::heap_erweitern(pages) —
  alle drei Allocatoren unterstützen extend
- meminfo zeigt jetzt auch die physische Frame-Statistik
- 5 neue Tests (Frame-Wiederverwendung, Kontiguität, map/unmap-
  Lebenszyklus, map_page_zu, Heap-Erweiterung via try_reserve)

### Verbessert
- Qualitäts-Pass: alle Clippy-Lints behoben, `# Safety`-Dokumentation
  für alle unsafe-Funktionen, Begründungs-Kommentare an jedem
  unsafe-Block, README/CHANGELOG ergänzt

## 0.1.0 — Meilensteine bis Juli 2026

### RAM-Dateisystem mit VFS (a55873d)
- `FileSystem`-Trait als VFS-Abstraktion (lesen, schreiben, liste,
  mkdir, loeschen, node_typ) — vorbereitet für FAT32/Disk-Dateisysteme
- RamFs: hierarchisches In-Memory-Dateisystem (BTreeMap-Baum)
- 9 neue Shell-Befehle: dir, cd, mkdir, type, write, del, copy, move,
  tree; Prompt zeigt aktuelles Verzeichnis; Tab-Vervollständigung
- Demo-Dateien beim Boot (/willkommen.txt, /system/info.txt)

### SpeedShell (bc9c0b7)
- Interaktive Shell als async Task: Prompt mit blinkendem
  Hardware-Cursor, Zeileneingabe mit Backspace/Entf, Befehlsverlauf
  (10 Einträge, Pfeiltasten)
- Befehls-Registry über das `Befehl`-Trait: help, echo, clear, ticks,
  meminfo, version, farbtest, neustart

### Kooperatives Multitasking (555fdc5)
- Task-Typ um Futures, Executor mit Waker-Support (fair, FIFO,
  hlt im Leerlauf, race-freies sleep_if_idle)
- Tastatur auf async umgestellt: Interrupt-Handler füllt nur noch
  eine lock-freie Queue (crossbeam ArrayQueue + AtomicWaker)

### Kernel-Heap (759d7f6)
- 100-KiB-Heap ab 0x4444_4444_0000; alloc-Crate aktiviert
  (Box, Vec, String, BTreeMap im Kernel)
- Drei Allocatoren: linked_list (Standard) sowie Bump und
  Fixed-Size-Block als dokumentierte Lern-Alternativen (Features)

### Paging (e4186a0)
- Bootloader mappt den physischen Speicher komplett
  (map_physical_memory); OffsetPageTable über CR3
- BootInfoFrameAllocator vergibt Frames aus der Memory Map
- entry_point!-Makro für typgeprüfte Kernel-Einstiege

### Tastatur, Backspace & Entf (41fa008, 8ae2b30)
- 8259 PIC remappt (32-47), Timer-Tick-Zähler, PS/2-Tastatur mit
  deutschem QWERTZ-Layout (De105Key) inkl. Umlauten
- Deadlock-Schutz: Ausgabe-Locks nur mit deaktivierten Interrupts
- Backspace/Entf löschen (VGA + seriell konsistent)

### Exception-Handling (41678ec)
- IDT mit Breakpoint-, Page-Fault- und Double-Fault-Handler
- GDT/TSS mit Interrupt Stack Table: Double Fault läuft auf eigenem
  Notfall-Stack — Stack Overflow führt zu sauberer Meldung statt
  Triple Fault + Reboot

### VGA-Treiber (3c9c0f8)
- println!/print! schreiben immer auf VGA UND seriell
- Codepage-437-Übersetzung (ä ö ü Ä Ö Ü ß é è ° § ² …),
  Ersatzzeichen für Nicht-Darstellbares, Farb-API, Scrolling-Fix

### Grundgerüst (6c877b6)
- no_std-Kernel mit eigenem Entry Point und Panic-Handler
- Eigenes Target x86_64-speedos.json, bootimage + QEMU-Workflow
- Serieller Port (COM1) als Debug-Kanal
- Eigenes Test-Framework: Tests booten in QEMU, isa-debug-exit
  liefert den Exit-Code
