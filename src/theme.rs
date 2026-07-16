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
/// Copy, weil `aktuell()` eine KOPIE mit eingesetzter Akzentfarbe
/// liefert (siehe unten) — die Statics bleiben unverändert.
#[derive(Clone, Copy)]
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
/// Seit der UI-Skalierung liefert `metrik()` eine SKALIERTE Kopie
/// dieser Struktur — UI-Code greift NIE auf die Basis zu.
#[derive(Clone, Copy)]
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
    // UI-Widgets (ui-Modul)
    /// Innenrand eines UI-Fensters (Abstand Wurzel-Widget zum Rand).
    pub ui_rand: i32,
    /// Höhe von Buttons, Textfeldern und Checkbox-Zeilen.
    pub ui_element_hoehe: i32,
    /// Höhe eines Eintrags in der ScrollListe.
    pub listen_eintrag_hoehe: i32,
    /// Breite des Scrollbalkens.
    pub scrollbalken_breite: i32,
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

/// Die UNSKALIERTE Basis (Faktor 1.0) — nur metrik() liest sie.
const BASIS_METRIK: Metrik = Metrik {
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
    ui_rand: 10,
    ui_element_hoehe: 30,
    listen_eintrag_hoehe: 26,
    scrollbalken_breite: 8,
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

// ---------------------------------------------------------------------------
// UI-Skalierung: Faktor 1.0 / 1.5 / 2.0, gespeichert in HALBEN
// (2/3/4) — Fließkomma gibt es im Kernel nicht (soft-float).
// Die Fonts liegen vorgerastert in 16/24/32 vor; die Faktoren mappen
// exakt darauf (16*1.0=16, 16*1.5=24, 16*2.0=32).
// ---------------------------------------------------------------------------

use core::sync::atomic::AtomicI32;

static SKALA_HALBE: AtomicI32 = AtomicI32::new(2);

/// Der aktuelle Faktor in Halben (2 = 1.0, 3 = 1.5, 4 = 2.0).
pub fn skala_halbe() -> i32 {
    SKALA_HALBE.load(Ordering::Relaxed)
}

/// Anzeigename des Faktors ("1.0", "1.5", "2.0").
pub fn skala_name() -> &'static str {
    match skala_halbe() {
        3 => "1.5",
        4 => "2.0",
        _ => "1.0",
    }
}

/// Boot-Standard nach Bildschirmbreite: ab 2560 -> 1.5, ab 3840 -> 2.0.
pub fn skala_setzen_nach_breite(breite: usize) {
    let halbe = if breite >= 3840 {
        4
    } else if breite >= 2560 {
        3
    } else {
        2
    };
    SKALA_HALBE.store(halbe, Ordering::Relaxed);
}

/// Schaltet zyklisch 1.0 -> 1.5 -> 2.0 -> 1.0. Der Aufrufer muss
/// danach alle Fenster neu zeichnen (fenster::skalierung_wechseln).
pub fn skala_weiter() {
    let neu = match skala_halbe() {
        2 => 3,
        3 => 4,
        _ => 2,
    };
    SKALA_HALBE.store(neu, Ordering::Relaxed);
}

/// Setzt die Skalierung DIREKT (in Halben: 2/3/4) — für die
/// Einstellungen-App und den Boot (gespeicherter Wert). Ungültige
/// Werte fallen auf 1.0 zurück. Der Aufrufer muss danach alle
/// Fenster neu zeichnen (fenster::skalierung_setzen erledigt beides).
pub fn skala_setzen_halbe(halbe: i32) {
    let geprueft = match halbe {
        3 => 3,
        4 => 4,
        _ => 2,
    };
    SKALA_HALBE.store(geprueft, Ordering::Relaxed);
}

/// DIE Metrik-Quelle für allen UI-Code: die Basis, skaliert mit dem
/// aktiven Faktor. Schriftgrößen mappen auf die vorgerasterten Fonts
/// (16/24/32); schrift_gross ist bei 32 gedeckelt (größer liegt
/// nicht vorgerastert vor).
pub fn metrik() -> Metrik {
    let halbe = skala_halbe();
    let sk = |wert: i32| wert * halbe / 2;
    let basis = BASIS_METRIK;
    Metrik {
        titel_hoehe: sk(basis.titel_hoehe),
        rand: sk(basis.rand),
        knopf_breite: sk(basis.knopf_breite),
        min_fenster_breite: (sk(basis.min_fenster_breite as i32)) as usize,
        min_fenster_hoehe: (sk(basis.min_fenster_hoehe as i32)) as usize,
        snap_rand: basis.snap_rand,
        taskleiste_hoehe: sk(basis.taskleiste_hoehe),
        start_knopf_breite: sk(basis.start_knopf_breite),
        leisten_knopf_breite: sk(basis.leisten_knopf_breite),
        systray_breite: sk(basis.systray_breite),
        menue_breite: sk(basis.menue_breite),
        menue_eintrag_hoehe: sk(basis.menue_eintrag_hoehe),
        menue_suchfeld_hoehe: sk(basis.menue_suchfeld_hoehe),
        ui_rand: sk(basis.ui_rand),
        ui_element_hoehe: sk(basis.ui_element_hoehe),
        listen_eintrag_hoehe: sk(basis.listen_eintrag_hoehe),
        scrollbalken_breite: sk(basis.scrollbalken_breite),
        abstand: sk(basis.abstand),
        radius_gross: sk(basis.radius_gross),
        radius_klein: sk(basis.radius_klein),
        schrift_ui: match halbe {
            3 => RasterHeight::Size24,
            4 => RasterHeight::Size32,
            _ => RasterHeight::Size16,
        },
        schrift_gross: RasterHeight::Size32,
        zeilen_hoehe: sk(basis.zeilen_hoehe),
    }
}

// ---------------------------------------------------------------------------
// Akzentfarben: wählbar UNABHÄNGIG von Hell/Dunkel. Jeder Eintrag
// trägt zwei Werte — leuchtend fürs dunkle Theme, satter fürs helle
// (sonst wäre z. B. Gelb auf hellem Grund unlesbar). Index 0 ist das
// klassische Aurora-Violett und reproduziert exakt die alten Farben.
// ---------------------------------------------------------------------------

/// Eine wählbare Akzentfarbe (Einstellungen -> Personalisierung).
pub struct Akzent {
    pub name: &'static str,
    /// Variante fürs dunkle Theme (leuchtend).
    pub dunkel: Rgba,
    /// Variante fürs helle Theme (satter, lesbar auf hellem Grund).
    pub hell: Rgba,
}

/// Die Akzent-Palette (klein und kuratiert — kein Farbwähler).
pub static AKZENTE: [Akzent; 6] = [
    Akzent { name: "Violett", dunkel: Rgba::neu(0x7c, 0x3a, 0xed), hell: Rgba::neu(0x6d, 0x28, 0xd9) },
    Akzent { name: "Cyan", dunkel: Rgba::neu(0x22, 0xd3, 0xee), hell: Rgba::neu(0x0e, 0x74, 0x90) },
    Akzent { name: "Gruen", dunkel: Rgba::neu(0x22, 0xc5, 0x5e), hell: Rgba::neu(0x15, 0x80, 0x3d) },
    Akzent { name: "Gelb", dunkel: Rgba::neu(0xfb, 0xbf, 0x24), hell: Rgba::neu(0xb4, 0x53, 0x09) },
    Akzent { name: "Rot", dunkel: Rgba::neu(0xef, 0x44, 0x44), hell: Rgba::neu(0xb9, 0x1c, 0x1c) },
    Akzent { name: "Rosa", dunkel: Rgba::neu(0xec, 0x48, 0x99), hell: Rgba::neu(0xbe, 0x18, 0x5d) },
];

/// Index in AKZENTE (0 = Aurora-Violett, der Standard).
static AKZENT_INDEX: AtomicI32 = AtomicI32::new(0);

pub fn akzent_index() -> usize {
    let index = AKZENT_INDEX.load(Ordering::Relaxed);
    (index.max(0) as usize).min(AKZENTE.len() - 1)
}

/// Setzt die Akzentfarbe (Index in AKZENTE; ungültig -> geklemmt).
/// Der Aufrufer muss danach alles neu zeichnen lassen
/// (fenster::alles_neu_zeichnen).
pub fn akzent_setzen(index: usize) {
    AKZENT_INDEX.store(index.min(AKZENTE.len() - 1) as i32, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Desktop-Hintergrund: Verlauf-Presets. Index 0 = "Aurora (Theme)",
// der Verlauf des aktiven Themes; alle anderen sind feste Farbpaare
// (in Hell und Dunkel gleich). Nach dem Umschalten muss der
// Hintergrund-Cache neu gerendert werden (fenster: hintergrund_neu).
// ---------------------------------------------------------------------------

/// Ein wählbarer Desktop-Verlauf (oben -> unten).
pub struct HintergrundPreset {
    pub name: &'static str,
    pub oben: Farbe,
    pub unten: Farbe,
}

/// Die Preset-Liste. Eintrag 0 ist nur der NAME — seine Farben kommen
/// live aus dem aktiven Theme (hintergrund_verlauf).
pub static HINTERGRUENDE: [HintergrundPreset; 5] = [
    HintergrundPreset { name: "Aurora (Theme)", oben: Farbe::neu(0, 0, 0), unten: Farbe::neu(0, 0, 0) },
    HintergrundPreset { name: "Ozean", oben: Farbe::neu(0x0a, 0x2a, 0x43), unten: Farbe::neu(0x04, 0x10, 0x1f) },
    HintergrundPreset { name: "Sonnenuntergang", oben: Farbe::neu(0x8a, 0x2b, 0x50), unten: Farbe::neu(0x1d, 0x10, 0x2e) },
    HintergrundPreset { name: "Wald", oben: Farbe::neu(0x11, 0x3a, 0x2b), unten: Farbe::neu(0x06, 0x14, 0x10) },
    HintergrundPreset { name: "Graphit", oben: Farbe::neu(0x3a, 0x3f, 0x4b), unten: Farbe::neu(0x10, 0x12, 0x16) },
];

static HINTERGRUND_INDEX: AtomicI32 = AtomicI32::new(0);

pub fn hintergrund_index() -> usize {
    let index = HINTERGRUND_INDEX.load(Ordering::Relaxed);
    (index.max(0) as usize).min(HINTERGRUENDE.len() - 1)
}

/// Wählt ein Hintergrund-Preset. Der Aufrufer muss danach den
/// Hintergrund-Cache invalidieren (fenster::alles_neu_zeichnen setzt
/// hintergrund_neu — sonst bliebe der alte Verlauf im Cache!).
pub fn hintergrund_setzen(index: usize) {
    HINTERGRUND_INDEX.store(index.min(HINTERGRUENDE.len() - 1) as i32, Ordering::Relaxed);
}

/// Der aktuell zu rendernde Desktop-Verlauf (oben, unten) — Preset 0
/// folgt dem aktiven Theme, alle anderen sind feste Farbpaare.
pub fn hintergrund_verlauf() -> (Farbe, Farbe) {
    let index = hintergrund_index();
    if index == 0 {
        let thema = aktuell();
        (thema.desktop_oben, thema.desktop_unten)
    } else {
        let preset = &HINTERGRUENDE[index];
        (preset.oben, preset.unten)
    }
}

/// Ist gerade das helle Theme aktiv? (AtomicBool statt Mutex: Das
/// Theme wird mitten im Compositor unter gehaltenen Locks abgefragt —
/// ein Lock hier würde nur neue Deadlock-Regeln erzwingen.)
static HELL_AKTIV: AtomicBool = AtomicBool::new(false);

pub fn hell_aktiv() -> bool {
    HELL_AKTIV.load(Ordering::Relaxed)
}

/// Setzt Hell/Dunkel direkt (Boot: gespeicherter Wert; Einstellungen-
/// App). Der Aufrufer muss danach alles neu zeichnen lassen.
pub fn hell_setzen(hell: bool) {
    HELL_AKTIV.store(hell, Ordering::Relaxed);
}

/// Das gerade aktive Theme — immer über DIESE Funktion holen, nie
/// AURORA_DUNKEL direkt referenzieren! Liefert seit der Akzent-
/// Einstellung eine KOPIE des Basis-Themes, in der die gewählte
/// Akzentfarbe eingesetzt ist (akzent + rahmen_aktiv) — dadurch folgt
/// automatisch JEDE Akzent-Stelle im UI der Auswahl, ohne dass ein
/// Aufrufer etwas ändern muss. Lock-frei wie zuvor.
pub fn aktuell() -> Theme {
    let hell = HELL_AKTIV.load(Ordering::Relaxed);
    let mut thema = if hell { AURORA_HELL } else { AURORA_DUNKEL };
    let akzent = &AKZENTE[akzent_index()];
    let farbe = if hell { akzent.hell } else { akzent.dunkel };
    thema.akzent = farbe;
    thema.rahmen_aktiv = farbe;
    thema
}

/// Wechselt zwischen Dunkel und Hell und liefert das NEUE Theme.
/// Achtung: Der Aufrufer muss danach alle Fenster neu zeichnen lassen
/// (fenster::theme_wechseln() erledigt beides zusammen).
pub fn umschalten() -> Theme {
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

    /// Die Akzentwahl überlebt den Hell/Dunkel-Wechsel (unabhängig!)
    /// und liefert je Theme die passende Farbvariante; Index 0 ist
    /// exakt das alte Aurora-Violett.
    #[test_case]
    fn test_akzent_unabhaengig_von_hell_dunkel() {
        let hell_vorher = hell_aktiv();
        hell_setzen(false);
        assert_eq!(aktuell().akzent, AURORA_DUNKEL.akzent); // Standard

        akzent_setzen(2); // Grün
        assert_eq!(aktuell().akzent, AKZENTE[2].dunkel);
        assert_eq!(aktuell().rahmen_aktiv, AKZENTE[2].dunkel);
        hell_setzen(true); // Wechsel ändert die WAHL nicht ...
        assert_eq!(akzent_index(), 2);
        assert_eq!(aktuell().akzent, AKZENTE[2].hell); // ... nur die Variante

        // Ungültiger Index wird geklemmt statt zu panicken:
        akzent_setzen(999);
        assert_eq!(akzent_index(), AKZENTE.len() - 1);

        akzent_setzen(0);
        hell_setzen(hell_vorher);
    }

    /// Hintergrund-Presets: 0 folgt dem Theme, feste Presets liefern
    /// ihre eigenen Farben — in Hell und Dunkel identisch.
    #[test_case]
    fn test_hintergrund_presets() {
        let hell_vorher = hell_aktiv();
        hell_setzen(false);
        hintergrund_setzen(0);
        assert_eq!(hintergrund_verlauf(), (AURORA_DUNKEL.desktop_oben, AURORA_DUNKEL.desktop_unten));
        hell_setzen(true);
        assert_eq!(hintergrund_verlauf(), (AURORA_HELL.desktop_oben, AURORA_HELL.desktop_unten));

        hintergrund_setzen(1); // Ozean — themeunabhängig
        let ozean = hintergrund_verlauf();
        hell_setzen(false);
        assert_eq!(hintergrund_verlauf(), ozean);
        assert_eq!(ozean.0, HINTERGRUENDE[1].oben);

        hintergrund_setzen(0);
        hell_setzen(hell_vorher);
    }
}
