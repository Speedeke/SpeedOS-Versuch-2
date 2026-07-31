// build.rs — Baut die USER-PROGRAMME mit und legt sie dem Kernel bei
//             (Serie 6, Teil 5, Aufgabe 4)
//
// ==========================================================================
// DAS HENNE-EI-PROBLEM UND SEINE LOESUNG
//
// Die Programme in userland/ sollen beim ersten Boot auf /platte/programme
// landen. Nur: Wie kommen sie dorthin? SpeedFS ist unser eigenes Format —
// ein Host-Werkzeug, das es beschreiben koennte, gibt es nicht (und es zu
// bauen waere ein eigenes Projekt).
//
// Also der andere Weg: Die fertigen ELF-Dateien werden per `include_bytes!`
// ins KERNEL-IMAGE eingebettet, und der Kernel schreibt sie beim Boot selbst
// aufs Dateisystem (src/programme.rs). Damit reist alles automatisch mit —
// `cargo run`, `cargo test` UND `cargo image` (der USB-Stick) bekommen die
// Programme ohne eine einzige Extra-Zeile im Runner.
//
// `include_bytes!` verlangt aber, dass die Dateien schon existieren, wenn
// der Kernel uebersetzt wird. Genau dafuer ist diese Datei da: Sie baut
// userland/ VOR dem Kernel und legt die Ergebnisse in OUT_DIR.
//
// ZWEI FALLSTRICKE beim Aufruf von cargo aus cargo:
//
//  (1) EIGENES target-Verzeichnis (userland/target). Wuerden beide Baeume
//      dasselbe benutzen, wartete der innere cargo auf die Dateisperre des
//      aeusseren — ein Deadlock, der wie ein Haenger aussieht.
//  (2) DIE GEERBTEN UMGEBUNGSVARIABLEN WEG. cargo setzt fuer Build-Skripte
//      unter anderem CARGO_ENCODED_RUSTFLAGS (aus der .cargo/config.toml des
//      Kernels). Die gelten fuer den KERNEL, nicht fuer User-Programme —
//      geerbt wuerden sie die Einstellungen aus userland/.cargo/config.toml
//      ueberschreiben.
// ==========================================================================

use std::path::{Path, PathBuf};
use std::process::Command;

/// Die Programme, die mitgebaut und eingebettet werden. Wer hier einen
/// Namen ergaenzt, muss ihn auch in userland/Cargo.toml als `[[bin]]` und
/// in src/programme.rs in die Liste eintragen.
const PROGRAMME: &[&str] = &[
    "hallo", "kopiere", "netzhole", "zaehle", "filter", "elternprobe",
    "angreifer", "messung", "zertifikate", "tlsspike", "holes", "news",
    "fenstertest", "uidemo",
];

fn main() {
    let wurzel = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let userland = wurzel.join("userland");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    // Neu bauen, wenn sich am User-Space etwas aendert.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=userland/src");
    println!("cargo:rerun-if-changed=userland/Cargo.toml");
    println!("cargo:rerun-if-changed=userland/build.rs");
    println!("cargo:rerun-if-changed=userland/speedos.ld");
    println!("cargo:rerun-if-changed=userland/.cargo/config.toml");
    // Der geteilte HTTP-Parser (Serie 7, Teil 4) haengt an BEIDEN Seiten.
    println!("cargo:rerun-if-changed=speedhttp/src");
    // Notausgang, falls die verschachtelte cargo-Ausfuehrung in einer
    // fremden Umgebung Probleme macht: SPEEDOS_OHNE_USERLAND=1 baut den
    // Kernel mit LEEREN Programmen (er bootet dann ohne /platte/programme).
    println!("cargo:rerun-if-env-changed=SPEEDOS_OHNE_USERLAND");

    // Serie 7, Teil 2: das Bau-Datum und das CA-Buendel.
    bau_datum_setzen();
    ca_buendel_einbetten(&wurzel, &out_dir);

    let ueberspringen = std::env::var("SPEEDOS_OHNE_USERLAND")
        .map(|wert| wert == "1")
        .unwrap_or(false);

    if ueberspringen {
        eprintln!("[build] SPEEDOS_OHNE_USERLAND=1 — User-Programme werden NICHT gebaut.");
        for name in PROGRAMME {
            std::fs::write(out_dir.join(name), []).expect("leeres Programm anlegen");
        }
        return;
    }

    userland_bauen(&userland);

    // Die fertigen ELFs nach OUT_DIR kopieren. Der Umweg ueber OUT_DIR ist
    // Absicht: `include_bytes!` zeigt dann auf einen Pfad, den cargo selbst
    // verwaltet, statt quer in einen fremden target-Baum.
    let quelle = userland.join("target/x86_64-unknown-none/release");
    for name in PROGRAMME {
        let von = quelle.join(name);
        if !von.exists() {
            panic!(
                "User-Programm '{}' wurde nicht gebaut (erwartet: {}). \
                 Notfalls mit SPEEDOS_OHNE_USERLAND=1 ohne Programme bauen.",
                name,
                von.display()
            );
        }
        std::fs::copy(&von, out_dir.join(name))
            .unwrap_or_else(|fehler| panic!("'{}' kopieren fehlgeschlagen: {fehler}", name));
        let groesse = std::fs::metadata(&von).map(|m| m.len()).unwrap_or(0);
        eprintln!("[build] User-Programm '{name}' eingebettet ({groesse} Byte).");
    }
}

/// Baut das userland-Crate mit einem eigenen cargo-Aufruf.
fn userland_bauen(userland: &Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| String::from("cargo"));
    let mut befehl = Command::new(cargo);
    befehl
        // Aus userland/ heraus bauen, damit userland/.cargo/config.toml
        // greift (cargo sucht sie vom ARBEITSVERZEICHNIS aufwaerts, nicht
        // vom Manifest aus).
        .current_dir(userland)
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("x86_64-unknown-none")
        // Eigener Baum — siehe Fallstrick (1) im Kopfkommentar.
        .arg("--target-dir")
        .arg("target");

    // Fallstrick (2): alles wegraeumen, was der aeussere cargo uns
    // aufgedraengt hat und was den inneren Bau verfaelschen wuerde.
    for variable in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTDOC",
        "RUSTDOCFLAGS",
        "CARGO_MAKEFLAGS",
        "CARGO_PRIMARY_PACKAGE",
        "CARGO_UNSTABLE_BUILD_STD",
        "TARGET",
        "HOST",
        "OUT_DIR",
        "DEBUG",
        "OPT_LEVEL",
        "PROFILE",
    ] {
        befehl.env_remove(variable);
    }

    let ausgabe = befehl
        .output()
        .expect("`cargo build` fuer userland/ konnte nicht gestartet werden");

    // cargo redet auf stderr; das reichen wir durch, damit man Fehler sieht.
    // stdout eines verschachtelten cargo darf NICHT auf unser stdout — dort
    // liest der aeussere cargo `cargo:`-Anweisungen.
    if !ausgabe.stdout.is_empty() {
        eprintln!("{}", String::from_utf8_lossy(&ausgabe.stdout));
    }
    if !ausgabe.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&ausgabe.stderr));
        panic!(
            "Der Bau der User-Programme in userland/ ist fehlgeschlagen \
             (Exit {:?}). Der Kernel wird NICHT gebaut — lieber ein klarer \
             Fehler als ein SpeedOS ohne Programme.",
            ausgabe.status.code()
        );
    }
}

// ===========================================================================
// DAS BAU-DATUM (Serie 7, Teil 2 — die Zeit-Plausibilitaet)
// ===========================================================================

/// Sekunden zwischen dem 1.1.1970 und dem 1.1.2000 — SpeedOS rechnet in der
/// 2000er-Epoche (`zeit::sekunden_seit_2000`), die Bauumgebung in der
/// UNIX-Epoche.
const EPOCHE_1970_BIS_2000: u64 = 946_684_800;

/// Legt das BAU-DATUM des Kernels als Umgebungsvariable fuer den Compiler ab.
///
/// WOZU: Eine Uhr, die VOR dem Bau des laufenden Kernels steht, ist
/// nachweislich falsch — dieser Kernel kann zu diesem Zeitpunkt nicht
/// existiert haben. Das ist die einzige Plausibilitaetsgrenze, die ein
/// System OHNE Netz und ohne zweite Zeitquelle ueberhaupt kennen kann, und
/// sie ist erstaunlich wirksam: Der klassische Ausfall (leere
/// Pufferbatterie) setzt die Uhr auf 1.1.2000 oder 1.1.1980 zurueck, also
/// weit VOR jedes Bau-Datum.
///
/// Sie erkennt NICHT: eine Uhr, die um Stunden oder Tage falsch geht, und
/// eine absichtlich vorgestellte Uhr. Dafuer braeuchte es NTP (docs/zeit.md).
fn bau_datum_setzen() {
    // Reproduzierbare Baue: Wer SOURCE_DATE_EPOCH setzt (der uebliche
    // Standard dafuer), bestimmt das Datum selbst.
    let unix_s = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|wert| wert.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                // Geht selbst DAS schief, ist 0 der ehrlichste Wert: Der
                // Kernel meldet dann "kein Bau-Datum" und prueft nicht,
                // statt eine erfundene Grenze zu benutzen.
                .unwrap_or(0)
        });
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let seit_2000 = unix_s.saturating_sub(EPOCHE_1970_BIS_2000);
    println!("cargo:rustc-env=SPEEDOS_BAU_EPOCHE_S={}", seit_2000);
}

// ===========================================================================
// DAS CA-BUENDEL (Serie 7, Teil 2 — der Vertrauensanker)
// ===========================================================================

/// Bettet `assets/ca-bundle.pem` ein, falls es da ist.
///
/// FEHLT DIE DATEI, wird der Kernel trotzdem gebaut — mit einem LEEREN
/// Buendel und einer deutlichen Meldung. Das ist Absicht und keine
/// Nachlaessigkeit: Wurzelzertifikate sind der Vertrauensanker des ganzen
/// Systems. Sie gehoeren aus einer nachvollziehbaren Quelle geholt und mit
/// Herkunft und Datum vermerkt (`docs/tls-vertrauen.md`), nicht von einem
/// Build-Skript stillschweigend irgendwo besorgt. Ein Buendel, das
/// unbemerkt entsteht, ist genau das, wogegen ein Vertrauensanker schuetzen
/// soll.
fn ca_buendel_einbetten(wurzel: &Path, out_dir: &Path) {
    let quelle = wurzel.join("assets").join("ca-bundle.pem");
    println!("cargo:rerun-if-changed=assets/ca-bundle.pem");
    let ziel = out_dir.join("ca-bundle.pem");
    match std::fs::read(&quelle) {
        Ok(inhalt) => {
            eprintln!(
                "[build] CA-Buendel eingebettet: {} ({} Byte)",
                quelle.display(),
                inhalt.len()
            );
            std::fs::write(&ziel, inhalt).expect("CA-Buendel nach OUT_DIR kopieren");
        }
        Err(_) => {
            eprintln!(
                "[build] KEIN CA-Buendel unter assets/ca-bundle.pem — SpeedOS bootet \
                 ohne Vertrauensanker. Holen mit: tools/ca_bundle_holen.ps1 \
                 (siehe docs/tls-vertrauen.md)."
            );
            std::fs::write(&ziel, []).expect("leeres CA-Buendel anlegen");
        }
    }
}
