# SpeedOS

Ein Betriebssystem **from scratch in Rust** — kein Linux, keine fremde
Kernel-Basis. Vom Bootsektor bis zur interaktiven Shell mit Dateisystem
ist alles selbst gebaut (auf Basis der bewährten Architektur aus
["Writing an OS in Rust"](https://os.phil-opp.com/) von Philipp Oppermann).

> Lernprojekt: Der Code ist bewusst ausführlich auf Deutsch kommentiert —
> jede Datei erklärt, *was* sie tut und *warum* es so funktioniert.

![SpeedShell mit Banner](docs/screenshots/shell.png)
*<!-- Screenshot-Platzhalter: SpeedShell nach dem Boot (Banner + Prompt) -->*

![Dateisystem-Befehle](docs/screenshots/dateisystem.png)
*<!-- Screenshot-Platzhalter: dir, cd, type und tree in Aktion -->*

## Features (Stand: Juli 2026)

- **Eigenständiger Boot:** `no_std`-Kernel, eigenes Target
  (`x86_64-speedos.json`), bootloader-Crate 0.9.x, startet in QEMU
- **Textausgabe:** VGA-Textmodus-Treiber mit Farben, Scrolling,
  CP437-Umlauten (ä ö ü ß) und blinkendem Hardware-Cursor; alle
  Ausgaben laufen parallel über die serielle Schnittstelle (COM1)
- **Absturzsicherheit:** IDT mit Handlern für Breakpoint, Page Fault
  und Double Fault — letzterer mit eigenem Notfall-Stack (IST/TSS),
  sodass selbst ein Kernel-Stack-Overflow sauber gemeldet wird
- **Hardware-Interrupts:** 8259 PIC (remappt), Timer-Ticks,
  PS/2-Tastatur mit deutschem QWERTZ-Layout
- **Speicherverwaltung:** Paging über OffsetPageTable,
  Frame-Allocator aus der Bootloader-Memory-Map, 100-KiB-Kernel-Heap
  mit drei wählbaren Allocatoren (linked_list, Bump, Fixed-Size-Block)
  → `Box`, `Vec`, `String`, `BTreeMap` funktionieren im Kernel
- **Multitasking:** kooperativ mit async/await — eigener Executor mit
  Waker-Support, lock-freien Task-Queues und `hlt`-Schlaf im Leerlauf
- **SpeedShell:** interaktive Kommandozeile mit Befehls-Registry,
  Verlauf (Pfeiltasten), Tab-Vervollständigung und 17 Befehlen
  (`help`, `echo`, `clear`, `ticks`, `meminfo`, `version`, `farbtest`,
  `neustart`, `dir`, `cd`, `mkdir`, `type`, `write`, `del`, `copy`,
  `move`, `tree`)
- **Dateisystem:** RAM-Dateisystem hinter einer VFS-Abstraktion
  (Trait `FileSystem`) — vorbereitet für FAT32/Disk-Dateisysteme
- **Tests:** 30 Integrationstests, die als eigene Mini-Kernel in QEMU
  booten und QEMU mit Erfolgs-/Fehlercode beenden

## Bauen & Starten

Voraussetzungen (einmalig):

```
# Rust nightly mit den nötigen Komponenten (rust-toolchain.toml
# wählt sie im Projektordner automatisch):
rustup toolchain install nightly --component rust-src --component llvm-tools-preview

# bootimage baut aus dem Kernel ein bootfähiges Image:
cargo install bootimage

# QEMU (Windows: https://qemu.weilnetz.de/w64/ oder winget install
# SoftwareFreedomConservancy.QEMU) — qemu-system-x86_64 muss im PATH sein.
```

Dann:

```
cargo run     # baut alles und startet SpeedOS im QEMU-Fenster
cargo test    # führt alle Tests in QEMU aus (headless)
```

Beim ersten Build kompiliert cargo die `core`-/`alloc`-Bibliotheken für
unser eigenes Target mit — das dauert einmalig ein paar Minuten.

Alternative Heap-Allocatoren zum Experimentieren:

```
cargo test --features bump-allocator          # Bump: schnell, vergisst nichts
cargo test --features fixed-block-allocator   # Frei-Listen fester Größen
```

## Projektstruktur

```
src/
├── main.rs          Kernel-Einstieg: Init-Reihenfolge, startet die Shell
├── lib.rs           Kern-Bibliothek: init(), Test-Framework, print-Makros
├── vga_buffer.rs    VGA-Textmodus (Farben, CP437, Scrolling, Cursor)
├── serial.rs        Serielle Debug-Ausgabe (COM1)
├── gdt.rs           GDT/TSS + Notfall-Stack für Double Faults
├── interrupts.rs    IDT, Exception- und Hardware-Interrupt-Handler
├── memory.rs        Paging: OffsetPageTable + Frame-Allocator
├── allocator.rs     Kernel-Heap (+ allocator/{bump,fixed_size_block}.rs)
├── task/            Async-Multitasking: Task, Executor, Tastatur-Stream
├── shell/           SpeedShell: Eingabe-Loop + Befehls-Registry
└── fs/              VFS-Trait + RamFs
tests/               Integrationstests (booten einzeln in QEMU)
```

## Roadmap (Kurzfassung)

- [x] Boot, VGA/Seriell, Exceptions, Interrupts, Tastatur (QWERTZ)
- [x] Paging, Heap, async/await, Shell, RAM-Dateisystem
- [ ] **Grafik:** Framebuffer statt VGA-Textmodus, eigener Text-Renderer
- [ ] **Persistenz:** Block-Device-Treiber + Disk-Dateisystem (VFS ist bereit)
- [ ] **User Space:** Ring-3-Prozesse, Syscalls, präemptiver Scheduler
- [ ] Ferner: eigene Programme laden (ELF), Netzwerk, Sound

## Lizenz

Lernprojekt — Code frei verwendbar (MIT).
