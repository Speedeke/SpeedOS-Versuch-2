// tests/async_task.rs — Integrationstest für den Async-Executor
//
// Prüft die Kernversprechen unseres kooperativen Multitaskings:
//   1. Gespawnte Tasks laufen und werden fertig.
//   2. Tasks, die mit yield_now abgeben, laufen VERSCHRÄNKT
//      (fair, FIFO) — nicht einer komplett nach dem anderen.
//
// Aufbau (harness = false): Zwei Zähl-Tasks schreiben ihre Schritte
// in ein gemeinsames Protokoll. Ein Prüf-Task (zuletzt gespawnt,
// gibt selbst oft genug ab) kontrolliert das Protokoll und beendet
// QEMU mit Erfolg oder schlägt per assert fehl.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{vec, vec::Vec};
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::allocator;
use speed_os::memory::{self, BootInfoFrameAllocator};
use speed_os::task::{executor::Executor, yield_now, Task};
use speed_os::{exit_qemu, serial_print, serial_println, QemuExitCode};
use spin::Mutex;
use x86_64::VirtAddr;

/// Das gemeinsame Protokoll: Wer hat wann gezählt?
static PROTOKOLL: Mutex<Vec<(&'static str, u32)>> = Mutex::new(Vec::new());

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    speed_os::init();

    // Heap aufsetzen — der Executor braucht Box, Arc und BTreeMap.
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("Heap-Initialisierung fehlgeschlagen");

    serial_print!("async_task::executor_verschraenkt_tasks...\t");

    let mut executor = Executor::new();
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
async fn pruef_task() {
    // 3x abgeben reicht: Danach sind A und B garantiert fertig
    // (jeder brauchte 3 polls). Großzügig 5x, schadet nicht.
    for _ in 0..5 {
        yield_now().await;
    }

    let protokoll = PROTOKOLL.lock();
    assert_eq!(
        *protokoll,
        vec![("A", 1), ("B", 1), ("A", 2), ("B", 2)],
        "Tasks liefen nicht fair verschraenkt"
    );

    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
