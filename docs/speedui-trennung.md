# speedui: das Toolkit vom Kernel trennen

*Serie 8, Teil 2 — Entwurf. Juli 2026.*

**Dieses Dokument entstand VOR dem Code** (Projektregel wie bei
`docs/speedfs-format.md` und `docs/scheduler-design.md`). Es legt fest, was
speedui vom Wirt VERLANGT — und erst danach durfte eine Zeile umziehen.

---

## 0. Warum das die eigentliche Architekturfrage ist

`docs/serie8-bestandsaufnahme.md` nennt drei Wege für den Browser:

* **(a)** Das Toolkit bleibt im Kernel, der Browser malt selbst.
* **(b)** Der Browser bekommt eine eigene, kleine UI-Schicht.
* **(c)** Das Toolkit wandert als geteilte Kiste in den User-Space.

Gewählt ist **(c)** — mit dem Vorbehalt, der dort schon notiert war und der
jetzt die eigentliche Arbeit ist:

> *Das Toolkit kennt Schriften, Themes und Zeit. Diese drei Abhängigkeiten
> müssen zu Argumenten werden, sonst zieht die Kiste den Kernel hinter sich
> her.*

Bei `speedhttp` (Serie 7) war das Herausziehen leicht: Ein Parser kennt nur
Bytes, und Bytes kennen keine Umgebung. Der leere `[dependencies]`-Block war
dort nach einer Stunde da. **Hier ist es anders**, und das ist keine
Überraschung, sondern die Definition eines Toolkits: Ein Widget existiert
nur, weil es etwas ZEICHNET, in einer bestimmten FARBE, in einer bestimmten
SCHRIFT, und weil ein Cursor mit einer bestimmten FREQUENZ blinkt.

Ein Toolkit ohne Umgebung ist deshalb nicht dasselbe wie ein Parser ohne
Umgebung. Der Trick ist nicht, die Umgebung wegzulassen, sondern sie
**umzudrehen**: speedui beschreibt, WAS es braucht, und der Wirt bringt es
mit.

---

## 1. Die Regel

> **speedui definiert Traits. speedui implementiert sie nicht.**
> Der Kernel liefert seine Implementierung, ein Ring-3-Prozess seine.
> `speedui/Cargo.toml` hat einen **leeren `[dependencies]`-Block** —
> derselbe Beweis wie bei `speedhttp`.

Und die Gegenprobe, damit die Regel nicht erodiert: Ein Test baut die Kiste
**allein** (`cargo build -p speedui` in ihrem eigenen Verzeichnis, ohne den
Kernel-Workspace). Wer speedui eine Abhängigkeit gibt, bricht ihn.

---

## 2. Die drei genannten Abhängigkeiten

### 2.1 `Thema` — Farben und Masse

Heute: `theme::aktuell()` liefert ein `Theme`-Struct mit ~40 Farbfeldern,
`theme::metrik()` ein `Metrik`-Struct mit ~25 Zahlen. Beide sind
Kernel-Globals mit Atomics dahinter (Hell/Dunkel, Akzentfarbe, Skalierung).

**Nicht** das ganze `Theme` in die Kiste ziehen: Zwei Drittel seiner Felder
sind Fenster-Dekoration (`titel_aktiv_oben`, `schatten`, `taskleiste_…`) —
die gehören dem Fenster-Manager und damit dem Kernel. Ein Widget braucht ein
gutes Dutzend.

Also ein **Rollen-Enum** statt eines Structs:

```rust
pub enum Farbrolle {
    Flaeche, InhaltHintergrund, Rahmen, RahmenAktiv,
    TextStark, TextNormal, TextSekundaer, TextGedimmt,
    Akzent, AkzentText, Eingabefeld, AuswahlHintergrund,
    KnopfFlaeche, KnopfHover, KnopfGedrueckt,
    Erfolg, Warnung, Fehler,
}

pub enum Mass {
    Abstand, UiRand, ElementHoehe, ListenEintragHoehe,
    ScrollbalkenBreite, RadiusGross, RadiusKlein,
    SchriftUi, SchriftGross, ZeilenHoehe, CursorBlinkUs,
}

pub trait Thema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe;
    fn mass(&self, mass: Mass) -> i32;
}
```

**Warum ein Enum und kein Struct mit Feldern:** Ein Struct wäre der bequeme
Weg und würde die Kopplung nur umbenennen — jede neue Kernel-Farbe müsste in
die Kiste. Mit dem Enum ist die Liste dessen, was ein Widget überhaupt
kennen darf, **abschliessend und lesbar**. Wer eine Rolle ergänzt, tut es
sichtbar und muss beide Wirte nachziehen.

`CursorBlinkUs` sitzt hier und nicht bei der Uhr: Es ist eine
Einstellungs-Sache (`einstellungen::cursor_blink_us`), kein Zeitbegriff.

### 2.2 `Schrift` — reine Metrik, kein Rastern

```rust
pub trait Schrift {
    fn zeichen_breite(&self, groesse: i32) -> i32;
    fn zeilen_hoehe(&self, groesse: i32) -> i32;
    fn text_breite(&self, text: &str, groesse: i32) -> i32;
}
```

**Die Schrift wird NICHT mitgenommen, auch nicht als Daten.** Der Kernel
benutzt `noto-sans-mono-bitmap` (vorgerasterte Bitmaps, ~1 MiB), ein
Ring-3-Prozess hat sie nicht und bekommt sie auch nicht — es gibt keinen
Syscall, der Kernel-Schriften herausgibt (`docs/grenzen.md`). Das Toolkit
braucht die Glyphen aber gar nicht: Es braucht **Masse** (um zu layouten)
und einen, der **malt**. Ersteres ist dieses Trait, Letzteres die Leinwand.

`groesse` ist ein `i32` und kein `RasterHeight`: Der Typ aus der
noto-Kiste ist genau die Sorte Abhängigkeit, die hier nicht hindurchdarf.
Die Zahl bedeutet Pixelhöhe; welche Raster der Wirt daraus macht, ist seine
Sache.

**Ehrliche Folge:** Ein Prozess ohne Schrift-Syscall muss seine eigene
mitbringen. `uidemo` tut das mit dem 5×7-Raster aus `libspeed::fenster` —
das sieht anders aus als der Kernel-Desktop, und das ist der sichtbare
Preis dieser Trennung. Er steht in §7.

### 2.3 `Uhr` — eine Zahl

```rust
pub trait Uhr {
    fn us(&self) -> u64;
}
```

Gebraucht für den Cursor-Blink im Textfeld und die Doppelklick-Erkennung im
`UiFenster` (500 ms, 6 px). Mehr nicht. Das ist die einfachste der drei —
und die einzige, bei der beide Wirte dieselbe Quelle haben (`zeit::us_seit_boot`
bzw. `libspeed::zeit_jetzt` × 1000).

---

## 3. Die versteckten Kopplungen — gefunden, bevor Code entstand

Die Aufgabe verlangte ausdrücklich, weitere Kopplungen VOR dem Code
aufzuschreiben. Sechs sind aufgefallen, und **die erste ist grösser als die
drei genannten zusammen.**

### 3.1 (die grösste) `Zeichner<'_, FensterPuffer>` — das Malen selbst

Der heutige Widget-Trait lautet:

```rust
fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck);
```

Das ist eine Kopplung an **zwei** Kernel-Typen auf einmal: an den
Zeichner-Algorithmus (`src/grafik.rs`, Bresenham, Alpha-Blending, Clipping)
und an den konkreten Fenster-Puffer. `Zeichner` ist zwar generisch über
`Zeichenflaeche`, aber das Trait selbst ist Kernel, und der Puffer ist es
auch.

**Entscheidung: ein `Leinwand`-Trait, das der Wirt implementiert.**

```rust
pub trait Leinwand {
    fn masse(&self) -> (i32, i32);
    fn clip(&self) -> Option<Rechteck>;
    fn clip_setzen(&mut self, clip: Option<Rechteck>);
    fn fuellen(&mut self, r: Rechteck, farbe: Farbe);
    fn abgerundet(&mut self, r: Rechteck, radius: i32, farbe: Farbe);
    fn rahmen(&mut self, r: Rechteck, farbe: Farbe);
    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe);
    fn text(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe);
    fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32);
}
```

Neun Methoden — genau die, die die Widgets heute benutzen (nachgezählt:
`rechteck_fuellen` 10×, `rechteck_rahmen` 9×, `text` 8×,
`rechteck_abgerundet` 7×, `linie` 5×, `clip_setzen` 5×, `icon` 3×,
`clip` 1×). `kreis_*`, `verlauf_*`, `blit` und `puffer_blit` benutzt **kein**
Widget — sie bleiben beim Kernel.

**Warum HOHE Operationen und keine Pixel:** Ein `set_pixel`-Trait wäre
schmaler, aber dann müsste speedui Bresenham, Alpha-Blending und
Rundungs-Ecken selbst mitbringen — also den halben Zeichner duplizieren, und
zwar in einer zweiten, langsameren Fassung ohne die Zeilen-Schnellpfade aus
Serie 3 (`flaeche_zeile_fuellen`). Die neun Operationen sind der Schnitt, an
dem beide Wirte ihre eigenen Schnellpfade behalten.

**Der Preis, ehrlich:** Das ist eine `dyn`-Aufruf pro Zeichenoperation statt
eines statischen. Bei ~50 Operationen je Fenster-Neuaufbau ist das nicht
messbar; bei einem Pixel-Trait wäre es Millionen.

### 3.2 `DecodedKey` — der Eingabe-Typ

`UiEreignis::Taste(DecodedKey)` bindet die Kiste an `pc_keyboard`. Der
Kernel dekodiert dort Scancodes; ein Ring-3-Prozess bekommt über
`fenster_ereignis` (Serie 8, Teil 1) schon fertige Unicode-Zeichen und
Sondertasten-Codes und hat `pc_keyboard` nie gesehen.

**Entscheidung: speedui bekommt ein eigenes `Taste`-Enum.**

```rust
pub enum Taste {
    Zeichen(char),
    Hoch, Runter, Links, Rechts, Pos1, Ende, BildHoch, BildRunter, Entf,
    F(u8),
}
```

Beide Wirte übersetzen: der Kernel aus `DecodedKey`, `uidemo` aus den
ABI-Codes. Das ist **die einzige Stelle, an der beide Seiten dasselbe
zweimal tun** — und es ist Absicht, denn sie tun es aus verschiedenen
Quellen. Vergleiche `docs/syscalls.md`: Eine ABI ist ein Vertrag, kein
geteilter Header.

### 3.3 `Rechteck` und `Farbe` — Typen, die in jeder Signatur stehen

Beide sind **reine Daten ohne Kernel-Bezug** (`Rechteck`: vier `i32` plus
`enthaelt`/`schneiden`/`umschliessen`; `Farbe`: vier `u8` plus Mischen).

**Entscheidung: sie ziehen mit nach speedui, der Kernel re-exportiert sie.**
`grafik::Rechteck` bleibt als Name gültig (`pub use speedui::Rechteck`), also
ändert sich in `fenster/mod.rs` und den Apps keine Zeile.

Die Alternative — je einen eigenen Typ auf beiden Seiten und Konvertierung
an der Grenze — wäre bei einem Typ, der in **jeder** Signatur vorkommt,
Lärm ohne Gewinn. Bei `Farbe` gibt es einen kleinen Unterschied: Der Kernel
kennt `Farbe` (RGB, 3 Byte, der Framebuffer-Typ) **und** `Rgba` (4 Byte, der
Zeichner-Typ). speedui nimmt nur einen — RGBA, weil Alpha in der UI
gebraucht wird (Hover-Flächen).

### 3.4 `Icon` — Daten, keine Logik

`grafik::Icon` ist `[&'static str; 16]` mit einer Palette aus 16 Buchstaben.
Reine Daten und eine reine Funktion. **Sie ziehen mit** (Typ und
Paletten-Funktion); die konkreten Icons (`ICON_ORDNER` & Co.) bleiben beim
Kernel, weil sie zum Kernel-Erscheinungsbild gehören — ein Prozess darf
eigene definieren.

### 3.5 `fs::` im Datei-Dialog

`ui::dialog::DateiDialog` listet Verzeichnisse (`fs::mit_fs(|f| f.liste(…))`)
und rechnet Pfade zusammen. Das ist die Kopplung mit dem grössten
Bauchweh-Potenzial: Ein Toolkit, das ein Dateisystem kennt, ist kein
Toolkit.

**Entscheidung: ein `Dateiquelle`-Trait mit drei Methoden.**

```rust
pub trait Dateiquelle {
    fn liste(&self, ordner: &str) -> Vec<(String, bool)>;   // (Name, ist_ordner)
    fn anhaengen(&self, basis: &str, name: &str) -> String;
    fn aufloesen(&self, basis: &str, eingabe: &str) -> String;
}
```

Die beiden Pfad-Methoden gehören dazu, obwohl sie reine Stringarbeit sind:
Was ein Pfad IST (`/` als Trenner, `..`, Mount-Präfixe), ist eine Eigenschaft
des Wirts, nicht des Toolkits.

### 3.6 `shell::editor::ZeilenEditor` im Textfeld

Das Textfeld-Widget benutzt den Zeilen-Editor der **Shell** — Eingabezeile,
Verlauf, Tab-Vervollständigung. Eine Abhängigkeit vom Toolkit auf die Shell
ist genau verkehrt herum.

Der Editor ist bis auf **eine Zeile** rein (`fs::pfad_aufloesen` in der
Tab-Vervollständigung). **Entscheidung: er zieht nach speedui um**, und die
eine Zeile wird zur vierten Methode des schon vorhandenen
`Vervollstaendiger`-Traits. Die Shell benutzt ihn danach aus der Kiste — der
Kernel hängt ohnehin an speedui, umgekehrt nicht.

### 3.7 Allokation und `Send`

`alloc` haben beide Wirte (der Kernel seit Serie 1, ein Prozess seit Serie 7,
Teil 3). Kein Problem.

`Widget: Send` bleibt: Der Kernel hält den Widget-Baum in einem
`Mutex<FensterManager>`, braucht die Schranke also. Ein einzelner Prozess
braucht sie nicht, sie kostet ihn aber nichts.

---

## 4. Wie die Traits an die Widgets kommen

Zwei Bündel, damit nicht vier Parameter durch jede Signatur wandern:

```rust
/// Was ein Widget zum RECHNEN braucht.
pub struct UiKontext<'a> {
    pub thema: &'a dyn Thema,
    pub schrift: &'a dyn Schrift,
    pub uhr: &'a dyn Uhr,
}

/// Was ein Widget zum ZEICHNEN braucht: Kontext + Leinwand.
pub struct Maler<'a> {
    pub leinwand: &'a mut dyn Leinwand,
    pub kontext: UiKontext<'a>,
}
```

Der Widget-Trait sieht danach so aus:

```rust
pub trait Widget: Send {
    fn wunschgroesse(&self, k: &UiKontext) -> (i32, i32);
    fn zeichnen(&self, m: &mut Maler<'_>, bereich: Rechteck);
    fn ereignis(&mut self, e: &UiEreignis, bereich: Rechteck, k: &UiKontext) -> UiReaktion;
    // flex / hat_fokus / fokus_weiter / fokus_entfernen unverändert
}
```

**Ein Parameter mehr bei `wunschgroesse` und `ereignis`, null bei
`zeichnen`** (dort ersetzt `Maler` den bisherigen `Zeichner`). Das ist der
gesamte Signatur-Bruch.

### Die verworfene Alternative: globale Registrierung

Naheliegend wäre gewesen, die drei Trait-Objekte EINMAL in speedui zu
hinterlegen (`speedui::umgebung_setzen(…)`) und weiter `metrik()` zu
schreiben. Dann hätte sich **keine einzige Signatur** geändert, und der
Umbau wäre auf ein Suchen-und-Ersetzen geschrumpft.

Verworfen, aus zwei Gründen:

1. **Es wäre keine Umkehr, sondern eine Umbenennung.** Die Abhängigkeit
   bliebe ambient — nur der Ort des Globals wechselte. Die Aufgabe sagt
   ausdrücklich „zu ARGUMENTEN werden".
2. **Attrappen-Tests bräuchten globalen Zustand.** Mit Parametern ist ein
   Test eine Zeile: Attrappe bauen, Widget fragen, fertig — und zwei Tests
   können gleichzeitig verschiedene Themen prüfen.

Der Preis (ein Parameter mehr an zwei Methoden, ~12 Widget-Implementierungen
anzupassen) ist einmalig.

---

## 5. Was NICHT umzieht — und warum

| bleibt im Kernel | Grund |
|---|---|
| `Zeichner`, `Zeichenflaeche`, `DoppelPuffer`, `FensterPuffer` | die Zeichen-MASCHINE samt Zeilen-Schnellpfaden; speedui bekommt sie über `Leinwand` |
| `theme::Theme`, `theme::metrik()` | zwei Drittel davon ist Fenster-Dekoration |
| die Schrift (`noto-sans-mono-bitmap`) | ~1 MiB Bitmaps; ein Prozess bekommt sie nicht |
| `ICON_ORDNER` & Co. | Kernel-Erscheinungsbild |
| `FensterManager`, Titelleiste, Snap, Alt+Tab, Taskleiste | Fenster-Verwaltung, nicht Widgets |
| das **Kontextmenü-Overlay** | es ist ein Manager-Overlay mit eigenem Offscreen-Puffer und einer Empfänger-`FensterId`; nur seine LISTE ist eine speedui-`ScrollListe` |
| `ui::App` / `AppFenster` | die App-Registry, `NachLock`, die Deadlock-Regel — alles Kernel-Mechanik |

Die letzten beiden Zeilen sind eine bewusste Einschränkung gegenüber der
Aufgabenstellung: Sie nennt „Dialog/Kontextmenü" in der Umzugsliste.
`dialog::bestaetigung` und `dialog::DateiDialog` ziehen um (sie sind
Widget-Bäume). Das Kontextmenü ist dagegen kein Widget, sondern ein
Fenster-Manager-Overlay — es umzuziehen hiesse, den Fenster-Manager
umzuziehen. Steht so in §7.

---

## 6. Der Regressionstest

**Alle bestehenden Apps müssen unverändert weiterlaufen**: Explorer,
Einstellungen, Task-Manager, SpeedText. „Unverändert" heisst hier: in ihrem
VERHALTEN. Ihre vier eigenen Widget-Implementierungen (`FarbFeld`,
`IconBild`, `KlickFlaeche`, `CpuGraph`) müssen die neuen Signaturen
mitmachen — das ist der Beweis, dass die Grenze auch für App-Autoren
benutzbar ist und nicht nur für die Kiste selbst.

Dazu:

* die bestehenden Toolkit-Tests laufen in der Kiste weiter (sie sind
  `#[test_case]` im Kernel-Testframework — sie werden zu gewöhnlichen
  `#[test]`s in speedui, wo sie auf dem HOST laufen, ohne QEMU);
* neue Tests an den Trait-Grenzen mit **Attrappen**;
* ein Test, dass die Kiste **ohne Kernel baut**.

---

## 7. Was diese Trennung KOSTET (vorher aufgeschrieben)

Damit es später nicht als Überraschung erscheint:

* **Ein Prozess hat keine Kernel-Schrift.** `uidemo` sieht anders aus als der
  Kernel-Desktop. Die saubere Lösung wäre ein Schrift-Syscall oder eine
  mitgelieferte Bitmap-Schrift im User-Space — beides eigene Vorhaben.
* **Ein `dyn`-Aufruf je Zeichenoperation.** Nicht messbar bei UI-Mengen,
  aber es ist ein Unterschied.
* **Das Kontextmenü bleibt Kernel-seitig** (siehe §5). Ein Prozess kann
  heute kein Kontextmenü öffnen.
* **Der Fokus-Rahmen und die Titelleiste** bleiben Sache des jeweiligen
  Wirts — im Prozess malt sie niemand, weil der Kernel sie ohnehin um das
  Fenster herum zeichnet.

---

## 8. Der Bericht

Wie sauber die Trennung wirklich wurde, welche Abhängigkeit die zäheste war
und was dupliziert werden musste: **§9 dieses Dokuments, nach dem Umbau
ergänzt.** Die Erwartung vorher: Die Schrift wird zäh, weil sie Daten ist und
kein Verhalten; und `Zeichner` wird der grösste Posten, weil er in jeder
Signatur steht.

---

## 9. DER BERICHT — wie sauber es wirklich wurde

*Nach dem Umbau geschrieben. Die Erwartung aus §8 steht oben; hier steht,
was daraus geworden ist.*

### 9.1 Das Ergebnis in Zahlen

| | |
|---|---:|
| `speedui/Cargo.toml`, Zeilen im `[dependencies]`-Block | **0** |
| Zeilen in der Kiste (ohne Tests) | ~1 900 |
| Traits, die sie verlangt | 5 |
| Farbrollen / Masse (die geschlossene Liste) | 13 / 9 |
| Widget-Implementierungen ausserhalb der Kiste | 4 |
| Toolkit-Tests, die jetzt auf dem **Host** laufen | **29** |
| Laufzeit dieser Tests | **0,00 s** (vorher: ein QEMU-Start) |

Der letzte Punkt war kein Ziel, sondern ein Nebenprodukt — und der
angenehmste. Ein Layout-Test, der einen Bootvorgang braucht, wird selten
ausgeführt.

### 9.2 Die zäheste Abhängigkeit: NICHT die Schrift

Die Erwartung war falsch. Die Schrift war **die leichteste der drei**: Ein
Toolkit braucht von ihr nur Masse, und Masse sind drei Methoden. Die
Glyphen wollte es nie haben.

**Am zähesten war der `Zeichner`** — und zwar aus einem Grund, der vorher
nicht auf der Liste stand: Er ist nicht nur eine Abhängigkeit, sondern eine
**Aufruf-Konvention**. `metrik()` zu ersetzen ist Suchen-und-Ersetzen;
`Zeichner<'_, FensterPuffer>` zu ersetzen ändert jede Signatur, jede
Aufrufstelle, und — der eigentliche Schmerz — die **Borrow-Struktur**:

```rust
let k = &m.kontext;   // haelt den Maler fest
m.fuellen(...);       // will ihn mutable — geht nicht
```

Diese eine Zeile kostete sechs Fehlermeldungen in `widgets.rs` und
dieselben vier in den App-Widgets. Die Lösung (`let kontext = m.kontext;`
— `UiKontext` ist `Copy`) ist zwei Zeichen lang und steht jetzt mit
Begründung in jedem `zeichnen()`. So etwas findet kein Entwurf vorher; es
findet der Compiler.

**Platz zwei: `fs::` im Datei-Dialog.** Nicht wegen der Umstellung (drei
Methoden, eine halbe Stunde), sondern weil dabei auffiel, dass der Dialog
`fs::NodeTyp` in seinem **Zustand** trug — nicht nur in einem Aufruf. Aus
`Vec<(String, NodeTyp)>` wurde `Vec<(String, bool)>`. Eine Abhängigkeit im
Datentyp ist zäher als eine im Funktionsaufruf, weil sie sich nicht
lokalisieren lässt.

**Platz drei: die Uhr.** Sie war in einer Minute erledigt — und hat den
grössten Testgewinn gebracht (§9.5).

### 9.3 Was dupliziert werden musste

Ehrlich aufgezählt, und es ist wenig:

1. **Die Tastatur-Übersetzung, zweimal.** `DecodedKey -> Taste` im Kernel
   (`ui/wirt.rs`), ABI-Code `-> Taste` im Prozess (`uidemo`). Das ist
   Absicht und keine Nachlässigkeit: Beide übersetzen **aus verschiedenen
   Quellen** in dasselbe Ziel. Ein gemeinsamer Typ hätte die Kiste an
   `pc_keyboard` gebunden — an eine Kiste, die nur eine Seite braucht.
   Dasselbe Argument wie bei der ABI in `docs/syscalls.md`: ein Vertrag,
   kein geteilter Header.
2. **Die Farb-Umrechnung, dreimal** (`speedui::Farbe` ↔ `grafik::Rgba` im
   Kernel, `-> u32` im Prozess). Jeweils vier Feldzuweisungen. Der Preis
   dafür, dass die Kiste einen eigenen Farbtyp hat — und der ist nötig,
   weil `Rgba` im Kernel wohnt.
3. **Eine zweite Zeichen-Implementierung im Prozess** (`PixelLeinwand`,
   ~70 Zeilen). Das ist keine Duplikation der Kiste, sondern die zweite
   Wirts-Implementierung — genau das, was das Trait ermöglichen sollte.
   Sie ist schlechter als die des Kernels (keine runden Ecken, nur
   waagerechte/senkrechte Linien), und **das ist erlaubt**: Der Wirt
   entscheidet, wie gut er malt.

Was **nicht** dupliziert werden musste: der Layout-Algorithmus, das
Event-Routing, die Fokus-Kette, die Schadens-Kombination, der
ZeilenEditor, die Dialoge. Also alles, worauf es ankommt.

### 9.4 Was NICHT umgezogen ist — und warum das keine Ausrede ist

Zwei Dinge aus der Umzugsliste der Aufgabe sind geblieben, beide mit
demselben Grund: **Sie sind keine Widgets.**

* **Das Kontextmenü** ist ein Fenster-Manager-Overlay mit eigenem
  Offscreen-Puffer und einer Empfänger-`FensterId`. Seine LISTE ist eine
  speedui-`ScrollListe` — der Rest ist Fensterverwaltung. Es umzuziehen
  hiesse, den Fenster-Manager umzuziehen.
* **`ui::texteditor`** (SpeedTexts mehrzeiliger Editor) braucht
  `Arc<Mutex<TextPuffer>>`, um den Puffer zwischen App und Widget zu
  teilen. `spin::Mutex` wäre eine Abhängigkeit gewesen; einen eigenen
  Spinlock zu schreiben wäre echte Duplikation für ein Widget, das nur
  **eine** App benutzt. Er steht auch nicht in der Umzugsliste.

Dass er trotzdem **`speedui::Widget` implementiert**, ist ein Nebeneffekt,
der mehr wert ist als der Umzug es gewesen wäre: Ein 700-Zeilen-Widget
ausserhalb der Kiste, das sauber an ihr andockt, beweist, dass die Grenze
auch für App-Autoren benutzbar ist — und nicht nur für die Kiste selbst.

### 9.5 Der unerwartete Gewinn: die Attrappen

Das Wertvollste an dieser Umkehr ist nichts, was in der Aufgabe stand.

`speedui::attrappe` liefert einen Wirt aus Pappe — und die Leinwand darin
(`MalProtokoll`) **zeichnet nicht, sie schreibt mit**. Damit lässt sich
prüfen, was vorher unprüfbar war:

* *„Fragt der Button wirklich das Thema?"* — vorher hätte man Pixel
  vergleichen müssen (bricht bei jeder Farbanpassung, sagt nichts über die
  Ursache). Jetzt: Zwei Zeilen gegen eine Liste von Zeichen-Operationen.
* *„Blinkt der Cursor nach der Uhr?"* — vorher hätte der Test **warten**
  müssen. Jetzt STELLT er die Zeit (`TestUhr::setzen`) und prüft, dass
  genau ein Strich mehr gezeichnet wird.
* *„Hängt die Layout-Breite an der Schrift?"* — zwei Wirte mit
  verschiedenen Zeichenbreiten, zwei verschiedene Ergebnisse. Mit einer
  eingebauten Schrift wäre dieser Test gar nicht formulierbar.

Genau das war das Argument gegen die globale Registrierung (§4), und es
hat sich als das stärkere erwiesen — nicht die Reinheit, sondern die
Prüfbarkeit.

### 9.6 Was die Trennung gekostet hat

* **Ein Parameter mehr** an `wunschgroesse` und `ereignis`. Zwölf
  Implementierungen anzupassen war eine Stunde mechanischer Arbeit.
* **Ein `dyn`-Aufruf je Zeichenoperation.** Bei ~50 Operationen je
  Fensteraufbau nicht messbar (der Compositor-Frame liegt unverändert bei
  279 µs Vollbild / 155 µs Drag).
* **`uidemo` sieht anders aus als der Kernel-Desktop.** Andere Schrift,
  eckige Ecken. Das ist der sichtbare Preis, und er steht in §7 — er war
  vorher angekündigt und ist keine Überraschung.
* **Eine Zeile Verhalten musste sich ändern**, und sie ist lehrreich: Die
  Standard-`aufloesen`-Implementierung des `Vervollstaendiger`-Traits
  normalisierte den Schluss-Schrägstrich nicht, und der **unveränderte**
  Serie-3-Test `test_tab_eindeutig` fiel darüber. Nicht der Test war
  falsch — die Kiste war es. Das ist der Wert unveränderter Tests bei
  einem Umzug: Sie sind die einzige Instanz, die merkt, wenn sich etwas
  bewegt hat, das sich nicht bewegen sollte.

### 9.7 Das Urteil

Bei `speedhttp` war die Aussage: *„Der Parser musste nicht angefasst
werden."* Hier ist sie eine andere und ehrlicher:

> **Das Toolkit musste angefasst werden — aber nur an den Stellen, an
> denen es den Wirt fragt. Die LOGIK (Layout, Routing, Fokus, Schaden)
> steht Zeile für Zeile unverändert da.**

Der Beweis dafür sind nicht die Zeilenzahlen, sondern die vier Apps: Sie
laufen unverändert, obwohl unter ihnen das ganze Fundament ausgetauscht
wurde.
