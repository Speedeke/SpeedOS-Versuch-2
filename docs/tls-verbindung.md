# Die erste verschlüsselte Verbindung (Serie 7, Teil 4)

Was Teil 3 als machbar nachgewiesen hat, tut Teil 4: **`holes` holt eine
HTTPS-Seite.** Aus Ring 3, über unseren eigenen TCP/IP-Stack, mit einer
Zertifikatskette, die gegen unseren eigenen Vertrauensanker geprüft wurde.

    starte holes https://example.com/ --info

Dieses Dokument hält fest, wie das gebaut ist, was es nicht kann, und welche
Zahlen dabei herauskamen.

---

## 1. Die Kette, Glied für Glied

```
  holes (Ring 3, eigener Adressraum, von /platte geladen)
    │
    ├── speedhttp          HTTP/1.1 zerlegen — DERSELBE Code wie im Kernel
    ├── rustls 0.23        TLS 1.3 / 1.2                      (fremd)
    │     ├── rustls-rustcrypto   Krypto, reines Rust         (fremd, ALPHA)
    │     └── rustls-webpki       X.509-Kettenprüfung         (fremd)
    │
    ├── libspeed::tls::TlsStrom   die Naht                    (Serie 7, T4)
    ├── libspeed::tls::TcpStrom   blockierender Byte-Strom    (Serie 7, T3)
    │
    └── int 0x80  →  Syscall-ABI                              (Serie 6, T4)
          └── socket::* / tcp.rs / ipv4.rs / arp.rs           (Serie 5)
                └── virtio/net.rs                             (Serie 5)
                      └── QEMU slirp  →  Internet
```

Dazu die drei Zulieferungen aus den vorherigen Teilen von Serie 7:

| was | woher | Syscall |
|---|---|---|
| Zufall für den Handshake | `src/zufall.rs` (ChaCha20-DRBG) | `zufall` (12) |
| Zeit für die Gültigkeitsprüfung | `src/zeit.rs`, plausibilisiert | `zeit_geprueft` (13) |
| Heap für rustls | `prozess.rs`, brk-Modell | `speicher` (14) |
| Wurzelzertifikate | `/platte/system/ca-bundle.pem` | `oeffne`/`lese_at` |

---

## 2. Der HTTP-Parser wurde nicht angefasst

Das war die eigentliche Aufgabe, und sie ist strukturell gelöst statt
behauptet.

Die reine Protokoll-Logik aus `src/netz/http.rs` (Serie 5) ist in die eigene
Kiste **`speedhttp/`** umgezogen — Zeile für Zeile unverändert. Sie hat
**keine Abhängigkeiten**: kein `speed_os`, kein `libspeed`, kein Socket, kein
TLS. Sie kennt Bytes.

* Der **Kernel** benutzt sie über `pub use speedhttp::*` und behält nur den
  Transport (`roh_ueber_socket`, DNS, Socket-API).
* **`holes`** benutzt sie über einen TLS-Strom.

**Die Belege:**

1. Die `#[test_case]`-Tests am Ende von `src/netz/http.rs` sind
   **unverändert** aus Serie 5. Sie prüfen heute den Code in `speedhttp`.
2. `tests/netz_https.rs::test_parser_ist_derselbe` vergleicht die
   **Funktionsadressen**: `http::antwort_parsen` und
   `speedhttp::antwort_parsen` sind dieselbe Funktion, keine Kopie. Und dass
   `assert_eq!(ueber_kernel, ueber_kiste)` überhaupt übersetzt, heißt schon,
   dass `http::Antwort` und `speedhttp::Antwort` derselbe Typ sind.
3. `speedhttp/Cargo.toml` hat einen leeren `[dependencies]`-Block. Ein
   Parser, der nichts kennt, muss nichts lernen, wenn der Transport wechselt.

### Was doch angepasst werden musste — und was nicht

**Nicht** angepasst: `antwort_parsen`, `chunked_dekodieren`, `url_parsen`,
`naechste_url`, `anfrage_bauen`, `Url`, `Antwort`, `HttpFehler`.

Angepasst wurde die **Transport-Schicht**, also genau die Schicht, um die es
ging:

* `HttpFehler` trug bis Serie 5 auch `Dns(..)` und `Socket(..)`. Die konnten
  nicht mitziehen (Kernel-Typen), also gibt es jetzt `http::KlientFehler`
  = Protokoll **plus** Weg. Ring 3 hat sein eigenes Gegenstück
  (`libspeed::tls::TlsFehler`), und das ist richtig so: Dort ist der Weg ein
  anderer.
* `url_parsen` lehnt `https://` weiterhin ab. `holes` schneidet das Schema
  selbst ab und legt `host[:port]/pfad` vor — schemalose Eingaben nahm der
  Parser schon immer an.
* **Eine** neue Funktion: `anfrage_bauen_mit_host`. Sie baut nichts nach,
  sondern ruft `anfrage_bauen` mit einer `Url` auf, deren Host schon der
  gewünschte Text ist (bei https gehört `:443` nicht in den Host-Kopf).

---

## 3. Der Handshake: die unbuffered-Zustandsmaschine

Ohne `std` gibt es in rustls keine `ClientConnection` mit `Read`/`Write`,
sondern nur `UnbufferedClientConnection`: eine Zustandsmaschine, die man
selbst dreht und deren Puffer man selbst hält (`docs/tls-entscheidung.md` §4).
`TlsStrom::takt()` ist genau ein Durchlauf davon:

| Zustand | was wir tun |
|---|---|
| `EncodeTlsData` | rustls schreiben lassen → `aus`-Puffer |
| `TransmitTlsData` | `aus` über TCP rausschicken |
| `BlockedHandshake` | TCP lesen → `ein`-Puffer |
| `WriteTraffic` | Handshake steht; Nutzdaten verschlüsseln |
| `ReadTraffic` | entschlüsselte Bytes abholen |
| `PeerClosed` / `Closed` | Ende |

**Die Falle, in die man dabei läuft:** `process_tls_records` *leiht sich den
Eingangspuffer aus*, und der geliehene Zustand lebt bis zum Ende des `match`.
Wer im `BlockedHandshake`-Zweig direkt in denselben Puffer nachliest, bekommt
einen Borrow-Fehler. Deshalb merkt sich `takt()` nur eine `Aktion` und handelt
erst, wenn die Leihe vorbei ist.

---

## 4. Was geprüft wird — und was nicht

### Geprüft

* **Kette bis zu einer Wurzel** aus `/platte/system/ca-bundle.pem`
  (119 Wurzeln übernommen von 119 gelesenen).
* **Gültigkeitszeitraum**, gegen `zeit_geprueft` — und wenn die Uhr
  nachweislich falsch geht, liefert `SpeedUhr::current_time` `None`, rustls
  meldet `FailedToGetCurrentTime` und **bricht ab**. „Uhr kaputt, prüfen wir
  halt nicht" ist nicht implementierbar.
* **Hostname**: `servername` geht als SNI raus *und* wird gegen die Namen im
  Zertifikat abgeglichen. `TlsStrom::verbinden` macht beides aus demselben
  einen Argument — es gibt keinen Weg, nur eins davon zu tun.
* **Signaturen** der ganzen Kette (rustls-webpki).

### Nicht geprüft (unverändert aus `docs/tls-vertrauen.md` §3a)

* **Keine Sperrlisten** — weder OCSP noch CRL. Ein gestohlenes, noch nicht
  abgelaufenes Zertifikat wird akzeptiert.
* Keine Certificate Transparency, kein Pinning, keine Benutzer-CAs.
* **Kein `close_notify` erzwungen.** Schließt die Gegenstelle die
  TCP-Verbindung ohne Abschiedsgruß, gilt der Strom als beendet. Das ist von
  einem Truncation-Angriff nicht zu unterscheiden. Was davor schützt, liegt
  eine Schicht höher: Der HTTP-Parser prüft den Rumpf gegen `Content-Length`
  bzw. den 0-Chunk und meldet `UnvollstaendigeAntwort`.

### Es gibt keinen Umgehungs-Schalter

Kein `--unsicher`, kein `--zertifikat-egal`, kein „trotzdem fortfahren"-Dialog.
Das ist eine Entscheidung: Ein solcher Schalter wird benutzt, sobald es ihn
gibt — erst „nur zum Testen", dann im Skript, dann überall. Und ein TLS, das
man abschalten kann, schützt vor genau dem Angreifer nicht, der einen dazu
bringt, es abzuschalten.

Stattdessen: **Meldungen, die den Grund nennen.** Abgelaufen, falscher Name
und unbekannte Wurzel sind drei verschiedene Lagen mit drei verschiedenen
Ursachen (`TlsFehler::text()`).

---

## 5. Die Fehlerfälle, gemessen

`tests/netz_https.rs`, alle Fälle enden mit Exit-Code 4 und einer deutschen
Begründung:

| Fall | Gegenstelle | rustls-Befund | unsere Meldung |
|---|---|---|---|
| unbekannte CA (**hartes Gate**) | `10.0.2.2:8443`, eigene Test-CA | `UnknownIssuer` | „UNBEKANNTE ZERTIFIZIERUNGSSTELLE …" |
| abgelaufen | `expired.badssl.com` | `ExpiredContext` | „ZERTIFIKAT ABGELAUFEN: Es galt nur bis …" |
| falscher Hostname | `wrong.host.badssl.com` | `NotValidForNameContext` | „FALSCHER HOSTNAME …" |
| self-signed | `self-signed.badssl.com` | `UnknownIssuer` | s. o. |
| fremde Wurzel | `untrusted-root.badssl.com` | `UnknownIssuer` | s. o. |
| kein TLS dahinter | `10.0.2.2:8000` (http) | `InvalidMessage(InvalidContentType)` | „PROTOKOLLFEHLER … spricht dort gar kein TLS." |

**Testmethodik wie bei TCP** (`docs/tcp-scope.md`): Das *harte* Gate liegt auf
dem lokalen Server, den wir kontrollieren (`tools/tls_testserver.py`). Die
badssl-Läufe sind Bericht — eine Testsuite darf nicht von fremden Servern
abhängen, aber sie darf von ihnen berichten.

### Zwei Lektionen aus dem Testserver

1. **`openssl req -x509` erzeugt keinen tauglichen Testfall.** Ein einzelnes
   selbst signierendes Zertifikat hat `CA:TRUE` und kein
   `extendedKeyUsage=serverAuth`, ist also formal gar kein Server-Zertifikat.
   rustls-webpki lehnt es aus **Formgründen** ab
   (`InvalidCertificate(Other(..))`) — der Test hätte die Formalien geprüft
   statt der Vertrauenskette. Der Server legt deshalb jetzt eine echte Kette
   vor: eigene Mini-CA → formal einwandfreies Serverzertifikat.
2. **Ohne `tls12` sieht man die Zertifikate nie.** `rustls-rustcrypto` liefert
   ohne dieses Feature nur die drei TLS-1.3-Suiten (daher „3 Ciphersuites" in
   Teil 3). Sämtliche badssl-Endpunkte können **kein** TLS 1.3 und antworten
   mit `HandshakeFailure`, *bevor* sie ein Zertifikat schicken. Man hält einen
   Aushandlungs-Fehlschlag dann für eine bestandene Prüfung. Mit `tls12` sind
   es neun Suiten.

---

## 6. Zahlen (QEMU/WHPX, TLS 1.3, `TLS13_AES_128_GCM_SHA256`)

| Messgröße | Wert |
|---|---|
| TLS-Handshake, example.com | **34–36 ms** (TCP allein 31–33 ms) |
| TLS-Handshake, curl.se | **12–13 ms** (TCP allein 8–10 ms) |
| Heap-Spitze, 559-Byte-Seite | **121 160 Byte** |
| Heap-Spitze, 186-KiB-Datei | **648 552 Byte** |
| Durchsatz, 186 446 Byte über TLS | **6 278 KiB/s** (29 ms) |
| Vergleich: dieselbe Datei, **ohne** TLS, Kernel-Klient, LAN | **406 KiB/s** (448 ms) |
| ELF-Größe `holes` | 949 984 Byte (`tlsspike`: 830 240) |
| Wurzeln | 119 von 119 übernommen |

### Die interessanteste Zahl ist die vorletzte

**TLS aus Ring 3 ist 15× schneller als plain TCP aus dem Kernel** — bei
derselben Datei, und der lokale Server ist der *nähere*. Verschlüsselung ist
also nicht der Engpass; das Warten war es.

Der Grund ist der Wecken-Fix aus Serie 7, Teil 0. Der Kernel-Klient
(`http::roh_ueber_socket`) wartet mit `zeit::warte_auf_interrupt()` und holt
im Wesentlichen ein Segment je Tick. `holes` wartet mit `abgeben()` — es gibt
die Zeitscheibe an den Netz-Task ab und bekommt sie sofort zurück, sobald
Daten da sind. Die 4 ms Weck-Latenz, die in Serie 6 den Pipe-Durchsatz auf
199 KiB/s gedrückt haben, drücken hier den HTTP-Durchsatz.

**Ehrliche Einordnung:** Die TLS-Zahl misst eine Internet-Verbindung durch
slirp (mit Varnish-Cache davor), die Vergleichszahl einen lokalen Server. Der
Vergleich taugt trotzdem, weil die Verzerrung in die *andere* Richtung geht:
Der lokale Server hat den kürzeren Weg und ist trotzdem 15× langsamer.

### Heap: warum 648 KiB für eine 186-KiB-Datei

`holes` hält beim großen Abruf drei Dinge gleichzeitig: die rohen
Antwort-Bytes (188 405), den geparsten Rumpf (186 446) und die
Zwischenpuffer. Das CA-Bündel liegt **nicht** auf dem Heap, sondern in
`.bss` — sonst würde die Spitze die Größe einer Datei messen statt den
Bedarf von TLS. Der reine TLS-Anteil ist die 121 KiB der kleinen Seite.

---

## 7. Was als Nächstes ansteht

* **`close_notify` erzwingen**, sobald die Gegenstelle es schickt — heute
  wird sein Fehlen nur toleriert, nicht unterschieden.
* **OCSP-Stapling** (der einzige Sperrlisten-Weg, der nicht das Surfverhalten
  verrät und nicht weich scheitert).
* **NTP**, damit die Zeit-Plausibilität mehr kann als „vor dem Bau-Datum".
* Der Anbieter ist **0.0.2-alpha**. Diese Warnung bleibt stehen, bis er es
  nicht mehr ist.
