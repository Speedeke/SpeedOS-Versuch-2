# unsafe-Audit: die Prozess-Schicht (Serie 6)

Stand: Juli 2026, Serie-6-Abschluss.

Serie 6 hat die **riskanteste `unsafe`-Fläche des ganzen Projekts**
hinzugefügt. Die Netz-Schicht aus Serie 5 war harmlos (`src/netz/` enthält
**0 `unsafe`** — reine Byte-Logik); hier dagegen wird der CPU-Zustand von
Hand umgeschaltet, es werden Page Tables geschrieben, und an genau zwei
Stellen folgt Kernel-Code einem Zeiger, den sich ein unprivilegiertes
Programm ausgedacht hat.

Dieses Dokument geht jede dieser Stellen durch und beantwortet für jede die
einzige Frage, die zählt: **Welche Invariante macht sie sicher — und wer
stellt sie her?**

---

## 0. Die Zahlen

| Datei | `unsafe`-Vorkommen | `unsafe fn` | Charakter |
|---|---:|---:|---|
| `src/adressraum.rs` | 28 | 0 | Page Tables, Physik-Mapping |
| `src/ring3.rs` | 14 | 0 | **copy-in/out**, `iretq`, setjmp |
| `src/prozess.rs` | 8 | 0 | Trap-Rahmen schreiben, Frames |
| `src/scheduler.rs` | 4 | 0 | Trap-Rahmen lesen, `int 0x80` |
| `src/syscall/mod.rs` | 2 | 0 | Trap-Rahmen als `&mut` |
| `src/syscall/datei.rs` | 2 | 0 | Struktur → Bytes (ABI) |
| `src/elf.rs` | **0** | 0 | — |
| `src/pipe.rs` | **0** | 0 | — |
| `src/syscall/{prozess,handle}.rs` | **0** | 0 | — |

**Bemerkenswert:** Die beiden Module, die AM MEISTEN mit fremden Daten
arbeiten — der ELF-Parser und die Pipes — kommen **ohne ein einziges
`unsafe`** aus.

* `elf.rs` liest jede Zahl über drei winzige, grenzgeprüfte Helfer
  (`u16_bei`/`u32_bei`/`u64_bei`) statt per `transmute` auf den Puffer zu
  zeigen. Das ist langsamer und dafür kann eine abgeschnittene Datei nichts
  kaputtmachen — der Test `test_abgeschnitten_an_jeder_stelle` wirft ihm
  jede Länge von 0 bis vollständig entgegen.
* `pipe.rs` benutzt den `Ringpuffer` aus Serie 5, der selbst sicher ist.

Das ist kein Zufall, sondern die Arbeitsteilung: `unsafe` konzentriert sich
dort, wo es unvermeidlich ist (CPU-Zustand, Page Tables), und wird von den
Stellen ferngehalten, die fremde Daten verarbeiten.

---

## 1. Die kritischste Stelle: `copy_in` / `copy_out`

`src/ring3.rs`. **Hier und nur hier folgt Kernel-Code einem Zeiger, den ein
unprivilegiertes Programm gewählt hat.** Ein Fehler an dieser Stelle ist eine
vollständige Kernel-Übernahme.

```rust
// copy_in, nach bestandener Prüfung:
unsafe {
    core::ptr::copy_nonoverlapping(user_ptr as *const u8, ziel.as_mut_ptr(), laenge);
}
```

### Welche Invariante macht das sicher?

Die Kopie steht **hinter** `user_bereich_pruefen(ptr, laenge, schreiben)`, und
diese Funktion stellt vor der Rückkehr mit `Ok` **alle vier** Bedingungen
her, die `copy_nonoverlapping` braucht:

| Anforderung von `copy_nonoverlapping` | Wer stellt sie her |
|---|---|
| Quelle ist für `laenge` Bytes lesbar | Stufe (b): **jede** berührte Seite ist in den Tabellen aus **CR3** als `PRESENT` nachgeschlagen |
| Ziel ist für `laenge` Bytes beschreibbar | Der Zielpuffer ist ein frisch alloziertes `Vec<u8>` **des Kernels** — nicht der User-Puffer |
| Bereiche überlappen nicht | Kernel-Heap und User-Slot 1 sind **disjunkte P4-Slots**; sie können sich nicht überschneiden |
| Adressen sind ausgerichtet | `u8` hat Ausrichtung 1 — entfällt |

Die drei Prüfstufen im Einzelnen, und warum jede unverzichtbar ist:

**(a) Bereich, rein arithmetisch.** `[ptr, ptr+len)` muss vollständig in
`USER_START..USER_ENDE` liegen, gerechnet mit `checked_add`. Diese Stufe
erledigt Kernel-Adressen, Nullzeiger und die obere Hälfte, **ohne eine
einzige Page Table anzufassen**. Der `checked_add` ist nicht Kosmetik: Ohne
ihn wäre `ptr = u64::MAX-4, len = 16` scheinbar gültig — das Ende „käme
hinten wieder heraus" und umschlösse dabei den gesamten Kernel.
*Angreifer-Angriff 4 fährt genau das.*

**(b) Mapping, Seite für Seite.** Jede berührte Seite muss im Adressraum des
**Aufrufers** gemappt und `USER_ACCESSIBLE` sein. Nachgeschlagen wird in den
Tabellen aus **CR3** — und darin liegt die eigentliche Eleganz: Beim Syscall
steht dort per Konstruktion der Adressraum des aufrufenden Prozesses. Es gibt
gar keine Möglichkeit, versehentlich gegen die Tabellen eines anderen
Prozesses zu prüfen. Eine Seite, die nur in einem **fremden** Adressraum
existiert, ist hier schlicht nicht gemappt.
Geprüft wird **jede** Seite, nicht nur die erste: Der klassische Angriff ist
ein Zeiger auf das letzte Byte einer gültigen Seite mit einer Länge, die
dahinter weiterreicht.

**(c) Schreibrecht, nur bei copy-out.** Sonst würde der Kernel in Ring 0
fröhlich in eine Seite schreiben, die der Prozess selbst nur lesen darf — die
Schreibsperre wäre wertlos.

### Und es wird wirklich KOPIERT

Der Kernel arbeitet nie auf User-Speicher weiter. Täte er es, könnte der
Prozess ihm die Daten zwischen Prüfung und Benutzung unter den Händen
ändern. Deshalb prüft `pfad_lesen` auch erst die **Kopie** auf gültiges
UTF-8 — der Kernel baut nie einen `&str` auf fremdem Speicher.

### TOCTOU — ehrlich benannt

Zwischen Prüfung und Kopie könnte theoretisch jemand die Seite aushängen. Bei
uns kann das nicht passieren: Ein Syscall läuft mit **ausgeschalteten
Interrupts**, es findet also kein Prozess-Wechsel statt, und SpeedOS ist
einkernig. **Die Invariante lautet: zwischen `user_bereich_pruefen` und der
Kopie darf kein `warte_fenster()` liegen.** Alle heutigen Aufrufer erfüllen
das (die Wartefenster liegen ausschliesslich in den Warteschleifen von
`verbinde`/`mit_vfs`, wo kein geprüfter Zeiger offen ist). Mit SMP oder
einem Aushängen aus einem anderen Kern müsste hier ein Lock auf den
Adressraum dazukommen.

### Geprüft durch

`src/ring3.rs::tests` (7 Angriffsvarianten), `tests/syscalls.rs` (jeder
Syscall mit bösartigen Zeigern aus Ring 3) und `userland/angreifer` Angriffe
1, 4, 5 — letzterer probiert Kernel-Heap, obere Hälfte, Nullzeiger,
Überlauf-Zeiger und 6 absurde Längen durch, **auch als copy-OUT-Ziel** (der
gefährlichste Fall: der Kernel würde sich selbst überschreiben lassen).

---

## 2. Der Übergang nach Ring 3: `iretq`

`src/ring3.rs`, `iretq_nach_ring3`.

```asm
push rcx        ; SS
push rsi        ; RSP
push 0x202      ; RFLAGS (IF gesetzt)
push rdx        ; CS
push rdi        ; RIP
iretq
```

**Invariante:** Die fünf gepushten Werte müssen einen *konsistenten*
Ring-3-Zustand beschreiben. Hergestellt durch:

* `cs`/`ss` kommen aus `gdt::user_code_selektor()`/`user_data_selektor()` —
  feste Konstanten aus unserer eigenen GDT mit RPL 3. Sie sind nicht
  beeinflussbar.
* `rip`/`rsp` sind vom Kernel selbst berechnete Adressen im User-Slot des
  gerade aktivierten Adressraums.
* `RFLAGS = 0x202` setzt IF und das reservierte Bit 1. **IF muss gesetzt
  sein** — sonst könnte der Timer den Prozess nie verdrängen, und ein
  einziger Ring-3-Prozess würde die Maschine anhalten. (`test_start_rahmen_ist_ring3_rahmen`
  prüft genau dieses Bit.)

Warum `iretq` und nicht `sysretq`: `iretq` lädt CS:RIP, RFLAGS und SS:RSP aus
einem Stack-Rahmen — wir bauen den Rahmen, den ein Trap aus Ring 3
hinterlassen *hätte*. `sysretq` bräuchte MSR-Einrichtung und eine bestimmte
Segment-Anordnung. Für die Sicherheit ist beides gleichwertig.

---

## 3. Der Kontext-Wechsel: Trap-Rahmen als `&mut`

`src/syscall/mod.rs` und `src/scheduler.rs`:

```rust
let f = unsafe { &mut *rahmen };            // syscall_dispatch
let war_ring3 = unsafe { (*rahmen).aus_ring3() };  // timer_dispatch
```

**Invariante:** `rahmen` zeigt auf den Register-Block, den der
Assembler-Einstieg *unmittelbar zuvor* auf den Kernel-Stack **des gerade
laufenden Prozesses** gepusht hat. Hergestellt durch:

* Der Zeiger kommt **ausschliesslich** aus dem Assembler (`mov rdi, rsp`
  direkt nach den 15 `push`), nie aus einer Berechnung oder von aussen.
* Das Speicher-Layout ist vertraglich festgelegt und **getestet**:
  `prozess::tests::test_trapframe_layout` nagelt alle 20 Feld-Offsets und
  `size_of == 160` fest. Stimmte die Reihenfolge nicht, würde der Wechsel
  Register vertauschen — ein Fehlerbild, das von aussen praktisch nicht
  diagnostizierbar ist.
* Der Kernel-Stack gehört exklusiv diesem Prozess (eigene Allokation mit
  Guard-Page), und solange der Handler läuft, kann ihn niemand verdrängen
  (Interrupts aus).

Die `&mut`-Referenz lebt nur innerhalb des Dispatchers; danach wird der
Rahmen wieder ausschliesslich vom Assembler benutzt. Es gibt zu keinem
Zeitpunkt eine zweite Referenz darauf.

### Der von Hand geschriebene Start-Rahmen

`prozess.rs`:
```rust
unsafe { core::ptr::write(rahmen_adresse as *mut TrapFrame, rahmen); }
```
**Invariante:** `rahmen_adresse` liegt im gerade allozierten, exklusiv
besessenen Kernel-Stack, ist gemappt, beschreibbar und 16-ausgerichtet
(`kern_stack.oben() - size_of::<TrapFrame>()`, und 160 ist durch 16 teilbar).
Der Stack existiert erst seit zwei Zeilen; niemand sonst kennt ihn.

---

## 4. Page Tables: `src/adressraum.rs`

28 Vorkommen, alle nach einem von drei Mustern.

**(a) Lesender Zugriff über das Physik-Komplettmapping** (`flags_in`,
`uebersetzen_in`, `user_slot_frei_im_kernel`):
```rust
let tabelle: &PageTable = unsafe { &*((offset + frame.start_address()).as_ptr()) };
```
*Invariante:* Der Bootloader hat den **kompletten** physischen Speicher ab
`offset` gemappt (`Mapping::Dynamic`, in `memory::init` festgehalten), und
`frame` stammt aus einem gültigen, `PRESENT`-markierten Tabelleneintrag. Der
Zeiger ist damit gültig und ausgerichtet (Page-Tables sind 4-KiB-ausgerichtet).
Nur Lesezugriff.

**(b) Exklusiver Zugriff auf die EIGENE P4** (`mit_mapper`,
`kernel_spiegeln`):
```rust
let tabelle: &mut PageTable = unsafe { &mut *(...as_mut_ptr()) };
```
*Invariante:* `self.p4` ist ein Frame, den **dieser** `AdressRaum` selbst
alloziert hat und exklusiv besitzt. Der globale `MAPPER` hält die
**Kernel-P4** — ein anderer Frame; es entsteht kein Aliasing. Gemappt wird
ausschliesslich in P4-Slot 1, dessen Unterbau uns allein gehört; die
gespiegelten Kernel-Slots werden nur **gelesen** und als 8-Byte-Einträge
kopiert.

**(c) CR3 schreiben** (`aktivieren`, `kernel_aktivieren`):
```rust
unsafe { Cr3::write(self.p4, flags) };
```
*Invariante:* Die geladene P4 enthält den **vollständig gespiegelten
Kernel** — Code, Stack, Heap und Physik-Mapping bleiben nach dem Wechsel an
derselben virtuellen Adresse erreichbar. Hergestellt von `kernel_spiegeln`,
das unmittelbar davor läuft (jedes `aktivieren` frischt den Spiegel auf).
Fehlte auch nur ein Slot, wäre der nächste Befehl ein Triple Fault.

**Warum das robust ist:** Gespiegelt wird **jeder** belegte Kernel-Slot, nicht
„die obere Hälfte" — bei uns liegt der Kernel in der unteren (ehrlich
dokumentiert in `adressraum.rs`). Und weil P4-**Einträge** kopiert werden
(Zeiger auf geteilte P3-Tabellen), sind spätere Kernel-Mappings innerhalb
schon gespiegelter Slots automatisch überall sichtbar.

**Frame nullen vor der Weitergabe** (`frame_nullen`): Sicherheit, nicht
Kosmetik. Ein Frame aus dem Allocator trägt die Bytes seines Vorbesitzers —
womöglich Kernel-Daten oder das Passwort eines anderen Prozesses. Er wird
gleich für Ring 3 lesbar gemappt, **muss** also vorher gelöscht werden. Als
Nebenwirkung ist damit auch die `.bss`-Garantie des ELF-Loaders erfüllt,
ohne dass dieser etwas tun müsste.

---

## 5. Frames freigeben

`prozess.rs` (Guard-Page, `KernStack::drop`), `adressraum.rs` (`Drop`):
```rust
unsafe { memory::frame_freigeben(frame) };
```
*Invariante:* Der Frame wurde **unmittelbar zuvor ausgehängt**
(`unmap_page`), oder der ganze Adressraum wird gerade zerstört und steht
nicht mehr in CR3 (`Drop` schaltet nötigenfalls zuerst auf den Kernel
zurück). Es existiert also keine Übersetzung mehr darauf, und niemand hält
eine Referenz.

Der Bitmap-Allocator erkennt Doppel-Freigaben per `assert` — eine zweite
Verteidigungslinie, falls diese Invariante je verletzt würde.

**Nachgewiesen** durch die byte-exakten Frame-Bilanzen: 100 Zyklen
starten/beenden (davon 33 mitten im Lauf abgeschossen), 39 Angriffe, alle
Pipe- und Prozess-Tests.

---

## 6. Die eine bekannte Unschärfe

`memory::allocate_pages` vergibt virtuellen Raum mit einem reinen
Vorwärts-Zähler; freigegebene Bereiche werden nie wiederverwendet. Die
**Frames** fliessen vollständig zurück, aber die **Page-Tables** für den
immer weiter wandernden Bereich bleiben: etwa **1 Frame je 512 vergebene
Seiten**, bei 5 Seiten pro Prozess also ~1 Frame je 100 Prozesse.

Das ist kein `unsafe`-Problem und kein Speicherfehler — es ist bewusst so
gebaut (notiert in `docs/scheduler-design.md` §8) und im Speicher-Test
ausgerechnet und benannt, statt die Bilanz aufzuweichen. Ein Freilisten-
Allocator für virtuelle Bereiche wäre die Behebung; er steht in der
Serie-7-Bestandsaufnahme.

---

## 7. Der User-Space

`userland/src/lib.rs` hat 35 `unsafe`-Vorkommen — praktisch alle sind
**derselbe** Syscall-Wrapper plus die `&str`/`&[u8]`-Zeiger, die er
weitergibt.

Das Bemerkenswerte daran: **Diese `unsafe` sind harmlos.** Ein
Ring-3-Programm kann mit einem falschen Zeiger nichts kaputtmachen — der
Kernel prüft jeden selbst (Abschnitt 1). Ein Fehler dort schadet höchstens
dem Programm, und der Kernel antwortet mit `UngueltigerZeiger`. Genau das ist
der Sinn der ganzen Übung: **Die Sicherheit hängt nicht am Wohlverhalten des
User-Codes.**

`userland/angreifer` nutzt das aus und ist voller absichtlich falscher
Zeiger. Er ist der lebende Beweis, dass diese Rechnung aufgeht.

---

## 8. Fazit

* **0 `unsafe fn`** in der gesamten Prozess-Schicht (die 5 in `memory.rs`
  stammen aus Serie 2).
* Die riskante Fläche ist **klein und konzentriert**: zwei copy-Helfer, ein
  `iretq`, ein Trap-Rahmen-Zeiger, drei Page-Table-Muster.
* Die beiden Module, die fremde Daten verarbeiten (`elf.rs`, `pipe.rs`),
  sind **vollständig sicher**.
* Jede Invariante hat einen Test, der sie angreift — nicht nur einen, der
  sie bestätigt.
* `cargo clippy --all-targets` ist warnungsfrei (Kernel **und** userland).
