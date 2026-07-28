// netz/http.rs — Ein einfacher HTTP/1.1-Client über die Socket-API
//
// HTTP ist die erste echte ANWENDUNG auf unserem TCP: ein Textprotokoll aus
// einer Anfragezeile, Kopfzeilen, einer Leerzeile und einem Rumpf.
//
//   GET /pfad HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n
//   HTTP/1.1 200 OK\r\nContent-Length: 42\r\n\r\n<42 Bytes Rumpf>
//
// Zwei Arten, die Rumpf-Länge zu bestimmen, beide beherrschen wir:
//   * `Content-Length: N` — genau N Bytes.
//   * `Transfer-Encoding: chunked` — eine Folge von Häppchen, jedes mit einer
//     HEX-Längenzeile davor; ein Häppchen der Länge 0 beendet den Rumpf.
//   * (Fehlt beides: bis zum Verbindungsende — der HTTP/1.0-Stil, den wir mit
//     `Connection: close` ohnehin erzwingen.)
//
// WEITERLEITUNGEN (3xx + `Location`) folgen wir bis zu einer kleinen Grenze;
// die Auflösung relativer Ziele ist eine reine, getestete Funktion.
//
// NUR http:// — https/TLS ist BEWUSST VERTAGT (es braucht geprüfte Krypto,
// siehe docs/serie5-netzwerk.md Abschnitt d). Eine https-URL wird deshalb
// sauber mit `TlsNichtUnterstuetzt` abgelehnt, nie halbherzig versucht.
//
// Der Netz-Teil nutzt AUSSCHLIESSLICH die Socket-API (netz::socket) — kein
// Griff in tcp::Verbindung. Genau dafür ist die Fassade da.

use super::dns::{self, DnsFehler};
use super::socket::{self, Handle, SocketFehler, SocketTyp, Verbindungszustand};
use super::Ipv4;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Obergrenze für eine Antwort (Schutz gegen endlose Downloads).
pub const MAX_ANTWORT: usize = 1024 * 1024;
/// Zeitlimit für Verbindungsaufbau + Transfer.
const TIMEOUT_MS: u64 = 15_000;
/// So vielen Weiterleitungen folgen wir höchstens.
pub const MAX_WEITERLEITUNGEN: u32 = 5;

/// Fehler des HTTP-Clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpFehler {
    UngueltigeUrl,
    /// https:// — bewusst (noch) nicht unterstützt.
    TlsNichtUnterstuetzt,
    Dns(DnsFehler),
    Socket(SocketFehler),
    /// Die Antwort hat keinen brauchbaren Kopf.
    KaputterKopf,
    /// Der Rumpf kam kürzer an als angekündigt (Content-Length/chunked).
    UnvollstaendigeAntwort,
    ZuGross,
    ZuVieleWeiterleitungen,
}

impl HttpFehler {
    pub fn meldung(&self) -> &'static str {
        match self {
            HttpFehler::UngueltigeUrl => "ungueltige URL",
            HttpFehler::TlsNichtUnterstuetzt => {
                "https/TLS wird noch nicht unterstuetzt — bitte eine http://-Adresse"
            }
            HttpFehler::Dns(f) => f.meldung(),
            HttpFehler::Socket(f) => f.meldung(),
            HttpFehler::KaputterKopf => "die Antwort hat keinen gueltigen HTTP-Kopf",
            HttpFehler::UnvollstaendigeAntwort => "die Antwort kam unvollstaendig an",
            HttpFehler::ZuGross => "die Antwort ist zu gross",
            HttpFehler::ZuVieleWeiterleitungen => "zu viele Weiterleitungen",
        }
    }
}

// ---------------------------------------------------------------------------
// URL — parsen und zusammensetzen (reine Logik)
// ---------------------------------------------------------------------------

/// Eine zerlegte http-URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub pfad: String,
}

impl Url {
    /// Die URL wieder als Text (für Anzeige und Weiterleitungs-Ketten).
    pub fn als_text(&self) -> String {
        if self.port == 80 {
            format!("http://{}{}", self.host, self.pfad)
        } else {
            format!("http://{}:{}{}", self.host, self.port, self.pfad)
        }
    }
}

/// Zerlegt eine URL. Ohne Schema wird http:// angenommen; https:// wird
/// sauber abgelehnt.
pub fn url_parsen(text: &str) -> Result<Url, HttpFehler> {
    let t = text.trim();
    if t.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    let rest = if let Some(r) = t.strip_prefix("http://") {
        r
    } else if t.starts_with("https://") {
        return Err(HttpFehler::TlsNichtUnterstuetzt);
    } else if t.contains("://") {
        return Err(HttpFehler::UngueltigeUrl); // fremdes Schema
    } else {
        t // ohne Schema: http annehmen
    };
    if rest.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    let (hostteil, pfad) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if hostteil.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    let (host, port) = match hostteil.rfind(':') {
        Some(i) => {
            let p = hostteil[i + 1..]
                .parse::<u16>()
                .map_err(|_| HttpFehler::UngueltigeUrl)?;
            (&hostteil[..i], p)
        }
        None => (hostteil, 80),
    };
    if host.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    Ok(Url {
        host: host.to_string(),
        port,
        pfad: pfad.to_string(),
    })
}

/// Löst das Ziel einer Weiterleitung gegen die aktuelle URL auf: absolute
/// http-URL, absoluter Pfad (/…) oder relativer Pfad. Reine, getestete Logik.
pub fn naechste_url(basis: &Url, location: &str) -> Result<Url, HttpFehler> {
    let ort = location.trim();
    if ort.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    if ort.starts_with("https://") {
        return Err(HttpFehler::TlsNichtUnterstuetzt);
    }
    if ort.starts_with("http://") {
        return url_parsen(ort);
    }
    if ort.starts_with('/') {
        return Ok(Url {
            host: basis.host.clone(),
            port: basis.port,
            pfad: ort.to_string(),
        });
    }
    // Relativ: gegen das VERZEICHNIS des Basispfads auflösen.
    let verzeichnis = match basis.pfad.rfind('/') {
        Some(i) => &basis.pfad[..=i],
        None => "/",
    };
    Ok(Url {
        host: basis.host.clone(),
        port: basis.port,
        pfad: format!("{}{}", verzeichnis, ort),
    })
}

// ---------------------------------------------------------------------------
// Antwort — Kopf und Rumpf parsen (reine Logik)
// ---------------------------------------------------------------------------

/// Eine geparste HTTP-Antwort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Antwort {
    pub status: u16,
    pub grund: String,
    pub header: Vec<(String, String)>,
    pub rumpf: Vec<u8>,
}

impl Antwort {
    /// Kopfzeilen-Wert, Name OHNE Rücksicht auf Groß-/Kleinschreibung.
    pub fn header_wert(&self, name: &str) -> Option<&str> {
        header_wert_in(&self.header, name)
    }
    /// Sieht der Inhalt nach Text aus (dann kann die Shell ihn anzeigen)?
    pub fn ist_text(&self) -> bool {
        match self.header_wert("content-type") {
            Some(t) => {
                let t = t.to_ascii_lowercase();
                t.starts_with("text/") || t.contains("json") || t.contains("xml")
            }
            None => true, // ohne Angabe: als Text versuchen
        }
    }
}

fn header_wert_in<'a>(header: &'a [(String, String)], name: &str) -> Option<&'a str> {
    header
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, w)| w.as_str())
}

/// Findet das Kopf-Ende: liefert (Kopflänge, Rumpf-Start). Toleriert sowohl
/// CRLFCRLF als auch (unsauberes) LFLF.
fn kopf_ende(roh: &[u8]) -> Option<(usize, usize)> {
    if roh.len() >= 4 {
        for i in 0..=roh.len() - 4 {
            if &roh[i..i + 4] == b"\r\n\r\n" {
                return Some((i, i + 4));
            }
        }
    }
    if roh.len() >= 2 {
        for i in 0..=roh.len() - 2 {
            if &roh[i..i + 2] == b"\n\n" {
                return Some((i, i + 2));
            }
        }
    }
    None
}

/// Zerlegt die Statuszeile ("HTTP/1.1 200 OK") in (Code, Grund).
fn statuszeile_parsen(zeile: &str) -> Option<(u16, String)> {
    let mut teile = zeile.trim().splitn(3, ' ');
    let version = teile.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code = teile.next()?.trim().parse::<u16>().ok()?;
    let grund = teile.next().unwrap_or("").trim().to_string();
    Some((code, grund))
}

/// Liest die Kopfzeilen (ohne die Statuszeile). Robust gegen "Wirrwarr":
/// beliebige Leerzeichen, Groß-/Kleinschreibung, Zeilen ohne Doppelpunkt
/// (werden übersprungen) und bloße LF-Zeilenenden.
fn header_parsen(kopf_text: &str) -> Vec<(String, String)> {
    kopf_text
        .lines()
        .skip(1) // Statuszeile
        .filter_map(|z| {
            let z = z.trim_end_matches('\r');
            if z.trim().is_empty() {
                return None;
            }
            let (name, wert) = z.split_once(':')?; // ohne ':' -> überspringen
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), wert.trim().to_string()))
        })
        .collect()
}

/// Sucht ab `start` das nächste Zeilenende und liefert
/// (Index HINTER dem letzten Zeichen der Zeile, Index der nächsten Zeile).
fn zeilenende(daten: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    while i < daten.len() {
        if daten[i] == b'\n' {
            let ende = if i > start && daten[i - 1] == b'\r' { i - 1 } else { i };
            return Some((ende, i + 1));
        }
        i += 1;
    }
    None
}

/// Dekodiert einen `Transfer-Encoding: chunked`-Rumpf. None, wenn die Folge
/// unvollständig oder kaputt ist (dann fehlt der abschließende 0-Chunk).
pub fn chunked_dekodieren(daten: &[u8]) -> Option<Vec<u8>> {
    let mut aus = Vec::new();
    let mut i = 0usize;
    loop {
        let (zeilen_ende, naechste) = zeilenende(daten, i)?;
        let zeile = core::str::from_utf8(&daten[i..zeilen_ende]).ok()?;
        // Chunk-Erweiterungen ("1a; foo=bar") abschneiden.
        let hex = zeile.split(';').next()?.trim();
        if hex.is_empty() {
            return None;
        }
        let laenge = usize::from_str_radix(hex, 16).ok()?;
        i = naechste;
        if laenge == 0 {
            return Some(aus); // letzter Chunk — etwaige Trailer ignorieren wir
        }
        let ende = i.checked_add(laenge)?;
        if ende > daten.len() {
            return None; // abgeschnitten
        }
        aus.extend_from_slice(&daten[i..ende]);
        i = ende;
        // Das CRLF hinter den Chunk-Daten überspringen.
        let (_, nach_crlf) = zeilenende(daten, i)?;
        i = nach_crlf;
    }
}

/// Zerlegt eine komplette rohe HTTP-Antwort in Kopf + Rumpf.
pub fn antwort_parsen(roh: &[u8]) -> Result<Antwort, HttpFehler> {
    let (kopf_len, rumpf_start) = kopf_ende(roh).ok_or(HttpFehler::KaputterKopf)?;
    let kopf_text = String::from_utf8_lossy(&roh[..kopf_len]);
    let statuszeile = kopf_text.lines().next().ok_or(HttpFehler::KaputterKopf)?;
    let (status, grund) = statuszeile_parsen(statuszeile).ok_or(HttpFehler::KaputterKopf)?;
    let header = header_parsen(&kopf_text);
    let roh_rumpf = &roh[rumpf_start..];

    let chunked = header_wert_in(&header, "transfer-encoding")
        .map(|w| w.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    let rumpf = if chunked {
        chunked_dekodieren(roh_rumpf).ok_or(HttpFehler::UnvollstaendigeAntwort)?
    } else if let Some(laenge_text) = header_wert_in(&header, "content-length") {
        let laenge: usize = laenge_text
            .trim()
            .parse()
            .map_err(|_| HttpFehler::KaputterKopf)?;
        if roh_rumpf.len() < laenge {
            return Err(HttpFehler::UnvollstaendigeAntwort);
        }
        roh_rumpf[..laenge].to_vec()
    } else {
        // Weder Content-Length noch chunked: bis zum Verbindungsende.
        roh_rumpf.to_vec()
    };

    Ok(Antwort {
        status,
        grund,
        header,
        rumpf,
    })
}

/// Baut die GET-Anfrage (Host-Header ist in HTTP/1.1 Pflicht; `Connection:
/// close` sagt dem Server, dass er nach der Antwort schließen soll — dann
/// wissen wir sicher, wann der Rumpf zu Ende ist).
pub fn anfrage_bauen(url: &Url) -> String {
    let host = if url.port == 80 {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: SpeedOS/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        url.pfad, host
    )
}

// ---------------------------------------------------------------------------
// Der Netz-Teil: über die Socket-API holen
// ---------------------------------------------------------------------------

/// Führt EINE Anfrage über einen frischen TCP-Socket aus und liefert die
/// rohen Antwort-Bytes.
fn roh_ueber_socket(h: Handle, url: &Url, ip: Ipv4) -> Result<Vec<u8>, HttpFehler> {
    socket::verbinden(h, ip, url.port).map_err(HttpFehler::Socket)?;
    let frist = crate::zeit::ms_seit_boot() + TIMEOUT_MS;

    // 1. Auf den Handshake warten (der Stack wird dabei gepumpt).
    loop {
        super::pumpen();
        match socket::zustand(h).map_err(HttpFehler::Socket)? {
            Verbindungszustand::Verbunden => break,
            Verbindungszustand::Geschlossen => {
                return Err(HttpFehler::Socket(SocketFehler::Abgebrochen))
            }
            _ => {}
        }
        if crate::zeit::ms_seit_boot() >= frist {
            return Err(HttpFehler::Socket(SocketFehler::Zeitueberschreitung));
        }
        crate::zeit::warte_auf_interrupt();
    }

    // 2. Anfrage senden.
    let anfrage = anfrage_bauen(url);
    socket::senden(h, anfrage.as_bytes()).map_err(HttpFehler::Socket)?;

    // 3. Antwort lesen, bis die Gegenstelle schließt.
    let mut roh = Vec::new();
    let mut puffer = [0u8; 1460];
    loop {
        super::pumpen();
        loop {
            let n = socket::empfangen(h, &mut puffer).map_err(HttpFehler::Socket)?;
            if n == 0 {
                break;
            }
            if roh.len() + n > MAX_ANTWORT {
                return Err(HttpFehler::ZuGross);
            }
            roh.extend_from_slice(&puffer[..n]);
        }
        let fertig = matches!(
            socket::zustand(h).map_err(HttpFehler::Socket)?,
            Verbindungszustand::PeerHatGeschlossen | Verbindungszustand::Geschlossen
        );
        if fertig {
            // Den Rest aus dem Empfangspuffer nachholen.
            loop {
                let n = socket::empfangen(h, &mut puffer).map_err(HttpFehler::Socket)?;
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
fn roh_holen(url: &Url) -> Result<Vec<u8>, HttpFehler> {
    let ip = dns::aufloesen(&url.host).map_err(HttpFehler::Dns)?;
    let h = socket::oeffnen(SocketTyp::Tcp).map_err(HttpFehler::Socket)?;
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
pub fn holen(url_text: &str) -> Result<(Url, Antwort), HttpFehler> {
    let mut url = url_parsen(url_text)?;
    let mut verbleibend = MAX_WEITERLEITUNGEN;
    loop {
        let roh = roh_holen(&url)?;
        let antwort = antwort_parsen(&roh)?;
        // 3xx mit Location -> weiterleiten.
        if (300..400).contains(&antwort.status) {
            if let Some(ort) = antwort.header_wert("location") {
                if verbleibend == 0 {
                    return Err(HttpFehler::ZuVieleWeiterleitungen);
                }
                let ort = ort.to_string();
                url = naechste_url(&url, &ort)?;
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
