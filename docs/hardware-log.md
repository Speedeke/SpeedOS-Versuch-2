# SpeedOS — Hardware-Log (Boot-Tests auf echten Geräten)

Hier wird festgehalten, auf welcher echten Hardware SpeedOS als
Live-System (siehe [`usb-boot.md`](usb-boot.md)) getestet wurde und was
funktioniert hat. **Ehrlich dokumentieren** — ein toter Mauszeiger ist
ein erwartetes Ergebnis, kein Scheitern (USB-Legacy-Emulation ist
Glückssache; echte USB-Eingabe kommt erst später).

Getestetes Image: `speedos-live.img` (`cargo image`).

---

## Übersichts-Tabelle

| Datum | Gerät | Bootet? | Desktop? | Tastatur | Maus | Auflösung | Notiz |
|-------|-------|:-------:|:--------:|:--------:|:----:|-----------|-------|
| 2026-07-22 | Acer Aspire A515-51 (N17C4) | ✅ | ✅ | ✅ | ⚠️ | 1920×1080 | Touchpad als PS/2 erkannt, bewegt aber den Cursor nicht |

Legende: ✅ ja · ❌ nein · ⚠️ teilweise · ⬜ noch offen

---

## Vorlage pro Gerät (kopieren und ausfüllen)

> Diesen Block für jedes getestete Gerät duplizieren.

### Gerät: _Marke + Modell_

- **Datum:** _JJJJ-MM-TT_
- **Typ:** _Laptop / Desktop / Mini-PC / …_
- **Firmware:** _UEFI-Hersteller/Version, Secure Boot aus? CSM aus?_
- **Bildschirm-Auflösung (laut Diagnose):** _z. B. 1920×1080_

| Prüfpunkt | Ergebnis | Bemerkung |
|---|:---:|---|
| Firmware bootet vom Stick (UEFI) | ⬜ | |
| Aurora-Bootscreen erscheint | ⬜ | |
| Desktop startet | ⬜ | |
| PS/2-/Legacy-**Tastatur** funktioniert | ⬜ | |
| **Maus** funktioniert | ⬜ | _USB-Legacy? oft ❌_ |
| Automatische HiDPI-Skalierung passt | ⬜ | _bei hoher Auflösung_ |
| „Keine PS/2-Eingabe"-Meldung (falls zutreffend) | ⬜ | |
| Diagnose-Modus (Taste D) zeigt Hardware | ⬜ | |

**Was ging:**
_… _

**Was nicht ging:**
_… _

**Diagnose-Ausgabe (Taste D) — was wurde erkannt?**
_Bildschirm / Tastatur / Maus / Laufwerke laut Diagnose-Schirm:_
_… _

**Fotos:**
_Bildschirmfotos vom Gerät hier verlinken (in `docs/screenshots/` ablegen):_

<!-- Beispiel:
![Boot auf Laptop XY](screenshots/hw-laptopxy-desktop.jpg)
![Diagnose auf Laptop XY](screenshots/hw-laptopxy-diagnose.jpg)
-->

---

## Bisherige Ergebnisse

### Gerät: Acer Aspire A515-51 (Modell N17C4)

- **Datum:** 2026-07-22 — **erster erfolgreicher Boot auf echter Hardware** 🎉
- **Typ:** Laptop (Baujahr 2018-06)
- **Firmware:** UEFI (Acer InsydeH2O). Secure Boot AUS — dazu erst ein
  Supervisor-Passwort gesetzt (Acer-Eigenheit); F12-Boot-Menü aktiviert.
- **Bildschirm-Auflösung (laut Diagnose):** 1920×1080 (natives Panel,
  korrekt erkannt; Skala 1.0, keine HiDPI-Skalierung nötig)

| Prüfpunkt | Ergebnis | Bemerkung |
|---|:---:|---|
| Firmware bootet vom Stick (UEFI) | ✅ | |
| Aurora-Bootscreen erscheint | ✅ | |
| Desktop startet | ✅ | |
| PS/2-/Legacy-**Tastatur** funktioniert | ✅ | als PS/2 erkannt, tippt normal |
| **Maus** funktioniert | ⚠️ | als PS/2 **erkannt**, aber **kein Cursor-Movement** |
| Automatische HiDPI-Skalierung passt | n/a | 1080p = Skala 1.0 |
| „Keine PS/2-Eingabe"-Meldung | n/a | Eingabe war ja vorhanden |
| Diagnose-Modus (Taste D) zeigt Hardware | ✅ | Ausgabe unten |

**Was ging:** Boot vom Stick, Aurora-Bootscreen, Desktop, **Tastatur**,
korrekte native Auflösung **1920×1080**, Diagnose-Modus (Taste D).

**Was nicht ging:** Das **Touchpad** bewegt den Cursor nicht — obwohl es
als PS/2-Maus erkannt wird (die Init-Handshakes werden beantwortet).

**Diagnose-Ausgabe (Taste D):**
```
Bildschirm : 1920x1080 Pixel, Bgr, 4 B/Pixel
Tastatur   : PS/2 erkannt
Maus       : PS/2 erkannt
ATA        : keine Laufwerke
Dateisystem: nur RAM (keine Platte gemountet)
```

**Lead für später (Touchpad-Init):** Das Acer-Touchpad (Synaptics/ELAN)
hängt am 8042 und ACKt unsere Standard-PS/2-Maus-Init (0xF6 / IntelliMouse-
Sequenz / 0xF4), streamt danach aber KEINE Bewegungspakete. Kein
„USB-Glücksspiel", sondern ein konkreter Punkt: vermutlich braucht das
Touchpad eine andere Aktivierung, oder die IntelliMouse-Ratensequenz
bringt es aus dem Standard-Stream. Zu prüfen, wenn PS/2-Touchpads dran
sind (schwer aus der Ferne — braucht Test-Iterationen auf genau diesem
Gerät). ATA „keine Laufwerke" ist erwartbar: der interne Speicher hängt
an NVMe/AHCI, nicht am Legacy-IDE-Port, den unser ATA-Treiber abfragt.

_Foto vom Diagnose-Schirm liegt vor; als `docs/screenshots/hw-acer-a515-diagnose.jpg` ablegen (optional)._

---

## OFFEN: Die RTC-Zone auf echter Hardware (Serie 7, Teil 2)

**Status: NICHT VERIFIZIERT.** Diese Prüfung braucht einen Menschen vor dem
Gerät und ist von der Entwicklungsmaschine aus nicht durchführbar — sie steht
hier als Auftrag, nicht als Ergebnis.

### Warum es geprüft werden muss

Seit Serie 7, Teil 2 liefert `zeit::jetzt()` **UTC**, und
Zertifikats-Gültigkeitszeiträume hängen daran. Was die CMOS-Uhr einer
konkreten Maschine liefert, ist aber **nicht festgelegt**:

* Ein Windows-PC führt die RTC üblicherweise in **Lokalzeit**.
* Ein Linux-System führt sie in **UTC**.
* Der Acer A515-51 kam mit Windows — die Erwartung ist also **Lokalzeit**,
  in Deutschland also UTC+1 bzw. UTC+2 (Sommerzeit).

SpeedOS' Voreinstellung ist `zeit.rtc_zone_min = 0`, also „die RTC läuft in
UTC" (das ist auch das, was unser QEMU-Runner mit `-rtc base=utc`
herstellt). Trifft das auf einer Maschine nicht zu, geht die Uhr um den
Zonenversatz falsch — bei Zertifikaten mit Monats-Laufzeiten harmlos, aber
es ist eine Unwahrheit im Kern, und sie gehört gemessen statt vermutet.

### Wie es geprüft wird

1. Live-Stick bauen und schreiben (`cargo image`, `tools/usb_schreiben.ps1`).
2. Beim Bootscreen **D** drücken (Diagnose-Modus).
3. Im Abschnitt „Erkannte Hardware" stehen jetzt drei neue Zeilen:
   ```
   RTC roh    : TT.MM.JJJJ HH:MM:SS  (RTC-Zone +0 min)
   UTC        : TT.MM.JJJJ HH:MM:SS  (plausibel)
   Kernel-Bau : TT.MM.JJJJ
   ```
4. **„RTC roh" mit der Uhr an der Wand vergleichen:**
   * Stimmt sie mit der **Ortszeit** überein → die RTC läuft in Lokalzeit.
     Dann in *Einstellungen → Zeit* die **Zone der HARDWARE-Uhr** auf den
     eigenen Versatz setzen (Sommer: UTC+02:00). Danach muss „UTC" die
     Weltzeit zeigen.
   * Stimmt sie mit der **UTC** überein → alles richtig, nichts zu tun.
5. Das Ergebnis hier eintragen (Datum, Gerät, welcher Fall).

### Was dabei ausserdem zu sehen ist

* `Kernel-Bau` — die Grenze der Plausibilitätsprüfung. Zeigt „UTC" ein Datum
  **davor**, meldet SpeedOS `UNPLAUSIBEL!` und verweigert die
  Zertifikatsprüfung. Genau das ist auf einem Gerät mit leerer
  Pufferbatterie zu erwarten und der eigentliche Zweck der Prüfung.
* `CA-Buendel` — ob ein Vertrauensanker im Image steckt.

### Ergebnis

| Datum | Gerät | RTC läuft in | RTC-Zone gesetzt auf | Bemerkung |
|---|---|---|---|---|
| — | Acer Aspire A515-51 | *offen* | — | zu prüfen |

---

> Als Referenz, wie ein voller Erfolg in der Emulation aussieht, dienen
> die QEMU-Screendumps der Generalprobe:
> [Desktop](screenshots/live-desktop.png) ·
> [Diagnose](screenshots/live-diagnose.png) ·
> [keine PS/2-Eingabe](screenshots/live-keine-ps2.png).
