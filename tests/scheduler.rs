// tests/scheduler.rs — DER PRÄEMPTIONS-BEWEIS (Serie 6, Teil 3)
//
// Bis hierhin war Multitasking in SpeedOS immer KOOPERATIV: Ein Task lief, bis
// er von sich aus `await`-te. Dieser Test beweist, dass das nicht mehr die
// einzige Möglichkeit ist — dass der Kernel einem Programm die CPU WEGNEHMEN
// kann, ohne es zu fragen.
//
// Der Beweis besteht aus drei maschinell geprüften Aussagen über zwei
// Prozesse, die beide in einer Endlosschleife zählen und ausgeben:
//   (1) Beide kommen VORAN, und ihre Ausgaben sind VERSCHRÄNKT.
//   (2) Beide wurden nachweislich AUS RING 3 verdrängt (praemptionen > 0).
//   (3) Keiner hat je FREIWILLIG abgegeben (abgaben == 0) — ihr Maschinencode
//       enthält keinen einzigen Abgabe-Syscall (Unit-Test in prozess.rs).
//
// Aus (3) und (1) folgt zwingend: Die CPU wurde weggenommen. Ein kooperativer
// Executor hätte hier für immer im ersten Prozess festgehangen.
//
// Dazu kommen die Beweise, die den Scheduler ERWACHSEN machen:
//   * die Kontext-SICHERUNG gegen einen synthetischen Registersatz,
//   * ein wartender Prozess (Zustand::Wartend) verbraucht fast keine CPU,
//   * ein ABGESTÜRZTER Prozess reisst weder den Kernel noch die anderen
//     Prozesse mit (Dauerregel II, jetzt prozess-weise),
//   * und am Ende ist die Frame-Bilanz BYTE-EXAKT wieder null.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(speed_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use speed_os::prozess::{Pid, Zustand};
use speed_os::{allocator, memory, prozess, ring3, scheduler, serial_println, zeit};
use x86_64::VirtAddr;

entry_point!(main, config = &speed_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    // Volle CPU-Grundausstattung: GDT (mit User-Segmenten + RSP0), IDT (mit
    // dem INT-0x80-Gate UND dem nackten Timer-Einstieg), Speicher + Heap.
    speed_os::init();
    zeit::init();
    let boot_info: &'static BootInfo = boot_info;
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("kein Physik-Mapping");
    memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    allocator::init_heap().expect("Heap-Initialisierung fehlgeschlagen");

    // DER SCHRITT, um den es hier geht: Der Kontext, in dem wir gerade laufen
    // (also dieser Test), wird zum KERNEL-PROZESS PID 0. Ab jetzt kann uns der
    // Timer verdrängen.
    scheduler::init();

    test_main();
    speed_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    speed_os::test_panic_handler(info)
}

/// Wartet `ms` Millisekunden, indem der Kernel-Prozess die CPU dem Scheduler
/// überlässt (hlt weckt bei jedem Timer-Tick). GENAU DAS macht die Wartezeit
/// zur Bühne der eingeplanten Prozesse: Wir tun nichts, und sie rechnen.
fn warten_ms(ms: u64) {
    let ziel = zeit::ms_seit_boot() + ms;
    while zeit::ms_seit_boot() < ziel {
        x86_64::instructions::hlt();
    }
}

/// Legt EINEN Prozess an und räumt ihn sofort wieder ab — damit die
/// Frame-Bilanzen danach byte-exakt aufgehen.
///
/// Warum das nötig ist, ehrlich erklärt: Ein Kernel-Stack entsteht über
/// `memory::allocate_pages`, und der globale Mapper legt für einen NOCH NIE
/// benutzten virtuellen Bereich Zwischentabellen (P3/P2/P1) an. Die gibt
/// `unmap_page` nicht zurück — es fehlt uns (bewusst, siehe
/// docs/scheduler-design.md §8) das Einsammeln leerer Page-Tables. Diese
/// Tabellen sind EINMALIGE Infrastruktur für den VA-Bereich (ein P1 deckt
/// 2 MiB, also gut 100 Kernel-Stacks), kein Leck PRO PROZESS. Genau das wollen
/// die Tests unten messen — deshalb wird die Einmal-Infrastruktur vorher
/// angelegt, statt sie dem ersten Prozess anzurechnen.
fn vorwaermen() {
    let pid = scheduler::einplanen(prozess::zaehler_prozess(b'V').expect("Aufwaerm-Prozess"))
        .expect("Aufwaerm-Prozess einplanen");
    scheduler::beenden(pid);
    scheduler::aufraeumen();
}

/// Räumt auf und wartet, bis wirklich nichts mehr in der Tabelle steht.
fn alle_beenden() {
    for zeile in scheduler::momentaufnahme() {
        if zeile.ist_user {
            scheduler::beenden(zeile.pid);
        }
    }
    // Ein Tick Luft, damit ein evtl. gerade laufender Prozess weggeschaltet
    // wird, bevor wir seinen Speicher freigeben.
    warten_ms(20);
    scheduler::aufraeumen();
    assert!(
        scheduler::momentaufnahme().iter().all(|z| !z.ist_user),
        "Es sind User-Prozesse in der Tabelle uebrig geblieben"
    );
}

// ---------------------------------------------------------------------------
// TEIL 1: Die Kontext-Sicherung gegen einen synthetischen Registersatz
// ---------------------------------------------------------------------------

/// Ein Assembler-Stub lädt ALLE General-Register mit unverwechselbaren
/// Magic-Werten und löst dann einen Syscall aus. Geprüft wird beides:
///   (a) SAVE:    Steht im gesicherten TrapFrame genau dieser Registersatz?
///   (b) RESTORE: Hat der Stub nach der Rückkehr noch genau diese Werte?
///
/// Warum das ein EIGENER Test ist: Ein Fehler in der Push-/Pop-Reihenfolge
/// erscheint nicht als Absturz, sondern als „ein Register hatte plötzlich den
/// Wert eines anderen" — irgendwann, irgendwo, in einem anderen Prozess. Von
/// aussen praktisch nicht zu diagnostizieren. Also messen wir es direkt.
#[test_case]
fn test_a_kontext_sicherung_synthetisch() {
    serial_println!("[SCHED-TEST] Kontext-Sicherung gegen synthetischen Registersatz:");
    ring3::kontext_test_ausfuehren();

    let erwartet = ring3::kontext_test_erwartung();
    let gesichert = ring3::kontext_gesichert().expect("Kein Kontext gesichert");
    let nachher = ring3::kontext_nachher().expect("Kein Register-Bild danach");

    // (a) SAVE — jedes einzelne Register, mit sprechender Meldung im Fehlerfall.
    //
    // ACHTUNG, ABI: `rax` (Fehlercode) und `rdx` (Ergebnis) sind seit Serie 6
    // Teil 4 AUSGABE-Register (docs/syscalls.md §1) und werden deshalb NICHT
    // erhalten — sie werden weiter unten getrennt geprüft. Alle ÜBRIGEN
    // Register müssen den Syscall unverändert überleben.
    let felder: [(&str, u64, u64, u64); 13] = [
        ("rbx", erwartet.rbx, gesichert.rbx, nachher.rbx),
        ("rcx", erwartet.rcx, gesichert.rcx, nachher.rcx),
        ("rsi", erwartet.rsi, gesichert.rsi, nachher.rsi),
        ("rdi", erwartet.rdi, gesichert.rdi, nachher.rdi),
        ("rbp", erwartet.rbp, gesichert.rbp, nachher.rbp),
        ("r8", erwartet.r8, gesichert.r8, nachher.r8),
        ("r9", erwartet.r9, gesichert.r9, nachher.r9),
        ("r10", erwartet.r10, gesichert.r10, nachher.r10),
        ("r11", erwartet.r11, gesichert.r11, nachher.r11),
        ("r12", erwartet.r12, gesichert.r12, nachher.r12),
        ("r13", erwartet.r13, gesichert.r13, nachher.r13),
        ("r14", erwartet.r14, gesichert.r14, nachher.r14),
        ("r15", erwartet.r15, gesichert.r15, nachher.r15),
    ];
    for (name, soll, gesichert_wert, nachher_wert) in felder {
        assert_eq!(
            gesichert_wert, soll,
            "SAVE-Pfad: {} wurde falsch gesichert",
            name
        );
        assert_eq!(
            nachher_wert, soll,
            "RESTORE-Pfad: {} kam falsch zurueck",
            name
        );
    }
    // Die beiden AUSGABE-Register getrennt:
    // rax trägt HIN die Syscall-Nummer und ZURÜCK den Fehlercode/Antwortwert.
    assert_eq!(gesichert.rax, erwartet.rax, "Syscall-Nummer nicht gesichert");
    assert_eq!(
        nachher.rax,
        ring3::KONTEXT_TEST_ANTWORT,
        "Rueckgabewert kam nicht in rax zurueck"
    );
    // rdx trägt HIN das dritte Argument (hier ein Magic-Wert, der im
    // gesicherten Rahmen ankommen MUSS — sonst wären Argumente nicht
    // übertragbar) und ZURÜCK das Ergebnis (beim Kontext-Test 0).
    assert_eq!(
        gesichert.rdx, erwartet.rdx,
        "SAVE-Pfad: das Argument in rdx wurde falsch gesichert"
    );
    assert_eq!(
        nachher.rdx, 0,
        "rdx ist das ERGEBNIS-Register: der Kontext-Test liefert dort 0"
    );
    // Und der CPU-Teil des Rahmens muss plausibel sein: Der Trap kam aus
    // Ring 0 (dieser Test läuft im Kernel), mit gesetztem IF und einem
    // Stack-Pointer, der ins Kernel-Gebiet zeigt.
    assert!(
        !gesichert.aus_ring3(),
        "Der Test laeuft im Kernel — CS darf nicht Ring 3 sein"
    );
    assert_eq!(gesichert.rflags & (1 << 9), 1 << 9, "IF fehlt im Rahmen");
    assert_ne!(gesichert.rip, 0, "Rueckkehr-Adresse fehlt im Rahmen");
    serial_println!("[SCHED-TEST] Alle 15 Register + CPU-Rahmen korrekt gesichert und zurueck.");
}

// ---------------------------------------------------------------------------
// TEIL 1b: Der Kontext-Wechsel darf die SSE-Register NICHT anfassen
// ---------------------------------------------------------------------------
//
// EINE GEFAHR, DIE MIT DEM NACKTEN EINSTIEG ENTSTEHT: Der frühere Timer-
// Handler war `extern "x86-interrupt"` — dort sichert der COMPILER alles, was
// der Handler-Rumpf anfasst, inklusive SSE-Registern. Unser Assembler-Einstieg
// sichert dagegen GENAU die 15 General-Register, die im TrapFrame stehen.
// Würde irgendetwas im Timer-, Wechsel- oder Syscall-Pfad ein XMM-Register
// benutzen (der Compiler tut das gern für Speicher-Kopien), wäre der Inhalt
// des unterbrochenen Codes weg — und der Fehler zeigte sich irgendwann als
// zerstörte Pixel im Compositor, nicht als Absturz.
//
// SpeedOS hält den Kernel bewusst fliesskomma-frei (soft-float), damit gerade
// das nicht passiert. Aber "bewusst" ist keine Zusage, solange es niemand
// nachmisst. Also messen wir es: XMM0-XMM15 mit Mustern füllen, sich über viele
// Timer-Ticks hinweg verdrängen lassen (mit einem laufenden Prozess, dessen
// Syscalls ebenfalls durch den Kernel gehen) und danach nachsehen.

// rdi = Muster (16 x 16 Byte), rsi = Ergebnis (16 x 16 Byte), rdx = hlt-Runden
core::arch::global_asm!(
    ".global sse_test_stub",
    "sse_test_stub:",
    "movdqu xmm0,  [rdi + 0x00]",
    "movdqu xmm1,  [rdi + 0x10]",
    "movdqu xmm2,  [rdi + 0x20]",
    "movdqu xmm3,  [rdi + 0x30]",
    "movdqu xmm4,  [rdi + 0x40]",
    "movdqu xmm5,  [rdi + 0x50]",
    "movdqu xmm6,  [rdi + 0x60]",
    "movdqu xmm7,  [rdi + 0x70]",
    "movdqu xmm8,  [rdi + 0x80]",
    "movdqu xmm9,  [rdi + 0x90]",
    "movdqu xmm10, [rdi + 0xA0]",
    "movdqu xmm11, [rdi + 0xB0]",
    "movdqu xmm12, [rdi + 0xC0]",
    "movdqu xmm13, [rdi + 0xD0]",
    "movdqu xmm14, [rdi + 0xE0]",
    "movdqu xmm15, [rdi + 0xF0]",
    // Zwischen Laden und Speichern passiert NICHTS ausser Warten — jede
    // Änderung muss also aus einem Interrupt-Handler kommen.
    "2:",
    "hlt",
    "dec rdx",
    "jnz 2b",
    "movdqu [rsi + 0x00], xmm0",
    "movdqu [rsi + 0x10], xmm1",
    "movdqu [rsi + 0x20], xmm2",
    "movdqu [rsi + 0x30], xmm3",
    "movdqu [rsi + 0x40], xmm4",
    "movdqu [rsi + 0x50], xmm5",
    "movdqu [rsi + 0x60], xmm6",
    "movdqu [rsi + 0x70], xmm7",
    "movdqu [rsi + 0x80], xmm8",
    "movdqu [rsi + 0x90], xmm9",
    "movdqu [rsi + 0xA0], xmm10",
    "movdqu [rsi + 0xB0], xmm11",
    "movdqu [rsi + 0xC0], xmm12",
    "movdqu [rsi + 0xD0], xmm13",
    "movdqu [rsi + 0xE0], xmm14",
    "movdqu [rsi + 0xF0], xmm15",
    "ret",
);

extern "C" {
    fn sse_test_stub(muster: *const u64, ergebnis: *mut u64, runden: u64);
}

#[test_case]
fn test_a2_kontext_wechsel_laesst_sse_register_unberuehrt() {
    serial_println!("[SCHED-TEST] SSE-Register ueber Praemption und Syscalls hinweg:");
    // Ein laufender Prozess sorgt dafür, dass echte Wechsel UND echte Syscalls
    // (debug_print mit Formatierung und Allokation) dazwischenkommen.
    let pid = scheduler::einplanen(prozess::zaehler_prozess(b'S').expect("Zaehler"))
        .expect("einplanen");

    // 16 Register x 2 u64 — jedes Register bekommt ein eigenes Muster.
    let mut muster = [0u64; 32];
    for (i, wert) in muster.iter_mut().enumerate() {
        *wert = 0x0505_0000_0000_0000 | (i as u64 + 1);
    }
    let mut ergebnis = [0u64; 32];
    let wechsel_vorher = scheduler::wechsel_gesamt();
    // 60 hlt-Runden = mindestens 60 Timer-Ticks (~240 ms), also viele
    // Zeitscheiben-Wechsel hin und zurück.
    // unsafe: reiner Assembler-Aufruf, der nur XMM-Register (alle
    // caller-saved) und die beiden übergebenen Puffer anfasst.
    unsafe { sse_test_stub(muster.as_ptr(), ergebnis.as_mut_ptr(), 60) };

    let wechsel = scheduler::wechsel_gesamt() - wechsel_vorher;
    assert!(
        wechsel > 4,
        "Zu wenige Kontext-Wechsel ({}) — der Test haette nichts gemessen",
        wechsel
    );
    for i in 0..32 {
        assert_eq!(
            ergebnis[i], muster[i],
            "XMM{} (Haelfte {}) wurde vom Kernel zerstoert — der Trap-Einstieg \
             muss dann auch die SSE-Register sichern",
            i / 2,
            i % 2
        );
    }
    serial_println!(
        "[SCHED-TEST] XMM0-XMM15 nach {} Kontext-Wechseln unveraendert \
         (der Kernel bleibt fliesskomma-frei).",
        wechsel
    );
    scheduler::beenden(pid);
    scheduler::aufraeumen();
}

// ---------------------------------------------------------------------------
// TEIL 2: DER PRÄEMPTIONS-BEWEIS
// ---------------------------------------------------------------------------

#[test_case]
fn test_b_zwei_zaehler_verschraenken_sich() {
    serial_println!(
        "[SCHED-TEST] Zwei Zaehler-Prozesse, die NIE freiwillig abgeben \
         (die Ausgabe unten muss sich verschraenken):"
    );
    vorwaermen();
    let (frei_vorher, _) = memory::frame_statistik();
    scheduler::spur_loeschen();
    let wechsel_vorher = scheduler::wechsel_gesamt();

    let pid_a = scheduler::einplanen(prozess::zaehler_prozess(b'A').expect("Prozess A bauen"))
        .expect("A einplanen");
    let pid_b = scheduler::einplanen(prozess::zaehler_prozess(b'B').expect("Prozess B bauen"))
        .expect("B einplanen");

    // Wir (PID 0) tun 1,5 Sekunden lang NICHTS. Was in dieser Zeit passiert,
    // passiert ausschliesslich, weil der Timer umschaltet.
    warten_ms(1500);

    let moment = scheduler::momentaufnahme();
    let a = moment.iter().find(|z| z.pid == pid_a).expect("A verschwunden");
    let b = moment.iter().find(|z| z.pid == pid_b).expect("B verschwunden");

    serial_println!(
        "[SCHED-TEST] PID {}: {} us CPU, {} Praemptionen, {} Abgaben, {} Syscalls",
        a.pid,
        a.cpu_us,
        a.praemptionen,
        a.abgaben,
        a.syscalls
    );
    serial_println!(
        "[SCHED-TEST] PID {}: {} us CPU, {} Praemptionen, {} Abgaben, {} Syscalls",
        b.pid,
        b.cpu_us,
        b.praemptionen,
        b.abgaben,
        b.syscalls
    );

    // ---- BEWEIS 1: Beide kommen voran, und die Ausgabe ist VERSCHRÄNKT ----
    let spur: Vec<Pid> = scheduler::spur_lesen().iter().map(|(pid, _)| *pid).collect();
    let befund = scheduler::spur_auswerten(&spur);
    serial_println!(
        "[SCHED-TEST] Ausgabe-Spur: {} Ausgaben, {} Beteiligte, {} Wechsel.",
        befund.gesamt,
        befund.beteiligte,
        befund.wechsel
    );
    assert!(befund.gesamt > 4, "Es wurde kaum etwas ausgegeben");
    assert_eq!(befund.beteiligte, 2, "Es hat nicht BEIDES ausgegeben");
    assert!(
        befund.wechsel >= 2,
        "Die Ausgabe ist nicht verschraenkt (nur {} Wechsel) — das waere \
         'erst alles von A, dann alles von B' und kein Beweis",
        befund.wechsel
    );

    // ---- BEWEIS 2: Beide wurden AUS RING 3 verdrängt ----
    // Dieser Zähler wird nur erhöht, wenn beim Wegschalten im gesicherten
    // Rahmen ein Ring-3-CS stand — also einem unprivilegierten Programm
    // mitten im Rechnen die CPU genommen wurde.
    assert!(
        a.praemptionen > 0,
        "PID {} wurde nie aus Ring 3 verdraengt",
        a.pid
    );
    assert!(
        b.praemptionen > 0,
        "PID {} wurde nie aus Ring 3 verdraengt",
        b.pid
    );

    // ---- BEWEIS 3: Keiner hat freiwillig abgegeben ----
    assert_eq!(a.abgaben, 0, "PID {} hat freiwillig abgegeben", a.pid);
    assert_eq!(b.abgaben, 0, "PID {} hat freiwillig abgegeben", b.pid);
    // Und sie leben immer noch (kein Prozess hat sich beendet).
    assert!(a.zustand.ist_lauffaehig() && b.zustand.ist_lauffaehig());

    // Fairness in der Praxis: Round-Robin gibt beiden ungefähr gleich viel
    // (Faktor 3 Toleranz — die Bremse ist eine Rechenschleife, keine Uhr).
    let (kleiner, groesser) = if a.cpu_us < b.cpu_us {
        (a.cpu_us, b.cpu_us)
    } else {
        (b.cpu_us, a.cpu_us)
    };
    assert!(kleiner > 0 && groesser < kleiner * 3 + 20_000, "grob unfair verteilt");

    // Der Kernel-Prozess selbst hat auch CPU bekommen — die Oberfläche
    // verhungert also nicht (genau der Punkt der Koexistenz-Entscheidung).
    let kernel = moment
        .iter()
        .find(|z| !z.ist_user)
        .expect("Kernel-Prozess fehlt in der Tabelle");
    assert!(kernel.cpu_us > 0, "Der Kernel-Prozess bekam keine CPU");
    assert!(
        scheduler::wechsel_gesamt() - wechsel_vorher > 20,
        "Zu wenige Kontext-Wechsel fuer 1,5 s bei 20-ms-Scheiben"
    );

    serial_println!(
        "[PRAEMPTIONS-MEILENSTEIN] Zwei Ring-3-Programme, die nie abgeben, \
         laufen verschraenkt — die CPU wurde ihnen WEGGENOMMEN."
    );

    // ---- Aufräumen: Frame-Bilanz muss byte-exakt aufgehen ----
    alle_beenden();
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(
        frei_vorher, frei_nachher,
        "Prozesse haben Frames geleckt (vorher {} frei, nachher {})",
        frei_vorher, frei_nachher
    );
}

// ---------------------------------------------------------------------------
// TEIL 3: Ein WARTENDER Prozess (Zustand::Wartend) verbraucht keine CPU
// ---------------------------------------------------------------------------

#[test_case]
fn test_c_schlaefer_wartet_statt_zu_rechnen() {
    serial_println!("[SCHED-TEST] Schlaefer (SYS_SCHLAFEN) gegen Dauerrechner:");
    let (frei_vorher, _) = memory::frame_statistik();

    let pid_schlaefer = scheduler::einplanen(
        prozess::schlaefer_prozess(50).expect("Schlaefer bauen"),
    )
    .expect("Schlaefer einplanen");
    let pid_rechner = scheduler::einplanen(prozess::zaehler_prozess(b'R').expect("Rechner bauen"))
        .expect("Rechner einplanen");

    // Mehrfach hinsehen: irgendwann MUSS der Schläfer im Zustand `Wartend`
    // angetroffen werden (er schläft 50 ms zwischen zwei Syscalls).
    let mut wartend_gesehen = false;
    for _ in 0..30 {
        warten_ms(20);
        if scheduler::momentaufnahme()
            .iter()
            .any(|z| z.pid == pid_schlaefer && z.zustand == Zustand::Wartend)
        {
            wartend_gesehen = true;
        }
    }
    assert!(
        wartend_gesehen,
        "Der Schlaefer war nie im Zustand 'wartend' — SYS_SCHLAFEN wirkt nicht"
    );

    let moment = scheduler::momentaufnahme();
    let schlaefer = moment.iter().find(|z| z.pid == pid_schlaefer).unwrap();
    let rechner = moment.iter().find(|z| z.pid == pid_rechner).unwrap();
    serial_println!(
        "[SCHED-TEST] Schlaefer: {} us CPU / {} Abgaben  |  Rechner: {} us CPU / {} Abgaben",
        schlaefer.cpu_us,
        schlaefer.abgaben,
        rechner.cpu_us,
        rechner.abgaben
    );
    // Der Schläfer gibt jedes Mal FREIWILLIG ab ...
    assert!(schlaefer.abgaben > 0, "Der Schlaefer hat nie abgegeben");
    // ... und verbraucht dadurch viel weniger CPU als der Dauerrechner.
    assert!(
        schlaefer.cpu_us * 4 < rechner.cpu_us,
        "Der Schlaefer verbraucht zu viel CPU ({} us gegen {} us)",
        schlaefer.cpu_us,
        rechner.cpu_us
    );

    alle_beenden();
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(frei_vorher, frei_nachher, "Frames geleckt");
}

// ---------------------------------------------------------------------------
// TEIL 4: Dauerregel II jetzt PROZESS-WEISE
// ---------------------------------------------------------------------------

/// Ein Prozess stürzt ab (verbotener Zugriff auf Kernel-Speicher). Erwartung:
/// GENAU DIESER Prozess stirbt — der Kernel läuft weiter, und der zweite
/// Prozess rechnet unbeirrt weiter. Vor Serie 6 hätte ein solcher Fehler den
/// ganzen Kernel angehalten.
#[test_case]
fn test_d_absturz_toetet_nur_den_einen_prozess() {
    serial_println!(
        "[SCHED-TEST] Ein Prozess stuerzt ab (ein Page Fault ist HIER erwartet):"
    );
    let (frei_vorher, _) = memory::frame_statistik();

    let pid_gut = scheduler::einplanen(prozess::zaehler_prozess(b'G').expect("Zaehler bauen"))
        .expect("Zaehler einplanen");
    let pid_boese =
        scheduler::einplanen(prozess::absturz_prozess().expect("Absturz-Prozess bauen"))
            .expect("Absturz-Prozess einplanen");

    warten_ms(400);

    let moment = scheduler::momentaufnahme();
    // Der Abstürzler ist beendet (oder schon abgeräumt) ...
    let boese_lebt = moment
        .iter()
        .any(|z| z.pid == pid_boese && z.zustand != Zustand::Beendet);
    assert!(
        !boese_lebt,
        "Der abgestuerzte Prozess laeuft noch — die Fault-Recovery greift nicht"
    );
    // ... und der brave Prozess rechnet weiter, verdrängt wie zuvor.
    let gut = moment
        .iter()
        .find(|z| z.pid == pid_gut)
        .expect("Der brave Prozess ist mitgestorben");
    assert!(
        gut.zustand.ist_lauffaehig(),
        "Der brave Prozess ist nicht mehr lauffaehig"
    );
    assert!(
        gut.praemptionen > 0,
        "Der brave Prozess wurde nach dem Absturz nicht mehr eingeplant"
    );
    serial_println!(
        "[SCHED-MEILENSTEIN] PID {} ist gestorben, PID {} rechnet weiter, \
         der Kernel lebt. (Dauerregel II, jetzt pro Prozess.)",
        pid_boese,
        pid_gut
    );

    alle_beenden();
    let (frei_nachher, _) = memory::frame_statistik();
    assert_eq!(
        frei_vorher, frei_nachher,
        "Auch ein ABGESTUERZTER Prozess muss alle Frames zurueckgeben"
    );
}

// ---------------------------------------------------------------------------
// TEIL 5: Danach ist alles wie vorher
// ---------------------------------------------------------------------------

/// Nach all dem muss der Kernel-Prozess wieder allein und unbeschädigt
/// dastehen — und der ALTE Einzelschuss-Ring-3-Pfad (`ring3test`) muss trotz
/// aktivem Scheduler weiter funktionieren (er sperrt die Planung, siehe
/// docs/scheduler-design.md §6).
#[test_case]
fn test_e_einzelschuss_pfad_ueberlebt_den_scheduler() {
    let moment = scheduler::momentaufnahme();
    assert_eq!(moment.len(), 1, "Es sind Prozesse uebrig geblieben");
    assert!(!moment[0].ist_user, "Slot 0 muss der Kernel-Prozess sein");
    assert!(scheduler::aktiv(), "Die Planung wurde abgeschaltet");

    serial_println!("[SCHED-TEST] Alter Einzelschuss-Pfad bei laufendem Scheduler:");
    ring3::ring3_erfolg();
    serial_println!("[SCHED-TEST] ring3_erfolg lief sauber — die Planungs-Sperre haelt.");

    // Und der Scheduler plant danach weiter (die Sperre wurde gelöst).
    let wechsel_vorher = scheduler::wechsel_gesamt();
    let pid = scheduler::einplanen(prozess::zaehler_prozess(b'Z').expect("Zaehler"))
        .expect("einplanen");
    warten_ms(200);
    let moment = scheduler::momentaufnahme();
    let z = moment.iter().find(|z| z.pid == pid).expect("Prozess fehlt");
    assert!(
        z.praemptionen > 0 && scheduler::wechsel_gesamt() > wechsel_vorher,
        "Nach dem Einzelschuss-Pfad plant der Scheduler nicht mehr um"
    );
    alle_beenden();
    serial_println!("[SCHED-TEST] Scheduler plant nach dem Einzelschuss-Pfad normal weiter.");
}

// ---------------------------------------------------------------------------
// TEIL 6: Die Shell-Befehle, wie ein Nutzer sie tippt
// ---------------------------------------------------------------------------

/// Fährt die neuen Prozess-Befehle end-to-end durch die echte Registry — mit
/// AKTIVEM Scheduler (in den Lib-Tests laeuft er nicht, dort nehmen die
/// Befehle nur ihren „Scheduler aus"-Pfad). Der Mitschnitt dieses Tests ist
/// gleichzeitig die Beispielsitzung fuer README/Devlog.
#[test_case]
fn test_f_shell_befehle_end_to_end() {
    use speed_os::shell::befehle::{alle_befehle, ShellKontext};
    use speed_os::shell::befehl_ausfuehren;

    // Das VFS wird von den Befehlen (Prompt/Pfad) erwartet.
    speed_os::fs::init();
    let registry = alle_befehle();
    let mut ctx = ShellKontext::neu();

    let sitzung = [
        "prozesse",
        "prozess-start zaehler A",
        "prozess-start schlaefer 100",
        "prozesse",
        "prozess-stop alle",
        "praemptionstest 2",
        "prozesse",
    ];
    for zeile in sitzung {
        serial_println!("\n----- SpeedOS:/> {} -----", zeile);
        befehl_ausfuehren(&registry, &mut ctx, zeile);
    }
    serial_println!("\n----- Ende der Prozess-Sitzung -----");

    // Danach ist wieder Ruhe: alles beendet und abgeraeumt.
    alle_beenden();
    assert_eq!(
        scheduler::momentaufnahme().len(),
        1,
        "Nach der Sitzung darf nur der Kernel-Prozess uebrig sein"
    );
}
