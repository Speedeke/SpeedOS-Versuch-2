#!/usr/bin/env python3
"""usb_leck_test.py - Geraete wiederholt ein- und ausstecken.

WOZU: Die Aufzaehlung legt je Geraet mehrere Seiten an (Device Context,
Input Context, EP0-Ring, Antwortpuffer, je Endpunkt ein Transfer Ring).
Beim Abziehen muessen sie ALLE zurueckkommen. Ein USB-Leck faellt sonst
erst nach dem zwanzigsten Umstecken auf - und dann sucht man an der
falschen Stelle.

Dieses Skript steckt N-mal ein Geraet ein und wieder ab. Der Kernel
protokolliert dabei je Runde, wie viele Seiten zurueckgegeben wurden;
die Frame-Bilanz steht im seriellen Protokoll.

AUFRUF:
    # Terminal 1:
    $env:SPEEDOS_QMP=4444; cargo run
    # Terminal 2:
    python tools/usb_leck_test.py 4444 10
"""

import json
import socket
import sys
import time


class Qmp:
    def __init__(self, port, host="127.0.0.1", timeout=30):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.datei = self.sock.makefile("rwb")
        self._lesen()
        self.cmd("qmp_capabilities")

    def _lesen(self):
        while True:
            zeile = self.datei.readline()
            if not zeile:
                raise RuntimeError("QMP-Verbindung zu")
            nachricht = json.loads(zeile)
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
    runden = int(argv[2]) if len(argv) > 2 else 10

    q = Qmp(port)
    print(f"[leck] verbunden auf Port {port}, {runden} Runden")

    for i in range(runden):
        q.cmd("device_add", driver="usb-tablet", id="leck1", bus="xhci.0")
        time.sleep(1.2)
        q.cmd("device_del", id="leck1")
        time.sleep(1.2)
        print(f"[leck] Runde {i + 1}/{runden} fertig")

    print("[leck] fertig. Im seriellen Protokoll pruefen:")
    print("[leck]   - je Runde ein 'erkannt' und ein 'abgeraeumt'")
    print("[leck]   - die Frame-Bilanz am Ende (Befehl 'meminfo')")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
