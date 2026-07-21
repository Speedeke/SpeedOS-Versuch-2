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
| _JJJJ-MM-TT_ | _Marke Modell_ | ⬜ | ⬜ | ⬜ | ⬜ | _z. B. 1920×1080_ | _kurz_ |

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

> Noch keine Einträge — das erste echte Gerät kommt hier hin.
>
> Als Referenz, wie ein voller Erfolg aussieht, dienen die
> QEMU-Screendumps der Generalprobe:
> [Desktop](screenshots/live-desktop.png) ·
> [Diagnose](screenshots/live-diagnose.png) ·
> [keine PS/2-Eingabe](screenshots/live-keine-ps2.png).
