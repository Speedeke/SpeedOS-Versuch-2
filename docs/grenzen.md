# Was SpeedOS NICHT kann

**Diese Seite ist der einzige Ort, an dem alle bekannten Lücken zusammen
stehen.** Nicht verstreut über sieben Serien, nicht in Fußnoten, nicht
zwischen den Zeilen.

Sie existiert, weil das Gegenteil die eigentliche Gefahr ist: Ein System, das
seine Grenzen nicht kennt, wird an einer Stelle vertraut, an der es nichts
leistet. Eine grüne Testsuite sagt, dass das Gebaute funktioniert — sie sagt
nichts über das Nicht-Gebaute.

Wer SpeedOS benutzt, soll hier nachsehen können und in einer Minute wissen,
worauf er sich **nicht** verlassen darf.

---

## 1. TLS und Zertifikate

### Keine Sperrlisten-Prüfung (weder OCSP noch CRL)

**Ein gestohlenes, noch nicht abgelaufenes Zertifikat wird akzeptiert.**

Das ist die schwerwiegendste Lücke der TLS-Implementierung. Wenn der private
Schlüssel eines Servers gestohlen und sein Zertifikat daraufhin gesperrt
wird, merkt SpeedOS davon nichts — bis das Zertifikat von selbst abläuft.

Warum nicht nachgerüstet: Klassisches OCSP verrät dem Aussteller das
Surfverhalten (jeder Abruf meldet, welche Seite man besucht) und scheitert in
der Praxis **weich**: Ist der Responder nicht erreichbar, verbinden alle
gängigen Browser trotzdem. Das ist eine Anzeige, kein Mechanismus. Der
richtige Weg wäre **OCSP-Stapling** (der Server liefert den Nachweis selbst
mit) — ein eigenes Vorhaben. Ausführlich: `docs/tls-vertrauen.md` §3a.

### Der Vertrauensanker wird von Hand aktualisiert

`assets/ca-bundle.pem` kommt von `curl.se/ca/cacert.pem` und wird per
`tools/ca_bundle_holen.ps1` geholt — **manuell**. Herkunft, Datum, SHA-256
und Zertifikatszahl stehen in `assets/ca-bundle.herkunft.txt`.

Die gefährliche Richtung ist dabei nicht die naheliegende: Ein zu altes
Bündel lehnt nicht zu viel ab, es **vertraut zu viel** — zurückgezogene
Wurzeln bleiben drin.

Automatisch ginge es nur mit einem eingebauten Signaturschlüssel, sonst ist
es ein Henne-Ei-Problem: Sicherer Abruf braucht TLS braucht Wurzeln.

### Die Vertrauensdatei ist nicht signiert

Wer `/platte/system/ca-bundle.pem` ersetzen kann, bestimmt, wem das System
vertraut. SpeedOS kennt keine Rechteverwaltung und keine Signatur darauf.

Was geprüft ist: Eine **kaputte** Datei führt nicht dazu, dass weniger
geprüft wird, sondern dass **gar nicht verbunden** wird
(`tests/sicherheit.rs::test_kaputter_vertrauensanker_verbindet_nicht`).
Gegen eine *bösartig ersetzte* Datei hilft das nicht.

### Kein `close_notify`-Zwang

Schließt die Gegenstelle die TCP-Verbindung ohne TLS-Abschiedsgruß, gilt der
Strom als beendet — von einem **Truncation-Angriff** nicht zu unterscheiden.
Erzwänge man es, wäre die halbe Welt unerreichbar (viele Server schließen bei
`Connection: close` einfach). Was schützt, liegt eine Schicht höher: Der
HTTP-Parser prüft gegen `Content-Length` bzw. den 0-Chunk.

### Ebenfalls nicht dabei

Kein Certificate Transparency, kein Public-Key-Pinning, keine Benutzer-CAs,
kein „trotzdem fortfahren"-Dialog (**letzteres mit Absicht** — siehe
`docs/tls-verbindung.md` §4).

### Der Krypto-Anbieter ist Alpha

`rustls-rustcrypto` ist **0.0.2-alpha**. Diese Warnung bleibt stehen, bis er
es nicht mehr ist. Was dagegen unternommen wurde: Er läuft unprivilegiert in
Ring 3, in eigenem Adressraum, mit NX-Heap und harter Speichergrenze — ein
Fehler dort trifft einen Prozess, nicht den Kernel.

---

## 2. Zeit

### Kein NTP

Die Uhr kommt aus der CMOS-RTC und wird gegen **eine** Plausibilität
geprüft: Sie kann nicht vor dem Bau-Datum des Kernels liegen. Das fängt den
häufigsten Fall (leere Pufferbatterie → 1.1.2000).

**Es findet NICHT:** eine Uhr, die um Stunden oder Tage falsch geht, und eine
absichtlich vorgestellte Uhr. Beides hat direkte Folgen für die
Zertifikatsprüfung — ein vorgestelltes Datum lässt abgelaufene Zertifikate
gültig erscheinen.

Von Hand stellen: `einstellungen::zeit_setzen_lokal` (persistiert auf
/platte). Die CMOS-Uhr wird dabei **nicht** geschrieben.

---

## 3. Netz

### Kein Treiber für echte Netzwerk-Hardware

SpeedOS hat **einen** NIC-Treiber: `virtio-net`. Der existiert nur in
virtuellen Maschinen. **Auf echter Hardware hat SpeedOS kein Netz** — kein
DHCP, kein DNS, kein TLS, kein `hole`.

Der USB-Live-Boot funktioniert (Desktop, Dateisystem, Shell), aber offline.
Was fehlte, wäre ein Treiber für eine verbreitete Karte (e1000, rtl8139) —
die Schicht darüber müsste sich nicht ändern, denn alles redet nur mit dem
Trait `netz::NetzGeraet`.

### TCP ist ein bewusstes Minimal-Viable

Kein Congestion-Control, kein Fast-Retransmit, kein SACK, kein
Window-Scaling, **keine Out-of-Order-Reassembly** (verworfene Segmente führen
zu kumulativem ACK und Retransmit — korrekt, aber unter Paketverlust
langsam). Umfang, Lücken und die registrierte Reißleine:
`docs/tcp-scope.md`.

Gemessen unter künstlichem Verlust: bei 10 % noch 5/5 saubere Abrufe, bei
20 % 2/3. Das Fehlerbild ist ausschließlich **Langsamkeit**, nie falsche
Daten.

### Keine IP-Fragmentierung

Fragmente werden erkannt und **verworfen** (kein Reassembly).

### IPv4 only

Kein IPv6.

---

## 4. Prozesse und Speicher

### Die P1-Tabellen-Buchhaltung

`memory::allocate_pages` vergibt virtuellen Raum monoton; alle 512 Seiten
bleibt eine P1-Tabelle im Kernel-Adressraum zurück (~1 Frame je 100
Prozesse). Das ist **kein Prozess-Leck** — der Adressraum eines Prozesses
fällt vollständig —, sondern eine Eigenschaft des Kernel-Allocators.

Sie wird in den Speicher-Tests **ausgerechnet statt weggelassen**: Beim
Speicher-Pass über 50 HTTPS-Zyklen war die erlaubte Schranke 34 Frames,
gemessen wurden **0**.

Behebung wäre ein Freilisten-Allocator für virtuelle Bereiche.

### Kein Auslagern, kein Überbuchen

Der User-Heap wächst auf Anforderung bis 12 MiB und wird nie zurückgegeben.
Ist er voll, ist er voll.

### Ein einziger Ausführungsstrang je Prozess

Keine Threads. Kein SMP — SpeedOS läuft auf **einem** Kern.

---

## 5. Dateisystem

* **FAT32 ist NUR-LESE.** Jeder Schreibweg endet in `IoFehler::NurLesen`.
* **SpeedFS hat kein Journal.** Konsistenz kommt aus der Schreibreihenfolge;
  ein Absturz hinterlässt höchstens Block-Lecks, nie falsche Zeiger
  (`docs/speedfs-format.md` §7, per Folter-Test nachgewiesen).
* **Der Block-Cache ist Write-Through** — einfach und ehrlich, aber
  langsamer als Write-Back.
* Maximale Dateigröße ~4,09 MiB (22 direkte + 1 einfach-indirekter Zeiger).
* Der ATA-Treiber kann **LBA28** — maximal 128 GiB.
* **Keine Rechte, keine Benutzer, keine Quotas.** Jeder Prozess darf alles,
  was das VFS hergibt.

---

## 6. Oberfläche

* Das **Widget-Toolkit** ist seit Serie 8, Teil 2 eine wirtsfreie Kiste
  (`speedui/`) — ein Prozess bekommt Knöpfe, Listen und Textfelder
  (`userland/uidemo`). **Was er NICHT bekommt: eine Schrift.** Die
  vorgerasterten Kernel-Fonts sind Kernel-Daten, es gibt keinen
  Schrift-Syscall; ein Prozess muss seine eigene mitbringen und sieht
  deshalb anders aus als der Desktop. Ebenfalls kernel-seitig geblieben:
  das **Kontextmenü** (es ist ein Fenster-Manager-Overlay) und der
  mehrzeilige **Texteditor** von SpeedText.
  Bericht: `docs/speedui-trennung.md` §9.
* ~~**Ein Fenster über den ganzen 4K-Schirm passt nicht in den
  User-Heap.**~~ **GESCHLOSSEN in Serie 8, Teil 7.** Der Pixelpuffer eines
  Programms liegt auf dessen eigenem Heap, und der war auf 12 MiB
  gedeckelt — 3840 × 2088 × 4 Byte sind 32,1 MiB, fast das Dreifache. Die
  damals genannten zwei Wege („grösseres Prozess-Layout oder Streifen")
  sind inzwischen **beide** gegangen: `prozess::HEAP_MAX_BYTES` steht auf
  **64 MiB** (Lücke 96 MiB), und der Browser malt in Streifen. Gerissen
  ist die alte Grenze am Ende nicht am Fenster, sondern am DOKUMENT — der
  Wikipedia-Artikel sprengte sie schon in 720p. Zahlen und Begründung:
  `docs/browser-rendern.md` §5. Die Grenze ist **angehoben, nicht
  abgeschafft**: Darüber gibt es weiterhin `KeinPlatz` (14).
* Ein Prozess-Fenster bekommt **keine Schrift vom Kernel** (die
  vorgerasterten Fonts sind Kernel-Daten, es gibt keinen Syscall dafür),
  **kein eigenes Icon**, **keine Zwischenablage** und **keine
  Modifikatortasten** in der Ereignis-ABI.
* Die Framebuffer-Konsole ist **Latin-1**: Gedankenstriche und typografische
  Anführungszeichen werden zu `?`.
* **Der Terminal-Rückblick hat Grenzen** (Bild auf/ab, Mausrad): 1000 Zeilen
  im Fenster, 300 in der Vollbild-Konsole. Er überlebt eine
  Größenänderung des Fensters (Maximieren/Wiederherstellen), aber die
  gespeicherten Zeilen werden dabei **nicht neu umbrochen**: Was für die
  neue Breite zu lang ist, wird rechts abgeschnitten. Ein echter Neu-Umbruch
  wäre ein eigenes Vorhaben (echte Terminals tun sich damit schwer). Die
  **Boot-Meldungen** vor
  `konsole::rueckblick_einrichten()` sind ebenfalls nicht zurückblätterbar:
  Zu dem Zeitpunkt gibt es den Heap für die Puffer noch nicht, und im
  `print!`-Pfad wird bewusst nie alloziert.
* Kein Markieren und Kopieren mit der Maus im Terminal.

### Schriften: vier Größen, kein Kursivschnitt

*(Serie 8, Teil 3 — vollständig in `docs/schrift-groessen.md`, abfragbar
mit dem Shell-Befehl `schrift`)*

* Schriftgrößen sind **vorgerastert**: 16, 20, 24, 32. Es gibt keinen
  Rasterizer, also keine beliebigen Größen — `font-size: 13px` wird auf
  die nächstliegende vorhandene gerundet.
* **Unterhalb der Fließtextgröße gibt es NICHTS.** Die kleinste Rasterung
  ist zugleich die Fließtextgröße, deshalb können `<small>`, `<h5>` und
  `<h6>` **nicht kleiner werden** als normaler Text; sie werden über das
  Gewicht (fett) und die Farbe unterschieden. Eine Seite mit Fußnoten
  sieht falsch aus. `speedui::text::exakt_moeglich` meldet das je Rolle,
  damit die Einschränkung abfragbar ist statt nur beschrieben.
* Bei **UI-Skalierung 2.0** (Basis 32) dreht es sich um: Dann ist der
  Vorrat nach oben erschöpft, `h1` und `h2` sehen gleich aus.
* **Kursiv ist SIMULIERT.** `noto-sans-mono-bitmap` liefert
  Light/Regular/Bold und keinen Italic-Schnitt; SpeedOS schert die
  Glyphen um ~14°. Ein geschertes `a` bleibt ein gerades `a`, das schief
  steht — echte Kursivformen (einstöckiges `a`, geschwungenes `f`) gibt
  es nicht. `Schrift::kursiv_echt()` liefert `false`, damit niemand etwas
  anderes annimmt. **Fett dagegen ist echt.**
* Nur **Monospace**, nur **Latin-1**. Eine Proportionalschrift gibt es
  nicht, kyrillische/griechische/CJK-Zeichen werden zu `?`.
* Der Ausweg für alles davon wäre ein **TrueType-Rasterizer**
  (`ab_glyph`, `fontdue` — beide `no_std`-tauglich). Eigenes Vorhaben mit
  eigenen Fragen (Glyph-Cache, Hinting, und woher ein Ring-3-Prozess die
  Font-Datei bekommt — es gibt weiterhin keinen Schrift-Syscall).

### Bilder: PNG und JPEG, aber nur so groß wie der Prozess-Heap

*(Serie 8, Teil 3 — vollständig in `docs/bild-entscheidung.md`)*

Seit Serie 8, Teil 3 dekodiert SpeedOS **PNG und JPEG** — in Ring 3
(`libspeed::bild`, `zune-png`/`zune-jpeg`), Ausgabe immer RGBA. Was
weiterhin fehlt:

* **Ein Bild darf höchstens 1 Mi Pixel haben** (`Grenzen::max_pixel`,
  z. B. 1024×1024 oder 1280×819). **Das ist eine HEAP-Grenze, keine
  Format-Grenze:** Ein Prozess hat 12 MiB, und Dateibytes + RGBA-Puffer +
  Fensterpuffer müssen zusammen hineinpassen. **Ein 1920×1080-Foto wird
  abgelehnt.** Dieselbe Wurzel wie das 4K-Fenster oben: Das Prozess-Layout
  ist zu klein, und es zu ändern ist eine ABI-Änderung.
* **Kein GIF, BMP, WebP.** Sie werden an der Signatur *erkannt* und mit
  „kann SpeedOS nicht" abgelehnt — das ist eine Auskunft, keine
  Ratlosigkeit.
* **Keine Animationen.** Von einem APNG wird das erste Bild gezeigt.
* **Keine Farbprofile (ICC), kein Gamma.** Ein Bild mit exotischem Profil
  sieht leicht falsch aus.
* **Chunk-Prüfsummen werden nicht geprüft** (zunes Voreinstellung). Ein
  gekipptes Byte ergibt ein leicht falsches Bild statt gar keins — für
  einen Betrachter die richtige Wahl, und eine Prüfsumme hält ohnehin
  keinen Angreifer auf, der sie neu berechnet.
* **Kein Verkleinern beim Dekodieren.** Ein zu großes Bild wird
  abgelehnt, statt in 1:2/1:4/1:8 dekodiert zu werden (`zune-jpeg` könnte
  das) — der nächste Hebel, wenn die Heap-Grenze drückt.
* Bilder **aus dem Netz** sind noch nicht verdrahtet: `libspeed::netz`
  liefert Bytes, `bild::dekodieren` nimmt Bytes, aber verbunden hat es
  noch niemand. Das macht der Renderer.

### HTML: geparst, aber noch nicht dargestellt

*(Serie 8, Teil 4 — Zuschnitt in `docs/browser-v1.md`)*

SpeedOS **versteht** HTML seit Serie 8, Teil 4 (`speedhtml`, Tokenizer +
DOM mit Fehlererholung, sichtbar mit `htmldump`) — **zeigen** kann es
noch nichts. Es gibt kein CSS, kein Layout und keinen Browser; das sind
die nächsten Schritte. Was am Parser selbst fehlt:

* **Keine Zeichensatz-Erkennung.** Bytes werden als UTF-8 gelesen
  (`from_utf8_lossy`); eine Seite in Latin-1 oder Windows-1252 bekommt
  Ersatzzeichen statt Umlauten. Weder `Content-Type: charset=` noch
  `<meta charset>` werden ausgewertet. Sichtbar falsch, nicht still
  falsch — aber falsch.
* **Nur ~120 benannte Zeichenreferenzen** von 2231. Unbekannte werden
  **durchgelassen** (`&foo;` bleibt stehen), gehen also nicht verloren.
* **Keine CDATA-Abschnitte**, kein Quirks-Modus, kein
  `<template>`-Sonderfall, keine SVG-/MathML-Fremdinhalte.
* **Kein implizites `<tbody>`.** Der Baum bildet ab, was im Dokument
  steht — ein synthetischer Knoten würde `htmldump` zu einer Lüge machen.
  Das Layout muss beide Formen behandeln.
* **Grenzen:** 200 000 Knoten, Tiefe 100, 1 MiB je Textknoten, 256
  Attribute je Tag. Darüber wird der Baum **abgeschnitten** (nicht
  abgelehnt) und `Befund::abgeschnitten` gesetzt. Die Tiefengrenze
  schützt nicht den Parser, sondern alles, was den Baum danach rekursiv
  durchläuft — der User-Stack ist 64 KiB.

### CSS: eine Teilmenge, und sie ist klein

*(Serie 8, Teil 5 — die vollständige Liste steht in
`docs/browser-v1.md` §2.3, sichtbar mit `cssdump`)*

SpeedOS versteht seit Serie 8, Teil 5 eine CSS-Teilmenge und rechnet
Kaskade und Vererbung durch (`speedcss`). Was **nicht** dabei ist:

* ~~Externe Stylesheets werden nicht geholt.~~ **Seit Serie 9, Teil 1
  werden sie geholt** (`browser::stil`); `speedcss` meldet über
  `blaetter_einsammeln` nur noch, *was* eine Seite braucht, und kennt
  weiterhin kein Netz. Was dabei an Grenzen gilt, steht unten beim
  Browser.
* **`@media` wird übersprungen** (sauber, mit balancierter Klammerung —
  die Regeln darin schlagen also *nicht* durch). Bei „mobile first"-Seiten,
  die ihr ganzes Layout in Media-Queries haben, bleibt entsprechend wenig.
  **Seit die externen Blätter da sind, ist das messbar:** lite.cnn.com
  liefert 199 KB CSS, und davon bleiben **16 Regeln** übrig — der Rest
  steckt in Media-Queries (`docs/browser-realitaet.md`, zweite Messung).
* **Kein `@supports`, kein `@layer`, keine Container-Queries** — sie
  laufen über dieselbe At-Regel-Behandlung und verschwinden mitsamt
  Inhalt.
* **Kein `calc()`, keine Custom Properties** (`--x`/`var()`).
* **Keine Einheiten** `vw`/`vh`/`ex`/`ch`/`cm`/`mm`/`in` — sie werden
  abgelehnt, nicht geraten.
* **Keine Kombinatoren** `>` `+` `~`, keine Attributselektoren, kein
  `:not()`/`:nth-child()`, keine Pseudo-Elemente (`::before`). Solche
  Selektoren machen die Regel **unerfüllbar** statt näherungsweise
  passend — `div > p { display: none }` als Nachfahren zu deuten würde
  mehr verstecken als gemeint.
  **Die eine Ausnahme, und sie ist keine:** Das HTML-Attribut `hidden`
  wird beachtet (`kaskade::hidden_deklarationen`, Herkunft *Standard*,
  Spezifität einer Klasse). Es ist kein Stil, sondern eine Aussage der
  HTML-Spezifikation über das Element; dass Browser es als
  `[hidden] { display: none }` im UA-Stylesheet umsetzen, ist eine
  Implementierungsform. Gefunden hat das der zweite Realitäts-Bericht:
  githubs Screenreader-Meldungen blieben trotz 4 678 geladener Regeln
  stehen, weil sie genau daran hängen.
* **Der Inhalt von `<template>` wird nie gezeichnet** (seit Serie 9,
  Teil 1 im Standard-Stylesheet). Er ist ein Bauplan für JavaScript;
  wer ihn zeichnet, zeigt Text, den auch ein echter Browser nie zeigt.
* **`:hover` ist vorbereitet, aber unbenutzt** — die Kaskade kann es, der
  Browser meldet den Zustand noch nicht.
* **Kein Blocksatz**: `text-align: justify` wird gelesen und wie `left`
  gerendert (Wortabstands-Verteilung ist Layout-Arbeit).
* **`border-style` kennt zwei Zustände** (keiner / durchgezogen);
  `dashed`, `dotted`, `double` werden zu durchgezogen.
* **Nur eine Schriftfamilie.** `font-family` wird auf
  `Proportional`/`Monospace` abgebildet, und beide rendern heute mit
  demselben Raster (siehe Schriften oben).
* **Grenzen:** 100 000 Regeln, 256 Selektoren und 256 Deklarationen je
  Regel. Darüber wird abgeschnitten und im `Befund` vermerkt.

### Layout: Blockfluss und Zeilen, mehr nicht

*(Serie 8, Teil 6 — `speedlayout`, sichtbar mit `cssdump --layout`)*

* **Keine Floats.** `float` wird ignoriert; ein umflossenes Bild steht in
  einer eigenen Zeile.
* **Kein `position: absolute/fixed`.** Beide laufen im normalen Fluss
  mit — ein fixierter Kopfbereich steht dann oben im Dokument statt am
  Bildschirmrand.
* **Margin-Kollaps nur zwischen GESCHWISTERN.** Die volle CSS-Regel
  kollabiert auch zwischen Elternteil und erstem/letztem Kind und durch
  leere Kästen hindurch. Sichtbare Folge: Ein `<div>` um einen `<p>`
  bekommt dessen Rand **innen** statt außen.
* **Kein Blocksatz.** `text-align: justify` wird wie `left` gesetzt.
* **`colspan`/`rowspan` werden nicht verteilt.** Eine Zelle mit
  `colspan=3` bekommt die Breite einer Spalte, nicht dreier.
* **Tabellen-Spaltenbreiten in EINEM Durchgang** (Wunschbreite messen,
  proportional herunterskalieren). Keine Mindestbreite aus dem längsten
  Wort, kein Ausgleich zwischen Spalten, die Platz übrig haben.
* **Hintergrund und Rahmen auf INLINE-Elementen gehen verloren.** Ein
  `<span style="background:yellow">` färbt nichts — der Inline-Strom wird
  beim Zeilenbau flachgeklopft, und ein Hintergrund müsste je Zeilenstück
  gemalt werden.
* **`<pre>` bricht nicht um.** Zu lange Zeilen laufen nach rechts hinaus.
* **Zu breiter Inhalt läuft ÜBER**, er wird nicht abgeschnitten
  (`overflow: hidden` gibt es nicht). Das ist Absicht: Stilles
  Abschneiden versteckt Text, und niemand sieht, warum er fehlt.
* **Jedes Wort ist ein eigener Anzeige-Befehl.** Korrekt, aber mehr
  Befehle als nötig — benachbarte Wörter gleichen Stils ließen sich zu
  einem Befehl zusammenfassen. Eine Optimierung für später, keine Lücke.
* **Grenzen:** Verschachtelungstiefe 64 (das Layout ist rekursiv, der
  User-Stack 64 KiB), 100 000 Kästen, 100 000 Zeilen. Darüber wird der
  Teilbaum abgeschnitten und gezählt.

### Der Renderer: es wird gemalt, aber mit der Schrift eines Prozesses

*(Serie 8, Teil 7 — `speedpaint` + `userland/browser`,
`docs/browser-rendern.md`)*

* **Die Schrift des Browsers ist das eingebaute 5×7-Raster von
  `libspeed`** — denn ein Prozess bekommt die vorgerasterten
  Kernel-Fonts nicht (kein Schrift-Syscall, siehe oben). Daraus folgen
  drei sichtbare Einschränkungen:
  * **Nur ASCII, und alles GROSS.** Die Rastertabelle kennt keine
    Kleinbuchstaben (`text` schlägt jedes Zeichen auf Großschreibung) und
    **keine Umlaute**: Aus „HAUPTMENÜ" wird „HAUPTMEN▪". Das gilt nur für
    die Darstellung — `RasterMetrik::text_breite` zählt `chars()`, die
    Zeilen brechen also an der richtigen Stelle um.
  * **Nur ganzzahlige Vergrößerungen** (1–4×). Eine Überschrift, die
    19 px will, bekommt 21.
  * **Fett wird angedeutet** (zweimal mit einem Pixel Versatz),
    **Kursiv gar nicht** — es würde das Raster unleserlich machen.
    Die Breite ändert sich in beiden Fällen nicht, Messung und Zeichnung
    bleiben also beieinander.
* **Bilder werden nur von der PLATTE geladen, nicht aus dem Netz.** Ein
  `<img src="https://…">` bekommt den Platzhalter. Nebenher zu laden
  braucht eine Ereignisschleife, die das kann — das gehört nicht in
  einen Renderer.
* ~~**Ein geladenes Bild löst NIE ein Neu-Layout aus.**~~ **GESCHLOSSEN in
  Serie 8, Teil 8.** Das Layout fragt jetzt über `Metrik::bild_masse` nach
  der Eigengröße, und ein Bild ohne `width`/`height` löst nach dem Laden
  ein Neu-Setzen aus — den berüchtigten Seitensprung. Ein Bild **mit**
  Angabe kostet weiterhin nur sein Rechteck.
* ~~**Keine Links, keine Auswahl, kein Suchen.**~~ **Links GESCHLOSSEN in
  Serie 8, Teil 8** (Klickziele über die `KnotenId` der Anzeige-Befehle,
  aufgelöst mit `speedhttp::verweis_aufloesen`). **Auswahl und Suchen in
  der Seite fehlen weiterhin**, ebenso Zoom.
* **Beim Umlegen auf eine neue Fensterbreite wird der Scroll-Versatz
  nicht inhaltlich mitgeführt.** Er wird nur in den neuen erlaubten
  Bereich geklemmt; nach einer Breitenänderung steht man deshalb an
  einer anderen Stelle des Textes als vorher.
* **Alpha wird gegen WEISS gemischt**, nicht gegen den echten
  Untergrund — `Fenster` gibt den gelesenen Pixel nicht heraus. Auf
  einer hellen Seite fällt das nicht auf, auf einer dunklen schon.

### Der Browser: was er als Programm nicht kann

*(Serie 8, Teil 8 — `userland/browser`, `docs/browser.md`)*

* **Kein JavaScript.** Gar keins. Eine Seite, die ihren Inhalt per Skript
  aufbaut, bleibt leer — der Browser **erkennt das und sagt es** (leeres
  Rendering + `<script>`-Blöcke), statt eine weiße Fläche zu zeigen.
* **Externe Stylesheets werden geholt — aber mit harten Grenzen**
  (seit Serie 9, Teil 1, `browser::stil`): höchstens **10 Blätter** je
  Dokument, **512 KiB** je Blatt, **1,5 MiB** CSS insgesamt, **8 s**
  Frist je Abruf. Darüber wird übersprungen und gezählt; github fordert
  27 an und bekommt 10. Ein Blatt, das nicht lädt, **verhindert die
  Seite nicht** — es steht in der Statuszeile.
* **Geholt wird SERIELL**, nicht parallel. Fünf Blätter sind fünf
  TLS-Handshakes hintereinander. Die Begründung steht im Kopfkommentar
  von `browser::stil`: Der Klient ist blockierend gebaut, parallel wäre
  eine zweite Abrufschicht, und der eigentliche Hebel ist
  HTTP/1.1-Keep-Alive (fehlt noch — `Connection: close` steht fest in
  `anfrage_bauen`), dem Parallelität hinterher im Weg stünde.
* **`@import` wird EINE Ebene tief verfolgt**, tiefer nicht (gezählt in
  `STIL_IMPORTE_IGNORIERT`), höchstens 4 insgesamt, mit Schleifenschutz
  über die aufgelöste Adresse. Ein `@import` nach der ersten Regel wird
  gar nicht erst gemeldet — er wirkt laut Spezifikation in keinem
  Browser.
* **Kein CSS-Cache über die Sitzung hinaus.** Die Blätter liegen im
  Sitzungs-Cache (8 MiB, geteilt mit den Bildern) und sind nach dem
  Schließen weg.
* **Die Blätter werden nach der Kaskade fallengelassen.** Sobald
  `:hover` wirklich gemeldet wird, braucht es sie ein zweites Mal — dann
  fällt es an `Tab::stile_setzen` auf.
* **Formulare werden angezeigt, aber nicht abgeschickt.** Keine Cookies,
  keine Anmeldung, keine Sitzungen.
* **Der Scroll-Versatz wird beim Umlegen auf eine neue Fensterbreite nur
  geklemmt, nicht inhaltlich mitgeführt** — nach einer Breitenänderung
  steht man an einer anderen Stelle des Textes.
* **Höchstens 8 Tabs**, weil mehr nicht in die Leiste passen.
* **Der Verlauf ist pro Tab und nicht dauerhaft** — er endet mit dem
  Fenster. Nur die Lesezeichen und die Startseite überleben
  (`/platte/system/lesezeichen.txt`).
* **Der Cache ist eine Sitzungs-Sache**: kein `Cache-Control`, kein
  `ETag`, kein Verfallsdatum, nichts auf der Platte. Er kann nicht
  veralten, weil er die Sitzung nicht überlebt.
* **Ein Ladevorgang blockiert die Oberfläche.** Der Klient ist synchron;
  vor dem Abruf wird die Ladeanzeige gezeichnet und übertragen, danach
  steht der Browser bis zur Antwort (Frist: 15 s). Nur die **Bilder**
  laden nebenher — eines je Durchgang.
* **`Strg+H` ist nicht von Backspace unterscheidbar** (beide U+0008, die
  Fenster-ABI hat keine Modifikatortasten). Aufgelöst am Kontext:
  im Adressfeld Backspace, sonst Verlauf.

### Was der Browser unter pathologischen Seiten tut

*(Serie-8-Abschluss, gemessen in `tests/browser_boesartig.rs`)*

Die Zusage lautet **nicht** „er verarbeitet alles", sondern: **Der
Prozess darf sterben, Kernel und Desktop bleiben unbeeindruckt.** Das ist
geprüft — nach jedem Angriff läuft ein gewöhnlicher Prozess weiter, und
Frames wie Fenster sind vollständig zurück. Was dabei wirklich passiert:

| Eingabe | Ausgang |
|---|---|
| 10 000-fach verschachteltes HTML | **Stack-Überlauf**, von der Guard-Page gefangen (Prozess tot) |
| 120 000 CSS-Regeln | Heap erschöpft → Exit 102 |
| 3-MiB-Dokument | Heap erschöpft → Exit 102 |
| 400 KiB Müll statt HTML | Heap erschöpft → Exit 102 |
| `<img width="99999" height="99999">` + Bildbomben | läuft sauber durch (geklemmt/abgelehnt) |
| 500 pathologische Verweise | läuft sauber durch |

**Die ehrliche Einordnung:** Die Tiefengrenzen der einzelnen Kisten
greifen jede für sich (DOM 100, Layout 64) — in ihrem *Zusammenspiel*
reicht der 64-KiB-User-Stack für ein extrem verschachteltes Dokument
trotzdem nicht. Es endet sicher (Guard-Page), aber nicht elegant. Ein
Browser, der stattdessen „Seite zu komplex" anzeigt, wäre besser; dafür
müsste die Rekursionstiefe über alle Stufen hinweg *gemeinsam* begrenzt
werden.

### Speicherbedarf

* **Ein Tab mit einer grossen Seite braucht 14–18 MB Heap**
  (Wikipedia-Artikel, 300 KiB Quelltext; bei 4K bis 47 MB, weil der
  Fensterpuffer dazukommt). Bei 64 MiB Prozess-Heap heisst das:
  **etwa drei grosse Seiten gleichzeitig**, nicht acht. Der Browser
  deckelt bei 8 Tabs, aber der Heap ist die frühere Grenze — ein
  vierter grosser Tab kann `KeinPlatz` bekommen.
* Der Sitzungs-Cache darf 8 MiB halten und zählt dazu.

---

## 7. Was ausdrücklich KEINE Lücke ist

Damit die Liste nicht als Wunschzettel missverstanden wird — diese Dinge
fehlen **mit Absicht**, und sie sollen fehlen:

| fehlt | warum das so bleibt |
|---|---|
| ein `--unsicher`-Schalter für TLS | Er wird benutzt, sobald es ihn gibt. Dann schützt TLS vor genau dem Angreifer nicht mehr, der einen dazu bringt. |
| Eigenbau-TLS | Anders als bei TCP gibt es kein messbares Kriterium für eine Reißleine — ein TLS-Bug ist **still**. |
| ein Fallback für ungesäten Zufall | Lieber warten als schwach. Halb gefüllter Zufall sieht aus wie Zufall. |
| Auto-Format einer unformatierten Platte | Formatieren ist eine Nutzer-Entscheidung. |
| Schreiben auf das Boot-Laufwerk | Per Konstruktion unmöglich, nicht per Prüfung. |
| dynamisches Linken (ET_DYN/PT_INTERP) | Ein versehentlicher PIE-Build soll sofort auffallen. |

---

## 8. Wo die Details stehen

| Thema | Dokument |
|---|---|
| TLS-Vertrauensanker, Sperrlisten, PEM-Parser | `docs/tls-vertrauen.md` |
| Warum rustls und nicht Eigenbau | `docs/tls-entscheidung.md` |
| Die verschlüsselte Verbindung, Messzahlen | `docs/tls-verbindung.md` |
| Zufall: Quellen, Anrechnung, Testvektoren | `docs/zufall.md` |
| TCP: Umfang, Lücken, Reißleine | `docs/tcp-scope.md` |
| SpeedFS: Format, Ordering, fsck | `docs/speedfs-format.md` |
| Syscall-ABI | `docs/syscalls.md` |
| Fenster aus Ring 3, Messzahlen, Umstiegskriterium | `docs/fenster-syscalls.md` |
| Toolkit-Trennung, Traits, ehrlicher Bericht | `docs/speedui-trennung.md` |
| Bild-Dekoder: Evaluation, Zahlen, Angriffe | `docs/bild-entscheidung.md` |
| Schriftgrößen, Textmetrik, Fett/Kursiv | `docs/schrift-groessen.md` |
| Browser-V1: Zuschnitt, CSS-Teilmenge, Layout, Zielmarke, Reißleine | `docs/browser-v1.md` |
| unsafe-Flächen | `docs/unsafe-audit-serie6.md`, `docs/unsafe-audit-serie7.md` |
| Echte Hardware | `docs/hardware-log.md`, `docs/usb-boot.md` |

## Browser: CSS-Eigenschaften, die messbar fehlen (Serie 9, Teil 2)

Gemessen an den zehn Seiten des Realitaets-Berichts, sortiert nach der
Zahl der Seiten (Verfahren und volle Tabelle: docs/browser-realitaet.md,
dritte Messung):

* **`overflow` (5/10 Seiten)** — nicht umgesetzt, und zwar ABSICHTLICH.
  `overflow: hidden` heisst abschneiden; das widerspricht der
  Entscheidung aus Serie 8, Teil 6 („zu breiter Inhalt laeuft ueber, er
  wird nicht abgeschnitten"). Solange unsere Kaesten ungenauer bemessen
  sind als die eines echten Browsers, wuerde Zuschneiden Text
  verschwinden lassen, den wir heute zeigen.
* **`position` + `top`/`left`/`right`/`bottom` (4/10, 445x)** — der
  groesste Einzelposten. Feste Kopfzeilen und Overlays stehen deshalb
  mitten im Text statt an ihrem Platz. Eine billige Teilfassung waere
  schaedlich (siehe Bericht); das ist ein eigener Schritt.
* **`float` (4/10, 102x)** — Textumfluss fehlt. Braucht verkuerzte
  Zeilen um das umflossene Element.
* **`opacity` (4/10, 110x)** — auch `opacity: 0` versteckt bei uns
  nichts.
* **`cursor` (4/10, 123x)** — die Zeigerform ist aus Ring 3 gar nicht
  setzbar, die Fenster-ABI kennt sie nicht.
* **CSS-Variablen (`var()`)** und Hersteller-Praefixe werden getrennt
  gezaehlt (`STIL_VARIABLEN`, `STIL_PRAEFIXE`) und nicht umgesetzt.

NEU UNTERSTUETZT seit Serie 9, Teil 2: `white-space`
(normal/nowrap/pre/pre-wrap/pre-line).

## Browser: Textauswahl und Kopieren fehlen (Serie 9, Teil 2)

Die GEOMETRIE ist gebaut und getestet (`speedlayout::textkarte`:
Punkt -> Textstelle, Auswahl -> Rechtecke, zeichenweise und
umlautfest). Was fehlt, ist die Verdrahtung mit der Maus UND ein
Grund, der ueber den Browser hinausgeht:

**Es gibt keinen Ablage-Syscall.** Die Zwischenablage aus Serie 3
(`src/ablage.rs`) ist ein Kernel-Dienst; die Syscall-ABI endet bei 52
(Fenster) und kennt sie nicht. Ein Ring-3-Programm kann heute nichts
in die Ablage legen und nichts daraus holen — das betrifft den Browser,
wuerde aber jedem User-Programm fehlen. Es ist also eine ABI-Erweiterung
(zwei Syscalls ueber copy_in/copy_out auf einen Blatt-Lock) und keine
Browser-Funktion.

## USB: der Controller laeuft, Geraete werden nicht bedient (Serie 9, Teil 3)

Stand nach Teil 3 (docs/xhci.md): Der xHCI-Controller wird gefunden,
ungecacht gemappt, zurueckgesetzt, mit DCBAA/Scratchpad/Command
Ring/Event Ring eingerichtet und laeuft. Port-Status-Aenderungen kommen
als Events an. **Mehr nicht.**

Es fehlen also weiterhin:

* **Geraete werden nicht angesprochen** — kein Slot aktiviert, keine
  Adresse vergeben, kein Deskriptor gelesen, keine Uebertragung.
* **KEINE USB-TASTATUR UND KEINE USB-MAUS.** Damit bleibt die Aussage
  der Serie-9-Bestandsaufnahme unveraendert gueltig: Auf echter
  Hardware ohne PS/2 ist SpeedOS nach wie vor NICHT BEDIENBAR. Der
  Controller zu starten ist die Voraussetzung dafuer, nicht die
  Loesung.
* **Keine Interrupts** — der Event Ring wird gepollt (100 ms).
  Ausreichend fuer Steckvorgaenge, zu langsam fuer eine Tastatur.
* **Kein Hub-Support**, keine isochronen Uebertragungen, keine
  USB-3-Streams.
* **BIOS-Handoff und Scratchpad sind ungetestet.** Beides ist gebaut
  und protokolliert, aber QEMU verlangt weder das eine (keine
  Extended Capabilities) noch das andere (0 Puffer). Sie werden sich
  erst auf echter Hardware beweisen — und das ist genau die Sorte
  Code, die man dort am schlechtesten debuggt.
* **Nur EIN Controller** wird benutzt (der erste gefundene).

TESTAUFBAU-EIGENHEIT, gemessen: `usb-kbd` in QEMU STIEHLT die
PS/2-Tastatur (QEMU leitet an die zuletzt angemeldete Tastatur). Die
USB-Eingabegeraete haengen deshalb nur mit `SPEEDOS_USB_GERAETE=1`
dran; sonst waere die Maschine nicht mehr bedienbar.
