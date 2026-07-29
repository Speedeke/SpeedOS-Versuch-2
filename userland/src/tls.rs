// tls.rs — Was eine TLS-Bibliothek von SpeedOS verlangt (Serie 7, Teil 3)
//
// Drei Dinge fordert `rustls` von der Plattform, und alle drei gibt es bei
// uns seit Serie 7 als Syscall. Diese Datei ist die Naht dazwischen —
// jeweils ein duenner Mantel, mehr nicht:
//
//   ZUFALL   -> `zufall` (12)          docs/zufall.md
//   ZEIT     -> `zeit_geprueft` (13)   docs/tls-vertrauen.md §3e
//   TRANSPORT-> die Socket-Syscalls    (32..37)
//
// ==========================================================================
// WARUM DIE ZEIT UEBER `zeit_geprueft` KOMMT UND NICHT UEBER `zeit_epoche`
//
// Weil eine falsche Uhr die Zertifikatspruefung entweder unbenutzbar macht
// oder — schlimmer — zu lax. `zeit_geprueft` liefert bei nachweislich
// falscher Uhr einen FEHLER, und der `UhrenQuelle` unten gibt dann `None`
// zurueck. rustls behandelt das als „keine Zeit" und lehnt die Pruefung ab,
// statt sie zu ueberspringen. Genau so muss es sein.

// ==========================================================================
// SEIT SERIE 7, TEIL 4 STEHT HIER AUCH DER STROM SELBST
//
// Teil 3 hat bewiesen, dass `rustls` in Ring 3 laeuft. Teil 4 laesst es
// reden: `TlsStrom` treibt die `UnbufferedClientConnection`-Zustandsmaschine
// ueber den `TcpStrom` von unten und sieht nach aussen aus wie er —
// `lesen`/`schreiben`, blockierend, Bytes rein, Bytes raus. Genau deshalb
// kann `holes` denselben HTTP-Parser benutzen wie der Kernel-Klient: Fuer den
// Parser ist ein TLS-Strom nur ein anderer Lieferant derselben Bytes.

extern crate alloc;

use crate::Fehler;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError};

// ---------------------------------------------------------------------------
// (1) ZUFALL — die getrandom-Anbindung
// ---------------------------------------------------------------------------

/// Fuellt einen Puffer mit kryptographisch brauchbarem Zufall.
///
/// Das ist die Funktion, die `getrandom` (und ueber es die halbe
/// RustCrypto-Welt) benutzt. Sie ist ein reiner Durchreicher an
/// `SYS_ZUFALL`; die ganze Arbeit steckt im Kernel (`src/zufall.rs`).
///
/// **Sie liefert nie schwache Bytes.** Ist der Pool ungesaet, blockiert der
/// Syscall bis zu 10 s und meldet dann `NICHT_GESAET` — dieser Fehler wird
/// hier zu einem `getrandom::Error` und damit zu einem Handshake-Abbruch.
/// Ein Handshake mit geratenem Zufall waere schlimmer als kein Handshake.
pub fn zufall_fuellen(ziel: &mut [u8]) -> Result<(), Fehler> {
    // Der Syscall deckelt bei MAX_PUFFER (64 KiB) — in Stuecken holen.
    for teil in ziel.chunks_mut(64 * 1024) {
        crate::zufall(teil)?;
    }
    Ok(())
}

/// Registriert `zufall_fuellen` als getrandom-Backend.
///
/// `getrandom` kennt `x86_64-unknown-none` nicht (und kann es auch nicht —
/// es weiss ja nicht, dass darunter SpeedOS liegt). Das `custom`-Feature ist
/// genau dafuer gedacht: Wir liefern die Implementierung.
///
/// Das Makro muss im BINARY stehen, nicht in dieser Bibliothek — deshalb
/// hier nur die Funktion, und `tlsspike` registriert sie.
#[macro_export]
macro_rules! zufall_als_getrandom {
    () => {
        fn __speedos_getrandom(ziel: &mut [u8]) -> ::core::result::Result<(), getrandom::Error> {
            match $crate::tls::zufall_fuellen(ziel) {
                Ok(()) => Ok(()),
                // getrandom verlangt einen NICHT-NULL-Code; wir nehmen einen
                // aus dem fuer Anwender reservierten Bereich und schlagen
                // unseren ABI-Fehlercode drauf, damit er sichtbar bleibt.
                Err(fehler) => Err(getrandom::Error::from(
                    ::core::num::NonZeroU32::new(
                        getrandom::Error::CUSTOM_START + fehler.0 as u32,
                    )
                    .unwrap(),
                )),
            }
        }
        getrandom::register_custom_getrandom!(__speedos_getrandom);
    };
}

// ---------------------------------------------------------------------------
// (3) DER TRANSPORT — ein blockierender Byte-Strom ueber die Socket-Syscalls
// ---------------------------------------------------------------------------

/// Ein TCP-Strom, wie ihn eine TLS-Bibliothek als Unterlage erwartet.
///
/// ==========================================================================
/// DIE EINE STELLE, AN DER UNSERE ABI NICHT PASST — und wie sie geglaettet
/// wird:
///
/// `empfange` ist laut ABI **nicht-blockierend**: 0 heisst „noch nichts da",
/// nicht „Ende" (docs/syscalls.md §6). Eine TLS-Bibliothek erwartet aber
/// einen blockierenden Strom: Sie ruft `read`, bis sie genug Bytes fuer den
/// naechsten Datensatz hat, und rechnet nicht damit, 0 zu bekommen.
///
/// Diese Struktur ueberbrueckt das mit einer WARTESCHLEIFE: Sie ruft
/// `empfange`, und solange 0 zurueckkommt und die Verbindung noch steht,
/// gibt sie die Zeitscheibe ab (`abgeben`) und versucht es erneut. Zwei
/// Abbruchbedingungen, beide noetig:
///   * Die Gegenstelle hat geschlossen -> `Ok(0)` = echtes Dateiende.
///   * Die Frist ist abgelaufen -> `ZEITUEBERSCHREITUNG`, nie ein Haenger.
///
/// `abgeben` statt `schlafe`: Wir warten auf Daten, die ein ANDERER
/// (der Netz-Task im Kernel) liefert — schlafen wuerde ihn nicht
/// beschleunigen, abgeben schon (Serie 7, Teil 0).
/// ==========================================================================
pub struct TcpStrom {
    handle: u64,
    /// Wie lange `lesen` hoechstens auf das erste Byte wartet.
    pub frist_ms: u64,
}

/// Voreingestellte Lese-Frist. Grosszuegig: Ein TLS-Handshake wartet auf
/// eine Gegenstelle im Internet.
pub const LESE_FRIST_MS: u64 = 15_000;

impl TcpStrom {
    /// Verbindet zu `ip:port` (IPv4 als u32, a.b.c.d in den Bytes 3..0).
    pub fn verbinden(ip: u32, port: u16) -> Result<TcpStrom, Fehler> {
        let handle = crate::socket(crate::TCP)?;
        // `verbinde` blockiert und pumpt selbst (docs/syscalls.md §6) — ein
        // Ring-3-Programm koennte den Handshake nicht selbst vorantreiben.
        if let Err(fehler) = crate::verbinde(handle, ip, port) {
            let _ = crate::schliesse(handle);
            return Err(fehler);
        }
        Ok(TcpStrom {
            handle,
            frist_ms: LESE_FRIST_MS,
        })
    }

    /// Das rohe Handle (fuer `socket_zustand` und Diagnose).
    pub fn handle(&self) -> u64 {
        self.handle
    }

    /// Steht die Verbindung noch?
    pub fn verbunden(&self) -> bool {
        matches!(crate::socket_zustand(self.handle), Ok(crate::Z_VERBUNDEN))
    }

    /// BLOCKIEREND lesen. `Ok(0)` heisst Dateiende (Gegenstelle zu).
    pub fn lesen(&mut self, ziel: &mut [u8]) -> Result<usize, Fehler> {
        if ziel.is_empty() {
            return Ok(0);
        }
        let frist = crate::zeit_jetzt() + self.frist_ms;
        loop {
            match crate::empfange(self.handle, ziel)? {
                0 => {
                    // Nichts da. Ist die Verbindung noch offen?
                    match crate::socket_zustand(self.handle)? {
                        crate::Z_VERBUNDEN => {}
                        // Peer hat geschlossen: Es koennen noch Restdaten im
                        // Puffer liegen — ein weiterer `empfange` holt sie.
                        // Erst wenn AUCH der 0 liefert, ist wirklich Schluss.
                        crate::Z_PEER_HAT_GESCHLOSSEN | crate::Z_GESCHLOSSEN => return Ok(0),
                        _ => return Err(Fehler::NICHT_VERBUNDEN),
                    }
                    if crate::zeit_jetzt() >= frist {
                        return Err(Fehler::ZEITUEBERSCHREITUNG);
                    }
                    // Dem Kernel-Netz-Task die CPU geben (siehe Kopf).
                    crate::abgeben();
                }
                gelesen => return Ok(gelesen as usize),
            }
        }
    }

    /// BLOCKIEREND schreiben — schreibt ALLES oder liefert einen Fehler.
    ///
    /// `sende` darf weniger uebernehmen als angeboten (der TCP-Sendepuffer
    /// ist endlich, docs/syscalls.md §6). Eine TLS-Bibliothek erwartet
    /// `write_all`-Semantik, also wird hier geschleift.
    pub fn schreiben(&mut self, daten: &[u8]) -> Result<(), Fehler> {
        let mut ab = 0usize;
        let frist = crate::zeit_jetzt() + self.frist_ms;
        while ab < daten.len() {
            match crate::sende(self.handle, &daten[ab..])? {
                0 => {
                    // Sendepuffer voll: abgeben, damit der Stack ihn leert.
                    if crate::zeit_jetzt() >= frist {
                        return Err(Fehler::ZEITUEBERSCHREITUNG);
                    }
                    crate::abgeben();
                }
                n => ab += n as usize,
            }
        }
        Ok(())
    }
}

impl Drop for TcpStrom {
    /// Das Handle gehoert dem Strom — beim Fallenlassen geht es zu.
    /// (Der Kernel schliesst es beim Prozess-Ende ohnehin; hier geht es
    /// darum, dass ein langlaufendes Programm keine Handles anhaeuft.)
    fn drop(&mut self) {
        let _ = crate::schliesse(self.handle);
    }
}

// ===========================================================================
// (2) DIE ZEIT — die Naht, an der die Gueltigkeitspruefung haengt
// ===========================================================================

/// DIE UHR FUER rustls.
///
/// Sie holt die Zeit ueber `zeit_geprueft` (Syscall 13) — also die, die der
/// Kernel gegen das Bau-Datum plausibilisiert hat. Ist die Uhr nachweislich
/// falsch, liefert `current_time` `None`.
///
/// **Und genau das ist der Punkt:** rustls behandelt `None` als „keine Zeit"
/// und bricht mit `Error::FailedToGetCurrentTime` ab, statt die
/// Gueltigkeitspruefung zu ueberspringen. „Die Uhr stimmt nicht, pruefen wir
/// halt nicht" ist in SpeedOS nicht implementierbar — nicht, weil es verboten
/// waere, sondern weil es die Naht nicht hergibt (docs/tls-vertrauen.md §3e).
#[derive(Debug)]
pub struct SpeedUhr;

impl rustls::time_provider::TimeProvider for SpeedUhr {
    fn current_time(&self) -> Option<rustls::pki_types::UnixTime> {
        let sekunden = crate::zeit_geprueft().ok()?;
        Some(rustls::pki_types::UnixTime::since_unix_epoch(
            core::time::Duration::from_secs(sekunden),
        ))
    }
}

// ===========================================================================
// FEHLER — laut und auf Deutsch, ohne Umgehung
// ===========================================================================

/// Was auf dem Weg zu einer verschluesselten Verbindung schiefgehen kann.
///
/// ==========================================================================
/// ES GIBT KEINEN SCHALTER, DER EINEN DIESER FEHLER UEBERGEHT.
///
/// Kein `--unsicher`, kein `--zertifikat-egal`, keinen „trotzdem
/// fortfahren"-Dialog. Das ist eine Entscheidung und keine fehlende Funktion:
/// Ein solcher Schalter wird benutzt, sobald er existiert — erst „nur zum
/// Testen", dann in einem Skript, dann ueberall. Und ein TLS, das man
/// abschalten kann, schuetzt vor dem Angreifer, der genau das provoziert,
/// gar nicht.
///
/// Was es stattdessen gibt: eine Meldung, die SAGT, was los ist. Ein
/// abgelaufenes Zertifikat, ein falscher Name und eine unbekannte Wurzel
/// sind drei verschiedene Lagen mit drei verschiedenen Ursachen, und der
/// Mensch davor soll sie unterscheiden koennen.
/// ==========================================================================
#[derive(Debug)]
pub enum TlsFehler {
    /// Der Transport darunter (Socket, DNS, Frist) — der ABI-Fehlercode.
    Netz(Fehler),
    /// Kein Vertrauensanker geladen. Ohne Wurzeln ist jede Pruefung sinnlos.
    KeineWurzeln,
    /// Der angegebene Name taugt nicht als Servername (SNI verlangt einen
    /// DNS-Namen; eine nackte IP hat kein Zertifikat auf einen Namen).
    KeinServername,
    /// Die Gegenstelle hat die Verbindung mitten im Handshake gekappt.
    HandshakeAbgebrochen,
    /// rustls hat abgelehnt — hier steckt die eigentliche Aussage drin.
    Tls(rustls::Error),
}

impl From<Fehler> for TlsFehler {
    fn from(fehler: Fehler) -> TlsFehler {
        TlsFehler::Netz(fehler)
    }
}

impl TlsFehler {
    /// Die Meldung fuer den Menschen — vollstaendige deutsche Saetze.
    ///
    /// Sie nennt IMMER auch, was das praktisch heisst, denn „UnknownIssuer"
    /// sagt niemandem etwas, der nicht schon weiss, was es heisst.
    pub fn text(&self) -> String {
        use alloc::format;
        use rustls::CertificateError as Z;
        use rustls::Error as E;

        match self {
            TlsFehler::Netz(fehler) => format!(
                "Die Verbindung kam nicht zustande: {}.",
                fehler.text()
            ),
            TlsFehler::KeineWurzeln => String::from(
                "Es ist kein Vertrauensanker geladen (0 Wurzelzertifikate). Ohne \
                 Wurzeln laesst sich kein Serverzertifikat pruefen, und eine \
                 ungepruefte Verbindung ist keine sichere Verbindung. Abhilfe: \
                 tools/ca_bundle_holen.ps1 ausfuehren und neu bauen.",
            ),
            TlsFehler::KeinServername => String::from(
                "Der angegebene Name ist kein gueltiger DNS-Name. TLS braucht ihn \
                 zweimal: als SNI in der Anfrage und zum Abgleich mit dem \
                 Zertifikat. Eine nackte IP-Adresse geht deshalb nicht.",
            ),
            TlsFehler::HandshakeAbgebrochen => String::from(
                "Die Gegenstelle hat die Verbindung mitten im TLS-Handshake \
                 geschlossen. Typisch, wenn dort gar kein TLS lauscht (z. B. ein \
                 http-Server auf dem Port) oder eine Zwischenstelle dazwischenfunkt.",
            ),

            // ---- Die vier Faelle, um die es eigentlich geht ----
            TlsFehler::Tls(E::InvalidCertificate(Z::Expired)) => String::from(
                "ZERTIFIKAT ABGELAUFEN: Die Gueltigkeit des Serverzertifikats ist \
                 verstrichen. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::ExpiredContext { time, not_after })) => format!(
                "ZERTIFIKAT ABGELAUFEN: Es galt nur bis UNIX-Sekunde {}, jetzt ist \
                 es {}. Abgebrochen.",
                not_after.as_secs(),
                time.as_secs()
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidYet)) => String::from(
                "ZERTIFIKAT NOCH NICHT GUELTIG: Sein Gueltigkeitsbeginn liegt in \
                 der Zukunft. Entweder geht die eigene Uhr nach, oder das \
                 Zertifikat wurde vordatiert. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidYetContext { time, not_before })) => {
                format!(
                    "ZERTIFIKAT NOCH NICHT GUELTIG: Es gilt erst ab UNIX-Sekunde {}, \
                     jetzt ist es {}. Abgebrochen.",
                    not_before.as_secs(),
                    time.as_secs()
                )
            }
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidForName)) => String::from(
                "FALSCHER HOSTNAME: Das Zertifikat ist gueltig, gehoert aber zu \
                 einem anderen Namen als dem angefragten. Genau so sieht es aus, \
                 wenn jemand die Verbindung umleitet. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidForNameContext { .. })) => String::from(
                "FALSCHER HOSTNAME: Das Zertifikat nennt diesen Servernamen nicht. \
                 Genau so sieht es aus, wenn jemand die Verbindung umleitet. \
                 Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::UnknownIssuer)) => String::from(
                // KEIN fester Pfad in der Meldung: Der Vertrauensanker liegt
                // je nach Boot auf /platte oder im RAM-VFS, und `holes` nennt
                // den tatsaechlichen Ort ohnehin in seiner Kopfzeile.
                "UNBEKANNTE ZERTIFIZIERUNGSSTELLE: Die Kette endet bei keiner der \
                 geladenen Wurzeln (siehe die Zeile 'Vertrauensanker' oben). \
                 Das ist der Normalfall \
                 bei einem selbst ausgestellten (self-signed) Zertifikat - und \
                 auch der Fall, in dem sich ein Angreifer selbst eines ausstellt. \
                 SpeedOS kann die beiden nicht unterscheiden und lehnt deshalb ab.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::BadSignature)) => String::from(
                "UNGUELTIGE SIGNATUR: Ein Zertifikat der Kette ist nicht korrekt \
                 von seinem angeblichen Aussteller unterschrieben. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::BadEncoding)) => String::from(
                "KAPUTTES ZERTIFIKAT: Die Gegenstelle hat etwas geschickt, das sich \
                 nicht als X.509 lesen laesst. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(Z::Revoked)) => String::from(
                "ZERTIFIKAT GESPERRT: Es steht auf einer Sperrliste. Abgebrochen.",
            ),
            TlsFehler::Tls(E::InvalidCertificate(sonst)) => format!(
                "ZERTIFIKAT ABGELEHNT: {:?}. Abgebrochen.",
                sonst
            ),
            TlsFehler::Tls(E::NoCertificatesPresented) => String::from(
                "Die Gegenstelle hat ueberhaupt kein Zertifikat vorgelegt. Es gibt \
                 also nichts zu pruefen, und damit auch nichts zu vertrauen.",
            ),

            // ---- Uhr ----
            TlsFehler::Tls(E::FailedToGetCurrentTime) => String::from(
                "DIE UHR IST NACHWEISLICH FALSCH - deshalb wurde NICHT verbunden. \
                 Ohne verlaessliche Zeit laesst sich nicht sagen, ob ein Zertifikat \
                 abgelaufen ist; die Pruefung dann zu ueberspringen waere der Punkt, \
                 an dem TLS aufhoert, etwas wert zu sein. Abhilfe: Uhr stellen \
                 (Einstellungen -> Zeit) und erneut versuchen.",
            ),
            TlsFehler::Tls(E::FailedToGetRandomBytes) => String::from(
                "KEIN ZUFALL: Der Entropie-Pool des Kernels ist noch nicht gesaet. \
                 Ein Handshake mit geratenem Zufall waere schlimmer als keiner - \
                 deshalb abgebrochen. Ein paar Sekunden Tastatur-/Maus-/Netzverkehr \
                 helfen (docs/zufall.md).",
            ),

            // ---- Protokoll ----
            TlsFehler::Tls(E::AlertReceived(alarm)) => format!(
                "DIE GEGENSTELLE HAT ABGELEHNT (TLS-Alarm {:?}). Der Fehler liegt \
                 also auf der anderen Seite - haeufig, weil sie keine unserer drei \
                 Ciphersuites oder keine unserer Protokollversionen mag.",
                alarm
            ),
            TlsFehler::Tls(E::InvalidMessage(was)) => format!(
                "PROTOKOLLFEHLER: Die Gegenstelle hat etwas geschickt, das kein \
                 gueltiger TLS-Datensatz ist ({:?}). Sehr wahrscheinlich spricht \
                 dort gar kein TLS.",
                was
            ),
            TlsFehler::Tls(E::PeerIncompatible(was)) => format!(
                "UNVEREINBAR: Die Gegenstelle kann nichts, was wir auch koennen \
                 ({:?}).",
                was
            ),
            TlsFehler::Tls(E::PeerMisbehaved(was)) => format!(
                "DIE GEGENSTELLE HAELT SICH NICHT AN DAS PROTOKOLL ({:?}). \
                 Abgebrochen.",
                was
            ),
            TlsFehler::Tls(E::DecryptError) => String::from(
                "ENTSCHLUESSELN FEHLGESCHLAGEN: Ein Datensatz liess sich nicht \
                 authentifizieren. Entweder wurde unterwegs manipuliert, oder die \
                 Gegenstelle hat sich verrechnet. Abgebrochen.",
            ),
            TlsFehler::Tls(sonst) => format!("TLS-Fehler: {}", sonst),
        }
    }

    /// Ein kurzes Schlagwort fuer Tests und Protokolle.
    pub fn kurz(&self) -> &'static str {
        use rustls::CertificateError as Z;
        use rustls::Error as E;
        match self {
            TlsFehler::Netz(_) => "netz",
            TlsFehler::KeineWurzeln => "keine-wurzeln",
            TlsFehler::KeinServername => "kein-servername",
            TlsFehler::HandshakeAbgebrochen => "handshake-abgebrochen",
            TlsFehler::Tls(E::InvalidCertificate(Z::Expired))
            | TlsFehler::Tls(E::InvalidCertificate(Z::ExpiredContext { .. })) => "abgelaufen",
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidYet))
            | TlsFehler::Tls(E::InvalidCertificate(Z::NotValidYetContext { .. })) => {
                "noch-nicht-gueltig"
            }
            TlsFehler::Tls(E::InvalidCertificate(Z::NotValidForName))
            | TlsFehler::Tls(E::InvalidCertificate(Z::NotValidForNameContext { .. })) => {
                "falscher-hostname"
            }
            TlsFehler::Tls(E::InvalidCertificate(Z::UnknownIssuer)) => "unbekannte-ca",
            TlsFehler::Tls(E::InvalidCertificate(_)) => "zertifikat",
            TlsFehler::Tls(E::FailedToGetCurrentTime) => "uhr-unplausibel",
            TlsFehler::Tls(E::FailedToGetRandomBytes) => "kein-zufall",
            TlsFehler::Tls(E::AlertReceived(_)) => "alarm",
            TlsFehler::Tls(E::InvalidMessage(_)) => "protokoll",
            TlsFehler::Tls(_) => "tls",
        }
    }
}

// ===========================================================================
// DER VERTRAUENSANKER
// ===========================================================================

/// Das Ergebnis des Wurzel-Ladens.
pub struct Wurzelbestand {
    /// Wie viele PEM-Bloecke unser Parser lesen konnte.
    pub gelesen: usize,
    /// Wie viele davon rustls-webpki als Wurzel akzeptiert hat.
    pub uebernommen: usize,
    /// Wie viele Bloecke unser PEM-Parser verworfen hat.
    pub kaputt: usize,
}

/// Laedt die Wurzelzertifikate aus einem PEM-Puffer in den rustls-Speicher.
///
/// `arbeitspuffer` muss mindestens `pem::MAX_DER_BYTES` gross sein. Der
/// Aufrufer haelt beide Puffer selbst, damit die Heap-Messung eines
/// TLS-Programms den TLS-Bedarf misst und nicht das Einlesen einer Datei.
///
/// **Ein kaputter Block macht nur diesen Block ungueltig** (siehe pem.rs) —
/// und `uebernommen` kann kleiner sein als `gelesen`, weil rustls-webpki
/// strenger prueft als unser Anzeige-Parser. Das ist richtig so.
pub fn wurzeln_laden(
    pem_datei: &[u8],
    arbeitspuffer: &mut [u8],
    speicher: &mut rustls::RootCertStore,
) -> Wurzelbestand {
    let mut uebernommen = 0usize;
    let bestand = crate::pem::bloecke_durchgehen(pem_datei, arbeitspuffer, |block| {
        // `to_vec`, weil rustls die DER-Bytes besitzen will und der
        // Arbeitspuffer beim naechsten Block ueberschrieben wird.
        let zert = rustls::pki_types::CertificateDer::from(block.der.to_vec());
        if speicher.add(zert).is_ok() {
            uebernommen += 1;
        }
    });
    Wurzelbestand {
        gelesen: bestand.gelesen,
        uebernommen,
        kaputt: bestand.kaputt,
    }
}

/// Baut die ClientConfig: RustCrypto als Anbieter, SpeedUhr als Zeitquelle,
/// die geladenen Wurzeln als einzige Vertrauensgrundlage, keine
/// Klient-Authentifizierung.
///
/// `builder_with_details` statt `builder()`: Ohne `std` sind `builder()` und
/// `builder_with_provider()` weg, und die Zeit ist dann ein PFLICHT-Argument
/// (docs/tls-entscheidung.md §4). Das ist ein Glueckfall — man KANN die
/// Zeitquelle nicht vergessen.
pub fn konfig_bauen(wurzeln: rustls::RootCertStore) -> Result<Arc<rustls::ClientConfig>, TlsFehler> {
    if wurzeln.is_empty() {
        return Err(TlsFehler::KeineWurzeln);
    }
    let anbieter = Arc::new(rustls_rustcrypto::provider());
    let konfig = rustls::ClientConfig::builder_with_details(anbieter, Arc::new(SpeedUhr))
        .with_safe_default_protocol_versions()
        .map_err(TlsFehler::Tls)?
        .with_root_certificates(wurzeln)
        .with_no_client_auth();
    Ok(Arc::new(konfig))
}

// ===========================================================================
// DER TLS-STROM
// ===========================================================================

/// Groesster TLS-Klartextblock je Datensatz (RFC 8446: 2^14).
const MAX_KLARTEXT_JE_SATZ: usize = 16 * 1024;
/// Anfangsgroesse der beiden TLS-Puffer. Ein Datensatz plus Reserve.
const PUFFER_START: usize = 18 * 1024;
/// Obergrenze der beiden TLS-Puffer. Ein Datensatz kann laut RFC nicht
/// groesser werden; die Reserve faengt Ketten und Ticket-Nachrichten ab.
const PUFFER_MAX: usize = 96 * 1024;

/// Was ein Takt der Zustandsmaschine ergeben hat — und was DANACH zu tun ist.
///
/// Warum ueberhaupt ein Zwischenschritt: `process_tls_records` leiht sich den
/// Eingangspuffer AUS, und der geliehene Zustand lebt bis zum Ende des
/// `match`. Solange darf niemand in denselben Puffer lesen. Also merkt sich
/// der Takt nur, WAS zu tun ist; getan wird es, wenn die Leihe vorbei ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aktion {
    /// Es ging voran — sofort noch ein Takt.
    Weiter,
    /// Der Ausgangspuffer will raus.
    Senden,
    /// rustls braucht mehr Bytes von der Gegenstelle.
    Lesen,
    /// Der Ausgangspuffer ist zu klein.
    Vergroessern(usize),
    /// Handshake steht, nichts zu tun.
    Bereit,
    /// Die Gegenstelle hat sauber zugemacht.
    Ende,
}

/// Ein TLS-gesicherter Byte-Strom ueber die Socket-Syscalls.
///
/// ==========================================================================
/// WARUM DAS PROGRAMMIERMODELL SO AUSSIEHT
///
/// Ohne `std` gibt es in rustls keine `ClientConnection` mit `Read`/`Write`,
/// sondern nur `UnbufferedClientConnection`: eine Zustandsmaschine, die man
/// SELBST dreht und deren Puffer man SELBST haelt. Der Ablauf ist immer
/// derselbe —
///
///     process_tls_records(eingang)
///        -> EncodeTlsData     : rustls will Bytes SCHREIBEN -> in `aus`
///        -> TransmitTlsData   : `aus` jetzt ueber TCP rausschicken
///        -> BlockedHandshake  : rustls will Bytes LESEN -> TCP -> `ein`
///        -> WriteTraffic      : Handshake steht, Nutzdaten duerfen raus
///        -> ReadTraffic       : entschluesselte Nutzdaten liegen an
///        -> PeerClosed/Closed : Feierabend
///
/// — und jeder Durchlauf sagt zusaetzlich, wie viele Bytes vorne aus `ein`
/// zu verwerfen sind. Genau das macht `takt()`.
///
/// Nach aussen sieht davon nichts durch: `lesen`/`schreiben` verhalten sich
/// wie beim `TcpStrom` darunter. Das ist der Sinn der Uebung — der
/// HTTP-Parser soll nicht merken, worauf er sitzt.
/// ==========================================================================
pub struct TlsStrom {
    tcp: TcpStrom,
    conn: rustls::client::UnbufferedClientConnection,
    /// Verschluesselte Bytes VON der Gegenstelle, noch nicht verarbeitet.
    ein: Vec<u8>,
    ein_belegt: usize,
    /// Verschluesselte Bytes AN die Gegenstelle, noch nicht gesendet.
    aus: Vec<u8>,
    aus_belegt: usize,
    /// Entschluesselte Nutzdaten, die `lesen` noch nicht abgeholt hat.
    klartext: Vec<u8>,
    klartext_ab: usize,
    /// Klartext, den `schreiben` noch verschluesseln lassen muss.
    sendeschlange: Vec<u8>,
    sende_ab: usize,
    /// Hat die Gegenstelle sauber (close_notify) oder auf TCP-Ebene zugemacht?
    peer_zu: bool,
    tcp_ende: bool,
    /// Wie lange der Handshake gedauert hat (ms) — die Messzahl aus Aufgabe 4.
    handshake_ms: u64,
    /// Frist fuer eine einzelne Lese-/Schreib-Operation.
    pub frist_ms: u64,
}

impl TlsStrom {
    /// Baut die TLS-Verbindung auf: SNI setzen, Handshake fuehren,
    /// Zertifikatskette pruefen lassen.
    ///
    /// **Die Pruefung ist nicht abschaltbar** — sie steckt in der
    /// `ClientConfig`, die `konfig_bauen` mit `with_root_certificates`
    /// erzeugt hat, und diese Funktion nimmt keine Konfiguration entgegen,
    /// die daran etwas aendern koennte. `servername` wird von rustls
    /// gleichzeitig als SNI verschickt UND gegen die Namen im Zertifikat
    /// abgeglichen; es gibt keinen Weg, nur eins von beiden zu tun.
    pub fn verbinden(
        tcp: TcpStrom,
        konfig: Arc<rustls::ClientConfig>,
        servername: &str,
    ) -> Result<TlsStrom, TlsFehler> {
        let name = rustls::pki_types::ServerName::try_from(servername)
            .map_err(|_| TlsFehler::KeinServername)?
            .to_owned();
        let conn = rustls::client::UnbufferedClientConnection::new(konfig, name)
            .map_err(TlsFehler::Tls)?;

        let mut strom = TlsStrom {
            frist_ms: tcp.frist_ms,
            tcp,
            conn,
            ein: alloc::vec![0u8; PUFFER_START],
            ein_belegt: 0,
            aus: alloc::vec![0u8; PUFFER_START],
            aus_belegt: 0,
            klartext: Vec::new(),
            klartext_ab: 0,
            sendeschlange: Vec::new(),
            sende_ab: 0,
            peer_zu: false,
            tcp_ende: false,
            handshake_ms: 0,
        };

        let start = crate::zeit_jetzt();
        strom.handshake_fuehren()?;
        strom.handshake_ms = crate::zeit_jetzt() - start;
        Ok(strom)
    }

    /// Dreht die Zustandsmaschine, bis der Handshake steht.
    fn handshake_fuehren(&mut self) -> Result<(), TlsFehler> {
        let frist = crate::zeit_jetzt() + self.frist_ms;
        loop {
            match self.takt()? {
                Aktion::Bereit => return Ok(()),
                Aktion::Ende => return Err(TlsFehler::HandshakeAbgebrochen),
                _ => {}
            }
            if crate::zeit_jetzt() >= frist {
                return Err(TlsFehler::Netz(Fehler::ZEITUEBERSCHREITUNG));
            }
        }
    }

    /// EIN Durchlauf der Zustandsmaschine, inklusive der Folge-Arbeit.
    fn takt(&mut self) -> Result<Aktion, TlsFehler> {
        let mut verwerfen;
        let aktion;

        // --- Der geliehene Teil: hier darf `self.ein` nicht angefasst werden ---
        {
            let belegt = self.ein_belegt;
            let status = self.conn.process_tls_records(&mut self.ein[..belegt]);
            verwerfen = status.discard;
            let zustand = match status.state {
                Ok(zustand) => zustand,
                Err(fehler) => return Err(TlsFehler::Tls(fehler)),
            };
            aktion = match zustand {
                ConnectionState::EncodeTlsData(mut schritt) => {
                    match schritt.encode(&mut self.aus[self.aus_belegt..]) {
                        Ok(n) => {
                            self.aus_belegt += n;
                            Aktion::Weiter
                        }
                        Err(EncodeError::InsufficientSize(zu_klein)) => {
                            Aktion::Vergroessern(self.aus_belegt + zu_klein.required_size)
                        }
                        // Kann nur passieren, wenn wir `encode` zweimal auf
                        // demselben Schritt rufen — tun wir nicht.
                        Err(EncodeError::AlreadyEncoded) => {
                            return Err(TlsFehler::Tls(rustls::Error::General(String::from(
                                "encode zweimal gerufen",
                            ))))
                        }
                    }
                }
                ConnectionState::TransmitTlsData(schritt) => {
                    // `done()` setzt nur ein Flag; GESENDET wird gleich, wenn
                    // die Leihe vorbei ist. Scheitert das Senden, brechen wir
                    // ohnehin ab — der Zustand ist dann egal.
                    schritt.done();
                    Aktion::Senden
                }
                ConnectionState::BlockedHandshake => Aktion::Lesen,
                ConnectionState::ReadTraffic(mut leser) => {
                    while let Some(satz) = leser.next_record() {
                        let satz = satz.map_err(TlsFehler::Tls)?;
                        verwerfen += satz.discard;
                        self.klartext.extend_from_slice(satz.payload);
                    }
                    Aktion::Weiter
                }
                ConnectionState::WriteTraffic(mut schreiber) => {
                    if self.sende_ab < self.sendeschlange.len() {
                        let rest = &self.sendeschlange[self.sende_ab..];
                        let stueck = rest.len().min(MAX_KLARTEXT_JE_SATZ);
                        match schreiber.encrypt(&rest[..stueck], &mut self.aus[self.aus_belegt..]) {
                            Ok(n) => {
                                self.aus_belegt += n;
                                self.sende_ab += stueck;
                                Aktion::Senden
                            }
                            Err(EncryptError::InsufficientSize(zu_klein)) => {
                                Aktion::Vergroessern(self.aus_belegt + zu_klein.required_size)
                            }
                            Err(EncryptError::EncryptExhausted) => {
                                return Err(TlsFehler::Tls(rustls::Error::EncryptError))
                            }
                        }
                    } else {
                        Aktion::Bereit
                    }
                }
                ConnectionState::PeerClosed => {
                    self.peer_zu = true;
                    Aktion::Weiter
                }
                ConnectionState::Closed => {
                    self.peer_zu = true;
                    Aktion::Ende
                }
                // `ReadEarlyData` gibt es beim Klienten nicht; `#[non_exhaustive]`
                // verlangt trotzdem einen Zweig. Nichts tun und weiterdrehen ist
                // die einzige Antwort, die nichts kaputtmacht.
                _ => Aktion::Weiter,
            };
        }

        // --- Die Leihe ist vorbei: aufraeumen und handeln ---
        if verwerfen > 0 {
            let belegt = self.ein_belegt;
            let verwerfen = verwerfen.min(belegt);
            self.ein.copy_within(verwerfen..belegt, 0);
            self.ein_belegt -= verwerfen;
        }

        match aktion {
            Aktion::Senden => {
                if self.aus_belegt > 0 {
                    let bis = self.aus_belegt;
                    self.tcp.schreiben(&self.aus[..bis])?;
                    self.aus_belegt = 0;
                }
                Ok(Aktion::Weiter)
            }
            Aktion::Vergroessern(noetig) => {
                if noetig > PUFFER_MAX {
                    // Sollte nach RFC nicht vorkommen — dann lieber ein
                    // klarer Fehler als ein wachsender Puffer ohne Ende.
                    return Err(TlsFehler::Tls(rustls::Error::General(String::from(
                        "TLS-Ausgangspuffer waere groesser als erlaubt",
                    ))));
                }
                // Erst leeren: Oft reicht der Platz danach schon.
                if self.aus_belegt > 0 {
                    let bis = self.aus_belegt;
                    self.tcp.schreiben(&self.aus[..bis])?;
                    self.aus_belegt = 0;
                }
                if noetig > self.aus.len() {
                    self.aus.resize(noetig.min(PUFFER_MAX), 0);
                }
                Ok(Aktion::Weiter)
            }
            Aktion::Lesen => {
                self.nachfuellen()?;
                Ok(Aktion::Weiter)
            }
            sonst => Ok(sonst),
        }
    }

    /// Holt Bytes von der Gegenstelle in den Eingangspuffer.
    fn nachfuellen(&mut self) -> Result<(), TlsFehler> {
        if self.tcp_ende {
            // Nichts mehr zu holen — und rustls will trotzdem mehr. Das ist
            // ein ABGESCHNITTENER Strom.
            return Err(TlsFehler::HandshakeAbgebrochen);
        }
        if self.ein_belegt == self.ein.len() {
            if self.ein.len() >= PUFFER_MAX {
                return Err(TlsFehler::Tls(rustls::Error::General(String::from(
                    "TLS-Eingangspuffer voll",
                ))));
            }
            let neu = (self.ein.len() * 2).min(PUFFER_MAX);
            self.ein.resize(neu, 0);
        }
        let ab = self.ein_belegt;
        self.tcp.frist_ms = self.frist_ms;
        match self.tcp.lesen(&mut self.ein[ab..])? {
            0 => {
                self.tcp_ende = true;
                Ok(())
            }
            n => {
                self.ein_belegt += n;
                Ok(())
            }
        }
    }

    /// BLOCKIEREND lesen. `Ok(0)` heisst Ende des Stroms.
    ///
    /// ==========================================================================
    /// EHRLICHE GRENZE: Ein Strom kann auf ZWEI Arten enden — sauber mit
    /// `close_notify` (dann liefert rustls `PeerClosed`) oder dadurch, dass die
    /// Gegenstelle die TCP-Verbindung einfach zumacht. Der zweite Fall ist von
    /// einem abgeschnittenen Strom (Truncation-Angriff) nicht zu unterscheiden.
    /// SpeedOS behandelt ihn als Ende und meldet es NICHT als Fehler — weil
    /// sonst die halbe Welt unerreichbar waere; sehr viele Server schliessen
    /// bei `Connection: close` ohne close_notify.
    ///
    /// Was davor schuetzt, liegt eine Schicht hoeher: Der HTTP-Parser prueft
    /// den Rumpf gegen `Content-Length` bzw. den abschliessenden 0-Chunk und
    /// meldet `UnvollstaendigeAntwort`, wenn etwas fehlt. Ein abgeschnittener
    /// Download faellt also auf, nur eben dort und nicht hier.
    /// ==========================================================================
    pub fn lesen(&mut self, ziel: &mut [u8]) -> Result<usize, TlsFehler> {
        if ziel.is_empty() {
            return Ok(0);
        }
        let frist = crate::zeit_jetzt() + self.frist_ms;
        loop {
            // Liegt schon Klartext bereit?
            if self.klartext_ab < self.klartext.len() {
                let rest = &self.klartext[self.klartext_ab..];
                let nehmen = rest.len().min(ziel.len());
                ziel[..nehmen].copy_from_slice(&rest[..nehmen]);
                self.klartext_ab += nehmen;
                if self.klartext_ab == self.klartext.len() {
                    self.klartext.clear();
                    self.klartext_ab = 0;
                }
                return Ok(nehmen);
            }
            if self.peer_zu {
                return Ok(0);
            }
            match self.takt() {
                Ok(Aktion::Ende) => return Ok(0),
                Ok(Aktion::Bereit) => {
                    // Handshake steht, nichts zu senden: Es fehlen Bytes von
                    // der Gegenstelle.
                    if self.tcp_ende {
                        return Ok(0);
                    }
                    self.nachfuellen()?;
                }
                Ok(_) => {}
                // Ein TCP-Ende mitten in einem TLS-Datensatz ist hier kein
                // Fehler, sondern das Ende (siehe Kopfkommentar).
                Err(TlsFehler::HandshakeAbgebrochen) if self.tcp_ende => return Ok(0),
                Err(fehler) => return Err(fehler),
            }
            if crate::zeit_jetzt() >= frist {
                return Err(TlsFehler::Netz(Fehler::ZEITUEBERSCHREITUNG));
            }
        }
    }

    /// BLOCKIEREND schreiben — verschluesselt und sendet ALLES.
    pub fn schreiben(&mut self, daten: &[u8]) -> Result<(), TlsFehler> {
        if daten.is_empty() {
            return Ok(());
        }
        self.sendeschlange.clear();
        self.sendeschlange.extend_from_slice(daten);
        self.sende_ab = 0;

        let frist = crate::zeit_jetzt() + self.frist_ms;
        while self.sende_ab < self.sendeschlange.len() {
            match self.takt()? {
                Aktion::Ende => return Err(TlsFehler::Netz(Fehler::NICHT_VERBUNDEN)),
                Aktion::Lesen | Aktion::Bereit => {}
                _ => {}
            }
            if crate::zeit_jetzt() >= frist {
                return Err(TlsFehler::Netz(Fehler::ZEITUEBERSCHREITUNG));
            }
        }
        self.sendeschlange.clear();
        self.sende_ab = 0;
        Ok(())
    }

    // --- Auskunft (fuer `holes --info`) ---

    /// Wie lange der Handshake gedauert hat, in Millisekunden.
    pub fn handshake_ms(&self) -> u64 {
        self.handshake_ms
    }

    /// Die ausgehandelte Protokollversion als Text.
    pub fn protokoll_text(&self) -> &'static str {
        use rustls::ProtocolVersion as V;
        match self.conn.protocol_version() {
            Some(V::TLSv1_3) => "TLS 1.3",
            Some(V::TLSv1_2) => "TLS 1.2",
            Some(_) => "andere (von uns nicht angeboten)",
            None => "noch nicht ausgehandelt",
        }
    }

    /// Die ausgehandelte Ciphersuite als Text (der RFC-Name).
    pub fn ciphersuite_text(&self) -> String {
        use alloc::format;
        match self.conn.negotiated_cipher_suite() {
            Some(suite) => format!("{:?}", suite.suite()),
            None => String::from("noch nicht ausgehandelt"),
        }
    }

    /// Die Zertifikatskette, wie die Gegenstelle sie vorgelegt hat
    /// (Server-Zertifikat zuerst, dann die Zwischenstellen).
    ///
    /// Sie liegt hier NUR zur Anzeige. Geprueft hat sie rustls-webpki, bevor
    /// dieser Strom ueberhaupt entstanden ist — waere sie nicht in Ordnung,
    /// gaebe es kein `TlsStrom`-Objekt, an dem man sie abfragen koennte.
    pub fn kette(&self) -> &[rustls::pki_types::CertificateDer<'static>] {
        self.conn.peer_certificates().unwrap_or(&[])
    }

    /// Sagt der Gegenstelle geordnet Auf Wiedersehen (close_notify).
    ///
    /// Fehler werden hier BEWUSST geschluckt: Wir sind fertig, die Antwort
    /// ist vollstaendig geparst, und ob der Abschiedsgruss noch ankommt,
    /// aendert daran nichts.
    pub fn schliessen(&mut self) {
        let belegt = self.ein_belegt;
        let status = self.conn.process_tls_records(&mut self.ein[..belegt]);
        if let Ok(ConnectionState::WriteTraffic(mut schreiber)) = status.state {
            if let Ok(n) = schreiber.queue_close_notify(&mut self.aus[self.aus_belegt..]) {
                self.aus_belegt += n;
            }
        }
        if self.aus_belegt > 0 {
            let bis = self.aus_belegt;
            let _ = self.tcp.schreiben(&self.aus[..bis]);
            self.aus_belegt = 0;
        }
    }
}
