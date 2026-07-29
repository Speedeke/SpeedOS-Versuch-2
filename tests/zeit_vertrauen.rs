// tests/zeit_vertrauen.rs — Die zwei Voraussetzungen für TLS (Serie 7, Teil 2)
//
// TLS-Bibliotheken verlangen zwei Dinge von der Plattform, die nichts mit
// Kryptographie zu tun haben und trotzdem über Sicherheit entscheiden:
//
//   (1) EINE VERLÄSSLICHE WANDUHR — Gültigkeitszeiträume sind in UTC.
//       Eine falsche Uhr macht die Prüfung entweder unbenutzbar oder,
//       schlimmer, zu lax.
//   (2) EINEN VERTRAUENSANKER — sonst kann TLS zwar verschlüsseln, aber
//       nicht prüfen, mit wem es spricht.
//
// Geprüft wird hier:
//   * die UTC/Anzeige-Trennung (die eigentliche Korrektur an Serie 3),
//   * der Plausibilitäts-Check gegen das Bau-Datum,
//   * der PEM-Parser gegen KAPUTTE Eingaben — er darf nie panicken und
//     nie mehr Zertifikate melden, als wirklich lesbar sind.
//
// WAS HIER NICHT GEPRÜFT WERDEN KANN: dass die RTC auf ECHTER Hardware
// UTC liefert. Das ist eine Eigenschaft der Maschine, nicht des Codes —
// dafür gibt es den Live-Stick und docs/hardware-log.md.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::zeit::{self, DatumUhrzeit, ZeitFehler};
use speed_os::{allocator, einstellungen, fs, memory, programme, serial_println};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(512).expect("Heap-Erweiterung fehlgeschlagen");

    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();
    fs::init();
    fs::platte_automounten();
    programme::installieren();
    programme::ca_buendel_installieren();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ===========================================================================
// 1. DIE UHR: UTC ist die Wahrheit, die Zone ist Kosmetik
// ===========================================================================

/// DIE TRENNUNG, die diesen Teil überhaupt nötig gemacht hat.
///
/// Bis Serie 3 hiess `zeit::jetzt()` „das echte Datum" und lieferte, was
/// die RTC gerade sagte — in QEMU also Host-LOKALZEIT. Der Anzeige-Offset
/// kam OBENDRAUF. Ein Nutzer in UTC+2 mit Offset +120 bekam damit local+2,
/// also zwei Stunden zu viel. Für eine Taskleiste egal, für ein
/// Zertifikats-Ablaufdatum nicht.
///
/// Jetzt gilt: `zeit::jetzt()` ist UTC, `einstellungen::jetzt_lokal()` ist
/// UTC + Anzeige-Zone. Der Test hält beides gegeneinander fest.
#[test_case]
fn test_utc_und_anzeige_sind_getrennt() {
    // Ausgangszustand merken, damit der Test nichts hinterlässt.
    let offset_vorher = einstellungen::utc_offset_min();

    let utc = zeit::jetzt();
    let utc_s = zeit::sekunden_seit_2000(&utc);

    // --- Anzeige-Zone auf +2 Stunden: die ANZEIGE verschiebt sich ---
    einstellungen::setze_zahl(einstellungen::S_UTC_OFFSET, 120);
    let lokal = einstellungen::jetzt_lokal();
    let lokal_s = zeit::sekunden_seit_2000(&lokal);
    // Ein bisschen Toleranz: Zwischen den beiden Aufrufen vergeht Zeit.
    let differenz = lokal_s as i64 - utc_s as i64;
    assert!(
        (7150..=7250).contains(&differenz),
        "Anzeige-Zone +120 min muesste ~7200 s Unterschied machen, machte {} s",
        differenz
    );

    // --- UND DER KERN: die UTC-Zeit hat sich NICHT bewegt ---
    let utc_nachher = zeit::jetzt();
    let utc_nachher_s = zeit::sekunden_seit_2000(&utc_nachher);
    let drift = utc_nachher_s as i64 - utc_s as i64;
    assert!(
        (0..=5).contains(&drift),
        "die Anzeige-Zone hat die UTC-ZEIT verschoben ({} s) — genau das darf sie nie",
        drift
    );

    // --- Und zurück auf 0: Anzeige == UTC ---
    einstellungen::setze_zahl(einstellungen::S_UTC_OFFSET, 0);
    let gleich = einstellungen::jetzt_lokal();
    assert_eq!(
        zeit::sekunden_seit_2000(&gleich) as i64 - zeit::sekunden_seit_2000(&zeit::jetzt()) as i64,
        0,
        "bei Zone 0 muessen Anzeige und UTC identisch sein"
    );

    einstellungen::setze_zahl(einstellungen::S_UTC_OFFSET, offset_vorher);
    serial_println!(
        "  UTC {:02}:{:02}:{:02} am {:02}.{:02}.{} — Anzeige-Zone verschiebt NUR die Anzeige.",
        utc.stunde, utc.minute, utc.sekunde, utc.tag, utc.monat, utc.jahr
    );
}

/// DIE RTC-ZONE ist die dritte, eigene Ebene: Sie deutet den Rohwert der
/// Hardware-Uhr und hat mit der Anzeige NICHTS zu tun.
#[test_case]
fn test_rtc_zone_ist_eigene_ebene() {
    let zone_vorher = zeit::rtc_zone_min();
    let utc_vorher = zeit::sekunden_seit_2000(&zeit::jetzt());

    // Die Hardware-Uhr als „läuft 2 Stunden vor UTC" deuten: Die UTC-Zeit
    // muss dadurch um 2 Stunden ZURÜCKgehen (der Rohwert war ja Lokalzeit).
    zeit::rtc_zone_setzen(120);
    let utc_nach = zeit::sekunden_seit_2000(&zeit::jetzt());
    let differenz = utc_vorher as i64 - utc_nach as i64;
    assert!(
        (7150..=7250).contains(&differenz),
        "RTC-Zone +120 min muesste die UTC-Zeit um ~7200 s zurueckdrehen, tat {} s",
        differenz
    );

    // Zurück — und wir sind wieder da, wo wir waren.
    zeit::rtc_zone_setzen(zone_vorher);
    let utc_zurueck = zeit::sekunden_seit_2000(&zeit::jetzt());
    assert!(
        utc_zurueck.abs_diff(utc_vorher) <= 5,
        "das Zuruecksetzen der RTC-Zone hat die Uhr nicht wiederhergestellt"
    );
    serial_println!("  RTC-Zone wirkt auf UTC, Anzeige-Zone nur auf die Anzeige. Getrennt. OK");
}

/// DER PLAUSIBILITÄTS-CHECK — als reine Funktion, in allen Richtungen.
#[test_case]
fn test_zeit_plausibilitaet() {
    let bau = zeit::BAU_EPOCHE_S;
    assert!(bau > 0, "kein Bau-Datum eingebettet — build.rs hat nicht gegriffen");

    // Genau auf dem Bau-Datum: gerade noch gut.
    assert_eq!(zeit::zeit_pruefen(bau, bau), Ok(()));
    // Eine Sekunde davor: nachweislich falsch.
    assert_eq!(zeit::zeit_pruefen(bau - 1, bau), Err(ZeitFehler::VorBauDatum));

    // DER KLASSISCHE AUSFALL: leere Pufferbatterie -> Uhr auf 1.1.2000.
    assert_eq!(zeit::zeit_pruefen(1, bau), Err(ZeitFehler::VorBauDatum));
    // Gar keine Uhr.
    assert_eq!(zeit::zeit_pruefen(0, bau), Err(ZeitFehler::KeineUhr));

    // Weit in der Zukunft (ein Register voller 0xFF).
    let zu_weit = bau + (zeit::PLAUSIBEL_JAHRE + 1) * 365 * 24 * 60 * 60;
    assert_eq!(
        zeit::zeit_pruefen(zu_weit, bau),
        Err(ZeitFehler::ZuWeitInDerZukunft)
    );
    // Knapp innerhalb der Obergrenze: noch gut.
    let gerade_so = bau + (zeit::PLAUSIBEL_JAHRE - 1) * 365 * 24 * 60 * 60;
    assert_eq!(zeit::zeit_pruefen(gerade_so, bau), Ok(()));

    // OHNE Bau-Datum wird NICHT geprüft — lieber gar keine Grenze als eine
    // erfundene.
    assert_eq!(zeit::zeit_pruefen(1, 0), Ok(()));
    assert_eq!(zeit::zeit_pruefen(u64::MAX, 0), Ok(()));
    // Aber „keine Uhr" bleibt auch dann ein Fehler.
    assert_eq!(zeit::zeit_pruefen(0, 0), Err(ZeitFehler::KeineUhr));

    // Keine Panik bei Extremwerten.
    let _ = zeit::zeit_pruefen(u64::MAX, u64::MAX);
    let _ = zeit::zeit_pruefen(u64::MAX, 1);

    let datum = zeit::datum_von_sekunden_seit_2000(bau);
    serial_println!(
        "  Bau-Datum dieses Kernels: {:02}.{:02}.{} — Uhren davor sind nachweislich falsch.",
        datum.tag, datum.monat, datum.jahr
    );
}

/// DIE KONSEQUENZ: Eine unplausible Uhr liefert KEINE Zertifikatszeit.
///
/// Das ist der Punkt, an dem aus „wir wissen, dass die Uhr falsch ist" ein
/// VERHALTEN wird — ohne das wäre die Prüfung eine Logzeile ohne Wirkung.
#[test_case]
fn test_unplausible_uhr_verweigert_zertifikatszeit() {
    let zone_vorher = zeit::rtc_zone_min();

    // Im Normalfall gibt es eine Zeit ...
    assert!(zeit::plausibel(), "die Uhr ist im Testlauf schon unplausibel?");
    let unix = zeit::zertifikatszeit().expect("plausible Uhr liefert eine Zeit");
    // ... und die liegt in der UNIX-Epoche, nicht in unserer.
    assert!(
        unix > 1_700_000_000,
        "die Zertifikatszeit ({}) sieht nicht nach UNIX-Sekunden aus",
        unix
    );
    assert_eq!(
        unix,
        zeit::sekunden_seit_2000(&zeit::jetzt()) + zeit::SEKUNDEN_1970_BIS_2000,
        "die Epochen-Umrechnung stimmt nicht"
    );

    // JETZT DIE UHR KAPUTT MACHEN: auf ein Datum weit vor dem Bau stellen.
    zeit::zeit_setzen(&DatumUhrzeit {
        jahr: 2001,
        monat: 1,
        tag: 1,
        stunde: 0,
        minute: 0,
        sekunde: 0,
    });
    assert!(!zeit::plausibel(), "eine Uhr im Jahr 2001 gilt als plausibel?");
    assert_eq!(
        zeit::zertifikatszeit(),
        Err(ZeitFehler::VorBauDatum),
        "eine nachweislich falsche Uhr liefert trotzdem eine Zertifikatszeit"
    );

    // Und über die ABI kommt der klare Fehler heraus, nicht eine Zahl.
    use speed_os::syscall::{ausfuehren, Fehler, SYS_ZEIT_GEPRUEFT};
    assert_eq!(
        ausfuehren(SYS_ZEIT_GEPRUEFT, 0, 0, 0, 0),
        Err(Fehler::ZeitUnplausibel)
    );
    // `zeit_epoche` (6) bleibt dagegen unverändert nutzbar — eine Anzeige
    // darf falsch gehen, eine Prüfung nicht.
    use speed_os::syscall::SYS_ZEIT_EPOCHE;
    assert!(
        ausfuehren(SYS_ZEIT_EPOCHE, 0, 0, 0, 0).is_ok(),
        "zeit_epoche muss auch bei kaputter Uhr eine Zahl liefern (es ist eine Anzeige)"
    );

    // Aufräumen: wieder der RTC glauben.
    zeit::rtc_zone_setzen(zone_vorher);
    assert!(zeit::plausibel(), "nach dem Zuruecksetzen ist die Uhr wieder gut");
    serial_println!("  Unplausible Uhr -> ZeitUnplausibel (26), keine Zertifikatszeit. OK");
}

// ===========================================================================
// 2. DER PEM-PARSER — vor allem gegen KAPUTTE Eingaben
// ===========================================================================
//
// Der Parser selbst lebt im User-Space (userland/src/pem.rs, Regel:
// krypto-nahes gehört nach Ring 3). Damit die Logik hier trotzdem prüfbar
// ist, ohne einen Prozess zu starten, liegt eine ZWEITE, identische Fassung
// als Testhilfe bei — dieselbe Entscheidung wie bei den ABI-Konstanten:
// zwei Seiten schreiben es getrennt auf, der Test hält sie zusammen.
//
// Der ECHTE Beweis, dass der User-Space-Parser läuft, ist der
// `zertifikate`-Lauf am Ende dieser Datei.

// `dead_code` erlaubt: Der Kernel-Testlauf benutzt nicht jedes Feld des
// User-Space-Moduls (`nummer`, `PemFehler::meldung` gehören der Anzeige im
// `zertifikate`-Programm). Es ist DIESELBE Datei — sie hier zu beschneiden
// wäre die falsche Antwort auf eine Warnung.
#[path = "../userland/src/pem.rs"]
#[allow(dead_code)]
mod pem;

/// Ein winziges, gültiges DER-„Zertifikat" für die Parser-Tests.
///
/// Es ist KEIN echtes X.509 — es hat genau die Struktur, die `kurzinfo`
/// abläuft (SEQUENCE > SEQUENCE > Validity + Subject-Name mit CN), und
/// nicht mehr. Ein echtes Zertifikat zu erfinden wäre unehrlich; hier geht
/// es um den Läufer, nicht um Kryptographie.
fn test_zertifikat() -> alloc::vec::Vec<u8> {
    use alloc::vec::Vec;
    // Validity: SEQUENCE { UTCTime "230101000000Z", UTCTime "330101000000Z" }
    let mut validity = Vec::new();
    for text in [b"230101000000Z", b"330101000000Z"] {
        validity.push(0x17); // UTCTime
        validity.push(text.len() as u8);
        validity.extend_from_slice(text);
    }
    let validity = mit_kopf(0x30, &validity);

    // Subject: SEQUENCE { SET { SEQUENCE { OID 2.5.4.3, UTF8String "Test CA" } } }
    let mut attribut = Vec::new();
    attribut.extend_from_slice(&[0x06, 0x03, 0x55, 0x04, 0x03]); // OID commonName
    attribut.extend_from_slice(&[0x0c, 0x07]); // UTF8String, 7 Zeichen
    attribut.extend_from_slice(b"Test CA");
    let attribut = mit_kopf(0x30, &attribut);
    let rdn = mit_kopf(0x31, &attribut);
    let subject = mit_kopf(0x30, &rdn);

    // TBSCertificate: INTEGER serial, SEQUENCE issuer, Validity, Subject
    let mut tbs = Vec::new();
    tbs.extend_from_slice(&[0x02, 0x01, 0x2a]); // INTEGER 42
    tbs.extend_from_slice(&mit_kopf(0x30, &[])); // leerer Issuer
    tbs.extend_from_slice(&validity);
    tbs.extend_from_slice(&subject);
    let tbs = mit_kopf(0x30, &tbs);

    mit_kopf(0x30, &tbs)
}

fn mit_kopf(tag: u8, inhalt: &[u8]) -> alloc::vec::Vec<u8> {
    let mut aus = alloc::vec![tag];
    if inhalt.len() < 128 {
        aus.push(inhalt.len() as u8);
    } else {
        aus.push(0x82);
        aus.push((inhalt.len() >> 8) as u8);
        aus.push(inhalt.len() as u8);
    }
    aus.extend_from_slice(inhalt);
    aus
}

/// Base64-Kodierer für die Testdaten (der Parser hat nur den Dekodierer).
fn base64(daten: &[u8]) -> alloc::string::String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut aus = alloc::string::String::new();
    for brocken in daten.chunks(3) {
        let b = [
            brocken[0],
            *brocken.get(1).unwrap_or(&0),
            *brocken.get(2).unwrap_or(&0),
        ];
        let wert = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        aus.push(ALPHABET[(wert >> 18) as usize & 63] as char);
        aus.push(ALPHABET[(wert >> 12) as usize & 63] as char);
        aus.push(if brocken.len() > 1 {
            ALPHABET[(wert >> 6) as usize & 63] as char
        } else {
            '='
        });
        aus.push(if brocken.len() > 2 {
            ALPHABET[wert as usize & 63] as char
        } else {
            '='
        });
    }
    aus
}

fn pem_block(der: &[u8]) -> alloc::string::String {
    alloc::format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64(der)
    )
}

/// Der Grundfall: Ein gültiger Block wird gelesen, und die Kurzinfo stimmt.
#[test_case]
fn test_pem_grundfall() {
    let der = test_zertifikat();
    let text = pem_block(&der);
    let mut puffer = alloc::vec![0u8; pem::MAX_DER_BYTES];
    let mut gefunden = 0usize;
    let mut name_ok = false;
    let mut zeiten = (0u64, 0u64);

    let bestand = pem::bloecke_durchgehen(text.as_bytes(), &mut puffer, |block| {
        gefunden += 1;
        assert_eq!(block.der, &der[..], "der dekodierte DER stimmt nicht");
        let info = pem::kurzinfo(block.der);
        name_ok = info.name == b"Test CA";
        zeiten = (info.gueltig_ab, info.gueltig_bis);
    });

    assert_eq!(bestand.gelesen, 1);
    assert_eq!(bestand.kaputt, 0);
    assert_eq!(gefunden, 1);
    assert!(name_ok, "der Common Name wurde nicht gefunden");
    // 01.01.2023 und 01.01.2033 in UNIX-Sekunden.
    assert_eq!(zeiten.0, pem::unix_aus_datum(2023, 1, 1, 0, 0, 0));
    assert_eq!(zeiten.1, pem::unix_aus_datum(2033, 1, 1, 0, 0, 0));
    serial_println!("  PEM-Grundfall: Block gelesen, CN und Gueltigkeit korrekt.");
}

/// KAPUTTE EINGABEN — der eigentliche Zweck dieses Tests.
///
/// Nichts davon darf panicken, und nichts darf mehr Zertifikate melden, als
/// wirklich lesbar sind. Der Parser verarbeitet eine Datei, die von aussen
/// kommt — dieselbe Haltung wie beim ELF-Lader.
#[test_case]
fn test_pem_kaputte_eingaben() {
    let mut puffer = alloc::vec![0u8; pem::MAX_DER_BYTES];
    let der = test_zertifikat();
    let gut = pem_block(&der);

    // Jeder Eintrag: (Beschreibung, Eingabe, erwartete gelesene Bloecke)
    let faelle: alloc::vec::Vec<(&str, alloc::string::String, usize)> = alloc::vec![
        ("leere Datei", alloc::string::String::new(), 0),
        ("nur Text", "Hallo, ich bin kein Zertifikat.\n".into(), 0),
        (
            "BEGIN ohne END",
            "-----BEGIN CERTIFICATE-----\nQUJD\n".into(),
            0
        ),
        (
            "END ohne BEGIN",
            "-----END CERTIFICATE-----\n".into(),
            0
        ),
        (
            "leerer Block",
            "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n".into(),
            0
        ),
        (
            "ungueltiges Base64-Zeichen",
            "-----BEGIN CERTIFICATE-----\nQU!C\n-----END CERTIFICATE-----\n".into(),
            0
        ),
        (
            "Base64-Laenge geht nicht auf",
            "-----BEGIN CERTIFICATE-----\nQUJDQ\n-----END CERTIFICATE-----\n".into(),
            0
        ),
        (
            "anderer Block-Typ (wird uebersprungen, nicht abgelehnt)",
            "-----BEGIN PRIVATE KEY-----\nQUJD\n-----END PRIVATE KEY-----\n".into(),
            0
        ),
        // DIE WICHTIGSTE ZEILE: ein kaputter Block darf die GUTEN nicht
        // mitreissen.
        (
            "kaputt + gut",
            alloc::format!(
                "-----BEGIN CERTIFICATE-----\nQU!C\n-----END CERTIFICATE-----\n{}",
                gut
            ),
            1
        ),
        (
            "gut + kaputt",
            alloc::format!(
                "{}-----BEGIN CERTIFICATE-----\nQU!C\n-----END CERTIFICATE-----\n",
                gut
            ),
            1
        ),
        (
            "gut + Fremdtyp + gut",
            alloc::format!("{}-----BEGIN X509 CRL-----\nQUJD\n-----END X509 CRL-----\n{}", gut, gut),
            2
        ),
        ("zwei gute", alloc::format!("{}{}", gut, gut), 2),
        (
            "Kommentare zwischen den Bloecken",
            alloc::format!("# Eine CA\nIrgendein Name\n{}\n# noch eine\n{}", gut, gut),
            2
        ),
        (
            "Windows-Zeilenenden",
            gut.replace('\n', "\r\n"),
            1
        ),
        (
            "BEGIN-Marke abgeschnitten",
            "-----BEGIN CERT".into(),
            0
        ),
    ];

    for (was, eingabe, erwartet) in &faelle {
        let mut aufrufe = 0usize;
        let bestand = pem::bloecke_durchgehen(eingabe.as_bytes(), &mut puffer, |_| aufrufe += 1);
        assert_eq!(
            bestand.gelesen, *erwartet,
            "'{}': {} Bloecke gelesen, erwartet {}",
            was, bestand.gelesen, erwartet
        );
        assert_eq!(
            aufrufe, *erwartet,
            "'{}': der Rueckruf lief {}x, erwartet {}x",
            was, aufrufe, erwartet
        );
    }

    // ABGESCHNITTEN AN JEDER STELLE — dieselbe Methodik wie beim ELF-Test.
    // Kein Präfix darf panicken.
    let ganz = alloc::format!("{}{}", gut, gut);
    for laenge in 0..ganz.len() {
        let bestand =
            pem::bloecke_durchgehen(&ganz.as_bytes()[..laenge], &mut puffer, |_| {});
        assert!(
            bestand.gelesen <= 2,
            "abgeschnitten bei {}: {} Bloecke gemeldet",
            laenge,
            bestand.gelesen
        );
    }

    // JEDES EINZELNE BYTE VERBOGEN: auch das darf nie panicken.
    let roh = gut.as_bytes().to_vec();
    for i in 0..roh.len() {
        let mut kaputt = roh.clone();
        kaputt[i] = kaputt[i].wrapping_add(1);
        let _ = pem::bloecke_durchgehen(&kaputt, &mut puffer, |block| {
            // Und der DER-Läufer muss mit allem klarkommen, was dabei
            // herauskommt — auch mit Unsinn.
            let _ = pem::kurzinfo(block.der);
        });
    }

    serial_println!(
        "  PEM gegen {} kaputte Eingaben + {} Abschnitte + {} Bit-Dreher: keine Panik.",
        faelle.len(),
        ganz.len(),
        roh.len()
    );
}

/// Der DER-Läufer gegen Unsinn — er darf raten, aber nie panicken.
#[test_case]
fn test_der_laeufer_robust() {
    // Leere und viel zu kurze Eingaben.
    for eingabe in [&[][..], &[0x30][..], &[0x30, 0x82][..], &[0x30, 0xff][..]] {
        let info = pem::kurzinfo(eingabe);
        assert!(info.name.is_empty());
        assert_eq!(info.gueltig_bis, 0);
    }
    // Eine Länge, die weit über den Puffer hinauszeigt.
    let luegner = alloc::vec![0x30, 0x84, 0xff, 0xff, 0xff, 0xff, 0x00];
    let info = pem::kurzinfo(&luegner);
    assert!(info.name.is_empty(), "eine erlogene Laenge wurde geglaubt");
    // Unbestimmte Länge (BER) wird abgelehnt.
    let ber = alloc::vec![0x30, 0x80, 0x00, 0x00];
    assert!(pem::kurzinfo(&ber).name.is_empty());
    // Zufälliger Müll in verschiedenen Längen.
    for laenge in [1usize, 2, 7, 33, 129, 300] {
        let muell: alloc::vec::Vec<u8> = (0..laenge).map(|i| (i * 37 + 11) as u8).collect();
        let _ = pem::kurzinfo(&muell);
    }
    serial_println!("  DER-Laeufer gegen erlogene Laengen und Muell: keine Panik.");
}

/// Die UTCTime-Zweistelligkeit (RFC 5280: 50..99 = 19xx, 00..49 = 20xx) —
/// eine der wenigen Stellen, an denen X.509 wirklich überraschend ist.
#[test_case]
fn test_utctime_jahrhundert() {
    let bauen = |zeit: &[u8]| {
        let mut validity = alloc::vec::Vec::new();
        for _ in 0..2 {
            validity.push(0x17);
            validity.push(zeit.len() as u8);
            validity.extend_from_slice(zeit);
        }
        let validity = mit_kopf(0x30, &validity);
        let mut tbs = alloc::vec::Vec::new();
        tbs.extend_from_slice(&validity);
        tbs.extend_from_slice(&mit_kopf(0x30, &[])); // leeres Subject
        mit_kopf(0x30, &mit_kopf(0x30, &tbs))
    };

    // "49" -> 2049, "50" -> 1950.
    let zert_2049 = bauen(b"490101000000Z");
    assert_eq!(
        pem::kurzinfo(&zert_2049).gueltig_ab,
        pem::unix_aus_datum(2049, 1, 1, 0, 0, 0)
    );
    let zert_1950 = bauen(b"500101000000Z");
    assert_eq!(
        pem::kurzinfo(&zert_1950).gueltig_ab,
        pem::unix_aus_datum(1950, 1, 1, 0, 0, 0)
    );

    // GeneralizedTime ist vierstellig und damit eindeutig.
    let mut validity = alloc::vec::Vec::new();
    for _ in 0..2 {
        validity.push(0x18);
        validity.push(15);
        validity.extend_from_slice(b"20501231235959Z");
    }
    let validity = mit_kopf(0x30, &validity);
    let mut tbs = validity.clone();
    tbs.extend_from_slice(&mit_kopf(0x30, &[]));
    let zert = mit_kopf(0x30, &mit_kopf(0x30, &tbs));
    assert_eq!(
        pem::kurzinfo(&zert).gueltig_bis,
        pem::unix_aus_datum(2050, 12, 31, 23, 59, 59)
    );

    // Unsinnige Datumsangaben werden abgelehnt, nicht gerechnet.
    for kaputt in [
        &b"231301000000Z"[..], // Monat 13
        &b"230100000000Z"[..], // Tag 0
        &b"23AB01000000Z"[..], // Buchstaben statt Ziffern
    ] {
        let zert = bauen(kaputt);
        assert_eq!(
            pem::kurzinfo(&zert).gueltig_ab,
            0,
            "ein unsinniges Datum wurde gerechnet statt abgelehnt"
        );
    }
    serial_println!("  UTCTime-Jahrhundertregel und Datums-Pruefung: OK");
}

// ===========================================================================
// 3. DER VERTRAUENSANKER, wie er wirklich installiert ist
// ===========================================================================

/// Was liegt tatsächlich auf der Platte? Ein BERICHTS-Test: Er kann nicht
/// fehlschlagen, wenn kein Bündel geholt wurde (das ist ein gültiger
/// Zustand, siehe docs/tls-vertrauen.md §1) — aber er sagt deutlich, was
/// Sache ist.
#[test_case]
fn test_ca_buendel_bestand() {
    let pfad = programme::ca_buendel_pfad();
    serial_println!("  === VERTRAUENSANKER ===");
    serial_println!("    Eingebettet: {} Byte", programme::CA_BUENDEL.len());

    if programme::CA_BUENDEL.is_empty() {
        serial_println!(
            "    KEIN Buendel geholt — SpeedOS haette fuer TLS keinen \
             Vertrauensanker."
        );
        serial_println!("    Holen mit tools/ca_bundle_holen.ps1 (docs/tls-vertrauen.md §1).");
        // Das ist KEIN Testfehler: Ein Bündel wird bewusst von Hand geholt
        // und liegt nicht im Repository.
        return;
    }

    let inhalt = fs::mit_fs(|dateisystem| dateisystem.lesen(pfad))
        .expect("das eingebettete Buendel muesste auf der Platte liegen");
    assert_eq!(
        inhalt.len(),
        programme::CA_BUENDEL.len(),
        "die Datei auf der Platte weicht vom eingebetteten Buendel ab"
    );

    let mut puffer = alloc::vec![0u8; pem::MAX_DER_BYTES];
    let mut namen = 0usize;
    let bestand = pem::bloecke_durchgehen(&inhalt, &mut puffer, |block| {
        if !pem::kurzinfo(block.der).name.is_empty() {
            namen += 1;
        }
    });
    serial_println!(
        "    {} Wurzeln gelesen, {} mit Common Name, {} unlesbar",
        bestand.gelesen,
        namen,
        bestand.kaputt
    );
    assert!(bestand.gelesen > 0, "aus dem Buendel kam keine einzige Wurzel");
    assert_eq!(bestand.kaputt, 0, "das Buendel enthaelt unlesbare Bloecke");
}

/// Das Programm `zertifikate` läuft — echt, aus Ring 3, von der Platte.
#[test_case]
fn test_zertifikate_programm_laeuft() {
    use speed_os::prozess;
    if programme::PROGRAMME.iter().any(|p| p.elf.is_empty()) {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
        return;
    }
    speed_os::scheduler::init();
    let pfad = programme::pfad("zertifikate");
    let pid = prozess::prozess_starten(&pfad, &["zertifikate"]).expect("zertifikate starten");
    let ende = speed_os::scheduler::warten_auf(pid, 30_000);
    speed_os::scheduler::aufraeumen();
    serial_println!("  zertifikate beendet: {:?}", ende);
    assert!(ende.is_some(), "zertifikate ist nicht in 30 s fertig geworden");
}
