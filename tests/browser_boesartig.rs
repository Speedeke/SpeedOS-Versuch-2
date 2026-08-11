// tests/browser_boesartig.rs — DIE AUSZAHLUNG VON SERIE 6
//
// ===========================================================================
// WORUM ES HIER GEHT
//
// Ein Browser ist das Programm, das absichtlich FREMDE, FEINDLICHE DATEN
// verarbeitet. Jede Seite im Netz ist Eingabe von jemandem, den wir nicht
// kennen — und die interessanten Eingaben sind die, die nicht gemeint
// sind, sondern gebaut.
//
// Bis Serie 5 waere das eine ernste Sorge gewesen: Der Parser lief IM
// KERNEL, und eine Endlosschleife in ihm haette das ganze System
// angehalten. Seit Serie 6 laeuft er in Ring 3, mit eigenem Adressraum,
// eigenem Heap und einem Scheduler, der ihn verdraengt.
//
// **DIESER TEST IST DIE PRUEFUNG DIESER ZUSAGE.** Die Erwartung ist
// bewusst SCHWACH fuer den Browser und HART fuer alles andere:
//
//   Der Browser-Prozess darf langsam werden, abschneiden, aufgeben oder
//   sterben — was er tut, ist seine Sache.
//   KERNEL UND DESKTOP MUESSEN UNBEEINDRUCKT BLEIBEN.
//
// Nach JEDEM Angriff wird deshalb geprueft, dass ein gewoehnlicher
// Prozess noch startet und sauber durchlaeuft. Das ist die eigentliche
// Zusage — nicht, dass der Browser die Seite schoen anzeigt.
//
// ===========================================================================
// DIE SEITEN ENTSTEHEN HIER, NICHT AUF DER PLATTE
//
// Sie werden im Test erzeugt und ins VFS geschrieben. Zwei Gruende: Ein
// 2-MiB-CSS-Monster gehoert nicht ins Repository, und wer eine
// Dekompressionsbombe baut, soll eine LESBARE LISTE VON ANGRIFFEN
// hinterlassen statt einer Binaerdatei (dieselbe Entscheidung wie bei
// tools/testbilder_erzeugen.py in Serie 8, Teil 3).

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::prozess::{self, ProzessEnde};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fenster, fs, memory, pipe, programme, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    // Die boesartigen Seiten entstehen IM KERNEL-HEAP, bevor sie
    // geschrieben werden — 8192 Seiten = 32 MiB lassen dafuer Luft.
    allocator::heap_erweitern(8192).expect("Heap-Erweiterung fehlgeschlagen");

    fs::init();
    programme::installieren();
    programme::testseite_installieren();
    scheduler::init();

    if let Some(fb) = framebuffer {
        speed_os::framebuffer::init(fb);
    }
    assert!(
        fenster::manager_fuer_test_starten(),
        "der Browser braucht einen Fenster-Manager"
    );

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Frist je Angriff. Grosszuegig — ein pathologisches Dokument DARF
/// langsam sein; es darf nur nicht das System mitnehmen.
const FRIST_MS: u64 = 90_000;

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Ein Programm starten, Ausgabe einsammeln, Ende abfragen.
fn laufen_lassen(name: &str, argumente: &[&str]) -> (Option<ProzessEnde>, String, u64) {
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad(name);
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let start = zeit::ms_seit_boot();
    let pid = prozess::prozess_starten_mit(
        &pfad,
        argumente,
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .unwrap_or_else(|fehler| panic!("'{}' starten: {}", name, fehler.meldung()));
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let mut gesammelt = Vec::new();
    let mut puffer = alloc::vec![0u8; 4096];
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    serial_println!("  (Frist abgelaufen — Prozess wird gestoppt)");
                    scheduler::beenden(pid);
                    break;
                }
                if ende.is_none() {
                    ende = scheduler::ende_abfragen(pid);
                }
                scheduler::aufraeumen();
                zeit::warte_auf_interrupt();
            }
            _ => break,
        }
    }
    if ende.is_none() {
        ende = scheduler::ende_abfragen(pid).or_else(|| scheduler::warten_auf(pid, 5_000));
    }
    pipe::ende_schliessen(leitung, pipe::Ende::Lesen);
    scheduler::aufraeumen();
    let dauer = zeit::ms_seit_boot().saturating_sub(start);
    (ende, String::from_utf8_lossy(&gesammelt).into_owned(), dauer)
}

/// **DIE EIGENTLICHE ZUSAGE.** Nach jedem Angriff: Laeuft das System noch?
///
/// Geprueft wird mit dem kleinsten gewoehnlichen Programm, das es gibt.
/// Kommt es sauber durch, sind Scheduler, Adressraeume, Handle-Tabelle,
/// Dateisystem und Pipes in Ordnung — und der Angriff hat nichts
/// hinterlassen.
fn system_lebt_noch(nach: &str) {
    let (ende, ausgabe, _) = laufen_lassen("hallo", &["hallo"]);
    assert_eq!(
        ende,
        Some(ProzessEnde::Beendet(0)),
        "nach '{}' laeuft kein gewoehnlicher Prozess mehr (Ende: {:?})",
        nach,
        ende
    );
    assert!(
        ausgabe.contains("Hallo"),
        "nach '{}' kommt keine Ausgabe mehr durch",
        nach
    );
    serial_println!("  [ok] System lebt nach '{}'", nach);
}

/// Eine Seite ins VFS schreiben und ihren Pfad liefern.
fn seite_schreiben(name: &str, inhalt: &str) -> String {
    let ordner = String::from(programme::seiten_verzeichnis());
    let pfad = fs::pfad_anhaengen(&ordner, name);
    fs::mit_fs(|dateisystem| dateisystem.schreiben(&pfad, inhalt.as_bytes()))
        .unwrap_or_else(|fehler| panic!("'{}' schreiben: {:?}", pfad, fehler));
    serial_println!("  Seite '{}' geschrieben ({} Byte).", pfad, inhalt.len());
    pfad
}

/// Den Browser auf eine Seite loslassen und berichten, was er tat.
fn browser_auf(pfad: &str, was: &str) -> Option<ProzessEnde> {
    let (ende, ausgabe, dauer) = laufen_lassen(
        "browser",
        &["browser", pfad, "--pruefen", "--fenster=800x600"],
    );
    serial_println!(
        "  [{}] Ende {:?} nach {} ms, {} Byte Ausgabe",
        was,
        ende,
        dauer,
        ausgabe.len()
    );
    // Was der Browser ausgegeben hat, interessiert hier nur als Beleg,
    // dass er ueberhaupt zum Zug kam.
    for zeile in ausgabe.lines().take(6) {
        serial_println!("    | {}", zeile);
    }
    ende
}

/// Die Bewertung eines Angriffs.
///
/// **ERLAUBT IST ALLES AUSSER EINEM HAENGER.** Der Browser darf sauber
/// durchlaufen (er hat abgeschnitten), mit einem Fehlercode aufgeben,
/// oder vom Kernel beendet werden (Speicher alle, Absturz). Nicht
/// erlaubt: gar kein Ende — dann haette er das System festgehalten.
fn bewerten(ende: Option<ProzessEnde>, was: &str) {
    match ende {
        Some(ProzessEnde::Beendet(code)) => {
            serial_println!("  [{}] Browser beendet mit Code {} — in Ordnung.", was, code)
        }
        Some(ProzessEnde::Abgestuerzt) => serial_println!(
            "  [{}] Browser ABGESTUERZT — in Ordnung, solange der Kernel lebt.",
            was
        ),
        Some(ProzessEnde::Gestoppt) => serial_println!(
            "  [{}] Browser wurde GESTOPPT (Frist) — in Ordnung, aber langsam.",
            was
        ),
        None => panic!(
            "[{}] der Browser hat sich weder beendet noch stoppen lassen — \
             DAS waere ein Haenger und damit ein Systemproblem",
            was
        ),
    }
}

// ===========================================================================
// (1) HTML: 10 000-FACH VERSCHACHTELT
// ===========================================================================

/// Der klassische Parser-Killer: Verschachtelung, die jeden REKURSIVEN
/// Durchlauf sprengt.
///
/// `speedhtml` deckelt die Tiefe bei 100 und schneidet ab
/// (`Befund::abgeschnitten`) — und der Grund dafuer steht in Teil 4:
/// **Die Tiefengrenze schuetzt nicht den Parser, sondern alles, was den
/// Baum danach REKURSIV durchlaeuft.** Das Layout ist rekursiv (Grenze
/// 64), und der User-Stack ist 64 KiB. Ohne beide Grenzen waere das hier
/// ein Stack-Ueberlauf.
#[test_case]
fn test_tief_verschachteltes_html() {
    if !programme_vorhanden() {
        return;
    }
    let mut html = String::with_capacity(200_000);
    html.push_str("<html><body>");
    for _ in 0..10_000 {
        html.push_str("<div>");
    }
    html.push_str("tief unten");
    for _ in 0..10_000 {
        html.push_str("</div>");
    }
    html.push_str("</body></html>");

    let pfad = seite_schreiben("boese-tief.html", &html);

    // EINGRENZEN STATT VERMUTEN: Dieselbe Seite durch die beiden
    // Debug-Werkzeuge, die je eine Stufe weniger machen. Wer stirbt,
    // sagt, WO die Rekursion sitzt — `htmldump` parst nur, `cssdump
    // --layout` parst, kaskadiert und setzt, der Browser malt zusaetzlich.
    let (h_ende, _, _) = laufen_lassen("htmldump", &["htmldump", &pfad, "--befund"]);
    let (c_ende, _, _) = laufen_lassen("cssdump", &["cssdump", &pfad, "--layout"]);
    serial_println!(
        "  [eingrenzung] htmldump {:?} | cssdump --layout {:?}",
        h_ende,
        c_ende
    );

    let ende = browser_auf(&pfad, "10000-fach verschachtelt");
    bewerten(ende, "10000-fach verschachtelt");
    system_lebt_noch("10000-fach verschachtelt");
}

// ===========================================================================
// (2) BILDER: MILLIARDEN-PIXEL-ANGABE
// ===========================================================================

/// Ein `<img>`, das eine absurde Groesse BEHAUPTET.
///
/// Zwei Angriffe in einem:
///   * `width="99999" height="99999"` — 10 Milliarden Pixel im LAYOUT.
///     Der Kasten wird so gross; ein Renderer, der das Rechteck einfach
///     fuellt, malt bis zum Sankt-Nimmerleins-Tag.
///   * dazu die echte Dekompressionsbombe aus Serie 8, Teil 3
///     (`bombe.png`: 48 KiB Datei, 50 MiB dekodiert, formal einwandfrei).
///
/// Erwartung: Der Maler CLIPPT auf das Fenster (er malt nie mehr als die
/// Flaeche), und der Bilddekoder lehnt die Bombe an seiner Pixelgrenze
/// ab. Beides steht schon; hier wird es GEMEINSAM losgelassen.
#[test_case]
fn test_milliarden_pixel_bild() {
    if !programme_vorhanden() {
        return;
    }
    let mut html = String::from("<html><body><h1>Bilder</h1>");
    html.push_str("<img src='bombe.png' width='99999' height='99999'>");
    html.push_str("<img src='bombe.png'>");
    html.push_str("<img src='absurde_masse.png' width='65535' height='65535'>");
    // Und viele davon, damit auch die Wiederholung zaehlt.
    for _ in 0..200 {
        html.push_str("<img src='bombe.png' width='9999' height='9999'>");
    }
    html.push_str("</body></html>");

    let pfad = seite_schreiben("boese-bilder.html", &html);
    let ende = browser_auf(&pfad, "Milliarden-Pixel-Bilder");
    bewerten(ende, "Milliarden-Pixel-Bilder");
    system_lebt_noch("Milliarden-Pixel-Bilder");
}

// ===========================================================================
// (3) CSS: SEHR VIELE REGELN
// ===========================================================================

/// Ein Stylesheet, das die Regel-Grenze von `speedcss` (100 000)
/// ueberschreitet.
///
/// Der interessante Teil ist nicht die Zahl, sondern die KASKADE: Sie
/// vergleicht jede Regel mit jedem Knoten. Bei 100 000 Regeln und 1 000
/// Knoten waeren das 100 Millionen Vergleiche — und genau deshalb gibt
/// es die Grenze.
#[test_case]
fn test_css_mit_sehr_vielen_regeln() {
    if !programme_vorhanden() {
        return;
    }
    let mut html = String::with_capacity(3_000_000);
    html.push_str("<html><head><style>");
    for i in 0..120_000 {
        html.push_str(".k");
        zahl_anhaengen(&mut html, i);
        html.push_str("{color:red}");
    }
    html.push_str("</style></head><body><p class='k1'>Text</p></body></html>");

    let pfad = seite_schreiben("boese-css.html", &html);
    let ende = browser_auf(&pfad, "120000 CSS-Regeln");
    bewerten(ende, "120000 CSS-Regeln");
    system_lebt_noch("120000 CSS-Regeln");
}

// ===========================================================================
// (4) EINE SEITE, DIE NICHT AUFHOERT
// ===========================================================================

/// Ein Dokument, das den Prozess-Heap fuellen will.
///
/// Ueber das Netz gibt es dafuer `max_bytes` (der Klient bricht WAEHREND
/// des Lesens ab, Serie 7, Teil 5). Von der PLATTE gibt es diese Grenze
/// nicht — die Datei wird ganz gelesen. Der Schutz ist dann der
/// User-Heap: 64 MiB, danach `KeinPlatz`.
///
/// Erwartung: Der Browser laeuft durch oder stirbt an seinem eigenen
/// Heap. Der Kernel merkt nichts davon — der Heap des Prozesses ist
/// SEIN Adressraum, und der faellt beim Ende als Ganzes.
#[test_case]
fn test_sehr_grosses_dokument() {
    if !programme_vorhanden() {
        return;
    }
    // 3 MiB Text in vielen Absaetzen — genug, um im gesetzten Zustand
    // mehrfach im Speicher zu liegen.
    let mut html = String::with_capacity(3_500_000);
    html.push_str("<html><body>");
    for i in 0..30_000 {
        html.push_str("<p>Absatz ");
        zahl_anhaengen(&mut html, i);
        html.push_str(" mit reichlich Text, damit das Dokument wirklich gross wird \
                       und der Umbruch etwas zu tun bekommt.</p>");
    }
    html.push_str("</body></html>");

    let pfad = seite_schreiben("boese-gross.html", &html);
    let ende = browser_auf(&pfad, "3-MiB-Dokument");
    bewerten(ende, "3-MiB-Dokument");
    system_lebt_noch("3-MiB-Dokument");
}

// ===========================================================================
// (5) MUELL, DER KEIN HTML IST
// ===========================================================================

/// Bytes, die nie ein Dokument waren.
///
/// `speedhtml` hat KEIN `Result` — jede Bytefolge ergibt einen Baum. Das
/// ist die Zusage aus Teil 4, und hier wird sie mit dem Browser
/// dahinter geprueft: kaputte Tags, unabgeschlossene Zeichenreferenzen,
/// ein `<` mitten im Tag (der Fall, der in Teil 4 eine Endlosschleife
/// ausloeste), Nullbytes und ungueltiges UTF-8.
#[test_case]
fn test_muell_statt_html() {
    if !programme_vorhanden() {
        return;
    }
    let mut muell = String::with_capacity(400_000);
    for i in 0..20_000 {
        muell.push_str("<p>a<b</p><i");
        muell.push_str("&#");
        zahl_anhaengen(&mut muell, i);
        muell.push_str("&nichtsdergleichen;<<<>>>");
        muell.push('\u{0}');
        muell.push_str("</");
    }
    let pfad = seite_schreiben("boese-muell.html", &muell);
    let ende = browser_auf(&pfad, "Muell statt HTML");
    bewerten(ende, "Muell statt HTML");
    system_lebt_noch("Muell statt HTML");
}

// ===========================================================================
// (6) VERWEISE, DIE IM KREIS ZEIGEN
// ===========================================================================

/// Eine Seite, die auf sich selbst verweist — und eine Kette, die im
/// Kreis laeuft.
///
/// Auf DIESER Ebene ist das harmlos (ein Verweis wird erst beim Klick
/// verfolgt); der Schleifenschutz sitzt eine Schicht tiefer, bei den
/// WEITERLEITUNGEN (`AbrufFehler::Schleife`, Serie 7, Teil 5). Geprueft
/// wird hier, dass die AUFLOESUNG selbst nicht in eine Schleife geraet —
/// mit Verweisen, die absichtlich pathologisch sind.
#[test_case]
fn test_pathologische_verweise() {
    if !programme_vorhanden() {
        return;
    }
    let mut html = String::from("<html><body>");
    for tiefe in 0..500 {
        html.push_str("<a href='");
        for _ in 0..tiefe {
            html.push_str("../");
        }
        html.push_str("boese-kreis.html#");
        zahl_anhaengen(&mut html, tiefe as u32);
        html.push_str("'>Link</a>");
    }
    // Verweise, die ins Nichts zeigen oder gar keine URL sind.
    html.push_str("<a href='javascript:while(1){}'>js</a>");
    html.push_str("<a href='mailto:x@y'>mail</a>");
    html.push_str("<a href=':'>doppelpunkt</a>");
    html.push_str("<a href='//'>nur schraegstriche</a>");
    html.push_str("<a href='http://'>leerer host</a>");
    html.push_str("</body></html>");

    let pfad = seite_schreiben("boese-kreis.html", &html);
    let ende = browser_auf(&pfad, "pathologische Verweise");
    bewerten(ende, "pathologische Verweise");
    system_lebt_noch("pathologische Verweise");
}

// ===========================================================================
// (7) DIE BILANZ
// ===========================================================================

/// Nach ALLEN Angriffen: Sind Frames und Fenster wieder da?
///
/// Der eigentliche Beweis, dass ein sterbender Prozess nichts
/// hinterlaesst — auch dann nicht, wenn er mitten im Zeichnen stirbt und
/// ein Fenster offen hat.
#[test_case]
fn test_nach_den_angriffen_ist_alles_zurueck() {
    if !programme_vorhanden() {
        return;
    }
    // Ein Vorlauf, damit einmalige Allokationen nicht als Leck erscheinen.
    let pfad = programme::testseite_pfad();
    let _ = browser_auf(&pfad, "Vorlauf");
    scheduler::aufraeumen();

    let (frei_vorher, gesamt) = memory::frame_statistik();
    let fenster_vorher = fenster::prozess_fenster_anzahl();

    // Die drei billigsten Angriffe noch einmal, hintereinander.
    for name in ["boese-tief.html", "boese-muell.html", "boese-kreis.html"] {
        let ordner = String::from(programme::seiten_verzeichnis());
        let pfad = fs::pfad_anhaengen(&ordner, name);
        let _ = browser_auf(&pfad, name);
        scheduler::aufraeumen();
    }

    let (frei_nachher, _) = memory::frame_statistik();
    let fenster_nachher = fenster::prozess_fenster_anzahl();
    let vorher = (gesamt - frei_vorher) as u64;
    let nachher = (gesamt - frei_nachher) as u64;
    // Der Kernel-Log-Puffer waechst mit jeder Ausgabe — herausgerechnet,
    // nicht ignoriert (die Messfalle aus dem Serie-6-Abschluss).
    let log_frames = (speed_os::protokoll::puffer_bytes() / 4096) as u64 + 4;

    serial_println!(
        "[BOESARTIG] Frames {} -> {} (Schranke +{}), Fenster {} -> {}",
        vorher,
        nachher,
        log_frames,
        fenster_vorher,
        fenster_nachher
    );
    assert!(
        nachher <= vorher + log_frames,
        "nach den Angriffen fehlen {} Frames",
        nachher.saturating_sub(vorher)
    );
    assert_eq!(
        fenster_nachher, fenster_vorher,
        "ein gestorbener Browser hat ein Fenster hinterlassen"
    );
}

/// Eine Zahl an einen String haengen (kein `format!` — das waere bei
/// 120 000 Durchlaeufen der teuerste Teil des Tests).
fn zahl_anhaengen(ziel: &mut String, wert: u32) {
    if wert == 0 {
        ziel.push('0');
        return;
    }
    let mut ziffern = [0u8; 10];
    let mut i = 10;
    let mut rest = wert;
    while rest > 0 {
        i -= 1;
        ziffern[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
    for &z in &ziffern[i..] {
        ziel.push(z as char);
    }
}

// ===========================================================================
// (8) DER ANGREIFER GEGEN DIE FENSTER-ABI
// ===========================================================================

/// `angreifer 10` und `11` — die neue Angriffsflaeche der Serie.
///
/// ===================================================================
/// WARUM DIESE ZWEI HIER STEHEN UND NICHT IN tests/sicherheit.rs
///
/// Sie brauchen einen FENSTER-MANAGER. `tests/sicherheit.rs` startet
/// keinen (es prueft die Syscalls, die ohne Desktop auskommen), und ein
/// Angriff, der still uebersprungen wird, weil das Fenster fehlt, ist
/// kein Angriff. Hier laeuft der Manager ohnehin.
///
/// Die Erwartung ist dieselbe wie bei allen ueberlebbaren Angriffen
/// seit Serie 6: **Exit 0 heisst, JEDER Versuch wurde sauber
/// abgelehnt.** Exit 1 hiesse, einer ist durchgekommen — das waere eine
/// echte Luecke.
#[test_case]
fn test_angreifer_gegen_die_fenster_abi() {
    if !programme_vorhanden() {
        return;
    }
    for (nummer, was) in [
        ("10", "Fenster-Syscalls mit boesen Rechtecken/Puffern"),
        ("11", "Fenster und Ereignisse fluten"),
    ] {
        serial_println!("  ANGRIFF {}: {}", nummer, was);
        let (ende, ausgabe, _) = laufen_lassen("angreifer", &["angreifer", nummer]);
        for zeile in ausgabe.lines() {
            serial_println!("    | {}", zeile);
        }
        assert_eq!(
            ende,
            Some(ProzessEnde::Beendet(0)),
            "ANGRIFF {} ({}) ist DURCHGEKOMMEN oder hat den Angreifer getoetet —              ein abgelehnter Syscall muss ein Fehlercode sein, kein Absturz",
            nummer,
            was
        );
        assert!(
            !ausgabe.contains("LUECKE"),
            "ANGRIFF {} meldet eine LUECKE:
{}",
            nummer,
            ausgabe
        );
        system_lebt_noch(was);
    }

    // Und der Fenster-Manager hat nichts behalten: Ein Angreifer, der
    // 32 Fenster oeffnet und stirbt, darf keines uebrig lassen.
    assert_eq!(
        fenster::prozess_fenster_anzahl(),
        0,
        "der Angreifer hat Fenster hinterlassen"
    );
}
