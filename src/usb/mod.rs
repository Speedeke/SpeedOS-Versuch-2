// usb — der USB-Unterbau
//
// Bis Serie 9, Teil 3 gab es Eingabegeraete nur ueber PS/2. Auf echter
// Hardware gibt es PS/2 nicht mehr — dieses Modul ist der Anfang des
// Weges dorthin. Aufgeteilt nach Controller-Sorte; heute gibt es nur
// xHCI (Begruendung in docs/xhci.md §0: Auf Rechnern der letzten zehn
// Jahre ist es der einzige Controller, der ueberhaupt vorhanden ist).

pub mod deskriptor;
pub mod geraet;
pub mod hid;
pub mod xhci;

/// Liefert USB gerade Eingaben?
///
/// ===================================================================
/// DIE FRAGE, DIE SEIT SERIE 4 OFFEN WAR
///
/// `framebuffer::meldung_zeigen("keine PS/2-Eingabe gefunden")` stand
/// bis hierher auf JEDEM Rechner ohne 8042 — auch auf einem, dessen
/// USB-Tastatur tadellos funktioniert. Ab jetzt ist die Meldung an
/// BEIDE Wege gebunden: Sie erscheint nur, wenn weder PS/2 noch USB
/// etwas liefert.
///
/// Gezaehlt wird nicht „ist ein USB-Geraet da", sondern „ist eins da,
/// das wir auch LESEN koennen" — ein Massenspeicher am selben
/// Controller macht die Maschine nicht bedienbar.
pub fn eingabe_vorhanden() -> bool {
    use deskriptor::KLASSE_HID;
    geraet::mit_geraeten(|liste| {
        liste.iter().any(|g| {
            let (k, u, p) = g.klasse_finden();
            k == KLASSE_HID && hid::art_von(k, u, p).is_some()
        })
    })
}
