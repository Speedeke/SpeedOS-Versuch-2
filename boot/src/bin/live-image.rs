// boot/src/bin/live-image.rs — Baut das USB-LIVE-Image von SpeedOS
//
// Aufruf am bequemsten über den Alias `cargo image` (siehe
// .cargo/config.toml), optional `cargo image --release`.
//
// Anders als der normale Runner (boot/src/main.rs) startet dieses
// Programm KEIN QEMU und hängt KEINE Daten-/FAT-Platte an: Es erzeugt
// nur die Datei `speedos-live.img` im Projekt-Root — ein bootfähiges
// GPT-Image mit EFI-System-Partition (UEFI-Boot über bootloader 0.11),
// das man mit Rufus & Co. auf einen USB-Stick schreibt und dann auf
// echter Hardware bootet. Die Schreib-Anleitung steht in
// docs/usb-boot.md.
//
// Ablauf:
//   1. Den Kernel bauen (cargo build für das eingebaute Bare-Metal-
//      Target x86_64-unknown-none — die .cargo/config.toml des Roots
//      liefert es automatisch).
//   2. Den frischen Kernel-ELF finden.
//   3. Mit bootloader::UefiBoot das Live-Image bauen — BEWUSST OHNE
//      erzwungene Mindestauflösung, damit die Firmware auf echter
//      Hardware ihren nativen GOP-Modus nimmt (der Kernel ist
//      auflösungsunabhängig und skaliert HiDPI automatisch).

use std::path::{Path, PathBuf};
use std::process::{self, Command};

fn main() {
    // --release baut die kleinere/schnellere Variante; Standard ist
    // dev (opt-level 2 laut Cargo.toml) — exakt das, was seit Monaten
    // in QEMU bootet, also die risikoärmste Wahl für den Erst-Boot auf
    // fremder Hardware.
    let release = std::env::args().any(|a| a == "--release" || a == "-r");
    let profil = if release { "release" } else { "debug" };

    // 1. Kernel bauen. CWD ist der Projekt-Root (dorther ruft man
    //    `cargo image`), also greift die Root-.cargo/config.toml mit
    //    dem Bare-Metal-Target. Wir bauen gezielt das Kernel-Paket.
    eprintln!("[live-image] Baue Kernel ({profil}) ...");
    let mut build = Command::new("cargo");
    build.arg("build").arg("-p").arg("speed_os");
    if release {
        build.arg("--release");
    }
    let status = build
        .status()
        .expect("`cargo build` fuer den Kernel konnte nicht gestartet werden");
    if !status.success() {
        eprintln!("[live-image] Kernel-Build fehlgeschlagen — Abbruch.");
        process::exit(1);
    }

    // 2. Den Kernel-ELF finden (Bare-Metal-Target, keine Dateiendung).
    let kernel_pfad = PathBuf::from(format!(
        "target/x86_64-unknown-none/{profil}/speed_os"
    ));
    if !kernel_pfad.exists() {
        eprintln!(
            "[live-image] Kernel-ELF nicht gefunden: {} — wird `cargo image` \
             aus dem Projekt-Root aufgerufen?",
            kernel_pfad.display()
        );
        process::exit(1);
    }

    // 3. Live-Image bauen. KEINE Mindestauflösung (Unterschied zum
    //    Runner!) — die Firmware liefert den nativen Modus des echten
    //    Bildschirms. Boot-Logging leise, damit weder Framebuffer noch
    //    serielle Ausgabe vom Bootloader vollgeschrieben werden.
    let mut boot_config = bootloader::BootConfig::default();
    boot_config.log_level = bootloader_boot_config::LevelFilter::Error;
    boot_config.frame_buffer_logging = false;

    let image_pfad = PathBuf::from("speedos-live.img");
    eprintln!("[live-image] Baue UEFI-Live-Image (GPT + EFI-System-Partition) ...");
    bootloader::UefiBoot::new(&kernel_pfad)
        .set_boot_config(&boot_config)
        .create_disk_image(&image_pfad)
        .expect("Live-Image konnte nicht erstellt werden");

    let groesse_mib = std::fs::metadata(&image_pfad)
        .map(|m| m.len() / 1024 / 1024)
        .unwrap_or(0);

    eprintln!();
    eprintln!(
        "[live-image] Fertig: {} ({} MiB, Profil {}).",
        Path::new(&image_pfad).display(),
        groesse_mib,
        profil
    );
    eprintln!("[live-image] Auf einen USB-Stick schreiben und booten:");
    eprintln!("[live-image]   -> Anleitung in docs/usb-boot.md");
    eprintln!("[live-image]   -> Vorher in QEMU testen: tools/live_qemu.ps1");
}
