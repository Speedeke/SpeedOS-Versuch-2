# Die Syscall-ABI von SpeedOS

Stand: Juli 2026, **Serie 6 abgeschlossen**. Dieses Dokument ist die
Schnittstelle zwischen Kernel und User-Space.

Die ABI umfasst **31 Syscalls** in drei Gruppen. Sie ist unter Feuer
geprüft: `tests/sicherheit.rs` lässt ein absichtlich böswilliges Programm
(`userland/angreifer`) systematisch dagegen anrennen — jeder Versuch endet
mit einem Fehlercode oder dem Tod des Angreifers, nie mit einem Schaden am
Kernel. Gemessene Kosten: **60–70 ns** je Syscall aus Ring 3
(`docs/../CHANGELOG.md`).

> **AB JETZT GILT:** Eine Änderung an den Tabellen in diesem Dokument ist eine
> bewusste **ABI-Änderung**, kein Refactoring. Nummern und Fehlercodes dürfen
> **wachsen**, aber niemals ihre Bedeutung wechseln — schon compilierte
> Programme würden sonst still etwas anderes tun. Die Nummern sind in
> `src/syscall/mod.rs::tests::test_abi_nummern_stabil` und die Struktur-Layouts
> in `src/syscall/datei.rs::tests::test_abi_strukturen_stabil` festgenagelt:
> Wer eine Zahl verschiebt, bricht einen Test.

---

## 1. Aufruf-Konvention

| | Register |
|---|---|
| Syscall-Nummer | `rax` |
| Argumente 0..3 | `rdi`, `rsi`, `rdx`, `r10` |
| **Fehlercode** | `rax` (0 = Erfolg) |
| **Ergebnis** | `rdx` |

Ausgelöst wird per **`int 0x80`** (IDT-Gate mit DPL 3).

**Register-Erhalt:** Der Trap-Einstieg sichert alle 15 General-Register und
stellt sie wieder her — **außer `rax` und `rdx`**, die als Ausgabe-Register
überschrieben werden. Ein Programm, das seine Argumente noch braucht, muss sie
also nicht retten (auch `rdi`, `rsi`, `r10` überleben) — nur `rax` und `rdx`
sind danach weg. `rflags` wird ebenfalls wiederhergestellt; SSE-/XMM-Register
fasst der Kernel nicht an (gemessen in `tests/scheduler.rs`).

```asm
    mov  rax, 3          ; SYS_SCHREIBE
    mov  edi, 1          ; Handle 1 = Ausgabe
    movabs rsi, text     ; Zeiger
    mov  edx, 13         ; Länge
    int  0x80
    ; rax = 0 -> Erfolg, rdx = geschriebene Bytes
```

**Warum zwei Rückgabe-Register** statt Linux' „negative errno in `rax`"? Ein
Ergebnis darf jeden `u64`-Wert annehmen — eine Uhrzeit, eine IP-Adresse, eine
Dateigröße. Mit getrenntem Fehlercode muss kein Bit für „ist das ein Fehler?"
reserviert werden, und es gibt keinen Wertebereich, in dem Erfolg und Fehler
nicht unterscheidbar wären.

**Warum `int 0x80` und nicht `syscall`/`sysret`?** Das IDT-Gate nutzt
Infrastruktur, die es schon gibt (ein Eintrag mit DPL 3). `syscall` bräuchte
MSR-Einrichtung (STAR/LSTAR/SFMASK), eine bestimmte GDT-Reihenfolge und eigenes
Stack-Switching. Für die ABI ist das einerlei — `syscall` wäre später eine
Beschleunigung, keine andere Schnittstelle.

### Zeiger und Längen — niemals Nullterminierung

Jeder Puffer und jeder Pfad wird als **(Zeiger, Länge)** übergeben. Der Kernel
**sucht nie ein Terminator-Byte im User-Speicher**: Das wäre ein Lesevorgang
unbekannter Länge in fremdem Speicher, also genau der unbegrenzte Zugriff, den
Dauerregel I verbietet. Ein Programm kann daher gar keinen „Pfad ohne
Nullterminierung" konstruieren — die Fehlerklasse existiert in dieser ABI nicht.

Jeder Zeiger läuft durch `ring3::copy_in` / `copy_out` und damit durch die
dreistufige Prüfung (Bereich mit Überlauf-Prüfung → jede berührte Seite im
Adressraum **des Aufrufers** gemappt und `USER_ACCESSIBLE` → bei copy-out
zusätzlich `WRITABLE`).

### Grenzen

| Grenze | Wert | Konstante |
|---|---|---|
| Pfad-Länge | 255 Byte | `MAX_PFAD` |
| Name für `aufloesen` | 255 Byte | `MAX_NAME` |
| Datenpuffer je Aufruf | 65 536 Byte | `MAX_PUFFER` |
| Datei-Offset | 1 GiB | (`datei::usize_aus`) |
| Handles je Prozess | 32 | `handle::MAX_HANDLES` |

---

## 2. Fehlercodes

Der **einzige** Fehler-Typ, den ein Prozess sieht. Kernel-interne Typen
(`FsFehler`, `IoFehler`, `SocketFehler`, `DnsFehler`, `CopyFehler`) werden
darauf abgebildet — ein Programm erfährt also nie, welches Dateisystem oder
welcher Treiber unter ihm liegt.

| Code | Name | Bedeutung |
|---:|---|---|
| 0 | `Ok` | kein Fehler |
| 1 | `UnbekannterSyscall` | diese Nummer gibt es nicht |
| 2 | `UngueltigesArgument` | Modus/Typ/Port/Länge unsinnig |
| 3 | `UngueltigerZeiger` | Zeiger gehört dem Prozess nicht |
| 4 | `ZuGross` | Länge über der Obergrenze |
| 5 | `UngueltigerHandle` | Handle unbekannt, fremd oder geschlossen |
| 6 | `KeineHandlesFrei` | Handle-Tabelle voll |
| 7 | `FalscherHandleTyp` | Datei-Operation auf Socket (oder umgekehrt) |
| 8 | `NichtGefunden` | Pfad/Name existiert nicht |
| 9 | `ExistiertBereits` | existiert schon |
| 10 | `KeinVerzeichnis` | Pfad-Bestandteil ist kein Verzeichnis |
| 11 | `KeineDatei` | ist ein Verzeichnis, Datei war gemeint |
| 12 | `NichtLeer` | Verzeichnis nicht leer |
| 13 | `UngueltigerPfad` | Syntax falsch, nicht absolut oder kein UTF-8 |
| 14 | `KeinPlatz` | Dateisystem voll, Datei zu groß, zu viele Sockets |
| 15 | `NurLesen` | Handle/Mount ohne Schreibrecht |
| 16 | `Geraetefehler` | Hardware meldet einen Fehler |
| 17 | `Zeitueberschreitung` | Frist abgelaufen |
| 18 | `NichtKonfiguriert` | kein Dateisystem / keine IP / kein DNS |
| 19 | `KeinGeraet` | keine passende Hardware |
| 20 | `NichtVerbunden` | Socket nicht verbunden |
| 21 | `BereitsVerbunden` | Socket schon verbunden |
| 22 | `Abgebrochen` | Verbindung abgebrochen/abgelehnt |
| 23 | `NichtUnterstuetzt` | Operation gibt es (noch) nicht |
| 24 | `Belegt` | Kernel-Ressource belegt, erneuter Versuch kann gelingen |
| 25 | `NichtGesaet` | Zufallsgenerator nicht ausreichend gesät (Serie 7, Teil 1) |
| 26 | `ZeitUnplausibel` | die Wanduhr ist nachweislich falsch (Serie 7, Teil 2) |

**Warum `NichtGesaet` (25) und nicht `Belegt` (24):** Der Aufrufer muss etwas
völlig anderes tun. `Belegt` heisst „gleich nochmal"; `NichtGesaet` heisst
„es gibt in diesem Zustand keinen Zufall — warte auf Entropie oder verzichte
auf Kryptographie". Es gibt dazu **keine schwachen Ersatzbytes**
(docs/zufall.md §4).

**Absichtlich GROB abgebildet:** Alle Zeiger-Prüfungsfehler außer der
Längen-Überschreitung werden zu `UngueltigerZeiger` (3). Ob eine Adresse
*gemappt* ist oder *dem Kernel gehört*, ist eine Information über den
Kernel-Zustand — ein Programm soll sie sich nicht durch Probieren
zusammensuchen können.

**`UnbekannterSyscall` statt Panik:** Jede undefinierte Nummer, auch
`u64::MAX`, liefert Code 1. Ein Programm darf beliebigen Unsinn übergeben; das
ist kein Kernel-Ereignis.

---

## 3. Standard-Handles

| Handle | Name | Bedeutung |
|---:|---|---|
| 0 | Eingabe | reserviert; Lesen liefert `NichtUnterstuetzt` (23) |
| 1 | **Ausgabe** | Bildschirm **und** seriell (Projektregel „Ausgabe läuft doppelt") |
| 2 | **Diagnose** | **nur** seriell |

Die drei gehören dem **Kernel**: `schliesse` auf 0/1/2 liefert
`UngueltigesArgument` (2). Eigene Handles beginnen bei **3**.

**Warum ein getrennter Diagnose-Kanal?** Ein Programm, das zehntausend Zeilen
protokolliert, würde über Handle 1 den Compositor überschwemmen — jede
Terminal-Zeile ist ein Fenster-Update. Handle 2 geht am Bildschirm vorbei.
Die Zähler-Demo-Prozesse benutzen deshalb 2.

Handles sind **pro Prozess**: Dieselbe Zahl bedeutet in jedem Prozess etwas
anderes, und eine Zahl, die im eigenen Prozess nicht belegt ist, ist einfach
`UngueltigerHandle`. Ein Prozess kann die Handles eines anderen nicht erraten.
Beim Prozess-Ende werden **alle** Handles automatisch geschlossen (Sockets
inklusive geordnetem TCP-Abbau).

---

## 4. Gruppe 0 — Prozess und Ausgabe

| Nr | Name | Argumente | Ergebnis (`rdx`) | Fehler |
|---:|---|---|---|---|
| 0 | `exit` | `code` | — (kehrt nie zurück) | — |
| 1 | `yield` | — | 0 | — |
| 2 | `getpid` | — | eigene PID | — |
| 3 | `schreibe` | `handle`, `ptr`, `len` | geschriebene Bytes | 5, 3, 4, 15, 23 |
| 4 | `schlafe` | `ms` | 0 | — |
| 5 | `zeit_jetzt` | — | Millisekunden seit dem Boot (monoton) | — |
| 6 | `zeit_epoche` | — | Sekunden seit 1.1.2000 (echte Uhr) | — |
| 12 | `zufall` | `ptr`, `len` | gefüllte Bytes | 3, 4, **25** |
| 13 | `zeit_geprueft` | — | UNIX-Sekunden (UTC) | **26** |
| 14 | `speicher` | `bytes` | Basis des neuen Heap-Stücks | 2, 4, 14, 23, 24 |

*(7–11 sind Ströme und Prozesse — siehe §8b.)*

- **`exit`** setzt den Prozess auf `Beendet` und schaltet auf den nächsten
  lauffähigen um. Der Aufräum-Task gibt Adressraum, Kernel-Stack und alle
  Handles zurück.
- **`yield`** und **`schlafe`** sind Kontext-Wechsel-Punkte (siehe
  `docs/scheduler-design.md`). `schlafe(0)` schläft 1 ms — 0 würde „kein
  Weckruf" bedeuten und ewig warten.
- **`schreibe`** wirkt je Handle unterschiedlich: 1/2 = Konsole bzw. seriell,
  ein **Datei**-Handle **hängt an** (es gibt keine Dateiposition, siehe §5),
  ein **Socket**-Handle sendet. Nicht-UTF-8 wird byteweise ausgegeben statt
  abgelehnt — Ausgabe ist ein Byte-Strom.
- **`zeit_jetzt` gegen `zeit_epoche`:** Das erste ist monoton (für Messungen),
  das zweite die Wanduhr (RTC-Anker + TSC). Wer Zeitspannen messen will, nimmt
  `zeit_jetzt` — die Wanduhr kann später Sprünge machen.
- **`zufall`** füllt den Puffer mit kryptographisch brauchbarem Zufall
  (ChaCha20-DRBG über einem Entropie-Pool, `docs/zufall.md`). Er ist
  **BLOCKIEREND**: Ist der Pool noch nicht gesät, wartet der Aufruf bis zu
  10 s (`ZUFALL_FRIST_MS`) und liefert dann `NichtGesaet` (25).
  **Er gibt unter keinen Umständen schwache Bytes heraus** — und im
  Fehlerfall bleibt der Puffer unverändert, damit ein Programm nicht
  versehentlich Nullen für Zufall hält. `len == 0` ist `Ok(0)`, `len` über
  `MAX_PUFFER` ist `ZuGross`.
- **`zeit_geprueft` gegen `zeit_epoche`** — der Unterschied ist die
  KONSEQUENZ, nicht die Zahl:
  * `zeit_epoche` (6) liefert **immer** etwas. Es füttert Anzeigen, und eine
    Uhr im Taskleisten-Feld darf falsch gehen.
  * `zeit_geprueft` (13) liefert **UNIX-Sekunden in UTC** — oder
    `ZeitUnplausibel` (26), wenn die Uhr vor dem Bau-Datum des Kernels steht
    oder absurd weit in der Zukunft. **Wer Zertifikate prüft, nimmt diesen
    hier**, und wenn er scheitert, wird nicht geprüft und nicht verbunden.
    Die Versuchung „die Uhr stimmt nicht, prüfen wir die Gültigkeit halt
    nicht" ist der Punkt, an dem TLS aufhört, etwas wert zu sein
    (`docs/tls-vertrauen.md` §3e).
  * Beide liefern **UTC**. Die Anzeige-Zeitzone ist reine Kosmetik und lebt
    in den Einstellungen, nicht in der ABI (`src/zeit.rs`).
- **`speicher(bytes)`** erweitert den **User-Heap** des aufrufenden Prozesses
  und liefert die Basis des NEUEN Stücks (Serie 7, Teil 3). `bytes` wird auf
  Seiten aufgerundet; die neuen Seiten sind beschreibbar, **nicht
  ausführbar** (W^X gilt weiter) und genullt.
  * **Das neue Stück schliesst IMMER lückenlos an das bisherige Heap-Ende
    an.** Darauf verlässt sich der Allocator im User-Space
    (`libspeed::heap`): Es gibt genau einen zusammenhängenden Heap, der nach
    oben wächst — dasselbe Modell wie `brk` unter Unix.
  * Layout: ab `prozess::HEAP_START` (= `elf::IMAGE_ENDE` + 4 KiB), höchstens
    `HEAP_MAX_BYTES` (12 MiB). Darüber `KeinPlatz` (14). Danach bleiben
    3 MiB ungemappt als Abstand zum Stack.
  * **Es gibt kein Gegenstück zum Freigeben, und das ist Absicht:** Ein
    Prozess gibt Seiten nie einzeln zurück; sein Adressraum fällt beim Ende
    als Ganzes (Serie 6, Teil 3). Der Allocator verwaltet innerhalb dessen,
    was er bekommen hat.
  * Aus dem Kernel-Prozess (PID 0) gerufen: `NichtUnterstuetzt` (23) — der
    Kernel hat seinen eigenen Heap.

### Nachgemessen statt behauptet (Serie-7-Abschluss)

Die beiden neuen Syscalls sind der Angriffsfläche wegen eigens beschossen
worden (`userland/angreifer`, ausgewertet in `tests/sicherheit.rs`):

| Angriff | Ergebnis |
|---|---|
| `zufall` mit Kernel-Zeiger als Ziel (5 Adressen) | jedes Mal `UngueltigerZeiger` (3) |
| `zufall` mit Längen bis `u64::MAX` | `ZuGross` (4), kein Überlauf |
| `zufall` mit Ziel, das über die Bereichsgrenze reicht | abgelehnt — **und der Puffer blieb Byte für Byte unverändert** |
| `zufall` mit `len == 0` | folgenlos, nichts geschrieben |
| `speicher` mit `u64::MAX`, `MAX-4095`, `1<<60`, `1<<40` | abgelehnt, kein Überlauf |
| `speicher` in 1-MiB-Schritten, bis der Kernel Nein sagt | **genau 12 MiB**, dann `KeinPlatz` (14) |

Die dritte Zeile ist die wichtigste: Ein halb gefüllter Zufallspuffer wäre
heimtückischer als ein Fehler — Nullen sehen aus wie Zufall. RNG-Dauerregel
IV („lieber warten als schwach") ist damit nicht nur formuliert, sondern
geprüft.

---

## 5. Gruppe 1 — Dateien (VFS)

| Nr | Name | Argumente | Ergebnis (`rdx`) | Typische Fehler |
|---:|---|---|---|---|
| 16 | `oeffne` | `pfad_ptr`, `pfad_len`, `modus` | Handle | 2, 3, 4, 8, 11, 13, 6 |
| 17 | `lese_at` | `handle`, `offset`, `ptr`, `len` | gelesene Bytes (0 = Dateiende) | 5, 7, 3, 4, 8, 23 |
| 18 | `schreibe_at` | `handle`, `offset`, `ptr`, `len` | geschriebene Bytes | 5, 7, 3, 4, 14, 15 |
| 19 | `schliesse` | `handle` | 0 | 2, 5 |
| 20 | `stat` | `pfad_ptr`, `pfad_len`, `ziel_ptr` | 32 (Bytes geschrieben) | 3, 8, 13 |
| 21 | `liste` | `pfad_ptr`, `pfad_len`, `ziel_ptr`, `ziel_len` | **Anzahl Einträge im Verzeichnis** | 3, 8, 10, 13 |
| 22 | `loesche` | `pfad_ptr`, `pfad_len` | 0 | 8, 12, 15 |
| 23 | `umbenenne` | `von_ptr`, `von_len`, `nach_ptr`, `nach_len` | 0 | 8, 9, 11, 23 |
| 24 | `mkdir` | `pfad_ptr`, `pfad_len` | 0 | 9, 10, 13, 14 |

**Pfade müssen ABSOLUT sein** (mit `/` beginnen). Relative Pfade bräuchten ein
Arbeitsverzeichnis pro Prozess — das gibt es nicht, also wird ehrlich mit
`UngueltigerPfad` abgelehnt statt still geraten.

### Modus-Bits für `oeffne`

| Bit | Wert | Bedeutung |
|---|---:|---|
| Lesen | 1 | Handle darf lesen |
| Schreiben | 2 | Handle darf schreiben |
| Anlegen | 4 | Datei anlegen, wenn sie fehlt |
| Abschneiden | 8 | vorhandene Datei auf Länge 0 bringen |

Unbekannte Bits → `UngueltigesArgument`. Ohne Lesen **oder** Schreiben →
`UngueltigesArgument`. Anlegen/Abschneiden ohne Schreiben →
`UngueltigesArgument` (Widerspruch). Ein Verzeichnis öffnen → `KeineDatei`.

### `StatDaten` (32 Byte, geschrieben von `stat`)

| Offset | Feld | Bedeutung |
|---:|---|---|
| 0 | `typ` | 0 = Datei, 1 = Verzeichnis |
| 8 | `groesse` | Bytes (Verzeichnisse: 0) |
| 16 | `erstellt` | Sekunden seit 1.1.2000 |
| 24 | `geaendert` | Sekunden seit 1.1.2000 |

### `DirEintragDaten` (128 Byte je Eintrag, geschrieben von `liste`)

| Offset | Feld | Bedeutung |
|---:|---|---|
| 0 | `typ` | 0 = Datei, 1 = Verzeichnis |
| 8 | `groesse` | Bytes |
| 16 | `name_laenge` | **echte** Namenslänge in Bytes |
| 24 | `name[104]` | Name, ggf. **abgeschnitten** |

`liste` schreibt so viele Einträge, wie in `ziel_len` passen
(`ziel_len / 128`), und liefert die **Gesamtzahl** zurück. Ist die größer als
die geschriebene Anzahl, war der Puffer zu klein — der Aufrufer kommt mit einem
größeren wieder. Namen über 104 Byte werden abgeschnitten, aber
`name_laenge` nennt die echte Länge: Der Aufrufer merkt, dass er etwas verpasst.

### Zwei ehrliche Folgen des pfadbasierten VFS

1. **Ein Datei-Handle merkt sich den PFAD**, nicht ein Kernel-Objekt. Wird die
   Datei zwischen `oeffne` und `lese_at` umbenannt oder gelöscht, liefert das
   Handle `NichtGefunden`. POSIX-Semantik („das Handle hält den Inode fest")
   gibt unser `FileSystem`-Trait nicht her — das wäre eine Änderung am VFS,
   nicht an der Syscall-Schicht.
2. **Es gibt keine Dateiposition.** Jeder Zugriff nennt seinen Offset
   (`lese_at`/`schreibe_at`). Position im Kernel zu halten wäre Zustand, den
   das VFS gar nicht braucht. Deshalb **hängt** `schreibe(datei_handle, ...)`
   aus Gruppe 0 **an** — die einzige sinnvolle Deutung eines Schreibvorgangs
   ohne Position.

---

## 6. Gruppe 2 — Netz (Sockets)

| Nr | Name | Argumente | Ergebnis (`rdx`) | Typische Fehler |
|---:|---|---|---|---|
| 32 | `socket` | `typ` (0 = TCP, 1 = UDP) | Handle | 2, 14, 6 |
| 33 | `verbinde` | `handle`, `ip`, `port` | 2 (= verbunden) | 5, 7, 2, 17, 18, 19, 21, 22 |
| 34 | `sende` | `handle`, `ptr`, `len` | **übernommene** Bytes | 5, 7, 3, 4, 20 |
| 35 | `empfange` | `handle`, `ptr`, `len` | Bytes (**0 = noch nichts**) | 5, 7, 3, 4 |
| 36 | `aufloesen` | `name_ptr`, `name_len` | IPv4 als `u32` | 2, 3, 4, 8, 17, 18 |
| 37 | `socket_zustand` | `handle` | Zustands-Zahl (siehe unten) | 5, 7 |

**IPv4-Darstellung:** `a.b.c.d` als `u32` = `a<<24 | b<<16 | c<<8 | d`.
`10.0.2.2` ist also `0x0A000202`. Werte über `u32::MAX` →
`UngueltigesArgument`.

**Port 0** ist ungültig (`UngueltigesArgument`) — ein Ziel-Port 0 ist immer ein
Fehler des Aufrufers.

### Zustands-Zahlen (`socket_zustand`)

| Zahl | Bedeutung |
|---:|---|
| 0 | Neu (offen, nicht verbunden) |
| 1 | Verbindet (Handshake läuft) |
| 2 | Verbunden |
| 3 | Gegenstelle hat geschlossen (alles empfangen) |
| 4 | Geschlossen |
| 5 | Lauscht |

### Blockierend oder nicht — bewusst unterschiedlich

- **`verbinde` BLOCKIERT** (Frist 8 s). Grund: `socket::verbinden` im Kernel
  ist nicht-blockierend — es startet nur den TCP-Handshake; wer wissen will, ob
  er geklappt hat, muss den Stack „pumpen". Ein Ring-3-Programm **kann** das
  nicht (Pumpen ist Kernel-Innenleben und wird nie ein Syscall). Ohne einen
  Kernel, der für ihn pumpt, würde ein Prozess auf einen Handshake warten, der
  nie vorankommt. Also pumpt der Syscall selbst.
- **`empfange` BLOCKIERT NICHT** (0 = noch nichts da). Ein blockierendes
  Empfangen bräuchte ein Warte-Modell mit Weck-Bedingung auf ein
  Socket-Ereignis — das ist der nächste Schritt. Bis dahin ist Pollen ehrlicher
  als ein verstecktes Timeout.
- **`sende` gibt ehrlich weniger zurück**, wenn der TCP-Sendepuffer voll ist.
  Der Aufrufer ruft nochmal — statt dass der Kernel intern schleift und dabei
  unbegrenzt blockiert.
- **`aufloesen` BLOCKIERT** (bis 3 Versuche à 1,2 s, siehe `netz::dns`).

---

## 7. Nur für Tests

| Nr | Name | Zweck |
|---:|---|---|
| 240 | `kontext_test` | kopiert den eingehenden Trap-Rahmen weg — beweist die Kontext-Sicherung (`tests/scheduler.rs`) |

---

## 8. Was ein Syscall darf — und was nicht (Lock-Disziplin)

Diese Regel gehört zur ABI, weil sie bestimmt, welche Syscalls es geben kann.

Ein Syscall läuft im Kontext eines **präemptiv geplanten** Prozesses, und im
Interrupt-Gate sind Interrupts **aus**. Daraus folgt beides:

- **(a)** Solange Interrupts aus bleiben, kann der Scheduler nicht wechseln —
  ein gehaltener Lock wird also garantiert wieder freigegeben. Locks, die der
  Kernel ohnehin nur mit ausgeschalteten Interrupts hält (KONSOLE,
  FRAMEBUFFER, MANAGER, SERIAL, SOCKETS, GERAET, alle Blatt-Locks), sind aus
  einem Syscall **gefahrlos** benutzbar: Wenn der Syscall läuft, hält sie
  niemand.
- **(b)** Auf einen Lock **warten** darf ein Syscall mit ausgeschalteten
  Interrupts **niemals**. Hält ihn der verdrängte Kernel-Prozess, kommt der nie
  wieder dran — Hänger. `fs::mit_fs` ist genau so ein Lock (ohne
  `without_interrupts`).

Deshalb gibt es zwei Bausteine in `src/syscall/mod.rs`:

- **`warte_fenster()`** — Interrupts an, `hlt`, Interrupts aus. Nur **hier**
  darf der Scheduler mitten im Syscall wechseln, und die eiserne Bedingung ist:
  **kein Lock in der Hand**. (Nebenbei: `hlt` mit ausgeschalteten Interrupts
  wäre ein Stillstand für immer.)
- **`mit_vfs()`** — `try_lock` in einer Schleife mit Wartefenstern dazwischen
  (bis 50 Versuche, dann `Belegt`). Hat der Lock geklappt, läuft die Operation
  mit ausgeschalteten Interrupts.

**Konsequenz für neue Syscalls:** Wer eine Kernel-Funktion in einen Syscall
hängen will, muss vorher wissen, wie ihre Locks gehalten werden. Bei einem
Lock, der ohne `without_interrupts` genommen wird, ist `mit_vfs`-Muster oder
ein Warte-Modell Pflicht — nicht der direkte Aufruf.

---

## 8b. Prozesse arbeiten zusammen (Serie 6, Teil 6)

| Nr | Name | Argumente | Ergebnis | Blockiert? |
|----|------|-----------|----------|-----------|
| 7 | `lese` | handle, ptr, len | Bytes (**0 = Dateiende**) | ja, auf leerer Pipe |
| 8 | `warte` | pid (0 = irgendein Kind) | Exit-Code | ja, bis das Kind endet |
| 9 | `beende` | pid | 0 | nein |
| 10 | `pipe` | — | lese \| (schreib << 32) | nein |
| 11 | `starte` | pfad_ptr, pfad_len, eingabe, ausgabe | PID | nein |

Zusätzlich kann seit Teil 6 auch **`schreibe` (3) blockieren** — auf einer
vollen Pipe.

### Wie ein blockierender Syscall funktioniert: der NEUSTART

Ein Syscall hält bei uns **nicht** mitten drin an. Der gesicherte Kontext ist
der Trap-Rahmen am Eingang (siehe `prozess.rs`); beim Umschalten wird er per
`iretq` geladen, die CPU landet also hinter dem `int 0x80` — der Rust-Stack
des halben Syscalls wäre verloren.

Stattdessen wird der Syscall **von vorn wiederholt**: Der Dispatcher stellt
`rip` um zwei Bytes zurück (die Länge von `int 0x80` = `CD 80`) und legt den
Prozess schlafen. Nach dem Aufwachen ist der nächste Befehl wieder der
Syscall, mit unveränderten Argumenten.

Daraus folgen zwei Regeln für jeden blockierenden Syscall:

* **Bis zum Blockieren darf nichts verändert worden sein** — sonst passiert
  es beim Neustart ein zweites Mal. Die Pipe-Operationen melden deshalb
  „blockiert", *bevor* sie Bytes anfassen.
* **`rax` und `rdx` bleiben unberührt** — sie tragen noch Syscall-Nummer und
  Argument 2.

Geweckt wird **durch Nachsehen, nicht durch Anstossen**: Der Timer prüft bei
jedem Tick (4 ms) die Weck-Bedingung jedes wartenden Prozesses (`Warteauf`).
Ein Weckruf aus dem schreibenden Prozess wäre schneller, würde aber eine
Lock-Kette quer durch den Kernel bedeuten — aus einem Syscall heraus, in dem
wir nicht warten dürfen. Der Preis ist höchstens ein Tick Verzögerung.

### Pipes

Ein Byte-Rohr fester Grösse (4 KiB), zwei Enden, jedes mit einem
**Besitz-Zähler** (nicht Flag — ein Ende kann mehrere Besitzer haben, etwa
während der Weitergabe an ein Kind).

* **voll** → der Schreiber blockiert (Gegendruck),
* **leer, Schreiber vorhanden** → der Leser blockiert,
* **leer, kein Schreiber mehr** → `lese` liefert **0 = Dateiende**,
* **kein Leser mehr** → `schreibe` liefert `Abgebrochen` (das POSIX-EPIPE).

Ein Pipe-Ende gehört einem Handle. Endet ein Prozess, schliesst der `Drop`
seiner Handle-Tabelle alle Enden — **beim Abräumen**, nicht beim Markieren
(Freigeben darf nicht im Interrupt passieren). Das Dateiende kommt also,
sobald der Aufräum-Task (250 ms) bzw. die Pump-Schleife der Shell den
beendeten Prozess geerntet hat.

### Handle-Weitergabe an ein Kind

`starte` nimmt zwei Handles, die im Kind zu **0 (Eingabe)** und **1
(Ausgabe)** werden. `u64::MAX` (`ERBE_KEINS`) heisst „nicht umleiten" —
bewusst nicht 0, denn **0 ist ein gültiges Handle**.

Ein Kind merkt davon nichts: Es liest von 0 und schreibt auf 1 wie immer.
Genau daraus wird `zaehle | filter 7`.

### Eltern, Kind und die Abwesenheit von Zombies

`starte` trägt den Aufrufer als **Elternteil** ein. Endet ein Kind, wandert
sein Ergebnis in einen kleinen Puffer **im Elternteil**, und der Kind-Eintrag
verschwindet sofort vollständig (Adressraum, Kernel-Stack, Handles).

Das ist die Umkehrung des Unix-Modells: Dort bleibt der *Kind*-Eintrag als
Zombie liegen, bis jemand `wait` ruft. Bei uns gibt es keinen Zustand, in dem
ein toter Prozess noch Ressourcen hält. Zwei Folgen, beide gewollt:

* Stirbt der Elternteil, verschwinden ungelesene Ergebnisse mit ihm — kein
  Waisen-Aufsammler nötig.
* Der Puffer ist **endlich** (feste Plätze, keine Allokation im Syscall-Pfad).
  Läuft er über, geht das *älteste* Ergebnis verloren.

`warte` auf ein Kind, das es nicht gibt, ist ein **Fehler**
(`NichtGefunden`) — darauf zu warten wäre ein Hänger für immer. Ein zweites
`warte` auf dasselbe Kind schlägt deshalb ebenfalls fehl.

**Rechte:** Es gibt keine. Jeder Prozess darf jeden anderen beenden — die
Folge davon, dass es keinen Benutzerbegriff gibt (§10). Geschützt ist nur
der Kernel-Prozess (PID 0).

---

## 9. Der Prozess-Start: Einsprung und Argumente

*(Serie 6, Teil 5 — ab hier lädt SpeedOS echte Programme, siehe `src/elf.rs`.)*

Ein Programm ist eine **statisch gelinkte ELF64-Datei vom Typ `ET_EXEC`** für
x86-64. Kein dynamisches Linken, kein PIE, kein Interpreter — `elf::pruefen`
lehnt das ausdrücklich ab (mit einem eigenen Fehler, damit ein versehentlicher
PIE-Build sofort erkennbar ist).

### Speicher-Layout

| Von | Bis | Inhalt |
|-----|-----|--------|
| `0x80_0000_0000` | `+16 MiB` | Programm-Image (die `PT_LOAD`-Segmente) |
| `+16 MiB` | `+32 MiB − 68 KiB` | **ungemappt** (trennt Programm und Stack) |
| `+32 MiB − 68 KiB` | `+32 MiB − 64 KiB` | Guard-Page (ungemappt) |
| `+32 MiB − 64 KiB` | `+32 MiB` | User-Stack (16 Seiten, **nicht ausführbar**) |

`0x80_0000_0000` ist `adressraum::USER_START`, der einzige P4-Slot, der jedem
Prozess privat gehört. Jedes Programm liegt an derselben Adresse — in seinem
eigenen Adressraum.

### Segment-Rechte: W^X

Jedes Segment wird mit **genau** seinen `p_flags` gemappt. Ein Segment, das
zugleich `PF_W` und `PF_X` trägt, wird **abgelehnt** — eine Seite ist entweder
beschreibbar oder ausführbar, nie beides. Durchgesetzt wird das per NX-Bit
(EFER.NXE, siehe `memory::nx_aktivieren`); der Stack ist ebenfalls NX.

Weil zwei Segmente mit verschiedenen Rechten sich keine Seite teilen dürfen
(sonst wäre W^X aushebelbar), lehnt der Loader auch **überlappende Segmente**
auf Seiten-Ebene ab. Das Linker-Skript `userland/speedos.ld` richtet deshalb
jede Sektion auf 4 KiB aus.

### `.bss`

`p_memsz > p_filesz` ist erlaubt; die Differenz ist `.bss` und **ist garantiert
genullt**. Der Kernel muss dafür nichts tun: Jeder frisch gemappte Frame wird
ohnehin genullt, damit kein Byte des Vorbesitzers nach Ring 3 leckt.

### Register beim ersten Befehl

| Register | Inhalt |
|----------|--------|
| `rip` | `e_entry` aus dem ELF-Header |
| `rsp` | Stack-Spitze, unterhalb der argv-Daten, 16-ausgerichtet |
| `rdi` | **argc** — Anzahl der Argumente (inkl. Programmname) |
| `rsi` | **argv** — Zeiger auf ein Feld von `ArgEintrag` |
| alle übrigen | 0 |

```c
struct ArgEintrag {   // 16 Byte, repr(C)
    uint64_t zeiger;  // auf die Bytes des Arguments
    uint64_t laenge;  // in Bytes
};
```

**Argumente sind (Zeiger, Länge), NIE nullterminiert** — dieselbe Regel wie
für jeden Puffer dieser ABI. Der Kernel sucht nirgends ein Terminator-Byte in
fremdem Speicher, und ein Argument darf deshalb jedes Byte enthalten.

`argv[0]` ist per Konvention der Programmname. Grenzen: höchstens 16 Argumente,
je 255 Byte, zusammen 2 KiB.

Die Register-Übergabe ist eine bewusste Abweichung von System V (das legt
argc/argv auf den Stack): Wir schreiben unsere Start-Runtime selbst, und
`rdi`/`rsi` sind genau die Register, die `extern "C" fn(u64, *const ArgEintrag)`
erwartet. `libspeed::hauptprogramm!` erzeugt dazu ein `_start`, das den Stack
16-ausrichtet und per `call` in Rust springt.

### Rückgabe

Der Rückgabewert von `haupt` wird zu `exit(code)`. Der Aufrufer (`starte`,
`scheduler::warten_auf`) erfährt über `ProzessEnde`, ob der Prozess sich
beendet hat (`Beendet(code)`), abgestürzt ist (`Abgestuerzt`, Code 139) oder
von aussen gestoppt wurde (`Gestoppt`, Code 143).

---

## 10. Bewusst NICHT in dieser ABI

- **Kein `fork`.** Ein Prozess entsteht immer aus einer DATEI (`starte`),
  nie als Kopie seines Elternteils. Das erspart uns Copy-on-Write und die
  halbe Semantik von Unix — und `starte` kann alles, wofür wir `fork`+`exec`
  bräuchten.
- **`starte` gibt KEINE Argumente an das Kind weiter** (nur `argv[0]` = der
  Dateiname). Ein Feld von Zeigern aus dem User-Speicher zu prüfen und zu
  kopieren wäre machbar, kauft aber nichts, solange die Shell die Pipelines
  baut. Wer Argumente braucht, wird von der Shell gestartet.
- **Kein `dup`/`dup2`.** Handles lassen sich beim START weitergeben, aber ein
  laufender Prozess kann seine Kanäle nicht umbiegen.
- **Kein `select`/`poll`** — auf MEHRERE Quellen gleichzeitig zu warten
  bräuchte eine Weck-Bedingung aus mehreren Gründen. `Warteauf` trägt genau
  einen; das zu erweitern ist additiv, aber noch nicht gebraucht.
- **Kein blockierendes `empfange`** (Sockets). `lese` auf Pipes blockiert
  seit Teil 6; für Sockets fehlt die Weck-Bedingung „Daten im Socket", weil
  der RX-Weg über den Netz-Task läuft.
- **Kein Arbeitsverzeichnis**, also nur absolute Pfade.
- **Keine Fenster-Syscalls.** Die Fenster/UI-API ist die aufwendigste der drei
  Nähte (ein Protokoll statt einer Funktion), und sie hat eine harte
  Vorbedingung: Ein Zeichenbefehl aus Ring 3 darf den MANAGER-Lock nicht
  synchron nehmen, wenn er lange dauert. Das braucht eine Kommando-Warteschlange
  pro Fenster — bewusst vertagt.
- **Keine Rechte/Benutzer.** Jeder Prozess darf alles, was die ABI hergibt —
  einschliesslich `beende` auf fremde Prozesse. Ein Rechte-Modell ohne
  Benutzerbegriff wäre Attrappe.
