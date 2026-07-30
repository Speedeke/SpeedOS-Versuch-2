# SpeedOS — Projektregeln für Claude

## Projekt
- SpeedOS: ein eigenes Betriebssystem from scratch in Rust. Kein Linux, keine fremde Kernel-Basis.
- Sprache: Rust **nightly**, `no_std`, Ziel-Architektur: **x86_64**.
- Bootloader: **bootloader_api 0.11** (UEFI-Boot mit linearem Framebuffer).
  Ursprünglich nach "Writing an OS in Rust" von Philipp Oppermann (bootloader 0.9)
  gebaut, im Juli 2026 migriert — Plan und Details in `docs/migration-011.md`.

## Build & Test-Umgebung
- Kernel-Target: das EINGEBAUTE `x86_64-unknown-none` (kein eigenes Target-JSON,
  kein build-std — rust-toolchain.toml installiert das Target automatisch).
- **Performance-Setup (Juli 2026, gegen Maus-/Desktop-Lag):** (1) Der
  Kernel baut auch im dev-Profil mit `opt-level = 2` (Cargo.toml) —
  unoptimiert braucht ein Compositor-Frame hunderte ms. (2) QEMU läuft
  mit `-accel whpx,kernel-irqchip=off -accel tcg` (Hardware-
  Virtualisierung, TCG nur als Fallback). (3) Auflösung standardmäßig
  klein (720p-Klasse), wählbar per SPEEDOS_AUFLOESUNG. (4) PIT auf
  250 Hz, (5) Maus-Abtastrate nach der IntelliMouse-Sequenz auf 200/s.
- **Auflösungswahl (Juli 2026):** SPEEDOS_AUFLOESUNG (720p Standard,
  1080p, 2k, 4k, ... oder BREITExHOEHE) — Logik im boot/-Runner.
  Mechanik: Der Bootloader nimmt den GRÖSSTEN GOP-Modus, der seine
  Minimums erfüllt (.last()), und die Firmware bietet nur Modi an, die
  ins VRAM passen — also wird vgamem_mb (Zweierpotenz!) gerade so groß
  gewählt, dass der Wunschmodus der größte verfügbare ist; der
  EDID-Wunsch allein wird von OVMF ignoriert. RAM (-m) skaliert mit
  (~20 B/Pixel + 96 MiB, max 1 GiB = Bitmap-Allocator-Grenze).
  Firmware-Obergrenze: 4096x2160 (5120x2880 fehlt in der edk2-Tabelle,
  8K/128-MiB-VRAM hängt die Firmware auf) — größere Wünsche werden mit
  Meldung gedeckelt. Der Kernel selbst ist auflösungsunabhängig.
- `cargo run`/`cargo test` rufen als Runner das Host-Programm **boot/** auf:
  Es baut per `bootloader::UefiBoot` das GPT-Disk-Image und startet
  **QEMU** (`qemu-system-x86_64`) mit der edk2/OVMF-Firmware aus dem
  QEMU-Installationsordner.
- Tests: Integrationstests laufen in QEMU mit dem **isa-debug-exit Device**;
  der Runner übersetzt Exit-Code 33 -> Erfolg (Timeout 300 s).

## Debug
- **ALLE** Ausgaben laufen doppelt: FramebufferKonsole (Bildschirm) UND
  serielle Schnittstelle (COM1 / Port 0x3F8, mit ANSI-Farben) — die Regel
  steckt in `konsole::_print`, nicht beim Aufrufer. Niemals nur Bildschirm.
  `serial_println!` nur für reine Debug-Ausgaben.

## Git
- Nach **JEDEM** funktionierenden Schritt ein Commit mit klarer Message.
- **NIEMALS** committen, wenn `cargo build` fehlschlägt oder QEMU nicht bootet.

## Arbeitsweise
- Kleine Schritte. Nach jeder Änderung selbst bauen, in QEMU starten, serielle Ausgabe prüfen.
- Fehler selbst debuggen und fixen, bevor "fertig" gemeldet wird.
- Der Projektbesitzer ist Anfänger in OS-Entwicklung: nach jedem Schritt in 2–3 Sätzen
  auf Deutsch erklären, was gebaut wurde.

## Code-Stil
- Ausführliche **deutsche Kommentare**, da der Projektbesitzer OS-Entwicklung lernen will.
- Jede Datei beginnt mit einem Kommentarblock, der erklärt, was sie tut.

## Architektur-Prinzip
- Mikrokernel-inspiriert: Treiber und Systemdienste so isoliert wie möglich
  (eigene Module, klare Schnittstellen, so wenig geteilter Zustand wie möglich).

## Daten-Integritäts-Regel (Juli 2026)
- Dateisystem- und Geräte-Fehler werden NIE verschluckt: keine Panik, kein
  stilles `let _ =`, kein leerer Fallback auf dem Nutzer-Pfad. Jede Operation
  liefert `Result<_, FsFehler>` (Geräte-Schicht: `Result<_, IoFehler>`), und
  die Oberfläche ZEIGT den Fehler an — Shell rot via fs_fehler_ausgeben,
  Explorer in der Statusleiste, SpeedText als Fehler-Dialog
  (`ui::dialog::fehler`, dünner Mantel um bestaetigung()).

## User-Space-Dauerregeln (Serie 6, ab Juli 2026 — gelten AB SOFORT überall)
- **(I) DER KERNEL FOLGT NIEMALS BLIND EINEM USER-ZEIGER.** Jeder Zeiger, den
  Ring-3-Code übergibt (Syscall-Argument), wird VOR der Benutzung GEPRÜFT und
  die Daten werden KOPIERT — nie direkt dereferenziert. `ring3::copy_in(ptr,
  laenge)` und `ring3::copy_out(ptr, daten)` sind die EINZIGEN Wege; beide
  laufen über `ring3::user_bereich_pruefen(ptr, laenge, schreiben)`, das
  DREISTUFIG prüft: **(a)** liegt [ptr, ptr+len) vollständig im User-Bereich
  (`adressraum::USER_START..USER_ENDE`, mit checked_add — ein Zeiger nahe
  u64::MAX darf nicht „hinten wieder rauskommen"), **(b)** ist JEDE berührte
  Page im ADRESSRAUM DES AUFRUFENDEN PROZESSES gemappt und USER_ACCESSIBLE
  (nachgeschlagen in den Tabellen aus CR3 via `adressraum::aktive_seiten_flags`
  — eine Page aus einem FREMDEN Adressraum ist damit schlicht ungemappt),
  **(c)** beim copy-OUT zusätzlich WRITABLE. Fehlerwerte:
  `CopyFehler::{Ueberlauf, ZuGross, AusserhalbUserBereich, NichtGemappt,
  KernelSpeicher, NichtBeschreibbar, FalscherAdressraum}`. PANICKT NIE.
  `copy_in_prozess`/`copy_out_prozess` nennen den Adressraum explizit und
  lehnen ab, wenn er nicht der aktive ist. Alle Angriffsvarianten sind in
  `src/ring3.rs` unit-getestet (Kernel-Adresse, Nullzeiger, obere Hälfte,
  Integer-Überlauf, Länge über die Seitengrenze, fremder Adressraum).
- **(II) EIN FEHLER IM USER-MODE DARF DEN KERNEL NIE MITREISSEN.** Page Fault
  oder #GP aus Ring 3 beenden den User-Code und kehren in den Kernel zurück —
  der Kernel läuft weiter. Mechanik: `interrupts::user_recovery()` prüft „kam
  der Trap aus Ring 3 (CS & 3 == 3) UND läuft Ring-3-Code?" und biegt dann den
  CPU-Interrupt-Rahmen auf den Landeplatz um (Ring 0, Kernel-Stack) — der
  Epilog-`iretq` springt in den Kernel statt zurück nach Ring 3. NUR ein Fehler
  im KERNEL selbst (Ring 0) hält an, denn das ist ein echter Bug.

## RNG-DAUERREGEL (Serie 7, ab Juli 2026 — gilt AB SOFORT ueberall)
- **(I) ES GIBT GENAU EINE ZUFALLSQUELLE: `zufall::fuellen()`.** Wer
  Zufall braucht (Schluessel, Nonces, TCP-Anfangssequenznummern, ephemere
  Ports, Token), nimmt sie — NIE die TSC, NIE einen LCG, NIE `RDRAND`
  direkt. Die zwei LCGs im Projekt (Plattentest, TCP-Verlusttest) sind
  ABSICHTLICH reproduzierbare TESTHILFEN und heissen deshalb nicht
  „Zufall"; sie duerfen nie auf einen Sicherheitspfad wandern.
- **(II) NIE AUS EINER QUELLE ALLEIN.** Auch wenn RDSEED/RDRAND vorhanden
  sind, werden sie nur EINGEMISCHT (XOR in den Pool) und hoechstens mit der
  HALBEN Schwelle angerechnet (128 von 256 Bit) — der Rest MUSS aus
  Interrupt-Jitter kommen. Gruende: Der Rauschgenerator einer CPU ist nicht
  auditierbar, und es gab reale Errata (AMD nach S3: dauerhaft 0xFFFF_FFFF
  MIT gesetztem Carry, also als „gueltig" gemeldet). Weil per XOR gemischt
  wird, kann eine defekte Quelle die anderen NICHT verschlechtern.
  Nachpruefbar im Bootlog: „RDSEED: ja … Start mit 134 von 256 Bit —
  noch nicht gesaet".
- **(III) SALZ IST KEINE ENTROPIE.** RTC-Zeit, Boot-TSC und Speicher-Layout
  werden eingemischt, aber mit NULL angerechneten Bits: Ein Angreifer kennt
  sie (die Bootzeit steht in jeder Logzeile). Sie trennen zwei identische
  Rechner — mehr nicht. `Quelle::Salz.bits_je_probe() == 0` ist getestet.
- **(IV) LIEBER WARTEN ALS SCHWACH — es gibt KEINEN Fallback.** Ist der Pool
  ungesaet, liefert `fuellen` `Err(NichtGesaet)` und LAESST DEN PUFFER IN
  RUHE (halb gefuellt waere heimtueckisch: Nullen sehen aus wie Zufall). Der
  Syscall `zufall` (Nr. 12) blockiert bis zu 10 s und liefert dann
  `Fehler::NichtGesaet` (25) — nie ein Haenger ohne Ende, nie schwache Bytes.
  Begruendung und die Bewertung der Alternativen: docs/zufall.md §4.
- **(V) ENTROPIE-SCHAETZUNG WIRD UNTERTRIEBEN, NICHT SCHOENGERECHNET.** Die
  Bits je Quelle (Tastatur 4, Maus 3, Netz/Platte 2, PIT 1 je 8. Probe) sind
  BEGRUENDETE UNTERTREIBUNGEN, keine Messungen — und im Code als solche
  gekennzeichnet. Wer eine Quelle hinzufuegt, traegt sie mit einer
  Begruendung in docs/zufall.md §3 ein. Zu wenig anzurechnen kostet
  Wartezeit, zu viel kostet Sicherheit, ohne dass es jemand merkt.
- **(VI) IM INTERRUPT NUR ATOMICS.** `zufall::einspeisen(Quelle)` ist die
  EINZIGE Funktion, die ein IRQ-Handler aufruft: ein `rdtsc` und drei
  atomare Operationen, kein Lock, keine Allokation. Der DRBG-Mutex ist ein
  BLATT-Lock und wird ausschliesslich mit ausgeschalteten Interrupts
  gehalten (`mit_drbg`) — nur deshalb darf ein Syscall ihn nehmen.
- **(VII) WAS TESTS BEWEISEN — und was nicht.** Statistik (Byteverteilung,
  keine Wiederholungen, andere Werte nach Neustart) findet GROBE FEHLER und
  beweist KEINE kryptographische Qualitaet: Ein Zaehler durch AES besteht
  jeden dieser Tests. BELASTBAR ist allein der Testvektor-Vergleich
  (RFC 8439 §2.1.1 + §2.3.2, zusaetzlich gegen eine unabhaengige
  Python-Referenz gegengeprueft). Diese Unterscheidung steht als Kommentar
  an den Tests und darf nicht wegredigiert werden.
- **EIGENBAU-KRYPTO-GRENZE, praezisiert:** ChaCha20 als DRBG-Kern ist SELBST
  geschrieben, TLS wird es NIE (docs/serie7-bestandsaufnahme.md). Der
  Unterschied ist die PRUEFBARKEIT: 40 Zeilen, eine RFC-Seite, bitgenaue
  Testvektoren, keine schluesselabhaengigen Zweige oder Tabellenzugriffe
  (also seitenkanalfrei von selbst). Bei TLS waere der Test ein Angreifer,
  den wir nicht haben. Regel: Eine kryptographische Primitive darf selbst
  gebaut werden, WENN es offizielle Testvektoren gibt und die
  Implementierung datenunabhaengig laeuft — ein PROTOKOLL nie.

## ZEIT-DAUERREGEL (Serie 7, Teil 2 — ab Juli 2026, gilt ueberall)
- **DREI EBENEN, die NIE miteinander verrechnet werden** (Kopfkommentar in
  `src/zeit.rs`):
  **(1) DIE RTC-ZONE** — eine Eigenschaft der HARDWARE: Laeuft die CMOS-Uhr
  in UTC oder Lokalzeit? Wird EINMAL beim Anker-Setzen angewandt
  (`zeit::rtc_zone_setzen`, Einstellung `zeit.rtc_zone_min`, Standard 0 =
  UTC). **(2) UTC** — die Wahrheit. `zeit::jetzt()` liefert **IMMER UTC**;
  alles, was rechnet oder prueft, benutzt ausschliesslich das.
  **(3) DIE ANZEIGE-ZONE** — reine Kosmetik (`einstellungen::jetzt_lokal`,
  `zeit.utc_offset_min`). Sie darf NIE in eine Berechnung geraten.
  Bis Serie 3 war (1) mit (3) vermischt: `jetzt()` lieferte, was die RTC
  sagte (in QEMU Lokalzeit), und der Offset kam obendrauf — ein Nutzer in
  UTC+2 bekam zwei Stunden zu viel. Fuer eine Taskleiste egal, fuer ein
  Zertifikats-Ablaufdatum nicht. QEMU laeuft deshalb jetzt mit
  `-rtc base=utc` (vorher localtime).
- **PLAUSIBILITAET STATT VERTRAUEN:** `build.rs` legt das Bau-Datum als
  `SPEEDOS_BAU_EPOCHE_S` ins Image; `zeit::zeit_pruefen` lehnt jede Uhr VOR
  dem Bau-Datum ab (ein Kernel kann nicht vor seinem Bau gelaufen sein — das
  faengt den haeufigsten Fall, die leere Pufferbatterie) und alles ueber
  `PLAUSIBEL_JAHRE` (30) danach. Beim Boot und nach jeder Korrektur laeuft
  die Pruefung, ein Fehlschlag wird LAUT gemeldet (beide Daten
  gegenuebergestellt). EHRLICHE GRENZE: Sie findet KEINE Uhr, die um Stunden
  falsch geht, und keine absichtlich vorgestellte — dafuer braucht es NTP
  (noch offen, docs/tls-vertrauen.md §5).
- **DIE KONSEQUENZ, sonst waere es nur eine Logzeile:**
  `zeit::zertifikatszeit()` und der Syscall `zeit_geprueft` (Nr. 13) liefern
  bei unplausibler Uhr `Fehler::ZeitUnplausibel` (26) statt einer Zahl.
  `zeit_epoche` (6) bleibt unveraendert und ungeprueft — eine ANZEIGE darf
  falsch gehen, eine PRUEFUNG nicht. **Die Gueltigkeitspruefung wird NIE
  stillschweigend uebersprungen**; „Zeit stimmt nicht, pruefen wir halt
  nicht" ist der Punkt, an dem TLS aufhoert, etwas wert zu sein.
- **UHR VON HAND:** `einstellungen::zeit_setzen_lokal` (Eingabe Lokalzeit,
  gespeichert als UTC in `zeit.manuell_utc_s`, persistiert auf /platte, bei
  jedem Boot neu angewandt). Auf Hardware ohne RTC-Batterie der einzige Weg.
  Die CMOS-Uhr wird NICHT geschrieben.
- **HARDWARE-PRUEFUNG:** Der Diagnose-Schirm (Taste D) zeigt „RTC roh",
  „UTC", „Kernel-Bau" und das CA-Buendel — auf echter Hardware die einzige
  Stelle, an der sich nachsehen laesst, was die CMOS-Uhr liefert (dort gibt
  es keine serielle Ausgabe). Verfahren und OFFENER Status:
  docs/hardware-log.md.

## TLS-VERTRAUENSANKER-REGEL (Serie 7, Teil 2)
- **DAS BUENDEL WIRD BEWUSST GEHOLT, NIE NEBENBEI.** Quelle, Datum,
  SHA-256 und Zertifikatszahl stehen in `assets/ca-bundle.herkunft.txt`
  (geschrieben von `tools/ca_bundle_holen.ps1`, Quelle
  `https://curl.se/ca/cacert.pem`). Ein `build.rs`, das im Hintergrund
  Wurzelzertifikate aus dem Netz zieht, waere genau das, wogegen ein
  Vertrauensanker schuetzt. FEHLT die Datei, baut SpeedOS trotzdem — mit
  leerem Buendel und deutlicher Meldung.
- **WEG INS SYSTEM:** `assets/ca-bundle.pem` -> `build.rs` (include_bytes)
  -> `programme::ca_buendel_installieren()` beim Boot ->
  `/platte/system/ca-bundle.pem`. Dasselbe Muster wie die User-Programme,
  aus demselben Grund (kein Host-Werkzeug fuer SpeedFS) — und es reist mit
  `cargo run`, `cargo test` UND `cargo image` mit.
- **AKTUALISIERUNG: VON HAND**, Skript erneut ausfuehren. Ehrlich notiert
  inklusive der gefaehrlichen Richtung: Ein zu altes Buendel lehnt nicht zu
  viel ab, es VERTRAUT zu viel. Automatisch ginge nur mit einem eingebauten
  Signaturschluessel (sonst Henne-Ei: sicherer Abruf braucht TLS braucht
  Wurzeln) — eigenes Vorhaben.
- **BEKANNTE LUECKE, ausdruecklich dokumentiert und nicht verschwiegen:
  KEINE Sperrlisten-Pruefung (weder OCSP noch CRL).** Ein gestohlenes, noch
  nicht abgelaufenes Zertifikat wird akzeptiert. Begruendung in
  docs/tls-vertrauen.md §3a: Klassisches OCSP verraet dem Aussteller das
  Surfverhalten und scheitert in der Praxis WEICH (Responder weg ->
  trotzdem verbinden), ist also eine Anzeige und kein Mechanismus. Der
  richtige Weg waere OCSP-Stapling. Ebenfalls NICHT dabei: Certificate
  Transparency, Pinning, Benutzer-CAs, „trotzdem fortfahren"-Dialoge.
- **PARSER IM USER-SPACE** (`userland/src/pem.rs`): PEM->DER, bewusst simpel
  (nur Base64-Bloecke zwischen BEGIN/END CERTIFICATE), plus ein
  DER-Laeufer, der NUR fuer die Anzeige Subject-CN und Gueltigkeit
  herausholt — **kein X.509-Parser**, es wird nichts validiert. Krypto-nahes
  lebt in Ring 3: Ein Parser-Fehler soll einen Prozess treffen, nicht den
  Kernel. WICHTIGSTE ENTSCHEIDUNG: **Ein kaputter Block macht nur DIESEN
  Block ungueltig, nicht die Datei** — ein Anker mit 118 von 119 Wurzeln ist
  brauchbar, einer, der bei einem Zeilenumbruch auf 0 faellt, ist eine
  Ausfallquelle. Panickt nie, feste Obergrenzen.
- **Programm `zertifikate`** zeigt Anzahl, Stichprobe der Namen und die
  Ablaufdaten-Spanne. Es holt die Zeit ueber `zeit_geprueft` — bei kaputter
  Uhr zeigt es Daten AN, bewertet sie aber NICHT.

## TLS-MACHBARKEIT + USER-HEAP (Serie 7, Teil 3)
- **ERGEBNIS DER EVALUATION (docs/tls-entscheidung.md): rustls GEHT.**
  `rustls` 0.23 (`default-features = false`, `custom-provider`) mit
  `rustls-rustcrypto` als Anbieter uebersetzt fuer x86_64-unknown-none und
  laeuft in Ring 3 (`userland/tlsspike`, `tests/tlsspike.rs`). Gewaehlt
  gegen `embedded-tls`, weil dessen Zertifikatspruefung schwach ist — und
  die ist die Haelfte, auf die es ankommt. WARNUNG, die stehen bleibt: Der
  Anbieter ist **0.0.2-ALPHA**.
- **DIE VIER cfg-FLAGGEN in userland/.cargo/config.toml sind PFLICHT** —
  ohne sie bricht LLVM ab (`Do not know how to split the result of this
  operator!`), und zwar bei JEDER TLS-Bibliothek: `aes_force_soft`,
  `polyval_force_soft`, `poly1305_force_soft`,
  `curve25519_dalek_backend="serial"`. Grund: Die RustCrypto-Kisten
  uebersetzen auf x86_64 IMMER ihren SIMD-Zweig mit (Auswahl erst zur
  LAUFZEIT per `cpufeatures`), unser Target hat SSE aber ab
  (`-sse,+soft-float`, wegen des Kontext-Wechsels). Es sind cfg-Flaggen und
  KEINE Cargo-Features — deshalb stehen sie in der config.toml.
- **FUENFTE BEDINGUNG: `opt-level >= 1`.** `sha2` hat keinen force-soft-
  Schalter und bricht bei `-O0` ab (der tote SHA-NI-Zweig ueberlebt bis zur
  LLVM-Legalisierung). Unser Bau ist `--release` — aber ein Debug-Build von
  userland/ scheitert, und das ist eine ueberraschende Bedingung.
- **no_std VERAENDERT DIE rustls-API:** `ClientConfig::builder()` und
  `builder_with_provider()` sind std-gated. Nutzbar ist
  `builder_with_details(provider, time_provider)` — die Zeit ist ein
  PFLICHT-ARGUMENT. Und statt `ClientConnection` gibt es nur
  `UnbufferedClientConnection`: ein anderes Programmiermodell (Zustands-
  maschine selbst treiben, Puffer selbst verwalten). Das ist die eigentliche
  Arbeit des Handshake-Schritts.
- **USER-HEAP: `SYS_SPEICHER` (14) + `libspeed::heap`.** Der Kernel mappt
  Seiten IMMER lueckenlos hinter dem bisherigen Heap-Ende (darauf verlaesst
  sich `linked_list_allocator::extend` — es gibt genau EINEN
  zusammenhaengenden Heap, `brk`-Modell). Lage: `HEAP_START =
  elf::IMAGE_ENDE + 4 KiB`, max 12 MiB, danach 3 MiB ungemappter Abstand zum
  Stack; Seiten sind NX (W^X gilt weiter). **KEIN `frei`-Gegenstueck, und
  das ist Absicht** — ein Prozess gibt Seiten nie einzeln zurueck, sein
  Adressraum faellt beim Ende als Ganzes.
- **DIE ZEIT-NAHT IST DIE WICHTIGE:** `SpeedUhr: TimeProvider` liefert bei
  unplausibler Uhr `None`, und rustls LEHNT die Gueltigkeitspruefung dann ab
  statt sie zu ueberspringen. Damit ist „Uhr kaputt, pruefen wir halt nicht"
  nicht implementierbar.
- **`TcpStrom` glaettet die EINE Stelle, an der unsere ABI nicht passt:**
  `empfange` ist nicht-blockierend (0 = „noch nichts"), TLS erwartet einen
  blockierenden Strom. Die Warteschleife benutzt `abgeben()` (nicht
  `schlafe` — wir warten auf den Netz-Task) und unterscheidet „noch nichts"
  / „Gegenstelle zu" (Dateiende) / „Frist abgelaufen".
- **ZAHLEN:** ELF 830 KiB (.text 567 KiB) gegen 28 KiB fuer `zertifikate`;
  **Heap-SPITZE 66 944 Byte**; 119/119 Wurzeln von rustls-webpki
  akzeptiert; 3 Ciphersuites; 201 Zeilen im cargo tree; 0 Frames geleckt
  ueber 3 Laeufe.
- **DER SPIKE SPRICHT KEIN TLS.** Kein Socket, kein Handshake — das ist der
  naechste Schritt und ausdruecklich nicht dieser.
- **MESSFALLE, selbst hineingelaufen:** Heap-Bedarf ist die SPITZE, nicht
  der Endstand (der war 16 Byte, die Spitze 65 KiB). Und ein Deadlock im
  eigenen Allocator: Der `MutexGuard` aus einer `if let`-BEDINGUNG lebt bis
  zum ENDE DES BLOCKS — ein zweites `lock()` darin dreht sich fuer immer.

## TLS-VERBINDUNG + EIN PARSER, ZWEI TRANSPORTE (Serie 7, Teil 4)
- **MEILENSTEIN: `starte holes https://example.com/ --info` laeuft.** TLS 1.3
  (`TLS13_AES_128_GCM_SHA256`) ueber die Socket-Syscalls, Kette gegen
  /platte/system/ca-bundle.pem geprueft (119/119 Wurzeln), Hostname
  abgeglichen, HTTP/1.1 darueber — aus Ring 3, eigener Adressraum.
  Vollstaendig in docs/tls-verbindung.md.
- **DER PARSER LIEGT JETZT IN `speedhttp/` UND HAT EINEN LEEREN
  `[dependencies]`-BLOCK.** Die reine Protokoll-Logik aus `src/netz/http.rs`
  (Url/Antwort/url_parsen/naechste_url/anfrage_bauen/antwort_parsen/
  chunked_dekodieren) ist ZEILE FUER ZEILE dorthin umgezogen; der Kernel
  re-exportiert sie mit `pub use speedhttp::*` und behaelt nur den TRANSPORT.
  **REGEL: Die `#[test_case]`-Tests am Ende von src/netz/http.rs sind
  unveraendert aus Serie 5 und duerfen NICHT angepasst werden** — sie sind der
  Beweis, dass der Parser fuer TLS nicht angefasst werden musste. Zweiter
  Beleg: `tests/netz_https.rs::test_parser_ist_derselbe` vergleicht die
  FUNKTIONSADRESSEN. Wer `speedhttp` eine Abhaengigkeit gibt, zerstoert die
  Aussage — das ist der Punkt der Kiste.
- **`HttpFehler` (Protokoll) vs. `http::KlientFehler` (Protokoll + Weg):**
  Die alten Varianten `Dns(..)`/`Socket(..)` konnten nicht mitziehen (sie
  tragen Kernel-Typen, ein Parser mit Socket-Fehler waere kein transportfreier
  Parser). Ring 3 hat sein eigenes Gegenstueck `libspeed::tls::TlsFehler`.
  EINZIGE Ergaenzung am Parser: `anfrage_bauen_mit_host` — sie ruft das
  ORIGINAL mit einer passend gefuellten `Url` (bei https gehoert `:443` nicht
  in den Host-Kopf), baut also nichts nach.
- **`libspeed::tls::TlsStrom` treibt `UnbufferedClientConnection` selbst.**
  Nach aussen sieht er aus wie der `TcpStrom` darunter (`lesen`/`schreiben`,
  blockierend) — nur deshalb merkt der HTTP-Parser nicht, worauf er sitzt.
  BORROW-FALLE: `process_tls_records` LEIHT SICH den Eingangspuffer aus, und
  der geliehene Zustand lebt bis zum ENDE DES `match`. Im
  `BlockedHandshake`-Zweig direkt in denselben Puffer nachzulesen geht nicht;
  `takt()` merkt sich deshalb nur eine `Aktion` und handelt danach.
- **KEIN UMGEHUNGS-SCHALTER, und das ist eine Entscheidung.** Kein
  `--unsicher`, kein `--zertifikat-egal`, kein „trotzdem fortfahren". Jeder
  Pruefungsfehler beendet `holes` mit Exit 4 und einem deutschen Satz
  (`TlsFehler::text()`); `kurz()` liefert das maschinenlesbare Schlagwort
  (`unbekannte-ca`, `abgelaufen`, `falscher-hostname`, `uhr-unplausibel`,
  `protokoll`, …). Ein Schalter wird benutzt, sobald es ihn gibt.
- **ZWEI LEKTIONEN, die den Test fast wertlos gemacht haetten:**
  **(1) `tls12` MUSS im `rustls-rustcrypto`-Feature stehen.** Ohne es gibt es
  nur die drei TLS-1.3-Suiten (daher „3 Ciphersuites" in Teil 3), und jeder
  Server ohne TLS 1.3 — u. a. SAEMTLICHE badssl.com-Endpunkte — antwortet mit
  `HandshakeFailure`, BEVOR er ein Zertifikat schickt. Man haelt einen
  Aushandlungs-Fehlschlag dann fuer eine bestandene Pruefung. Mit `tls12`
  sind es neun. **(2) `openssl req -x509` erzeugt keinen tauglichen
  Testfall:** Ein einzelnes selbst signierendes Zertifikat hat `CA:TRUE` und
  kein `extendedKeyUsage=serverAuth`, ist also formal gar kein
  Serverzertifikat; webpki lehnt es aus FORMGRUENDEN ab
  (`InvalidCertificate(Other(..))`). `tools/tls_testserver.py` legt deshalb
  eine echte Kette vor (eigene Mini-CA -> einwandfreies Serverzertifikat) —
  erst dann lautet der Befund `UnknownIssuer` und der Test prueft die
  VERTRAUENSKETTE statt der Formalien.
- **TESTMETHODIK wie bei TCP:** Das HARTE Gate liegt auf dem lokalen
  `tools/tls_testserver.py` (10.0.2.2:8443, muss IMMER abgelehnt werden); die
  badssl.com-Laeufe (expired/wrong.host/self-signed/untrusted-root) sind
  BERICHT und werden sauber uebersprungen, wenn kein Internet da ist.
- **ZAHLEN:** Handshake 34–36 ms (example.com) bzw. 12–13 ms (curl.se);
  Heap-SPITZE 121 160 B (kleine Seite) / 648 552 B (186-KiB-Datei; das
  CA-Buendel liegt bewusst in `.bss`, nicht auf dem Heap, sonst misst die
  Spitze eine Dateigroesse); Durchsatz 186 446 B in 29 ms = **6 278 KiB/s**.
  **DIE WICHTIGE ZAHL: dieselbe Datei ohne TLS ueber den KERNEL-Klienten im
  LAN schafft nur 406 KiB/s — TLS aus Ring 3 ist 15x SCHNELLER.**
  Verschluesselung ist nicht der Engpass, das WARTEN war es: Der
  Kernel-Klient wartet mit `zeit::warte_auf_interrupt()` (~ein Segment je
  Tick), `holes` mit `abgeben()`. Der Wecken-Fix aus Serie 7, Teil 0 schlaegt
  hier voll durch. ELF `holes` 949 984 B.
- **NEUE LUECKE, ausdruecklich notiert: `close_notify` wird NICHT erzwungen.**
  Schliesst die Gegenstelle die TCP-Verbindung ohne Abschiedsgruss, gilt der
  Strom als beendet — von einem Truncation-Angriff nicht zu unterscheiden.
  Erzwaenge man es, waere die halbe Welt unerreichbar (viele Server schliessen
  bei `Connection: close` einfach). Was schuetzt, liegt eine Schicht hoeher:
  Der HTTP-Parser prueft gegen `Content-Length` bzw. den 0-Chunk. Die
  Luecken aus Teil 2 (keine Sperrlisten, kein CT, kein Pinning) bleiben.

## DIE ABRUFSCHICHT — „hol mir diese URL" (Serie 7, Teil 5)
- **`libspeed::netz::Klient` IST DIE STELLE, an der ein User-Programm eine
  URL holt.** Wer in einem neuen Programm DNS, Socket, Handshake und
  Rumpf-Schleife selbst schreibt, macht es falsch — `holes` und `news` sind
  beide nur Bedienoberflaechen darauf (drei Zeilen Netz-Code), und der
  Browser aus Serie 8 wird es genauso machen. Zusagen: **haengt nie**
  (Frist je Versuch), **frisst keinen Speicher** (`max_bytes` wird WAEHREND
  des Lesens geprueft, nicht danach), folgt Weiterleitungen **auch ueber das
  Schema hinweg**, aber nie im Kreis, und **prueft Zertifikate immer** — es
  gibt keinen Parameter dagegen.
- **`AbrufFehler` trennt nach SCHICHT, nicht nach Bequemlichkeit:** Url /
  Dns / Verbindung / **Tls** / Http(fehler, bytes) / LeereAntwort / ZuGross /
  ZuVieleWeiterleitungen / Schleife / Frist. `ist_sicherheitsfehler()`
  beantwortet die eine Frage, die zaehlt (darf man das wiederholen?);
  `kurz()` liefert das maschinenlesbare Schlagwort, an dem die Tests haengen.
  `TlsFehler::Netz(..)` wird BEWUSST zu `Verbindung` und NICHT zu `Tls` —
  sonst saehe ein abgerissenes Kabel wie ein Zertifikatsproblem aus, und das
  waere eine falsche Sicherheitsaussage.
- **DER WETTLAUF AM STROM-ENDE — die wichtigste Lehre dieses Teils.**
  `TcpStrom::lesen` fragte `empfange` und `socket_zustand` als ZWEI Syscalls
  ab. Am Ende einer HTTP-Antwort liegen Nutzdaten und FIN nur **49 us**
  auseinander (im Mitschnitt nachgemessen), und der Stack verarbeitet beide
  im selben Durchgang. Traf er ZWISCHEN die beiden Syscalls, meldete der
  Zustand „zu", waehrend die Daten schon im Puffer lagen — und `lesen` gab
  Dateiende zurueck. Fehlerbild: „Verbindung angenommen, null Bytes",
  ungefaehr jede fuenfte Verbindung in einer schnellen Serie.
  **DIE REGEL: Ein Zustand, der „zu" sagt, ist KEIN Dateiende — er ist die
  Aufforderung, den Puffer NOCH EINMAL zu leeren.** Erst ein `empfange`, das
  NACH dem Schliess-Befund leer bleibt, ist wirklich das Ende. Wer irgendwo
  sonst Zustand und Daten in zwei Schritten abfragt, hat denselben Fehler.
  Warum es lange unentdeckt blieb: Der KERNEL-Klient pumpt nach jedem
  Schliessen 60 Ticks (~240 ms) und ist zu langsam, um den Wettlauf zu
  verlieren (30/30); erst ein Ring-3-Programm ohne diese Bremse ist schnell
  genug. Regressionswaechter:
  `tests/netz_klient.rs::test_kein_wettlauf_am_strom_ende` (30/30 in beiden
  Betriebsarten, 0 Wiederholungen).
- **DIE SUCHE, damit sie niemand wiederholt:** Zwei Vermutungen waren FALSCH
  und stehen deshalb im Code — „liegt am Prozess-Start/-Ende" (widerlegt: 30
  Abrufe in EINEM Prozess waren SCHLECHTER als 30 Prozesse mit je einem, es
  lag also an der RATE) und „liegt am Listen-Backlog des Testservers"
  (widerlegt: von 5 auf 128, Fehlerrate unveraendert). Gefunden hat es der
  MITSCHNITT: `SPEEDOS_NET_DUMP=1` plus ein kleiner pcap-Leser zeigte, dass
  auf der Leitung JEDE der 143 Verbindungen ihre Antwort bekommen hatte — die
  Bytes kamen an und wurden nur nicht ausgeliefert.
- **WIEDERHOLT WIRD GENAU EIN FALL: `LeereAntwort`** (Verbindung angenommen,
  null Bytes, sofort zu). Ein GET, bei dem nichts ankam, ist gefahrlos
  wiederholbar — es kann nichts zweimal passiert sein. Ein Zertifikatsfehler
  wird NIE wiederholt (das waere ein Angreifer, der es nochmal versucht),
  eine abgeschnittene Antwort auch nicht (dort ist schon etwas passiert),
  eine Frist erst recht nicht. Die Zahl steht in `Abruf::wiederholungen`,
  damit sie MESSBAR ist statt heimlich, und `holes --serie=N` schaltet sie ab
  — so faellt auf, wenn sie wieder etwas verdecken muesste. (Sie war
  urspruenglich eingefuehrt worden, um genau den Wettlauf oben zu
  ueberdecken; seit dessen Behebung feuert sie in den Tests NIE.)
- **TESTKERNEL-REGEL:** Endet ein Prozess, schliesst seine Handle-Tabelle die
  Sockets — aber „schliessen" heisst bei TCP FIN, ACK, TIME_WAIT, und dafuer
  muss jemand den Stack DREHEN. Im Betrieb tun das `netz_task` und der
  Socket-Takt; ein Testkernel hat keinen Executor und muss nach jedem Prozess
  selbst pumpen (`for _ in 0..60 { netz::pumpen(); warte_auf_interrupt(); }`).
- **`hole` WAEHLT DEN WEG SELBST:** `http://` -> Kernel-Klient (kein Prozess),
  `https://` -> Ring-3-Programm `holes` ueber `pipeline_ausfuehren`,
  schemalos -> https. **Die http->https-Weiterleitung ist eine UEBERGABE und
  kein Fehler:** `http::holen` rechnet das Ziel aus und liefert
  `KlientFehler::BrauchtTls(adresse)`; die Shell reicht es weiter. Eine
  Zieldatei ohne `/` landet im Zuhause (`explorer::start_ordner`).
- **`speedhttp::ziel_parsen` / `naechstes_ziel` sind die EINE Stelle fuer
  Schema und Standard-Port.** Sie sind ERGAENZUNGEN und bauen auf den
  unveraenderten Serie-5-Funktionen auf: `url_parsen` nimmt schemalose
  Eingaben an und lehnt `https://` weiterhin ab — das bleibt so, es ist der
  transportfreie Unterbau. `Ziel::als_text` NORMALISIERT (Standard-Port
  weggelassen), und genau davon lebt der Schleifenschutz.
- **`news` ist KEIN HTML-Renderer** und soll auch keiner werden (das ist
  Serie 8). Ein Zeichen-Automat mit drei Zustaenden, damit er auch kaputtes
  HTML uebersteht. DIE Entscheidung: `<script>`/`<style>`/`<head>` fliegen
  MITSAMT INHALT raus — wer nur Tags entfernt, bekommt seitenweise
  JavaScript zu lesen und haelt das Ergebnis fuer kaputt.
- **`tools/tls_testserver.py` benimmt sich auf Wunsch schlecht** (`/abbruch`
  kappt mitten im Rumpf, `/endlos` sendet ohne Ende, `/schleife` und
  `/ringelreihen` leiten im Kreis, `/kette` zehnmal, `/nach-tls` wechselt das
  Schema, Port 8444 nimmt an und schweigt) und hat einen KLARTEXT-Port
  (8080). Der ist noetig, weil das TLS-Zertifikat zu Recht abgelehnt wird,
  BEVOR je ein Byte Rumpf fliesst — die Rumpf-Fehlerfaelle laufen deshalb
  ueber http, es ist dieselbe Zustandsmaschine ohne Verschluesselung.

## SERIE-7-ABSCHLUSS (Juli 2026) — Angriffe, Zahlen, Grenzen, Serie-8-Naht
- **DIE EINE STELLE FUER ALLE LUECKEN: `docs/grenzen.md`.** Keine
  Sperrlisten-Pruefung (OCSP/CRL), manueller Root-Store OHNE Signatur, kein
  NTP, KEIN NETZ-TREIBER FUER ECHTE HARDWARE (nur virtio-net, also nur in
  VMs), TCP ohne Congestion-Control/SACK/Window-Scaling/Out-of-Order, FAT32
  nur lesend, SpeedFS ohne Journal, kein SMP, keine Rechte/Benutzer.
  **REGEL: Wer eine neue Luecke findet oder schafft, traegt sie DORT ein** —
  verstreute Ehrlichkeit ist keine. Das Dokument hat einen eigenen Abschnitt
  fuer das, was MIT ABSICHT fehlt (`--unsicher`-Schalter, Eigenbau-TLS,
  Zufall-Fallback, Auto-Format) — das ist keine Wunschliste.
- **DER ANGREIFER KENNT JETZT AUCH TLS UND RNG** (`angreifer 7/8/9`):
  `zufall` mit Kernel-Zeigern und Riesenlaengen, `speicher` mit absurden
  Groessen, PEM-Buendel aller Kaputtheitsgrade. **DIE SCHAERFSTE EINZELNE
  PRUEFUNG: Nach einem abgelehnten `zufall` muss der Puffer Byte fuer Byte
  UNVERAENDERT sein** (RNG-Dauerregel IV) — halb gefuellter Zufall waere
  heimtueckischer als ein Fehler, denn Nullen sehen aus wie Zufall.
  Gemessen: `speicher` gibt GENAU 12 MiB und sagt dann Nein.
- **DIE VERTRAUENSDATEI IST EIN ANGRIFFSZIEL, und das steht im Test:**
  `tests/sicherheit.rs::test_kaputter_vertrauensanker_verbindet_nicht`
  ersetzt /platte/system/ca-bundle.pem durch vier Sorten Muell und stellt sie
  danach wieder her. SpeedOS kann das Ersetzen NICHT verhindern (keine
  Signatur, siehe grenzen.md §1); es darf sich nur nicht TAEUSCHEN lassen —
  eine kaputte Datei fuehrt zu GAR KEINER Verbindung, nicht zu weniger
  Pruefung. Wer den Test anfasst: Der Anker MUSS am Ende wiederhergestellt
  sein, sonst sind alle folgenden Tests wertlos.
- **SPEICHER-PASS: 50 HTTPS-Zyklen, Frames byte-exakt 0, Sockets/Pipes
  stabil, 352 ms je Zyklus.** Die P1-Buchhaltung (1 Frame je 512 Seiten) wird
  AUSGERECHNET (50 Prozesse a ~340 Seiten -> Schranke 34), nicht weggelassen.
- **MESSFALLE, zweimal hineingelaufen:** `socket::schliessen` MARKIERT nur;
  der Eintrag faellt erst nach TIME_WAIT (2 s). Wer vorher zaehlt, sieht
  Sockets, die keine mehr sind — und weil ein TCB zwei 8-KiB-Ringpuffer
  haelt, sieht der HEAP gleich mit nach einem Leck aus (17 KiB = genau eine
  Verbindung). BEIDE Messpunkte (vorher UND nachher) brauchen dieselbe
  Ruhe-Prozedur (`zur_ruhe_kommen()` in tests/netz_klient.rs).
- **LEISTUNG FINAL:** Syscall-Roundtrip **70 ns**, Kontext-Wechsel **479 ns**,
  Weck-Latenz **5 us** (war 3829), Pipe **228 MiB/s** (war 199 KiB/s),
  Socket-`sende` 29 869 KiB/s, TLS-Handshake 11–29 ms, **HTTPS-Durchsatz
  6 743 KiB/s** gegen 625 KiB/s beim Kernel-Klienten ohne TLS —
  Verschluesselung ist NICHT der Engpass, das Warten war es.
- **unsafe-AUDIT SERIE 7 (`docs/unsafe-audit-serie7.md`):** Der ENTROPIE-PFAD
  IM IRQ-KONTEXT IST unsafe-FREI (ein `rdtsc` + drei Atomics), und der
  SPEICHER-SYSCALL AUCH — die gefaehrliche Arbeit steckt in
  `adressraum::bereich_mappen_mit_rechten` (Serie 6 auditiert), was bleibt,
  sind vier Zeilen `checked_add` + harte Grenze. `userland/tls.rs`,
  `netz.rs`, `pem.rs` und `speedhttp/` haben zusammen NULL unsafe-Bloecke.
  Wer dort einen einbaut, begruendet ihn im Audit-Dokument.
- **DIE NAHT ZU SERIE 8 (`docs/serie8-bestandsaufnahme.md`):** Empfohlen ist
  **Pixelpuffer per Syscall** (`fenster_neu`/`fenster_zeichnen` ueber
  `copy_in`) — nicht weil es das schnellste ist, sondern weil es KEINE
  Sicherheitszusage kostet und geteilten Speicher nicht verbaut. Das
  Kriterium fuer den Umstieg ist VORHER festgelegt (Scroll-Frame > ~8 ms und
  die Kopie mehr als die Haelfte davon) — dieselbe Methodik wie die
  TCP-Reissleine. Die Architekturfrage lautet: Toolkit als GETEILTE Kiste
  `speedui` nach dem Muster von `speedhttp`; der Vorbehalt dabei ist ehrlich
  notiert (das Toolkit kennt Schriften, Themes und Zeit — drei
  Abhaengigkeiten, die zu Argumenten werden muessen).

## Platten-Sicherheits-Regel (Juli 2026)
- Der ATA-Treiber weigert sich PER KONSTRUKTION, auf das Boot-Laufwerk
  zu schreiben: Das Feld `beschreibbar` ist privat, Laufwerke entstehen
  ausschließlich in `ata::init()`, und nur die konfigurierte DATEN-
  Platte (Primary Slave) bekommt Schreibrechte — es gibt keinen
  API-Weg, das zu umgehen (`IoFehler::Schreibgeschuetzt`). Tests
  laufen zusätzlich gegen ein EIGENES Daten-Image
  (speedos-daten-test.img), nie gegen speedos-daten.img.

## Architektur-Entscheidungen
- **PCI + virtio-blk (Juli 2026) — die para-virtualisierte Platte:**
  `src/pci.rs` enumeriert den PCI-Bus über die Legacy-Ports
  0xCF8/0xCFC (Config-Space; keine PCI-Bridge-Rekursion — QEMU legt
  alles auf Bus 0), dekodiert Vendor/Device/Klasse/BARs (reine,
  unit-getestete Funktionen) und ist die Grundlage jedes modernen
  Treibers. Shell: `pci`. `src/virtio/virtqueue.rs` ist die
  Split-Virtqueue (Deskriptoren + Avail-/Used-Ring in physisch
  zusammenhängendem Speicher via memory::allocate_pages, Physik-
  Adresse per uebersetzen) — BEWUSST geräte- UND transport-unabhängig
  und ausführlich kommentiert, weil virtio-net (Serie 5) sie
  UNVERÄNDERT weiterbenutzt (nur der Transport unterscheidet sich).
  `src/virtio/blk.rs` ist der virtio-blk-Treiber über den PCI-LEGACY-
  Transport (Port-I/O-BAR): ENTSCHEIDUNG Legacy statt Modern, weil
  QEMUs transitional device es anbietet, wir Port-I/O vom ATA-Treiber
  kennen und die Virtqueue (der wiederverwendbare Teil) bei beiden
  identisch ist. Feature-Negotiation (nur FLUSH), eine Virtqueue,
  Requests gepollt mit TSC-Timeout, DMA über einen BOUNCE-Puffer
  (der Heap-Puffer des Aufrufers ist nicht physisch zusammenhängend).
  Implementiert BlockDevice inkl. sync (FLUSH). BACKEND-WAHL:
  `SPEEDOS_PLATTE=ide|virtio` im Runner; `fs::daten_geraet()` ist DIE
  Stelle, die virtio ODER ATA als Daten-Platte liefert (virtio hat
  Vorrang) — alle Aufrufer sehen nur `Box<dyn BlockDevice>`. STANDARD
  ist virtio (plattentest misst es ~1000x schneller als IDE-PIO, weil
  PIO pro 16-Bit-Wort einen Port-I/O-VM-Exit kostet); IDE bleibt
  wählbar, u. a. weil tests/ata_platte.rs den ATA-Treiber direkt
  testet und dafür eine IDE-Daten-Platte braucht (unter virtio
  überspringt es seine Daten-Tests sauber). main.rs: pci::init +
  virtio::blk::init laufen NACH der Heap-Erweiterung (die Virtqueue
  alloziert), VOR den Auto-Mounts.
- **virtio-net + Netz-Stack (Serie 5, Juli 2026) — vom RX-Hexdump zur
  Architektur-Naht:** Der Treiber `src/virtio/net.rs` ist Legacy-Init wie
  blk, aber MEHRERE Queues (RX=0, TX=1) und INTERRUPTS statt Polling —
  RX-Pakete kommen unaufgefordert. Die Virtqueue wird UNVERÄNDERT
  weiterbenutzt. RX-Queue hält 16 gerätebeschreibbare DMA-Puffer (kein
  Bounce, wir besitzen sie); `RxRing` führt Kopf→Puffer und stellt nach
  dem Verbrauch wieder ein. IRQ-PFAD (Tastatur-/Maus-Muster):
  interrupts.rs registriert Handler für die PCI-Vektoren 41/42/43 (IRQ
  9/10/11), liest im Handler das ISR-Register (0x13, quittiert + sagt
  „waren WIR es?" bei Shared Interrupts) und weckt — KEIN Lock/keine
  Allokation im Handler. `interrupts::irq_freischalten(irq)` schaltet die
  zur Laufzeit gefundene IRQ am PIC frei (in `net::init`, nicht
  `lib::init` — die IRQ steht erst nach der PCI-Enumeration fest;
  QEMU-i440fx gibt der NIC IRQ 11). Der gepollte virtio-blk bekommt
  `Virtqueue::interrupts_aus()` (VIRTQ_AVAIL_F_NO_INTERRUPT), damit er nie
  interruptet. IO_BASIS ist eine globale AtomicU16, damit der Handler das
  ISR lock-frei liest. RUNNER: `-netdev user + virtio-net-pci` (slirp-NAT,
  immer, auch im Test — der PCI-Fund-Test braucht die NIC);
  SPEEDOS_NET_DUMP=1 → filter-dump-pcap.
  **DIE NAHT: `netz::NetzGeraet`** (analog `BlockDevice`, `src/netz/`):
  `mac()`, `sende_frame(&[u8])`, `empfange_frame()`. virtio-net
  implementiert es und REGISTRIERT sich in der Netz-Schicht
  (`geraet_registrieren`); der Stack redet NUR mit dem Trait (e1000/rtl8139
  später ohne Stack-Änderung). Kein Treiber-`rx_task` mehr — den RX-Weg
  treibt der Stack. SCHICHTEN: `netz/puffer.rs` (Leser/Schreiber,
  grenzgeprüft, Big-Endian — von Ethernet UND ARP genutzt),
  `netz/ethernet.rs` (Frame parse/bau + Hexdump, geräteunabhängig),
  `netz/arp.rs` (IP↔MAC: Requests beantworten/senden, Cache mit
  2-Min-Timeout — reine Logik, `jetzt_ms` übergeben), `netz/geraet.rs`
  (NIC-Registry + RX-Waker). DER `netz_task` (main.rs, NACH blk::init):
  vom IRQ geweckt, holt Frames vom NetzGeraet, dispatcht nach EtherType
  (ARP → arp; IPv4 folgt). `netz::rx_verarbeiten()` ist SYNCHRON
  aufrufbar, damit `arp-ping` den Empfang selbst pumpt (der kooperative
  Executor gibt während eines Shell-Befehls keinem Task Zeit). Statische
  IP-Konfig (DHCP später), Shell: `netz`, `netz-ip <ip> <maske>
  <gateway>`, `netz-lausch`, `arp`, `arp-ping <ip>`. LOCK-ORDNUNG:
  KONFIG/ARP_CACHE → GERAET (sende_frame nimmt nur GERAET); Dispatch
  sammelt Frames EIN (GERAET-Lock los), bevor er antwortet — kein
  verschachtelter Lock. Meilenstein „SpeedOS antwortet auf ARP" doppelt
  bewiesen: Mock-NIC-Unit-Test + `tests/netz_arp.rs` gegen slirp
  (arp-ping 10.0.2.2 → Gateway-MAC 52:55:0a:00:02:02).
- **IPv4 + ICMP (Serie 5, Juli 2026, `src/netz/ipv4.rs`+`icmp.rs`) — SpeedOS
  ist anpingbar:** IPv4 parst/baut den 20-Byte-Kopf; die INTERNET-CHECKSUMME
  (RFC 1071) ist eine reine, gegen bekannten Vektor (0xB861) getestete
  Funktion — sie liefert 0 über einen Kopf MIT korrekter Prüfsumme (so
  prüft man RX) und den einzusetzenden Wert bei Feld=0 (so baut man TX).
  FRAGMENTE werden ERKANNT (MF/Offset) und VERWORFEN (kein Reassembly —
  bewusst, dokumentiert). Ausgehend: Next-Hop = eigenes Subnetz direkt,
  sonst Gateway; MAC per ARP-Cache, bei MISS Paket ZURÜCKSTELLEN
  (`AUSSTEHEND`, TTL 3 s) + ARP-Request, `ausstehend_ausliefern()` läuft
  nach JEDEM Dispatch (`rx_verarbeiten`). ICMP beantwortet Echo-Requests
  (Reply mit gespiegeltem Identifier/Sequenz/Daten, Checksumme über die
  GANZE Nachricht) und vermerkt Echo-REPLIES (ident/seq/ttl) für `ping`.
  Shell `ping <ip>`: 4 Echos, RTT über die TSC-µs-Uhr, min/schnitt/max —
  pumpt synchron. MEILENSTEINE: (1) „Host pingt SpeedOS" geräteunabhängig
  per Mock (`test_icmp_echo_antwort_meilenstein`) — über slirp-NAT ist der
  Gast von außen NICHT direkt pingbar (bräuchte TAP/Bridge); (2) „SpeedOS
  pingt Gateway" ECHT gegen slirp (`tests/netz_ping.rs`, ping 10.0.2.2 →
  ttl 255). ipv4::verarbeiten prüft „an UNS gerichtet?" (dest == unsere IP,
  255.255.255.255 oder Subnetz-Broadcast — Broadcast nötig für DHCP).
- **UDP + DHCP + DNS (Serie 5, Juli 2026, `src/netz/{udp,dhcp,dns}.rs`) —
  SpeedOS ist im Internet:** UDP parst/baut Datagramme; die PRÜFSUMME läuft
  über den PSEUDO-HEADER (src/dst-IP, Proto, Länge + Segment) — reine
  Funktion auf der Internet-Checksumme; 0 im Feld = „keine". PORT-DEMUX:
  `udp::binden(port)` legt eine Empfangs-Queue an, `udp::verarbeiten`
  (aus ipv4 für Proto 17) stellt zu, `udp::empfangen(port)` holt ab —
  VORÜBUNG für die Socket-API (Handles/Ports, Puffer-Ownership je Vec).
  DHCP-Client: DISCOVER→OFFER→REQUEST→ACK über UDP-Broadcast (68→67),
  BROADCAST-Flag gesetzt (Server antwortet an 255.255.255.255, bevor wir
  eine IP haben); `ipv4::senden_an_mac` (Quell 0.0.0.0 an Broadcast-MAC,
  ohne ARP/Config) ist der DHCP-TX-Weg. Optionen (53 Typ, 1 Maske, 3
  Router, 6 DNS, 51 Lease, 54 Server-ID) als reine, getestete TLV-Schleife.
  `dhcp::autokonfig(3000)` läuft BEIM BOOT (main.rs nach net::init, pumpt
  synchron — kein Executor nötig); Timeout → Fallback statisch. NetzKonfig
  trägt jetzt dns + quelle (Keine/Statisch/Dhcp) + lease_sekunden.
  DNS-Resolver: A-Query bauen, Antwort parsen MIT Namens-KOMPRESSION
  (0xC0-Zeiger, `name_lesen` folgt ihnen mit Sprung-Limit; liefert Name +
  Offset hinter dem ERSTEN Zeiger); Cache (Name→IP, TTL, mind. 10 s);
  ephemerer Quell-Port rotiert. Shell: `netz-status`, `dhcp`, `nslookup`.
  MEILENSTEIN „im Internet" ECHT (`tests/netz_dhcp_dns.rs`): DHCP →
  10.0.2.15/…/DNS 10.0.2.3, dann `example.com` → echte IP (braucht Host-
  Internet; DNS-Protokoll separat per Unit-Test bewiesen).
- **TCP (Serie 5, Juli 2026, `src/netz/tcp.rs`) — Minimal-Viable, bewusstes
  LERN-ARTEFAKT:** Umfang/Lücken/REISSLEINE stehen VOR dem Code in
  docs/tcp-scope.md (Reißleine: < 9/10 saubere HTTP-Läufe ⇒ smoltcp NUR für
  die TCP-Schicht; gemessen 10/10 → Eigenbau bleibt). Der `Verbindung`-TCB
  ist eine REINE Zustandsmaschine: Eingaben `segment_empfangen/senden/
  schliessen/tick`, Ausgabe ein AUSGANG gebauter Segmente (kein Selbst-
  Senden) — derselbe Code läuft gegen echte Hardware UND im Loopback-Test
  gegen sich selbst (Kanal mit einstellbarem Verlust). Voller Automat (11
  Zustände), Handshake aktiv+passiv, In-Order-Daten mit festem Fenster,
  Retransmit mit fester RTO + exp. Backoff (KEIN Karn/Jacobson), TIME_WAIT
  (2·MSL auf 2 s verkürzt). BEWUSST NICHT: Congestion-Control, Fast-Retx,
  SACK, Window-Scaling, Out-of-Order-Reassembly (Out-of-Order verworfen →
  kumulatives ACK → Retransmit; Go-Back-N-artig, korrekt aber bei Verlust
  langsam). Seq-Arithmetik zyklisch (seq_lt via `(a-b) as i32 < 0`). Puffer:
  `netz::puffer::Ringpuffer` (Byte-Ring, spitzen=peek für Retransmit,
  verwerfen=ACK-Freigabe) für Sende-/Empfangspuffer; Ownership copy-in/out.
  TREIBER (`tcp::verarbeiten` aus IPv4-Proto-6-Dispatch + `tcp::hole`): EINE
  aktive Verbindung (Mutex<Option<Verbindung>>), synchron gepumpt wie
  ping/dns (Ausgang per ipv4::senden, Empfang per rx_verarbeiten, tick).
  MEILENSTEIN ECHT (`tests/netz_tcp.rs`): 10/10 example.com:80 sauber.
- **Socket-API + HTTP-Client (Serie 5, Juli 2026, `src/netz/{socket,http}.rs`)
  — die öffentliche Fassade:** `socket.rs` ist DIE NAHT FÜR SERIE 6:
  HANDLES statt Zeiger (undurchsichtige, monoton wachsende IDs — kein
  Recycling; nach `schliessen` liefert JEDE Operation `UngueltigerHandle`),
  klare Fehler-Enums, PUFFER-OWNERSHIP explizit (senden=copy-in,
  empfangen=copy-out in Aufrufer-Slices — die künftige Kernel/User-Grenze),
  TLS-agnostisch (kennt nur Bytes). TCP UND UDP über dieselbe API: TCP trägt
  `tcp::Verbindung`, UDP nutzt den bestehenden Port-Demux. Der alte
  Einzelverbindungs-Treiber in tcp.rs ist WEG; `tcp::verarbeiten` →
  `socket::tcp_zustellen` (Zustellung per 4-Tupel, sonst lauschender Port).
  `socket::bedienen()` tickt Timer, sendet die erzeugten Segmente per IPv4
  (Socket-Lock beim Senden NIE gehalten) und räumt fertige Sockets ab;
  `netz::pumpen()` = rx_verarbeiten + bedienen (nutzen netz_task UND jede
  synchrone Shell-Pumpe). Ein "Socket-Takt"-Task (100 ms) lässt Retransmits
  auch ohne eingehenden Verkehr feuern. `http.rs`: Anfrage bauen
  (Host, Connection: close), Antwort parsen (Statuszeile, Header
  case-insensitiv/robust, Rumpf per Content-Length ODER chunked mit
  0-Chunk-Prüfung), 3xx-Weiterleitungen mit absoluter/relativer
  Location-Auflösung, NUR http:// (https ⇒ `TlsNichtUnterstuetzt`).
  Shell `hole <url> [zieldatei]` zeigt Status+Header und speichert den Body
  wahlweise aufs Dateisystem (mit sync). MEILENSTEIN protokolliert in
  docs/tcp-scope.md: LAN-Server 10/10 à 21 700 Byte (> Fenster!), Internet
  10/10; Body byte-identisch auf /platte.
- **REISSLEINEN-ENTSCHEID (Juli 2026) — Eigenbau-TCP BLEIBT:** Der
  Stresstest (`tests/netz_stress.rs`) misst gegen 8 verschiedene echte
  Internet-Server und mit künstlichem Paketverlust
  (`netz::geraet::verlust_setzen(prozent)`, je Richtung — auf Windows gibt es
  kein tc/netem; zusätzlich QEMU `SPEEDOS_NET_DELAY=<µs>` → filter-buffer).
  ERGEBNIS: 56/60 Internet-Abrufe sauber (93 %, alle 4 Fehlschläge auf EINEM
  auffällig langsamen Server), LAN 10/10, unter 10–20 % Verlust 4/5 bzw. 2/3.
  Fehlerbild ehrlich: KEINE Deadlocks, KEINE falschen Daten, KEINE
  Socket-/TIME_WAIT-Lecks (0 Einträge danach) — ausschließlich TIMEOUTS durch
  krasse VERLANGSAMUNG unter Verlust (kein Fast-Retransmit, Out-of-Order wird
  verworfen, RTO-Backoff bis 8 s). Das vorher registrierte Kriterium (≥ 9/10)
  ist erfüllt ⇒ Reißleine NICHT gezogen (Kriterien werden nachträglich nicht
  verschoben). Cargo-Feature `tcp-eigen` (Standard an) markiert die
  Tausch-Stelle; ohne das Feature schlägt der Bau mit einer erklärenden
  `compile_error!`-Meldung fehl (es ist keine Alternative eingebunden).
  TESTMETHODIK: Das HARTE Gate liegt auf dem kontrollierbaren LAN-Server
  (`tests/netz_http.rs`); der Internet-Lauf ist Bericht + Grundschwelle —
  eine Testsuite darf nicht von fremden Servern abhängen. Nächster Hebel bei
  Bedarf laut Messung: Fast-Retransmit + niedrigerer RTO-Deckel, DANN erst
  SACK/smoltcp.
- **SERIE-5-ABSCHLUSS (Juli 2026) — Härtetests + unsafe-Audit + Serie-6-Naht:**
  Feature-Lücken geschlossen: DNS-RETRY (`dns::aufloesen` sendet bis 3× erneut,
  1,2 s/Versuch — ein verlorenes Datagramm scheitert nicht mehr alles);
  DHCP-LEASE-ERNEUERUNG (NetzKonfig trägt `lease_start_ms`, reine getestete
  `erneuerung_faellig`/`abgelaufen` bei T1=50 %, `dhcp::erneuerung_task` in
  main.rs). RX-DMA-HÄRTUNG (Audit): `virtio::net::empfange_frame` KLEMMT die
  gerätegemeldete Länge auf PUFFER_BYTES vor dem Slice — buggy/böses Gerät kann
  nie über den DMA-Puffer hinaus lesen. unsafe-Fläche: `src/netz/` = 0 unsafe
  (reine Byte-Logik), riskante Fläche nur in `virtio/net.rs` (Port-I/O + DMA,
  alle mit `# Safety`). TESTS (`tests/netz_abschluss.rs`): SPEICHER — 150
  Zyklen hole/nslookup/ping → 0 B Heap-Wachstum, 0 geleakte Frames/Sockets
  (Frame-Allocator byte-exakt stabil); ROBUSTHEIT — Kabel weg
  (`geraet::verlust_setzen(100)`), Server stumm, DNS tot, Gateway-MAC-Wechsel
  (ARP-Cache übernimmt) → alles saubere Fehler in Frist, kein Hänger/Panik;
  LEISTUNG — Durchsatz ~0,6 MiB/s (8-KiB-Fenster ohne Scaling + synchrones
  Pumpen/Segment), Ping-RTT ~0,2 ms. `tests/netz_shell.rs` fährt die
  Netz-Befehle end-to-end durch die Registry (die README-Beispielsitzung ist
  ihr Mitschnitt). SERIE-6-BESTANDSAUFNAHME: `docs/serie6-bestandsaufnahme.md`
  (User-Space braucht Ring 3 + Adressraum-Trennung + präemptiven Scheduler +
  ELF-Loader; APIC/MSI erzwingt erst SMP, NICHT User-Space; die Handle-/
  copy-in/out-APIs sind schon Syscall-fertig; kleinster erster Prozess = Ring-3-
  „Hallo Welt" per INT 0x80; TLS ist der Browser-Blocker).
- **RING 3 — der erste User-Mode (Serie 6, Juli 2026, `src/ring3.rs`):** Der
  Beweis, dass CPU-Code UNPRIVILEGIERT läuft und sauber zurückkommt — noch
  OHNE ELF, OHNE eigenen Adressraum, OHNE Scheduler (bewusst nur der
  Privilegienwechsel). GDT (`gdt.rs`) hat jetzt User-Code/-Data (DPL 3,
  Selektoren mit RPL 3 über `user_code_selektor()`/`user_data_selektor()`) und
  das TSS `privilege_stack_table[0]` = RSP0 (der Kernel-Stack, auf den die CPU
  bei Traps AUS Ring 3 umschaltet; 16-ausgerichtet wegen SSE im Dispatcher).
  Die IST-Nutzung für Double Fault bleibt unangetastet. USER-PAGES:
  `memory::map_page_benutzer` mappt PRESENT|WRITABLE|USER_ACCESSIBLE — WICHTIG,
  die CPU UND-verknüpft das U-Bit über ALLE Ebenen, deshalb setzt
  `benutzer_pfad_freischalten` U auch auf schon existierenden P4/P3/P2-
  Einträgen. ÜBERGANG: `iretq` (nicht sysretq — es braucht keine MSR-Einrichtung
  und keine Segment-Anordnung; wir bauen den Rahmen, den ein Trap aus Ring 3
  hinterlassen hätte). RÜCKWEG: INT 0x80 als Trap-Gate mit **DPL 3** (sonst
  dürfte Ring 3 es nicht auslösen), Einstieg ist nacktes `global_asm`, das ALLE
  General-Register als `TrapFrame` sichert, `syscall_dispatch` ruft und per
  `iretq` zurückkehrt. Syscalls: 0 = debug_print(ptr,len) über den GEPRÜFTEN
  `copy_in` (Dauerregel I), 1 = exit. KRITISCHE LEKTION: Der Kernel-Kontext
  wird per **setjmp/longjmp-Muster** (`kern_setjmp` + `kern_ring3_landing` in
  global_asm) gesichert/wiederhergestellt — ein einzelner Inline-asm-Block mit
  Sprung-Label FUNKTIONIERT NICHT, weil der Rückweg über einen Trap-Handler
  kommt, den der Compiler nicht als Kontrollfluss sieht (er verwaltet die
  Register dann falsch → Korruption/#GP). Neuer #GP-Handler fängt (wie der
  Page-Fault-Handler) User-Mode-Traps über `user_recovery()` ab. Shell:
  `ring3test` (+ `ring3test absturz`). Beweise in `tests/ring3.rs`:
  „Hallo aus Ring 3!" + Page Fault aus User-Mode aufgefangen + Ring 3 läuft
  danach weiter.
- **PRO-PROZESS-ADRESSRÄUME (Serie 6, Juli 2026, `src/adressraum.rs`) — echte
  Isolation:** Jeder Prozess bekommt eine EIGENE Level-4-Tabelle. GRUNDPRINZIP
  „Kernel spiegeln, User privat": Beim Anlegen werden die Kernel-P4-EINTRÄGE
  (8-Byte-Zeiger auf GETEILTE P3-Tabellen) hineinkopiert — nötig, weil ein
  Interrupt jederzeit mitten im User-Code zuschlägt und die CPU dabei NICHT
  CR3 wechselt. EHRLICHE ABWEICHUNG VOM LEHRBUCH: „die obere Hälfte spiegeln"
  gilt bei uns NICHT — bootloader_api 0.11 (`Mapping::Dynamic`) legt ALLES in
  die untere Hälfte (nachgemessen: P4[0] Frühmappings, P4[2,3] Kernel-Image,
  P4[4], P4[5] Physik-Komplettmapping, P4[6,7] Stack/BootInfo/Framebuffer,
  P4[136] Heap; die obere Hälfte ist KOMPLETT leer). Nur die obere Hälfte zu
  spiegeln gäbe einen sofortigen Triple Fault. Deshalb: gespiegelt wird JEDER
  belegte Kernel-Slot, privat ist genau **P4-Slot 1** (`USER_START` 512 GiB ..
  `USER_ENDE` 1 TiB) — der einzige freie Slot. WEIL wir P4-EINTRÄGE kopieren,
  sind spätere Kernel-Mappings INNERHALB schon gespiegelter Slots (z. B.
  heap_erweitern) automatisch überall sichtbar; nur ein komplett NEUER
  Kernel-Slot wäre es nicht — deshalb frischt `aktivieren()` den Spiegel
  jedes Mal auf. BESITZ/ABRISS: `eigene: Vec<PhysFrame>` führt Buch über P4,
  ALLE Zwischentabellen (der `BuchAllocator` notiert auch die, die `map_to`
  im Verborgenen anlegt) und alle Datenseiten; `Drop` schaltet nötigenfalls
  erst auf den Kernel zurück und gibt exakt diese Frames frei — Kernel-Frames
  sind nur gespiegelt, stehen nicht in `eigene`. API: `map_benutzer`
  (PRESENT|WRITABLE|USER_ACCESSIBLE, Frame VORHER GENULLT — sonst leckt der
  Inhalt des Vorbesitzers nach Ring 3), `bereich_mappen`, `stack_anlegen(top,
  seiten)` mit UNGEMAPPTER GUARD-PAGE darunter (Stack-Überlauf = Page Fault
  statt stiller Zerstörung), `schreiben`/`lesen` über das Physik-Komplett-
  mapping OHNE Aktivierung (das Muster des künftigen ELF-Loaders),
  `seiten_flags` (auch für INAKTIVE Räume — so testet man den „fremden
  Adressraum"), `aktivieren`/`adressraum::kernel_aktivieren` (CR3),
  `abreissen`. Der Page-Table-Läufer (`flags_in`/`uebersetzen_in`) geht die
  vier Ebenen VON HAND ab: lock-frei (Syscall-Pfad!), funktioniert für
  Tabellen, die nicht in CR3 stehen, und behandelt HUGE_PAGE korrekt (das
  Physik-Mapping des Bootloaders benutzt 2-MiB-/1-GiB-Seiten).
  `memory::map_page_benutzer` ist ERSATZLOS ENTFALLEN — User-Speicher darf es
  im Kernel-Adressraum nie geben; `memory::kernel_p4_frame()` hält die
  Kernel-P4 fest (der globale MAPPER schreibt IMMER dorthin, egal was in CR3
  steht). ring3.rs läuft jetzt komplett darüber: `prozess_aufsetzen` baut
  Adressraum + Code-Seite + Stack, `nach_ring3` wechselt CR3 und schaltet auf
  BEIDEN Rückwegen (exit UND Absturz) zurück. Neuer Syscall 2 = `zeit_ms(ptr)`
  — der erste, der copy-OUT benutzt. Shell: `adressraum`, `ring3test stack`.
  BEWEISE (`tests/adressraum.rs`, echt in QEMU): zwei Adressräume, dieselbe
  VA 0x8000100000, Inhalt „A" bzw. „B" je nach CR3; 5x anlegen/abreißen mit
  Spitzenbedarf 53 Frames → Frame-Bilanz BYTE-EXAKT null (auch nach Absturz
  und Stack-Überlauf); Guard-Page fängt den Push bei 0x80000fbff8.
- **PRÄEMPTIVER SCHEDULER (Serie 6, Teil 3, Juli 2026, `src/prozess.rs` +
  `src/scheduler.rs`) — der PIT wird zum Scheduler-Herz:** Entwurf VOR dem
  Code in `docs/scheduler-design.md`. DIE ENTSCHEIDUNG: Der kooperative
  Executor wird NICHT ersetzt, sondern ist SELBST ein schedulebarer Kontext —
  der KERNEL-PROZESS PID 0. Er steht als normaler Eintrag in der
  Prozess-Tabelle, bekommt seine Zeitscheibe wie jeder User-Prozess und
  multiplext INNERHALB seiner Scheibe weiter kooperativ (Compositor,
  netz_task, Shell-Sitzungen). Folgen: EIN Wechsel-Mechanismus ohne
  Sonderfälle; der LEERLAUF BLEIBT WIE ER WAR (PID 0 mit leerer Task-Queue
  schläft per hlt — er IST der Idle-Prozess, „nichts lauffähig" kann es nicht
  geben); die Oberfläche verhungert nicht. Verworfene Alternativen mit
  Begründung im Entwurf §1.
  DER KONTEXT-WECHSEL: Ein gesicherter Kontext ist EINE ZAHL — der RSP, an
  dem der Trap-Rahmen auf dem EIGENEN Kernel-Stack des Prozesses liegt (die
  CPU legt RIP/CS/RFLAGS/RSP/SS dort ab, bei Ring-3-Traps dank TSS.RSP0; der
  Assembler-Einstieg die 15 GP-Register dahinter). DREI EINSTIEGE
  (`timer_entry` = Präemption, `syscall_entry` = INT 0x80/freiwillig,
  `prozess_sterben` = Ring-0-Stub nach einem Fault), EIN AUSSTIEG:
  `schalte_auf_rahmen` (mov rsp, rdi / pop x15 / iretq) ist der EINZIGE
  Kontext-Lade-Punkt des Kernels. Jeder Dispatcher LIEFERT einen Rahmen
  ZURÜCK — denselben (kein Wechsel) oder den eines anderen Prozesses. Die
  VIER SCHRITTE eines Wechsels stehen in `wechsel_ausfuehren` beieinander:
  Kontext-RSP merken, CPU-Zeit abrechnen, CR3 wechseln, **TSS.RSP0 setzen**
  (der vergessene vierte — sonst überschreibt der nächste Ring-3-Trap den
  Kontext eines FREMDEN Prozesses; `gdt::rsp0_setzen`).
  PROZESS-START IST KEIN SONDERFALL: Ein neuer Prozess bekommt von Hand einen
  Trap-Rahmen ans obere Kernel-Stack-Ende — er sieht aus wie „schon gelaufen
  und gerade verdrängt". INVARIANTE 1: Der erste Wechsel zu ihm passiert IMMER
  im Timer/Syscall, also dort, wo PID 0s Kontext ohnehin gesichert wird
  („einplanen", nicht „starten"). TABELLE: festes `[Option<Prozess>; 8]` —
  KEIN Vec, der Timer liest sie und darf nicht allozieren; Kernel-Kontext
  sperrt IMMER mit `without_interrupts` (dann kann der Timer nicht feuern),
  der Interrupt nur mit `try_lock` (wartet NIE). BEENDETE PROZESSE WERDEN NIE
  IM INTERRUPT ABGERÄUMT (Drop nimmt memory-Locks + Heap) — der Timer
  markiert, `aufraeum_task` räumt ab, und zwar erst AUSSERHALB des Locks.
  Zeitscheibe 5 Ticks = 20 ms bei 250 Hz; `wechsel_entscheiden` ist eine REINE
  Funktion (4 Regeln, Fairness über 200 Runden nachgerechnet). Kernel-Stack je
  Prozess = 4 Seiten mit GUARD-PAGE (5 mappen, unterste aushängen).
  DAUERREGEL II JETZT PROZESS-WEISE: `user_recovery` erkennt „eingeplanter
  User-Prozess" und biegt den Rahmen auf den Sterbe-Stub um (Ring 0,
  Kernel-Stack des Sterbenden, IF aus) — der Prozess stirbt, Kernel und alle
  anderen Prozesse laufen weiter. Syscalls neu: 3 schlafen(ms) (erster
  BLOCKIERENDER, Zustand `Wartend`), 4 yield, 5 getpid; `yield` benutzt der
  EXECUTOR selbst (`sleep_if_idle` gibt bei lauffähigen Prozessen sofort ab
  statt zu hlt-en, und rechnet die FREMDZEIT aus der hlt-Messung heraus).
  Shell: `prozesse`, `prozess-start`, `prozess-stop`, `praemptionstest`;
  Task-Manager zeigt ZWEI Tabellen (Prozesse präemptiv / Kernel-Tasks
  kooperativ). SYSCALL-RANDBEDINGUNG (Entwurf §8): Ein Syscall darf nur Locks
  anfassen, die im Kernel AUSSCHLIESSLICH mit ausgeschalteten Interrupts
  gehalten werden (KONSOLE/FRAMEBUFFER/MANAGER/SERIAL/Blatt-Locks erfüllen
  das) — `fs::mit_fs` NICHT; in Teil 4 gelöst über `syscall::mit_vfs`
  (try_lock + Wartefenster), siehe den ABI-Eintrag unten. BEWEISE (`tests/scheduler.rs`, `tests/scheduler_executor.rs`):
  2 Zähler-Prozesse ohne jede Abgabe → je 26 Präemptionen aus Ring 3, 0
  Abgaben, verschränkte Ausgabe, CPU-Zeit auf 1 % gleich; Kontext-Sicherung
  gegen synthetische Registersätze (SAVE- UND RESTORE-Pfad) + TrapFrame-Layout
  per `offset_of!` (size == 160, sonst stimmt die C-ABI-Ausrichtung am `call`
  nicht); XMM0-15 über 24 Wechsel unverändert (der nackte Einstieg sichert nur
  GP — „Kernel ist fließkomma-frei" ist damit GEMESSEN); Koexistenz mit dem
  echten Executor; Frame-Bilanz byte-exakt null. LEKTION: Statische
  Kernel-Stacks MÜSSEN `static mut` sein — ohne `mut` landen sie in `.rodata`
  → Page Fault → Double Fault → TRIPLE FAULT ohne Meldung. Der alte
  Einzelschuss-Pfad (`ring3test`) und der `adressraum`-Befehl juggeln CR3 im
  Kontext von PID 0 und SPERREN deshalb die Planung
  (`scheduler::sperre_erhoehen/senken`) — die einzigen zwei Stellen.
- **DIE SYSCALL-ABI (Serie 6, Teil 4, Juli 2026, `src/syscall/`) — der Kernel
  wird GEBETEN:** DIE Tabelle mit jeder Nummer, jedem Argument, jeder Rückgabe
  und jedem Fehler steht in **docs/syscalls.md**. AB JETZT GILT: Eine Änderung
  daran ist eine bewusste ABI-ÄNDERUNG; `syscall::tests::test_abi_nummern_stabil`
  und `datei::tests::test_abi_strukturen_stabil` nageln Nummern und
  Struktur-Layouts fest. KONVENTION: Nummer in rax, Argumente in
  rdi/rsi/rdx/r10, **Fehlercode in rax (0 = Ok), Ergebnis in rdx** (zwei
  Register statt negativer errno, weil ein Ergebnis jeden u64-Wert annehmen
  darf); rax und rdx sind AUSGABE-Register und werden NICHT erhalten, alle
  übrigen GP-Register und XMM schon. Puffer/Pfade IMMER als (Zeiger, Länge),
  NIE nullterminiert — der Kernel sucht nie ein Terminator-Byte in fremdem
  Speicher. `Fehler` (25 Codes) ist die EINZIGE Außensicht: FsFehler/IoFehler/
  SocketFehler/DnsFehler/CopyFehler werden darauf ABGEBILDET (`Fehler::von_fs`
  usw.) — ein Prozess erfährt nie, welches Dateisystem unter ihm liegt
  (KeinSpeedFs/KeinFat32 → beide NichtKonfiguriert), und Zeiger-Fehler werden
  absichtlich GROB abgebildet (ob eine Adresse gemappt ist, ist Kernel-Zustand).
  PANICKT NIE — jede unbekannte Nummer und jedes kaputte Argument ist ein Code.
  PER-PROZESS-HANDLE-TABELLE (`syscall/handle.rs`): Ein Handle ist ein INDEX in
  die EIGENE Tabelle (max 32), das globale Socket-Handle verlässt den Kernel
  nie; 0/1/2 sind reserviert (Eingabe / AUSGABE = Bildschirm+seriell /
  DIAGNOSE = nur seriell — der getrennte Kanal verhindert, dass ein
  protokollierender Prozess den Compositor überschwemmt) und nicht schliessbar.
  Die Tabelle steckt IM `Prozess`, also schliesst ihr `Drop` beim Prozess-Ende
  ALLES automatisch (auch nach einem Absturz) — kein Pfad kann es vergessen.
  DIE NEUE LOCK-DISZIPLIN (docs/syscalls.md §8, die eigentliche Denkarbeit):
  Ein Syscall läuft mit ausgeschalteten Interrupts, deshalb (a) sind Locks, die
  der Kernel nur mit `without_interrupts` hält, gefahrlos benutzbar (wenn der
  Syscall läuft, hält sie niemand), aber (b) auf einen Lock WARTEN ist ein
  Hänger — `fs::mit_fs` ist genau so ein Lock. Lösung: `warte_fenster()`
  (Interrupts an, hlt, aus — NUR hier darf gewechselt werden, und nur OHNE
  Lock in der Hand; `hlt` mit Interrupts aus wäre Stillstand für immer) und
  `mit_vfs()` (`fs::vfs_versuchen` = try_lock + Wartefenster, 50 Versuche,
  dann `Belegt`). DIE NÄHTE HABEN GEHALTEN: Socket-API und VFS wurden 1:1
  durchgereicht (kein Zeichen Änderung in socket.rs); nachgeschärft werden
  musste NUR die SEMANTIK von `verbinde` — es blockiert jetzt mit 8-s-Frist und
  pumpt selbst, weil ein Ring-3-Programm `netz::pumpen()` nicht aufrufen kann.
  `empfange` bleibt bewusst nicht-blockierend (0 = noch nichts). TEST-VEHIKEL:
  `prozess::pruefstand_programm` — ein 75-Byte-Ring-3-Programm als
  FERNBEDIENUNG (liest Nummer+Argumente aus seinem Speicher, `int 0x80`, legt
  rax/rdx zurück, schläft dazwischen). Dadurch ist jeder Testfall gewöhnlicher
  Rust-Code, während der Aufruf ECHT unprivilegiert ist. BEWEISE
  (`tests/syscalls.rs`): alle drei Gruppen im Erfolgsfall inkl. copy-OUT
  (lese_at/stat schreiben in den Prozess, der Test liest es zurück); jede
  Angriffsvariante (Kernel-Zeiger, ungemappt, Seitengrenze, u64::MAX-Längen,
  relative/nicht-UTF-8-Pfade, fremde/geschlossene/reservierte Handles,
  falscher Typ); HANDLE-ISOLATION aus Ring 3 (zwei Prozesse, beide Handle 3,
  B probiert alle 32 Zahlen durch); LECK-TEST über exit UND Absturz
  (5 Sockets, kein `schliesse` → alle automatisch zu, Frame-Bilanz null).
  MESSFALLE (selbst hineingelaufen): `socket::schliessen` MARKIERT nur;
  `socket::anzahl()` sinkt erst nach dem nächsten `aufraeumen` (steckt in
  `oeffnen`/`bedienen`) — vor einer Leck-Messung also `netz::pumpen()`.
- **ECHTE PROGRAMME (Serie 6, Teil 5, Juli 2026, `src/elf.rs` + `userland/`)
  — SpeedOS fuehrt fremden Code aus:** Der ELF64-Lader nimmt NUR statisch
  gelinkte `ET_EXEC` fuer x86-64; dynamisches Linken ist BEWUSST draussen
  (ET_DYN/PT_INTERP -> eigene Fehler, damit ein versehentlicher PIE-Build
  sofort erkennbar ist). HALTUNG: JEDE ZAHL IN DER DATEI IST EINE BEHAUPTUNG
  EINES FREMDEN — Dateigrenzen mit checked_add, jedes Segment VOLLSTAENDIG im
  Programm-Bereich (`elf::IMAGE_START..IMAGE_ENDE` = USER_START .. +16 MiB;
  Kernel-Adressen/Nullseite/obere Haelfte fallen raus, BEVOR die erste Seite
  gemappt wird), Groessen gedeckelt, Ausrichtung, KEINE ueberlappenden
  Segmente auf SEITEN-Ebene (zwei Segmente in einer Seite muessten sich die
  Rechte teilen -> W^X waere aushebelbar), Einsprung muss in ausfuehrbarem,
  aus der DATEI geladenem Code liegen. `elf::pruefen` ist eine REINE Funktion
  auf `&[u8]`: kein Adressraum, kein Lock, KEIN unsafe, panickt NIE. `laden`
  mappt erst NACH der vollstaendigen Pruefung — und zwar mit den ENDGUELTIGEN
  Rechten, weil der Inhalt ueber `AdressRaum::schreiben` (Physik-Mapping)
  hineinkommt; es gibt also kein Zeitfenster, in dem Code-Seiten schreibbar
  waeren. `.bss` (memsz > filesz) wird NICHT eigens genullt: frisch gemappte
  Frames sind ohnehin genullt (Datenleck-Schutz) — die Garantie faellt ab.
  W^X IN HARDWARE: `memory::nx_aktivieren()` (EFER.NXE, in `lib::init` VOR
  jedem User-Mapping) + `adressraum::Rechte` mit GETRENNTEN Flag-Saetzen fuer
  Blatt und Zwischentabellen (NX auf einer Zwischentabelle wuerde ALLES
  darunter unausfuehrbar machen; die Zwischenebenen bleiben permissiv, das
  Blatt kann nur wegnehmen). ACHTUNG: Ohne NXE ist Bit 63 RESERVIERT und
  wuerde JEDEN Zugriff zum Page Fault machen — deshalb setzt
  `Rechte::seiten_flags` NO_EXECUTE nur, wenn `memory::nx_aktiv()`. Der
  User-Stack ist jetzt ebenfalls NX. PROZESS-LAYOUT (docs/syscalls.md §9):
  Image ab USER_START (max 16 MiB), 16 MiB ungemappte LUECKE, dann Guard-Page
  + 16 Seiten Stack bis `prozess::ELF_STACK_OBEN`. ARGUMENTE: argc in rdi,
  argv in rsi als Feld von `ArgEintrag{zeiger,laenge}` — NIE nullterminiert
  (dieselbe Regel wie die ganze ABI); max 16 Argumente à 255 B, zusammen 2 KiB.
  `ProzessEnde` (Beendet(code)/Abgestuerzt=139/Gestoppt=143) wird an ALLEN
  DREI Beendigungs-Stellen gesetzt; `scheduler::warten_auf(pid, frist)` wartet
  aus Kernel-Kontext und ERNTET den Prozess selbst (der Aufraeum-Task kommt
  waehrend eines synchronen Shell-Befehls nicht dran — dafuer steht waehrend
  `starte` auch der Compositor, wie bei `praemptionstest`).
  **userland/ ist die ANDERE SEITE der Grenze:** eigener Workspace, libspeed
  (Syscall-Wrapper, print!, Datei/Socket, Panic-Handler, `_start` via
  `hauptprogramm!`-Makro) + hallo/kopiere/netzhole. KEINE Kernel-Abhaengigkeit
  — die ABI-Konstanten stehen dort NOCH EINMAL, und das ist Absicht: Eine ABI
  ist ein VERTRAG, kein geteilter Header. Dasselbe Target
  x86_64-unknown-none (-sse/+soft-float!) wie der Kernel, weil der
  Kontext-Wechsel nur GP-Register sichert — ein Programm mit XMM bekaeme
  stillschweigend falsche Zahlen. BAU-LEKTIONEN (in userland/.cargo/config.toml
  und build.rs dokumentiert): (1) `relocation-model=static` erzeugt absolute
  32-Bit-Adressen -> bei Ladeort 512 GiB hunderte `R_X86_64_32S out of range`;
  die Voreinstellung `pic` (RIP-relativ) laeuft an jeder Adresse. (2) Ohne
  `--no-pie` entsteht ET_DYN UND der PIE-Link zieht .dynsym/.rela.dyn/.dynamic
  als WAISEN-Sektionen hinter .text — sie zerlegen die ausgerichtete
  Segment-Folge, und zwei PT_LOADs landen in einer Seite. (3) `speedos.ld`
  richtet JEDE Sektion (auch .bss) auf 4096 aus, sonst teilen sich Segmente
  eine Seite. BUILD-INTEGRATION: Das kernel-`build.rs` baut userland/ mit
  (EIGENER Ziel-Baum — sonst wartet der innere cargo auf die Dateisperre des
  aeusseren; geerbte RUSTFLAGS/CARGO_*-Variablen werden weggeraeumt), die ELFs
  wandern per `include_bytes!` ins Kernel-Image, und `programme::installieren()`
  schreibt sie beim Boot nach /platte/programme (byteweiser Vergleich, nur bei
  echter Aenderung). WARUM eingebettet statt ins Disk-Image: Ein Host-seitiger
  SpeedFS-Writer waere eine dauerhafte Doppelpflege des eigenen Formats; so
  reisen die Programme mit `cargo run`, `cargo test` UND `cargo image` mit.
  Notausgang `SPEEDOS_OHNE_USERLAND=1`. Shell: `starte <programm> [args]`
  (Exit-Code-Anzeige, Kurzname ohne Pfad), `programme`, `elfinfo`; Explorer-
  Doppelklick entscheidet an den ERSTEN BYTES (`prozess::ist_programm`), nicht
  an einer Endung — unser VFS kennt keine. MEILENSTEIN (`tests/programme.rs`):
  `starte /platte/programme/netzhole http://example.com` -> 571 Byte von
  example.com, geholt von einem Ring-3-Programm von der eigenen Platte ueber
  den eigenen Netz-Stack.
- **PROZESS-ZUSAMMENSPIEL (Serie 6, Teil 6, `src/pipe.rs` + Warte-Modell):**
  PIPES: `netz::puffer::Ringpuffer` WIEDERVERWENDET (nicht nachgebaut — zwei
  Ringpuffer waeren zwei Stellen fuer denselben Off-by-one) plus zwei
  Besitz-ZAEHLER je Ende. ZAEHLER statt Flags, weil ein Ende mehrere Besitzer
  hat (die Shell haelt es, waehrend sie es dem Kind gibt); ein Flag waere je
  nach Schliess-Reihenfolge ein Leck ODER ein zu frueh gemeldetes Dateiende.
  Semantik: voll -> Schreiber blockiert (Gegendruck), leer+Schreiber da ->
  Leser blockiert, leer+kein Schreiber -> `lese` liefert 0 = DATEIENDE, kein
  Leser mehr -> `schreibe` liefert `Abgebrochen` (EPIPE). PIPES ist ein
  BLATT-Lock; der Timer fragt `lesbar`/`schreibbar` mit try_lock ab.
  BLOCKIERENDE SYSCALLS — DIE NEUSTART-MECHANIK: Ein Syscall haelt NICHT
  mitten drin an (der gesicherte Kontext ist der Trap-Rahmen am EINGANG; beim
  Umschalten landet die CPU per iretq hinter dem `int 0x80`, der Rust-Stack
  des halben Syscalls waere weg). Stattdessen `rip -= 2` (Laenge von
  `int 0x80` = CD 80) und Prozess schlafen legen -> der Syscall laeuft von
  vorn. EISERNE REGEL: Bis zum `Blockieren` darf NICHTS veraendert worden
  sein (sonst passiert es beim Neustart zweimal), und rax/rdx duerfen nicht
  beschrieben werden (sie tragen noch Nummer und Argument 2).
  GEWECKT WIRD DURCH NACHSEHEN, NICHT DURCH ANSTOSSEN: `prozess::Warteauf`
  (Zeit/Kind/PipeLesen/PipeSchreiben) steht im PCB, und `warter_wecken` im
  Timer prueft je Tick die Bedingung. Anstossen aus dem schreibenden Prozess
  waere schneller — und eine Lock-Kette quer durch den Kernel, aus einem
  Syscall heraus, in dem man nicht warten darf. Preis: max. 1 Tick (4 ms).
  **UEBERHOLT IN SERIE 7, TEIL 0** (siehe „SOFORTIGES WECKEN" weiter unten):
  Angestossen wird jetzt doch — die Lock-Kette wurde vermieden, indem der
  Weckruf UNTER dem Blatt-Lock nur ermittelt und DANACH ausgeloest wird, und
  indem `wecken` nie auf einen Lock wartet. Das Nachsehen im Timer BLEIBT
  als Sicherheitsnetz; der Preis von 4 ms war gemessen der Faktor 1000 beim
  Pipe-Durchsatz.
  ELTERN/KIND OHNE ZOMBIES — die UMKEHRUNG des Unix-Modells: Nicht der
  Kind-Eintrag bleibt liegen, sondern das ERGEBNIS wandert beim Ende in
  `Prozess::kinder_enden` des ELTERNTEILS (FESTES Feld, keine Allokation im
  Syscall-Pfad; Ueberlauf verwirft das AELTESTE), und das Kind verschwindet
  sofort vollstaendig. Stirbt der Elternteil, verfallen ungelesene Ergebnisse
  mit ihm — kein Waisen-Aufsammler. `warte` auf ein nicht existierendes Kind
  ist ein FEHLER (warten waere ein Haenger fuer immer); ein zweites `warte`
  auf dasselbe Kind ebenso. `scheduler::ende_vermerken` ist DIE EINE STELLE,
  an der ein Prozess endet (exit/Absturz/Stopp) — vorher drei Kopien, und mit
  der Eltern-Beziehung kam ein vierter Schritt dazu.
  ACHTUNG PIPE-DATEIENDE: Ein Pipe-Ende haengt an der Handle-Tabelle und
  faellt erst beim ABRAEUMEN des beendeten Prozesses (Freigeben darf nicht im
  Interrupt passieren). Wer auf ein Dateiende wartet, muss also `aufraeumen()`
  mitlaufen lassen — die Shell-Pumpschleife und die Tests tun das; im Betrieb
  erledigt es der Aufraeum-Task (250 ms).
  ACHTUNG EXIT-CODE: `aufraeumen()` LOESCHT den Tabelleneintrag und damit den
  Exit-Code. Wer in einer Schleife aufraeumt, muss `scheduler::ende_abfragen`
  VORHER einsammeln (genau dieser Fehler liess die Shell „laeuft noch" fuer
  laengst beendete Prozesse melden).
  SHELL: `starte` gibt dem Kind eine PIPE als Handle 1 und liest selbst heraus
  — die Ausgabe ist damit ein DATENSTROM statt eines Kernel-Seiteneffekts, und
  daraus wird `a | b` (`MAX_PIPELINE` Stufen). Gelesen wird WAEHREND der
  Laufzeit; „erst warten, dann abholen" waere ein Deadlock ab 4 KiB Ausgabe.
  Die Shell schliesst nach dem Start ihre EIGENEN Kopien aller weitergegebenen
  Enden — sonst bekaeme die naechste Stufe nie ein Dateiende (der Klassiker).
  STRG+C geht NICHT in die Tasten-Queue (die Shell steckt beim Warten in einem
  synchronen Befehl und kaeme nicht heran): Der Eingabe-Router setzt ein Flag
  PRO SITZUNG (`sitzung::abbruch_anfordern`), das die Pumpschleife abfragt.
  HANDLE-WEITERGABE: `handle::ERBE_KEINS` ist `u64::MAX` und bewusst NICHT 0
  — 0 ist ein gueltiges Handle.
- **SERIE-6-ABSCHLUSS (Juli 2026) — der Angreifer, die Zahlen, die Weiche:**
  DER SICHERHEITS-PASS (`userland/angreifer` + `tests/sicherheit.rs`) ist der
  wertvollste Test des Projekts: ein ABSICHTLICH BOESWILLIGES Programm IM
  REPOSITORY, das systematisch ausbrechen will. Es hat eine echte Luecke
  gefunden: Bis dahin hatten nur #PF und #GP einen IDT-Handler — ein
  Ring-3-`ud2` (#UD) oder eine Division durch Null (#DE) traf auf einen
  Vektor OHNE Eintrag und eskalierte zum Double Fault, der den Kernel
  anhaelt. EIN `div rax, 0` in einem unprivilegierten Programm haette also
  SpeedOS gestoppt. Behoben, indem die KLASSE geschlossen wurde: Jede aus
  Ring 3 erreichbare CPU-Exception hat jetzt einen Handler (Makro
  `user_exception_handler!` in interrupts.rs), alle laufen durch dieselbe
  `user_recovery`. REGEL AB JETZT: Wer einen neuen Trap-Vektor benutzt, gibt
  ihm einen Handler mit user_recovery — ein Vektor ohne Eintrag ist ein
  Kernel-Stopp, den ein User-Programm ausloesen kann.
  MESSZAHLEN (QEMU/WHPX 4,2 GHz, Bestwert aus 7 Runden): Syscall-Roundtrip
  aus Ring 3 **60-70 ns**, Kontext-Wechsel (yield-Roundtrip) **~450 ns**,
  Prozess-Start **6-11 us**, Pipe-Ringpuffer allein **241 MiB/s**, Pipe
  Prozess->Kernel nur **199 KiB/s**. DIE LETZTE ZAHL IST DIE WICHTIGE: Der
  Unterschied ist NICHT das Kopieren, sondern die WECK-LATENZ — 4 KiB Pipe /
  20 ms Scheduling-Runde. Hebel: groesserer Puffer oder sofortiges Wecken
  statt der Timer-Pruefung. Beim Syscall waere SYSCALL/SYSRET statt
  `int 0x80` der offensichtliche spaetere Gewinn (spart IDT+TSS, braucht
  MSR-Setup).
  MESSFALLEN, beide selbst hineingelaufen: (1) Kontext-Wechsel misst man
  NICHT, waehrend der Messende `hlt`-t — dann misst man die Tick-Rate (war um
  Faktor 1000 daneben); der Messende muss selbst mit-`abgeben`. (2) Der
  Kernel-Log-Puffer (protokoll.rs) waechst mit jeder Ausgabe bis 64 KiB —
  Speicher-Bilanzen muessen ihn ueber `protokoll::puffer_bytes()`
  herausrechnen und BENENNEN, sonst faerbt er den Test falsch rot.
  BEKANNTE, AUSGERECHNETE UNSCHAERFE: `memory::allocate_pages` vergibt
  virtuellen Raum monoton; alle 512 Seiten bleibt eine P1-Tabelle im
  Kernel-Adressraum zurueck (~1 Frame je 100 Prozesse). Kein Prozess-Leck —
  der Speicher-Test rechnet die Schranke aus, statt die Bilanz aufzuweichen.
  Behebung waere ein Freilisten-Allocator fuer virtuelle Bereiche.
  DOKUMENTE: `docs/unsafe-audit-serie6.md` (jeder unsafe-Block mit seiner
  INVARIANTE; fuer copy_in/out einzeln aufgeschluesselt, welche Pruefstufe
  welche Anforderung von copy_nonoverlapping herstellt; 0 `unsafe fn` in der
  Prozess-Schicht; elf.rs und pipe.rs sind unsafe-FREI),
  `docs/serie7-bestandsaufnahme.md` (TLS/RNG/Zertifikate/Fenster-Naht).
  SERIE-7-VORENTSCHEIDUNGEN, hier registriert: **Eigenbau-TLS wird NICHT
  gebaut** — anders als bei TCP gibt es kein messbares Kriterium, an dem man
  eine Reissleine ziehen koennte (ein TLS-Bug ist STILL). Stattdessen rustls
  (no_std) mit RustCrypto-Provider, IM USER-SPACE (nicht im Kernel — ein
  Fehler in 30k Zeilen Fremdcode soll den Kernel nicht treffen). Der Kernel
  bekommt dafuer genau EINEN neuen Syscall: `zufall`. Voraussetzung und
  erster Schritt ist `src/zufall.rs` (RDSEED/RDRAND + Interrupt-Entropie +
  ChaCha20-DRBG) — es gibt heute KEINEN Zufallsgenerator im System.
- **SOFORTIGES WECKEN + RESCHEDULE-PUNKT (Serie 7, Teil 0) — die Weck-Latenz
  ist weg:** Die Regel „NACHSEHEN STATT ANSTOSSEN" aus Teil 6 gilt NICHT mehr
  als einziger Weg: `scheduler::wecken(Warteauf)` macht Warter SOFORT
  lauffaehig (Pipe gefuellt/geleert/geschlossen, Kind beendet). Der TIMER
  BLEIBT SICHERHEITSNETZ — `wecken` nimmt `try_lock` und darf folgenlos
  aussetzen, `warter_wecken` prueft weiter jeden Tick nach; die Zeit-Bedingung
  (`schlafe`) hat ohnehin keinen Anstosser (`weck_passt` laesst `Zeit` von
  KEINEM Ereignis wecken). LOCK-FALLE: Der Weckruf wird UNTER dem Blatt-Lock
  nur ERMITTELT und DANACH ausgeloest — der Timer nimmt TABELLE→PIPES, aus
  `mit_pipes` heraus zu wecken waere PIPES→TABELLE = ABBA.
  DIE ENTSCHEIDUNG (Begruendung in scheduler.rs): NUR MARKIEREN reicht nicht
  (der Wecker haelt seine Scheibe), DIREKTE UEBERGABE an den Geweckten
  hungert aus (Ping-Pong-Paar sperrt Dritte aus) — gewaehlt ist ein
  RESCHEDULE-PUNKT ueber die NORMALE Round-Robin-Wahl: `wechsel_entscheiden`
  mit `freiwillig = true`, Ziel ist immer der naechste Lauffaehige HINTER dem
  aktuellen. Weil `naechster_lauffaehig` zyklisch sucht, ist Aushungern
  STRUKTURELL ausgeschlossen; die FAIRNESS-BREMSE (`SOFORT_MAX_PRO_TICK = 16`
  je Tick, danach nur noch markieren) deckelt nicht das Verhungern, sondern
  den UMSCHALT-AUFWAND (16 x 450 ns je 4 ms = 0,18 % CPU). Zwei Punkte:
  `syscall::umplanen_falls_noetig` (nur wenn der Syscall nicht schon selbst
  gewechselt hat) und `scheduler::umplanen_im_kernel` (aus
  `zeit::warte_auf_interrupt`). `Grund::Umplanung` zaehlt bewusst NICHT als
  `abgaben` — sonst waeren die Praemptions-Beweise aus Teil 3 („0 Abgaben")
  ploetzlich falsch. ALT/NEU-Schalter fuer Messungen:
  `scheduler::sofort_wecken_setzen`, `pipe::kapazitaet_setzen`.
  PIPE-PUFFER: `pipe::STANDARD_KAPAZITAET` = **64 KiB** (war 4 KiB), zur
  Laufzeit einstellbar (512 B .. 256 KiB), `anlegen_mit` fuer eine
  ausdrueckliche Groesse; eine bestehende Pipe aendert ihre Groesse NIE.
  Begruendung: Die Puffergroesse IST die Stueckgroesse je Weckruf
  (Durchsatz <= Kapazitaet / Weck-Latenz), und 64 KiB == `MAX_PUFFER`, also
  fuellt EIN `schreibe` eine leere Pipe. ZAHLEN (`tests/wecken.rs`, ALT/NEU im
  selben Lauf): Weck-Latenz 3558 -> **17 us**, Pipe Prozess->Kernel 203 KiB/s
  -> **202 MiB/s**, Prozess->Prozess 101 KiB/s -> **199 MiB/s** (= roher
  Ringpuffer, es begrenzt jetzt das Kopieren), Socket-`sende` 24 MiB/s /
  40 us. SOCKETS sind NICHT betroffen und das ist nachpruefbar: `empfange`
  ist laut ABI nicht-blockierend, es WARTET also nie ein Prozess auf einen
  Socket — die Maschinerie ist allgemein, ein spaeteres blockierendes
  `empfange` braucht nur einen `wecken`-Aufruf im Zustell-Pfad.
- **DIE hlt-FALLE IM SYSCALL (Serie 6, Teil 5) — `zeit::warte_auf_interrupt()`:**
  `int 0x80` geht durch ein INTERRUPT-Gate, im Syscall sind Interrupts also
  AUS. Ein blankes `hlt` haelt die CPU dann FUER IMMER an (nichts kann sie
  wecken) — ohne Meldung, ohne Panik. Genau daran haengte sich der Meilenstein
  auf: `dns::aufloesen` (und dhcp/http) hatten `hlt()` in ihren synchronen
  Warteschleifen, korrekt fuer Kernel-Kontext, toedlich aus Ring 3.
  `zeit::warte_auf_interrupt()` prueft `are_enabled()` und oeffnet bei aus-
  geschalteten Interrupts ein Wartefenster (`enable_and_hlt` + `disable`).
  REGEL: Jede synchrone Warteschleife, die AUCH aus einem Syscall laufen kann,
  benutzt diese Funktion — und haelt dabei KEINEN Lock (im Wartefenster darf
  der Scheduler verdraengen). SEIT SERIE 7, TEIL 0 tut sie noch etwas: Kann
  ein anderer Prozess laufen, GIBT SIE AB statt zu schlafen. `hlt` heisst „ich
  habe nichts zu tun" — wer auf Daten eines anderen PROZESSES wartet und dabei
  schlaeft, blockiert ihn, denn die verschlafene Zeitscheibe laeuft trotzdem
  20 ms. Das war zugleich die Messfalle des Serie-6-Abschlusses.
- **FAT32-Treiber (Juli 2026, `src/fs/fat32.rs`) — NUR LESEN:**
  SpeedOS liest fremde FAT32-Medien ("der USB-Stick"), schreibt sie
  aber NIE (jeder Schreib-Weg -> `IoFehler::NurLesen`). Kein/kaputtes
  FAT wird sauber mit `FsFehler::KeinFat32` abgelehnt, NIE per Panik:
  Die BPB-Validierung ist eine reine, unit-getestete Funktion, die
  JEDEN Wert prüft (Signatur 0x55AA, bytes_pro_sektor Zweierpotenz +
  Vielfaches der Gerätesektorgröße, sektoren_pro_cluster Zweierpotenz,
  FAT16-Kennzeichen ausgeschlossen, Layout passt ins Gerät,
  >= 65525 Cluster = echtes FAT32). Der Treiber liest die ganze FAT
  einmal in den RAM (ein u32/Cluster); Cluster-Ketten haben einen
  SCHLEIFEN-SCHUTZ (Ring in kaputter FAT -> Geraetefehler, nie
  hängen). VFAT-LFN: die 32-Byte-Zusatzeinträge (UTF-16-LE, Positionen
  1/14/28) werden per Prüfsumme dem Kurznamen zugeordnet und zu
  unserem String zusammengesetzt — daher stimmen die Umlaute
  (char::decode_utf16). FAT-Zeitstempel -> zeit-Epoche (reine
  Funktion). Läuft wie SpeedFS nur auf dem BlockDevice-Trait
  (RamDisk-Tests via SPARSE Test-Disk, weil 65525 Cluster ~34 MiB
  wären; ATA in Produktion) und nutzt RefCell (VFS-Mutex
  serialisiert). Runner: tools/fat32_image_erzeugen.py baut
  speedos-fat.img — bevorzugt mit HOST-mtools (mformat/mcopy), sonst
  eigener Python-FAT32-Writer; Secondary Master, gitignored. Mount:
  fs::fat_automounten() beim Boot -> /fat (nur lesen); platten zeigt
  den Typ. Explorer graut Schreib-Aktionen auf Nur-Lese-Pfaden aus
  (fs::pfad_beschreibbar über die neue Trait-Methode
  FileSystem::ist_beschreibbar(pfad); FileSystem::typ_name für
  platten/Speicher-Seite; fs::mount_uebersicht). ACHTUNG: main.rs
  lässt den Heap VOR den Auto-Mounts wachsen (heap_erweitern(256)) —
  der FAT-Treiber alloziert ~256 KiB für die FAT, bevor
  desktop_starten den Heap groß macht.
- **SpeedFS (Juli 2026, `src/fs/speedfs.rs`) — das eigene Disk-
  Dateisystem:** Das On-Disk-Format ist in docs/speedfs-format.md
  SPEZIFIZIERT (Dokument vor Code; Format-Änderung = erst Doku,
  dann Version+1). Kurzform: Superblock "SPFS" | Block-Bitmap |
  Inode-Tabelle | Daten, 4-KiB-Blöcke, alles Little-Endian; Inodes
  128 B mit 22 direkten + 1 einfach-indirektem Zeiger (max. Datei
  ~4,09 MiB); Verzeichnisse = Byte-Listen [Inode u32|Länge u8|Name].
  KEIN JOURNAL — Konsistenz über die Schreib-Reihenfolge (§7 im
  Format-Doc): Belegen vor Benutzen, Inhalt vor Verweis, Entkoppeln
  vor Freigeben; jeder Op hat EINEN sektor-atomaren Commit-Punkt,
  Absturz hinterlässt höchstens Block-Lecks, nie falsche Zeiger.
  BLOCK-CACHE: Write-Through (ENTSCHEIDUNG: einfach und ehrlich —
  Code-Reihenfolge == Platten-Reihenfolge, die Absturz-Analyse gilt
  ohne Zusatzannahmen; Write-Back + geordnetes Flush wäre schneller
  und ist Serie-5-Stoff). SpeedFS kennt NUR das BlockDevice-Trait
  (läuft identisch auf RamDisk-Tests und ATA); Innen-Mutabilität
  über RefCell (kein Lock — der VFS-Mutex serialisiert schon).
  MOUNT-TABELLE (fs/mod.rs): Aus dem Root-Mount wurde
  `MountTabelle` (Wurzel-RamFs + Präfix-Mounts wie /platte), die
  SELBST FileSystem implementiert und per Pfad-Präfix routet —
  mit_fs() und ALLE Befehle/Apps blieben unverändert. rename über
  die Mount-Grenze -> FsFehler::MountGrenze; fs::verschieben(_rekursiv)
  fällt dann auf kopieren+löschen zurück. fs::mounten legt den
  Mount-Punkt im Wurzel-FS an; fs::unmounten synct ERST (bei Fehler
  bleibt gemountet). ata::daten_platte() = besitzbares
  BlockDevice-Handle, das an die Registry delegiert (Lock-Ordnung
  VFS -> LAUFWERKE, LAUFWERKE bleibt Blatt). Shell: mkfs.speedfs
  (nur mit Argument JA, nie bei gemountetem /platte), mount, umount.
  ACHTUNG TESTS: tests/ata_platte.rs schreibt Roh-Sektoren seit
  SpeedFS nur noch ans PLATTEN-ENDE (Sektor 130500+), weil vorne
  der Superblock liegt; tests/speedfs_platte.rs führt den
  Persistenz-Beweis mit einer echten Datei über das VFS.
  ERWACHSEN-PASS (Juli 2026): (1) Explorer-Ausschneiden+Einfügen
  läuft über fs::verschieben_rekursiv = echtes rename (das alte
  kopieren+löschen ist tot; nur die Mount-Grenze kopiert noch —
  im VFS-Fallback). (2) sync-KETTE: fs::sync -> alle Mounts ->
  BlockDevice-Flush (ATA 0xE7); der Shell-Befehl sync,
  SpeedText-Speichern und einstellungen::speichern rufen sie —
  "gespeichert" heißt "auf dem Medium", ein sync-Fehler wird wie
  ein Schreibfehler angezeigt. (3) pruefe.speedfs = unser fsck
  (SpeedFs::pruefen, Format-Doc §10): Baum-Scan + Bilanz gegen
  Bitmap/Inode-Tabelle; LECKS (belegt-unreferenziert, der
  erlaubte Absturz-Schaden) sind mit --repariere reparierbar,
  DEFEKTE werden NUR gemeldet (nie automatisch "repariert" —
  das würde Daten zerstören); Doppel-Eintrag nach rename-Absturz
  ist ein BEFUND, kein Defekt. Läuft nur ungemountet. (4) Der
  FOLTER-TEST (test_speedfs_folter_absturz) schneidet die
  Schreibfolge per AbsturzDisk (verwirft Writes nach Budget N —
  Präfix-Semantik wie echter Stromausfall) an JEDER Stelle ab:
  Lecks erlaubt, Defekte nie — der maschinelle Beweis der §7-
  Ordering-Disziplin.
- **ATA-PIO-Treiber (Juli 2026, `src/ata.rs`) — die erste echte
  Platte:** PIO gepollt über die Legacy-Ports des Primary-Kanals
  (0x1F0/0x3F6, fest verdrahtet — bewusst KEINE PCI-Enumeration),
  Kanal-Interrupts aus (nIEN). Jedes Status-Polling hat einen
  TSC-Timeout (`IoFehler::Zeitueberschreitung` — nie endlos auf
  Hardware warten; leerer Steckplatz wird am Status 0x00/0xFF sofort
  erkannt). IDENTIFY liefert Modell/Kapazität (Dekoder = reine,
  unit-getestete Funktionen; Modell-Bytes paarweise vertauscht!).
  LBA28 = max. 128 GiB und 256 Sektoren pro Kommando — der Treiber
  zerlegt größere Aufträge selbst; LBA48 wäre rein additiv.
  FLUSH CACHE (0xE7) ist das sync(). Implementiert `BlockDevice`;
  die Laufwerks-Registry LAUFWERKE ist ein BLATT-Lock (nur aus
  Task-Kontext). Der Runner hängt speedos-daten.img (64 MiB,
  persistent, Projekt-Root, gitignored) als Primary Slave an —
  ata::init() läuft in main.rs NACH zeit::init() (Timeouts brauchen
  die TSC-Zeit). Shell: `platten` + `blocktest <lba>` (Hexdump).
  tests/ata_platte.rs führt den PERSISTENZ-BEWEIS: Generationen-
  Muster in Sektor 1000 überlebt QEMU-Neustarts.
- **VFS-Abstraktion (Juli 2026, erweitert um die Serie-4-Naht):** Alle
  Dateisysteme implementieren das Trait `FileSystem` in `src/fs/mod.rs`
  (lesen, schreiben, liste, mkdir, loeschen, node_typ, read_at, write_at,
  stat, rename, sync — absolute, normalisierte Pfade mit `/`). Shell-Befehle
  und Kernel greifen NIE auf eine konkrete Implementierung zu, sondern nur
  über `fs::mit_fs()` auf das global gemountete VFS. Erste Implementierung ist
  `RamFs` (`src/fs/ramfs.rs`, in-memory); FAT32 und ein eigenes
  Disk-Dateisystem sollen später exakt dieselbe Schnittstelle bedienen —
  dann wird nur das gemountete Dateisystem ausgetauscht, kein Befehl ändert sich.
  API-ENTSCHEIDUNGEN der Erweiterung: read_at liefert die GELESENE Anzahl
  (0 am/hinter dem Dateiende = kein Fehler, POSIX-read-Semantik); write_at
  legt fehlende Dateien an und füllt Lücken hinterm Dateiende mit Nullbytes;
  stat liefert `Metadaten` (Typ, Größe, erstellt/geaendert als Sekunden seit
  1.1.2000 — zeit-Epoche, Anzeige über einstellungen::stempel_text mit dem
  Systray-Uhr-Offset); rename ist die ATOMARE Primitive (erst komplett
  validieren, dann entnehmen+einfügen; Ziel-DATEI wird ersetzt,
  Ziel-VERZEICHNIS ist Fehler, Ziel im eigenen Teilbaum ist Fehler,
  Zeitstempel wandern mit) — fs::verschieben/verschieben_rekursiv laufen
  darüber (kein kopieren+löschen mehr; bei mehreren Mounts braucht die
  FS-Grenze wieder einen Kopier-Fallback); sync() drückt Schreib-Caches aufs
  Medium (RamFs: ehrliches No-Op; einstellungen::speichern ruft es bereits).
  `FsFehler::Io(IoFehler)` transportiert Geräte-Fehler durchs ganze VFS.
- **BlockDevice-Naht (Juli 2026, `src/fs/block.rs`):** JEDER Massenspeicher-
  Treiber (RamDisk heute, AHCI/NVMe/virtio später) implementiert das schmale
  Trait `BlockDevice` (sektor_groesse, anzahl_sektoren, lese_sektoren,
  schreibe_sektoren, sync — alles `Result<_, IoFehler>`). SEKTOR-Adressierung
  (LBA), Puffer = Vielfaches der Sektorgröße (validiert, nie still
  abgeschnitten). Disk-Dateisysteme reden NUR mit BlockDevice, nie mit einem
  konkreten Treiber; die `RamDisk` (Vec-basiert) ist Referenz-Implementierung
  und Test-Unterbau — die Naht existiert BEWUSST vor dem ersten Treiber.
- **Grafik-Architektur (Juli 2026):** `framebuffer.rs` = Double Buffering
  (Back-Buffer aus `memory::allocate_pages`, `present()` kopiert als Block,
  `hochscrollen()` = memmove im Back-Buffer, NIE neu rendern). Font:
  noto-sans-mono-bitmap (vorgerastert, Latin-1 für Umlaute). `konsole.rs` =
  FramebufferKonsole (Raster, Farben = Obsidian-Aurora-Palette, Software-
  Cursor als async Blink-Task über `zeit::warte_ms`). Lock-Ordnung:
  KONSOLE vor FRAMEBUFFER, beide nur mit Interrupts aus. Niemals direkt
  in den echten Framebuffer zeichnen — immer Back-Buffer + present.
- **Async-Zeitwarten:** `zeit::warte_ms(ms)` statt yield-Polling — der
  Timer-Interrupt weckt per AtomicWaker (aktuell EIN Warter; bei Bedarf
  auf eine Waker-Liste erweitern).
- **Dirty-Rect-Compositing (Juli 2026) — DAS PROTOKOLL:** Änderungen
  melden ihre Bildschirm-Fläche per `dirty_melden(rect)` an (max.
  MAX_DIRTY_RECTS=16, Überlauf -> alles_dirty-Vollbild-Fallback):
  Fenster-Drag/Resize melden ALTE+NEUE Fläche (fenster_flaeche =
  gesamt_rechteck + 10px Schatten), Heben meldet Fenster + Alt-Fokus +
  Taskleiste, der Uhr-Sekundenwechsel NUR das systray_rechteck,
  Startmenü/Switcher ihre Panel-Flächen; Fenster mit fenster.dirty
  werden in dirty_abholen eingesammelt. Der Compositor holt per
  `dirty_abholen(b, h)` die (geklemmten) Rects, komponiert JE Rect mit
  Zeichner-Clip (Fenster ohne Schnitt werden übersprungen, Alpha-Fills
  clippen vorab) und presentet nur diese Bereiche. Der Desktop-
  Verlauf liegt als BYTE-IDENTISCHER Cache im DoppelPuffer
  (hintergrund_uebernehmen/_wiederherstellen = memcpy pro Zeile —
  NICHT als Farbe-Array, das wäre eine Pro-Pixel-Konvertierung und
  LANGSAMER als der alte Gradient!); das Flag manager.hintergrund_neu
  lässt den Compositor ihn beim ersten Frame/Theme-Wechsel neu
  rendern. Gemessen: Uhr-Tick bei 4K 0,31 ms statt 9,3 ms Vollbild.
- **Widget-Schadensmeldung (Juli 2026, Performance-Pass):** Die
  Dirty-Rect-Mechanik bekommt jetzt bis in die Widgets FEINE
  Meldungen. `UiReaktion.schaden: Option<Rechteck>` (Fensterinhalt-
  Koordinaten; None + neu_zeichnen = Vollbild-Fallback, KORREKTHEIT
  vor Eleganz) über `neu_zeichnen_bereich()`/`mit_schaden()`;
  Container reichen es via `und()` nach oben (Bounding-Box; die
  Koordinaten sind schon fenster-absolut, weil jedem Widget sein
  `bereich` übergeben wird). Das Fenster sammelt MEHRERE Rects
  (`inhalt_schaden: Vec<Rechteck>`, kein Bounding-Box-Union — sonst
  würden Cursorzeile OBEN und Statuszeile UNTEN fast das ganze
  Fenster umfassen!); `inhalte_rendern` rendert JEDES Rect einzeln
  geclippt (`ui.zeichnen_bereich`) und meldet nur den Streifen per
  dirty_melden statt fenster.dirty. Wer VOLL neu will
  (neu_aufbauen, Textfeld-Modi, blink, Theme), setzt `inhalt_voll`
  (gewinnt über Teilschäden). KRITISCH für 4K: Der Editor CULLT im
  zeichnen Textzeilen außerhalb von `z.clip()` — ohne das prüft
  `text()` bei 4K Millionen Glyph-Pixel gegen den Clip. Die
  Statuszeile (unten, außerhalb des Cursor-Schadens) meldet die App
  über `AppReaktion.status_neu`; der Manager macht daraus mit den
  Fenstermaßen einen Streifen am Content-Rand (knapp EINE Zeilenhöhe
  — jeder Extra-Pixel kostet bei 4K Füllen+Komponieren+Übertragen).
  GEMESSEN (messung_serie3, ALT/NEU im selben Lauf): Editor-Tippen
  bei 4K 417 µs statt 15,4 ms (~37x), bei 720p 350 µs statt 2,55 ms.
  Der Task-Manager bleibt bewusst Vollbild (tickt nur 1x/s, ändert
  Zahlen+Graph+Liste gemeinsam — kein interaktiver Hot-Path).
- **Fenster & Compositor (Juli 2026):** `src/fenster/mod.rs`. JEDES
  Fenster = eigener Pixel-Puffer (`FensterPuffer`, Vec<Farbe>) +
  Metadaten (Position, Größe, Titel, Fokus). Z-Ordnung = Reihenfolge
  im `Vec<Fenster>` (letztes = ganz vorne). Apps zeichnen NUR in ihren
  Puffer, NIE auf den Bildschirm — dafür ist `grafik::Zeichner`
  generisch über das `Zeichenflaeche`-Trait (Bildschirm-Back-Buffer
  UND Fenster-Puffer implementieren es identisch). Der Compositor-Task
  setzt pro Frame zusammen: Desktop-Aurora-Hintergrund -> Fenster in
  Z-Reihenfolge (Schatten/Titelzeile/Rahmen malt der COMPOSITOR, nicht
  die App) -> present() -> Maus-Cursor obenauf. Dirty-Flags
  (`alles_dirty` + pro Fenster `dirty`): NUR komponieren, wenn sich
  etwas geändert hat. Event-Routing: Maus -> oberstes Fenster unter dem
  Cursor (in Fenster-Koordinaten umgerechnet, Titelzeile zählt nicht
  zum Inhalt), Klick hebt+fokussiert; Tastatur -> fokussiertes Fenster;
  Titelzeilen-Drag verschiebt. Lock-Ordnung: FRAMEBUFFER -> MANAGER.
  Der Desktop-Modus (AtomicBool) pausiert Konsole/Cursor; ESC kehrt
  zurück, die Fenster bleiben erhalten.
- **Fenster-Deko & Bedienung (Juli 2026):** Die Titelleiste (Icon,
  Titel, 3 Knöpfe Minimieren/Maximieren/Schließen — Schließen rot)
  zeichnet der COMPOSITOR, nicht die App. Interaktion-Enum:
  Verschieben (Titel-Drag), Größe (Rand-Drag; kante_bei berechnet die
  Zone, Cursor wechselt die Form via `maus::cursor_form_setzen`).
  Maximieren speichert die Vorher-Geometrie (Vollbild minus 40px
  Taskleisten-Reserve); Schließen droppt den Fenster-Puffer (Heap
  frei). Snap: Ziehen an den Bildschirmrand -> halbe Fläche; Vorschau
  (snap_hinweis) UND Loslassen nutzen denselben Wert (konsistent,
  positionsunabhängig). Alt+Tab: Der KeyStream greift LAlt/Tab VOR dem
  Dekodieren ab (KeyEvent.state), der Switcher lebt im Manager,
  Loslassen von Alt bestätigt. WICHTIG: Ein maximiertes/gesnapptes
  Fenster braucht einen fast bildschirmgroßen Puffer — desktop_starten
  lässt den Heap passend zur Auflösung wachsen (Breite*Höhe*3*3 Bytes).
- **PS/2-Paket-Grenze:** Ein Maus-Paket trägt nur 9-Bit-Deltas
  (±255); größere Bewegungen setzen das Overflow-Bit und werden
  verworfen (Spec-konform). Automatisierte QMP-Tests müssen die Maus
  in kleinen Schritten bewegen.
- **Mehrere Tick-Warter (Juli 2026):** `zeit::warte_ms` nutzt eine
  feste Slot-Liste von AtomicWakern (nicht EINEN!), weil Cursor,
  Compositor und Uhr gleichzeitig auf Ticks warten — ein einzelner
  AtomicWaker ließe alle bis auf den letzten verhungern. Slots werden
  lock-frei per compare_exchange belegt und in Drop zurückgegeben.
- **PS/2-Maus (Juli 2026):** `src/maus.rs` — Controller-Init NUR über
  die Maus-Bits (Tastatur-Bits 0/4/6 der 8042-Konfiguration niemals
  anfassen!), alle Handshakes gepollt mit Timeout (fehlende Maus hängt
  den Boot nicht), VOR sti. IntelliMouse-Rad per 200/100/80-Sequenz.
  Paket-Parsing ist eine reine, unit-getestete Funktion (Sync-Bit,
  9-Bit-Vorzeichen, Overflow -> verwerfen). IRQ 12 -> lock-freie Queue
  -> async maus_task (Tastatur-Muster). Cursor = Overlay NUR im
  Front-Buffer: Der Back-Buffer bleibt die "Wahrheit ohne Cursor",
  Wiederherstellen = present_bereich der alten Position.
- **Grafik-Schnellpfade (Juli 2026, Qualitäts-Pass):** Das
  Zeichenflaeche-Trait hat zwei ZEILEN-Methoden (flaeche_zeile_fuellen,
  flaeche_zeile_kopieren) mit korrekten Pro-Pixel-Defaults; DoppelPuffer
  und FensterPuffer überschreiben sie speicher-nah. Der Zeichner clippt
  dafür VORAB rechteckig (sichtbar() = Rechteck ∩ Clip ∩ Fläche) —
  deckendes rechteck_fuellen, verlauf_vertikal und puffer_blit (der
  Compositor-Blit für Fensterinhalte) laufen also OHNE Prüfungen pro
  Pixel. Alpha bleibt auf dem Pixel-Pfad (muss den Untergrund lesen).
  Frame-Zeit-Messung: fenster::tests::messung_compositor_frame_zeit
  (Berichts-Test, Zahlen im CHANGELOG). Die frühere Mess-Falle
  ("ticks() steht unter without_interrupts still") ist seit der
  TSC-Zeitquelle TOT — zeit::us_seit_boot()/ms_seit_boot() dürfen
  ÜBERALL genommen werden, auch in mit_framebuffer-Blöcken.
- **Zeichen-Werkzeuge (Juli 2026):** `grafik.rs` = Zeichner auf dem
  Back-Buffer mit optionalem Clip-Rechteck und Alpha-Blending (alle
  Pixel laufen durch EINEN Pfad: Zeichner::pixel). Clipping-Schnitt
  und Alpha-Formel sind reine, unit-getestete Funktionen. Icons =
  16x16-ASCII-Art-Konstanten mit gemeinsamer Palette (unbekanntes
  Zeichen -> Magenta = sichtbarer Tippfehler). Demo-Modi (grafiktest)
  über AtomicBool-Flag: Shell fängt die nächste Taste ab und stellt
  die Konsole wieder her. Fließkomma gibt es NICHT (soft-float!) —
  alle Algorithmen ganzzahlig (Bresenham, Midpoint).
- **Bootloader-0.11-Migration (Juli 2026, docs/migration-011.md):** UEFI
  statt BIOS (BIOS-Stages von 0.11.15 bauen auf aktuellem Nightly nicht).
  Drei hart erkämpfte UEFI-Lektionen, alle im Code dokumentiert:
  (1) Nach dem GDT-Laden SS/DS/ES explizit neu setzen — sonst #GP beim
  ersten iretq (gdt.rs). (2) Den PIT selbst programmieren — UEFI tut es
  nicht (interrupts.rs). (3) PIC-Masken explizit setzen — OVMF übergibt
  alles maskiert; LAPIC deaktivieren für die Pre-APIC-Verdrahtung (lib.rs).
- **Theme-System (Juli 2026):** `src/theme.rs` = `Theme` (ALLE UI-Farben;
  zwei Instanzen: AURORA_DUNKEL Standard, AURORA_HELL) + `metrik()` (alle
  Abstände/Schriftgrößen, in beiden Themes gleich). Aktives Theme über
  AtomicBool, `theme::aktuell()` ist lockfrei (wird unter gehaltenen
  Locks im Compositor gerufen). SEITDEM GILT: KEINE hartcodierten Farben
  oder Abstände in UI-Code — alles über theme::aktuell()/metrik().
- **UI-Skalierung (Juli 2026):** Faktor 1.0/1.5/2.0, gespeichert in
  HALBEN (AtomicI32: 2/3/4 — kein Fließkomma im Kernel!). `metrik()`
  liefert die SKALIERTE Kopie der BASIS_METRIK; die Schrift mappt auf
  die vorgerasterten Fonts (16/24/32 — Cargo-Features size_16/24/32),
  schrift_gross ist bei 32 gedeckelt. Boot-Standard nach Breite
  (>=2560 -> 1.5, >=3840 -> 2.0, desktop_starten); Umschalten zur
  Laufzeit über die Registry-App "Skalierung" (fenster::
  skalierung_wechseln = Theme-Wechsel-Mechanik: Inhalte neu zeichnen
  + alles_dirty). ACHTUNG Tests: metrik()-abhängige Koordinaten gelten
  für Skala 1.0 — der Shell-Befehls-Test setzt die Skala im Cleanup
  zurück (desktop_starten hätte sie bei 4K-Testläufen verstellt).
  Wechsel via `fenster::theme_wechseln()` (schaltet um UND rendert alle
  Fenster-Inhalte neu). Das Terminal bleibt bewusst in beiden Themes
  dunkel (Shell-Farben sind auf dunklen Grund abgestimmt, Zellen-
  Hintergrund == Color::Black == theme.terminal_hintergrund).
- **Taskleiste & Startmenü (Juli 2026):** Der Compositor zeichnet die
  Taskleiste NACH den Fenstern (immer im Vordergrund), das Startmenü
  darüber; Klicks prüfen dieselbe Reihenfolge (Menü -> Leiste ->
  Fenster). Fenster-Knöpfe sind nach FensterId (= Erstellungsreihen-
  folge) sortiert, damit sie beim Fokuswechsel nicht springen; Klick =
  Fokus/Minimieren-Toggle. Uhr+Datum kommen aus `einstellungen::
  jetzt_lokal()`/`uhrzeit_text()` (echte RTC+TSC-Zeit via zeit::jetzt(),
  plus Anzeige-Offset und 12/24h aus den Einstellungen — der frühere
  Tick-Platzhalter ist Geschichte); neu komponiert wird nur beim
  Sekundenwechsel.
- **App-Registry & App-Trait (Juli 2026):** `src/apps.rs` — jeder
  Registry-Eintrag (`AppEintrag`) = Name + Icon + `start: fn()`.
  NEUE Apps implementieren `ui::App` (name/icon/aufbau/nachricht/tick)
  und landen als `Inhalt::App(AppFenster)` im Fenster (die Brücke vom
  Enum zum Trait; das Enum bleibt für Terminal und alte Demos) —
  start-fn ruft `fenster::app_starten(Box::new(MeineApp))`. LOCK-REGEL
  (ui/app.rs): App::nachricht/tick laufen UNTER dem MANAGER-Lock —
  eigener Zustand/fs/serial_println erlaubt, print!/fenster:: verboten;
  Außenwirkung über AppReaktion.danach (fn(), läuft nach dem Lock).
  Startmenü und Alt+Tab-Switcher laufen aufs Toolkit: Suchfeld =
  Textfeld-Widget (Änderungs-Nachricht = Live-Filter), Liste =
  ScrollListe, gezeichnet in einen OFFSCREEN-FensterPuffer, den der
  Compositor per puffer_blit zeigt (Muster für alle Overlays).
  Deadlock-Regel unverändert: Start-Funktionen/Nachrichten via
  NachLock (jetzt: Keine | Ausfuehren(fn()) | Nachricht) nach draußen.
- **UI-Widget-Toolkit (Juli 2026):** `src/ui/` = das UI-Fundament
  aller Apps. Retained Widget-Baum: `trait Widget` (wunschgroesse,
  zeichnen in den FensterPuffer, ereignis mit Rechteck-Routing wie
  kante_bei — alle Koordinaten in Fensterinhalt-Koordinaten, kein
  Umrechnen). `UiEreignis` (Klick/Doppelklick/Losgelassen/Bewegt/
  Scroll/Taste/MausRein/MausRaus/FokusRein/FokusRaus); MausRein/Raus
  erzeugt das ROUTING in den Box-Containern (hover_kind) — Widgets
  pflegen damit ihren Hover-Zustand. `UiReaktion` ist bewusst ein
  STRUCT (verbraucht + neu_zeichnen + nachricht sind kombinierbar).
  App-Nachrichten als u32-ID an einen fn(u32)-Handler (KEINE
  Closures: Borrow-Hölle; KEIN generischer Typ: macht das Trait
  un-objektsicher) — zustandsbehaftete Apps bekommen später ein
  App-Trait. Fokus-Kette: fokus_weiter (Blätter nehmen/geben,
  Container iterieren ab Fokus-Kind, UiFenster wrappt bei Tab);
  Tasten laufen den Baum entlang, bis das fokussierte Widget sie
  verbraucht. Layout primitiv: laengen_verteilen (pure, getestet)
  + VBox/HBox/Fueller mit METRIK.abstand, quer wird IMMER auf volle
  Breite gestreckt — kein Constraint-Solver. Widgets: Label,
  Trennlinie, Button (Nachricht beim LOSLASSEN im Bereich),
  Checkbox, Textfeld (Innenleben = shell::editor::ZeilenEditor,
  Cursor blinkt über zeit-API + Uhr-Task-Anstoß via UiFenster::
  blinkt), ScrollListe (Rad + ziehbarer Balken + Doppelklick).
  Doppelklick erkennt das UiFenster (500 ms, 6 px, us_seit_boot);
  seine Nachricht hat Vorrang vor der zweiten Klick-Nachricht.
  Fenster-Anbindung: `Inhalt::Ui(UiFenster)`; der Manager reicht
  Klick/Losgelassen/Scroll/Bewegt (Hover! ui_hover_fenster erzeugt
  MausRaus beim Fensterwechsel) und Tasten weiter. Ui-NACHRICHTEN
  laufen wie App-Starts NIE unter dem MANAGER-Lock (`NachLock`-Enum
  wird nach draußen gereicht). Der PANIC-HANDLER druckt ZUERST roh
  seriell (println! würde im Desktop-Modus via Terminal-Umleitung
  den MANAGER-Lock brauchen -> Deadlock bei Panik unterm Lock).
- **Dateioperationen & Kontextmenü (Juli 2026):** Rekursives
  Kopieren/Löschen/Verschieben lebt in fs/mod.rs (liste() IMMER vor
  dem Abstieg abschließen — mit_fs nie verschachteln). Papierkorb =
  /papierkorb; Ursprung steht in einer METADATEN-Datei
  (<name>.herkunft — kein Namens-Parser, Ansicht filtert sie aus).
  Ablage (`src/ablage.rs`) = globaler Blatt-Lock (darf unter dem
  MANAGER-Lock genutzt werden) — Strg+C/X/V fensterübergreifend;
  KeyStream dekodiert mit MapLettersToUnicode (Strg+C = U+0003).
  Kontextmenü = GENERISCHES Manager-Overlay (Offscreen + Blit;
  Empfänger als FensterId): Apps liefern es via AppReaktion::menue
  auf UiEreignis::Rechtsklick (ScrollListe::mit_rechtsklick);
  AppReaktion::danach ist eine Box<dyn FnOnce> (Aktion MIT Daten,
  z. B. Betrachter-Pfad) -> NachLock::Einmal.
- **Einstellungen (Juli 2026):** `src/einstellungen.rs` = Store + App.
  (1) STORE: /system/einstellungen.txt im VFS (Schlüssel=Wert;
  parsen/serialisieren rein + getestet), typisierter Zugriff
  (hole_/setze_zahl/bool/text — setze_* speichert SOFORT). Der
  SPEICHER-Mutex ist ein BLATT-Lock wie die Ablage (unter dem
  MANAGER-Lock erlaubt); main.rs lädt nach fs::init und wendet auf
  die theme-Atomics an. API-Naht für Serie 4 (Disk-FS = nur VFS
  tauschen). (2) APP: Kategorien-ScrollListe links, Seiten rechts.
  DAS MUSTER für sofort wirkende Optik-Optionen: lock-freies Atomic
  UNTER dem MANAGER-Lock setzen (theme::hell_setzen/akzent_setzen/
  hintergrund_setzen/skala_setzen_halbe — sonst markiert der direkt
  folgende Neu-Aufbau den alten Zustand!), setze_* persistieren,
  Neuzeichnen via AppReaktion.danach -> fenster::alles_neu_zeichnen()
  (hintergrund_neu + alle Inhalte + alles_dirty). NEUE Theme-
  Fähigkeiten: theme::aktuell() liefert eine KOPIE mit eingesetzter
  Akzentfarbe (Palette AKZENTE, je Eintrag Hell-/Dunkel-Variante,
  patcht akzent + rahmen_aktiv); Desktop-Verlauf über theme::
  hintergrund_verlauf() (HINTERGRUENDE, Preset 0 = Theme-Aurora).
  Systray-Uhr: einstellungen::jetzt_lokal() + uhrzeit_text()
  (UTC-Offset = reine ANZEIGE-Verschiebung; die RTC liefert in QEMU
  die Host-LOKALZEIT, -rtc base=localtime). Cursor-Blinktempo:
  cursor_blink_ms/us, live gelesen von Textfeld + Konsolen-Task.
  Info-Seite: Auflösung wird beim App-Start GECACHT (mit_framebuffer
  unter dem MANAGER-Lock wäre die falsche Lock-Ordnung!); Task-Zahl
  als Atomic im Executor. Boot-Skala: gespeicherter Wert schlägt die
  Auto-Wahl nach Breite (desktop_starten).
- **Explorer & App-Muster (Juli 2026):** `src/explorer.rs` = die
  Blaupause für Trait-Apps: Die App hält ZUSTAND (Pfad, Verlauf,
  Auswahl, aufgeklappte Baum-Ordner) plus ABGELEITETE Listen
  (neu_laden nach jeder Navigation); aufbau() baut die Widgets rein
  daraus. WELCHER Listeneintrag gemeint ist, steckt in der Nachricht:
  ScrollListe::mit_index_nachrichten kodiert BASIS+Index (Basen weit
  auseinander legen!). Auswahl überlebt Neu-Aufbauten via
  mit_auswahl + auswahl_sichtbar (Scroll ist eine Cell — zeichnen ist
  &self). Eingabemodi (Adresszeile) laufen über den App::taste-Hook
  (VOR den Widgets, App puffert selbst); fokus_initial gibt der
  ersten fokussierbaren Liste die Pfeiltasten. Mehrere Fenster einer
  App = mehrere App-Instanzen (app_starten baut immer neu).
- **Terminal-SITZUNGEN (Juli 2026, löst das Ein-Terminal-Limit ab):**
  `shell/sitzung.rs`. Jedes Terminal-Fenster = eigene Sitzungs-Id +
  EIGENER Shell-Task (shell::sitzung_laufen; apps::terminal_starten
  spawnt ihn nach fenster::terminal_oeffnen() -> Option<Sitzungs-Id>).
  Der EINGABE-ROUTER (shell::eingabe_router, einziger KeyStream-
  Leser) routet: Startmenü/ESC wie gehabt, sonst Tasten in die
  lock-freie Queue der fokussierten Sitzung (terminal_fokus_sitzung);
  im Vollbild-Modus an die HAUPT-Sitzung. AUSGABE: Der Shell-Task
  legt AUSGABE_SITZUNG um jede synchrone Verarbeitung (KEIN await
  dazwischen — deshalb race-frei); konsole::_print schreibt an
  ausgabe_ziel() (Ausgabe-Sitzung, sonst Haupt-Terminal = Kernel-
  Log). Ohne offenes Terminal wird Kernel-Log GEPUFFERT und beim
  nächsten terminal_oeffnen nachgereicht; Ausgaben toter Sitzungen
  verfallen. SCHLIESSEN: fenster_schliessen trägt die Sitzung aus
  (beendet-Flag + Waker) -> naechste_taste liefert None -> Task endet
  sauber; das Haupt-Terminal vererbt seine Rolle ans nächste.
  `fenster/terminal.rs` bleibt das reine Text-Raster; gerendert wird
  weiter GEBÜNDELT (inhalt_neu + inhalte_rendern pro Frame).
  prompt_nachholen() nutzt nur noch der Vollbild-Pfad (ESC/Demo-Ende,
  cwd-Spiegel der Haupt-Sitzung). SEIT DEM SERIE-3-PERFORMANCE-PASS
  führt das Raster DIRTY-ZEILEN: terminal_rendern zeichnet nur den
  geänderten Zeilenbereich in den persistenten Fenster-Puffer, und
  terminal_schreiben meldet dem Compositor nur den Zeilen-STREIFEN
  (2x schnellere Prompt-Ausgabe); Scroll/Resize/inhalt_zeichnen
  (Theme!) markieren alles. Der Frame-Pfad für Terminals läuft in
  inhalte_rendern OHNE fenster.dirty.
- **SpeedText & Dialog-Bausteine (Juli 2026):** `src/speedtext.rs` +
  `ui/texteditor.rs` + `ui/dialog.rs`. Der TextPuffer ist ein
  Zeilen-Vec (BEWUSST kein Rope — KiB-Dateien, Begründung im Code)
  mit Zeichen-Spalten (chars, nie Bytes!); das Editor-Widget teilt
  ihn per Arc<Mutex> (Blatt-Lock) mit der App, damit der Text die
  ständigen Neu-Aufbauten (Statuszeile!) überlebt — DAS Muster für
  großen, heißen Widget-Zustand. Dialoge ERSETZEN den Fenster-Inhalt
  über App-Zustand (kein Overlay): dialog::bestaetigung() = generische
  Frage+Knöpfe; dialog::DateiDialog = Zustands-Baustein (Ordner-Liste
  + selbst gepufferte Pfad-Eingabe via App::taste-Hook, Nachrichten
  in einem Id-Fenster ab Basis, DIALOG_ID_BREITE). Neue App-Trait-
  Fähigkeiten: fenster_titel() (Start-Titel), AppReaktion.titel
  (Titel ändern -> "name.txt *"), AppReaktion.schliessen (Fenster
  aus der App schließen) und App::schliessen_abfragen() (X-Knopf
  abfangen -> Nachfrage-Dialog; None = sofort zu). Explorer-
  Doppelklick auf Dateien öffnet SpeedText (Betrachter entfernt).
  SpeedTexts Tipp-Pfad ist seit dem Performance-Pass schlank: KEIN
  Baum-Neuaufbau pro Taste — die StatusZeile (texteditor.rs) liest
  Zeile/Spalte/Zeichen beim ZEICHNEN live aus dem Arc, der Titel
  wird nur bei echtem Wechsel gemeldet (letzter_titel-Vergleich).
- **Toolkit-Konventionen (Serie-3-Review):** `ui::w(widget)` statt
  `Box::new(widget) as Box<dyn Widget>` (neue Apps/Umbauten);
  `ui::app::SekundenTick` für 1-Hz-Live-Apps (Einstellungen,
  Task-Manager) statt eigener letzte_sekunde-Buchhaltung. Bekannte
  Ecken (bewusst offen): Tab ist global die Fokus-Taste (Editor kann
  keine Tabs einfügen); Nachricht-Basen sind Handarbeit (Basen weit
  auseinander legen, DIALOG_ID_BREITE als Muster); Textfeld-Inhalt
  überlebt Neu-Aufbauten nicht (Apps puffern selbst oder teilen
  Zustand per Arc wie der Editor).
- **Persistenz-Standard (Juli 2026) — SpeedOS überlebt den
  Neustart:** fs::platte_automounten() läuft beim Boot (main.rs,
  NACH ata::init/fs::init, VOR einstellungen::laden): mountet das
  SpeedFS der Daten-Platte auf /platte und legt die Standard-Ordner
  /platte/heim, /platte/dokumente, /platte/system an. KEIN
  AUTO-FORMAT — eine unformatierte Platte bekommt nur den
  mkfs-Hinweis in der Shell (Formatieren ist Nutzer-Entscheidung).
  DIE Orts-Abstraktion ist fs::persistenter_pfad(platte, ram)
  (EINE Stelle, kein if-Wildwuchs): einstellungen::pfad() ->
  /platte/system/einstellungen.txt (Fallback /system/...), Explorer
  papierkorb() -> /platte/papierkorb, start_ordner() ->
  /platte/heim (auch SpeedTexts Datei-Dialoge). ACHTUNG (neue
  Deadlock-Erkenntnis): persistenter_pfad/ist_gemountet nehmen den
  VFS-Lock — sie dürfen NIE innerhalb einer mit_fs-Closure
  ausgewertet werden, auch nicht versteckt als Argument-Ausdruck
  (`f.lesen(pfad())` ist der Klassiker) — Pfad IMMER vorher binden.
  KERNEL-LOG (src/protokoll.rs): konsole::_print hängt jede Ausgabe
  zusätzlich an einen Blatt-Lock-RAM-Puffer (64-KiB-Fenster; vor
  der Heap-Init No-Op); der Log-Schreiber-Task flusht sekündlich
  rotierend nach /platte/system/log.txt (write_at ans Ende, bei
  64 KiB rename -> log.alt.txt). WARUM Puffer+Task: _print hält
  KONSOLE, Shell-Befehle halten VFS und drucken dann — synchrones
  Schreiben aus _print wäre ABBA; Log-Task-Fehler werden NUR
  seriell gemeldet (println wäre Rekursion). Einstellungen-App:
  Kategorie "Speicher" (Laufwerke, Mount-Status + frei/gesamt über
  das neue FileSystem::speicher_info (Default Ok(None), SpeedFS
  zählt die Bitmap), sync-Knopf, pruefe.speedfs-Knopf: hängt kurz
  aus, prüft, hängt wieder ein; Ergebnis als Dialog im
  SpeedText-Muster). Runner: SPEEDOS_OHNE_DATENPLATTE=1 startet
  ohne Daten-Platte (RAM-Fallback-Test).
- **Live-USB-Boot + Diagnose (Serie-4-Abschluss, Juli 2026):**
  `cargo image` (Alias -> `boot/src/bin/live-image.rs`) baut
  `speedos-live.img`: ein UEFI-GPT-Image OHNE QEMU/Platten, BEWUSST
  ohne erzwungene Mindestauflösung (der Kernel nimmt den größten
  GOP-Modus der Firmware -> auf 4K-fähiger Hardware 4K, sonst nativ).
  `tools/live_qemu.ps1` bootet es in OVMF (Schalter -KeinePS2 =
  i8042=off, -Qmp für Screendumps); `tools/usb_schreiben.ps1` schreibt
  es (nur Admin, wählt nur eine USB-Wechselplatte) roh auf den Stick —
  ein bootfähiger UEFI-Stick hat nur die EFI-System-Partition und ist
  im Windows-Explorer deshalb UNSICHTBAR (normal!). ACHTUNG: .ps1 in
  diesem Repo ASCII-only halten (PowerShell 5.1 liest UTF-8-ohne-BOM
  als ANSI -> Umlaute/Gedankenstriche zerlegen den Parser). Robustheit
  gegen fremde Hardware: `maus::tastatur_vorhanden()` ist eine
  NICHT-intrusive PS/2-Probe (First-Port-Test 0xAB, ändert KEINE
  8042-Config); fehlt die Tastatur, zeigt `framebuffer::meldung_zeigen`
  vor dem Desktop eine klare Meldung statt still zu hängen; keine Maus
  -> Tastatur-Desktop; keine Platte -> RAM-VFS. Der DIAGNOSE-Modus
  (`src/diagnose.rs`, Auslöser: Taste D auf dem Bootscreen ODER
  `SPEEDOS_DIAGNOSE=1` -> Runner hängt per `UefiBoot::set_ramdisk` ein
  Marker-Ramdisk an, Kernel prüft `boot_info.ramdisk_addr`) schreibt
  die Boot-Schritte + `hardware_zusammenfassung()` auf den Schirm (auf
  echter Hardware gibt es keine serielle Ausgabe). ACHTUNG Framebuffer-
  Konsole ist Latin-1: Em-Dash/Smart-Quotes werden zu '?'. Verifiziert:
  Acer Aspire A515-51, 1080p (docs/hardware-log.md, docs/usb-boot.md).
- **Serie-4-Abschluss-Tests (Juli 2026):** Neben dem Folter- und dem
  Persistenz-Beweis prüfen jetzt: `test_speedfs_mount_fehlerpfade`
  (jeder Mount-Fehler sauber), `test_speedfs_voll_sauber` (volle Platte
  -> `FsFehler::Voll`, NICHT das nicht existierende `IoFehler::KeinPlatz`
  -- ein fixes Blockgerät ist nie "voll", nur das FS; die
  alles-oder-nichts-`bloecke_allozieren` korrumpiert nichts),
  `test_speedfs_folter_fast_voll` (Folter auf fast voller Platte). Der
  große E2E liegt als geteilte `speedfs::e2e_ops`/`e2e_verifizieren`
  (doc(hidden) pub, damit auch das Integrationstest-Crate sie sieht):
  Unit-Test gegen RamDisk (inkl. Absturz-Sim), `tests/e2e_speedfs.rs`
  gegen IDE+virtio NON-DESTRUKTIV im Unterbaum /platte/e2e (schützt die
  geteilten Test-Images). plattentest gemessen: virtio ~1500x (seq) bis
  ~8600x schneller als IDE-PIO (0,21 MiB/s, architektonisch).
- **Deadlock-Regeln:** (1) Ausgabe-Locks (WRITER, SERIAL1) werden nur mit
  deaktivierten Interrupts gehalten (`without_interrupts` in den _print-
  Funktionen). (2) Interrupt-Handler sind minimal: nie blockieren, nie
  allokieren, nie printen — Daten in lock-freie Queues, Verarbeitung in
  async Tasks (siehe Tastatur). (3) `fs::mit_fs()` nie verschachteln.
  (4) Lock-Ordnung KONSOLE -> FRAMEBUFFER -> MANAGER (die Terminal-
  Umleitung nimmt KONSOLE dann MANAGER, der Compositor FRAMEBUFFER dann
  MANAGER — nie andersherum). (5) App-Start-Funktionen nie unter dem
  MANAGER-Lock ausführen (siehe App-Registry).
- **Globale Speicher-API (Juli 2026):** Mapper und Frame-Allocator leben als
  globale `Mutex<Option<...>>` in `src/memory.rs` (Muster wie das VFS) —
  NICHT als Locals in kernel_main. Zugriff NUR über die API (map_page,
  map_page_zu für MMIO, unmap_page, allocate_pages, frame_allozieren/
  frame_freigeben, uebersetzen, frame_statistik). Beide Locks werden
  ausschließlich in `mit_speicher()` genommen (feste Reihenfolge: Mapper
  vor Frame-Allocator, Interrupts aus) — nie direkt.
- **Bitmap-Frame-Allocator (Juli 2026):** 1 Bit pro 4-KiB-Frame (statische
  32-KiB-Bitmap für max. 1 GiB RAM), Next-Fit-Zeiger. Bewusst KEINE
  Free-List: Die Bitmap findet zusammenhängende physische Bereiche
  (Framebuffer/DMA!) per Scan, kann O(1) freigeben, erkennt
  Doppel-Freigaben (assert) — eine Free-List kann Kontiguität praktisch
  nicht liefern. Freigegebene Frames setzen den Next-Fit-Zeiger zurück
  und werden sofort wiederverwendet.
- **Heap wächst zur Laufzeit:** `allocator::heap_erweitern(pages)` mappt
  neue Pages nahtlos ans Heap-Ende und ruft `extend` des Allocators.
  Alle drei Allocatoren (linked_list, Bump, Fixed-Block) unterstützen
  extend mit derselben Signatur. Kein automatisches Wachsen — bewusst
  manuell vor großen Puffern aufrufen.
- **Boot-/Init-Reihenfolge (main.rs):** GDT/TSS → IDT → PIC → Interrupts an
  → zeit::init → **zufall::init** (braucht die TSC und die RTC fürs Salz;
  sät NICHT, stellt nur die Quellen fest)
  → memory::init (globaler Mapper + Frame-Allocator) → Heap → Dateisystem
  → scheduler::init (trägt den LAUFENDEN Kontext als Kernel-Prozess PID 0
  ein — muss NACH dem Heap laufen und VOR dem ersten Prozess)
  → Executor + Shell.
  Statics mit einmaligem Seiteneffekt (Scancode-Queue) über conquer_once
  OnceCell explizit initialisieren, NICHT lazy_static (sonst passiert die
  Erst-Initialisierung womöglich im Interrupt-Kontext).
- **Multitasking kooperativ (async/await):** Eigener Executor
  (`src/task/executor.rs`) mit Waker-Support, FIFO-fair, schläft per
  hlt (race-frei via disable/enable_and_hlt). Tasks spawnen neue Tasks
  über `task::spawn()` (globale Spawn-Queue; NIE aus Interrupt-Handlern,
  denn Task::new alloziert). Tasks/Futures müssen `Send` sein.
  SEIT DEM TASK-MANAGER: Task::new(NAME, future) — jeder Task trägt
  Name/Art/beendbar (Builder mit_art/als_beendbar); die Registry
  in `task/uebersicht.rs` ist die Schatten-Buchhaltung (Blatt-Lock,
  Momentaufnahme unterm MANAGER-Lock erlaubt), die heißen Zähler
  (Polls/Wecken/wach) sind Atomics im geteilten Arc — der WAKER
  zählt aus dem Interrupt-Kontext, ohne Lock. Beenden ist
  KOOPERATIV: beenden_anfordern setzt nur ein Flag, der Executor
  lässt den Task in der nächsten Runde am await-Punkt FALLEN (Drop
  der Future — nur beendbare Demo-Tasks, Kernel-Tasks geschützt).
  CPU-Auslastung: run() misst per TSC Arbeit (run_ready_tasks) vs.
  Ruhe (hlt) und verbucht in ein 10x100-ms-Gleitfenster
  (cpu_auslastung_prozent, reine getestete Fenster-Logik).
  Die Task-Manager-App (src/taskmanager.rs) zeigt alles sekündlich
  per tick; Graph-Downsampling nimmt das Spalten-MAXIMUM.
  Volle Warteschlangen panicken NICHT: Überlauf setzt ein Notfall-Flag,
  die nächste Runde pollt alle Tasks — kein Wecken geht verloren.
  Kapazität konfigurierbar (`Executor::mit_kapazitaet`, Standard 128).
  SEIT SERIE 6, TEIL 3 ist dieser Executor selbst der KERNEL-PROZESS
  (PID 0) des präemptiven Schedulers — Kernel-Tasks bleiben kooperativ,
  PROZESSE werden präemptiv umgeschaltet (siehe Scheduler-Eintrag und
  docs/scheduler-design.md). `sleep_if_idle` ist damit der Idle-Zustand
  des ganzen Systems.
- **Shell-Befehle als Registry:** Jeder Befehl = Struct mit `Befehl`-Trait
  (`src/shell/befehle.rs`, `Send + Sync`), eingetragen in `alle_befehle()`.
  Gemeinsamer Zustand (aktuelles Verzeichnis) nur über `ShellKontext`.
- **ZeilenEditor getrennt von der Anzeige (Juli 2026):** Die gesamte
  Eingabelogik (Tippen, Backspace, Verlauf, Tab) lebt in
  `src/shell/editor.rs`: Eingabe = eigenes `Taste`-Enum, Ausgabe =
  `Reaktion`-Enum (Anzeige-ANWEISUNGEN als Daten, der Editor druckt nie
  selbst). Tab-Kandidaten kommen über das `Vervollstaendiger`-Trait
  (Shell: VFS, Tests: Mock) — dadurch ist die Eingabelogik als reiner
  Unit-Test prüfbar. shell::run() ist nur noch Übersetzer:
  Taste rein, Reaktion zeichnen, fertige Zeilen an die Registry.
- **Zeit nur über `src/zeit.rs` (seit Juli 2026 TSC-basiert):**
  us_seit_boot()/ms_seit_boot() laufen über den beim Boot gegen den
  PIT kalibrierten TSC (zeit::init, ~200 ms, loggt Frequenz/
  Genauigkeit/CPUID-Invariant) — mikrosekundengenau und UNABHÄNGIG
  von Interrupts (kein Stillstand unter without_interrupts; Zeit darf
  überall genommen werden). Der PIT (250 Hz, Teiler zeit::PIT_TEILER,
  denselben Wert nutzt interrupts::pit_initialisieren) ist nur noch
  WECKGEBER für warte_ms/Executor und Fallback vor der Kalibrierung.
  Echte Uhrzeit: rtc.rs liest die CMOS-Uhr EINMAL beim Boot (Update-
  in-Progress-Flag, BCD/12h-Modus, Doppel-Lesen bis stabil, Timeout);
  zeit::jetzt() = RTC-Anker + TSC-Zeit. QEMU-RTC läuft per Runner auf
  der Host-LOKALZEIT (-rtc base=localtime). zeit::init() MUSS nach
  speed_os::init() laufen (PIT muss ticken) — auch im Test-Kernel.
- **Heap-Allocator austauschbar:** Standard linked_list_allocator; eigene
  Lern-Allocatoren (Bump, Fixed-Size-Block) über Cargo-Features
  `bump-allocator` / `fixed-block-allocator` — gleiche init-Schnittstelle.
- **unsafe-Politik:** Jede unsafe-Funktion dokumentiert ihre Bedingungen in
  einem `# Safety`-Abschnitt; jeder unsafe-Block hat einen Kommentar, WARUM
  er safe ist. `cargo clippy --all-targets` muss warnungsfrei sein.
  Audit Serie-4-Abschluss: die 50 unsafe-Blöcke der Port-I/O-Treiber
  (pci/virtio-blk/virtqueue/ata) sind ausnahmslos Port-I/O auf
  Legacy-Registern oder `read_volatile` auf validierten Indizes,
  0 `unsafe fn` — die riskante Fläche ist bewusst klein und geprüft.

## Bekannte Abweichungen vom blog_os-Buch
- (Historisch, seit der 0.11-Migration irrelevant: eigenes Target-JSON
  brauchte auf neuem Nightly `json-target-spec`, Zahlen statt Strings und
  `"rustc-abi": "softfloat"` — alles Geschichte, wir nutzen das eingebaute
  Target `x86_64-unknown-none`.)
