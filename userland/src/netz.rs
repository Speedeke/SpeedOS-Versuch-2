// netz.rs — „Hol mir diese URL." Einmal richtig, fuer alle.
//           (Serie 7, Teil 5)
//
// ==========================================================================
// WOZU ES DIESE SCHICHT GIBT
//
// Nach Teil 4 konnte SpeedOS TLS — aber nur `holes` konnte es. Der ganze
// Ablauf (Schema erkennen, DNS, verbinden, Wurzeln laden, Handshake,
// Anfrage bauen, Rumpf einsammeln, Weiterleitungen folgen) stand in einem
// Programm. Das naechste Programm haette ihn abgeschrieben, das
// uebernaechste auch, und beim dritten waeren die Fristen unterschiedlich
// gewesen und das Groessenlimit vergessen.
//
// Hier steht er EINMAL. `holes` ist seitdem eine Bedienoberflaeche dafuer,
// `news` ebenso — und der Browser aus Serie 8 wird ihr erster grosser Kunde.
//
// ==========================================================================
// WAS DIESE SCHICHT ZUSICHERT
//
//  (1) **Sie haengt nie.** Jede Operation hat eine Frist; laeuft sie ab,
//      kommt ein Fehler, kein Warten ohne Ende.
//  (2) **Sie frisst keinen Speicher.** `max_bytes` wird WAEHREND des Lesens
//      geprueft, nicht danach — ein Server, der endlos sendet, wird nach
//      dem Limit abgeschnitten und nicht erst, wenn der Heap voll ist.
//  (3) **Sie folgt Weiterleitungen, auch ueber das Schema hinweg** (http ->
//      https ist im heutigen Web der Normalfall), aber nur bis zu einer
//      Grenze und nie im Kreis.
//  (4) **Sie prueft Zertifikate immer.** Es gibt keinen Parameter, der das
//      abschaltet — siehe `libspeed::tls`.
//  (5) **Sie raeumt auf.** Jeder Strom wird beim Verlassen geschlossen,
//      auch auf dem Fehlerweg (`Drop` von `TcpStrom`/`TlsStrom`).

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use speedhttp::{Antwort, Ziel};

use crate::tls::{TcpStrom, TlsFehler, TlsStrom};
use crate::Fehler;

// ---------------------------------------------------------------------------
// Voreinstellungen
// ---------------------------------------------------------------------------

/// Wie lange EIN Versuch hoechstens dauern darf (Verbinden + Handshake +
/// Uebertragung). Jede Weiterleitung bekommt ihre eigene Frist; die
/// Gesamtdauer ist damit durch `max_weiterleitungen` gedeckelt.
pub const FRIST_MS: u64 = 20_000;
/// Wie viel Antwort hoechstens angenommen wird. 1 MiB ist dieselbe Grenze,
/// die der Kernel-Klient seit Serie 5 hat (`speedhttp::MAX_ANTWORT`).
pub const MAX_BYTES: usize = speedhttp::MAX_ANTWORT;
/// So vielen Weiterleitungen wird hoechstens gefolgt.
pub const MAX_WEITERLEITUNGEN: u32 = speedhttp::MAX_WEITERLEITUNGEN;
/// Stueckgroesse je Lesevorgang.
const STUECK: usize = 8192;

// ---------------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------------

/// Was beim Abrufen schiefgehen kann.
///
/// BEWUSST GETRENNT nach der Schicht, in der es passiert ist — denn die
/// Antwort darauf ist jeweils eine andere: Eine kaputte URL ist ein
/// Tippfehler, ein DNS-Fehler ein Netzproblem, ein Zertifikatsfehler eine
/// SICHERHEITSAUSSAGE. Sie alle zu `Fehler(u64)` zu verschmelzen waere
/// bequem und wuerde genau die Unterscheidung wegwerfen, auf die es ankommt.
#[derive(Debug)]
pub enum AbrufFehler {
    /// Die Adresse liess sich nicht zerlegen.
    Url(speedhttp::HttpFehler),
    /// Der Name liess sich nicht aufloesen.
    Dns(Fehler),
    /// Socket/Verbindung (auch: Frist beim Verbinden).
    Verbindung(Fehler),
    /// TLS — Handshake, Zertifikat, Protokoll. Hier steckt die
    /// Sicherheitsaussage drin.
    Tls(TlsFehler),
    /// Die Antwort war kein brauchbares HTTP. Die Zahl ist, wie viele rohe
    /// Bytes ankamen — ohne sie liesse sich „Server redet Unsinn" nicht von
    /// „Antwort abgeschnitten" unterscheiden.
    Http(speedhttp::HttpFehler, usize),
    /// Die Verbindung stand, aber es kam KEIN EINZIGES Byte.
    ///
    /// Ein eigener Fall, und zwar ein wichtiger: Er heisst, dass die
    /// Gegenstelle angenommen und sofort wieder aufgelegt hat. Das als
    /// „kaputter HTTP-Kopf" zu melden waere eine Falschaussage — es gab gar
    /// keinen Kopf, und der Fehler liegt eine Schicht tiefer.
    LeereAntwort,
    /// Groesser als `max_bytes` — abgebrochen, WAEHREND es lief.
    ZuGross { grenze: usize },
    /// Mehr Weiterleitungen als erlaubt.
    ZuVieleWeiterleitungen(u32),
    /// Eine Weiterleitung zeigte auf eine Stelle, die schon besucht wurde.
    Schleife(String),
    /// Die Frist fuer diesen Versuch ist abgelaufen.
    Frist(u64),
}

impl AbrufFehler {
    /// Vollstaendiger deutscher Satz fuer den Menschen.
    pub fn text(&self) -> String {
        use alloc::format;
        match self {
            AbrufFehler::Url(fehler) => {
                format!("Die Adresse ist nicht brauchbar: {}.", fehler.meldung())
            }
            AbrufFehler::Dns(fehler) => format!(
                "Der Name liess sich nicht aufloesen: {}. Steht der Rechner im Netz \
                 (`netz-status`), und stimmt der Name?",
                fehler.text()
            ),
            // Eine Zeitueberschreitung IST ein Fristablauf, egal in welcher
            // Schicht sie auftritt. Sie hier als „Verbindung kam nicht
            // zustande" zu melden waere zwar nicht falsch, verwischt aber den
            // Unterschied zu „niemand lauscht" — und genau den will man beim
            // Suchen wissen.
            AbrufFehler::Verbindung(fehler) if *fehler == Fehler::ZEITUEBERSCHREITUNG => {
                String::from(
                    "Zeitueberschreitung: Die Gegenstelle hat die Verbindung zwar \
                     angenommen, antwortet aber nicht (oder zu langsam). Bei https \
                     ist das der typische Fall, wenn dort etwas lauscht, das kein \
                     TLS spricht und auch nichts sagt.",
                )
            }
            AbrufFehler::Verbindung(fehler) => format!(
                "Die Verbindung kam nicht zustande: {}.",
                fehler.text()
            ),
            AbrufFehler::Tls(fehler) => fehler.text(),
            AbrufFehler::Http(fehler, bytes) => format!(
                "Die Antwort war nicht lesbar: {} ({} Byte kamen an).",
                fehler.meldung(),
                bytes
            ),
            AbrufFehler::LeereAntwort => String::from(
                "Die Gegenstelle hat die Verbindung angenommen und dann sofort \
                 aufgelegt, ohne ein einziges Byte zu schicken.",
            ),
            AbrufFehler::ZuGross { grenze } => format!(
                "Die Antwort ist groesser als das Limit von {} Byte und wurde \
                 abgebrochen. Mit einem hoeheren Limit erneut versuchen.",
                grenze
            ),
            AbrufFehler::ZuVieleWeiterleitungen(anzahl) => format!(
                "Mehr als {} Weiterleitungen — hier stimmt etwas nicht.",
                anzahl
            ),
            AbrufFehler::Schleife(ort) => format!(
                "Weiterleitungs-SCHLEIFE: '{}' war schon dran. Abgebrochen, \
                 bevor es sich im Kreis dreht.",
                ort
            ),
            AbrufFehler::Frist(ms) => format!(
                "Zeitueberschreitung nach {} ms. Die Gegenstelle antwortet nicht \
                 (oder zu langsam).",
                ms
            ),
        }
    }

    /// Kurzes Schlagwort — fuer Tests, Protokolle und Exit-Codes.
    pub fn kurz(&self) -> &'static str {
        match self {
            AbrufFehler::Url(_) => "url",
            AbrufFehler::Dns(_) => "dns",
            // Eine Frist bleibt eine Frist, auch wenn sie unter TLS ablaeuft.
            AbrufFehler::Verbindung(code) if *code == Fehler::ZEITUEBERSCHREITUNG => "frist",
            AbrufFehler::Verbindung(_) => "verbindung",
            AbrufFehler::Tls(fehler) => fehler.kurz(),
            AbrufFehler::Http(_, _) => "http",
            AbrufFehler::LeereAntwort => "leere-antwort",
            AbrufFehler::ZuGross { .. } => "zu-gross",
            AbrufFehler::ZuVieleWeiterleitungen(_) => "zu-viele-weiterleitungen",
            AbrufFehler::Schleife(_) => "schleife",
            AbrufFehler::Frist(_) => "frist",
        }
    }

    /// Ist das eine Sicherheitsaussage (Zertifikat/TLS) und kein Netzproblem?
    ///
    /// Der Unterschied ist wichtig genug fuer eine eigene Frage: „Server
    /// nicht erreichbar" darf man nochmal versuchen, „Zertifikat abgelehnt"
    /// nicht.
    pub fn ist_sicherheitsfehler(&self) -> bool {
        matches!(self, AbrufFehler::Tls(_))
    }
}

impl From<TlsFehler> for AbrufFehler {
    fn from(fehler: TlsFehler) -> AbrufFehler {
        match fehler {
            // Ein reiner Transportfehler unter TLS bleibt ein Transportfehler
            // — sonst sieht ein abgebrochenes Kabel wie ein Zertifikatsproblem
            // aus, und das waere eine falsche Sicherheitsaussage.
            TlsFehler::Netz(code) => AbrufFehler::Verbindung(code),
            sonst => AbrufFehler::Tls(sonst),
        }
    }
}

// ---------------------------------------------------------------------------
// Das Ergebnis
// ---------------------------------------------------------------------------

/// Womit gesprochen wurde (fuer Anzeige und Messung).
#[derive(Debug, Clone, Default)]
pub struct Verbindungsinfo {
    pub tls: bool,
    /// „TLS 1.3" / „TLS 1.2" / „" bei http.
    pub protokoll: &'static str,
    /// Der RFC-Name der Ciphersuite (leer bei http).
    pub suite: String,
    /// Wie viele Zertifikate die Gegenstelle vorgelegt hat.
    pub kettenlaenge: usize,
    /// Die Zertifikate selbst (DER) — NUR wenn `Klient::kette_behalten`
    /// gesetzt war.
    ///
    /// Warum nicht immer: Eine Kette sind schnell 4 KiB, und der Strom, dem
    /// sie gehoert, ist beim Auswerten des Ergebnisses laengst geschlossen —
    /// sie muss also KOPIERT werden. Das lohnt sich fuer `holes --info` und
    /// fuer einen Browser, der ein Schloss-Symbol erklaeren will; fuer einen
    /// Abruf, der nur den Rumpf braucht, waere es Verschwendung.
    pub kette: Vec<Vec<u8>>,
    pub tcp_ms: u64,
    pub handshake_ms: u64,
}

/// Ein abgeschlossener Abruf.
pub struct Abruf {
    /// Die ENDGUELTIGE Adresse (nach allen Weiterleitungen).
    pub ziel: Ziel,
    pub antwort: Antwort,
    pub info: Verbindungsinfo,
    /// Wie vielen Weiterleitungen gefolgt wurde.
    pub weiterleitungen: u32,
    /// Wie oft ein Versuch wiederholt werden musste (0 = beim ersten Mal
    /// geklappt). Steht im Ergebnis, damit es sich MESSEN laesst statt nur
    /// im Verborgenen zu passieren.
    pub wiederholungen: u32,
    /// Rohe Antwort-Bytes (Kopf + Rumpf, vor dem Zerlegen).
    pub roh_bytes: usize,
    /// Gesamtdauer ueber alle Versuche (inkl. DNS, Verbinden, Handshake).
    pub dauer_ms: u64,
    /// NUR die Uebertragung des LETZTEN Versuchs — ohne DNS, Verbinden und
    /// Handshake.
    ///
    /// Warum getrennt: Sonst misst „Durchsatz" bei einer kleinen Seite vor
    /// allem den Handshake. Fuer die Frage „wie schnell kommen Bytes durch"
    /// ist genau diese Zahl die richtige.
    pub uebertragung_ms: u64,
}

// ---------------------------------------------------------------------------
// Der Transport — http und https hinter EINER Naht
// ---------------------------------------------------------------------------

/// Ein Byte-Strom, verschluesselt oder nicht.
///
/// Genau hier verschwindet der Unterschied: Ab dieser Stelle sieht der
/// HTTP-Teil nur noch `lesen`/`schreiben`. Dass unter dem einen Zweig ein
/// TLS-Handshake steckt und unter dem anderen nichts, ist eine Frage der
/// Verbindungsaufnahme und keine des Protokolls darueber.
enum Strom {
    Klar(TcpStrom),
    // `Box`, weil `TlsStrom` seine Puffer traegt und damit deutlich groesser
    // ist als `TcpStrom` — sonst waere jedes `Strom` so gross wie der
    // groessere von beiden, auch das klare.
    Sicher(alloc::boxed::Box<TlsStrom>),
}

impl Strom {
    fn lesen(&mut self, ziel: &mut [u8]) -> Result<usize, AbrufFehler> {
        match self {
            Strom::Klar(strom) => strom.lesen(ziel).map_err(AbrufFehler::Verbindung),
            Strom::Sicher(strom) => strom.lesen(ziel).map_err(AbrufFehler::from),
        }
    }

    fn schreiben(&mut self, daten: &[u8]) -> Result<(), AbrufFehler> {
        match self {
            Strom::Klar(strom) => strom.schreiben(daten).map_err(AbrufFehler::Verbindung),
            Strom::Sicher(strom) => strom.schreiben(daten).map_err(AbrufFehler::from),
        }
    }

    /// Geordneter Abschied (bei TLS: close_notify). Fehler sind hier egal —
    /// wir sind fertig.
    fn verabschieden(&mut self) {
        if let Strom::Sicher(strom) = self {
            strom.schliessen();
        }
    }

    fn info(&self, tcp_ms: u64, kette_behalten: bool) -> Verbindungsinfo {
        match self {
            Strom::Klar(_) => Verbindungsinfo {
                tcp_ms,
                ..Verbindungsinfo::default()
            },
            Strom::Sicher(strom) => Verbindungsinfo {
                tls: true,
                protokoll: strom.protokoll_text(),
                suite: strom.ciphersuite_text(),
                kettenlaenge: strom.kette().len(),
                kette: if kette_behalten {
                    strom.kette().iter().map(|z| z.as_ref().to_vec()).collect()
                } else {
                    Vec::new()
                },
                tcp_ms,
                handshake_ms: strom.handshake_ms(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Der Klient
// ---------------------------------------------------------------------------

/// Holt URLs. Einstellbar, wiederverwendbar, und er haengt nicht.
///
/// Ein `Klient` lohnt sich ueber mehrere Abrufe hinweg, weil er die
/// TLS-Konfiguration BEHAELT: Die Wurzelzertifikate zu lesen und zu parsen
/// kostet ~190 KiB Datei und 119 Zertifikate. Das einmal je Abruf zu tun
/// waere die teuerste Zeile im ganzen Ablauf.
pub struct Klient {
    /// Frist je Versuch (Verbinden + Handshake + Uebertragung).
    pub frist_ms: u64,
    /// Obergrenze der Antwort in Bytes.
    pub max_bytes: usize,
    /// Hoechstzahl Weiterleitungen.
    pub max_weiterleitungen: u32,
    /// Die vorgelegte Zertifikatskette im Ergebnis mitliefern (kostet eine
    /// Kopie von einigen KiB — siehe `Verbindungsinfo::kette`).
    pub kette_behalten: bool,
    /// Wie oft ein Versuch WIEDERHOLT wird, der ohne ein einziges Byte endete.
    ///
    /// ==================================================================
    /// WARUM ES DAS GIBT — UND WARUM NUR FUER DIESEN EINEN FALL
    ///
    /// „Verbindung angenommen, null Bytes, sofort wieder zu" ist der
    /// klassische FLUECHTIGE Fehler: ein Server, der gerade seinen
    /// Arbeiter-Thread wechselt, eine NAT-Tabelle, die einen Eintrag
    /// recycelt, ein Paket, das unterwegs verlorenging.
    ///
    /// HISTORISCHE NOTIZ, weil sie eine Lehre enthaelt: Als es diese
    /// Einstellung zum ersten Mal gab, sollte sie einen Fehler ueberdecken,
    /// der bei UNS lag — der Wettlauf am Strom-Ende in
    /// `TcpStrom::lesen` (siehe dort). Der ist behoben; seitdem sind es in
    /// denselben Messungen 0 von 30 statt 6 von 30. Die Wiederholung bleibt
    /// trotzdem, aber jetzt aus dem richtigen Grund: Netze sind unzuverlaessig,
    /// und ein GET, bei dem NULL Bytes ankamen, ist gefahrlos wiederholbar —
    /// es kann nichts zweimal passiert sein, wir haben ja nichts gesehen.
    ///
    /// NUR FUER DIESEN FALL. Ein Zertifikatsfehler wird NIE wiederholt (das
    /// waere ein Angreifer, der es einfach nochmal versucht), eine
    /// abgeschnittene Antwort auch nicht (dort ist schon etwas passiert),
    /// und eine Frist erst recht nicht (dann dauert es doppelt so lange).
    ///
    /// **Eine Wiederholung, die man nicht MISST, ist ein Teppich, unter den
    /// man kehrt.** Deshalb steht die Zahl im Ergebnis
    /// (`Abruf::wiederholungen`), und deshalb schaltet `holes --serie` sie
    /// ab: So faellt es auf, wenn sie wieder etwas verdecken muesste.
    /// ==================================================================
    pub wiederholungen: u32,
    /// Die TLS-Konfiguration — erst beim ersten https-Abruf gebaut.
    konfig: Option<Arc<rustls::ClientConfig>>,
}

impl Default for Klient {
    fn default() -> Klient {
        Klient::neu()
    }
}

impl Klient {
    /// Ein Klient mit den Voreinstellungen. Die Wurzelzertifikate werden
    /// erst geladen, wenn wirklich ein https-Ziel drankommt — ein Programm,
    /// das nur http spricht, zahlt nichts dafuer.
    pub fn neu() -> Klient {
        Klient {
            frist_ms: FRIST_MS,
            max_bytes: MAX_BYTES,
            max_weiterleitungen: MAX_WEITERLEITUNGEN,
            kette_behalten: false,
            wiederholungen: 2,
            konfig: None,
        }
    }

    /// Ein Klient mit einer FERTIGEN TLS-Konfiguration.
    ///
    /// Fuer Aufrufer, die die Wurzeln selbst geladen haben — etwa weil sie
    /// den Lesepuffer in `.bss` halten wollen statt auf dem Heap (`holes`
    /// tut das, damit seine Heap-Messung den TLS-Bedarf zeigt und nicht die
    /// Groesse einer Datei).
    pub fn mit_konfig(konfig: Arc<rustls::ClientConfig>) -> Klient {
        Klient {
            konfig: Some(konfig),
            ..Klient::neu()
        }
    }

    /// Die TLS-Konfiguration, notfalls jetzt gebaut.
    fn konfig(&mut self) -> Result<Arc<rustls::ClientConfig>, AbrufFehler> {
        if self.konfig.is_none() {
            self.konfig = Some(crate::tls::konfig_vom_datentraeger()?);
        }
        Ok(self.konfig.as_ref().expect("gerade gesetzt").clone())
    }

    /// DIE FUNKTION: hol mir diese URL.
    ///
    /// Folgt Weiterleitungen (auch mit Schema-Wechsel), prueft bei https die
    /// Zertifikatskette, bricht bei `max_bytes` ab und haelt die Frist ein.
    pub fn holen(&mut self, adresse: &str) -> Result<Abruf, AbrufFehler> {
        let start = crate::zeit_jetzt();
        let mut ziel = speedhttp::ziel_parsen(adresse).map_err(AbrufFehler::Url)?;

        // DER SCHLEIFENSCHUTZ: die schon besuchten Stellen, in Textform.
        // `Ziel::als_text` normalisiert dabei (Standard-Port weggelassen),
        // sonst waeren „https://a/" und „https://a:443/" zwei verschiedene
        // Stellen und die Schleife liefe doch.
        let mut besucht: Vec<String> = Vec::new();
        besucht.push(ziel.als_text());

        let mut weiterleitungen = 0u32;
        let mut wiederholt = 0u32;
        loop {
            let (roh, info, uebertragung_ms) = match self.einmal_holen(&ziel) {
                Ok(paar) => paar,
                // Der EINE wiederholbare Fall — siehe `Klient::wiederholungen`.
                Err(AbrufFehler::LeereAntwort) if wiederholt < self.wiederholungen => {
                    wiederholt += 1;
                    crate::diagnoseln!(
                        "[netz] {} lieferte 0 Byte — Versuch {} von {}.",
                        ziel.als_text(),
                        wiederholt + 1,
                        self.wiederholungen + 1
                    );
                    // Kurz Luft lassen: Wenn die Gegenstelle gerade beschaeftigt
                    // war, hilft sofortiges Nachbohren niemandem.
                    crate::schlafe(50);
                    continue;
                }
                Err(fehler) => return Err(fehler),
            };
            let antwort = speedhttp::antwort_parsen(&roh)
                .map_err(|fehler| AbrufFehler::Http(fehler, roh.len()))?;

            // 3xx mit Location -> weiterleiten.
            if (300..400).contains(&antwort.status) {
                if let Some(ort) = antwort.header_wert("location") {
                    if weiterleitungen >= self.max_weiterleitungen {
                        return Err(AbrufFehler::ZuVieleWeiterleitungen(
                            self.max_weiterleitungen,
                        ));
                    }
                    let naechstes =
                        speedhttp::naechstes_ziel(&ziel, ort).map_err(AbrufFehler::Url)?;
                    let text = naechstes.als_text();
                    if besucht.contains(&text) {
                        return Err(AbrufFehler::Schleife(text));
                    }
                    besucht.push(text);
                    ziel = naechstes;
                    weiterleitungen += 1;
                    continue;
                }
                // 3xx OHNE Location ist keine Weiterleitung, sondern eine
                // Antwort — sie wird durchgereicht, nicht verschluckt.
            }

            return Ok(Abruf {
                ziel,
                roh_bytes: roh.len(),
                antwort,
                info,
                weiterleitungen,
                wiederholungen: wiederholt,
                dauer_ms: crate::zeit_jetzt() - start,
                uebertragung_ms,
            });
        }
    }

    /// EIN Versuch: verbinden, Anfrage senden, Antwort einsammeln.
    fn einmal_holen(
        &mut self,
        ziel: &Ziel,
    ) -> Result<(Vec<u8>, Verbindungsinfo, u64), AbrufFehler> {
        let frist = crate::zeit_jetzt() + self.frist_ms;

        // --- 1. Name -> IP ---
        let ip = match ip_aus_text(&ziel.url.host) {
            Some(ip) => ip,
            None => crate::aufloesen(&ziel.url.host).map_err(AbrufFehler::Dns)?,
        };

        // --- 2. TCP ---
        let tcp_start = crate::zeit_jetzt();
        let mut tcp = TcpStrom::verbinden(ip, ziel.url.port).map_err(AbrufFehler::Verbindung)?;
        tcp.frist_ms = self.frist_ms;
        let tcp_ms = crate::zeit_jetzt() - tcp_start;

        // --- 3. Bei https: der Handshake ---
        //
        // `ziel.url.host` geht hier zweimal ein und beide Male zwingend: als
        // SNI und als Pruefname gegen das Zertifikat. Es gibt keinen Weg,
        // nur eins von beiden zu tun (siehe libspeed::tls).
        let mut strom = if ziel.tls {
            let konfig = self.konfig()?;
            let tls = TlsStrom::verbinden(tcp, konfig, &ziel.url.host)?;
            Strom::Sicher(alloc::boxed::Box::new(tls))
        } else {
            Strom::Klar(tcp)
        };
        let info = strom.info(tcp_ms, self.kette_behalten);

        // --- 4. Anfrage ---
        let uebertragung_start = crate::zeit_jetzt();
        strom.schreiben(ziel.anfrage().as_bytes())?;

        // --- 5. Antwort einsammeln ---
        //
        // DAS GROESSENLIMIT WIRD HIER GEPRUEFT, NICHT HINTERHER. Ein Server,
        // der endlos sendet (oder eine falsche Content-Length nennt), soll
        // nicht erst auffallen, wenn der Heap voll ist.
        let mut roh: Vec<u8> = Vec::new();
        let mut stueck = alloc::vec![0u8; STUECK];
        loop {
            if crate::zeit_jetzt() >= frist {
                strom.verabschieden();
                return Err(AbrufFehler::Frist(self.frist_ms));
            }
            let gelesen = match strom.lesen(&mut stueck) {
                Ok(0) => break, // Ende des Stroms
                Ok(n) => n,
                Err(fehler) => return Err(fehler),
            };
            if roh.len() + gelesen > self.max_bytes {
                // Abbrechen, ohne den Rest noch anzunehmen.
                strom.verabschieden();
                return Err(AbrufFehler::ZuGross {
                    grenze: self.max_bytes,
                });
            }
            roh.extend_from_slice(&stueck[..gelesen]);
        }
        strom.verabschieden();
        // `strom` faellt hier — und mit ihm das Socket-Handle (Drop von
        // TcpStrom). Auch auf jedem Fehlerweg oben, denn Drop laeuft immer.

        let uebertragung_ms = crate::zeit_jetzt() - uebertragung_start;

        if roh.is_empty() {
            return Err(AbrufFehler::LeereAntwort);
        }
        Ok((roh, info, uebertragung_ms))
    }
}

/// Bequemlichkeit fuer den Einmal-Gebrauch: ein Klient, ein Abruf.
///
/// Wer mehrere URLs holt, legt sich einen `Klient` an und behaelt ihn — sonst
/// werden die Wurzelzertifikate jedes Mal neu gelesen.
pub fn holen(adresse: &str) -> Result<Abruf, AbrufFehler> {
    Klient::neu().holen(adresse)
}

// ---------------------------------------------------------------------------
// Kleinkram
// ---------------------------------------------------------------------------

/// Erkennt eine reine IPv4-Adresse ("10.0.2.2"). `None` heisst „das ist ein
/// Name, frag DNS".
pub fn ip_aus_text(text: &str) -> Option<u32> {
    let mut teile = [0u32; 4];
    let mut anzahl = 0usize;
    for stueck in text.split('.') {
        if anzahl == 4 || stueck.is_empty() || stueck.len() > 3 {
            return None;
        }
        let mut wert = 0u32;
        for ziffer in stueck.bytes() {
            if !ziffer.is_ascii_digit() {
                return None;
            }
            wert = wert * 10 + (ziffer - b'0') as u32;
        }
        if wert > 255 {
            return None;
        }
        teile[anzahl] = wert;
        anzahl += 1;
    }
    if anzahl != 4 {
        return None;
    }
    Some((teile[0] << 24) | (teile[1] << 16) | (teile[2] << 8) | teile[3])
}

/// Eine IPv4 als Text ("10.0.2.2").
pub fn ip_als_text(ip: u32) -> String {
    alloc::format!(
        "{}.{}.{}.{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff
    )
}

/// Schreibt Bytes in eine Datei (anlegen/abschneiden) — der haeufigste
/// Wunsch direkt nach einem Abruf.
pub fn speichern(pfad: &str, daten: &[u8]) -> Result<(), Fehler> {
    let handle = crate::oeffne(
        pfad,
        crate::SCHREIBEN | crate::ANLEGEN | crate::ABSCHNEIDEN,
    )?;
    let mut geschrieben = 0usize;
    let ergebnis = loop {
        if geschrieben == daten.len() {
            break Ok(());
        }
        // Ein Syscall uebernimmt hoechstens MAX_PUFFER (64 KiB).
        let rest = daten.len() - geschrieben;
        let jetzt = rest.min(32 * 1024);
        match crate::schreibe_at(
            handle,
            geschrieben as u64,
            &daten[geschrieben..geschrieben + jetzt],
        ) {
            Ok(n) if n > 0 => geschrieben += n as usize,
            Ok(_) => break Err(Fehler::KEIN_PLATZ),
            Err(fehler) => break Err(fehler),
        }
    };
    let _ = crate::schliesse(handle);
    ergebnis
}

/// Liest eine ganze Datei auf den Heap.
pub fn datei_lesen(pfad: &str) -> Result<Vec<u8>, Fehler> {
    let handle = crate::oeffne(pfad, crate::LESEN)?;
    let mut inhalt: Vec<u8> = Vec::new();
    let mut stueck = alloc::vec![0u8; 32 * 1024];
    let ergebnis = loop {
        match crate::lese_at(handle, inhalt.len() as u64, &mut stueck) {
            Ok(0) => break Ok(()),
            Ok(n) => inhalt.extend_from_slice(&stueck[..n as usize]),
            Err(fehler) => break Err(fehler),
        }
    };
    let _ = crate::schliesse(handle);
    ergebnis.map(|()| inhalt)
}

/// Der Text einer Antwort, so gut es geht (fuer die Anzeige im Terminal).
pub fn als_text(antwort: &Antwort) -> String {
    String::from_utf8_lossy(&antwort.rumpf).to_string()
}
