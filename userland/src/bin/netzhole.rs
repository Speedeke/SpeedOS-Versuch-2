// netzhole <url> [zieldatei] — HTTP-GET aus dem User-Space
//
// ==========================================================================
// DAS IST DER MEILENSTEIN VON SERIE 6.
//
// Wenn dieses Programm laeuft, steht die GANZE Kette, und zwar ueber alle
// Schichten hinweg, die SpeedOS in fuenf Serien gebaut hat:
//
//   Ring 3 (dieses Programm, eigener Adressraum, von /platte geladen)
//     -> int 0x80          Syscall-ABI          (Serie 6, Teil 4)
//     -> socket::*         Socket-API           (Serie 5)
//     -> tcp.rs            eigene TCP-Schicht   (Serie 5)
//     -> ipv4.rs / arp.rs  eigener IP-Stack     (Serie 5)
//     -> virtio/net.rs     eigener NIC-Treiber  (Serie 5)
//     -> QEMU slirp        -> das echte Internet
//
// Kein Byte davon ist geliehen. Und dieses Programm hier weiss von alledem
// NICHTS — es kennt nur Handles und Zahlen.
// ==========================================================================
//
// Bewusst minimal: HTTP/1.0-artiges GET mit `Connection: close`, keine
// Weiterleitungen, kein chunked-Decoding, kein HTTPS (dafuer fehlt TLS —
// der bekannte Browser-Blocker aus docs/serie6-bestandsaufnahme.md). Wer
// mehr will, nimmt den Shell-Befehl `hole`, der im Kernel den vollen
// HTTP-Client benutzt.

#![no_std]
#![no_main]

use libspeed::{println, Argumente, Fehler};

libspeed::hauptprogramm!(haupt);

/// Wie lange auf die vollstaendige Antwort gewartet wird.
const FRIST_MS: u64 = 20_000;
/// Empfangs-Stueck je `empfange`-Aufruf.
const STUECK: usize = 4096;
/// Wie viel Antwort wir im Speicher halten (in `.bss`, siehe unten).
const ANTWORT_MAX: usize = 64 * 1024;

/// Der Sammelpuffer fuer die Antwort.
///
/// Er liegt bewusst als `static` in der `.bss` und nicht auf dem Stack: 64
/// KiB waeren der GANZE Stack. Nebenbei ist er damit der Beweis, dass unser
/// ELF-Loader `.bss` richtig behandelt — diese 64 KiB stehen NICHT in der
/// Programmdatei (`p_filesz` < `p_memsz`), der Kernel muss sie beim Laden
/// als genullten Speicher bereitstellen.
static mut ANTWORT: [u8; ANTWORT_MAX] = [0; ANTWORT_MAX];

/// Zugriff auf den Sammelpuffer.
///
/// Ein SpeedOS-Prozess ist einfaedig — es gibt genau einen Aufrufer, also
/// keine zwei gleichzeitigen `&mut`. `addr_of_mut!` statt `&mut ANTWORT`,
/// weil eine Referenz auf ein `static mut` sonst schon beim Bilden eine
/// Regelverletzung waere.
fn antwort_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(ANTWORT) }
}

fn haupt(argumente: &Argumente) -> i32 {
    let url = match argumente.get(1) {
        Some(url) => url,
        None => {
            println!("Benutzung: {} <url> [zieldatei]", argumente.programm());
            println!("Beispiel:  netzhole http://example.com");
            return 2;
        }
    };
    let ziel_datei = argumente.get(2);

    let (gastgeber, port, pfad) = match url_zerlegen(url) {
        Ok(teile) => teile,
        Err(meldung) => {
            println!("{}", meldung);
            return 2;
        }
    };

    println!("netzhole: {} (Port {}), Pfad {}", gastgeber, port, pfad);

    // --- 1. Namensaufloesung ---
    // Eine reine IP darf man auch direkt schreiben; sonst fragen wir DNS.
    let ip = match ip_aus_text(gastgeber) {
        Some(ip) => ip,
        None => {
            println!("  DNS: loese '{}' auf ...", gastgeber);
            match libspeed::aufloesen(gastgeber) {
                Ok(ip) => ip,
                Err(fehler) => {
                    println!("  DNS fehlgeschlagen: {}", fehler.text());
                    return 1;
                }
            }
        }
    };
    println!(
        "  IP: {}.{}.{}.{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff
    );

    // --- 2. Verbinden ---
    let sock = match libspeed::socket(libspeed::TCP) {
        Ok(handle) => handle,
        Err(fehler) => {
            println!("  Socket anlegen fehlgeschlagen: {}", fehler.text());
            return 1;
        }
    };
    // Ab hier gibt es EINEN Rueckgabepfad, damit das Handle nie leckt.
    // (Selbst wenn wir es vergaessen: Die Handle-Tabelle im Prozess-
    // Kontrollblock schliesst beim Prozess-Ende alles. Aber sich darauf zu
    // verlassen waere schlampig.)
    let code = holen(sock, ip, port, gastgeber, pfad, ziel_datei);
    let _ = libspeed::schliesse(sock);
    code
}

fn holen(
    sock: u64,
    ip: u32,
    port: u16,
    gastgeber: &str,
    pfad: &str,
    ziel_datei: Option<&str>,
) -> i32 {
    let start = libspeed::zeit_jetzt();

    // `verbinde` BLOCKIERT (bis 8 s) — der Kernel treibt den Handshake
    // selbst voran. Genau diese Nachschaerfung war noetig, damit ein
    // Ring-3-Programm ueberhaupt eine TCP-Verbindung aufbauen kann: Es
    // koennte `netz::pumpen()` nicht aufrufen (docs/syscalls.md).
    println!("  TCP: verbinde ...");
    if let Err(fehler) = libspeed::verbinde(sock, ip, port) {
        println!("  Verbinden fehlgeschlagen: {}", fehler.text());
        return 1;
    }
    println!("  TCP: verbunden nach {} ms.", libspeed::zeit_jetzt() - start);

    // --- 3. Die Anfrage bauen und senden ---
    let puffer = antwort_puffer();
    let anfrage_bytes = anfrage_bauen(puffer, pfad, gastgeber);
    if let Err(fehler) = alles_senden(sock, &puffer[..anfrage_bytes]) {
        println!("  Senden fehlgeschlagen: {}", fehler.text());
        return 1;
    }

    // --- 4. Antwort einsammeln ---
    // `empfange` ist NICHT blockierend: 0 heisst „gerade nichts da", nicht
    // „Ende". Das Ende erkennt man am Socket-ZUSTAND — und auch dann erst,
    // wenn danach nichts mehr nachkommt (es kann noch Gepuffertes geben).
    let mut belegt = 0usize;
    let mut stueck = [0u8; STUECK];
    let mut leerlauf = 0u32;
    let frist = libspeed::zeit_jetzt() + FRIST_MS;

    loop {
        match libspeed::empfange(sock, &mut stueck) {
            Ok(0) => {
                let zustand = libspeed::socket_zustand(sock).unwrap_or(libspeed::Z_GESCHLOSSEN);
                let zu = zustand == libspeed::Z_GESCHLOSSEN
                    || zustand == libspeed::Z_PEER_HAT_GESCHLOSSEN;
                if zu {
                    // Dreimal nichts mehr, obwohl die Gegenseite zu ist:
                    // jetzt ist wirklich Schluss.
                    leerlauf += 1;
                    if leerlauf >= 3 {
                        break;
                    }
                }
                libspeed::schlafe(5);
            }
            Ok(anzahl) => {
                leerlauf = 0;
                let anzahl = anzahl as usize;
                let platz = puffer.len() - belegt;
                if platz == 0 {
                    println!("  (Antwort groesser als {} KiB — abgeschnitten)", ANTWORT_MAX / 1024);
                    break;
                }
                let nehmen = if anzahl < platz { anzahl } else { platz };
                puffer[belegt..belegt + nehmen].copy_from_slice(&stueck[..nehmen]);
                belegt += nehmen;
            }
            Err(fehler) => {
                println!("  Empfangen fehlgeschlagen: {}", fehler.text());
                return 1;
            }
        }
        if libspeed::zeit_jetzt() >= frist {
            println!("  Zeitueberschreitung nach {} ms.", FRIST_MS);
            return 1;
        }
    }

    if belegt == 0 {
        println!("  Keine Antwort erhalten.");
        return 1;
    }

    // --- 5. Kopf und Rumpf trennen ---
    let (kopf_ende, rumpf_start) = match kopf_ende_finden(&puffer[..belegt]) {
        Some(stellen) => stellen,
        None => {
            println!("  Antwort ohne vollstaendigen HTTP-Kopf ({} Byte).", belegt);
            return 1;
        }
    };
    let kopf = &puffer[..kopf_ende];
    let dauer = libspeed::zeit_jetzt() - start;

    println!();
    println!("--- Antwort ({} Byte in {} ms) ---", belegt, dauer);
    // Die Statuszeile ist die erste Zeile des Kopfes.
    if let Some(zeilen_ende) = kopf.iter().position(|b| *b == b'\r' || *b == b'\n') {
        let _ = libspeed::schreibe(libspeed::AUSGABE, &kopf[..zeilen_ende]);
        println!();
    }

    let rumpf = &puffer[rumpf_start..belegt];
    println!("--- Rumpf: {} Byte ---", rumpf.len());

    match ziel_datei {
        // Auf die Platte schreiben — dann laeuft in EINEM Programm die
        // Netz-Kette UND die Datei-Kette ueber Syscalls.
        Some(pfad) => match speichern(pfad, rumpf) {
            Ok(()) => {
                println!("{} Byte nach '{}' geschrieben.", rumpf.len(), pfad);
                0
            }
            Err(fehler) => {
                println!("Speichern nach '{}' fehlgeschlagen: {}", pfad, fehler.text());
                1
            }
        },
        None => {
            // Direkt ausgeben — als BYTES, nicht als Text: Eine Webseite ist
            // nicht zwingend gueltiges UTF-8, und `schreibe` ist ein
            // Byte-Strom.
            let _ = libspeed::schreibe(libspeed::AUSGABE, rumpf);
            println!();
            0
        }
    }
}

/// Sendet einen Puffer VOLLSTAENDIG (`sende` darf weniger uebernehmen).
fn alles_senden(sock: u64, daten: &[u8]) -> Result<(), Fehler> {
    let mut fertig = 0usize;
    let frist = libspeed::zeit_jetzt() + FRIST_MS;
    while fertig < daten.len() {
        match libspeed::sende(sock, &daten[fertig..]) {
            Ok(0) => {
                if libspeed::zeit_jetzt() >= frist {
                    return Err(Fehler::ZEITUEBERSCHREITUNG);
                }
                // Sendepuffer voll: kurz warten, statt zu drehen.
                libspeed::schlafe(5);
            }
            Ok(anzahl) => fertig += anzahl as usize,
            Err(fehler) => return Err(fehler),
        }
    }
    Ok(())
}

/// Schreibt den Rumpf in eine Datei (anlegen/abschneiden).
fn speichern(pfad: &str, daten: &[u8]) -> Result<(), Fehler> {
    let handle = libspeed::oeffne(
        pfad,
        libspeed::SCHREIBEN | libspeed::ANLEGEN | libspeed::ABSCHNEIDEN,
    )?;
    let mut geschrieben = 0usize;
    let ergebnis = loop {
        if geschrieben == daten.len() {
            break Ok(());
        }
        // In Stuecken, weil ein Syscall hoechstens MAX_PUFFER (64 KiB)
        // uebernimmt.
        let rest = daten.len() - geschrieben;
        let jetzt = if rest > STUECK { STUECK } else { rest };
        match libspeed::schreibe_at(
            handle,
            geschrieben as u64,
            &daten[geschrieben..geschrieben + jetzt],
        ) {
            Ok(anzahl) if anzahl > 0 => geschrieben += anzahl as usize,
            Ok(_) => break Err(Fehler::KEIN_PLATZ),
            Err(fehler) => break Err(fehler),
        }
    };
    let _ = libspeed::schliesse(handle);
    ergebnis
}

/// Baut die HTTP-Anfrage in den Puffer und liefert ihre Laenge.
///
/// `Connection: close` ist hier keine Bequemlichkeit, sondern die
/// Abbruch-Bedingung: Ohne sie liesse der Server die Verbindung offen, und
/// wir wuessten nie, wann die Antwort zu Ende ist (dafuer braeuchte es
/// Content-Length- und chunked-Auswertung — das kann der Kernel-Client
/// `hole`, dieses Minimal-Programm nicht).
fn anfrage_bauen(puffer: &mut [u8], pfad: &str, gastgeber: &str) -> usize {
    let mut stelle = 0usize;
    let mut anfuegen = |text: &str, stelle: &mut usize| {
        let bytes = text.as_bytes();
        let platz = puffer.len() - *stelle;
        let nehmen = if bytes.len() < platz { bytes.len() } else { platz };
        puffer[*stelle..*stelle + nehmen].copy_from_slice(&bytes[..nehmen]);
        *stelle += nehmen;
    };
    anfuegen("GET ", &mut stelle);
    anfuegen(pfad, &mut stelle);
    anfuegen(" HTTP/1.1\r\nHost: ", &mut stelle);
    anfuegen(gastgeber, &mut stelle);
    anfuegen(
        "\r\nUser-Agent: SpeedOS-netzhole/1.0\r\nConnection: close\r\nAccept: */*\r\n\r\n",
        &mut stelle,
    );
    stelle
}

/// Findet das Ende des HTTP-Kopfes. Liefert `(kopf_ende, rumpf_start)`.
/// Versteht sowohl \r\n\r\n (die Regel) als auch \n\n (die Wirklichkeit).
fn kopf_ende_finden(daten: &[u8]) -> Option<(usize, usize)> {
    for i in 0..daten.len() {
        if daten[i..].starts_with(b"\r\n\r\n") {
            return Some((i, i + 4));
        }
        if daten[i..].starts_with(b"\n\n") {
            return Some((i, i + 2));
        }
    }
    None
}

/// Zerlegt `http://gastgeber[:port]/pfad` in seine drei Teile.
fn url_zerlegen(url: &str) -> Result<(&str, u16, &str), &'static str> {
    if url.starts_with("https://") {
        return Err("https:// braucht TLS — das kann SpeedOS noch nicht. Nimm http://.");
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or("Die URL muss mit http:// beginnen.")?;
    if rest.is_empty() {
        return Err("Die URL enthaelt keinen Gastgeber.");
    }

    // Pfad abtrennen (alles ab dem ersten /).
    let (autoritaet, pfad) = match rest.find('/') {
        Some(stelle) => (&rest[..stelle], &rest[stelle..]),
        None => (rest, "/"),
    };
    if autoritaet.is_empty() {
        return Err("Die URL enthaelt keinen Gastgeber.");
    }

    // Port abtrennen.
    let (gastgeber, port) = match autoritaet.find(':') {
        Some(stelle) => {
            let text = &autoritaet[stelle + 1..];
            let mut wert: u32 = 0;
            if text.is_empty() {
                return Err("Port fehlt hinter dem Doppelpunkt.");
            }
            for ziffer in text.bytes() {
                if !ziffer.is_ascii_digit() {
                    return Err("Port ist keine Zahl.");
                }
                wert = wert * 10 + (ziffer - b'0') as u32;
                if wert > 65535 {
                    return Err("Port ist groesser als 65535.");
                }
            }
            (&autoritaet[..stelle], wert as u16)
        }
        None => (autoritaet, 80u16),
    };
    if gastgeber.is_empty() {
        return Err("Die URL enthaelt keinen Gastgeber.");
    }
    Ok((gastgeber, port, pfad))
}

/// Erkennt eine reine IPv4-Adresse ("10.0.2.2") und wandelt sie um.
/// `None` heisst „das ist ein Name, frag DNS".
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
