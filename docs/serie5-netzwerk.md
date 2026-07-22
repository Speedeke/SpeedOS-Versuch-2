# Bestandsaufnahme für Serie 5 — Netzwerk

Stand: Ende Serie 4 (Juli 2026). Serie 4 hat SpeedOS **persistent**
gemacht (BlockDevice-Naht, ATA-PIO- und virtio-blk-Treiber, SpeedFS mit
fsck + Absturz-Folter-Test, FAT32-Lesen, Platten-Log) und als
**Live-System** auf echte Hardware gebracht. Serie 5 soll SpeedOS ans
**Netz** bringen. Diese Bestandsaufnahme beantwortet die vier
Kernfragen ehrlich — inklusive dessen, was NICHT reicht und wo „alles
selbst" an die Realität stößt.

---

## (a) virtio-net auf unserer Virtqueue-Basis — was fehlt konkret?

Die gute Nachricht zuerst: Der **wiederverwendbare Teil steht schon**.
`src/virtio/virtqueue.rs` ist BEWUSST geräte- und transport-unabhängig
gebaut (Split-Virtqueue: Deskriptoren + Avail-/Used-Ring in physisch
zusammenhängendem Speicher). Seine API —
`kette_anlegen(&[(PhysAddr, len, beschreibbar)]) -> Option<kopf>`,
`verfuegbar_machen(kopf)`, `used_abholen() -> Option<(kopf, len)>` — ist
genau die, die auch virtio-net braucht. Sie wird **unverändert**
weiterbenutzt. Auch die PCI-Schicht (`src/pci.rs`) liefert schon alles
Nötige: Vendor/Device/Klasse, die dekodierten BARs und — wichtig —
`interrupt_line()` (Config-Offset 0x3C), also die IRQ-Nummer, die die
Firmware dem Gerät zugewiesen hat.

Was für **virtio-net** über virtio-blk hinaus fehlt, konkret:

1. **Interrupts — DAS zentrale fehlende Stück.** virtio-blk ist
   **gepollt** (`blk.rs`: Notify schreiben, dann `used_abholen()` in
   einer TSC-Timeout-Schleife). Das geht bei der Platte, weil WIR den
   Auftrag auslösen und sofort auf die Antwort warten. Beim Netz kommen
   **Pakete unaufgefordert** — Polling würde entweder die CPU verbrennen
   oder Pakete verspätet sehen. Also brauchen wir einen echten
   **IRQ-Pfad**: einen IDT-Handler für die PCI-IRQ des Geräts (aus
   `interrupt_line()`; auf unserem 8259-PIC landen PCI-Leitungen typisch
   auf IRQ 9–11 am zweiten PIC), im Handler das virtio-**ISR-Register**
   (Legacy-BAR-Offset 0x13) lesen (quittiert den Interrupt) und einen
   async RX-Task **wecken** — exakt das Tastatur-/Maus-Muster
   (`interrupts.rs` → lock-freie Queue → `AtomicWaker` → Task). Dazu:
   die PIC-Maske für diese IRQ freischalten (heute setzt `lib::init()`
   nur Timer/Tastatur/Kaskade/Maus frei).
2. **Mehrere Virtqueues.** virtio-net hat mindestens **RX (Queue 0)**
   und **TX (Queue 1)** (plus optional eine Control-Queue für
   VIRTIO_NET_F_CTRL_VQ). virtio-blk nutzt EINE. Die `Virtqueue`-Struct
   ist schon **pro Queue** — der Treiber verwaltet einfach mehrere
   Instanzen und wählt beim Notify den richtigen Queue-Index. Kein
   Umbau an der Virtqueue selbst.
3. **RX-Puffer vorab einstellen (+ Nachfüllen).** Anders als bei der
   Platte (wir liefern den Puffer zum Auftrag) muss die RX-Queue
   **im Voraus** mit leeren, gerätebeschreibbaren Puffern gefüllt sein,
   in die die NIC ankommende Pakete DMA-t. Nach jedem konsumierten Paket
   wird der Puffer neu eingestellt. Jedes Paket trägt vorne einen
   `virtio_net_hdr` (10 bzw. 12 Byte). DMA-Puffer müssen physisch
   zusammenhängend sein — `memory::allocate_pages` liefert das (der
   Bounce-Puffer-Trick aus `blk.rs` entfällt, weil wir hier eigene
   DMA-Puffer besitzen).
4. **Net-spezifische Feature-Negotiation + Config.** MAC-Adresse aus dem
   Device-Config-Bereich lesen (VIRTIO_NET_F_MAC), ggf. Status-Feld.
   Ansonsten identische Init-Sequenz wie blk (Reset → ACK → DRIVER →
   Features → Queues → DRIVER_OK).

**Fazit (a):** Kein Neuanfang. Der Transport (PCI-Legacy-Port-I/O) und
die Virtqueue bleiben. Die echte neue Arbeit ist der **Interrupt-Pfad**
(RX kann nicht gepollt werden) und die **Mehr-Queue-/RX-Puffer-
Verwaltung**. Das ist überschaubar und hochlehrreich — es bringt uns
zum ersten Mal echte asynchrone Hardware-Events jenseits von Tastatur/
Maus/Timer.

---

## (b) Eigener TCP/IP vs. smoltcp — die ehrliche Bewertung

Das ist DIE Weichenstellung. Beides ehrlich:

### Der Lernwert-Fall (selbst bauen)
Die ganze Seele des Projekts ist „from scratch, alles selbst, um es zu
VERSTEHEN". Der Netzwerk-Stack ist dafür das vielleicht lehrreichste
OS-Subsystem überhaupt. Und die **unteren Schichten sind absolut
machbar**: Ethernet-Framing, ARP, IPv4, ICMP (Echo) und UDP haben klare,
kompakte Spezifikationen und wenig fiese Ecken. Wer die selbst baut,
versteht Netzwerke danach wirklich — und kommt mit reinem UDP schon zu
sichtbaren Erfolgen (ping, DNS, DHCP).

### Der Realitäts-Fall (TCP ist HART)
**TCP korrekt** ist eine andere Größenordnung. Nicht der Zustands-
automat an sich (11 Zustände, überschaubar), sondern alles drumherum:
Retransmission mit RTO-Schätzung (Karn/Jacobson), das Sliding Window,
Zero-Window-Probes, Delayed/Duplicate ACKs, Out-of-Order-Reassembly,
Nagle vs. Delayed-ACK-Interaktion, Silly-Window-Syndrome, der
Verbindungsabbau samt TIME_WAIT-Rennen — und ein Heer von Edge-Cases.
Ein naives TCP „funktioniert" im Labor und hängt sich dann gegen echte
Gegenstellen auf mysteriöse Weise auf. Das kann Wochen fressen, ohne
dass man klüger wird als nach den ersten 80 %.

**smoltcp** ist ein reifer, `no_std`-fähiger Rust-TCP/IP-Stack, exakt
für Embedded/OS-Einsatz gebaut, gut getestet und **lesbar** — man lernt
sogar beim Lesen.

### Meine Empfehlung: gestaffelt selbst, smoltcp als geplante Reissleine
1. **Untere Schichten SELBST** (Ethernet, ARP, IPv4, ICMP, UDP). Kein
   Kompromiss — das ist machbar, lehrreich und trägt DNS/DHCP/ping.
2. **TCP zuerst BEWUSST SELBST, aber scharf abgegrenzt** — ein
   „Minimal-Viable-TCP": 3-Wege-Handshake, In-Order-Datentransfer,
   simpler Retransmit-on-Timeout, sauberer Close. **Ausdrücklich NICHT**
   Congestion-Control, Window-Scaling, SACK, Fast-Retransmit. Das lehrt
   den TCP-Kern (den eigentlichen Lerngewinn) und holt uns eine echte
   HTTP-Anfrage im LAN. Ehrlich dazusagen: Dieses TCP ist ein
   **Lern-Artefakt**, nicht robust gegen verlustreiche/feindliche Netze
   und nicht schnell.
3. **smoltcp als VORHER festgelegte Reissleine — nur für TCP.** Das
   Umschalt-Kriterium JETZT festlegen, damit es eine geplante
   Ingenieur-Entscheidung ist und keine zermürbende Überraschung: „Wenn
   wir binnen X Aufwand keine echte HTTP-Seite zuverlässig laden, ersetzt
   smoltcp die TCP-Schicht (nur die), untere Schichten bleiben unser."

Damit bauen wir ~80 % des Stacks selbst — inklusive der lehrreichsten
Teile UND des TCP-Kerns — und begrenzen zugleich das Risiko, dass
TCP-Korrektheit zum Fass ohne Boden wird. Das respektiert die Seele des
Projekts UND die Realität. Meine klare Neigung: **Weg 1+2 gehen**, Weg 3
als bewusste Absicherung im Hinterkopf — nicht als Startpunkt.

---

## (c) Wo der Stack leben soll

**Jetzt: als Kernel-async-Task**, das passt nahtlos zum kooperativen
Executor (kein präemptiver Scheduler, keine User-Space-Prozesse — das
kommt in Serie 6). Konkret ein neues `src/netz/`-Modul mit:

- einem **`NetzGeraet`-Trait** (analog zu `BlockDevice`): `mac()`,
  `sende_frame(&[u8])`, und ein RX-Weg, der ankommende Frames über eine
  lock-freie Queue + Waker an den Stack gibt — vom Geräte-IRQ getrieben.
  virtio-net implementiert es heute, ein e1000/rtl8139 später ohne
  Änderung am Stack.
- den **Schichten als klaren Funktionsgrenzen** (Frame → ARP/IPv4 →
  UDP/TCP → Socket) plus dem Pro-Verbindungs-Zustand.
- einer **socket-artigen API** (`bind/connect/sende/empfange/schliesse`,
  Handles statt roher Zeiger).
- einem **async `netz_task`**, der RX-Frames abholt, die Zustands-
  automaten treibt und Sockets bedient — der IRQ-Handler bleibt minimal
  (nur ISR quittieren + wecken), wie bei der Tastatur.

**Nähte für Serie 6 (User-Space) schon jetzt richtig legen:** Die
Socket-API so formen, dass sie später zu **Syscalls** wird (Handles,
klare Fehler, keine Kernel-Zeiger nach außen). Die **Puffer-Ownership
explizit** machen (wer besitzt RX-/TX-Puffer?), damit eine spätere
Kernel/User-Grenze (copy-in/out) sauber einzuziehen ist, statt sie
nachträglich aus einem Zeiger-Wildwuchs herauszuoperieren.

---

## (d) DNS/TLS/HTTP — was sie später brauchen, welche Nähte jetzt

- **DHCP** (UDP-Broadcast): um überhaupt eine IP zu bekommen, ohne sie
  hart zu verdrahten. Von-selbst-machbar, früh sinnvoll.
- **DNS** (UDP + Resolver): Namen auflösen — Query bauen/parsen, kein
  TCP nötig für einfache A-Records. Gut selbst baubar, erster
  „Internet"-Moment.
- **HTTP** (TCP + einfacher Parser): die erste echte TCP-Anwendung, ein
  starker Meilenstein (`hole http://…` in der Shell).
- **TLS** — hier bricht „alles selbst" ehrlich. TLS 1.3 braucht **Krypto**
  (AES-GCM/ChaCha20-Poly1305, X25519, SHA-2, Zertifikatsprüfung mit
  RSA/ECDSA) UND einen eigenen Handshake-Automaten. Das ist ein
  **Monats-Projekt** und **sicherheitskritisch**: ein fehlerhaftes TLS
  ist schlimmer als keins. Ehrliche Empfehlung: TLS ist der Punkt, an
  dem man eine geprüfte `no_std`-Krypto-/TLS-Bibliothek nimmt ODER HTTPS
  bewusst vertagt — nicht selbst-basteln.

**Die Nähte, die JETZT sitzen müssen, damit das später glatt geht:**
1. **`NetzGeraet`-Trait** (geräte-agnostisch, wie `BlockDevice`).
2. **Socket-API mit expliziter Puffer-Ownership** — TLS ist ein Layer
   ÜBER dem Socket (er umhüllt einen TCP-Strom), also die Socket-API
   TLS-agnostisch halten; dann lässt sich TLS (eigen oder Crate) später
   darüber legen, ohne die unteren Schichten anzufassen.
3. **Saubere Schicht-Grenzen** (Frame/IP/Transport/Socket als
   Funktions-Interfaces), damit HTTP/TLS oben andocken.
4. **Eine Byte-Puffer-/Ring-Abstraktion**, wiederverwendbar für
   RX/TX und Socket-Puffer.

---

## Empfohlene erste Schritte für Serie 5

1. **virtio-net-Treiber**: PCI finden → IRQ-Handler (ISR quittieren +
   Task wecken) → RX/TX-Queues → `NetzGeraet`-Trait. Erster Meilenstein:
   rohe Ethernet-Frames empfangen und seriell hexdumpen.
2. **ARP + Ethernet-TX**: Gateway-MAC auflösen, Frames senden.
   Meilenstein: SpeedOS antwortet auf ARP.
3. **IPv4 + ICMP-Echo**: Meilenstein: der Host kann SpeedOS **anpingen**
   (und umgekehrt).
4. **UDP + DHCP + DNS**: Meilenstein: eine IP per DHCP, ein Name per DNS
   aufgelöst — aus der Shell.
5. **TCP (scoped, selbst)**: Handshake + Daten + Close. Meilenstein:
   eine kleine HTTP-Seite aus einem LAN-Server holen.
6. **Entscheidungspunkt TCP**: eigenes TCP robust genug? Sonst die
   vorher festgelegte smoltcp-Reissleine ziehen (nur TCP).

Jeder Schritt ist ein sichtbarer Erfolg, jeder baut auf dem vorigen —
und die riskante Wette (TCP-Korrektheit) ist bewusst ans Ende gestellt
und mit einer geplanten Reissleine abgesichert.
