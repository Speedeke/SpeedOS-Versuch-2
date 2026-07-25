# Die Syscall-ABI von SpeedOS

Stand: Juli 2026, Serie 6 Teil 4. **Dieses Dokument ist die Schnittstelle
zwischen Kernel und User-Space.**

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

## 9. Bewusst NICHT in dieser ABI

- **Kein `fork`/`exec`** — Prozesse entstehen (noch) nur kernelseitig. Ohne
  ELF-Loader gäbe es auch nichts zu `exec`-en.
- **Kein blockierendes `empfange`/`lese`** und kein `select`/`poll` — beides
  braucht das Warte-Modell (Prozess wird `Wartend` auf ein Ereignis, ein
  Kernel-Task weckt ihn). Der nächste Schritt.
- **Kein Arbeitsverzeichnis**, also nur absolute Pfade.
- **Keine Fenster-Syscalls.** Die Fenster/UI-API ist die aufwendigste der drei
  Nähte (ein Protokoll statt einer Funktion), und sie hat eine harte
  Vorbedingung: Ein Zeichenbefehl aus Ring 3 darf den MANAGER-Lock nicht
  synchron nehmen, wenn er lange dauert. Das braucht eine Kommando-Warteschlange
  pro Fenster — bewusst vertagt.
- **Keine Rechte/Benutzer.** Jeder Prozess darf alles, was die ABI hergibt.
  Ein Rechte-Modell ohne Benutzerbegriff wäre Attrappe.
- **Kein `dup`/`dup2`**, keine Handle-Vererbung — es gibt keinen
  Prozess-Elternteil.
