// gdt.rs — Global Descriptor Table, Task State Segment und der
//          Notfall-Stack für Double Faults
//
// Warum gibt es diese Datei?
// Wenn ein Stack Overflow passiert, zeigt der Stack-Pointer auf eine
// ungültige Adresse. Die CPU will dann den Exception-Handler aufrufen —
// und dafür etwas AUF DEN STACK legen. Das schlägt fehl (der Stack ist
// ja kaputt!), es folgt ein Double Fault, dessen Handler wieder den
// kaputten Stack bräuchte → Triple Fault → die CPU resettet den Rechner.
//
// Die Lösung: Die x86_64-CPU kann beim Aufruf bestimmter Handler
// automatisch auf einen FRISCHEN, garantiert gültigen Stack umschalten.
// Diese Ersatz-Stacks stehen in der "Interrupt Stack Table" (IST),
// die Teil des "Task State Segment" (TSS) ist. Und das TSS wiederum
// wird über einen Eintrag in der "Global Descriptor Table" (GDT)
// bekannt gemacht — einer alten Segment-Tabelle aus 16-Bit-Zeiten,
// die im 64-Bit-Modus fast nur noch für genau solche Dinge da ist.

use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Welcher der 7 IST-Einträge für den Double-Fault-Handler benutzt wird.
/// Die Nummer ist frei wählbar — wichtig ist nur, dass IDT (interrupts.rs)
/// und TSS (hier) dieselbe verwenden.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

lazy_static! {
    /// Das Task State Segment mit unserem Notfall-Stack.
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            // 5 Seiten (20 KiB) reichen für den Handler locker aus.
            const STACK_SIZE: usize = 4096 * 5;
            // Der Notfall-Stack selbst: ein simples statisches Array.
            // (Später, mit Speicherverwaltung, bekommt er eine richtige
            // Guard Page — für jetzt genügt das.)
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            // Stacks wachsen auf x86 nach UNTEN, deshalb tragen wir das
            // obere Ende des Arrays als Startadresse ein.
            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            stack_start + STACK_SIZE
        };
        tss
    };
}

/// Die Selektoren merken wir uns, um sie nach dem Laden der GDT
/// in die CPU-Register zu schreiben.
struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

lazy_static! {
    /// Unsere GDT: Code- und Daten-Segment für den Kernel + das TSS.
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { code_selector, data_selector, tss_selector })
    };
}

/// Lädt GDT und TSS in die CPU. Muss beim Boot VOR der IDT
/// initialisiert werden (die IDT verweist auf den IST-Eintrag).
pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    // `unsafe`: Wir müssen der CPU gültige Selektoren geben —
    // falsche Werte würden das System sofort crashen. Unsere stammen
    // direkt aus der gerade geladenen GDT, sind also korrekt.
    unsafe {
        // Code-Segment-Register neu laden, damit es auf UNSERE GDT zeigt
        // (bis hierhin zeigte es noch auf die GDT des Bootloaders).
        CS::set_reg(GDT.1.code_selector);
        // WICHTIG (Lektion aus der bootloader-0.11-Migration): Auch
        // SS/DS/ES neu laden! Der Bootloader hinterlässt dort Selektoren
        // aus SEINER GDT — in unserer zeigen dieselben Nummern auf ganz
        // andere Einträge (0x10 wäre unser TSS!). Im 64-Bit-Modus fällt
        // das im Normalbetrieb nicht auf, aber `iretq` VALIDIERT das
        // gepoppte SS streng: veraltetes SS -> #GP -> Double Fault bei
        // der allerersten Exception-Rückkehr.
        SS::set_reg(GDT.1.data_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        // Der CPU sagen, wo unser TSS liegt.
        load_tss(GDT.1.tss_selector);
    }
}
