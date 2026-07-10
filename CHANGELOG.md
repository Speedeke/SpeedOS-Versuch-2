# Changelog

Alle nennenswerten Änderungen an SpeedOS, neueste zuerst.
Format angelehnt an [Keep a Changelog](https://keepachangelog.com/de/).

## [Unveröffentlicht]

### Hinzugefügt (PS/2-Maus)
- Maus-Treiber am zweiten 8042-Port: vorsichtige Controller-Init
  (Tastatur-Bits unangetastet, alles mit Timeouts), IntelliMouse-
  Scrollrad (200/100/80-Magie, 4-Byte-Pakete)
- Reines, unit-getestetes Paket-Parsing (Sync, 9-Bit-Vorzeichen,
  Overflow, Tasten, 4-Bit-Rad)
- IRQ 12 -> lock-freie Queue -> async maus_task; globaler Zustand
  (geklemmte Position, Tasten) und MausEvent-Typ (Bewegt/Gedrueckt/
  Losgelassen/Gescrollt)
- Pfeil-Cursor als Front-Buffer-Overlay (Back-Buffer bleibt
  cursorfrei; Wiederherstellen = present_bereich)
- grafiktest interaktiv: Klick malt Punkte, Rad wechselt die
  Malfarbe (Anzeige unten rechts)

### Hinzugefügt (Zeichen-Werkzeuge: grafik.rs)
- Zeichner auf dem Back-Buffer: Linien (Bresenham), Rechtecke
  (gefüllt/Rahmen/abgerundet), Kreise (gefüllt/Midpoint-Rahmen),
  vertikale Farbverläufe, Text an Pixelposition (per Intensität
  auf jeden Untergrund geblendet), Bitmap-Blitting mit
  Transparenz-Farbe
- Rgba-Farben mit Alpha-Blending (reine, unit-getestete Formel)
- Clipping: optionales Clip-Rechteck für alle Operationen
  (Schnitt-Mathematik unit-getestet)
- Eingebettetes Icon-Format (16x16-ASCII-Art + Palette) mit vier
  Icons: Ordner, Datei, Zahnrad, SpeedOS-Logo
- Shell-Befehl grafiktest: zeigt alle Primitive, beliebige Taste
  kehrt zur Konsole zurück; framebuffer::pixel_lesen für Alpha

### Hinzugefügt (Framebuffer-Konsole — die Konsole ist zurück!)
- framebuffer.rs: Double Buffering (Back-Buffer aus allocate_pages,
  present() als Block-Kopie), Pixel-/Text-Primitive mit Antialiasing
  (Farbmischung), hochscrollen() als memmove im Back-Buffer
- Font: noto-sans-mono-bitmap (vorgerastert, pure Rust, Latin-1 =
  Umlaute ä ö ü ß; Größe 16 Konsole, 32 fett für den Boot-Screen)
- konsole.rs ist jetzt die FramebufferKonsole: Zeichenraster,
  Zeilenumbruch, Scrolling, Farben pro Zeichen (Obsidian-Aurora-
  Palette) — print!/println! schreiben wieder DOPPELT (Bildschirm +
  seriell mit ANSI), Shell und Makros blieben unberührt (die Naht!)
- Software-Cursor: blinkt über einen async Task; dafür zeit::warte_ms
  mit Tick-Future (Timer-Interrupt weckt per AtomicWaker — kein
  Busy-Polling, die CPU schläft zwischen den Blinks)
- Boot-Screen: Obsidian-Hintergrund, Aurora-Farbverlauf
  (violett-blau-cyan), SpeedOS-Schriftzug in Größe 32 fett
- QEMU-Auflösung per EDID auf 1280x720 gewünscht (Runner)

### Geändert (Migration auf bootloader 0.11 — UEFI + Framebuffer)
- Kernel bootet jetzt per UEFI (edk2/OVMF) und bekommt einen linearen
  Framebuffer vom Bootloader (QEMU: 2560x1600 BGR) — Grundlage für
  alles Grafische; beim Boot wird er mit SpeedOS-Blau gefüllt und
  seine Eckdaten seriell geloggt
- Build-System: eingebautes Target x86_64-unknown-none (Target-JSON,
  build-std, bootimage entfallen); neues boot/-Crate als Runner baut
  das UEFI-Disk-Image und startet QEMU (inkl. Test-Exit-Codes)
- VGA-Textmodus ist Geschichte: vga_buffer.rs gelöscht; konsole.rs
  liefert die gewohnte Farb-API übergangsweise als ANSI-Codes über
  die serielle Leitung (Shell voll benutzbar im Terminal)
- Drei UEFI-Lektionen gefixt und dokumentiert: SS/DS/ES nach
  GDT-Laden neu setzen (sonst #GP bei iretq), PIT selbst
  programmieren, PIC-Masken selbst setzen + LAPIC deaktivieren
- Migrationsplan in docs/migration-011.md

### Geändert (Shell-Editor, Executor, Zeit-API)
- ZeilenEditor aus shell::run() extrahiert (shell/editor.rs):
  Eingabepuffer, Verlauf und Tab-Vervollständigung als reine Logik
  mit Taste-/Reaktion-Enums und Vervollstaendiger-Trait — komplett
  per Unit-Test abgedeckt (8 Tests mit Mock, ohne Tastatur/VGA/VFS)
- Executor: task::spawn() erlaubt Tasks, selbst Tasks zu starten
  (globale Spawn-Queue); volle Warteschlangen panicken nicht mehr
  (Überlauf-Flag -> alle Tasks einmal pollen, kein Wecken geht
  verloren); Queue-Kapazität konfigurierbar (mit_kapazitaet)
- Neue Zeit-API src/zeit.rs: ticks(), ms_seit_boot(), ms_von_ticks()
  — zentrale Naht für den späteren Wechsel auf den APIC-Timer;
  ticks-Befehl zeigt jetzt die Uptime in Sekunden

### Geändert (Speicherverwaltung generalüberholt)
- Mapper + Frame-Allocator sind jetzt global (`Mutex<Option<...>>` in
  memory.rs, Muster wie das VFS) — jedes Modul kann zur Laufzeit Pages
  mappen. Neue API: map_page, map_page_zu (MMIO), unmap_page,
  allocate_pages (virtuell UND physisch zusammenhängend, für
  Framebuffer/DMA), frame_allozieren/frame_freigeben, uebersetzen,
  frame_statistik
- BootInfoFrameAllocator (O(n²), konnte nie freigeben) ersetzt durch
  Bitmap-Frame-Allocator: Next-Fit-Suche, O(1)-Freigabe mit
  Doppel-Freigabe-Erkennung, freigegebene Frames werden sofort
  wiederverwendet, zusammenhängende Allokation möglich
- Heap wächst zur Laufzeit: allocator::heap_erweitern(pages) —
  alle drei Allocatoren unterstützen extend
- meminfo zeigt jetzt auch die physische Frame-Statistik
- 5 neue Tests (Frame-Wiederverwendung, Kontiguität, map/unmap-
  Lebenszyklus, map_page_zu, Heap-Erweiterung via try_reserve)

### Verbessert
- Qualitäts-Pass: alle Clippy-Lints behoben, `# Safety`-Dokumentation
  für alle unsafe-Funktionen, Begründungs-Kommentare an jedem
  unsafe-Block, README/CHANGELOG ergänzt

## 0.1.0 — Meilensteine bis Juli 2026

### RAM-Dateisystem mit VFS (a55873d)
- `FileSystem`-Trait als VFS-Abstraktion (lesen, schreiben, liste,
  mkdir, loeschen, node_typ) — vorbereitet für FAT32/Disk-Dateisysteme
- RamFs: hierarchisches In-Memory-Dateisystem (BTreeMap-Baum)
- 9 neue Shell-Befehle: dir, cd, mkdir, type, write, del, copy, move,
  tree; Prompt zeigt aktuelles Verzeichnis; Tab-Vervollständigung
- Demo-Dateien beim Boot (/willkommen.txt, /system/info.txt)

### SpeedShell (bc9c0b7)
- Interaktive Shell als async Task: Prompt mit blinkendem
  Hardware-Cursor, Zeileneingabe mit Backspace/Entf, Befehlsverlauf
  (10 Einträge, Pfeiltasten)
- Befehls-Registry über das `Befehl`-Trait: help, echo, clear, ticks,
  meminfo, version, farbtest, neustart

### Kooperatives Multitasking (555fdc5)
- Task-Typ um Futures, Executor mit Waker-Support (fair, FIFO,
  hlt im Leerlauf, race-freies sleep_if_idle)
- Tastatur auf async umgestellt: Interrupt-Handler füllt nur noch
  eine lock-freie Queue (crossbeam ArrayQueue + AtomicWaker)

### Kernel-Heap (759d7f6)
- 100-KiB-Heap ab 0x4444_4444_0000; alloc-Crate aktiviert
  (Box, Vec, String, BTreeMap im Kernel)
- Drei Allocatoren: linked_list (Standard) sowie Bump und
  Fixed-Size-Block als dokumentierte Lern-Alternativen (Features)

### Paging (e4186a0)
- Bootloader mappt den physischen Speicher komplett
  (map_physical_memory); OffsetPageTable über CR3
- BootInfoFrameAllocator vergibt Frames aus der Memory Map
- entry_point!-Makro für typgeprüfte Kernel-Einstiege

### Tastatur, Backspace & Entf (41fa008, 8ae2b30)
- 8259 PIC remappt (32-47), Timer-Tick-Zähler, PS/2-Tastatur mit
  deutschem QWERTZ-Layout (De105Key) inkl. Umlauten
- Deadlock-Schutz: Ausgabe-Locks nur mit deaktivierten Interrupts
- Backspace/Entf löschen (VGA + seriell konsistent)

### Exception-Handling (41678ec)
- IDT mit Breakpoint-, Page-Fault- und Double-Fault-Handler
- GDT/TSS mit Interrupt Stack Table: Double Fault läuft auf eigenem
  Notfall-Stack — Stack Overflow führt zu sauberer Meldung statt
  Triple Fault + Reboot

### VGA-Treiber (3c9c0f8)
- println!/print! schreiben immer auf VGA UND seriell
- Codepage-437-Übersetzung (ä ö ü Ä Ö Ü ß é è ° § ² …),
  Ersatzzeichen für Nicht-Darstellbares, Farb-API, Scrolling-Fix

### Grundgerüst (6c877b6)
- no_std-Kernel mit eigenem Entry Point und Panic-Handler
- Eigenes Target x86_64-speedos.json, bootimage + QEMU-Workflow
- Serieller Port (COM1) als Debug-Kanal
- Eigenes Test-Framework: Tests booten in QEMU, isa-debug-exit
  liefert den Exit-Code
