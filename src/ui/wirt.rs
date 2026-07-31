// ui/wirt.rs — DER KERNEL ALS WIRT VON speedui (Serie 8, Teil 2)
//
// ==========================================================================
// DIE ANDERE SEITE DER UMKEHR
//
// `speedui` sagt in fuenf Traits, WAS es braucht. Hier steht, WIE der
// Kernel es liefert — und zwar vollstaendig: Wenn eine Methode hier
// fehlte, waere die Kiste nicht benutzbar, und das faellt beim Uebersetzen
// auf. Genau das ist der Gewinn gegenueber `metrik()` und
// `theme::aktuell()`: Vorher konnte das Toolkit sich still bedienen, jetzt
// muss der Wirt sich ausdruecklich erklaeren.
//
// Die zweite Implementierung dieser Traits steht in `userland/uidemo` —
// und die sieht ganz anders aus (5x7-Raster statt vorgerasterter Schrift,
// feste Farben statt Theme-System, Syscall-Uhr statt TSC). Dass beide
// dieselben Widgets bedienen, ist der Beweis, dass die Grenze traegt.

use crate::fenster::FensterPuffer;
use crate::grafik::{Rgba, Zeichenflaeche, Zeichner};
use crate::theme::{self, metrik};
use alloc::string::String;
use alloc::vec::Vec;
use noto_sans_mono_bitmap::{get_raster_width, FontWeight, RasterHeight};
use pc_keyboard::{DecodedKey, KeyCode};
use speedui::{Farbe, Farbrolle, Icon, Leinwand, Mass, Rechteck, Schrift, Stil, Taste, Thema, Uhr};

// ---------------------------------------------------------------------------
// (1) THEMA
// ---------------------------------------------------------------------------

/// Reicht das Kernel-Theme und die Metrik an die Kiste durch.
///
/// Die Uebersetzung ist eine reine Tabelle — und genau deshalb ist sie
/// wertvoll: Sie ist die vollstaendige, nachlesbare Liste dessen, was ein
/// Widget vom Erscheinungsbild ueberhaupt sehen darf. Alles andere
/// (Titelleiste, Schatten, Taskleiste) bleibt beim Fenster-Manager.
pub struct KernelThema;

impl Thema for KernelThema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe {
        let t = theme::aktuell();
        let f = match rolle {
            Farbrolle::Flaeche => t.flaeche,
            // `inhalt_hintergrund` ist im Kernel-Theme ein `Farbe`
            // (3 Byte, der Framebuffer-Typ) und kein `Rgba` — deshalb
            // hier der einzige Sonderfall der Tabelle.
            Farbrolle::InhaltHintergrund => Rgba::neu(
                t.inhalt_hintergrund.r,
                t.inhalt_hintergrund.g,
                t.inhalt_hintergrund.b,
            ),
            Farbrolle::Rahmen => t.rahmen_passiv,
            Farbrolle::Akzent => t.akzent,
            Farbrolle::Auswahl => t.auswahl,
            Farbrolle::Eingabefeld => t.eingabefeld,
            Farbrolle::KnopfFlaeche => t.leiste_knopf,
            Farbrolle::KnopfAktiv => t.leiste_knopf_aktiv,
            Farbrolle::TextStark => t.text_stark,
            Farbrolle::TextNormal => t.text_normal,
            Farbrolle::TextSekundaer => t.text_sekundaer,
            Farbrolle::TextGedimmt => t.text_gedimmt,
            Farbrolle::TextAufAkzent => t.text_titel_aktiv,
        };
        Farbe::mit_alpha(f.r, f.g, f.b, f.a)
    }

    fn mass(&self, mass: Mass) -> i32 {
        let m = metrik();
        match mass {
            Mass::Abstand => m.abstand,
            Mass::UiRand => m.ui_rand,
            Mass::ElementHoehe => m.ui_element_hoehe,
            Mass::ListenEintragHoehe => m.listen_eintrag_hoehe,
            Mass::ScrollbalkenBreite => m.scrollbalken_breite,
            Mass::RadiusKlein => m.radius_klein,
            // Die Schriftgroesse ist im Kernel ein `RasterHeight`-Enum;
            // nach aussen ist sie eine PIXELHOEHE. `raster_hoehe` unten
            // rechnet zurueck.
            Mass::SchriftUi => m.schrift_ui as i32,
            Mass::ZeilenHoehe => m.zeilen_hoehe,
            // Das Blink-Tempo ist eine EINSTELLUNG, kein Zeitbegriff —
            // deshalb sitzt es beim Thema und nicht bei der Uhr.
            Mass::CursorBlinkUs => crate::einstellungen::cursor_blink_us() as i32,
        }
    }
}

/// Die Schriftgroessen, die der Kernel WIRKLICH hat.
///
/// AUFSTEIGEND SORTIERT — `Schrift::groesse_waehlen` verlaesst sich
/// darauf (bei Gleichstand gewinnt die zuerst gesehene, also die
/// kleinere). Mehr gibt `noto-sans-mono-bitmap` nicht her; die 20 kam in
/// Serie 8, Teil 3 dazu, damit `<h3>` nicht auf Fliesstextgroesse faellt.
/// Was das bedeutet und wo es nicht reicht: docs/schrift-groessen.md.
pub const SCHRIFT_GROESSEN: &[i32] = &[16, 20, 24, 32];

/// Pixelhoehe zurueck in das vorgerasterte `RasterHeight`.
///
/// ABGERUNDET AUF DIE NAECHSTKLEINERE, und das ist bewusst etwas anderes
/// als `Schrift::groesse_waehlen` (das auf die NAECHSTLIEGENDE rundet):
/// Hier kommt eine Zahl an, die schon durch `groesse_waehlen` gegangen
/// sein SOLLTE. Ist sie es nicht — weil ein Aufrufer `Mass::SchriftUi`
/// direkt durchreicht oder eine Zahl selbst ausgerechnet hat —, ist zu
/// klein der harmlosere Fehler: Zu gross sprengt das Layout, zu klein
/// sieht nur mickrig aus.
fn raster_hoehe(pixel: i32) -> RasterHeight {
    if pixel >= 32 {
        RasterHeight::Size32
    } else if pixel >= 24 {
        RasterHeight::Size24
    } else if pixel >= 20 {
        RasterHeight::Size20
    } else {
        RasterHeight::Size16
    }
}

// ---------------------------------------------------------------------------
// (2) SCHRIFT
// ---------------------------------------------------------------------------

/// Die vorgerasterte Kernel-Schrift — aber nur ihre MASSE.
///
/// Die Glyphen selbst gehen nicht ueber die Grenze (~1 MiB Bitmaps, und
/// ein Prozess bekommt sie nicht). Gezeichnet wird ueber die Leinwand.
pub struct KernelSchrift;

impl Schrift for KernelSchrift {
    fn zeichen_breite(&self, groesse: i32) -> i32 {
        get_raster_width(FontWeight::Regular, raster_hoehe(groesse)) as i32
    }
    fn zeilen_hoehe(&self, groesse: i32) -> i32 {
        // FRUEHER STAND HIER `metrik().zeilen_hoehe` UND DER PARAMETER WAR
        // UNBENUTZT — richtig, solange es genau eine Schriftgroesse gab.
        // Ein Renderer setzt eine 32er-Ueberschrift ueber 16er-Fliesstext;
        // faende er beide Zeilen gleich hoch, ueberlappten sie sich.
        //
        // Die Zeilenhoehe der UI-Groesse kommt weiter aus der Metrik (sie
        // ist dort abgestimmt, nicht ausgerechnet); jede andere Groesse
        // bekommt den ueblichen Durchschuss von 25 %.
        let m = metrik();
        // `RasterHeight` ist nicht `PartialEq` — verglichen wird deshalb
        // ueber den Diskriminanten, und der IST die Pixelhoehe
        // (`Size16 = 16`). Dieselbe Umdeutung benutzt `grafik::text_kursiv`
        // fuer den Scherungs-Winkel.
        if raster_hoehe(groesse) as usize == m.schrift_ui as usize {
            m.zeilen_hoehe
        } else {
            let g = self.groesse_waehlen(groesse);
            g + (g / 4)
        }
    }
    fn groessen(&self) -> &[i32] {
        SCHRIFT_GROESSEN
    }
    /// ECHTES FETT: `FontWeight::Bold` ist ein eigener vorgerasterter
    /// Schnitt, keine Doppelzeichnung.
    fn fett_echt(&self) -> bool {
        true
    }
    /// KEIN ECHTES KURSIV: Die Kiste liefert Light/Regular/Bold und sonst
    /// nichts. Wir SCHEREN (siehe `grafik::Zeichner::text_kursiv`) — und
    /// sagen es hier, statt es zu verschweigen.
    fn kursiv_echt(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// (3) UHR
// ---------------------------------------------------------------------------

/// Die TSC-Uhr des Kernels.
pub struct KernelUhr;

impl Uhr for KernelUhr {
    fn us(&self) -> u64 {
        crate::zeit::us_seit_boot()
    }
}

// ---------------------------------------------------------------------------
// (4) LEINWAND
// ---------------------------------------------------------------------------

/// Der Kernel-Zeichner auf einem Fenster-Puffer, als Leinwand verpackt.
///
/// DAS IST DER TEUERSTE TEIL DER TRENNUNG und zugleich der wichtigste:
/// Vorher stand `Zeichner<'_, FensterPuffer>` in JEDER Widget-Signatur.
/// Jetzt sieht ein Widget nur noch neun Operationen — und der Kernel
/// behaelt darunter seine Zeilen-Schnellpfade aus Serie 3
/// (`flaeche_zeile_fuellen`), die ein Pixel-Trait weggeworfen haette.
pub struct FensterLeinwand<'a> {
    zeichner: Zeichner<'a, FensterPuffer>,
    masse: (i32, i32),
}

impl<'a> FensterLeinwand<'a> {
    pub fn neu(puffer: &'a mut FensterPuffer) -> Self {
        let masse = (puffer.flaeche_breite() as i32, puffer.flaeche_hoehe() as i32);
        FensterLeinwand { zeichner: Zeichner::neu(puffer), masse }
    }
}

/// speedui-Farbe -> Zeichner-Farbe. Beide sind RGBA mit denselben
/// Feldern; die Umrechnung ist ein Feldtausch und kein Rechenaufwand.
fn rgba(f: Farbe) -> Rgba {
    Rgba::mit_alpha(f.r, f.g, f.b, f.a)
}

impl Leinwand for FensterLeinwand<'_> {
    fn masse(&self) -> (i32, i32) {
        self.masse
    }
    fn clip(&self) -> Option<Rechteck> {
        self.zeichner.clip()
    }
    fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.zeichner.clip_setzen(clip);
    }
    fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.zeichner.rechteck_fuellen(rechteck, rgba(farbe));
    }
    fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe) {
        self.zeichner.rechteck_abgerundet(rechteck, radius, rgba(farbe));
    }
    fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.zeichner.rechteck_rahmen(rechteck, rgba(farbe));
    }
    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe) {
        self.zeichner.linie(x0, y0, x1, y1, rgba(farbe));
    }
    fn text(&mut self, x: i32, y: i32, text: &str, groesse: i32, fett: bool, farbe: Farbe) {
        let gewicht = if fett { FontWeight::Bold } else { FontWeight::Regular };
        self.zeichner
            .text(x, y, text, raster_hoehe(groesse), gewicht, rgba(farbe));
    }
    fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32) {
        self.zeichner.icon(x, y, icon, skalierung);
    }
    /// DER KERNEL KANN SCHEREN — also ueberschreibt er die Voreinstellung
    /// des Traits (die das Kursiv wegwirft). Wie die Scherung arbeitet und
    /// warum sie kein echter Kursivschnitt ist: `grafik::Zeichner::
    /// text_kursiv`.
    fn text_stil(&mut self, x: i32, y: i32, text: &str, groesse: i32, stil: Stil, farbe: Farbe) {
        let gewicht = if stil.fett {
            FontWeight::Bold
        } else {
            FontWeight::Regular
        };
        self.zeichner.text_kursiv(
            x,
            y,
            text,
            raster_hoehe(groesse),
            gewicht,
            stil.kursiv,
            rgba(farbe),
        );
    }
}

// ---------------------------------------------------------------------------
// (5) DATEIQUELLE
// ---------------------------------------------------------------------------

/// Das VFS des Kernels als Dateiquelle fuer den Datei-Dialog.
///
/// ACHTUNG LOCK-ORDNUNG: `fs::mit_fs` ist unter dem MANAGER-Lock erlaubt
/// (so war es auch vorher, als der Dialog direkt zugriff) — aber
/// `fs::persistenter_pfad` waere es NICHT (es nimmt den VFS-Lock selbst).
/// Deshalb kommen Pfade hier immer schon fertig herein.
pub struct VfsQuelle;

impl speedui::Dateiquelle for VfsQuelle {
    fn liste(&self, ordner: &str) -> Vec<(String, bool)> {
        crate::fs::mit_fs(|dateisystem| dateisystem.liste(ordner))
            .map(|liste| {
                liste
                    .into_iter()
                    .map(|eintrag| {
                        (eintrag.name, eintrag.typ == crate::fs::NodeTyp::Verzeichnis)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn anhaengen(&self, basis: &str, name: &str) -> String {
        crate::fs::pfad_anhaengen(basis, name)
    }
    fn aufloesen(&self, basis: &str, eingabe: &str) -> String {
        crate::fs::pfad_aufloesen(basis, eingabe)
    }
}

// ---------------------------------------------------------------------------
// Der Kontext — die eine Stelle, an der der Kernel sich zusammensetzt
// ---------------------------------------------------------------------------

/// Die Wirts-Objekte als Konstanten (sie haben keinen Zustand — der
/// steckt in den globalen Atomics von `theme` und `zeit`).
pub static THEMA: KernelThema = KernelThema;
pub static SCHRIFT: KernelSchrift = KernelSchrift;
pub static UHR: KernelUhr = KernelUhr;
pub static DATEIEN: VfsQuelle = VfsQuelle;

/// Der UiKontext des Kernels — jede Zeichen- und Ereignis-Operation
/// bekommt ihn.
pub fn kontext() -> speedui::UiKontext<'static> {
    speedui::UiKontext::neu(&THEMA, &SCHRIFT, &UHR)
}

// ---------------------------------------------------------------------------
// Die Tastatur-Uebersetzung
// ---------------------------------------------------------------------------

/// `pc_keyboard::DecodedKey` -> `speedui::Taste`.
///
/// DIE EINZIGE STELLE, AN DER BEIDE WIRTE DASSELBE ZWEIMAL TUN — und es
/// ist Absicht: Der Kernel dekodiert Scancodes, ein Ring-3-Prozess bekommt
/// ueber `fenster_ereignis` schon fertige Zeichen und Sondertasten-Codes
/// und hat `pc_keyboard` nie gesehen. Ein gemeinsamer Typ haette die Kiste
/// an eine Tastatur-Kiste gebunden, die nur eine Seite braucht.
///
/// `None` = diese Taste hat in der Toolkit-ABI keine Entsprechung
/// (Umschalt, Strg, Alt) — sie wird schlicht nicht zugestellt.
pub fn taste_von(taste: DecodedKey) -> Option<Taste> {
    match taste {
        DecodedKey::Unicode(zeichen) => Some(Taste::Zeichen(zeichen)),
        DecodedKey::RawKey(code) => Some(match code {
            KeyCode::ArrowUp => Taste::Hoch,
            KeyCode::ArrowDown => Taste::Runter,
            KeyCode::ArrowLeft => Taste::Links,
            KeyCode::ArrowRight => Taste::Rechts,
            KeyCode::Home => Taste::Pos1,
            KeyCode::End => Taste::Ende,
            KeyCode::PageUp => Taste::BildHoch,
            KeyCode::PageDown => Taste::BildRunter,
            KeyCode::Delete => Taste::Entf,
            KeyCode::F1 => Taste::F(1),
            KeyCode::F2 => Taste::F(2),
            KeyCode::F3 => Taste::F(3),
            KeyCode::F4 => Taste::F(4),
            KeyCode::F5 => Taste::F(5),
            KeyCode::F6 => Taste::F(6),
            KeyCode::F7 => Taste::F(7),
            KeyCode::F8 => Taste::F(8),
            KeyCode::F9 => Taste::F(9),
            KeyCode::F10 => Taste::F(10),
            KeyCode::F11 => Taste::F(11),
            KeyCode::F12 => Taste::F(12),
            _ => return None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DIE ROLLEN-TABELLE IST VOLLSTAENDIG: Jede Rolle liefert eine
    /// Farbe, jedes Mass eine Zahl. Bricht dieser Test, hat jemand eine
    /// Rolle ergaenzt und den Kernel nicht nachgezogen — genau das soll
    /// auffallen.
    #[test_case]
    fn test_kernel_wirt_beantwortet_alles() {
        let alle_rollen = [
            Farbrolle::Flaeche,
            Farbrolle::InhaltHintergrund,
            Farbrolle::Rahmen,
            Farbrolle::Akzent,
            Farbrolle::Auswahl,
            Farbrolle::Eingabefeld,
            Farbrolle::KnopfFlaeche,
            Farbrolle::KnopfAktiv,
            Farbrolle::TextStark,
            Farbrolle::TextNormal,
            Farbrolle::TextSekundaer,
            Farbrolle::TextGedimmt,
            Farbrolle::TextAufAkzent,
        ];
        for rolle in alle_rollen {
            // Voll deckend — eine durchsichtige UI-Farbe waere ein Fehler.
            assert_eq!(THEMA.farbe(rolle).a, 255, "Rolle {:?} ist durchsichtig", rolle);
        }
        let alle_masse = [
            Mass::Abstand,
            Mass::UiRand,
            Mass::ElementHoehe,
            Mass::ListenEintragHoehe,
            Mass::ScrollbalkenBreite,
            Mass::RadiusKlein,
            Mass::SchriftUi,
            Mass::ZeilenHoehe,
            Mass::CursorBlinkUs,
        ];
        for mass in alle_masse {
            assert!(THEMA.mass(mass) > 0, "Mass {:?} ist nicht positiv", mass);
        }
        // Und die Schrift antwortet auch:
        assert!(SCHRIFT.zeichen_breite(THEMA.mass(Mass::SchriftUi)) > 0);
        assert!(SCHRIFT.text_breite("abc", THEMA.mass(Mass::SchriftUi)) > 0);
    }

    /// Die Tastatur-Uebersetzung: Zeichen kommen durch, Sondertasten
    /// werden abgebildet, Modifikatoren fallen weg.
    #[test_case]
    fn test_tasten_uebersetzung() {
        assert_eq!(taste_von(DecodedKey::Unicode('a')), Some(Taste::Zeichen('a')));
        assert_eq!(taste_von(DecodedKey::Unicode('ä')), Some(Taste::Zeichen('ä')));
        assert_eq!(taste_von(DecodedKey::RawKey(KeyCode::ArrowUp)), Some(Taste::Hoch));
        assert_eq!(taste_von(DecodedKey::RawKey(KeyCode::F5)), Some(Taste::F(5)));
        // Modifikatoren haben keine Entsprechung und werden verworfen:
        assert_eq!(taste_von(DecodedKey::RawKey(KeyCode::LShift)), None);
    }

    /// Die Schriftgroessen-Umrechnung rundet nach UNTEN — zu gross wuerde
    /// das Layout sprengen.
    ///
    /// GEAENDERT IN SERIE 8, TEIL 3, und zwar nicht am Verhalten, sondern
    /// am BESTAND: Mit `size_20` gibt es eine vierte Stufe, also rundet
    /// 23 jetzt auf 20 statt auf 16. Die REGEL („nach unten") ist
    /// dieselbe geblieben — das ist der Unterschied zwischen einem Test,
    /// der zu Recht nachgezogen wird, und einem, den man passend macht.
    #[test_case]
    fn test_raster_hoehe_rundet_ab() {
        // `RasterHeight` kennt kein `PartialEq` — die Pixelhoehe `val()`
        // ist die Zahl, auf die es ankommt.
        assert_eq!(raster_hoehe(16).val(), 16);
        assert_eq!(raster_hoehe(19).val(), 16);
        assert_eq!(raster_hoehe(20).val(), 20);
        assert_eq!(raster_hoehe(23).val(), 20);
        assert_eq!(raster_hoehe(24).val(), 24);
        assert_eq!(raster_hoehe(31).val(), 24);
        assert_eq!(raster_hoehe(32).val(), 32);
        assert_eq!(raster_hoehe(999).val(), 32);
        // Auch Unsinn liefert etwas Gueltiges statt zu panicken:
        assert_eq!(raster_hoehe(0).val(), 16);
        assert_eq!(raster_hoehe(-5).val(), 16);
    }

    /// Der WIRT meldet genau die Groessen, die als Cargo-Features
    /// eingebunden sind — und `raster_hoehe` kann jede davon liefern.
    ///
    /// DIESER TEST IST DIE KLAMMER zwischen Cargo.toml und Code: Wer eine
    /// Groesse einbindet und hier nicht eintraegt (oder umgekehrt), faellt
    /// auf. Ohne ihn koennte `SCHRIFT_GROESSEN` eine Wunschliste sein.
    #[test_case]
    fn test_gemeldete_groessen_gibt_es_wirklich() {
        let s = KernelSchrift;
        assert_eq!(s.groessen(), &[16, 20, 24, 32]);
        for &g in s.groessen() {
            assert_eq!(
                raster_hoehe(g).val() as i32,
                g,
                "Groesse {g} wird gemeldet, aber nicht gerastert"
            );
            // Und sie hat eine Zeichenbreite > 0 — die Kiste liefert sie
            // nur, wenn das Feature wirklich an ist.
            assert!(s.zeichen_breite(g) > 0, "Groesse {g} hat keine Breite");
        }
    }

    /// `groesse_waehlen` (Voreinstellung des Traits) rundet auf die
    /// NAECHSTLIEGENDE, `raster_hoehe` auf die naechstkleinere. Beide sind
    /// richtig — fuer verschiedene Fragen.
    #[test_case]
    fn test_groesse_waehlen_gegen_raster_hoehe() {
        let s = KernelSchrift;
        // 19 liegt naeher an 20: der Renderer bekommt 20 ...
        assert_eq!(s.groesse_waehlen(19), 20);
        // ... waehrend die rohe Rasterwahl abrundet.
        assert_eq!(raster_hoehe(19).val(), 16);
        // Bei Gleichstand (18) gewinnt die kleinere.
        assert_eq!(s.groesse_waehlen(18), 16);
    }

    /// Verschiedene Schriftgroessen brauchen verschiedene Zeilenhoehen —
    /// sonst ueberlappen eine 32er-Ueberschrift und 16er-Fliesstext.
    #[test_case]
    fn test_zeilenhoehe_haengt_an_der_groesse() {
        let s = KernelSchrift;
        let klein = s.zeilen_hoehe(16);
        let gross = s.zeilen_hoehe(32);
        assert!(
            gross > klein,
            "32er-Zeilen ({gross}) muessen hoeher sein als 16er ({klein})"
        );
        // Und mindestens so hoch wie die Schrift selbst.
        assert!(gross >= 32, "eine 32er-Zeile darf nicht unter 32 px sein");
    }

    /// Die ehrliche Auskunft ueber den Font-Bestand: Fett ist ECHT,
    /// Kursiv ist es NICHT.
    ///
    /// Faellt dieser Test, hat jemand einen Kursivschnitt eingebunden —
    /// dann gehoert die Scherung in `grafik::text_kursiv` weg und
    /// docs/schrift-groessen.md nachgezogen.
    #[test_case]
    fn test_fett_ist_echt_kursiv_nicht() {
        let s = KernelSchrift;
        assert!(s.fett_echt(), "FontWeight::Bold ist eingebunden");
        assert!(
            !s.kursiv_echt(),
            "noto-sans-mono-bitmap hat keinen Kursivschnitt — wer hier \
             'true' schreibt, luegt ueber den Font-Bestand"
        );
    }

    /// Die Rollen-Abbildung auf dem ECHTEN Wirt: h1..h4 unterscheidbar,
    /// h5/h6/small nicht.
    ///
    /// Dieselbe Aussage prueft `speedui::text` gegen die Attrappe
    /// `VierRaster`. Hier steht sie noch einmal gegen den WIRKLICHEN
    /// Kernel-Wirt — denn eine Attrappe beweist nur, dass die Kiste
    /// rechnet, nicht dass der Kernel dieselben Raster hat.
    #[test_case]
    fn test_rollen_abbildung_auf_dem_kernel_wirt() {
        use speedui::text::{self, Rolle};
        let s = KernelSchrift;
        assert_eq!(text::groesse_fuer(Rolle::H1, 16, &s), 32);
        assert_eq!(text::groesse_fuer(Rolle::H2, 16, &s), 24);
        assert_eq!(text::groesse_fuer(Rolle::H3, 16, &s), 20);
        assert_eq!(text::groesse_fuer(Rolle::H4, 16, &s), 16);
        assert_eq!(text::groesse_fuer(Rolle::P, 16, &s), 16);
        // DIE LUECKE: unter der Fliesstextgroesse gibt es nichts.
        assert_eq!(text::groesse_fuer(Rolle::H5, 16, &s), 16);
        assert_eq!(text::groesse_fuer(Rolle::H6, 16, &s), 16);
        assert_eq!(text::groesse_fuer(Rolle::Klein, 16, &s), 16);
        assert!(!text::exakt_moeglich(Rolle::Klein, 16, &s));
    }

    /// Die Textmetrik des ECHTEN Wirts zaehlt ZEICHEN, nicht Bytes.
    ///
    /// `KernelSchrift` erbt `text_breite` vom Trait, misst also ueber
    /// `chars().count()`. Der Test steht hier trotzdem: Wenn jemand die
    /// Methode spaeter ueberschreibt (Proportionalschrift!), soll der
    /// Umlaut-Fall sofort auffallen — hier wie in speedui::text.
    #[test_case]
    fn test_textbreite_mit_umlauten_auf_dem_kernel_wirt() {
        let s = KernelSchrift;
        let z = s.zeichen_breite(16);
        assert!(z > 0);
        assert_eq!(s.text_breite("", 16), 0);
        assert_eq!(s.text_breite("abc", 16), 3 * z);
        // 5 Zeichen, 7 Bytes.
        assert_eq!(s.text_breite("Grüße", 16), 5 * z);
        assert_eq!(s.text_breite("äöüÄÖÜß", 16), 7 * z);
        // Und die Breite waechst mit der Groesse.
        assert!(s.zeichen_breite(32) > s.zeichen_breite(16));
    }
}
