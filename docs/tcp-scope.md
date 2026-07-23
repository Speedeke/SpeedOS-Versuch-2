# TCP in SpeedOS — Umfang, Reißleine und bewusste Lücken

Stand: Juli 2026, Serie 5. Dieses Dokument wird **vor dem Code** festgelegt
(die Ingenieur-Entscheidung aus der Bestandsaufnahme, `serie5-netzwerk.md`
Abschnitt b), damit die riskanteste Wette des Projekts — TCP-Korrektheit —
eine geplante Entscheidung bleibt und keine zermürbende Überraschung wird.

## Warum TCP selbst?

TCP ist das lehrreichste Subsystem des Stacks: der Zustandsautomat, die
Sequenznummern-Arithmetik, Retransmit und der geordnete Verbindungsabbau
sind der Kern dessen, was „ein verlässlicher Bytestrom über ein unzuverlässiges
Netz" bedeutet. **Deshalb bauen wir es selbst** — aber SCHARF ABGEGRENZT als
**Lern-Artefakt**, nicht als produktionsreifer Stack.

## Was wir bauen (Minimal-Viable-TCP)

- Der **vollständige Zustandsautomat**: CLOSED, LISTEN, SYN_SENT, SYN_RCVD,
  ESTABLISHED, FIN_WAIT_1, FIN_WAIT_2, CLOSING, TIME_WAIT, CLOSE_WAIT,
  LAST_ACK. Der ist überschaubar — den machen wir ganz.
- **3-Wege-Handshake**, aktiv (`connect`) UND passiv (`listen`/`accept`).
- **In-Order-Datentransfer** mit Sequenz-/ACK-Nummern und einem **festen**
  Sende-/Empfangsfenster (die Größe der Byte-Ringpuffer — KEIN Window-Scaling).
- **Retransmit-on-Timeout** mit **simpler RTO**: fester Startwert,
  exponentielles Backoff, feste Obergrenze der Versuche. KEIN Karn/Jacobson-
  RTT-Schätzer.
- **Sauberer Verbindungsabbau** inkl. TIME_WAIT (mit bewusst VERKÜRZTEM
  2·MSL — siehe unten).
- Alles über die **Byte-Ring-Abstraktion** (`netz::puffer::Ringpuffer`);
  die Puffer-Ownership ist explizit dokumentiert (siehe `tcp.rs`).

## Was BEWUSST fehlt (und warum)

- **Congestion-Control** (Slow Start, AIMD, CUBIC): braucht es erst, um Netze
  nicht zu überlasten — für unseren LAN-Lernzweck unnötig und komplex.
- **Fast-Retransmit / Duplicate-ACK-Erkennung**: reine Beschleunigung; der
  Timeout-Retransmit reicht funktional.
- **SACK (Selective ACK)**: Optimierung bei Verlust; wir kommen mit
  kumulativem ACK aus.
- **Window-Scaling**: nur für hohe Bandbreite·Latenz nötig; festes Fenster
  genügt.
- **Out-of-Order-Reassembly**: Out-of-Order-Segmente werden **verworfen** und
  per kumulativem ACK (der Sender läuft in den Timeout und sendet erneut)
  wieder angefordert. Das ist die größte bewusste Vereinfachung — ehrlich
  benannt: bei Verlust/Umordnung ist unser TCP LANGSAM (Go-Back-artig), aber
  korrekt.
- **TIME_WAIT = 2·MSL** ist auf **2 Sekunden verkürzt** (statt der RFC-793-
  240 s). Für einen einzelnen Client, der Verbindungen nicht sofort auf
  demselben Port-Paar wiederverwendet, ist das ungefährlich — und es macht
  Tests und Bedienung erträglich. Ehrlich dokumentiert als Abweichung.
- **PMTU-Discovery / Fragmentierung**: entfällt (IPv4 fragmentieren wir nicht,
  Segmente bleiben klein).
- **Urgent Pointer, TCP-Optionen außer implizitem MSS-Default**: ignoriert.

## DAS REISSLEINEN-KRITERIUM (jetzt festgelegt)

> **Wenn eine HTTP/1.0-Anfrage gegen einen erreichbaren Server (LAN oder über
> das QEMU-slirp-NAT) nicht in mindestens 9 von 10 Versuchen SAUBER lädt —
> also (1) 3-Wege-Handshake erfolgreich, (2) die Antwort VOLLSTÄNDIG empfangen
> (Status + Header + Body bis zum Verbindungsende), (3) danach ein SAUBERER
> Close (kein Hänger, kein Reset mitten im Transfer) — dann ersetzen wir in
> einem späteren Schritt die TCP-SCHICHT (nur die) durch die geprüfte
> `no_std`-Bibliothek `smoltcp`. Die unteren Schichten (Ethernet, ARP, IPv4,
> ICMP, UDP, DHCP, DNS) bleiben in JEDEM Fall unser Eigenbau.**

**Messvorschrift:** 10 aufeinanderfolgende `hole`-Läufe gegen denselben
Server; ein Lauf gilt als Erfolg, wenn die drei Bedingungen oben erfüllt sind.
< 9 Erfolge ⇒ Reißleine ziehen. Das Kriterium ist absichtlich an das
BEOBACHTBARE Verhalten geknüpft (nicht an Code-Ästhetik), damit die
Entscheidung nachprüfbar und unemotional ist.

**Warum diese Schwelle:** Ein naives TCP „funktioniert im Labor" und hängt sich
gegen echte Gegenstellen auf mysteriöse Weise auf. 9/10 gegen eine echte
Gegenstelle ist die Grenze, ab der das Eigenbau-TCP den Lerngewinn geliefert
hat UND belastbar genug für die nächste Stufe (TLS/HTTP-Anwendung) ist. Darunter
verbrennt weiteres Debugging Zeit, ohne mehr zu lehren — dann ist der Wechsel
die richtige Ingenieur-Entscheidung, keine Niederlage.

## Prüfstrategie (ohne echten Peer im Unit-Test)

- **Zustandsübergänge** als Tabelle (Erwartung je (Zustand, Ereignis)).
- **Sequenznummern-Arithmetik** inkl. u32-Wraparound.
- **Retransmit-Auslösung** (Timer läuft ab → erneute Sendung, Backoff).
- **Loopback-Test**: zwei TCP-Instanzen über einen SIMULIERTEN Kanal mit
  einstellbarem Paketverlust — Handshake + Daten + Close müssen auch bei
  moderatem Verlust durchkommen (der Beweis, dass Retransmit + Zustandsautomat
  zusammenspielen).
- **End-to-End** (soweit Netz verfügbar): eine echte HTTP-Anfrage über slirp —
  das ist zugleich die Messung für die Reißleine oben.

## Messergebnisse (Juli 2026)

### Messung 1 — Internet über slirp (`tests/netz_tcp.rs`)

10 HTTP-Abrufe gegen `example.com:80`: **10/10 sauber** — jeder Lauf mit
erfolgreichem Handshake, vollständiger Antwort (`HTTP/1.1 200 OK`) und
sauberem Close.

### Messung 2 — LAN-Server, der eigentliche Prüfpunkt (`tests/netz_http.rs`)

Aufbau: auf dem Host `python -m http.server 8000` in einem Verzeichnis mit
einer **21 700 Byte** großen `probe.txt`; QEMUs slirp zeigt den Host dem Gast
als `10.0.2.2`. Die Datei ist ABSICHTLICH größer als unser Empfangsfenster
(8 KiB) — der Transfer läuft also über mehrere Fensterfüllungen samt
Fenster-Updates.

Geprüft wird bei JEDEM Abruf streng: Status 200, Rumpflänge **exakt** gleich
`Content-Length`, Anfang UND Ende des Inhalts vorhanden (kein verlorenes oder
doppeltes Byte).

**Ergebnis: 10/10 Abrufe sauber, je 21 700 Byte vollständig.**

```
[LAN] Versuch  1..10: OK — HTTP 200 , 21700 Byte vollstaendig
[LAN-REISSLEINE] 10/10 Abrufe sauber (21700 Byte je Datei). Kriterium: >= 9/10.
```

### Verdikt

Das Kriterium (≥ 9/10) ist in BEIDEN Messungen erfüllt → **die Reißleine wird
NICHT gezogen, das Eigenbau-TCP bleibt.** Beide Messungen sind als Tests
reproduzierbar (`cargo test --test netz_http`, `--test netz_tcp`); manuell:
`hole http://10.0.2.2:8000/probe.txt` bzw. `hole http://example.com`.

Zusätzlich beweist `test_tcp_loopback_mit_verlust`, dass Handshake +
mehrsegmentige Daten + Close auch bei **20 % Paketverlust** durchkommen
(Retransmit + Zustandsautomat greifen zusammen), und
`test_http_auf_platte_speichern`, dass ein über den eigenen Stack geholter
Body byte-identisch auf der SpeedFS-Platte landet.

### Reproduktion der LAN-Messung

```
# auf dem Host (Verzeichnis mit probe.txt):
python -m http.server 8000
# dann im Projekt:
cargo test --test netz_http
```
Läuft kein Server, überspringt der Test die Messung sauber (statt rot zu
werden) — der TCP-Kern ist ohnehin per Loopback-Unit-Test abgesichert.
