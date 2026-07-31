# Schriftgrößen, Textmetrik, Fett und Kursiv

*Serie 8, Teil 3 — Juli 2026*

Die Serie-8-Bestandsaufnahme nannte Schriften „die größte echte Lücke" für
einen HTML-Renderer. Dieses Dokument klärt sie: **Was gibt der
Font-Bestand her, reicht das für V1, und was fehlt ehrlich?**

---

## 0. Die Antwort in vier Sätzen

**Nach oben reicht es, nach unten nicht.** Es gibt vier vorgerasterte
Größen — 16, 20, 24, 32 —, damit bekommen `h1` bis `h4` vier
unterscheidbare Stufen. `h5`, `h6` und `small` wollen *kleiner* als
Fließtext sein und **können es nicht**, weil die kleinste Größe zugleich
die Fließtextgröße ist; sie werden über das **Gewicht** unterschieden.

**Fett ist echt, Kursiv ist simuliert.** `FontWeight::Bold` ist ein
eigener vorgerasterter Schnitt; einen Kursivschnitt gibt es nicht, also
werden die Glyphen geschert.

Für V1 reicht das. Der Ausweg für alles Weitere ist ein
TrueType-Rasterizer, und der ist Serie 9.

---

## 1. Der Bestand — nachgesehen, nicht geraten

`noto-sans-mono-bitmap` 0.3.2 bietet in ihrer `Cargo.toml`:

* **Rasterhöhen:** `size_16`, `size_20`, `size_24`, `size_32`. **Mehr
  gibt es nicht.**
* **Gewichte:** `light`, `regular`, `bold`. **Kein Italic.**
* Zeichenvorrat bei uns: Basic Latin + Latin-1 Supplement (also `ä ö ü ß`).
* Monospace. Eine Proportionalschrift ist in dieser Kiste nicht enthalten.

SpeedOS band bis Serie 8, Teil 2 drei Größen ein (16/24/32 — sie waren die
UI-Skalierungsstufen 1.0/1.5/2.0). **`size_20` kam in Teil 3 dazu**, und
zwar für genau eine Rolle: `<h3>` will 1,17 em, bei Basis 16 also 19 px,
und wäre ohne die 20 auf 16 zurückgefallen — auf Fließtextgröße. Eine
Überschrift, die aussieht wie Fließtext, ist keine.

---

## 2. Die Rollen-Abbildung

`speedui::text::Rolle` kennt neun Rollen mit den CSS-Voreinstellungen, die
jeder Browser seit CSS 2.1 benutzt. Die Faktoren stehen in **Promille**,
nicht als `f32` — unser Target hat `-sse,+soft-float`, es gibt kein
Fließkomma (dieselbe Entscheidung wie bei der UI-Skalierung in „halben").

Bei Fließtextgröße **16** (UI-Skalierung 1.0):

| Rolle | Faktor | Wunsch | bekommt | exakt? | fett |
|---|---:|---:|---:|:--|:--|
| `h1` | 2,0 em | 32 | **32** | ja | ja |
| `h2` | 1,5 em | 24 | **24** | ja | ja |
| `h3` | 1,17 em | 19 | **20** | nein | ja |
| `h4` | 1,0 em | 16 | **16** | ja | ja |
| `h5` | 0,83 em | 13 | **16** | **NEIN** | ja |
| `h6` | 0,67 em | 11 | **16** | **NEIN** | ja |
| `p` | 1,0 em | 16 | **16** | ja | – |
| `small` | 0,8 em | 13 | **16** | **NEIN** | – |
| `code` | 1,0 em | 16 | **16** | ja | – |

Diese Tabelle ist **abfragbar statt behauptet**: `schrift` in der Shell
gibt sie aus, gegen den *echten* Wirt und die *aktuelle* UI-Skalierung.
Eine Tabelle in einem Dokument wäre ab der ersten Änderung eine
Behauptung.

### Die Lücke ist eine Funktion, kein Kommentar

```rust
speedui::text::exakt_moeglich(Rolle::Klein, 16, &schrift)  // -> false
```

Eine Einschränkung, die man abfragen kann, ist eine dokumentierte
Einschränkung. Eine, die man nur bemerkt, wenn die Seite komisch
aussieht, ist ein Fehler.

### Was mit `h5`, `h6` und `small` geschieht

Sie bekommen alle 16 px. Unterschieden werden sie über das **Gewicht**:
`h5` und `h6` sind fett (das sind sie in jedem Browser ohnehin), `small`
ist normal. Ein `<small>` ist damit von Fließtext **nicht
unterscheidbar** — das ist die ehrliche Lage und keine Wahl.

Der Renderer kann zusätzlich über die **Farbe** differenzieren
(`Farbrolle::TextSekundaer` für Kleingedrucktes). Das ist ein
Ausweichmanöver und wird auch so benannt.

### Bei anderer UI-Skalierung dreht sich das um

Bei Basis 32 (Skalierung 2.0) ist der Vorrat *nach oben* erschöpft: `h1`
will 64 und bekommt 32, `h2` will 48 und bekommt 32 — **`h1` und `h2`
sehen dann gleich aus**. Dafür wird es nach unten sauber (`small` will 26
und bekommt 24). Festgehalten in
`speedui::text::tests::test_bei_grosser_basis_geht_der_vorrat_nach_oben_aus`.

---

## 3. Reichen diskrete Größen für V1?

**Ja — mit zwei benannten Einschränkungen.**

Dafür:

* Die Abstufung, auf die es beim Lesen ankommt, ist die zwischen
  Überschrift und Fließtext, und die gibt es (32/24/20/16).
* `font-size: 13px` wird auf die nächstliegende vorhandene gerundet
  (`Schrift::groesse_waehlen`); bei Gleichstand auf die **kleinere**, weil
  zu groß das Layout sprengt und zu klein nur mickrig aussieht.
* Vorgerasterte Bitmaps sind **schnell** und kosten keinen Glyph-Cache.
  Ein Rasterizer, der bei jedem Scroll-Frame Kurven füllt, wäre für
  unseren Compositor eine neue Leistungsfrage.

Dagegen (und das bleibt so stehen):

* **Unterhalb der Fließtextgröße gibt es nichts.** Eine Seite mit
  Fußnoten sieht falsch aus.
* **Vier Größen sind vier Größen.** Eine Seite, die mit `font-size` fein
  abstuft, bekommt Stufen.

**Der Ausweg ist ein TrueType-Rasterizer** (`ab_glyph`, `fontdue` — beide
`no_std`-tauglich). Das ist ein eigenes Vorhaben mit eigenen Fragen
(Glyph-Cache, Hinting-Verzicht, Subpixel ja/nein, und wo die
Font-Datei herkommt, wenn ein Ring-3-Prozess sie braucht — es gibt keinen
Schrift-Syscall). **Serie 9**, wie schon in der Bestandsaufnahme
vorgesehen.

---

## 4. Textmetrik — die Funktion, ohne die es keinen Umbruch gibt

`Schrift::text_breite(text, groesse)` gab es schon; sie ist jetzt der
dokumentierte Einstieg und hat drei Geschwister bekommen:

```rust
fn groessen(&self) -> &[i32];                       // was es WIRKLICH gibt
fn groesse_waehlen(&self, wunsch: i32) -> i32;      // font-size -> Raster
fn text_breite_stil(&self, t, g, stil) -> i32;      // stil-abhängig
fn fett_echt(&self) -> bool;
fn kursiv_echt(&self) -> bool;
```

Alle mit Voreinstellung — **bestehende Wirte laufen unverändert weiter**.
`groessen()` liefert leer, was „jede Größe" heißt (ein Wirt mit echtem
Rasterizer sagt nichts anderes).

### Die Umlaut-Falle

Die Voreinstellung von `text_breite` zählt `chars().count()`, **nicht**
`len()`. `len()` ist die Zahl der UTF-8-**Bytes**:

```
"Grüße".len()          == 7
"Grüße".chars().count() == 5
```

Wer Bytes zählt, rechnet für jeden Umlaut eine Zeichenbreite zu viel und
**bricht jede deutsche Zeile zu früh um**. Der Test dazu steht an zwei
Stellen — `speedui::text::tests` gegen die Attrappe und
`ui::wirt::tests` gegen den echten Kernel-Wirt — und deckt `ä ö ü Ä Ö Ü ß`
einzeln, dazu `€` (3 Bytes) und ein Emoji (4 Bytes).

### Der Zeilenumbruch als Beweis

`speedui::text::umbrechen` ist die Funktion, für die es die Metrik gibt.
Sie läuft **ausschließlich** über `text_breite_stil` und nie über
`chars().count()` — bei Monospace ginge das Raten sogar gut, bei jeder
Proportionalschrift bricht es sofort. Ein eigener Test mit einem Wirt,
bei dem fett doppelt so breit ist, hält fest, *dass* die Stil-Metrik
befragt wird.

Regeln: Umbruch an Leerzeichen (die nicht zur Zeile gehören), `\n` bricht
immer, ein **zu langes Wort wird hart getrennt** (eine lange URL ist der
Normalfall dafür, nicht die Ausnahme), und die Schnittstellen liegen
**immer auf Zeichengrenzen** — sie stammen aus `char_indices()`, nie aus
einer Rechnung. Deshalb panickt das Schneiden auch bei „Grüße öffnen
Türen" nicht; der Test prüft das für sieben verschiedene Breiten.

---

## 5. Fett und Kursiv

### Fett: echt

`FontWeight::Bold` ist ein eigener vorgerasterter Schnitt.
`Schrift::fett_echt()` liefert `true`. Keine Doppelzeichnung, kein
Verschmieren.

### Kursiv: simuliert, und zugegebenermaßen

Es gibt keinen Kursivschnitt. Entweder man verzichtet auf `<i>`, oder man
schert — SpeedOS schert.

`grafik::Zeichner::text_kursiv` verschiebt jede Glyph-**Zeile**
horizontal: je weiter oben, desto weiter nach rechts, Faktor 1/4 (etwa
14°). Das ist die Größenordnung eines echten Italic-Winkels und flach
genug, dass benachbarte Zeichen sich nicht ineinanderschieben.

**Was das nicht ist:** ein Kursivschnitt. Ein echter hat *andere
Buchstabenformen* — das einstöckige `a`, das geschwungene `f`. Ein
geschertes `a` bleibt ein gerades `a`, das schief steht. Für einen
Renderer, der `<i>` vom Fließtext unterscheidbar machen soll, reicht es.

**`Schrift::kursiv_echt()` meldet `false`**, damit niemand etwas anderes
annimmt. Die Auskunft steht im *Programm* und nicht nur in diesem
Dokument — ein Renderer, der wissen will, ob er `<i>` ehrlich darstellen
kann, fragt sie ab, und `schrift` in der Shell zeigt sie an.

**Die Breite ändert sich durch die Scherung nicht.** Das ist Absicht: Der
Vorschub bleibt `raster.width()`, also misst `text_breite` kursiven Text
richtig. Nur das letzte Zeichen ragt oben um bis zu `höhe/4` Pixel nach
rechts heraus — beim Clipping abgeschnitten, nie über den Puffer hinaus.

### Wie es an die Widgets kommt

`Leinwand::text_stil(x, y, text, groesse, Stil, farbe)` ist eine
**zehnte** Trait-Operation mit Voreinstellung, keine Änderung an `text`.
Begründung: `text` steht in jedem Widget und in beiden Wirten; eine
geänderte Signatur wäre ein Umbau von zwanzig Aufrufstellen für eine
Fähigkeit, die **kein Widget benutzt** — nur der kommende Renderer. Die
Voreinstellung wirft das Kursiv weg und zeichnet den Rest korrekt; ein
Wirt, der scheren kann, überschreibt sie (der Kernel tut das, `uidemo`
nicht).

Dasselbe Muster wie die Zeilen-Schnellpfade des `Zeichenflaeche`-Traits
in Serie 3: eine Voreinstellung, die richtig ist, und ein Wirt, der es
besser kann, wenn er will.

---

## 6. Die beiden Wirte im Vergleich

| | Kernel (`ui::wirt`) | `uidemo` (Ring 3) |
|---|---|---|
| Größen | 16, 20, 24, 32 | nur 8 |
| Fett | **echt** | nein |
| Kursiv | simuliert (Scherung) | nein |
| Zeichenvorrat | Latin-1 (mit Umlauten) | 5×7-Raster, ASCII |

Dass `uidemo` fast nichts kann und **trotzdem alle Widgets bedient**, ist
der Punkt der Trait-Voreinstellungen. Ein Ring-3-Prozess bekommt die
vorgerasterten Kernel-Schriften nicht (es gibt keinen Schrift-Syscall,
`docs/grenzen.md`) — er bringt seine eigene mit, und wie gut die ist,
entscheidet er.

Für den Browser aus Serie 9 heißt das: Er wird entweder mit einem eigenen
mitgebrachten Rasterizer arbeiten oder mit einem Schrift-Syscall, den es
noch nicht gibt. **Das ist die offene Architekturfrage dieses Kapitels**
und in `docs/grenzen.md` eingetragen.

---

## 7. Was geprüft ist

* `speedui` — 43 Tests auf dem **Host** in 0,00 s
  (`cd speedui && cargo test --target x86_64-pc-windows-msvc`), davon 14
  neu für Rollen, Größenwahl, Metrik und Umbruch. Die Größen-Lücke wird
  gegen die Attrappe `VierRaster` geprüft, die den echten Font-Bestand
  nachbildet — **nicht** gegen `TestSchrift`, die jede Größe kann und
  deshalb keine Lücke hätte.
* Kernel — 6 neue Tests in `ui::wirt::tests`: dass die *gemeldeten*
  Größen wirklich gerastert sind (die Klammer zwischen `Cargo.toml` und
  Code), dass Zeilenhöhen mit der Größe wachsen, dass die Rollen-Abbildung
  auf dem echten Wirt dieselbe ist wie auf der Attrappe, und der
  Umlaut-Fall.
* Sichtprüfung: `schrift` in der Shell.
