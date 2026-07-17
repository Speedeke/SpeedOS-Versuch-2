// ui/texteditor.rs — Mehrzeiliger Textpuffer + Editor-Widget
//
// Das Herz von SpeedText. Zwei Teile:
//
//   * `TextPuffer` — die reine, unit-getestete Editier-Logik:
//     Einfügen/Löschen an der Cursor-Position (auch über
//     Zeilengrenzen), Cursor-Bewegung (Pfeile, Pos1/Ende, Bild),
//     Scroll-Zustand. WARUM ein Vec<String> statt Rope/Gap-Buffer?
//     Bewusste Wahl: Unsere Dateien sind Kilobytes (RamFs, Configs,
//     Notizen) — jede Operation kostet O(Zeilenlänge), das ist bei
//     80-Zeichen-Zeilen unmessbar. Ein Rope lohnt erst, wenn
//     Megabyte-Dateien mit ständigen Einfügungen in der Mitte kommen;
//     seine Komplexität (Baum-Balancierung, Iteratoren) wäre hier
//     nur Lernballast am falschen Ort. Die Naht bleibt: Wer später
//     einen Rope will, tauscht NUR dieses Struct.
//   * `TextEditor` — das Widget: Zeilennummern-Spalte, vertikales
//     Scrolling mit ziehbarem Balken, blinkender Cursor, Klick setzt
//     den Cursor. Der PUFFER liegt in einem Arc<Mutex<...>>, das
//     sich Widget und App teilen — so überlebt der Text die
//     Neu-Aufbauten des Widget-Baums (dasselbe Problem löste der
//     Explorer mit App-Zustand; hier ist der Zustand zu groß und zu
//     heiß, um ihn bei jeder Nachricht zu kopieren).
//
// Cursor-Spalten zählen ZEICHEN (chars), nicht Bytes — sonst
// zerschneidet ein Umlaut die UTF-8-Sequenz.

use super::{UiEreignis, UiReaktion, Widget};
use crate::fenster::FensterPuffer;
use crate::grafik::{Rechteck, Zeichner};
use crate::theme::{self, metrik};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use noto_sans_mono_bitmap::{get_raster_width, FontWeight};
use pc_keyboard::{DecodedKey, KeyCode};
use spin::Mutex;

// ---------------------------------------------------------------------------
// TextPuffer — reine Editier-Logik
// ---------------------------------------------------------------------------

pub struct TextPuffer {
    /// Der Text als Zeilen (immer mindestens EINE, ggf. leere Zeile).
    zeilen: Vec<String>,
    pub cursor_zeile: usize,
    /// Cursor-Spalte in ZEICHEN (0 = vor dem ersten Zeichen).
    pub cursor_spalte: usize,
    /// Erste sichtbare Zeile (vertikales Scrolling).
    pub scroll_zeile: usize,
    /// Ungespeicherte Änderungen? (Titel-Stern, Schließen-Dialog)
    pub geaendert: bool,
}

impl TextPuffer {
    pub fn leer() -> Self {
        TextPuffer {
            zeilen: alloc::vec![String::new()],
            cursor_zeile: 0,
            cursor_spalte: 0,
            scroll_zeile: 0,
            geaendert: false,
        }
    }

    /// Baut den Puffer aus einem Text (Datei-Inhalt).
    pub fn aus_text(text: &str) -> Self {
        let mut zeilen: Vec<String> = text.lines().map(String::from).collect();
        // lines() verschluckt ein abschließendes \n — die leere
        // Schlusszeile gehört aber zum Editier-Gefühl dazu:
        if text.ends_with('\n') || zeilen.is_empty() {
            zeilen.push(String::new());
        }
        TextPuffer { zeilen, ..TextPuffer::leer() }
    }

    /// Der komplette Text (Zeilen mit \n verbunden) — fürs Speichern.
    pub fn als_text(&self) -> String {
        self.zeilen.join("\n")
    }

    pub fn zeilen_anzahl(&self) -> usize {
        self.zeilen.len()
    }

    pub fn zeile(&self, index: usize) -> &str {
        &self.zeilen[index]
    }

    /// Gesamtzahl der Zeichen (Zeilenumbrüche zählen mit).
    pub fn zeichen_anzahl(&self) -> usize {
        let text: usize = self.zeilen.iter().map(|z| z.chars().count()).sum();
        text + self.zeilen.len().saturating_sub(1) // die \n dazwischen
    }

    /// Zeichenlänge der aktuellen Cursor-Zeile.
    fn zeilen_laenge(&self, zeile: usize) -> usize {
        self.zeilen[zeile].chars().count()
    }

    /// Byte-Position der Cursor-Spalte in der Zeile (chars -> Bytes).
    fn byte_spalte(zeile: &str, spalte: usize) -> usize {
        zeile
            .char_indices()
            .nth(spalte)
            .map(|(index, _)| index)
            .unwrap_or(zeile.len())
    }

    /// Fügt ein Zeichen an der Cursor-Position ein ('\n' teilt die
    /// Zeile — der Rest wandert in eine neue Zeile darunter).
    pub fn einfuegen(&mut self, zeichen: char) {
        let byte = Self::byte_spalte(&self.zeilen[self.cursor_zeile], self.cursor_spalte);
        if zeichen == '\n' {
            let rest = self.zeilen[self.cursor_zeile].split_off(byte);
            self.zeilen.insert(self.cursor_zeile + 1, rest);
            self.cursor_zeile += 1;
            self.cursor_spalte = 0;
        } else {
            self.zeilen[self.cursor_zeile].insert(byte, zeichen);
            self.cursor_spalte += 1;
        }
        self.geaendert = true;
    }

    /// Backspace: Zeichen VOR dem Cursor löschen; am Zeilenanfang
    /// verschmilzt die Zeile mit der darüber.
    pub fn backspace(&mut self) {
        if self.cursor_spalte > 0 {
            self.cursor_spalte -= 1;
            let byte = Self::byte_spalte(&self.zeilen[self.cursor_zeile], self.cursor_spalte);
            self.zeilen[self.cursor_zeile].remove(byte);
            self.geaendert = true;
        } else if self.cursor_zeile > 0 {
            let zeile = self.zeilen.remove(self.cursor_zeile);
            self.cursor_zeile -= 1;
            self.cursor_spalte = self.zeilen_laenge(self.cursor_zeile);
            self.zeilen[self.cursor_zeile].push_str(&zeile);
            self.geaendert = true;
        }
    }

    /// Entf: Zeichen HINTER dem Cursor löschen; am Zeilenende zieht
    /// es die Folgezeile heran.
    pub fn entfernen(&mut self) {
        if self.cursor_spalte < self.zeilen_laenge(self.cursor_zeile) {
            let byte = Self::byte_spalte(&self.zeilen[self.cursor_zeile], self.cursor_spalte);
            self.zeilen[self.cursor_zeile].remove(byte);
            self.geaendert = true;
        } else if self.cursor_zeile + 1 < self.zeilen.len() {
            let zeile = self.zeilen.remove(self.cursor_zeile + 1);
            self.zeilen[self.cursor_zeile].push_str(&zeile);
            self.geaendert = true;
        }
    }

    // ----- Cursor-Bewegung (Spalte wird auf die Zeilenlänge geklemmt) -----

    pub fn links(&mut self) {
        if self.cursor_spalte > 0 {
            self.cursor_spalte -= 1;
        } else if self.cursor_zeile > 0 {
            // Über die Zeilengrenze ans Ende der Zeile darüber.
            self.cursor_zeile -= 1;
            self.cursor_spalte = self.zeilen_laenge(self.cursor_zeile);
        }
    }

    pub fn rechts(&mut self) {
        if self.cursor_spalte < self.zeilen_laenge(self.cursor_zeile) {
            self.cursor_spalte += 1;
        } else if self.cursor_zeile + 1 < self.zeilen.len() {
            self.cursor_zeile += 1;
            self.cursor_spalte = 0;
        }
    }

    pub fn hoch(&mut self) {
        if self.cursor_zeile > 0 {
            self.cursor_zeile -= 1;
            self.cursor_spalte = self.cursor_spalte.min(self.zeilen_laenge(self.cursor_zeile));
        }
    }

    pub fn runter(&mut self) {
        if self.cursor_zeile + 1 < self.zeilen.len() {
            self.cursor_zeile += 1;
            self.cursor_spalte = self.cursor_spalte.min(self.zeilen_laenge(self.cursor_zeile));
        }
    }

    pub fn pos1(&mut self) {
        self.cursor_spalte = 0;
    }

    pub fn ende(&mut self) {
        self.cursor_spalte = self.zeilen_laenge(self.cursor_zeile);
    }

    /// Bild hoch/runter: eine Seitenhöhe springen (Cursor UND Scroll).
    pub fn bild(&mut self, seiten_zeilen: usize, runter: bool) {
        if runter {
            self.cursor_zeile = (self.cursor_zeile + seiten_zeilen).min(self.zeilen.len() - 1);
        } else {
            self.cursor_zeile = self.cursor_zeile.saturating_sub(seiten_zeilen);
        }
        self.cursor_spalte = self.cursor_spalte.min(self.zeilen_laenge(self.cursor_zeile));
    }

    /// Holt den Cursor in den sichtbaren Bereich (nach jeder Aktion).
    pub fn cursor_sichtbar_machen(&mut self, sicht_zeilen: usize) {
        let sicht_zeilen = sicht_zeilen.max(1);
        if self.cursor_zeile < self.scroll_zeile {
            self.scroll_zeile = self.cursor_zeile;
        } else if self.cursor_zeile >= self.scroll_zeile + sicht_zeilen {
            self.scroll_zeile = self.cursor_zeile + 1 - sicht_zeilen;
        }
    }
}

// ---------------------------------------------------------------------------
// TextEditor — das Widget
// ---------------------------------------------------------------------------

/// Geteilter Puffer (App <-> Widget): BLATT-Lock, nur unter dem
/// MANAGER-Lock kurz genommen (zeichnen/ereignis/nachricht).
pub type GeteilterPuffer = Arc<Mutex<TextPuffer>>;

pub fn geteilter_puffer(puffer: TextPuffer) -> GeteilterPuffer {
    Arc::new(Mutex::new(puffer))
}

pub struct TextEditor {
    puffer: GeteilterPuffer,
    /// Nachricht an die App nach JEDER Änderung/Cursorbewegung
    /// (Statuszeile + Titel-Stern aktualisieren).
    nachricht: u32,
    fokus: bool,
    /// Wird der Scrollbalken-Griff gezogen? (y-Versatz im Griff)
    balken_griff: Option<i32>,
}

impl TextEditor {
    pub fn neu(puffer: GeteilterPuffer, nachricht: u32) -> Self {
        TextEditor { puffer, nachricht, fokus: true, balken_griff: None }
    }

    fn zeichen_breite() -> i32 {
        get_raster_width(FontWeight::Regular, metrik().schrift_ui) as i32
    }

    /// Breite der Zeilennummern-Spalte (Stellenzahl + Luft).
    fn nummern_breite(zeilen_anzahl: usize) -> i32 {
        let stellen = alloc::format!("{}", zeilen_anzahl.max(1)).len() as i32;
        stellen * Self::zeichen_breite() + metrik().abstand * 2
    }

    /// Sichtbare Textzeilen im Bereich.
    fn sicht_zeilen(bereich: Rechteck) -> usize {
        (bereich.hoehe / metrik().zeilen_hoehe).max(1) as usize
    }

    /// Scrollbalken-Griff (None = alles sichtbar) — dieselbe
    /// Geometrie-Idee wie in der ScrollListe, nur in Zeilen.
    fn balken_rechteck(&self, bereich: Rechteck, zeilen: usize, scroll: usize) -> Option<Rechteck> {
        let sicht = Self::sicht_zeilen(bereich);
        if zeilen <= sicht {
            return None;
        }
        let hoehe = (bereich.hoehe * sicht as i32 / zeilen as i32).max(24);
        let weg = bereich.hoehe - hoehe;
        let y = bereich.y + weg * scroll as i32 / (zeilen - sicht) as i32;
        Some(Rechteck::neu(
            bereich.x + bereich.breite - metrik().scrollbalken_breite,
            y,
            metrik().scrollbalken_breite,
            hoehe,
        ))
    }

    /// Verarbeitet eine Taste im Puffer. true = verarbeitet.
    fn taste_im_puffer(puffer: &mut TextPuffer, taste: DecodedKey, sicht: usize) -> bool {
        match taste {
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => puffer.einfuegen('\n'),
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => puffer.backspace(),
            DecodedKey::RawKey(KeyCode::Delete) => puffer.entfernen(),
            DecodedKey::RawKey(KeyCode::ArrowLeft) => puffer.links(),
            DecodedKey::RawKey(KeyCode::ArrowRight) => puffer.rechts(),
            DecodedKey::RawKey(KeyCode::ArrowUp) => puffer.hoch(),
            DecodedKey::RawKey(KeyCode::ArrowDown) => puffer.runter(),
            DecodedKey::RawKey(KeyCode::Home) => puffer.pos1(),
            DecodedKey::RawKey(KeyCode::End) => puffer.ende(),
            DecodedKey::RawKey(KeyCode::PageUp) => puffer.bild(sicht, false),
            DecodedKey::RawKey(KeyCode::PageDown) => puffer.bild(sicht, true),
            DecodedKey::Unicode(zeichen) if zeichen >= ' ' => puffer.einfuegen(zeichen),
            _ => return false,
        }
        puffer.cursor_sichtbar_machen(sicht);
        true
    }
}

impl Widget for TextEditor {
    fn wunschgroesse(&self) -> (i32, i32) {
        (200, 4 * metrik().zeilen_hoehe)
    }

    fn flex(&self) -> i32 {
        1 // nimmt den Restplatz des Fensters
    }

    fn hat_fokus(&self) -> bool {
        self.fokus
    }

    fn fokus_weiter(&mut self) -> bool {
        self.fokus = !self.fokus;
        self.fokus
    }

    fn fokus_entfernen(&mut self) {
        self.fokus = false;
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        let puffer = x86_64::instructions::interrupts::without_interrupts(|| {
            // Zum Zeichnen reicht ein kurzer Blick — wir kopieren die
            // SICHTBAREN Zeilen heraus (Blatt-Lock nicht über das
            // ganze Rendern halten).
            let p = self.puffer.lock();
            let sicht = Self::sicht_zeilen(bereich);
            let von = p.scroll_zeile.min(p.zeilen_anzahl().saturating_sub(1));
            let bis = (von + sicht).min(p.zeilen_anzahl());
            (
                (von..bis).map(|i| String::from(p.zeile(i))).collect::<Vec<_>>(),
                von,
                p.zeilen_anzahl(),
                p.cursor_zeile,
                p.cursor_spalte,
                p.scroll_zeile,
            )
        });
        let (zeilen, von, gesamt, cursor_zeile, cursor_spalte, scroll) = puffer;

        let zb = Self::zeichen_breite();
        let zh = metrik().zeilen_hoehe;
        let nummern = Self::nummern_breite(gesamt);

        // Grundflächen: Nummern-Spalte gedimmt, Textfläche wie ein
        // Eingabefeld, Rahmen zeigt den Fokus.
        z.rechteck_fuellen(bereich, thema.eingabefeld);
        z.rechteck_fuellen(
            Rechteck::neu(bereich.x, bereich.y, nummern, bereich.hoehe),
            thema.flaeche,
        );
        z.rechteck_rahmen(bereich, if self.fokus { thema.akzent } else { thema.rahmen_passiv });

        z.clip_setzen(Some(Rechteck::neu(
            bereich.x + 1,
            bereich.y + 1,
            bereich.breite - 2,
            bereich.hoehe - 2,
        )));
        let text_x = bereich.x + nummern + metrik().abstand;
        for (i, zeile) in zeilen.iter().enumerate() {
            let y = bereich.y + 2 + i as i32 * zh;
            let nummer = von + i + 1;
            z.text(
                bereich.x + metrik().abstand,
                y,
                &alloc::format!("{}", nummer),
                metrik().schrift_ui,
                FontWeight::Regular,
                thema.text_gedimmt,
            );
            z.text(text_x, y, zeile, metrik().schrift_ui, FontWeight::Regular, thema.text_normal);
        }

        // Cursor (blinkend, Tempo aus den Einstellungen):
        if self.fokus
            && cursor_zeile >= von
            && cursor_zeile < von + zeilen.len().max(1)
            && (crate::zeit::us_seit_boot() / crate::einstellungen::cursor_blink_us())
                .is_multiple_of(2)
        {
            let cx = text_x + cursor_spalte as i32 * zb;
            let cy = bereich.y + 2 + (cursor_zeile - von) as i32 * zh;
            z.rechteck_fuellen(Rechteck::neu(cx, cy, 2, zh), thema.akzent);
        }
        z.clip_setzen(None);

        // Scrollbalken:
        if let Some(griff) = self.balken_rechteck(bereich, gesamt, scroll) {
            z.rechteck_fuellen(
                Rechteck::neu(griff.x, bereich.y, metrik().scrollbalken_breite, bereich.hoehe),
                thema.leiste_knopf,
            );
            z.rechteck_abgerundet(
                griff,
                metrik().radius_klein,
                if self.balken_griff.is_some() { thema.akzent } else { thema.text_gedimmt },
            );
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        let sicht = Self::sicht_zeilen(bereich);
        match ereignis {
            UiEreignis::Taste(taste) if self.fokus => {
                let verarbeitet = x86_64::instructions::interrupts::without_interrupts(|| {
                    Self::taste_im_puffer(&mut self.puffer.lock(), *taste, sicht)
                });
                if verarbeitet {
                    UiReaktion::nachricht(self.nachricht)
                } else {
                    UiReaktion::verbraucht() // fokussiert: schlucken
                }
            }
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                self.fokus = true;
                let (zeilen, scroll) = x86_64::instructions::interrupts::without_interrupts(|| {
                    let p = self.puffer.lock();
                    (p.zeilen_anzahl(), p.scroll_zeile)
                });
                // Scrollbalken zuerst (liegt über dem Text):
                if let Some(griff) = self.balken_rechteck(bereich, zeilen, scroll) {
                    if griff.enthaelt(*x, *y) {
                        self.balken_griff = Some(*y - griff.y);
                        return UiReaktion::neu_zeichnen();
                    }
                    if *x >= griff.x {
                        // Spur-Klick: eine Seite springen.
                        let runter = *y >= griff.y;
                        x86_64::instructions::interrupts::without_interrupts(|| {
                            let mut p = self.puffer.lock();
                            p.bild(sicht, runter);
                            p.cursor_sichtbar_machen(sicht);
                        });
                        return UiReaktion::nachricht(self.nachricht);
                    }
                }
                // Klick in den Text: Cursor dorthin setzen.
                let nummern = Self::nummern_breite(zeilen);
                let text_x = bereich.x + nummern + metrik().abstand;
                let spalte = ((*x - text_x).max(0) / Self::zeichen_breite()) as usize;
                let zeile = scroll + ((*y - bereich.y - 2).max(0) / metrik().zeilen_hoehe) as usize;
                x86_64::instructions::interrupts::without_interrupts(|| {
                    let mut p = self.puffer.lock();
                    p.cursor_zeile = zeile.min(p.zeilen_anzahl() - 1);
                    p.cursor_spalte = spalte.min(p.zeile(p.cursor_zeile).chars().count());
                });
                UiReaktion::nachricht(self.nachricht)
            }
            UiEreignis::Bewegt { y, .. } => {
                if let Some(versatz) = self.balken_griff {
                    x86_64::instructions::interrupts::without_interrupts(|| {
                        let mut p = self.puffer.lock();
                        let zeilen = p.zeilen_anzahl();
                        if zeilen > sicht {
                            let griff_hoehe =
                                (bereich.hoehe * sicht as i32 / zeilen as i32).max(24);
                            let weg = (bereich.hoehe - griff_hoehe).max(1);
                            let ziel = (*y - versatz - bereich.y).clamp(0, weg);
                            p.scroll_zeile =
                                (ziel * (zeilen - sicht) as i32 / weg).max(0) as usize;
                        }
                    });
                    return UiReaktion::neu_zeichnen();
                }
                UiReaktion::ignoriert()
            }
            UiEreignis::Losgelassen { .. } | UiEreignis::MausRaus => {
                if self.balken_griff.take().is_some() {
                    return UiReaktion::neu_zeichnen();
                }
                UiReaktion::ignoriert()
            }
            UiEreignis::Scroll { delta, x, y } if bereich.enthaelt(*x, *y) => {
                x86_64::instructions::interrupts::without_interrupts(|| {
                    let mut p = self.puffer.lock();
                    let max_scroll = p.zeilen_anzahl().saturating_sub(sicht);
                    let neu = p.scroll_zeile as i64 - *delta as i64 * 3;
                    p.scroll_zeile = neu.clamp(0, max_scroll as i64) as usize;
                });
                UiReaktion::neu_zeichnen()
            }
            UiEreignis::FokusRein => {
                self.fokus = true;
                UiReaktion::neu_zeichnen()
            }
            UiEreignis::FokusRaus => {
                self.fokus = false;
                UiReaktion::neu_zeichnen()
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

// ---------------------------------------------------------------------------
// StatusZeile — liest ihre Werte beim ZEICHNEN live aus dem Puffer
//
// Performance-Pass Serie 3: Vorher baute SpeedText bei JEDER Taste
// den ganzen Widget-Baum neu, nur damit die Statuszeile (Zeile:
// Spalte, Zeichen, Status) aktuell war. Da der Puffer ohnehin
// geteilt ist, kann die Statuszeile ihre Zahlen einfach beim
// Zeichnen holen — Tippen braucht dann NUR noch Neuzeichnen.
// ---------------------------------------------------------------------------

pub struct StatusZeile {
    puffer: GeteilterPuffer,
}

impl StatusZeile {
    pub fn neu(puffer: GeteilterPuffer) -> Self {
        StatusZeile { puffer }
    }
}

impl Widget for StatusZeile {
    fn wunschgroesse(&self) -> (i32, i32) {
        (0, metrik().zeilen_hoehe)
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, FensterPuffer>, bereich: Rechteck) {
        let (zeile, spalte, zeichen, geaendert) =
            x86_64::instructions::interrupts::without_interrupts(|| {
                let p = self.puffer.lock();
                (p.cursor_zeile + 1, p.cursor_spalte + 1, p.zeichen_anzahl(), p.geaendert)
            });
        let text = alloc::format!(
            "Zeile {}, Spalte {}  |  {} Zeichen  |  {}",
            zeile,
            spalte,
            zeichen,
            if geaendert { "Geaendert *" } else { "Gespeichert" }
        );
        z.text(
            bereich.x,
            bereich.y,
            &text,
            metrik().schrift_ui,
            FontWeight::Regular,
            theme::aktuell().text_sekundaer,
        );
    }

    fn ereignis(&mut self, _e: &UiEreignis, _b: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Tests — der Puffer pur, ohne Fenster
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Einfügen über Zeilengrenzen: \n teilt, Backspace am
    /// Zeilenanfang und Entf am Zeilenende verschmelzen wieder.
    #[test_case]
    fn test_einfuegen_loeschen_ueber_zeilengrenzen() {
        let mut p = TextPuffer::aus_text("Hallo Welt");
        assert_eq!(p.zeilen_anzahl(), 1);

        // Cursor hinter "Hallo", dann Zeile teilen:
        p.cursor_spalte = 5;
        p.einfuegen('\n');
        assert_eq!(p.als_text(), "Hallo\n Welt");
        assert_eq!((p.cursor_zeile, p.cursor_spalte), (1, 0));
        assert!(p.geaendert);

        // Backspace am Zeilenanfang verschmilzt zurück:
        p.backspace();
        assert_eq!(p.als_text(), "Hallo Welt");
        assert_eq!((p.cursor_zeile, p.cursor_spalte), (0, 5));

        // Entf am Zeilenende zieht die Folgezeile heran:
        let mut p = TextPuffer::aus_text("eins\nzwei");
        p.cursor_zeile = 0;
        p.cursor_spalte = 4;
        p.entfernen();
        assert_eq!(p.als_text(), "einszwei");

        // Umlaute (mehrbyte-UTF-8) über die Zeichen-Spalte:
        let mut p = TextPuffer::aus_text("äöü");
        p.cursor_spalte = 2;
        p.einfuegen('x');
        assert_eq!(p.als_text(), "äöxü");
        p.backspace();
        assert_eq!(p.als_text(), "äöü");
    }

    /// Cursor-Bewegung: links/rechts wandern über Zeilengrenzen,
    /// hoch/runter klemmen die Spalte, Pos1/Ende/Bild springen.
    #[test_case]
    fn test_cursor_bewegung() {
        let mut p = TextPuffer::aus_text("lang und laenger\nkurz\ndritte Zeile");

        // Rechts am Zeilenende springt an den Anfang der nächsten:
        p.cursor_zeile = 0;
        p.ende();
        assert_eq!(p.cursor_spalte, 16);
        p.rechts();
        assert_eq!((p.cursor_zeile, p.cursor_spalte), (1, 0));
        // Links am Zeilenanfang springt ans Ende der vorigen:
        p.links();
        assert_eq!((p.cursor_zeile, p.cursor_spalte), (0, 16));

        // Runter in die kurze Zeile klemmt die Spalte:
        p.runter();
        assert_eq!((p.cursor_zeile, p.cursor_spalte), (1, 4));

        p.pos1();
        assert_eq!(p.cursor_spalte, 0);

        // Bild runter über das Ende hinaus bleibt in der letzten Zeile:
        p.bild(10, true);
        assert_eq!(p.cursor_zeile, 2);
        p.bild(10, false);
        assert_eq!(p.cursor_zeile, 0);

        // Scroll folgt dem Cursor:
        let mut p = TextPuffer::aus_text(&"x\n".repeat(50));
        p.cursor_zeile = 40;
        p.cursor_sichtbar_machen(10);
        assert_eq!(p.scroll_zeile, 31); // 40 sichtbar als letzte von 10
        p.cursor_zeile = 5;
        p.cursor_sichtbar_machen(10);
        assert_eq!(p.scroll_zeile, 5);
    }

    /// aus_text/als_text-Roundtrip inklusive Schlusszeilen-Regel
    /// und die Zeichen-Zählung (Umbrüche zählen mit).
    #[test_case]
    fn test_text_roundtrip_und_zaehlung() {
        for text in ["", "eine Zeile", "a\nb\nc", "endet mit Umbruch\n"] {
            let p = TextPuffer::aus_text(text);
            assert_eq!(p.als_text(), text, "Roundtrip kaputt fuer {:?}", text);
            assert!(!p.geaendert);
        }
        let p = TextPuffer::aus_text("ab\ncd");
        assert_eq!(p.zeichen_anzahl(), 5); // a b \n c d
        assert_eq!(TextPuffer::leer().zeichen_anzahl(), 0);
    }
}
