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
use alloc::vec::Vec;
use spin::Mutex;

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
        name: "fenstertest",
        beschreibung: "fenstertest [--breite=N] [--hoehe=N] — ein Ring-3-Prozess mit eigenem Fenster",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/fenstertest")),
    },
    Programm {
        name: "uidemo",
        beschreibung: "uidemo — das Widget-Toolkit in Ring 3 (Beweis der speedui-Trennung)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/uidemo")),
    },
    Programm {
        name: "bilder",
        beschreibung: "bilder <datei.png|.jpg> — Bildbetrachter in Ring 3 (mit & starten!)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/bilder")),
    },
    Programm {
        name: "htmldump",
        beschreibung: "htmldump <datei|url> [--befund|--text|--tags] — den HTML-Baum anzeigen",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/htmldump")),
    },
    Programm {
        name: "cssdump",
        beschreibung: "cssdump <datei|url> [pfad] — berechnete Stile und ihre Regeln",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/cssdump")),
    },
    Programm {
        name: "browser",
        beschreibung: "browser <datei|url> — Webseiten anzeigen (mit & starten!)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/browser")),
    },
    Programm {
        name: "elternprobe",
        beschreibung: "elternprobe [ms] — startet ein Kind und wartet auf es (Ring 3)",
        elf: include_bytes!(concat!(env!("OUT_DIR"), "/elternprobe")),
    },
];

// ===========================================================================
// WO DIE PROGRAMME LIEGEN — und warum das ZWEI Fragen sind
// ===========================================================================
//
// Bis Serie 7 gab es hier nur eine Funktion: „auf der Platte, wenn eine
// gemountet ist, sonst im RAM". Das ist richtig fuer die Frage WOHIN
// INSTALLIERT WIRD — und falsch fuer die Frage WO ETWAS LIEGT.
//
// Der Fall, an dem der Unterschied auffiel: Beim Boot ist keine Platte
// gemountet (unformatiert), die Programme landen also im RAM-VFS unter
// /programme. Mitten in der Sitzung formatiert und mountet man
// (`mkfs.speedfs JA`, `mount`) — ab da liefert `persistenter_pfad`
// /platte/programme, und dort liegt NICHTS. `starte hallo` fand sein
// Programm nicht mehr, obwohl es da war.
//
// „Neustart loest es" waere keine Loesung, sondern das Eingestaendnis, dass
// der Zustand nicht stimmt. Also zwei getrennte Funktionen:
//
//   `verzeichnis()` — WOHIN wird installiert (der bevorzugte, persistente
//                     Ort). Unveraendert.
//   `pfad(name)`    — WO liegt dieses Programm WIRKLICH (nachgesehen, nicht
//                     geraten).
//
// Und drittens `nach_mount_wechsel()`, damit der Zustand sich von selbst
// wieder einrenkt: Nach einem Mount wandern die Programme auf die Platte.

/// Der bevorzugte (persistente) Ort.
const PLATTEN_VERZEICHNIS: &str = "/platte/programme";
/// Der RAM-Fallback, solange keine Platte gemountet ist.
const RAM_VERZEICHNIS: &str = "/programme";

/// Das Verzeichnis, in das INSTALLIERT wird — auf der Platte, wenn eine
/// gemountet ist, sonst im RAM-Dateisystem.
///
/// Dieselbe Orts-Abstraktion wie bei Einstellungen und Papierkorb
/// (`fs::persistenter_pfad`): EINE Stelle entscheidet, kein if-Wildwuchs.
pub fn verzeichnis() -> &'static str {
    fs::persistenter_pfad(PLATTEN_VERZEICHNIS, RAM_VERZEICHNIS)
}

/// Der volle Pfad eines Programms — NACHGESEHEN, nicht geraten.
///
/// Erst der bevorzugte Ort, dann der jeweils andere. Gibt es die Datei
/// nirgends, wird der bevorzugte Pfad geliefert: Dann nennt die
/// Fehlermeldung den Ort, an dem das Programm hingehoert.
pub fn pfad(name: &str) -> String {
    let bevorzugt = fs::pfad_anhaengen(verzeichnis(), name);
    if datei_vorhanden(&bevorzugt) {
        return bevorzugt;
    }
    // Der andere Ort. `verzeichnis()` liefert genau einen der beiden,
    // also ist der Fallback immer der jeweils andere.
    let anderer = if verzeichnis() == PLATTEN_VERZEICHNIS {
        RAM_VERZEICHNIS
    } else {
        PLATTEN_VERZEICHNIS
    };
    let ausweich = fs::pfad_anhaengen(anderer, name);
    if datei_vorhanden(&ausweich) {
        return ausweich;
    }
    bevorzugt
}

/// Liegt dort eine DATEI? (Ein Verzeichnis gleichen Namens zaehlt nicht.)
fn datei_vorhanden(pfad: &str) -> bool {
    fs::mit_fs(|dateisystem| dateisystem.node_typ(pfad)) == Ok(fs::NodeTyp::Datei)
}

/// Wohin zuletzt installiert wurde. Nur fuer `nach_mount_wechsel` —
/// beide Werte sind `&'static str`, es wird also nie alloziert.
static INSTALLIERT_IN: Mutex<Option<&'static str>> = Mutex::new(None);

/// Ein Mount hat sich geaendert: Liegen die Programme jetzt am falschen
/// Ort, werden sie neu installiert. Liefert die Zahl der geschriebenen
/// Dateien (0 = es gab nichts zu tun).
///
/// AUFRUFER sind die Stellen, die einen Mount VERAENDERN — die Shell-
/// Befehle `mount`/`umount` und die Einstellungen-App (die haengt fuer
/// `pruefe.speedfs` kurz aus). Der Boot-Weg braucht ihn nicht: Dort laeuft
/// `installieren()` ohnehin nach dem Auto-Mount.
///
/// Idempotent und billig: Hat sich der Ort nicht geaendert, kostet es einen
/// Zeiger-Vergleich.
pub fn nach_mount_wechsel() -> usize {
    let ziel = verzeichnis();
    {
        let installiert = INSTALLIERT_IN.lock();
        if *installiert == Some(ziel) {
            return 0;
        }
    }
    crate::serial_println!(
        "[programme] Mount-Wechsel: die Programme gehoeren jetzt nach {}.",
        ziel
    );
    let geschrieben = installieren();
    ca_buendel_installieren();
    testbilder_installieren();
    testseite_installieren();
    geschrieben
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

    // Ab hier steht das Verzeichnis — merken, damit `nach_mount_wechsel`
    // einen spaeteren Ortswechsel erkennt.
    *INSTALLIERT_IN.lock() = Some(verzeichnis());

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

const CA_PLATTE: &str = "/platte/system/ca-bundle.pem";
const CA_RAM: &str = "/system/ca-bundle.pem";

/// Wo das Buendel im Dateisystem liegt — nachgesehen wie bei `pfad`.
///
/// Dieselbe Falle wie bei den Programmen: Nach einem Mount mitten in der
/// Sitzung liegt die Datei noch im RAM-VFS. Ein Vertrauensanker, der
/// „nicht gefunden" meldet, obwohl er da ist, waere besonders aergerlich —
/// die Folge ist keine Verbindung.
pub fn ca_buendel_pfad() -> &'static str {
    let bevorzugt = ca_buendel_ziel();
    if datei_vorhanden(bevorzugt) {
        return bevorzugt;
    }
    let anderer = if bevorzugt == CA_PLATTE { CA_RAM } else { CA_PLATTE };
    if datei_vorhanden(anderer) {
        return anderer;
    }
    bevorzugt
}

/// WOHIN das Buendel installiert wird (der bevorzugte Ort — im Gegensatz zu
/// `ca_buendel_pfad`, das nachsieht, wo es LIEGT).
///
/// Ohne Platte landet es im RAM-VFS. Dann ist es nach dem Neustart weg, was
/// fuer einen Vertrauensanker ehrlicher ist als ein halber Zustand.
fn ca_buendel_ziel() -> &'static str {
    fs::persistenter_pfad(CA_PLATTE, CA_RAM)
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
    let ziel = String::from(ca_buendel_ziel());
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
        // Achtzehn seit Serie 8, Teil 7 (`browser` kam dazu). Die Zahl ist
        // Absicht: Wer ein Programm ergaenzt, muss es an DREI Stellen tun
        // (userland/Cargo.toml, build.rs, PROGRAMME) — dieser Test faengt
        // die vergessene dritte.
        assert_eq!(PROGRAMME.len(), 18, "es sollen achtzehn Programme mitkommen");
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

    /// DER MOUNT-WECHSEL-FEHLER, festgehalten: `pfad` muss ein Programm
    /// auch dann finden, wenn es im JEWEILS ANDEREN Verzeichnis liegt.
    ///
    /// Der reale Fall: Beim Boot war keine Platte gemountet, die Programme
    /// landeten im RAM-VFS. Mitten in der Sitzung wird formatiert und
    /// gemountet — `verzeichnis()` zeigt ab da auf die (leere) Platte, und
    /// `starte hallo` fand nichts mehr. Der Test baut genau diese Lage
    /// nach, indem er die Datei in das Verzeichnis legt, das gerade NICHT
    /// das bevorzugte ist.
    #[test_case]
    fn test_pfad_findet_das_andere_verzeichnis() {
        let bevorzugt = verzeichnis();
        let anderes = if bevorzugt == PLATTEN_VERZEICHNIS {
            RAM_VERZEICHNIS
        } else {
            PLATTEN_VERZEICHNIS
        };
        // Das andere Verzeichnis anlegen (mkdir legt keine Elternordner an —
        // ist /platte nicht gemountet, schlaegt es fehl; dann ist der Test
        // an dieser Stelle nicht durchfuehrbar und wird uebersprungen).
        let _ = fs::mit_fs(|dateisystem| dateisystem.mkdir(anderes));
        if fs::mit_fs(|dateisystem| dateisystem.node_typ(anderes)).is_err() {
            return;
        }
        let datei = fs::pfad_anhaengen(anderes, "pfadprobe");
        fs::mit_fs(|dateisystem| dateisystem.schreiben(&datei, b"x")).expect("schreiben");

        // GEFUNDEN, obwohl es nicht im bevorzugten Verzeichnis liegt:
        assert_eq!(pfad("pfadprobe"), datei);

        // Und was es NIRGENDS gibt, bekommt den BEVORZUGTEN Pfad — damit
        // die Fehlermeldung den Ort nennt, an den es gehoert.
        assert_eq!(
            pfad("gibtesnicht"),
            fs::pfad_anhaengen(bevorzugt, "gibtesnicht")
        );

        let _ = fs::mit_fs(|dateisystem| dateisystem.loeschen(&datei));
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


// ===========================================================================
// DIE TESTBILDER (Serie 8, Teil 3)
// ===========================================================================
//
// Denselben Weg wie die Programme und das CA-Buendel, aus demselben Grund
// (kein Host-Werkzeug fuer SpeedFS). Sie landen unter /platte/bilder und
// sind danach gewoehnliche Dateien:
//
//     starte bilder /platte/bilder/verlauf.png &
//
// WARUM DIE KAPUTTEN MITKOMMEN: Sie sind der eigentliche Testfall. Ein
// Dekoder, der nur gute Bilder gesehen hat, ist ungeprueft — und
// `tests/bilder.rs` braucht sie AUF DEM DATEISYSTEM, weil es den ganzen Weg
// prueft (Datei lesen, dekodieren, ablehnen) und nicht nur eine Funktion.
//
// Die Namen stehen hier ein zweites Mal (nach build.rs). Das ist dieselbe
// bewusste Doppelung wie bei den Programmen: `include_bytes!` braucht einen
// literalen Pfad, eine Schleife gibt es dafuer nicht.

/// Ein eingebettetes Testbild.
pub struct Testbild {
    pub name: &'static str,
    pub daten: &'static [u8],
    /// Was der Dekoder damit tun MUSS — die Spalte aus
    /// tools/testbilder_erzeugen.py, hier als Typ statt als Kommentar.
    pub erwartung: Erwartung,
}

/// Was ein Testbild beweisen soll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Erwartung {
    /// Muss dekodieren.
    Gut,
    /// MUSS abgelehnt werden — mit einem Fehler, nicht mit einer Panik.
    Abgelehnt,
    /// Darf beides, solange es weder abstuerzt noch haengt.
    Egal,
}

macro_rules! testbild {
    ($name:literal, $erwartung:expr) => {
        Testbild {
            name: $name,
            daten: include_bytes!(concat!(env!("OUT_DIR"), "/testbild_", $name)),
            erwartung: $erwartung,
        }
    };
}

pub static TESTBILDER: &[Testbild] = &[
    testbild!("verlauf.png", Erwartung::Gut),
    testbild!("gross.png", Erwartung::Gut),
    testbild!("rgba.png", Erwartung::Gut),
    testbild!("grau.png", Erwartung::Gut),
    testbild!("palette.png", Erwartung::Gut),
    testbild!("abgeschnitten.png", Erwartung::Abgelehnt),
    testbild!("ohne_iend.png", Erwartung::Egal),
    testbild!("crc_kaputt.png", Erwartung::Egal),
    testbild!("falsche_signatur.png", Erwartung::Abgelehnt),
    testbild!("absurde_masse.png", Erwartung::Abgelehnt),
    testbild!("null_masse.png", Erwartung::Abgelehnt),
    testbild!("bombe.png", Erwartung::Abgelehnt),
    testbild!("riesige_chunk_laenge.png", Erwartung::Abgelehnt),
    testbild!("viele_chunks.png", Erwartung::Egal),
    testbild!("leer.png", Erwartung::Abgelehnt),
    testbild!("nur_signatur.png", Erwartung::Abgelehnt),
    testbild!("kein_bild.png", Erwartung::Abgelehnt),
];

const BILDER_PLATTE: &str = "/platte/bilder";
const BILDER_RAM: &str = "/bilder";

/// Wohin die Testbilder installiert werden.
pub fn bilder_verzeichnis() -> &'static str {
    fs::persistenter_pfad(BILDER_PLATTE, BILDER_RAM)
}

/// Der volle Pfad eines Testbildes — nachgesehen wie `pfad`, aus demselben
/// Grund (ein Mount mitten in der Sitzung verschiebt den Ort).
pub fn bild_pfad(name: &str) -> String {
    let bevorzugt = fs::pfad_anhaengen(bilder_verzeichnis(), name);
    if datei_vorhanden(&bevorzugt) {
        return bevorzugt;
    }
    let anderes = if bilder_verzeichnis() == BILDER_PLATTE {
        BILDER_RAM
    } else {
        BILDER_PLATTE
    };
    let anderer = fs::pfad_anhaengen(anderes, name);
    if datei_vorhanden(&anderer) {
        return anderer;
    }
    bevorzugt
}

/// Schreibt die Testbilder aufs Dateisystem. Liefert die Zahl der
/// geschriebenen Dateien.
///
/// `leer.png` ist NULL BYTE GROSS und wird trotzdem geschrieben — es ist
/// ein Testfall, kein Versehen. Deshalb prueft diese Funktion (anders als
/// `installieren`) nicht auf `is_empty()`; sie kann es auch nicht, denn ein
/// nicht gefundenes Testbild sieht genauso aus.
pub fn testbilder_installieren() -> usize {
    let ordner = String::from(bilder_verzeichnis());
    if fs::mit_fs(|dateisystem| dateisystem.node_typ(&ordner)).is_err() {
        if let Err(fehler) = fs::mit_fs(|dateisystem| dateisystem.mkdir(&ordner)) {
            crate::serial_println!(
                "[bilder] Verzeichnis {} liess sich nicht anlegen: {:?}",
                ordner,
                fehler
            );
            return 0;
        }
    }

    let mut geschrieben = 0usize;
    for bild in TESTBILDER {
        let ziel = fs::pfad_anhaengen(&ordner, bild.name);
        if ist_aktuell(&ziel, bild.daten) {
            continue;
        }
        match fs::mit_fs(|dateisystem| dateisystem.schreiben(&ziel, bild.daten)) {
            Ok(()) => geschrieben += 1,
            Err(fehler) => crate::serial_println!(
                "[bilder] {} liess sich NICHT schreiben: {:?}",
                ziel,
                fehler
            ),
        }
    }

    if geschrieben > 0 {
        if let Err(fehler) = fs::sync() {
            crate::serial_println!("[bilder] sync fehlgeschlagen: {:?}", fehler);
        }
        crate::serial_println!(
            "[bilder] {} Testbild(er) nach {} installiert.",
            geschrieben,
            ordner
        );
    }
    geschrieben
}


// ===========================================================================
// DIE TESTSEITE (Serie 8, Teil 4)
// ===========================================================================

/// Die erste Webseite der Welt (1991), eingebettet — damit `htmldump` auch
/// ohne Netz etwas zu tun hat.
///
/// Sie ist zugleich Pruefseite A aus docs/browser-v1.md: reines HTML, kein
/// CSS, kein JavaScript — und nach heutigen Massstaeben kaputt (nicht
/// geschlossene `<P>`, Grossschreibung, Attribute ohne Anfuehrungszeichen).
/// Herkunft und Datum: assets/testseiten/HERKUNFT.txt.
pub static TESTSEITE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/testseite.html"));

/// Pruefseite B (Serie 8, Teil 7): ein langer Wikipedia-Artikel, ~293 KiB.
///
/// SIE IST DIE MESSEINGABE. Der Scroll-Frame, an dem das
/// Umstiegskriterium aus docs/fenster-syscalls.md haengt, wird an ihr
/// gemessen — an einer echten Seite mit Tabellen, Listen, Ueberschriften
/// und ein paar tausend Anzeige-Befehlen, nicht an einem Absatz
/// Blindtext. Eine Messung an einer kleinen Seite waere eine Messung des
/// Hintergrund-Fuellens.
pub static GROSSE_TESTSEITE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/grosse_testseite.html"));

const SEITEN_PLATTE: &str = "/platte/seiten";
const SEITEN_RAM: &str = "/seiten";

/// Wohin die Testseiten installiert werden.
pub fn seiten_verzeichnis() -> &'static str {
    fs::persistenter_pfad(SEITEN_PLATTE, SEITEN_RAM)
}

/// Der Pfad der kleinen Testseite — nachgesehen wie `pfad`, aus
/// demselben Grund.
pub fn testseite_pfad() -> String {
    seite_pfad("cern.html")
}

/// Der Pfad der grossen Testseite.
pub fn grosse_testseite_pfad() -> String {
    seite_pfad("wikipedia.html")
}

/// NACHSEHEN, wo eine Testseite wirklich liegt.
///
/// Dieselbe Trennung wie bei `pfad()` und `verzeichnis()` (Serie 8,
/// Teil 1): `seiten_verzeichnis()` sagt, WOHIN installiert wird, diese
/// Funktion sieht nach, wo die Datei IST. Nach einem Mount mitten in der
/// Sitzung sind das zwei verschiedene Orte.
fn seite_pfad(name: &str) -> String {
    let bevorzugt = fs::pfad_anhaengen(seiten_verzeichnis(), name);
    if datei_vorhanden(&bevorzugt) {
        return bevorzugt;
    }
    let anderes = if seiten_verzeichnis() == SEITEN_PLATTE { SEITEN_RAM } else { SEITEN_PLATTE };
    let anderer = fs::pfad_anhaengen(anderes, name);
    if datei_vorhanden(&anderer) {
        return anderer;
    }
    bevorzugt
}

/// Schreibt die Testseiten aufs Dateisystem. Liefert `true`, wenn
/// mindestens eine geschrieben wurde.
pub fn testseite_installieren() -> bool {
    let ordner = String::from(seiten_verzeichnis());
    if fs::mit_fs(|dateisystem| dateisystem.node_typ(&ordner)).is_err() {
        if let Err(fehler) = fs::mit_fs(|dateisystem| dateisystem.mkdir(&ordner)) {
            crate::serial_println!("[seiten] {} liess sich nicht anlegen: {:?}", ordner, fehler);
            return false;
        }
    }
    let mut geschrieben = seite_installieren(&ordner, "cern.html", TESTSEITE);
    geschrieben |= seite_installieren(&ordner, "wikipedia.html", GROSSE_TESTSEITE);
    if geschrieben {
        if let Err(fehler) = fs::sync() {
            crate::serial_println!("[seiten] sync fehlgeschlagen: {:?}", fehler);
        }
    }
    geschrieben
}

/// Eine einzelne Seite schreiben — nur, wenn sie sich geaendert hat.
fn seite_installieren(ordner: &str, name: &str, inhalt: &[u8]) -> bool {
    if inhalt.is_empty() {
        return false;
    }
    let ziel = fs::pfad_anhaengen(ordner, name);
    if ist_aktuell(&ziel, inhalt) {
        return false;
    }
    match fs::mit_fs(|dateisystem| dateisystem.schreiben(&ziel, inhalt)) {
        Ok(()) => {
            crate::serial_println!("[seiten] {} geschrieben ({} Byte).", ziel, inhalt.len());
            true
        }
        Err(fehler) => {
            crate::serial_println!("[seiten] {} liess sich NICHT schreiben: {:?}", ziel, fehler);
            false
        }
    }
}

// ===========================================================================
// DER BROWSER ALS SYSTEM-DIENST (Serie 8, Teil 8)
// ===========================================================================

/// Oeffnet den Browser — optional mit einer Adresse.
///
/// ===================================================================
/// EINE STELLE, DURCH DIE ALLES LAEUFT
///
/// Startmenue, Explorer-Doppelklick auf eine HTML-Datei und der
/// Shell-Befehl `browser` rufen ALLE diese Funktion. Das ist die
/// „Registrierung als Standard" aus der Aufgabe: nicht eine Tabelle von
/// Zuordnungen, sondern eine Funktion, an der man sieht, WAS passiert —
/// und die man an genau einer Stelle aendert, wenn der Browser einmal
/// anders gestartet werden soll.
///
/// IMMER IM HINTERGRUND, nie synchron: Ein Kernel-Task, der auf den
/// Browser wartet, haelt den Compositor an, und dann sieht niemand das
/// Fenster (Serie 8, Teil 1). Deshalb kein `warten_auf`.
///
/// Liefert die PID, damit ein Aufrufer melden kann, dass es geklappt hat.
pub fn browser_oeffnen(adresse: Option<&str>) -> Result<crate::prozess::Pid, String> {
    let pfad = pfad("browser");
    if !datei_vorhanden(&pfad) {
        return Err(String::from(
            "Der Browser ist nicht installiert (/platte/programme/browser fehlt).",
        ));
    }
    // `argv[0]` ist der Programmname — wie ueberall in der ABI.
    let mut argumente: Vec<&str> = alloc::vec!["browser"];
    if let Some(adresse) = adresse {
        argumente.push(adresse);
    }
    match crate::prozess::prozess_starten_mit(&pfad, &argumente, None, None, None, false) {
        Ok(pid) => Ok(pid),
        Err(fehler) => Err(fehler.meldung()),
    }
}

/// Sieht diese Datei nach HTML aus?
///
/// ===================================================================
/// ERST DER INHALT, DANN DER NAME — und warum es hier BEIDES braucht
///
/// Bei PROGRAMMEN entscheidet SpeedOS an den ersten Bytes
/// (`prozess::ist_programm`), und der Kommentar dort sagt zu Recht: Eine
/// Endung ist nur eine Behauptung. Bei HTML geht das nur zur Haelfte —
/// **HTML hat keine verlaessliche Signatur.** Viele Seiten beginnen mit
/// `<!DOCTYPE html>` oder `<html`, aber genauso viele mit einem
/// Kommentar, einem `<?xml`, einer Leerzeile oder gleich mit `<body`.
///
/// Deshalb: Zuerst wird HINEINGESEHEN (das ist die verlaessliche
/// Auskunft), und nur wenn das nichts ergibt, entscheidet die Endung.
/// Die Endung ist hier nicht die bequeme Abkuerzung, sondern der
/// Notnagel — genau andersherum als bei den Programmen.
pub fn sieht_nach_html_aus(pfad: &str) -> bool {
    // (1) Hineinsehen: die ersten 256 Byte reichen fuer jede Einleitung.
    let anfang = fs::mit_fs(|dateisystem| {
        let mut puffer = alloc::vec![0u8; 256];
        dateisystem
            .read_at(pfad, 0, &mut puffer)
            .map(|gelesen| {
                puffer.truncate(gelesen);
                puffer
            })
    });
    if let Ok(bytes) = anfang {
        let text = String::from_utf8_lossy(&bytes);
        let klein: String = text.chars().take(256).flat_map(|z| z.to_lowercase()).collect();
        let gestutzt = klein.trim_start();
        for marke in ["<!doctype html", "<html", "<head", "<body", "<!-- "] {
            if gestutzt.starts_with(marke) {
                return true;
            }
        }
        // Auch mitten am Anfang: `<html` nach einem Kommentar o. Ae.
        if klein.contains("<html") || klein.contains("<!doctype html") {
            return true;
        }
    }
    // (2) Der Notnagel: die Endung.
    let name = match pfad.rfind('/') {
        Some(i) => &pfad[i + 1..],
        None => pfad,
    };
    let klein: String = name.chars().flat_map(|z| z.to_lowercase()).collect();
    klein.ends_with(".html") || klein.ends_with(".htm")
}
