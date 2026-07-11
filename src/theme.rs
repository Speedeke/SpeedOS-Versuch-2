// theme.rs — Das zentrale Theme-System von SpeedOS
//
// AB JETZT GILT: Kein UI-Code (Fenster-Deko, Taskleiste, Startmenü,
// Fenster-Inhalte) benutzt hartcodierte Farben mehr — ALLE Farben
// kommen aus dem aktiven Theme, alle Abstände und Schriftgrößen aus
// der METRIK. Nur so kann ein Theme-Wechsel zur Laufzeit wirklich
// JEDES Element umfärben.
//
// Aufbau:
//   * `Theme`  = alle FARBEN. Davon gibt es zwei Instanzen:
//     AURORA_DUNKEL (Standard) und AURORA_HELL. Welche gerade aktiv
//     ist, steht in einem AtomicBool — `aktuell()` liefert immer eine
//     &'static Referenz, ganz ohne Lock (wichtig: wird mitten im
//     Compositor unter gehaltenen Locks abgefragt).
//   * `Metrik` = alle ABSTÄNDE und SCHRIFTGRÖSSEN. Sie sind bewusst
//     in beiden Themes identisch (ein Theme-Wechsel soll umfärben,
//     nicht das Layout verschieben) — deshalb eine eigene Konstante
//     METRIK statt Felder im Theme.
//
// Nach einem Wechsel müssen alle Fenster neu gezeichnet werden —
// das erledigt fenster::theme_wechseln() (Inhalte + alles_dirty).

use crate::framebuffer::Farbe;
use crate::grafik::Rgba;
use core::sync::atomic::{AtomicBool, Ordering};
use noto_sans_mono_bitmap::RasterHeight;

/// Alle Farben der Desktop-Oberfläche. Farbverläufe brauchen `Farbe`
/// (RGB), alles andere ist `Rgba` (kann halbtransparent sein).
pub struct Theme {
    /// Anzeigename (fürs Startmenü / Debug).
    pub name: &'static str,

    // ----- Desktop-Hintergrund (vertikaler Verlauf) -----
    pub desktop_oben: Farbe,
    pub desktop_unten: Farbe,

    // ----- Fenster-Dekoration -----
    /// Titelleiste des fokussierten Fensters (Aurora-Verlauf).
    pub titel_aktiv_oben: Farbe,
    pub titel_aktiv_unten: Farbe,
    /// Titelleiste unfokussierter Fenster (flach, gedimmt).
    pub titel_passiv: Rgba,
    pub text_titel_aktiv: Rgba,
    pub text_titel_passiv: Rgba,
    pub rahmen_aktiv: Rgba,
    pub rahmen_passiv: Rgba,
    /// Fensterschatten (halbtransparent).
    pub schatten: Rgba,
    pub knopf_symbol_aktiv: Rgba,
    pub knopf_symbol_passiv: Rgba,
    /// Der Schließen-Knopf ist IMMER rot — in jedem Theme.
    pub knopf_schliessen: Rgba,

    // ----- Flächen (Fenster-Inhalte, Panels, Menüs, Eingaben) -----
    /// Standard-Hintergrund eines Fenster-Inhalts.
    pub inhalt_hintergrund: Farbe,
    /// Panels und Overlays (Startmenü, Alt+Tab-Box).
    pub flaeche: Rgba,
    /// Eingabefelder (Suchfeld, Text-Boxen).
    pub eingabefeld: Rgba,
    /// Ausgewählter/hervorgehobener Eintrag in Listen.
    pub auswahl: Rgba,

    // ----- Text auf Flächen (vier Helligkeitsstufen) -----
    pub text_stark: Rgba,
    pub text_normal: Rgba,
    pub text_sekundaer: Rgba,
    pub text_gedimmt: Rgba,

    // ----- Akzentfarben -----
    /// DIE Akzentfarbe (Fokus-Rahmen, Snap-Vorschau, Auswahl-Balken).
    pub akzent: Rgba,
    pub akzent_cyan: Rgba,
    pub akzent_gruen: Rgba,
    pub akzent_gelb: Rgba,

    // ----- Taskleiste -----
    pub leiste_hintergrund: Rgba,
    /// Trennlinie am oberen Rand der Leiste.
    pub leiste_linie: Rgba,
    /// Fenster-Knopf in der Leiste (normal / fokussiertes Fenster).
    pub leiste_knopf: Rgba,
    pub leiste_knopf_aktiv: Rgba,

    // ----- Terminal -----
    /// Bewusst in BEIDEN Themes dunkel (wie bei den "Großen"): Die
    /// Shell-Farben (konsole::Color) sind auf dunklen Grund abgestimmt.
    /// Identisch mit Color::Black, damit Zellen-Hintergründe nahtlos
    /// in den Fenster-Hintergrund übergehen.
    pub terminal_hintergrund: Farbe,
}

/// Alle Abstände und Schriftgrößen — in beiden Themes gleich
/// (ein Theme-Wechsel färbt um, verschiebt aber kein Layout).
pub struct Metrik {
    // Fenster
    pub titel_hoehe: i32,
    /// Dicke der Resize-Randzone.
    pub rand: i32,
    /// Breite eines Titelleisten-Knopfes.
    pub knopf_breite: i32,
    pub min_fenster_breite: usize,
    pub min_fenster_hoehe: usize,
    /// Wie nah an den Bildschirmrand fürs Snap-Layout.
    pub snap_rand: i32,
    // Taskleiste
    pub taskleiste_hoehe: i32,
    pub start_knopf_breite: i32,
    pub leisten_knopf_breite: i32,
    /// Rechts reservierter Systray-Bereich (Uhr + Icons).
    pub systray_breite: i32,
    // Startmenü
    pub menue_breite: i32,
    pub menue_eintrag_hoehe: i32,
    pub menue_suchfeld_hoehe: i32,
    // Allgemein
    /// Standard-Innenabstand von Panels und Knöpfen.
    pub abstand: i32,
    /// Eckenradius großer Panels (Menü, Alt+Tab-Box).
    pub radius_gross: i32,
    /// Eckenradius kleiner Elemente (Knöpfe, Einträge).
    pub radius_klein: i32,
    // Schrift
    pub schrift_ui: RasterHeight,
    pub schrift_gross: RasterHeight,
    /// Höhe einer Textzeile in Pixeln (passend zu schrift_ui).
    pub zeilen_hoehe: i32,
}

pub const METRIK: Metrik = Metrik {
    titel_hoehe: 30,
    rand: 6,
    knopf_breite: 30,
    min_fenster_breite: 220,
    min_fenster_hoehe: 100,
    snap_rand: 8,
    taskleiste_hoehe: 40,
    start_knopf_breite: 48,
    leisten_knopf_breite: 180,
    systray_breite: 170,
    menue_breite: 340,
    menue_eintrag_hoehe: 40,
    menue_suchfeld_hoehe: 46,
    abstand: 8,
    radius_gross: 14,
    radius_klein: 6,
    schrift_ui: RasterHeight::Size16,
    schrift_gross: RasterHeight::Size32,
    zeilen_hoehe: 16,
};

/// "Aurora Dunkel" — der Obsidian-Aurora-Look, das Standard-Theme.
pub static AURORA_DUNKEL: Theme = Theme {
    name: "Aurora Dunkel",

    desktop_oben: Farbe::neu(0x17, 0x12, 0x33),
    desktop_unten: Farbe::neu(0x0b, 0x0e, 0x14),

    titel_aktiv_oben: Farbe::neu(0x5b, 0x2e, 0xc7),
    titel_aktiv_unten: Farbe::neu(0x2a, 0x4a, 0x9e),
    titel_passiv: Rgba::neu(0x23, 0x28, 0x34),
    text_titel_aktiv: Rgba::neu(0xf8, 0xfa, 0xfc),
    text_titel_passiv: Rgba::neu(0x8a, 0x91, 0xa3),
    rahmen_aktiv: Rgba::neu(0x7c, 0x3a, 0xed),
    rahmen_passiv: Rgba::neu(0x33, 0x3a, 0x4a),
    schatten: Rgba::mit_alpha(0, 0, 0, 90),
    knopf_symbol_aktiv: Rgba::neu(0xc4, 0xca, 0xd6),
    knopf_symbol_passiv: Rgba::neu(0x6a, 0x72, 0x84),
    knopf_schliessen: Rgba::neu(0xef, 0x44, 0x44),

    inhalt_hintergrund: Farbe::neu(0x12, 0x16, 0x20),
    flaeche: Rgba::neu(0x1a, 0x1f, 0x2e),
    eingabefeld: Rgba::neu(0x1c, 0x22, 0x30),
    auswahl: Rgba::neu(0x3b, 0x2e, 0x7a),

    text_stark: Rgba::neu(0xf8, 0xfa, 0xfc),
    text_normal: Rgba::neu(0xc4, 0xca, 0xd6),
    text_sekundaer: Rgba::neu(0x8a, 0x91, 0xa3),
    text_gedimmt: Rgba::neu(0x56, 0x5f, 0x73),

    akzent: Rgba::neu(0x7c, 0x3a, 0xed),
    akzent_cyan: Rgba::neu(0x22, 0xd3, 0xee),
    akzent_gruen: Rgba::neu(0x22, 0xc5, 0x5e),
    akzent_gelb: Rgba::neu(0xfb, 0xbf, 0x24),

    leiste_hintergrund: Rgba::mit_alpha(0x11, 0x14, 0x1e, 235),
    leiste_linie: Rgba::neu(0x2a, 0x30, 0x40),
    leiste_knopf: Rgba::neu(0x23, 0x28, 0x34),
    leiste_knopf_aktiv: Rgba::neu(0x3b, 0x2e, 0x7a),

    terminal_hintergrund: Farbe::neu(0x0b, 0x0e, 0x14),
};

/// "Aurora Hell" — dieselben Aurora-Akzente auf hellem Grund.
pub static AURORA_HELL: Theme = Theme {
    name: "Aurora Hell",

    desktop_oben: Farbe::neu(0xd7, 0xd3, 0xee),
    desktop_unten: Farbe::neu(0xee, 0xf1, 0xf6),

    titel_aktiv_oben: Farbe::neu(0x6d, 0x4f, 0xd6),
    titel_aktiv_unten: Farbe::neu(0x3a, 0x6b, 0xd0),
    titel_passiv: Rgba::neu(0xdd, 0xe1, 0xea),
    text_titel_aktiv: Rgba::neu(0xf8, 0xfa, 0xfc),
    text_titel_passiv: Rgba::neu(0x5a, 0x64, 0x78),
    rahmen_aktiv: Rgba::neu(0x6d, 0x28, 0xd9),
    rahmen_passiv: Rgba::neu(0xb8, 0xc0, 0xd0),
    schatten: Rgba::mit_alpha(0x1e, 0x22, 0x3c, 60),
    knopf_symbol_aktiv: Rgba::neu(0xee, 0xf1, 0xf8),
    knopf_symbol_passiv: Rgba::neu(0x6a, 0x72, 0x84),
    knopf_schliessen: Rgba::neu(0xef, 0x44, 0x44),

    inhalt_hintergrund: Farbe::neu(0xf5, 0xf7, 0xfa),
    flaeche: Rgba::neu(0xf8, 0xf9, 0xfc),
    eingabefeld: Rgba::neu(0xe7, 0xeb, 0xf3),
    auswahl: Rgba::neu(0xd7, 0xcd, 0xf5),

    text_stark: Rgba::neu(0x14, 0x1a, 0x28),
    text_normal: Rgba::neu(0x2e, 0x38, 0x50),
    text_sekundaer: Rgba::neu(0x55, 0x60, 0x7a),
    text_gedimmt: Rgba::neu(0x9a, 0xa4, 0xb8),

    akzent: Rgba::neu(0x6d, 0x28, 0xd9),
    akzent_cyan: Rgba::neu(0x0e, 0x74, 0x90),
    akzent_gruen: Rgba::neu(0x15, 0x80, 0x3d),
    akzent_gelb: Rgba::neu(0xb4, 0x53, 0x09),

    leiste_hintergrund: Rgba::mit_alpha(0xf2, 0xf4, 0xfa, 240),
    leiste_linie: Rgba::neu(0xc8, 0xcf, 0xdd),
    leiste_knopf: Rgba::neu(0xe3, 0xe7, 0xf0),
    leiste_knopf_aktiv: Rgba::neu(0xd7, 0xcd, 0xf5),

    terminal_hintergrund: Farbe::neu(0x0b, 0x0e, 0x14),
};

/// Ist gerade das helle Theme aktiv? (AtomicBool statt Mutex: Das
/// Theme wird mitten im Compositor unter gehaltenen Locks abgefragt —
/// ein Lock hier würde nur neue Deadlock-Regeln erzwingen.)
static HELL_AKTIV: AtomicBool = AtomicBool::new(false);

/// Das gerade aktive Theme — immer über DIESE Funktion holen, nie
/// AURORA_DUNKEL direkt referenzieren!
pub fn aktuell() -> &'static Theme {
    if HELL_AKTIV.load(Ordering::Relaxed) {
        &AURORA_HELL
    } else {
        &AURORA_DUNKEL
    }
}

/// Wechselt zwischen Dunkel und Hell und liefert das NEUE Theme.
/// Achtung: Der Aufrufer muss danach alle Fenster neu zeichnen lassen
/// (fenster::theme_wechseln() erledigt beides zusammen).
pub fn umschalten() -> &'static Theme {
    HELL_AKTIV.fetch_xor(true, Ordering::Relaxed);
    aktuell()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Umschalten wechselt wirklich hin UND zurück.
    #[test_case]
    fn test_theme_umschalten() {
        let start = aktuell().name;
        let neu = umschalten().name;
        assert_ne!(start, neu);
        let zurueck = umschalten().name;
        assert_eq!(start, zurueck);
    }

    /// Beide Themes teilen sich den dunklen Terminal-Hintergrund —
    /// die Shell-Farben sind auf dunklen Grund abgestimmt.
    #[test_case]
    fn test_terminal_bleibt_dunkel() {
        assert_eq!(
            AURORA_DUNKEL.terminal_hintergrund,
            AURORA_HELL.terminal_hintergrund
        );
    }
}
