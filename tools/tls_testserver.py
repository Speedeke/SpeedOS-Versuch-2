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
import ssl
import subprocess
import sys
import threading

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


def gross_erzeugen(n):
    """Ein deterministisches Muster der Laenge n."""
    zeile = b"0123456789abcdef" * 4 + b"\n"  # 65 Byte
    voll = (zeile * (n // len(zeile) + 1))[:n]
    return voll


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "SpeedOS-TLS-Testserver/1.0"

    def _antworten(self, koerper, typ="text/plain"):
        self.send_response(200)
        self.send_header("Content-Type", typ)
        self.send_header("Content-Length", str(len(koerper)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(koerper)

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
        else:
            koerper = b"nicht gefunden\r\n"
            self.send_response(404)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(koerper)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(koerper)

    def log_message(self, format, *args):
        sys.stderr.write("  [tls-testserver] %s\n" % (format % args))


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


def main():
    zerleger = argparse.ArgumentParser(description=__doc__)
    zerleger.add_argument("--port", type=int, default=8443)
    zerleger.add_argument("--host", default="0.0.0.0")
    argumente = zerleger.parse_args()

    zertifikat_sicherstellen()

    kontext = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    kontext.load_cert_chain(certfile=ZERT, keyfile=SCHLUESSEL)

    server = http.server.ThreadingHTTPServer((argumente.host, argumente.port), Handler)
    server.socket = kontext.wrap_socket(server.socket, server_side=True)

    print("=" * 70)
    print(" SpeedOS TLS-Testserver -- SELBST AUSGESTELLTES Zertifikat")
    print("=" * 70)
    print("  https://%s:%d/klein.txt   (%d Byte)" % (argumente.host, argumente.port, len(KLEIN)))
    print("  https://%s:%d/gross.bin   (%d Byte)" % (argumente.host, argumente.port, GROSS_BYTES))
    print("  https://%s:%d/chunked     (chunked kodiert)" % (argumente.host, argumente.port))
    print()
    print("  Aus SpeedOS heraus (QEMU-slirp zeigt den Host als 10.0.2.2):")
    print("      starte holes https://10.0.2.2:%d/klein.txt" % argumente.port)
    print()
    print("  ERWARTETES ERGEBNIS: ABLEHNUNG (unbekannte Zertifizierungsstelle).")
    print("  Wenn holes hier eine Seite anzeigt, ist die Pruefung kaputt.")
    print("=" * 70)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n  beendet.")


if __name__ == "__main__":
    main()
