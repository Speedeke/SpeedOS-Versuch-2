# SpeedOS — Projektregeln für Claude

## Projekt
- SpeedOS: ein eigenes Betriebssystem from scratch in Rust. Kein Linux, keine fremde Kernel-Basis.
- Sprache: Rust **nightly**, `no_std`, Ziel-Architektur: **x86_64**.
- Bootloader: **bootloader_api 0.11** (UEFI-Boot mit linearem Framebuffer).
  Ursprünglich nach "Writing an OS in Rust" von Philipp Oppermann (bootloader 0.9)
  gebaut, im Juli 2026 migriert — Plan und Details in `docs/migration-011.md`.

## Build & Test-Umgebung
- Kernel-Target: das EINGEBAUTE `x86_64-unknown-none` (kein eigenes Target-JSON,
  kein build-std — rust-toolchain.toml installiert das Target automatisch).
- `cargo run`/`cargo test` rufen als Runner das Host-Programm **boot/** auf:
  Es baut per `bootloader::UefiBoot` das GPT-Disk-Image und startet
  **QEMU** (`qemu-system-x86_64`) mit der edk2/OVMF-Firmware aus dem
  QEMU-Installationsordner.
- Tests: Integrationstests laufen in QEMU mit dem **isa-debug-exit Device**;
  der Runner übersetzt Exit-Code 33 -> Erfolg (Timeout 300 s).

## Debug
- **ALLE** Ausgaben laufen doppelt: FramebufferKonsole (Bildschirm) UND
  serielle Schnittstelle (COM1 / Port 0x3F8, mit ANSI-Farben) — die Regel
  steckt in `konsole::_print`, nicht beim Aufrufer. Niemals nur Bildschirm.
  `serial_println!` nur für reine Debug-Ausgaben.

## Git
- Nach **JEDEM** funktionierenden Schritt ein Commit mit klarer Message.
- **NIEMALS** committen, wenn `cargo build` fehlschlägt oder QEMU nicht bootet.

## Arbeitsweise
- Kleine Schritte. Nach jeder Änderung selbst bauen, in QEMU starten, serielle Ausgabe prüfen.
- Fehler selbst debuggen und fixen, bevor "fertig" gemeldet wird.
- Der Projektbesitzer ist Anfänger in OS-Entwicklung: nach jedem Schritt in 2–3 Sätzen
  auf Deutsch erklären, was gebaut wurde.

## Code-Stil
- Ausführliche **deutsche Kommentare**, da der Projektbesitzer OS-Entwicklung lernen will.
- Jede Datei beginnt mit einem Kommentarblock, der erklärt, was sie tut.

## Architektur-Prinzip
- Mikrokernel-inspiriert: Treiber und Systemdienste so isoliert wie möglich
  (eigene Module, klare Schnittstellen, so wenig geteilter Zustand wie möglich).

## Architektur-Entscheidungen
- **VFS-Abstraktion (Juli 2026):** Alle Dateisysteme implementieren das Trait
  `FileSystem` in `src/fs/mod.rs` (lesen, schreiben, liste, mkdir, loeschen,
  node_typ — absolute, normalisierte Pfade mit `/`). Shell-Befehle und Kernel
  greifen NIE auf eine konkrete Implementierung zu, sondern nur über
  `fs::mit_fs()` auf das global gemountete VFS. Erste Implementierung ist
  `RamFs` (`src/fs/ramfs.rs`, in-memory); FAT32 und ein eigenes
  Disk-Dateisystem sollen später exakt dieselbe Schnittstelle bedienen —
  dann wird nur das gemountete Dateisystem ausgetauscht, kein Befehl ändert sich.
- **Grafik-Architektur (Juli 2026):** `framebuffer.rs` = Double Buffering
  (Back-Buffer aus `memory::allocate_pages`, `present()` kopiert als Block,
  `hochscrollen()` = memmove im Back-Buffer, NIE neu rendern). Font:
  noto-sans-mono-bitmap (vorgerastert, Latin-1 für Umlaute). `konsole.rs` =
  FramebufferKonsole (Raster, Farben = Obsidian-Aurora-Palette, Software-
  Cursor als async Blink-Task über `zeit::warte_ms`). Lock-Ordnung:
  KONSOLE vor FRAMEBUFFER, beide nur mit Interrupts aus. Niemals direkt
  in den echten Framebuffer zeichnen — immer Back-Buffer + present.
- **Async-Zeitwarten:** `zeit::warte_ms(ms)` statt yield-Polling — der
  Timer-Interrupt weckt per AtomicWaker (aktuell EIN Warter; bei Bedarf
  auf eine Waker-Liste erweitern).
- **Fenster & Compositor (Juli 2026):** `src/fenster/mod.rs`. JEDES
  Fenster = eigener Pixel-Puffer (`FensterPuffer`, Vec<Farbe>) +
  Metadaten (Position, Größe, Titel, Fokus). Z-Ordnung = Reihenfolge
  im `Vec<Fenster>` (letztes = ganz vorne). Apps zeichnen NUR in ihren
  Puffer, NIE auf den Bildschirm — dafür ist `grafik::Zeichner`
  generisch über das `Zeichenflaeche`-Trait (Bildschirm-Back-Buffer
  UND Fenster-Puffer implementieren es identisch). Der Compositor-Task
  setzt pro Frame zusammen: Desktop-Aurora-Hintergrund -> Fenster in
  Z-Reihenfolge (Schatten/Titelzeile/Rahmen malt der COMPOSITOR, nicht
  die App) -> present() -> Maus-Cursor obenauf. Dirty-Flags
  (`alles_dirty` + pro Fenster `dirty`): NUR komponieren, wenn sich
  etwas geändert hat. Event-Routing: Maus -> oberstes Fenster unter dem
  Cursor (in Fenster-Koordinaten umgerechnet, Titelzeile zählt nicht
  zum Inhalt), Klick hebt+fokussiert; Tastatur -> fokussiertes Fenster;
  Titelzeilen-Drag verschiebt. Lock-Ordnung: FRAMEBUFFER -> MANAGER.
  Der Desktop-Modus (AtomicBool) pausiert Konsole/Cursor; ESC kehrt
  zurück, die Fenster bleiben erhalten.
- **Fenster-Deko & Bedienung (Juli 2026):** Die Titelleiste (Icon,
  Titel, 3 Knöpfe Minimieren/Maximieren/Schließen — Schließen rot)
  zeichnet der COMPOSITOR, nicht die App. Interaktion-Enum:
  Verschieben (Titel-Drag), Größe (Rand-Drag; kante_bei berechnet die
  Zone, Cursor wechselt die Form via `maus::cursor_form_setzen`).
  Maximieren speichert die Vorher-Geometrie (Vollbild minus 40px
  Taskleisten-Reserve); Schließen droppt den Fenster-Puffer (Heap
  frei). Snap: Ziehen an den Bildschirmrand -> halbe Fläche; Vorschau
  (snap_hinweis) UND Loslassen nutzen denselben Wert (konsistent,
  positionsunabhängig). Alt+Tab: Der KeyStream greift LAlt/Tab VOR dem
  Dekodieren ab (KeyEvent.state), der Switcher lebt im Manager,
  Loslassen von Alt bestätigt. WICHTIG: Ein maximiertes/gesnapptes
  Fenster braucht einen fast bildschirmgroßen Puffer — desktop_starten
  lässt den Heap passend zur Auflösung wachsen (Breite*Höhe*3*3 Bytes).
- **PS/2-Paket-Grenze:** Ein Maus-Paket trägt nur 9-Bit-Deltas
  (±255); größere Bewegungen setzen das Overflow-Bit und werden
  verworfen (Spec-konform). Automatisierte QMP-Tests müssen die Maus
  in kleinen Schritten bewegen.
- **Mehrere Tick-Warter (Juli 2026):** `zeit::warte_ms` nutzt eine
  feste Slot-Liste von AtomicWakern (nicht EINEN!), weil Cursor,
  Compositor und Uhr gleichzeitig auf Ticks warten — ein einzelner
  AtomicWaker ließe alle bis auf den letzten verhungern. Slots werden
  lock-frei per compare_exchange belegt und in Drop zurückgegeben.
- **PS/2-Maus (Juli 2026):** `src/maus.rs` — Controller-Init NUR über
  die Maus-Bits (Tastatur-Bits 0/4/6 der 8042-Konfiguration niemals
  anfassen!), alle Handshakes gepollt mit Timeout (fehlende Maus hängt
  den Boot nicht), VOR sti. IntelliMouse-Rad per 200/100/80-Sequenz.
  Paket-Parsing ist eine reine, unit-getestete Funktion (Sync-Bit,
  9-Bit-Vorzeichen, Overflow -> verwerfen). IRQ 12 -> lock-freie Queue
  -> async maus_task (Tastatur-Muster). Cursor = Overlay NUR im
  Front-Buffer: Der Back-Buffer bleibt die "Wahrheit ohne Cursor",
  Wiederherstellen = present_bereich der alten Position.
- **Zeichen-Werkzeuge (Juli 2026):** `grafik.rs` = Zeichner auf dem
  Back-Buffer mit optionalem Clip-Rechteck und Alpha-Blending (alle
  Pixel laufen durch EINEN Pfad: Zeichner::pixel). Clipping-Schnitt
  und Alpha-Formel sind reine, unit-getestete Funktionen. Icons =
  16x16-ASCII-Art-Konstanten mit gemeinsamer Palette (unbekanntes
  Zeichen -> Magenta = sichtbarer Tippfehler). Demo-Modi (grafiktest)
  über AtomicBool-Flag: Shell fängt die nächste Taste ab und stellt
  die Konsole wieder her. Fließkomma gibt es NICHT (soft-float!) —
  alle Algorithmen ganzzahlig (Bresenham, Midpoint).
- **Bootloader-0.11-Migration (Juli 2026, docs/migration-011.md):** UEFI
  statt BIOS (BIOS-Stages von 0.11.15 bauen auf aktuellem Nightly nicht).
  Drei hart erkämpfte UEFI-Lektionen, alle im Code dokumentiert:
  (1) Nach dem GDT-Laden SS/DS/ES explizit neu setzen — sonst #GP beim
  ersten iretq (gdt.rs). (2) Den PIT selbst programmieren — UEFI tut es
  nicht (interrupts.rs). (3) PIC-Masken explizit setzen — OVMF übergibt
  alles maskiert; LAPIC deaktivieren für die Pre-APIC-Verdrahtung (lib.rs).
- **Theme-System (Juli 2026):** `src/theme.rs` = `Theme` (ALLE UI-Farben;
  zwei Instanzen: AURORA_DUNKEL Standard, AURORA_HELL) + `METRIK` (alle
  Abstände/Schriftgrößen, in beiden Themes gleich). Aktives Theme über
  AtomicBool, `theme::aktuell()` ist lockfrei (wird unter gehaltenen
  Locks im Compositor gerufen). SEITDEM GILT: KEINE hartcodierten Farben
  oder Abstände in UI-Code — alles über theme::aktuell()/METRIK.
  Wechsel via `fenster::theme_wechseln()` (schaltet um UND rendert alle
  Fenster-Inhalte neu). Das Terminal bleibt bewusst in beiden Themes
  dunkel (Shell-Farben sind auf dunklen Grund abgestimmt, Zellen-
  Hintergrund == Color::Black == theme.terminal_hintergrund).
- **Taskleiste & Startmenü (Juli 2026):** Der Compositor zeichnet die
  Taskleiste NACH den Fenstern (immer im Vordergrund), das Startmenü
  darüber; Klicks prüfen dieselbe Reihenfolge (Menü -> Leiste ->
  Fenster). Fenster-Knöpfe sind nach FensterId (= Erstellungsreihen-
  folge) sortiert, damit sie beim Fokuswechsel nicht springen; Klick =
  Fokus/Minimieren-Toggle. Uhr+Datum leitet `zeit::datum_nach` aus den
  Ticks ab (fester Boot-Zeitpunkt als Platzhalter — RTC/CMOS-Kalibrierung
  ist bekanntes TODO); neu komponiert wird nur beim Sekundenwechsel.
- **App-Registry (Juli 2026):** `src/apps.rs` — jede App = Name + Icon +
  `start: fn()`. Das Startmenü filtert die Liste live (Suchfeld, Basis
  der späteren Schnellsuche); Bedienung per Maus UND Tastatur (Tippen,
  Pfeile, Enter; Super-Taste öffnet — Abgriff im KeyStream wie Alt+Tab).
  WICHTIG (Deadlock-Regel): Start-Funktionen werden NIE unter dem
  MANAGER-Lock gerufen — Manager-Methoden geben die fn() als
  Rückgabewert nach draußen, die Wrapper in fenster/mod.rs führen sie
  nach dem Loslassen aus. Neue App = Start-Funktion + ein Eintrag in
  APPS, fertig.
- **Terminal-Fenster / Konsole-in-Fenster (Juli 2026):** SpeedOS bootet
  in den Desktop (main.rs ruft desktop_starten VOR dem Executor; die
  Shell druckt ihr Banner dann ins Terminal). Im Desktop-Modus leitet
  `konsole::_print` JEDE print!-Ausgabe ins Terminal-Fenster um
  (`fenster/terminal.rs` = reines, unit-getestetes Text-Raster mit
  Zellen/Cursor/Scrolling; Resize behält die UNTEREN Zeilen). Die
  serielle Doppel-Ausgabe bleibt unberührt. Gerendert wird GEBÜNDELT:
  terminal_schreiben setzt nur `inhalt_neu`, der Compositor ruft einmal
  pro Frame `inhalte_rendern()`. Tastatur-Routing: Ist das Terminal
  fokussiert, verarbeitet die Shell die Taste SELBST (ZeilenEditor),
  sonst geht sie ans Fenster; Startmenü-Tasten davor. clear leert das
  Raster, der Konsolen-Cursor bleibt im Desktop-Modus aus (Terminal
  zeichnet seinen eigenen). `shell::prompt_nachholen()` (cwd-Spiegel)
  druckt den Prompt, wenn die Terminal-App ein frisches Fenster öffnet.
- **Deadlock-Regeln:** (1) Ausgabe-Locks (WRITER, SERIAL1) werden nur mit
  deaktivierten Interrupts gehalten (`without_interrupts` in den _print-
  Funktionen). (2) Interrupt-Handler sind minimal: nie blockieren, nie
  allokieren, nie printen — Daten in lock-freie Queues, Verarbeitung in
  async Tasks (siehe Tastatur). (3) `fs::mit_fs()` nie verschachteln.
  (4) Lock-Ordnung KONSOLE -> FRAMEBUFFER -> MANAGER (die Terminal-
  Umleitung nimmt KONSOLE dann MANAGER, der Compositor FRAMEBUFFER dann
  MANAGER — nie andersherum). (5) App-Start-Funktionen nie unter dem
  MANAGER-Lock ausführen (siehe App-Registry).
- **Globale Speicher-API (Juli 2026):** Mapper und Frame-Allocator leben als
  globale `Mutex<Option<...>>` in `src/memory.rs` (Muster wie das VFS) —
  NICHT als Locals in kernel_main. Zugriff NUR über die API (map_page,
  map_page_zu für MMIO, unmap_page, allocate_pages, frame_allozieren/
  frame_freigeben, uebersetzen, frame_statistik). Beide Locks werden
  ausschließlich in `mit_speicher()` genommen (feste Reihenfolge: Mapper
  vor Frame-Allocator, Interrupts aus) — nie direkt.
- **Bitmap-Frame-Allocator (Juli 2026):** 1 Bit pro 4-KiB-Frame (statische
  32-KiB-Bitmap für max. 1 GiB RAM), Next-Fit-Zeiger. Bewusst KEINE
  Free-List: Die Bitmap findet zusammenhängende physische Bereiche
  (Framebuffer/DMA!) per Scan, kann O(1) freigeben, erkennt
  Doppel-Freigaben (assert) — eine Free-List kann Kontiguität praktisch
  nicht liefern. Freigegebene Frames setzen den Next-Fit-Zeiger zurück
  und werden sofort wiederverwendet.
- **Heap wächst zur Laufzeit:** `allocator::heap_erweitern(pages)` mappt
  neue Pages nahtlos ans Heap-Ende und ruft `extend` des Allocators.
  Alle drei Allocatoren (linked_list, Bump, Fixed-Block) unterstützen
  extend mit derselben Signatur. Kein automatisches Wachsen — bewusst
  manuell vor großen Puffern aufrufen.
- **Boot-/Init-Reihenfolge (main.rs):** GDT/TSS → IDT → PIC → Interrupts an
  → memory::init (globaler Mapper + Frame-Allocator) → Heap → Dateisystem
  → Executor + Shell.
  Statics mit einmaligem Seiteneffekt (Scancode-Queue) über conquer_once
  OnceCell explizit initialisieren, NICHT lazy_static (sonst passiert die
  Erst-Initialisierung womöglich im Interrupt-Kontext).
- **Multitasking kooperativ (async/await):** Eigener Executor
  (`src/task/executor.rs`) mit Waker-Support, FIFO-fair, schläft per
  hlt (race-frei via disable/enable_and_hlt). Tasks spawnen neue Tasks
  über `task::spawn()` (globale Spawn-Queue; NIE aus Interrupt-Handlern,
  denn Task::new alloziert). Tasks/Futures müssen `Send` sein.
  Volle Warteschlangen panicken NICHT: Überlauf setzt ein Notfall-Flag,
  die nächste Runde pollt alle Tasks — kein Wecken geht verloren.
  Kapazität konfigurierbar (`Executor::mit_kapazitaet`, Standard 128).
  Präemptiver Scheduler kommt erst mit User-Space-Prozessen.
- **Shell-Befehle als Registry:** Jeder Befehl = Struct mit `Befehl`-Trait
  (`src/shell/befehle.rs`, `Send + Sync`), eingetragen in `alle_befehle()`.
  Gemeinsamer Zustand (aktuelles Verzeichnis) nur über `ShellKontext`.
- **ZeilenEditor getrennt von der Anzeige (Juli 2026):** Die gesamte
  Eingabelogik (Tippen, Backspace, Verlauf, Tab) lebt in
  `src/shell/editor.rs`: Eingabe = eigenes `Taste`-Enum, Ausgabe =
  `Reaktion`-Enum (Anzeige-ANWEISUNGEN als Daten, der Editor druckt nie
  selbst). Tab-Kandidaten kommen über das `Vervollstaendiger`-Trait
  (Shell: VFS, Tests: Mock) — dadurch ist die Eingabelogik als reiner
  Unit-Test prüfbar. shell::run() ist nur noch Übersetzer:
  Taste rein, Reaktion zeichnen, fertige Zeilen an die Registry.
- **Zeit nur über `src/zeit.rs`:** ticks(), ms_seit_boot(). Niemals
  direkt den Tick-Zähler benutzen — die API-Naht erlaubt später den
  Wechsel PIT (~55 ms Auflösung) -> APIC-Timer ohne Aufrufer-Änderung.
- **Heap-Allocator austauschbar:** Standard linked_list_allocator; eigene
  Lern-Allocatoren (Bump, Fixed-Size-Block) über Cargo-Features
  `bump-allocator` / `fixed-block-allocator` — gleiche init-Schnittstelle.
- **unsafe-Politik:** Jede unsafe-Funktion dokumentiert ihre Bedingungen in
  einem `# Safety`-Abschnitt; jeder unsafe-Block hat einen Kommentar, WARUM
  er safe ist. `cargo clippy --all-targets` muss warnungsfrei sein.

## Bekannte Abweichungen vom blog_os-Buch
- (Historisch, seit der 0.11-Migration irrelevant: eigenes Target-JSON
  brauchte auf neuem Nightly `json-target-spec`, Zahlen statt Strings und
  `"rustc-abi": "softfloat"` — alles Geschichte, wir nutzen das eingebaute
  Target `x86_64-unknown-none`.)
