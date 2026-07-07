# SpeedOS — Projektregeln für Claude

## Projekt
- SpeedOS: ein eigenes Betriebssystem from scratch in Rust. Kein Linux, keine fremde Kernel-Basis.
- Sprache: Rust **nightly**, `no_std`, Ziel-Architektur: **x86_64**.
- Bootloader: das `bootloader`-Crate (**Version 0.9.x**), kompatibel mit dem Buch
  "Writing an OS in Rust" von Philipp Oppermann — wir folgen dessen bewährter Architektur.

## Build & Test-Umgebung
- Test-Umgebung: **QEMU** (`qemu-system-x86_64`), gestartet über `cargo run` mittels `bootimage`.
- Eigenes Target-JSON: `x86_64-speedos.json`.
- Tests: Integrationstests laufen in QEMU mit dem **isa-debug-exit Device** für
  automatisches Beenden mit Exit-Code (`cargo test`).

## Debug
- **ALLE** Debug-Ausgaben gehen über die serielle Schnittstelle (COM1 / Port 0x3F8)
  ins Terminal, zusätzlich zur VGA-Ausgabe. **Niemals nur VGA.**

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
- **Ausgabe immer doppelt:** `print!`/`println!` (lib.rs) schreiben IMMER auf
  VGA UND seriell — die Projektregel ist in die Makros eingebaut, nicht dem
  Aufrufer überlassen. `serial_println!` nur für reine Debug-Ausgaben.
- **Deadlock-Regeln:** (1) Ausgabe-Locks (WRITER, SERIAL1) werden nur mit
  deaktivierten Interrupts gehalten (`without_interrupts` in den _print-
  Funktionen). (2) Interrupt-Handler sind minimal: nie blockieren, nie
  allokieren, nie printen — Daten in lock-freie Queues, Verarbeitung in
  async Tasks (siehe Tastatur). (3) `fs::mit_fs()` nie verschachteln.
- **Boot-/Init-Reihenfolge (main.rs):** GDT/TSS → IDT → PIC → Interrupts an
  → Paging (OffsetPageTable) → Heap → Dateisystem → Executor + Shell.
  Statics mit einmaligem Seiteneffekt (Scancode-Queue) über conquer_once
  OnceCell explizit initialisieren, NICHT lazy_static (sonst passiert die
  Erst-Initialisierung womöglich im Interrupt-Kontext).
- **Multitasking kooperativ (async/await):** Eigener Executor
  (`src/task/executor.rs`) mit Waker-Support, FIFO-fair, schläft per
  hlt (race-frei via disable/enable_and_hlt). Präemptiver Scheduler kommt
  erst mit User-Space-Prozessen.
- **Shell-Befehle als Registry:** Jeder Befehl = Struct mit `Befehl`-Trait
  (`src/shell/befehle.rs`), eingetragen in `alle_befehle()`. Gemeinsamer
  Zustand (aktuelles Verzeichnis) nur über `ShellKontext`.
- **Heap-Allocator austauschbar:** Standard linked_list_allocator; eigene
  Lern-Allocatoren (Bump, Fixed-Size-Block) über Cargo-Features
  `bump-allocator` / `fixed-block-allocator` — gleiche init-Schnittstelle.
- **unsafe-Politik:** Jede unsafe-Funktion dokumentiert ihre Bedingungen in
  einem `# Safety`-Abschnitt; jeder unsafe-Block hat einen Kommentar, WARUM
  er safe ist. `cargo clippy --all-targets` muss warnungsfrei sein.

## Bekannte Abweichungen vom blog_os-Buch (aktueller Nightly)
- `.cargo/config.toml` braucht `json-target-spec = true` unter `[unstable]`.
- Target-JSON: `target-pointer-width`/`target-c-int-width` als Zahlen,
  `"rustc-abi": "softfloat"` ist wegen `+soft-float` Pflicht.
