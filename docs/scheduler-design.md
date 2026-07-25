# Der präemptive Scheduler von SpeedOS — Entwurf VOR dem Code

Stand: Juli 2026, Serie 6 Teil 3. Dieses Dokument entsteht **bevor** eine Zeile
Scheduler-Code geschrieben wird — wie bei `docs/speedfs-format.md` und
`docs/tcp-scope.md`. Grund: Der Kontext-Wechsel ist die Stelle, an der ein
Denkfehler nicht als Bug, sondern als Triple Fault erscheint. Also wird zuerst
aufgeschrieben, was genau passieren soll.

Vorgeschichte: `docs/serie6-bestandsaufnahme.md` hat den APIC ausdrücklich
vertagt (der ehrliche Auslöser ist SMP, nicht User-Space). Der **PIT mit
250 Hz** ist damit unser Scheduler-Herz — derselbe Timer, der seit Serie 2 die
`warte_ms`-Schläfer weckt.

---

## 1. Die Architektur-Entscheidung: Executor und Scheduler koexistieren

### Was wir haben
Ein **kooperativer async-Executor** (`src/task/executor.rs`) mit inzwischen
einem Dutzend Kernel-Tasks: Compositor, Eingabe-Router, Shell-Sitzungen,
`netz_task`, Socket-Takt, Log-Schreiber, Maus, Desktop-Uhr. Sie geben die CPU
an ihren `await`-Punkten freiwillig ab. Das funktioniert seit Serie 2
zuverlässig, und **es soll nicht ersetzt werden**: Kernel-Tasks sind Code, den
wir selbst geschrieben haben und dem wir Kooperation zutrauen dürfen.

### Was dazukommt
**Prozesse** — Ring-3-Code, der uns nicht gehört. Ein Prozess kann (und wird)
eine Endlosschleife drehen, ohne je in den Kernel zurückzukehren. Ihm die CPU
freiwillig abzuverlangen ist keine Option; sie muss ihm **weggenommen** werden.

### Die Entscheidung: EINE Round-Robin-Runde, in der der Executor mitläuft

> **Der Executor ist selbst ein schedulebarer Kontext: der Kernel-Prozess
> (PID 0).** Er steht als ganz normaler Eintrag in der Prozess-Tabelle und
> bekommt seine Zeitscheibe wie jeder User-Prozess. *Innerhalb* seiner
> Zeitscheibe multiplext er weiter kooperativ zwischen den Kernel-Tasks.

Damit gibt es **zwei Ebenen**, klar getrennt:

```
   +-------------------------------------------------------------+
   |  Präemptiv, PIT-getrieben (20 ms), src/scheduler.rs         |
   |                                                             |
   |   PID 0 (Kernel-Prozess) --> PID 1 --> PID 2 --> PID 0 ...  |
   |        |                                                    |
   |        | innerhalb SEINER Zeitscheibe:                      |
   |        v                                                    |
   |   +-----------------------------------------------------+   |
   |   | Kooperativ, await-getrieben, src/task/executor.rs   |   |
   |   |  Compositor | netz_task | Shell | Maus | Uhr | ...  |   |
   |   +-----------------------------------------------------+   |
   +-------------------------------------------------------------+
```

**Warum das die richtige Aufteilung ist:**

1. **Ein einziger Wechsel-Mechanismus.** Es gibt keine Sonderbehandlung „der
   Kernel läuft" gegen „ein Prozess läuft". Der Timer-Interrupt sichert immer
   denselben Registersatz an derselben Stelle und wählt immer aus derselben
   Tabelle. Sonderfälle sind bei Kontext-Wechseln die Hauptfehlerquelle.
2. **Der Leerlauf bleibt genau, wie er ist.** Bisher schläft der Executor per
   `hlt` (`sleep_if_idle`, race-frei mit `enable_and_hlt`). Weil PID 0 als
   Prozess weiterläuft, ist der **Idle-Zustand einfach PID 0 mit leerer
   Task-Queue** — wir brauchen keinen separaten Idle-Prozess, und das mühsam
   erarbeitete `hlt`-Verhalten (samt CPU-Messung) geht nicht verloren.
   „Nichts lauffähig" kann per Konstruktion nicht vorkommen: PID 0 ist
   **immer** lauffähig, und er ist der, der schläft.
3. **Kein Verhungern der Oberfläche.** Ein User-Prozess in einer
   Endlosschleife bekommt genau eine Zeitscheibe von N+1 — Compositor, Maus
   und Shell laufen weiter. Genau das wollen wir sichtbar beweisen.
4. **Kein Umbau am Executor.** Er merkt vom Scheduler nichts. Er wird
   angehalten und fortgesetzt, ohne es zu bemerken — das ist die Definition
   von Präemption.

### Verworfene Alternativen (und warum)

| Alternative | Warum nicht |
|---|---|
| **Jeder Prozess wird ein async-Task** („Prozess als Future") | Ein `poll()` müsste nach Ring 3 springen und *irgendwann* zurückkommen — der Rücksprung kommt aber aus einem Interrupt, nicht aus einem `await`. Man müsste im Timer-Interrupt aus der Future „herausspringen", also genau den Kontext-Wechsel bauen — plus die ganze Future-Maschinerie obendrauf. Mehr Komplexität, kein Gewinn. |
| **Executor bleibt ausserhalb, Prozesse laufen „daneben"** (Timer wechselt nur zwischen Prozessen, der Kernel ist ein Sonderfall) | Braucht doch zwei Pfade („zurück in den Kernel" vs. „zu Prozess B") und eine Extra-Regel, wann der Kernel wieder drankommt. Genau die Sonderfälle, die (1) vermeidet. |
| **Kernel-Tasks werden echte präemptive Kernel-Threads** (jeder Task ein eigener Stack, präemptiv) | Wäre der „grosse" Weg (Linux-Kernel-Threads), macht aber jeden bisher lock-sicheren Kernel-Pfad angreifbar: Unsere Locks sind auf *kooperative* Kernel-Tasks ausgelegt. Das ist ein eigenes Projekt und kein Teil von „User-Space bauen". |
| **PID 0 bekommt Priorität (läuft, wann er will)** | Klingt gut fürs Gefühl (Desktop reagiert immer sofort), aber ein Prioritäts-Scheduler ist nicht Round-Robin und nicht das, was hier bewiesen werden soll. Notiert als möglicher nächster Schritt (siehe §8). |

### Der eine Kompromiss, ehrlich benannt
Bei 2 rechnenden User-Prozessen bekommt der Desktop nur noch ~1/3 der CPU;
Fenster-Ziehen wird dann sichtbar zäher. Das ist **korrektes**
Round-Robin-Verhalten und kein Fehler — aber es ist ein Kompromiss, und
er wird nicht versteckt. §8 nennt die Stellschrauben.

---

## 2. Der Kontext-Wechsel (Aufgabe 1) — der fiselige Kern

### Die Kernidee: der gesicherte Kontext IST ein Trap-Rahmen auf dem Kernel-Stack

Jeder Prozess hat einen **eigenen Kernel-Stack**. Wenn die CPU einen Trap
auslöst (Timer-Interrupt, `int 0x80`, Page Fault), landet sie auf genau diesem
Stack — bei einem Trap aus Ring 3 sorgt `TSS.RSP0` dafür. Unser
Assembler-Einstieg pusht dort **alle 15 General-Register**; die CPU hat davor
schon `RIP/CS/RFLAGS/RSP/SS` gepusht. Zusammen ist das der **komplette
Zustand** des unterbrochenen Prozesses — und er liegt vollständig auf *dessen*
Stack.

Damit reduziert sich „Kontext sichern und wiederherstellen" auf:

> **Ein Prozess-Kontext ist EINE Zahl: der RSP, an dem sein Trap-Rahmen liegt.**
> Umschalten heisst: `RSP` auf den Rahmen des *anderen* Prozesses setzen,
> Register zurückpoppen, `iretq`.

```
 Kernel-Stack von Prozess A (16 KiB)          PCB von A
 +------------------------------+ <- top      +-------------------+
 |  SS      (von der CPU)       |             | kontext = 0x...   | --+
 |  RSP     (von der CPU)       |             | ...               |   |
 |  RFLAGS  (von der CPU)       |             +-------------------+   |
 |  CS      (von der CPU)       |                                     |
 |  RIP     (von der CPU)       |                                     |
 |  rax rbx rcx rdx rsi rdi rbp |  <- unser Assembler-Einstieg        |
 |  r8 ... r15                  |                                     |
 +------------------------------+ <- kontext -----------------------+
 |  (frei — hier arbeitet der   |
 |   Rust-Dispatcher)           |
 +------------------------------+ <- unteres Ende (Guard-Page darunter)
```

### Der Ablauf im Assembler (drei Einstiege, EIN Ausstieg)

```
timer_entry:                       prozess_sterben:      (Ring-0-Stub nach einem
  push rax .. r15                    call scheduler_sterben   Fault in Ring 3)
  mov  rdi, rsp                      mov rdi, rax
  call timer_dispatch                jmp schalte_auf_rahmen
  mov  rdi, rax
  jmp  schalte_auf_rahmen          syscall_entry:
                                     push rax .. r15
schalte_auf_rahmen:                  mov  rdi, rsp
  mov rsp, rdi     <-- DER WECHSEL   call syscall_dispatch
  pop r15 .. rax                     mov  rdi, rax
  iretq                              jmp  schalte_auf_rahmen
```

Der Rust-Dispatcher bekommt den Rahmen als Zeiger und **gibt einen Rahmen
zurück** — denselben (kein Wechsel) oder den eines anderen Prozesses (Wechsel).
`schalte_auf_rahmen` ist der einzige Ort im Kernel, an dem ein Kontext
wiederhergestellt wird. Das ist die wichtigste Vereinfachung dieses Entwurfs:
**Präemption, freiwillige Abgabe, Prozess-Start und Prozess-Tod laufen alle
durch dieselben drei Assembler-Zeilen.**

### Was der Dispatcher beim Wechsel zusätzlich tun MUSS

1. **`kontext = rsp`** im PCB des alten Prozesses speichern.
2. **CPU-Zeit** verbuchen (`us_seit_boot()` minus Startzeitpunkt der Scheibe).
3. **`CR3` wechseln** — `AdressRaum::aktivieren()` für einen User-Prozess,
   `adressraum::kernel_aktivieren()` für PID 0. `aktivieren()` frischt dabei
   den Kernel-Spiegel auf (siehe `src/adressraum.rs`).
4. **`TSS.privilege_stack_table[0]` (RSP0)** auf das **obere Ende** des
   Kernel-Stacks des neuen Prozesses setzen. Ohne das würde der nächste Trap
   aus Ring 3 auf dem Stack des *falschen* Prozesses landen und ihn zerstören.
   Das ist der klassische, leicht zu vergessende vierte Schritt.
5. Zeitscheibe zurücksetzen, `AKTUELL` umsetzen.

### Der Start eines Prozesses ist kein Sonderfall
Ein neuer Prozess bekommt **von Hand einen Trap-Rahmen** an das obere Ende
seines Kernel-Stacks geschrieben: `RIP = Einsprung`, `CS = User-Code (RPL 3)`,
`RSP = User-Stack-Spitze`, `SS = User-Data (RPL 3)`, `RFLAGS = 0x202` (IF
gesetzt), alle General-Register 0. Er sieht damit exakt so aus, als wäre er
schon einmal gelaufen und gerade verdrängt worden. Der Scheduler „startet" ihn
also nie — er **wechselt** nur zu ihm.

Daraus folgt eine tragende Invariante:

> **INVARIANTE 1:** Der erste Wechsel zu einem neuen Prozess passiert IMMER im
> Timer-Interrupt (oder in einem Syscall) — also an einer Stelle, an der der
> Kontext des Kernel-Prozesses ohnehin gesichert wird. Ein Prozess wird
> „eingeplant", nicht „gestartet". Deshalb gibt es keinen Pfad, auf dem PID 0
> ohne gesicherten Kontext verlassen wird.

### Ausrichtung (die Falle, die man nur einmal übersieht)
Die C-ABI verlangt `RSP % 16 == 0` unmittelbar vor einem `call`. Im Long Mode
richtet die CPU `RSP` **vor** dem Pushen des Interrupt-Rahmens auf 16 Byte aus.
Danach: 5 × 8 = 40 Byte (Rahmen) + 15 × 8 = 120 Byte (unsere Pushes) = 160
Byte, und 160 ist durch 16 teilbar → am `call` stimmt die Ausrichtung. Der
handgebaute Start-Rahmen liegt bei `top - 160` und damit ebenfalls
16-ausgerichtet. `sizeof(TrapFrame) == 160` ist deshalb kein Zufall, sondern
Voraussetzung — und wird per Test festgenagelt (siehe §7).

---

## 3. Prozess-Kontrollblock und Prozess-Tabelle (Aufgabe 2)

```rust
struct Prozess {
    pid:            Pid,             // 0 = Kernel-Prozess
    name:           String,
    zustand:        Zustand,         // Laeuft | Lauffaehig | Wartend | Beendet
    raum:           Option<AdressRaum>,  // None = Kernel (CR3 = Kernel-P4)
    kern_stack:     Option<KernStack>,   // None = Kernel (Boot-Stack)
    kontext:        u64,             // gesicherter RSP (0 = läuft gerade)
    start_us:       u64,             // Erzeugungszeitpunkt
    cpu_us:         u64,             // aufsummierte CPU-Zeit
    scheibe_start_us: u64,           // Beginn der aktuellen Zeitscheibe
    praemptionen:   u64,             // aus Ring 3 verdrängt (Beweis-Zähler!)
    abgaben:        u64,             // freiwillig abgegeben
    syscalls:       u64,
    wach_ab_ms:     u64,             // für Zustand::Wartend
}
```

**Die Tabelle ist ein festes Array** `[Option<Prozess>; 8]` in einem
`spin::Mutex` — **kein `Vec`, keine `BTreeMap`**. Grund: Der Timer-Interrupt
liest sie, und im Interrupt-Kontext darf nicht allokiert werden
(Deadlock-Regel 2). Slot 0 ist per Konstruktion der Kernel-Prozess.

**Zustände.** `Lauffaehig`/`Beendet` sind offensichtlich. `Wartend` ist bewusst
schon jetzt echt und nicht nur Attrappe: `SYS_SCHLAFEN(ms)` setzt den Prozess
auf `Wartend` mit Weck-Zeitpunkt, und der Timer weckt ihn. Das ist die Naht,
an der später blockierende Syscalls (VFS, Sockets) hängen werden.

### Lock-Disziplin (die neue Gefahrenstelle)
- **Aus Kernel-Kontext** wird die Tabelle immer mit `without_interrupts`
  gesperrt (Projektmuster). Damit kann der Timer während der Sperre gar nicht
  feuern.
- **Im Timer-Interrupt** wird `try_lock` benutzt: Schlägt es fehl, findet in
  diesem Tick eben kein Wechsel statt (der nächste Tick kommt in 4 ms). Ein
  Interrupt-Handler wartet **nie** auf einen Lock.
- **Beendete Prozesse werden NICHT im Interrupt abgeräumt.** Ihr `Drop` gibt
  Frames frei (`memory`-Locks) und Heap-Speicher — beides im Interrupt
  verboten. Der Timer *markiert* nur; ein Kernel-Task („Prozess-Aufräumer")
  räumt auf.

---

## 4. Round-Robin, Zeitscheibe, Leerlauf (Aufgabe 3)

- **Zeitscheibe:** 5 PIT-Ticks. Bei 250 Hz (4 ms/Tick) sind das **20 ms** —
  fein genug, dass eine 33-FPS-Oberfläche nicht ruckelt, grob genug, dass der
  Wechsel-Aufwand (ein CR3-Wechsel leert den TLB!) nicht ins Gewicht fällt.
- **Auswahl:** zyklisch ab `aktuell + 1` den nächsten lauffähigen Slot. Damit
  ist Fairness strukturell garantiert (jeder kommt vor dem Zweiten dran).
- **Die Entscheidung ist eine REINE FUNKTION** ohne Globals:

```rust
fn wechsel_entscheiden(
    zustaende: &[Option<Zustand>],   // Momentaufnahme der Tabelle
    aktuell: usize,
    scheibe_abgelaufen: bool,
    freiwillig: bool,
) -> Option<usize>                   // None = bleiben
```

  Regeln, in dieser Reihenfolge:
  1. Ist der **aktuelle** Prozess nicht mehr lauffähig (beendet, wartend, weg),
     MUSS gewechselt werden — auch mitten in der Zeitscheibe.
  2. Sonst nur bei **abgelaufener Zeitscheibe** oder **freiwilliger Abgabe**.
  3. Ist der Nächste der Aktuelle selbst → kein Wechsel (Scheibe wird neu
     gefüllt). Das ist der Normalfall im Alltag: nur PID 0 ist lauffähig.

- **Freiwillige Abgabe:** `SYS_YIELD` (Nummer 4). Weil auch der Kernel-Prozess
  ein Prozess ist, benutzt **der Executor sie selbst**: Findet
  `sleep_if_idle()` keine Arbeit, aber es gibt lauffähige User-Prozesse, gibt
  er die Scheibe sofort ab, statt bis zum nächsten Tick zu `hlt`-en. Sonst
  `hlt` wie bisher.
- **Leerlauf:** siehe §1 (2) — PID 0 *ist* der Idle-Prozess.

---

## 5. Der Rückweg: Präemption trifft Ring 3 (Dauerregel II bleibt gültig)

Ein Page Fault oder #GP aus einem *eingeplanten* Prozess darf nicht mehr per
setjmp/longjmp in den Kernel zurückspringen (dieser Weg gehört dem alten
Einzelschuss-Pfad, siehe §6). Stattdessen:

1. `user_recovery()` erkennt „Trap aus Ring 3 **und** aktueller Prozess ist ein
   User-Prozess".
2. Der Prozess wird auf `Beendet` gesetzt, und der **Interrupt-Rahmen wird auf
   einen Ring-0-Stub umgebogen**: `RIP = prozess_sterben`, `CS/SS = Kernel`,
   `RSP = Kernel-Stack-Spitze des sterbenden Prozesses`, `RFLAGS` ohne IF.
3. Das `iretq` des Handlers landet damit im Kernel, auf dem Stack des
   Sterbenden. Der Stub holt sich den nächsten lauffähigen Prozess und springt
   über `schalte_auf_rahmen` dorthin. Der sterbende Kernel-Stack wird danach
   nie wieder gebraucht.

Der Kernel läuft weiter, der Prozess ist weg — Dauerregel II, jetzt
prozess-weise statt global.

---

## 6. Verhältnis zum bestehenden `ring3.rs` (Einzelschuss-Pfad)

`ring3test` fährt Ring-3-Code **synchron** im Kontext des Kernel-Prozesses
(setjmp/longjmp, siehe Kopfkommentar in `src/ring3.rs`). Dieser Pfad bleibt —
er ist der didaktisch klarste Ring-3-Beweis und deckt die copy-in/out-Angriffe
ab. Er ist aber mit einem Kontext-Wechsel **unvereinbar**: Der Kernel-Prozess
führt dort selbst Ring-3-Code mit fremdem CR3 aus; würde der Timer dazwischen
wechseln, käme PID 0 mit Kernel-CR3 mitten im User-Code zurück.

Deshalb: **`nach_ring3()` sperrt die Planung** (`scheduler::sperre_erhoehen()`
/ `sperre_senken()`, ein Zähler; `> 0` heisst „Timer plant nicht um"). Das ist
die einzige Stelle mit einer solchen Sperre, und sie ist genau deshalb nötig,
weil dieser Pfad die Regel aus Invariante 1 bewusst bricht.

---

## 7. Was bewiesen wird (Tests)

**Reine Funktionen (Unit-Tests, keine Hardware):**
- `wechsel_entscheiden`: Beendete/wartende/leere Slots werden übersprungen;
  ohne abgelaufene Scheibe kein Wechsel; ein nicht mehr lauffähiger Aktueller
  erzwingt einen Wechsel; **Fairness über N Runden** (bei k lauffähigen
  Prozessen bekommt über k·N Runden jeder genau N Scheiben).
- `scheibe_tick`: Ablauf genau beim 5. Tick, Selbst-Nachfüllen.
- `weck_faellig`: Schläfer-Deadline.

**Kontext-Sicherung gegen synthetische Register-Sätze (echt, in QEMU):**
- `TrapFrame`-**Layout** per `offset_of!` gegen die Push-Reihenfolge des
  Assemblers festgenagelt (`size == 160`) — das ist die Bug-Klasse, die sonst
  als Register-Korruption erscheint.
- Ein Assembler-Stub lädt **alle** General-Register mit Magic-Werten, löst
  `int 0x80` aus (Syscall `SYS_KONTEXT_TEST`, der den eingehenden Rahmen
  wegkopiert) und schreibt danach seine Register in einen zweiten Block.
  Geprüft wird: (a) der **gesicherte** Rahmen enthält genau die Magic-Werte
  (Save-Pfad), (b) die Register **nach** der Rückkehr sind unverändert
  (Restore-Pfad).

**Der Präemptions-Beweis (Integrationstest, Aufgabe 4):**
Zwei Prozesse mit handgeschriebenem Maschinencode, die in einer Endlosschleife
zählen und per `debug_print` ausgeben — **ohne jede freiwillige Abgabe** (kein
`yield`, kein `exit`, keine Blockade; zwischen zwei Ausgaben liegt eine lange
Rechenschleife). Geprüft wird maschinell:
- beide Prozesse haben ausgegeben, und die Ausgabe-**Spur wechselt** mehrfach
  zwischen ihnen (mindestens ein Wechsel in jede Richtung),
- jeder Prozess wurde **nachweislich aus Ring 3 verdrängt**
  (`praemptionen > 0`),
- **`abgaben == 0`** für beide — sie haben nie freiwillig abgegeben.

Zusammen ist das der Beweis: Wenn keiner abgibt und beide vorankommen, kann die
CPU nur *weggenommen* worden sein.

---

## 8. Bewusst NICHT dabei (und was der nächste Hebel wäre)

- **Prioritäten / faire Gewichtung.** Reines Round-Robin. Wenn der Desktop
  unter rechnenden Prozessen zu zäh wird, ist der einfachste ehrliche Hebel:
  PID 0 eine doppelte Scheibe geben (eine Zahl, keine neue Struktur).
- **Blockierende Syscalls.** `Wartend` und `SYS_SCHLAFEN` existieren, aber
  `sys_read`/`sys_recv` sind noch nicht da. **Wichtige Randbedingung dafür:**
  Ein Syscall darf nur Locks anfassen, die im Kernel **ausschliesslich mit
  ausgeschalteten Interrupts** gehalten werden (KONSOLE, FRAMEBUFFER, MANAGER,
  SERIAL, alle Blatt-Locks erfüllen das). `fs::mit_fs` erfüllt es **nicht** —
  ein VFS-Syscall braucht deshalb erst das Warte-Modell („Prozess wird
  `Wartend`, ein Kernel-Task erledigt die Arbeit"), nicht einfach einen
  `mit_fs`-Aufruf im Syscall. Das ist Teil 4.
- **SMP / APIC-Timer.** Unverändert vertagt (Bestandsaufnahme (a)).
- **PCID.** Jeder Wechsel leert den TLB. Bei 20-ms-Scheiben irrelevant.
- **Kernel-Stack-Wiederverwendung.** `memory::allocate_pages` zählt virtuelle
  Adressen nur vorwärts; ein abgeräumter Prozess gibt seine physischen Frames
  zurück, aber nicht seinen virtuellen Bereich. Bei 8 Prozessen à 20 KiB in
  einem 512-GiB-Slot ist das kein Problem, aber es ist ein Leck und steht hier.
- **Präemption im Kernel-Prozess auf Task-Ebene.** Ein Kernel-Task, der nie
  `await`-t, blockiert weiterhin die anderen Kernel-Tasks (nur nicht mehr die
  User-Prozesse). Das ist unverändert kooperativ — und bewusst so.
