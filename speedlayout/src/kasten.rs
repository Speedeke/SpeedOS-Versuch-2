// speedlayout::kasten — das Box-Modell und der Kastenbaum
//
// ===========================================================================
// DAS BOX-MODELL, EINMAL AUFGESCHRIEBEN
//
//   +-----------------------------------------------+  margin-Box
//   |                  margin                        |
//   |   +---------------------------------------+   |  rahmen-Box
//   |   |               border                   |   |
//   |   |   +-------------------------------+   |   |  padding-Box
//   |   |   |            padding             |   |   |
//   |   |   |   +-----------------------+   |   |   |  INHALTS-Box
//   |   |   |   |       Inhalt          |   |   |   |
//   |   |   |   +-----------------------+   |   |   |
//   |   |   +-------------------------------+   |   |
//   |   +---------------------------------------+   |
//   +-----------------------------------------------+
//
// **Gespeichert wird die INHALTS-Box.** Alle anderen ergeben sich daraus
// durch Aufaddieren — und zwar in genau einer Richtung. Wuerde man die
// Rahmen-Box speichern, muesste man bei jeder Frage nach dem Inhalt
// zurueckrechnen, und die Rechnung staende an zwanzig Stellen.
//
// Das ist auch die Wahl der CSS-Spezifikation (`box-sizing: content-box`).
// Die moderne Alternative `border-box` waere fuer Autoren bequemer und
// fuer uns eine zweite Rechenart — wir haben sie nicht, und `box-sizing`
// steht auch nicht in der unterstuetzten Liste (docs/browser-v1.md §2.3).
//
// ===========================================================================
// DER KASTENBAUM IST NICHT DER DOM-BAUM
//
// Drei Unterschiede, und alle drei sind der Grund, warum es ihn gibt:
//
//   1. `display: none` FAELLT WEG — mitsamt Teilbaum.
//   2. Textknoten werden zu Text-Kaesten, aber nur, wenn sie nicht nur
//      aus Leerraum bestehen (dazu unten mehr).
//   3. **Anonyme Kaesten.** Ein Block-Container, der SOWOHL Inline- als
//      auch Block-Kinder hat, bekommt um jede Folge von Inline-Kindern
//      einen anonymen Block. Ohne das gaebe es keinen Ort, an dem die
//      Zeilen dieser Inline-Folge leben — und `<div>Text<p>Absatz</p></div>`
//      wuerde den Text verlieren oder falsch stapeln.

use crate::{Befund, Grenzen};
use alloc::string::String;
use alloc::vec::Vec;
use speedcss::stil::Display;
use speedcss::{Stil, StilBaum};
use speedhtml::dom::Art;
use speedhtml::{Dokument, KnotenId};

// ---------------------------------------------------------------------------
// GEOMETRIE
// ---------------------------------------------------------------------------

/// Ein Rechteck in ganzen Pixeln, absolut auf der Seite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rechteck {
    pub x: i32,
    pub y: i32,
    pub breite: i32,
    pub hoehe: i32,
}

impl Rechteck {
    pub const fn neu(x: i32, y: i32, breite: i32, hoehe: i32) -> Rechteck {
        Rechteck { x, y, breite, hoehe }
    }
    pub fn rechts(&self) -> i32 {
        self.x + self.breite
    }
    pub fn unten(&self) -> i32 {
        self.y + self.hoehe
    }
    /// Um `k` nach aussen erweitert.
    pub fn erweitert(&self, k: Kanten) -> Rechteck {
        Rechteck {
            x: self.x - k.links,
            y: self.y - k.oben,
            breite: self.breite + k.links + k.rechts,
            hoehe: self.hoehe + k.oben + k.unten,
        }
    }
    pub fn ist_leer(&self) -> bool {
        self.breite <= 0 || self.hoehe <= 0
    }
}

/// Vier Kantenmasse in ganzen Pixeln.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Kanten {
    pub oben: i32,
    pub rechts: i32,
    pub unten: i32,
    pub links: i32,
}

impl Kanten {
    pub const fn alle(v: i32) -> Kanten {
        Kanten {
            oben: v,
            rechts: v,
            unten: v,
            links: v,
        }
    }
    pub fn waagerecht(&self) -> i32 {
        self.links + self.rechts
    }
    pub fn senkrecht(&self) -> i32 {
        self.oben + self.unten
    }
    pub fn plus(&self, andere: Kanten) -> Kanten {
        Kanten {
            oben: self.oben + andere.oben,
            rechts: self.rechts + andere.rechts,
            unten: self.unten + andere.unten,
            links: self.links + andere.links,
        }
    }
}

/// Die vier Rechtecke eines Kastens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Masse {
    /// Die INHALTS-Box, absolut. Alles andere ergibt sich daraus.
    pub inhalt: Rechteck,
    pub padding: Kanten,
    pub rahmen: Kanten,
    pub margin: Kanten,
}

impl Masse {
    pub fn padding_box(&self) -> Rechteck {
        self.inhalt.erweitert(self.padding)
    }
    pub fn rahmen_box(&self) -> Rechteck {
        self.padding_box().erweitert(self.rahmen)
    }
    pub fn margin_box(&self) -> Rechteck {
        self.rahmen_box().erweitert(self.margin)
    }
    /// Was der Kasten waagerecht ZUSAETZLICH zum Inhalt braucht.
    pub fn drumherum_waagerecht(&self) -> i32 {
        self.padding.waagerecht() + self.rahmen.waagerecht() + self.margin.waagerecht()
    }
}

// ---------------------------------------------------------------------------
// DER KASTEN
// ---------------------------------------------------------------------------

/// Was fuer ein Kasten das ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KastenArt {
    /// Ein Block: stapelt sich senkrecht, nimmt die volle Breite.
    Block,
    /// Ein Inline-Kasten: `<b>`, `<a>`, `<span>`. Er hat KEINE eigene
    /// Geometrie im Blockfluss — seine Kinder wandern in die Zeilen des
    /// umgebenden Block-Containers.
    Inline,
    /// `inline-block`: aussen inline, innen ein Block. Er hat Breite und
    /// Hoehe und sitzt in einer Zeile.
    InlineBlock,
    /// Ein Textstueck. Der String ist schon leerraum-normalisiert
    /// (ausser in `<pre>`).
    Text(String),
    /// `<img>` — die Quelle und die Wunschmasse. Die BYTES holt der
    /// Browser; diese Kiste kennt keine Bilder.
    Bild {
        quelle: String,
        alt: String,
        /// Aus den Attributen bzw. dem Stil; `None` = unbekannt, dann
        /// wird ein Platzhalter gesetzt.
        breite: Option<i32>,
        hoehe: Option<i32>,
    },
    /// Ein erzwungener Umbruch (`<br>`).
    Umbruch,
    /// Eine ZEILE — entsteht erst beim Inline-Layout und enthaelt
    /// ausgerichtete Text- und Inline-Stuecke.
    Zeile,
    /// Ein ANONYMER Block. Er steht im Dokument nicht; er buendelt eine
    /// Folge von Inline-Kindern in einem gemischten Container.
    AnonymerBlock,
    /// Tabellen-Bestandteile.
    Tabelle,
    TabellenZeile,
    TabellenZelle,
    /// Das Aufzaehlungszeichen eines `list-item`.
    Marke(String),
}

impl KastenArt {
    /// Nimmt dieser Kasten am BLOCK-Fluss teil (stapelt sich senkrecht)?
    pub fn ist_block(&self) -> bool {
        matches!(
            self,
            KastenArt::Block
                | KastenArt::AnonymerBlock
                | KastenArt::Tabelle
                | KastenArt::TabellenZeile
                | KastenArt::TabellenZelle
        )
    }
    /// Gehoert dieser Kasten in eine ZEILE?
    pub fn ist_inline(&self) -> bool {
        matches!(
            self,
            KastenArt::Inline
                | KastenArt::InlineBlock
                | KastenArt::Text(_)
                | KastenArt::Bild { .. }
                | KastenArt::Umbruch
        )
    }
}

/// Ein Kasten im Layout-Baum.
#[derive(Debug, Clone)]
pub struct Kasten {
    pub art: KastenArt,
    /// Der berechnete Stil. KOPIERT und nicht geliehen: Anonyme Kaesten
    /// haben keinen Knoten, von dem sie ihn leihen koennten, und `Stil`
    /// ist `Copy`.
    pub stil: Stil,
    /// Der Knoten, aus dem er entstand — `None` bei anonymen Kaesten.
    /// Der Browser braucht ihn, um Klicks auf Links zurueckzuverfolgen.
    pub knoten: Option<KnotenId>,
    pub masse: Masse,
    pub kinder: Vec<Kasten>,
}

impl Kasten {
    pub fn neu(art: KastenArt, stil: Stil, knoten: Option<KnotenId>) -> Kasten {
        Kasten {
            art,
            stil,
            knoten,
            masse: Masse::default(),
            kinder: Vec::new(),
        }
    }

    /// Der Text eines Text-Kastens.
    pub fn text(&self) -> Option<&str> {
        match &self.art {
            KastenArt::Text(t) => Some(t),
            KastenArt::Marke(t) => Some(t),
            _ => None,
        }
    }

    /// Alle Kaesten des Teilbaums, Eltern vor Kindern.
    ///
    /// Iterativ — dieselbe Begruendung wie ueberall: Der Baum kann tief
    /// sein, und der User-Stack ist 64 KiB.
    pub fn alle(&self) -> Vec<&Kasten> {
        let mut aus = Vec::new();
        let mut stapel = alloc::vec![self];
        while let Some(k) = stapel.pop() {
            aus.push(k);
            for kind in k.kinder.iter().rev() {
                stapel.push(kind);
            }
        }
        aus
    }

    /// Der erste Kasten, dessen Knoten dieses Tag traegt — fuer Tests.
    pub fn finde_tag<'a>(&'a self, dokument: &Dokument, tag: &str) -> Option<&'a Kasten> {
        self.alle().into_iter().find(|k| {
            k.knoten
                .and_then(|id| dokument.knoten(id))
                .and_then(|n| n.name())
                == Some(tag)
        })
    }
}

// ---------------------------------------------------------------------------
// LEERRAUM
// ---------------------------------------------------------------------------

/// Leerraum zusammenfassen: mehrere Leerzeichen werden EINS, Zeilenumbrueche
/// im Quelltext werden zu Leerzeichen.
///
/// ===================================================================
/// WARUM DAS HIER UND NICHT IM PARSER PASSIERT
///
/// Der HTML-Parser liefert den Text so, wie er dasteht — mit allen
/// Umbruechen und Einrueckungen der Quelldatei. Das ist richtig: In
/// `<pre>` zaehlt jedes Zeichen, und ein Parser, der schon
/// normalisiert, hat die Information unwiederbringlich weggeworfen.
///
/// Die Regel gehoert zum LAYOUT, weil sie vom Stil abhaengt
/// (`white-space`). Wir kennen nur zwei Faelle: normal und `<pre>`.
pub fn leerraum_falten(text: &str) -> String {
    let mut aus = String::with_capacity(text.len());
    let mut letztes_war_raum = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !letztes_war_raum {
                aus.push(' ');
                letztes_war_raum = true;
            }
        } else {
            aus.push(c);
            letztes_war_raum = false;
        }
    }
    aus
}

// ---------------------------------------------------------------------------
// DEN BAUM BAUEN
// ---------------------------------------------------------------------------

/// Aus Dokument und Stilen einen Kastenbaum machen.
pub fn baum_bauen(
    dokument: &Dokument,
    stile: &StilBaum,
    grenzen: Grenzen,
    befund: &mut Befund,
) -> Kasten {
    let wurzel_stil = *stile.stil(Dokument::WURZEL);
    let mut wurzel = Kasten::neu(KastenArt::Block, wurzel_stil, Some(Dokument::WURZEL));
    let kinder = kinder_bauen(dokument, stile, Dokument::WURZEL, grenzen, befund, 0);
    wurzel.kinder = anonyme_bloecke(kinder, &wurzel_stil);
    befund.kaesten = wurzel.alle().len();
    wurzel
}

/// Die Kinder eines Knotens zu Kaesten machen.
fn kinder_bauen(
    dokument: &Dokument,
    stile: &StilBaum,
    eltern: KnotenId,
    grenzen: Grenzen,
    befund: &mut Befund,
    tiefe: usize,
) -> Vec<Kasten> {
    let mut aus = Vec::new();
    if tiefe >= grenzen.max_tiefe {
        befund.zu_tief += 1;
        befund.abgeschnitten = true;
        return aus;
    }
    let Some(knoten) = dokument.knoten(eltern) else {
        return aus;
    };
    // DIE VERERBUNG ERLEDIGT DEN BAUMLAUF. Vorher lief hier
    // `ist_vorformatiert` bis zu 32 Ebenen nach oben und suchte nach dem
    // Tag-Namen `pre`; seit `white-space` eine echte Eigenschaft ist
    // (Serie 9, Teil 2), steht die Antwort schon im berechneten Stil —
    // `white-space` wird vererbt, also hat die Kaskade den Weg nach oben
    // laengst gemacht. Eine Schleife weniger, und eine Autor-Regel wirkt.
    let vorformatiert = stile.stil(eltern).leerraum.erhaelt_leerraum();

    for kind_id in &knoten.kinder {
        let Some(kind) = dokument.knoten(*kind_id) else {
            continue;
        };
        let stil = *stile.stil(*kind_id);

        match &kind.art {
            // (1) `display: none` faellt weg — MITSAMT Teilbaum.
            _ if stil.display == Display::Keine => continue,
            // Kommentare und DOCTYPE erscheinen nie.
            Art::Kommentar(_) | Art::Doctype(_) | Art::Wurzel => continue,

            // (2) TEXT
            Art::Text(roh) => {
                let text = if vorformatiert {
                    String::from(roh.as_str())
                } else {
                    leerraum_falten(roh)
                };
                // REINER LEERRAUM ZWISCHEN BLOECKEN FAELLT WEG. Ohne das
                // erzeugt jede eingerueckte HTML-Zeile eine leere
                // Textzeile, und die Seite wird doppelt so hoch.
                // ZWISCHEN Inline-Inhalt ist ein Leerzeichen dagegen
                // bedeutsam („a <b>b</b>"), deshalb bleibt es hier stehen
                // und wird erst beim Zeilenbau am Zeilenanfang verworfen.
                if text.is_empty() {
                    continue;
                }
                if aus.len() >= grenzen.max_kaesten {
                    befund.abgeschnitten = true;
                    break;
                }
                aus.push(Kasten::neu(KastenArt::Text(text), stil, Some(*kind_id)));
            }

            // (3) ELEMENTE
            Art::Element { name, attribute } => {
                if aus.len() >= grenzen.max_kaesten {
                    befund.abgeschnitten = true;
                    break;
                }
                // `<br>` ist ein erzwungener Umbruch, kein Kasten.
                if name == "br" {
                    aus.push(Kasten::neu(KastenArt::Umbruch, stil, Some(*kind_id)));
                    continue;
                }
                // `<img>` ist ein ersetztes Element: Es hat Masse, aber
                // keinen Inhalt, den wir setzen koennten.
                if name == "img" {
                    let hole = |n: &str| -> Option<String> {
                        attribute
                            .iter()
                            .find(|(a, _)| a == n)
                            .map(|(_, w)| String::from(w.as_str()))
                    };
                    let zahl = |n: &str| -> Option<i32> {
                        hole(n).and_then(|w| w.trim().parse::<i32>().ok())
                    };
                    // Der Stil schlaegt das Attribut (so die
                    // Spezifikation): `img { width: 100px }` gewinnt.
                    let breite = stil.breite.px_ganz().or_else(|| zahl("width"));
                    let hoehe = stil.hoehe.px_ganz().or_else(|| zahl("height"));
                    aus.push(Kasten::neu(
                        KastenArt::Bild {
                            quelle: hole("src").unwrap_or_default(),
                            alt: hole("alt").unwrap_or_default(),
                            breite,
                            hoehe,
                        },
                        stil,
                        Some(*kind_id),
                    ));
                    continue;
                }

                let art = match stil.display {
                    Display::Block => KastenArt::Block,
                    Display::Inline => KastenArt::Inline,
                    Display::InlineBlock => KastenArt::InlineBlock,
                    Display::Listenpunkt => KastenArt::Block,
                    Display::Tabelle => KastenArt::Tabelle,
                    // Eine Tabellen-Gruppe (`<tbody>`) ist fuer unser
                    // Layout durchsichtig: Ihre Zeilen gehoeren der
                    // Tabelle. Als eigener Kasten waere sie eine Ebene,
                    // die die Spaltenrechnung nur verstecken wuerde.
                    Display::TabellenGruppe => KastenArt::Block,
                    Display::TabellenZeile => KastenArt::TabellenZeile,
                    Display::TabellenZelle => KastenArt::TabellenZelle,
                    Display::Keine => continue,
                };

                let mut kasten = Kasten::neu(art.clone(), stil, Some(*kind_id));
                let enkel = kinder_bauen(dokument, stile, *kind_id, grenzen, befund, tiefe + 1);

                // Ein `list-item` bekommt sein Aufzaehlungszeichen als
                // erstes Kind — als eigener Kasten, damit das Layout es
                // wie jeden anderen Inhalt behandeln kann.
                if stil.display == Display::Listenpunkt {
                    let nummer = geschwister_nummer(dokument, *kind_id);
                    let marke = marke_text(stil.listenzeichen, nummer);
                    if !marke.is_empty() {
                        kasten
                            .kinder
                            .push(Kasten::neu(KastenArt::Marke(marke), stil, Some(*kind_id)));
                    }
                }

                // Ein BLOCK-Container mit gemischten Kindern braucht
                // anonyme Bloecke; ein INLINE-Kasten nicht (seine Kinder
                // wandern ohnehin in die Zeilen des Vorfahren).
                if art.ist_block() || art == KastenArt::InlineBlock {
                    let mit_anonymen = anonyme_bloecke(enkel, &stil);
                    kasten.kinder.extend(mit_anonymen);
                } else {
                    kasten.kinder.extend(enkel);
                }
                aus.push(kasten);
            }
        }
    }
    aus
}

/// Der wievielte Listenpunkt ist das (fuer `list-style-type: decimal`)?
fn geschwister_nummer(dokument: &Dokument, id: KnotenId) -> usize {
    let Some(knoten) = dokument.knoten(id) else {
        return 1;
    };
    let Some(eltern) = knoten.eltern.and_then(|e| dokument.knoten(e)) else {
        return 1;
    };
    let mut n = 0usize;
    for kind in &eltern.kinder {
        if dokument.knoten(*kind).and_then(|k| k.name()) == Some("li") {
            n += 1;
            if *kind == id {
                return n;
            }
        }
    }
    n.max(1)
}

/// Der Text eines Aufzaehlungszeichens.
///
/// Roemische Zahlen und Buchstaben sind dabei, weil sie in Fliesstext
/// vorkommen und billig sind. Ueber 3999 (roemisch nicht darstellbar)
/// bzw. 26 Buchstaben faellt es auf Dezimal zurueck — sichtbar richtig
/// statt sichtbar falsch.
fn marke_text(zeichen: speedcss::Listenzeichen, nummer: usize) -> String {
    use alloc::format;
    use speedcss::Listenzeichen as L;
    match zeichen {
        L::Keins => String::new(),
        // Ein Leerzeichen dahinter, damit die Marke nicht am Text klebt.
        L::Punkt => String::from("\u{2022} "),
        L::Kreis => String::from("\u{25E6} "),
        L::Quadrat => String::from("\u{25AA} "),
        L::Dezimal => format!("{}. ", nummer),
        L::LateinKlein => format!("{}. ", buchstabe(nummer, b'a')),
        L::LateinGross => format!("{}. ", buchstabe(nummer, b'A')),
        L::RoemischKlein => format!("{}. ", roemisch(nummer, false)),
        L::RoemischGross => format!("{}. ", roemisch(nummer, true)),
    }
}

fn buchstabe(nummer: usize, ab: u8) -> String {
    if nummer == 0 || nummer > 26 {
        return alloc::format!("{}", nummer);
    }
    String::from((ab + (nummer as u8 - 1)) as char)
}

fn roemisch(nummer: usize, gross: bool) -> String {
    if nummer == 0 || nummer > 3999 {
        return alloc::format!("{}", nummer);
    }
    const WERTE: [(usize, &str, &str); 13] = [
        (1000, "M", "m"),
        (900, "CM", "cm"),
        (500, "D", "d"),
        (400, "CD", "cd"),
        (100, "C", "c"),
        (90, "XC", "xc"),
        (50, "L", "l"),
        (40, "XL", "xl"),
        (10, "X", "x"),
        (9, "IX", "ix"),
        (5, "V", "v"),
        (4, "IV", "iv"),
        (1, "I", "i"),
    ];
    let mut rest = nummer;
    let mut aus = String::new();
    for (wert, g, k) in WERTE {
        while rest >= wert {
            aus.push_str(if gross { g } else { k });
            rest -= wert;
        }
    }
    aus
}

/// ANONYME BLOECKE einziehen.
///
/// ===================================================================
/// DIE REGEL, OHNE DIE GEMISCHTE CONTAINER ZERFALLEN
///
/// Ein Block-Container enthaelt ENTWEDER nur Bloecke ODER nur Inlines.
/// Steht beides darin — `<div>Text<p>Absatz</p>mehr Text</div>` —, dann
/// bekommt JEDE Folge von Inline-Kindern einen anonymen Block um sich.
///
/// Ohne diese Regel gaebe es keinen Ort, an dem die Zeilen der
/// Inline-Folge leben: Das Block-Layout stapelt seine Kinder senkrecht
/// und wuesste nicht, was es mit einem nackten Textstueck anfangen soll.
///
/// Nur-Inline-Container bleiben unangetastet (der haeufigste Fall — jeder
/// `<p>`), sonst haette jeder Absatz eine ueberfluessige Ebene.
fn anonyme_bloecke(kinder: Vec<Kasten>, eltern_stil: &Stil) -> Vec<Kasten> {
    let hat_block = kinder.iter().any(|k| k.art.ist_block());
    if !hat_block {
        return kinder; // reiner Inline-Container: nichts zu tun
    }

    let mut aus: Vec<Kasten> = Vec::new();
    let mut sammler: Vec<Kasten> = Vec::new();

    for kind in kinder {
        if kind.art.ist_block() {
            anonymen_abschliessen(&mut sammler, &mut aus, eltern_stil);
            aus.push(kind);
        } else {
            sammler.push(kind);
        }
    }
    anonymen_abschliessen(&mut sammler, &mut aus, eltern_stil);
    aus
}

fn anonymer_ist_leer(sammler: &[Kasten]) -> bool {
    sammler.iter().all(|k| match &k.art {
        KastenArt::Text(t) => t.trim().is_empty(),
        _ => false,
    })
}

fn anonymen_abschliessen(sammler: &mut Vec<Kasten>, aus: &mut Vec<Kasten>, eltern_stil: &Stil) {
    if sammler.is_empty() {
        return;
    }
    // Ein anonymer Block aus lauter Leerraum waere eine leere Zeile
    // zwischen zwei Bloecken — genau der Fehler, den eingerueckter
    // HTML-Quelltext sonst erzeugt.
    if anonymer_ist_leer(sammler) {
        sammler.clear();
        return;
    }
    let mut anonym = Kasten::neu(KastenArt::AnonymerBlock, *eltern_stil, None);
    // Ein anonymer Block hat KEINE eigenen Raender, Rahmen oder
    // Hintergruende — er erbt nur, was Text betrifft. Sonst bekaeme
    // `<div style="margin:20px">Text<p>x</p></div>` den Rand zweimal.
    anonym.stil.margin = speedcss::Kanten::alle(speedcss::Laenge::Px(0));
    anonym.stil.padding = speedcss::Kanten::alle(speedcss::Laenge::Px(0));
    anonym.stil.rahmen_breite = speedcss::Kanten::alle(speedcss::Laenge::Px(0));
    anonym.stil.hintergrund = speedcss::Farbe::DURCHSICHTIG;
    anonym.stil.breite = speedcss::Laenge::Auto;
    anonym.stil.hoehe = speedcss::Laenge::Auto;
    anonym.kinder = core::mem::take(sammler);
    aus.push(anonym);
}
