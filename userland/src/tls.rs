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

use crate::Fehler;

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
