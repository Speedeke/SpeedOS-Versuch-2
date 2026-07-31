// ui/mod.rs — Die Anbindung des Kernels an das Widget-Toolkit
//
// ==========================================================================
// WAS HIER SEIT SERIE 8, TEIL 2 NOCH STEHT — UND WAS NICHT MEHR
//
// Das Toolkit selbst (Widget-Trait, UiEreignis, UiReaktion, Layout,
// Label/Button/Checkbox/Textfeld/ScrollListe, die Dialoge und der
// ZeilenEditor) wohnt in der Kiste `speedui/` — ohne jede
// Kernel-Abhaengigkeit, mit einem leeren `[dependencies]`-Block.
//
// Uebrig bleibt hier genau das, was WIRKLICH Kernel ist:
//
//   * `wirt`        — die fuenf Trait-Implementierungen (Thema, Schrift,
//                     Uhr, Leinwand, Dateiquelle) und die
//                     Tastatur-Uebersetzung. DIE Naht.
//   * `app`         — das App-Trait, `AppFenster`, `AppReaktion`,
//                     `SekundenTick`. Alles Kernel-Mechanik (die
//                     Deadlock-Regel, `NachLock`, die App-Registry).
//   * `texteditor`  — SpeedTexts mehrzeiliger Editor. Er ist KEIN
//                     allgemeines Toolkit-Widget (er steht auch nicht in
//                     der Umzugsliste) und braucht `Arc<Mutex<..>>` — eine
//                     Abhaengigkeit, die speedui sich nicht leisten kann.
//                     Dass er trotzdem `speedui::Widget` implementiert,
//                     ist der Beweis, dass die Grenze auch fuer
//                     App-Autoren benutzbar ist.
//
// Die Re-Exports unten halten alle alten Namen gueltig: `ui::Widget`,
// `ui::vbox`, `ui::widgets::Button` und so weiter zeigen jetzt in die
// Kiste. Deshalb musste in Explorer, Einstellungen, Task-Manager und
// SpeedText fast nichts geaendert werden — genau das war der
// Regressionstest.

pub mod app;
pub mod texteditor;
pub mod wirt;

pub use app::{App, AppFenster, AppReaktion};

// Das Toolkit — unter seinen alten Namen weiterverwendbar.
pub use speedui::{
    dialog, hbox, laengen_verteilen, vbox, w, widgets, BoxContainer, Farbrolle, Fueller, Leinwand,
    Maler, Mass, NachrichtHandler, Taste, UiEreignis, UiFenster, UiKontext, UiReaktion, Widget,
};

// Die Naht: der Kernel als Wirt.
pub use wirt::{kontext, taste_von, FensterLeinwand};

use crate::fenster::FensterPuffer;
use crate::grafik::Rechteck;

// ---------------------------------------------------------------------------
// Die drei Bequemlichkeiten, die der Fenster-Manager braucht
//
// Er arbeitet mit `FensterPuffer`n; die Kiste kennt nur `dyn Leinwand`.
// Diese drei Funktionen sind die Uebersetzung — und der einzige Grund,
// warum im Manager keine Zeile mehr stehen muss als vorher.
// ---------------------------------------------------------------------------

/// Zeichnet einen Widget-Baum vollflaechig in einen Fenster-Puffer.
pub fn ui_zeichnen(ui: &UiFenster, puffer: &mut FensterPuffer) {
    let k = kontext();
    let mut leinwand = FensterLeinwand::neu(puffer);
    ui.zeichnen(&mut leinwand, &k);
}

/// Zeichnet NUR den Schadensbereich (der Performance-Pfad aus Serie 3).
pub fn ui_zeichnen_bereich(ui: &UiFenster, puffer: &mut FensterPuffer, schaden: Rechteck) {
    let k = kontext();
    let mut leinwand = FensterLeinwand::neu(puffer);
    ui.zeichnen_bereich(&mut leinwand, schaden, &k);
}

/// Die Masse eines Fenster-Puffers als (Breite, Hoehe).
pub fn puffer_masse(puffer: &FensterPuffer) -> (i32, i32) {
    use crate::grafik::Zeichenflaeche;
    (puffer.flaeche_breite() as i32, puffer.flaeche_hoehe() as i32)
}

/// Ein Maus-Ereignis an einen Widget-Baum (mit den Massen des Puffers).
pub fn ui_maus(ui: &mut UiFenster, ereignis: UiEreignis, puffer: &FensterPuffer) -> UiReaktion {
    let k = kontext();
    ui.maus(ereignis, puffer_masse(puffer), &k)
}

/// Eine Taste an einen Widget-Baum.
pub fn ui_taste(
    ui: &mut UiFenster,
    taste: pc_keyboard::DecodedKey,
    puffer: &FensterPuffer,
) -> UiReaktion {
    let k = kontext();
    match taste_von(taste) {
        Some(taste) => ui.taste(taste, puffer_masse(puffer), &k),
        // Modifikatoren haben in der Toolkit-ABI keine Entsprechung —
        // sie werden nicht zugestellt und aendern nichts.
        None => UiReaktion::ignoriert(),
    }
}
