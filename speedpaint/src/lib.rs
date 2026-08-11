// speedpaint — aus Anzeige-Befehlen wird ein Bild
//
// ===========================================================================
// DER WEG DURCH DIESE KISTE
//
//   Anzeigeliste + Sicht --[maler]--> Zeichen-Aufrufe auf einer Leinwand
//   Ereignis             --[sicht]--> neuer Versatz + welcher Streifen
//   Anlass        --[invalidierung]-> neu layouten? neu malen? gar nichts?
//
// Die drei Module beantworten drei verschiedene Fragen, und alle drei
// sind REINE FUNKTIONEN auf Daten — deshalb laufen ihre Tests auf dem
// Host in Millisekunden, ohne Fenster und ohne QEMU.
//
// ===========================================================================
// DIE EINE ZUSAGE DIESES TEILS: SCROLLEN LAYOUTET NICHT NEU
//
// Ein Layout ueber einen Wikipedia-Artikel kostet zweistellige
// Millisekunden. Waere es Teil eines Scroll-Frames, waere fluessiges
// Scrollen unmoeglich — bei JEDER Rastung des Mausrads das ganze
// Dokument neu zu setzen ist der Fehler, den man einmal macht.
//
// Er ist hier STRUKTURELL ausgeschlossen: `malen` bekommt die
// `Anzeigeliste` als `&`-Referenz und kann sie gar nicht aendern; der
// Versatz steckt in der `Sicht` und wird beim Malen nur ADDIERT. Es gibt
// keinen Weg von einem Scroll-Ereignis zu `speedlayout::setzen` —
// `invalidierung::entscheiden` ist die einzige Stelle, die ein
// Neu-Layout ueberhaupt verlangen kann, und ihre Regeln sind einzeln
// getestet.
//
// ===========================================================================
// WAS HIER NICHT PASSIERT
//
// Kein Framebuffer, kein Syscall, kein Bild-Dekoder, kein Netz. Ein
// `<img>` wird zu einem Aufruf an die `Bildquelle`; wer die Bytes holt
// und dekodiert, ist der Browser (`libspeed::netz`, `libspeed::bild`).
// Ein Bilddekoder ist ein Parser fuer fremde Daten und gehoert nicht in
// die Kiste, die malt.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod invalidierung;
pub mod maler;
pub mod sicht;

pub use invalidierung::{Anlass, Massnahme};
pub use maler::{malen, Bildquelle, MalBefund, OhneBilder};
pub use sicht::{Balken, Scrollschritt, Sicht};

// ---------------------------------------------------------------------------
// DIE BRUECKE ZWISCHEN DEN TYPEN
// ---------------------------------------------------------------------------
//
// `speedcss::Farbe` und `speedui::Farbe` sind feldgleich, und
// `speedlayout::Rechteck` und `speedui::Rechteck` auch. Trotzdem werden
// sie hier UMGERECHNET statt vereinheitlicht: Die beiden Kisten sollen
// sich nicht kennen muessen (speedcss hat keinen Grund, ein
// UI-Toolkit zu importieren, und speedui keinen, CSS zu kennen).
//
// Die Umrechnung ist ein Feldkopieren, das der Compiler wegoptimiert —
// derselbe Handel wie bei `Taste` in Serie 8, Teil 2: ein bisschen
// Laerm an der Grenze gegen zwei Kisten, die unabhaengig bleiben.

/// Eine CSS-Farbe als UI-Farbe.
#[inline]
pub fn farbe_nach_ui(farbe: speedcss::Farbe) -> speedui::Farbe {
    speedui::Farbe::mit_alpha(farbe.r, farbe.g, farbe.b, farbe.a)
}

/// Ein Layout-Rechteck als UI-Rechteck.
#[inline]
pub fn rechteck_nach_ui(rechteck: speedlayout::Rechteck) -> speedui::Rechteck {
    speedui::Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe)
}

/// Ein UI-Rechteck als Layout-Rechteck.
#[inline]
pub fn rechteck_nach_layout(rechteck: speedui::Rechteck) -> speedlayout::Rechteck {
    speedlayout::Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, rechteck.hoehe)
}

#[cfg(test)]
mod tests;
