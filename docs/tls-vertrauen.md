# Vertrauensanker für TLS — woher SpeedOS weiß, wem es glaubt

Stand: Juli 2026, Serie 7, Teil 2. Entstanden **vor** dem Code, wie
`docs/zufall.md` und `docs/speedfs-format.md`.

---

## 0. Worum es geht — und warum das der heikelste Teil von TLS ist

TLS besteht aus zwei Hälften, und nur eine davon ist Kryptographie:

1. **Verschlüsseln**, damit niemand mitliest.
2. **Prüfen, mit wem man überhaupt spricht.**

Die erste Hälfte übernimmt `rustls` mit einem RustCrypto-Provider
(`docs/serie7-bestandsaufnahme.md` §a). Die zweite hängt an einer Frage, die
keine Bibliothek für uns beantworten kann: **Welchen Zertifizierungsstellen
vertraut dieses System?** Genau das ist der Wurzelspeicher.

Ohne ihn ist TLS nicht „etwas schwächer": Verschlüsselung ohne Prüfung
schützt gegen Mitlesen, aber **nicht gegen einen Mitspieler in der Mitte** —
und der kann alles lesen und alles ändern. Es ist die Hälfte, auf die es
ankommt.

**Die Konsequenz für dieses Dokument:** Ein Wurzelspeicher, der unbemerkt
entsteht, ist wertlos. Deshalb wird hier festgehalten, **woher** die Datei
kommt, **wann** sie geholt wurde und **was sie nicht leistet**.

---

## 1. Woher kommt das CA-Bündel?

### Die Kandidaten

| Quelle | Wie sie kommt | Warum (nicht) |
|---|---|---|
| **`webpki-roots`** (Crate) | Mozillas Speicher als Rust-Datenmodul | Ideal für ein Rust-TLS — aber es ist ein *Crate*, das der TLS-Bibliothek gehört, kein Systembestand. SpeedOS soll den Anker BESITZEN, nicht jedes Programm einen eigenen mitbringen. |
| **`cacert.pem` von curl.se** | Kuratierter Mozilla-Export, PEM, täglich gebaut, mit Datum und SHA-256 veröffentlicht | **Gewählt.** Eine Datei, ein Format, nachprüfbare Herkunft, unabhängig von der TLS-Bibliothek. |
| **System-Speicher des Host-Betriebssystems** | aus Windows/Linux kopiert | Nicht reproduzierbar (jede Maschine ein anderer Stand) und schwer zu dokumentieren. |
| **Selbst kuratieren** | von Hand ausgewählte CAs | Ein Kuratierungs-Projekt für sich; die Auswahlkriterien wären dieselben, die Mozilla schon anwendet — nur schlechter geprüft. |

### Die Entscheidung

> **`https://curl.se/ca/cacert.pem`** — der von curl gepflegte Export von
> Mozillas NSS-Wurzelspeicher, als eine PEM-Datei.
>
> Sie wird mit `tools/ca_bundle_holen.ps1` geholt, landet als
> `assets/ca-bundle.pem` im Repository, wird vom `build.rs` ins Kernel-Image
> eingebettet und beim Boot nach **`/platte/system/ca-bundle.pem`**
> geschrieben — genau der Weg, den die User-Programme seit Serie 6 nehmen
> (`src/programme.rs`).

**Warum derselbe Weg wie bei den Programmen:** Ein Host-Werkzeug, das SpeedFS
beschreiben kann, gibt es nicht (und es zu bauen wäre eine dauerhafte
Doppelpflege unseres Formats). Eingebettet reist das Bündel mit `cargo run`,
`cargo test` **und** `cargo image` — auch auf den USB-Stick, ohne eine Zeile
Extra-Logik im Runner. Der Preis sind ~230 KiB im Image.

**Warum eine Datei auf `/platte` und nicht nur im Image:** Weil ein
Vertrauensanker sichtbar und ersetzbar sein soll. Ein Anker, den man nur
durch einen Neubau des Kernels ändern kann, lädt dazu ein, ihn *nie* zu
ändern. Die Datei kann angesehen, gezählt und ausgetauscht werden — und
`zertifikate` zeigt, was tatsächlich geladen ist.

### Die Herkunfts-Notiz

Der Holer schreibt neben die Datei ein **`assets/ca-bundle.herkunft.txt`**
mit URL, Abrufdatum, Byte-Zahl und SHA-256. Das ist der Unterschied zwischen
„da liegt eine Datei" und „wir wissen, was das ist". Wer das Bündel
aktualisiert, aktualisiert diese Notiz mit — und der Unterschied ist im
`git diff` sichtbar.

### Wenn die Datei fehlt

Der Bau **läuft trotzdem durch**, mit einem leeren Bündel und einer
deutlichen Meldung. Das ist Absicht: Ein Build-Skript, das im Hintergrund
Wurzelzertifikate aus dem Netz zieht, ist genau das, wogegen ein
Vertrauensanker schützen soll. Das Holen ist ein **bewusster, einmaliger,
dokumentierter Schritt** — kein Nebeneffekt von `cargo build`.

---

## 2. Wie wird es aktualisiert?

> **Von Hand. `tools/ca_bundle_holen.ps1` erneut ausführen, das Ergebnis
> ansehen, committen.** Es gibt keine automatische Aktualisierung.

Das ist ehrlich und nicht ideal, deshalb steht hier beides:

**Was daran schlecht ist.** CA-Speicher ändern sich — Stellen werden
aufgenommen, andere nach Vorfällen ausgeschlossen (DigiNotar 2011, Symantec
2018, entrust 2024). Ein Bündel, das ein Jahr alt ist, vertraut womöglich
einer Stelle, der die Welt längst nicht mehr vertraut. **Das ist die
gefährlichere Richtung**: Ein zu altes Bündel lehnt nicht etwa zu viel ab, es
vertraut zu viel.

**Warum trotzdem von Hand.** Eine automatische Aktualisierung bräuchte einen
gesicherten Abruf — also TLS, also einen Wurzelspeicher. Das ist ein
handfestes Henne-Ei-Problem: Der einzige saubere Ausweg ist ein
**eingebauter Signaturschlüssel**, mit dem SpeedOS eine signierte
Bündel-Aktualisierung prüfen kann. Das ist ein eigenes Vorhaben (Schlüssel
erzeugen, verwahren, ein Format, Rollback-Schutz) und gehört nicht in denselben
Schritt wie „TLS zum ersten Mal ans Laufen bringen".

**Bis dahin gilt:** Das Abrufdatum steht in `ca-bundle.herkunft.txt`, und
`zertifikate` zeigt es an. Wer ein altes Datum sieht, weiß, was zu tun ist.

---

## 3. Was SpeedOS ausdrücklich NICHT tut

Diese Liste steht hier, damit niemand später glaubt, es sei vergessen worden.

### (a) Keine Sperrlisten-Prüfung — weder OCSP noch CRL

**Was fehlt:** Ein Zertifikat kann vor seinem Ablaufdatum für ungültig erklärt
werden (gestohlener Schlüssel, Fehlausstellung). Dafür gibt es OCSP
(Online-Abfrage beim Aussteller) und CRLs (Sperrlisten zum Herunterladen).
SpeedOS prüft **weder das eine noch das andere**.

**Was das bedeutet:** Ein gestohlenes, aber noch nicht abgelaufenes
Zertifikat wird von SpeedOS akzeptiert. Bei Laufzeiten von heute meist
90 Tagen ist das ein Fenster von bis zu drei Monaten.

**Warum trotzdem nicht:** Nicht aus Bequemlichkeit, sondern weil die
naheliegende Umsetzung **schlechter wäre als gar keine**:

* Eine OCSP-Abfrage verrät dem Aussteller, welche Seite wann besucht wurde —
  ein Datenschutz-Rückschritt, den moderne Browser deshalb weitgehend
  abgeschafft haben.
* Und sie ist in der Praxis **weich**: Antwortet der Responder nicht (Netz
  weg, Server aus), verbinden fast alle Umsetzungen trotzdem („soft fail").
  Ein Angreifer, der ohnehin in der Leitung sitzt, blockiert einfach die
  Abfrage. Der Schutz ist dann eine Anzeige, kein Mechanismus.
* Ein **hartes** Scheitern wäre ehrlich, macht aber jeden Netzausfall zu
  einem TLS-Ausfall.

**Der richtige Weg wäre OCSP-Stapling** (der Server liefert eine frische,
signierte Bestätigung im Handshake mit — kein Extra-Abruf, kein
Datenschutzproblem). Das ist der Kandidat, wenn TLS steht; es setzt voraus,
dass die TLS-Schicht die Erweiterung durchreicht.

> **Bis dahin ist die Sperrprüfung eine bekannte, benannte Lücke von SpeedOS
> — kein Versehen.**

### (b) Kein Certificate Transparency

Ob ein Zertifikat in öffentlichen CT-Logs steht, wird nicht geprüft. Das
würde Fehlausstellungen aufdecken. Braucht Log-Listen, deren Signaturschlüssel
und eine Aktualisierungs-Strategie — dieselbe Henne-Ei-Frage wie in §2.

### (c) Kein Pinning, keine Ausnahmen, keine Benutzer-CAs

Es gibt genau **einen** Vertrauensanker: die Bündel-Datei. Kein
„einmal trotzdem verbinden", kein Klick-durch-Dialog bei ungültigem
Zertifikat. Das ist eine bewusste Härte: Der Dialog „Diese Verbindung ist
nicht sicher — trotzdem fortfahren?" ist die meistgeklickte
Sicherheitsfrage der Computergeschichte, und sie wird fast immer falsch
beantwortet.

### (d) Keine Namens-Einschränkungen jenseits dessen, was webpki prüft

Pfadbildung, Signaturen, Namensabgleich (SAN, Wildcards) und
Gültigkeitszeiträume macht `rustls-webpki`. Wir prüfen nichts davon selbst
nach — es selbst zu schreiben wäre wieder Eigenbau-Krypto (dieselbe Absage
wie überall).

### (e) Die Uhr ist eine Voraussetzung, kein Detail

Gültigkeitszeiträume sind in **UTC**. Eine falsche Uhr macht die Prüfung
entweder grundlos streng (alles abgelaufen) oder — schlimmer — zu lax
(Abgelaufenes gilt noch). Deshalb hängt an diesem Dokument die andere Hälfte
von Serie 7, Teil 2:

* `zeit::jetzt()` liefert seit jetzt **immer UTC**, die Anzeige-Zone ist
  reine Kosmetik (`src/zeit.rs`, Regel oben in der Datei).
* Beim Boot läuft ein **Plausibilitäts-Check** gegen das Bau-Datum des
  Kernels. Schlägt er fehl, liefert der Syscall `zeit_geprueft` (13) den
  Fehler `ZeitUnplausibel` (26), **und die Zertifikatsprüfung findet nicht
  statt**.
* Die Versuchung, „die Uhr stimmt nicht, prüfen wir die Gültigkeit halt
  nicht" zu tun, ist genau der Punkt, an dem TLS aufhört, etwas wert zu
  sein. Diesen Weg gibt es nicht.

---

## 4. Das Format und der Parser

### PEM: was wir lesen — und was bewusst nicht

Eine `.pem`-Datei ist eine Folge von Base64-Blöcken zwischen Markierungen:

```text
Kommentarzeilen, Namen, Datumsangaben — alles ausserhalb wird IGNORIERT
-----BEGIN CERTIFICATE-----
MIIDdzCCAl+gAwIBAgIEAgAAuTANBgkqhkiG9w0BAQUFADBaMQswCQYDVQQGEwJJ
...
-----END CERTIFICATE-----
```

Der Parser (`userland/src/pem.rs`) macht **genau das und nichts weiter**:

* Er sucht `-----BEGIN CERTIFICATE-----`, sammelt bis
  `-----END CERTIFICATE-----`, dekodiert Base64 zu DER.
* Alles ausserhalb der Blöcke ist Kommentar. Andere Block-Typen
  (`BEGIN PRIVATE KEY` …) werden **übersprungen**, nicht abgelehnt — in
  einem CA-Bündel haben sie nichts zu suchen, aber sie sind auch kein Grund,
  die restlichen 140 Zertifikate wegzuwerfen.
* Ein **kaputter Block macht nur diesen Block ungültig**, nicht die Datei.
  Das ist die wichtigste Entscheidung am Parser: Ein Vertrauensanker mit
  145 von 146 lesbaren Wurzeln ist brauchbar; einer, der bei einem
  Zeilenumbruch-Fehler auf 0 fällt, ist eine Ausfallquelle.
* **Er panickt nie** und hat feste Obergrenzen (Blockzahl, Blockgröße).
  Er verarbeitet eine Datei, die von aussen kommt — dieselbe Haltung wie
  beim ELF-Lader: *jede Zahl in der Datei ist die Behauptung eines Fremden*.

### Warum im USER-SPACE

Weil er krypto-nah ist und aus einer Datei liest, die ein Angreifer ersetzt
haben könnte. Ein Parser-Fehler soll einen **Prozess** treffen, nicht den
Kernel — genau dafür gibt es seit Serie 6 Ring 3. Der Kernel kennt die Datei
nur als Bytes; er schreibt sie beim Boot hin und liest sie nie.

### Die Minimal-Zerlegung von X.509

`zertifikate` zeigt Subject-Namen und Gültigkeitszeiträume. Dafür braucht es
etwas mehr als PEM: einen **DER-Läufer**, der die Tag-Länge-Wert-Struktur
abgeht und genau drei Dinge herausholt — den Common Name des Subjects und die
beiden Zeitangaben aus `Validity`.

Das ist ausdrücklich **kein X.509-Parser**: Es wird nichts geprüft, nichts
validiert und nichts geglaubt. Es ist eine **Anzeige-Hilfe**, damit ein
Mensch sieht, was da liegt. Die echte Zerlegung macht später `rustls-webpki`.
Steht die Struktur nicht wie erwartet, wird das Feld als „unbekannt"
angezeigt — nie geraten, nie gepanickt.

---

## 5. Bewusst offen

* **Signierte Bündel-Aktualisierung** (§2) — der Ausweg aus dem Henne-Ei.
* **OCSP-Stapling** (§3a) — der richtige Weg zur Sperrprüfung.
* **NTP/SNTP** — die Uhr aus einer unabhängigen Quelle statt aus einer
  Batterie. Wir haben UDP, DNS und einen funktionierenden Stack; ein
  SNTP-Client sind wenige hundert Zeilen. Er würde §3e von „Plausibilitäts-
  Filter" auf „echte Zeitquelle" heben und ist der nächste sinnvolle Schritt.
* **Ein eigener Vertrauensanker für SpeedOS-Updates** — sobald es Updates
  gibt.
