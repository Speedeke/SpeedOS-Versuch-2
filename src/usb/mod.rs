// usb — der USB-Unterbau
//
// Bis Serie 9, Teil 3 gab es Eingabegeraete nur ueber PS/2. Auf echter
// Hardware gibt es PS/2 nicht mehr — dieses Modul ist der Anfang des
// Weges dorthin. Aufgeteilt nach Controller-Sorte; heute gibt es nur
// xHCI (Begruendung in docs/xhci.md §0: Auf Rechnern der letzten zehn
// Jahre ist es der einzige Controller, der ueberhaupt vorhanden ist).

pub mod xhci;
