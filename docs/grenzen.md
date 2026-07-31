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
* **Ein Fenster über den ganzen 4K-Schirm passt nicht in den User-Heap.**
  Der Pixelpuffer eines Programms liegt auf dessen eigenem Heap, und der ist
  auf 12 MiB gedeckelt (`prozess::HEAP_MAX_BYTES`, in der 16-MiB-Lücke
  zwischen Programm-Image und Stack). 3840 × 2088 × 4 Byte sind **32,1 MiB**
  — fast das Dreifache. Ein Browser bei 4K braucht deshalb entweder ein
  grösseres Prozess-Layout (eine ABI-Änderung) oder er hält seinen Inhalt in
  Streifen. Gemessen und ausgerechnet in `docs/fenster-syscalls.md` §6.
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
| Browser-V1: Zuschnitt, Zielmarke, Reißleine | `docs/browser-v1.md` |
| unsafe-Flächen | `docs/unsafe-audit-serie6.md`, `docs/unsafe-audit-serie7.md` |
| Echte Hardware | `docs/hardware-log.md`, `docs/usb-boot.md` |
