// diagnose.rs — Boot-Diagnose und Hardware-Erkennungs-Status
//
// Auf echter Hardware gibt es KEINE serielle Schnittstelle, über die
// man beim Boot zuschauen könnte (in QEMU ist COM1 unser Lebensnerv,
// am echten Laptop hängt da nichts). Deshalb dieser Diagnose-Modus:
// Ist er aktiv, werden die Boot-Schritte und die erkannte Hardware
// AUF DEN BILDSCHIRM geschrieben statt nur seriell — man sieht also
// mit eigenen Augen, wie weit der Kernel kommt und was er findet.
//
// Ausgelöst wird er auf zwei Wegen (beide setzen AKTIV):
//   1. Taste D beim Boot  (echte Hardware — man drückt D auf dem
//      Aurora-Bootscreen; main.rs prüft die Tastatur-Queue).
//   2. SPEEDOS_DIAGNOSE=1  (Komfort in QEMU — der Runner hängt eine
//      1-Byte-Ramdisk an, der Kernel erkennt sie an boot_info.ramdisk).
//
// Dieses Modul ist bewusst ein QUERSCHNITT (wie protokoll.rs): Es hält
// nur den Zustand (drei Atomics) und die Ausgabe-Weiche. Die einzelnen
// Subsysteme melden ihre Funde über bereits vorhandene Abfrage-APIs;
// hardware_zusammenfassung() setzt daraus das Bild zusammen.

use core::sync::atomic::{AtomicBool, Ordering};

/// Ist der Diagnose-Modus aktiv? (D beim Boot oder SPEEDOS_DIAGNOSE.)
static AKTIV: AtomicBool = AtomicBool::new(false);

/// Wurde beim Boot eine PS/2-Tastatur erkannt? Standard: ja — die
/// nicht-intrusive Probe in lib::init() überschreibt das explizit.
static TASTATUR_DA: AtomicBool = AtomicBool::new(true);

/// Wurde beim Boot eine PS/2-Maus erkannt? (maus::initialisieren.)
static MAUS_DA: AtomicBool = AtomicBool::new(true);

/// Schaltet den Diagnose-Modus scharf (idempotent).
pub fn aktivieren() {
    AKTIV.store(true, Ordering::Relaxed);
}

/// Läuft der Diagnose-Modus?
pub fn aktiv() -> bool {
    AKTIV.load(Ordering::Relaxed)
}

/// Merkt sich das Ergebnis der Tastatur-Erkennung (lib::init()).
pub fn tastatur_setzen(vorhanden: bool) {
    TASTATUR_DA.store(vorhanden, Ordering::Relaxed);
}

/// Wurde eine PS/2-Tastatur erkannt?
pub fn tastatur_vorhanden() -> bool {
    TASTATUR_DA.load(Ordering::Relaxed)
}

/// Merkt sich das Ergebnis der Maus-Erkennung (lib::init()).
pub fn maus_setzen(vorhanden: bool) {
    MAUS_DA.store(vorhanden, Ordering::Relaxed);
}

/// Wurde eine PS/2-Maus erkannt?
pub fn maus_vorhanden() -> bool {
    MAUS_DA.load(Ordering::Relaxed)
}

/// Die Ausgabe-Weiche für Boot-Schritte. IMMER seriell (dort ändert
/// sich nichts gegenüber früher). Ist der Diagnose-Modus aktiv UND
/// steht der Framebuffer schon UND läuft noch kein Desktop, dann
/// ZUSÄTZLICH auf den Bildschirm (println! = konsole::_print, also
/// Bildschirm + seriell + Protokoll in einem).
///
/// Warum die Desktop-Prüfung? Sobald der Compositor läuft, würde
/// blanker Text den Bildschirm zerstören — Diagnose ist eine reine
/// VOR-Desktop-Angelegenheit.
pub fn schritt(args: core::fmt::Arguments) {
    if aktiv() && crate::framebuffer::ist_initialisiert() && !crate::fenster::desktop_aktiv() {
        crate::println!("{}", args);
    } else {
        crate::serial_println!("{}", args);
    }
}

/// Bequemes Makro für Boot-Schritte: `diagnose_schritt!("[X] ...", ..)`.
/// Landet immer seriell, im Diagnose-Modus zusätzlich auf dem Schirm.
#[macro_export]
macro_rules! diagnose_schritt {
    ($($arg:tt)*) => ($crate::diagnose::schritt(format_args!($($arg)*)));
}

/// Zeigt die erkannte Hardware am Ende der Boot-Sequenz an (nur im
/// Diagnose-Modus sichtbar, weil schritt() dann auf den Schirm geht).
/// Fragt ausschließlich vorhandene Abfrage-APIs ab — kein neuer
/// Zustand, keine Kopplung an die Init-Reihenfolge.
///
/// Braucht den Heap (format!) — wird erst spät im Boot gerufen.
pub fn hardware_zusammenfassung() {
    use crate::fs::block::BlockDevice;

    schritt(format_args!(""));
    schritt(format_args!("=== Erkannte Hardware ==="));

    // Bildschirm/Framebuffer:
    match crate::framebuffer::mit_framebuffer(|fb| fb.info()) {
        Some(info) => schritt(format_args!(
            "  Bildschirm : {}x{} Pixel, {:?}, {} B/Pixel",
            info.width, info.height, info.pixel_format, info.bytes_per_pixel
        )),
        None => schritt(format_args!("  Bildschirm : keiner (nur seriell)")),
    }

    // Eingabegeräte:
    schritt(format_args!(
        "  Tastatur   : {}",
        if tastatur_vorhanden() { "PS/2 erkannt" } else { "NICHT erkannt (USB folgt in Serie 6+)" }
    ));
    schritt(format_args!(
        "  Maus       : {}",
        if maus_vorhanden() { "PS/2 erkannt" } else { "NICHT erkannt (Desktop laeuft per Tastatur)" }
    ));

    // Massenspeicher: virtio-blk hat Vorrang, dann die ATA-Laufwerke.
    if crate::virtio::blk::daten_platte().is_some() {
        schritt(format_args!("  Platte     : virtio-blk (para-virtualisiert)"));
    }
    crate::ata::mit_laufwerken(|laufwerke| {
        if laufwerke.is_empty() {
            schritt(format_args!("  ATA        : keine Laufwerke"));
        } else {
            for lw in laufwerke.iter_mut() {
                let mib = lw.anzahl_sektoren() * lw.sektor_groesse() as u64 / 1024 / 1024;
                schritt(format_args!(
                    "  ATA {:<6}: {} ({} MiB, {})",
                    lw.rolle(),
                    lw.modell(),
                    mib,
                    if lw.ist_beschreibbar() { "beschreibbar" } else { "schreibgeschuetzt" }
                ));
            }
        }
    });

    // Gemountete Dateisysteme (leer = reines RAM-VFS, kein Fehler):
    let mounts = crate::fs::mount_uebersicht();
    if mounts.is_empty() {
        schritt(format_args!("  Dateisystem: nur RAM (keine Platte gemountet)"));
    } else {
        for m in &mounts {
            schritt(format_args!(
                "  Mount {:<6}: {} ({})",
                m.praefix,
                m.typ,
                if m.beschreibbar { "rw" } else { "ro" }
            ));
        }
    }
    // DIE UHR (Serie 7, Teil 2) — auf ECHTER Hardware die einzige Stelle,
    // an der sich nachsehen laesst, was die CMOS-Uhr wirklich liefert:
    // Es gibt dort keine serielle Ausgabe, nur diesen Bildschirm.
    //
    // WAS MAN DAMIT PRUEFT: Steht unter "RTC roh" die UTC-Zeit oder die
    // Ortszeit? Ist es die Ortszeit, gehoert die RTC-Zone in den
    // Einstellungen auf den eigenen Versatz (Windows stellt die Uhr auf
    // Lokalzeit, Linux auf UTC). Erst dann stimmt "UTC" darunter — und
    // erst dann taugt die Uhr fuer Zertifikate.
    let utc = crate::zeit::jetzt();
    let roh = crate::zeit::datum_von_sekunden_seit_2000(
        crate::zeit::sekunden_seit_2000(&utc)
            .saturating_add((crate::zeit::rtc_zone_min().max(0) as u64) * 60),
    );
    schritt(format_args!(
        "  RTC roh    : {:02}.{:02}.{} {:02}:{:02}:{:02}  (RTC-Zone {:+} min)",
        roh.tag, roh.monat, roh.jahr, roh.stunde, roh.minute, roh.sekunde,
        crate::zeit::rtc_zone_min()
    ));
    schritt(format_args!(
        "  UTC        : {:02}.{:02}.{} {:02}:{:02}:{:02}  ({})",
        utc.tag, utc.monat, utc.jahr, utc.stunde, utc.minute, utc.sekunde,
        if crate::zeit::plausibel() { "plausibel" } else { "UNPLAUSIBEL!" }
    ));
    let bau = crate::zeit::datum_von_sekunden_seit_2000(crate::zeit::BAU_EPOCHE_S);
    schritt(format_args!(
        "  Kernel-Bau : {:02}.{:02}.{}  (Uhren davor sind nachweislich falsch)",
        bau.tag, bau.monat, bau.jahr
    ));
    schritt(format_args!(
        "  CA-Buendel : {}",
        if crate::programme::CA_BUENDEL.is_empty() {
            alloc::string::String::from("KEINES (TLS haette keinen Vertrauensanker)")
        } else {
            alloc::format!("{} Byte eingebettet", crate::programme::CA_BUENDEL.len())
        }
    ));

    schritt(format_args!("========================"));
    schritt(format_args!(""));
}
