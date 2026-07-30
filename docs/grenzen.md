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

* Die **Widget-Schicht** lebt im Kernel. Ein User-Space-Prozess kann seit
  Serie 8, Teil 1 zwar ein **Fenster besitzen** und Pixel hineinmalen
  (`docs/fenster-syscalls.md`), aber er bekommt keine Knöpfe, keine Listen
  und keine Textfelder — die gibt es nur kernel-seitig. Ob das Toolkit in
  den User-Space wandert, ist DIE offene Architekturfrage für Serie 8
  (`docs/serie8-bestandsaufnahme.md`).
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
* Schriftgrößen sind **vorgerastert** (16/24/32) — es gibt keinen
  Rasterizer, also keine beliebigen Größen.
* Keine Bild-Dekodierung (PNG/JPEG).

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
| unsafe-Flächen | `docs/unsafe-audit-serie6.md`, `docs/unsafe-audit-serie7.md` |
| Echte Hardware | `docs/hardware-log.md`, `docs/usb-boot.md` |
