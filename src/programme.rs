// programme.rs — Die mitgelieferten User-Programme (Serie 6, Teil 5)
//
// Jedes SpeedOS-Image traegt die Programme aus userland/ als Bytes in sich
// (eingebettet vom build.rs) und legt sie beim Boot auf das Dateisystem.
// Danach sind es ganz gewoehnliche Dateien: `lies`, `kopiere`, der Explorer
// und `starte` sehen keinen Unterschied zu etwas, das ein Benutzer selbst
// dorthin geschrieben hat.
//
// ==========================================================================
// WARUM EINBETTEN UND NICHT INS DISK-IMAGE SCHREIBEN?
//
// Der naheliegende Weg waere, die Programme beim Bau in speedos-daten.img
// hineinzuschreiben. Dafuer braeuchte es aber ein HOST-Werkzeug, das SpeedFS
// beschreiben kann — unser eigenes Format, in Python oder Rust noch einmal
// nachgebaut, mit der Pflicht, jede Format-Aenderung an ZWEI Stellen
// nachzuziehen. Das waere eine dauerhafte Fehlerquelle fuer einen einmaligen
// Komfort.
//
// So herum schreibt der Code, der SpeedFS ohnehin kennt (der Kernel), die
// Dateien selbst — und alles reist automatisch mit: `cargo run`, `cargo
// test` und `cargo image` (der USB-Stick) brauchen keine Zeile Extra-Logik.
// Der Preis sind ~70 KiB im Kernel-Image. Der ist es wert.
//
// ==========================================================================
// WANN WIRD GESCHRIEBEN?
//
// Nur, wenn es noetig ist: Beim Boot wird jede Datei mit der eingebetteten
// Fassung VERGLICHEN und nur bei Unterschied neu geschrieben. Das hat drei
// Folgen, die alle erwuenscht sind:
//   * Der Normalfall (nichts geaendert) kostet nur Lesezugriffe.
//   * Ein neu uebersetztes Programm ersetzt beim naechsten Boot automatisch
//     das alte — kein "warum laeuft die alte Fassung?"-Raetsel.
//   * Eine Platte, die schon alles hat, wird nicht bei jedem Start
//     unnoetig beschrieben.

use crate::fs;
use alloc::format;
use alloc::string::String;

/// Ein eingebettetes Programm.
pub struct Programm {
    /// Der Dateiname, unter dem es installiert wird.
    pub name: &'static str,
    /// Was es tut (fuer `programme` in der Shell).
    pub beschreibung: &'static str,
    /// Die fertige ELF-Datei.
    pub elf: &'static [u8],
}

/// Alle mitgelieferten Programme. Die Reihenfolge ist die Anzeige-
/// Reihenfolge; die Bytes kommen aus dem build.rs.
pub static PROGRAMME: &[Programm] = &[
    Programm {
        name: "hallo",
        beschreibung: "Gibt Text aus und beendet sich (der Beweis, dass die Kette steht)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/hallo")),
    },
    Programm {
        name: "kopiere",
        beschreibung: "kopiere <von> <nach> — Datei-Werkzeug ueber Syscalls",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/kopiere")),
    },
    Programm {
        name: "netzhole",
        beschreibung: "netzhole <url> [datei] — HTTP-GET ueber die Socket-Syscalls",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/netzhole")),
    },
    Programm {
        name: "zaehle",
        beschreibung: "zaehle [bis] [pause_ms] — gibt Zahlen aus (die linke Pipe-Haelfte)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/zaehle")),
    },
    Programm {
        name: "filter",
        beschreibung: "filter <text> — gibt passende Zeilen der Eingabe aus (die rechte)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/filter")),
    },
    Programm {
        name: "elternprobe",
        beschreibung: "elternprobe [ms] — startet ein Kind und wartet auf es (Ring 3)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/elternprobe")),
    },
];

/// Das Verzeichnis, in dem die Programme wohnen — auf der Platte, wenn eine
/// gemountet ist, sonst im RAM-Dateisystem.
///
/// Dieselbe Orts-Abstraktion wie bei Einstellungen und Papierkorb
/// (`fs::persistenter_pfad`): EINE Stelle entscheidet, kein if-Wildwuchs.
pub fn verzeichnis() -> &'static str {
    fs::persistenter_pfad("/platte/programme", "/programme")
}

/// Der volle Pfad eines mitgelieferten Programms.
pub fn pfad(name: &str) -> String {
    fs::pfad_anhaengen(verzeichnis(), name)
}

/// Schreibt alle eingebetteten Programme ins Dateisystem, sofern sie fehlen
/// oder sich geaendert haben. Liefert die Zahl der geschriebenen Dateien.
///
/// LÄUFT BEIM BOOT (main.rs, nach den Auto-Mounts). Fehler werden gemeldet,
/// nicht verschluckt (Daten-Integritaets-Regel) — aber sie halten den Boot
/// nicht auf: Ein SpeedOS ohne /platte/programme ist immer noch ein
/// benutzbares SpeedOS.
pub fn installieren() -> usize {
    let ordner = String::from(verzeichnis());

    // Das Verzeichnis anlegen, falls es fehlt. "Existiert bereits" ist der
    // Normalfall und kein Fehler.
    if fs::mit_fs(|dateisystem| dateisystem.node_typ(&ordner)).is_err() {
        if let Err(fehler) = fs::mit_fs(|dateisystem| dateisystem.mkdir(&ordner)) {
            crate::serial_println!(
                "[programme] Verzeichnis {} liess sich nicht anlegen: {:?}",
                ordner,
                fehler
            );
            return 0;
        }
    }

    let mut geschrieben = 0usize;
    for programm in PROGRAMME {
        if programm.elf.is_empty() {
            // Mit SPEEDOS_OHNE_USERLAND=1 gebaut — nichts zu installieren.
            continue;
        }
        let ziel = fs::pfad_anhaengen(&ordner, programm.name);

        if ist_aktuell(&ziel, programm.elf) {
            continue;
        }
        match fs::mit_fs(|dateisystem| dateisystem.schreiben(&ziel, programm.elf)) {
            Ok(()) => {
                geschrieben += 1;
                crate::serial_println!(
                    "[programme] {} geschrieben ({} Byte).",
                    ziel,
                    programm.elf.len()
                );
            }
            Err(fehler) => crate::serial_println!(
                "[programme] {} liess sich NICHT schreiben: {:?}",
                ziel,
                fehler
            ),
        }
    }

    if geschrieben > 0 {
        // "Geschrieben" heisst bei SpeedOS "auf dem Medium" — also syncen
        // (Persistenz-Standard des Projekts).
        if let Err(fehler) = fs::sync() {
            crate::serial_println!("[programme] sync fehlgeschlagen: {:?}", fehler);
        }
        crate::serial_println!(
            "[programme] {} Programm(e) nach {} installiert.",
            geschrieben,
            ordner
        );
    }
    geschrieben
}

/// Stimmt die Datei auf der Platte byteweise mit der eingebetteten ueberein?
///
/// Erst die GROESSE vergleichen (ein `stat`, sehr billig) und nur bei
/// Gleichstand wirklich lesen. Bei ~70 KiB ueber drei Dateien ist auch der
/// Vollvergleich guenstig — und er ist der einzige Weg, ein neu uebersetztes
/// Programm zuverlaessig zu erkennen (Zeitstempel koennen luegen, wenn die
/// Uhr zurueckgestellt wurde).
fn ist_aktuell(pfad: &str, elf: &[u8]) -> bool {
    let groesse = match fs::mit_fs(|dateisystem| dateisystem.stat(pfad)) {
        Ok(meta) => meta.groesse,
        Err(_) => return false, // gibt es nicht -> schreiben
    };
    if groesse != elf.len() {
        return false;
    }
    match fs::mit_fs(|dateisystem| dateisystem.lesen(pfad)) {
        Ok(vorhanden) => vorhanden == elf,
        Err(_) => false,
    }
}

/// Die Uebersicht fuer den Shell-Befehl `programme`.
pub fn uebersicht() -> alloc::vec::Vec<String> {
    let ordner = verzeichnis();
    PROGRAMME
        .iter()
        .map(|programm| {
            format!(
                "{:<10} {:>7} B  {}/{}  — {}",
                programm.name,
                programm.elf.len(),
                ordner,
                programm.name,
                programm.beschreibung
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die eingebetteten Programme sind wirklich da und sind gueltige
    /// SpeedOS-ELFs. Bricht dieser Test, stimmt etwas am Bau der
    /// userland-Crate nicht — und zwar BEVOR jemand versucht, sie zu starten.
    #[test_case]
    fn test_eingebettete_programme_sind_gueltig() {
        assert_eq!(PROGRAMME.len(), 6, "es sollen sechs Programme mitkommen");
        for programm in PROGRAMME {
            // Mit SPEEDOS_OHNE_USERLAND=1 gebaut? Dann gibt es nichts zu
            // pruefen — aber das ist der Notfall-Pfad, nicht der Normalfall.
            if programm.elf.is_empty() {
                continue;
            }
            assert!(
                crate::elf::sieht_ausfuehrbar_aus(programm.elf),
                "'{}' sieht nicht wie ein ausfuehrbares ELF aus",
                programm.name
            );
            let geprueft = crate::elf::pruefen(programm.elf).unwrap_or_else(|fehler| {
                panic!("'{}' ist kein ladbares Programm: {}", programm.name, fehler.meldung())
            });
            // Der Einsprung muss im Programm-Bereich liegen ...
            assert!(geprueft.einsprung >= crate::elf::IMAGE_START);
            assert!(geprueft.einsprung < crate::elf::IMAGE_ENDE);
            // ... und es muss ein Code- UND ein Datensegment geben (sonst
            // haette das Linker-Skript die Segmente falsch zusammengelegt).
            assert!(
                geprueft.segmente.iter().any(|s| s.rechte.ausfuehren),
                "'{}' hat kein ausfuehrbares Segment",
                programm.name
            );
            assert!(
                geprueft.segmente.iter().any(|s| s.rechte.schreiben),
                "'{}' hat kein beschreibbares Segment",
                programm.name
            );
            // W^X: KEIN Segment darf beides sein (pruefen() lehnt das zwar
            // ab, aber hier steht es als ausdrueckliche Zusage).
            assert!(
                !geprueft
                    .segmente
                    .iter()
                    .any(|s| s.rechte.schreiben && s.rechte.ausfuehren),
                "'{}' verletzt W^X",
                programm.name
            );
        }
    }

    /// `netzhole` hat eine grosse `.bss` (den 64-KiB-Antwortpuffer). Das ist
    /// unser Beleg, dass ein Segment mit `memsz > filesz` mitkommt — der
    /// Fall, den ein Loader ohne BSS-Behandlung falsch machen wuerde.
    #[test_case]
    fn test_netzhole_hat_bss() {
        let netzhole = PROGRAMME
            .iter()
            .find(|programm| programm.name == "netzhole")
            .expect("netzhole fehlt");
        if netzhole.elf.is_empty() {
            return;
        }
        let geprueft = crate::elf::pruefen(netzhole.elf).expect("netzhole muss gueltig sein");
        let bss: u64 = geprueft.segmente.iter().map(|s| s.bss_bytes()).sum();
        assert!(
            bss >= 64 * 1024,
            "netzhole sollte >= 64 KiB .bss haben, hat aber {}",
            bss
        );
    }
}
