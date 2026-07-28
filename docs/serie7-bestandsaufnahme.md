# Bestandsaufnahme für Serie 7 — TLS, Zufall, Zertifikate, und der Weg zum Browser

Stand: Juli 2026, nach dem Serie-6-Abschluss.

Serie 6 hat den User-Space gebaut: Ring 3, Adressräume, präemptiver
Scheduler, Syscall-ABI, ELF-Loader, Pipes. Damit ist die Frage „kann ein
Programm bei SpeedOS laufen?" beantwortet. Die nächste Frage lautet: **kann
es sicher ins Netz?** — und danach: **kann es ein Browser sein?**

Dieses Dokument klärt, was dafür fehlt, und trifft die Vorentscheidungen,
solange sie noch billig sind.

---

## Die eine Lehre aus Serie 5, die hier alles bestimmt

Bei TCP haben wir Eigenbau gewagt und gewonnen — mit einer registrierten
Reißleine (`docs/tcp-scope.md`), einem messbaren Kriterium (≥ 9/10 saubere
HTTP-Läufe) und einem Ergebnis (10/10), das die Entscheidung getragen hat.

**Bei TLS gilt das Gegenteil, und der Grund ist eine Asymmetrie:**

| | TCP | TLS |
|---|---|---|
| Wie zeigt sich ein Fehler? | Hänger, Verlangsamung, falsche Daten | **gar nicht** |
| Kann man ihn messen? | ja — 60 Abrufe, Paketverlust, Durchsatz | **nein** |
| Was kostet ein Fehler? | eine hängende Verbindung | die gesamte Vertraulichkeit |
| Merkt es der Nutzer? | sofort | nie |

Ein TCP-Bug meldet sich. Ein TLS-Bug ist still: Die Verbindung steht, die
Seite lädt, das Schloss-Symbol ist da — und der Inhalt ist trotzdem lesbar
oder manipulierbar. Man kann sich nicht zu „mein TLS ist sicher" hin testen,
weil der Test, der fehlschlagen müsste, ein *Angreifer* ist, den man nicht
hat.

Dazu kommt: TLS 1.3 ist nicht der Zustandsautomat (der ist überschaubar),
sondern die **Krypto darunter** — konstante Laufzeit, korrekte
Nonce-Behandlung, Seitenkanäle, Padding, Schlüsselableitung. Genau die
Klasse von Code, in der ein selbstgeschriebener Fehler unsichtbar bleibt und
Jahre später ausgenutzt wird.

> **ENTSCHEIDUNG, hier und jetzt registriert: Eigenbau-TLS wird NICHT gebaut.**
> Nicht als Lernprojekt, nicht „erstmal einfach". Die Reißleine wird gar nicht
> erst gelegt, weil es kein Kriterium gäbe, an dem man sie ziehen könnte.
>
> Was wir sehr wohl selbst machen: das *Einbetten* — Zufall, Zeit,
> Zertifikatsspeicher, die Naht zwischen Socket und TLS-Bibliothek. Das ist
> Betriebssystem-Arbeit, und dort liegt der Lerngewinn dieser Serie.

---

## (a) TLS: welche Bibliothek, und was verlangt sie von uns?

### Die realistischen Kandidaten

**1. `rustls` (+ `rustls-webpki`) — die empfohlene Wahl**

Der de-facto-Standard für TLS in Rust; auditiert, in Produktion, TLS 1.2 und
1.3. Seit 0.23 `no_std`-fähig mit `default-features = false` und `alloc`.

* **Braucht:** einen `CryptoProvider` (die Krypto ist austauschbar), einen
  Zeitgeber für die Zertifikats-Gültigkeit, einen **kryptographisch sicheren
  RNG**, und einen Allocator.
* **Krypto-Provider ohne C:** Die Standardanbieter (`aws-lc-rs`, `ring`)
  bringen C-Code und `std` mit — für uns beides untauglich. Der Weg ist ein
  Provider auf **RustCrypto**-Basis (`aes-gcm`, `chacha20poly1305`, `sha2`,
  `hmac`, `hkdf`, `p256`, `x25519`), alle `no_std` und ohne C.
* **Preis:** Der Provider ist Handarbeit (Trait-Implementierung), und
  RustCrypto ist langsamer als `ring`. Für einen Browser auf einem Lernsystem
  ist das gleichgültig.
* **Gewinn:** Zertifikatsprüfung, Ketten, Hostname-Abgleich,
  Protokoll-Zustandsautomat — alles fertig und geprüft.

**2. `embedded-tls` — der kleine Gegenentwurf**

Für Embedded gebaut, TLS 1.3 nur als Client, RustCrypto darunter, deutlich
weniger Code.

* **Braucht:** einen `CryptoRng`, keinen Allocator zwingend.
* **Der Haken, und er ist gross:** Die Zertifikatsprüfung ist historisch
  schwach bis abschaltbar. Ein TLS ohne Kettenprüfung schützt gegen
  Mitlesen, aber **nicht** gegen einen Mitspieler in der Mitte — und das ist
  genau die Hälfte, auf die es ankommt. Nur brauchbar, wenn die Prüfung
  nachweislich vollständig läuft.

**3. RustCrypto direkt + eigener Zustandsautomat** — das ist Eigenbau-TLS
unter anderem Namen. Siehe oben: **nein**.

**4. mbedTLS/BearSSL über FFI** — technisch möglich, bricht aber mit „from
scratch in Rust" und bringt einen C-Toolchain-Zwang in den Bau. Nur als
Rückfallebene notieren.

### Empfehlung

**`rustls` (no_std + alloc) mit einem RustCrypto-Provider**, dazu
`rustls-webpki` für die Kettenprüfung und `webpki-roots` als Wurzelspeicher.
Fällt das an einer Hürde, ist `embedded-tls` **mit nachgewiesener**
Zertifikatsprüfung der Rückfall.

### Wo läuft TLS? — die Architektur-Entscheidung

**Im USER-SPACE, als Bibliothek neben `libspeed` — nicht im Kernel.**

Drei Gründe, und der dritte ist der eigentliche:

1. TLS braucht Heap, Zeit und Zufall — alles über die bestehende ABI
   erreichbar (bis auf den Zufall, siehe (b)).
2. Es braucht **keine** neuen Kernel-Fähigkeiten. `socket`, `verbinde`,
   `sende`, `empfange` reichen; TLS ist reine Byte-Verarbeitung darüber.
3. **Ein Fehler in einer 30.000-Zeilen-Fremdbibliothek soll den Kernel nicht
   treffen können.** Genau dafür gibt es seit Serie 6 Ring 3. TLS in den
   Kernel zu legen, würde die gerade gebaute Isolation an der
   sicherheitskritischsten Stelle wieder aufgeben.

Der Kernel bekommt also **genau einen** neuen Syscall: `zufall`.

### Was SpeedOS dafür konkret fehlt

| Anforderung | Stand | Aufwand |
|---|---|---|
| Heap im User-Space | **fehlt** — `libspeed` ist allokationsfrei | mittel: ein Bump-/Freilisten-Allocator über eine `brk`-artige Syscall-Erweiterung oder eine feste Arena |
| Kryptographischer Zufall | **fehlt vollständig** | siehe (b) — das eigentliche Thema |
| Wanduhr-Zeit | ✓ `zeit_epoche` (Sekunden seit 2000; Umrechnung auf UNIX trivial) | klein, aber Genauigkeitsfrage (siehe (c)) |
| Wurzelzertifikate | **fehlt** | klein: `webpki-roots` einbetten oder Datei auf `/platte` |
| Stack-Tiefe | 64 KiB je Prozess | vermutlich knapp für rustls — messen, ggf. erhöhen |
| Rechenzeit | ~450 ns je Kontext-Wechsel, 60–70 ns je Syscall | unkritisch; ein Handshake ist Millisekunden-Arbeit |

**Der erste Schritt ist der User-Space-Heap**, nicht TLS. Ohne Allocator
läuft `rustls` gar nicht an. Ein `SYS_SPEICHER(seiten)`, der dem Prozess
weitere Seiten in seinen Adressraum mappt, ist eine kleine, saubere
Erweiterung — der Adressraum-Code kann das längst
(`AdressRaum::bereich_mappen`).

---

## (b) Zufall — die eigentliche Voraussetzung

### Was wir haben: **nichts**

Es gibt in SpeedOS keinen einzigen Zufallsgenerator. Die zwei Stellen, die
„Zufall" im Namen tragen, sind ein LCG im Plattentest und ein LCG im
TCP-Verlusttest — beide bewusst reproduzierbar und für Krypto vollkommen
untauglich. Auch die TCP-Anfangssequenznummern und die ephemeren DNS-Ports
sind heute aus der TSC abgeleitet: für ihren Zweck ausreichend, aber
vorhersagbar.

**Das ist der Blocker für TLS.** Ein TLS-Client, der seine Zufallszahlen
raten lässt, ist wertlos: Der Client-Random, die Schlüsselanteile des
Handshakes und die Nonces hängen daran.

### Was die Hardware bietet

* **RDRAND** (CPUID.01H:ECX[30]) und **RDSEED** (CPUID.07H:EBX[18]).
  Auf der verifizierten Zielhardware (Acer Aspire A515-51, Kaby Lake)
  vorhanden; unter QEMU mit WHPX wird das Host-Feature durchgereicht.
* **Zu beachten:** `RDRAND`/`RDSEED` setzen CF=0, wenn gerade kein Wert
  bereitsteht — die Rückgabe **muss** geprüft und begrenzt wiederholt werden.
  Und es gab reale Errata (AMD-CPUs, die nach dem Aufwachen dauerhaft
  0xFFFFFFFF lieferten). Einer einzelnen Hardware-Quelle blind zu vertrauen,
  ist deshalb keine gute Idee.

### Was wir selbst haben — Entropie aus dem laufenden System

SpeedOS hat mehr davon, als man denkt:

* **TSC-Zeitstempel bei Interrupts.** Wir haben eine mikrosekundengenaue,
  invariante Uhr und vier unabhängige Interrupt-Quellen: Tastatur (IRQ 1),
  Maus (IRQ 12), Netz-RX (IRQ 11), PIT (IRQ 0). Die *unteren Bits* der
  TSC-Differenz zwischen Interrupts sind eine klassische, brauchbare
  Entropiequelle — besonders bei Tastatur und Maus (Mensch) und beim Netz
  (fremde Gegenstelle).
* **Platten-Antwortzeiten** (virtio/ATA-Abschlüsse).
* **RTC-Startzeit + Speicher-Layout** als einmalige Startwürze (schwach, aber
  kostenlos).

### Der Plan: `src/zufall.rs`

```
  RDSEED/RDRAND  ─┐
  IRQ-TSC-Jitter ─┼─►  Entropie-Pool  ─►  ChaCha20-DRBG  ─►  zufall(ptr,len)
  Platten-Timing ─┘     (Hash-Mixer)        (reseed alle N)
```

1. **CPUID-Erkennung** von RDSEED/RDRAND, mit Wiederholungsgrenze und
   Gesundheitsprüfung (mehrere Werte, nie alle gleich, nie alle 0/1 —
   angelehnt an NIST SP 800-90B).
2. **Entropie-Sammler** in den bestehenden Interrupt-Handlern: lock-frei ein
   TSC-Wert in einen Ring (die Handler dürfen nichts anderes, siehe
   Deadlock-Regel 2), Verarbeitung in einem Task.
3. **DRBG**: ChaCha20 als Generator (`chacha20`-Crate, `no_std`, keine
   Abhängigkeit von einem RNG). Reseed periodisch und nach Prozess-Start.
4. **Nie aus einer Quelle allein.** Auch wenn RDSEED da ist, wird gemischt —
   dann schadet ein defektes RDRAND nicht.
5. **Boot-Problem ehrlich behandeln:** Direkt nach dem Boot ist wenig
   Entropie da, und genau dann will ein Programm vielleicht schon TLS. Der
   Syscall soll deshalb **blockieren**, bis der Pool ausreichend gefüllt ist
   (wie Linux' `getrandom`) — das Warte-Modell aus Serie 6 kann das jetzt.
   Ein „geht schon irgendwie"-Fallback wäre die schlimmste Lösung.

**Neuer Syscall:** `zufall(ptr, len)` → gefüllte Bytes. Blockierend, bis der
Pool bereit ist. Deckel wie bei allen Puffern.

**Nebengewinn:** Sobald es ihn gibt, sollten TCP-Anfangssequenznummern und
ephemere Ports daraus gespeist werden — das schliesst eine echte, wenn auch
kleine, Lücke des Netz-Stacks.

---

## (c) Zertifikate — und warum unsere Uhr plötzlich wichtig wird

### Woher kommt der Wurzelspeicher?

`webpki-roots` — Mozillas CA-Bundle als reines Rust-Datenmodul, `no_std`,
etwa 150 KiB. Kein Netz, keine Laufzeitabhängigkeit.

**Zwei Wege, ihn abzulegen:**

* **Eingebettet** in das TLS-Programm (`include_bytes!`-Muster wie bei
  `src/programme.rs`): einfach, immer konsistent, aber nur mit einem Neubau
  aktualisierbar.
* **Als Datei** auf `/platte/system/wurzeln.pem`: aktualisierbar, und ein
  guter Anlass für einen `zertifikate`-Shell-Befehl. Dafür muss man den
  Ladepfad absichern (eine manipulierte Datei = ein manipuliertes
  Vertrauensanker-Set).

**Empfehlung:** eingebettet beginnen, Datei später — mit derselben
Begründung wie bei den Programmen: erst funktionieren, dann bequem.

### Kettenprüfung

`rustls-webpki` erledigt Pfadbildung, Signaturprüfung, Namensabgleich
(inklusive SAN/Wildcards) und Gültigkeitszeiträume. Das selbst zu schreiben
wäre wieder Eigenbau-Krypto — dieselbe Absage wie oben.

**Was wir liefern müssen:** die aktuelle Zeit als `UnixTime`.

### Und da wird unsere Uhr zum Thema

`zeit::jetzt()` liefert RTC-Anker + TSC — in QEMU die **Host-Lokalzeit**
(`-rtc base=localtime`), auf echter Hardware die CMOS-Uhr.

**Drei Probleme, alle real:**

1. **Zeitzone.** Wir nehmen die RTC als Lokalzeit; Zertifikate rechnen in
   UTC. Bei ±2 Stunden Versatz ist das für Gültigkeitsfenster von Monaten
   harmlos — aber es ist schlampig, und es gehört sauber getrennt
   (RTC → UTC → Anzeige-Offset).
2. **Eine falsch gestellte CMOS-Uhr** (leere Pufferbatterie, nie gestellt)
   lässt jede Kettenprüfung scheitern — oder, schlimmer, ein abgelaufenes
   Zertifikat gültig erscheinen.
3. **Die Versuchung.** „Zeit stimmt nicht, prüfen wir die Gültigkeit halt
   nicht" ist der Punkt, an dem TLS aufhört, etwas wert zu sein.

**Haltung dazu (Daten-Integritäts-Regel dieses Projekts, auf Sicherheit
angewandt):** Die Gültigkeitsprüfung wird **nie** stillschweigend
übersprungen. Ist die Zeit unplausibel (vor dem Baudatum, weit in der
Zukunft), sagt SpeedOS das **deutlich** und verweigert die Verbindung. Die
saubere Lösung ist **NTP** — wir haben UDP, DNS und einen funktionierenden
Stack; ein SNTP-Client sind wenige hundert Zeilen und ein guter erster
Schritt der Serie.

**Ausdrücklich draussen:** Sperrlisten (CRL) und OCSP. Beide brauchen
zusätzliche Netzabrufe und eigene Zustandsverwaltung; für ein Lernsystem ist
das der falsche Aufwand. Wird dokumentiert, nicht verschwiegen.

---

## (d) Browser-Vorbereitung: die Fenster-Naht

### Soll der Browser ein User-Space-Prozess sein? — **Ja, unbedingt.**

Ein Browser ist das grösste und am wenigsten vertrauenswürdige Programm des
Systems: Er verarbeitet HTML, CSS und Bilder von wildfremden Servern. Ein
Parser-Fehler in einer Kernel-App wäre eine Kernel-Übernahme durch eine
Webseite.

**Genau dafür existiert alles, was Serie 6 gebaut hat.** Den Browser als
Kernel-App zu schreiben, hiesse, die ganze Serie 6 an der einen Stelle
wegzuwerfen, an der sie am meisten zählt.

### Was der Fenster-/UI-Schicht dafür fehlt

Heute ist ein Fensterinhalt ein `Box<dyn App>` — ein Rust-Trait-Objekt **im
Kernel**, dessen `nachricht`/`tick` **unter dem MANAGER-Lock** laufen. Ein
Prozess kann so etwas nicht besitzen. Es fehlen vier Dinge:

**1. Ein Fenster-Handle.**
`KernelObjekt::Fenster(FensterId)` in der Handle-Tabelle. Damit erbt man das
Aufräumen geschenkt: Stirbt der Prozess, schliesst der `Drop` der Tabelle
sein Fenster — genau wie heute schon Sockets und Pipe-Enden. Kein Pfad kann
es vergessen.

**2. Der Pixel-Weg — die eigentliche Entwurfsentscheidung.**

| | (a) Zeichen-Kommandos | (b) **geteilter Fenster-Puffer** |
|---|---|---|
| Wie | Prozess schickt „Rechteck, Text, Blit" | Kernel mappt den `FensterPuffer` in den Prozess |
| Kernel-Arbeit je Frame | interpretiert Kommandos, zeichnet | „schmutzig" markieren + komponieren |
| Protokollumfang | gross und wachsend | **eine** Syscall-Familie |
| Lock-Problem | akut (Zeichnen unter MANAGER-Lock) | **entschärft** (Kernel-Arbeit ist kurz) |
| Passt zum Browser? | nein — er will selbst rendern | **ja** |

→ **(b) ist die Wahl.** Der Prozess bekommt seinen Fenster-Puffer als
gemappten Speicher, zeichnet selbst und meldet mit einem Syscall
`fenster_fertig(handle, x, y, breite, hoehe)`, welcher Bereich neu ist. Das
fügt sich exakt in das bestehende Dirty-Rect-Protokoll ein.

*Was dafür im Kernel fehlt:* `AdressRaum::map_benutzer` legt heute immer
einen **frischen** Frame an. Für das Teilen braucht es eine Variante
`map_benutzer_frame(page, frame, rechte)`, die einen **vorhandenen** Frame
einblendet — und die den Frame **nicht** in `eigene` einträgt, damit `Drop`
ihn nicht freigibt (er gehört dem Fenster, nicht dem Prozess). Das ist eine
kleine, klar begrenzte Ergänzung; die Besitz-Buchführung dafür ist schon da.

*Sicherheit:* Der Prozess kann in seinen eigenen Puffer beliebigen Unsinn
schreiben — das ist sein Fenster, der Schaden bleibt darin. Fremde Puffer
kann er nicht erreichen, weil nur seine eigenen Frames gemappt werden.

**3. Ereignisse zurück zum Prozess.**
Maus und Tastatur des fokussierten Fensters müssen zum Besitzer. Das ist
strukturell dasselbe Problem wie „aus einer Pipe lesen", und es ist seit
Serie 6 gelöst: eine Warteschlange je Fenster plus ein **blockierender**
`fenster_ereignis(handle, ptr)`. Der Warte-Grund
(`Warteauf::FensterEreignis(id)`) fügt sich in das bestehende Modell ein.

**4. Der Compositor darf nicht auf den Prozess warten.**
Zeichnet ein Prozess nicht (oder stürzt ab), muss der Compositor trotzdem
seine Frames liefern — er zeigt dann eben den letzten Puffer-Inhalt. Da der
Puffer im Kernel liegt und nur *gelesen* wird, fällt das von selbst an.
**Ein hängender Browser darf den Desktop nicht einfrieren**, und mit (b) tut
er es nicht.

### Was der Browser sonst noch braucht

* **Schriften im User-Space.** Der Prozess rendert selbst, also braucht er
  einen Font. `noto-sans-mono-bitmap` ist ein gewöhnliches Crate — `userland`
  kann es direkt einbinden. Kein Kernel-Anteil nötig.
* **Einen User-Space-Heap** (siehe (a)) — HTML-Parsen ohne Allocator ist
  keine ernsthafte Option.
* **Mehr als 8 Prozesse?** `MAX_PROZESSE = 8` reicht heute; ein Browser mit
  Prozess je Tab würde das sprengen. Die Grenze ist eine Konstante, aber die
  Prozess-Tabelle wird im Timer gelesen und darf nicht allozieren — sie
  müsste also grösser, nicht dynamisch werden.

---

## Empfohlene Reihenfolge

**Serie 7 — Sicher ins Netz:**

1. **`src/zufall.rs`** — RDSEED/RDRAND + Interrupt-Entropie + ChaCha20-DRBG,
   Syscall `zufall`, Gesundheitsprüfungen. *Ohne das geht nichts weiter.*
   Nebengewinn: TCP-Sequenznummern und DNS-Ports werden unvorhersagbar.
2. **SNTP-Client** — damit die Zertifikats-Gültigkeit auf einer Zeit fusst,
   der man trauen kann. Klein, und wir haben alles dafür.
3. **User-Space-Heap** — `SYS_SPEICHER` plus ein Allocator in `libspeed`.
4. **TLS als userland-Bibliothek** — `rustls` (no_std) mit
   RustCrypto-Provider, `rustls-webpki`, `webpki-roots`.
5. **Meilenstein:** `starte netzhole https://example.com` — dieselbe
   Programmzeile wie heute, nur mit `s`.

**Serie 8 — Der Browser:**

6. Fenster-Syscalls nach Entwurf (d): Handle, geteilter Puffer,
   Ereignis-Warteschlange.
7. Der Browser als Prozess: HTML-Teilmenge, Layout, Rendern — der grosse,
   *unkritische* Teil, bei dem Eigenbau genau richtig ist.

---

## Die Grenze, die bleibt

JavaScript. Eine Engine ist eine Grössenordnung mehr Arbeit als alles
bisher Gebaute zusammen, und ein JIT wäre nach W^X ein Widerspruch in sich
(er bräuchte schreib- **und** ausführbaren Speicher — genau das, was
`elf::pruefen` heute ablehnt). Ein Browser für statische Seiten ist ein
ehrliches, erreichbares Ziel. Das soll von Anfang an so heissen.
