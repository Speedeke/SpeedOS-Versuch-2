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
- **Performance-Setup (Juli 2026, gegen Maus-/Desktop-Lag):** (1) Der
  Kernel baut auch im dev-Profil mit `opt-level = 2` (Cargo.toml) —
  unoptimiert braucht ein Compositor-Frame hunderte ms. (2) QEMU läuft
  mit `-accel whpx,kernel-irqchip=off -accel tcg` (Hardware-
  Virtualisierung, TCG nur als Fallback). (3) Auflösung standardmäßig
  klein (720p-Klasse), wählbar per SPEEDOS_AUFLOESUNG. (4) PIT auf
  250 Hz, (5) Maus-Abtastrate nach der IntelliMouse-Sequenz auf 200/s.
- **Auflösungswahl (Juli 2026):** SPEEDOS_AUFLOESUNG (720p Standard,
  1080p, 2k, 4k, ... oder BREITExHOEHE) — Logik im boot/-Runner.
  Mechanik: Der Bootloader nimmt den GRÖSSTEN GOP-Modus, der seine
  Minimums erfüllt (.last()), und die Firmware bietet nur Modi an, die
  ins VRAM passen — also wird vgamem_mb (Zweierpotenz!) gerade so groß
  gewählt, dass der Wunschmodus der größte verfügbare ist; der
  EDID-Wunsch allein wird von OVMF ignoriert. RAM (-m) skaliert mit
  (~20 B/Pixel + 96 MiB, max 1 GiB = Bitmap-Allocator-Grenze).
  Firmware-Obergrenze: 4096x2160 (5120x2880 fehlt in der edk2-Tabelle,
  8K/128-MiB-VRAM hängt die Firmware auf) — größere Wünsche werden mit
  Meldung gedeckelt. Der Kernel selbst ist auflösungsunabhängig.
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

## Daten-Integritäts-Regel (Juli 2026)
- Dateisystem- und Geräte-Fehler werden NIE verschluckt: keine Panik, kein
  stilles `let _ =`, kein leerer Fallback auf dem Nutzer-Pfad. Jede Operation
  liefert `Result<_, FsFehler>` (Geräte-Schicht: `Result<_, IoFehler>`), und
  die Oberfläche ZEIGT den Fehler an — Shell rot via fs_fehler_ausgeben,
  Explorer in der Statusleiste, SpeedText als Fehler-Dialog
  (`ui::dialog::fehler`, dünner Mantel um bestaetigung()).

## Platten-Sicherheits-Regel (Juli 2026)
- Der ATA-Treiber weigert sich PER KONSTRUKTION, auf das Boot-Laufwerk
  zu schreiben: Das Feld `beschreibbar` ist privat, Laufwerke entstehen
  ausschließlich in `ata::init()`, und nur die konfigurierte DATEN-
  Platte (Primary Slave) bekommt Schreibrechte — es gibt keinen
  API-Weg, das zu umgehen (`IoFehler::Schreibgeschuetzt`). Tests
  laufen zusätzlich gegen ein EIGENES Daten-Image
  (speedos-daten-test.img), nie gegen speedos-daten.img.

## Architektur-Entscheidungen
- **SpeedFS (Juli 2026, `src/fs/speedfs.rs`) — das eigene Disk-
  Dateisystem:** Das On-Disk-Format ist in docs/speedfs-format.md
  SPEZIFIZIERT (Dokument vor Code; Format-Änderung = erst Doku,
  dann Version+1). Kurzform: Superblock "SPFS" | Block-Bitmap |
  Inode-Tabelle | Daten, 4-KiB-Blöcke, alles Little-Endian; Inodes
  128 B mit 22 direkten + 1 einfach-indirektem Zeiger (max. Datei
  ~4,09 MiB); Verzeichnisse = Byte-Listen [Inode u32|Länge u8|Name].
  KEIN JOURNAL — Konsistenz über die Schreib-Reihenfolge (§7 im
  Format-Doc): Belegen vor Benutzen, Inhalt vor Verweis, Entkoppeln
  vor Freigeben; jeder Op hat EINEN sektor-atomaren Commit-Punkt,
  Absturz hinterlässt höchstens Block-Lecks, nie falsche Zeiger.
  BLOCK-CACHE: Write-Through (ENTSCHEIDUNG: einfach und ehrlich —
  Code-Reihenfolge == Platten-Reihenfolge, die Absturz-Analyse gilt
  ohne Zusatzannahmen; Write-Back + geordnetes Flush wäre schneller
  und ist Serie-5-Stoff). SpeedFS kennt NUR das BlockDevice-Trait
  (läuft identisch auf RamDisk-Tests und ATA); Innen-Mutabilität
  über RefCell (kein Lock — der VFS-Mutex serialisiert schon).
  MOUNT-TABELLE (fs/mod.rs): Aus dem Root-Mount wurde
  `MountTabelle` (Wurzel-RamFs + Präfix-Mounts wie /platte), die
  SELBST FileSystem implementiert und per Pfad-Präfix routet —
  mit_fs() und ALLE Befehle/Apps blieben unverändert. rename über
  die Mount-Grenze -> FsFehler::MountGrenze; fs::verschieben(_rekursiv)
  fällt dann auf kopieren+löschen zurück. fs::mounten legt den
  Mount-Punkt im Wurzel-FS an; fs::unmounten synct ERST (bei Fehler
  bleibt gemountet). ata::daten_platte() = besitzbares
  BlockDevice-Handle, das an die Registry delegiert (Lock-Ordnung
  VFS -> LAUFWERKE, LAUFWERKE bleibt Blatt). Shell: mkfs.speedfs
  (nur mit Argument JA, nie bei gemountetem /platte), mount, umount.
  ACHTUNG TESTS: tests/ata_platte.rs schreibt Roh-Sektoren seit
  SpeedFS nur noch ans PLATTEN-ENDE (Sektor 130500+), weil vorne
  der Superblock liegt; tests/speedfs_platte.rs führt den
  Persistenz-Beweis mit einer echten Datei über das VFS.
- **ATA-PIO-Treiber (Juli 2026, `src/ata.rs`) — die erste echte
  Platte:** PIO gepollt über die Legacy-Ports des Primary-Kanals
  (0x1F0/0x3F6, fest verdrahtet — bewusst KEINE PCI-Enumeration),
  Kanal-Interrupts aus (nIEN). Jedes Status-Polling hat einen
  TSC-Timeout (`IoFehler::Zeitueberschreitung` — nie endlos auf
  Hardware warten; leerer Steckplatz wird am Status 0x00/0xFF sofort
  erkannt). IDENTIFY liefert Modell/Kapazität (Dekoder = reine,
  unit-getestete Funktionen; Modell-Bytes paarweise vertauscht!).
  LBA28 = max. 128 GiB und 256 Sektoren pro Kommando — der Treiber
  zerlegt größere Aufträge selbst; LBA48 wäre rein additiv.
  FLUSH CACHE (0xE7) ist das sync(). Implementiert `BlockDevice`;
  die Laufwerks-Registry LAUFWERKE ist ein BLATT-Lock (nur aus
  Task-Kontext). Der Runner hängt speedos-daten.img (64 MiB,
  persistent, Projekt-Root, gitignored) als Primary Slave an —
  ata::init() läuft in main.rs NACH zeit::init() (Timeouts brauchen
  die TSC-Zeit). Shell: `platten` + `blocktest <lba>` (Hexdump).
  tests/ata_platte.rs führt den PERSISTENZ-BEWEIS: Generationen-
  Muster in Sektor 1000 überlebt QEMU-Neustarts.
- **VFS-Abstraktion (Juli 2026, erweitert um die Serie-4-Naht):** Alle
  Dateisysteme implementieren das Trait `FileSystem` in `src/fs/mod.rs`
  (lesen, schreiben, liste, mkdir, loeschen, node_typ, read_at, write_at,
  stat, rename, sync — absolute, normalisierte Pfade mit `/`). Shell-Befehle
  und Kernel greifen NIE auf eine konkrete Implementierung zu, sondern nur
  über `fs::mit_fs()` auf das global gemountete VFS. Erste Implementierung ist
  `RamFs` (`src/fs/ramfs.rs`, in-memory); FAT32 und ein eigenes
  Disk-Dateisystem sollen später exakt dieselbe Schnittstelle bedienen —
  dann wird nur das gemountete Dateisystem ausgetauscht, kein Befehl ändert sich.
  API-ENTSCHEIDUNGEN der Erweiterung: read_at liefert die GELESENE Anzahl
  (0 am/hinter dem Dateiende = kein Fehler, POSIX-read-Semantik); write_at
  legt fehlende Dateien an und füllt Lücken hinterm Dateiende mit Nullbytes;
  stat liefert `Metadaten` (Typ, Größe, erstellt/geaendert als Sekunden seit
  1.1.2000 — zeit-Epoche, Anzeige über einstellungen::stempel_text mit dem
  Systray-Uhr-Offset); rename ist die ATOMARE Primitive (erst komplett
  validieren, dann entnehmen+einfügen; Ziel-DATEI wird ersetzt,
  Ziel-VERZEICHNIS ist Fehler, Ziel im eigenen Teilbaum ist Fehler,
  Zeitstempel wandern mit) — fs::verschieben/verschieben_rekursiv laufen
  darüber (kein kopieren+löschen mehr; bei mehreren Mounts braucht die
  FS-Grenze wieder einen Kopier-Fallback); sync() drückt Schreib-Caches aufs
  Medium (RamFs: ehrliches No-Op; einstellungen::speichern ruft es bereits).
  `FsFehler::Io(IoFehler)` transportiert Geräte-Fehler durchs ganze VFS.
- **BlockDevice-Naht (Juli 2026, `src/fs/block.rs`):** JEDER Massenspeicher-
  Treiber (RamDisk heute, AHCI/NVMe/virtio später) implementiert das schmale
  Trait `BlockDevice` (sektor_groesse, anzahl_sektoren, lese_sektoren,
  schreibe_sektoren, sync — alles `Result<_, IoFehler>`). SEKTOR-Adressierung
  (LBA), Puffer = Vielfaches der Sektorgröße (validiert, nie still
  abgeschnitten). Disk-Dateisysteme reden NUR mit BlockDevice, nie mit einem
  konkreten Treiber; die `RamDisk` (Vec-basiert) ist Referenz-Implementierung
  und Test-Unterbau — die Naht existiert BEWUSST vor dem ersten Treiber.
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
- **Dirty-Rect-Compositing (Juli 2026) — DAS PROTOKOLL:** Änderungen
  melden ihre Bildschirm-Fläche per `dirty_melden(rect)` an (max.
  MAX_DIRTY_RECTS=16, Überlauf -> alles_dirty-Vollbild-Fallback):
  Fenster-Drag/Resize melden ALTE+NEUE Fläche (fenster_flaeche =
  gesamt_rechteck + 10px Schatten), Heben meldet Fenster + Alt-Fokus +
  Taskleiste, der Uhr-Sekundenwechsel NUR das systray_rechteck,
  Startmenü/Switcher ihre Panel-Flächen; Fenster mit fenster.dirty
  werden in dirty_abholen eingesammelt. Der Compositor holt per
  `dirty_abholen(b, h)` die (geklemmten) Rects, komponiert JE Rect mit
  Zeichner-Clip (Fenster ohne Schnitt werden übersprungen, Alpha-Fills
  clippen vorab) und presentet nur diese Bereiche. Der Desktop-
  Verlauf liegt als BYTE-IDENTISCHER Cache im DoppelPuffer
  (hintergrund_uebernehmen/_wiederherstellen = memcpy pro Zeile —
  NICHT als Farbe-Array, das wäre eine Pro-Pixel-Konvertierung und
  LANGSAMER als der alte Gradient!); das Flag manager.hintergrund_neu
  lässt den Compositor ihn beim ersten Frame/Theme-Wechsel neu
  rendern. Gemessen: Uhr-Tick bei 4K 0,31 ms statt 9,3 ms Vollbild.
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
- **Grafik-Schnellpfade (Juli 2026, Qualitäts-Pass):** Das
  Zeichenflaeche-Trait hat zwei ZEILEN-Methoden (flaeche_zeile_fuellen,
  flaeche_zeile_kopieren) mit korrekten Pro-Pixel-Defaults; DoppelPuffer
  und FensterPuffer überschreiben sie speicher-nah. Der Zeichner clippt
  dafür VORAB rechteckig (sichtbar() = Rechteck ∩ Clip ∩ Fläche) —
  deckendes rechteck_fuellen, verlauf_vertikal und puffer_blit (der
  Compositor-Blit für Fensterinhalte) laufen also OHNE Prüfungen pro
  Pixel. Alpha bleibt auf dem Pixel-Pfad (muss den Untergrund lesen).
  Frame-Zeit-Messung: fenster::tests::messung_compositor_frame_zeit
  (Berichts-Test, Zahlen im CHANGELOG). Die frühere Mess-Falle
  ("ticks() steht unter without_interrupts still") ist seit der
  TSC-Zeitquelle TOT — zeit::us_seit_boot()/ms_seit_boot() dürfen
  ÜBERALL genommen werden, auch in mit_framebuffer-Blöcken.
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
  zwei Instanzen: AURORA_DUNKEL Standard, AURORA_HELL) + `metrik()` (alle
  Abstände/Schriftgrößen, in beiden Themes gleich). Aktives Theme über
  AtomicBool, `theme::aktuell()` ist lockfrei (wird unter gehaltenen
  Locks im Compositor gerufen). SEITDEM GILT: KEINE hartcodierten Farben
  oder Abstände in UI-Code — alles über theme::aktuell()/metrik().
- **UI-Skalierung (Juli 2026):** Faktor 1.0/1.5/2.0, gespeichert in
  HALBEN (AtomicI32: 2/3/4 — kein Fließkomma im Kernel!). `metrik()`
  liefert die SKALIERTE Kopie der BASIS_METRIK; die Schrift mappt auf
  die vorgerasterten Fonts (16/24/32 — Cargo-Features size_16/24/32),
  schrift_gross ist bei 32 gedeckelt. Boot-Standard nach Breite
  (>=2560 -> 1.5, >=3840 -> 2.0, desktop_starten); Umschalten zur
  Laufzeit über die Registry-App "Skalierung" (fenster::
  skalierung_wechseln = Theme-Wechsel-Mechanik: Inhalte neu zeichnen
  + alles_dirty). ACHTUNG Tests: metrik()-abhängige Koordinaten gelten
  für Skala 1.0 — der Shell-Befehls-Test setzt die Skala im Cleanup
  zurück (desktop_starten hätte sie bei 4K-Testläufen verstellt).
  Wechsel via `fenster::theme_wechseln()` (schaltet um UND rendert alle
  Fenster-Inhalte neu). Das Terminal bleibt bewusst in beiden Themes
  dunkel (Shell-Farben sind auf dunklen Grund abgestimmt, Zellen-
  Hintergrund == Color::Black == theme.terminal_hintergrund).
- **Taskleiste & Startmenü (Juli 2026):** Der Compositor zeichnet die
  Taskleiste NACH den Fenstern (immer im Vordergrund), das Startmenü
  darüber; Klicks prüfen dieselbe Reihenfolge (Menü -> Leiste ->
  Fenster). Fenster-Knöpfe sind nach FensterId (= Erstellungsreihen-
  folge) sortiert, damit sie beim Fokuswechsel nicht springen; Klick =
  Fokus/Minimieren-Toggle. Uhr+Datum kommen aus `einstellungen::
  jetzt_lokal()`/`uhrzeit_text()` (echte RTC+TSC-Zeit via zeit::jetzt(),
  plus Anzeige-Offset und 12/24h aus den Einstellungen — der frühere
  Tick-Platzhalter ist Geschichte); neu komponiert wird nur beim
  Sekundenwechsel.
- **App-Registry & App-Trait (Juli 2026):** `src/apps.rs` — jeder
  Registry-Eintrag (`AppEintrag`) = Name + Icon + `start: fn()`.
  NEUE Apps implementieren `ui::App` (name/icon/aufbau/nachricht/tick)
  und landen als `Inhalt::App(AppFenster)` im Fenster (die Brücke vom
  Enum zum Trait; das Enum bleibt für Terminal und alte Demos) —
  start-fn ruft `fenster::app_starten(Box::new(MeineApp))`. LOCK-REGEL
  (ui/app.rs): App::nachricht/tick laufen UNTER dem MANAGER-Lock —
  eigener Zustand/fs/serial_println erlaubt, print!/fenster:: verboten;
  Außenwirkung über AppReaktion.danach (fn(), läuft nach dem Lock).
  Startmenü und Alt+Tab-Switcher laufen aufs Toolkit: Suchfeld =
  Textfeld-Widget (Änderungs-Nachricht = Live-Filter), Liste =
  ScrollListe, gezeichnet in einen OFFSCREEN-FensterPuffer, den der
  Compositor per puffer_blit zeigt (Muster für alle Overlays).
  Deadlock-Regel unverändert: Start-Funktionen/Nachrichten via
  NachLock (jetzt: Keine | Ausfuehren(fn()) | Nachricht) nach draußen.
- **UI-Widget-Toolkit (Juli 2026):** `src/ui/` = das UI-Fundament
  aller Apps. Retained Widget-Baum: `trait Widget` (wunschgroesse,
  zeichnen in den FensterPuffer, ereignis mit Rechteck-Routing wie
  kante_bei — alle Koordinaten in Fensterinhalt-Koordinaten, kein
  Umrechnen). `UiEreignis` (Klick/Doppelklick/Losgelassen/Bewegt/
  Scroll/Taste/MausRein/MausRaus/FokusRein/FokusRaus); MausRein/Raus
  erzeugt das ROUTING in den Box-Containern (hover_kind) — Widgets
  pflegen damit ihren Hover-Zustand. `UiReaktion` ist bewusst ein
  STRUCT (verbraucht + neu_zeichnen + nachricht sind kombinierbar).
  App-Nachrichten als u32-ID an einen fn(u32)-Handler (KEINE
  Closures: Borrow-Hölle; KEIN generischer Typ: macht das Trait
  un-objektsicher) — zustandsbehaftete Apps bekommen später ein
  App-Trait. Fokus-Kette: fokus_weiter (Blätter nehmen/geben,
  Container iterieren ab Fokus-Kind, UiFenster wrappt bei Tab);
  Tasten laufen den Baum entlang, bis das fokussierte Widget sie
  verbraucht. Layout primitiv: laengen_verteilen (pure, getestet)
  + VBox/HBox/Fueller mit METRIK.abstand, quer wird IMMER auf volle
  Breite gestreckt — kein Constraint-Solver. Widgets: Label,
  Trennlinie, Button (Nachricht beim LOSLASSEN im Bereich),
  Checkbox, Textfeld (Innenleben = shell::editor::ZeilenEditor,
  Cursor blinkt über zeit-API + Uhr-Task-Anstoß via UiFenster::
  blinkt), ScrollListe (Rad + ziehbarer Balken + Doppelklick).
  Doppelklick erkennt das UiFenster (500 ms, 6 px, us_seit_boot);
  seine Nachricht hat Vorrang vor der zweiten Klick-Nachricht.
  Fenster-Anbindung: `Inhalt::Ui(UiFenster)`; der Manager reicht
  Klick/Losgelassen/Scroll/Bewegt (Hover! ui_hover_fenster erzeugt
  MausRaus beim Fensterwechsel) und Tasten weiter. Ui-NACHRICHTEN
  laufen wie App-Starts NIE unter dem MANAGER-Lock (`NachLock`-Enum
  wird nach draußen gereicht). Der PANIC-HANDLER druckt ZUERST roh
  seriell (println! würde im Desktop-Modus via Terminal-Umleitung
  den MANAGER-Lock brauchen -> Deadlock bei Panik unterm Lock).
- **Dateioperationen & Kontextmenü (Juli 2026):** Rekursives
  Kopieren/Löschen/Verschieben lebt in fs/mod.rs (liste() IMMER vor
  dem Abstieg abschließen — mit_fs nie verschachteln). Papierkorb =
  /papierkorb; Ursprung steht in einer METADATEN-Datei
  (<name>.herkunft — kein Namens-Parser, Ansicht filtert sie aus).
  Ablage (`src/ablage.rs`) = globaler Blatt-Lock (darf unter dem
  MANAGER-Lock genutzt werden) — Strg+C/X/V fensterübergreifend;
  KeyStream dekodiert mit MapLettersToUnicode (Strg+C = U+0003).
  Kontextmenü = GENERISCHES Manager-Overlay (Offscreen + Blit;
  Empfänger als FensterId): Apps liefern es via AppReaktion::menue
  auf UiEreignis::Rechtsklick (ScrollListe::mit_rechtsklick);
  AppReaktion::danach ist eine Box<dyn FnOnce> (Aktion MIT Daten,
  z. B. Betrachter-Pfad) -> NachLock::Einmal.
- **Einstellungen (Juli 2026):** `src/einstellungen.rs` = Store + App.
  (1) STORE: /system/einstellungen.txt im VFS (Schlüssel=Wert;
  parsen/serialisieren rein + getestet), typisierter Zugriff
  (hole_/setze_zahl/bool/text — setze_* speichert SOFORT). Der
  SPEICHER-Mutex ist ein BLATT-Lock wie die Ablage (unter dem
  MANAGER-Lock erlaubt); main.rs lädt nach fs::init und wendet auf
  die theme-Atomics an. API-Naht für Serie 4 (Disk-FS = nur VFS
  tauschen). (2) APP: Kategorien-ScrollListe links, Seiten rechts.
  DAS MUSTER für sofort wirkende Optik-Optionen: lock-freies Atomic
  UNTER dem MANAGER-Lock setzen (theme::hell_setzen/akzent_setzen/
  hintergrund_setzen/skala_setzen_halbe — sonst markiert der direkt
  folgende Neu-Aufbau den alten Zustand!), setze_* persistieren,
  Neuzeichnen via AppReaktion.danach -> fenster::alles_neu_zeichnen()
  (hintergrund_neu + alle Inhalte + alles_dirty). NEUE Theme-
  Fähigkeiten: theme::aktuell() liefert eine KOPIE mit eingesetzter
  Akzentfarbe (Palette AKZENTE, je Eintrag Hell-/Dunkel-Variante,
  patcht akzent + rahmen_aktiv); Desktop-Verlauf über theme::
  hintergrund_verlauf() (HINTERGRUENDE, Preset 0 = Theme-Aurora).
  Systray-Uhr: einstellungen::jetzt_lokal() + uhrzeit_text()
  (UTC-Offset = reine ANZEIGE-Verschiebung; die RTC liefert in QEMU
  die Host-LOKALZEIT, -rtc base=localtime). Cursor-Blinktempo:
  cursor_blink_ms/us, live gelesen von Textfeld + Konsolen-Task.
  Info-Seite: Auflösung wird beim App-Start GECACHT (mit_framebuffer
  unter dem MANAGER-Lock wäre die falsche Lock-Ordnung!); Task-Zahl
  als Atomic im Executor. Boot-Skala: gespeicherter Wert schlägt die
  Auto-Wahl nach Breite (desktop_starten).
- **Explorer & App-Muster (Juli 2026):** `src/explorer.rs` = die
  Blaupause für Trait-Apps: Die App hält ZUSTAND (Pfad, Verlauf,
  Auswahl, aufgeklappte Baum-Ordner) plus ABGELEITETE Listen
  (neu_laden nach jeder Navigation); aufbau() baut die Widgets rein
  daraus. WELCHER Listeneintrag gemeint ist, steckt in der Nachricht:
  ScrollListe::mit_index_nachrichten kodiert BASIS+Index (Basen weit
  auseinander legen!). Auswahl überlebt Neu-Aufbauten via
  mit_auswahl + auswahl_sichtbar (Scroll ist eine Cell — zeichnen ist
  &self). Eingabemodi (Adresszeile) laufen über den App::taste-Hook
  (VOR den Widgets, App puffert selbst); fokus_initial gibt der
  ersten fokussierbaren Liste die Pfeiltasten. Mehrere Fenster einer
  App = mehrere App-Instanzen (app_starten baut immer neu).
- **Terminal-SITZUNGEN (Juli 2026, löst das Ein-Terminal-Limit ab):**
  `shell/sitzung.rs`. Jedes Terminal-Fenster = eigene Sitzungs-Id +
  EIGENER Shell-Task (shell::sitzung_laufen; apps::terminal_starten
  spawnt ihn nach fenster::terminal_oeffnen() -> Option<Sitzungs-Id>).
  Der EINGABE-ROUTER (shell::eingabe_router, einziger KeyStream-
  Leser) routet: Startmenü/ESC wie gehabt, sonst Tasten in die
  lock-freie Queue der fokussierten Sitzung (terminal_fokus_sitzung);
  im Vollbild-Modus an die HAUPT-Sitzung. AUSGABE: Der Shell-Task
  legt AUSGABE_SITZUNG um jede synchrone Verarbeitung (KEIN await
  dazwischen — deshalb race-frei); konsole::_print schreibt an
  ausgabe_ziel() (Ausgabe-Sitzung, sonst Haupt-Terminal = Kernel-
  Log). Ohne offenes Terminal wird Kernel-Log GEPUFFERT und beim
  nächsten terminal_oeffnen nachgereicht; Ausgaben toter Sitzungen
  verfallen. SCHLIESSEN: fenster_schliessen trägt die Sitzung aus
  (beendet-Flag + Waker) -> naechste_taste liefert None -> Task endet
  sauber; das Haupt-Terminal vererbt seine Rolle ans nächste.
  `fenster/terminal.rs` bleibt das reine Text-Raster; gerendert wird
  weiter GEBÜNDELT (inhalt_neu + inhalte_rendern pro Frame).
  prompt_nachholen() nutzt nur noch der Vollbild-Pfad (ESC/Demo-Ende,
  cwd-Spiegel der Haupt-Sitzung). SEIT DEM SERIE-3-PERFORMANCE-PASS
  führt das Raster DIRTY-ZEILEN: terminal_rendern zeichnet nur den
  geänderten Zeilenbereich in den persistenten Fenster-Puffer, und
  terminal_schreiben meldet dem Compositor nur den Zeilen-STREIFEN
  (2x schnellere Prompt-Ausgabe); Scroll/Resize/inhalt_zeichnen
  (Theme!) markieren alles. Der Frame-Pfad für Terminals läuft in
  inhalte_rendern OHNE fenster.dirty.
- **SpeedText & Dialog-Bausteine (Juli 2026):** `src/speedtext.rs` +
  `ui/texteditor.rs` + `ui/dialog.rs`. Der TextPuffer ist ein
  Zeilen-Vec (BEWUSST kein Rope — KiB-Dateien, Begründung im Code)
  mit Zeichen-Spalten (chars, nie Bytes!); das Editor-Widget teilt
  ihn per Arc<Mutex> (Blatt-Lock) mit der App, damit der Text die
  ständigen Neu-Aufbauten (Statuszeile!) überlebt — DAS Muster für
  großen, heißen Widget-Zustand. Dialoge ERSETZEN den Fenster-Inhalt
  über App-Zustand (kein Overlay): dialog::bestaetigung() = generische
  Frage+Knöpfe; dialog::DateiDialog = Zustands-Baustein (Ordner-Liste
  + selbst gepufferte Pfad-Eingabe via App::taste-Hook, Nachrichten
  in einem Id-Fenster ab Basis, DIALOG_ID_BREITE). Neue App-Trait-
  Fähigkeiten: fenster_titel() (Start-Titel), AppReaktion.titel
  (Titel ändern -> "name.txt *"), AppReaktion.schliessen (Fenster
  aus der App schließen) und App::schliessen_abfragen() (X-Knopf
  abfangen -> Nachfrage-Dialog; None = sofort zu). Explorer-
  Doppelklick auf Dateien öffnet SpeedText (Betrachter entfernt).
  SpeedTexts Tipp-Pfad ist seit dem Performance-Pass schlank: KEIN
  Baum-Neuaufbau pro Taste — die StatusZeile (texteditor.rs) liest
  Zeile/Spalte/Zeichen beim ZEICHNEN live aus dem Arc, der Titel
  wird nur bei echtem Wechsel gemeldet (letzter_titel-Vergleich).
- **Toolkit-Konventionen (Serie-3-Review):** `ui::w(widget)` statt
  `Box::new(widget) as Box<dyn Widget>` (neue Apps/Umbauten);
  `ui::app::SekundenTick` für 1-Hz-Live-Apps (Einstellungen,
  Task-Manager) statt eigener letzte_sekunde-Buchhaltung. Bekannte
  Ecken (bewusst offen): Tab ist global die Fokus-Taste (Editor kann
  keine Tabs einfügen); Nachricht-Basen sind Handarbeit (Basen weit
  auseinander legen, DIALOG_ID_BREITE als Muster); Textfeld-Inhalt
  überlebt Neu-Aufbauten nicht (Apps puffern selbst oder teilen
  Zustand per Arc wie der Editor).
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
  SEIT DEM TASK-MANAGER: Task::new(NAME, future) — jeder Task trägt
  Name/Art/beendbar (Builder mit_art/als_beendbar); die Registry
  in `task/uebersicht.rs` ist die Schatten-Buchhaltung (Blatt-Lock,
  Momentaufnahme unterm MANAGER-Lock erlaubt), die heißen Zähler
  (Polls/Wecken/wach) sind Atomics im geteilten Arc — der WAKER
  zählt aus dem Interrupt-Kontext, ohne Lock. Beenden ist
  KOOPERATIV: beenden_anfordern setzt nur ein Flag, der Executor
  lässt den Task in der nächsten Runde am await-Punkt FALLEN (Drop
  der Future — nur beendbare Demo-Tasks, Kernel-Tasks geschützt).
  CPU-Auslastung: run() misst per TSC Arbeit (run_ready_tasks) vs.
  Ruhe (hlt) und verbucht in ein 10x100-ms-Gleitfenster
  (cpu_auslastung_prozent, reine getestete Fenster-Logik).
  Die Task-Manager-App (src/taskmanager.rs) zeigt alles sekündlich
  per tick; Graph-Downsampling nimmt das Spalten-MAXIMUM.
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
- **Zeit nur über `src/zeit.rs` (seit Juli 2026 TSC-basiert):**
  us_seit_boot()/ms_seit_boot() laufen über den beim Boot gegen den
  PIT kalibrierten TSC (zeit::init, ~200 ms, loggt Frequenz/
  Genauigkeit/CPUID-Invariant) — mikrosekundengenau und UNABHÄNGIG
  von Interrupts (kein Stillstand unter without_interrupts; Zeit darf
  überall genommen werden). Der PIT (250 Hz, Teiler zeit::PIT_TEILER,
  denselben Wert nutzt interrupts::pit_initialisieren) ist nur noch
  WECKGEBER für warte_ms/Executor und Fallback vor der Kalibrierung.
  Echte Uhrzeit: rtc.rs liest die CMOS-Uhr EINMAL beim Boot (Update-
  in-Progress-Flag, BCD/12h-Modus, Doppel-Lesen bis stabil, Timeout);
  zeit::jetzt() = RTC-Anker + TSC-Zeit. QEMU-RTC läuft per Runner auf
  der Host-LOKALZEIT (-rtc base=localtime). zeit::init() MUSS nach
  speed_os::init() laufen (PIT muss ticken) — auch im Test-Kernel.
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
