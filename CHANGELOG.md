# Changelog

Alle nennenswerten Änderungen an SpeedOS, neueste zuerst.
Format angelehnt an [Keep a Changelog](https://keepachangelog.com/de/).

## [Unveröffentlicht]

### Geändert (Toolkit-Beweis: App-Trait, Startmenü + Alt+Tab umgezogen)
- trait ui::App (name/icon/aufbau/nachricht/tick) + Inhalt::App als
  Brücke vom Enum zum Trait — jede NEUE App implementiert das Trait
  (Enum bleibt für Terminal/Demos); fenster::app_starten öffnet
  Trait-Apps, die Widget-Galerie ist die erste (Nachrichten laufen
  über App::nachricht statt fn-Handler; Lock-Regel in ui/app.rs)
- Startmenü aufs Toolkit umgezogen: Suchfeld = Textfeld-Widget (neue
  Änderungs-Nachricht für den Live-Filter), App-Liste = ScrollListe;
  Panel zeichnet in einen Offscreen-Puffer, der Compositor blittet
  (Overlay-Muster). Verhalten unverändert: Super-Taste, Klick
  außerhalb schließt, Tippen filtert live, Pfeile + Enter, Klick
  startet — per QMP-Screenshot-Vergleich verifiziert
- Alt+Tab-Switcher ebenso: ScrollListe mit Auswahl-Highlight statt
  handgemalter Liste (auswahl_bewegen mit Wrap-Around)
- Aufräum-Bilanz der Event-Kaskade: maus_gedrueckt 61 -> 55 Zeilen;
  handgestrickter Startmenü-Sondercode (Tasten-Kaskade, Eintrags-
  Geometrie, Hover-Pflege, Panel-Malerei) 161 -> 69 Zeilen
  Widget-Anbindung; switcher_zeichnen (46 Zeilen) ersatzlos
- NachLock::AppStarten -> NachLock::Ausfuehren (traegt jetzt auch
  AppReaktion.danach-Aktionen)

### Hinzugefügt (UI-Widget-Toolkit: das Fundament aller Apps)
- Neues Modul src/ui/: retained Widget-Baum mit `trait Widget`
  (wunschgroesse/zeichnen/ereignis), UiEreignis (Klick, Doppelklick,
  Losgelassen, Bewegt, Scroll, Taste, MausRein/Raus, FokusRein/Raus)
  und UiReaktion (verbraucht + neu_zeichnen + Nachricht kombinierbar);
  App-Nachrichten als u32-ID an fn(u32)-Handler (Begründung im
  Kopfkommentar: keine Closures/Generics im no_std-Kernel)
- Hover-Konzept: das Container-Routing erzeugt MausRein/MausRaus
  (Fenster-Ebene: ui_hover_fenster im Manager, MausRaus beim
  Fensterwechsel); Fokus-Kette pro Fenster mit Tab (Wrap-Around),
  Tasten laufen zum fokussierten Widget
- Layout bewusst primitiv: laengen_verteilen (pure) + VBox/HBox mit
  METRIK-Abständen und Fueller (flex) — kein Constraint-Solver
- Widgets im Aurora-Stil: Label (mehrzeilig), Trennlinie, Button
  (Hover/Pressed, Icon optional, feuert beim Loslassen im Bereich),
  Checkbox, Textfeld (Innenleben = ZeilenEditor der Shell, Cursor
  blinkt über die zeit-API), ScrollListe (Scrollrad, ziehbarer
  Scrollbalken, Auswahl-Highlight, Doppelklick-Nachricht)
- Fenster-Integration: Inhalt::Ui, Maus-Routing inkl. Scroll und
  Losgelassen ins Fenster, NachLock-Enum (Ui-Nachrichten laufen wie
  App-Starts nie unter dem MANAGER-Lock); Panic-Handler meldet
  zuerst roh seriell (Deadlock-Fix bei Panik unter dem Lock)
- Widget-Galerie als Demo-App im Startmenü: zeigt alle Widgets,
  loggt jede Interaktion seriell — in QEMU end-to-end bedient
  (Tab-Fokus, Tippen, Button-Klick per Maus, Scroll, Doppelklick)
- 10 neue Unit-Tests: Layout-Verteilung, Klick-Routing,
  Hover-Enter/Leave, Fokus-Kette mit Tab, Doppelklick-Erkennung,
  Scroll-Klemmen, ScrollListe-Sichtbereich/Auswahl, Button-Zustände,
  Textfeld+Checkbox

### Hinzugefügt (Echte Zeit: TSC-Zeitquelle + RTC-Uhrzeit)
- TSC als monotone Zeitquelle: zeit::init() kalibriert den TSC beim
  Boot gegen den PIT (2 Messungen à ~100 ms, loggt Frequenz,
  Abweichung in Promille, Dauer und den CPUID-Invariant-Status);
  us_seit_boot()/ms_seit_boot() laufen seitdem über den TSC —
  mikrosekundengenau und unabhängig von Interrupts (kein Uhr-
  Stillstand mehr unter without_interrupts). Der PIT ist nur noch
  Weckgeber für warte_ms/Executor und Fallback vor der Kalibrierung
- Neues Modul rtc.rs: liest die CMOS-Echtzeituhr einmal beim Boot
  (Update-in-Progress-Flag, BCD- und 12h-Modus, Doppel-Lesen bis
  stabil, Timeout bei fehlender RTC); zeit::jetzt() liefert daraus
  plus TSC echte Uhrzeit und echtes Datum — die Taskleiste zeigt
  jetzt die WIRKLICHE Zeit (QEMU-RTC via -rtc base=localtime auf
  der Host-Uhr). Datums-Arithmetik auf "Sekunden seit 1.1.2000"
  umgestellt (reine, roundtrip-getestete Funktionen)
- Die Mess-Falle aus dem Qualitäts-Pass ist damit tot: Zeit darf
  überall genommen werden, der Frame-Zeit-Berichts-Test misst in µs
- 6 neue Tests: TSC-Kalibrierung plausibel (100-ms-Messung ±20 %),
  Uhr läuft unter without_interrupts weiter, BCD-Konvertierung,
  RTC-Konvertierung (BCD/24h und binär/12h inkl. Mitternacht),
  Datums-Roundtrip inkl. Schalttag

### Geändert (Qualitäts-Pass Grafik-Schicht)
- Zeilen-Schnellpfade in der Zeichenflaeche: flaeche_zeile_fuellen /
  flaeche_zeile_kopieren (Default korrekt pro Pixel; DoppelPuffer
  per Muster-Verdopplung/Formatwandlung einmal pro Zeile,
  FensterPuffer per slice fill/copy). Der Zeichner clippt Rechtecke
  VORAB (sichtbar = Rechteck ∩ Clip ∩ Fläche) — rechteck_fuellen
  (deckend), verlauf_vertikal und der neue puffer_blit laufen ohne
  Pro-Pixel-Prüfungen; der Compositor blittet Fenster-Inhalte damit
- Gemessen (1360x768, 3 Fenster + Drag, WHPX): 2,30 -> 1,20 ms/Frame
  im Erstlauf, eingeschwungen 0,40 ms/Frame; isolierter Fenster-Blit
  (560x140, 100x): 25,8 -> 4,4 ms (Faktor ~6; präzise nachgemessen
  mit der späteren TSC-µs-Uhr)
- 8 neue Unit-Tests: Koordinaten-Grenzen, Resize-Zonen, Z-Ordnung,
  Dirty-Flag-Logik, Theme-Umschaltung färbt Puffer, Schnellpfad-
  Clipping, Speicherleck-Schleife (Fenster+Terminal öffnen/schließen),
  Frame-Zeit-Messung (Berichts-Test)
- Speicher-Pass: kein Leck gefunden (Heap-Belegung nach 30 Zyklen
  Fenster+Terminal öffnen/schließen unverändert)
- unsafe-Audit: Serie 2 (Theme, Taskleiste, Startmenü, Terminal,
  Schnellpfade) kommt komplett OHNE neue unsafe-Blöcke aus

### Hinzugefügt (Auflösungswahl 720p bis 4K)
- SPEEDOS_AUFLOESUNG wählt den Grafikmodus (720p Standard, 1080p,
  1200p, 2k, 1600p, 4k, 5k, 8k oder frei BREITExHOEHE) — der Runner
  leitet VRAM (vgamem_mb als Modus-Wähler, da OVMF den EDID-Wunsch
  ignoriert und immer den größten passenden Modus nimmt) und
  Arbeitsspeicher (-m, ~20 B/Pixel + Grundbedarf) automatisch ab
- 1080p und 4K treffen exakt; 5K/8K werden ehrlich auf das
  Firmware-Maximum 4096x2160 gedeckelt (5K fehlt in der
  edk2-Modustabelle, 8K lässt die Firmware hängen) — der Kernel
  selbst ist auflösungsunabhängig (in 4K end-to-end geprüft)

### Geändert (Performance gegen Maus-/Desktop-Lag)
- QEMU mit Hardware-Virtualisierung (-accel whpx, TCG-Fallback)
- Kernel baut im dev-Profil mit opt-level 2 (Debug-Checks bleiben)
- Standard-Auflösung 1360x768 statt 2560x1600 (4x weniger Pixel)
- PIT von ~18,2 Hz auf 250 Hz (zeit::PIT_TEILER zentral) — 4-ms-
  statt 55-ms-Auflösung für warte_ms, Compositor erreicht ~33 FPS
- Maus-Abtastrate nach IntelliMouse-Init auf 200/s (statt 80/s)

### Hinzugefügt (Desktop komplett: Theme, Taskleiste, Startmenü, Terminal)
- Theme-System (src/theme.rs): ALLE UI-Farben in einer zentralen
  Struktur, zwei Themes "Aurora Dunkel" (Standard) und "Aurora Hell",
  zur Laufzeit umschaltbar (alle Fenster werden neu gezeichnet);
  METRIK bündelt alle Abstände und Schriftgrößen — keine
  hartcodierten Farben mehr im UI-Code
- Taskleiste (40px, unten, immer im Vordergrund): SpeedOS-Startknopf,
  ein Knopf pro Fenster (Icon + Titel, fokussiert hervorgehoben,
  Klick = Fokus/Minimieren-Toggle, stabile Reihenfolge), Systray mit
  Platzhalter-Icons und Uhrzeit + Datum (aus Ticks abgeleitet;
  RTC-Kalibrierung bleibt bekanntes TODO)
- App-Registry (src/apps.rs): Name + Icon + Start-Funktion — die
  Architektur für alle künftigen Apps; Einträge: Terminal, Uhr,
  Tastatur-Echo, Malkasten, Theme wechseln, Neustart
- Startmenü im Aurora-Stil (Startknopf oder Super-Taste): Suchfeld
  filtert die App-Liste live (Grundlage der späteren Schnellsuche),
  Bedienung per Maus (Klick/Hover) und Tastatur (Pfeile, Enter, ESC)
- SpeedShell als Terminal-FENSTER: konsole::_print leitet im
  Desktop-Modus in ein unit-getestetes Text-Raster um
  (fenster/terminal.rs, Zellen/Cursor/Scrolling/Resize), gerendert
  gebündelt pro Compositor-Frame; serielle Doppel-Ausgabe unberührt
- SpeedOS bootet jetzt DIREKT in den Desktop (Terminal offen);
  ESC wechselt zur Vollbild-Konsole, `desktop` zurück
- Fenster-Icons je nach Inhalt (Terminal/Uhr/Tastatur/Pinsel) in
  Titelleiste, Taskleiste und Alt+Tab; 6 neue 16x16-Icons
- Datum/Uhrzeit-Arithmetik in zeit.rs (Schaltjahre, Monatsübertrag),
  Neustart-Logik nach lib.rs gezogen (Shell-Befehl + App teilen sie)
- 9 neue Unit-Tests (Terminal-Raster, Taskleiste, Startmenü,
  App-Filter, Datum, Theme)

### Hinzugefügt (Fenster-Deko & volle Bedienung)
- Titelleiste pro Fenster: Icon, Titel, 3 Knöpfe (Minimieren,
  Maximieren/Wiederherstellen, Schließen rot); Aurora-Verlauf beim
  fokussierten, gedimmt beim inaktiven Fenster; Schatten (Alpha)
- Verschieben per Titel-Drag, Größe ändern per Rand-/Ecken-Drag
  (Cursor wechselt die Form: horizontal/vertikal/diagonal)
- Minimieren (lebt weiter), Maximieren (Vollbild minus 40px
  Taskleisten-Reserve), Schließen (Fenster-Puffer wird freigegeben)
- Alt+Tab-Fensterwechsler mit zentriertem Overlay (Titelliste +
  Auswahl-Highlight), Loslassen von Alt wechselt; holt auch
  minimierte Fenster zurück
- Snap-Layouts: Ziehen an den linken/rechten Rand = halbe Fläche
  (mit halbtransparenter Vorschau)
- grafik::Zeichner generisch über Zeichenflaeche-Trait (Apps malen
  in ihren Puffer, Compositor auf den Bildschirm — identisch);
  framebuffer::zeile_fuellen (schneller Zeilen-Füller),
  present_bereich, pixel_setzen_vorne
- zeit::warte_ms nutzt eine Slot-Liste von AtomicWakern (mehrere
  gleichzeitige Tick-Warter: Cursor, Compositor, Uhr)
- Heap wächst beim Desktop-Start passend zur Auflösung
- 3 neue Unit-Tests (Minimieren/Schließen, Größe ändern, Snap)

### Hinzugefügt (Fenster-Desktop)
- src/fenster/mod.rs: Fenster mit eigenem Pixel-Puffer + Metadaten,
  FensterManager mit Z-Ordnung und Fokus-Verwaltung
- Compositor-Task mit Dirty-Flags (nur komponieren, wenn nötig):
  Aurora-Hintergrund -> Fenster in Z-Reihenfolge -> present -> Cursor
- Event-Routing: Maus -> oberstes Fenster unter dem Cursor (in
  Fenster-Koordinaten), Klick hebt+fokussiert; Tastatur -> Fokus-
  Fenster; Drag an der Titelzeile verschiebt
- grafik::Zeichner generisch über Zeichenflaeche-Trait (Apps malen
  mit denselben Primitiven in ihren Puffer wie der Compositor auf
  den Bildschirm); schneller Zeilen-Füller (framebuffer::zeile_fuellen)
- zeit::warte_ms: mehrere gleichzeitige Tick-Warter (Slot-Liste
  statt einem AtomicWaker)
- Shell-Befehl `desktop`: 3 Demo-Fenster (Live-Uhr, Tastatur-Echo,
  statische Grafik mit Klick-Markern), ESC kehrt zur Konsole zurück
- 3 Unit-Tests (Fokus/Z-Ordnung, Verschieben, Koordinaten/Routing)

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
