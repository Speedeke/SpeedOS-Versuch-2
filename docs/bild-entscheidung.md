# Bild-Dekodierung in SpeedOS — Evaluation und Entscheidung

*Serie 8, Teil 3 — Juli 2026*

Dieses Dokument beantwortet die Frage aus der Serie-8-Bestandsaufnahme (d):
**Wie kommt SpeedOS zu PNG und JPEG?** Es folgt derselben Methodik wie
`docs/tls-entscheidung.md` in Serie 7: erst messen, dann entscheiden, und
das Ergebnis mitsamt der verworfenen Alternative aufschreiben.

---

## 0. Das Ergebnis in drei Sätzen

**`zune-png` und `zune-jpeg` (beide 0.5, `default-features = false`)
übersetzen für `x86_64-unknown-none` und laufen in Ring 3.** Die
Bestandsaufnahme hatte für PNG einen Eigenbau aus `miniz_oxide` plus
eigenem Chunk-Leser veranschlagt und JPEG als „deutlich mehr Arbeit"
eingestuft — beides ist nicht nötig geworden.

Der Preis: **46 KiB Code für PNG, weitere 83 KiB für JPEG**, und ein
Prozess, der beides einbindet, wird 235 KiB groß. Der Nutzen: ein
Dekoder, der in 17 Prüffällen **kein einziges Mal** panickt.

---

## 1. Was geprüft wurde und wie

Der Spike hat fünf Kisten gegen unser echtes Target gebaut — dasselbe
`x86_64-unknown-none` mit `-sse,+soft-float`, das der Kernel und alle
User-Programme benutzen, mit `panic = "abort"` und `opt-level = "s"`.

| Kiste | Version | baut für x86_64-unknown-none? | Befund |
|---|---|---|---|
| **`zune-png`** | 0.5.2 | **ja** | no_std + alloc, keine weiteren Bedingungen |
| **`zune-jpeg`** | 0.5.15 | **ja** | dito |
| `miniz_oxide` | 0.8.9 | ja (mit `with-alloc`) | nur inflate — der PNG-Teil bliebe Eigenbau |
| `png` | 0.17.16 | **nein** | zieht `bitflags` und `simd-adler32` mit `std` |
| `jpeg-decoder` | 0.3.2 | **nein** | 653 Fehler, `std::io::Read` durchgehend |

Die beiden abgelehnten scheitern nicht an einer Kleinigkeit: Sie sind
gegen `std` geschrieben (`std::io::Read` als Eingabe-Abstraktion). Das
ist keine Kritik an ihnen — es ist der Unterschied zwischen einer Kiste,
die `no_std` als Feature führt, und einer, die es nicht tut.

### 1a. Die Versions-Falle, die Zeit gekostet hat

`zune-jpeg 0.4` hängt an `zune-core 0.4`, `zune-png 0.5` an `zune-core
0.5`. Wer beide in der jeweils neuesten Fassung nimmt, die cargo ohne
Widerspruch auflöst, bekommt **denselben Unterbau zweimal ins Programm** —
und die `DecoderOptions` des einen sind nicht die des anderen. Der
Compiler sagt es, aber erst an der Aufrufstelle und mit einer
Fehlermeldung, die nach einem Tippfehler aussieht.

**Beide auf 0.5** ist deshalb keine Kosmetik, sondern die Bedingung dafür,
dass die Grenzen (§3) für beide Formate dieselben sind.

---

## 2. Die Zahlen

### Codegröße

Gemessen mit `llvm-nm --print-size` am fertigen ELF `bilder`, Symbole
nach Herkunftskiste summiert:

| Kiste | Bytes |
|---|---:|
| `zune-jpeg` | 83 484 |
| `zune-png` | 34 603 |
| `zune-inflate` | 9 225 |
| `zune-core` | 2 098 |
| `simd-adler32` | 229 |
| `libspeed` | 4 015 |
| Betrachter + core + Rest | 30 368 |

* **PNG-Kette allein: ~46 KiB** (png + inflate + core + adler32)
* **JPEG zusätzlich: ~83 KiB**
* **ELF `bilder` gesamt: 235 688 Byte** (mit 4-KiB-Sektionsausrichtung
  aus `speedos.ld` und den Huffman-/Quantisierungstabellen von JPEG)

Zum Vergleich: `holes` (rustls + TLS) ist 977 KiB, `uidemo` 61 KiB,
`hallo` 19 KiB.

**Programme, die den Dekoder nicht benutzen, zahlen nichts.** `hallo` ist
mit und ohne die neue Abhängigkeit in `libspeed` byte-identisch 19 264
Byte groß — LTO und `--gc-sections` werfen den ungenutzten Code weg. Das
war die Bedingung dafür, die Kisten überhaupt in `libspeed` aufzunehmen
statt in ein eigenes Crate.

### Laufzeit und Heap

Gemessen in QEMU/WHPX (`tests/bilder.rs`):

| Bild | Datei | RGBA | Heap-Spitze | Dauer |
|---|---:|---:|---:|---:|
| `rgba.png` 32×32 | 114 B | 4 096 B | 8 808 B | 3 ms |
| `verlauf.png` 64×48 | 6 321 B | 12 288 B | 53 264 B | 3 ms |
| `gross.png` 160×120 | 40 424 B | 76 800 B | 356 888 B | 4 ms |
| `bombe.png` (abgelehnt) | 48 992 B | — | — | 4 ms |

Die Dauer ist dabei überwiegend **Prozess-Start**, nicht Dekodierung —
ein Prozess-Start kostet 6–11 µs, aber der Test misst von der Pipe-Anlage
bis zum eingesammelten Ende.

Im laufenden Betrieb (Betrachter mit 720×520-Fenster, `i`-Taste) steht
die Heap-Spitze bei **3 007 520 Byte** — davon sind ~1,5 MiB der
Fensterpuffer und ~12 KiB das Bild. **Der Fensterpuffer ist der
Speicherfresser, nicht das Bild.**

---

## 3. Die drei Schritte — und warum es drei sind

Der naive Weg ist `decode(bytes) -> Bild` in einem Rutsch. Der ist
angreifbar, und der Testfall dazu liegt im Repository.

`assets/testbilder/bombe.png` ist **48 KiB groß, deklariert 4096×4096 und
dekodiert zu 50 MiB**. Sie ist **formal einwandfrei**: Es gibt nichts
Unplausibles an ihr, kein Parser findet einen Fehler, und die
`max_width`/`max_height`-Riegel der Kiste (8192) lässt sie glatt
passieren. Der Prozess-Heap ist 12 MiB.

Deshalb läuft `libspeed::bild::dekodieren_mit` in drei Schritten:

1. **`decode_headers()`** — nur IHDR bzw. SOF, keine Bilddaten, keine
   Allokation für Pixel.
2. **Grenzen prüfen** — Kantenlänge, **Pixelzahl**, Puffergröße. *Hier
   stirbt die Bombe*, mit einem Fehler und ohne ein einziges alloziertes
   Byte.
3. **`decode_into()`** in einen Puffer, den **wir** angelegt haben und
   dessen Größe **wir** bestimmt haben.

Schritt 3 ist der zweite Gewinn: `decode_raw()` würde selbst allozieren,
und zwar so viel, wie die Datei will. Mit `decode_into` bestimmt der
Dekoder nicht mehr, wie viel Speicher er bekommt.

### Der Trick, der die Spitze halbiert

Der Puffer ist von Anfang an `breite × höhe × 4` Byte groß. Der Dekoder
schreibt seine `output_buffer_size()` Bytes (bei RGB 3/Pixel, bei Grau
1/Pixel) in den **vorderen** Teil; danach wird **von hinten nach vorn**
auf 4 Byte je Pixel auseinandergezogen — rückwärts, weil das Ziel jedes
Pixels weiter hinten liegt als seine Quelle.

Der naheliegende Weg (Dekoder-`Vec` plus Umbau in einen zweiten `Vec`)
hätte beide gleichzeitig im Speicher: bei 1 Mi Pixeln **8 MiB statt 4 MiB
Spitze**. Bei 12 MiB Heap ist das der Unterschied zwischen „geht" und
„geht nicht".

### Die Grenzen und woher ihre Zahlen kommen

```rust
Grenzen::standard() = {
    max_kante:        8192,          // eine absurde EINZELNE Kante
    max_pixel:        1 Mi,          // 1024×1024 oder 1280×819
    max_datei_bytes:  4 MiB,
}
```

Die Rechnung dahinter, mit 12 MiB Prozess-Heap:

```
Dateibytes (4 MiB) + RGBA (1 Mi × 4 B = 4 MiB) = 8 MiB
                                    bleibt für ein Fenster:  4 MiB
                                     720p-Fensterpuffer:    ~3,5 MiB
```

**`max_pixel` ist eine HEAP-Grenze, keine Format-Grenze.** Ein
1920×1080-Foto (2,07 Mi Pixel) wird abgelehnt — nicht weil PNG das nicht
könnte, sondern weil ein SpeedOS-Prozess 12 MiB hat. Eingetragen in
`docs/grenzen.md`.

Deshalb ist `Grenzen` ein **Argument und keine Konstante**: Wächst das
Prozess-Layout (ABI-Änderung), hebt ein Aufrufer sie an, ohne dass in
`bild.rs` eine Zeile geändert werden muss.

---

## 4. Was der Prüfstand sagt

`tools/testbilder_erzeugen.py` baut 17 Dateien — fünf gute, zwölf kaputte
bis böswillige. Sie werden **erzeugt und nicht eingecheckt**: Wer eine
Dekompressionsbombe als Binärdatei ins Repository legt, hat Testdaten,
die niemand nachvollziehen kann; wer sie erzeugt, hat eine *lesbare Liste
von Angriffen*.

`tests/bilder.rs` startet für jede Datei `bilder --pruefen` als
**Ring-3-Prozess** und liest die Ausgabe — dasselbe Muster wie
`tests/sicherheit.rs` mit `angreifer`. Ergebnis:

```
  verlauf.png                   0  ok 64 48 12288 ff000040 ffffff40
  gross.png                     0  ok 160 120 76800 ff000040 ffffff40
  rgba.png                      0  ok 32 32 4096 00e03090 ffe03090
  grau.png                      0  ok 40 24 3840 ff000000 ff949494
  palette.png                   0  ok 24 16 1536 ffff0000 ff0000ff
  abgeschnitten.png             1  fehler kaputter-kopf
  ohne_iend.png                 1  fehler kaputter-kopf
  crc_kaputt.png                0  ok 32 32 4096 ff000040 ffffff40
  falsche_signatur.png          1  fehler unbekanntes-format
  absurde_masse.png             1  fehler kaputter-kopf
  null_masse.png                1  fehler kaputter-kopf
  bombe.png                     1  fehler zu-gross            (4 ms)
  riesige_chunk_laenge.png      1  fehler kaputter-kopf
  viele_chunks.png              0  ok 8 8 256 ff000040 ffffff40
  leer.png                      1  fehler leer
  nur_signatur.png              1  fehler kaputter-kopf
  kein_bild.png                 1  fehler unbekanntes-format

  5 dekodiert, 9 abgelehnt, 0 Paniken.
  17 Dekodier-Prozesse: 0 Frames verloren (Schranke 13).
```

**Die drei Aussagen, auf die es ankommt:**

* **Kein Exit-Code 101.** Eine Panik im Dekoder wäre in Ring 3 zwar
  folgenlos für den Kernel (Dauerregel II), aber sie wäre ein Programm,
  das *verschwindet* statt „kaputtes Bild" anzuzeigen — und im kommenden
  Renderer ein Bild, das die ganze Seite abschießt.
* **Die Bombe stirbt in 4 ms an der Pixelgrenze**, nicht nach 50 MiB.
* **Die Pixel stimmen**, nicht nur die Maße. `verlauf.png` wird gegen die
  Formel aus dem Erzeuger-Skript geprüft — ein Dekoder, der RGB als BGR
  ausliefert, liefert ein Bild in der richtigen Größe, es ist nur blau
  statt rot.

### Zwei Befunde, die kein Fehler sind

* **`crc_kaputt.png` dekodiert.** zune prüft die Chunk-Prüfsummen in der
  Voreinstellung nicht (`DecoderOptions::set_strict_mode` täte es). Das
  ist für einen Betrachter die richtige Wahl: Ein gekipptes Byte soll ein
  leicht falsches Bild ergeben, nicht gar keins. Eine Prüfsumme ist eine
  Integritäts-, keine Sicherheitszusage — sie hält keinen Angreifer auf,
  der sie einfach neu berechnet.
* **`absurde_masse.png` meldet `kaputter-kopf` statt `zu-gross`.** Der
  `max_width`-Riegel der Kiste greift schon im Kopf-Parser, also noch vor
  unserer eigenen Prüfung. Beide Riegel zu haben ist kein Übereifer: Der
  Riegel der Kiste kennt unsere Pixelzahl nicht (die Bombe läuft mit
  4096×4096 glatt durch ihn hindurch), und unsere Prüfung liefe ohne ihn
  erst, nachdem der Kopf-Parser mit absurden Zahlen gearbeitet hat.

---

## 5. Die verworfene Alternative: Eigenbau

Die Bestandsaufnahme hatte `miniz_oxide` + eigener PNG-Chunk-Leser
vorgeschlagen, und der Auftrag ließ ihn ausdrücklich zu. Er wurde **nicht**
gebaut, und zwar aus einem Grund, der über PNG hinausgeht.

**Dafür spräche:** PNG ist ein überschaubares Format (Chunks, ein
zlib-Strom, fünf Zeilenfilter). Der Eigenbau wäre ~600 Zeilen, gegen
bekannte Testvektoren prüfbar und ohne fremde Abhängigkeit außer inflate.
Er läge in derselben Größenordnung wie unser ChaCha20 — und den haben wir
selbst geschrieben.

**Dagegen sprach das Entscheidende:** Bei ChaCha20 gilt die
Eigenbau-Krypto-Grenze aus Serie 7, weil es dort **offizielle
Testvektoren** gibt, die bitgenau prüfen, ob die Implementierung stimmt.
Für einen PNG-Dekoder gibt es das nicht in derselben Schärfe — es gibt
PngSuite, aber kein „richtig oder falsch" je Bit, sondern ein „sieht
plausibel aus". Und die interessanten Fehler eines Bild-Dekoders sind
nicht die falschen Farben, sondern die **Pufferüberläufe bei kaputten
Eingaben** — genau die Klasse, die man mit selbst geschriebenen
Testfällen schlecht findet, weil man dieselben blinden Flecken hat wie
beim Schreiben.

`zune-png` ist gefuzzt, in Produktion im Einsatz, und liegt bei uns in
**Ring 3** — ein Fehler in ihm trifft einen Prozess, nicht den Kernel.
Das ist genau die Begründung, mit der auch rustls hereinkam.

**Die Bedingung, unter der die Entscheidung fällt:** Wenn `zune-*` je
nicht mehr für unser Target baut (das ist die einzige harte Abhängigkeit
— keine Cargo-Features, keine cfg-Flaggen, anders als beim TLS-Stapel),
ist der PNG-Eigenbau der Rückfallweg. `miniz_oxide` baut, das ist
gemessen; die Arbeit wäre dann der Chunk-Leser und die fünf Filter.
**JPEG wäre in diesem Fall nicht ersetzbar** und V1 müsste auf PNG
zurückfallen.

---

## 6. Was die Ausgabe ist

**Immer RGBA**, immer `breite × höhe × 4` Byte, in der Reihenfolge R, G,
B, A — egal ob die Datei Graustufen, Palette, RGB, RGBA oder (bei JPEG)
YCbCr enthielt. Ein Aufrufer soll **keine Farbräume kennen müssen**,
sonst wandert die Fallunterscheidung in jeden Aufrufer und einer vergisst
sie.

Warum `Vec<u8>` und nicht `Vec<u32>`: Ein `Vec<u32>` bräuchte eine
Umdeutung des Puffers und damit `unsafe`. `libspeed::pem`, `netz` und
`tls` haben zusammen **null unsafe-Blöcke**
(`docs/unsafe-audit-serie7.md`); `bild.rs` hat auch keinen. Die
Umrechnung ins Fenster-Format (`0x00RRGGBB`, das Fenster-ABI kennt kein
Alpha) passiert an **einer** Stelle — `Bild::nach_fenster` bzw.
`Bild::pixel_auf` — und ist gewöhnliches Rust mit ganzzahliger
Alpha-Mischung (`-sse,+soft-float`, es gibt kein Fließkomma).

---

## 7. Was NICHT dabei ist

| fehlt | warum |
|---|---|
| **GIF, BMP, WebP** | Werden an der Signatur **erkannt** und mit `NichtUnterstuetzt(Format)` abgelehnt — „GIF kann SpeedOS nicht" ist eine Auskunft, „unbekanntes Format" bei einer offensichtlichen GIF-Datei ist Ratlosigkeit. |
| **Animationen (APNG, animiertes GIF)** | `zune-png` kann APNG lesen; wir holen nur das erste Bild. Ein Renderer, der Animationen zeigt, braucht einen Zeitgeber je Bild — eigenes Vorhaben. |
| **Farbprofile (ICC), Gamma** | Werden ignoriert. Ein Bild mit exotischem Profil sieht leicht falsch aus. Farbmanagement ist ein eigenes Fachgebiet. |
| **Progressive JPEGs** | `zune-jpeg` kann sie; ungetestet, weil unser Prüfstand keine erzeugt. |
| **Skalierung beim Dekodieren** | Ein 8000×8000-Bild wird abgelehnt, statt verkleinert dekodiert zu werden. `zune-jpeg` könnte 1:2/1:4/1:8 direkt — das wäre der nächste Hebel, wenn die Heap-Grenze drückt. |
| **Bilder aus dem Netz** | `libspeed::netz::Klient` liefert Bytes, `bild::dekodieren` nimmt Bytes. Verdrahtet ist es noch nicht — das macht der Renderer. |

---

## 8. Wo was liegt

```
tools/testbilder_erzeugen.py   erzeugt 17 Prüfbilder (5 gut, 12 böse)
assets/testbilder/*.png        das Ergebnis (gitignored? nein — klein genug)
build.rs                       bettet sie + `bilder` ins Kernel-Image ein
src/programme.rs               TESTBILDER, testbilder_installieren()
                               -> /platte/bilder beim Boot
userland/src/bild.rs           DER DEKODER (Ring 3, 0 unsafe)
userland/src/bin/bilder.rs     Betrachter + `--pruefen`-Modus
tests/bilder.rs                12 Tests, 17 Bilder, 0 Paniken
```

Bedienung:

```
starte bilder /platte/bilder/verlauf.png &
```

Das `&` ist Pflicht — solange ein Shell-Befehl synchron läuft, kommt der
Compositor nicht dran (`docs/fenster-syscalls.md`).
