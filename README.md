# SpeedOS

Ein Betriebssystem **from scratch in Rust** — kein Linux, keine fremde
Kernel-Basis. Vom Bootsektor bis zur interaktiven Shell mit Dateisystem
ist alles selbst gebaut (auf Basis der bewährten Architektur aus
["Writing an OS in Rust"](https://os.phil-opp.com/) von Philipp Oppermann).

> Lernprojekt: Der Code ist bewusst ausführlich auf Deutsch kommentiert —
> jede Datei erklärt, *was* sie tut und *warum* es so funktioniert.

![SpeedOS-Desktop](docs/screenshots/desktop-komplett.png)
*SpeedOS bootet direkt in den Desktop: SpeedShell als Terminal-Fenster, Taskleiste mit
Startknopf/Fenster-Knöpfen/Uhr, Startmenü mit App-Registry und Live-Suche.*

![Aurora Hell](docs/screenshots/desktop-hell.png)
*Dasselbe System nach einem Klick auf "Theme wechseln": Aurora Hell — alle UI-Farben
kommen aus dem zentralen Theme-Modul, nur das Terminal bleibt bewusst dunkel.*

![Widget-Galerie](docs/screenshots/widget-galerie.png)
*Das UI-Toolkit (ui-Modul): Buttons, Checkbox, Textfeld (ZeilenEditor + blinkender
Cursor), ScrollListe mit Scrollbalken — das Fundament für Explorer & Co.*

![Fenster-Desktop](docs/screenshots/desktop-fenster.png)
*Fenster mit Titelleiste, Icon, Knöpfen (Minimieren/Maximieren/Schließen), Schatten und Aurora-Fokus.*

![Alt+Tab](docs/screenshots/alt-tab.png)
*Alt+Tab-Fensterwechsler mit zentriertem Overlay und Auswahl-Highlight.*

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
  Verlauf (Pfeiltasten), Tab-Vervollständigung und 19 Befehlen —
  läuft im Desktop als Terminal-FENSTER (Ausgabe-Umleitung in ein
  unit-getestetes Text-Raster), auf Wunsch (ESC) auch im Vollbild
- **Desktop:** Fenster-Manager + Compositor (private Fenster-Puffer,
  Dirty-Flags), Theme-System (Aurora Dunkel/Hell, zur Laufzeit
  umschaltbar — keine hartcodierten UI-Farben), Taskleiste
  (Startknopf, Fenster-Knöpfe, Uhr+Datum aus Ticks), Startmenü mit
  App-Registry und Live-Suche (Super-Taste), PS/2-Maus, Snap, Alt+Tab
- **Dateisystem:** RAM-Dateisystem hinter einer VFS-Abstraktion
  (Trait `FileSystem`) — vorbereitet für FAT32/Disk-Dateisysteme
- **Tests:** 60+ Unit-/Integrationstests, die als eigene Mini-Kernel
  in QEMU booten (inkl. Frame-Zeit-Messung, Speicherleck-Test und
  Clipping-Prüfung der Grafik-Schnellpfade)

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

### Auflösung wählen

SpeedOS ist auflösungsunabhängig — die Umgebungsvariable
`SPEEDOS_AUFLOESUNG` wählt den Grafikmodus (Standard: 720p, die
flotteste Wahl). Der Runner dimensioniert VRAM und Arbeitsspeicher
automatisch passend:

```
$env:SPEEDOS_AUFLOESUNG="4k"; cargo run        # PowerShell
SPEEDOS_AUFLOESUNG=1080p cargo run             # bash
```

![4K-Desktop](docs/screenshots/desktop-4k.png)
*Derselbe Desktop in 4096x2160 — der Kernel ist auflösungsunabhängig.*

Presets: `720p`, `1080p`, `1200p`, `2k`/`1440p`, `1600p`, `4k`,
`5k`, `8k` — oder frei als `BREITExHOEHE` (z. B. `1600x900`).
1080p und 4K treffen exakt; dazwischen nimmt die Firmware den
nächstgrößeren Modus ihrer Tabelle (720p → 1360x768, 2k → 2560x1600).
5K/8K versteht der Runner, deckelt sie aber ehrlich auf das Maximum
der QEMU-VGA-Firmware (4096x2160) — der Kernel selbst könnte sie
darstellen.


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
- [x] **Desktop:** Maus, Zeichen-Werkzeuge, Fenster mit Compositor,
      Titelleisten, Verschieben/Größe/Min/Max/Close, Snap, Alt+Tab
- [x] **Desktop komplett:** Theme-System (Aurora Dunkel/Hell, zur
      Laufzeit umschaltbar), Taskleiste mit Startknopf/Fenster-Knöpfen/
      Uhr+Datum, Startmenü mit App-Registry und Live-Suche (Super-Taste),
      SpeedShell als Terminal-Fenster — SpeedOS bootet in den Desktop
- [ ] **Persistenz:** Block-Device-Treiber + Disk-Dateisystem (VFS ist bereit)
- [ ] **User Space:** Ring-3-Prozesse, Syscalls, präemptiver Scheduler
- [ ] Ferner: eigene Programme laden (ELF), Netzwerk, Sound

## Lizenz

Lernprojekt — Code frei verwendbar (MIT).
