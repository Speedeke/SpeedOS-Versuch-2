// boot/src/main.rs — Der SpeedOS-Runner (Nachfolger von bootimage)
//
// cargo ruft dieses Programm nach jedem Kernel-Build als "runner" auf
// und übergibt den Pfad zum frisch gebauten Kernel-ELF. Wir:
//   1. bauen daraus mit bootloader::BiosBoot ein bootfähiges
//      MBR-Disk-Image (landet neben dem Kernel im target-Ordner),
//   2. starten QEMU damit,
//   3. und übersetzen im Test-Modus den QEMU-Exit-Code für cargo
//      (33 = alle Tests grün -> 0, alles andere -> 1).
//
// Test-Erkennung wie bei bootimage: Test-Binaries legt cargo unter
// .../deps/ ab, das normale `cargo run`-Binary direkt im Profil-Ordner.

use std::path::{Path, PathBuf};
use std::process::{self, Child, Command};
use std::time::{Duration, Instant};

/// So lange darf ein Test-Kernel in QEMU laufen, bevor wir abbrechen.
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

fn main() {
    let kernel_pfad = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("Aufruf: boot <pfad-zum-kernel-elf> (macht cargo automatisch)"),
    );

    // Test-Binary? (liegt in .../deps/, cargo-run-Binary nicht)
    let test_modus = kernel_pfad
        .parent()
        .and_then(|p| p.file_name())
        .map(|name| name == "deps")
        .unwrap_or(false);

    // 0. Wunsch-Auflösung bestimmen (SPEEDOS_AUFLOESUNG, siehe
    //    aufloesung_waehlen) und daraus VRAM + Arbeitsspeicher ableiten.
    //
    //    Der Trick hinter der Auflösungswahl: Der Bootloader nimmt den
    //    GRÖSSTEN Grafikmodus, der seine Minimums erfüllt (.last() über
    //    die GOP-Modi), und die Firmware bietet nur Modi an, die ins
    //    VRAM passen. Also machen wir das VRAM (vgamem_mb, Zweierpotenz)
    //    gerade so groß, dass der Wunschmodus hineinpasst — dann IST
    //    der größte verfügbare Modus (fast) genau der Wunsch.
    let (breite, hoehe) = aufloesung_waehlen();
    // Obergrenze der QEMU-VGA-Firmware: Die edk2-Modustabelle endet
    // bei 4096x2160 (empirisch geprüft) — 5120x2880 existiert nicht
    // (Fallback auf winzige 1280x800), und 8K (VRAM 128 MiB) lässt
    // die Firmware komplett hängen. SpeedOS selbst ist auflösungs-
    // unabhängig; wir deckeln also EHRLICH mit einer Meldung, statt
    // den Nutzer in einen Hänger booten zu lassen.
    let (breite, hoehe) = if breite > 4096 || hoehe > 2160 {
        eprintln!(
            "[Runner] {breite}x{hoehe} liegt oberhalb der QEMU-VGA-Firmware \
             (max. 4096x2160) — nutze 4096x2160."
        );
        (4096, 2160)
    } else {
        (breite, hoehe)
    };
    let bedarf_bytes = breite * hoehe * 4;
    let mut vgamem_mb: u64 = 4;
    while vgamem_mb * 1024 * 1024 < bedarf_bytes {
        vgamem_mb *= 2;
    }
    let vgamem_mb = vgamem_mb.min(256);
    // Arbeitsspeicher passend zur Auflösung: Back-Buffer (4 B/Pixel)
    // plus die Fenster-Puffer-Reserve des Desktops (~9 B/Pixel) plus
    // Grundbedarf — großzügig auf 64er-Schritte gerundet. Obergrenze
    // 1 GiB: mehr verwaltet der Bitmap-Frame-Allocator nicht.
    let ram_mb = ((breite * hoehe * 20) / (1024 * 1024) + 96)
        .next_multiple_of(64)
        .clamp(128, 1024);

    // 1. Bootfähiges UEFI-Disk-Image (GPT) neben dem Kernel erzeugen.
    //    Über die BootConfig wünschen wir uns die gewählte Auflösung
    //    als Minimum — geht das nicht (krumme Custom-Auflösung), fällt
    //    der Bootloader dokumentiert auf einen anderen Modus zurück.
    //    Außerdem: Boot-Logging leise (nur Fehler), damit weder
    //    Framebuffer noch serielle Ausgabe vollgeschrieben werden.
    let mut boot_config = bootloader::BootConfig::default();
    boot_config.frame_buffer.minimum_framebuffer_width = Some(breite);
    boot_config.frame_buffer.minimum_framebuffer_height = Some(hoehe);
    boot_config.log_level = bootloader_boot_config::LevelFilter::Error;
    boot_config.frame_buffer_logging = false;

    let image_pfad = kernel_pfad.with_extension("img");
    bootloader::UefiBoot::new(&kernel_pfad)
        .set_boot_config(&boot_config)
        .create_disk_image(&image_pfad)
        .expect("Disk-Image konnte nicht erstellt werden");

    // 2. QEMU starten — mit UEFI-Firmware (edk2/OVMF, liegt bei QEMU
    //    dabei) als Flash-Speicher: Code readonly, NVRAM-Variablen als
    //    beschreibbare Kopie neben dem Image (jede VM braucht ihre eigene).
    let firmware = firmware_finden();
    let vars_vorlage = firmware.with_file_name("edk2-i386-vars.fd");
    let vars_pfad = kernel_pfad.with_extension("vars.fd");
    std::fs::copy(&vars_vorlage, &vars_pfad).expect("UEFI-Vars-Datei konnte nicht kopiert werden");

    let mut qemu = Command::new(qemu_finden());
    qemu.arg("-drive").arg(format!(
        "if=pflash,format=raw,readonly=on,file={}",
        firmware.display()
    ));
    qemu.arg("-drive")
        .arg(format!("if=pflash,format=raw,file={}", vars_pfad.display()));
    qemu.arg("-drive")
        .arg(format!("format=raw,file={}", image_pfad.display()));
    // Die DATEN-Platte: ein persistentes Roh-Image, das als Primary
    // SLAVE am emulierten IDE-Controller hängt (die Boot-Platte ist
    // Primary Master, index=0). Sie überlebt QEMU-Neustarts — das ist
    // der erste echte persistente Speicher von SpeedOS. Im Test-Modus
    // nehmen wir bewusst ein EIGENES Image, damit Testläufe nie die
    // Nutzerdaten der normalen Daten-Platte überschreiben.
    let daten_pfad = daten_image_sicherstellen(test_modus);
    qemu.arg("-drive").arg(format!(
        "format=raw,file={},if=ide,index=1",
        daten_pfad.display()
    ));
    // Hardware-Virtualisierung: WHPX (Windows Hypervisor Platform)
    // lässt den Kernel-Code DIREKT auf der CPU laufen statt ihn
    // Befehl für Befehl zu übersetzen (TCG) — der Unterschied ist
    // eine Größenordnung. Mehrere -accel werden der Reihe nach
    // probiert: Ist WHPX nicht verfügbar (Windows-Feature aus),
    // fällt QEMU sauber auf TCG zurück. kernel-irqchip=off, weil
    // unser PIC/PIT von QEMU emuliert werden soll (WHPX-Eigenheit).
    qemu.arg("-accel").arg("whpx,kernel-irqchip=off");
    qemu.arg("-accel").arg("tcg");
    // Arbeitsspeicher passend zur Auflösung (siehe Rechnung oben):
    qemu.arg("-m").arg(format!("{ram_mb}M"));
    // Die emulierte RTC läuft auf der LOKALEN Host-Uhr — die
    // Taskleisten-Uhr von SpeedOS zeigt damit dieselbe Zeit wie die
    // Windows-Uhr daneben (Standard wäre UTC).
    qemu.arg("-rtc").arg("base=localtime");
    // Grafikkarte: Das VRAM (vgamem_mb) ist der eigentliche
    // Auflösungs-Wähler — der EDID-Wunsch allein wird von OVMF
    // ignoriert (es nähme sonst immer den größten Modus überhaupt).
    // Der Kernel kommt mit JEDER Auflösung klar (720p bis 8K).
    qemu.arg("-vga").arg("none");
    qemu.arg("-device").arg(format!(
        "VGA,edid=on,xres={breite},yres={hoehe},vgamem_mb={vgamem_mb}"
    ));
    if !test_modus {
        eprintln!(
            "[Runner] Aufloesung {breite}x{hoehe} gewuenscht \
             (VRAM {vgamem_mb} MiB, RAM {ram_mb} MiB) — \
             andere Groesse: SPEEDOS_AUFLOESUNG=1080p|2k|4k|8k|BREITExHOEHE"
        );
    }
    // Die serielle Schnittstelle ist unser Debug-Lebensnerv:
    // immer ins Terminal spiegeln.
    qemu.arg("-serial").arg("stdio");
    if test_modus {
        // isa-debug-exit: Der Test-Kernel beendet QEMU mit Exit-Code
        // (wert << 1) | 1 — unser Erfolgswert 0x10 ergibt 33.
        qemu.arg("-device")
            .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
        qemu.arg("-display").arg("none");
        qemu.arg("-no-reboot");
    }

    let mut kind = qemu.spawn().expect(
        "QEMU konnte nicht gestartet werden — ist qemu-system-x86_64 \
         im PATH oder unter C:\\Program Files\\qemu installiert?",
    );

    // 3. Exit-Code auswerten:
    if test_modus {
        let code = warten_mit_timeout(&mut kind, TEST_TIMEOUT);
        match code {
            33 => process::exit(0), // Erfolg (0x10 << 1 | 1)
            35 => {
                eprintln!("Tests fehlgeschlagen (QEMU-Exit-Code 35).");
                process::exit(1);
            }
            andere => {
                eprintln!("Unerwarteter QEMU-Exit-Code: {}", andere);
                process::exit(1);
            }
        }
    } else {
        let status = kind.wait().expect("Warten auf QEMU fehlgeschlagen");
        process::exit(status.code().unwrap_or(0));
    }
}

/// Liest die Wunsch-Auflösung aus der Umgebungsvariablen
/// SPEEDOS_AUFLOESUNG. Verstanden werden die gängigen Namen
/// (720p/hd, 1080p/fhd, 1440p/2k/qhd, 2160p/4k/uhd, 2880p/5k,
/// 4320p/8k) sowie freie Angaben wie "1600x900". Ohne Variable:
/// 720p — die flotteste Wahl für die Emulation.
///
/// Hinweise: Die Firmware bietet eine feste Modus-Tabelle an; bei
/// Wünschen zwischen zwei Modi landet man auf dem nächstgrößeren
/// (z. B. 720p -> 1360x768, 2k -> 2560x1600); 1080p und 4k treffen
/// exakt. 5k/8k werden verstanden, aber auf das Firmware-Maximum
/// 4096x2160 gedeckelt (siehe Kommentar in main) — SpeedOS selbst
/// könnte sie darstellen.
fn aufloesung_waehlen() -> (u64, u64) {
    let wunsch = std::env::var("SPEEDOS_AUFLOESUNG").unwrap_or_default();
    let klein = wunsch.trim().to_ascii_lowercase();
    match klein.as_str() {
        "" | "720p" | "hd" => (1280, 720),
        "1080p" | "fhd" | "fullhd" => (1920, 1080),
        "1200p" | "wuxga" => (1920, 1200),
        "1440p" | "2k" | "qhd" | "wqhd" => (2560, 1440),
        "1600p" => (2560, 1600),
        "2160p" | "4k" | "uhd" => (3840, 2160),
        "2880p" | "5k" => (5120, 2880),
        "4320p" | "8k" => (7680, 4320),
        _ => {
            // Freie Angabe "BREITExHOEHE" (z. B. "1600x900"):
            if let Some((b, h)) = klein.split_once('x') {
                if let (Ok(b), Ok(h)) = (b.trim().parse::<u64>(), h.trim().parse::<u64>()) {
                    if (640..=4096).contains(&b) && (480..=2160).contains(&h) {
                        return (b, h);
                    }
                }
            }
            eprintln!(
                "[Runner] SPEEDOS_AUFLOESUNG '{wunsch}' nicht verstanden — nutze 720p. \
                 Presets: 720p, 1080p, 2k, 4k, 5k, 8k oder BREITExHOEHE."
            );
            (1280, 720)
        }
    }
}

/// Größe der Daten-Platte beim ersten Anlegen: 64 MiB (131072 Sektoren
/// zu 512 Bytes) — reichlich für die ersten Disk-Dateisystem-Schritte.
const DATEN_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Stellt sicher, dass das persistente Daten-Platten-Image existiert,
/// und liefert seinen Pfad. Liegt im Projekt-Root (dem Arbeits-
/// verzeichnis von cargo run), NICHT im target-Ordner — `cargo clean`
/// darf die Nutzerdaten nicht löschen. set_len füllt mit Nullen: wie
/// eine fabrikneue Platte, und NTFS legt das platzsparend (sparse) ab.
fn daten_image_sicherstellen(test_modus: bool) -> PathBuf {
    let pfad = PathBuf::from(if test_modus {
        "speedos-daten-test.img"
    } else {
        "speedos-daten.img"
    });
    if !pfad.exists() {
        let datei = std::fs::File::create(&pfad)
            .expect("Daten-Platten-Image konnte nicht angelegt werden");
        datei
            .set_len(DATEN_IMAGE_BYTES)
            .expect("Daten-Platten-Image konnte nicht auf Groesse gebracht werden");
        eprintln!(
            "[Runner] Neue Daten-Platte angelegt: {} ({} MiB)",
            pfad.display(),
            DATEN_IMAGE_BYTES / 1024 / 1024
        );
    }
    pfad
}

/// Wartet auf QEMU, bricht aber nach dem Timeout ab (hängender Test).
fn warten_mit_timeout(kind: &mut Child, timeout: Duration) -> i32 {
    let start = Instant::now();
    loop {
        match kind.try_wait().expect("try_wait fehlgeschlagen") {
            Some(status) => return status.code().unwrap_or(1),
            None if start.elapsed() > timeout => {
                let _ = kind.kill();
                eprintln!("Test-Timeout nach {} Sekunden!", timeout.as_secs());
                return 1;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Sucht die UEFI-Firmware (edk2/OVMF) im QEMU-Installationsordner.
fn firmware_finden() -> PathBuf {
    let standard = Path::new(r"C:\Program Files\qemu\share\edk2-x86_64-code.fd");
    if standard.exists() {
        return standard.to_path_buf();
    }
    panic!("UEFI-Firmware nicht gefunden (erwartet: {standard:?})");
}

/// Sucht qemu-system-x86_64: erst im PATH, dann am Standard-Installationsort.
fn qemu_finden() -> PathBuf {
    let standard = Path::new(r"C:\Program Files\qemu\qemu-system-x86_64.exe");
    // Erst den PATH probieren (funktioniert auch auf anderen Systemen):
    if Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .is_ok()
    {
        return PathBuf::from("qemu-system-x86_64");
    }
    if standard.exists() {
        return standard.to_path_buf();
    }
    panic!("qemu-system-x86_64 nicht gefunden (weder PATH noch {standard:?})");
}
