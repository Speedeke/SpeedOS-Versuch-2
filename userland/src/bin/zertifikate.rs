// zertifikate [datei] — zeigt den Vertrauensanker von SpeedOS
//
// Ohne Argument liest es /platte/system/ca-bundle.pem — die Datei, die der
// Kernel beim Boot aus dem eingebetteten Buendel schreibt
// (src/programme.rs, docs/tls-vertrauen.md).
//
// ==========================================================================
// WARUM ES DAS GIBT
//
// Ein Vertrauensanker, den man nicht ansehen kann, ist eine Behauptung. Wer
// gleich TLS benutzt, soll vorher wissen koennen: WIE VIELE Wurzeln liegen
// da, WEM gehoeren sie, und BIS WANN gelten sie. Und er soll sehen, wenn
// etwas fehlt — eine leere Datei, ein kaputter Block, ein Buendel, dessen
// juengstes Zertifikat vor Jahren abgelaufen ist.
//
// DIE ZEIT IST TEIL DER ANTWORT: Das Programm holt sie ueber
// `zeit_geprueft()` (Syscall 13). Geht die Uhr nachweislich falsch, sagt es
// das DEUTLICH und zeigt KEINE Gueltigkeits-Bewertung — eine „gueltig
// bis"-Aussage auf Basis einer kaputten Uhr waere schlimmer als gar keine.

#![no_std]
#![no_main]

use libspeed::pem;
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

/// Der Standard-Ort des Buendels.
const STANDARD_PFAD: &str = "/platte/system/ca-bundle.pem";
/// Wie viele Namen wir auflisten (ein volles Buendel hat ~150 — die alle
/// auszugeben waere eine Bildschirmwand ohne Erkenntnisgewinn).
const NAMEN_ZEIGEN: usize = 8;

/// Der Lesepuffer fuer die PEM-Datei. Liegt in `.bss`, NICHT auf dem Stack:
/// Der User-Stack ist 64 KiB (`prozess::ELF_STACK_SEITEN`) — ein
/// 512-KiB-Feld darauf waere ein sofortiger Guard-Page-Treffer.
static mut DATEI: [u8; 512 * 1024] = [0; 512 * 1024];
/// Arbeitspuffer fuer EIN dekodiertes Zertifikat.
static mut DER: [u8; pem::MAX_DER_BYTES] = [0; pem::MAX_DER_BYTES];

/// # Safety
/// Einzelner Thread, einzelner Prozess — es gibt keinen zweiten Benutzer
/// dieser Puffer und keine Nebenlaeufigkeit.
fn datei_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DATEI) }
}
fn der_puffer() -> &'static mut [u8] {
    unsafe { &mut *core::ptr::addr_of_mut!(DER) }
}

fn haupt(argumente: &Argumente) -> i32 {
    let pfad = argumente.get(1).unwrap_or(STANDARD_PFAD);

    // --- 1. Die Datei lesen ---
    let puffer = datei_puffer();
    let laenge = match datei_lesen(pfad, puffer) {
        Ok(0) => {
            println!("Der Vertrauensanker {} ist LEER.", pfad);
            println!();
            hinweis_kein_buendel();
            return 1;
        }
        Ok(laenge) => laenge,
        Err(fehler) => {
            println!("{} laesst sich nicht lesen: {}", pfad, fehler.text());
            println!();
            hinweis_kein_buendel();
            return 1;
        }
    };
    println!("Vertrauensanker: {}", pfad);
    println!("  {} Byte gelesen", laenge);

    // --- 2. Die Zeit holen — GEPRUEFT ---
    let (jetzt, zeit_ok) = match libspeed::zeit_geprueft() {
        Ok(sekunden) => (sekunden, true),
        Err(fehler) => {
            println!();
            println!("  !!! UHR UNPLAUSIBEL: {} !!!", fehler.text());
            println!("  Ablaufdaten werden deshalb NUR ANGEZEIGT, nicht bewertet.");
            println!("  (Eine 'gueltig bis'-Aussage auf Basis einer kaputten Uhr");
            println!("   waere schlimmer als gar keine. Einstellungen -> Zeit.)");
            (0, false)
        }
    };

    // --- 3. Durchgehen ---
    let mut anzahl = 0usize;
    let mut abgelaufen = 0usize;
    let mut noch_nicht = 0usize;
    let mut ohne_datum = 0usize;
    // Die Spanne der Ablaufdaten: das fruehste und das spaeteste.
    let mut fruehestes_ende = u64::MAX;
    let mut spaetestes_ende = 0u64;

    let der = der_puffer();
    let bestand = pem::bloecke_durchgehen(&puffer[..laenge], der, |block| {
        let info = pem::kurzinfo(block.der);
        anzahl += 1;

        if info.gueltig_bis == 0 {
            ohne_datum += 1;
        } else {
            if info.gueltig_bis < fruehestes_ende {
                fruehestes_ende = info.gueltig_bis;
            }
            if info.gueltig_bis > spaetestes_ende {
                spaetestes_ende = info.gueltig_bis;
            }
            if zeit_ok {
                if info.gueltig_bis < jetzt {
                    abgelaufen += 1;
                } else if info.gueltig_ab > jetzt {
                    noch_nicht += 1;
                }
            }
        }

        // Die ersten paar Namen zeigen — als Stichprobe, nicht als Liste.
        if block.nummer < NAMEN_ZEIGEN {
            print_zertifikat(&info);
        }
    });

    // --- 4. Das Ergebnis ---
    println!();
    println!("Geladene Wurzeln: {}", bestand.gelesen);
    if bestand.kaputt > 0 {
        // WICHTIG, dass das auffaellt: Ein kaputter Block ist kein
        // Weltuntergang (der Rest gilt weiter), aber er gehoert gemeldet.
        println!(
            "  {} Block/Bloecke UNLESBAR uebersprungen{}",
            bestand.kaputt,
            match bestand.erster_fehler {
                Some(fehler) => {
                    print_fehlergrund(fehler);
                    ""
                }
                None => "",
            }
        );
    }
    if bestand.uebrig > 0 {
        println!(
            "  {} weitere nicht angesehen (Grenze: {})",
            bestand.uebrig,
            pem::MAX_ZERTIFIKATE
        );
    }
    if ohne_datum > 0 {
        println!("  {} ohne lesbares Ablaufdatum", ohne_datum);
    }

    if anzahl > 0 && spaetestes_ende > 0 {
        println!();
        println!("Ablaufdaten-Spanne:");
        print_datum("  frueheste  ", fruehestes_ende);
        print_datum("  spaeteste  ", spaetestes_ende);
        if zeit_ok {
            print_datum("  jetzt (UTC)", jetzt);
            println!();
            if abgelaufen > 0 {
                println!("  {} Wurzel(n) sind ABGELAUFEN.", abgelaufen);
            }
            if noch_nicht > 0 {
                println!("  {} Wurzel(n) sind noch nicht gueltig.", noch_nicht);
            }
            if abgelaufen == 0 && noch_nicht == 0 {
                println!("  Alle Wurzeln sind zum jetzigen Zeitpunkt gueltig.");
            }
        }
    }

    println!();
    println!("Bekannte Einschraenkung: SpeedOS prueft KEINE Sperrlisten");
    println!("(weder OCSP noch CRL) — siehe docs/tls-vertrauen.md, Abschnitt 3a.");

    if bestand.gelesen == 0 {
        return 1;
    }
    0
}

/// Liest eine Datei vollstaendig in `ziel`.
fn datei_lesen(pfad: &str, ziel: &mut [u8]) -> Result<usize, libspeed::Fehler> {
    let handle = libspeed::oeffne(pfad, libspeed::LESEN)?;
    let mut gelesen = 0usize;
    loop {
        // In Haeppchen, weil ein Syscall hoechstens MAX_PUFFER (64 KiB)
        // uebertraegt.
        let rest = &mut ziel[gelesen..];
        if rest.is_empty() {
            break; // Puffer voll — der Rest der Datei wird ignoriert
        }
        let stueck = rest.len().min(32 * 1024);
        match libspeed::lese_at(handle, gelesen as u64, &mut rest[..stueck]) {
            Ok(0) => break, // Dateiende
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

/// Eine Zeile je Zertifikat: Name und Gueltigkeit.
fn print_zertifikat(info: &pem::Kurzinfo) {
    // Der Name kommt aus einer fremden Datei — er wird BYTEWEISE gefiltert
    // ausgegeben, nicht als &str gedeutet. Ein Zertifikat mit
    // Steuerzeichen im Namen soll das Terminal nicht durcheinanderbringen.
    libspeed::print!("  ");
    if info.name.is_empty() {
        libspeed::print!("(kein Common Name)");
    } else {
        for &byte in info.name.iter().take(48) {
            if byte.is_ascii_graphic() || byte == b' ' {
                libspeed::print!("{}", byte as char);
            } else {
                libspeed::print!(".");
            }
        }
    }
    if info.gueltig_bis > 0 {
        libspeed::print!("  (bis ");
        print_datum_roh(info.gueltig_bis);
        libspeed::print!(")");
    }
    println!();
}

fn print_fehlergrund(fehler: pem::PemFehler) {
    println!(" — erster Grund: {}", fehler.meldung());
}

fn print_datum(beschriftung: &str, unix: u64) {
    libspeed::print!("{}: ", beschriftung);
    print_datum_roh(unix);
    println!();
}

/// UNIX-Sekunden als `TT.MM.JJJJ` (die Uhrzeit interessiert bei
/// Zertifikats-Laufzeiten von Jahren niemanden).
fn print_datum_roh(unix: u64) {
    let (jahr, monat, tag) = datum_aus_unix(unix);
    libspeed::print!("{:02}.{:02}.{}", tag, monat, jahr);
}

/// UNIX-Sekunden -> (Jahr, Monat, Tag). Die Umkehrung von
/// `pem::unix_aus_datum`.
fn datum_aus_unix(unix: u64) -> (u64, u64, u64) {
    let schaltjahr =
        |j: u64| j.is_multiple_of(4) && (!j.is_multiple_of(100) || j.is_multiple_of(400));
    let mut tage = unix / 86_400;
    let mut jahr = 1970u64;
    loop {
        let im_jahr = if schaltjahr(jahr) { 366 } else { 365 };
        if tage < im_jahr {
            break;
        }
        tage -= im_jahr;
        jahr += 1;
        // Sicherheitsnetz gegen absurde Eingaben: Ein Zertifikat mit einem
        // Ablaufdatum im Jahr 12000 soll keine Endlosschleife ergeben.
        if jahr > 9999 {
            return (9999, 12, 31);
        }
    }
    const MONATSTAGE: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut monat = 1u64;
    for (i, &laenge) in MONATSTAGE.iter().enumerate() {
        let laenge = laenge + if i == 1 && schaltjahr(jahr) { 1 } else { 0 };
        if tage < laenge {
            break;
        }
        tage -= laenge;
        monat += 1;
    }
    (jahr, monat, tage + 1)
}

fn hinweis_kein_buendel() {
    println!("SpeedOS hat damit KEINEN Vertrauensanker — TLS koennte zwar");
    println!("verschluesseln, aber nicht pruefen, mit wem es spricht. Das ist");
    println!("die Haelfte, auf die es ankommt (docs/tls-vertrauen.md).");
    println!();
    println!("Holen (einmalig, auf dem Host):");
    println!("  tools/ca_bundle_holen.ps1");
    println!("Danach cargo build — das Buendel wird eingebettet und beim");
    println!("naechsten Boot nach {} geschrieben.", STANDARD_PFAD);
}
