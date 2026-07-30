#!/usr/bin/env python3
"""tls_testserver.py -- Ein HTTPS-Server mit SELBST AUSGESTELLTEM Zertifikat.

WOZU ER DA IST
==============
Damit `holes` (SpeedOS, Serie 7, Teil 4) einen Fehlerfall hat, der IMMER
gleich ausgeht und nicht vom Internet abhaengt.

Die badssl.com-Endpunkte (expired./wrong.host./self-signed.) sind grossartig,
aber sie sind fremde Server: Sie koennen umziehen, ausfallen oder ihr
Verhalten aendern. Ein Test, dessen HARTE Zusage an fremder Infrastruktur
haengt, ist kein Test, sondern eine Wettervorhersage. Deshalb dieselbe
Methodik wie bei TCP (docs/tcp-scope.md): Das harte Gate liegt hier auf einem
Server, den wir kontrollieren; die badssl-Laeufe sind Bericht.

WAS ER BEWEIST
==============
Der Server legt ein Zertifikat vor, das er sich selbst ausgestellt hat. Keine
Wurzel in assets/ca-bundle.pem hat es unterschrieben. `holes` MUSS die
Verbindung ablehnen ("unbekannte-ca") -- und zwar, ohne dass es einen Weg
gaebe, das zu uebergehen.

BENUTZUNG
=========
    python tools/tls_testserver.py            # Port 8443
    python tools/tls_testserver.py --port 9443

Der Gast erreicht den Host unter 10.0.2.2 (QEMU-slirp), also:
    starte holes https://10.0.2.2:8443/klein.txt

Das Zertifikat wird beim ersten Start mit `openssl` erzeugt und danach
wiederverwendet (tools/testcert/). Der Ordner ist gitignored -- ein
Schluessel gehoert nicht ins Repository, auch kein wertloser.
"""

import argparse
import http.server
import os
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time

HIER = os.path.dirname(os.path.abspath(__file__))
ZERT_ORDNER = os.path.join(HIER, "testcert")
# Die eigene, kleine Zertifizierungsstelle ...
CA_ZERT = os.path.join(ZERT_ORDNER, "speedos-test-ca.crt")
CA_SCHLUESSEL = os.path.join(ZERT_ORDNER, "speedos-test-ca.key")
# ... und das davon ausgestellte Server-Zertifikat (Kette = Server + CA).
ZERT = os.path.join(ZERT_ORDNER, "speedos-test-kette.crt")
SCHLUESSEL = os.path.join(ZERT_ORDNER, "speedos-test.key")

# Der Inhalt, den der Server ausliefert. Deterministisch, damit ein Test
# Byte fuer Byte vergleichen kann.
KLEIN = b"SpeedOS TLS-Testserver: dieser Text kommt ueber ein SELBST\r\n" \
        b"ausgestelltes Zertifikat. Wer ihn liest, hat die Pruefung\r\n" \
        b"uebersprungen -- und genau das darf nicht passieren.\r\n"
GROSS_BYTES = 512 * 1024

# Eine kleine HTML-Seite fuer `news` -- mit allem, woran eine naive
# Tag-Entfernung scheitert: <script>, <style>, Entities, Einrueckung.
HTML = b"""<!DOCTYPE html>
<html lang="de"><head>
<title>SpeedOS &ndash; Testseite</title>
<style>body { color: #123; } /* das hier darf NICHT im Text landen */</style>
<script>var geheim = "auch das nicht";</script>
</head>
<body>
  <h1>Willkommen bei SpeedOS</h1>
  <p>Dieser Absatz kommt &uuml;ber eine <b>verschl&uuml;sselte</b>
     Verbindung &ndash; und er ist absichtlich lang genug, damit der
     Zeilenumbruch etwas zu tun bekommt und man sieht, ob er an
     Wortgrenzen trennt.</p>
  <ul><li>Erstens</li><li>Zweitens</li><li>Drittens &amp; Schluss</li></ul>
  <p>Sonderzeichen: &lt;spitz&gt; &quot;doppelt&quot; &#228;&#246;&#252;</p>
</body></html>
"""

# Der Port des TLS-Servers -- als Liste, damit `main` ihn setzen kann und
# der Handler ihn fuer die /nach-tls-Weiterleitung kennt.
TLS_PORT = [8443]


def gross_erzeugen(n):
    """Ein deterministisches Muster der Laenge n."""
    zeile = b"0123456789abcdef" * 4 + b"\n"  # 65 Byte
    voll = (zeile * (n // len(zeile) + 1))[:n]
    return voll


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "SpeedOS-TLS-Testserver/1.0"

    def _antworten(self, koerper, typ="text/plain", status=200, extra=()):
        self.send_response(status)
        self.send_header("Content-Type", typ)
        self.send_header("Content-Length", str(len(koerper)))
        self.send_header("Connection", "close")
        for name, wert in extra:
            self.send_header(name, wert)
        self.end_headers()
        self.wfile.write(koerper)

    def _weiterleiten(self, ziel, status=302):
        koerper = b"weiter\r\n"
        self.send_response(status)
        self.send_header("Location", ziel)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(koerper)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(koerper)

    # -- Die BOESARTIGEN Endpunkte (Serie 7, Teil 5) --------------------
    #
    # Sie sind der Grund, warum dieser Server existiert und nicht
    # `python -m http.server` reicht: Ein braver Server kann nicht
    # beweisen, dass ein Klient mit einem unbraven zurechtkommt.

    def _abbrechen(self):
        """Kopf mit grosser Content-Length, dann MITTENDRIN die Leitung kappen.

        Das ist der Fall 'Server bricht mitten im Strom ab'. Aus Sicht des
        Klienten endet der Strom vorzeitig -- er MUSS das bemerken (der
        Rumpf ist kuerzer als angekuendigt) und darf nicht haengen.
        """
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(GROSS_BYTES))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(b"A" * 4096)
        self.wfile.flush()
        # Hart schliessen (RST statt sauberem FIN, kein TLS-close_notify).
        try:
            self.connection.setsockopt(
                socket.SOL_SOCKET, socket.SO_LINGER,
                struct.pack("ii", 1, 0))
            self.connection.close()
        except OSError:
            pass
        self.close_connection = True

    def _endlos(self):
        """Sendet ohne Ende (chunked, ohne Abschluss) -- der Test fuers Limit.

        Ein Klient ohne Groessenlimit laedt hier, bis der Heap voll ist.
        """
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        self.end_headers()
        stueck = b"X" * 4096
        try:
            while True:
                self.wfile.write(b"1000\r\n" + stueck + b"\r\n")
        except OSError:
            pass          # Klient hat abgebrochen -- genau so soll es sein
        self.close_connection = True

    def do_GET(self):
        pfad = self.path.split("?")[0]
        if pfad in ("/", "/klein.txt"):
            self._antworten(KLEIN)
        elif pfad == "/gross.bin":
            self._antworten(gross_erzeugen(GROSS_BYTES), "application/octet-stream")
        elif pfad == "/chunked":
            # Fuer den Parser-Beweis: derselbe Rumpf, aber chunked kodiert.
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Transfer-Encoding", "chunked")
            self.send_header("Connection", "close")
            self.end_headers()
            for i in range(0, len(KLEIN), 17):
                stueck = KLEIN[i:i + 17]
                self.wfile.write(b"%x\r\n" % len(stueck) + stueck + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        elif pfad == "/html":
            self._antworten(HTML, "text/html; charset=utf-8")
        # --- Weiterleitungen ---
        elif pfad == "/weiter1":
            self._weiterleiten("/weiter2")
        elif pfad == "/weiter2":
            self._weiterleiten("/klein.txt")
        elif pfad == "/schleife":
            self._weiterleiten("/schleife")          # zeigt auf sich selbst
        elif pfad == "/ringelreihen":
            self._weiterleiten("/ringelreihen2")     # zwei, die sich kreuzen
        elif pfad == "/ringelreihen2":
            self._weiterleiten("/ringelreihen")
        elif pfad == "/kette":
            # Laenger als jedes vernuenftige Limit: /kette9 .. /kette0
            self._weiterleiten("/kette9")
        elif pfad.startswith("/kette"):
            nummer = pfad[len("/kette"):]
            if nummer.isdigit() and int(nummer) > 0:
                self._weiterleiten("/kette%d" % (int(nummer) - 1))
            else:
                self._antworten(b"Ende der Kette\r\n")
        elif pfad == "/nach-tls":
            # SCHEMA-WECHSEL: http -> https (im Web der Normalfall).
            self._weiterleiten("https://10.0.2.2:%d/klein.txt" % TLS_PORT[0])
        # --- Boesartiges ---
        elif pfad == "/abbruch":
            self._abbrechen()
        elif pfad == "/endlos":
            self._endlos()
        else:
            self._antworten(b"nicht gefunden\r\n", status=404)

    def log_message(self, format, *args):
        sys.stderr.write("  [testserver] %s\n" % (format % args))


def _openssl(*argumente):
    ergebnis = subprocess.run(["openssl", *argumente], capture_output=True)
    if ergebnis.returncode != 0:
        sys.exit(
            "openssl %s fehlgeschlagen:\n%s"
            % (argumente[0], ergebnis.stderr.decode("utf-8", "replace"))
        )


def zertifikat_sicherstellen():
    """Erzeugt eine eigene Mini-CA und ein davon ausgestelltes Serverzertifikat.

    WARUM EINE KETTE UND NICHT EIN EINZELNES SELBST SIGNIERTES ZERTIFIKAT
    ====================================================================
    Der erste Versuch war `openssl req -x509` -- ein einzelnes, sich selbst
    signierendes Zertifikat. Das laesst sich mit `curl -k` wunderbar
    abrufen, ist als TESTFALL aber untauglich: So ein Zertifikat hat
    basicConstraints CA:TRUE und keinen extendedKeyUsage=serverAuth, ist
    also gar kein gueltiges SERVER-Zertifikat. rustls-webpki lehnt es
    deshalb schon aus Formgruenden ab (`InvalidCertificate(Other(..))`) --
    und damit prueft der Test die Formalien statt der Vertrauenskette.

    Was hier entsteht, ist der ECHTE Fall: ein formal einwandfreies
    Serverzertifikat (CA:FALSE, serverAuth, SAN), ausgestellt von einer
    Zertifizierungsstelle, die es wirklich gibt -- die aber NICHT in
    assets/ca-bundle.pem steht. Genau das ist die Lage, in der ein
    Angreifer waere, und die Antwort muss `UnknownIssuer` lauten.
    """
    if os.path.exists(ZERT) and os.path.exists(SCHLUESSEL):
        return
    if shutil.which("openssl") is None:
        sys.exit(
            "FEHLER: openssl nicht gefunden. Es wird gebraucht, um die\n"
            "Test-Zertifizierungsstelle zu erzeugen. Git for Windows bringt\n"
            "es mit (Git Bash)."
        )
    os.makedirs(ZERT_ORDNER, exist_ok=True)
    print("  Erzeuge Test-Zertifizierungsstelle und Serverzertifikat ...")

    server_konf = os.path.join(ZERT_ORDNER, "server.ext")
    with open(server_konf, "w") as f:
        # SAN mit dem Namen UND der slirp-Host-Adresse: So scheitert der
        # Test NICHT am Hostnamen, sondern genau an dem, worum es geht --
        # der unbekannten Zertifizierungsstelle.
        f.write("basicConstraints=critical,CA:FALSE\n"
                "keyUsage=critical,digitalSignature\n"
                "extendedKeyUsage=serverAuth\n"
                "subjectAltName=DNS:speedos.test,DNS:localhost,"
                "IP:10.0.2.2,IP:127.0.0.1\n"
                "subjectKeyIdentifier=hash\n"
                "authorityKeyIdentifier=keyid\n")

    ca_key_roh = os.path.join(ZERT_ORDNER, "ca.keyraw")
    server_key_roh = os.path.join(ZERT_ORDNER, "server.keyraw")
    csr = os.path.join(ZERT_ORDNER, "server.csr")

    # ECDSA P-256: von rustls-rustcrypto sicher unterstuetzt, klein, schnell.
    _openssl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", ca_key_roh)
    _openssl("pkcs8", "-topk8", "-nocrypt", "-in", ca_key_roh, "-out", CA_SCHLUESSEL)
    # `req -x509` kennt kein -extfile (das hat `x509`), dafuer -addext.
    _openssl("req", "-x509", "-new", "-key", CA_SCHLUESSEL, "-sha256", "-days", "3650",
             "-subj", "/CN=SpeedOS-Test-CA NICHT vertrauenswuerdig",
             "-addext", "basicConstraints=critical,CA:TRUE,pathlen:0",
             "-addext", "keyUsage=critical,keyCertSign,cRLSign",
             "-out", CA_ZERT)

    _openssl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", server_key_roh)
    _openssl("pkcs8", "-topk8", "-nocrypt", "-in", server_key_roh, "-out", SCHLUESSEL)
    _openssl("req", "-new", "-key", SCHLUESSEL, "-subj", "/CN=speedos.test", "-out", csr)
    server_zert = os.path.join(ZERT_ORDNER, "server.crt")
    _openssl("x509", "-req", "-in", csr, "-CA", CA_ZERT, "-CAkey", CA_SCHLUESSEL,
             "-CAcreateserial", "-days", "3650", "-sha256",
             "-extfile", server_konf, "-out", server_zert)

    # Die Kette, die der Server vorlegt: Serverzertifikat, dann die CA.
    with open(ZERT, "wb") as ziel:
        for teil in (server_zert, CA_ZERT):
            with open(teil, "rb") as quelle:
                ziel.write(quelle.read())


def stummer_lauscher(host, port):
    """Nimmt Verbindungen an und sagt dann NICHTS.

    Das ist der Testfall 'Handshake-Timeout': TCP steht, aber die
    Gegenstelle schickt nie ein ServerHello. Ein Klient ohne Frist wartet
    hier bis zum Sankt-Nimmerleins-Tag -- genau das soll nicht passieren.

    Die angenommenen Verbindungen werden bewusst FESTGEHALTEN (Liste), denn
    ein geschlossener Socket waere ein sauberes Dateiende und damit ein
    anderer Fall.
    """
    lauscher = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lauscher.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lauscher.bind((host, port))
    lauscher.listen(8)
    gehalten = []
    while True:
        try:
            verbindung, _ = lauscher.accept()
            gehalten.append(verbindung)
            # Nicht mehr als noetig festhalten.
            if len(gehalten) > 32:
                gehalten.pop(0).close()
        except OSError:
            time.sleep(0.1)


def main():
    zerleger = argparse.ArgumentParser(description=__doc__)
    zerleger.add_argument("--port", type=int, default=8443, help="HTTPS-Port")
    zerleger.add_argument("--klarport", type=int, default=8080,
                          help="derselbe Server OHNE TLS (fuer die Rumpf-Testfaelle)")
    zerleger.add_argument("--stummport", type=int, default=8444,
                          help="nimmt an und schweigt (Handshake-Timeout)")
    zerleger.add_argument("--host", default="0.0.0.0")
    argumente = zerleger.parse_args()

    zertifikat_sicherstellen()
    TLS_PORT[0] = argumente.port

    kontext = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    kontext.load_cert_chain(certfile=ZERT, keyfile=SCHLUESSEL)

    tls_server = http.server.ThreadingHTTPServer((argumente.host, argumente.port), Handler)
    tls_server.socket = kontext.wrap_socket(tls_server.socket, server_side=True)

    # DERSELBE Handler ohne TLS. Warum das noetig ist: Die Rumpf-Testfaelle
    # (Abbruch mitten im Strom, endlose Antwort, Weiterleitungsketten)
    # brauchen einen Server, dessen ANTWORT der Klient auch annimmt -- und
    # das TLS-Zertifikat hier wird ja zu Recht abgelehnt, bevor je ein Byte
    # Rumpf fliesst. Ueber Klartext laesst sich derselbe Klient-Code
    # deterministisch pruefen.
    klar_server = http.server.ThreadingHTTPServer((argumente.host, argumente.klarport), Handler)

    for ziel, name in ((tls_server.serve_forever, "https"),
                       (klar_server.serve_forever, "http")):
        faden = threading.Thread(target=ziel, name=name, daemon=True)
        faden.start()
    threading.Thread(target=stummer_lauscher,
                     args=(argumente.host, argumente.stummport),
                     name="stumm", daemon=True).start()

    print("=" * 70)
    print(" SpeedOS-Testserver")
    print("=" * 70)
    print("  https://10.0.2.2:%d/...   SELBST AUSGESTELLTES Zertifikat" % argumente.port)
    print("      -> MUSS abgelehnt werden (unbekannte Zertifizierungsstelle)")
    print("  http://10.0.2.2:%d/...    derselbe Server im Klartext" % argumente.klarport)
    print("      /klein.txt      %d Byte" % len(KLEIN))
    print("      /gross.bin      %d Byte" % GROSS_BYTES)
    print("      /html           eine Seite fuer `news`")
    print("      /chunked        chunked kodiert")
    print("      /weiter1        -> /weiter2 -> /klein.txt")
    print("      /nach-tls       -> https://10.0.2.2:%d/klein.txt (Schema-Wechsel)" % argumente.port)
    print("      /schleife       -> sich selbst          (Schleifenschutz)")
    print("      /ringelreihen   -> und zurueck          (Schleifenschutz)")
    print("      /kette          10 Weiterleitungen      (Zaehler-Grenze)")
    print("      /abbruch        kappt die Leitung MITTEN im Rumpf")
    print("      /endlos         sendet ohne Ende        (Groessenlimit)")
    print("  10.0.2.2:%d              nimmt an und schweigt (Handshake-Timeout)"
          % argumente.stummport)
    print("=" * 70)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        print("\n  beendet.")


if __name__ == "__main__":
    main()
