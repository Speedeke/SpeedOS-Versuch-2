# unsafe-Audit Serie 7 (Zufall, User-Heap, TLS)

Fortsetzung von `docs/unsafe-audit-serie6.md`. Dort steht die riskante
Fläche der Prozess-Schicht (copy_in/out, `iretq`, Page Tables); hier die
**drei Flächen, die Serie 7 hinzugefügt hat**:

* der **Entropie-Pool**, der aus dem **Interrupt-Kontext** gefüttert wird,
* der **User-Heap-Syscall**, der einem Ring-3-Prozess Speicher gibt,
* der **TLS-Stapel** in Ring 3 (rustls + unsere Naht).

---

## 0. Die Zahlen

| Bereich | `unsafe`-Blöcke | `unsafe fn` |
|---|---|---|
| `src/zufall.rs` (Zufall, inkl. IRQ-Pfad) | **3** | 0 |
| `src/scheduler.rs::heap_erweitern` (der Syscall) | **0** | 0 |
| `userland/src/heap.rs` (der Allocator in Ring 3) | 3 | 0 |
| `userland/src/tls.rs` (die TLS-Naht) | **0** | 0 |
| `userland/src/netz.rs` (die Abrufschicht) | **0** | 0 |
| `userland/src/pem.rs` (der Zertifikats-Parser) | **0** | 0 |
| `speedhttp/` (der HTTP-Parser) | **0** | 0 |

Die vier Zeilen mit fetter Null sind die eigentliche Aussage dieses
Dokuments. Sie waren nicht selbstverständlich — sie sind das Ergebnis von
Entscheidungen, die weiter unten begründet werden.

---

## 1. Der Entropie-Pool: der IRQ-Pfad ist unsafe-FREI

Das ist die heikelste Stelle der ganzen Serie, denn `zufall::einspeisen`
läuft **im Interrupt-Handler** — bei jedem Tastendruck, jeder Mausbewegung,
jedem Netzpaket, jedem Timer-Tick. Was dort passiert, passiert mitten in
beliebigem anderen Code.

```rust
pub fn einspeisen(quelle: Quelle) {
    let jetzt = crate::zeit::tsc_roh();
    einspeisen_wert(quelle, jetzt);
}
```

Und `einspeisen_wert` besteht aus genau vier Sorten Operation:

```rust
let vorher    = LETZTER_TSC[index].swap(wert, Ordering::Relaxed);
let proben    = PROBEN[index].fetch_add(1, Ordering::Relaxed);
let platz     = POOL_INDEX.fetch_add(1, Ordering::Relaxed) % POOL_WORTE;
POOL[platz].fetch_xor(beitrag, Ordering::Relaxed);
```

**Kein Lock, keine Allokation, kein `unsafe` — nur Atomics.** Das ist
RNG-Dauerregel VI, und sie ist hier nicht bloß eingehalten, sondern
*strukturell erzwungen*: Es gibt in diesem Pfad nichts, an dem man sich
verletzen könnte.

Warum das wichtiger ist als es klingt: Ein Lock im IRQ-Handler wäre ein
Deadlock, sobald der unterbrochene Code denselben Lock hält. Eine Allokation
wäre dasselbe eine Ebene tiefer. Und `unsafe` in einem Pfad, der jederzeit
zuschlagen kann, wäre extrem schwer zu prüfen — man müsste die Invariante
gegen *jeden* möglichen unterbrochenen Zustand argumentieren.

### Die drei Blöcke, die es doch gibt

Alle drei liegen **außerhalb** des IRQ-Pfads.

**(1) `loeschen` — das Ausradieren verbrauchter Schlüssel**

```rust
unsafe { core::ptr::write_volatile(byte, 0) };
```

*Invariante:* `byte` ist eine `&mut u8` aus `iter_mut()` — gültig,
ausgerichtet, exklusiv. `write_volatile` darauf ist bedingungslos erlaubt.

*Warum überhaupt `volatile`:* Ein gewöhnliches `*byte = 0` darf der
Optimierer wegwerfen, weil danach niemand liest. Genau das würde die
Key-Erasure-Zusage (Vorwärts-Sicherheit) zu einer Behauptung machen: Der
verbrauchte ChaCha20-Schlüssel bliebe im Speicher stehen. `volatile` ist
hier also kein Aberglaube, sondern das, was die Zusage trägt.

**(2)/(3) `_rdseed64_step` / `_rdrand64_step`**

```rust
let ok = unsafe { core::arch::x86_64::_rdseed64_step(&mut wert) };
```

*Invariante:* Beide Intrinsics setzen voraus, dass die CPU den Befehl kennt.
Das wird **vor** dem Aufruf per CPUID geprüft (`RDSEED`-Bit bzw.
`RDRAND`-Bit); ohne das Bit wird der Zweig nie betreten. Der Rückgabewert
(Carry-Flag) wird ausgewertet — ein „nicht bereit" gilt als Fehlschlag und
liefert keine Bits.

*Und die eigentliche Absicherung ist gar keine Speicher-Frage:*
RNG-Dauerregel II rechnet diesen Quellen höchstens die **halbe** Schwelle an
(128 von 256 Bit) und mischt sie **per XOR** ein. Das bekannte AMD-Erratum
(nach S3 dauerhaft `0xFFFF_FFFF` **mit** gesetztem Carry, also als „gültig"
gemeldet) kann deshalb nichts verschlechtern — eine defekte Quelle, die per
XOR beiträgt, macht den Pool nicht schwächer.

**`tsc_roh()`** (in `src/zeit.rs`) hat einen weiteren Block:
```rust
unsafe { core::arch::x86_64::_rdtsc() }
```
*Invariante:* RDTSC liest ein CPU-Register, berührt keinen Speicher und ist
auf jeder x86-64-CPU vorhanden. Der Block ist so harmlos, wie ein
`unsafe`-Block sein kann — er steht nur da, weil das Intrinsic so deklariert
ist.

---

## 2. Der User-Heap-Syscall: 0 unsafe, und das ist kein Zufall

`SYS_SPEICHER` (14) gibt einem Ring-3-Prozess Speicher. Man würde hier
Zeiger-Arithmetik und `map_to`-Aufrufe erwarten. Tatsächlich enthält
`scheduler::heap_erweitern` **keinen einzigen `unsafe`-Block** — die
gefährliche Arbeit steckt vollständig in `adressraum::bereich_mappen_mit_rechten`,
das in Serie 6 auditiert wurde.

Was diese Funktion selbst tut, ist **Arithmetik und Politik**, und beides ist
prüfbar ohne `unsafe`:

```rust
let bytes = bytes.checked_add(4095).ok_or(Fehler::ZuGross)? & !0xfff;
if bytes == 0 { return Err(Fehler::UngueltigesArgument); }
let neu = alt.checked_add(bytes).ok_or(Fehler::ZuGross)?;
if neu > crate::prozess::HEAP_MAX_BYTES { return Err(Fehler::KeinPlatz); }
```

Die vier Zeilen sind die ganze Absicherung, und jede fängt einen echten
Angriff:

| Zeile | fängt ab |
|---|---|
| `checked_add(4095)` beim Aufrunden | `u64::MAX` → Überlauf auf eine winzige Zahl |
| `bytes == 0` | eine Anforderung, die nichts anfordert |
| `checked_add(bytes)` | Überlauf beim Aufsummieren über viele Aufrufe |
| `> HEAP_MAX_BYTES` | der Prozess frisst die Lücke zum Stack auf |

**Nachgemessen** (`angreifer 8`): `u64::MAX`, `u64::MAX - 4095`, `1<<60` und
`1<<40` werden abgelehnt; danach gibt der Kernel in Ein-MiB-Schritten
**genau 12 MiB** heraus und sagt dann Nein — exakt die dokumentierte
Obergrenze.

Zwei Eigenschaften, die dabei mitlaufen und leicht zu übersehen wären:

* **Die Seiten sind NX.** `Rechte::SCHREIBEN` ohne `AUSFUEHREN` — ein
  ausführbarer Heap wäre die Einladung, W^X aus Serie 6 wieder aufzuheben.
* **Die Frames sind genullt.** `map_benutzer` nullt jeden Frame, sonst
  leckte der Inhalt des Vorbesitzers nach Ring 3.

Und die Lock-Disziplin: `TABELLE.try_lock()` statt `lock()`. Im Syscall sind
Interrupts aus; auf einen Lock zu *warten* wäre ein Hänger (docs/syscalls.md
§8). Kein Lock frei → `Fehler::Belegt`, und der Aufrufer versucht es erneut.

---

## 3. Der TLS-Stapel: 0 unsafe in unserem Code

`userland/src/tls.rs`, `userland/src/netz.rs` und `userland/src/pem.rs`
enthalten zusammen **null** `unsafe`-Blöcke — und `speedhttp/` ebenfalls.

Das ist die Konsequenz aus zwei Entscheidungen:

1. **TLS lebt in Ring 3** (docs/tls-entscheidung.md). Ein Fehler in 30 000
   Zeilen fremdem Krypto-Code trifft einen Prozess, nicht den Kernel. Der
   Kernel bekam dafür genau *einen* neuen Syscall (`zufall`) und einen
   Speicher-Syscall — beide oben auditiert.
2. **Die Parser arbeiten auf Slices.** `pem.rs` und `speedhttp` bekommen
   `&[u8]` und geben `&[u8]` zurück; es gibt keinen rohen Zeiger, keine
   Länge, die man falsch rechnen könnte. Der DER-Läufer prüft jede Länge mit
   `checked_add` gegen `daten.len()` und gibt bei jeder Unstimmigkeit auf.

**Nachgemessen** (`angreifer 9`): BEGIN ohne END, Base64-Müll, 4000 Blöcke
gegen ein Limit von 512, ein Block größer als der Arbeitspuffer, sieben
DER-Muster von „leer" bis „Länge 4 GiB", und verschachtelte Marken — kein
Absturz, keine Schleife, alles in Millisekunden.

### Was `userland/src/heap.rs` doch braucht

Drei Blöcke, alle im `GlobalAlloc`:

```rust
unsafe { heap.init(basis as *mut u8, bytes) };   // der erste Bereich
unsafe { heap.extend(bytes) };                   // jeder weitere
unsafe { self.innen.lock().deallocate(nn, layout) };
```

*Invariante `init`:* `basis` kommt frisch von `SYS_SPEICHER` — der Kernel hat
genau diesen Bereich soeben für **uns** gemappt, er ist beschreibbar,
genullt, und gehört niemandem sonst.

*Invariante `extend`:* `SYS_SPEICHER` mappt **immer** lückenlos hinter dem
bisherigen Heap-Ende (`HEAP_START + heap_bytes`). Genau das verlangt
`extend` — und genau deshalb reicht *ein* `linked_list_allocator`. Wäre das
`brk`-Modell nicht garantiert, wäre dieser Block falsch.

*Invariante `deallocate`:* Zeiger und Layout stammen laut `GlobalAlloc`-Vertrag
aus einem früheren `alloc` desselben Allocators.

**Der Fehler, der hier schon einmal steckte** (Serie 7, Teil 3) war keine
Speicher-Verletzung, sondern ein Deadlock: Der `MutexGuard` aus einer
`if let`-**Bedingung** lebt bis zum Ende des Blocks, ein zweites `lock()`
darin dreht sich für immer. Er steht als Warnung im Code — `unsafe` ist nicht
die einzige Art, sich zu verletzen.

---

## 4. Was NICHT geprüft ist — und warum das hier steht

* **Der Code von `rustls`, `rustls-webpki` und `rustls-rustcrypto` ist nicht
  auditiert.** Er ist fremd, er ist groß, und der Anbieter ist **0.0.2-alpha**.
  Was wir tun können, haben wir getan: Er läuft unprivilegiert, in einem
  eigenen Adressraum, mit NX-Heap und einer harten Speichergrenze. Was wir
  *nicht* können, ist ihn für korrekt erklären.
* **Seitenkanäle** sind kein Thema dieses Audits. Der ChaCha20-DRBG ist
  datenunabhängig (keine schlüsselabhängigen Zweige oder Tabellenzugriffe,
  siehe RNG-Regel „Eigenbau-Krypto-Grenze"); für den fremden Code gilt das
  ungeprüft.
* Die bekannte P1-Tabellen-Unschärfe aus Serie 6 (`allocate_pages` vergibt
  virtuellen Raum monoton) besteht unverändert. Sie ist im Speicher-Pass
  über 50 HTTPS-Zyklen ausgerechnet statt weggelassen — gemessen wurden
  **0 verlorene Frames** bei einer erlaubten Schranke von 34.

---

## 5. Fazit

Die drei neuen Flächen von Serie 7 haben zusammen **6 `unsafe`-Blöcke**
(3 im Zufall, 3 im User-Allocator) und **0 `unsafe fn`**. Die beiden
Stellen, bei denen man am ehesten mit gefährlichem Code gerechnet hätte —
der Interrupt-Pfad des Entropie-Pools und der Speicher-Syscall — kommen
**ganz ohne** aus.

Das ist kein Glück. Es folgt daraus, dass die riskanten Operationen an
Stellen liegen, die schon auditiert waren (`adressraum`), und dass die neuen
Schichten auf Slices und Atomics arbeiten statt auf Zeigern.
