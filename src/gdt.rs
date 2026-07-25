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

use core::cell::UnsafeCell;
use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Welcher der 7 IST-Einträge für den Double-Fault-Handler benutzt wird.
/// Die Nummer ist frei wählbar — wichtig ist nur, dass IDT (interrupts.rs)
/// und TSS (hier) dieselbe verwenden.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Grösse der beiden statischen Kernel-Stacks (Notfall-Stack für Double
/// Faults und der STANDARD-RSP0-Stack für Traps aus Ring 3).
const STACK_GROESSE: usize = 4096 * 5; // 20 KiB

/// Ein 16-Byte-ausgerichteter Stack-Block. Die Ausrichtung ist Pflicht, nicht
/// Kosmetik: Unsere Trap-Dispatcher sind normale Rust-Funktionen (C-ABI) und
/// dürfen SSE-Befehle nutzen, die einen 16-ausgerichteten Stack verlangen —
/// ein nacktes `[u8; N]` garantiert das nicht.
#[repr(align(16))]
#[allow(dead_code)] // nur als ausgerichteter Speicher genutzt, nie gelesen
struct Stack([u8; STACK_GROESSE]);

// TEURE LEKTION (Serie 6, Teil 3): Diese beiden MÜSSEN `static mut` sein!
// Ein unveränderliches `static` legt der Compiler ins Segment `.rodata`, und
// der Bootloader mappt das SCHREIBGESCHÜTZT. Ein schreibgeschützter Stack
// bedeutet: Der erste `push` der CPU beim Ring-3-Trap gibt einen Page Fault,
// dessen Handler wieder pushen will -> Double Fault -> dessen IST-Stack ist
// AUCH schreibgeschützt -> Triple Fault, Reboot ohne jede Meldung. Adressiert
// werden sie nur über `&raw const` (roher Zeiger, keine Referenz).

/// Der Notfall-Stack für den Double-Fault-Handler (IST-Eintrag 0).
static mut NOTFALL_STACK: Stack = Stack([0; STACK_GROESSE]);
/// Der STANDARD-Kernel-Stack für Traps aus Ring 3 (TSS.RSP0), solange kein
/// Prozess eingeplant ist. Sobald der Scheduler läuft, trägt RSP0 den
/// Kernel-Stack des JEWEILS laufenden Prozesses (siehe `rsp0_setzen`).
static mut RSP0_STACK: Stack = Stack([0; STACK_GROESSE]);

/// Das Task State Segment — in einer `UnsafeCell`, weil `RSP0` ab Serie 6
/// Teil 3 bei JEDEM Kontext-Wechsel neu geschrieben wird (jeder Prozess hat
/// seinen eigenen Kernel-Stack). Ein `lazy_static` wäre unveränderlich.
///
/// SICHERHEIT: SpeedOS läuft auf EINEM Kern, und geschrieben wird
/// ausschliesslich in `tss_aufsetzen()` (beim Boot) und in `rsp0_setzen()` —
/// letzteres nur aus dem Kontext-Wechsel, wo Interrupts aus sind. Es gibt
/// also nie zwei gleichzeitige Schreiber. Gelesen wird das TSS von der CPU
/// (nicht von Rust-Code), und zwar erst beim NÄCHSTEN Trap.
struct TssHalter(UnsafeCell<TaskStateSegment>);
// unsafe impl: siehe Sicherheits-Absatz oben (Einzelkern, Schreiber nur mit
// ausgeschalteten Interrupts).
unsafe impl Sync for TssHalter {}

static TSS: TssHalter = TssHalter(UnsafeCell::new(TaskStateSegment::new()));

/// Zeiger auf das TSS (die einzige Stelle, die ihn erzeugt).
fn tss_zeiger() -> *mut TaskStateSegment {
    TSS.0.get()
}

/// Füllt IST-Eintrag 0 und den Standard-RSP0. Läuft VOR dem GDT-Bau.
fn tss_aufsetzen() {
    // Stacks wachsen auf x86 nach UNTEN, deshalb tragen wir jeweils das
    // OBERE Ende des Arrays ein.
    let notfall_oben = VirtAddr::from_ptr(&raw const NOTFALL_STACK) + STACK_GROESSE;
    let rsp0_oben = VirtAddr::from_ptr(&raw const RSP0_STACK) + STACK_GROESSE;
    // unsafe: Einziger Schreiber, noch vor dem ersten Trap (siehe TssHalter).
    unsafe {
        (*tss_zeiger()).interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = notfall_oben;
        // RSP0 = der Kernel-Stack, auf den die CPU AUTOMATISCH umschaltet,
        // wenn aus Ring 3 (User-Mode) ein Trap/Interrupt/Syscall kommt. OHNE
        // ihn würde die CPU beim ersten Syscall aus Ring 3 versuchen, auf dem
        // (User-)Stack weiterzumachen bzw. hätte gar keinen gültigen Kernel-
        // Stack — Triple Fault. Getrennt vom IST-Stack (der ist nur für
        // Double Faults, dieser für den normalen Ring-3→Ring-0-Übergang).
        (*tss_zeiger()).privilege_stack_table[0] = rsp0_oben;
    }
}

/// Setzt RSP0 — den Kernel-Stack, auf dem der NÄCHSTE Trap aus Ring 3 landet.
///
/// DER vierte, leicht vergessene Schritt jedes Kontext-Wechsels: Jeder Prozess
/// hat seinen eigenen Kernel-Stack, damit sein gesicherter Trap-Rahmen liegen
/// bleiben kann, während ein anderer Prozess läuft. Zeigte RSP0 noch auf den
/// Stack des vorigen Prozesses, würde der nächste Trap dessen gesicherten
/// Kontext überschreiben — der Fehler äussert sich viel später als
/// Register-Korruption in einem völlig anderen Prozess.
pub fn rsp0_setzen(kern_stack_oben: VirtAddr) {
    // unsafe: einziger Schreiber, Interrupts sind im Kontext-Wechsel aus.
    unsafe {
        (*tss_zeiger()).privilege_stack_table[0] = kern_stack_oben;
    }
}

/// Das obere Ende des STANDARD-RSP0-Stacks (der Kernel-Prozess benutzt ihn:
/// Traps aus Ring 0 wechseln den Stack ohnehin nicht, aber ein gültiger Wert
/// muss dort stehen, falls doch einmal ein Ring-3-Trap kommt).
pub fn rsp0_standard() -> VirtAddr {
    VirtAddr::from_ptr(&raw const RSP0_STACK) + STACK_GROESSE
}

/// Die Selektoren merken wir uns, um sie nach dem Laden der GDT
/// in die CPU-Register zu schreiben (und für den Ring-3-Übergang).
struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

lazy_static! {
    /// Unsere GDT: Kernel-Code/Daten (Ring 0), User-Code/Daten (Ring 3)
    /// und das TSS. Die Reihenfolge ist für `iretq` frei wählbar (nur
    /// SYSCALL/SYSRET stellt Anforderungen — das nutzen wir bewusst nicht).
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        // Ring-3-Segmente (DPL 3): Code und Daten für den User-Mode.
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        // unsafe: `tss_segment` liest aus der Referenz nur BASIS-ADRESSE und
        // Länge für den Deskriptor und behält sie nicht — der spätere
        // Schreibzugriff über `rsp0_setzen` kollidiert damit nicht.
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(unsafe { &*tss_zeiger() }));
        (
            gdt,
            Selectors {
                code_selector,
                data_selector,
                user_code_selector,
                user_data_selector,
                tss_selector,
            },
        )
    };
}

/// Die Kernel-Code-Selektor-Nummer (für die Rückkehr aus Ring 3 in den
/// Kernel — der Page-Fault-/Exit-Pfad setzt CS/SS damit zurück).
pub fn kernel_code_selektor() -> u64 {
    GDT.1.code_selector.0 as u64
}
/// Die Kernel-Daten-Selektor-Nummer.
pub fn kernel_data_selektor() -> u64 {
    GDT.1.data_selector.0 as u64
}
/// Der User-Code-Selektor MIT RPL 3 (die unteren 2 Bits = angeforderte
/// Privilegstufe; `iretq` verlangt RPL 3 für einen Sprung nach Ring 3).
pub fn user_code_selektor() -> u64 {
    (GDT.1.user_code_selector.0 | 3) as u64
}
/// Der User-Daten-Selektor MIT RPL 3.
pub fn user_data_selektor() -> u64 {
    (GDT.1.user_data_selector.0 | 3) as u64
}

/// Lädt GDT und TSS in die CPU. Muss beim Boot VOR der IDT
/// initialisiert werden (die IDT verweist auf den IST-Eintrag).
pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    use x86_64::instructions::tables::load_tss;

    // Erst das TSS füllen (Notfall-Stack + Standard-RSP0), dann die GDT
    // laden, die darauf verweist.
    tss_aufsetzen();
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
