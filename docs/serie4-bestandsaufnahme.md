# Bestandsaufnahme für Serie 4 — Persistenz und erste Netzwerk-Schritte

Stand: Ende Serie 3 (Juli 2026). Serie 3 hat die App-Schicht geliefert
(UI-Toolkit, Explorer, Einstellungen, Task-Manager, SpeedText,
Terminal-Sitzungen). Serie 4 soll SpeedOS **persistent** machen
(Block-Treiber + Disk-Dateisystem) und die ersten Netzwerk-Schritte
gehen. Diese Bestandsaufnahme beantwortet die vier Kernfragen ehrlich —
inklusive dessen, was NICHT reicht.

## (a) Was fehlt dem VFS-Trait für echte Block-Devices?

Das heutige Trait (`src/fs/mod.rs`) ist eine **Ganzdatei-API**:
`lesen()` liefert die komplette Datei als `Vec<u8>`, `schreiben()`
ersetzt sie komplett. Für RamFs mit KiB-Dateien ist das richtig; für
ein Disk-Dateisystem fehlt:

1. **Offset-I/O**: `read_at(pfad, offset, puffer)` /
   `write_at(pfad, offset, daten)` — sonst liest jeder Editor-Save
   und jedes Log-Append die ganze Datei. Der TextPuffer und die
   Einstellungen kämen zwar weiter mit Ganzdatei-I/O aus (bewusst
   kleine Dateien), aber ein `dd`- oder Kopier-Befehl über eine
   4-MiB-Datei braucht Häppchen.
2. **Metadaten**: Es gibt nur `node_typ()` und die Größe im
   Verzeichnis-Eintrag. Ein `stat(pfad) -> Metadaten` (Größe,
   Zeitstempel erstellt/geändert — die RTC liefert sie jetzt!) gehört
   ins Trait, sonst rät der Explorer weiter.
3. **Umbenennen als Primitive**: Heute ist Verschieben
   kopieren+löschen (fs-Helfer). Auf einer Disk ist `rename()` eine
   Verzeichnis-Operation und muss atomar im Dateisystem passieren —
   sonst kostet das Umbenennen einer 100-MiB-Datei 200 MiB I/O.
4. **Handles statt Pfade (mittelfristig)**: Jede Operation löst heute
   den Pfad neu auf. Für Serie 4 verkraftbar (Baumtiefe ist klein),
   aber `open() -> Handle` mit Positionszeiger ist die Voraussetzung
   für alles Spätere (User-Space-Dateideskriptoren!). Empfehlung:
   JETZT nur read_at/write_at/stat/rename ergänzen, Handles erst mit
   den Syscalls einführen — zwei Nähte auf einmal sind eine zu viel.
5. **Fehler & Haltbarkeit**: `FsFehler` braucht eine `IoFehler`-
   Variante (Gerät antwortet nicht, CRC kaputt) und das Trait ein
   `sync()` — RamFs implementiert es als No-op, das Disk-FS schreibt
   seine Puffer raus. Ohne sync-Naht gibt es später keinen sauberen
   `neustart`-Befehl.
6. **Darunter, nicht im VFS**: ein eigenes, schmales
   `BlockDevice`-Trait (`block_lesen(lba, &mut [u8; 512])`,
   `block_schreiben`, `anzahl_bloecke()`, `block_groesse()`).
   Das Disk-FS konsumiert BlockDevice; das VFS bleibt davon
   unberührt — genau die Schichtung, für die die VFS-Naht seit
   Serie 1 gebaut wurde.

## (b) Der Weg zum Block-Treiber: ATA, AHCI oder virtio-blk?

**Empfehlung: ATA PIO zuerst, virtio-blk als zweiter Schritt, AHCI
gar nicht (vorerst).**

- **ATA PIO (IDE)** — der Lern- und Einstiegstreiber:
  QEMU hängt `-drive`-Platten standardmäßig an den emulierten
  IDE-Controller; unser Runner muss nur eine ZWEITE Daten-Platte
  anhängen (die Boot-Platte bleibt dem UEFI-Bootloader). PIO braucht
  NUR Port-I/O (0x1F0-0x1F7) — keine PCI-Enumeration, kein DMA, kein
  MSI. Mit Polling (BSY/DRQ-Bits) funktioniert er sogar komplett ohne
  Interrupts; IRQ 14/15 sind am PIC frei, wenn wir sie wollen.
  Er ist langsam (~ein Sektor pro Handshake), aber für ein
  RamFs-großes Disk-FS völlig ausreichend — und jede Zeile lehrt
  echtes Hardware-Protokoll. Passt zum Projektstil (PS/2-Maus lief
  genauso: gepollte Handshakes, dann IRQ).
- **virtio-blk** — der richtige zweite Schritt: eine Größenordnung
  schneller (DMA-Ringe statt Port-Handshakes) und der Einstieg in die
  virtio-Welt, die uns bei **virtio-net gleich wieder begegnet**:
  PCI-Enumeration, BAR-Mapping (map_page_zu existiert!), Virtqueues.
  Der Aufwand steckt in der EINMALIGEN virtio-Infrastruktur — die
  sich dann für Netz, Konsole, RNG wiederverwenden lässt.
- **AHCI (SATA)** — erst für echte Hardware relevant: Command Lists,
  FIS-Strukturen, MMIO-Register — deutlich mehr Spezifikation bei
  null Zusatznutzen in QEMU. Moderne echte Rechner haben ohnehin eher
  NVMe als AHCI. Verschieben, bis echte Hardware das Ziel ist.

Reihenfolge Serie 4: BlockDevice-Trait → ATA-PIO-Treiber (gepollt)
→ Disk-Dateisystem (eigenes, einfaches Format oder FAT32 — FAT32 hat
den Charme, dass der Host die Daten-Platte direkt mounten kann!)
→ PCI-Enumeration + virtio-blk, wenn das FS steht.

## (c) Interrupt-/Treiber-Fundament: reicht PIC/PS2 noch?

**Ja — für Serie 4 trägt der PIC noch.** Die ehrliche Rechnung:

- Frei am PIC: IRQ 14/15 (IDE) sind unbelegt, IRQ 10/11 für
  PCI-INTx-Geräte (virtio legacy, e1000) auch. Timer (0), Tastatur
  (1), Maus (12) belegen drei von fünfzehn.
- virtio-blk und virtio-net funktionieren mit Legacy-INTx über den
  PIC (und zur Not per Polling im Compositor-Takt) — MSI/MSI-X ist
  eine Optimierung, keine Voraussetzung.
- **Der APIC-Umstieg wird fällig, wenn eines von drei Dingen kommt**:
  SMP (mehrere Kerne brauchen den LAPIC sowieso — er ist bei uns
  explizit deaktiviert!), NVMe/moderne Geräte, die nur MSI-X
  sprechen, oder mehr Geräte als PIC-Leitungen. Empfehlung: APIC als
  EIGENER Meilenstein am ENDE von Serie 4 oder Anfang Serie 5
  („User Space"), nicht als Voraussetzung — sonst blockiert ein
  Infrastruktur-Umbau die Persistenz.
- PS/2 bleibt, wie es ist: Der 8042 stört den Disk-Pfad nicht.

## (d) Was blockiert die USB-Stick-Bootfähigkeit auf echter Hardware?

Kurzfassung: **Booten wird vermutlich funktionieren, die Eingabe ist
das Risiko, die Persistenz auf dem Stick ist der echte Blocker.**

1. **Booten selbst: wenig Blocker.** bootloader 0.11 erzeugt ein
   GPT/UEFI-Image — auf einen Stick geschrieben (`dd`/Rufus) bootet
   das auf UEFI-Rechnern; die Firmware lädt Kernel + Framebuffer
   (GOP), genau wie OVMF. Secure Boot muss aus sein (unser Image ist
   unsigniert).
2. **Eingabe: hängt an der Firmware.** Wir sprechen NUR PS/2 (8042).
   Auf echter Hardware existiert der 8042 oft nur noch als
   Firmware-Emulation („USB Legacy Support") — und manche UEFI-
   Firmware schaltet die Emulation ab, sobald ein OS ExitBootServices
   ruft. Dann bootet SpeedOS in einen Desktop ohne Tastatur und Maus.
   Der saubere Ausweg heißt USB-HID über xHCI — das ist ein GROSSES
   Projekt (eigene Serie), kein Serie-4-Nebenprodukt.
3. **Timer/Uhr: unkritisch.** PIT, RTC und TSC existieren auf echter
   Hardware (Chipsatz-Emulation); die TSC-Kalibrierung ist gebaut,
   CPUID-invariant wird geloggt.
4. **RAM-Grenze: prüfenswert.** Der Bitmap-Frame-Allocator verwaltet
   maximal 1 GiB — echte Rechner haben mehr. Er muss den Rest sauber
   IGNORIEREN (deckeln statt panicken); das gehört als kleiner Test
   in Serie 4.
5. **Persistenz auf dem Stick: der echte Blocker.** Der Stick ist ein
   USB-Mass-Storage-Gerät — ohne xHCI-Treiber können wir NICHT auf
   ihn schreiben. Serie-4-Persistenz zielt deshalb auf QEMU-Platten
   (ATA/virtio); auf echter Hardware bleibt SpeedOS vorerst ein
   Live-System ohne Speichern. Das ist die ehrliche Ansage.

## Priorisierte Serie-4-Reihenfolge (Vorschlag)

1. VFS-Naht erweitern (read_at/write_at, stat, rename, sync,
   IoFehler) + BlockDevice-Trait — RamFs zieht mit (Tests!).
2. Runner: zweite Daten-Platte (`SPEEDOS_DATEN=daten.img`).
3. ATA-PIO-Treiber, gepollt, mit Timeouts (PS/2-Muster).
4. Disk-Dateisystem (FAT32 empfohlen: Host-lesbar, Spezifikation
   überschaubar) hinter dem FileSystem-Trait; Einstellungen +
   SpeedText überleben den Reboot — DER Meilenstein.
5. PCI-Enumeration + virtio-blk (Basis für virtio-net).
6. Netzwerk-Einstieg: virtio-net + minimaler Stack (ARP, ICMP-Ping)
   — mit PIC-INTx, ohne APIC-Umbau.
7. (Wenn Luft: APIC/MSI-Meilenstein als Vorbereitung auf Serie 5.)
