// browser::ort — wo eine Seite herkommt
//
// ===========================================================================
// DREI HERKUENFTE, EIN TYP
//
// Ein Browser in SpeedOS zeigt Seiten aus drei Quellen an, und alle drei
// muessen dasselbe koennen: im Verlauf stehen, in der Adressleiste
// erscheinen, Ziel eines Verweises sein.
//
//   Netz    http:// und https://          -> libspeed::netz::Klient
//   Datei   /platte/seiten/cern.html      -> Datei lesen
//   Intern  speedos:info                  -> selbst erzeugtes HTML
//
// EIN ENUM UND KEINE DREI PFADE DURCH DEN BROWSER: Sonst braeuchte jede
// Stelle, die eine Seite laedt, anzeigt oder in den Verlauf legt, drei
// Faelle — und die dritte davon wird irgendwann vergessen.
//
// ===========================================================================
// DIE AUFLOESUNG LIEGT NICHT HIER
//
// Was ein relativer Verweis bedeutet, steht in `speedhttp`
// (`verweis_aufloesen`, `pfad_normalisieren`) und ist dort mit 25 Tests
// festgenagelt. Diese Datei WAEHLT nur, welche der beiden Formen gilt:
// die volle URL-Aufloesung fuers Netz, die Pfad-Normalisierung fuer
// Dateien. Beide benutzen denselben Normalisierer — deshalb kann ein
// `../..` in einer lokalen Seite genauso wenig aus dem Ordner ausbrechen
// wie im Netz.

use alloc::string::{String, ToString};
use speedhttp::{HttpFehler, Ziel};

/// Die eingebauten Seiten. Sie haben ein eigenes Schema, damit sie im
/// Verlauf und in der Adressleiste wie jede andere Seite aussehen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intern {
    /// Was dieser Browser kann und was nicht — der Ehrlichkeits-Teil.
    Info,
    /// Der Verlauf dieses Tabs.
    Verlauf,
    /// Die gespeicherten Lesezeichen.
    Lesezeichen,
}

impl Intern {
    pub fn name(&self) -> &'static str {
        match self {
            Intern::Info => "info",
            Intern::Verlauf => "verlauf",
            Intern::Lesezeichen => "lesezeichen",
        }
    }

    fn von_name(name: &str) -> Option<Intern> {
        match name {
            "info" | "" | "start" => Some(Intern::Info),
            "verlauf" | "history" => Some(Intern::Verlauf),
            "lesezeichen" | "bookmarks" => Some(Intern::Lesezeichen),
            _ => None,
        }
    }
}

/// Das Schema der eingebauten Seiten.
pub const SCHEMA: &str = "speedos:";

/// Woher eine Seite kommt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ort {
    Netz(Ziel),
    /// Ein absoluter Pfad im VFS.
    Datei(String),
    Intern(Intern),
}

/// Was beim Deuten einer Adresse schiefgehen kann.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrtFehler {
    /// Die Adresse ergibt keine URL.
    Ungueltig,
    /// `mailto:`, `javascript:` … — in Ordnung, aber nicht ansteuerbar.
    NichtNavigierbar,
    /// `speedos:` mit einem Namen, den es nicht gibt.
    UnbekannteSeite,
}

impl OrtFehler {
    pub fn text(&self) -> &'static str {
        match self {
            OrtFehler::Ungueltig => "Diese Adresse ergibt keine gueltige URL.",
            OrtFehler::NichtNavigierbar => {
                "Verweise dieser Art oeffnet SpeedOS nicht (mailto:, javascript: und aehnliche)."
            }
            OrtFehler::UnbekannteSeite => {
                "Diese eingebaute Seite gibt es nicht. Bekannt sind speedos:info, \
                 speedos:verlauf und speedos:lesezeichen."
            }
        }
    }
}

impl From<HttpFehler> for OrtFehler {
    fn from(fehler: HttpFehler) -> OrtFehler {
        match fehler {
            HttpFehler::SchemaNichtNavigierbar => OrtFehler::NichtNavigierbar,
            _ => OrtFehler::Ungueltig,
        }
    }
}

impl Ort {
    /// Deutet eine EINGETIPPTE Adresse.
    ///
    /// Die Reihenfolge ist die Reihenfolge der Eindeutigkeit: erst die
    /// Schemata, die man ansieht, dann der absolute Pfad, und alles
    /// Uebrige ist eine Netzadresse (mit https als Standard — dieselbe
    /// Entscheidung wie `ziel_parsen` in Serie 7, Teil 5).
    pub fn parsen(eingabe: &str) -> Result<Ort, OrtFehler> {
        let text = eingabe.trim();
        if text.is_empty() {
            return Err(OrtFehler::Ungueltig);
        }
        if let Some(rest) = text.strip_prefix(SCHEMA) {
            return match Intern::von_name(rest.trim_start_matches('/')) {
                Some(seite) => Ok(Ort::Intern(seite)),
                None => Err(OrtFehler::UnbekannteSeite),
            };
        }
        // `file://` als Bequemlichkeit — der Explorer und die Shell
        // reichen aber schlicht Pfade herein.
        if let Some(rest) = text.strip_prefix("file://") {
            let pfad = if rest.starts_with('/') {
                rest.to_string()
            } else {
                let mut p = String::from("/");
                p.push_str(rest);
                p
            };
            return Ok(Ort::Datei(speedhttp::pfad_normalisieren(&pfad)));
        }
        if text.starts_with('/') {
            return Ok(Ort::Datei(speedhttp::pfad_normalisieren(text)));
        }
        Ok(Ort::Netz(speedhttp::ziel_parsen(text)?))
    }

    /// Loest einen `href` gegen DIESEN Ort auf.
    ///
    /// Liefert das Ziel, das Fragment und ob es dieselbe Seite ist. Die
    /// letzte Angabe ist die wichtige: Ein `href="#oben"` darf NICHT
    /// laden, sonst holt jeder Klick aufs Inhaltsverzeichnis die ganze
    /// Seite neu.
    pub fn aufloesen(&self, referenz: &str) -> Result<Verweis, OrtFehler> {
        let roh = referenz.trim();
        if roh.is_empty() {
            return Ok(Verweis {
                ort: self.clone(),
                fragment: None,
                gleiche_seite: true,
            });
        }
        // Ein absolutes Schema gewinnt IMMER — auch auf einer lokalen
        // Seite. `<a href="https://…">` in einer Datei ist ein Netz-Link.
        if roh.starts_with(SCHEMA) || roh.starts_with("http://") || roh.starts_with("https://") {
            let (ohne_fragment, fragment) = fragment_trennen(roh);
            let ort = Ort::parsen(ohne_fragment)?;
            let gleiche_seite = ort == *self;
            return Ok(Verweis {
                ort,
                fragment,
                gleiche_seite,
            });
        }

        match self {
            // Im Netz gilt die volle RFC-Aufloesung.
            Ort::Netz(ziel) => {
                let v = speedhttp::verweis_aufloesen(ziel, roh)?;
                Ok(Verweis {
                    ort: Ort::Netz(v.ziel),
                    fragment: v.fragment,
                    gleiche_seite: v.gleiche_seite,
                })
            }
            // Bei Dateien gibt es keinen Host — nur Pfade. Normalisiert
            // wird mit DERSELBEN Funktion, also verpufft `..` auch hier
            // an der Wurzel.
            Ort::Datei(pfad) => {
                let (ohne_fragment, fragment) = fragment_trennen(roh);
                if ohne_fragment.is_empty() {
                    return Ok(Verweis {
                        ort: self.clone(),
                        fragment,
                        gleiche_seite: true,
                    });
                }
                let zusammen = if ohne_fragment.starts_with('/') {
                    String::from(ohne_fragment)
                } else {
                    let mut p = String::from(ordner_von(pfad));
                    p.push('/');
                    p.push_str(ohne_fragment);
                    p
                };
                let neu = speedhttp::pfad_normalisieren(&zusammen);
                let gleiche_seite = neu == *pfad;
                Ok(Verweis {
                    ort: Ort::Datei(neu),
                    fragment,
                    gleiche_seite,
                })
            }
            // Von einer eingebauten Seite aus sind nur absolute Verweise
            // sinnvoll — die haben wir oben schon behandelt. Alles andere
            // waere geraten.
            Ort::Intern(_) => Err(OrtFehler::Ungueltig),
        }
    }

    /// Die Adresse als Text — was in der Adressleiste steht.
    pub fn als_text(&self) -> String {
        match self {
            Ort::Netz(ziel) => ziel.als_text(),
            Ort::Datei(pfad) => pfad.clone(),
            Ort::Intern(seite) => {
                let mut s = String::from(SCHEMA);
                s.push_str(seite.name());
                s
            }
        }
    }

    /// Ein kurzer Name fuer die Tab-Leiste, wenn es keinen `<title>` gibt.
    pub fn kurzname(&self) -> String {
        match self {
            Ort::Netz(ziel) => ziel.url.host.clone(),
            Ort::Datei(pfad) => String::from(dateiname_von(pfad)),
            Ort::Intern(seite) => String::from(seite.name()),
        }
    }
}

/// Ein aufgeloester Verweis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verweis {
    pub ort: Ort,
    pub fragment: Option<String>,
    /// `true` = dieselbe Seite, nur ein anderer Anker. **Nicht laden.**
    pub gleiche_seite: bool,
}

/// Trennt `#fragment` ab.
fn fragment_trennen(text: &str) -> (&str, Option<String>) {
    match text.find('#') {
        Some(i) => (&text[..i], Some(text[i + 1..].to_string())),
        None => (text, None),
    }
}

/// Der Ordner eines Pfades (ohne Schluss-Schraegstrich).
pub fn ordner_von(pfad: &str) -> &str {
    match pfad.rfind('/') {
        Some(0) | None => "",
        Some(i) => &pfad[..i],
    }
}

/// Der Dateiname eines Pfades.
pub fn dateiname_von(pfad: &str) -> &str {
    match pfad.rfind('/') {
        Some(i) if i + 1 < pfad.len() => &pfad[i + 1..],
        _ => pfad,
    }
}
