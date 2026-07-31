// tests/bilder.rs — DER BILDDEKODER GEGEN KAPUTTE UND BOESARTIGE DATEIEN
//                    (Serie 8, Teil 3)
//
// ===========================================================================
// WAS HIER BEWIESEN WIRD
//
// Ein Bilddekoder ist ein Parser fuer FREMDE DATEN. Die Frage ist deshalb
// nicht „kann er ein PNG lesen" — das kann jeder —, sondern: Was tut er,
// wenn die Datei luegt?
//
// Geprueft wird an 17 Dateien, die `tools/testbilder_erzeugen.py`
// ABSICHTLICH baut (fuenf gute, zwoelf kaputte bis boesartige) und die das
// build.rs mit ins Image legt. Jede traegt in `programme::TESTBILDER` ihre
// Erwartung mit sich:
//
//   Gut       -> MUSS dekodieren, und die Pixel werden gegen die FORMEL
//                aus dem Erzeuger-Skript geprueft.
//   Abgelehnt -> MUSS abgelehnt werden. Mit einem Fehler. Nicht mit einer
//                Panik, nicht mit einem Haenger, nicht mit 50 MiB Heap.
//   Egal      -> darf beides, solange es weder abstuerzt noch haengt.
//
// ===========================================================================
// WARUM DER TEST EINEN PROZESS STARTET
//
// Der Dekoder liegt in RING 3 (`libspeed::bild`) — der Kernel kennt ihn
// nicht und soll ihn nicht kennen (Regel: Fremdcode-Parser gehoeren in den
// User-Space). Ein Kernel-Test kann ihn also nicht aufrufen; er startet
// `bilder --pruefen <datei>` und liest die Ausgabe.
//
// Das ist kein Umweg, sondern die schaerfere Pruefung: Sie misst den
// GANZEN Weg — Datei vom Dateisystem, Syscalls, Heap-Wachstum, Dekoder,
// Prozess-Ende — und der Exit-Code 101 (Rust-Panik in libspeed) ist von
// einem sauberen „abgelehnt" (1) unterscheidbar. Dasselbe Muster wie
// `tests/sicherheit.rs` mit `angreifer`.

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
use speed_os::prozess::{self, Pid, ProzessEnde};
use speed_os::programme::{self, Erwartung};
use speed_os::syscall::handle::KernelObjekt;
use speed_os::{allocator, fs, memory, pipe, scheduler, serial_println, zeit};
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
    // 8 MiB — siehe tests/html.rs: `programme::installieren()` liest
    // jedes Programm ganz in den Heap, und die groessten sind ueber
    // 1 MB.
    allocator::heap_erweitern(2048).expect("Heap-Erweiterung fehlgeschlagen");

    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();

    fs::init();
    fs::platte_automounten();
    programme::installieren();
    programme::testbilder_installieren();

    scheduler::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

/// Frist je Bild. Grosszuegig — eine Dekompressionsbombe SOLL schnell
/// abgelehnt werden, aber wenn sie es nicht wird, soll der Test das als
/// Haenger melden und nicht selbst haengen.
const FRIST_MS: u64 = 20_000;

/// Exit-Code: sauber dekodiert.
const CODE_OK: i32 = 0;
/// Exit-Code: sauber abgelehnt (ein `BildFehler`).
const CODE_ABGELEHNT: i32 = 1;
/// Exit-Code des Panic-Handlers von libspeed. DER DARF NIE VORKOMMEN.
const CODE_PANIK: i32 = 101;

struct Lauf {
    ausgabe: String,
    ende: Option<ProzessEnde>,
    dauer_ms: u64,
}

impl Lauf {
    fn code(&self) -> i32 {
        match self.ende {
            Some(ende) => ende.code() as i32,
            None => -1,
        }
    }
    /// Die Wortliste der Ausgabezeile.
    fn felder(&self) -> Vec<&str> {
        self.ausgabe.split_whitespace().collect()
    }
    fn ist_ok(&self) -> bool {
        self.ausgabe.starts_with("ok ")
    }
    /// Das Fehler-Schlagwort (`BildFehler::kurz`), falls abgelehnt.
    fn fehler_kurz(&self) -> Option<&str> {
        self.ausgabe.strip_prefix("fehler ").map(|r| r.trim())
    }
}

/// Startet `bilder --pruefen <pfad>` und sammelt die Ausgabe ein.
fn pruefen(bild_pfad: &str) -> Lauf {
    let start = zeit::ms_seit_boot();
    let leitung = pipe::anlegen().expect("Pipe");
    let pfad = programme::pfad("bilder");
    pipe::ende_uebernehmen(leitung, pipe::Ende::Schreiben);
    let pid: Pid = prozess::prozess_starten_mit(
        &pfad,
        // argv[0] MUSS mitgegeben werden — `prozess_starten_mit` stellt
        // den Programmnamen NICHT selbst voran (dieselbe Konvention wie
        // in tests/netz_klient.rs). Ohne ihn waere `--pruefen` das
        // nullte Argument, und das Programm oeffnete ein Fenster.
        &["bilder", "--pruefen", bild_pfad],
        None,
        None,
        Some(KernelObjekt::PipeSchreiben(leitung)),
        false,
    )
    .expect("'bilder' starten");
    pipe::ende_schliessen(leitung, pipe::Ende::Schreiben);

    let mut gesammelt: Vec<u8> = Vec::new();
    let mut puffer = alloc::vec![0u8; 4096];
    let mut ende = None;
    let frist = zeit::ms_seit_boot() + FRIST_MS;
    loop {
        match pipe::lesen(leitung, &mut puffer) {
            pipe::PipeErgebnis::Bytes(0) => break,
            pipe::PipeErgebnis::Bytes(n) => gesammelt.extend_from_slice(&puffer[..n]),
            pipe::PipeErgebnis::Blockiert => {
                if zeit::ms_seit_boot() >= frist {
                    serial_println!("  !! Frist abgelaufen — der Dekoder haengt.");
                    break;
                }
                if ende.is_none() {
                    ende = scheduler::ende_abfragen(pid);
                }
                // Ein Pipe-Ende faellt erst beim ABRAEUMEN des beendeten
                // Prozesses (Serie 6, Teil 6) — ohne das kaeme nie ein
                // Dateiende.
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

    Lauf {
        ausgabe: String::from_utf8_lossy(&gesammelt).into_owned(),
        ende,
        dauer_ms: zeit::ms_seit_boot() - start,
    }
}

fn programme_vorhanden() -> bool {
    let da = programme::PROGRAMME.iter().all(|p| !p.elf.is_empty());
    if !da {
        serial_println!("  (uebersprungen: mit SPEEDOS_OHNE_USERLAND=1 gebaut)");
    }
    da
}

/// Sind die Testbilder eingebettet? `leer.png` ist absichtlich 0 Byte —
/// gefragt wird also nach den GUTEN.
fn testbilder_vorhanden() -> bool {
    let da = programme::TESTBILDER
        .iter()
        .filter(|b| b.erwartung == Erwartung::Gut)
        .all(|b| !b.daten.is_empty());
    if !da {
        serial_println!(
            "  (uebersprungen: keine Testbilder eingebettet — \
             `python tools/testbilder_erzeugen.py` und neu bauen)"
        );
    }
    da
}

// ---------------------------------------------------------------------------
// 1. Die Testbilder liegen auf dem Dateisystem
// ---------------------------------------------------------------------------

#[test_case]
fn test_testbilder_sind_installiert() {
    if !testbilder_vorhanden() {
        return;
    }
    let mut gefunden = 0;
    for bild in programme::TESTBILDER {
        let pfad = programme::bild_pfad(bild.name);
        let groesse = fs::mit_fs(|f| f.stat(&pfad))
            .unwrap_or_else(|fehler| panic!("{} fehlt: {:?}", pfad, fehler))
            .groesse;
        assert_eq!(
            groesse as usize,
            bild.daten.len(),
            "{} hat die falsche Groesse",
            pfad
        );
        gefunden += 1;
    }
    assert_eq!(gefunden, 17, "es sollen 17 Testbilder sein");
    serial_println!("  {} Testbilder auf dem Dateisystem.", gefunden);
}

// ---------------------------------------------------------------------------
// 2. DER HAUPTTEST: jedes Bild einmal durch den Dekoder
// ---------------------------------------------------------------------------

/// Jedes Testbild wird dekodiert — und JEDES Ergebnis muss zu seiner
/// Erwartung passen.
///
/// DIE EINE ZUSAGE, DIE FUER ALLE 17 GILT: kein Exit-Code 101. Eine Panik
/// im Dekoder waere in Ring 3 zwar folgenlos fuer den Kernel (Dauerregel
/// II), aber sie waere ein Programm, das VERSCHWINDET statt „kaputtes
/// Bild" anzuzeigen — und im kommenden Renderer ein Bild, das die ganze
/// Seite abschiesst.
#[test_case]
fn test_jedes_testbild_wird_sauber_behandelt() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    serial_println!("  Datei                      Code  Ausgabe");
    serial_println!("  ---------------------------------------------------------------");

    let mut gut = 0;
    let mut abgelehnt = 0;
    for bild in programme::TESTBILDER {
        let pfad = programme::bild_pfad(bild.name);
        let lauf = pruefen(&pfad);

        serial_println!(
            "  {:<26} {:>4}  {} ({} ms)",
            bild.name,
            lauf.code(),
            lauf.ausgabe.trim(),
            lauf.dauer_ms
        );

        // (a) NIE eine Panik — fuer alle drei Erwartungen.
        assert_ne!(
            lauf.code(),
            CODE_PANIK,
            "{} hat den Dekoder zum PANICKEN gebracht: {}",
            bild.name,
            lauf.ausgabe.trim()
        );
        // (b) NIE ein Haenger: der Prozess muss geendet haben.
        assert!(
            lauf.ende.is_some(),
            "{} hat den Dekoder haengen lassen",
            bild.name
        );

        match bild.erwartung {
            Erwartung::Gut => {
                assert_eq!(
                    lauf.code(),
                    CODE_OK,
                    "{} MUSS dekodieren, meldete aber: {}",
                    bild.name,
                    lauf.ausgabe.trim()
                );
                assert!(lauf.ist_ok(), "{}: unerwartete Ausgabe", bild.name);
                gut += 1;
            }
            Erwartung::Abgelehnt => {
                assert_eq!(
                    lauf.code(),
                    CODE_ABGELEHNT,
                    "{} MUSS abgelehnt werden, meldete aber: {}",
                    bild.name,
                    lauf.ausgabe.trim()
                );
                assert!(
                    lauf.fehler_kurz().is_some(),
                    "{}: kein Fehler-Schlagwort in '{}'",
                    bild.name,
                    lauf.ausgabe.trim()
                );
                abgelehnt += 1;
            }
            Erwartung::Egal => {
                assert!(
                    lauf.code() == CODE_OK || lauf.code() == CODE_ABGELEHNT,
                    "{}: unerwarteter Code {}",
                    bild.name,
                    lauf.code()
                );
            }
        }
    }
    serial_println!("  {} dekodiert, {} abgelehnt, 0 Paniken.", gut, abgelehnt);
}

// ---------------------------------------------------------------------------
// 3. Die guten Bilder stimmen PIXELGENAU
// ---------------------------------------------------------------------------

/// `verlauf.png` ist 64x48 RGB nach der Formel
///
///     rot = x * 255 / (breite-1),  gruen = y * 255 / (hoehe-1),  blau = 0x40
///
/// (tools/testbilder_erzeugen.py, `bild_verlauf`). Pixel (0,0) ist also
/// `0xFF000040` als AARRGGBB (Alpha 255, rot 0, gruen 0, blau 0x40),
/// Pixel (63,47) `0xFFFFFF40`.
///
/// WARUM PIXEL UND NICHT NUR MASSE: Ein Dekoder, der RGB als BGR ausliefert,
/// liefert ein Bild in der richtigen Groesse — es ist nur blau statt rot.
/// Genau diesen Fehler faengt kein Groessen-Vergleich.
#[test_case]
fn test_verlauf_hat_die_richtigen_pixel() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("verlauf.png"));
    let f = lauf.felder();
    assert!(f.len() >= 6, "Ausgabe zu kurz: {}", lauf.ausgabe.trim());
    assert_eq!(f[0], "ok");
    assert_eq!(f[1], "64", "Breite");
    assert_eq!(f[2], "48", "Hoehe");
    // 64 * 48 * 4 Byte RGBA.
    assert_eq!(f[3], "12288", "RGBA-Bytes");
    assert_eq!(f[4], "ff000040", "Pixel (0,0): rot=0, gruen=0, blau=0x40");
    assert_eq!(f[5], "ffffff40", "Pixel (63,47): rot=255, gruen=255, blau=0x40");
    serial_println!("  verlauf.png ist pixelgenau richtig.");
}

/// `rgba.png` hat ein DURCHSICHTIGES linkes oberes Viertel (Alpha 0) und
/// ist sonst `0xE03090` deckend. Pixel (0,0) liegt im durchsichtigen Teil.
///
/// Der Test beweist, dass der Alpha-Kanal ueberhaupt ankommt — ohne ihn
/// koennte der Renderer keine transparenten Bilder ueber Hintergruende
/// legen, und `pixel_auf` haette nichts zu mischen.
#[test_case]
fn test_alpha_kommt_an() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("rgba.png"));
    let f = lauf.felder();
    assert_eq!(f[0], "ok", "Ausgabe: {}", lauf.ausgabe.trim());
    assert_eq!(f[1], "32");
    assert_eq!(f[2], "32");
    // (0,0) ist im durchsichtigen Viertel: Alpha 0.
    assert_eq!(f[4], "00e03090", "Pixel (0,0) muss durchsichtig sein");
    // (31,31) ist ausserhalb: voll deckend.
    assert_eq!(f[5], "ffe03090", "Pixel (31,31) muss deckend sein");
    serial_println!("  Alpha-Kanal kommt korrekt an.");
}

/// Graustufen und Palette muessen ebenfalls als RGBA herauskommen — die
/// Zusage „die Ausgabe ist IMMER RGBA" gilt fuer JEDEN Eingabefarbraum.
///
/// `grau.png` ist 40x24, Wert `(x*8 + y*4) & 0xFF`; (0,0) ist also 0
/// (schwarz, deckend), (39,23) ist (39*8 + 23*4) & 0xFF = 312+92 = 404 &
/// 0xFF = 0x94.
#[test_case]
fn test_grau_und_palette_werden_zu_rgba() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let grau = pruefen(&programme::bild_pfad("grau.png"));
    let f = grau.felder();
    assert_eq!(f[0], "ok", "grau: {}", grau.ausgabe.trim());
    assert_eq!(f[1], "40");
    assert_eq!(f[2], "24");
    // 40*24*4 — auch ein 1-Kanal-Bild belegt am Ende 4 Byte je Pixel.
    assert_eq!(f[3], "3840", "auch Grau kommt als RGBA heraus");
    assert_eq!(f[4], "ff000000", "Grauwert 0 -> schwarz, deckend");
    assert_eq!(f[5], "ff949494", "Grauwert 0x94 -> auf alle drei Kanaele");

    let palette = pruefen(&programme::bild_pfad("palette.png"));
    let f = palette.felder();
    assert_eq!(f[0], "ok", "palette: {}", palette.ausgabe.trim());
    assert_eq!(f[3], "1536", "24*16*4");
    // Index (x+y)%4, Palette[0] = rot.
    assert_eq!(f[4], "ffff0000", "Palette-Index 0 ist rot");
    serial_println!("  Grau und Palette werden korrekt zu RGBA.");
}

// ---------------------------------------------------------------------------
// 4. DIE ANGRIFFE, einzeln und mit Begruendung
// ---------------------------------------------------------------------------

/// DIE DEKOMPRESSIONSBOMBE — der wichtigste Einzeltest dieser Datei.
///
/// `bombe.png` ist 48 KiB gross, deklariert 4096x4096 und ist FORMAL
/// EINWANDFREI: Es gibt nichts Unplausibles daran, ein Dekoder findet
/// keinen Fehler. Dekodiert waere es 50 MiB — mehr als das Vierfache des
/// gesamten Prozess-Heaps (12 MiB).
///
/// Sie MUSS an einer GRENZE scheitern, nicht an einer Prueffung. Genau
/// deshalb hat `libspeed::bild` `Grenzen::max_pixel` und liest den Kopf,
/// BEVOR es alloziert. Ohne diese Grenze wuerde hier der Heap volllaufen —
/// und zwar mit einer Datei, die in eine E-Mail passt.
///
/// Der Test prueft ausserdem die ZEIT: Eine Ablehnung muss SCHNELL kommen.
/// Wer erst 50 MiB dekodiert und dann ablehnt, hat die Grenze nicht
/// verstanden.
#[test_case]
fn test_dekompressionsbombe_stirbt_an_der_grenze() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("bombe.png"));
    assert_eq!(
        lauf.code(),
        CODE_ABGELEHNT,
        "die Bombe kam durch: {}",
        lauf.ausgabe.trim()
    );
    assert_eq!(
        lauf.fehler_kurz(),
        Some("zu-gross"),
        "die Bombe muss an der PIXELGRENZE sterben, nicht anderswo"
    );
    // Der Kopf eines PNG steht in den ersten 33 Bytes. Die Ablehnung
    // darf nicht laenger dauern als ein Prozess-Start.
    assert!(
        lauf.dauer_ms < 3_000,
        "die Ablehnung dauerte {} ms — es wurde offenbar erst dekodiert",
        lauf.dauer_ms
    );
    serial_println!(
        "  Bombe (48 KiB -> 50 MiB) in {} ms abgelehnt: zu-gross.",
        lauf.dauer_ms
    );
}

/// ABSURDE MASSE: 100000 x 100000 (10 Milliarden Pixel, 40 GB als RGBA)
/// bei 69 Byte Datei. Wer dem Kopf glaubt und vorab alloziert, ist mit
/// EINER Mail zu toeten.
#[test_case]
fn test_absurde_masse_werden_abgelehnt() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("absurde_masse.png"));
    assert_eq!(lauf.code(), CODE_ABGELEHNT, "{}", lauf.ausgabe.trim());
    // Kein Anspruch, WELCHE Grenze zuerst greift (Kantenlaenge oder
    // Pixelzahl) — nur, DASS eine greift und es eine Groessen-Aussage ist.
    let kurz = lauf.fehler_kurz().unwrap_or("");
    assert!(
        kurz == "zu-gross" || kurz == "kaputter-kopf",
        "unerwarteter Grund: {}",
        kurz
    );
    serial_println!("  100000x100000 abgelehnt ({}).", kurz);
}

/// ABGESCHNITTEN — der haeufigste ECHTE Schaden (abgebrochener Download,
/// volle Platte). Anders als die Angriffe oben ist das kein Boeswilliger,
/// sondern der Alltag.
#[test_case]
fn test_abgeschnittene_datei_wird_abgelehnt() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("abgeschnitten.png"));
    assert_eq!(lauf.code(), CODE_ABGELEHNT, "{}", lauf.ausgabe.trim());
    serial_println!(
        "  abgeschnittenes PNG abgelehnt ({}).",
        lauf.fehler_kurz().unwrap_or("?")
    );
}

/// LEER, NUR SIGNATUR, KEIN BILD — die drei Faelle, die man beim Testen
/// vergisst, weil sie zu einfach aussehen.
#[test_case]
fn test_leere_und_fremde_dateien() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let leer = pruefen(&programme::bild_pfad("leer.png"));
    assert_eq!(leer.code(), CODE_ABGELEHNT);
    assert_eq!(leer.fehler_kurz(), Some("leer"), "0 Byte ist ein eigener Fall");

    let sig = pruefen(&programme::bild_pfad("nur_signatur.png"));
    assert_eq!(sig.code(), CODE_ABGELEHNT);

    let fremd = pruefen(&programme::bild_pfad("kein_bild.png"));
    assert_eq!(fremd.code(), CODE_ABGELEHNT);
    assert_eq!(
        fremd.fehler_kurz(),
        Some("unbekanntes-format"),
        "eine Textdatei ist kein kaputtes Bild, sondern gar keins"
    );
    serial_println!("  leer / nur Signatur / Textdatei: alle drei sauber abgelehnt.");
}

/// EINE RIESIGE CHUNK-LAENGE (0xFFFFFFFF). Wer daraus eine Puffergroesse
/// macht, hat verloren.
#[test_case]
fn test_riesige_chunk_laenge() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    let lauf = pruefen(&programme::bild_pfad("riesige_chunk_laenge.png"));
    assert_eq!(lauf.code(), CODE_ABGELEHNT, "{}", lauf.ausgabe.trim());
    assert_ne!(lauf.code(), CODE_PANIK);
    serial_println!(
        "  Chunk-Laenge 0xFFFFFFFF abgelehnt ({}).",
        lauf.fehler_kurz().unwrap_or("?")
    );
}

// ---------------------------------------------------------------------------
// 5. DER SPEICHER: 17 Bilder hintereinander lecken nichts
// ---------------------------------------------------------------------------

/// Nach 17 Dekodier-Prozessen muss die Frame-Bilanz stimmen.
///
/// DIE MESSUNG IST DIESELBE WIE IN SERIE 6/7, und die dort gefundene
/// Unschaerfe gilt weiter: `memory::allocate_pages` laesst alle 512 Seiten
/// eine P1-Tabelle im Kernel-Adressraum zurueck. Die Schranke wird
/// AUSGERECHNET, nicht weggelassen — sonst waere die Bilanz eine
/// Behauptung.
#[test_case]
fn test_kein_leck_ueber_alle_bilder() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    // Zur Ruhe kommen, dann messen.
    scheduler::aufraeumen();
    let vorher = memory::frame_statistik().0;
    let log_vorher = speed_os::protokoll::puffer_bytes();

    for bild in programme::TESTBILDER {
        let _ = pruefen(&programme::bild_pfad(bild.name));
    }
    scheduler::aufraeumen();

    let nachher = memory::frame_statistik().0;
    let log_nachher = speed_os::protokoll::puffer_bytes();

    // 17 Prozesse, jeder mit Adressraum, Stacks und (bei den guten) einem
    // MiB-Puffer. Die P1-Schranke: ~340 Seiten je Prozess / 512.
    let schranke = (17 * 340) / 512 + 2;
    let verloren = vorher.saturating_sub(nachher);
    serial_println!(
        "  17 Dekodier-Prozesse: {} Frames verloren (Schranke {}), \
         Log-Puffer +{} Byte.",
        verloren,
        schranke,
        log_nachher.saturating_sub(log_vorher)
    );
    assert!(
        verloren <= schranke,
        "Frame-Leck: {} verloren, erlaubt sind {}",
        verloren,
        schranke
    );
}

// ---------------------------------------------------------------------------
// 6. Die Heap-Spitze — die Zahl, an der die Grenzen haengen
// ---------------------------------------------------------------------------

/// Das groesste gute Bild (160x120) und was es an Heap kostet.
///
/// BERICHTS-TEST mit einer Obergrenze: Die Spitze MUSS unter dem
/// Prozess-Heap (12 MiB) liegen, sonst waeren die Grenzen in
/// `libspeed::bild` falsch gerechnet. Die genaue Zahl wandert ins
/// CHANGELOG und nach docs/bild-entscheidung.md.
#[test_case]
fn test_heap_spitze_wird_berichtet() {
    if !programme_vorhanden() || !testbilder_vorhanden() {
        return;
    }
    for name in ["verlauf.png", "gross.png", "rgba.png"] {
        let lauf = pruefen(&programme::bild_pfad(name));
        let f = lauf.felder();
        assert_eq!(f[0], "ok", "{}: {}", name, lauf.ausgabe.trim());
        let rgba: usize = f[3].parse().unwrap_or(0);
        let spitze: usize = f[6].parse().unwrap_or(0);
        serial_println!(
            "  {:<14} RGBA {:>8} B   Heap-Spitze {:>8} B   ({} ms)",
            name,
            rgba,
            spitze,
            lauf.dauer_ms
        );
        assert!(
            spitze < 12 * 1024 * 1024,
            "{}: Heap-Spitze {} B sprengt den Prozess-Heap",
            name,
            spitze
        );
        // Die Spitze muss mindestens den RGBA-Puffer enthalten — sonst
        // misst die Messung etwas anderes als das, was sie behauptet.
        assert!(
            spitze >= rgba,
            "{}: Spitze {} < RGBA-Puffer {} — die Messung stimmt nicht",
            name,
            spitze,
            rgba
        );
    }
}
