#!/usr/bin/env python3
"""usb_einstecken.py - ein USB-Geraet zur Laufzeit ein- und ausstecken.

WOZU: Der xHCI-Treiber (Serie 9, Teil 3) soll beweisen, dass sein Event
Ring funktioniert. Geraete, die beim Start schon stecken, taugen dafuer
NICHT - ihr CSC-Bit stand schon, bevor der Controller lief, also kommt
kein Port-Status-Change-Event. Nur ein Geraet, das WAEHREND des Betriebs
dazukommt, erzeugt eins.

QEMU kann das ueber QMP: device_add / device_del. Dieses Skript macht
genau das und wartet dazwischen, damit der Poll-Task (100 ms) das Event
sicher sieht.

AUFRUF:
    # Terminal 1:
    $env:SPEEDOS_QMP=4444; cargo run
    # Terminal 2:
    python tools/usb_einstecken.py 4444

Im seriellen Protokoll muessen dann Zeilen wie diese erscheinen:
    [xhci] EVENT: Port-Status-Aenderung an Port 3
    [xhci]   Port 3: angeschlossen, Tempo high (480 Mbit/s)
"""

import json
import socket
import sys
import time


class Qmp:
    def __init__(self, port, host="127.0.0.1", timeout=30):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.datei = self.sock.makefile("rwb")
        # Begruessung lesen, dann den Handshake abschliessen.
        self._lesen()
        self.cmd("qmp_capabilities")

    def _lesen(self):
        while True:
            zeile = self.datei.readline()
            if not zeile:
                raise RuntimeError("QMP-Verbindung zu")
            nachricht = json.loads(zeile)
            # Ereignisse (z. B. DEVICE_DELETED) ueberspringen - wir
            # warten auf eine Antwort.
            if "event" in nachricht:
                continue
            return nachricht

    def cmd(self, name, **args):
        anfrage = {"execute": name}
        if args:
            anfrage["arguments"] = args
        self.datei.write((json.dumps(anfrage) + "\n").encode())
        self.datei.flush()
        antwort = self._lesen()
        if "error" in antwort:
            raise RuntimeError(f"{name}: {antwort['error']['desc']}")
        return antwort.get("return")


def main(argv):
    port = int(argv[1]) if len(argv) > 1 else 4444
    # usb-tablet, weil es sich von der schon steckenden usb-mouse
    # unterscheidet und QEMU es an einen FREIEN Port haengt.
    geraet = argv[2] if len(argv) > 2 else "usb-tablet"

    q = Qmp(port)
    print(f"[usb-test] verbunden auf Port {port}")

    print(f"[usb-test] device_add {geraet} (id=probe1) ...")
    q.cmd("device_add", driver=geraet, id="probe1", bus="xhci.0")
    print("[usb-test] eingesteckt - 1,5 s warten, damit der Poll-Task es sieht")
    time.sleep(1.5)

    print("[usb-test] device_del probe1 ...")
    q.cmd("device_del", id="probe1")
    print("[usb-test] ausgesteckt - 1,5 s warten")
    time.sleep(1.5)

    print("[usb-test] fertig. Im seriellen Protokoll muessen jetzt ZWEI")
    print("[usb-test] Port-Status-Aenderungen stehen (einstecken + ziehen).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
