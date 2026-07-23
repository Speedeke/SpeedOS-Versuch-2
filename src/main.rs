// main.rs — Einstiegspunkt des SpeedOS-Kernels
//
// Diese Datei ist das, was nach dem Bootloader als Erstes läuft.
// Es gibt kein Betriebssystem unter uns — nur wir und die CPU.
//
// Seit der Migration auf bootloader 0.11 booten wir in einen
// GRAFIKMODUS: Der Bootloader richtet per VESA einen linearen
// Framebuffer ein und übergibt ihn uns in der BootInfo. Text läuft
// übergangsweise nur über die serielle Schnittstelle (siehe konsole.rs),
// bis der Framebuffer-Text-Renderer gebaut ist.
//
// Ablauf beim Booten:
//   BIOS -> bootloader (Stage 1-4) -> kernel_main (hier!)
//   -> CPU-Strukturen (GDT/IDT/PIC) -> Speicher -> Heap -> Dateisystem
//   -> Framebuffer-Demo -> Executor startet die SpeedShell (seriell)

#![no_std] // Keine Standardbibliothek — es gibt ja noch kein OS, das sie tragen könnte
#![no_main] // Kein normales main(): Der Bootloader springt direkt zu unserem Entry Point
#![feature(custom_test_frameworks)] // Eigenes Test-Framework (siehe lib.rs)
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::task::{executor::Executor, Task, TaskArt};
use speed_os::{allocator, framebuffer, konsole, memory, serial_println, shell};
use x86_64::VirtAddr;

// Das entry_point!-Makro erzeugt die echte _start-Funktion und prüft
// die Signatur zur Compile-Zeit; die Config wird in eine spezielle
// ELF-Sektion serialisiert, aus der der Bootloader sie liest.
// (Die Framebuffer-Mindestauflösung 1280x720 wird seit bootloader
// 0.11.x NICHT hier, sondern beim Image-Bau konfiguriert — siehe
// boot/src/main.rs, BootConfig.)
entry_point!(kernel_main, config = &speed_os::BOOTLOADER_CONFIG);

/// Der eigentliche Kernel-Einstieg. Rückgabetyp `!`: kehrt nie zurück.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // 1. CPU-Strukturen: GDT, TSS, IDT laden, PIC scharf schalten.
    speed_os::init();
    serial_println!("[BOOT] SpeedOS startet (bootloader_api 0.11) ...");

    // 1b. Die Zeit aufsetzen (braucht den tickenden PIT aus init):
    //     TSC gegen den PIT kalibrieren (~200 ms, loggt Frequenz und
    //     Genauigkeit) und die echte Uhrzeit einmal aus der RTC lesen.
    speed_os::zeit::init();

    // 2. Den Framebuffer aus der BootInfo HERAUSNEHMEN (take), bevor
    //    wir die BootInfo zu &'static abwerten — sonst Borrow-Konflikt.
    let framebuffer = boot_info.framebuffer.take();
    let boot_info: &'static BootInfo = boot_info;

    // Diagnose-Signal des Runners: SPEEDOS_DIAGNOSE=1 lässt den
    // Bootloader eine winzige Ramdisk anhängen — wir erkennen sie am
    // gesetzten ramdisk_addr. Reines Lesen, noch kein Heap nötig.
    // (Auf echter Hardware wird der Diagnose-Modus stattdessen mit der
    // Taste D beim Boot ausgelöst — siehe Bootscreen-Schleife unten.)
    let diagnose_ramdisk = boot_info.ramdisk_addr.into_option().is_some();

    // 3. Speicherverwaltung: globaler Mapper + Bitmap-Frame-Allocator,
    //    dann Heap. Danach funktionieren Box, Vec, String & Co.
    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("Bootloader hat kein Physik-Mapping angelegt");
    memory::init(VirtAddr::new(phys_mem_offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    // Scancode-Queue FRÜH bereitstellen (braucht den Heap), damit die
    // Tastendrücke auf dem Bootscreen nicht verloren gehen — sonst
    // könnten wir die D-Taste für den Diagnose-Modus nicht abfangen.
    speed_os::task::keyboard::queue_bereitstellen();

    // 4. Grafik: Doppel-Puffer + Text-Konsole auf dem Framebuffer,
    //    dann der Boot-Screen (Obsidian-Aurora, ~1,5 Sekunden).
    let mut d_beim_boot = false;
    match framebuffer {
        Some(fb) => {
            let info = fb.info();
            serial_println!(
                "[FB] Linearer Framebuffer: {}x{} Pixel, Format {:?}, {} Bytes/Pixel, Stride {} Pixel, Puffer {} KiB",
                info.width,
                info.height,
                info.pixel_format,
                info.bytes_per_pixel,
                info.stride,
                info.byte_len / 1024
            );
            framebuffer::init(fb);
            konsole::init();
            // Bootscreen malen und ~1,5 s verweilen — dabei lauschen wir
            // auf die Taste D (Diagnose-Modus auf echter Hardware, wo es
            // keine serielle Ausgabe gibt). Das Verweilen bleibt gleich
            // lang; bei D brechen wir früher ab.
            framebuffer::bootscreen_malen();
            let start = speed_os::zeit::ms_seit_boot();
            while speed_os::zeit::ms_seit_boot() < start + 1500 {
                x86_64::instructions::hlt();
                if speed_os::task::keyboard::diagnose_taste_beim_boot() {
                    d_beim_boot = true;
                    break;
                }
            }
            konsole::clear_screen();
        }
        None => serial_println!("[FB] WARNUNG: Kein Framebuffer — Ausgabe nur seriell!"),
    }

    // Diagnose-Modus scharf schalten, wenn die Taste D gedrückt wurde
    // ODER der Runner die Diagnose-Ramdisk angehängt hat. Ab jetzt
    // schreiben die diagnose::schritt-Aufrufe die Boot-Schritte auch
    // auf den Bildschirm (sonst wie bisher nur seriell).
    if d_beim_boot || diagnose_ramdisk {
        speed_os::diagnose::aktivieren();
        speed_os::diagnose::schritt(format_args!("=== SpeedOS Live - Diagnose-Modus ==="));
        speed_os::diagnose::schritt(format_args!(
            "Ausgeloest durch {}.",
            if d_beim_boot { "Taste D" } else { "SPEEDOS_DIAGNOSE" }
        ));
    }

    // 5. Massenspeicher: die ATA-Laufwerke erkennen (IDENTIFY loggt
    //    Modell und Größe seriell). Braucht die TSC-Zeit aus 1b für
    //    seine Polling-Timeouts. Die Boot-Platte ist per Konstruktion
    //    schreibgeschützt; beschreibbar ist nur die Daten-Platte.
    speed_os::diagnose::schritt(format_args!("[1/4] ATA-Laufwerke erkennen ..."));
    speed_os::ata::init();

    //    Der Heap wächst später in desktop_starten auf Bildschirm-
    //    größe — aber die Auto-Mounts hier davor brauchen schon Luft
    //    (der FAT32-Treiber liest die ganze FAT in den RAM, ~256 KiB;
    //    dazu die SpeedFS-Block-Caches). 1 MiB vorab reicht bequem —
    //    und der virtio-blk-Treiber braucht den Heap für seine
    //    Virtqueue-Verwaltung, also VOR pci/virtio wachsen lassen.
    allocator::heap_erweitern(256).expect("Heap-Erweiterung vor den Mounts fehlgeschlagen");

    //    PCI enumerieren und den virtio-blk-Treiber aufsetzen (falls
    //    QEMU die Daten-Platte per virtio statt IDE anbietet). Danach
    //    entscheidet fs::daten_geraet(), welches Backend /platte trägt.
    speed_os::diagnose::schritt(format_args!("[2/4] PCI-Bus + virtio-blk + virtio-net ..."));
    speed_os::pci::init();
    speed_os::virtio::blk::init();
    // virtio-net (Serie 5): NIC finden, RX+TX aufsetzen und als
    // NetzGeraet in der Netz-Schicht REGISTRIEREN (Ethernet/ARP/IPv4/UDP
    // reden ab jetzt nur mit dem Trait). Kein Gerät -> stille Rückkehr.
    speed_os::virtio::net::init();
    // DHCP: sich beim Boot automatisch eine IP holen (IP/Maske/Gateway/DNS).
    // Timeout 3 s -> Fallback auf statische Konfiguration per 'netz-ip'.
    // Pumpt den Empfang synchron; auf QEMU-slirp antwortet der Server sofort.
    speed_os::netz::dhcp::autokonfig(3000);

    //    Dateisystem: RamFs als Wurzel mounten (mit Demo-Dateien),
    //    dann die Daten-Platte AUTO-MOUNTEN (SpeedFS auf /platte;
    //    unformatiert -> nur Hinweis, NIE Auto-Format) und ERST
    //    DANACH die Einstellungen laden — sie wohnen jetzt auf der
    //    Platte und überleben so den Neustart (Theme, Skala, Uhr).
    speed_os::diagnose::schritt(format_args!("[3/4] Dateisystem + Auto-Mounts ..."));
    speed_os::fs::init();
    speed_os::fs::platte_automounten();
    // Das FAT-Laufwerk ("der USB-Stick") NUR LESEND unter /fat
    // mounten, wenn eines angeschlossen ist — nach der Daten-Platte,
    // damit die Boot-Meldungen in der richtigen Reihenfolge stehen.
    speed_os::fs::fat_automounten();
    speed_os::diagnose::schritt(format_args!("[4/4] Einstellungen laden ..."));
    speed_os::einstellungen::laden();

    serial_println!("[BOOT] GDT/IDT/PIC, Speicher, Heap, Grafik und RamFs initialisiert.");

    // Im Testmodus (cargo test) stattdessen die Tests ausführen.
    #[cfg(test)]
    test_main();

    // Diagnose-Abschluss ODER Kein-Eingabe-Meldung — BEVOR der Desktop
    // den Bildschirm übernimmt:
    if speed_os::diagnose::aktiv() {
        // Diagnose: die erkannte Hardware zusammenfassen und kurz
        // verweilen (Taste zum Fortfahren, sonst ~8 s Timeout).
        speed_os::diagnose::hardware_zusammenfassung();
        speed_os::diagnose::schritt(format_args!(
            "Weiter zum Desktop - Taste druecken oder ~8 s warten ..."
        ));
        let start = speed_os::zeit::ms_seit_boot();
        while speed_os::zeit::ms_seit_boot() < start + 8000 {
            x86_64::instructions::hlt();
            if speed_os::task::keyboard::boot_taste_vorhanden() {
                break;
            }
        }
    } else if !speed_os::diagnose::tastatur_vorhanden() {
        // Keine PS/2-Tastatur erkannt: eine KLARE Meldung auf den
        // Bildschirm (auf echter Hardware gibt es keine serielle
        // Ausgabe!), statt still zu hängen. Der Desktop startet danach
        // trotzdem — mit Maus (falls vorhanden) bleibt er bedienbar.
        speed_os::framebuffer::meldung_zeigen(
            &[
                "SpeedOS Live: keine PS/2-Eingabe gefunden",
                "USB-Eingabe kommt in einer kuenftigen Version.",
                "",
                "Der Desktop startet trotzdem.",
            ],
            6000,
        );
    }

    // 6. Direkt in den DESKTOP booten: FensterManager + Terminal-
    //    Fenster stehen bereit, BEVOR der erste Task läuft — die
    //    SpeedShell druckt ihr Banner dann gleich ins Terminal-Fenster
    //    (konsole::_print leitet im Desktop-Modus dorthin um).
    //    ESC führt weiterhin zurück zur Vollbild-Konsole.
    speed_os::fenster::desktop_starten();

    // 7. Der Executor übernimmt als Hauptschleife: SpeedShell,
    //    Compositor, Maus & Co. laufen als kooperative Tasks.
    //    Jeder Task bekommt einen NAMEN (und die Fenster-Welt ihre
    //    Art) — so sind sie im Task-Manager erkennbar.
    //    SEIT DEN TERMINAL-SITZUNGEN: Der Eingabe-Router ist der
    //    einzige KeyStream-Leser; pro Terminal-Fenster läuft ein
    //    eigener Shell-Task. desktop_starten hat das Boot-Terminal
    //    samt Sitzung angelegt (= Haupt-Terminal); ohne Desktop
    //    (kein Framebuffer) bekommt die Vollbild-Konsole eine
    //    fensterlose Haupt-Sitzung.
    let boot_sitzung = {
        let haupt = shell::sitzung::haupt();
        if haupt != 0 {
            haupt
        } else {
            let id = shell::sitzung::neu_registrieren();
            shell::sitzung::haupt_setzen(id);
            id
        }
    };
    let mut executor = Executor::new();
    executor.spawn(Task::new("Eingabe-Router", shell::eingabe_router()));
    executor.spawn(
        Task::new(
            alloc::format!("Shell-Sitzung {}", boot_sitzung),
            shell::sitzung_laufen(boot_sitzung, true),
        )
        .mit_art(TaskArt::Fenster),
    );
    executor.spawn(Task::new("Konsolen-Cursor", konsole::cursor_blink_task()));
    executor.spawn(Task::new("Log-Schreiber", speed_os::protokoll::log_task()));
    executor.spawn(Task::new("PS/2-Maus", speed_os::maus::maus_task()));
    executor.spawn(Task::new("Netz-Dispatch", speed_os::netz::netz_task()));
    executor.spawn(
        Task::new("Compositor", speed_os::fenster::compositor_task()).mit_art(TaskArt::Fenster),
    );
    executor.spawn(
        Task::new("Desktop-Uhr", speed_os::fenster::uhr_task()).mit_art(TaskArt::Fenster),
    );
    executor.run();
}

/// Panic-Handler für den normalen Betrieb: ZUERST roh seriell melden
/// (nimmt nur den SERIAL-Lock — funktioniert selbst dann, wenn die
/// Panik unter dem MANAGER-Lock passierte, wo die Terminal-Umleitung
/// von println! deadlocken würde), dann Versuch auf den Bildschirm.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use speed_os::konsole::{self, Color};

    speed_os::serial_println!("\x1b[91mKERNEL PANIC: {}\x1b[0m", info);
    konsole::set_color(Color::LightRed, Color::Black);
    speed_os::println!("KERNEL PANIC: {}", info);
    speed_os::hlt_loop();
}

/// Panic-Handler im Testmodus: an das Test-Framework weiterreichen.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
