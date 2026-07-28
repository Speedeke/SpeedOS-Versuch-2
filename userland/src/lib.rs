// libspeed — Die Standard-Bibliothek der SpeedOS-Programme
//               (Serie 6, Teil 5, Aufgabe 3)
//
// ==========================================================================
// DIE ANDERE SEITE DER GRENZE
//
// Jede andere Datei dieses Projekts ist Kernel. Diese hier ist es NICHT —
// und das ist ihr Sinn. libspeed kennt von SpeedOS ausschliesslich das, was
// in docs/syscalls.md steht: 23 Nummern, ihre Argumente, ihre Rueckgaben.
// Kein `use speed_os::...`, kein gemeinsamer Typ, keine gemeinsame Datei.
// Waere hier auch nur EINE Kernel-Abhaengigkeit, waere das kein User-Space,
// sondern Kernel-Code in einem anderen Verzeichnis.
//
// Deshalb stehen die Konstanten unten NOCH EINMAL da, obwohl sie im Kernel
// schon existieren. Das ist keine vergessene Vereinheitlichung, sondern der
// Punkt: Eine ABI ist ein VERTRAG, kein geteilter Header. Beide Seiten
// schreiben ihn getrennt auf, und der Test (tests/syscalls.rs,
// tests/programme.rs) prueft, dass sie dasselbe meinen. Genau so, wie ein
// Linux-Programm auch nicht die Kernel-Quellen einbindet.
//
// ==========================================================================
// DIE AUFRUF-KONVENTION (docs/syscalls.md)
//
//   Eingabe:   rax = Nummer, rdi/rsi/rdx/r10 = Argumente 0..3
//   Ausgabe:   rax = FEHLERCODE (0 = Ok), rdx = ERGEBNIS
//
// Alle uebrigen Register bleiben erhalten — der Kernel sichert sie im
// TrapFrame und stellt sie wieder her. Deshalb braucht das `asm!` unten
// keine Clobber-Liste ausser den beiden Ausgabe-Registern.
//
// Puffer und Pfade sind IMMER (Zeiger, Laenge), NIE nullterminiert. Ein
// SpeedOS-Programm hat deshalb keine C-Strings; es reicht `&str`/`&[u8]`
// direkt durch.
// ==========================================================================

#![no_std]
#![allow(clippy::missing_safety_doc)]

use core::fmt::{self, Write};

// ---------------------------------------------------------------------------
// Die Syscall-Nummern (Abschrift der ABI aus docs/syscalls.md)
// ---------------------------------------------------------------------------

pub const SYS_EXIT: u64 = 0;
pub const SYS_YIELD: u64 = 1;
pub const SYS_GETPID: u64 = 2;
pub const SYS_SCHREIBE: u64 = 3;
pub const SYS_SCHLAFE: u64 = 4;
pub const SYS_ZEIT_JETZT: u64 = 5;
pub const SYS_ZEIT_EPOCHE: u64 = 6;

pub const SYS_OEFFNE: u64 = 16;
pub const SYS_LESE_AT: u64 = 17;
pub const SYS_SCHREIBE_AT: u64 = 18;
pub const SYS_SCHLIESSE: u64 = 19;
pub const SYS_STAT: u64 = 20;
pub const SYS_LISTE: u64 = 21;
pub const SYS_LOESCHE: u64 = 22;
pub const SYS_UMBENENNE: u64 = 23;
pub const SYS_MKDIR: u64 = 24;

pub const SYS_SOCKET: u64 = 32;
pub const SYS_VERBINDE: u64 = 33;
pub const SYS_SENDE: u64 = 34;
pub const SYS_EMPFANGE: u64 = 35;
pub const SYS_AUFLOESEN: u64 = 36;
pub const SYS_SOCKET_ZUSTAND: u64 = 37;

/// Die drei reservierten Handles.
pub const AUSGABE: u64 = 1;
/// Nur seriell — geht am Bildschirm vorbei (fuer Protokolle und Fehler).
pub const DIAGNOSE: u64 = 2;

/// Modus-Bits fuer `oeffne`.
pub const LESEN: u64 = 1;
pub const SCHREIBEN: u64 = 2;
pub const ANLEGEN: u64 = 4;
pub const ABSCHNEIDEN: u64 = 8;

/// Socket-Typen.
pub const TCP: u64 = 0;
pub const UDP: u64 = 1;

/// Socket-Zustaende (`socket_zustand`).
pub const Z_NEU: u64 = 0;
pub const Z_VERBINDET: u64 = 1;
pub const Z_VERBUNDEN: u64 = 2;
pub const Z_PEER_HAT_GESCHLOSSEN: u64 = 3;
pub const Z_GESCHLOSSEN: u64 = 4;
pub const Z_LAUSCHT: u64 = 5;

/// Groesse eines `stat`-Ergebnisses in Bytes.
pub const STAT_BYTES: usize = 32;

// ---------------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------------

/// Ein Fehlercode des Kernels. Die Zahlen sind ABI (docs/syscalls.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fehler(pub u64);

impl Fehler {
    pub const UNBEKANNTER_SYSCALL: Fehler = Fehler(1);
    pub const UNGUELTIGES_ARGUMENT: Fehler = Fehler(2);
    pub const UNGUELTIGER_ZEIGER: Fehler = Fehler(3);
    pub const ZU_GROSS: Fehler = Fehler(4);
    pub const UNGUELTIGER_HANDLE: Fehler = Fehler(5);
    pub const NICHT_GEFUNDEN: Fehler = Fehler(8);
    pub const EXISTIERT_BEREITS: Fehler = Fehler(9);
    pub const KEINE_DATEI: Fehler = Fehler(11);
    pub const UNGUELTIGER_PFAD: Fehler = Fehler(13);
    pub const KEIN_PLATZ: Fehler = Fehler(14);
    pub const NUR_LESEN: Fehler = Fehler(15);
    pub const ZEITUEBERSCHREITUNG: Fehler = Fehler(17);
    pub const NICHT_KONFIGURIERT: Fehler = Fehler(18);
    pub const ABGEBROCHEN: Fehler = Fehler(22);

    /// Deutsche Klartext-Meldung. Bewusst hier und nicht im Kernel: Der
    /// Kernel liefert eine ZAHL ueber die Grenze, die Uebersetzung in Text
    /// ist Sache des Programms (bei einem echten System waere das die Stelle
    /// fuer Mehrsprachigkeit).
    pub fn text(self) -> &'static str {
        match self.0 {
            0 => "kein Fehler",
            1 => "unbekannter Syscall",
            2 => "ungueltiges Argument",
            3 => "ungueltiger Zeiger",
            4 => "zu gross",
            5 => "ungueltiges Handle",
            6 => "keine Handles frei",
            7 => "Handle vom falschen Typ",
            8 => "nicht gefunden",
            9 => "existiert bereits",
            10 => "kein Verzeichnis",
            11 => "keine Datei",
            12 => "Verzeichnis nicht leer",
            13 => "ungueltiger Pfad",
            14 => "kein Platz mehr",
            15 => "nur lesbar",
            16 => "Geraetefehler",
            17 => "Zeitueberschreitung",
            18 => "nicht konfiguriert",
            19 => "kein Geraet",
            20 => "nicht verbunden",
            21 => "bereits verbunden",
            22 => "Verbindung abgebrochen",
            23 => "nicht unterstuetzt",
            24 => "Ressource belegt",
            _ => "unbekannter Fehlercode",
        }
    }
}

/// Das Ergebnis eines Syscalls.
pub type Ergebnis = Result<u64, Fehler>;

// ---------------------------------------------------------------------------
// DER SYSCALL SELBST
// ---------------------------------------------------------------------------

/// Loest `int 0x80` aus und uebersetzt (rax, rdx) in ein `Result`.
///
/// # Safety
/// Der Aufrufer muss dafuer sorgen, dass Zeiger-Argumente auf gueltigen,
/// eigenen Speicher zeigen. Tut er es nicht, ist das KEIN undefiniertes
/// Verhalten — der Kernel prueft jeden Zeiger selbst (Dauerregel I) und
/// antwortet mit `UNGUELTIGER_ZEIGER`. Diese Funktion ist trotzdem `unsafe`,
/// weil ein Syscall in den eigenen Speicher schreiben kann (copy-out).
#[inline]
unsafe fn syscall(nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> Ergebnis {
    let fehler: u64;
    let ergebnis: u64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") nummer => fehler,
        in("rdi") a0,
        in("rsi") a1,
        // rdx traegt Argument 2 hinein und das ERGEBNIS heraus.
        inlateout("rdx") a2 => ergebnis,
        in("r10") a3,
        // Kein `nomem`: Ein Syscall darf in unsere Puffer schreiben.
        // Keine weiteren Clobber: Der Kernel stellt alle uebrigen Register
        // aus dem TrapFrame wieder her.
    );
    if fehler == 0 {
        Ok(ergebnis)
    } else {
        Err(Fehler(fehler))
    }
}

// ---------------------------------------------------------------------------
// Gruppe 0: Prozess und Ausgabe
// ---------------------------------------------------------------------------

/// Beendet den Prozess. Kehrt nie zurueck.
pub fn beenden(code: u64) -> ! {
    // Sicherheit: keine Zeiger im Spiel.
    unsafe {
        let _ = syscall(SYS_EXIT, code, 0, 0, 0);
    }
    // Der Kernel kommt nie zurueck. Falls doch (Scheduler aus, Einzelschuss-
    // Pfad), nicht weiterlaufen — dann waere der Zustand unklar.
    loop {
        core::hint::spin_loop();
    }
}

/// Gibt die Zeitscheibe freiwillig ab.
pub fn abgeben() {
    unsafe {
        let _ = syscall(SYS_YIELD, 0, 0, 0, 0);
    }
}

/// Die eigene Prozess-Nummer.
pub fn pid() -> u64 {
    unsafe { syscall(SYS_GETPID, 0, 0, 0, 0).unwrap_or(0) }
}

/// Schlaeft `ms` Millisekunden (der Prozess bekommt solange keine CPU).
pub fn schlafe(ms: u64) {
    unsafe {
        let _ = syscall(SYS_SCHLAFE, ms, 0, 0, 0);
    }
}

/// Millisekunden seit dem Systemstart (monoton).
pub fn zeit_jetzt() -> u64 {
    unsafe { syscall(SYS_ZEIT_JETZT, 0, 0, 0, 0).unwrap_or(0) }
}

/// Sekunden seit dem 1.1.2000 (echte Uhr).
pub fn zeit_epoche() -> u64 {
    unsafe { syscall(SYS_ZEIT_EPOCHE, 0, 0, 0, 0).unwrap_or(0) }
}

/// Schreibt Bytes auf ein Handle. Liefert die uebernommene Anzahl.
pub fn schreibe(handle: u64, daten: &[u8]) -> Ergebnis {
    unsafe {
        syscall(
            SYS_SCHREIBE,
            handle,
            daten.as_ptr() as u64,
            daten.len() as u64,
            0,
        )
    }
}

// ---------------------------------------------------------------------------
// Gruppe 1: Dateien
// ---------------------------------------------------------------------------

/// Oeffnet eine Datei und liefert ein Handle. `modus` ist eine Kombination
/// aus `LESEN`, `SCHREIBEN`, `ANLEGEN`, `ABSCHNEIDEN`.
pub fn oeffne(pfad: &str, modus: u64) -> Ergebnis {
    unsafe {
        syscall(
            SYS_OEFFNE,
            pfad.as_ptr() as u64,
            pfad.len() as u64,
            modus,
            0,
        )
    }
}

/// Liest ab `offset` in den Puffer. Liefert die gelesene Anzahl
/// (0 = Dateiende).
pub fn lese_at(handle: u64, offset: u64, ziel: &mut [u8]) -> Ergebnis {
    unsafe {
        syscall(
            SYS_LESE_AT,
            handle,
            offset,
            ziel.as_mut_ptr() as u64,
            ziel.len() as u64,
        )
    }
}

/// Schreibt ab `offset`. Liefert die geschriebene Anzahl.
pub fn schreibe_at(handle: u64, offset: u64, daten: &[u8]) -> Ergebnis {
    unsafe {
        syscall(
            SYS_SCHREIBE_AT,
            handle,
            offset,
            daten.as_ptr() as u64,
            daten.len() as u64,
        )
    }
}

/// Schliesst ein Handle (Datei ODER Socket).
pub fn schliesse(handle: u64) -> Ergebnis {
    unsafe { syscall(SYS_SCHLIESSE, handle, 0, 0, 0) }
}

/// Metadaten einer Datei (die 32 Byte, die `stat` liefert).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Metadaten {
    /// 0 = Datei, 1 = Verzeichnis.
    pub typ: u64,
    pub groesse: u64,
    pub erstellt: u64,
    pub geaendert: u64,
}

impl Metadaten {
    pub fn ist_verzeichnis(&self) -> bool {
        self.typ == 1
    }
}

/// Fragt die Metadaten eines Pfades ab.
pub fn stat(pfad: &str) -> Result<Metadaten, Fehler> {
    let mut roh = [0u8; STAT_BYTES];
    unsafe {
        syscall(
            SYS_STAT,
            pfad.as_ptr() as u64,
            pfad.len() as u64,
            roh.as_mut_ptr() as u64,
            0,
        )?;
    }
    let zahl = |i: usize| {
        let mut wert = [0u8; 8];
        wert.copy_from_slice(&roh[i * 8..i * 8 + 8]);
        u64::from_le_bytes(wert)
    };
    Ok(Metadaten {
        typ: zahl(0),
        groesse: zahl(1),
        erstellt: zahl(2),
        geaendert: zahl(3),
    })
}

/// Loescht eine Datei oder ein LEERES Verzeichnis.
pub fn loesche(pfad: &str) -> Ergebnis {
    unsafe { syscall(SYS_LOESCHE, pfad.as_ptr() as u64, pfad.len() as u64, 0, 0) }
}

/// Benennt um / verschiebt (innerhalb eines Mounts).
pub fn umbenenne(von: &str, nach: &str) -> Ergebnis {
    unsafe {
        syscall(
            SYS_UMBENENNE,
            von.as_ptr() as u64,
            von.len() as u64,
            nach.as_ptr() as u64,
            nach.len() as u64,
        )
    }
}

/// Legt ein Verzeichnis an.
pub fn mkdir(pfad: &str) -> Ergebnis {
    unsafe { syscall(SYS_MKDIR, pfad.as_ptr() as u64, pfad.len() as u64, 0, 0) }
}

/// Zaehlt die Eintraege eines Verzeichnisses (ohne sie zu holen).
pub fn eintraege_zaehlen(pfad: &str) -> Ergebnis {
    unsafe {
        syscall(
            SYS_LISTE,
            pfad.as_ptr() as u64,
            pfad.len() as u64,
            0,
            0,
        )
    }
}

// ---------------------------------------------------------------------------
// Gruppe 2: Netz
// ---------------------------------------------------------------------------

/// Oeffnet einen Socket (`TCP` oder `UDP`) und liefert sein Handle.
pub fn socket(typ: u64) -> Ergebnis {
    unsafe { syscall(SYS_SOCKET, typ, 0, 0, 0) }
}

/// Verbindet einen TCP-Socket. BLOCKIEREND mit 8-Sekunden-Frist: Der Kernel
/// treibt den Handshake selbst voran (siehe docs/syscalls.md) — ein
/// Ring-3-Programm koennte das nicht.
pub fn verbinde(handle: u64, ip: u32, port: u16) -> Ergebnis {
    unsafe { syscall(SYS_VERBINDE, handle, ip as u64, port as u64, 0) }
}

/// Sendet Bytes. Die uebernommene Anzahl kann KLEINER als `daten.len()`
/// sein (der Sendepuffer ist endlich) — dann nochmal rufen.
pub fn sende(handle: u64, daten: &[u8]) -> Ergebnis {
    unsafe {
        syscall(
            SYS_SENDE,
            handle,
            daten.as_ptr() as u64,
            daten.len() as u64,
            0,
        )
    }
}

/// Holt empfangene Bytes ab. NICHT blockierend: 0 heisst „noch nichts da",
/// nicht „Ende". Fuer das Ende fragt man `socket_zustand`.
pub fn empfange(handle: u64, ziel: &mut [u8]) -> Ergebnis {
    unsafe {
        syscall(
            SYS_EMPFANGE,
            handle,
            ziel.as_mut_ptr() as u64,
            ziel.len() as u64,
            0,
        )
    }
}

/// Der Zustand eines Sockets (`Z_*`).
pub fn socket_zustand(handle: u64) -> Ergebnis {
    unsafe { syscall(SYS_SOCKET_ZUSTAND, handle, 0, 0, 0) }
}

/// Loest einen Namen per DNS auf. Liefert die IPv4 als u32 (a.b.c.d in den
/// Bytes 3..0). Kann Sekunden dauern (bis zu drei Versuche).
pub fn aufloesen(name: &str) -> Result<u32, Fehler> {
    let ip = unsafe {
        syscall(
            SYS_AUFLOESEN,
            name.as_ptr() as u64,
            name.len() as u64,
            0,
            0,
        )?
    };
    Ok(ip as u32)
}

/// Baut eine IPv4 aus vier Zahlen (fuer Adressen ohne DNS).
pub fn ipv4(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | d as u32
}

// ---------------------------------------------------------------------------
// Ausgabe: print!/println! ohne Betriebssystem-Bibliothek
// ---------------------------------------------------------------------------

/// Ein `fmt::Write`-Ziel, das auf ein Handle schreibt.
///
/// PUFFERT bewusst: Ohne Puffer wuerde jedes `{}` in einem Format-String
/// einen eigenen Syscall ausloesen — und jeder Syscall ist ein
/// Privilegienwechsel. 256 Byte sind ein guter Kompromiss auf einem
/// 64-KiB-Stack.
pub struct Ausgabe {
    handle: u64,
    puffer: [u8; 256],
    belegt: usize,
}

impl Ausgabe {
    pub fn neu(handle: u64) -> Ausgabe {
        Ausgabe {
            handle,
            puffer: [0; 256],
            belegt: 0,
        }
    }

    /// Schiebt den Puffer raus.
    pub fn leeren(&mut self) {
        if self.belegt > 0 {
            let _ = schreibe(self.handle, &self.puffer[..self.belegt]);
            self.belegt = 0;
        }
    }
}

impl Drop for Ausgabe {
    fn drop(&mut self) {
        self.leeren();
    }
}

impl Write for Ausgabe {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for stueck in text.as_bytes() {
            if self.belegt == self.puffer.len() {
                self.leeren();
            }
            self.puffer[self.belegt] = *stueck;
            self.belegt += 1;
        }
        Ok(())
    }
}

/// Interne Hilfe der Makros.
#[doc(hidden)]
pub fn _drucken(handle: u64, args: fmt::Arguments) {
    let mut ziel = Ausgabe::neu(handle);
    // Ein Schreibfehler (volle Platte, geschlossenes Handle) darf ein
    // Programm nicht abstuerzen lassen — Ausgabe ist Beiwerk.
    let _ = ziel.write_fmt(args);
    ziel.leeren();
}

/// Schreibt auf die Standard-Ausgabe (Bildschirm UND seriell).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_drucken($crate::AUSGABE, format_args!($($arg)*)));
}

/// Wie `print!`, mit Zeilenumbruch.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Schreibt auf den DIAGNOSE-Kanal (nur seriell, kein Bildschirm).
#[macro_export]
macro_rules! diagnose {
    ($($arg:tt)*) => ($crate::_drucken($crate::DIAGNOSE, format_args!($($arg)*)));
}

/// Wie `diagnose!`, mit Zeilenumbruch.
#[macro_export]
macro_rules! diagnoseln {
    () => ($crate::diagnose!("\n"));
    ($($arg:tt)*) => ($crate::diagnose!("{}\n", format_args!($($arg)*)));
}

// ---------------------------------------------------------------------------
// Argumente
// ---------------------------------------------------------------------------

/// Ein Argument, so wie der Kernel es hinlegt: (Zeiger, Laenge).
/// 16 Byte, `repr(C)` — das ist ABI (docs/syscalls.md §9).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArgEintrag {
    pub zeiger: u64,
    pub laenge: u64,
}

/// Die Argumente des Programms. `argv[0]` ist der Programmname.
pub struct Argumente {
    eintraege: &'static [ArgEintrag],
}

impl Argumente {
    /// # Safety
    /// `argv` muss auf `argc` gueltige Eintraege zeigen — das tut es, weil
    /// der Kernel sie beim Start selbst auf unseren Stack gelegt hat.
    unsafe fn neu(argc: u64, argv: *const ArgEintrag) -> Argumente {
        if argc == 0 || argv.is_null() {
            Argumente { eintraege: &[] }
        } else {
            Argumente {
                eintraege: core::slice::from_raw_parts(argv, argc as usize),
            }
        }
    }

    /// Wie viele Argumente gibt es (inklusive Programmname)?
    pub fn anzahl(&self) -> usize {
        self.eintraege.len()
    }

    /// Das i-te Argument als Bytes.
    pub fn roh(&self, index: usize) -> Option<&'static [u8]> {
        let eintrag = self.eintraege.get(index)?;
        if eintrag.laenge == 0 {
            return Some(&[]);
        }
        // Sicherheit: Der Kernel hat diesen Bereich beim Start in unseren
        // eigenen Stack geschrieben; er ist gemappt und gehoert uns.
        Some(unsafe {
            core::slice::from_raw_parts(eintrag.zeiger as *const u8, eintrag.laenge as usize)
        })
    }

    /// Das i-te Argument als Text (None, wenn es kein gueltiges UTF-8 ist).
    pub fn get(&self, index: usize) -> Option<&'static str> {
        core::str::from_utf8(self.roh(index)?).ok()
    }

    /// Der Programmname (`argv[0]`), oder ein Ersatz.
    pub fn programm(&self) -> &'static str {
        self.get(0).unwrap_or("programm")
    }
}

// ---------------------------------------------------------------------------
// Der Einstieg
// ---------------------------------------------------------------------------

/// Wird vom `hauptprogramm!`-Makro gerufen: Argumente einpacken, `haupt`
/// laufen lassen, Rueckgabewert als Exit-Code weitergeben.
///
/// # Safety
/// Nur aus dem generierten `_start` heraus aufzurufen.
pub unsafe fn lauf(argc: u64, argv: *const ArgEintrag, haupt: fn(&Argumente) -> i32) -> ! {
    let argumente = Argumente::neu(argc, argv);
    let code = haupt(&argumente);
    beenden(code as u64)
}

/// Erzeugt den Einsprungpunkt `_start` und verdrahtet ihn mit `$haupt`.
///
/// WAS DER ASSEMBLER-TEIL TUT UND WARUM:
///
///  * `xor ebp, ebp` — den Rahmenzeiger auf 0. Damit endet jeder
///    Stack-Rueckverfolger sauber, statt in den Argument-Bereich zu laufen.
///  * `and rsp, -16` — den Stack 16-ausrichten. Der Kernel legt uns rsp
///    schon ausgerichtet hin, aber sich darauf zu VERLASSEN waere eine
///    unnoetige Kopplung an die Kernel-Version.
///  * `call` — und genau deshalb ein `call` und kein `jmp`: Die
///    C-Aufrufkonvention erwartet beim Betreten einer Funktion rsp ≡ 8
///    (mod 16), weil normalerweise eine Ruecksprungadresse draufliegt. Der
///    `call` legt sie hin und stellt die Ausrichtung damit her. Ein `jmp`
///    wuerde still eine falsch ausgerichtete Funktion betreten.
///  * `ud2` — unerreichbar. `lauf` endet in `exit`; kaeme die CPU je
///    hierher, waere etwas grundlegend falsch, und dann ist ein sofortiger
///    Absturz ehrlicher als weiterzulaufen.
#[macro_export]
macro_rules! hauptprogramm {
    ($haupt:path) => {
        ::core::arch::global_asm!(
            ".globl _start",
            ".section .text._start,\"ax\",@progbits",
            "_start:",
            "xor ebp, ebp",
            "and rsp, -16",
            "call speedos_start",
            "ud2",
            ".previous",
        );

        /// Der erste Rust-Code des Prozesses. rdi = argc, rsi = argv
        /// (so legt der Kernel den Start-TrapFrame an).
        ///
        /// Nicht `unsafe`, obwohl ein roher Zeiger hineinkommt: Diese
        /// Funktion wird NIE aus Rust gerufen, sondern ausschliesslich vom
        /// `_start`-Assembler oben — und dessen Argumente stammen aus dem
        /// Start-TrapFrame, den der Kernel gebaut hat.
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        #[no_mangle]
        pub extern "C" fn speedos_start(argc: u64, argv: *const $crate::ArgEintrag) -> ! {
            // Sicherheit: Diese Werte kommen aus dem Start-TrapFrame, den
            // `prozess::neu_ring3_mit_start` gebaut hat.
            unsafe { $crate::lauf(argc, argv, $haupt) }
        }
    };
}

// ---------------------------------------------------------------------------
// Panik
// ---------------------------------------------------------------------------

/// Was passiert, wenn ein SpeedOS-Programm panickt.
///
/// Die Meldung geht auf den DIAGNOSE-Kanal (nur seriell): Eine Panik ist
/// eine Entwickler-Information, kein Programm-Ergebnis — sie soll die
/// Ausgabe des Programms nicht verunreinigen. Danach `exit(101)`, derselbe
/// Code, den auch Rust-Programme mit std benutzen.
///
/// Was hier NICHT passiert: abwickeln. `panic = "abort"` ist gesetzt, es
/// gibt keine Landing Pads und keine Personality-Funktion.
#[panic_handler]
fn panik(info: &core::panic::PanicInfo) -> ! {
    diagnoseln!("[Panik] {}", info);
    beenden(101)
}
