// netz/http.rs — Der HTTP-Client des KERNELS: Transport über die Socket-API
//
// ==========================================================================
// SEIT SERIE 7, TEIL 4 STEHT DER PARSER NICHT MEHR HIER
//
// Die reine Protokoll-Logik (URL zerlegen, Antwort in Kopf und Rumpf
// zerlegen, chunked dekodieren, Anfrage bauen) ist in die eigene Kiste
// `speedhttp/` umgezogen — ZEILE FUER ZEILE unveraendert. Was hier
// zurueckgeblieben ist, ist genau das, was sie NICHT enthaelt: der
// TRANSPORT. DNS auflösen, Socket öffnen, verbinden, pumpen, Bytes
// einsammeln.
//
// WARUM: Weil es einen zweiten Transport gibt. `userland/holes` fährt
// denselben Parser über einen TLS-Strom in Ring 3 (Serie 7, Teil 4). Ein
// Parser, der beides bedient, ohne angefasst zu werden, ist der Beweis, dass
// die Schichtgrenze an der richtigen Stelle liegt.
//
// DER BELEG STEHT UNTEN: Die `#[test_case]`-Tests am Ende dieser Datei sind
// dieselben wie in Serie 5. Sie prüfen jetzt den Code in `speedhttp` — über
// das `pub use` gleich hier drunter, ohne eine einzige geänderte Zeile.
//
// ==========================================================================
// Der Netz-Teil nutzt AUSSCHLIESSLICH die Socket-API (netz::socket) — kein
// Griff in tcp::Verbindung. Genau dafür ist die Fassade da.
//
// NUR http:// — dieser Klient hier bekommt KEIN TLS, und das ist Absicht:
// TLS lebt in Ring 3 (docs/tls-entscheidung.md), damit ein Fehler in 30k
// Zeilen Fremdcode einen Prozess trifft und nicht den Kernel. Eine
// https-URL wird deshalb sauber mit `TlsNichtUnterstuetzt` abgelehnt; die
// Shell verweist dann auf `starte holes <url>`.

use super::dns::{self, DnsFehler};
use super::socket::{self, Handle, SocketFehler, SocketTyp, Verbindungszustand};
use super::Ipv4;
use alloc::string::ToString;
use alloc::vec::Vec;

/// DER PARSER — unveraendert, nur woanders zu Hause (siehe Kopfkommentar).
///
/// Das `pub use` haelt jede bisherige Verwendung am Leben:
/// `http::Antwort`, `http::url_parsen`, `http::HttpFehler::KaputterKopf` …
/// heissen weiterhin genau so.
pub use speedhttp::*;

/// Zeitlimit für Verbindungsaufbau + Transfer.
const TIMEOUT_MS: u64 = 15_000;

/// Was beim HOLEN schiefgehen kann — Protokoll ODER Transport.
///
/// ==========================================================================
/// WARUM DAS EIN ZWEITER TYP IST UND NICHT MEHR EINER
///
/// Bis Serie 5 hatte `HttpFehler` auch die Varianten `Dns(..)` und
/// `Socket(..)`. Die konnten nicht mit in die Parser-Kiste umziehen: Sie
/// tragen Kernel-Typen, und ein Parser, der einen Socket-Fehler kennt, ist
/// kein transportfreier Parser mehr — dann haette `userland/holes` ihn nicht
/// benutzen koennen.
///
/// Also die saubere Trennung: `HttpFehler` = was am PROTOKOLL scheitert
/// (kommt aus `speedhttp`), `KlientFehler` = das plus, was am WEG scheitert.
/// Ring 3 hat sein eigenes Gegenstueck dazu (`libspeed::Fehler`) — und genau
/// so soll es sein, denn dort ist der Weg ein anderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KlientFehler {
    /// Das Protokoll selbst (URL, Kopf, Rumpf, Weiterleitungen).
    Http(HttpFehler),
    Dns(DnsFehler),
    Socket(SocketFehler),
    /// **Der Weg geht weiter, aber nur mit TLS.** Enthält die ausgerechnete
    /// absolute https-Adresse.
    ///
    /// ==================================================================
    /// WARUM DAS EIN EIGENER FALL IST UND KEIN FEHLER
    ///
    /// http -> https ist im heutigen Web der NORMALFALL: Fast jeder Server
    /// antwortet auf eine http-Anfrage mit `301 Location: https://…`. Für
    /// den Kernel-Klienten ist dort Endstation — er hat kein TLS, und das
    /// soll auch so bleiben (docs/tls-entscheidung.md).
    ///
    /// Aber „geht nicht" wäre die falsche Antwort, denn SpeedOS KANN das
    /// ja: in Ring 3. Also gibt der Klient das ausgerechnete Ziel heraus,
    /// und die Shell reicht es an `holes` weiter. Der Benutzer merkt vom
    /// Wechsel nur, dass in der Ausgabe eine Zeile mehr steht.
    /// ==================================================================
    BrauchtTls(alloc::string::String),
}

impl KlientFehler {
    pub fn meldung(&self) -> &'static str {
        match self {
            KlientFehler::Http(f) => f.meldung(),
            KlientFehler::Dns(f) => f.meldung(),
            KlientFehler::Socket(f) => f.meldung(),
            KlientFehler::BrauchtTls(_) => "die Gegenstelle leitet auf https weiter",
        }
    }

    /// Ist das die https-Absage? Die Shell hängt daran ihre Übergabe an
    /// `holes` — der Parser selbst kennt `holes` nicht und soll es auch nicht.
    pub fn ist_tls_absage(&self) -> bool {
        matches!(
            self,
            KlientFehler::Http(HttpFehler::TlsNichtUnterstuetzt) | KlientFehler::BrauchtTls(_)
        )
    }

    /// Die https-Adresse, auf die weitergeleitet wurde (falls es eine gibt).
    pub fn tls_ziel(&self) -> Option<&str> {
        match self {
            KlientFehler::BrauchtTls(url) => Some(url.as_str()),
            _ => None,
        }
    }
}

impl From<HttpFehler> for KlientFehler {
    fn from(fehler: HttpFehler) -> KlientFehler {
        KlientFehler::Http(fehler)
    }
}

// ---------------------------------------------------------------------------
// Der Netz-Teil: über die Socket-API holen
// ---------------------------------------------------------------------------

/// Führt EINE Anfrage über einen frischen TCP-Socket aus und liefert die
/// rohen Antwort-Bytes.
fn roh_ueber_socket(h: Handle, url: &Url, ip: Ipv4) -> Result<Vec<u8>, KlientFehler> {
    socket::verbinden(h, ip, url.port).map_err(KlientFehler::Socket)?;
    let frist = crate::zeit::ms_seit_boot() + TIMEOUT_MS;

    // 1. Auf den Handshake warten (der Stack wird dabei gepumpt).
    loop {
        super::pumpen();
        match socket::zustand(h).map_err(KlientFehler::Socket)? {
            Verbindungszustand::Verbunden => break,
            Verbindungszustand::Geschlossen => {
                return Err(KlientFehler::Socket(SocketFehler::Abgebrochen))
            }
            _ => {}
        }
        if crate::zeit::ms_seit_boot() >= frist {
            return Err(KlientFehler::Socket(SocketFehler::Zeitueberschreitung));
        }
        crate::zeit::warte_auf_interrupt();
    }

    // 2. Anfrage senden.
    let anfrage = anfrage_bauen(url);
    socket::senden(h, anfrage.as_bytes()).map_err(KlientFehler::Socket)?;

    // 3. Antwort lesen, bis die Gegenstelle schließt.
    let mut roh = Vec::new();
    let mut puffer = [0u8; 1460];
    loop {
        super::pumpen();
        loop {
            let n = socket::empfangen(h, &mut puffer).map_err(KlientFehler::Socket)?;
            if n == 0 {
                break;
            }
            if roh.len() + n > MAX_ANTWORT {
                return Err(KlientFehler::Http(HttpFehler::ZuGross));
            }
            roh.extend_from_slice(&puffer[..n]);
        }
        let fertig = matches!(
            socket::zustand(h).map_err(KlientFehler::Socket)?,
            Verbindungszustand::PeerHatGeschlossen | Verbindungszustand::Geschlossen
        );
        if fertig {
            // Den Rest aus dem Empfangspuffer nachholen.
            loop {
                let n = socket::empfangen(h, &mut puffer).map_err(KlientFehler::Socket)?;
                if n == 0 {
                    break;
                }
                roh.extend_from_slice(&puffer[..n]);
            }
            break;
        }
        if crate::zeit::ms_seit_boot() >= frist {
            break; // was da ist, ist da — antwort_parsen entscheidet
        }
        crate::zeit::warte_auf_interrupt();
    }
    Ok(roh)
}

/// Holt die rohen Antwort-Bytes für eine URL (DNS + Socket + Abbau).
fn roh_holen(url: &Url) -> Result<Vec<u8>, KlientFehler> {
    let ip = dns::aufloesen(&url.host).map_err(KlientFehler::Dns)?;
    let h = socket::oeffnen(SocketTyp::Tcp).map_err(KlientFehler::Socket)?;
    let ergebnis = roh_ueber_socket(h, url, ip);
    // Immer sauber schließen — auch im Fehlerfall.
    let _ = socket::schliessen(h);
    // Den geordneten Abbau (FIN/ACK) noch zu Ende pumpen.
    for _ in 0..60 {
        super::pumpen();
        crate::zeit::warte_auf_interrupt();
    }
    ergebnis
}

/// DER Einstieg: holt eine http-URL und folgt dabei Weiterleitungen (bis
/// `MAX_WEITERLEITUNGEN`). Liefert die ENDGÜLTIGE URL samt Antwort.
///
/// Führt eine Weiterleitung auf **https**, endet der Abruf mit
/// `KlientFehler::BrauchtTls` und der ausgerechneten Zieladresse — siehe
/// dort, warum das kein Fehler ist, sondern eine Übergabe.
pub fn holen(url_text: &str) -> Result<(Url, Antwort), KlientFehler> {
    let mut url = url_parsen(url_text)?;
    let mut verbleibend = MAX_WEITERLEITUNGEN;
    loop {
        let roh = roh_holen(&url)?;
        let antwort = antwort_parsen(&roh)?;
        // 3xx mit Location -> weiterleiten.
        if (300..400).contains(&antwort.status) {
            if let Some(ort) = antwort.header_wert("location") {
                if verbleibend == 0 {
                    return Err(KlientFehler::Http(HttpFehler::ZuVieleWeiterleitungen));
                }
                let ort = ort.to_string();
                url = match naechste_url(&url, &ort) {
                    Ok(naechste) => naechste,
                    // Die https-Weiterleitung: nicht abbrechen, sondern das
                    // Ziel ausrechnen und dem Aufrufer geben. `naechstes_ziel`
                    // kann das Schema — `naechste_url` (Serie 5) bewusst nicht.
                    Err(HttpFehler::TlsNichtUnterstuetzt) => {
                        let basis = Ziel {
                            tls: false,
                            url: url.clone(),
                        };
                        let ziel = naechstes_ziel(&basis, &ort)?;
                        return Err(KlientFehler::BrauchtTls(ziel.als_text()));
                    }
                    Err(fehler) => return Err(KlientFehler::Http(fehler)),
                };
                verbleibend -= 1;
                continue;
            }
        }
        return Ok((url, antwort));
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------
//
// ===========================================================================
// DIESE TESTS SIND DER BEWEIS AUS SERIE 7, TEIL 4 — NICHT ANFASSEN
//
// Sie stammen unveraendert aus Serie 5, aus der Zeit, als der Parser noch in
// dieser Datei stand. Heute pruefen sie den Code in `speedhttp/` (ueber das
// `pub use speedhttp::*` oben), und zwar OHNE dass eine Zeile an ihnen
// geaendert werden musste. Genau das ist die Aussage: Derselbe Parser
// bedient den Kernel-Transport (TCP-Socket) und den Ring-3-Transport (TLS,
// `userland/holes`).
//
// Wer sie anpasst, weil "die Kiste ja jetzt woanders liegt", loescht damit
// den Beleg. Sie gehoeren bewusst hier her und nicht nach speedhttp/: Nur an
// dieser Stelle sind sie der Nachweis, dass ein UMZUG stattgefunden hat.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// URL-Zerlegung: Schema optional, Port optional, Pfad optional; https
    /// wird sauber abgelehnt.
    #[test_case]
    fn test_http_url_parsen() {
        let u = url_parsen("http://example.com/index.html").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 80);
        assert_eq!(u.pfad, "/index.html");

        // Ohne Schema und ohne Pfad.
        let u = url_parsen("example.com").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.pfad, "/");

        // Mit Port.
        let u = url_parsen("http://10.0.2.2:8000/datei.txt").unwrap();
        assert_eq!(u.host, "10.0.2.2");
        assert_eq!(u.port, 8000);
        assert_eq!(u.pfad, "/datei.txt");
        assert_eq!(u.als_text(), "http://10.0.2.2:8000/datei.txt");

        // https und Unsinn.
        assert_eq!(
            url_parsen("https://example.com"),
            Err(HttpFehler::TlsNichtUnterstuetzt)
        );
        assert_eq!(url_parsen("ftp://example.com"), Err(HttpFehler::UngueltigeUrl));
        assert_eq!(url_parsen(""), Err(HttpFehler::UngueltigeUrl));
        assert_eq!(url_parsen("http://"), Err(HttpFehler::UngueltigeUrl));
    }

    /// Antwort mit Content-Length: Rumpf wird exakt abgeschnitten; ein zu
    /// kurzer Rumpf gilt als unvollständig.
    #[test_case]
    fn test_http_antwort_content_length() {
        let roh = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nHalloUEBERSCHUSS";
        let a = antwort_parsen(roh).unwrap();
        assert_eq!(a.status, 200);
        assert_eq!(a.grund, "OK");
        assert_eq!(a.rumpf, b"Hallo");
        assert_eq!(a.header_wert("content-type"), Some("text/plain"));
        assert!(a.ist_text());

        // Zu kurzer Rumpf -> unvollständig.
        let kurz = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        assert_eq!(
            antwort_parsen(kurz),
            Err(HttpFehler::UnvollstaendigeAntwort)
        );

        // Ohne Kopf-Ende -> kaputter Kopf.
        assert_eq!(
            antwort_parsen(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n"),
            Err(HttpFehler::KaputterKopf)
        );
    }

    /// Chunked: mehrere Häppchen (auch mit Erweiterung) werden korrekt
    /// zusammengesetzt; ohne abschließenden 0-Chunk ist die Antwort unvollständig.
    #[test_case]
    fn test_http_antwort_chunked() {
        let roh = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
5\r\nHallo\r\n6\r\n Welt!\r\n0\r\n\r\n";
        let a = antwort_parsen(roh).unwrap();
        assert_eq!(a.rumpf, b"Hallo Welt!");

        // Chunk-Erweiterung hinter der Länge wird ignoriert.
        assert_eq!(
            chunked_dekodieren(b"3; name=wert\r\nabc\r\n0\r\n\r\n").unwrap(),
            b"abc"
        );

        // Ohne den abschliessenden 0-Chunk: unvollstaendig.
        let offen = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nHallo\r\n";
        assert_eq!(
            antwort_parsen(offen),
            Err(HttpFehler::UnvollstaendigeAntwort)
        );
        // Abgeschnittene Chunk-Daten ebenso.
        assert!(chunked_dekodieren(b"10\r\nzuwenig\r\n").is_none());
    }

    /// Header-Wirrwarr: krumme Abstände, gemischte Schreibweise, Zeilen ohne
    /// Doppelpunkt, doppelte Namen, bloße LF-Zeilenenden.
    #[test_case]
    fn test_http_header_wirrwarr() {
        let roh = b"HTTP/1.0 404 Not Found\nCONTENT-length:   7   \nX-Muell ohne Doppelpunkt\n\
Set-Cookie: a=1\nSet-Cookie: b=2\nLeer:\n\nnichtda";
        let a = antwort_parsen(roh).unwrap();
        assert_eq!(a.status, 404);
        assert_eq!(a.grund, "Not Found");
        // Name case-insensitiv, Wert getrimmt:
        assert_eq!(a.header_wert("Content-Length"), Some("7"));
        // Die Zeile ohne Doppelpunkt wurde uebersprungen:
        assert!(a.header.iter().all(|(n, _)| n != "X-Muell ohne Doppelpunkt"));
        // Erster Treffer bei doppelten Namen:
        assert_eq!(a.header_wert("set-cookie"), Some("a=1"));
        // Leerer Wert ist erlaubt:
        assert_eq!(a.header_wert("leer"), Some(""));
        assert_eq!(a.rumpf, b"nichtda");

        // Statuszeile ohne HTTP-Version -> kaputt.
        assert_eq!(
            antwort_parsen(b"200 OK\r\n\r\n"),
            Err(HttpFehler::KaputterKopf)
        );
    }

    /// Die Weiterleitungs-Logik: absolute URL, absoluter Pfad, relativer Pfad
    /// — und https als sauberer Fehler.
    #[test_case]
    fn test_http_weiterleitung() {
        let basis = url_parsen("http://alt.example/verzeichnis/seite.html").unwrap();

        // Absolute URL (auch anderer Host/Port).
        let z = naechste_url(&basis, "http://neu.example:8080/ziel").unwrap();
        assert_eq!(z.host, "neu.example");
        assert_eq!(z.port, 8080);
        assert_eq!(z.pfad, "/ziel");

        // Absoluter Pfad: Host/Port bleiben.
        let z = naechste_url(&basis, "/woanders").unwrap();
        assert_eq!(z.host, "alt.example");
        assert_eq!(z.pfad, "/woanders");

        // Relativ: gegen das Verzeichnis der Basis.
        let z = naechste_url(&basis, "nachbar.html").unwrap();
        assert_eq!(z.pfad, "/verzeichnis/nachbar.html");

        // https-Weiterleitung wird als solche gemeldet (nicht still versucht).
        assert_eq!(
            naechste_url(&basis, "https://sicher.example/"),
            Err(HttpFehler::TlsNichtUnterstuetzt)
        );
        assert_eq!(naechste_url(&basis, "  "), Err(HttpFehler::UngueltigeUrl));
    }

    /// Die gebaute Anfrage trägt Host und Connection: close.
    #[test_case]
    fn test_http_anfrage_bauen() {
        let u = url_parsen("http://example.com/a/b").unwrap();
        let a = anfrage_bauen(&u);
        assert!(a.starts_with("GET /a/b HTTP/1.1\r\n"));
        assert!(a.contains("Host: example.com\r\n"));
        assert!(a.contains("Connection: close\r\n"));
        assert!(a.ends_with("\r\n\r\n"));
        // Nicht-Standard-Port steht im Host-Header.
        let u = url_parsen("http://h:8000/").unwrap();
        assert!(anfrage_bauen(&u).contains("Host: h:8000\r\n"));
    }
}
