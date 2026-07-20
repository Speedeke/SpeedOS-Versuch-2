# SpeedFS — das On-Disk-Format (Version 1)

Das eigene Dateisystem von SpeedOS. Dieses Dokument ist die
**verbindliche Spezifikation** des Platten-Formats — es entstand VOR
der Implementierung (`src/fs/speedfs.rs`), und der Code richtet sich
nach dem Dokument, nicht umgekehrt. Wer das Format ändern will,
ändert ZUERST hier (und erhöht die Version).

Vorbilder: die klassische Unix-Dateisystem-Familie (ext2 in klein) —
Superblock, Block-Bitmap, Inode-Tabelle, Datenblöcke. Bewusst KEIN
Journal (das ist Serie-5+-Stoff): Konsistenz nach einem Absturz wird
über eine strikte **Schreib-Reihenfolge** erreicht (Abschnitt 7).

SpeedFS spricht ausschließlich mit dem `BlockDevice`-Trait
(`src/fs/block.rs`) — es läuft damit unverändert auf der RamDisk
(Tests) und der echten ATA-Platte.

---

## 1. Grundgrößen

| Größe            | Wert    | Begründung |
|------------------|---------|------------|
| Blockgröße       | 4096 B  | 8 ATA-Sektoren; gängige Seiten-/Cluster-Größe |
| Inode-Größe      | 128 B   | 32 Inodes pro Block, ohne Verschnitt |
| Max. Namenslänge | 255 B   | Längenfeld ist 1 Byte (UTF-8-Bytes) |

Die Sektorgröße des Geräts muss 4096 teilen (512er- und
4096er-Sektoren passen). Alle Block-Nummern zählen in
4-KiB-Dateisystem-Blöcken ab Platten-Anfang (Block 0 = Superblock).

**Endianness: alles Little-Endian** (x86-nativ; gelesen/geschrieben
mit `from_le_bytes`/`to_le_bytes` — nie durch Pointer-Casts, damit
das Format unabhängig vom Compiler-Layout ist).

## 2. Platten-Layout

```
Block 0        Superblock
Block 1 ..     Block-Bitmap        (bitmap_bloecke Stück)
danach         Inode-Tabelle       (inode_bloecke Stück)
danach         Datenblöcke         (bis anzahl_bloecke)
```

Alle Bereichsgrenzen stehen im Superblock — Leser rechnen nichts
selbst aus, sie glauben dem Superblock (Vorwärts-Kompatibilität:
eine spätere Version darf die Bereiche anders anordnen).

## 3. Der Superblock (Block 0)

| Offset | Größe | Feld | Wert in v1 |
|--------|-------|------|------------|
| 0  | 4 | Magic | `"SPFS"` (Bytes `53 50 46 53`) |
| 4  | 4 | Version (u32) | 1 |
| 8  | 4 | Blockgröße (u32) | 4096 |
| 12 | 4 | Anzahl Inodes (u32) | Geräte-abhängig (Abschnitt 8) |
| 16 | 8 | Anzahl Blöcke gesamt (u64) | Gerätegröße / 4096 |
| 24 | 8 | Bitmap-Start (u64, Block-Nr) | 1 |
| 32 | 8 | Bitmap-Blöcke (u64) | ⌈Blöcke / 32768⌉ |
| 40 | 8 | Inode-Tabellen-Start (u64) | Bitmap-Start + Bitmap-Blöcke |
| 48 | 8 | Inode-Tabellen-Blöcke (u64) | ⌈Inodes / 32⌉ |
| 56 | 8 | Daten-Start (u64) | Inode-Start + Inode-Blöcke |
| 64 | 4 | Wurzel-Inode (u32) | 1 |
| 68 | … | reserviert | 0 |

Mount prüft: Magic, Version == 1, Blockgröße == 4096, und dass die
Bereiche in die Gerätegröße passen. Alles andere → Fehler
`KeinSpeedFs` (kein Panik-Fall: eine fremde/leere Platte ist normal).

**Versionierung:** Die Version ist eine reine Zahl (keine
Feature-Flags in v1). Ein Treiber mountet nur Versionen, die er
exakt kennt. Format-Änderungen = Version + 1 und neuer Abschnitt in
diesem Dokument.

## 4. Die Block-Bitmap

1 Bit pro Dateisystem-Block: 1 = belegt, 0 = frei. Block n liegt in
Bitmap-Byte `n / 8`, Bit `n % 8` (LSB zuerst — Bit 0 des ersten
Bytes ist Block 0). Ein Bitmap-Block deckt 4096·8 = 32768 Blöcke
(128 MiB) ab.

`mkfs` markiert Superblock, Bitmap und Inode-Tabelle als belegt —
die Bitmap beschreibt also IMMER die ganze Platte, nicht nur den
Datenbereich. Die Allokation sucht First-Fit ab dem Datenbereich.

## 5. Der Inode (128 Bytes)

| Offset | Größe | Feld |
|--------|-------|------|
| 0   | 4  | Typ (u32): 0 = frei, 1 = Datei, 2 = Verzeichnis |
| 4   | 4  | reserviert (0) |
| 8   | 8  | Größe in Bytes (u64) — bei Verzeichnissen die Länge der Eintragsliste |
| 16  | 8  | erstellt (u64, Sekunden seit 1.1.2000 — die zeit-Epoche) |
| 24  | 8  | geändert (u64, gleiche Epoche) |
| 32  | 88 | 22 direkte Blockzeiger (u32, 0 = kein Block) |
| 120 | 4  | einfach-indirekter Zeiger (u32, 0 = keiner) |
| 124 | 4  | reserviert (0) |

* Blockzeiger sind u32 (bis 16 TiB bei 4-KiB-Blöcken — weit mehr als
  LBA28 überhaupt adressiert). Der Wert 0 heißt "kein Block": Block 0
  ist immer der Superblock und kann nie Datenblock sein.
* Der **einfach-indirekte** Zeiger zeigt auf einen Datenblock, der
  selbst 4096/4 = **1024 weitere Blockzeiger** enthält (u32, LE).
* **Maximale Dateigröße:** (22 direkte + 1024 indirekte) Blöcke
  × 4096 B = 1046 × 4096 = **4.284.416 Bytes ≈ 4,09 MiB.**
  Das reicht für unsere Zwecke (Textdateien, Einstellungen,
  Screenshots wären knapp) — doppelt-indirekt wäre eine rein
  additive Erweiterung in einer späteren Version.
* Inode-Nummern: **0 ist ungültig** (Markierung "kein Eintrag"),
  **1 ist das Wurzelverzeichnis**. Inode n liegt im Tabellen-Block
  `inode_start + n/32`, Slot `n % 32`; Slot 0 bleibt ungenutzt.
* Ein Inode (128 B) liegt IMMER vollständig in einem 512-B-Sektor —
  ein Inode-Update ist damit auf Geräteebene atomar (wichtig für
  Abschnitt 7).

## 6. Verzeichnisse

Ein Verzeichnis ist inhaltlich eine "Datei" (gleiche Blockzeiger-
Mechanik), deren Bytes eine Eintragsliste sind:

```
[ Inode-Nr (u32, LE) | Namenslänge (u8) | Name (UTF-8, 1..=255 Bytes) ]  wiederholt
```

* Die Inode-Größe des Verzeichnisses ist die EXAKTE Byte-Länge der
  Liste — es gibt keinen Terminator und kein Padding; der Parser
  liest sequenziell, bis die Größe erreicht ist.
* Einträge liegen unsortiert auf der Platte; `liste()` sortiert im
  RAM (wie beim RamFs).
* Namen: 1–255 UTF-8-Bytes, ohne `/` und nicht `.`/`..` (die kennt
  nur die Pfad-Auflösung im VFS, sie erreichen das Dateisystem nie).
* Ein leeres Verzeichnis hat Größe 0 und KEINE Datenblöcke — auch
  die Wurzel direkt nach mkfs.
* Änderungen schreiben die Liste KOMPLETT neu — in **frische**
  Blöcke (Abschnitt 7). Bei KiB-großen Verzeichnissen ist das
  billig und macht das Absturzverhalten trivial.

## 7. Absturz-Analyse: die Schreib-Reihenfolge

SpeedFS hat **kein Journal**. Stattdessen gilt eine feste Disziplin,
die nach einem Absturz an JEDER Stelle höchstens harmlose Lecks
hinterlässt (Blöcke/Inodes als belegt markiert, aber unreferenziert),
aber **nie Metadaten, die auf falsche oder halbe Daten zeigen**:

> **Regel 1 — Belegen vor Benutzen:** Bitmap-Bits und Inode-Slots
> werden als belegt geschrieben, BEVOR irgendein Zeiger auf sie
> zeigt.
> **Regel 2 — Inhalt vor Verweis:** Datenblöcke (und der indirekte
> Block) sind vollständig geschrieben, BEVOR der Inode auf sie
> zeigt; der Inode ist geschrieben, BEVOR der Verzeichnis-Eintrag
> auf ihn zeigt.
> **Regel 3 — Entkoppeln vor Freigeben:** Erst verschwindet der
> Verweis (Verzeichnis-Eintrag bzw. Inode-Zeiger), DANN werden
> Inode/Blöcke in der Bitmap freigegeben.

Der Commit-Punkt jeder Operation ist genau EIN Schreibvorgang
(Inode-Update oder Verzeichnis-Umstellung — beide sektor-atomar).

Konkrete Abläufe (Schreibvorgänge in dieser Reihenfolge):

| Operation | Reihenfolge | Nach Absturz mittendrin |
|-----------|-------------|-------------------------|
| **create** (Datei/mkdir) | 1. Bitmap: neue Dir-Blöcke belegen · 2. Inode des Kindes schreiben · 3. neue Verzeichnis-Blöcke schreiben · 4. **Commit:** Verzeichnis-Inode auf neue Blöcke/Größe · 5. alte Verzeichnis-Blöcke freigeben | Leck (Inode/Blöcke belegt, unreferenziert) — Verzeichnis zeigt bis zum Commit auf den alten Stand |
| **write** (Inhalt ersetzen/erweitern) | 1. Bitmap: neue Blöcke belegen · 2. Datenblöcke schreiben · 3. ggf. indirekten Block schreiben · 4. **Commit:** Inode (Zeiger, Größe, geändert) · 5. ersetzte Blöcke freigeben | Leck — die Datei zeigt bis zum Commit vollständig auf den alten Inhalt |
| **write_at in bestehende Blöcke** | Datenblock(e) direkt überschreiben (in place) | Wie bei ext2: der überschriebene BEREICH kann halb alt/halb neu sein (Daten, nie Metadaten). Wer das nicht will, braucht ein Journal — Serie 5+ |
| **delete** | 1. **Commit:** Verzeichnis-Umstellung ohne den Eintrag (= neue Dir-Blöcke, wie create) · 2. Inode als frei schreiben · 3. Blöcke in Bitmap freigeben | Leck — die Datei ist ab dem Commit weg, die Blöcke schlimmstenfalls noch belegt |
| **rename** (gleiches Verzeichnis) | Verzeichnis-Umstellung mit neuem Namen (EIN Commit) | alter oder neuer Name — nie beides, nie keins |
| **rename** (über Verzeichnisse) | 1. Ziel-Verzeichnis-Commit (Eintrag zeigt auf den Inode) · 2. Quell-Verzeichnis-Commit (Eintrag weg) | Schlimmstenfalls ist der Eintrag kurz in BEIDEN Verzeichnissen sichtbar (beide zeigen auf denselben, konsistenten Inode). Die umgekehrte Reihenfolge würde die Datei verlieren — darum diese. |
| **rename** (Ziel-Datei ersetzen) | 1. Ziel-Commit (Eintrag zeigt auf den neuen Inode) · 2. Quelle raus · 3. alten Inode + Blöcke freigeben | Leck des alten Inodes |
| **mkfs** | 1. Bitmap, Inode-Tabelle, Wurzel-Inode schreiben · 2. **zuletzt** den Superblock (Magic) | Ohne fertigen Superblock ist die Platte einfach "kein SpeedFS" — halbes mkfs ist unsichtbar |

Lecks sind bewusst der akzeptierte Schaden: Ein späteres
`fsck.speedfs` (Serie 5+) kann sie durch einen Baum-Scan wieder
einsammeln. Verlorene oder falsch verkettete Daten kann es NICHT
reparieren — deshalb schließt die Reihenfolge genau das aus.

**Block-Cache:** Write-Through — jeder Block-Schreibvorgang geht
sofort ans Gerät, der Cache beschleunigt nur Lesezugriffe
(begrenzte Größe, FIFO-Verdrängung). Damit ist die Schreib-
Reihenfolge im Code IDENTISCH mit der Reihenfolge auf der Platte —
die Absturz-Analyse oben gilt ohne weitere Annahmen. (Write-Back
mit geordnetem Flush wäre schneller und ist Serie-5-Stoff;
Entscheidung dokumentiert in CLAUDE.md.) `sync()` reicht nur noch
das FLUSH CACHE ans Gerät durch (dessen interner Schreib-Cache).

## 8. mkfs-Parameter

* Blöcke gesamt = Gerätegröße / 4096 (Rest ignoriert).
* Inodes = Blöcke / 16, mindestens 64, maximal 65536
  (1 Inode je 64 KiB Platte — großzügig für kleine Dateien).
* Beispiel 64-MiB-Daten-Platte: 16384 Blöcke → Bitmap 1 Block,
  1024 Inodes → 32 Inode-Blöcke, Datenbereich ab Block 34
  (16350 Blöcke ≈ 63,9 MiB nutzbar).

## 9. Grenzen von v1 (bewusst, dokumentiert)

* Max. Dateigröße ≈ 4,09 MiB (einfach-indirekt, Abschnitt 5).
* Keine harten Links, keine Rechte/Besitzer, keine Symlinks.
* Kein Journal — Lecks nach Absturz möglich (Abschnitt 7),
  kein fsck in v1.
* Verzeichnis-Umschreiben ist O(Verzeichnisgröße) je Änderung.
* Ein Mount pro Gerät; kein gleichzeitiger Zugriff von außen.
