// speedhttp — HTTP/1.1 zerlegen und bauen. Sonst nichts.
//
// ==========================================================================
// HERKUNFT: DAS HIER IST DER PARSER AUS SERIE 5
//
// Jede Funktion in dieser Datei stand bis Serie 7, Teil 4 in
// `src/netz/http.rs` und ist beim Umzug ZEILE FUER ZEILE dieselbe geblieben.
// Das ist keine Nachlaessigkeit, sondern der Punkt der Uebung: Als `holes`
// (Ring 3, TLS) HTTP sprechen sollte, musste am Parser NICHTS geaendert
// werden — er bekam nur einen anderen Transport untergeschoben.
//
// Nachpruefbar ist das an zwei Stellen:
//   * Die `#[test_case]`-Tests in `src/netz/http.rs` sind ebenfalls
//     unveraendert geblieben und pruefen jetzt DIESEN Code (der Kernel
//     re-exportiert ihn per `pub use speedhttp::*`).
//   * `tests/netz_https.rs` faehrt beide Transporte gegen dieselbe Quelle
//     und vergleicht die geparsten Ergebnisse.
//
// ==========================================================================
// HTTP ist ein Textprotokoll aus einer Anfragezeile, Kopfzeilen, einer
// Leerzeile und einem Rumpf.
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
// ==========================================================================
// DIE https-STELLEN, DIE HIER ABSICHTLICH STEHEN GEBLIEBEN SIND
//
// `url_parsen` und `naechste_url` lehnen `https://` weiterhin mit
// `TlsNichtUnterstuetzt` ab. Das ist KEIN vergessener Zustand: Diese Kiste
// spricht kein TLS und wird es nie — sie kennt keinen Transport. Wer TLS hat
// (`userland/holes`), schneidet das Schema selbst ab und legt den Rest hier
// vor; `url_parsen` nimmt schemalose Eingaben ohnehin an. Der Kernel-Klient
// `hole` hat kein TLS und meldet deshalb genau das, was Sache ist.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Obergrenze für eine Antwort (Schutz gegen endlose Downloads).
pub const MAX_ANTWORT: usize = 1024 * 1024;
/// So vielen Weiterleitungen folgen wir höchstens.
pub const MAX_WEITERLEITUNGEN: u32 = 5;

/// Fehler beim Zerlegen von HTTP — alles, was OHNE Transport auftreten kann.
///
/// Was hier BEWUSST NICHT drinsteht: Socket- und DNS-Fehler. Die gehören dem
/// jeweiligen Transport (im Kernel `netz::http::KlientFehler`, in Ring 3
/// `libspeed::Fehler`) und haben in einem Parser nichts zu suchen. Genau
/// diese Trennung macht die Kiste zweifach benutzbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpFehler {
    UngueltigeUrl,
    /// https:// — diese Kiste kennt keinen Transport, also auch kein TLS.
    TlsNichtUnterstuetzt,
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
// Die EINZIGE Ergaenzung von Serie 7, Teil 4
// ---------------------------------------------------------------------------

/// Baut dieselbe Anfrage, aber mit einem ausdruecklich vorgegebenen
/// `Host:`-Text.
///
/// WOZU: Bei https ist 443 der Standard-Port und gehoert deshalb NICHT in den
/// Host-Kopf — `anfrage_bauen` kennt aber nur die http-Regel (nur 80 wird
/// weggelassen). Ein `Host: example.com:443` waere zwar RFC-konform, aber
/// viele Server vergleichen den Kopf stur mit ihrem Namen.
///
/// **`anfrage_bauen` wurde dafuer nicht angefasst.** Diese Funktion baut auch
/// nichts nach, sondern legt dem Original schlicht eine `Url` vor, deren Host
/// schon der gewuenschte Text ist und deren Port 80 lautet — dann laesst das
/// Original den Port weg. Ein Aufruf, keine zweite Fassung, kein Risiko, dass
/// die beiden auseinanderlaufen.
pub fn anfrage_bauen_mit_host(url: &Url, host_kopf: &str) -> String {
    anfrage_bauen(&Url {
        host: host_kopf.to_string(),
        port: 80,
        pfad: url.pfad.clone(),
    })
}

// ===========================================================================
// SCHEMA-BEWUSSTE ZIELE (Serie 7, Teil 5)
// ===========================================================================
//
// Bis Teil 4 hat sich JEDER Aufrufer das Schema selbst abgeschnitten:
// `holes` tat es, der Kernel-Klient tat es anders, und die Standard-Ports
// (80 gegen 443) standen an drei Stellen. Das ist genau die Sorte
// Verdopplung, die irgendwann auseinanderlaeuft — deshalb jetzt EINE
// getestete Stelle.
//
// `Ziel` ist bewusst `Url` PLUS ein Bit, statt eines neuen URL-Typs: Der
// ganze Rest des Parsers arbeitet auf `Url`, und der soll so bleiben, wie
// er seit Serie 5 ist. Die Serie-5-Funktionen `url_parsen`/`naechste_url`
// bleiben unveraendert und lehnen `https://` weiterhin ab — sie sind der
// Unterbau, nicht die Fassade.

/// Ein Abruf-Ziel: wohin, und ob verschluesselt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ziel {
    /// `true` = https (TLS), `false` = http.
    pub tls: bool,
    /// Host, Port und Pfad. Der Port ist IMMER gesetzt (80 bzw. 443).
    pub url: Url,
}

impl Ziel {
    /// Das Ziel wieder als Text — die Umkehrung von `ziel_parsen`.
    ///
    /// Der Port wird weggelassen, wenn er der Standard des Schemas ist.
    /// Diese Form ist auch der SCHLUESSEL fuer den Schleifenschutz: Zwei
    /// Weiterleitungen auf dieselbe Stelle ergeben denselben Text.
    pub fn als_text(&self) -> String {
        let schema = if self.tls { "https" } else { "http" };
        if self.port_ist_standard() {
            format!("{}://{}{}", schema, self.url.host, self.url.pfad)
        } else {
            format!(
                "{}://{}:{}{}",
                schema, self.url.host, self.url.port, self.url.pfad
            )
        }
    }

    /// Ist der Port der Standard des Schemas (80 bzw. 443)?
    pub fn port_ist_standard(&self) -> bool {
        self.url.port == standard_port(self.tls)
    }

    /// Der Text fuer den `Host:`-Kopf: Der Standard-Port gehoert NICHT hinein.
    pub fn host_kopf(&self) -> String {
        if self.port_ist_standard() {
            self.url.host.clone()
        } else {
            format!("{}:{}", self.url.host, self.url.port)
        }
    }

    /// Baut die GET-Anfrage fuer dieses Ziel.
    pub fn anfrage(&self) -> String {
        anfrage_bauen_mit_host(&self.url, &self.host_kopf())
    }
}

/// Der Standard-Port eines Schemas.
pub fn standard_port(tls: bool) -> u16 {
    if tls {
        443
    } else {
        80
    }
}

/// Zerlegt `http://…` oder `https://…` in ein `Ziel`.
///
/// OHNE SCHEMA wird **https** angenommen. Das ist eine bewusste Umkehrung
/// gegenueber `url_parsen` (dort ist es http): 2026 ist unverschluesselt die
/// Ausnahme, und wer `hole example.com` tippt, meint nicht „bitte im
/// Klartext". Wer wirklich http will, schreibt es hin.
///
/// Der Port ist danach IMMER gesetzt: der angegebene, sonst 80 bzw. 443.
pub fn ziel_parsen(text: &str) -> Result<Ziel, HttpFehler> {
    let t = text.trim();
    let (tls, rest) = if let Some(rest) = t.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = t.strip_prefix("http://") {
        (false, rest)
    } else if t.contains("://") {
        return Err(HttpFehler::UngueltigeUrl); // fremdes Schema
    } else {
        (true, t)
    };
    ziel_aus_autoritaet(tls, rest)
}

/// Der gemeinsame Rumpf von `ziel_parsen` und `naechstes_ziel`: `rest` ist
/// alles HINTER dem Schema, also `host[:port][/pfad]`.
fn ziel_aus_autoritaet(tls: bool, rest: &str) -> Result<Ziel, HttpFehler> {
    if rest.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    // HIER LAEUFT DIE SERIE-5-ZERLEGUNG: `url_parsen` nimmt schemalose
    // Eingaben an und kann Host, Port und Pfad. Sie wird NICHT nachgebaut.
    let mut url = url_parsen(rest)?;
    // `url_parsen` setzt mangels Schema 80 ein, wenn kein Port dastand.
    // Ob wirklich einer dastand, sieht man nur am Text — also nachsehen.
    let autoritaet = rest.split('/').next().unwrap_or("");
    if !autoritaet.contains(':') {
        url.port = standard_port(tls);
    }
    Ok(Ziel { tls, url })
}

/// Loest ein `Location:`-Ziel gegen das aktuelle auf — **inklusive
/// Schema-Wechsel**.
///
/// ==========================================================================
/// WARUM DAS EINE EIGENE FUNKTION IST UND NICHT `naechste_url`
///
/// `naechste_url` (Serie 5) lehnt eine absolute `https://`-Weiterleitung ab,
/// weil sie fuer einen Klienten ohne TLS geschrieben wurde. In der Praxis
/// ist http -> https aber der NORMALFALL: Fast jeder Server antwortet auf
/// http mit `301 Location: https://…`. Ein Klient, der das nicht mitgeht,
/// kommt im heutigen Web nirgends an.
///
/// Diese Funktion geht ihn mit — und benutzt fuer die beiden anderen Faelle
/// (absoluter Pfad, relativer Pfad) das unveraenderte `naechste_url`.
/// ==========================================================================
pub fn naechstes_ziel(basis: &Ziel, location: &str) -> Result<Ziel, HttpFehler> {
    let ort = location.trim();
    if ort.is_empty() {
        return Err(HttpFehler::UngueltigeUrl);
    }
    // Absolut mit Schema: neues Ziel, neues Schema.
    if let Some(rest) = ort.strip_prefix("https://") {
        return ziel_aus_autoritaet(true, rest);
    }
    if let Some(rest) = ort.strip_prefix("http://") {
        return ziel_aus_autoritaet(false, rest);
    }
    if ort.contains("://") {
        return Err(HttpFehler::UngueltigeUrl); // fremdes Schema
    }
    // Alles Uebrige ist schema-los und damit Sache der Serie-5-Funktion:
    // absoluter Pfad (/…) oder relativ zum Verzeichnis der Basis. Schema,
    // Host und Port bleiben, wie sie sind.
    let url = naechste_url(&basis.url, ort)?;
    Ok(Ziel {
        tls: basis.tls,
        url,
    })
}
