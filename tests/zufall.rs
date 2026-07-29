// tests/zufall.rs — Der Zufallsgenerator, gegen echte Hardware geprüft
//
// ==========================================================================
// WAS DIESE DATEI BEWEIST — UND WAS NICHT
//
// Das ist bei einem Zufallsgenerator keine Floskel, sondern die wichtigste
// Aussage der ganzen Datei, deshalb steht sie ganz oben:
//
//   BELASTBAR ist genau EIN Teil: die DRBG-Konstruktion gegen die
//   Testvektoren aus RFC 8439 (`speed_os::zufall::tests::
//   test_chacha20_rfc8439_vektoren`, im Lib-Test). Wäre dort eine Rotation,
//   eine Addition oder die Byte-Reihenfolge falsch, stimmte kein einziges
//   Byte. Das ist ein Beweis.
//
//   NICHT BELASTBAR ist alles Statistische in dieser Datei. Byteverteilung,
//   keine Wiederholungen über N MiB, unterschiedliche Werte nach Neustart —
//   ein simpler Zähler, durch AES geschickt, besteht jeden dieser Tests mit
//   Bestnote und ist trotzdem vollständig vorhersagbar.
//
// WOZU DANN? Weil sie eine bestimmte Fehlerklasse finden, und zwar
// zuverlässig: der Generator lief gar nicht (Puffer bleibt genullt), er
// liefert einen konstanten Wert (Schlüssel/Zähler bewegen sich nicht), er
// zählt statt zu würfeln (erkennbare Struktur), oder er startet nach jedem
// Neustart identisch (Salz/Pool wirken nicht). Das sind die Fehler, die man
// beim Bauen tatsächlich macht — nicht die, die einen Angreifer freuen.
//
// Ein grünes Häkchen hier heisst also: „keine groben Fehler". Es heisst
// NICHT: „kryptographisch sicher". Wer das verwechselt, hat den gefährlichen
// Teil der Arbeit noch vor sich.
// ==========================================================================

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::prozess::{self, Pid};
use speed_os::scheduler;
use speed_os::syscall::{Fehler, SYS_ZUFALL};
use speed_os::zufall::{self, Quelle, ZufallFehler};
use speed_os::{allocator, memory, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    zufall::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(512).expect("Heap-Erweiterung fehlgeschlagen");
    // Fuer den Ring-3-Beweis des Syscalls (Pruefstand-Prozess).
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

/// Wartet, bis der Generator gesät ist (oder die Frist abläuft).
///
/// DIESER WEG WIRD IMMER DURCHLAUFEN, auch mit Hardware-Quelle — und das ist
/// kein Zufall, sondern der Deckel aus docs/zufall.md §3: RDSEED/RDRAND
/// dürfen höchstens die HALBE Schwelle beisteuern (128 von 256 Bit). Der
/// Rest muss aus Interrupt-Jitter kommen. Der Bootlog zeigt es:
///
/// ```text
/// [ZUFALL] RDSEED: ja, RDRAND: ja — Start mit 134 von 256 Bit.
/// [ZUFALL] Noch nicht gesaet — es fehlen 122 Bit aus Interrupt-Jitter.
/// ```
///
/// Damit ist der unangenehme Fall aus §4 bei JEDEM Testlauf Teil des Wegs
/// und kann nicht unbemerkt verrotten.
fn warten_bis_gesaet(frist_ms: u64) -> bool {
    let frist = zeit::ms_seit_boot() + frist_ms;
    while !zufall::bereit() {
        if zeit::ms_seit_boot() >= frist {
            return false;
        }
        // Nicht `hlt` allein: `nachsaeen` läuft im Betrieb aus einem Task,
        // den es im Testkernel nicht gibt. Also selbst anstossen, sobald die
        // Schwelle erreicht ist.
        if zufall::status().entropie_bits >= zufall::SCHWELLE_BITS {
            zufall::nachsaeen();
        }
        zeit::warte_auf_interrupt();
    }
    true
}

/// Holt `n` Bytes oder bricht den Test ab.
fn bytes(n: usize) -> Vec<u8> {
    let mut puffer = alloc::vec![0u8; n];
    zufall::fuellen(&mut puffer).expect("der Generator ist gesaet, liefert aber nichts");
    puffer
}

// ===========================================================================
// 1. DER ZUSTAND „NOCH NICHT GESÄT" IST EIN ECHTER ZUSTAND
// ===========================================================================

/// Der Generator durchläuft beim Boot nachweislich BEIDE Zustände — und im
/// ersten gibt es KEINE Bytes.
///
/// Das ist der Test, der die Entscheidung aus docs/zufall.md §4 festnagelt:
/// lieber warten als schwachen Zufall. Er läuft als erster, weil der
/// ungesäte Zustand nach ein paar Sekunden nicht mehr herstellbar ist.
#[test_case]
fn test_zustand_nicht_gesaet_dann_gesaet() {
    // DER STARTBEFUND, festgehalten in `init()`. Ohne ihn prüfte dieser Test
    // gar nichts: Die Tests laufen alphabetisch, dieser hier zuletzt — bis
    // dahin haben die anderen den Pool längst gefüllt, und der interessante
    // Zustand ist nicht mehr herstellbar. Genau deshalb wird er zum einzig
    // richtigen Zeitpunkt eingefroren.
    let (start_bits, start_gesaet) =
        zufall::startbefund().expect("zufall::init() lief nicht");
    serial_println!("  === STARTBEFUND (Ende von zufall::init) ===");
    serial_println!("    {} Bit, gesaet: {}", start_bits, start_gesaet);
    assert!(
        !start_gesaet,
        "der Generator war direkt nach init() schon gesaet ({} Bit) — dann hat \
         eine einzelne Quelle die Schwelle allein erreicht, und der Deckel aus \
         docs/zufall.md §3 greift nicht",
        start_bits
    );
    assert!(
        start_bits < zufall::SCHWELLE_BITS,
        "beim Start lagen {} von {} Bit an — die Hardware-Quelle darf hoechstens \
         die Haelfte beisteuern",
        start_bits,
        zufall::SCHWELLE_BITS
    );
    serial_println!(
        "    -> beim Start NICHT gesaet, obwohl RDSEED/RDRAND vorhanden sind."
    );
    serial_println!("       Das ist der Deckel bei der Arbeit: nie aus einer Quelle allein.");

    let start = zufall::status();
    serial_println!("  === ZUSTAND BEIM BOOT ===");
    serial_println!(
        "    gesaet: {}, Entropie: {} von {} Bit",
        start.gesaet,
        start.entropie_bits,
        start.schwelle_bits
    );
    serial_println!(
        "    RDSEED: {}, RDRAND: {}, Hardware defekt: {}",
        start.rdseed,
        start.rdrand,
        start.hardware_defekt
    );

    if !start.gesaet {
        // DER WICHTIGE FALL: ungesät heisst KEINE Bytes, und der Puffer
        // bleibt unangetastet. Ein halb gefüllter Puffer wäre hier besonders
        // heimtückisch — die Nullen sähen wie Zufall aus.
        let mut puffer = [0xA5u8; 64];
        assert_eq!(
            zufall::fuellen(&mut puffer),
            Err(ZufallFehler::NichtGesaet),
            "ungesaet muss ein Fehler sein, kein schwacher Zufall"
        );
        assert_eq!(puffer, [0xA5u8; 64], "der Puffer wurde trotz Fehler angefasst");
        serial_println!("    -> ungesaet liefert NICHTS (und laesst den Puffer in Ruhe). OK");
    } else {
        serial_println!(
            "    -> beim Start bereits gesaet (Hardware-Quelle vorhanden); \
             der ungesaete Zustand ist hier nicht pruefbar."
        );
    }

    // Und jetzt der Übergang: Der Pool füllt sich aus Interrupt-Jitter.
    let gesaet = warten_bis_gesaet(30_000);
    let jetzt = zufall::status();
    serial_println!(
        "    Nach dem Warten: gesaet {}, {} Bit, {} Nachsaaten, {} ms seit Boot",
        jetzt.gesaet,
        jetzt.entropie_bits,
        jetzt.nachsaaten,
        zeit::ms_seit_boot()
    );
    assert!(
        gesaet,
        "der Generator wurde in 30 s nicht gesaet — mit welcher Quelle auch immer, \
         das ist zu langsam"
    );
    // Jetzt gibt es Bytes.
    let mut puffer = [0u8; 32];
    assert_eq!(zufall::fuellen(&mut puffer), Ok(()));
}

// ===========================================================================
// 2. DIE QUELLEN LIEFERN WIRKLICH
// ===========================================================================

/// Die Entropie-Quellen sind ANGESCHLOSSEN — der Test prüft, dass die
/// IRQ-Handler tatsächlich einspeisen, nicht nur, dass es die Funktion gibt.
///
/// Der PIT muss laufen (er tickt immer). Tastatur und Maus können in einem
/// automatisierten Lauf naturgemäss fehlen — das ist kein Fehler, sondern
/// der Grund, warum der PIT-Pfad überhaupt existiert.
#[test_case]
fn test_quellen_speisen_ein() {
    let vorher = zufall::status();
    // Eine halbe Sekunde ticken lassen.
    let bis = zeit::ms_seit_boot() + 500;
    while zeit::ms_seit_boot() < bis {
        zeit::warte_auf_interrupt();
    }
    let nachher = zufall::status();

    serial_println!("  === QUELLEN ===");
    for quelle in Quelle::alle() {
        let i = quelle.index();
        let zuwachs = nachher.proben[i].saturating_sub(vorher.proben[i]);
        serial_println!(
            "    {:<16} {:>8} Proben (+{} in 500 ms), {} Bit/Probe",
            quelle.name(),
            nachher.proben[i],
            zuwachs,
            quelle.bits_je_probe()
        );
    }

    // Der PIT MUSS ticken — sonst ist der Handler-Hook nicht angeschlossen.
    let pit = Quelle::Pit.index();
    let pit_zuwachs = nachher.proben[pit] - vorher.proben[pit];
    assert!(
        pit_zuwachs > 100,
        "in 500 ms kamen nur {} PIT-Proben an (erwartet ~125) — \
         der Einspeise-Haken im Timer-Handler fehlt",
        pit_zuwachs
    );
    // Das Boot-Salz wurde eingemischt (aber mit 0 Bit angerechnet).
    assert!(
        nachher.proben[Quelle::Salz.index()] > 0,
        "das Boot-Salz wurde nie eingemischt"
    );
    assert_eq!(
        Quelle::Salz.bits_je_probe(),
        0,
        "SALZ IST KEINE ENTROPIE — es darf nie angerechnet werden"
    );
}

// ===========================================================================
// 3. STATISTIK — findet grobe Fehler, beweist KEINE Qualität
// ===========================================================================

/// STATISTIK-TEST 1: Byteverteilung über 1 MiB.
///
/// ACHTUNG, und das ist der Punkt dieses Kommentars: Dieser Test BEWEIST
/// NICHTS über die kryptographische Qualität. Ein Zähler, durch eine
/// Blockchiffre geschickt, hat eine perfekte Byteverteilung und ist
/// vollständig vorhersagbar.
///
/// Was er findet: einen Generator, der gar nicht läuft (alles 0), einen mit
/// festem Wert, oder einen mit grob schiefer Verteilung (kaputte Maskierung,
/// falsche Byte-Extraktion). Genau die Fehler, die man beim Bauen macht.
///
/// Die Schranken sind BEWUSST WEIT: Bei 1 MiB und 256 Werten liegt der
/// Erwartungswert bei 4096 je Byte, die Standardabweichung bei ~64. Wir
/// erlauben ±50 % — ein echter Ausreisser wäre um Grössenordnungen daneben,
/// und ein enger Test würde gelegentlich grundlos rot (dieselbe Methodik wie
/// beim Netz-Stresstest).
#[test_case]
fn test_statistik_byteverteilung() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    const GESAMT: usize = 1024 * 1024;
    const BLOCK: usize = 16 * 1024;

    let mut haeufigkeit = [0u32; 256];
    let mut nullen_bloecke = 0usize;
    for _ in 0..(GESAMT / BLOCK) {
        let block = bytes(BLOCK);
        if block.iter().all(|b| *b == 0) {
            nullen_bloecke += 1;
        }
        for byte in &block {
            haeufigkeit[*byte as usize] += 1;
        }
    }

    let erwartet = (GESAMT / 256) as u32;
    let min = *haeufigkeit.iter().min().unwrap();
    let max = *haeufigkeit.iter().max().unwrap();
    // Fehlende Werte sind der eindeutigste grobe Fehler.
    let nie_gesehen = haeufigkeit.iter().filter(|h| **h == 0).count();

    serial_println!("  === STATISTIK: Byteverteilung ueber 1 MiB ===");
    serial_println!(
        "    Erwartung {} je Wert; gemessen min {}, max {}, nie gesehen: {}",
        erwartet,
        min,
        max,
        nie_gesehen
    );
    serial_println!(
        "    (Findet grobe Fehler. BEWEIST KEINE kryptographische Qualitaet —"
    );
    serial_println!("     ein Zaehler durch AES bestuende diesen Test ebenfalls.)");

    assert_eq!(nullen_bloecke, 0, "der Generator lieferte einen Block aus lauter Nullen");
    assert_eq!(nie_gesehen, 0, "{} Byte-Werte kamen NIE vor", nie_gesehen);
    assert!(
        min > erwartet / 2,
        "der seltenste Wert kam nur {}x vor (erwartet ~{})",
        min,
        erwartet
    );
    assert!(
        max < erwartet * 3 / 2,
        "der haeufigste Wert kam {}x vor (erwartet ~{})",
        max,
        erwartet
    );
}

/// STATISTIK-TEST 2: keine Wiederholungen und keine Struktur.
///
/// Auch hier: Das findet den Zähler-statt-Zufall-Fehler und den
/// „Schlüssel bewegt sich nicht"-Fehler. Mehr nicht.
///
/// Drei Prüfungen mit unterschiedlicher Zielrichtung:
///  * Kein 16-Byte-Block wiederholt sich (bei einem echten Generator wäre
///    das astronomisch unwahrscheinlich; bei einem festen Zustand passiert
///    es sofort).
///  * Aufeinanderfolgende u64 sind nicht monoton (das wäre ein Zähler).
///  * Zwei aufeinanderfolgende Aufrufe liefern Verschiedenes (Key Erasure).
#[test_case]
fn test_statistik_keine_wiederholung() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    const BLOECKE: usize = 64 * 1024; // 1 MiB in 16-Byte-Blöcken

    let daten = bytes(BLOECKE * 16);

    // Wiederholte 16-Byte-Blöcke — ohne Sortieren (kein Heap-Druck): Wir
    // vergleichen jeden Block gegen einen kleinen Ring der letzten 256.
    // Das findet den Fall „Zustand steht still" sicher, ohne O(n²).
    let mut ring: [[u8; 16]; 256] = [[0; 16]; 256];
    let mut treffer = 0usize;
    for (i, brocken) in daten.chunks_exact(16).enumerate() {
        let mut block = [0u8; 16];
        block.copy_from_slice(brocken);
        if i >= 256 && ring.contains(&block) {
            treffer += 1;
        }
        ring[i % 256] = block;
    }

    // Monotonie: ein Zähler wäre streng steigend (oder fallend).
    let mut steigend = 0usize;
    let werte: Vec<u64> = daten
        .chunks_exact(8)
        .take(4096)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .collect();
    for paar in werte.windows(2) {
        if paar[1] > paar[0] {
            steigend += 1;
        }
    }

    // Key Erasure: zwei Aufrufe, garantiert verschieden.
    let a = bytes(64);
    let b = bytes(64);

    serial_println!("  === STATISTIK: Wiederholung und Struktur ===");
    serial_println!(
        "    {} wiederholte 16-Byte-Bloecke in 1 MiB (erwartet: 0)",
        treffer
    );
    serial_println!(
        "    {} von {} u64-Paaren steigend (erwartet ~50 %)",
        steigend,
        werte.len() - 1
    );

    assert_eq!(treffer, 0, "es gab wiederholte Bloecke — der Zustand steht still");
    // Ein Zähler wäre bei ~100 %, ein Konstant-Generator bei 0 %.
    let anteil = steigend * 100 / (werte.len() - 1);
    assert!(
        (25..=75).contains(&anteil),
        "{} % der Paare sind steigend — das sieht nach einem Zaehler aus",
        anteil
    );
    assert_ne!(a, b, "zwei Aufrufe liefern dieselben Bytes (Key Erasure kaputt)");
}

/// STATISTIK-TEST 3: unterschiedliche Werte nach einem NEUSTART.
///
/// Der einzige der drei Statistik-Tests, der etwas prüft, das nicht schon in
/// den Testvektoren steckt: dass Pool und Salz überhaupt in den Zustand
/// eingehen. Ein Generator mit fest eincompiliertem Startschlüssel lieferte
/// nach jedem Boot dieselbe Folge — der klassische, katastrophale Fehler
/// (und einer, den es in freier Wildbahn wirklich gab).
///
/// UMSETZUNG OHNE echten Neustart: Wir vergleichen mit einem Wert, den ein
/// FRÜHERER Lauf auf die Platte geschrieben hat. Ist keine Platte da, wird
/// der Test ehrlich übersprungen statt etwas Falsches zu behaupten.
#[test_case]
fn test_neustart_liefert_andere_werte() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    use speed_os::fs;

    speed_os::ata::init();
    speed_os::pci::init();
    speed_os::virtio::blk::init();
    fs::init();
    fs::platte_automounten();

    const PFAD: &str = "/platte/system/zufall-probe.bin";
    let jetzt = bytes(32);

    let vorher = fs::mit_fs(|dateisystem| dateisystem.lesen(PFAD)).ok();
    match &vorher {
        Some(alt) if alt.len() == 32 => {
            serial_println!("  === NEUSTART-VERGLEICH ===");
            serial_println!("    Probe des vorigen Laufs gefunden ({} Byte).", alt.len());
            assert_ne!(
                alt, &jetzt,
                "der Generator liefert nach einem Neustart DIESELBE Folge — \
                 Pool und Salz gehen nicht in den Zustand ein"
            );
            serial_println!("    -> andere Werte als beim vorigen Lauf. OK");
        }
        _ => {
            serial_println!(
                "  (Neustart-Vergleich: keine Probe des vorigen Laufs — beim \
                 naechsten Lauf greift der Test.)"
            );
        }
    }

    // Für den nächsten Lauf hinterlegen. Ein Fehler ist hier KEIN
    // Testfehler (die Platte ist optional), aber er wird gemeldet.
    match fs::mit_fs(|dateisystem| dateisystem.schreiben(PFAD, &jetzt)) {
        Ok(()) => {
            let _ = fs::sync();
        }
        Err(fehler) => serial_println!(
            "  (Probe nicht gespeichert: {} — der Vergleich entfaellt naechstes Mal)",
            fehler.meldung()
        ),
    }
}

// ===========================================================================
// 4. DIE ROBUSTHEIT
// ===========================================================================

/// Jede Puffergrösse funktioniert — besonders die Übergänge zwischen den
/// 64-Byte-Blöcken und die 32-Byte-Grenze der Key Erasure.
///
/// Dort wohnt der klassische Off-by-one: Der erste Block liefert nur 32 Byte
/// Ausgabe (die anderen 32 sind der neue Schlüssel), jeder weitere 64.
#[test_case]
fn test_alle_puffergroessen() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    for groesse in [
        0usize, 1, 7, 8, 31, 32, 33, 63, 64, 65, 95, 96, 97, 127, 128, 1000, 4096,
    ] {
        let mut puffer = alloc::vec![0xEEu8; groesse];
        assert_eq!(
            zufall::fuellen(&mut puffer),
            Ok(()),
            "Groesse {} schlug fehl",
            groesse
        );
        if groesse >= 16 {
            // Nicht mehr das Füllmuster, und nicht alles gleich.
            assert!(
                puffer.iter().any(|b| *b != 0xEE),
                "Groesse {}: der Puffer wurde nicht beschrieben",
                groesse
            );
            assert!(
                puffer.iter().any(|b| *b != puffer[0]),
                "Groesse {}: alle Bytes sind gleich",
                groesse
            );
        }
    }
    serial_println!("  Alle Puffergroessen (0 bis 4096, inkl. der 32/64-Byte-Grenzen): OK");
}

/// Nachsäen im laufenden Betrieb bricht nichts — und der Generator liefert
/// danach andere Werte.
#[test_case]
fn test_nachsaeen_im_betrieb() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    let vor = bytes(64);
    let saaten_vorher = zufall::status().nachsaaten;

    for _ in 0..10 {
        zufall::nachsaeen();
        let mittendrin = bytes(64);
        assert_ne!(mittendrin, vor, "nach dem Nachsaeen dieselben Bytes");
    }
    let s = zufall::status();
    assert!(
        s.nachsaaten >= saaten_vorher + 10,
        "die Nachsaaten wurden nicht gezaehlt"
    );
    assert!(s.gesaet, "der Generator hat durch Nachsaeen seinen Zustand verloren");
    serial_println!(
        "  10x nachgesaet im Betrieb: {} Nachsaaten gesamt, {} Byte ausgegeben.",
        s.nachsaaten,
        s.ausgegebene_bytes
    );
}

/// DURCHSATZ — ein Berichts-Test. TLS zieht bei jedem Handshake einige
/// Dutzend Byte; der Generator darf dabei nicht die Bremse sein.
#[test_case]
fn test_durchsatz_bericht() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    const BYTES: usize = 256 * 1024;
    let mut puffer = alloc::vec![0u8; 4096];
    let start = zeit::us_seit_boot();
    let mut gesamt = 0usize;
    while gesamt < BYTES {
        zufall::fuellen(&mut puffer).expect("gesaet");
        gesamt += puffer.len();
    }
    let dauer = zeit::us_seit_boot() - start;
    let mib_pro_s = (gesamt as u64) * 1_000_000 / dauer.max(1) / (1024 * 1024);

    // Und ein einzelner kleiner Aufruf (der TLS-Fall).
    let mut klein = [0u8; 32];
    let start_klein = zeit::us_seit_boot();
    for _ in 0..1000 {
        zufall::fuellen(&mut klein).expect("gesaet");
    }
    let ns_je_aufruf = (zeit::us_seit_boot() - start_klein) * 1_000 / 1000;

    serial_println!("  === LEISTUNG: Zufall ===");
    serial_println!("    {} KiB am Stueck: {} MiB/s", gesamt / 1024, mib_pro_s);
    serial_println!("    32 Byte einzeln:  {} ns je Aufruf", ns_je_aufruf);
    serial_println!(
        "    (ChaCha20 in Software, ohne SIMD — unser Target ist -sse/+soft-float.)"
    );
    assert!(gesamt >= BYTES);
}

// ===========================================================================
// 5. DER SYSCALL — echt aus Ring 3
// ===========================================================================

/// Der Syscall-Pruefstand: ein winziges Ring-3-Programm als Fernbedienung
/// (dasselbe Muster wie tests/syscalls.rs). Dadurch ist jeder Testfall
/// gewoehnlicher Rust-Code, waehrend der Aufruf ECHT unprivilegiert
/// stattfindet — mit eigenem Adressraum und echter Zeigerpruefung.
struct Pruefstand {
    pid: Pid,
}

const AUFTRAG_VA: u64 = prozess::ZAEHLER_CODE_VA + prozess::PRUEFSTAND_AUFTRAG_OFFSET;
const PUFFER_VA: u64 = prozess::ZAEHLER_CODE_VA + prozess::PRUEFSTAND_PUFFER_OFFSET;

impl Pruefstand {
    fn neu() -> Pruefstand {
        let prozess = prozess::pruefstand_prozess().expect("Pruefstand bauen");
        let pid = scheduler::einplanen(prozess).expect("Pruefstand einplanen");
        Pruefstand { pid }
    }

    fn feld_setzen(&self, offset: u64, wert: u64) {
        scheduler::mit_prozess_raum(self.pid, |raum| {
            raum.schreiben(VirtAddr::new(AUFTRAG_VA + offset), &wert.to_le_bytes())
        })
        .expect("Prozess existiert")
        .expect("Auftragsfeld schreiben");
    }

    fn feld_lesen(&self, offset: u64) -> u64 {
        let mut bytes = [0u8; 8];
        scheduler::mit_prozess_raum(self.pid, |raum| {
            raum.lesen(VirtAddr::new(AUFTRAG_VA + offset), &mut bytes)
        })
        .expect("Prozess existiert")
        .expect("Auftragsfeld lesen");
        u64::from_le_bytes(bytes)
    }

    fn hinlegen(&self, daten: &[u8]) {
        scheduler::mit_prozess_raum(self.pid, |raum| {
            raum.schreiben(VirtAddr::new(PUFFER_VA), daten)
        })
        .expect("Prozess existiert")
        .expect("in den Prozess schreiben");
    }

    fn abholen(&self, ziel: &mut [u8]) {
        scheduler::mit_prozess_raum(self.pid, |raum| raum.lesen(VirtAddr::new(PUFFER_VA), ziel))
            .expect("Prozess existiert")
            .expect("aus dem Prozess lesen");
    }

    /// Loest den Syscall aus und liefert `(rax, rdx)` — Fehlercode und
    /// Ergebnis, genau wie Ring 3 sie sieht.
    fn ruf(&self, nummer: u64, a0: u64, a1: u64) -> (u64, u64) {
        self.feld_setzen(prozess::PRUEFSTAND_NUMMER, nummer);
        self.feld_setzen(prozess::PRUEFSTAND_ARG0, a0);
        self.feld_setzen(prozess::PRUEFSTAND_ARG1, a1);
        self.feld_setzen(prozess::PRUEFSTAND_ARG2, 0);
        self.feld_setzen(prozess::PRUEFSTAND_ARG3, 0);
        self.feld_setzen(prozess::PRUEFSTAND_FEHLER, u64::MAX);
        self.feld_setzen(prozess::PRUEFSTAND_ERGEBNIS, u64::MAX);
        // Die Flagge ZULETZT — der Prozess darf nie einen halben Auftrag sehen.
        self.feld_setzen(prozess::PRUEFSTAND_FLAGGE, 1);

        let frist = zeit::ms_seit_boot() + 30_000;
        while self.feld_lesen(prozess::PRUEFSTAND_FLAGGE) != 0 {
            assert!(
                zeit::ms_seit_boot() < frist,
                "Syscall {} hat nicht geantwortet",
                nummer
            );
            zeit::warte_auf_interrupt();
        }
        (
            self.feld_lesen(prozess::PRUEFSTAND_FEHLER),
            self.feld_lesen(prozess::PRUEFSTAND_ERGEBNIS),
        )
    }
}

/// `zufall(ptr, len)` AUS RING 3 — der eigentliche Zweck der ganzen Uebung.
///
/// Geprueft wird nicht nur, DASS Bytes ankommen, sondern auch der ganze
/// Rand: Nulllaenge, Ueberlaenge, Kernel-Zeiger. Ein Zufalls-Syscall, der
/// bei einem kaputten Zeiger einen halb gefuellten Puffer hinterliesse,
/// waere besonders heimtueckisch — die Nullen darin saehen aus wie Zufall.
#[test_case]
fn test_syscall_zufall_aus_ring3() {
    assert!(warten_bis_gesaet(30_000), "nicht gesaet");
    let ps = Pruefstand::neu();

    // Ein erkennbares Muster hinlegen, damit „nicht beschrieben" auffaellt.
    let muster = [0xA5u8; 128];
    ps.hinlegen(&muster);

    // --- Der Erfolgsfall ---
    let (fehler, gefuellt) = ps.ruf(SYS_ZUFALL, PUFFER_VA, 64);
    assert_eq!(fehler, Fehler::Ok.code(), "zufall aus Ring 3 schlug fehl");
    assert_eq!(gefuellt, 64, "es wurden nicht 64 Byte gemeldet");

    let mut zurueck = [0u8; 128];
    ps.abholen(&mut zurueck);
    assert_ne!(&zurueck[..64], &muster[..64], "der Puffer wurde nicht beschrieben");
    assert!(
        zurueck[..64].iter().any(|b| *b != zurueck[0]),
        "alle 64 Byte sind gleich"
    );
    // GENAU 64 Byte — kein Byte darueber hinaus. Ein Ueberlauf in fremden
    // User-Speicher waere hier sichtbar.
    assert_eq!(
        &zurueck[64..128],
        &muster[64..128],
        "der Syscall hat ueber die angeforderte Laenge hinaus geschrieben"
    );

    // --- Zweimal hintereinander: verschiedene Bytes ---
    let erste = {
        let mut kopie = [0u8; 64];
        kopie.copy_from_slice(&zurueck[..64]);
        kopie
    };
    ps.ruf(SYS_ZUFALL, PUFFER_VA, 64);
    ps.abholen(&mut zurueck);
    assert_ne!(
        &zurueck[..64],
        &erste[..],
        "zwei Aufrufe aus Ring 3 liefern dieselben Bytes"
    );

    // --- Der Rand ---
    // Laenge 0 ist kein Fehler.
    assert_eq!(ps.ruf(SYS_ZUFALL, PUFFER_VA, 0), (Fehler::Ok.code(), 0));
    // Ueber dem Puffer-Deckel der ABI.
    let (fehler, _) = ps.ruf(SYS_ZUFALL, PUFFER_VA, 64 * 1024 + 1);
    assert_eq!(fehler, Fehler::ZuGross.code(), "der Laengen-Deckel fehlt");
    let (fehler, _) = ps.ruf(SYS_ZUFALL, PUFFER_VA, u64::MAX);
    assert_eq!(fehler, Fehler::ZuGross.code(), "u64::MAX als Laenge");
    // KERNEL-ADRESSE als Ziel (Dauerregel I): sauber abgelehnt.
    let (fehler, _) = ps.ruf(SYS_ZUFALL, speed_os::allocator::HEAP_START as u64, 32);
    assert_eq!(
        fehler,
        Fehler::UngueltigerZeiger.code(),
        "ein Kernel-Zeiger wurde NICHT abgelehnt"
    );
    // Nullzeiger.
    let (fehler, _) = ps.ruf(SYS_ZUFALL, 0, 32);
    assert_eq!(fehler, Fehler::UngueltigerZeiger.code(), "Nullzeiger");
    // Ueber die Seitengrenze des Prozess-Puffers hinaus.
    let (fehler, _) = ps.ruf(SYS_ZUFALL, PUFFER_VA, 8192);
    assert_eq!(
        fehler,
        Fehler::UngueltigerZeiger.code(),
        "ein Bereich ueber die gemappte Seite hinaus wurde nicht abgelehnt"
    );

    scheduler::beenden(ps.pid);
    scheduler::warten_auf(ps.pid, 10_000);
    scheduler::aufraeumen();
    serial_println!("  Syscall zufall(ptr,len) aus Ring 3: Erfolgsfall und alle Randfaelle. OK");
}
