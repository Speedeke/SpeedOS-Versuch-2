// tests/scheduler_executor.rs — KOEXISTENZ: der kooperative Executor als
//                               Kernel-Prozess (Serie 6, Teil 3)
//
// Die Architektur-Entscheidung dieser Serie lautet: Der kooperative
// async-Executor wird NICHT ersetzt, sondern ist selbst ein schedulebarer
// Kontext — der Kernel-Prozess PID 0 (docs/scheduler-design.md §1). Dieser
// Test prüft genau diese Behauptung mit dem ECHTEN Executor:
//
//   Ein Kernel-Task schläft in einer Schleife per `zeit::warte_ms(...)` —
//   also kooperativ — WÄHREND ein Ring-3-Prozess in einer Endlosschleife
//   rechnet und nie abgibt. Danach muss BEIDES zutreffen:
//     * der Kernel-Task hat alle seine Runden geschafft (er ist nicht
//       verhungert — die Oberfläche würde also weiterlaufen),
//     * der Prozess wurde vielfach aus Ring 3 verdrängt (er hat auch
//       gerechnet, nicht nur gewartet).
//
// Damit ist auch der Leerlauf-Pfad abgedeckt: `Executor::sleep_if_idle` gibt
// bei lauffähigen Prozessen die Zeitscheibe SOFORT ab (yield) statt zu `hlt`-en.
//
// Eigener Testkern OHNE Test-Framework (`harness = false`), weil
// `Executor::run()` nie zurückkehrt — das Urteil fällt und beendet QEMU
// deshalb aus einem Task heraus (dasselbe Muster wie tests/async_task.rs).

#![no_std]
#![no_main]

extern crate alloc;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::task::{executor::Executor, Task};
use speed_os::{allocator, memory, prozess, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

/// Wie viele kooperative Runden der Kernel-Task schaffen muss.
const RUNDEN: u32 = 20;
/// Pause je Runde (ms) — deutlich länger als eine Zeitscheibe (20 ms), damit
/// der Task wirklich schläft und der Prozess dazwischen rechnen kann.
const RUNDE_MS: u64 = 50;

fn main(boot_info: &'static mut BootInfo) -> ! {
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    // Der Kontext, der gleich der Executor wird, ist ab hier PID 0.
    scheduler::init();

    // Ein Ring-3-Prozess, der NIE freiwillig abgibt.
    let pid = prozess::zaehler_prozess(b'K')
        .and_then(scheduler::einplanen)
        .expect("Zaehler-Prozess einplanen");
    serial_println!(
        "[KOEXISTENZ] Prozess PID {} rechnet endlos; der Executor arbeitet \
         gleichzeitig kooperativ weiter.",
        pid
    );

    let mut executor = Executor::new();
    executor.spawn(Task::new("Koexistenz-Pruefer", pruefer(pid)));
    executor.run();
}

/// Der Kernel-Task: schläft RUNDEN-mal kooperativ und urteilt dann.
async fn pruefer(pid: prozess::Pid) {
    let start_ms = zeit::ms_seit_boot();
    let mut runden = 0u32;
    while runden < RUNDEN {
        zeit::warte_ms(RUNDE_MS).await;
        runden += 1;
    }
    let dauer_ms = zeit::ms_seit_boot() - start_ms;

    // (1) Der Kernel-Task ist nicht verhungert: Er hat alle Runden gemacht,
    //     und zwar in ungefähr der erwarteten Zeit (nicht vielfach verzögert).
    let soll_ms = RUNDEN as u64 * RUNDE_MS;
    serial_println!(
        "[KOEXISTENZ] Kernel-Task: {} Runden in {} ms (Soll ~{} ms).",
        runden,
        dauer_ms,
        soll_ms
    );
    assert_eq!(runden, RUNDEN);
    assert!(
        dauer_ms < soll_ms * 3,
        "Der Kernel-Task wurde massiv ausgebremst ({} ms statt ~{} ms) — \
         ein rechnender Prozess darf die Kernel-Welt nicht lahmlegen",
        dauer_ms,
        soll_ms
    );

    // (2) Der Prozess hat trotzdem gerechnet und wurde dabei verdrängt.
    let moment = scheduler::momentaufnahme();
    let prozess_zeile = moment
        .iter()
        .find(|zeile| zeile.pid == pid)
        .expect("Der Prozess ist verschwunden");
    serial_println!(
        "[KOEXISTENZ] Prozess PID {}: {} us CPU, {} Praemptionen, {} Abgaben.",
        prozess_zeile.pid,
        prozess_zeile.cpu_us,
        prozess_zeile.praemptionen,
        prozess_zeile.abgaben
    );
    assert!(
        prozess_zeile.praemptionen > 4,
        "Der Prozess wurde kaum verdraengt ({} mal)",
        prozess_zeile.praemptionen
    );
    assert_eq!(prozess_zeile.abgaben, 0, "Der Prozess hat freiwillig abgegeben");
    assert!(prozess_zeile.cpu_us > 0, "Der Prozess bekam keine CPU");

    // (3) Und der Kernel-Prozess selbst ist genau EIN Eintrag der Tabelle —
    //     alle Kernel-Tasks leben in ihm.
    let kernel = moment
        .iter()
        .filter(|zeile| !zeile.ist_user)
        .count();
    assert_eq!(kernel, 1, "Es muss genau EINEN Kernel-Prozess geben");

    serial_println!(
        "[KOEXISTENZ-MEILENSTEIN] Kooperativer Executor und praemptiver Prozess \
         laufen gleichzeitig — die Zwei-Ebenen-Architektur haelt."
    );
    scheduler::beenden(pid);
    speed_os::exit_qemu(speed_os::QemuExitCode::Success);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}
