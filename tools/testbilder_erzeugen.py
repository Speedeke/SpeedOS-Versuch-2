#!/usr/bin/env python3
# tools/testbilder_erzeugen.py — Testbilder fuer den SpeedOS-Bilddekoder
#
# ===========================================================================
# WARUM DIESES SKRIPT UND KEINE FERTIGEN PNG-DATEIEN IM REPOSITORY
#
# Die interessanten Testbilder sind die KAPUTTEN, und die kann man nicht
# herunterladen: Ein abgeschnittenes PNG, eines mit falscher Pruefsumme und
# vor allem eine Dekompressionsbombe (ein paar hundert Byte, die zu
# Gigabytes aufblasen) entstehen nur, wenn man sie ABSICHTLICH baut. Wer sie
# als Binaerdateien eincheckt, hat Testdaten, die niemand nachvollziehen
# kann; wer sie erzeugt, hat eine LISTE VON ANGRIFFEN, die man lesen kann.
#
# Dasselbe Argument wie bei `userland/angreifer`: Eine Sicherheitszusage,
# die nicht geprueft wird, ist eine Behauptung.
#
# ===========================================================================
# WIE EIN PNG AUFGEBAUT IST (das Noetigste, damit die Angriffe lesbar sind)
#
# 8 Byte Signatur, danach eine Folge von CHUNKS:
#
#     [Laenge u32-BE][Typ 4 ASCII][Daten ...][CRC32 u32-BE]
#
# Die CRC laeuft ueber Typ UND Daten, nicht ueber die Laenge. Pflicht sind
# IHDR (Breite, Hoehe, Bittiefe, Farbtyp, ...), mindestens ein IDAT (die
# zlib-komprimierten Bilddaten) und IEND. Vor der Kompression bekommt JEDE
# Bildzeile ein FILTER-Byte vorangestellt (0..4) — deshalb ist der
# unkomprimierte Strom `hoehe * (1 + breite * kanaele)` Byte gross.
#
# Aufruf:  python tools/testbilder_erzeugen.py [zielordner]
# Standard-Zielordner: assets/testbilder/
# ===========================================================================

import os
import struct
import sys
import zlib

# --- PNG-Bausteine ---------------------------------------------------------

SIGNATUR = b"\x89PNG\r\n\x1a\n"

# Farbtypen laut PNG-Spezifikation
GRAU = 0
RGB = 2
PALETTE = 3
GRAU_ALPHA = 4
RGBA = 6

KANAELE = {GRAU: 1, RGB: 3, PALETTE: 1, GRAU_ALPHA: 2, RGBA: 4}


def chunk(typ: bytes, daten: bytes) -> bytes:
    """Ein PNG-Chunk mit korrekter Laenge und CRC."""
    return (
        struct.pack(">I", len(daten))
        + typ
        + daten
        + struct.pack(">I", zlib.crc32(typ + daten) & 0xFFFFFFFF)
    )


def ihdr(breite: int, hoehe: int, bittiefe: int = 8, farbtyp: int = RGB) -> bytes:
    """Der Kopf-Chunk. Kompression 0, Filter 0, Interlace 0 — mehr gibt es nicht."""
    return chunk(
        b"IHDR",
        struct.pack(">IIBBBBB", breite, hoehe, bittiefe, farbtyp, 0, 0, 0),
    )


def roh_zu_idat(zeilen: list, breite: int, farbtyp: int, stufe: int = 6) -> bytes:
    """Bildzeilen (je eine bytes-Folge) filtern (immer 0 = None) und zippen.

    `stufe` ist nur fuer die Bombe interessant: Sie soll KLEIN sein, das ist
    ihr ganzer Witz."""
    strom = b"".join(b"\x00" + z for z in zeilen)
    return chunk(b"IDAT", zlib.compress(strom, stufe))


def png_bauen(breite: int, hoehe: int, farbtyp: int, zeilen: list,
              extra: bytes = b"") -> bytes:
    return (
        SIGNATUR
        + ihdr(breite, hoehe, 8, farbtyp)
        + extra
        + roh_zu_idat(zeilen, breite, farbtyp)
        + chunk(b"IEND", b"")
    )


# --- Die GUTEN Bilder ------------------------------------------------------


def bild_verlauf(breite: int, hoehe: int) -> bytes:
    """RGB-Verlauf: rot waechst nach rechts, gruen nach unten, blau fest.

    Berechenbar, und genau darum geht es: Der Test prueft EINZELNE PIXEL
    gegen die Formel, nicht nur „hat irgendwas dekodiert"."""
    zeilen = []
    for y in range(hoehe):
        z = bytearray()
        for x in range(breite):
            z += bytes(
                (
                    (x * 255) // max(1, breite - 1),
                    (y * 255) // max(1, hoehe - 1),
                    0x40,
                )
            )
        zeilen.append(bytes(z))
    return png_bauen(breite, hoehe, RGB, zeilen)


def bild_rgba(breite: int, hoehe: int) -> bytes:
    """RGBA mit einem durchsichtigen Viertel — prueft den Alpha-Kanal."""
    zeilen = []
    for y in range(hoehe):
        z = bytearray()
        for x in range(breite):
            durchsichtig = x < breite // 2 and y < hoehe // 2
            z += bytes((0xE0, 0x30, 0x90, 0x00 if durchsichtig else 0xFF))
        zeilen.append(bytes(z))
    return png_bauen(breite, hoehe, RGBA, zeilen)


def bild_grau(breite: int, hoehe: int) -> bytes:
    """8-Bit-Graustufen — der Farbtyp, den ein Dekoder am leichtesten vergisst."""
    zeilen = []
    for y in range(hoehe):
        zeilen.append(bytes(((x * 8 + y * 4) & 0xFF) for x in range(breite)))
    return png_bauen(breite, hoehe, GRAU, zeilen)


def bild_palette(breite: int, hoehe: int) -> bytes:
    """Indiziert mit 4 Farben (PLTE-Chunk). Der Weg, den fast alle Icons nehmen."""
    palette = bytes((0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00,
                     0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x00))
    zeilen = [bytes((x + y) % 4 for x in range(breite)) for y in range(hoehe)]
    return png_bauen(breite, hoehe, PALETTE, zeilen, extra=chunk(b"PLTE", palette))


# --- Die BOESARTIGEN Bilder ------------------------------------------------
#
# Jedes davon steht fuer eine konkrete Frage an den Dekoder. Die Antwort
# muss IMMER dieselbe sein: ein Fehler, kein Absturz, keine Allokation
# jenseits der Grenzen.


def kaputt_abgeschnitten() -> bytes:
    """Mitten im IDAT abgeschnitten — der haeufigste echte Schaden
    (abgebrochener Download, volle Platte)."""
    voll = bild_verlauf(64, 64)
    return voll[: len(voll) // 2]


def kaputt_ohne_iend() -> bytes:
    """Alles da, nur der Schluss-Chunk fehlt. Ein Dekoder, der die Bilddaten
    schon hat, DARF das Bild liefern — er darf nur nicht haengen."""
    voll = bild_verlauf(32, 32)
    return voll[: -len(chunk(b"IEND", b""))]


def kaputt_crc() -> bytes:
    """Ein Byte in den Bilddaten gekippt, CRC stimmt nicht mehr."""
    voll = bytearray(bild_verlauf(32, 32))
    voll[-40] ^= 0xFF
    return bytes(voll)


def kaputt_signatur() -> bytes:
    """Kein PNG, sieht aber auf den ersten Blick so aus."""
    return b"\x89PNG\r\n\x1a\x00" + bild_verlauf(8, 8)[8:]


def angriff_absurde_masse() -> bytes:
    """IHDR behauptet 100000 x 100000 (= 10 Milliarden Pixel, 40 GB als RGBA),
    die Bilddaten sind ein paar Byte.

    DAS IST DER WICHTIGSTE TESTFALL: Ein Dekoder, der dem IHDR glaubt und
    vorab alloziert, ist mit EINER 200-Byte-Datei zu toeten."""
    zeilen = [bytes(3 * 4) for _ in range(4)]
    return (
        SIGNATUR
        + ihdr(100000, 100000, 8, RGB)
        + roh_zu_idat(zeilen, 4, RGB)
        + chunk(b"IEND", b"")
    )


def angriff_null_masse() -> bytes:
    """Breite 0, Hoehe 0 — formal ungueltig, beliebter Division-durch-Null-Fund."""
    return (
        SIGNATUR
        + ihdr(0, 0, 8, RGB)
        + roh_zu_idat([], 0, RGB)
        + chunk(b"IEND", b"")
    )


def angriff_bombe() -> bytes:
    """Die DEKOMPRESSIONSBOMBE: 4096 x 4096 deklariert, und das IDAT ist ein
    zlib-Strom aus lauter Nullen — ein paar Kilobyte Datei, 50 MiB
    unkomprimiert.

    Anders als `absurde_masse` ist das hier ein FORMAL GUELTIGES Bild. Es
    scheitert nicht an der Plausibilitaet, sondern muss an einer GRENZE
    scheitern — und genau deshalb braucht der Dekoder eine."""
    breite, hoehe = 4096, 4096
    zeile = bytes(breite * 3)
    return (
        SIGNATUR
        + ihdr(breite, hoehe, 8, RGB)
        + roh_zu_idat([zeile] * hoehe, breite, RGB, stufe=9)
        + chunk(b"IEND", b"")
    )


def angriff_riesige_chunk_laenge() -> bytes:
    """Ein Chunk behauptet 0xFFFFFFFF Byte Laenge. Wer daraus eine
    Puffergroesse macht, hat verloren."""
    kopf = SIGNATUR + ihdr(8, 8, 8, RGB)
    boese = struct.pack(">I", 0xFFFFFFFF) + b"IDAT" + b"\x00" * 8
    return kopf + boese + chunk(b"IEND", b"")


def angriff_viele_chunks() -> bytes:
    """4000 leere Chunks vor den Bilddaten — der Versuch, den Dekoder in
    einer Schleife oder im Speicher zu verlieren."""
    fuellung = b"".join(chunk(b"teXt", b"x") for _ in range(4000))
    voll = bild_verlauf(8, 8)
    # Fuellung zwischen IHDR (8+25 Byte) und den Rest schieben.
    schnitt = 8 + 25
    return voll[:schnitt] + fuellung + voll[schnitt:]


def angriff_leer() -> bytes:
    """Null Byte. Der Fall, den man beim Testen vergisst."""
    return b""


def angriff_nur_signatur() -> bytes:
    """Signatur, sonst nichts."""
    return SIGNATUR


def angriff_kein_bild() -> bytes:
    """Etwas voellig anderes (hier: eine Textdatei)."""
    return b"Das ist kein Bild, sondern eine Textdatei.\n" * 8


# --- Zusammenstellung ------------------------------------------------------

DATEIEN = [
    # (Name, Erzeuger, erwartetes Verhalten — steht so auch im Test)
    ("verlauf.png", lambda: bild_verlauf(64, 48), "ok"),
    ("gross.png", lambda: bild_verlauf(160, 120), "ok"),
    ("rgba.png", lambda: bild_rgba(32, 32), "ok"),
    ("grau.png", lambda: bild_grau(40, 24), "ok"),
    ("palette.png", lambda: bild_palette(24, 16), "ok"),
    ("abgeschnitten.png", kaputt_abgeschnitten, "fehler"),
    ("ohne_iend.png", kaputt_ohne_iend, "egal"),
    ("crc_kaputt.png", kaputt_crc, "egal"),
    ("falsche_signatur.png", kaputt_signatur, "fehler"),
    ("absurde_masse.png", angriff_absurde_masse, "fehler"),
    ("null_masse.png", angriff_null_masse, "fehler"),
    ("bombe.png", angriff_bombe, "fehler"),
    ("riesige_chunk_laenge.png", angriff_riesige_chunk_laenge, "fehler"),
    ("viele_chunks.png", angriff_viele_chunks, "egal"),
    ("leer.png", angriff_leer, "fehler"),
    ("nur_signatur.png", angriff_nur_signatur, "fehler"),
    ("kein_bild.png", angriff_kein_bild, "fehler"),
]


def main() -> int:
    ziel = sys.argv[1] if len(sys.argv) > 1 else os.path.join("assets", "testbilder")
    os.makedirs(ziel, exist_ok=True)

    print(f"Testbilder nach {ziel}/")
    gesamt = 0
    for name, erzeuger, erwartung in DATEIEN:
        daten = erzeuger()
        pfad = os.path.join(ziel, name)
        with open(pfad, "wb") as f:
            f.write(daten)
        gesamt += len(daten)
        print(f"  {name:26s} {len(daten):8d} Byte   (erwartet: {erwartung})")

    print(f"\n{len(DATEIEN)} Dateien, {gesamt} Byte insgesamt.")
    print("Die 'erwartet'-Spalte ist die Vorgabe fuer tests/bilder.rs:")
    print("  ok      = muss dekodieren, Pixel werden gegen die Formel geprueft")
    print("  fehler  = MUSS abgelehnt werden (Fehler, keine Panik, kein Haenger)")
    print("  egal    = darf beides, solange es weder abstuerzt noch haengt")
    return 0


if __name__ == "__main__":
    sys.exit(main())
