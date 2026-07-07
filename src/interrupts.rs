// interrupts.rs — Die Interrupt Descriptor Table (IDT) und unsere
//                 Exception-Handler
//
// Die IDT ist eine Tabelle mit bis zu 256 Einträgen, in der die CPU
// nachschlägt, welche Funktion sie bei welchem Ereignis aufrufen soll.
// Die ersten 32 Einträge sind für CPU-Exceptions reserviert — Fehler,
// die die CPU selbst meldet, z. B.:
//   - Breakpoint (int3): absichtlicher Debug-Stopp, harmlos
//   - Page Fault: Zugriff auf nicht gemappten/verbotenen Speicher
//   - Double Fault: beim Behandeln einer Exception ging etwas schief
//
// Ohne IDT führt JEDE Exception zum Triple Fault und damit zum Reboot.
// Mit dieser Datei wird der Kernel absturzsicher: Er meldet Fehler
// sauber auf VGA + seriell, statt wortlos neu zu starten.

use crate::{gdt, println};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

lazy_static! {
    /// Die IDT muss so lange leben, wie der Kernel läuft, denn die CPU
    /// greift bei jeder Exception darauf zu — deshalb `static` über
    /// lazy_static (normale `static`s erlauben die nötige Initialisierung
    /// mit Funktionsaufrufen nicht).
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        // `unsafe`: Wir versprechen, dass der IST-Index gültig ist und
        // nicht für mehrere Handler gleichzeitig verwendet wird.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                // Double Fault bekommt den Notfall-Stack aus der IST —
                // so funktioniert der Handler selbst bei Stack Overflow.
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}

/// Lädt die IDT in die CPU (lidt-Befehl). Ab diesem Moment werden
/// Exceptions von unseren Handlern behandelt statt vom Nichts.
pub fn init_idt() {
    IDT.load();
}

/// Handler für die Breakpoint-Exception (Befehl `int3`).
///
/// Das ist die harmloseste Exception: Debugger benutzen sie, um
/// Programme anzuhalten. Wir geben nur eine Meldung aus und kehren
/// zurück — das Programm läuft danach normal weiter.
/// `extern "x86-interrupt"`: spezielle Aufrufkonvention, bei der der
/// Compiler ALLE Register sichert, weil eine Exception mitten in
/// beliebigem Code zuschlagen kann.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT (int3) bei {:?}", stack_frame.instruction_pointer);
}

/// Handler für Page Faults: Zugriff auf Speicher, der nicht gemappt
/// oder geschützt ist. Gibt alle Infos lesbar aus und hält dann an —
/// weiterlaufen wäre gefährlich, ohne den Fehler zu beheben.
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    // Die CPU legt die Adresse, deren Zugriff fehlschlug,
    // automatisch ins Register CR2.
    println!("EXCEPTION: PAGE FAULT");
    println!("  Zugriff auf Adresse: {:?}", Cr2::read());
    // Den Fehlercode in verständliche Aussagen übersetzen:
    let ursache = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        "Schutzverletzung (Seite vorhanden, Zugriff verboten)"
    } else {
        "Seite nicht vorhanden (nicht gemappt)"
    };
    let zugriff = if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        "Schreibzugriff"
    } else {
        "Lesezugriff"
    };
    println!("  Ursache:  {}", ursache);
    println!("  Zugriff:  {}", zugriff);
    if error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
        println!("  Ausgeloest beim Laden eines Befehls (Instruction Fetch)");
    }
    println!("  Roher Fehlercode: {:?}", error_code);
    println!("{:#?}", stack_frame);
    crate::hlt_loop();
}

/// Handler für Double Faults: Beim Behandeln einer Exception ist eine
/// weitere passiert (klassisch: Stack Overflow). Läuft dank IST auf
/// unserem Notfall-Stack (siehe gdt.rs) und kann deshalb selbst dann
/// noch eine saubere Fehlermeldung ausgeben.
/// Rückgabetyp `!`: Von einem Double Fault gibt es auf x86_64 kein
/// Zurück — die Architektur erlaubt keine Rückkehr.
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64, // ist bei Double Faults immer 0
) -> ! {
    // panic! gibt die Meldung über unseren Panic-Handler
    // auf VGA UND seriell aus und hält dann an.
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::println;

    /// Löst absichtlich eine Breakpoint-Exception aus. Der Test gilt als
    /// bestanden, wenn die Ausführung danach normal weiterläuft — das
    /// beweist, dass unser Handler aufgerufen wird und sauber zurückkehrt.
    #[test_case]
    fn test_breakpoint_exception_laeuft_weiter() {
        x86_64::instructions::interrupts::int3();
        println!("Nach int3 geht es hier normal weiter.");
    }
}
