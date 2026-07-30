#!/usr/bin/env python3
"""qmp_steuern.py -- SpeedOS in QEMU fernsteuern: tippen, klicken, fotografieren.

WOZU
====
Ein Betriebssystem laesst sich nicht mit `cargo test` fotografieren. Fuer
Devlog-Bilder, fuer "sieht das Fenster wirklich so aus?" und fuer Fehler, die
sich nur mit der Maus ausloesen lassen (Fenster maximieren!), braucht es einen
Weg, dem laufenden Gast Tasten und Mausbewegungen zu schicken und den
Bildschirm abzugreifen.

Den Weg gibt es: QEMUs QMP-Schnittstelle. Der Runner oeffnet sie, wenn
SPEEDOS_QMP gesetzt ist:

    SPEEDOS_QMP=4444 cargo run

Danach:

    python tools/qmp_steuern.py 4444 foto bild.png
    python tools/qmp_steuern.py 4444 --esc tippen "hole https://example.com" foto b.png
    python tools/qmp_steuern.py 4444 zeiger 913 171 klick warte 2 foto b.png

Die Befehle werden der Reihe nach abgearbeitet, alle in EINER QMP-Sitzung.


DIE DREI FALLEN, die dieses Werkzeug ueberhaupt erst noetig machen
=================================================================

(1) `sendkey` GIBT ES IN QMP NICHT (mehr). In QEMU 11 ist es ein reiner
    HMP-Befehl. In QMP heisst er `input-send-event`, und Druecken und
    Loslassen muessen EINZELN geschickt werden.

(2) TASTATURLAYOUT: QEMU verschickt SCANCODES, also TASTENPOSITIONEN nach
    US-Layout. SpeedOS dekodiert aber DEUTSCHES QWERTZ. Wer 'z' schicken
    will, muss die US-'y'-Taste druecken -- und ':' ist Shift+'.',
    '/' ist Shift+'7', '-' liegt auf der US-'/'-Taste. Ohne diese Tabelle
    wird aus "type" ein "tzpe" und aus "https://" Buchstabensalat.

(3) ABSOLUTE MAUSPOSITIONEN GEHEN NICHT. SpeedOS hat eine PS/2-Maus, also
    ein RELATIVES Geraet; `abs`-Ereignisse quittiert QEMU mit "Input handler
    not found for event type abs". Gefahren wird deshalb relativ -- und in
    Schritten von hoechstens 100 Pixeln, weil ein PS/2-Paket nur +-255
    traegt und unser Treiber Pakete mit gesetztem Overflow-Bit
    spezifikationsgemaess VERWIRFT. `zeiger x y` faehrt den Cursor zuerst in
    die linke obere Ecke (viele grosse Schritte gegen den Anschlag) und von
    dort ans Ziel; die Ecke ist die einzige Position, die sich ohne
    Rueckmeldung sicher herstellen laesst.
"""
import json
import os
import socket
import sys
import time

# ---------------------------------------------------------------------------
# Tastatur: deutsche Zeichen -> US-Tastenpositionen (qcodes)
# ---------------------------------------------------------------------------

# Zeichen, die auf einer anderen Position liegen als ihr Name vermuten laesst.
SONDER = {
    " ": ["spc"],
    "\n": ["ret"],
    "\t": ["tab"],
    ":": ["shift", "dot"],     # QWERTZ: Doppelpunkt ist Shift+Punkt
    ";": ["shift", "comma"],
    ".": ["dot"],
    ",": ["comma"],
    "/": ["shift", "7"],       # QWERTZ: Schraegstrich ist Shift+7
    "-": ["slash"],            # QWERTZ-Bindestrich liegt auf der US-'/'-Taste
    "_": ["shift", "slash"],
    "=": ["shift", "0"],
    "?": ["shift", "minus"],
    "!": ["shift", "1"],
    '"': ["shift", "2"],
    "$": ["shift", "4"],
    "%": ["shift", "5"],
    "&": ["shift", "6"],
    "(": ["shift", "9"],
    ")": ["shift", "0"],
    "+": ["bracket_right"],
    "*": ["shift", "bracket_right"],
    "#": ["backslash"],
    "'": ["shift", "backslash"],
    "<": ["less"],
    ">": ["shift", "less"],
}

# Y und Z sind zwischen QWERTZ und QWERTY vertauscht -- und nur die beiden.
VERTAUSCHT = {"y": "z", "z": "y"}

# Namen, die man auf der Kommandozeile schreiben koennen soll.
TASTEN_ALIAS = {
    "esc": "esc",
    "enter": "ret",
    "return": "ret",
    "space": "spc",
    "tab": "tab",
    "bild-hoch": "pgup",
    "bild-runter": "pgdn",
    "pgup": "pgup",
    "pgdn": "pgdn",
    "hoch": "up",
    "runter": "down",
    "links": "left",
    "rechts": "right",
    "pos1": "home",
    "ende": "end",
    "entf": "delete",
    "rueck": "backspace",
}

# Ein PS/2-Paket traegt nur +-255; groessere Spruenge setzen das
# Overflow-Bit und werden vom Treiber verworfen. 100 ist bequem darunter.
MAUS_SCHRITT = 100
# So weit gegen den Anschlag, dass jeder Bildschirm sicher erreicht wird.
ANSCHLAG = 6000


class Qmp:
    """Eine offene QMP-Sitzung. Auch als Bibliothek benutzbar."""

    def __init__(self, port, host="127.0.0.1", timeout=30):
        self.s = socket.create_connection((host, port), timeout=timeout)
        self.f = self.s.makefile("rw", encoding="utf-8", newline="\n")
        self.f.readline()               # Begruessung
        self.cmd("qmp_capabilities")

    # --- Grundlage ---------------------------------------------------------

    def cmd(self, name, **args):
        """Schickt einen QMP-Befehl und liefert sein `return`."""
        nachricht = {"execute": name}
        if args:
            nachricht["arguments"] = args
        self.f.write(json.dumps(nachricht) + "\n")
        self.f.flush()
        while True:
            zeile = self.f.readline()
            if not zeile:
                raise RuntimeError("QMP-Verbindung zu")
            antwort = json.loads(zeile)
            if "event" in antwort:
                continue                # Events ueberspringen
            if "error" in antwort:
                raise RuntimeError("QMP-Fehler: %s" % antwort["error"])
            return antwort.get("return")

    # --- Tastatur ----------------------------------------------------------

    def taste(self, qcodes, pause=0.09):
        """Drueckt die Tasten gemeinsam und laesst sie rueckwaerts wieder los."""
        ereignisse = []
        for q in qcodes:
            ereignisse.append({"type": "key",
                               "data": {"down": True,
                                        "key": {"type": "qcode", "data": q}}})
        for q in reversed(qcodes):
            ereignisse.append({"type": "key",
                               "data": {"down": False,
                                        "key": {"type": "qcode", "data": q}}})
        self.cmd("input-send-event", events=ereignisse)
        time.sleep(pause)

    def tippen(self, text):
        """Tippt Text -- mit der QWERTZ-Umrechnung aus dem Kopfkommentar."""
        for zeichen in text:
            if zeichen in SONDER:
                self.taste(SONDER[zeichen])
            elif zeichen.isdigit():
                self.taste([zeichen])
            elif zeichen.isalpha() and zeichen.isascii():
                klein = VERTAUSCHT.get(zeichen.lower(), zeichen.lower())
                self.taste(["shift", klein] if zeichen.isupper() else [klein])
            else:
                raise ValueError(
                    "kein Mapping fuer %r -- Tabelle SONDER ergaenzen" % zeichen)

    # --- Maus --------------------------------------------------------------

    def maus_relativ(self, dx, dy):
        """EIN relatives Ereignis (ungeteilt -- fuer kleine Wege)."""
        ereignisse = []
        if dx:
            ereignisse.append({"type": "rel", "data": {"axis": "x", "value": dx}})
        if dy:
            ereignisse.append({"type": "rel", "data": {"axis": "y", "value": dy}})
        if ereignisse:
            self.cmd("input-send-event", events=ereignisse)

    def bewegen(self, dx, dy):
        """Faehrt einen Weg in Schritten, die ein PS/2-Paket vertraegt."""
        while dx or dy:
            sx = max(-MAUS_SCHRITT, min(MAUS_SCHRITT, dx))
            sy = max(-MAUS_SCHRITT, min(MAUS_SCHRITT, dy))
            self.maus_relativ(sx, sy)
            dx -= sx
            dy -= sy
            time.sleep(0.02)

    def zeiger(self, x, y):
        """Setzt den Cursor auf (x, y) -- ueber den Anschlag oben links.

        Es gibt keinen Weg, die aktuelle Position zu ERFRAGEN. Also wird sie
        HERGESTELLT: weit genug nach links oben, dass der Cursor garantiert in
        der Ecke klebt, und von dort das Ziel abfahren.
        """
        self.bewegen(-ANSCHLAG, -ANSCHLAG)
        time.sleep(0.2)
        self.bewegen(x, y)
        time.sleep(0.2)

    def klick(self, knopf="left", pause=0.08):
        self.cmd("input-send-event",
                 events=[{"type": "btn", "data": {"down": True, "button": knopf}}])
        time.sleep(pause)
        self.cmd("input-send-event",
                 events=[{"type": "btn", "data": {"down": False, "button": knopf}}])
        time.sleep(pause)

    def rad(self, rastungen):
        """Mausrad: positiv = nach oben, negativ = nach unten."""
        knopf = "wheel-up" if rastungen > 0 else "wheel-down"
        for _ in range(abs(rastungen)):
            self.klick(knopf, pause=0.04)

    # --- Bildschirm --------------------------------------------------------

    def foto(self, ziel):
        # ABSOLUT machen: `screendump` schreibt die Datei im
        # Arbeitsverzeichnis von QEMU, nicht in unserem -- ein relativer
        # Pfad landet sonst irgendwo.
        self.cmd("screendump", filename=os.path.abspath(ziel), format="png")


# ---------------------------------------------------------------------------
# Kommandozeile
# ---------------------------------------------------------------------------

HILFE = """qmp_steuern.py <port> [--esc] <befehl> [<befehl> ...]

Befehle (werden der Reihe nach abgearbeitet):
  tippen <text>        Text tippen (QWERTZ-Umrechnung inklusive)
  taste <name>...      Tasten gemeinsam druecken (esc, enter, pgup, ...)
  zeiger <x> <y>       Mauszeiger auf eine Bildschirmposition setzen
  bewegen <dx> <dy>    Maus relativ bewegen
  klick [links|rechts] Maustaste druecken und loslassen
  rad <n>              Mausrad (positiv = hoch, negativ = runter)
  warte <sekunden>     Pause
  foto <datei.png>     Bildschirmfoto

--esc drueckt vor dem ersten Befehl ESC (verlaesst den Desktop-Modus und
      zeigt die Vollbild-Shell).

Voraussetzung: QEMU laeuft mit SPEEDOS_QMP=<port> cargo run
"""


def main(argv):
    if len(argv) < 3:
        print(HILFE)
        return 2
    port = int(argv[1])
    rest = argv[2:]

    vor_esc = False
    if rest and rest[0] == "--esc":
        vor_esc = True
        rest = rest[1:]

    q = Qmp(port)
    print("QMP verbunden (Port %d)." % port)
    if vor_esc:
        q.taste(["esc"])
        time.sleep(1.5)

    i = 0
    while i < len(rest):
        befehl = rest[i]
        if befehl == "tippen":
            q.tippen(rest[i + 1])
            print("  getippt: %s" % rest[i + 1])
            i += 2
        elif befehl == "taste":
            # Alle folgenden bekannten Tastennamen gehoeren zusammen.
            namen = []
            i += 1
            while i < len(rest) and (rest[i] in TASTEN_ALIAS or rest[i] == "shift"):
                namen.append("shift" if rest[i] == "shift" else TASTEN_ALIAS[rest[i]])
                i += 1
            if not namen:
                raise SystemExit("taste: kein bekannter Tastenname")
            q.taste(namen)
            print("  Taste: %s" % "+".join(namen))
        elif befehl == "zeiger":
            q.zeiger(int(rest[i + 1]), int(rest[i + 2]))
            print("  Zeiger auf (%s, %s)" % (rest[i + 1], rest[i + 2]))
            i += 3
        elif befehl == "bewegen":
            q.bewegen(int(rest[i + 1]), int(rest[i + 2]))
            i += 3
        elif befehl == "klick":
            knopf = "left"
            if i + 1 < len(rest) and rest[i + 1] in ("links", "rechts"):
                knopf = "left" if rest[i + 1] == "links" else "right"
                i += 1
            q.klick(knopf)
            print("  Klick (%s)" % knopf)
            i += 1
        elif befehl == "rad":
            q.rad(int(rest[i + 1]))
            i += 2
        elif befehl == "warte":
            time.sleep(float(rest[i + 1]))
            i += 2
        elif befehl == "foto":
            q.foto(rest[i + 1])
            print("  Bildschirmfoto: %s" % rest[i + 1])
            i += 2
        else:
            raise SystemExit("unbekannter Befehl: %s\n\n%s" % (befehl, HILFE))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
