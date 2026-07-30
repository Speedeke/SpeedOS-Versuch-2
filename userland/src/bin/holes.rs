// holes <url> [--info] [--still] [--max=N] [--frist=MS] [zieldatei]
//   — HTTP und HTTPS aus SpeedOS
//
// ==========================================================================
// SEIT SERIE 7, TEIL 5 IST DIESES PROGRAMM EINE BEDIENOBERFLAECHE.
//
// In Teil 4 stand hier der ganze Ablauf: Schema abschneiden, DNS,
// verbinden, Wurzeln laden, Handshake, Anfrage bauen, Rumpf einsammeln.
// Er steht jetzt in `libspeed::netz` — und zwar EINMAL, fuer alle
// Programme. Was hier geblieben ist: Argumente lesen, Ergebnis anzeigen,
// Fehler erklaeren. Das ist der ganze Unterschied zwischen einer Demo und
// einer Systemfaehigkeit.
//
// Der Beweis, dass die Schicht traegt, steht nebenan: `news` benutzt
// dieselben drei Zeilen und macht etwas voellig anderes damit.
//
// ==========================================================================
// DIE KETTE, die dabei laeuft (unveraendert seit Teil 4):
//
//   Ring 3 (dieses Programm, eigener Adressraum, von /platte geladen)
//     -> libspeed::netz          Abruf-Schicht                (Teil 5)
//     -> rustls + rustcrypto     TLS 1.3 / 1.2                (fremd)
//     -> libspeed::tls           die Naht                     (Teil 4)
//     -> int 0x80                Syscall-ABI                  (Serie 6)
//     -> socket/tcp/ipv4/arp     eigener Stack                (Serie 5)
//     -> virtio/net.rs           eigener NIC-Treiber          (Serie 5)
//     -> QEMU slirp              -> das echte Internet
//
// ==========================================================================
// ES GIBT KEINEN UNSICHER-SCHALTER.
//
// Kein `--kein-zertifikat`, kein `--unsicher`, keine Nachfrage „trotzdem
// fortfahren?". Jeder Pruefungsfehler beendet das Programm mit Exit 4 und
// einer deutschen Begruendung.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use libspeed::netz::{AbrufFehler, Klient};
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

// Der Zufall des Kernels wird das getrandom-Backend (siehe libspeed::tls).
libspeed::zufall_als_getrandom!();

// ---------------------------------------------------------------------------
// Exit-Codes — damit ein Test (und ein Skript) den Grund maschinell sieht
// ---------------------------------------------------------------------------
const OK: i32 = 0;
const FEHLER_BEDIENUNG: i32 = 2;
const FEHLER_NETZ: i32 = 3;
/// DIE ZERTIFIKATSPRUEFUNG HAT ABGELEHNT. Ein eigener Code, weil das kein
/// gewoehnlicher Netzfehler ist, sondern der Fall, fuer den es TLS gibt.
const FEHLER_TLS: i32 = 4;
const FEHLER_HTTP: i32 = 5;
const FEHLER_DATEI: i32 = 6;

/// Lesepuffer fuer das CA-Buendel — bewusst in `.bss` und NICHT auf dem Heap.
///
/// Zwei Gruende: (1) 64 KiB Stack reichen dafuer nie. (2) Die Heap-Messung am
/// Ende soll den TLS-Bedarf zeigen und nicht das Einlesen einer 190-KiB-Datei;
/// laege der Puffer auf dem Heap, waere die Spitze eine Aussage ueber die
/// Groesse des CA-Buendels. Deshalb baut `holes` seine TLS-Konfiguration
/// selbst und gibt sie dem Klienten mit, statt `Klient::neu()` zu nehmen.
static mut DATEI: [u8; 512 * 1024] = [0; 512 * 1024];
static mut DER: [u8; libspeed::pem::MAX_DER_BYTES] = [0; libspeed::pem::MAX_DER_BYTES];

/// # Safety
/// Ein SpeedOS-Prozess hat einen Ausfuehrungsstrang; es gibt genau einen
/// Aufrufer und damit nie zwei gleichzeitige `&mut`.
fn datei_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DATEI) }
}
fn der_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DER) }
}

/// Was die Kommandozeile hergab.
struct Auftrag<'a> {
    url: &'a str,
    info: bool,
    still: bool,
    zieldatei: Option<&'a str>,
    max_bytes: Option<usize>,
    frist_ms: Option<u64>,
}

fn haupt(argumente: &Argumente) -> i32 {
    let auftrag = match auftrag_lesen(argumente) {
        Some(auftrag) => auftrag,
        None => {
            hilfe(argumente.programm());
            return FEHLER_BEDIENUNG;
        }
    };
    holen(&auftrag)
}

fn hilfe(programm: &str) {
    println!("Benutzung: {} <url> [schalter ...] [zieldatei]", programm);
    println!("  {} https://example.com", programm);
    println!("  {} example.com --info", programm);
    println!("  {} http://10.0.2.2:8000/datei.txt /platte/heim/datei.txt", programm);
    println!();
    println!("  --info        Protokollversion, Ciphersuite und Zertifikatskette");
    println!("  --still       nur eine maschinenlesbare Messzeile");
    println!("  --max=N       hoechstens N Byte annehmen (Standard {})", libspeed::netz::MAX_BYTES);
    println!("  --frist=MS    Frist je Versuch (Standard {} ms)", libspeed::netz::FRIST_MS);
    println!();
    println!("Ohne Schema wird https angenommen. http:// geht auch.");
    println!("Es gibt KEINEN Schalter, der die Zertifikatspruefung abschaltet.");
}

fn auftrag_lesen<'a>(argumente: &'a Argumente) -> Option<Auftrag<'a>> {
    let mut auftrag = Auftrag {
        url: "",
        info: false,
        still: false,
        zieldatei: None,
        max_bytes: None,
        frist_ms: None,
    };
    let mut url = None;
    for i in 1..argumente.anzahl() {
        let wort = argumente.get(i)?;
        if let Some(wert) = wort.strip_prefix("--max=") {
            auftrag.max_bytes = Some(wert.parse().ok()?);
        } else if let Some(wert) = wort.strip_prefix("--frist=") {
            auftrag.frist_ms = Some(wert.parse().ok()?);
        } else {
            match wort {
                "--info" => auftrag.info = true,
                "--still" => auftrag.still = true,
                _ if wort.starts_with("--") => return None,
                _ if url.is_none() => url = Some(wort),
                _ => auftrag.zieldatei = Some(wort),
            }
        }
    }
    auftrag.url = url?;
    Some(auftrag)
}

// ===========================================================================
// DER ABLAUF — und wie kurz er geworden ist
// ===========================================================================

fn holen(auftrag: &Auftrag) -> i32 {
    let laut = !auftrag.still;

    // --- Der Klient ---
    //
    // Mit EIGENER TLS-Konfiguration, damit die Wurzeln in `.bss` gelesen
    // werden und die Heap-Zahl am Ende den TLS-Bedarf misst. Wer das nicht
    // braucht, schreibt `Klient::neu()` — eine Zeile.
    let mut klient = match konfig_aus_bss(laut) {
        Ok(konfig) => Klient::mit_konfig(konfig),
        Err(code) => return code,
    };
    if let Some(max) = auftrag.max_bytes {
        klient.max_bytes = max;
    }
    if let Some(frist) = auftrag.frist_ms {
        klient.frist_ms = frist;
    }
    // Die Kette kostet eine Kopie — nur holen, wenn sie gezeigt wird.
    klient.kette_behalten = auftrag.info;

    if laut {
        println!("holes: {}", auftrag.url);
    }

    // --- Der Abruf. Das ist alles. ---
    let abruf = match klient.holen(auftrag.url) {
        Ok(abruf) => abruf,
        Err(fehler) => {
            fehler_melden(&fehler);
            return code_fuer(&fehler);
        }
    };

    // --- Anzeige ---
    if laut {
        if abruf.info.tls {
            println!(
                "  TLS: {} / {} - Handshake in {} ms (TCP {} ms)",
                abruf.info.protokoll, abruf.info.suite, abruf.info.handshake_ms, abruf.info.tcp_ms
            );
        } else {
            println!("  KLARTEXT (http) - kein TLS, TCP in {} ms", abruf.info.tcp_ms);
        }
        if abruf.weiterleitungen > 0 {
            println!(
                "  {} Weiterleitung(en) -> {}",
                abruf.weiterleitungen,
                abruf.ziel.als_text()
            );
        }
    }
    if auftrag.info {
        kette_zeigen(&abruf);
    }

    let (belegt, gemappt, spitze) = libspeed::heap::heap_stand();
    if auftrag.still {
        println!(
            "MESSUNG tcp_ms={} handshake_ms={} dauer_ms={} weiterleitungen={} roh={} \
             rumpf={} status={} heap_spitze={} heap_belegt={} heap_gemappt={} \
             protokoll={} suite={}",
            abruf.info.tcp_ms,
            abruf.info.handshake_ms,
            abruf.dauer_ms,
            abruf.weiterleitungen,
            abruf.roh_bytes,
            abruf.antwort.rumpf.len(),
            abruf.antwort.status,
            spitze,
            belegt,
            gemappt,
            if abruf.info.tls { abruf.info.protokoll } else { "http" },
            abruf.info.suite
        );
        return OK;
    }

    println!();
    println!("HTTP {} {}", abruf.antwort.status, abruf.antwort.grund);
    for (name, wert) in &abruf.antwort.header {
        println!("  {}: {}", name, wert);
    }
    println!(
        "--- Rumpf: {} Byte (roh {} Byte in {} ms) ---",
        abruf.antwort.rumpf.len(),
        abruf.roh_bytes,
        abruf.dauer_ms
    );

    let code = match auftrag.zieldatei {
        Some(pfad) => match libspeed::netz::speichern(pfad, &abruf.antwort.rumpf) {
            Ok(()) => {
                println!("{} Byte nach '{}' geschrieben.", abruf.antwort.rumpf.len(), pfad);
                OK
            }
            Err(fehler) => {
                println!("Speichern nach '{}' fehlgeschlagen: {}.", pfad, fehler.text());
                FEHLER_DATEI
            }
        },
        None => {
            // Als BYTES ausgeben: Eine Webseite ist nicht zwingend UTF-8.
            let _ = libspeed::schreibe(libspeed::AUSGABE, &abruf.antwort.rumpf);
            println!();
            OK
        }
    };

    println!(
        "Heap: Spitze {} Byte, jetzt {} von {} Byte gemappt.",
        spitze, belegt, gemappt
    );
    code
}

/// Baut die TLS-Konfiguration mit den `.bss`-Puffern (siehe oben).
fn konfig_aus_bss(laut: bool) -> Result<Arc<rustls::ClientConfig>, i32> {
    let mut wurzeln = rustls::RootCertStore::empty();
    let puffer = datei_puffer();
    let mut gefunden = None;
    for ort in libspeed::tls::BUENDEL_ORTE {
        if let Ok(laenge) = datei_in_puffer(ort, puffer) {
            gefunden = Some((*ort, laenge));
            break;
        }
    }
    let (ort, laenge) = match gefunden {
        Some(paar) => paar,
        None => {
            println!("Kein Vertrauensanker gefunden. Gesucht wurde in:");
            for ort in libspeed::tls::BUENDEL_ORTE {
                println!("  {}", ort);
            }
            println!("Ohne Wurzelzertifikate wird NICHT verbunden.");
            return Err(FEHLER_TLS);
        }
    };
    let bestand = libspeed::tls::wurzeln_laden(&puffer[..laenge], der_puffer(), &mut wurzeln);
    if laut {
        println!(
            "  Vertrauensanker: {} Wurzeln uebernommen (von {} gelesen, {} verworfen) aus {}",
            bestand.uebernommen, bestand.gelesen, bestand.kaputt, ort
        );
    }
    libspeed::tls::konfig_bauen(wurzeln).map_err(|fehler| {
        println!("{}", fehler.text());
        FEHLER_TLS
    })
}

fn datei_in_puffer(pfad: &str, ziel: &mut [u8]) -> Result<usize, libspeed::Fehler> {
    let handle = libspeed::oeffne(pfad, libspeed::LESEN)?;
    let mut gelesen = 0usize;
    let ergebnis = loop {
        let rest = &mut ziel[gelesen..];
        if rest.is_empty() {
            break Ok(gelesen);
        }
        let stueck = rest.len().min(32 * 1024);
        match libspeed::lese_at(handle, gelesen as u64, &mut rest[..stueck]) {
            Ok(0) => break Ok(gelesen),
            Ok(n) => gelesen += n as usize,
            Err(fehler) => break Err(fehler),
        }
    };
    let _ = libspeed::schliesse(handle);
    ergebnis
}

/// Welcher Exit-Code zu welchem Fehler gehoert.
fn code_fuer(fehler: &AbrufFehler) -> i32 {
    match fehler {
        AbrufFehler::Url(_) => FEHLER_BEDIENUNG,
        AbrufFehler::Tls(_) => FEHLER_TLS,
        AbrufFehler::Http(..) | AbrufFehler::ZuGross { .. } => FEHLER_HTTP,
        AbrufFehler::LeereAntwort => FEHLER_NETZ,
        AbrufFehler::ZuVieleWeiterleitungen(_) | AbrufFehler::Schleife(_) => FEHLER_HTTP,
        AbrufFehler::Dns(_) | AbrufFehler::Verbindung(_) | AbrufFehler::Frist(_) => FEHLER_NETZ,
    }
}

/// Meldet einen Fehler LAUT — die Dauerregel aus CLAUDE.md, angewandt.
///
/// Auf die normale Ausgabe, nicht auf den Diagnose-Kanal: Das ist keine
/// Entwickler-Information, sondern das Ergebnis.
fn fehler_melden(fehler: &AbrufFehler) {
    println!();
    if fehler.ist_sicherheitsfehler() {
        println!("VERBINDUNG ABGELEHNT ({}).", fehler.kurz());
    } else {
        println!("ABRUF FEHLGESCHLAGEN ({}).", fehler.kurz());
    }
    println!("{}", fehler.text());
    if fehler.ist_sicherheitsfehler() {
        println!();
        println!("Es gibt keinen Schalter, der diese Pruefung uebergeht.");
    }
    // Der technische Wortlaut zusaetzlich auf den Diagnose-Kanal — fuer den
    // Entwickler, ohne die Ausgabe des Programms zu verunreinigen.
    libspeed::diagnoseln!("[holes] {:?}", fehler);
}

/// Zeigt die Zertifikatskette, die die Gegenstelle vorgelegt hat.
///
/// GEPRUEFT hat sie rustls-webpki, BEVOR es diesen Abruf gab. Was hier
/// passiert, ist reine Anzeige mit unserem Minimal-DER-Laeufer aus Serie 7,
/// Teil 2 — er validiert nichts und darf das auch gar nicht.
fn kette_zeigen(abruf: &libspeed::netz::Abruf) {
    println!();
    if !abruf.info.tls {
        println!("  === Keine Zertifikatskette: die Verbindung lief im KLARTEXT. ===");
        return;
    }
    println!(
        "  === Zertifikatskette ({} Glieder, geprueft und angenommen) ===",
        abruf.info.kettenlaenge
    );
    println!("  Endgueltige Adresse: {}", abruf.ziel.als_text());
    let jetzt = libspeed::zeit_geprueft().ok();
    for (nummer, der) in abruf.info.kette.iter().enumerate() {
        let kurz = libspeed::pem::kurzinfo(der);
        let rolle = if nummer == 0 { "Server " } else { "Zwischen" };
        println!(
            "  [{}] {}  {}",
            nummer,
            rolle,
            core::str::from_utf8(kurz.name).unwrap_or("(Name nicht lesbar)")
        );
        println!(
            "      gueltig {} .. {} (UNIX-Sekunden){}",
            kurz.gueltig_ab,
            kurz.gueltig_bis,
            match jetzt {
                Some(jetzt) if jetzt < kurz.gueltig_bis => "",
                Some(_) => "  <- ABGELAUFEN?!",
                None => "  (Uhr unplausibel, nicht bewertet)",
            }
        );
        println!("      {} Byte DER", der.len());
    }
    println!("  Die Wurzel selbst kommt aus dem Vertrauensanker und steht nicht in der Kette.");
}
