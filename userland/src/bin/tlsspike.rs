// tlsspike — DER MACHBARKEITS-NACHWEIS FUER TLS (Serie 7, Teil 3)
//
// ==========================================================================
// WAS DIESES PROGRAMM IST UND WAS NICHT
//
// Es ist ein SPIKE: Es soll herausfinden, OB `rustls` in SpeedOS ueberhaupt
// laufen kann — nicht, wie man damit eine Seite abruft. Deshalb tut es genau
// vier Dinge und hoert dann auf:
//
//   1. Heap benutzen (der neue `SYS_SPEICHER`-Allocator).
//   2. Den Zufall des Kernels an `getrandom` haengen und pruefen.
//   3. Die gepruefte UTC-Zeit holen (rustls braucht einen `TimeProvider`).
//   4. Eine rustls-ClientConfig bauen und eine ClientConnection anlegen —
//      MIT den echten Wurzelzertifikaten von /platte/system/ca-bundle.pem.
//
// ES SPRICHT KEIN TLS. Kein Socket, kein Handshake, kein Byte ueber die
// Leitung. Der Erfolg ist: es uebersetzt, es laeuft in Ring 3, es stuerzt
// nicht ab, und es sagt, wie viel Heap es dafuer gebraucht hat.
//
// Der Transport (`libspeed::tls::TcpStrom`) wird trotzdem angelegt und
// gemessen — er ist Teil der Naht, und ob er kompiliert und ein
// Socket-Handle bekommt, gehoert zur Machbarkeitsfrage.
//
// ==========================================================================
// DIE DREI cfg-FLAGGEN, OHNE DIE HIER NICHTS BAUT
//
// In userland/.cargo/config.toml stehen:
//     --cfg aes_force_soft --cfg polyval_force_soft --cfg poly1305_force_soft
//
// Grund: Die RustCrypto-Kisten `aes`, `polyval` und `poly1305` uebersetzen
// auf x86_64 IMMER ihren SIMD-Zweig mit (die Auswahl passiert zur LAUFZEIT
// per `cpufeatures`). Unser Target `x86_64-unknown-none` hat SSE aber
// ABGESCHALTET — und LLVM kann `__m128i` dann nicht legalisieren:
//     rustc-LLVM ERROR: Do not know how to split the result of this operator!
// Die cfg-Flaggen waehlen stattdessen die reinen Software-Implementierungen.
// Ausfuehrlich in docs/tls-entscheidung.md §3.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use libspeed::pem;
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

// Der Zufall des Kernels wird das getrandom-Backend (siehe libspeed::tls).
libspeed::zufall_als_getrandom!();

/// Wo der Vertrauensanker liegt (Serie 7, Teil 2).
const BUENDEL: &str = "/platte/system/ca-bundle.pem";

/// Lesepuffer fuer das CA-Buendel — in `.bss`, nicht auf dem 64-KiB-Stack.
static mut DATEI: [u8; 512 * 1024] = [0; 512 * 1024];
static mut DER: [u8; pem::MAX_DER_BYTES] = [0; pem::MAX_DER_BYTES];

/// # Safety
/// Einzelner Ausfuehrungsstrang, keine Nebenlaeufigkeit.
fn datei_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DATEI) }
}
fn der_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DER) }
}

/// DIE UHR FUER rustls.
///
/// Sie holt die Zeit ueber `zeit_geprueft` (Syscall 13) — also die, die der
/// Kernel gegen das Bau-Datum plausibilisiert hat. Ist die Uhr nachweislich
/// falsch, liefert diese Funktion `None`.
///
/// **Und genau das ist der Punkt:** rustls behandelt `None` als „keine
/// Zeit" und LEHNT die Gueltigkeitspruefung ab, statt sie zu ueberspringen.
/// Die Versuchung „Uhr kaputt, pruefen wir halt nicht" ist damit gar nicht
/// erst implementierbar (docs/tls-vertrauen.md §3e).
#[derive(Debug)]
struct SpeedUhr;

impl rustls::time_provider::TimeProvider for SpeedUhr {
    fn current_time(&self) -> Option<rustls::pki_types::UnixTime> {
        let sekunden = libspeed::zeit_geprueft().ok()?;
        Some(rustls::pki_types::UnixTime::since_unix_epoch(
            core::time::Duration::from_secs(sekunden),
        ))
    }
}

fn haupt(argumente: &Argumente) -> i32 {
    let ziel = argumente.get(1).unwrap_or("example.com");
    println!("=== TLS-SPIKE (Machbarkeitsnachweis, kein Handshake) ===");
    println!();

    // --- 1. HEAP ---
    // Der erste Test ueberhaupt: Gibt es einen Heap? Vor Serie 7, Teil 3
    // haette diese Zeile das Programm beendet.
    let mut probe: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    probe.extend_from_slice(b"Heap lebt");
    let (belegt, gesamt, _) = libspeed::heap::heap_stand();
    println!("1) Heap:   {} Byte belegt von {} Byte gemappt", belegt, gesamt);
    if probe.as_slice() != b"Heap lebt" {
        println!("   FEHLER: der Heap liefert falsche Daten.");
        return 1;
    }

    // --- 2. ZUFALL ---
    let mut zufall = [0u8; 32];
    match libspeed::tls::zufall_fuellen(&mut zufall) {
        Ok(()) => {
            println!(
                "2) Zufall: 32 Byte geholt, beginnt mit {:02x} {:02x} {:02x} {:02x}",
                zufall[0], zufall[1], zufall[2], zufall[3]
            );
            if zufall.iter().all(|b| *b == zufall[0]) {
                println!("   FEHLER: alle Bytes gleich — das ist kein Zufall.");
                return 1;
            }
        }
        Err(fehler) => {
            // KEIN Weitermachen: Ohne Zufall ist TLS wertlos, und ein
            // Spike, der das uebergeht, beweist das Falsche.
            println!("2) Zufall: FEHLGESCHLAGEN ({})", fehler.text());
            println!("   Ohne Zufall gibt es kein TLS. Abbruch.");
            return 1;
        }
    }

    // --- 3. ZEIT ---
    match libspeed::zeit_geprueft() {
        Ok(unix) => println!("3) Zeit:   {} (UNIX-Sekunden, UTC, geprueft)", unix),
        Err(fehler) => {
            println!("3) Zeit:   UNPLAUSIBEL ({})", fehler.text());
            println!("   rustls bekommt dann `None` und lehnt die Pruefung ab —");
            println!("   der Spike laeuft weiter, um genau das zu zeigen.");
        }
    }

    // --- 4. WURZELZERTIFIKATE ---
    let mut wurzeln = rustls::RootCertStore::empty();
    let (gelesen, uebernommen) = wurzeln_laden(&mut wurzeln);
    println!(
        "4) Wurzeln: {} aus dem Buendel gelesen, {} von rustls uebernommen",
        gelesen, uebernommen
    );
    if uebernommen == 0 {
        println!("   WARNUNG: kein Vertrauensanker — ein echter Handshake");
        println!("   koennte niemanden pruefen (docs/tls-vertrauen.md).");
    }

    // --- 5. DIE CLIENTCONFIG ---
    // Der eigentliche Machbarkeits-Test: Baut rustls seinen Zustand auf,
    // ohne dass etwas fehlt?
    let anbieter = Arc::new(rustls_rustcrypto::provider());
    println!(
        "5) Krypto:  {} Ciphersuites vom RustCrypto-Anbieter",
        anbieter.cipher_suites.len()
    );

    let konfig = match rustls::ClientConfig::builder_with_details(anbieter, Arc::new(SpeedUhr))
        .with_safe_default_protocol_versions()
    {
        Ok(builder) => builder
            .with_root_certificates(wurzeln)
            .with_no_client_auth(),
        Err(fehler) => {
            println!("   ClientConfig fehlgeschlagen: {:?}", fehler);
            return 1;
        }
    };
    println!("6) Konfig:  ClientConfig steht");

    // --- 6. DIE VERBINDUNG (Zustand, kein Netzverkehr) ---
    let name = match rustls::pki_types::ServerName::try_from(ziel) {
        Ok(name) => name.to_owned(),
        Err(_) => {
            println!("   '{}' ist kein gueltiger Servername.", ziel);
            return 2;
        }
    };
    match rustls::client::UnbufferedClientConnection::new(Arc::new(konfig), name) {
        Ok(verbindung) => {
            core::hint::black_box(&verbindung);
            println!("7) Zustand: ClientConnection fuer '{}' angelegt", ziel);
        }
        Err(fehler) => {
            println!("7) Zustand: FEHLGESCHLAGEN: {:?}", fehler);
            return 1;
        }
    }

    let (belegt, gesamt, spitze) = libspeed::heap::heap_stand();
    println!();
    println!("=== ERGEBNIS ===");
    println!("  rustls laeuft in Ring 3. Kein Absturz.");
    println!("  Heap-SPITZE:  {} Byte  <- die Zahl, auf die es ankommt", spitze);
    println!("  Heap-Endstand: {} Byte belegt, {} Byte gemappt", belegt, gesamt);
    println!("  ES WURDE KEIN HANDSHAKE GEFUEHRT — das ist der naechste Schritt.");
    0
}

/// Laedt die Wurzelzertifikate aus dem Buendel in den rustls-Speicher.
///
/// Liefert `(aus dem PEM gelesen, von rustls uebernommen)`. Die zweite Zahl
/// kann KLEINER sein: rustls-webpki lehnt Zertifikate ab, die es nicht
/// versteht — und das ist richtig so. Unser PEM-Parser prueft nichts
/// (docs/tls-vertrauen.md §4), rustls schon.
fn wurzeln_laden(speicher: &mut rustls::RootCertStore) -> (usize, usize) {
    let puffer = datei_puffer();
    let laenge = match datei_lesen(BUENDEL, puffer) {
        Ok(laenge) => laenge,
        Err(_) => return (0, 0),
    };
    let der = der_puffer();
    let mut uebernommen = 0usize;
    let bestand = pem::bloecke_durchgehen(&puffer[..laenge], der, |block| {
        // `to_vec` ist noetig: rustls will die DER-Bytes besitzen, unser
        // Arbeitspuffer wird beim naechsten Block ueberschrieben. Genau
        // dafuer gibt es jetzt einen Heap.
        let zert = rustls::pki_types::CertificateDer::from(block.der.to_vec());
        if speicher.add(zert).is_ok() {
            uebernommen += 1;
        }
    });
    (bestand.gelesen, uebernommen)
}

fn datei_lesen(pfad: &str, ziel: &mut [u8]) -> Result<usize, libspeed::Fehler> {
    let handle = libspeed::oeffne(pfad, libspeed::LESEN)?;
    let mut gelesen = 0usize;
    loop {
        let rest = &mut ziel[gelesen..];
        if rest.is_empty() {
            break;
        }
        let stueck = rest.len().min(32 * 1024);
        match libspeed::lese_at(handle, gelesen as u64, &mut rest[..stueck]) {
            Ok(0) => break,
            Ok(n) => gelesen += n as usize,
            Err(fehler) => {
                let _ = libspeed::schliesse(handle);
                return Err(fehler);
            }
        }
    }
    let _ = libspeed::schliesse(handle);
    Ok(gelesen)
}
