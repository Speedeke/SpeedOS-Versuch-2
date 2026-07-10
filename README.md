# SpeedOS

Ein Betriebssystem **from scratch in Rust** — kein Linux, keine fremde
Kernel-Basis. Vom Bootsektor bis zur interaktiven Shell mit Dateisystem
ist alles selbst gebaut (auf Basis der bewährten Architektur aus
["Writing an OS in Rust"](https://os.phil-opp.com/) von Philipp Oppermann).

> Lernprojekt: Der Code ist bewusst ausführlich auf Deutsch kommentiert —
> jede Datei erklärt, *was* sie tut und *warum* es so funktioniert.

![Boot-Screen](docs/screenshots/bootscreen.png)
*Der Boot-Screen: Obsidian-Aurora-Farbverlauf, gerendert Pixel für Pixel.*

![SpeedShell](docs/screenshots/shell.png)
*Die SpeedShell auf der Framebuffer-Konsole: help, farbtest, blinkender Cursor.*

## Features (Stand: Juli 2026)

- **Eigenständiger Boot:** `no_std`-Kernel (Target `x86_64-unknown-none`),
  UEFI-Boot über bootloader_api 0.11, startet in QEMU
- **Grafik-Konsole:** linearer Framebuffer (1280x720) mit Double
  Buffering, vorgerastertem Noto-Sans-Mono-Font (Antialiasing,
  Umlaute!), Scrolling per memmove, blinkendem Software-Cursor und
  Obsidian-Aurora-Boot-Screen — alle Ausgaben laufen zusätzlich mit
  ANSI-Farben über die serielle Schnittstelle
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
# Rust nightly — Komponenten und Targets installiert rust-toolchain.toml
# im Projektordner automatisch beim ersten cargo-Aufruf.
rustup toolchain install nightly

# QEMU (Windows: https://qemu.weilnetz.de/w64/ oder winget install
# SoftwareFreedomConservancy.QEMU) — qemu-system-x86_64 im PATH oder
# unter C:\Program Files\qemu (die mitgelieferte edk2/OVMF-Firmware
# wird für den UEFI-Boot gebraucht).
```

Dann:

```
cargo run     # baut Kernel + Disk-Image und startet SpeedOS im QEMU-Fenster
cargo test    # führt alle Tests in QEMU aus (headless)
```

Hinter den Kulissen ruft cargo unser `boot/`-Programm als Runner auf:
Es verpackt den Kernel mit `bootloader::UefiBoot` in ein bootfähiges
GPT-Image und startet QEMU. Der allererste Build kompiliert dabei
einmalig die Bootloader-Stages (ein paar Minuten).


Alternative Heap-Allocatoren zum Experimentieren:

```
cargo test --features bump-allocator          # Bump: schnell, vergisst nichts
cargo test --features fixed-block-allocator   # Frei-Listen fester Größen
```

## Projektstruktur

```
src/
├── main.rs          Kernel-Einstieg: Init-Reihenfolge, Framebuffer, Shell
├── lib.rs           Kern-Bibliothek: init(), Test-Framework, print-Makros
├── framebuffer.rs   Double Buffering, Font-Rendering, Boot-Screen
├── konsole.rs       FramebufferKonsole: Raster, Farben, Blink-Cursor
├── serial.rs        Serielle Ausgabe (COM1), parallel zum Bildschirm
├── gdt.rs           GDT/TSS + Notfall-Stack für Double Faults
├── interrupts.rs    IDT, Exceptions, PIC/PIT/LAPIC, Timer & Tastatur
├── memory.rs        Paging: globale API + Bitmap-Frame-Allocator
├── allocator.rs     Kernel-Heap (+ allocator/{bump,fixed_size_block}.rs)
├── zeit.rs          Zeit-API (ticks, ms_seit_boot)
├── task/            Async-Multitasking: Task, Executor, Tastatur-Stream
├── shell/           SpeedShell: ZeilenEditor + Befehls-Registry
└── fs/              VFS-Trait + RamFs
boot/                Host-Runner: baut das UEFI-Disk-Image, startet QEMU
tests/               Integrationstests (booten einzeln in QEMU)
docs/                Migrationsplan bootloader 0.9 -> 0.11
```

## Roadmap (Kurzfassung)

- [x] Boot, Seriell, Exceptions, Interrupts, Tastatur (QWERTZ)
- [x] Paging, Heap, async/await, Shell, RAM-Dateisystem
- [x] **UEFI-Boot mit linearem Framebuffer** (bootloader 0.11)
- [x] **Grafik-Konsole:** Font-Rendering, Double Buffering, Boot-Screen
- [ ] **Persistenz:** Block-Device-Treiber + Disk-Dateisystem (VFS ist bereit)
- [ ] **User Space:** Ring-3-Prozesse, Syscalls, präemptiver Scheduler
- [ ] Ferner: eigene Programme laden (ELF), Netzwerk, Sound

## Lizenz

Lernprojekt — Code frei verwendbar (MIT).
