# TLS in SpeedOS — Machbarkeit, Auswahl, Spike

Stand: Juli 2026, Serie 7, Teil 3.

> **ERGEBNIS VORWEG: Es geht.** `rustls` (no_std) mit dem RustCrypto-Anbieter
> übersetzt für `x86_64-unknown-none`, läuft in Ring 3 und legt einen
> Client-Zustand samt 119 Wurzelzertifikaten an. Der Spike
> (`userland/tlsspike`) beendet sich mit Code 0.
>
> **Der Preis: vier `--cfg`-Flaggen und ein Mindest-Optimierungsgrad.** Ohne
> sie bricht LLVM ab — und zwar bei *jeder* der geprüften Bibliotheken, weil
> die Ursache nicht in der TLS-Schicht liegt, sondern in den
> RustCrypto-Primitiven darunter. §3 erklärt es.
>
> **Was NICHT passiert ist: ein Handshake.** Der Spike spricht kein TLS. Das
> ist der nächste Schritt und ausdrücklich nicht dieser.

Alle Angaben unten sind **gemessen**, nicht erinnert: erhoben mit
`cargo info`, Probe-Crates gegen unser echtes Target und `llvm-size` auf den
Ergebnissen. Toolchain: `rustc 1.99.0-nightly (2026-07-06)`.

---

## 1. Die Kandidaten, wie sie heute aussehen

### rustls 0.23.42 (stabile Linie; 0.24 ist als `0.24.0-dev.1` unterwegs)

```
features: default = [aws_lc_rs, logging, prefer-post-quantum, std, tls12]
          std, custom-provider, ring, aws_lc_rs, hashbrown, zlib, brotli, fips
rust-version: 1.71
```

* **no_std:** ja, über `default-features = false`. Der Schlüssel ist
  `custom-provider` — er schaltet die eingebauten Anbieter (`aws-lc-rs`,
  `ring`, beide mit C-Code) ab und erwartet einen eigenen.
* **Abhängigkeiten ohne `std`:** nur `rustls-pki-types`, `rustls-webpki`,
  `zeroize`, `subtle`, `untrusted`, `once_cell` — **alle** übersetzen
  anstandslos gegen `x86_64-unknown-none` (gemessen: `cargo build`
  fehlerfrei, 25 Pakete).
* **TLS 1.3:** ja. TLS 1.2 optional über `tls12`.
* **Zertifikatsprüfung:** `rustls-webpki` — Pfadbildung, Signaturen,
  Namensabgleich, Gültigkeitszeiträume.

**Was es von der Plattform verlangt** (das ist die eigentliche Frage dieses
Abschnitts, und die Antwort war teils überraschend):

| Anforderung | Wie rustls sie stellt | Bei uns |
|---|---|---|
| Allocator | `alloc` zwingend | **musste gebaut werden** — §4 |
| Zufall | über den `CryptoProvider` (`secure_random`) | Syscall 12 |
| **Zeit** | **explizit: `Arc<dyn TimeProvider>`** | Syscall 13 |
| Transport | keiner — rustls kennt nur Bytes | `libspeed::tls::TcpStrom` |

> **Der Fund, der die API-Wahl bestimmt:** In `no_std` sind
> `ClientConfig::builder()` **und** `builder_with_provider()`
> `#[cfg(feature = "std")]`. Nutzbar ist nur
> **`builder_with_details(provider, time_provider)`** — und die verlangt den
> Zeitgeber als Argument. Ebenso ist `ClientConnection` (die gepufferte
> `std::io`-Variante) nicht verfügbar; in `no_std` gibt es
> **`UnbufferedClientConnection`**.
>
> Das ist keine Kleinigkeit: Der ungepufferte Weg ist ein anderes
> Programmiermodell. Man treibt die Zustandsmaschine selbst voran und
> verwaltet die Puffer selbst, statt `read`/`write` auf einem Stream
> aufzurufen. Für den Handshake-Schritt ist das die eigentliche Arbeit.

### rustls-rustcrypto 0.0.2-alpha (der Krypto-Anbieter)

```
features: default = [std, tls12, zeroize];  alloc, std, tls12, zeroize, logging
rust-version: 1.75
```

* Reines Rust, kein C. Mit `default-features = false, features = ["alloc"]`
  no_std-tauglich.
* **Es ist ALPHA**, und die Versionsnummer meint es ernst: `0.0.2-alpha`,
  von der RustCrypto-Organisation, aber nicht als produktionsreif erklärt.
  Das ist die grösste offene Flanke dieser Wahl und gehört benannt.
* Es zieht **RSA, P-256, P-384, x25519, ed25519, AES-GCM, ChaCha20Poly1305,
  SHA-2** — daher die 201 Zeilen im Abhängigkeitsbaum.
* Gemessen liefert es **3 Ciphersuites** an rustls.

### embedded-tls 0.19.0

```
"TLS 1.3 client with no_std support and no allocator"
features: default = [std, log, tokio];  alloc, webpki, rustpki, p384, rsa, ed25519
```

* Kleiner, no_std von Haus aus, `embedded-io-async` als Transport-Trait.
* **Baut gegen unser Target** — mit **genau denselben** Flaggen wie rustls
  (§3). Es hat also nicht den geringeren Aufwand, nur den kleineren Umfang.
* **Der Ausschlussgrund bleibt der aus der Bestandsaufnahme:** Die
  Zertifikatsprüfung ist historisch schwach bis abschaltbar. Ein TLS ohne
  Kettenprüfung schützt gegen Mitlesen, aber nicht gegen einen Mitspieler in
  der Mitte — und das ist die Hälfte, auf die es ankommt.

### portable-rustls 0.0.2

Ein Fork von rustls, ausdrücklich auf Portabilität gemünzt
(`custom-provider`, `hashbrown`, kein `std` im Default). Interessant als
Rückfallebene, aber: ein **Fork** eines Sicherheitsprojekts bedeutet, dass
Sicherheitsaktualisierungen des Originals nicht automatisch ankommen. Nicht
gewählt, notiert.

### Nicht weiter verfolgt

* **`ring` / `aws-lc-rs`** — C-Code und Assembler, `std`. Bricht mit „from
  scratch in Rust" und zwingt eine C-Toolchain in den Bau.
* **mbedTLS/BearSSL über FFI** — dasselbe Argument, plus FFI.

---

## 2. Die Entscheidung

> **`rustls` 0.23 (no_std + alloc, `custom-provider`) mit
> `rustls-rustcrypto` als Anbieter.**

Begründung in einem Satz: Es ist die einzige Kombination, die **echte
Kettenprüfung** (`rustls-webpki`) mitbringt und gegen unser Target baut.
`embedded-tls` wäre kleiner, aber genau an der Stelle schwächer, an der TLS
seinen Wert hat.

**Die Alpha-Warnung bleibt stehen:** Der Anbieter ist Version 0.0.2-alpha.
Für ein Lernsystem ist das vertretbar — für „SpeedOS surft sicher im
Internet" wäre es das nicht, und dieser Satz steht hier, damit er später
nicht vergessen wird.

---

## 3. Der Blocker — und warum er nicht bei TLS liegt

### Was passiert ist

Der erste Bauversuch (rustls + rustls-rustcrypto, blankes
`x86_64-unknown-none`) endete so:

```
rustc-LLVM ERROR: Do not know how to split the result of this operator!
error: could not compile `polyval`
error: could not compile `sha2`
error: could not compile `poly1305`
error: could not compile `aes`
```

Und **`embedded-tls` scheitert an exakt derselben Stelle** — dieselben vier
Kisten. Das war der Hinweis: Die Ursache liegt nicht in der TLS-Schicht.

### Die Ursache, nachgesehen statt vermutet

`aes 0.8.4/src/lib.rs`:

```rust
} else if #[cfg(all(any(target_arch = "x86", target_arch = "x86_64"),
                    not(aes_force_soft)))] {
    mod autodetect;
    mod ni;          // <- AES-NI, benutzt __m128i
    pub use autodetect::*;
}
```

Auf x86_64 wird der **AES-NI-Zweig immer mitübersetzt**; ausgewählt wird
erst zur *Laufzeit* (`cpufeatures`). Unser Target hat SSE aber
**abgeschaltet** (`-sse`, `+soft-float` — nötig, weil unser Kontext-Wechsel
nur die 15 General-Register sichert und keine XMM). LLVM kann `__m128i` dann
nicht legalisieren und bricht ab.

Zwei Gegenproben, beide gemacht:

* **`u128` allein baut** auf dem blanken Target — es liegt also nicht an
  128-Bit-Arithmetik, sondern wirklich an Vektortypen.
* **Mit `+sse,+sse2,-soft-float` baut der gesamte Stapel** — was die
  Diagnose bestätigt und zugleich einen Weg zeigt, den wir **nicht** gehen
  (er verlangte, dass der Kernel bei jedem Wechsel FXSAVE/FXRSTOR macht;
  eigenes Vorhaben, siehe §6).

### Der Ausweg, der genommen wurde

Jede betroffene Kiste hat einen Schalter für den reinen Software-Zweig — als
**cfg-Flagge, nicht als Cargo-Feature** (deshalb stehen sie in
`userland/.cargo/config.toml` und nicht in `Cargo.toml`):

```toml
rustflags = [
    "--cfg", "aes_force_soft",
    "--cfg", "polyval_force_soft",
    "--cfg", "poly1305_force_soft",
    "--cfg", "curve25519_dalek_backend=\"serial\"",
]
```

Die vierte kam später und mit anderer Mechanik: `curve25519-dalek` wählt
seinen Backend im **Build-Skript** und nimmt auf nightly + x86_64
automatisch `simd`. Der Schalter dagegen ist eine cfg-Variable, die sein
`build.rs` über `CARGO_CFG_CURVE25519_DALEK_BACKEND` abfragt.

### Die fünfte Bedingung: `opt-level ≥ 1`

`sha2` hat **keinen** `force_soft`-Schalter — und braucht auch keinen,
solange optimiert wird. Gemessen:

| opt-level | `sha2` baut? |
|---|---|
| 0 | **nein** (LLVM-Abbruch) |
| 1 | ja |
| 2 | ja |
| "s" | ja |

Erklärung: Bei `-O0` überlebt der tote SHA-NI-Zweig bis zur
Legalisierung; mit Optimierung wird er vorher entfernt.

**Unser Bau erfüllt das** — `build.rs` baut userland mit `--release`
(`opt-level = "s"`). Es ist trotzdem eine **fragile Bedingung**, und sie
steht deshalb hier: Ein Debug-Build von `userland/` bricht ab.

---

## 4. Was libspeed dafür bekommen hat

### (a) Ein Heap — `SYS_SPEICHER` (14) + `libspeed::heap`

Bis hierher war `libspeed` **allokationsfrei**. `rustls` verlangt `alloc`
ohne Alternative, also gibt es jetzt einen Heap:

* **Kernel:** `SYS_SPEICHER(bytes)` mappt Seiten *hinter* dem bisherigen
  Heap-Ende des Prozesses und liefert die Basis. Er liegt in der 16-MiB-Lücke
  zwischen Programm-Image und Stack: `HEAP_START = IMAGE_ENDE + 4 KiB`,
  Obergrenze **12 MiB**, danach bleiben **3 MiB ungemappter Abstand** zum
  Stack. NX-Seiten (W^X gilt weiter).
* **Kein `frei`-Gegenstück**, und das ist Absicht: Ein Prozess gibt Seiten
  nie einzeln zurück, sein Adressraum fällt beim Ende als Ganzes (Serie 6,
  Teil 3). Dasselbe Modell wie `brk` unter Unix.
* **User-Space:** `linked_list_allocator` als `#[global_allocator]`, der bei
  Bedarf nachfordert (mindestens 64 KiB je Anforderung — einzelne Seiten
  wären ein Syscall je 4 KiB). Weil der Kernel immer *anschliessend* mappt,
  reicht ein einziger zusammenhängender Heap und `extend`.
* Geht der Speicher aus, meldet der `alloc_error_handler` es auf den
  Diagnose-Kanal und beendet den Prozess mit Code 102 — kein stiller
  Nullzeiger.

### (b) Zufall, Zeit, Transport — `libspeed::tls`

* **Zufall:** `zufall_fuellen` → Syscall 12, registriert als
  `getrandom`-Backend (`custom`-Feature; `getrandom` kennt unser Target
  nicht und kann es nicht kennen). Liefert nie schwache Bytes: Ist der Pool
  ungesät, kommt ein Fehler und der Handshake bricht ab.
* **Zeit:** `SpeedUhr: TimeProvider` → Syscall 13 (`zeit_geprueft`). **Bei
  unplausibler Uhr `None`** — rustls lehnt die Gültigkeitsprüfung dann ab,
  statt sie zu überspringen. Die Versuchung „Uhr kaputt, prüfen wir halt
  nicht" ist damit nicht implementierbar.
* **Transport:** `TcpStrom` über die Socket-Syscalls. Er glättet **die eine
  Stelle, an der unsere ABI nicht passt**: `empfange` ist laut ABI
  nicht-blockierend (0 = „noch nichts"), eine TLS-Bibliothek erwartet aber
  einen blockierenden Strom. `TcpStrom::lesen` schleift deshalb mit
  `abgeben()` (nicht `schlafe` — wir warten auf den Netz-Task, der soll
  laufen) und unterscheidet sauber zwischen „noch nichts", „Gegenstelle hat
  geschlossen" (Dateiende) und „Frist abgelaufen".

---

## 5. Der Spike: Zahlen

`starte /platte/programme/tlsspike example.com`, echt in Ring 3:

```
1) Heap:   16 Byte belegt von 65536 Byte gemappt
2) Zufall: 32 Byte geholt, beginnt mit 32 a4 46 8b
3) Zeit:   1785344504 (UNIX-Sekunden, UTC, geprueft)
4) Wurzeln: 119 aus dem Buendel gelesen, 119 von rustls uebernommen
5) Krypto:  3 Ciphersuites vom RustCrypto-Anbieter
6) Konfig:  ClientConfig steht
7) Zustand: ClientConnection fuer 'example.com' angelegt
   Heap-SPITZE:  66944 Byte
```

| Grösse | Wert |
|---|---|
| ELF gesamt | **830 224 Byte** |
| `.text` | **581 053 Byte** (~567 KiB) |
| `.rodata` | 46 176 Byte |
| `.bss` | 540 736 Byte (davon 528 KiB eigene Lesepuffer, nicht rustls) |
| **Heap-Spitze** | **66 944 Byte** (~65 KiB) für Config + Connection |
| Heap gemappt | 131 072 Byte (2 × 64 KiB) |
| Abhängigkeiten | 201 Zeilen im `cargo tree` |
| Zum Vergleich: `zertifikate` (ohne TLS) | 28 312 Byte, `.text` 10 543 |

**Einordnung:** Das Programm ist **~30× so gross** wie unser bisher grösstes.
Es passt trotzdem locker — die Obergrenze für ein Programm-Image ist 16 MiB
(`elf::IMAGE_ENDE`), und der Heap-Bedarf liegt bei einem halben Prozent der
12-MiB-Grenze. Die Heap-Spitze ohne Handshake ist 65 KiB; ein echter
Handshake mit Kettenprüfung wird mehr brauchen, aber nicht Grössenordnungen.

**Kein Leck:** drei Spike-Läufe hintereinander, Frame-Bilanz **0**.

---

## 6. Was gehakt hat — der ehrliche Teil

1. **Die vier cfg-Flaggen** (§3). Der Fehler `Do not know how to split the
   result of this operator!` nennt weder Kiste noch Ursache; es hat vier
   Probe-Builds gebraucht, um von „TLS geht nicht" auf „`aes` übersetzt
   seinen AES-NI-Zweig immer mit" zu kommen. Der entscheidende Hinweis war,
   dass **beide** TLS-Bibliotheken an denselben vier Kisten scheitern.
2. **`opt-level = 0` bricht `sha2`.** Unser Bau ist `--release`, also
   unauffällig — aber es ist eine Bedingung, die niemand erwartet.
3. **`ClientConnection` gibt es in no_std nicht.** Ohne den Blick in die
   Quellen wäre erst beim Handshake aufgefallen, dass das Programmiermodell
   ein anderes ist (`UnbufferedClientConnection`).
4. **Ein Deadlock im eigenen Allocator.** Die erste Fassung schrieb den
   Heap-Höchststand so fort:
   ```rust
   if let Ok(z) = self.innen.lock().allocate_first_fit(layout) {
       hoechststand_merken(&self.innen);   // lockt ERNEUT
   ```
   Der `MutexGuard` aus einer `if let`-Bedingung lebt **bis zum Ende des
   Blocks**, nicht bis zum Ende der Bedingung. Ein Spinlock, der auf sich
   selbst wartet, dreht sich für immer — der Spike blieb nach der ersten
   Zeile stehen. Steht als Warnung im Code.
5. **Die erste Messung war die falsche Zahl.** Der Spike meldete „16 Byte
   Heap" — den *Endstand*, nachdem rustls alles freigegeben hatte. Wer
   wissen will, wie viel ein Programm *braucht*, muss die **Spitze** messen.

**Was NICHT abgeschaltet werden musste:** `tls12` ist an, die
Zertifikatsprüfung ist vollständig (119/119 Wurzeln von `rustls-webpki`
akzeptiert), und es wurde keine Sicherheitsfunktion geopfert, um den Bau
hinzubekommen. Die Flaggen wählen nur Software- statt SIMD-Implementierungen
derselben Algorithmen.

---

## 7. Was als Nächstes ansteht

* **Der Handshake.** Der Spike beweist, dass rustls *läuft* — nicht, dass es
  *spricht*. Mit `UnbufferedClientConnection` heisst das: die
  Zustandsmaschine selbst treiben und die Puffer selbst verwalten. Das ist
  der eigentliche nächste Schritt, und er ist grösser als dieser hier.
* **SSE im User-Space** (optional). Mit `+sse,+sse2,-soft-float` baut alles
  ohne die cfg-Flaggen, und die Krypto würde deutlich schneller. Der Preis
  ist ein Kernel-Vorhaben: **FXSAVE/FXRSTOR je Prozess im Kontext-Wechsel**
  (heute sichert er nur die 15 GP-Register, gemessen in `tests/scheduler.rs`).
  Ohne das würden zwei SSE-nutzende Prozesse einander die XMM-Register
  zerschreiben — still. Erst messen, ob die Software-Krypto zu langsam ist.
* **Den Anbieter im Auge behalten.** `rustls-rustcrypto` ist Alpha. Wird es
  stabil oder wird es aufgegeben, ändert das die Bewertung aus §2.
* **`getrandom` 0.3.** Wir hängen an 0.2 (über `rand_core` 0.6). Die
  Registrierung des Backends funktioniert dort anders (`--cfg
  getrandom_backend="custom"`); beim nächsten Versionssprung der
  RustCrypto-Kisten fällt das an.
