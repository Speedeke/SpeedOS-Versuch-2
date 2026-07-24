# Bestandsaufnahme für Serie 6 — User-Space, Syscalls und der Weg zum Browser

Stand: Ende Serie 5 (Juli 2026). Serie 5 hat SpeedOS ins **Netz** gebracht
(virtio-net, eigener Stack Ethernet→ARP→IPv4→ICMP→UDP→DHCP→DNS→TCP, eine
Socket-API und einen HTTP/1.1-Client). Serie 6 soll den vielleicht größten
konzeptionellen Sprung machen: von einem **Kernel, der alles selbst tut**, zu
**echten User-Space-Prozessen**, die den Kernel nur noch über **Syscalls**
bitten. Diese Bestandsaufnahme beantwortet die vier Kernfragen ehrlich —
inklusive dessen, was das erzwingt und wo es wehtun wird.

Wichtig vorweg, was wir SCHON haben (das verkürzt den Weg erheblich):

- **GDT mit TSS** (`src/gdt.rs`) — inklusive eines IST-Eintrags für den
  Double-Fault-Handler. Ring-3-Segmente und ein Kernel-Stack-Pointer im TSS
  (RSP0) fehlen noch, aber das Gerüst steht.
- **Paging über einen globalen Mapper** (`src/memory.rs`): `map_page`,
  `map_page_zu`, `unmap_page`, `allocate_pages`, ein Bitmap-Frame-Allocator.
  Wir mappen heute alles in EINEN Adressraum (den des Kernels).
- **Ein kooperativer async-Executor** (`src/task/executor.rs`) mit Waker-
  Support, benannten Tasks und einer Schatten-Registry (Task-Manager).
- **Drei Handle-basierte, copy-in/out-fähige APIs**, bewusst als
  Syscall-Nähte gebaut: die **Socket-API** (`src/netz/socket.rs` — Handles,
  klare Fehler-Enums, Puffer-Ownership explizit), das **VFS** (`src/fs/` —
  `FileSystem`-Trait, pfadbasiert) und die **Fenster/UI-APIs** (`src/fenster/`,
  `src/ui/`).
- **Der PIT-Timer-Interrupt** (250 Hz) tickt bereits und weckt Tasks.

---

## (a) Was der Sprung zu echten User-Space-Prozessen konkret braucht

Vier Bausteine, in der Reihenfolge ihrer Abhängigkeit:

### 1. Ring 3 (die CPU-seitige Trennung)
Ein User-Prozess läuft in **Privilegstufe 3** statt 0. Konkret:
- **GDT** um einen User-Code- und User-Data-Deskriptor (DPL 3) erweitern.
- Das **TSS** um `RSP0` (den Kernel-Stack, auf den die CPU bei einem Trap aus
  Ring 3 umschaltet) ergänzen — das TSS haben wir schon, es fehlt nur das Feld.
- Den ersten Sprung nach Ring 3 per `iretq` mit gefälschtem Stack-Frame
  (User-CS/SS mit RPL 3, User-RIP/RSP, RFLAGS).
- Die Seiten des User-Codes/Stacks/Heaps als `USER_ACCESSIBLE` mappen (das
  Page-Table-Flag existiert in `x86_64` schon, wir setzen es heute nur nie).

**Schwierigkeit: mittel.** Das ist gut dokumentiertes Standard-x86-Handwerk;
die hart erkämpften UEFI-/GDT-Lektionen aus der 0.11-Migration (SS/DS/ES nach
GDT-Load neu setzen) sind genau die Klasse von Fallstricken, die uns hier
wieder begegnen.

### 2. Adressraum-Trennung pro Prozess
Jeder Prozess braucht sein **eigenes Page-Table-Wurzelverzeichnis** (eigenes
`CR3`), damit er den Speicher anderer Prozesse (und den Kernel-Speicher) NICHT
sehen kann. Konkret:
- Pro Prozess ein neues Level-4-Table (PML4) anlegen, den **Kernel-Bereich
  darin teilen** (obere Hälfte gemeinsam mappen, damit Syscalls/Interrupts
  funktionieren), den User-Bereich privat.
- Beim Kontextwechsel `CR3` umladen (das leert den TLB — akzeptabel ohne
  PCID).
- Unser `memory.rs` mappt heute nur in EINEN Adressraum. Das muss zu „mappe in
  DIESES PML4" verallgemeinert werden — überschaubar, weil die Frame-Allocator-
  und Mapper-Bausteine schon getrennt sind.

**Schwierigkeit: mittel-hoch.** Die Kernel/User-Aufteilung des Adressraums
sauber zu ziehen (higher-half-Kernel) ist die eigentliche Denkarbeit.

### 3. Präemptiver Scheduler (Timer-getrieben)
Heute ist Multitasking **kooperativ**: Ein Task läuft, bis er `await`-t. Ein
User-Prozess kann aber nicht kooperieren (er kennt unsere `await`-Punkte nicht
und darf eine Endlosschleife drehen, ohne das System einzufrieren). Also:
- Der **Timer-Interrupt** (haben wir, 250 Hz) muss den laufenden User-Prozess
  **unterbrechen**, seinen Registersatz sichern, und ggf. zu einem anderen
  Prozess wechseln (Register + `CR3` + `RSP0` tauschen).
- Ein **Zeitscheiben-Scheduler** (Round-Robin genügt zum Start) wählt den
  nächsten lauffähigen Prozess.
- Der **kooperative Kernel-Executor bleibt** für die Kernel-Dienste (Netz,
  Compositor, …) — die beiden koexistieren: Kernel-Tasks laufen kooperativ,
  User-Prozesse präemptiv, der Scheduler steht dazwischen. (So macht es auch
  die Realität: Kernel-Threads vs. User-Threads.)

**Schwierigkeit: hoch.** Das Registersichern/-wiederherstellen im Interrupt-
Kontext und die Interaktion mit dem bestehenden Executor sind die kniffligsten
Stellen der ganzen Serie.

### 4. Ein ELF-Loader
Ein User-Programm liegt als Datei vor (statisch gelinktes ELF). Der Loader:
- liest die **Program-Header** (`PT_LOAD`-Segmente), mappt sie an ihre
  virtuellen Adressen (Code als ausführbar+nur-lesen, Daten als schreibbar),
- richtet einen **User-Stack** ein,
- setzt den Einsprungspunkt (`e_entry`) und springt nach Ring 3.
- Wir haben schon ein **VFS** — das ELF kommt einfach aus einer Datei
  (`/platte/bin/hallo`), gelesen über `fs::mit_fs`.

**Schwierigkeit: mittel.** Ein MINIMALER statischer-ELF-Loader (nur `PT_LOAD`,
keine dynamische Verlinkung, keine Relocations) ist erstaunlich kompakt.

### Was uns endlich zu APIC/MSI zwingt — und was NICHT

Ehrliche Antwort: **Der Sprung zu User-Space erzwingt APIC noch NICHT.** Ein
präemptiver Round-Robin-Scheduler läuft prima auf dem PIT-Interrupt, den wir
schon haben — der 8259-PIC reicht für einen Single-Core-Zeitscheiben-Wechsel
vollkommen. Man kann echtes präemptives Multitasking mit User-Prozessen bauen,
ohne den APIC anzufassen.

**Was den APIC (und dann MSI) WIRKLICH erzwingt**, kommt später und aus drei
Richtungen:
1. **SMP (mehrere CPU-Kerne).** Der 8259-PIC ist per Konstruktion Single-CPU.
   Sobald wir einen zweiten Kern hochfahren wollen (AP-Start über den Local
   APIC, Interrupt-Routing über den I/O-APIC), führt kein Weg am APIC vorbei.
   Das ist der eigentliche, große APIC-Meilenstein.
2. **Ein präziser, per-CPU-Timer.** Der PIT ist ein einziger globaler Timer;
   der **LAPIC-Timer** ist pro Kern und feiner — nötig für gutes Scheduling
   auf mehreren Kernen, für Single-Core aber verzichtbar.
3. **MSI/MSI-X** (Message Signaled Interrupts) werden interessant, wenn wir
   viele/moderne PCIe-Geräte mit vielen Interrupt-Vektoren bedienen (mehrere
   virtio-Queues mit eigenem Vektor, NVMe-Completion-Queues). Für unsere EINE
   virtio-net-IRQ auf dem PIC ist es noch kein Blocker.

**Fazit:** Der vertagte APIC-Meilenstein bleibt vertagt, bis wir **SMP** wollen
— das ist der ehrliche Auslöser, nicht User-Space an sich. User-Space Serie 6
läuft auf dem PIT/PIC, den wir haben. (Das ist eine gute Nachricht: eine große
Unbekannte weniger auf dem kritischen Pfad.)

---

## (b) Wie aus Socket-/VFS-/Fenster-APIs echte SYSCALLS werden

Der entscheidende Punkt: **Wir haben die Nähte bewusst schon richtig gelegt.**
Die Socket-API arbeitet mit Handles (undurchsichtige Zahlen, kein Kernel-
Zeiger nach außen), klaren Fehler-Enums und expliziter copy-in/out-Puffer-
Ownership — genau das Modell, das ein Syscall braucht. Der Umbau ist deshalb
weniger „neu erfinden" als „eine Grenze einziehen".

### Der Trap-Mechanismus
- **`syscall`/`sysret`** (die schnellen MSR-basierten Instruktionen) oder ein
  klassisches **Trap-Gate** (INT 0x80). Für den Anfang ist ein Interrupt-Gate
  (INT 0x80) am einfachsten — wir haben die IDT-Infrastruktur schon
  (`src/interrupts.rs`), es kommt ein Eintrag mit **DPL 3** dazu (damit Ring 3
  ihn auslösen darf) und ein Handler, der die Argumente aus den Registern
  liest.
- Konvention: Syscall-Nummer in `rax`, Argumente in `rdi, rsi, rdx, r10, r8,
  r9` (die Linux-x86_64-Konvention ist eine erprobte Vorlage), Rückgabe in
  `rax`.

### copy-in / copy-out (die Sicherheitsgrenze)
Ein User-Prozess übergibt **Zeiger in SEINEN Adressraum**. Der Kernel darf
diesen Zeigern nicht blind folgen (der Prozess könnte auf Kernel-Speicher
zeigen). Also:
- **copy-in**: Der Kernel prüft, dass der User-Puffer vollständig in
  gemapptem, dem Prozess gehörendem User-Space liegt, und KOPIERT die Daten in
  einen Kernel-Puffer, bevor er sie benutzt.
- **copy-out**: umgekehrt — der Kernel schreibt das Ergebnis in einen Kernel-
  Puffer und kopiert es dann in den geprüften User-Puffer.
- **Genau darauf ist unsere Socket-API schon ausgelegt**: `senden` kopiert
  HINEIN, `empfangen` kopiert HERAUS, in vom Aufrufer gestellte Slices. Aus
  „Slice, den die Kernel-Shell stellt" wird „Slice im User-Space, den der
  Kernel prüft und kopiert" — die Funktions-SIGNATUREN ändern sich nicht.

### Handle-Tabelle pro Prozess
Heute sind Socket-Handles global (eine monoton wachsende ID). In Serie 6:
- Jeder Prozess bekommt eine **eigene Handle-Tabelle** (kleine, prozess-lokale
  Zahlen 0,1,2,… wie POSIX-Dateideskriptoren), die auf die globalen Kernel-
  Objekte (Sockets, offene Dateien, Fenster) zeigt.
- Ein Syscall bekommt eine Prozess-Handle-Nummer, schlägt sie in der Tabelle
  des AUFRUFENDEN Prozesses nach und findet das Kernel-Objekt. Ein Prozess
  kann so die Handles eines anderen NICHT erraten/benutzen.
- Unsere globale Socket-Tabelle (`SOCKETS`) wird zur Kernel-Objekt-Tabelle;
  die per-Prozess-Tabelle ist eine dünne Indirektion darüber.

### Die drei APIs, konkret als Syscalls
- **Netz**: `sys_socket`, `sys_connect`, `sys_send`, `sys_recv`, `sys_close`
  — 1:1 auf `socket::{oeffnen, verbinden, senden, empfangen, schliessen}`.
- **VFS**: `sys_open`, `sys_read`, `sys_write`, `sys_close`, `sys_readdir` —
  auf `fs::mit_fs`. (`read_at`/`write_at` gibt es schon.)
- **Fenster**: `sys_fenster_oeffnen`, `sys_zeichnen`, `sys_ereignis_holen` —
  eine Anwendung bekommt einen Fenster-Puffer und schickt Zeichenbefehle bzw.
  holt Eingaben. Das ist der aufwendigste Teil (ein Protokoll statt einer
  Funktion), aber die `FensterPuffer`-/`Zeichner`-Abstraktion trägt schon.

**Fazit (b):** Kein Umbau der API-Semantik, sondern das Einziehen der
Kernel/User-Grenze DAVOR. Die Fassaden aus Serie 4/5 waren genau dafür gemacht.

---

## (c) Der kleinste sinnvolle erste User-Space-Prozess

**Ein statisch gelinktes „Hallo Welt" in Ring 3, das per Syscall druckt.**
Konkret der minimale Meilenstein-Pfad:

1. Ein winziges `no_std`-Programm (eigenes Crate, eigenes Bare-Metal-Target),
   dessen `_start` genau EINEN Syscall macht: `sys_write(1, "Hallo aus Ring 3\n",
   17)` — die Bytes über INT 0x80, dann `sys_exit(0)`.
2. Als **statisches ELF** bauen, in eine Datei legen (`/platte/bin/hallo`).
3. Kernel-seitig: GDT um Ring-3-Segmente + TSS-RSP0 erweitern, den INT-0x80-
   Handler mit `sys_write`/`sys_exit` bereitstellen (write leitet vorerst auf
   unsere serielle/Konsolen-Ausgabe um), den ELF-Loader die `PT_LOAD`-Segmente
   mappen lassen, einen User-Stack einrichten, per `iretq` nach Ring 3 springen.
4. **Beweis**: „Hallo aus Ring 3" erscheint — gedruckt von Code, der NICHT im
   Kernel-Privileg lief, sondern den Kernel per Syscall gebeten hat.

Das braucht bewusst NOCH KEINEN präemptiven Scheduler und KEINE Adressraum-
Trennung im Vollausbau: Ein einziger User-Prozess, der druckt und sich beendet,
ist der ehrliche „erste Ring-3-Atemzug". Scheduler und mehrere Adressräume
kommen als nächste Schritte, sobald ein einzelner Prozess nachweislich läuft.
(Dasselbe „kleinster sichtbarer Erfolg zuerst"-Muster wie in jeder Serie.)

---

## (d) Ausblick Browser — was an User-Space hängt, was früher geht, wo TLS blockt

Ein „Browser" ist kein Einzelschritt, sondern ein Bündel. Ehrlich zerlegt:

### Was SCHON heute (oder als Kernel-App früh) gehen könnte
- **HTTP holen**: haben wir (`hole`). Eine Ressource per http:// laden ist
  gelöst.
- **Ein simpler HTML-Renderer als KERNEL-APP** (analog SpeedText/Explorer im
  Toolkit): HTML ist Text; ein Parser für eine Teilmenge (Überschriften,
  Absätze, Links, Listen, vielleicht Tabellen) plus ein Block-Layout auf
  unserem `Zeichner`/Fenster-Toolkit ist machbar, OHNE User-Space. Das wäre ein
  „Text-Browser für http://-Seiten" — ein realistischer, lehrreicher
  Zwischenschritt, der nur unsere bestehenden Bausteine (HTTP-Client + UI-
  Toolkit) verbindet.
- **Bilder** (unkomprimiert/BMP, vielleicht ein einfacher PNG/GIF-Dekoder)
  kämen inkrementell dazu.

### Was WIRKLICH an User-Space hängt
- **Sicherheit/Isolation**: Ein echter Browser führt fremden, potenziell
  bösartigen Code aus (JavaScript, komplexe Parser auf Netz-Daten). Das gehört
  in einen **isolierten Prozess mit eingeschränkten Rechten** — das ist der
  Kern, warum Browser Prozess-Sandboxing haben. Ohne User-Space/Adressraum-
  Trennung ist jeder Parser-Bug ein Kernel-Exploit.
- **JavaScript** (eine Skript-Engine) ist ein eigenes Monats-Projekt und
  gehört klar in User-Space.
- **Nebenläufigkeit** vieler Tabs/Ressourcen-Ladungen profitiert stark von
  echten Prozessen/Threads (und irgendwann von SMP → APIC).

### Wo TLS/HTTPS zum echten Blocker wird
Das ist der harte Punkt — und er kommt FRÜH, sobald man „echte Webseiten"
will: **das offene Web ist praktisch vollständig auf https:// umgestiegen.**
Unsere `hole`-Fassade lehnt https bewusst sauber ab (`TlsNichtUnterstuetzt`).
Für echte Seiten (nicht nur die Handvoll verbliebener http-Server aus unserem
Stresstest) führt kein Weg an TLS 1.3 vorbei, und TLS ist:
- **krypto-schwer**: AES-GCM/ChaCha20-Poly1305, X25519, SHA-2,
  Zertifikatsprüfung mit RSA/ECDSA und eine Vertrauenskette (Root-CAs),
- **sicherheitskritisch**: ein fehlerhaftes TLS ist SCHLIMMER als keins,
- **kein „from scratch"-Kandidat** im selben Sinn wie TCP: Bei TCP war ein
  Lern-Artefakt vertretbar (falsch = langsam); bei TLS ist falsch = unsicher.

**Ehrliche Empfehlung für die Browser-Richtung:**
1. **Zuerst der http-Text-Browser als Kernel-App** (HTTP-Client + HTML-Teilmenge
   + UI-Toolkit) — ein sichtbarer, ehrlicher Erfolg ohne neue große Wette.
2. **Parallel/ danach User-Space** (Serie 6) für Isolation — dann kann der
   Browser (und später JS) in einen Prozess wandern.
3. **TLS** ist der Punkt, an dem „alles selbst" endet: eine geprüfte
   `no_std`-TLS-/Krypto-Bibliothek einbinden (wie bei TCP die smoltcp-Reißleine,
   nur dass wir sie hier von vornherein empfehlen) — ODER https bewusst
   vertagen und beim http-Text-Browser bleiben. Das ist die nächste große
   Weichenstellung, und sie sollte — wie die TCP-Reißleine — VOR dem Code als
   bewusste Entscheidung getroffen werden.

---

## Empfohlene erste Schritte für Serie 6

1. **GDT/TSS um Ring 3 erweitern** (User-Segmente, RSP0) — kleiner, testbarer
   Schritt.
2. **INT-0x80-Syscall-Gate** mit `sys_write`/`sys_exit` (Ausgabe vorerst auf
   die Konsole).
3. **Minimaler statischer ELF-Loader** (`PT_LOAD`) + ein „Hallo Welt"-User-
   Programm. **Meilenstein: erster Ring-3-Druck.**
4. **Adressraum pro Prozess** (eigenes CR3, higher-half-Kernel geteilt).
5. **Präemptiver Round-Robin-Scheduler** auf dem PIT (Kontextwechsel im
   Timer-Interrupt), koexistierend mit dem kooperativen Kernel-Executor.
6. **Socket/VFS-Syscalls** mit copy-in/out + per-Prozess-Handle-Tabelle —
   dann läuft ein User-Programm, das per Syscall eine http-Seite holt.

Jeder Schritt ein sichtbarer Erfolg; die riskanten Wetten (Scheduler-Kontext-
wechsel, TLS) sind bewusst benannt und ans passende Ende gestellt. APIC/SMP
bleibt vertagt, bis wir mehrere Kerne wollen — der ehrliche Auslöser.
