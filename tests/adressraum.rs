// tests/adressraum.rs — Der Adressraum-Beweis (Serie 6, Teil 2)
//
// Vier Dinge, die es in SpeedOS vorher NICHT gab:
//   1. ZWEI Adressräume, in beiden DIESELBE virtuelle Adresse gemappt, aber
//      mit unterschiedlichem Inhalt — nach dem CR3-Wechsel sieht man jeweils
//      den richtigen. Das IST Prozess-Isolation, sichtbar gemacht.
//   2. Ein Adressraum-Abriss gibt ALLE Frames zurück: frame_statistik()
//      vorher und nachher byte-exakt gleich (wie in den Speicher-Pässen der
//      Netz-Serie).
//   3. Ring-3-Code läuft in seinem EIGENEN Adressraum — Erfolg, Absturz und
//      Stack-Überlauf, und der Kernel überlebt alles drei.
//   4. Die Guard-Page unter dem User-Stack fängt einen Überlauf wirklich ab.
//
// Ein Hänger oder Triple Fault (kaputte Kernel-Spiegelung, freigegebene
// Tabellen unter den eigenen Füßen) würde den Test in den Timeout laufen
// lassen — Erfolg heißt hier also wirklich Erfolg.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::adressraum::{self, AdressRaum, AdressRaumFehler};
use speed_os::shell::befehle::{alle_befehle, ShellKontext};
use speed_os::shell::befehl_ausfuehren;
use speed_os::{allocator, memory, ring3, serial_println};
use x86_64::structures::paging::{Page, PageTableFlags};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    speed_os::zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");
    allocator::heap_erweitern(256).expect("Heap-Erweiterung fehlgeschlagen");
    speed_os::fs::init(); // die Shell-Registry braucht ein VFS

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Die gemeinsame Test-Adresse — 1 MiB in den User-Bereich hinein.
const PROBE_VA: u64 = adressraum::USER_START + 0x10_0000;

/// Liest 16 Byte über die VIRTUELLE Adresse, also durch die gerade aktiven
/// Page Tables. Genau das ist der Punkt: Dieselbe Zeile Code liefert je nach
/// CR3 einen anderen Inhalt.
fn probe_lesen() -> [u8; 16] {
    let mut ziel = [0u8; 16];
    // unsafe: Der Aufrufer stellt sicher, dass ein Adressraum aktiv ist, in
    // dem PROBE_VA gemappt ist. Ring 0 darf User-Seiten lesen (SMAP ist aus).
    unsafe {
        core::ptr::copy_nonoverlapping(PROBE_VA as *const u8, ziel.as_mut_ptr(), 16);
    }
    ziel
}

/// ===================== DER HAUPT-BEWEIS (Aufgabe 5) =====================
/// Zwei Adressräume, dieselbe virtuelle Adresse, unterschiedlicher Inhalt.
#[test_case]
fn test_zwei_adressraeume_gleiche_adresse_anderer_inhalt() {
    let mut a = AdressRaum::neu().expect("Adressraum A");
    let mut b = AdressRaum::neu().expect("Adressraum B");
    let page = Page::containing_address(VirtAddr::new(PROBE_VA));
    a.map_benutzer(page).expect("Seite in A");
    b.map_benutzer(page).expect("Seite in B");

    // Es sind wirklich VERSCHIEDENE physische Frames hinter derselben VA:
    assert_ne!(a.p4_frame(), b.p4_frame());

    a.schreiben(VirtAddr::new(PROBE_VA), b"AAAA-ich-bin-A--")
        .expect("A befuellen");
    b.schreiben(VirtAddr::new(PROBE_VA), b"BBBB-ich-bin-B--")
        .expect("B befuellen");

    // Im Kernel-Adressraum existiert die Adresse gar nicht.
    assert!(memory::seiten_flags(VirtAddr::new(PROBE_VA)).is_none());

    // Jetzt der eigentliche Beweis: identischer Code, anderes CR3.
    a.aktivieren();
    let gelesen_a = probe_lesen();
    b.aktivieren();
    let gelesen_b = probe_lesen();
    // Und zurück nach A — der Wechsel ist in beide Richtungen sauber.
    a.aktivieren();
    let gelesen_a2 = probe_lesen();
    adressraum::kernel_aktivieren();

    serial_println!(
        "[ADRESSRAUM-MEILENSTEIN] Adresse {:#x}: in A = {:?}, in B = {:?}",
        PROBE_VA,
        core::str::from_utf8(&gelesen_a).unwrap_or("?"),
        core::str::from_utf8(&gelesen_b).unwrap_or("?")
    );
    assert_eq!(&gelesen_a, b"AAAA-ich-bin-A--");
    assert_eq!(&gelesen_b, b"BBBB-ich-bin-B--");
    assert_eq!(&gelesen_a2, b"AAAA-ich-bin-A--");
    assert_ne!(gelesen_a, gelesen_b);

    a.abreissen();
    b.abreissen();
}

/// ===================== DER FRAME-BEWEIS (Aufgabe 5) =====================
/// Abreißen gibt ALLES zurück — byte-exakt derselbe Frame-Stand.
#[test_case]
fn test_abreissen_gibt_alle_frames_zurueck() {
    // Aufwärmrunde: Ein erster Durchlauf lässt eventuelle Heap-Nachschläge
    // (Vec-Wachstum) einmalig passieren — die verfälschen sonst die Bilanz.
    {
        let mut warm = AdressRaum::neu().expect("Aufwaermen");
        warm.bereich_mappen(VirtAddr::new(adressraum::USER_START), 16 * 4096)
            .expect("Aufwaermen mappen");
    }

    let (frei_vorher, gesamt) = memory::frame_statistik();

    let mut belegt_maximal = 0usize;
    for runde in 0..5 {
        let mut raum = AdressRaum::neu().expect("Adressraum");
        // Genug Seiten, dass mehrere Zwischentabellen entstehen (die zählen
        // auch zum Besitz und müssen mit zurück!).
        raum.bereich_mappen(VirtAddr::new(adressraum::USER_START), 40 * 4096)
            .expect("Bereich mappen");
        raum.stack_anlegen(VirtAddr::new(adressraum::USER_START + 0x40_0000), 8)
            .expect("Stack");
        // Auch ein AKTIVER Adressraum muss sich abreißen lassen (Drop schaltet
        // vorher auf den Kernel zurück).
        if runde % 2 == 0 {
            raum.aktivieren();
        }
        let (frei_mit_raum, _) = memory::frame_statistik();
        belegt_maximal = belegt_maximal.max(frei_vorher - frei_mit_raum);
        assert_eq!(
            frei_vorher - frei_mit_raum,
            raum.frames_besitz(),
            "Buchfuehrung stimmt nicht mit dem Allocator ueberein"
        );
        raum.abreissen();
        let (frei_danach, _) = memory::frame_statistik();
        assert_eq!(
            frei_vorher, frei_danach,
            "Runde {}: Adressraum hat Frames geleckt",
            runde
        );
    }

    let (frei_nachher, _) = memory::frame_statistik();
    serial_println!(
        "[ADRESSRAUM-MEILENSTEIN] 5x anlegen/abreissen: {} von {} Frames frei (vorher {}), \
         Spitzenbedarf {} Frames — Bilanz exakt null.",
        frei_nachher,
        gesamt,
        frei_vorher,
        belegt_maximal
    );
    assert_eq!(frei_vorher, frei_nachher);
    assert!(belegt_maximal > 40, "Es wurden gar keine Frames belegt?");
}

/// Der Kernel ist in jedem Adressraum gespiegelt — sonst wäre der CR3-Wechsel
/// ein sofortiger Triple Fault. Wir prüfen es an den Stellen, die WÄHREND des
/// User-Codes gebraucht werden: Heap, Physik-Mapping und der Kernel-Code selbst.
#[test_case]
fn test_kernel_ist_gespiegelt() {
    let raum = AdressRaum::neu().expect("Adressraum");
    // Irgendeine Kernel-Funktion dieses Test-Kernels als Code-Adresse.
    let kernel_code = probe_lesen as *const () as u64;
    for (name, va) in [
        ("Heap", allocator::HEAP_START as u64),
        ("Physik-Mapping", memory::phys_offset().as_u64()),
        ("Kernel-Code", kernel_code),
    ] {
        let flags = raum
            .seiten_flags(VirtAddr::new(va))
            .unwrap_or_else(|| panic!("{} fehlt im neuen Adressraum!", name));
        // Gespiegelt heisst NICHT user-zugaenglich: Ring 3 kommt trotzdem
        // nicht heran (das U-Bit fehlt) — es ist nur DA, damit der Kernel
        // nach einem Interrupt weiterlaufen kann.
        assert!(
            !flags.contains(PageTableFlags::USER_ACCESSIBLE),
            "{} ist im Prozess-Adressraum user-zugaenglich — Sicherheitsloch!",
            name
        );
    }
    // Der User-Bereich ist dagegen komplett leer.
    assert!(raum.seiten_flags(VirtAddr::new(PROBE_VA)).is_none());
}

/// Der Adressraum weist alles ab, was ihm nicht gehört.
#[test_case]
fn test_mapping_grenzen() {
    let mut raum = AdressRaum::neu().expect("Adressraum");
    // Kernel-Heap-Adresse: ausserhalb des User-Slots.
    assert_eq!(
        raum.map_benutzer(Page::containing_address(VirtAddr::new(
            allocator::HEAP_START as u64
        ))),
        Err(AdressRaumFehler::AusserhalbUserBereich)
    );
    // Nullseite: ausserhalb (Nullzeiger-Falle).
    assert_eq!(
        raum.map_benutzer(Page::containing_address(VirtAddr::new(0))),
        Err(AdressRaumFehler::AusserhalbUserBereich)
    );
    // Obere Hälfte: ausserhalb.
    assert_eq!(
        raum.map_benutzer(Page::containing_address(VirtAddr::new(
            0xffff_8000_0000_0000
        ))),
        Err(AdressRaumFehler::AusserhalbUserBereich)
    );
    // Doppelt mappen: abgelehnt (sonst würde der erste Frame lecken).
    let page = Page::containing_address(VirtAddr::new(PROBE_VA));
    raum.map_benutzer(page).expect("erstes Mapping");
    assert_eq!(raum.map_benutzer(page), Err(AdressRaumFehler::SchonGemappt));
}

/// Ring 3 im eigenen Adressraum: Erfolg (mit copy-in UND copy-out).
#[test_case]
fn test_ring3_erfolg_im_eigenen_adressraum() {
    serial_println!("[ADRESSRAUM-TEST] Ring-3-Lauf im eigenen Adressraum:");
    ring3::ring3_erfolg();
    // Nach der Rückkehr muss wieder der KERNEL-Adressraum aktiv sein.
    assert!(
        adressraum::aktiver_user_raum().is_none(),
        "Nach dem Prozess ist noch ein User-Adressraum aktiv!"
    );
    serial_println!("[ADRESSRAUM-TEST] Zurueck im Kernel-Adressraum.");
}

/// Ring 3 stürzt ab (verbotener Kernel-Zugriff) — der Kernel lebt weiter,
/// und der Adressraum wird trotzdem sauber zurückgeschaltet und abgerissen.
#[test_case]
fn test_ring3_x_absturz_raeumt_adressraum_auf() {
    let (frei_vorher, _) = memory::frame_statistik();
    serial_println!("[ADRESSRAUM-TEST] Absturz-Lauf (ein Page Fault ist HIER erwartet):");
    ring3::ring3_absturz();
    assert!(
        adressraum::aktiver_user_raum().is_none(),
        "Nach dem Absturz ist noch ein User-Adressraum aktiv!"
    );
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(
        frei_vorher, frei_nachher,
        "Der abgestuerzte Prozess hat Frames zurueckgelassen"
    );
    serial_println!(
        "[ADRESSRAUM-MEILENSTEIN] Absturz aufgefangen UND der Adressraum vollstaendig abgeraeumt."
    );
}

/// Die Guard-Page: Der Prozess pusht unter seinen Stack — Page Fault statt
/// stiller Speicherzerstörung. Danach läuft alles weiter.
#[test_case]
fn test_ring3_y_stack_ueberlauf_trifft_guard_page() {
    let (frei_vorher, _) = memory::frame_statistik();
    serial_println!("[ADRESSRAUM-TEST] Stack-Ueberlauf (ein Page Fault ist HIER erwartet):");
    ring3::ring3_stack_ueberlauf();
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(frei_vorher, frei_nachher);
    serial_println!("[ADRESSRAUM-MEILENSTEIN] Guard-Page hat den Stack-Ueberlauf gefangen.");

    // Und der Härtetest zum Schluss: Nach zwei Abstürzen muss ein normaler
    // Prozess immer noch fehlerfrei laufen (Tabellen, TSS, GDT alles heil).
    serial_println!("[ADRESSRAUM-TEST] Erneuter Ring-3-Lauf nach beiden Abstuerzen:");
    ring3::ring3_erfolg();
    serial_println!("[ADRESSRAUM-MEILENSTEIN] Auch nach zwei Abstuerzen laeuft Ring 3 fehlerfrei.");
}

/// Und zum Schluss so, wie ein Nutzer es tippt: die Befehle durch die echte
/// Shell-Registry — inklusive Frame-Bilanz über die ganze Sitzung.
#[test_case]
fn test_z_shell_befehle_end_to_end() {
    let registry = alle_befehle();
    let mut ctx = ShellKontext::neu();
    let (frei_vorher, _) = memory::frame_statistik();
    for zeile in [
        "adressraum",
        "ring3test",
        "ring3test absturz",
        "ring3test stack",
    ] {
        serial_println!("\n----- SpeedOS:/> {} -----", zeile);
        befehl_ausfuehren(&registry, &mut ctx, zeile);
    }
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(
        frei_vorher, frei_nachher,
        "Die Shell-Sitzung hat Frames zurueckgelassen"
    );
    serial_println!("\n[ADRESSRAUM-MEILENSTEIN] Ganze Shell-Sitzung: Frame-Bilanz exakt null.");
}
