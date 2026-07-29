// holes <url> [--info] [zieldatei] — HTTPS aus SpeedOS
//
// ==========================================================================
// DAS IST DER MEILENSTEIN VON SERIE 7.
//
// Wenn dieses Programm laeuft, steht eine VERSCHLUESSELTE Verbindung, und
// zwar ueber eine Kette, in der kein Glied geliehen ist ausser der
// Krypto-Bibliothek selbst:
//
//   Ring 3 (dieses Programm, eigener Adressraum, von /platte geladen)
//     -> rustls + rustls-rustcrypto     TLS 1.3      (fremd, geprueft)
//     -> libspeed::tls::TlsStrom        die Naht     (Serie 7, Teil 4)
//     -> int 0x80                       Syscall-ABI  (Serie 6, Teil 4)
//     -> socket::* / tcp.rs             eigenes TCP  (Serie 5)
//     -> ipv4.rs / arp.rs               eigenes IP   (Serie 5)
//     -> virtio/net.rs                  eigener NIC  (Serie 5)
//     -> QEMU slirp                     -> das echte Internet
//
// Der Zufall fuer den Handshake kommt aus `src/zufall.rs` (Serie 7, Teil 1),
// die Zeit fuer die Gueltigkeitspruefung aus `zeit_geprueft` (Teil 2), die
// Wurzelzertifikate von /platte/system/ca-bundle.pem (Teil 2), der Heap aus
// `SYS_SPEICHER` (Teil 3).
//
// ==========================================================================
// UND DER HTTP-PARSER IST DERSELBE WIE IN SERIE 5.
//
// `speedhttp::antwort_parsen` ist Zeile fuer Zeile der Code, der seit Serie 5
// im Kernel die http-Antworten zerlegt. Er musste fuer TLS NICHT angefasst
// werden — er bekommt hier schlicht einen anderen Lieferanten fuer seine
// Bytes. Genau das ist die Aussage dieses Programms, noch vor der
// Verschluesselung: Die Schichtgrenze liegt an der richtigen Stelle.
//
// ==========================================================================
// ES GIBT KEINEN UNSICHER-SCHALTER.
//
// Kein `--kein-zertifikat`, kein `--unsicher`, keine Nachfrage „trotzdem
// fortfahren?". Jeder Pruefungsfehler beendet das Programm mit einem eigenen
// Exit-Code und einer deutschen Begruendung. Ein Schalter, der die Pruefung
// abschaltet, wird benutzt — und dann schuetzt TLS vor genau dem Angreifer
// nicht mehr, der ihn provoziert hat.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use libspeed::tls::{TcpStrom, TlsFehler, TlsStrom};
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

// Der Zufall des Kernels wird das getrandom-Backend (siehe libspeed::tls).
libspeed::zufall_als_getrandom!();

/// Wo der Vertrauensanker liegen kann (Serie 7, Teil 2).
///
/// ZWEI Orte, in dieser Reihenfolge — das ist die Ring-3-Entsprechung von
/// `fs::persistenter_pfad` im Kernel: Ist eine Platte gemountet, liegt das
/// Buendel dort; ohne Platte (Live-USB, RAM-VFS) unter /system. Ein
/// festverdrahtetes /platte/... haette `holes` beim USB-Boot ohne
/// Vertrauensanker dastehen lassen — und das haette nicht wie ein fehlendes
/// Buendel ausgesehen, sondern wie ein kaputtes TLS.
const BUENDEL_ORTE: &[&str] = &[
    "/platte/system/ca-bundle.pem",
    "/system/ca-bundle.pem",
];
/// Obergrenze fuer den Rumpf, den wir im Speicher halten.
const MAX_ANTWORT: usize = speedhttp::MAX_ANTWORT;
/// Empfangs-Stueck je `lesen`-Aufruf.
const STUECK: usize = 8192;

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
/// Ende soll den TLS-Bedarf zeigen und nicht das Einlesen einer 220-KiB-Datei;
/// laege der Puffer auf dem Heap, waere die Spitze eine Aussage ueber die
/// Groesse des CA-Buendels.
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
    zieldatei: Option<&'a str>,
    /// Nur die Zahlen ausgeben (fuer die Messung).
    still: bool,
}

fn haupt(argumente: &Argumente) -> i32 {
    let auftrag = match auftrag_lesen(argumente) {
        Some(auftrag) => auftrag,
        None => {
            println!("Benutzung: {} <url> [--info] [zieldatei]", argumente.programm());
            println!("  {} https://example.com", argumente.programm());
            println!("  {} https://example.com --info", argumente.programm());
            println!("  {} https://example.com /platte/heim/seite.html", argumente.programm());
            println!();
            println!("  --info   Protokollversion, Ciphersuite und Zertifikatskette zeigen");
            println!("  --still  nur die Kennzahlen ausgeben");
            println!();
            println!("Es gibt KEINEN Schalter, der die Zertifikatspruefung abschaltet.");
            return FEHLER_BEDIENUNG;
        }
    };
    holen(&auftrag)
}

fn auftrag_lesen<'a>(argumente: &'a Argumente) -> Option<Auftrag<'a>> {
    let mut url = None;
    let mut zieldatei = None;
    let mut info = false;
    let mut still = false;
    for i in 1..argumente.anzahl() {
        let wort = argumente.get(i)?;
        match wort {
            "--info" => info = true,
            "--still" => still = true,
            _ if wort.starts_with("--") => return None,
            _ if url.is_none() => url = Some(wort),
            _ => zieldatei = Some(wort),
        }
    }
    Some(Auftrag {
        url: url?,
        info,
        zieldatei,
        still,
    })
}

// ===========================================================================
// DER ABLAUF
// ===========================================================================

fn holen(auftrag: &Auftrag) -> i32 {
    let laut = !auftrag.still;

    // --- 1. Die URL zerlegen ---
    //
    // Das Schema schneiden WIR ab, nicht der Parser: `speedhttp::url_parsen`
    // stammt aus Serie 5, kennt kein TLS und lehnt `https://` ab. Es nimmt
    // aber schemalose Eingaben an — also bekommt es `host[:port]/pfad` und
    // bleibt so, wie es ist. (Das ist die ganze „Anpassung", die noetig war.)
    let (tls, rest, port_vorgabe) = if let Some(rest) = auftrag.url.strip_prefix("https://") {
        (true, rest, 443u16)
    } else if let Some(rest) = auftrag.url.strip_prefix("http://") {
        (false, rest, 80u16)
    } else {
        // Ohne Schema: https annehmen. 2026 ist das die richtige Voreinstellung.
        (true, auftrag.url, 443u16)
    };
    if !tls {
        println!("holes spricht https. Fuer http:// gibt es `netzhole` oder den");
        println!("Shell-Befehl `hole`.");
        return FEHLER_BEDIENUNG;
    }

    let mut url = match speedhttp::url_parsen(rest) {
        Ok(url) => url,
        Err(fehler) => {
            println!("Ungueltige Adresse: {}", fehler.meldung());
            return FEHLER_BEDIENUNG;
        }
    };
    // `url_parsen` setzt mangels Schema 80 ein, wenn kein Port dasteht.
    // Stand einer da, hat es ihn uebernommen — dann bleibt er.
    let port_angegeben = rest
        .split('/')
        .next()
        .map(|autoritaet| autoritaet.contains(':'))
        .unwrap_or(false);
    if !port_angegeben {
        url.port = port_vorgabe;
    }

    let gastgeber = url.host.clone();
    if laut {
        println!("holes: https://{}:{}{}", gastgeber, url.port, url.pfad);
    }

    // --- 2. Vertrauensanker laden ---
    let mut wurzeln = rustls::RootCertStore::empty();
    let bestand = {
        let puffer = datei_puffer();
        let mut gefunden = None;
        for ort in BUENDEL_ORTE {
            if let Ok(laenge) = datei_lesen(ort, puffer) {
                gefunden = Some((*ort, laenge));
                break;
            }
        }
        let (ort, laenge) = match gefunden {
            Some(paar) => paar,
            None => {
                println!("Kein Vertrauensanker gefunden. Gesucht wurde in:");
                for ort in BUENDEL_ORTE {
                    println!("  {}", ort);
                }
                println!("Ohne Wurzelzertifikate wird NICHT verbunden.");
                return FEHLER_TLS;
            }
        };
        let bestand = libspeed::tls::wurzeln_laden(&puffer[..laenge], der_puffer(), &mut wurzeln);
        if laut {
            println!(
                "  Vertrauensanker: {} Wurzeln uebernommen (von {} gelesen, {} verworfen) \
                 aus {}",
                bestand.uebernommen, bestand.gelesen, bestand.kaputt, ort
            );
        }
        bestand
    };
    let _ = bestand;

    let konfig = match libspeed::tls::konfig_bauen(wurzeln) {
        Ok(konfig) => konfig,
        Err(fehler) => {
            println!("{}", fehler.text());
            return FEHLER_TLS;
        }
    };

    // --- 3. Namensaufloesung ---
    let ip = match ip_aus_text(&gastgeber) {
        Some(ip) => ip,
        None => match libspeed::aufloesen(&gastgeber) {
            Ok(ip) => ip,
            Err(fehler) => {
                println!("DNS fuer '{}' fehlgeschlagen: {}.", gastgeber, fehler.text());
                return FEHLER_NETZ;
            }
        },
    };
    if laut {
        println!(
            "  IP: {}.{}.{}.{}",
            (ip >> 24) & 0xff,
            (ip >> 16) & 0xff,
            (ip >> 8) & 0xff,
            ip & 0xff
        );
    }

    // --- 4. TCP ---
    let tcp_start = libspeed::zeit_jetzt();
    let tcp = match TcpStrom::verbinden(ip, url.port) {
        Ok(tcp) => tcp,
        Err(fehler) => {
            println!("TCP-Verbindung fehlgeschlagen: {}.", fehler.text());
            return FEHLER_NETZ;
        }
    };
    let tcp_ms = libspeed::zeit_jetzt() - tcp_start;

    // --- 5. TLS ---
    //
    // `gastgeber` geht hier zweimal ein, und beide Male zwingend:
    //   * als SNI, damit der Server das richtige Zertifikat schickt,
    //   * als Pruefname, gegen den rustls die Namen im Zertifikat abgleicht.
    // Beides macht `TlsStrom::verbinden` aus DIESEM einen Argument — es gibt
    // keinen Weg, nur das eine zu tun.
    let mut strom = match TlsStrom::verbinden(tcp, konfig, &gastgeber) {
        Ok(strom) => strom,
        Err(fehler) => {
            fehler_melden(&fehler);
            return FEHLER_TLS;
        }
    };
    let handshake_ms = strom.handshake_ms();

    if laut {
        println!(
            // Bindestrich statt Gedankenstrich: Die FramebufferKonsole ist
            // Latin-1, ein "—" wird dort zu "?" (CLAUDE.md, Serie-4-Abschluss).
            "  TLS: {} / {} - Handshake in {} ms (TCP {} ms)",
            strom.protokoll_text(),
            strom.ciphersuite_text(),
            handshake_ms,
            tcp_ms
        );
    }
    if auftrag.info {
        kette_zeigen(&strom);
    }

    // --- 6. HTTP ueber den verschluesselten Strom ---
    //
    // AB HIER IST NICHTS MEHR TLS-SPEZIFISCH. Anfrage bauen, Bytes senden,
    // Bytes sammeln, parsen — derselbe Ablauf wie in `netzhole`, nur dass
    // `strom` verschluesselt.
    let host_kopf = if url.port == 443 {
        gastgeber.clone()
    } else {
        alloc::format!("{}:{}", gastgeber, url.port)
    };
    let anfrage = speedhttp::anfrage_bauen_mit_host(&url, &host_kopf);
    if let Err(fehler) = strom.schreiben(anfrage.as_bytes()) {
        fehler_melden(&fehler);
        return FEHLER_NETZ;
    }

    let uebertragung_start = libspeed::zeit_jetzt();
    let mut roh: Vec<u8> = Vec::new();
    let mut stueck = alloc::vec![0u8; STUECK];
    loop {
        match strom.lesen(&mut stueck) {
            Ok(0) => break, // Ende des Stroms
            Ok(n) => {
                if roh.len() + n > MAX_ANTWORT {
                    println!("Die Antwort ist groesser als {} KiB. Abgebrochen.", MAX_ANTWORT / 1024);
                    return FEHLER_HTTP;
                }
                roh.extend_from_slice(&stueck[..n]);
            }
            Err(fehler) => {
                fehler_melden(&fehler);
                return FEHLER_NETZ;
            }
        }
    }
    let uebertragung_ms = libspeed::zeit_jetzt() - uebertragung_start;
    strom.schliessen();

    // --- 7. DER PARSER AUS SERIE 5, UNVERAENDERT ---
    let antwort = match speedhttp::antwort_parsen(&roh) {
        Ok(antwort) => antwort,
        Err(fehler) => {
            // LAUT SEIN heisst auch: die Zahlen nennen, mit denen sich der
            // Fehler nachvollziehen laesst. „Unvollstaendig" allein sagt
            // nicht, ob 10 oder 100000 Byte fehlen — und genau das ist der
            // Unterschied zwischen einem Verbindungsabbruch und einem Bug.
            println!("Die Antwort war nicht lesbar: {}.", fehler.meldung());
            println!("  angekommen: {} Byte roh", roh.len());
            if let Some(laenge) = kopf_zeile_wert(&roh, b"content-length:") {
                println!("  angekuendigt: Content-Length: {}", laenge);
            }
            println!("  Uebertragung lief {} ms.", uebertragung_ms);
            return FEHLER_HTTP;
        }
    };

    // --- 8. Ausgabe ---
    let (belegt, gemappt, spitze) = libspeed::heap::heap_stand();
    if auftrag.still {
        // Eine Zeile, maschinenlesbar — dafuer gibt es --still.
        println!(
            "MESSUNG tcp_ms={} handshake_ms={} uebertragung_ms={} roh={} rumpf={} \
             heap_spitze={} heap_belegt={} heap_gemappt={} protokoll={} suite={}",
            tcp_ms,
            handshake_ms,
            uebertragung_ms,
            roh.len(),
            antwort.rumpf.len(),
            spitze,
            belegt,
            gemappt,
            strom.protokoll_text(),
            strom.ciphersuite_text()
        );
        return OK;
    }

    println!();
    println!("HTTP {} {}", antwort.status, antwort.grund);
    for (name, wert) in &antwort.header {
        println!("  {}: {}", name, wert);
    }
    println!(
        "--- Rumpf: {} Byte (roh {} Byte in {} ms) ---",
        antwort.rumpf.len(),
        roh.len(),
        uebertragung_ms
    );

    let code = match auftrag.zieldatei {
        Some(pfad) => match speichern(pfad, &antwort.rumpf) {
            Ok(()) => {
                println!("{} Byte nach '{}' geschrieben.", antwort.rumpf.len(), pfad);
                OK
            }
            Err(fehler) => {
                println!("Speichern nach '{}' fehlgeschlagen: {}.", pfad, fehler.text());
                FEHLER_DATEI
            }
        },
        None => {
            // Als BYTES ausgeben: Eine Webseite ist nicht zwingend UTF-8.
            let _ = libspeed::schreibe(libspeed::AUSGABE, &antwort.rumpf);
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

/// Meldet einen TLS-Fehler LAUT — die Dauerregel aus CLAUDE.md, angewandt.
///
/// Auf die normale Ausgabe, nicht auf den Diagnose-Kanal: Das ist keine
/// Entwickler-Information, sondern das Ergebnis. Wer `holes` benutzt, muss
/// den Grund sehen, ohne die serielle Schnittstelle mitzulesen.
fn fehler_melden(fehler: &TlsFehler) {
    println!();
    println!("VERBINDUNG ABGELEHNT ({}).", fehler.kurz());
    println!("{}", fehler.text());
    println!();
    println!("Es gibt keinen Schalter, der diese Pruefung uebergeht.");
    // Der technische Wortlaut zusaetzlich auf den Diagnose-Kanal — fuer den
    // Entwickler, ohne die Ausgabe des Programms zu verunreinigen.
    libspeed::diagnoseln!("[holes] {:?}", fehler);
}

/// Zeigt die Zertifikatskette, die die Gegenstelle vorgelegt hat.
///
/// GEPRUEFT hat sie rustls-webpki, BEVOR es diesen Strom gab. Was hier
/// passiert, ist reine Anzeige mit unserem Minimal-DER-Laeufer aus Serie 7,
/// Teil 2 — er validiert nichts und darf das auch gar nicht.
fn kette_zeigen(strom: &TlsStrom) {
    let kette = strom.kette();
    println!();
    println!("  === Zertifikatskette ({} Glieder, geprueft und angenommen) ===", kette.len());
    if kette.is_empty() {
        println!("  (die Gegenstelle hat keine Kette hinterlassen)");
        return;
    }
    let jetzt = libspeed::zeit_geprueft().ok();
    for (nummer, zertifikat) in kette.iter().enumerate() {
        let info = libspeed::pem::kurzinfo(zertifikat.as_ref());
        let rolle = if nummer == 0 { "Server " } else { "Zwischen" };
        println!(
            "  [{}] {}  {}",
            nummer,
            rolle,
            core::str::from_utf8(info.name).unwrap_or("(Name nicht lesbar)")
        );
        println!(
            "      gueltig {} .. {} (UNIX-Sekunden){}",
            info.gueltig_ab,
            info.gueltig_bis,
            match jetzt {
                Some(jetzt) if jetzt < info.gueltig_bis => "",
                Some(_) => "  <- ABGELAUFEN?!",
                None => "  (Uhr unplausibel, nicht bewertet)",
            }
        );
        println!("      {} Byte DER", zertifikat.as_ref().len());
    }
    println!("  Die Wurzel selbst kommt aus dem Vertrauensanker und steht nicht in der Kette.");
}

// ===========================================================================
// Kleinkram
// ===========================================================================

fn datei_lesen(pfad: &str, ziel: &mut [u8]) -> Result<usize, libspeed::Fehler> {
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

fn speichern(pfad: &str, daten: &[u8]) -> Result<(), libspeed::Fehler> {
    let handle = libspeed::oeffne(
        pfad,
        libspeed::SCHREIBEN | libspeed::ANLEGEN | libspeed::ABSCHNEIDEN,
    )?;
    let mut geschrieben = 0usize;
    let ergebnis = loop {
        if geschrieben == daten.len() {
            break Ok(());
        }
        // Ein Syscall uebernimmt hoechstens MAX_PUFFER (64 KiB).
        let rest = daten.len() - geschrieben;
        let jetzt = rest.min(32 * 1024);
        match libspeed::schreibe_at(
            handle,
            geschrieben as u64,
            &daten[geschrieben..geschrieben + jetzt],
        ) {
            Ok(n) if n > 0 => geschrieben += n as usize,
            Ok(_) => break Err(libspeed::Fehler::KEIN_PLATZ),
            Err(fehler) => break Err(fehler),
        }
    };
    let _ = libspeed::schliesse(handle);
    ergebnis
}

/// Sucht im ROHEN Kopf eine Zeile und liefert ihren Wert — nur fuer die
/// Fehlermeldung oben, wenn der richtige Parser schon aufgegeben hat.
fn kopf_zeile_wert<'a>(roh: &'a [u8], name_klein: &[u8]) -> Option<&'a str> {
    let kopf = &roh[..roh.len().min(8192)];
    let mut i = 0usize;
    while i + name_klein.len() <= kopf.len() {
        let kandidat = &kopf[i..i + name_klein.len()];
        if kandidat.eq_ignore_ascii_case(name_klein) {
            let rest = &kopf[i + name_klein.len()..];
            let ende = rest.iter().position(|b| *b == b'\r' || *b == b'\n')?;
            return core::str::from_utf8(&rest[..ende]).ok().map(|t| t.trim());
        }
        i += 1;
    }
    None
}

/// Erkennt eine reine IPv4-Adresse ("10.0.2.2"). `None` heisst „das ist ein
/// Name, frag DNS".
fn ip_aus_text(text: &str) -> Option<u32> {
    let mut teile = [0u32; 4];
    let mut anzahl = 0usize;
    for stueck in text.split('.') {
        if anzahl == 4 || stueck.is_empty() || stueck.len() > 3 {
            return None;
        }
        let mut wert = 0u32;
        for ziffer in stueck.bytes() {
            if !ziffer.is_ascii_digit() {
                return None;
            }
            wert = wert * 10 + (ziffer - b'0') as u32;
        }
        if wert > 255 {
            return None;
        }
        teile[anzahl] = wert;
        anzahl += 1;
    }
    if anzahl != 4 {
        return None;
    }
    Some((teile[0] << 24) | (teile[1] << 16) | (teile[2] << 8) | teile[3])
}
