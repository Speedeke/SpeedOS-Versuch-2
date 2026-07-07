// tests/stack_overflow.rs — Integrationstest: Überlebt der Kernel
//                           einen Stack Overflow?
//
// Dieser Test arbeitet im "should_panic-Stil", nur eine Stufe härter:
// Er provoziert absichtlich den schlimmsten anzunehmenden Fehler — einen
// Stack Overflow, der ohne unsere Vorkehrungen zum Triple Fault und
// Reboot führen würde. Der Test gilt als BESTANDEN, wenn der
// Double-Fault-Handler aufgerufen wird (Beweis: er beendet QEMU mit
// Erfolgs-Code). Läuft die Ausführung stattdessen normal weiter oder
// rebootet QEMU, schlägt der Test fehl.
//
// Deshalb hat dieser Test in Cargo.toml `harness = false`: Er braucht
// kein Test-Framework, denn er besteht nur aus diesem einen Ablauf mit
// seiner eigenen, speziellen IDT.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;
use lazy_static::lazy_static;
use speed_os::{exit_qemu, serial_print, serial_println, QemuExitCode};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    serial_print!("stack_overflow::stack_overflow...\t");

    // GDT + TSS laden (stellt den Notfall-Stack bereit) ...
    speed_os::gdt::init();
    // ... aber unsere TEST-IDT statt der normalen: Ihr Double-Fault-
    // Handler meldet ERFOLG statt zu panicken.
    init_test_idt();

    // Und jetzt: Stack absichtlich zum Überlaufen bringen.
    stack_overflow();

    // Hierher dürfen wir NIE kommen.
    panic!("Ausfuehrung lief nach dem Stack Overflow einfach weiter");
}

/// Endlose Rekursion: Jeder Aufruf legt eine Rücksprungadresse auf den
/// Stack, bis er überläuft und die Guard-Grenze durchbricht.
#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    // Der Volatile-Zugriff verhindert, dass der Compiler die Rekursion
    // in eine Schleife umwandelt (Tail-Call-Optimierung) — wir wollen
    // ja echte Stack-Frames verbrauchen!
    volatile::Volatile::new(0).read();
}

lazy_static! {
    /// Eigene IDT nur für diesen Test: Der Double-Fault-Handler ist
    /// hier der ERFOLGSFALL. Wichtig: derselbe IST-Index wie im echten
    /// Kernel, damit der Handler den Notfall-Stack benutzt.
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
                .set_handler_fn(test_double_fault_handler)
                .set_stack_index(speed_os::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}

fn init_test_idt() {
    TEST_IDT.load();
}

/// Wenn wir hier landen, hat alles funktioniert: Der Stack Overflow
/// wurde erkannt, die CPU hat auf den Notfall-Stack umgeschaltet und
/// unseren Handler aufgerufen — Test bestanden!
extern "x86-interrupt" fn test_double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    speed_os::hlt_loop();
}

/// Ein Panic (z. B. "lief einfach weiter") bedeutet: Test fehlgeschlagen.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
