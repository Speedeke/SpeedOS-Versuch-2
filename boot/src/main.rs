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

    // 1. Bootfähiges UEFI-Disk-Image (GPT) neben dem Kernel erzeugen.
    //    Über die BootConfig wünschen wir uns einen Framebuffer mit
    //    mindestens 1280x720 — geht das nicht, fällt der Bootloader
    //    dokumentiert auf einen kleineren Modus zurück. Außerdem:
    //    Boot-Logging leise (nur Fehler), damit weder Framebuffer
    //    noch serielle Ausgabe vollgeschrieben werden.
    let mut boot_config = bootloader::BootConfig::default();
    boot_config.frame_buffer.minimum_framebuffer_width = Some(1280);
    boot_config.frame_buffer.minimum_framebuffer_height = Some(720);
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
    // Hardware-Virtualisierung: WHPX (Windows Hypervisor Platform)
    // lässt den Kernel-Code DIREKT auf der CPU laufen statt ihn
    // Befehl für Befehl zu übersetzen (TCG) — der Unterschied ist
    // eine Größenordnung. Mehrere -accel werden der Reihe nach
    // probiert: Ist WHPX nicht verfügbar (Windows-Feature aus),
    // fällt QEMU sauber auf TCG zurück. kernel-irqchip=off, weil
    // unser PIC/PIT von QEMU emuliert werden soll (WHPX-Eigenheit).
    qemu.arg("-accel").arg("whpx,kernel-irqchip=off");
    qemu.arg("-accel").arg("tcg");
    // Grafikkarte mit Wunsch-Auflösung 1280x720 (per EDID). Der
    // EDID-Wunsch allein reicht nicht — OVMF wählt sonst trotzdem
    // den größten Modus (2560x1600 = 4x so viele Pixel pro Frame!).
    // Deshalb zusätzlich vgamem_mb=4: Mit 4 MiB VRAM passen nur
    // Modi bis 1280x720 (3,5 MiB) — die Firmware MUSS klein wählen.
    // (Der Kernel kommt trotzdem mit jeder Auflösung klar.)
    qemu.arg("-vga").arg("none");
    qemu.arg("-device")
        .arg("VGA,edid=on,xres=1280,yres=720,vgamem_mb=4");
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
