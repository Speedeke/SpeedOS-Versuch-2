#!/usr/bin/env python3
# tools/fat32_image_erzeugen.py — baut das FAT32-Beispiel-Image
#
# Der boot/-Runner ruft dieses Skript auf, wenn speedos-fat.img fehlt.
# Es baut ein spec-konformes FAT32-Image (>= 65525 Cluster!) mit den
# Beispieldateien, die tests/fat_platte.rs Byte fuer Byte gegenprueft
# (INHALTE HIER UND DORT SYNCHRON HALTEN!).
#
# Zwei Wege:
#   1. HOST-WERKZEUG (bevorzugt): mformat/mcopy/mmd aus mtools —
#      ein fremdes, etabliertes Werkzeug erzeugt das Dateisystem;
#      unser Treiber liest also NICHT nur sein eigenes Geschreibsel.
#   2. FALLBACK ohne mtools: ein eigener Mini-FAT32-Writer weiter
#      unten in diesem Skript (dieselben Dateien und Inhalte).
#
# Aufruf: python tools/fat32_image_erzeugen.py <ziel.img> [--ohne-mtools]

import os
import shutil
import struct
import subprocess
import sys
import tempfile

# ---- Geometrie (fest, damit beide Wege dasselbe Layout erzeugen) ----
BPS = 512          # Bytes pro Sektor
SPC = 1            # Sektoren pro Cluster
RESERVIERT = 32    # reservierte Sektoren (Bootsektor, FSInfo, Backup)
NFATS = 2
CLUSTER = 66000    # > 65525 -> laut Microsoft-Algorithmus echtes FAT32
FATSZ = (CLUSTER + 2) * 4 // BPS + 1          # 516 Sektoren je FAT
TOTAL = RESERVIERT + NFATS * FATSZ + CLUSTER  # 67064 Sektoren (~32,7 MiB)

# ---- Die Beispieldateien (SYNCHRON mit tests/fat_platte.rs!) ----
GROSS_LAENGE = 100_000

def gross_bin() -> bytes:
    # Dasselbe Primzahl-Muster wie in den Kernel-Tests:
    return bytes(i % 251 for i in range(GROSS_LAENGE))

DATEIEN = {
    "hallo.txt":
        "Hallo von FAT32!\nDiese Datei liest SpeedOS von einem fremden Dateisystem.\n",
    "Grüße und Umlaute äöüß.txt":
        "Grüße vom FAT-Laufwerk!\nUmlaute funktionieren: Ä Ö Ü ä ö ü ß\n",
    "Dokumente/Übergabe-Protokoll.txt":
        "Protokoll der Übergabe:\nDateien vom USB-Stick nach SpeedOS holen.\n",
    "Dokumente/ein-sehr-langer-dateiname-der-mehrere-lfn-eintraege-braucht.txt":
        "Langer Name, kurzer Inhalt.\n",
}

# ---------------------------------------------------------------------------
# Weg 1: mtools (mformat + mmd + mcopy)
# ---------------------------------------------------------------------------

def mit_mtools(ziel: str) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["mformat", "-i", ziel, "-F", "-C", "-T", str(TOTAL),
             "-h", "16", "-s", "63", "-v", "SPEEDOSFAT", "::"],
            check=True,
        )
        subprocess.run(["mmd", "-i", ziel, "::Dokumente"], check=True)
        for name, inhalt in DATEIEN.items():
            quelle = os.path.join(tmp, "quelle.bin")
            with open(quelle, "wb") as f:
                f.write(inhalt.encode("utf-8"))
            subprocess.run(["mcopy", "-i", ziel, quelle, f"::{name}"], check=True)
        quelle = os.path.join(tmp, "gross.bin")
        with open(quelle, "wb") as f:
            f.write(gross_bin())
        subprocess.run(["mcopy", "-i", ziel, quelle, "::gross.bin"], check=True)


# ---------------------------------------------------------------------------
# Weg 2: eigener Mini-FAT32-Writer (nur was wir brauchen)
# ---------------------------------------------------------------------------

def kurzname(alias: str) -> bytes:
    """'GROSS.BIN' -> 11 Bytes '8.3'-Feld."""
    if "." in alias:
        basis, endung = alias.rsplit(".", 1)
    else:
        basis, endung = alias, ""
    return (basis.ljust(8)[:8] + endung.ljust(3)[:3]).encode("ascii")

def pruefsumme(kurz: bytes) -> int:
    s = 0
    for b in kurz:
        s = (((s & 1) << 7) + (s >> 1) + b) & 0xFF
    return s

# Fester Zeitstempel: 20.07.2026, 12:00:00 (FAT zaehlt Jahre ab 1980).
FAT_DATUM = ((2026 - 1980) << 9) | (7 << 5) | 20
FAT_ZEIT = (12 << 11) | (0 << 5) | 0

def kurz_eintrag(kurz: bytes, attr: int, cluster: int, groesse: int) -> bytes:
    e = bytearray(32)
    e[0:11] = kurz
    e[11] = attr
    struct.pack_into("<H", e, 14, FAT_ZEIT)   # erstellt
    struct.pack_into("<H", e, 16, FAT_DATUM)
    struct.pack_into("<H", e, 18, FAT_DATUM)  # letzter Zugriff
    struct.pack_into("<H", e, 20, cluster >> 16)
    struct.pack_into("<H", e, 22, FAT_ZEIT)   # geaendert
    struct.pack_into("<H", e, 24, FAT_DATUM)
    struct.pack_into("<H", e, 26, cluster & 0xFFFF)
    struct.pack_into("<I", e, 28, groesse)
    return bytes(e)

def lfn_eintraege(name: str, kurz: bytes) -> bytes:
    """Die VFAT-Langnamen-Einträge (rückwärts nummeriert, UTF-16-LE)."""
    utf16 = name.encode("utf-16-le") + b"\x00\x00"
    while len(utf16) % 26 != 0:
        utf16 += b"\xff\xff"
    stuecke = [utf16[i:i + 26] for i in range(0, len(utf16), 26)]
    ck = pruefsumme(kurz)
    eintraege = b""
    for nr in range(len(stuecke), 0, -1):  # letztes Stück zuerst
        e = bytearray(32)
        e[0] = nr | (0x40 if nr == len(stuecke) else 0)
        s = stuecke[nr - 1]
        e[1:11] = s[0:10]
        e[11] = 0x0F  # das LFN-Attribut
        e[13] = ck
        e[14:26] = s[10:22]
        e[28:32] = s[22:26]
        eintraege += bytes(e)
    return eintraege

def eigener_writer(ziel: str) -> None:
    fat = [0] * (CLUSTER + 2)
    fat[0], fat[1] = 0x0FFFFFF8, 0x0FFFFFFF
    daten = {}          # cluster -> bytes (ein Cluster)
    naechster = [3]     # Cluster 2 = Wurzelverzeichnis

    def kette_schreiben(inhalt: bytes) -> int:
        """Legt den Inhalt in eine Cluster-Kette, liefert den Start."""
        start, vorher = 0, 0
        for i in range(0, max(len(inhalt), 1), BPS * SPC):
            c = naechster[0]
            naechster[0] += 1
            daten[c] = inhalt[i:i + BPS * SPC]
            if vorher:
                fat[vorher] = c
            else:
                start = c
            vorher = c
        fat[vorher] = 0x0FFFFFFF
        return start

    # Dateien und das Unterverzeichnis einsammeln:
    root = kurz_eintrag(b"SPEEDOSFAT ", 0x08, 0, 0)  # Volume-Label
    dokumente = b""
    aliase = {"hallo.txt": "HALLO.TXT",
              "Grüße und Umlaute äöüß.txt": "GRUESS~1.TXT",
              "Dokumente/Übergabe-Protokoll.txt": "UEBERG~1.TXT",
              "Dokumente/ein-sehr-langer-dateiname-der-mehrere-lfn-eintraege-braucht.txt":
                  "EINSEH~1.TXT",
              "gross.bin": "GROSS.BIN"}

    def datei_eintrag(pfadname: str, inhalt: bytes) -> bytes:
        name = pfadname.rsplit("/", 1)[-1]
        kurz = kurzname(aliase[pfadname])
        start = kette_schreiben(inhalt) if inhalt else 0
        return lfn_eintraege(name, kurz) + kurz_eintrag(kurz, 0x20, start, len(inhalt))

    for pfadname, text in DATEIEN.items():
        eintrag = datei_eintrag(pfadname, text.encode("utf-8"))
        if pfadname.startswith("Dokumente/"):
            dokumente += eintrag
        else:
            root += eintrag
    root += datei_eintrag("gross.bin", gross_bin())

    # Unterverzeichnis (mit . und ..):
    dok_kurz = kurzname("DOKUME~1")
    dok_inhalt = kurz_eintrag(b".          ", 0x10, 0, 0)  # Cluster traegt der Writer nach
    dok_inhalt += kurz_eintrag(b"..         ", 0x10, 0, 0)
    dok_inhalt += dokumente
    dok_start = kette_schreiben(dok_inhalt)
    # "." muss auf sich selbst zeigen — nachtraeglich einsetzen:
    roh = bytearray(daten[dok_start])
    struct.pack_into("<H", roh, 20, dok_start >> 16)
    struct.pack_into("<H", roh, 26, dok_start & 0xFFFF)
    daten[dok_start] = bytes(roh)
    root += lfn_eintraege("Dokumente", dok_kurz) + kurz_eintrag(dok_kurz, 0x10, dok_start, 0)

    # Wurzelverzeichnis in Cluster 2 (darf mehrere Cluster brauchen):
    fat[2] = 0x0FFFFFFF
    if len(root) <= BPS * SPC:
        daten[2] = root
    else:
        daten[2] = root[:BPS * SPC]
        rest_start = kette_schreiben(root[BPS * SPC:])
        fat[2] = rest_start

    # ---- Alles in die Image-Datei giessen ----
    boot = bytearray(BPS)
    boot[0:3] = b"\xeb\x58\x90"
    boot[3:11] = b"SPEEDFAT"
    struct.pack_into("<H", boot, 11, BPS)
    boot[13] = SPC
    struct.pack_into("<H", boot, 14, RESERVIERT)
    boot[16] = NFATS
    boot[21] = 0xF8
    struct.pack_into("<H", boot, 24, 63)
    struct.pack_into("<H", boot, 26, 16)
    struct.pack_into("<I", boot, 32, TOTAL)
    struct.pack_into("<I", boot, 36, FATSZ)
    struct.pack_into("<I", boot, 44, 2)      # Wurzel-Cluster
    struct.pack_into("<H", boot, 48, 1)      # FSInfo
    struct.pack_into("<H", boot, 50, 6)      # Backup-Bootsektor
    boot[64] = 0x80
    boot[66] = 0x29
    struct.pack_into("<I", boot, 67, 0x5EED05)
    boot[71:82] = b"SPEEDOSFAT "
    boot[82:90] = b"FAT32   "
    boot[510:512] = b"\x55\xaa"

    fsinfo = bytearray(BPS)
    struct.pack_into("<I", fsinfo, 0, 0x41615252)
    struct.pack_into("<I", fsinfo, 484, 0x61417272)
    struct.pack_into("<I", fsinfo, 488, CLUSTER - (naechster[0] - 2))
    struct.pack_into("<I", fsinfo, 492, naechster[0])
    fsinfo[510:512] = b"\x55\xaa"

    fat_roh = b"".join(struct.pack("<I", e) for e in fat)
    fat_roh += b"\x00" * (FATSZ * BPS - len(fat_roh))

    with open(ziel, "wb") as f:
        f.write(boot)
        f.write(fsinfo)
        f.write(b"\x00" * (4 * BPS))
        f.write(boot)  # Backup-Bootsektor in Sektor 6
        f.write(b"\x00" * ((RESERVIERT - 7) * BPS))
        f.write(fat_roh)
        f.write(fat_roh)
        for c in range(2, naechster[0]):
            block = daten.get(c, b"")
            f.write(block + b"\x00" * (BPS * SPC - len(block)))
        f.truncate(TOTAL * BPS)


def main() -> int:
    if len(sys.argv) < 2:
        print("Aufruf: fat32_image_erzeugen.py <ziel.img> [--ohne-mtools]")
        return 1
    ziel = sys.argv[1]
    ohne_mtools = "--ohne-mtools" in sys.argv
    if not ohne_mtools and shutil.which("mformat") and shutil.which("mcopy") \
            and shutil.which("mmd"):
        mit_mtools(ziel)
        print(f"[fat32] {ziel} mit mtools (mformat/mcopy) erzeugt.")
    else:
        eigener_writer(ziel)
        print(f"[fat32] {ziel} mit dem eingebauten Python-Writer erzeugt (kein mtools).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
