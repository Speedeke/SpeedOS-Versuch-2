// ring3.rs — Der erste Schritt in echten User-Space (Serie 6, Teil 1)
//
// DER historische Sprung: bis jetzt lief JEDER Befehl von SpeedOS im
// Kernel-Privileg (Ring 0) — voller Zugriff auf alles. Ab hier beweisen wir,
// dass wir CPU-Code in RING 3 (User-Mode) ausführen können, der NICHT alles
// darf, und sauber in den Kernel zurückkehrt — auch wenn er abstürzt.
//
// Bewusst KLEIN: noch KEIN ELF-Loader, KEIN eigener Adressraum pro Prozess,
// KEIN präemptiver Scheduler (das ist der Fahrplan aus
// docs/serie6-bestandsaufnahme.md). Nur der reine PRIVILEGIENWECHSEL:
//   1. Ein Stück Maschinencode in eine als USER_ACCESSIBLE gemappte Page.
//   2. Per `iretq` nach Ring 3 springen (WARUM iretq statt sysret: siehe
//      `nach_ring3`).
//   3. Der Rückweg über einen Syscall (INT 0x80) — der Handler sichert den
//      vollen User-Kontext und stellt ihn wieder her.
//   4. Der Absturz-Beweis: verbotener Zugriff aus Ring 3 -> Page Fault ->
//      der KERNEL LEBT WEITER.
//
// ==========================================================================
// ZWEI DAUERREGELN, die ab dieser Datei GELTEN (auch in CLAUDE.md):
//
//  (I)  KERNEL FOLGT NIEMALS BLIND EINEM USER-ZEIGER. Jeder Zeiger, den
//       Ring-3-Code übergibt, wird VOR der Benutzung geprüft und die Daten
//       werden KOPIERT (copy-in) — nie direkt dereferenziert. `copy_in` unten
//       ist der einzige Weg; die Prüfung deckt Kernel-Adressen, ungemappte
//       Adressen und Längen-Überläufe ab und PANICKT NIE.
//
//  (II) EIN FEHLER IM USER-MODE DARF DEN KERNEL NIE MITREISSEN. Ein Page
//       Fault (oder ein anderer Trap) aus Ring 3 beendet den User-Code und
//       kehrt in den Kernel zurück — der Kernel läuft weiter. Nur ein Fehler
//       im KERNEL selbst (Ring 0) hält an (das ist ein echter Bug).
// ==========================================================================

use crate::adressraum::{self, AdressRaum};
use crate::prozess::TrapFrame;
use crate::{gdt, scheduler, serial_print, serial_println};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

// SPEICHER-LAYOUT eines SpeedOS-Prozesses (Serie 6, Teil 2).
// Alles liegt im PRIVATEN P4-Slot 1 (ab 512 GiB) seines eigenen Adressraums —
// im Kernel-Adressraum ist an diesen Adressen NICHTS gemappt.
//
//   USER_BASIS      +0x0000  Code-Seite  (Programm)
//                   +0x0100  Nachricht (Daten für debug_print)
//                   +0x0200  Puffer für copy-OUT (der Kernel schreibt hier hin)
//   ... Lücke (ungemappt) ...
//   STACK_GUARD             GUARD-PAGE — bewusst NICHT gemappt
//   STACK_UNTEN .. STACK_TOP  4 Seiten User-Stack (wächst nach unten)

/// Basis-Adresse des User-Bereichs = Anfang des privaten P4-Slots.
const USER_BASIS: u64 = adressraum::USER_START;
/// Offset der Nachricht in der Code-Page (der Code selbst ist << 0x100 Byte).
const NACHRICHT_OFFSET: u64 = 0x100;
/// Offset des copy-out-Puffers in der Code-Page.
const ZEIT_OFFSET: u64 = 0x200;
/// Anzahl Seiten des User-Stacks.
const STACK_SEITEN: usize = 4;
/// Oberes Ende des User-Stacks (1 MiB über der Code-Seite — die grosse Lücke
/// dazwischen ist ungemappt und trennt Code und Stack zusätzlich).
const USER_STACK_TOP: u64 = USER_BASIS + 0x10_0000;
/// Unterkante des Stacks; direkt DARUNTER liegt die Guard-Page.
const USER_STACK_UNTEN: u64 = USER_STACK_TOP - (STACK_SEITEN as u64) * 4096;

/// Die Nachricht, die der Ring-3-Code per Syscall drucken soll.
const NACHRICHT: &[u8] = b"Hallo aus Ring 3 - mit eigenem Adressraum!\n";

// ---------------------------------------------------------------------------
// Syscall-Nummern (in rax). Argumente in rdi/rsi, Rückgabe in rax
// (Linux-x86_64-Konvention als erprobte Vorlage).
// ---------------------------------------------------------------------------

/// debug_print(ptr, len): Bytes des Prozesses seriell ausgeben (copy-IN).
pub const SYS_DEBUG_PRINT: u64 = 0;
/// exit(): Prozess beenden.
pub const SYS_EXIT: u64 = 1;
/// zeit_ms(ptr): schreibt die Millisekunden seit dem Boot als 8 Byte in den
/// User-Puffer — der erste Syscall, der copy-OUT benutzt (die Rückrichtung).
pub const SYS_ZEIT_MS: u64 = 2;
/// schlafen(ms): Prozess auf `Zustand::Wartend` legen; der Timer weckt ihn
/// (Serie 6, Teil 3 — der erste BLOCKIERENDE Syscall).
pub const SYS_SCHLAFEN: u64 = 3;
/// yield(): Zeitscheibe freiwillig abgeben. Auch der KERNEL-Prozess benutzt
/// ihn (Executor::sleep_if_idle) — siehe scheduler::abgeben.
pub const SYS_YIELD: u64 = 4;
/// getpid(): eigene Prozess-Nummer.
pub const SYS_GETPID: u64 = 5;
/// NUR FÜR TESTS: kopiert den eingehenden Trap-Rahmen weg, damit ein Test
/// prüfen kann, dass die Kontext-SICHERUNG jedes Register korrekt erfasst.
pub const SYS_KONTEXT_TEST: u64 = 6;

// ---------------------------------------------------------------------------
// DAUERREGEL (I): der geprüfte copy-in-Helfer
// ---------------------------------------------------------------------------

/// Fehler beim Prüfen/Kopieren eines User-Puffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFehler {
    /// Adresse + Länge laufen über den Adressraum hinaus (u64-Überlauf).
    Ueberlauf,
    /// Der Puffer wäre zu groß (Schutz gegen absurde Längen).
    ZuGross,
    /// Der Bereich liegt (ganz oder teilweise) AUSSERHALB des User-Bereichs —
    /// Kernel-Adresse, Nullseite oder ein Bereich, der über die Obergrenze
    /// hinausragt. Die Prüfung ist reine Arithmetik und braucht keine
    /// Page Tables: Sie fängt den Angriff, bevor irgendetwas nachgeschlagen
    /// wird (Adressraum-Regel (a)).
    AusserhalbUserBereich,
    /// Eine berührte Page ist im Adressraum des Aufrufers nicht gemappt —
    /// z. B. weil sie in einem FREMDEN Adressraum liegt (Regel (b)).
    NichtGemappt,
    /// Eine berührte Page gehört dem KERNEL (nicht USER_ACCESSIBLE) —
    /// ein User-Zeiger, der auf Kernel-Speicher zeigt, wird abgelehnt.
    KernelSpeicher,
    /// Beim copy-OUT: Die Page ist nur lesbar (Regel (c)).
    NichtBeschreibbar,
    /// Der angegebene Prozess-Adressraum steht gar nicht in CR3 — wir würden
    /// über die Zeiger eines Adressraums in einen ANDEREN schreiben.
    FalscherAdressraum,
}

/// Höchstlänge eines copy-in/copy-out in Bytes (64 KiB — reicht für
/// Debug-Ausgaben, begrenzt den Schaden eines fehlerhaften/böswilligen
/// Längen-Arguments).
const COPY_IN_MAX: usize = 64 * 1024;

// ===========================================================================
// DIE KRITISCHSTE SICHERHEITSFLÄCHE DES KERNELS
//
// Hier — und nur hier — folgt Kernel-Code einem Zeiger, den ein
// unprivilegiertes Programm sich ausgedacht hat. Jeder Fehler in dieser
// Funktion ist eine vollständige Kernel-Übernahme. Deshalb steht die
// Prüfung DREISTUFIG und in dieser Reihenfolge da:
//
//   (a) BEREICH: Liegt [ptr, ptr+len) vollständig im User-Bereich
//       (adressraum::USER_START .. USER_ENDE)? Reine Arithmetik MIT
//       Überlauf-Prüfung — ein Zeiger nahe u64::MAX plus grosse Länge darf
//       nicht "hinten wieder rauskommen" und dabei den Kernel umschliessen.
//       Diese Stufe erledigt Kernel-Adressen und Nullzeiger, ohne auch nur
//       eine Page Table anzufassen.
//
//   (b) MAPPING: Ist JEDE berührte Page im Adressraum des AUFRUFENDEN
//       Prozesses gemappt und USER_ACCESSIBLE? Nachgeschlagen wird in den
//       Tabellen aus CR3 — beim Syscall steht dort genau der Adressraum des
//       Aufrufers. Eine Page, die nur in einem FREMDEN Adressraum existiert,
//       ist hier schlicht ungemappt. Geprüft wird JEDE Page einzeln, nicht
//       nur die erste: Der klassische Angriff ist ein Zeiger auf das letzte
//       Byte einer gültigen Seite mit einer Länge, die in ungemapptes (oder
//       fremdes) Gebiet dahinter reicht.
//
//   (c) SCHREIBRECHT (nur copy-out): Trägt die Page WRITABLE? Sonst würde
//       der Kernel im Ring 0 fröhlich in eine Seite schreiben, die der
//       Prozess selbst nur lesen darf — die Schreibsperre wäre wertlos.
//
// Erst wenn ALLES stimmt, wird kopiert. Und zwar wirklich KOPIERT: Der
// Kernel arbeitet nie auf User-Speicher weiter, sonst könnte der Prozess
// ihm die Daten unter den Händen wegändern.
//
// PANICKT NIE. Jeder Fehler ist ein Result — ein Programm darf Unsinn
// übergeben, das ist kein Kernel-Bug.
//
// TOCTOU fürs Protokoll: Zwischen Prüfung und Kopie könnte in einem
// PRÄEMPTIVEN System ein anderer Thread die Page aushängen. Bei uns läuft
// der Syscall synchron und ohne Task-Wechsel, deshalb ist Prüfen-dann-
// Kopieren korrekt. Mit dem präemptiven Scheduler kommt hier ein Lock auf
// den Adressraum dazu.
// ===========================================================================

/// Die reine Prüfung ohne Kopie — Stufen (a), (b) und (optional) (c).
/// Geprüft wird gegen den AKTIVEN Adressraum (CR3), also den des
/// aufrufenden Prozesses.
pub fn user_bereich_pruefen(
    user_ptr: u64,
    laenge: usize,
    schreiben: bool,
) -> Result<(), CopyFehler> {
    if laenge == 0 {
        return Ok(());
    }
    if laenge > COPY_IN_MAX {
        return Err(CopyFehler::ZuGross);
    }
    // (a) Bereich + Überlauf. `im_user_bereich` rechnet mit checked_add:
    // Ein Überlauf ist damit KEIN gültiger Bereich.
    let ende = user_ptr
        .checked_add(laenge as u64)
        .ok_or(CopyFehler::Ueberlauf)?;
    if !adressraum::im_user_bereich(user_ptr, laenge) {
        return Err(CopyFehler::AusserhalbUserBereich);
    }

    // (b)/(c) Jede einzelne berührte Page im Adressraum des Aufrufers.
    let mut seite = user_ptr & !0xfff;
    while seite < ende {
        match adressraum::aktive_seiten_flags(VirtAddr::new(seite)) {
            None => return Err(CopyFehler::NichtGemappt),
            Some(flags) => {
                if !flags.contains(PageTableFlags::USER_ACCESSIBLE) {
                    return Err(CopyFehler::KernelSpeicher);
                }
                if schreiben && !flags.contains(PageTableFlags::WRITABLE) {
                    return Err(CopyFehler::NichtBeschreibbar);
                }
            }
        }
        seite += 4096;
    }
    Ok(())
}

/// Kopiert `laenge` Bytes vom User-Zeiger `user_ptr` in einen frischen
/// Kernel-Vec — erst nach der vollen Prüfung oben. Panickt NIE.
pub fn copy_in(user_ptr: u64, laenge: usize) -> Result<Vec<u8>, CopyFehler> {
    if laenge == 0 {
        return Ok(Vec::new());
    }
    user_bereich_pruefen(user_ptr, laenge, false)?;

    let mut ziel = alloc::vec![0u8; laenge];
    // unsafe: Der gesamte Bereich ist geprüft im aktiven Adressraum gemappt
    // und user-zugänglich — kein Fault möglich.
    unsafe {
        core::ptr::copy_nonoverlapping(user_ptr as *const u8, ziel.as_mut_ptr(), laenge);
    }
    Ok(ziel)
}

/// Die Rückrichtung: kopiert Kernel-Daten in einen User-Puffer. Zusätzlich
/// zu allen copy-in-Prüfungen muss der Zielbereich BESCHREIBBAR sein.
pub fn copy_out(user_ptr: u64, daten: &[u8]) -> Result<(), CopyFehler> {
    if daten.is_empty() {
        return Ok(());
    }
    user_bereich_pruefen(user_ptr, daten.len(), true)?;

    // unsafe: Zielbereich ist geprüft gemappt, user-zugänglich UND
    // beschreibbar — im Adressraum, der gerade in CR3 steht.
    unsafe {
        core::ptr::copy_nonoverlapping(daten.as_ptr(), user_ptr as *mut u8, daten.len());
    }
    Ok(())
}

/// Wie `copy_in`, aber mit EXPLIZIT genanntem Prozess-Adressraum: Es wird
/// zusätzlich geprüft, dass dieser Adressraum wirklich der aktive ist.
/// Sobald es mehrere Prozesse gibt, ist das die Form, die ein Verwechseln
/// unmöglich macht — man kann nicht versehentlich die Tabellen von Prozess A
/// prüfen und in den Speicher von Prozess B greifen.
pub fn copy_in_prozess(
    raum: &adressraum::AdressRaum,
    user_ptr: u64,
    laenge: usize,
) -> Result<Vec<u8>, CopyFehler> {
    if !raum.ist_aktiv() {
        return Err(CopyFehler::FalscherAdressraum);
    }
    copy_in(user_ptr, laenge)
}

/// Wie `copy_out`, mit explizit genanntem Prozess-Adressraum.
pub fn copy_out_prozess(
    raum: &adressraum::AdressRaum,
    user_ptr: u64,
    daten: &[u8],
) -> Result<(), CopyFehler> {
    if !raum.ist_aktiv() {
        return Err(CopyFehler::FalscherAdressraum);
    }
    copy_out(user_ptr, daten)
}

// ---------------------------------------------------------------------------
// Der Syscall-Weg: INT 0x80 mit vollständiger Kontext-Sicherung
// ---------------------------------------------------------------------------
//
// WARUM INT 0x80 (Trap-Gate) und nicht SYSCALL/SYSRET?
//   * INT 0x80 nutzt die IDT-Infrastruktur, die wir schon haben — ein
//     Eintrag mit DPL 3 (damit Ring 3 ihn auslösen darf), fertig. SYSCALL
//     braucht MSR-Setup (STAR/LSTAR/SFMASK), eine bestimmte GDT-Reihenfolge
//     und ein eigenes Stack-Switching von Hand.
//   * Für den ERSTEN Beweis ist INT 0x80 einfacher UND lehrreicher (man sieht
//     den ganzen Trap-/Return-Weg explizit). SYSCALL/SYSRET (schneller, kein
//     IDT-Umweg) ist ein sinnvoller späterer Optimierungsschritt.
//
// Der Handler ist ein NACKTER (naked) Einstieg in Assembler: Er sichert ALLE
// General-Register (baut damit einen `prozess::TrapFrame` auf dem Kernel-Trap-
// Stack, RSP0) und ruft den Rust-Dispatcher mit einem Zeiger darauf.
//
// SEIT DEM SCHEDULER (Serie 6, Teil 3): Der Dispatcher LIEFERT einen Rahmen
// ZURÜCK — normalerweise denselben, bei `yield`/`exit`/`schlafen` aber den
// eines ANDEREN Prozesses. Wiederhergestellt wird er von
// `schalte_auf_rahmen` (scheduler.rs), dem einzigen Kontext-Lade-Punkt des
// Kernels. Damit ist ein Syscall ein vollwertiger Wechsel-Punkt, und
// Präemption und freiwillige Abgabe teilen genau einen Code-Pfad.
//
// Der Trap-Rahmen selbst wohnt jetzt in prozess.rs (er ist nicht mehr nur
// „Syscall-Kram", sondern DER gesicherte Prozess-Kontext).

// Der nackte Assembler-Einstiegspunkt für INT 0x80. global_asm ist robuster
// als eine naked fn (versionsunabhängig) und zeigt den Ablauf glasklar.
core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    // Alle General-Register sichern (baut den TrapFrame auf dem RSP0-Stack).
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",              // Argument 1 = Zeiger auf den TrapFrame
    "call syscall_dispatch",     // Rust-Dispatcher; rax = Rahmen, mit dem es
                                 // weitergeht (evtl. ein ANDERER Prozess!)
    "mov rdi, rax",
    "jmp schalte_auf_rahmen",    // gemeinsamer Ausstieg (scheduler.rs)
);

extern "C" {
    fn syscall_entry();
}

/// Adresse des Syscall-Einstiegs (für die IDT-Registrierung in interrupts.rs).
pub fn syscall_handler_adresse() -> u64 {
    syscall_entry as *const () as u64
}

/// Der Rust-Dispatcher: bekommt einen Zeiger auf den gesicherten Kontext,
/// liest die Syscall-Nummer (rax) + Argumente, schreibt den Rückgabewert nach
/// rax — und LIEFERT den Rahmen, mit dem es weitergeht.
///
/// Meist ist das derselbe Rahmen (der Prozess läuft weiter). Bei
/// `yield`/`schlafen`/`exit` ist es der Rahmen eines ANDEREN Prozesses: dann
/// ist dieser Syscall ein Kontext-Wechsel. Für den alten Einzelschuss-Pfad
/// (`ring3test`, kein eingeplanter Prozess) biegt `exit` wie bisher auf den
/// setjmp-Landeplatz im Kernel um.
///
/// WICHTIGE RANDBEDINGUNG (docs/scheduler-design.md §8): Ein Syscall läuft im
/// Kontext eines präemptiv geplanten Prozesses. Er darf deshalb NUR Locks
/// anfassen, die der Kernel ausschliesslich mit ausgeschalteten Interrupts
/// hält (SERIAL, Blatt-Locks, die Prozess-Tabelle) — sonst könnte er auf einen
/// Lock warten, den der verdrängte Kernel-Prozess hält: Deadlock. Alles
/// darunter (VFS, Sockets) braucht erst das Warte-Modell.
#[no_mangle]
extern "C" fn syscall_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame {
    // unsafe: `frame` zeigt auf unseren eben gesicherten Register-Block auf
    // dem Kernel-Stack des aufrufenden Prozesses — gültig für die Dauer des
    // Handlers.
    let f = unsafe { &mut *frame };
    scheduler::syscall_zaehlen();
    match f.rax {
        SYS_DEBUG_PRINT => {
            // debug_print(ptr = rdi, len = rsi) — copy-in, dann seriell.
            match copy_in(f.rdi, f.rsi as usize) {
                Ok(bytes) => {
                    // In EINEM Stück ausgeben (ein Lock-Zyklus statt einem pro
                    // Byte) — der Zähler-Prozess druckt oft.
                    match core::str::from_utf8(&bytes) {
                        Ok(text) => serial_print!("{}", text),
                        Err(_) => {
                            for &b in &bytes {
                                serial_print!("{}", b as char);
                            }
                        }
                    }
                    // Für den Präemptions-BEWEIS: Wer hat wann gedruckt? Der
                    // lock-freie Spur-Ring hält die Reihenfolge fest (das
                    // letzte druckbare Zeichen ist bei unseren Zähler-
                    // Prozessen gerade die Zählerziffer).
                    let zeichen = bytes
                        .iter()
                        .rev()
                        .copied()
                        .find(|b| b.is_ascii_graphic())
                        .unwrap_or(b'?');
                    scheduler::spur_melden(scheduler::laufende_user_pid(), zeichen);
                    f.rax = bytes.len() as u64; // Rückgabe: gedruckte Bytes
                }
                Err(fehler) => {
                    serial_println!("[syscall] debug_print: ungueltiger Puffer ({:?})", fehler);
                    f.rax = u64::MAX; // Fehler-Kennung
                }
            }
        }
        SYS_YIELD => {
            // Freiwillige Abgabe: der einzige Syscall, der NICHTS tut, ausser
            // die Zeitscheibe zurückzugeben.
            f.rax = 0;
            return scheduler::syscall_abgeben(frame);
        }
        SYS_SCHLAFEN => {
            // schlafen(ms = rdi): Zustand::Wartend, der Timer weckt wieder.
            f.rax = 0;
            if let Some(neuer) = scheduler::syscall_schlafen(frame, f.rdi) {
                return neuer;
            }
        }
        SYS_GETPID => {
            f.rax = scheduler::laufende_user_pid() as u64;
        }
        SYS_KONTEXT_TEST => {
            // Nur Tests: den EINGEHENDEN Rahmen wegkopieren. Damit prüft
            // tests/scheduler.rs, dass die Kontext-Sicherung wirklich jedes
            // einzelne Register erfasst (synthetische Magic-Werte).
            kontext_test_sichern(*f);
            f.rax = KONTEXT_TEST_ANTWORT;
        }
        SYS_ZEIT_MS => {
            // zeit_ms(ptr = rdi): 8 Byte in den User-Puffer schreiben.
            // Die Rückrichtung ist NICHT harmloser als copy-in — im Gegenteil:
            // Hier schreibt Ring-0-Code an eine Adresse, die sich Ring 3
            // ausgedacht hat. `copy_out` prüft deshalb zusätzlich WRITABLE.
            let ms = crate::zeit::ms_seit_boot();
            match copy_out(f.rdi, &ms.to_le_bytes()) {
                Ok(()) => f.rax = 0,
                Err(fehler) => {
                    serial_println!("[syscall] zeit_ms: ungueltiger Puffer ({:?})", fehler);
                    f.rax = u64::MAX;
                }
            }
        }
        SYS_EXIT => {
            // (a) EINGEPLANTER Prozess: beenden und auf den nächsten
            //     lauffähigen Prozess umschalten (der Scheduler-Weg).
            if let Some(neuer) = scheduler::syscall_beenden(frame) {
                return neuer;
            }
            // (b) EINZELSCHUSS-PFAD (ring3test, kein Prozess): Zurück in den
            //     Kernel, indem der CPU-Interrupt-Rahmen auf den LANDEPLATZ
            //     umgebogen wird (Ring 0). Der iretq springt dann dorthin
            //     statt nach Ring 3; der Landeplatz stellt den gesicherten
            //     Kernel-Kontext wieder her.
            f.rip = recovery_rip();
            f.rsp = recovery_rsp();
            f.cs = gdt::kernel_code_selektor();
            f.ss = gdt::kernel_data_selektor();
            f.rflags = 0x202; // IF gesetzt
        }
        andere => {
            serial_println!("[syscall] unbekannte Nummer {}", andere);
            f.rax = u64::MAX;
        }
    }
    // Normalfall: derselbe Prozess läuft weiter.
    frame
}

// ---------------------------------------------------------------------------
// Der Kontext-Sicherungs-TEST (synthetische Register-Sätze)
// ---------------------------------------------------------------------------
//
// Die Kontext-Sicherung ist die Stelle, an der ein Fehler NICHT als Absturz
// erscheint, sondern als „irgendein Register hatte plötzlich einen falschen
// Wert" — praktisch nicht diagnostizierbar. Deshalb prüfen wir sie direkt:
// Ein Assembler-Stub lädt ALLE General-Register mit unverwechselbaren
// Magic-Werten, löst `int 0x80` aus und schreibt danach seine Register in
// einen zweiten Block. Der Test vergleicht beides.
//   * Block 1 (GESICHERT) beweist den SAVE-Pfad: kam alles im TrapFrame an?
//   * Block 2 (NACHHER)   beweist den RESTORE-Pfad: kam alles zurück?

/// Rückgabewert von SYS_KONTEXT_TEST (unverwechselbar, damit der Test auch
/// den rax-Rückweg prüfen kann).
pub const KONTEXT_TEST_ANTWORT: u64 = 0x5EED_0000_C0DE_0042;

/// Der gesicherte Rahmen des letzten SYS_KONTEXT_TEST.
static KONTEXT_GESICHERT: spin::Mutex<Option<TrapFrame>> = spin::Mutex::new(None);

fn kontext_test_sichern(rahmen: TrapFrame) {
    // Blatt-Lock, im Syscall-Kontext erlaubt (niemand hält ihn lange).
    *KONTEXT_GESICHERT.lock() = Some(rahmen);
}

/// Der beim letzten `kontext_test_ausfuehren()` gesicherte Rahmen.
pub fn kontext_gesichert() -> Option<TrapFrame> {
    *KONTEXT_GESICHERT.lock()
}

// Der Stub. Er darf die callee-saved Register (rbx, rbp, r12-r15) nicht
// dauerhaft zerstören — deshalb erst sichern, am Ende zurückholen.
// Nach `int 0x80` werden ALLE Register in TrapFrame-Reihenfolge gepusht und
// als Zeiger an Rust gegeben (`kontext_test_nachher`); die fünf
// CPU-Rahmen-Felder werden mit 0 aufgefüllt, damit dieselbe Struktur passt.
core::arch::global_asm!(
    ".global kontext_test_stub",
    "kontext_test_stub:",
    // Die callee-saved Register des Aufrufers retten (C-ABI-Pflicht).
    "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
    // AUSRICHTUNG: Beim Betreten ist RSP ≡ 8 (mod 16) — die Rücksprung-
    // Adresse liegt drauf. 6 Pushes ändern daran nichts (48 ≡ 0), also
    // schaffen wir hier die fehlenden 8 Byte, damit RSP am `call` unten
    // 16-ausgerichtet ist.
    "sub rsp, 8",
    // Alle Register mit unverwechselbaren Magic-Werten laden.
    "movabs rbx, 0x1111111111111111",
    "movabs rcx, 0x2222222222222222",
    "movabs rdx, 0x3333333333333333",
    "movabs rsi, 0x4444444444444444",
    "movabs rdi, 0x5555555555555555",
    "movabs rbp, 0x6666666666666666",
    "movabs r8,  0x7777777777777777",
    "movabs r9,  0x0808080808080808",
    "movabs r10, 0x0A0A0A0A0A0A0A0A",
    "movabs r11, 0x0B0B0B0B0B0B0B0B",
    "movabs r12, 0x0C0C0C0C0C0C0C0C",
    "movabs r13, 0x0D0D0D0D0D0D0D0D",
    "movabs r14, 0x0E0E0E0E0E0E0E0E",
    "movabs r15, 0x7F7F7F7F7F7F7F7F",
    "mov rax, 6",                 // SYS_KONTEXT_TEST
    "int 0x80",
    // Jetzt der Zustand NACH der Rückkehr — in TrapFrame-Reihenfolge pushen
    // (erst die fünf CPU-Felder als 0, dann rax..r15).
    "push 0", "push 0", "push 0", "push 0", "push 0",   // ss/rsp/rflags/cs/rip
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "call kontext_test_nachher",
    "add rsp, 168",               // 20 Qwords + die 8 Ausrichtungs-Bytes
    "pop r15", "pop r14", "pop r13", "pop r12", "pop rbp", "pop rbx",
    "ret",
);

extern "C" {
    fn kontext_test_stub();
}

/// Der Register-Zustand NACH der Syscall-Rückkehr.
static KONTEXT_NACHHER: spin::Mutex<Option<TrapFrame>> = spin::Mutex::new(None);

/// Wird vom Stub gerufen (mit einem Zeiger auf den gepushten Block).
#[no_mangle]
extern "C" fn kontext_test_nachher(rahmen: *const TrapFrame) {
    // unsafe: Der Stub hat 20 Qwords in TrapFrame-Reihenfolge gepusht und
    // zeigt genau darauf.
    let kopie = unsafe { *rahmen };
    *KONTEXT_NACHHER.lock() = Some(kopie);
}

/// Der Register-Zustand nach dem letzten `kontext_test_ausfuehren()`.
pub fn kontext_nachher() -> Option<TrapFrame> {
    *KONTEXT_NACHHER.lock()
}

/// Führt den Kontext-Sicherungs-Test aus (Ring 0 genügt: gesichert und
/// wiederhergestellt wird auf beiden Privilegstufen mit demselben Code).
pub fn kontext_test_ausfuehren() {
    *KONTEXT_GESICHERT.lock() = None;
    *KONTEXT_NACHHER.lock() = None;
    // unsafe: Ein reiner Assembler-Aufruf, der nur Register setzt, `int 0x80`
    // auslöst und alle callee-saved Register wiederherstellt.
    unsafe { kontext_test_stub() };
}

/// Die erwarteten Magic-Werte (Reihenfolge wie im Stub) für den Test.
pub fn kontext_test_erwartung() -> TrapFrame {
    TrapFrame {
        rbx: 0x1111_1111_1111_1111,
        rcx: 0x2222_2222_2222_2222,
        rdx: 0x3333_3333_3333_3333,
        rsi: 0x4444_4444_4444_4444,
        rdi: 0x5555_5555_5555_5555,
        rbp: 0x6666_6666_6666_6666,
        r8: 0x7777_7777_7777_7777,
        r9: 0x0808_0808_0808_0808,
        r10: 0x0A0A_0A0A_0A0A_0A0A,
        r11: 0x0B0B_0B0B_0B0B_0B0B,
        r12: 0x0C0C_0C0C_0C0C_0C0C,
        r13: 0x0D0D_0D0D_0D0D_0D0D,
        r14: 0x0E0E_0E0E_0E0E_0E0E,
        r15: 0x7F7F_7F7F_7F7F_7F7F,
        rax: SYS_KONTEXT_TEST,
        ..TrapFrame::default()
    }
}

// ---------------------------------------------------------------------------
// Der Übergang nach Ring 3 (und der Recovery-Punkt für den Rückweg)
// ---------------------------------------------------------------------------

/// Läuft gerade Ring-3-Code? (Der Page-Fault-/GP-Handler recovert NUR dann.)
static RING3_AKTIV: AtomicBool = AtomicBool::new(false);

/// Der gespeicherte KERNEL-Kontext (setjmp-Puffer): callee-saved Register +
/// Stack + Rücksprung-Adresse. WARUM setjmp/longjmp statt eines einzelnen
/// Inline-asm-Blocks: Der Rückweg aus Ring 3 kommt über einen TRAP-Handler
/// (Syscall/Page Fault), verlässt und betritt den Kernel-Code also an einer
/// Stelle, die der Compiler nicht als normalen Kontrollfluss sieht. Würde man
/// das in EINEN asm-Block mit Sprung-Label packen, wüsste der Compiler nichts
/// von der Rückkehr und verwaltet die Register falsch (führt zu Korruption).
/// setjmp SICHERT den Kernel-Kontext sauber, ein Landeplatz STELLT ihn wieder
/// her — das klassische, robuste Muster.
#[repr(C)]
struct SprungPuffer {
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rsp: u64,
    rip: u64,
}

static mut RING3_SPRUNG: SprungPuffer = SprungPuffer {
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
    rsp: 0,
    rip: 0,
};

// Drei Assembler-Bausteine (glasklar, versionsunabhängig via global_asm):
//   * kern_setjmp(buf): sichert callee-saved + rsp + Rücksprung-RIP, gibt 0.
//   * kern_ring3_landing: der LANDEPLATZ — stellt den Kernel-Kontext aus dem
//     Puffer wieder her und „kehrt" als kern_setjmp-Rückkehr mit 1 zurück.
//   * iretq_nach_ring3(entry, stack, cs, ss): baut den iret-Rahmen und springt
//     EINWEG nach Ring 3 (kommt nur über einen Trap -> Landeplatz zurück).
core::arch::global_asm!(
    ".global kern_setjmp",
    "kern_setjmp:",                 // rdi = &SprungPuffer
    "mov [rdi + 0x00], rbx",
    "mov [rdi + 0x08], rbp",
    "mov [rdi + 0x10], r12",
    "mov [rdi + 0x18], r13",
    "mov [rdi + 0x20], r14",
    "mov [rdi + 0x28], r15",
    "lea rax, [rsp + 8]",           // rsp des Aufrufers (nach dem ret)
    "mov [rdi + 0x30], rax",
    "mov rax, [rsp]",               // Rücksprung-Adresse
    "mov [rdi + 0x38], rax",
    "xor eax, eax",                 // erste Rückkehr: 0
    "ret",

    ".global kern_ring3_landing",
    "kern_ring3_landing:",          // kein Argument; nutzt RING3_SPRUNG
    "lea rdi, [rip + {buf}]",
    "mov rbx, [rdi + 0x00]",
    "mov rbp, [rdi + 0x08]",
    "mov r12, [rdi + 0x10]",
    "mov r13, [rdi + 0x18]",
    "mov r14, [rdi + 0x20]",
    "mov r15, [rdi + 0x28]",
    "mov rsp, [rdi + 0x30]",
    "mov r11, [rdi + 0x38]",        // gesicherte Rücksprung-Adresse
    "mov eax, 1",                   // zweite „Rückkehr" von kern_setjmp: 1
    "jmp r11",

    ".global iretq_nach_ring3",
    "iretq_nach_ring3:",            // rdi=entry, rsi=stack, rdx=cs, rcx=ss
    "push rcx",                     // SS
    "push rsi",                     // RSP (User-Stack)
    "push 0x202",                   // RFLAGS (IF gesetzt)
    "push rdx",                     // CS
    "push rdi",                     // RIP (Einsprung)
    "iretq",
    buf = sym RING3_SPRUNG,
);

extern "C" {
    fn kern_setjmp(buf: *mut SprungPuffer) -> u64;
    fn kern_ring3_landing();
    fn iretq_nach_ring3(entry: u64, stack: u64, cs: u64, ss: u64) -> !;
}

/// Läuft gerade unser Ring-3-Testcode? (für Page-Fault-/GP-Handler).
pub fn ring3_aktiv() -> bool {
    RING3_AKTIV.load(Ordering::SeqCst)
}
/// Der Kernel-Recovery-RIP: der Landeplatz, der den Kernel-Kontext
/// wiederherstellt (Ziel, auf das der Trap-Handler den iret-Rahmen umbiegt).
pub fn recovery_rip() -> u64 {
    kern_ring3_landing as *const () as u64
}
/// Der Kernel-Recovery-RSP (ein gültiger Kernel-Stack für den iret des
/// Handlers; der Landeplatz setzt rsp gleich selbst aus dem Puffer).
pub fn recovery_rsp() -> u64 {
    // unsafe: reiner Lesezugriff auf ein u64-Feld, single-threaded.
    unsafe { core::ptr::addr_of!(RING3_SPRUNG.rsp).read() }
}


/// Springt nach Ring 3 (Einsprung `user_entry`, User-Stack `user_stack`) und
/// kehrt erst zurück, wenn der User-Code per `exit` endet oder abstürzt.
///
/// NEU in Serie 6, Teil 2: Vorher wird auf den ADRESSRAUM DES PROZESSES
/// umgeschaltet (CR3). Ab dann sieht die CPU nur noch dessen private Seiten —
/// plus den gespiegelten Kernel, denn ein Timer-Interrupt kann jederzeit
/// mitten im User-Code zuschlagen und braucht Kernel-Code und Kernel-Stack
/// an ihrer gewohnten Adresse. Beim Rückweg (exit ODER Absturz) schalten wir
/// auf den Kernel-Adressraum zurück.
///
/// WARUM iretq (und nicht sysretq)? `iretq` ist der GENERISCHE Sprung-Befehl:
/// Er lädt CS:RIP, RFLAGS UND SS:RSP aus einem Stack-Rahmen — wir bauen den
/// Rahmen, den ein Trap aus Ring 3 hinterlassen HÄTTE, und „springen dorthin".
/// `sysretq` bräuchte MSR-Einrichtung und eine bestimmte Segment-Anordnung.
fn nach_ring3(raum: &mut AdressRaum, user_entry: u64, user_stack: u64) {
    let ucs = gdt::user_code_selektor();
    let uds = gdt::user_data_selektor();

    // SCHEDULER-SPERRE (Serie 6, Teil 3): Dieser Pfad führt Ring-3-Code IM
    // KONTEXT DES KERNEL-PROZESSES aus, mit fremdem CR3 und ohne PCB. Käme
    // hier ein Kontext-Wechsel dazwischen, würde PID 0 später mit
    // Kernel-CR3 mitten im User-Code fortgesetzt — Absturz. Der Einzelschuss-
    // Pfad bricht die Invariante des Schedulers bewusst, also sperrt er die
    // Planung, solange er läuft. Es ist die EINZIGE Stelle mit dieser Sperre.
    scheduler::sperre_erhoehen();

    // Adressraum-Wechsel: ab hier ist der Prozess-Adressraum aktiv.
    raum.aktivieren();
    RING3_AKTIV.store(true, Ordering::SeqCst);

    // setjmp: Kernel-Kontext sichern. Beim ERSTEN Aufruf 0 -> nach Ring 3
    // springen (kehrt NICHT hierher zurück). Der Trap-Handler biegt den
    // Rückweg auf den Landeplatz um, der kern_setjmp „mit 1" zurückkehren
    // lässt -> wir landen hier mit erste != 0 und laufen weiter.
    // unsafe: roher Kontext-Wechsel / Privilegienwechsel.
    let erste = unsafe { kern_setjmp(core::ptr::addr_of_mut!(RING3_SPRUNG)) };
    if erste == 0 {
        // unsafe: iretq nach Ring 3 — kehrt nur über einen Trap zurück.
        unsafe { iretq_nach_ring3(user_entry, user_stack, ucs, uds) };
    }

    RING3_AKTIV.store(false, Ordering::SeqCst);
    // Zurück in den Kernel-Adressraum — egal ob der Prozess sauber beendet
    // hat oder abgestürzt ist. Diese Zeile MUSS auf beiden Wegen laufen.
    adressraum::kernel_aktivieren();
    // Und ebenso die Sperre lösen (beide Rückwege kommen hier vorbei).
    scheduler::sperre_senken();
}

// ---------------------------------------------------------------------------
// Die Testprogramme (hand-assemblierter Ring-3-Maschinencode)
// ---------------------------------------------------------------------------

/// Baut das ERFOLGS-Programm:
///   1. zeit_ms(puffer)   -> beweist copy-OUT (Kernel schreibt zu Ring 3)
///   2. debug_print(text) -> beweist copy-IN  (Kernel liest von Ring 3)
///   3. exit
///
/// Position-unabhängig muss es nicht sein — wir kennen die Ziel-Adressen.
fn programm_erfolg(nachricht_va: u64, laenge: usize, zeit_va: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xC7, 0xC0, 0x02, 0x00, 0x00, 0x00]); // mov rax, 2 (zeit_ms)
    p.extend_from_slice(&[0x48, 0xBF]); // movabs rdi, zeit_va
    p.extend_from_slice(&zeit_va.to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    p.extend_from_slice(&[0x48, 0xC7, 0xC0, 0x00, 0x00, 0x00, 0x00]); // mov rax, 0 (debug_print)
    p.extend_from_slice(&[0x48, 0xBF]); // movabs rdi, nachricht_va
    p.extend_from_slice(&nachricht_va.to_le_bytes());
    p.extend_from_slice(&[0x48, 0xC7, 0xC6]); // mov rsi, laenge (imm32)
    p.extend_from_slice(&(laenge as u32).to_le_bytes());
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    p.extend_from_slice(&[0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00]); // mov rax, 1 (exit)
    p.extend_from_slice(&[0xCD, 0x80]); // int 0x80
    p.extend_from_slice(&[0xEB, 0xFE]); // jmp $ (Sicherheitsschleife)
    p
}

/// Baut das ABSTURZ-Programm: liest aus einer KERNEL-Adresse (verboten aus
/// Ring 3) -> Page Fault.
fn programm_absturz(kernel_adresse: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xB8]); // movabs rax, kernel_adresse
    p.extend_from_slice(&kernel_adresse.to_le_bytes());
    p.extend_from_slice(&[0x8A, 0x00]); // mov al, [rax]  -> Zugriff -> #PF
    p.extend_from_slice(&[0xEB, 0xFE]); // jmp $ (wird nie erreicht)
    p
}

/// Baut das STACK-ÜBERLAUF-Programm: setzt rsp auf die UNTERKANTE des Stacks
/// und pusht — der Push landet damit 8 Byte darunter, also mitten in der
/// GUARD-PAGE. Genau dafür ist sie da: Statt still den Speicher unterhalb des
/// Stacks zu zerschreiben, gibt es sofort einen Page Fault.
fn programm_stack_ueberlauf(stack_unterkante: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0x48, 0xBC]); // movabs rsp, stack_unterkante
    p.extend_from_slice(&stack_unterkante.to_le_bytes());
    p.push(0x50); // push rax -> schreibt bei rsp-8 = Guard-Page -> #PF
    p.extend_from_slice(&[0xEB, 0xFE]); // jmp $ (wird nie erreicht)
    p
}

// ---------------------------------------------------------------------------
// Prozess-Aufbau: ein Adressraum, eine Code-Seite, ein Stack mit Guard-Page
// ---------------------------------------------------------------------------

/// Baut einen frischen Prozess-Adressraum mit Code-Seite und User-Stack.
///
/// Beachte, wie der Kernel den Code hineinbekommt: über `raum.schreiben(...)`
/// — also über das Physik-Komplettmapping, OHNE den Adressraum zu aktivieren.
/// Genau dieses Muster wird der ELF-Loader benutzen (Programm laden, während
/// der Prozess noch gar nicht läuft).
fn prozess_aufsetzen(code: &[u8], nachricht: Option<&[u8]>) -> AdressRaum {
    let mut raum = AdressRaum::neu().expect("Prozess-Adressraum anlegen");
    raum.bereich_mappen(VirtAddr::new(USER_BASIS), 4096)
        .expect("Code-Seite mappen");
    raum.stack_anlegen(VirtAddr::new(USER_STACK_TOP), STACK_SEITEN)
        .expect("User-Stack anlegen");
    raum.schreiben(VirtAddr::new(USER_BASIS), code)
        .expect("Code laden");
    if let Some(n) = nachricht {
        raum.schreiben(VirtAddr::new(USER_BASIS + NACHRICHT_OFFSET), n)
            .expect("Nachricht laden");
    }
    raum
}

/// TEST 1 (Erfolg): Ring-3-Code läuft im EIGENEN Adressraum, holt sich per
/// copy-out die Uptime, druckt per copy-in seine Nachricht und beendet sich.
pub fn ring3_erfolg() {
    let code = programm_erfolg(
        USER_BASIS + NACHRICHT_OFFSET,
        NACHRICHT.len(),
        USER_BASIS + ZEIT_OFFSET,
    );
    let mut raum = prozess_aufsetzen(&code, Some(NACHRICHT));
    serial_println!(
        "[ring3] Eigener Adressraum (P4 {:#x}), {} Frames. Einsprung {:#x}, Stack {:#x} ...",
        raum.p4_frame().start_address().as_u64(),
        raum.frames_besitz(),
        USER_BASIS,
        USER_STACK_TOP
    );
    nach_ring3(&mut raum, USER_BASIS, USER_STACK_TOP);
    serial_println!("[ring3] Sauber zurueck im Kernel (Ring 0) — System laeuft weiter.");

    // BEWEIS für copy-out: Der Kernel hat in den User-Puffer geschrieben.
    // Wir lesen ihn jetzt aus dem (inaktiven!) Adressraum zurück.
    let mut puffer = [0u8; 8];
    if raum
        .lesen(VirtAddr::new(USER_BASIS + ZEIT_OFFSET), &mut puffer)
        .is_ok()
    {
        let ms = u64::from_le_bytes(puffer);
        serial_println!(
            "[ring3] copy-out belegt: Der Prozess hat {} ms Uptime erhalten.",
            ms
        );
    }
    raum.abreissen();
}

/// TEST 2 (Absturz): Ring-3-Code greift auf eine KERNEL-Adresse zu. Sie ist
/// in seinem Adressraum zwar GEMAPPT (der Kernel ist ja gespiegelt), aber
/// ohne U-Bit — Page Fault. Erwartung: klare Meldung „aus User-Mode", und
/// der KERNEL LEBT WEITER.
pub fn ring3_absturz() {
    let kernel_adresse = crate::allocator::HEAP_START as u64;
    let code = programm_absturz(kernel_adresse);
    let mut raum = prozess_aufsetzen(&code, None);
    serial_println!(
        "[ring3] Ring-3-Code greift jetzt VERBOTEN auf Kernel-Adresse {:#x} zu ...",
        kernel_adresse
    );
    nach_ring3(&mut raum, USER_BASIS, USER_STACK_TOP);
    serial_println!(
        "[ring3] Der Absturz wurde aufgefangen — der KERNEL LEBT WEITER. (Genau das ist neu!)"
    );
    raum.abreissen();
}

/// TEST 3 (Stack-Überlauf): Der Prozess pusht über die Unterkante seines
/// Stacks hinaus. Die GUARD-PAGE fängt es als Page Fault ab, statt still
/// fremden Speicher zu zerschreiben.
pub fn ring3_stack_ueberlauf() {
    let code = programm_stack_ueberlauf(USER_STACK_UNTEN);
    let mut raum = prozess_aufsetzen(&code, None);
    serial_println!(
        "[ring3] Ring-3-Code pusht unter die Stack-Unterkante {:#x} (Guard-Page bei {:#x}) ...",
        USER_STACK_UNTEN,
        USER_STACK_UNTEN - 4096
    );
    nach_ring3(&mut raum, USER_BASIS, USER_STACK_TOP);
    serial_println!("[ring3] Stack-Ueberlauf sauber als Page Fault gefangen (Guard-Page wirkt).");
    raum.abreissen();
}

// ---------------------------------------------------------------------------
// Tests — DIE ANGRIFFSVARIANTEN gegen die copy-Helfer
//
// Diese Tests sind der Grund, warum copy_in/copy_out überhaupt existieren.
// Jeder einzelne bildet einen echten Angriff nach, mit dem ein bösartiges
// Ring-3-Programm den Kernel übernehmen könnte. Erwartet wird IMMER: ein
// sauberer Fehlerwert, keine Panik, kein Fault, keine Datenpreisgabe.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::structures::paging::Page;

    /// Eine Test-Adresse im User-Bereich, weit weg von Code und Stack.
    const TEST_VA: u64 = USER_BASIS + 0x0020_0000; // +2 MiB

    /// Legt einen Adressraum mit einer beschreibbaren User-Seite bei TEST_VA
    /// an und AKTIVIERT ihn — danach ist er „der aufrufende Prozess".
    fn testraum_aktiv() -> AdressRaum {
        let mut raum = AdressRaum::neu().expect("Adressraum");
        raum.map_benutzer(Page::containing_address(VirtAddr::new(TEST_VA)))
            .expect("User-Page");
        raum.aktivieren();
        raum
    }

    /// ANGRIFF 1–5: alle Varianten, mit denen ein User-Zeiger den Kernel
    /// aus der Spur bringen soll.
    #[test_case]
    fn test_copy_in_angriffe() {
        let raum = testraum_aktiv();

        // (1) KERNEL-ADRESSE: Der Prozess zeigt auf den Kernel-Heap. Der
        //     liegt in SEINEM Adressraum sogar gemappt vor (gespiegelt!) —
        //     aber ausserhalb des User-Bereichs, also sofort abgelehnt.
        assert_eq!(
            copy_in(crate::allocator::HEAP_START as u64, 8),
            Err(CopyFehler::AusserhalbUserBereich)
        );
        // (2) NULLZEIGER / niedrige Adresse: ausserhalb des User-Bereichs.
        assert_eq!(copy_in(0, 8), Err(CopyFehler::AusserhalbUserBereich));
        assert_eq!(copy_in(0x1_0000, 8), Err(CopyFehler::AusserhalbUserBereich));
        // (3) OBERE HÄLFTE (klassische Kernel-Adresse): dito.
        assert_eq!(
            copy_in(0xffff_8000_0000_0000, 8),
            Err(CopyFehler::AusserhalbUserBereich)
        );
        // (4) INTEGER-ÜBERLAUF bei Zeiger + Länge: Ein Zeiger dicht unter
        //     u64::MAX plus Länge darf nicht „hinten wieder rauskommen".
        assert_eq!(copy_in(u64::MAX - 3, 16), Err(CopyFehler::Ueberlauf));
        assert_eq!(copy_in(u64::MAX, 1), Err(CopyFehler::Ueberlauf));
        // (5) ABSURDE LÄNGE: Deckel greift, bevor irgendetwas alloziert wird.
        assert_eq!(copy_in(TEST_VA, 8 * 1024 * 1024), Err(CopyFehler::ZuGross));
        // Länge 0 ist immer ok (leer), egal welche Adresse — nichts wird
        // dereferenziert, es gibt also auch nichts zu schützen.
        assert_eq!(copy_in(0, 0), Ok(Vec::new()));
        assert_eq!(
            copy_in(crate::allocator::HEAP_START as u64, 0),
            Ok(Vec::new())
        );

        adressraum::kernel_aktivieren();
        drop(raum);
    }

    /// ANGRIFF 6: Länge läuft über die Seitengrenze in UNGEMAPPTES Gebiet.
    /// Der Klassiker — die erste Seite ist gültig, deshalb reicht es nicht,
    /// nur den Anfangszeiger zu prüfen.
    #[test_case]
    fn test_copy_in_ueber_seitengrenze() {
        let mut raum = testraum_aktiv();
        let daten = b"copy-in funktioniert";
        // schreiben() geht über das Physik-Mapping und braucht den Adressraum
        // nicht aktiv — er ist es hier nur, weil copy_in gleich prüfen soll.
        raum.schreiben(VirtAddr::new(TEST_VA), daten)
            .expect("Daten in den Prozess legen");

        // Der gültige Fall: liefert genau die Bytes.
        assert_eq!(copy_in(TEST_VA, daten.len()), Ok(daten.to_vec()));

        // Der Angriff: 6 Byte vor Seitenende beginnen, 32 Byte verlangen —
        // die Folgeseite ist nicht gemappt. Es wird NICHTS kopiert.
        assert_eq!(copy_in(TEST_VA + 4090, 32), Err(CopyFehler::NichtGemappt));
        // Auch der Kopf des Bereichs bleibt unangetastet: ein zweiter,
        // gültiger Zugriff liefert weiter die Originaldaten.
        assert_eq!(copy_in(TEST_VA, daten.len()), Ok(daten.to_vec()));

        adressraum::kernel_aktivieren();
        drop(raum);
    }

    /// ANGRIFF 7: FREMDER ADRESSRAUM. Prozess B hat bei TEST_VA eine Seite;
    /// Prozess A (der gerade läuft) nicht. A darf sie unter KEINEN Umständen
    /// sehen — obwohl die Adresse identisch ist und die Seite physisch
    /// existiert. Das ist die eigentliche Isolations-Prüfung.
    #[test_case]
    fn test_copy_in_fremder_adressraum() {
        // Prozess B: hat die Seite und legt ein Geheimnis hinein.
        let mut b = AdressRaum::neu().expect("Adressraum B");
        b.map_benutzer(Page::containing_address(VirtAddr::new(TEST_VA)))
            .expect("User-Page in B");
        b.schreiben(VirtAddr::new(TEST_VA), b"GEHEIMNIS-VON-PROZESS-B")
            .expect("Geheimnis schreiben");

        // Prozess A: hat bei TEST_VA NICHTS und läuft jetzt.
        let mut a = AdressRaum::neu().expect("Adressraum A");
        a.aktivieren();
        assert_eq!(copy_in(TEST_VA, 23), Err(CopyFehler::NichtGemappt));
        // Auch die explizite Form schützt: Wer den Adressraum von B nennt,
        // während A läuft, bekommt einen Fehler statt B's Daten.
        assert_eq!(
            copy_in_prozess(&b, TEST_VA, 23),
            Err(CopyFehler::FalscherAdressraum)
        );

        adressraum::kernel_aktivieren();
        drop(a);
        drop(b);
    }

    /// copy-OUT: dieselben Prüfungen plus Schreibrecht — und der Nachweis,
    /// dass wirklich in den User-Speicher geschrieben wurde.
    #[test_case]
    fn test_copy_out_angriffe_und_erfolg() {
        let mut raum = testraum_aktiv();

        // Erfolgsfall: schreiben und im Adressraum wiederfinden.
        assert_eq!(copy_out(TEST_VA, b"Kernel war hier"), Ok(()));
        let mut zurueck = [0u8; 15];
        adressraum::kernel_aktivieren();
        raum.lesen(VirtAddr::new(TEST_VA), &mut zurueck)
            .expect("zurueck lesen");
        assert_eq!(&zurueck, b"Kernel war hier");
        raum.aktivieren();

        // Kernel-Adresse als ZIEL — der gefährlichste Fall überhaupt
        // (der Kernel würde sich selbst überschreiben lassen).
        assert_eq!(
            copy_out(crate::allocator::HEAP_START as u64, b"boese"),
            Err(CopyFehler::AusserhalbUserBereich)
        );
        // Ungemappt, Überlauf und über die Seitengrenze hinaus:
        assert_eq!(copy_out(TEST_VA + 4096, b"x"), Err(CopyFehler::NichtGemappt));
        assert_eq!(
            copy_out(u64::MAX - 3, b"abcdefgh"),
            Err(CopyFehler::Ueberlauf)
        );
        assert_eq!(
            copy_out(TEST_VA + 4090, &[0u8; 32]),
            Err(CopyFehler::NichtGemappt)
        );

        adressraum::kernel_aktivieren();
        drop(raum);
    }

    /// Ohne aktiven User-Adressraum (reiner Kernel-Kontext) ist im
    /// User-Bereich NICHTS gemappt — jeder User-Zeiger läuft ins Leere.
    /// Der Kernel kann also gar nicht versehentlich User-Daten anfassen,
    /// wenn kein Prozess läuft.
    #[test_case]
    fn test_copy_in_ohne_prozess() {
        adressraum::kernel_aktivieren();
        assert_eq!(copy_in(TEST_VA, 8), Err(CopyFehler::NichtGemappt));
        assert_eq!(copy_out(TEST_VA, b"x"), Err(CopyFehler::NichtGemappt));
    }
}
