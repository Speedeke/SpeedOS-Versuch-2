// tests/async_task.rs — Integrationstest für den Async-Executor
//
// Prüft die Kernversprechen unseres kooperativen Multitaskings:
//   1. Gespawnte Tasks laufen und werden fertig.
//   2. Tasks, die mit yield_now abgeben, laufen VERSCHRÄNKT
//      (fair, FIFO) — nicht einer komplett nach dem anderen.
//   3. Tasks können per task::spawn() selbst neue Tasks starten
//      (der finale Task wird AUS dem Prüf-Task heraus gespawnt —
//      würde das nicht funktionieren, hinge QEMU bis zum Timeout).
//
// Aufbau (harness = false): Zwei Zähl-Tasks schreiben ihre Schritte
// in ein gemeinsames Protokoll. Ein Prüf-Task (zuletzt gespawnt,
// gibt selbst oft genug ab) kontrolliert das Protokoll und spawnt
// dann den End-Task, der QEMU mit Erfolg beendet.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{vec, vec::Vec};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::allocator;
use speed_os::memory;
use speed_os::task::{self, executor::Executor, yield_now, Task};
use speed_os::{exit_qemu, serial_print, serial_println, QemuExitCode};
use spin::Mutex;
use x86_64::VirtAddr;

/// Das gemeinsame Protokoll: Wer hat wann gezählt?
static PROTOKOLL: Mutex<Vec<(&'static str, u32)>> = Mutex::new(Vec::new());

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    let boot_info: &'static BootInfo = boot_info;

    // Heap aufsetzen — der Executor braucht Box, Arc und BTreeMap.
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    serial_print!("async_task::executor_verschraenkt_und_spawnt...\t");

    // Bewusst mit kleiner konfigurierter Kapazität (Aufgabe 2c) —
    // auch damit muss alles funktionieren.
    let mut executor = Executor::mit_kapazitaet(8);
    executor.spawn(Task::new(zaehler("A")));
    executor.spawn(Task::new(zaehler("B")));
    executor.spawn(Task::new(pruef_task()));
    executor.run();
}

/// Zählt zwei Schritte, gibt nach jedem die CPU ab.
async fn zaehler(name: &'static str) {
    for i in 1..=2 {
        PROTOKOLL.lock().push((name, i));
        yield_now().await;
    }
}

/// Wartet (per yield), bis beide Zähler durch sind, und prüft dann:
/// Das Protokoll muss exakt A1 B1 A2 B2 sein. Wäre der Executor nicht
/// fair (FIFO), stünde dort z. B. A1 A2 B1 B2.
/// Zum Schluss der Spawn-Test: Der End-Task wird AUS diesem Task
/// heraus über task::spawn() gestartet.
async fn pruef_task() {
    // 3x abgeben reicht: Danach sind A und B garantiert fertig
    // (jeder brauchte 3 polls). Großzügig 5x, schadet nicht.
    for _ in 0..5 {
        yield_now().await;
    }

    {
        let protokoll = PROTOKOLL.lock();
        assert_eq!(
            *protokoll,
            vec![("A", 1), ("B", 1), ("A", 2), ("B", 2)],
            "Tasks liefen nicht fair verschraenkt"
        );
    }

    // Aufgabe 2a: Ein Task spawnt einen neuen Task — ohne Executor-
    // Referenz, einfach über die globale Spawn-Queue.
    if task::spawn(Task::new(end_task())).is_err() {
        panic!("task::spawn fehlgeschlagen (Spawn-Queue voll?)");
    }
    // Dieser Task endet hier — wenn end_task nie liefe, bliebe QEMU
    // hängen und der Test schlüge per Timeout fehl.
}

/// Wird aus pruef_task heraus gespawnt und beendet den Test.
async fn end_task() {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
