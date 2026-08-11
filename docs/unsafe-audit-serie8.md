# unsafe-Audit Serie 8

*August 2026 — nach `unsafe-audit-serie6.md` und `unsafe-audit-serie7.md`*

Serie 8 hat eine neue Angriffsfläche geschaffen: **die Fenster-Syscalls**.
Ein Prozess konnte dem Kernel vorher Zeiger auf Dateien und Sockets
unterschieben; jetzt kann er ihm **Megabytes von Pixeln** unterschieben —
mit einem Rechteck daneben, das sagt, wie sie zu deuten sind.

Dieses Dokument prüft, wo dabei `unsafe` steht und welche Invariante
jeder Block herstellt.

---

## Das Ergebnis in einem Satz

**Die gesamte Serie-8-Fläche im Kernel ist `unsafe`-frei, und die fünf
neuen Kisten sind es auch.**

| Ort | `unsafe`-Blöcke | Anmerkung |
|---|---:|---|
| `src/syscall/fenster.rs` (die 5 Syscalls) | **0** | Prüft und delegiert; das Kopieren macht `ring3` |
| `src/fenster/prozessfenster.rs` | **0** | Reine Datenstruktur (Puffer, Queue, Felder) |
| `speedhtml` | **0** | Arena-Baum aus `Vec` + Indizes |
| `speedcss` | **0** | |
| `speedlayout` | **0** | |
| `speedpaint` | **0** | |
| `speedui` | **0** | |
| `userland/browser` (7 Module) | **0** | |
| `libspeed::leinwand` | **0** | |
| `libspeed::fenster` | **5** | ausschließlich `int 0x80` |

Die einzigen fünf Blöcke der ganzen Serie sind die rohen Syscall-Aufrufe
in `libspeed::fenster` — dieselbe Sorte wie in jedem anderen
libspeed-Modul seit Serie 6, mit derselben Begründung: Ein Syscall ist
Inline-Assembler, und daran führt kein Weg vorbei.

---

## Warum das kein Zufall ist

Drei Entscheidungen, jede aus einem anderen Teil der Serie, führen
zusammen zu diesem Ergebnis:

**(1) Der Kernel kopiert, er teilt nicht.** Die Grundentscheidung von
Teil 1 (`docs/fenster-syscalls.md`) war *Pixelpuffer per Syscall*, und
zwar ausdrücklich nicht, weil es am schnellsten wäre, sondern weil es
**keine Sicherheitszusage kostet**: Der Prozess übergibt Bytes, der
Kernel prüft mit demselben `copy_in`-Apparat wie überall und kopiert.
Bei geteiltem Speicher läge dieselbe Seite in zwei Adressräumen, und
„prüfen, dann kopieren" gälte nicht mehr — dann bräuchte es hier neuen
`unsafe`-Code mit neuen Invarianten.

**(2) Der Arena-Baum statt `Rc<RefCell<…>>`.** `speedhtml` speichert
Knoten in einem `Vec` und referenziert sie über Indizes (Teil 4). Das
war als Entscheidung gegen Laufzeit-Aliasing-Fehler getroffen — und es
bedeutet nebenbei, dass der Parser für fremde Daten **ohne einen
einzigen rohen Zeiger** auskommt.

**(3) Die Ausgabe ist `Vec<u8>`, nicht `Vec<u32>`.** Schon in Teil 3
(Bilder) wurde festgehalten: RGBA als Bytes, weil ein `Vec<u32>` eine
Umdeutung des Puffers bräuchte. Dieselbe Regel gilt in `speedpaint` und
in `libspeed::leinwand` — die Umrechnung ins Fenster-ABI (`0x00RRGGBB`)
passiert ganzzahlig an einer Stelle, ohne `transmute`.

---

## Die neue Fläche im Einzelnen

### `fenster_zeichnen` (49) — der interessanteste Syscall des Projekts

Er ist der einzige, bei dem **zwei Angaben zusammenpassen müssen**:
`laenge` (wie viele Bytes) und `rechteck` (wie sie zu deuten sind). Ein
Kernel, der dem Rechteck glaubt und die Länge nicht nachrechnet, liest
über das Ende des Puffers hinaus — und zwar Megabytes.

Die Prüfkette (`src/syscall/fenster.rs`), in dieser Reihenfolge:

1. Handle → Fenster-Id (aus der **eigenen** Handle-Tabelle; das globale
   Fenster verlässt den Kernel nie).
2. Rechteck entpacken; `breite`/`hoehe` > 0 und ≤ `MAX_FENSTER_*`.
3. **`laenge == breite * hoehe * 4`** — auf das Byte. Das ist die
   Bedingung, die es bei keinem anderen Syscall gibt.
4. `bereich_pruefen_gross(ptr, noetig)` — der **gesamte** Bereich,
   alles-oder-nichts, in 32-KiB-Stücken durch
   `ring3::user_bereich_pruefen`, mit `checked_add` gegen den Zeiger
   nahe `u64::MAX`.
5. Erst dann zeilenweise `ring3::copy_in_scheibe` — das prüft **noch
   einmal**, billig, und ist die eine Stelle, an der einem User-Zeiger
   gefolgt wird.

Warum Schritt 4 *und* 5: Schritt 4 stellt sicher, dass ein halb
gültiger Bereich gar nicht erst anfängt zu kopieren (sonst stünde die
halbe Zeichnung im Fenster, bevor der Fehler kommt). Schritt 5 ist die
Prüfung, die auch dann noch gilt, wenn jemand Schritt 4 später umbaut.

Die 64-KiB-Grenze von `ring3::user_bereich_pruefen` wurde dafür
**nicht aufgeweicht** — sie begrenzt den Schaden eines fehlerhaften
Längen-Arguments bei jedem anderen Syscall. Stattdessen wird in Stücken
geprüft, mit einer eigenen, fensterbezogenen Obergrenze.

### `fenster_ereignis` (50) — das copy-OUT

16 Byte in den Prozess. Der gefährliche Fall ist derselbe wie bei `stat`
in Serie 6: Zeigt das Ziel in den Kernel, würde der Kernel sich selbst
überschreiben lassen. Es läuft über `ring3::copy_out`, also über die
dreistufige Prüfung inklusive `WRITABLE`.

### Was der Angreifer versucht

`userland/angreifer 10` und `11` (neu in dieser Serie) schießen auf
genau diese Stellen: Rechteck ohne Puffer, Puffer ohne Rechteck,
Kernel-Adressen als Pixelquelle, Zeiger nahe `u64::MAX`, erfundene und
doppelt geschlossene Handles, Titel aus dem Kernel-Heap, Ereignis-Ziel
im Kernel, und so viele Fenster wie die Handle-Tabelle hergibt.

**Erwartung und Ergebnis: jeder Fall ein sauberer Fehlercode.** Geprüft
in `tests/sicherheit.rs`.

Der Fall, der am meisten über die Zusage sagt: *Kernel-Speicher als
Pixel zeichnen lassen*. Der Kernel **darf** diese Adresse lesen — täte
er es für uns, stünde Kernel-Speicher als Bild auf dem Schirm und wäre
mit einem Bildschirmfoto auslesbar.

---

## Was Serie 8 an bestehendem `unsafe` NICHT angefasst hat

`ring3::copy_in`, `copy_out`, `copy_in_scheibe`, `adressraum::*`,
`memory::*` — die Blöcke, die in
[`unsafe-audit-serie6.md`](unsafe-audit-serie6.md) einzeln
aufgeschlüsselt sind, sind unverändert. Die Fenster-Syscalls sind
**Benutzer** dieser Maschinerie, keine Erweiterung.

`copy_in_scheibe` ist die einzige Ergänzung aus Serie 8, Teil 1: Sie
kopiert in einen **schon vorhandenen** Kernel-Puffer, statt je Bildzeile
einen frischen `Vec` anzulegen (bei 4K wären das 2 160 Allokationen für
*einen* Syscall). Die Prüfung ist unverändert dreistufig, die
64-KiB-Grenze unangetastet — eine 4K-Zeile sind 15 KiB.

---

## Regel für die Zukunft

> Wer in `speedhtml`, `speedcss`, `speedlayout`, `speedpaint` oder
> `speedui` einen `unsafe`-Block einbaut, begründet ihn in diesem
> Dokument. Diese fünf Kisten verarbeiten **fremde, feindliche Daten** —
> sie sind die Stelle, an der `unsafe` am teuersten wäre, und dass sie
> ohne auskommen, ist kein Zufall, sondern Ergebnis der drei
> Entscheidungen oben.
