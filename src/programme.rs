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
        name: "angreifer",
        beschreibung: "angreifer <nr> — versucht den Kernel anzugreifen (Sicherheits-Test)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/angreifer")),
    },
    Programm {
        name: "messung",
        beschreibung: "messung <1|2|3> — misst Syscall-Kosten und Durchsatz",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/messung")),
    },
    Programm {
        name: "zertifikate",
        beschreibung: "zertifikate [datei] — zeigt den TLS-Vertrauensanker (Wurzeln, Ablaufdaten)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/zertifikate")),
    },
    Programm {
        name: "tlsspike",
        beschreibung: "tlsspike [name] — Machbarkeitsnachweis: rustls in Ring 3 (kein Handshake)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/tlsspike")),
    },
    Programm {
        name: "holes",
        beschreibung: "holes <url> [--info] [datei] — http und https ueber die Abrufschicht",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/holes")),
    },
    Programm {
        name: "news",
        beschreibung: "news <url> — holt eine Seite und zeigt sie als Text (kein HTML-Renderer)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/news")),
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

// ===========================================================================
// DAS CA-BUENDEL (Serie 7, Teil 2 — der Vertrauensanker)
// ===========================================================================
//
// Denselben Weg wie die Programme, aus demselben Grund: Ein Host-Werkzeug,
// das SpeedFS beschreiben kann, gibt es nicht. Eingebettet reist die Datei
// mit `cargo run`, `cargo test` UND `cargo image` — also auch auf den
// USB-Stick, ohne eine Zeile Extra-Logik im Runner.
//
// WOHER die Bytes stammen und WAS sie nicht leisten (keine Sperrlisten-
// Pruefung!), steht vollstaendig in docs/tls-vertrauen.md. Geholt wird von
// Hand mit tools/ca_bundle_holen.ps1 — ein Vertrauensanker, der unbemerkt
// entsteht, ist wertlos.

/// Das eingebettete CA-Buendel (leer, wenn assets/ca-bundle.pem fehlt).
pub static CA_BUENDEL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ca-bundle.pem"));

/// Wo das Buendel im Dateisystem liegt.
pub fn ca_buendel_pfad() -> &'static str {
    // Ohne Platte landet es im RAM-VFS — dann ist es nach dem Neustart weg,
    // was fuer einen Vertrauensanker ehrlicher ist als ein halber Zustand.
    fs::persistenter_pfad("/platte/system/ca-bundle.pem", "/system/ca-bundle.pem")
}

/// Schreibt das CA-Buendel aufs Dateisystem (wie `installieren`, nur fuer
/// diese eine Datei). Liefert `true`, wenn geschrieben wurde.
///
/// FEHLT DAS BUENDEL, passiert NICHTS und es wird deutlich gemeldet. Eine
/// leere Datei anzulegen waere schlechter als keine: `zertifikate` koennte
/// „0 Wurzeln" nicht mehr von „nie geholt" unterscheiden.
pub fn ca_buendel_installieren() -> bool {
    if CA_BUENDEL.is_empty() {
        crate::serial_println!(
            "[ca] KEIN CA-Buendel eingebettet — TLS haette keinen Vertrauensanker. \
             Holen mit tools/ca_bundle_holen.ps1 (siehe docs/tls-vertrauen.md)."
        );
        return false;
    }
    let ziel = String::from(ca_buendel_pfad());
    if ist_aktuell(&ziel, CA_BUENDEL) {
        crate::serial_println!(
            "[ca] Vertrauensanker {} ist aktuell ({} Byte).",
            ziel,
            CA_BUENDEL.len()
        );
        return false;
    }
    match fs::mit_fs(|dateisystem| dateisystem.schreiben(&ziel, CA_BUENDEL)) {
        Ok(()) => {
            crate::serial_println!("[ca] {} geschrieben ({} Byte).", ziel, CA_BUENDEL.len());
            if let Err(fehler) = fs::sync() {
                crate::serial_println!("[ca] sync fehlgeschlagen: {:?}", fehler);
            }
            true
        }
        Err(fehler) => {
            crate::serial_println!("[ca] {} liess sich NICHT schreiben: {:?}", ziel, fehler);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die eingebetteten Programme sind wirklich da und sind gueltige
    /// SpeedOS-ELFs. Bricht dieser Test, stimmt etwas am Bau der
    /// userland-Crate nicht — und zwar BEVOR jemand versucht, sie zu starten.
    #[test_case]
    fn test_eingebettete_programme_sind_gueltig() {
        // Zwoelf seit Serie 7, Teil 5 (`news` kam dazu). Die feste Zahl ist
        // Absicht: Wer ein Programm ergaenzt, muss es an DREI Stellen tun
        // (userland/Cargo.toml, build.rs, PROGRAMME) — dieser Test faengt
        // die vergessene dritte.
        assert_eq!(PROGRAMME.len(), 12, "es sollen zwoelf Programme mitkommen");
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

