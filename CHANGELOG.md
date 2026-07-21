# Changelog

Alle nennenswerten Änderungen an SpeedOS, neueste zuerst.
Format angelehnt an [Keep a Changelog](https://keepachangelog.com/de/).

## [Unveröffentlicht]

### SpeedOS bootet vom USB-Stick (Live-System) + Diagnose-Modus
- `cargo image` (neuer Alias + Host-Binary `boot/src/bin/live-image.rs`)
  baut **`speedos-live.img`** — ein bootfähiges UEFI-GPT-Image mit
  EFI-System-Partition, OHNE QEMU-Start und OHNE Daten-/FAT-Platte.
  BEWUSST ohne erzwungene Mindestauflösung (Unterschied zum Runner):
  auf echter Hardware nimmt die Firmware ihren nativen GOP-Modus, der
  Kernel ist ohnehin auflösungsunabhängig + skaliert HiDPI automatisch
- Robustheit gegen fehlende Geräte (ohne Übertreibung):
  - **Keine PS/2-Tastatur** wird per nicht-intrusiver Probe erkannt
    (First-Port-Test 0xAB, ändert KEINE Controller-Config — die Regel
    "Tastatur-Bits 0/4/6 nie anfassen" bleibt gewahrt); statt still zu
    hängen erscheint die Bildschirm-Meldung „keine PS/2-Eingabe
    gefunden — USB-Eingabe kommt in einer künftigen Version"
  - **Keine PS/2-Maus** → Desktop läuft per Tastatur (war schon so;
    jetzt als Presence-Flag gemerkt)
  - **Keine Platte** → RAM-VFS-Fallback (war schon so; nun sichtbar)
- **Diagnose-Modus** (`src/diagnose.rs`): auf echter Hardware gibt es
  keine serielle Ausgabe — Taste **D** beim Boot (oder `SPEEDOS_DIAGNOSE=1`
  im Runner via `set_ramdisk`) zeigt die Boot-Schritte [1/4]..[4/4] und
  eine Hardware-Zusammenfassung (Bildschirm, Tastatur/Maus, Laufwerke,
  Mounts) auf dem Bildschirm
- QEMU-Generalprobe (`tools/live_qemu.ps1`, UEFI/OVMF) für alle Fälle
  verifiziert (Screendumps in `docs/screenshots/live-*.png`): Normal-
  Desktop, `i8042=off` → Eingabe-Meldung, Taste D → Diagnose-Schirm,
  2560×1440 → HiDPI-Skalierung. `cargo test` bleibt grün
- Anleitungen: [`docs/usb-boot.md`](docs/usb-boot.md) (auf Stick
  schreiben inkl. Laufwerks-Warnung, BIOS/UEFI-Einstellungen, Erst-Boot)
  und [`docs/hardware-log.md`](docs/hardware-log.md) (Vorlage für echte
  Geräte-Tests mit Fotos)

### Feinkörniges Dirty-Rect: das Voll-Zeichnen pro Taste stirbt
- Widgets melden jetzt SCHADENS-RECHTECKE statt "ganzes Fenster neu":
  `UiReaktion.schaden: Option<Rechteck>` (Fensterinhalt-Koordinaten),
  neu über `neu_zeichnen_bereich()` / `mit_schaden()`. Ohne Rect
  bleibt der ehrliche Vollbild-Fallback (Korrektheit vor Eleganz).
  Fenster sammeln MEHRERE Schadens-Rects (keine Bounding-Box —
  Cursorzeile oben und Statuszeile unten würden sonst fast das ganze
  Fenster umfassen); der Compositor rendert/komponiert je Rect nur den
  gemeldeten Streifen (die Dirty-Rect-Mechanik bekam feinere Meldungen)
- Heiße Pfade umgestellt: SpeedText-Tippen (nur Cursorzeile +
  Statusstreifen; der Editor CULLT Zeilen außerhalb des Clips — ohne
  das würde `text()` bei 4K Millionen Glyph-Pixel gegen den Clip
  prüfen), Textfelder, ScrollListen (Scrollen = Listenfläche),
  Button-/Checkbox-Hover (nur der Button). Der Task-Manager bleibt
  bewusst Vollbild (er tickt nur 1×/s und aktualisiert Zahlen + Graph
  + Liste gemeinsam — kein interaktiver Hot-Path)
- BEWEIS (Berichts-Test messung_serie3, ALT/NEU im selben Lauf —
  immun gegen die WHPX/TCG-Lotterie), großes SpeedText-Fenster:

  | Szenario                | 720p ALT | 720p NEU | 4K ALT   | 4K NEU |
  |-------------------------|----------|----------|----------|--------|
  | Editor-Tippen (µs/Taste)| 2553     | 350      | 15430    | **417** |
  | Terminal-Ausgabe (µs)   | 1713     | 1017     | 1325     | 744    |

  ZIEL ERREICHT: Editor-Tippen bei 4K **417 µs** (< 500 µs) — vorher
  15,4 ms/Taste (unbenutzbar), jetzt ~37× schneller. Der größte
  Rest-Fresser (ein zu großzügiger Statusstreifen) wurde analysiert
  und auf eine Zeilenhöhe geschrumpft (502 µs -> 417 µs)
- Keine sichtbaren Artefakte: per QMP-Screenshots an den heiklen
  Stellen geprüft (überlappende Fenster, Terminal-Streifen-Ausgabe
  unter/über anderem Fenster) — der Compositor komponiert je Rect
  alle Fenster in Z-Ordnung, Überlappung bleibt korrekt
- 142 Lib-Tests (inkl. neuer Schadens-Kombinations- und
  umschliessen-Tests) + alle Integrationstests grün; die
  Routing-Tests unverändert bestanden

### PCI + virtio-blk: die schnelle Platte (Infrastruktur für virtio-net)
- Neues PCI-Modul `src/pci.rs`: Config-Space-Enumeration über die
  Legacy-Ports 0xCF8/0xCFC, Bus/Gerät/Funktion durchgehen, Vendor/
  Device/Klasse und BARs (I/O- und 64-Bit-MMIO) dekodieren.
  Shell-Befehl `pci` listet alles lesbar. Reine Dekoder-Funktionen
  unit-getestet — die Grundlage für jeden künftigen modernen Treiber
- Neues virtqueue-Modul `src/virtio/virtqueue.rs`: die Split-Virtqueue
  (Deskriptor-Tabelle + Available-/Used-Ring in physisch
  zusammenhängendem Speicher) — GERÄTE- UND TRANSPORT-UNABHÄNGIG und
  ausführlich kommentiert als BLAUPAUSE für virtio-net (Serie 5).
  Speicher-Barrieren an den Übergabepunkten, Schleifen-Schutz beim
  Kettenfreigeben, Freiliste-Verwaltung
- Neuer virtio-blk-Treiber `src/virtio/blk.rs`: virtio über den
  PCI-LEGACY-Transport (Port-I/O-BAR — Begründung im Code: QEMUs
  transitional device bietet es an, wir kennen Port-I/O vom
  ATA-Treiber, und die Virtqueue bleibt für Modern/virtio-net gleich).
  Feature-Negotiation (FLUSH), eine Virtqueue, Requests gepollt mit
  Timeout, Bounce-Puffer für DMA. Implementiert BlockDevice inkl.
  sync (FLUSH)
- Runner: `SPEEDOS_PLATTE=ide|virtio` wählt das Backend der
  Daten-Platte; `fs::daten_geraet()` liefert das richtige BlockDevice
  (virtio hat Vorrang), alle Aufrufer (mkfs/mount/pruefe/automount/
  Einstellungen) laufen unverändert darüber. SpeedFS funktioniert auf
  beiden Backends identisch — der Persistenz-Beweis besteht auf beiden
- Neuer Shell-Befehl `plattentest`: Benchmark der rohen Daten-Platte
  (sequenziell + zufällig, lesen + schreiben, MiB/s). GEMESSEN
  (2 MiB seq, 100×4 KiB zufällig, QEMU/WHPX):

  | Zugriff             | IDE (PIO) | virtio-blk | Faktor |
  |---------------------|-----------|------------|--------|
  | seq. schreiben      | 0,70 MiB/s | 1810 MiB/s | ~2600× |
  | seq. lesen          | 0,70 MiB/s | 4819 MiB/s | ~6900× |
  | zufällig schreiben  | 0,69 MiB/s |  144 MiB/s |  ~200× |
  | zufällig lesen      | 0,70 MiB/s |  298 MiB/s |  ~425× |

  Der IDE-PIO-Pfad ist so langsam, weil jedes 16-Bit-Wort einen
  Port-I/O-VM-Exit kostet; virtio nutzt DMA + Virtqueue. virtio
  gewinnt klar -> **Standard jetzt virtio** (IDE bleibt per
  SPEEDOS_PLATTE=ide wählbar, u. a. für die volle ATA-Test-Abdeckung)
- Tests: PCI-Dekoder (config_adresse, BAR, Klasse), virtqueue-Layout
  und Ring-Ablauf ohne Gerät; die Persistenz-Tests laufen über
  fs::daten_geraet und bestehen auf BEIDEN Backends (ata_platte
  überspringt seine ATA-Daten-Tests unter virtio sauber). 140
  Lib-Tests + alle Integrationstests grün

### FAT32 lesen: SpeedOS versteht fremde Medien (nur lesend)
- Neuer FAT32-Treiber `src/fs/fat32.rs` (NUR LESEN) auf dem
  BlockDevice-Trait: Bootsektor/BPB parsen und streng validieren
  (nie panicken -> `FsFehler::KeinFat32`), die FAT einmal in den RAM,
  Cluster-Ketten mit Schleifen-Schutz verfolgen, Verzeichnisse
  inklusive langer Dateinamen (VFAT-LFN, UTF-16-LE -> unsere Strings,
  Umlaute stimmen), Dateien über read_at lesen, FAT-Zeitstempel ->
  zeit-Epoche. Jeder Schreib-Weg lehnt sauber mit
  `IoFehler::NurLesen` ab
- Runner: `tools/fat32_image_erzeugen.py` baut speedos-fat.img mit
  Beispieldateien, Unterordner und Umlaut-Namen — bevorzugt mit den
  Host-mtools (mformat/mcopy), sonst mit eingebautem Python-FAT32-
  Writer. Der Runner hängt es als Secondary Master an ("USB-Stick");
  gitignored
- Mount-Integration: `fs::fat_automounten()` beim Boot mountet das
  FAT-Laufwerk NUR LESEND unter /fat; `platten` zeigt jetzt eine
  Mount-Übersicht mit Dateisystem-Typ und Zugriffsrecht (neue
  Trait-Methoden `FileSystem::ist_beschreibbar`/`typ_name`,
  `fs::mount_uebersicht`/`pfad_beschreibbar`)
- Explorer graut Schreib-Aktionen (+O/+D, Umbenennen, Löschen,
  Ausschneiden, Einfügen) auf Nur-Lese-Mounts aus — Kontextmenü und
  Tastenkürzel gesperrt; Kopieren VON /fat bleibt erlaubt. Der
  Alltag funktioniert: durch /fat navigieren, .txt in SpeedText
  öffnen (Umlaute im Namen UND im Inhalt), Dateien nach /platte
  kopieren
- Tests: BPB-Validierung gegen kaputte Werte (kein Panik),
  LFN-Zusammensetzung inkl. Umlauten, komplettes Mini-FAT32 auf
  einer sparse RamDisk; Integrationstest tests/fat_platte.rs
  vergleicht die gelesenen Inhalte Byte für Byte mit dem
  mtools-Image (Texte, Umlaut-Namen, 100-KiB-Datei über viele
  Cluster, Kopieren /fat -> /platte). 134 Lib-Tests + alle
  Integrationstests grün

### Persistenz wird Standard: SpeedOS überlebt den Neustart
- AUTO-MOUNT beim Boot: fs::platte_automounten() erkennt die
  Daten-Platte, mountet ihr SpeedFS unter /platte (klare serielle
  Meldung mit Füllstand) und legt beim ersten Mal /platte/heim,
  /platte/dokumente und /platte/system an. Eine unformatierte
  Platte bekommt NUR den mkfs-Hinweis in der Shell — nie
  Auto-Format (Formatieren bleibt eine Nutzer-Entscheidung)
- DER Demo-Moment, maschinell bewiesen: Die Einstellungen wohnen
  jetzt auf /platte/system/einstellungen.txt (RAM-Fallback ohne
  Platte) — Theme, Akzent, Skala und Uhrformat überleben den
  QEMU-Neustart. tests/speedfs_platte.rs führt den Beweis mit
  Boot-Zähler und Theme-Testwert über echte Neustarts; dazu ein
  Lib-Roundtrip über simuliertes umount+mount. Die Ortswahl trifft
  zentral fs::persistenter_pfad(platte, ram) — EINE Abstraktion,
  kein if-Wildwuchs
- Umzug auf die Platte: Papierkorb -> /platte/papierkorb,
  Explorer-Startordner und SpeedText-Dialoge -> /platte/heim
  (jeweils mit RAM-Fallback)
- Rotierendes Kernel-Log (src/protokoll.rs): jede println!-Ausgabe
  landet zusätzlich in einem RAM-Puffer (Blatt-Lock, 64-KiB-
  Fenster); der Log-Schreiber-Task flusht sekündlich nach
  /platte/system/log.txt — write_at ans Dateiende, bei 64 KiB
  Rotation per rename nach log.alt.txt. Bewusst Puffer+Task statt
  synchronem Schreiben aus _print (ABBA-Deadlock KONSOLE/VFS)
- Einstellungen-App, neue Seite "Speicher": Laufwerksliste
  (Modell, Größe, Schreibschutz), Mount-Status mit frei/gesamt aus
  der SpeedFS-Bitmap (neue Trait-Methode FileSystem::speicher_info),
  sync-Knopf und pruefe.speedfs-Knopf (hängt kurz aus, prüft,
  hängt wieder ein) mit Ergebnis-Dialog; Startbreite 700
- Runner-Option SPEEDOS_OHNE_DATENPLATTE=1 (Boot ohne Platte —
  der RAM-Fallback-Pfad, manuell verifiziert: sauberer Boot,
  alles im RAM); Deadlock-Lehre dokumentiert: ist_gemountet/
  persistenter_pfad nie in mit_fs-Closures auswerten
- 131 Lib-Tests (u. a. Log-Rotation, Puffer-Fenster, Einstellungs-
  Roundtrip) + alle Integrationstests grün

### SpeedFS wird erwachsen: rename überall, sync-Kette, fsck, Folter-Test
- Explorer-Ausschneiden+Einfügen nutzt jetzt `fs::verschieben_rekursiv`
  (echtes atomares rename) statt kopieren+löschen — auf einer echten
  Platte wäre das Kopieren untragbar gewesen; nur über die
  Mount-Grenze kopiert weiterhin der VFS-Fallback. Format-Doc §7
  präzisiert: Nach einem rename-Absturz existiert die Datei IMMER
  (nie in keinem Ordner), schlimmstenfalls kurz in beiden —
  `pruefe.speedfs` meldet so einen Doppel-Eintrag als Befund
- sync-Kette komplett: `fs::sync()` → alle gemounteten Dateisysteme
  → BlockDevice (ATA FLUSH CACHE). Neuer Shell-Befehl `sync`;
  SpeedText-Speichern ruft sync automatisch (ein sync-Fehler zählt
  wie ein Schreibfehler und erscheint als Dialog), die
  Einstellungen taten es schon
- `pruefe.speedfs [--repariere]` — unser fsck (Format-Doc §10, nur
  ungemountet): Baum-Scan ab der Wurzel prüft Verzeichnis-Einträge
  gegen die Inode-Tabelle, Blockzeiger gegen Datenbereich und
  Doppel-Referenzen, Größen gegen die Zeiger-Belegung; die Bilanz
  gegen die Bitmap findet LECKS (belegt, aber unreferenziert —
  der erlaubte Absturz-Schaden), --repariere gibt sie frei.
  DEFEKTE werden nur gemeldet, nie automatisch repariert
- DER FOLTER-TEST: `AbsturzDisk` verwirft alle Schreibvorgänge nach
  einem Budget N (Präfix-Semantik eines Stromausfalls); eine Serie
  aus create/write/rename/delete wird an JEDEM N abgeschnitten,
  neu gemountet und geprüft — Ergebnis: 72 Abschneide-Punkte,
  58 mit (reparierbaren) Lecks, 0 kaputte Metadaten. Die
  Ordering-Disziplin aus §7 ist damit maschinell belegt
- 128 Lib-Tests + alle Integrationstests grün

### SpeedFS: das eigene Dateisystem-Format (Design zuerst!)
- Neues Design-Dokument `docs/speedfs-format.md` — geschrieben VOR
  dem Code: Superblock ("SPFS", Version 1) | Block-Bitmap |
  Inode-Tabelle | Datenblöcke in 4-KiB-Blöcken, alles Little-Endian.
  Inodes (128 B): Typ, Größe, Zeitstempel, 22 direkte + 1 einfach-
  indirekter Blockzeiger (max. Dateigröße 1046 Blöcke ≈ 4,09 MiB,
  Rechnung im Dokument); Verzeichnisse als Byte-Listen
  [Inode u32 | Länge u8 | Name]. Absturz-Analyse in §7: KEIN
  Journal, stattdessen Ordering-Disziplin (Belegen vor Benutzen,
  Inhalt vor Verweis, Entkoppeln vor Freigeben) — nach einem
  Absturz schlimmstenfalls Block-Lecks, nie falsche Metadaten
- Neue Implementierung `src/fs/speedfs.rs` auf dem BlockDevice-Trait
  (läuft auf RamDisk UND ATA): mkfs (formatieren), mounten/aushängen,
  alle FileSystem-Trait-Methoden (lesen, schreiben, read_at/write_at,
  liste, mkdir, loeschen, stat, rename, sync). Block-Cache mit
  WRITE-THROUGH (bewusst einfach und ehrlich — Entscheidung in
  CLAUDE.md, Write-Back ist Serie-5-Stoff). Neue Fehler: Voll,
  DateiZuGross, KeinSpeedFs
- Aus dem Root-Mount wurde eine MOUNT-TABELLE: Wurzel-RamFs plus
  Präfix-Mounts, selbst ein FileSystem — mit_fs() und alle Befehle
  blieben unverändert; rename über die Mount-Grenze fällt in
  fs::verschieben auf kopieren+löschen zurück (MountGrenze)
- Neue Shell-Befehle: `mkfs.speedfs` (Sicherheitsabfrage: nur mit
  Argument JA, nie bei gemountetem /platte), `mount`, `umount` —
  danach arbeiten dir, type, write, copy, tree, cd, SpeedText …
  transparent auf /platte, ohne einen einzigen Sonderfall
- 9 neue SpeedFS-Lib-Tests auf der RamDisk (Superblock-/Inode-/
  Verzeichnis-Roundtrips, Datei über Blockgrenzen, indirekter
  Block, Verzeichnis-Blocküberlauf, Bitmap-Bilanz beim Löschen,
  rename-Semantik, Wiedermount) — 127 Lib-Tests gesamt, alle grün
- Persistenz-Beweis 2.0 (`tests/speedfs_platte.rs`): Eine ECHTE
  DATEI (/platte/beweis.txt), über das VFS geschrieben, überlebt
  den QEMU-Neustart; die Roh-Sektor-Tests aus ata_platte.rs leben
  jetzt am Platten-ENDE, damit sie das Dateisystem nicht anfassen

### ATA-PIO-Treiber: SpeedOS spricht mit einer echten Platte
- Neues `src/ata.rs`: ATA im PIO-Modus über die Legacy-Ports des
  Primary-Kanals (0x1F0/0x3F6) — gepollt mit TSC-Timeout, keine
  PCI-Enumeration, Kanal-Interrupts aus (nIEN). IDENTIFY liest
  Modell und Kapazität (reine, unit-getestete Dekoder), Lesen/
  Schreiben über LBA28 (max. 128 GiB; 256 Sektoren pro Kommando,
  größere Aufträge zerlegt der Treiber), FLUSH CACHE als sync().
  Implementiert das `BlockDevice`-Trait der Serie-4-Naht
- SICHERHEITSREGEL: Die Boot-Platte (Primary Master) ist PER
  KONSTRUKTION schreibgeschützt — nur die Daten-Platte (Primary
  Slave) bekommt Schreibrechte; Verstöße melden den neuen
  `IoFehler::Schreibgeschuetzt` (dazu neu: `Zeitueberschreitung`)
- Runner: `cargo run` hängt automatisch ein persistentes 64-MiB-Image
  `speedos-daten.img` als zweite IDE-Platte an (beim ersten Start
  angelegt, gitignored); Tests bekommen ein EIGENES
  `speedos-daten-test.img`, damit sie nie Nutzerdaten überschreiben
- Neue Shell-Befehle: `platten` (erkannte Laufwerke mit Modell,
  Größe, Schreibschutz) und `blocktest <lba>` (klassischer Hexdump
  eines Sektors der Daten-Platte)
- ERSTER PERSISTENZ-BEWEIS: tests/ata_platte.rs schreibt ein
  Generationen-Muster in Sektor 1000, und der nächste Testlauf —
  nach QEMU-Neustart! — findet es intakt wieder
  (`[PERSISTENZ-BEWEIS]`-Zeile in der seriellen Ausgabe). Dazu:
  Roundtrip über die 256-Sektoren-Grenze, Boot-Platten-Schreibschutz,
  leerer Steckplatz antwortet in ~130 µs statt zu hängen
- 3 neue Unit-Tests (Laufwerkswahl-Byte, IDENTIFY-Dekoder) und
  5 Integrationstests — 118 Lib-Tests insgesamt, alle grün

### Serie-4-Auftakt: BlockDevice-Naht + VFS-Erweiterung
- Neues `src/fs/block.rs`: das schmale `BlockDevice`-Trait
  (sektor_groesse, anzahl_sektoren, lese_sektoren, schreibe_sektoren,
  sync — alles `Result<_, IoFehler>`, LBA-Adressierung, Puffer =
  Sektor-Vielfaches) plus `RamDisk` als Vec-basierte
  Referenz-Implementierung. Die Naht entsteht BEWUSST vor dem ersten
  echten Treiber — Disk-Dateisysteme reden nur mit diesem Trait
- VFS-Trait erweitert: `read_at`/`write_at` (Offset-basiert,
  POSIX-read-Semantik bzw. Nullbyte-Lücken), `stat` (`Metadaten` mit
  Typ, Größe, erstellt/geaendert aus der RTC/zeit-API), `rename` als
  ATOMARE Primitive (validieren, dann entnehmen+einfügen; Ziel-Datei
  wird ersetzt, Ziel-Verzeichnis/eigener Teilbaum sind Fehler) und
  `sync`; `FsFehler::Io(IoFehler)` transportiert Geräte-Fehler
- RamFs-Knoten tragen jetzt Metadaten (erstellt/geaendert);
  `fs::verschieben`/`verschieben_rekursiv` laufen über rename statt
  kopieren+löschen (atomar, kann jetzt auch ganze Ordner); `dir` und
  die Explorer-Statusleiste zeigen echte Zeitstempel
  (einstellungen::stempel_text, gleicher Anzeige-Offset wie die Uhr)
- Fehler sichtbar: SpeedText meldet Lade-/Speicherfehler als Dialog
  (`ui::dialog::fehler`, Mantel um bestaetigung()); Einstellungen
  rufen nach dem Schreiben fs::sync()
- 8 neue Tests (RamDisk-Roundtrip/Grenzen, read_at/write_at-Grenzen,
  rename-Semantik, stat-Zeitstempel, verschieben+sync,
  SpeedText-Fehlerdialog) — 115 Lib-Tests insgesamt, alle grün

### Serie-3-Abschluss (Qualitäts-, Performance- und Speicher-Pass)
- Neue Tests für die kritischsten App-Schicht-Lücken: Event-Routing
  durch VERSCHACHTELTE Container (Klick/Taste/MausRaus über zwei
  Ebenen), Fokus über Fenster-Wechsel hinweg (Widget-Fokus bleibt
  pro Fenster erhalten), Sitzungs-Zuordnung der Terminals
  (fokus_terminal_sitzung als testbare Manager-Methode)
- Performance-Pass mit neuem Serie-3-Berichts-Test (5 Fenster:
  2 Terminal-Sitzungen, Explorer, Task-Manager mit vollem Graph,
  SpeedText mit 60 Zeilen; A/B alt-gegen-neu im SELBEN Lauf, damit
  die WHPX/TCG-Lotterie die Zahlen nicht verfälscht):

  | Szenario (720p)      | vorher | nachher |
  |----------------------|--------|---------|
  | Terminal-Ausgabe     | 555 us | 275 us  |
  | Editor-Tippen        | 466 us | 403 us  |
  | Vollbild 5 Fenster   | 544 us | (unverändert) |

  Optimierung 1: Das Terminal-Raster führt DIRTY-ZEILEN — eine
  Prompt-Ausgabe rendert nur ihre Rasterzeile in den persistenten
  Fenster-Puffer, und terminal_schreiben meldet dem Compositor nur
  noch den Zeilen-STREIFEN statt der Fensterfläche (2x schneller;
  Scroll/Resize/Theme markieren weiterhin alles).
  Optimierung 2: SpeedText baut beim Tippen den Widget-Baum NICHT
  mehr neu — die neue StatusZeile liest Zeile/Spalte/Zeichen live
  aus dem geteilten Puffer, der Titel wird nur bei echtem Wechsel
  gemeldet. Ehrliche Bilanz: nur -14 %, denn der Fresser ist das
  Voll-Zeichnen+Komponieren der Fensterfläche pro Taste — Teil-
  Rect-Compositing für Widget-Fenster ist als Serie-4-Kandidat
  notiert
- Speicher-Pass: App-Zyklen-Test (Terminal + alle vier Trait-Apps
  20x öffnen/benutzen/schließen) — Heap exakt stabil, KEIN Leck
  gefunden (die Besitz-Ketten Arc-Puffer/Sitzungs-Registry/
  Fenster-Puffer geben vollständig frei)
- unsafe-Audit: Die GESAMTE Serie 3 (Toolkit, vier Apps,
  Terminal-Sitzungen, Task-Übersicht) kommt ohne einen einzigen
  neuen unsafe-Block aus — alle 82 unsafe-Stellen liegen in den
  Hardware-Modulen aus Serie 1/2
- Toolkit-Review nach drei echten Apps: BEWÄHRT haben sich
  Nachricht-IDs mit Basis-Kodierung, Zustand-in-App + aufbau(),
  die NachLock-Disziplin (null Deadlocks in der ganzen Serie) und
  das Arc-Muster für heißen Widget-Zustand. UMSTÄNDLICH waren das
  Box-Cast-Boilerplate (-> neuer Helfer ui::w()), die duplizierte
  Sekunden-Tick-Logik (-> ui::app::SekundenTick, Einstellungen +
  Task-Manager umgestellt) und der Textfeld-Zustandsverlust bei
  Neu-Aufbauten (Muster dokumentiert; echte Lösung = geteilter
  Zustand wie im Editor). Notiert, nicht geändert: Tab ist global
  Fokus-Taste (der Editor kann keine Tabs einfügen), Nachricht-
  Basen bleiben Handarbeit
- docs/serie4-bestandsaufnahme.md: die ehrliche Bestandsaufnahme
  für Serie 4 — VFS-Lücken (read_at/write_at, stat, rename, sync,
  BlockDevice-Trait), Treiber-Empfehlung (ATA PIO zuerst, dann
  virtio-blk; AHCI vertagt), PIC trägt Serie 4 (APIC/MSI als
  eigener Meilenstein), USB-Stick-Boot: Booten ja, PS/2-Emulation
  ist das Risiko, Persistenz auf dem Stick braucht xHCI (Blocker)

### Hinzugefügt (Terminal-Sitzungen + SpeedText — die letzte App der Serie)
- TERMINAL-SITZUNGEN (das Ein-Terminal-Limit fällt): Jedes
  Terminal-Fenster trägt eine Sitzungs-Id, pro Sitzung läuft ein
  EIGENER Shell-Task (shell/sitzung.rs). Der neue Eingabe-Router ist
  der einzige KeyStream-Leser und wirft Tasten in die Queue der
  fokussierten Sitzung (lock-freie Queue + AtomicWaker); die
  _print-Umleitung wurde zum Sitzungs-Konzept: Jede Shell schreibt
  in IHR Fenster (AUSGABE_SITZUNG um die synchrone Verarbeitung —
  race-frei, weil kooperativ ohne await dazwischen), Kernel-Log geht
  ans designierte HAUPT-Terminal und wird GEPUFFERT, wenn keins
  offen ist (Nachlieferung beim nächsten Öffnen). Zwei Terminals
  arbeiten unabhängig (tree hier, tippen dort); Schließen trägt die
  Sitzung aus — der Shell-Task endet sauber am await-Punkt, das
  Haupt-Terminal vererbt seine Rolle
- src/speedtext.rs — SpeedText, der Texteditor:
  * Mehrzeiliges Editor-Widget (ui/texteditor.rs): TextPuffer als
    Zeilen-Vec (im Kommentar begründet: KiB-Dateien brauchen keinen
    Rope), Einfügen/Löschen über Zeilengrenzen, Pfeile/Pos1/Ende/
    Bild-Tasten, Klick setzt den Cursor, vertikales Scrolling mit
    ziehbarem Balken, Zeilennummern-Spalte, blinkender Cursor. Der
    Puffer lebt GETEILT (Arc<Mutex>) zwischen App und Widget — so
    überlebt der Text die Widget-Neu-Aufbauten der Statuszeile
  * Datei öffnen/speichern übers VFS: Strg+S speichert (ohne Pfad:
    Speichern-unter-Dialog), Strg+O öffnet den neuen DATEI-DIALOG
    (ui/dialog.rs, wiederverwendbar: Ordner-ScrollListe +
    Pfad-Eingabe + OK/Abbrechen; Doppelklick navigiert/wählt) —
    der Explorer-Doppelklick auf Dateien öffnet jetzt SpeedText,
    der alte Nur-Lese-Betrachter ist Geschichte
  * Titelleiste zeigt "name.txt * - SpeedText" (Stern = ungespeichert,
    via neuem AppReaktion.titel); Schließen mit Änderungen fragt nach
    (Speichern/Verwerfen/Abbrechen) über den generischen
    BESTÄTIGUNGS-Dialog + neuen App::schliessen_abfragen-Hook und
    AppReaktion.schliessen
  * Statusleiste: Zeile:Spalte, Zeichenzahl, Änderungs-Status
- Unit-Tests: TextPuffer (Einfügen/Löschen über Zeilengrenzen,
  Umlaute, Cursor-Klemmen, Scroll-Folgen, Roundtrip), Datei-Roundtrip
  über das Test-VFS, Datei-Dialog-Zustandsmaschine, Schließen-Dialog-
  Logik, Terminal-Sitzungen (Unabhängigkeit, Haupt-Vererbung,
  Lebenszyklus, Log-Puffer-Deckel)

### Hinzugefügt (Task-Manager: App + Executor-Übersicht)
- Executor-Erweiterung (src/task/uebersicht.rs): Jeder Task bekommt
  beim Spawnen einen NAMEN, seine Id und den Startzeitpunkt; eine
  globale Registry (Blatt-Lock) führt die Schatten-Buchhaltung, die
  heißen Zähler (Polls, Wecken, wach/schläft) sind Atomics in einem
  Arc, das sich Executor, Waker und Registry teilen — der Waker
  feuert aus Interrupt-Handlern und darf keinen Lock nehmen.
  momentaufnahme() liefert die sortierte Liste (Id, Name, Art,
  Laufzeit, Status, Zähler), anzahl() die Gesamtzahl. Alle
  bestehenden spawn-Aufrufe tragen jetzt sinnvolle Namen
  (SpeedShell (Terminal), Compositor, Desktop-Uhr, PS/2-Maus, ...)
- CPU-Metrik: Der Executor misst per TSC die Zeit in
  run_ready_tasks (Arbeit) vs. im hlt-Schlaf (Ruhe) und verbucht
  beides in einem gleitenden Fenster aus 10 Eimern à 100 ms —
  cpu_auslastung_prozent() ist die ehrliche System-Auslastung über
  ~1 s (reine, unit-getestete Fenster-Logik)
- src/taskmanager.rs — die Task-Manager-App: Kopfzeile mit CPU-%,
  Live-Graph der letzten 60 s (Linien-Rendering mit Spalten-MAXIMUM
  beim Downsampling — Spitzen bleiben sichtbar) und Heap-Belegung;
  Tabelle (Id, Name, Art Kernel/Fenster/Demo, Laufzeit, Polls/s,
  wach/schläft) mit sekündlichem tick-Update, Auswahl überlebt als
  Task-ID; "Task beenden" wirkt nur auf beendbare Tasks (Demos),
  bei geschützten Kernel-/Fenster-Tasks ist der Knopf gedimmt
  (neues Button::mit_deaktiviert). EHRLICH dokumentiert (Info-Zeile
  + Code): Kooperative Tasks werden beim nächsten Executor-
  Durchlauf an ihrem await-Punkt FALLEN GELASSEN (Drop), nicht
  abgeschossen — "Demo-Task starten" spawnt einen beendbaren
  Zähler-Task zum gefahrlosen Ausprobieren
- Neues Icon ICON_TASKS (Balkendiagramm); Registry-Eintrag
  "Task-Manager" im Startmenü
- Unit-Tests: Momentaufnahme-Konsistenz (Zähler, Sortierung,
  Beenden-Schutz), Auslastungs-Gleitfenster mit synthetischen
  Zahlen (Rotation, Zeitsprünge, 100-%-Deckel), Graph-Downsampling
  (Spalten-Maximum, Achsen-Abbildung, Grenzfälle), Laufzeit-Format

### Hinzugefügt (Einstellungen: App + persistenter Store)
- src/einstellungen.rs, Teil 1 — der persistente Einstellungs-Store:
  Schlüssel=Wert-Datei /system/einstellungen.txt im VFS (parsen/
  serialisieren als reine, unit-getestete Funktionen; Kommentare und
  kaputte Zeilen werden toleriert), typisierter Zugriff (hole_/
  setze_zahl/bool/text — jedes setze_* speichert SOFORT), Laden +
  Anwenden beim Boot (main.rs nach fs::init). Die API-Naht für
  Serie 4: Kommt das Disk-Dateisystem, wird nur das gemountete VFS
  getauscht — dann überleben die Werte auch echte Neustarts
- Teil 2 — die Einstellungen-App (ui::App, Kategorien-ScrollListe
  links, Inhaltsseiten rechts):
  * Personalisierung: Theme Dunkel/Hell, Akzentfarbe aus 6er-Palette
    (NEU: unabhängig von Hell/Dunkel, mit passender Farbvariante je
    Theme — theme::aktuell() liefert jetzt eine Kopie mit
    eingesetztem Akzent), Desktop-Hintergrund aus 5 Verlauf-Presets
    (theme::hintergrund_verlauf; der Compositor-Hintergrund-CACHE
    wird über hintergrund_neu invalidiert)
  * Anzeige: Auflösung (nur Anzeige, mit SPEEDOS_AUFLOESUNG-Hinweis),
    UI-Skalierung 1.0/1.5/2.0 direkt wählbar, Cursor-Blinktempo
    (wirken live: Textfeld-Cursor + Konsolen-Blink-Task lesen
    einstellungen::cursor_blink_ms)
  * Datum & Uhrzeit: Live-Uhr, UTC-Offset in 30-min-Schritten und
    12/24h-Format für die Systray-Uhr (jetzt_lokal/uhrzeit_text;
    dokumentierte Annahme: die RTC liefert in QEMU die Host-
    LOKALZEIT, der Offset ist eine reine Anzeige-Verschiebung)
  * Info: Logo, Version aus Cargo.toml (Compile-Zeit-env!),
    Auflösung, Speicher frei/gesamt (frame_statistik), TSC-Frequenz,
    Uptime live (tick beim Sekundenwechsel), Task-Anzahl (neues
    Atomic im Executor)
- Alle Optionen wirken SOFORT und überleben Fenster-zu/auf — per
  QMP-Fernsteuerung in QEMU verifiziert (Screenshots: Theme, Akzent
  Grün, Ozean-Hintergrund, 12h-Systray, Skalierung; die Datei per
  `type /system/einstellungen.txt` geprüft). Muster dafür: Atomics
  UNTER dem MANAGER-Lock setzen, Neuzeichnen via AppReaktion.danach
  -> fenster::alles_neu_zeichnen()
- Toolkit-Ausbau: Button::mit_aktiv (markierte Wahl in Options-
  Gruppen), Farbfeld- und Icon-Widgets in der App; Registry-Apps
  "Theme wechseln"/"Skalierung" persistieren ihre Wahl jetzt auch
- Gespeicherte UI-Skala schlägt beim Desktop-Start die Auto-Wahl
  nach Bildschirmbreite

### Hinzugefügt (Explorer Teil 2: Dateioperationen)
- Umbenennen mit F2 (der Auswahl-Eintrag wird zur Eingabezeile, die
  App puffert über den taste-Hook; Enter übernimmt, Esc bricht ab),
  Neu-Ordner/Neu-Datei (Werkzeugleiste + Kontextmenü; legt mit
  eindeutigem Namen an und springt direkt in den Umbenennen-Modus),
  Entf verschiebt in den Papierkorb
- Papierkorb /papierkorb: Der Ursprungs-Ordner wird in einer
  METADATEN-Datei (<name>.herkunft) gemerkt — begründet im Code:
  normale VFS-Zugriffe statt Namens-Parser, echter Anzeigename,
  die Ansicht filtert die Metadaten einfach aus. Papierkorb-Ansicht
  mit Wiederherstellen (Konflikt -> " (2)") und Endgültig-Löschen;
  Entf im Papierkorb löscht endgültig
- Kopieren/Ausschneiden/Einfügen mit Strg+C/X/V — die Ablage ist
  ein GLOBALER Zustand (src/ablage.rs, Grundstein der System-
  Zwischenablage): funktioniert innerhalb eines und zwischen zwei
  Explorer-Fenstern. Rekursives Kopieren/Löschen/Verschieben von
  Ordnern als fs-Helfer (Deadlock-Regel: liste() vor dem Abstieg
  abschließen); Namenskonflikt hängt automatisch " (2)" an.
  Strg-Erkennung: KeyStream dekodiert mit MapLettersToUnicode
  (Strg+C = U+0003 usw.)
- GENERISCHES Kontextmenü-Overlay im Fenster-Manager (Offscreen-
  Puffer + Blit wie Startmenü/Switcher; Empfänger = FensterId,
  Taskleiste/Desktop können es später nutzen): Rechtsklick auf
  Eintrag (Öffnen/Umbenennen/Kopieren/Ausschneiden/Löschen, im
  Papierkorb + Wiederherstellen) und auf freie Fläche (Einfügen/
  Neu/Aktualisieren). Neu dafür: UiEreignis::Rechtsklick,
  ScrollListe::mit_rechtsklick, AppReaktion::menue sowie
  AppReaktion::danach als Box<dyn FnOnce> (Aktionen MIT Daten,
  NachLock::Einmal)
- Aktualisieren-Button (@): Shell und Explorer sehen dasselbe VFS —
  write /test.txt im Terminal, @ im Explorer, Datei ist da
- Doppelklick auf Datei öffnet den minimalen Betrachter
  (BetrachterApp: Pfad, Zeilen-ScrollListe, nur Lesen)
- Unit-Tests: eindeutiger_name, rekursives Kopieren/Löschen (echtes
  Test-VFS), Papierkorb-Roundtrip (Herkunft-Metadaten,
  Wiederherstellen, Endgültig)

### Hinzugefügt (Explorer — die erste echte App auf dem Toolkit)
- src/explorer.rs: ExplorerApp (ui::App) mit Werkzeugleiste
  (Zurück/Vor/Hoch + klickbare Breadcrumbs + KlickFlaeche für den
  Eingabemodus), Ordnerbaum-Spalte (aufklappbar ab /, Klick
  navigiert), Dateiliste (Icons nach Typ, Größen als B/KiB/MiB,
  Ordner-zuerst-Sortierung) und Statusleiste (Eintragszahl,
  Auswahl-Info, Fehlermeldungen)
- Navigation: Doppelklick/Enter öffnet Ordner, Backspace/^-Button =
  hoch, Zurück/Vor-Verlauf wie im Browser (Vorwärts-Historie wird
  beim Abbiegen gekappt), Adressleiste per Klick tippbar (Enter
  navigiert, Esc bricht ab); mehrere Explorer-Fenster laufen als
  eigene App-Instanzen unabhängig — alles per QMP verifiziert
- Toolkit-Ausbau dafür: ScrollListe mit Index-kodierten Nachrichten
  (BASIS+Index — Apps erfahren, WELCHER Eintrag), Fokus +
  Pfeiltasten/Enter-Navigation, Auswahl-Erhalt über Neu-Aufbauten
  (Scroll als Cell, auswahl_sichtbar), konfigurierbarem Layout;
  BoxContainer::mit_flex; UiFenster::fokus_initial;
  App::taste-Hook (App-Shortcuts/Eingabemodi VOR den Widgets)
- 4 neue Unit-Tests: Breadcrumb-Zerlegung + Elternpfad,
  Größen-Formatierung, Ordner-zuerst-Sortierung, Browser-Verlauf

### Hinzugefügt (UI-Skalierung + Dirty-Rect-Compositing — die 4K-Baustellen)
- UI-Skalierung 1.0/1.5/2.0 (in Halben, soft-float-frei): metrik()
  liefert die skalierte Metrik, Schrift mappt auf die vorgerasterten
  Fonts 16/24/32 (neues Cargo-Feature size_24). Boot-Standard nach
  Breite (>=2560 -> 1.5, >=3840 -> 2.0), Laufzeit-Umschaltung über
  die neue Registry-App "Skalierung" (Mechanik wie Theme-Wechsel).
  4K-Screenshot mit Faktor 2.0: docs/screenshots/desktop-4k-skaliert
- Dirty-Rect-Compositing: Änderungen melden ihre Fläche (Drag/Resize
  alte+neue, Uhr-Tick nur den Systray, Menü/Switcher ihre Panels,
  max. 16 Rects mit Vollbild-Fallback); der Compositor komponiert
  je Rect mit Clip (Fenster ohne Schnitt übersprungen) und presentet
  nur diese Bereiche. Desktop-Verlauf als byte-identischer Cache im
  DoppelPuffer (Wiederherstellen = memcpy pro Zeile; die erste
  Fassung als Farbe-Array war LANGSAMER als der Gradient — Lehre:
  Cache immer im Zielformat)
- Messwerte (Berichts-Test, drei Szenarien, warm):

  | Szenario     | 720p vorher | 720p nachher | 4K vorher | 4K nachher |
  |--------------|-------------|--------------|-----------|------------|
  | Vollbild     | ~1,2 ms     | ~1,1 ms      | ~9,3 ms   | ~9,3 ms    |
  | Uhr-Tick     | = Vollbild  | 0,25 ms      | = Vollbild| 0,31 ms    |
  | Fenster-Drag | = Vollbild  | 0,37 ms      | = Vollbild| 0,41 ms    |
  (vorher war JEDER dirty Frame ein Vollbild-Frame; Ziel
  "Uhr-Tick bei 4K unter 1 ms" mit 0,31 ms erreicht)
- Taskleiste skaliert sauber mit (Logo waechst mit der Leiste,
  Uhr/Datum als zentrierter Block statt fester Offsets)

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
