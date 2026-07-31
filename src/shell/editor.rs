// shell/editor.rs — Der ZeilenEditor der Shell
//
// ==========================================================================
// DER CODE IST UMGEZOGEN, DIE BENUTZUNG NICHT
//
// Bis Serie 8, Teil 2 stand die ganze Eingabelogik hier. Sie wohnt jetzt in
// `speedui::editor` — nicht, weil die Shell sie nicht mehr braucht, sondern
// weil das TEXTFELD-WIDGET sie braucht: Eine Abhaengigkeit des Toolkits auf
// die Shell waere genau verkehrt herum.
//
// Uebrig bleibt hier, was WIRKLICH zur Shell gehoert: die
// Vervollstaendigung ueber das VFS. Und die ist der Grund, warum der
// Umzug ueberhaupt eine Aenderung brauchte — der Editor loeste Pfade mit
// `fs::pfad_aufloesen` auf, also mit Kernel-Wissen. Jetzt ist das eine
// Methode des `Vervollstaendiger`-Traits, und die Shell fuellt sie mit
// ihrer echten VFS-Auflösung.

pub use speedui::editor::{EditorTaste as Taste, Reaktion, Vervollstaendiger, ZeilenEditor};
