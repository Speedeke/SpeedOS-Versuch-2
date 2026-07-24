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

use crate::{gdt, memory, serial_print, serial_println};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::structures::paging::{Page, PageTableFlags};
use x86_64::VirtAddr;

/// Basis-Adresse des User-Bereichs für diesen ersten Versuch: 512 GiB.
/// Bewusst in einem eigenen, sonst ungenutzten P4-Slot (Index 1) der unteren
/// (User-)Adressraumhälfte — weit weg von Heap (~75 TiB), DMA (~122 TiB) und
/// dem Kernel (obere Hälfte). Eine Page trägt Code + Nachricht, die zweite den
/// User-Stack.
const USER_BASIS: u64 = 0x0000_0080_0000_0000;
/// Offset der Nachricht in der Code-Page (der Code selbst ist << 0x100 Byte).
const NACHRICHT_OFFSET: u64 = 0x100;
/// Oberes Ende des User-Stacks (Top der zweiten Page).
const USER_STACK_TOP: u64 = USER_BASIS + 0x2000;

/// Die Nachricht, die der Ring-3-Code per Syscall drucken soll.
const NACHRICHT: &[u8] = b"Hallo aus Ring 3!\n";

/// Syscall-Nummern (in rax). Für diesen Prompt genügt debug_print; `exit`
/// ist der triviale Rückweg (aus Ring 3 zurück in den Kernel).
const SYS_DEBUG_PRINT: u64 = 0;
const SYS_EXIT: u64 = 1;

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
    /// Eine berührte Page ist gar nicht gemappt.
    NichtGemappt,
    /// Eine berührte Page gehört dem KERNEL (nicht USER_ACCESSIBLE) —
    /// ein User-Zeiger, der auf Kernel-Speicher zeigt, wird abgelehnt.
    KernelSpeicher,
}

/// Höchstlänge eines copy-in in Bytes (64 KiB — reicht für Debug-Ausgaben,
/// begrenzt den Schaden eines fehlerhaften/böswilligen Längen-Arguments).
const COPY_IN_MAX: usize = 64 * 1024;

/// Kopiert `laenge` Bytes vom User-Zeiger `user_ptr` in einen frischen
/// Kernel-Vec — ABER erst nach voller Prüfung: jede berührte Page muss
/// gemappt UND USER_ACCESSIBLE sein. Panickt NIE (Dauerregel I).
///
/// (Einschränkung fürs Protokoll: In einem präemptiven System könnte ein
/// anderer Thread die Page zwischen Prüfung und Kopie unmappen — TOCTOU. Bei
/// uns läuft alles single-threaded und synchron, deshalb ist die Prüfung-
/// dann-Kopie hier korrekt; mit echten Prozessen kommt die Absicherung dazu.)
pub fn copy_in(user_ptr: u64, laenge: usize) -> Result<Vec<u8>, CopyFehler> {
    if laenge == 0 {
        return Ok(Vec::new());
    }
    if laenge > COPY_IN_MAX {
        return Err(CopyFehler::ZuGross);
    }
    // Länge darf den Adressraum nicht überlaufen.
    let ende = (user_ptr as usize)
        .checked_add(laenge)
        .ok_or(CopyFehler::Ueberlauf)?;

    // JEDE berührte Page prüfen: gemappt + USER_ACCESSIBLE.
    let mut seite = user_ptr as usize & !0xfff;
    while seite < ende {
        match memory::seiten_flags(VirtAddr::new(seite as u64)) {
            Some(flags) if flags.contains(PageTableFlags::USER_ACCESSIBLE) => {}
            Some(_) => return Err(CopyFehler::KernelSpeicher),
            None => return Err(CopyFehler::NichtGemappt),
        }
        seite += 4096;
    }

    // Jetzt ist das Lesen sicher (alle Pages gemappt + user-zugänglich).
    let mut ziel = alloc::vec![0u8; laenge];
    // unsafe: Der gesamte Bereich ist geprüft gemappt — kein Fault möglich.
    unsafe {
        core::ptr::copy_nonoverlapping(user_ptr as *const u8, ziel.as_mut_ptr(), laenge);
    }
    Ok(ziel)
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
// General-Register (baut damit einen `TrapFrame` auf dem Kernel-Trap-Stack,
// RSP0), ruft den Rust-Dispatcher mit einem Zeiger darauf, stellt die Register
// wieder her und kehrt per `iretq` nach Ring 3 zurück.

/// Das vollständige, gesicherte User-Register-Bild auf dem Trap-Stack.
/// Reihenfolge = umgekehrte Push-Reihenfolge des Einstiegs; danach folgt der
/// von der CPU gepushte Interrupt-Rahmen (rip/cs/rflags/rsp/ss).
#[repr(C)]
struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    // ab hier: von der CPU beim Trap gepusht
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

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
    "call syscall_dispatch",     // Rust-Dispatcher (liest/schreibt den Frame)
    // Register wiederherstellen (rax trägt jetzt den Rückgabewert).
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp", "pop rdi", "pop rsi",
    "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

extern "C" {
    fn syscall_entry();
}

/// Adresse des Syscall-Einstiegs (für die IDT-Registrierung in interrupts.rs).
pub fn syscall_handler_adresse() -> u64 {
    syscall_entry as *const () as u64
}

/// Der Rust-Dispatcher: bekommt einen Zeiger auf den gesicherten User-Kontext,
/// liest die Syscall-Nummer (rax) + Argumente und schreibt den Rückgabewert
/// nach rax zurück. Bei `exit` biegt er den Interrupt-Rahmen auf den Kernel-
/// Recovery-Punkt um (zurück in den Kernel statt nach Ring 3).
#[no_mangle]
extern "C" fn syscall_dispatch(frame: *mut TrapFrame) {
    // unsafe: `frame` zeigt auf unseren eben gesicherten Register-Block auf
    // dem RSP0-Stack — gültig für die Dauer des Handlers.
    let f = unsafe { &mut *frame };
    match f.rax {
        SYS_DEBUG_PRINT => {
            // debug_print(ptr = rdi, len = rsi) — copy-in, dann seriell.
            match copy_in(f.rdi, f.rsi as usize) {
                Ok(bytes) => {
                    for &b in &bytes {
                        serial_print!("{}", b as char);
                    }
                    f.rax = bytes.len() as u64; // Rückgabe: gedruckte Bytes
                }
                Err(fehler) => {
                    serial_println!("[syscall] debug_print: ungueltiger Puffer ({:?})", fehler);
                    f.rax = u64::MAX; // Fehler-Kennung
                }
            }
        }
        SYS_EXIT => {
            // Zurück in den Kernel: den CPU-Interrupt-Rahmen auf den
            // LANDEPLATZ umbiegen (Ring 0). Der iretq am Ende des Handlers
            // springt dann dorthin statt nach Ring 3; der Landeplatz stellt
            // den gesicherten Kernel-Kontext wieder her.
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
/// WARUM iretq (und nicht sysretq)? `iretq` ist der GENERISCHE Sprung-Befehl:
/// Er lädt CS:RIP, RFLAGS UND SS:RSP aus einem Stack-Rahmen — wir bauen den
/// Rahmen, den ein Trap aus Ring 3 hinterlassen HÄTTE, und „springen dorthin".
/// `sysretq` bräuchte MSR-Einrichtung und eine bestimmte Segment-Anordnung.
fn nach_ring3(user_entry: u64, user_stack: u64) {
    let ucs = gdt::user_code_selektor();
    let uds = gdt::user_data_selektor();
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
}

// ---------------------------------------------------------------------------
// Die beiden Testprogramme (hand-assemblierter Ring-3-Maschinencode)
// ---------------------------------------------------------------------------

/// Baut das ERFOLGS-Programm: druckt die Nachricht per Syscall, dann exit.
/// Position-unabhängig ist es NICHT nötig — wir kennen die Ziel-Adresse
/// (USER_BASIS) und setzen absolute Adressen ein.
fn programm_erfolg(nachricht_va: u64, laenge: usize) -> Vec<u8> {
    let mut p = Vec::new();
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

/// Mappt die beiden User-Pages (Code + Stack) und schreibt `code` (und, wenn
/// gesetzt, die Nachricht bei NACHRICHT_OFFSET) hinein.
fn user_pages_aufsetzen(code: &[u8], nachricht: Option<&[u8]>) {
    let code_page = Page::containing_address(VirtAddr::new(USER_BASIS));
    let stack_page = Page::containing_address(VirtAddr::new(USER_BASIS + 0x1000));
    memory::map_page_benutzer(code_page).expect("User-Code-Page mappen");
    memory::map_page_benutzer(stack_page).expect("User-Stack-Page mappen");
    // unsafe: die Pages sind frisch gemappt (present, writable) — der Kernel
    // (Ring 0) darf sie beschreiben.
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), USER_BASIS as *mut u8, code.len());
        if let Some(n) = nachricht {
            core::ptr::copy_nonoverlapping(
                n.as_ptr(),
                (USER_BASIS + NACHRICHT_OFFSET) as *mut u8,
                n.len(),
            );
        }
    }
}

/// Gibt die beiden User-Pages wieder frei (Frames zurück in den Allocator).
fn user_pages_abraeumen() {
    for va in [USER_BASIS, USER_BASIS + 0x1000] {
        let page = Page::containing_address(VirtAddr::new(va));
        if let Ok(frame) = memory::unmap_page(page) {
            // unsafe: die Page ist eben ausgehängt, der Frame nirgends mehr in
            // Benutzung.
            unsafe { memory::frame_freigeben(frame) };
        }
    }
}

/// TEST 1 (Erfolg): Ring-3-Code druckt „Hallo aus Ring 3!" per Syscall und
/// kehrt sauber zurück. Der Kernel läuft danach normal weiter.
pub fn ring3_erfolg() {
    let code = programm_erfolg(USER_BASIS + NACHRICHT_OFFSET, NACHRICHT.len());
    user_pages_aufsetzen(&code, Some(NACHRICHT));
    serial_println!(
        "[ring3] Springe nach Ring 3 (Einsprung {:#x}, Stack {:#x}) ...",
        USER_BASIS,
        USER_STACK_TOP
    );
    nach_ring3(USER_BASIS, USER_STACK_TOP);
    serial_println!("[ring3] Sauber zurueck im Kernel (Ring 0) — System laeuft weiter.");
    user_pages_abraeumen();
}

/// TEST 2 (Absturz): Ring-3-Code greift auf eine KERNEL-Adresse zu. Erwartung:
/// Page Fault mit klarer Meldung „aus User-Mode", und der KERNEL LEBT WEITER.
pub fn ring3_absturz() {
    // Eine garantiert gemappte KERNEL-Adresse (Heap-Start, U=0).
    let kernel_adresse = crate::allocator::HEAP_START as u64;
    let code = programm_absturz(kernel_adresse);
    user_pages_aufsetzen(&code, None);
    serial_println!(
        "[ring3] Ring-3-Code greift jetzt VERBOTEN auf Kernel-Adresse {:#x} zu ...",
        kernel_adresse
    );
    nach_ring3(USER_BASIS, USER_STACK_TOP);
    serial_println!(
        "[ring3] Der Absturz wurde aufgefangen — der KERNEL LEBT WEITER. (Genau das ist neu!)"
    );
    user_pages_abraeumen();
}

// ---------------------------------------------------------------------------
// Tests — Dauerregel (I): copy_in gegen ungültige Adressen (nie panicken)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Eine eigene User-Test-Page, getrennt von der Ring-3-Code-Page.
    const TEST_VA: u64 = USER_BASIS + 0x0010_0000; // +1 MiB

    /// copy_in gegen ungültige Eingaben: Kernel-Adresse, ungemappt und
    /// Längen-Überlauf — ALLE liefern Fehler, KEINE panickt.
    #[test_case]
    fn test_copy_in_ungueltig() {
        // Kernel-Bereich (Heap, U=0) -> KernelSpeicher.
        assert_eq!(
            copy_in(crate::allocator::HEAP_START as u64, 8),
            Err(CopyFehler::KernelSpeicher)
        );
        // Nicht gemappte (niedrige, freie) Adresse -> NichtGemappt.
        assert_eq!(copy_in(0x1_0000, 8), Err(CopyFehler::NichtGemappt));
        // Länge läuft über den Adressraum hinaus -> Ueberlauf.
        assert_eq!(copy_in(u64::MAX - 3, 16), Err(CopyFehler::Ueberlauf));
        // Absurd große Länge -> ZuGross.
        assert_eq!(copy_in(0x1_0000, 8 * 1024 * 1024), Err(CopyFehler::ZuGross));
        // Länge 0 ist immer ok (leer), egal welche Adresse.
        assert_eq!(copy_in(0, 0), Ok(Vec::new()));
        assert_eq!(copy_in(crate::allocator::HEAP_START as u64, 0), Ok(Vec::new()));
    }

    /// copy_in gegen eine GÜLTIGE User-Page: liefert die Daten; ein Puffer,
    /// der über die gemappte Page hinausragt, wird abgelehnt.
    #[test_case]
    fn test_copy_in_gueltig() {
        let page = Page::containing_address(VirtAddr::new(TEST_VA));
        memory::map_page_benutzer(page).expect("Test-User-Page mappen");
        let daten = b"copy-in funktioniert";
        // unsafe: frisch gemappte, beschreibbare Page.
        unsafe {
            core::ptr::copy_nonoverlapping(daten.as_ptr(), TEST_VA as *mut u8, daten.len());
        }
        assert_eq!(copy_in(TEST_VA, daten.len()), Ok(daten.to_vec()));

        // Ein Puffer, der in die (ungemappte) Folge-Page ragt, scheitert
        // sauber — der geprüfte Teil wird NICHT halb kopiert.
        assert_eq!(copy_in(TEST_VA + 4090, 32), Err(CopyFehler::NichtGemappt));

        let frame = memory::unmap_page(page).expect("Test-User-Page aushaengen");
        // unsafe: eben ausgehängt.
        unsafe { memory::frame_freigeben(frame) };
    }
}
