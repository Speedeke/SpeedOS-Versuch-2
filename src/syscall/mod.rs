// syscall/mod.rs — DIE ABI VON SPEEDOS (Serie 6, Teil 4)
//
// Hier wird aus „der Kernel tut alles selbst" endgültig „der Kernel wird
// GEBETEN". Die vollständige Tabelle mit allen Nummern, Argumenten, Rückgaben
// und Fehlern steht in **docs/syscalls.md** — und ab jetzt gilt: Eine Änderung
// an dieser Tabelle ist eine bewusste ABI-ÄNDERUNG, nicht ein Refactoring.
//
// ==========================================================================
// DIE AUFRUF-KONVENTION
//
//   Eingabe:   rax = Syscall-Nummer
//              rdi, rsi, rdx, r10 = Argumente 0..3
//   Ausgabe:   rax = FEHLERCODE (0 = Erfolg)
//              rdx = ERGEBNIS   (Handle, Byte-Zahl, Zeitwert, ...)
//
// Zwei Register für die Rückgabe (statt Linux' „negativer errno") sind hier
// die klarere Wahl: Ein Ergebnis darf jeden u64-Wert annehmen (eine Uhrzeit,
// eine IP-Adresse), ohne dass ein Bit für „ist das ein Fehler?" reserviert
// werden muss. Der Preis ist ein Register mehr — geschenkt.
//
// ZEIGER UND LÄNGEN, NIE NULLTERMINIERUNG: Jeder Puffer und jeder Pfad wird
// als (Zeiger, Länge) übergeben. Der Kernel SUCHT NIE ein Terminator-Byte im
// User-Speicher — das wäre ein Lesevorgang unbekannter Länge in fremdem
// Speicher, also genau die Sorte unbegrenzter Zugriff, die Dauerregel I
// verbietet. Pfade sind zusätzlich auf MAX_PFAD Bytes begrenzt.
//
// KEINE KERNEL-TYPEN NACH AUSSEN: `Fehler` unten ist der EINZIGE Fehler-Typ,
// den ein Prozess sieht. FsFehler, IoFehler, SocketFehler, DnsFehler und
// CopyFehler werden darauf ABGEBILDET (`von_fs`, `von_io`, ...). Ein Prozess
// erfährt damit nie, welches Dateisystem oder welcher Treiber unter ihm liegt.
//
// PANICKT NIE. Unbekannte Nummer, kaputte Zeiger, fremde Handles, absurde
// Längen — alles wird ein Fehlercode. Ein Programm darf Unsinn übergeben.
// ==========================================================================
//
// ==========================================================================
// DIE LOCK-DISZIPLIN EINES SYSCALLS (die neue, subtile Gefahr)
//
// Ein Syscall läuft im Kontext eines PRÄEMPTIV geplanten Prozesses, und im
// Interrupt-Gate sind Interrupts AUS. Daraus folgt beides:
//
//   (a) Solange wir Interrupts aus lassen, kann uns der Scheduler NICHT
//       verdrängen — ein Lock, den wir halten, wird also garantiert wieder
//       freigegeben. Deshalb sind Locks, die der Kernel ohnehin nur mit
//       ausgeschalteten Interrupts hält (KONSOLE, FRAMEBUFFER, MANAGER,
//       SERIAL, SOCKETS, GERAET, alle Blatt-Locks), aus einem Syscall
//       gefahrlos benutzbar: Wenn wir laufen, hält sie niemand.
//
//   (b) Auf einen Lock WARTEN dürfen wir mit ausgeschalteten Interrupts
//       NIEMALS: Hält ihn der verdrängte Kernel-Prozess, kommt der nie wieder
//       dran — Hänger. `fs::mit_fs` ist genau so ein Lock (ohne
//       without_interrupts). Deshalb `mit_vfs` unten: try_lock, und bei
//       Misserfolg ein WARTEFENSTER.
//
// Ein WARTEFENSTER (`warte_fenster`) erlaubt Interrupts und hält dabei
// GARANTIERT keinen Lock. Genau dort darf der Scheduler den Prozess
// verdrängen — und genau dort darf `hlt` stehen (mit ausgeschalteten
// Interrupts wäre `hlt` ein Stillstand für immer!).
// ==========================================================================

pub mod datei;
pub mod audio;
pub mod fenster;
pub mod handle;
pub mod netz;
pub mod prozess;

use crate::prozess::TrapFrame;
use crate::ring3::{self, CopyFehler};
use crate::scheduler;
use alloc::string::String;

// ---------------------------------------------------------------------------
// Die Syscall-Nummern (die ABI — Änderungen sind ABI-Änderungen!)
// ---------------------------------------------------------------------------

// ----- Gruppe 0: Prozess und Ausgabe (0..15) -----
/// `exit(code)` — beendet den Prozess. Kehrt nie zurück.
pub const SYS_EXIT: u64 = 0;
/// `yield()` — gibt die Zeitscheibe freiwillig ab.
pub const SYS_YIELD: u64 = 1;
/// `getpid()` -> eigene Prozess-Nummer.
pub const SYS_GETPID: u64 = 2;
/// `schreibe(handle, ptr, len)` -> geschriebene Bytes.
pub const SYS_SCHREIBE: u64 = 3;
/// `schlafe(ms)` — Zustand `Wartend`, bis der Timer weckt.
pub const SYS_SCHLAFE: u64 = 4;
/// `zeit_jetzt()` -> Millisekunden seit dem Boot (monoton).
pub const SYS_ZEIT_JETZT: u64 = 5;
/// `zeit_epoche()` -> Sekunden seit dem 1.1.2000 (echte Uhr, RTC-Anker).
pub const SYS_ZEIT_EPOCHE: u64 = 6;
/// `lese(handle, ptr, len)` -> Bytes (0 = Dateiende). BLOCKIEREND auf Pipes.
pub const SYS_LESE: u64 = 7;
/// `warte(pid)` -> Exit-Code des Kindes. BLOCKIEREND. `pid = 0` = irgendeines.
pub const SYS_WARTE: u64 = 8;
/// `beende(pid)` — beendet einen Prozess von aussen.
pub const SYS_BEENDE: u64 = 9;
/// `pipe()` -> Lese-Handle in den unteren, Schreib-Handle in den oberen
/// 32 Bit des Ergebnisses.
pub const SYS_PIPE: u64 = 10;
/// `starte(pfad_ptr, pfad_len, eingabe_handle, ausgabe_handle)` -> PID.
pub const SYS_STARTE: u64 = 11;
/// `zufall(ptr, len)` -> gefüllte Bytes. Kryptographisch brauchbarer Zufall.
/// BLOCKIEREND, bis der Entropie-Pool gesät ist (Frist: `ZUFALL_FRIST_MS`,
/// danach `NichtGesaet`). Siehe docs/zufall.md.
pub const SYS_ZUFALL: u64 = 12;
/// `zeit_geprueft()` -> UNIX-Sekunden (UTC) — **nur wenn die Uhr plausibel
/// ist**, sonst `ZeitUnplausibel`. Das ist die Zeitquelle für die
/// Zertifikatsprüfung (Serie 7, Teil 2; siehe docs/zeit.md).
pub const SYS_ZEIT_GEPRUEFT: u64 = 13;
/// `speicher(bytes)` -> Basisadresse des NEUEN Heap-Stuecks.
/// Erweitert den User-Heap des aufrufenden Prozesses (Serie 7, Teil 3).
pub const SYS_SPEICHER: u64 = 14;

// ----- Gruppe 1: Dateien über das VFS (16..31) -----
/// `oeffne(pfad_ptr, pfad_len, modus)` -> Handle.
pub const SYS_OEFFNE: u64 = 16;
/// `lese_at(handle, offset, ptr, len)` -> gelesene Bytes (0 = Dateiende).
pub const SYS_LESE_AT: u64 = 17;
/// `schreibe_at(handle, offset, ptr, len)` -> geschriebene Bytes.
pub const SYS_SCHREIBE_AT: u64 = 18;
/// `schliesse(handle)` — schliesst JEDES Handle (Datei ODER Socket).
pub const SYS_SCHLIESSE: u64 = 19;
/// `stat(pfad_ptr, pfad_len, ziel_ptr)` — schreibt `StatDaten` (32 Byte).
pub const SYS_STAT: u64 = 20;
/// `liste(pfad_ptr, pfad_len, ziel_ptr, ziel_len)` -> Anzahl Einträge im
/// Verzeichnis (auch wenn weniger in den Puffer passten).
pub const SYS_LISTE: u64 = 21;
/// `loesche(pfad_ptr, pfad_len)`.
pub const SYS_LOESCHE: u64 = 22;
/// `umbenenne(von_ptr, von_len, nach_ptr, nach_len)`.
pub const SYS_UMBENENNE: u64 = 23;
/// `mkdir(pfad_ptr, pfad_len)`.
pub const SYS_MKDIR: u64 = 24;

// ----- Gruppe 2: Netz über die Socket-API (32..47) -----
/// `socket(typ)` -> Handle (typ: 0 = TCP, 1 = UDP).
pub const SYS_SOCKET: u64 = 32;
/// `verbinde(handle, ip, port)` — TCP: wartet auf den Handshake (Timeout).
pub const SYS_VERBINDE: u64 = 33;
/// `sende(handle, ptr, len)` -> übernommene Bytes.
pub const SYS_SENDE: u64 = 34;
/// `empfange(handle, ptr, len)` -> Bytes (0 = noch nichts da).
pub const SYS_EMPFANGE: u64 = 35;
/// `aufloesen(name_ptr, name_len)` -> IPv4 als u32 (Big-Endian-Reihenfolge
/// a.b.c.d in den Bytes 3..0).
pub const SYS_AUFLOESEN: u64 = 36;
/// `socket_zustand(handle)` -> Zustand als Zahl (siehe docs/syscalls.md).
pub const SYS_SOCKET_ZUSTAND: u64 = 37;

// ----- Gruppe 3: Fenster (48..63) — Serie 8 -----
/// `fenster_oeffnen(titel_ptr, titel_len, breite, hoehe)` -> Handle.
pub const SYS_FENSTER_OEFFNEN: u64 = 48;
/// `fenster_zeichnen(handle, ptr, len, rechteck)` -> gesetzte Pixel.
/// Das Rechteck steckt gepackt in einem u64 (siehe syscall/fenster.rs).
pub const SYS_FENSTER_ZEICHNEN: u64 = 49;
/// `fenster_ereignis(handle, ziel_ptr, frist_ms)` -> Ereignis-Art.
/// BLOCKIEREND mit Frist; `0` (= Keins) heisst „nichts passiert".
pub const SYS_FENSTER_EREIGNIS: u64 = 50;
/// `fenster_titel_setzen(handle, titel_ptr, titel_len)`.
pub const SYS_FENSTER_TITEL: u64 = 51;
/// `fenster_schliessen(handle)` — dasselbe wie `schliesse` auf einem
/// Fenster-Handle, nur mit dem Namen, der im Programm lesbar ist.
pub const SYS_FENSTER_SCHLIESSEN: u64 = 52;

// ----- Gruppe 9: nur für Tests (240..) -----
/// Kopiert den eingehenden Trap-Rahmen weg (Kontext-Sicherungs-Test).
/// Eine Tonquelle anmelden (Serie 10). Liefert ein Handle.
pub const SYS_AUDIO_OEFFNEN: u64 = 53;
/// PCM anhaengen (Handle, Zeiger, Laenge in Bytes).
pub const SYS_AUDIO_SCHREIBEN: u64 = 54;
/// Wie viele Samples noch warten (Handle).
pub const SYS_AUDIO_STATUS: u64 = 55;
/// Lautstaerke DIESER Quelle in Promille (Handle, 0..1000).
pub const SYS_AUDIO_LAUTSTAERKE: u64 = 56;

pub const SYS_KONTEXT_TEST: u64 = 240;

/// Höchstlänge eines Pfad-Arguments in Bytes.
pub const MAX_PFAD: usize = 255;
/// Höchstlänge eines Namens für `aufloesen`.
pub const MAX_NAME: usize = 255;
/// Höchstlänge eines Datenpuffers je Aufruf (wie `ring3::COPY_IN_MAX`).
pub const MAX_PUFFER: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// DER EINHEITLICHE FEHLER-TYP DES USER-SPACE
// ---------------------------------------------------------------------------

/// Jeder Fehler, den ein Prozess sehen kann. Die Zahlen sind ABI — sie dürfen
/// sich NIE ändern, nur wachsen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Fehler {
    /// Kein Fehler.
    Ok = 0,
    /// Diese Syscall-Nummer gibt es nicht.
    UnbekannterSyscall = 1,
    /// Ein Argument ist unsinnig (Modus, Typ, Länge 0 wo verboten ...).
    UngueltigesArgument = 2,
    /// Ein übergebener Zeiger/Bereich gehört dem Prozess nicht (Kernel-
    /// Adresse, ungemappt, über die Seitengrenze, Integer-Überlauf).
    UngueltigerZeiger = 3,
    /// Länge über der erlaubten Obergrenze (Puffer/Pfad).
    ZuGross = 4,
    /// Dieses Handle gibt es in DIESEM Prozess nicht (fremd/geschlossen/
    /// nie vergeben).
    UngueltigerHandle = 5,
    /// Die Handle-Tabelle des Prozesses ist voll.
    KeineHandlesFrei = 6,
    /// Das Handle ist vom falschen Typ (Datei-Operation auf Socket o. ä.).
    FalscherHandleTyp = 7,
    /// Pfad/Datei/Verzeichnis existiert nicht.
    NichtGefunden = 8,
    /// Existiert schon.
    ExistiertBereits = 9,
    /// Ein Pfad-Bestandteil ist kein Verzeichnis.
    KeinVerzeichnis = 10,
    /// Es ist ein Verzeichnis, aber eine Datei war gemeint.
    KeineDatei = 11,
    /// Verzeichnis nicht leer.
    NichtLeer = 12,
    /// Der Pfad ist syntaktisch ungültig (auch: kein gültiges UTF-8).
    UngueltigerPfad = 13,
    /// Kein Platz mehr (Dateisystem voll, Datei zu groß, zu viele Sockets).
    KeinPlatz = 14,
    /// Nur lesbar (schreibgeschützter Mount oder Handle ohne Schreibrecht).
    NurLesen = 15,
    /// Ein Gerät hat einen Hardware-Fehler gemeldet.
    Geraetefehler = 16,
    /// Zeitüberschreitung.
    Zeitueberschreitung = 17,
    /// Es ist kein Dateisystem/Netz konfiguriert.
    NichtKonfiguriert = 18,
    /// Kein passendes Gerät vorhanden.
    KeinGeraet = 19,
    /// Socket nicht verbunden.
    NichtVerbunden = 20,
    /// Socket bereits verbunden.
    BereitsVerbunden = 21,
    /// Die Verbindung wurde abgebrochen.
    Abgebrochen = 22,
    /// Diese Operation gibt es (noch) nicht — z. B. Lesen von Handle 0.
    NichtUnterstuetzt = 23,
    /// Eine Kernel-Ressource ist gerade belegt; ein erneuter Versuch kann
    /// gelingen (siehe die Lock-Disziplin im Kopfkommentar).
    Belegt = 24,
    /// Der Zufallsgenerator ist noch nicht ausreichend gesät (Serie 7,
    /// Teil 1). **Ein eigener Code und kein `Belegt`**, weil der Aufrufer
    /// etwas völlig anderes tun muss: nicht „gleich nochmal", sondern
    /// „warte auf Entropie — oder verzichte auf Kryptographie". Es gibt in
    /// diesem Zustand KEINE schwachen Bytes (docs/zufall.md §4).
    NichtGesaet = 25,
    /// Die Wanduhr ist nachweislich falsch (vor dem Bau-Datum des Kernels
    /// oder absurd weit in der Zukunft) — Serie 7, Teil 2.
    ///
    /// **Ein eigener Code, weil die Konsequenz eine andere ist:** Wer
    /// Zertifikate prüfen will, darf jetzt NICHT weitermachen. Die
    /// Versuchung „Zeit stimmt nicht, prüfen wir die Gültigkeit halt nicht"
    /// ist genau der Punkt, an dem TLS aufhört, etwas wert zu sein
    /// (docs/tls-vertrauen.md).
    ZeitUnplausibel = 26,
}

impl Fehler {
    /// Der ABI-Zahlenwert.
    pub fn code(self) -> u64 {
        self as u64
    }

    /// Deutsche Meldung (Shell/Diagnose — der Prozess sieht nur die Zahl).
    pub fn meldung(self) -> &'static str {
        match self {
            Fehler::Ok => "kein Fehler",
            Fehler::UnbekannterSyscall => "unbekannte Syscall-Nummer",
            Fehler::UngueltigesArgument => "ungueltiges Argument",
            Fehler::UngueltigerZeiger => "ungueltiger Zeiger (nicht im eigenen Speicher)",
            Fehler::ZuGross => "Laenge ueber der Obergrenze",
            Fehler::UngueltigerHandle => "ungueltiges Handle",
            Fehler::KeineHandlesFrei => "keine Handles mehr frei",
            Fehler::FalscherHandleTyp => "Handle vom falschen Typ",
            Fehler::NichtGefunden => "nicht gefunden",
            Fehler::ExistiertBereits => "existiert bereits",
            Fehler::KeinVerzeichnis => "kein Verzeichnis",
            Fehler::KeineDatei => "keine Datei",
            Fehler::NichtLeer => "Verzeichnis nicht leer",
            Fehler::UngueltigerPfad => "ungueltiger Pfad",
            Fehler::KeinPlatz => "kein Platz mehr",
            Fehler::NurLesen => "nur lesbar",
            Fehler::Geraetefehler => "Geraetefehler",
            Fehler::Zeitueberschreitung => "Zeitueberschreitung",
            Fehler::NichtKonfiguriert => "nicht konfiguriert",
            Fehler::KeinGeraet => "kein Geraet vorhanden",
            Fehler::NichtVerbunden => "nicht verbunden",
            Fehler::BereitsVerbunden => "bereits verbunden",
            Fehler::Abgebrochen => "abgebrochen",
            Fehler::NichtUnterstuetzt => "nicht unterstuetzt",
            Fehler::Belegt => "Ressource belegt, spaeter erneut versuchen",
            Fehler::NichtGesaet => "Zufallsgenerator noch nicht ausreichend gesaet",
            Fehler::ZeitUnplausibel => "die Uhr ist nachweislich falsch gestellt",
        }
    }

    /// Zeit-Plausibilitätsfehler -> User-Space.
    pub fn von_zeit(fehler: crate::zeit::ZeitFehler) -> Fehler {
        // Bewusst EIN Code für alle drei Fälle: Ob die Uhr zu früh, zu spät
        // oder gar nicht da ist, ändert für den Aufrufer nichts — er hat
        // keine Zeitbasis. Der GRUND steht im Kernel-Log und in den
        // Einstellungen, wo ein Mensch ihn lesen kann.
        let _ = fehler;
        Fehler::ZeitUnplausibel
    }

    // --- DIE ABBILDUNGEN: Kernel-Typen werden zu User-Space-Fehlern ---
    // Genau hier endet die Kernel-Welt. Ein Prozess erfährt NICHT, ob unter
    // ihm SpeedFS, FAT32 oder ein RamFs liegt — nur, WAS schiefgegangen ist.

    /// Geräte-Fehler (`IoFehler`) -> User-Space.
    pub fn von_io(fehler: crate::fs::IoFehler) -> Fehler {
        use crate::fs::IoFehler as I;
        match fehler {
            I::AusserhalbDesGeraets | I::UngueltigePufferGroesse => Fehler::UngueltigesArgument,
            I::Geraetefehler => Fehler::Geraetefehler,
            I::NichtBereit => Fehler::KeinGeraet,
            I::Zeitueberschreitung => Fehler::Zeitueberschreitung,
            I::Schreibgeschuetzt | I::NurLesen => Fehler::NurLesen,
        }
    }

    /// Dateisystem-Fehler (`FsFehler`) -> User-Space.
    pub fn von_fs(fehler: crate::fs::FsFehler) -> Fehler {
        use crate::fs::FsFehler as F;
        match fehler {
            F::NichtGefunden => Fehler::NichtGefunden,
            F::ExistiertBereits => Fehler::ExistiertBereits,
            F::KeinVerzeichnis => Fehler::KeinVerzeichnis,
            F::KeineDatei => Fehler::KeineDatei,
            F::VerzeichnisNichtLeer => Fehler::NichtLeer,
            F::UngueltigerPfad => Fehler::UngueltigerPfad,
            F::NichtInitialisiert => Fehler::NichtKonfiguriert,
            F::Voll | F::DateiZuGross => Fehler::KeinPlatz,
            // Dass das Medium kein SpeedFS/FAT32 trägt, ist für einen Prozess
            // dasselbe wie „da ist kein Dateisystem" — welches Format der
            // Kernel erwartet, ist NICHT seine Angelegenheit.
            F::KeinSpeedFs | F::KeinFat32 => Fehler::NichtKonfiguriert,
            F::MountGrenze => Fehler::NichtUnterstuetzt,
            F::Io(io) => Fehler::von_io(io),
        }
    }

    /// Socket-Fehler -> User-Space.
    pub fn von_socket(fehler: crate::netz::socket::SocketFehler) -> Fehler {
        use crate::netz::socket::SocketFehler as S;
        match fehler {
            S::UngueltigerHandle => Fehler::UngueltigerHandle,
            S::FalscherTyp => Fehler::FalscherHandleTyp,
            S::NichtVerbunden => Fehler::NichtVerbunden,
            S::BereitsVerbunden => Fehler::BereitsVerbunden,
            S::NichtGebunden => Fehler::UngueltigesArgument,
            S::KeinPlatz => Fehler::KeinPlatz,
            S::NichtKonfiguriert => Fehler::NichtKonfiguriert,
            S::KeinGeraet => Fehler::KeinGeraet,
            S::Abgebrochen => Fehler::Abgebrochen,
            S::Zeitueberschreitung => Fehler::Zeitueberschreitung,
        }
    }

    /// DNS-Fehler -> User-Space.
    pub fn von_dns(fehler: crate::netz::dns::DnsFehler) -> Fehler {
        use crate::netz::dns::DnsFehler as D;
        match fehler {
            D::KeinDnsServer => Fehler::NichtKonfiguriert,
            D::Zeitueberschreitung => Fehler::Zeitueberschreitung,
            D::NichtGefunden => Fehler::NichtGefunden,
            D::Netz(_) => Fehler::KeinGeraet,
        }
    }

    /// Zeiger-Prüfungs-Fehler (Dauerregel I) -> User-Space. Bewusst GROB:
    /// Ein Prozess soll nicht erfahren, WARUM genau sein Zeiger abgelehnt
    /// wurde (ob eine Adresse gemappt ist, ist eine Information über den
    /// Kernel-Zustand) — nur DASS er ungültig ist.
    pub fn von_copy(fehler: CopyFehler) -> Fehler {
        match fehler {
            CopyFehler::ZuGross => Fehler::ZuGross,
            _ => Fehler::UngueltigerZeiger,
        }
    }
}

/// Das Ergebnis eines Syscalls: Fehlercode plus Ergebniswert.
pub type SysErgebnis = Result<u64, Fehler>;

// ---------------------------------------------------------------------------
// Die Lock-Disziplin in Code (siehe Kopfkommentar)
// ---------------------------------------------------------------------------

/// EIN WARTEFENSTER: Interrupts an, bis zum nächsten Interrupt schlafen,
/// Interrupts wieder aus.
///
/// Hier — und nur hier — darf der Scheduler einen Prozess mitten im Syscall
/// verdrängen. Deshalb die eiserne Bedingung: **beim Aufruf darf KEIN Lock
/// gehalten werden.** Sonst könnte der Prozess mit gehaltenem Lock verdrängt
/// (oder beendet) werden und ihn nie wieder freigeben.
///
/// `enable_and_hlt` statt `enable(); hlt()`, damit zwischen beidem kein
/// Interrupt verloren geht (dasselbe Muster wie `Executor::sleep_if_idle`).
pub fn warte_fenster() {
    use x86_64::instructions::interrupts;
    interrupts::enable_and_hlt();
    interrupts::disable();
}

/// Wie viele Versuche `mit_vfs` unternimmt, bevor es `Belegt` meldet.
/// Jeder Fehlversuch kostet ein Wartefenster (~1 Timer-Tick = 4 ms), das
/// reicht für 200 ms — weit mehr, als ein Kernel-Task den VFS-Lock hält.
const VFS_VERSUCHE: usize = 50;

/// Führt eine VFS-Operation aus einem SYSCALL aus.
///
/// Der Unterschied zu `fs::mit_fs`: Es wird NIE blockierend auf den Lock
/// gewartet (das wäre mit ausgeschalteten Interrupts ein Hänger — siehe
/// Kopfkommentar (b)). Stattdessen `try_lock` in einer Schleife mit
/// Wartefenstern dazwischen. Hat der Lock geklappt, läuft die Operation mit
/// ausgeschalteten Interrupts — wir können also nicht verdrängt werden,
/// während wir ihn halten.
pub fn mit_vfs<T>(
    f: impl FnOnce(&mut dyn crate::fs::FileSystem) -> crate::fs::FsErgebnis<T>,
) -> Result<T, Fehler> {
    let mut aufgabe = Some(f);
    for _ in 0..VFS_VERSUCHE {
        if let Some(mut wache) = crate::fs::vfs_versuchen() {
            let fs = wache.fs().map_err(Fehler::von_fs)?;
            // `aufgabe` wird genau EINMAL verbraucht — der Lock ist da.
            let f = aufgabe.take().expect("VFS-Aufgabe doppelt verbraucht");
            return f(fs).map_err(Fehler::von_fs);
        }
        // Lock belegt: Der Kernel-Prozess soll weiterlaufen dürfen.
        warte_fenster();
    }
    Err(Fehler::Belegt)
}

// ---------------------------------------------------------------------------
// Argument-Helfer (immer über copy_in — Dauerregel I)
// ---------------------------------------------------------------------------

/// Liest einen PFAD aus dem User-Speicher: Längen-Deckel, copy-in, UTF-8.
///
/// Drei Prüfungen, alle nötig: **(1)** Die Länge wird VOR dem Kopieren
/// gedeckelt (ein Prozess könnte 4 GiB behaupten). **(2)** `copy_in` prüft
/// den Bereich dreistufig und kopiert (Dauerregel I). **(3)** Erst die Kopie
/// wird als UTF-8 geprüft — der Kernel baut nie einen `&str` auf fremdem
/// Speicher, dessen Inhalt sich unter den Händen ändern könnte.
pub fn pfad_lesen(ptr: u64, laenge: u64) -> Result<String, Fehler> {
    if laenge == 0 {
        return Err(Fehler::UngueltigerPfad);
    }
    if laenge as usize > MAX_PFAD {
        return Err(Fehler::ZuGross);
    }
    let bytes = ring3::copy_in(ptr, laenge as usize).map_err(Fehler::von_copy)?;
    let text = String::from_utf8(bytes).map_err(|_| Fehler::UngueltigerPfad)?;
    // Absolute Pfade only: relative Pfade hätten ein Arbeitsverzeichnis pro
    // Prozess zur Voraussetzung, das es (noch) nicht gibt. Ehrlich ablehnen.
    if !text.starts_with('/') {
        return Err(Fehler::UngueltigerPfad);
    }
    Ok(text)
}

/// Liest einen DATEN-Puffer aus dem User-Speicher (Längen-Deckel + copy-in).
pub fn puffer_lesen(ptr: u64, laenge: u64) -> Result<alloc::vec::Vec<u8>, Fehler> {
    if laenge as usize > MAX_PUFFER {
        return Err(Fehler::ZuGross);
    }
    ring3::copy_in(ptr, laenge as usize).map_err(Fehler::von_copy)
}

/// Schreibt Kernel-Daten in einen User-Puffer (copy-out mit Schreibrecht-
/// Prüfung).
pub fn puffer_schreiben(ptr: u64, daten: &[u8]) -> Result<(), Fehler> {
    ring3::copy_out(ptr, daten).map_err(Fehler::von_copy)
}

/// Prüft eine Länge gegen die Puffer-Obergrenze und gibt sie als usize.
pub fn laenge_pruefen(laenge: u64) -> Result<usize, Fehler> {
    if laenge as usize > MAX_PUFFER {
        Err(Fehler::ZuGross)
    } else {
        Ok(laenge as usize)
    }
}

// ---------------------------------------------------------------------------
// Gruppe 0: Prozess und Ausgabe
// ---------------------------------------------------------------------------

/// Die reservierten Standard-Handles (siehe handle.rs).
pub use handle::{HANDLE_AUSGABE, HANDLE_DIAGNOSE, HANDLE_EINGABE};

/// `schreibe(handle, ptr, len)` — die Ausgabe-Syscall.
///
/// Seit Teil 6 kann sie BLOCKIEREN: Schreibt der Prozess in eine volle Pipe,
/// wird er schlafen gelegt und der Syscall später wiederholt (deshalb der
/// `Ausgang` statt eines schlichten Ergebnisses).
fn sys_schreibe(h: u64, ptr: u64, laenge: u64) -> prozess::Ausgang {
    match sys_schreibe_inner(h, ptr, laenge) {
        Ok(ausgang) => ausgang,
        Err(fehler) => prozess::Ausgang::Fertig(Err(fehler)),
    }
}

fn sys_schreibe_inner(h: u64, ptr: u64, laenge: u64) -> Result<prozess::Ausgang, Fehler> {
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(prozess::Ausgang::Fertig(Ok(0)));
    }
    // Das Ziel BEVOR wir kopieren bestimmen — ein ungültiges Handle soll
    // nicht erst nach einer 64-KiB-Kopie auffallen.
    let ziel = handle::schreib_ziel(h)?;
    // Der Pipe-Zweig hat seine eigene Reihenfolge (erst Platz prüfen, dann
    // kopieren), damit ein Neustart nach dem Blockieren nichts wiederholt.
    if let handle::SchreibZiel::Pipe(id) = ziel {
        return Ok(prozess::pipe_schreiben(id, ptr, laenge));
    }
    let bytes = ring3::copy_in(ptr, laenge).map_err(Fehler::von_copy)?;
    // Nicht-UTF-8 wird zeichenweise ausgegeben statt abgelehnt: Ausgabe ist
    // ein Byte-Strom, kein Text-Kanal.
    let geschrieben = match ziel {
        handle::SchreibZiel::Konsole => {
            match core::str::from_utf8(&bytes) {
                // print! = Bildschirm UND seriell (Projektregel).
                Ok(text) => crate::print!("{}", text),
                Err(_) => {
                    for &b in &bytes {
                        crate::print!("{}", b as char);
                    }
                }
            }
            bytes.len()
        }
        handle::SchreibZiel::Diagnose => {
            match core::str::from_utf8(&bytes) {
                Ok(text) => crate::serial_print!("{}", text),
                Err(_) => {
                    for &b in &bytes {
                        crate::serial_print!("{}", b as char);
                    }
                }
            }
            bytes.len()
        }
        handle::SchreibZiel::Datei { pfad, .. } => {
            // Eine Datei ohne Offset zu beschreiben heißt: ANHÄNGEN. Das ist
            // die einzige sinnvolle Deutung für einen Strom-Schreibvorgang.
            let laenge_vorher = mit_vfs(|fs| fs.stat(&pfad).map(|m| m.groesse)).unwrap_or(0);
            mit_vfs(|fs| fs.write_at(&pfad, laenge_vorher, &bytes))?
        }
        handle::SchreibZiel::Socket(sh) => {
            crate::netz::socket::senden(sh, &bytes).map_err(Fehler::von_socket)?
        }
        // Oben abgefangen — hier nur, damit das match vollständig ist.
        handle::SchreibZiel::Pipe(_) => unreachable!("Pipe-Zweig läuft über pipe_schreiben"),
    };
    // Für den Präemptions-Beweis (Serie 6, Teil 3): Wer hat wann gedruckt?
    if let Some(letztes) = bytes.iter().rev().copied().find(|b| b.is_ascii_graphic()) {
        scheduler::spur_melden(scheduler::laufende_user_pid(), letztes);
    }
    Ok(prozess::Ausgang::Fertig(Ok(geschrieben as u64)))
}

// ---------------------------------------------------------------------------
// zufall
// ---------------------------------------------------------------------------

/// Wie lange `zufall` höchstens auf einen gesäten Pool wartet.
///
/// 10 Sekunden, weil der schwächste denkbare Fall (nur PIT-Jitter, keine
/// Hardware-Quelle, keine Eingabe) rechnerisch ~8 s braucht — die Frist ist
/// also so gewählt, dass die schlechteste Konfiguration es gerade noch
/// schafft und alles darunter ein echter Befund ist (docs/zufall.md §4).
pub const ZUFALL_FRIST_MS: u64 = 10_000;

/// `zufall(ptr, len)` -> gefüllte Bytes.
///
/// ==========================================================================
/// DIE ENTSCHEIDUNG „BLOCKIEREN STATT SCHWACHEM ZUFALL", in Code:
///
/// Ist der Pool nicht gesät, WARTET dieser Syscall — er liefert unter keinen
/// Umständen Bytes aus einem ungesäten Generator. Der Grund steht in
/// docs/zufall.md §4 und ist kurz: Der Aufrufer kann Zufall nicht auf
/// Qualität prüfen. Er baut daraus einen Sitzungsschlüssel, die Verbindung
/// steht, das Schloss-Symbol erscheint — und der Fehler bleibt für immer
/// still. Ein Fallback wäre hier die schlimmste Lösung, nicht die bequemste.
///
/// ABER MIT FRIST, und das ist der Unterschied zu Linux' `getrandom`: Ewiges
/// Blockieren wäre auf einem Rechner ohne Netz und ohne Tastatur ein Hänger
/// ohne Meldung — und „keine Meldung" verstösst gegen die
/// Daten-Integritäts-Regel dieses Projekts genauso wie stiller Datenverlust.
/// Nach `ZUFALL_FRIST_MS` gibt es deshalb einen klaren Fehler.
///
/// Gewartet wird über `warte_fenster()` (dasselbe Muster wie `verbinde`):
/// Interrupts an, `hlt`, Interrupts aus. Genau dort darf der Scheduler
/// verdrängen, und genau dort fällt die Entropie an, auf die wir warten —
/// jeder Timer-Tick speist den Pool.
/// ==========================================================================
fn sys_zufall(ptr: u64, laenge: u64) -> SysErgebnis {
    let laenge = laenge_pruefen(laenge)?;
    if laenge == 0 {
        return Ok(0);
    }
    // Den Zielbereich VOR dem Warten prüfen: Ein kaputter Zeiger soll nicht
    // erst nach zehn Sekunden auffallen.
    ring3::user_bereich_pruefen(ptr, laenge, true).map_err(Fehler::von_copy)?;

    let frist = crate::zeit::ms_seit_boot() + ZUFALL_FRIST_MS;
    while !crate::zufall::bereit() {
        if crate::zeit::ms_seit_boot() >= frist {
            return Err(Fehler::NichtGesaet);
        }
        // WARTEFENSTER: Wir halten hier nichts. Interrupts an, damit der
        // Timer tickt — er ist die Quelle, die den Pool füllt.
        warte_fenster();
    }

    // In einen KERNEL-Puffer erzeugen und geprüft hinauskopieren
    // (Dauerregel I). Nie direkt in User-Speicher schreiben: Schlägt die
    // Prüfung fehl, hat der Prozess NICHTS bekommen statt eines halb
    // gefüllten Puffers — und bei Zufall wäre „halb gefüllt" besonders
    // heimtückisch, weil die Nullen wie Zufall aussehen.
    let mut puffer = alloc::vec![0u8; laenge];
    crate::zufall::fuellen(&mut puffer).map_err(|_| Fehler::NichtGesaet)?;
    let ergebnis = puffer_schreiben(ptr, &puffer);
    // Den Kernel-Puffer nicht mit Schlüsselmaterial liegen lassen.
    puffer.iter_mut().for_each(|b| *b = 0);
    ergebnis?;
    Ok(laenge as u64)
}

// ---------------------------------------------------------------------------
// DER DISPATCHER — die Syscall-Tabelle
// ---------------------------------------------------------------------------

/// Führt einen Syscall aus. Reine Zahlen rein, `SysErgebnis` raus — dadurch
/// ist die ganze Tabelle ohne Trap-Rahmen testbar.
///
/// `exit`, `yield` und `schlafe` fehlen hier bewusst: Sie WECHSELN den
/// Prozess und brauchen deshalb den Trap-Rahmen. Sie stehen in `dispatch`.
pub fn ausfuehren(nummer: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> SysErgebnis {
    match nummer {
        // ----- Gruppe 0: Prozess und Ausgabe -----
        SYS_GETPID => Ok(scheduler::laufende_user_pid() as u64),
        SYS_ZEIT_JETZT => Ok(crate::zeit::ms_seit_boot()),
        SYS_ZEIT_EPOCHE => Ok(crate::zeit::sekunden_seit_2000(&crate::zeit::jetzt())),
        SYS_BEENDE => prozess::sys_beende(a0),
        SYS_PIPE => prozess::sys_pipe(),
        SYS_STARTE => prozess::sys_starte(a0, a1, a2, a3),
        SYS_ZUFALL => sys_zufall(a0, a1),
        // Die GEPRÜFTE Uhrzeit. `zeit_epoche` (6) bleibt unverändert und
        // ungeprüft — es füttert Anzeigen, und eine Uhr im Taskleisten-Feld
        // darf falsch gehen. Wer PRÜFT, nimmt diesen hier und bekommt bei
        // einer kaputten Uhr einen Fehler statt einer Zahl.
        SYS_ZEIT_GEPRUEFT => crate::zeit::zertifikatszeit().map_err(Fehler::von_zeit),
        // DER USER-HEAP. Es gibt bewusst kein `frei` dazu: Ein Prozess gibt
        // Seiten nie einzeln zurueck, sein Adressraum faellt beim Ende als
        // Ganzes (Serie 6, Teil 3). Der Allocator im User-Space verwaltet
        // innerhalb dessen, was er bekommen hat — genau wie `brk` unter Unix.
        SYS_SPEICHER => scheduler::heap_erweitern(a0).map(|(basis, _)| basis),

        // ----- Gruppe 1: Dateien -----
        SYS_OEFFNE => datei::sys_oeffne(a0, a1, a2),
        SYS_LESE_AT => datei::sys_lese_at(a0, a1, a2, a3),
        SYS_SCHREIBE_AT => datei::sys_schreibe_at(a0, a1, a2, a3),
        SYS_SCHLIESSE => handle::sys_schliesse(a0),
        SYS_STAT => datei::sys_stat(a0, a1, a2),
        SYS_LISTE => datei::sys_liste(a0, a1, a2, a3),
        SYS_LOESCHE => datei::sys_loesche(a0, a1),
        SYS_UMBENENNE => datei::sys_umbenenne(a0, a1, a2, a3),
        SYS_MKDIR => datei::sys_mkdir(a0, a1),

        // ----- Gruppe 2: Netz -----
        SYS_SOCKET => netz::sys_socket(a0),
        SYS_VERBINDE => netz::sys_verbinde(a0, a1, a2),
        SYS_SENDE => netz::sys_sende(a0, a1, a2),
        SYS_EMPFANGE => netz::sys_empfange(a0, a1, a2),
        SYS_AUFLOESEN => netz::sys_aufloesen(a0, a1),
        SYS_SOCKET_ZUSTAND => netz::sys_socket_zustand(a0),

        // ----- Gruppe 3: Fenster (Serie 8) -----
        // `fenster_ereignis` fehlt hier: Es BLOCKIERT und braucht deshalb
        // den Trap-Rahmen — es steht im Dispatcher unten.
        SYS_FENSTER_OEFFNEN => fenster::sys_oeffnen(a0, a1, a2, a3),
        SYS_FENSTER_ZEICHNEN => fenster::sys_zeichnen(a0, a1, a2, a3),
        SYS_FENSTER_TITEL => fenster::sys_titel_setzen(a0, a1, a2),
        SYS_FENSTER_SCHLIESSEN => fenster::sys_schliessen(a0),
        SYS_AUDIO_OEFFNEN => audio::sys_oeffnen(),
        SYS_AUDIO_SCHREIBEN => audio::sys_schreiben(a0, a1, a2),
        SYS_AUDIO_STATUS => audio::sys_status(a0),
        SYS_AUDIO_LAUTSTAERKE => audio::sys_lautstaerke(a0, a1),

        // Unbekannt: sauberer Fehler, NIE eine Panik.
        _ => Err(Fehler::UnbekannterSyscall),
    }
}

// Der nackte Assembler-Einstieg für INT 0x80. Er sichert ALLE General-Register
// (baut einen `prozess::TrapFrame` auf dem Kernel-Stack des Prozesses), ruft
// den Rust-Dispatcher — und der LIEFERT einen Rahmen zurück: normalerweise
// denselben, bei exit/yield/schlafe den eines ANDEREN Prozesses. Geladen wird
// er von `schalte_auf_rahmen` (scheduler.rs), dem einzigen Kontext-Lade-Punkt.
//
// WARUM INT 0x80 und nicht SYSCALL/SYSRET: Das Trap-Gate nutzt die
// IDT-Infrastruktur, die wir schon haben — ein Eintrag mit DPL 3, fertig.
// SYSCALL bräuchte MSR-Setup (STAR/LSTAR/SFMASK), eine bestimmte GDT-Ordnung
// und eigenes Stack-Switching. Für die ABI ist es einerlei; SYSCALL wäre eine
// spätere Optimierung, kein anderes Modell.
core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",              // Argument 1 = Zeiger auf den TrapFrame
    "call syscall_dispatch",     // -> rax = Rahmen, mit dem es weitergeht
    "mov rdi, rax",
    "jmp schalte_auf_rahmen",    // gemeinsamer Ausstieg (scheduler.rs)
);

extern "C" {
    fn syscall_entry();
}

/// Adresse des Syscall-Einstiegs (für die IDT-Registrierung in interrupts.rs).
pub fn handler_adresse() -> u64 {
    syscall_entry as *const () as u64
}

/// Länge des Befehls `int 0x80` in Bytes (CD 80).
///
/// Diese Zahl ist der ganze Trick hinter den blockierenden Syscalls: Um
/// `rip` um genau sie zurückzustellen, zeigt der nächste Befehl nach dem
/// Aufwachen wieder auf den Syscall — er läuft von vorn.
const INT_0X80_LAENGE: u64 = 2;

/// Verarbeitet den `Ausgang` eines möglicherweise blockierenden Syscalls.
///
/// Fertig    -> Fehlercode nach rax, Ergebnis nach rdx, weiter wie immer.
/// Blockiert -> `rip` um zwei Bytes zurück, Prozess schlafen legen, auf
///              einen anderen Prozess umschalten. **rax und rdx bleiben
///              unberührt** — sie tragen noch Syscall-Nummer und Argument 2,
///              die der Neustart braucht.
fn blockierend_behandeln(
    rahmen: *mut TrapFrame,
    f: &mut TrapFrame,
    ausgang: prozess::Ausgang,
) -> *mut TrapFrame {
    match ausgang {
        prozess::Ausgang::Fertig(ergebnis) => {
            ergebnis_eintragen(f, ergebnis);
            rahmen
        }
        prozess::Ausgang::Blockieren(grund, wach_ab_ms) => {
            f.rip -= INT_0X80_LAENGE;
            scheduler::blockieren(rahmen, grund, wach_ab_ms)
        }
    }
}

/// Trägt Fehlercode (rax) und Ergebnis (rdx) in den Rahmen ein.
fn ergebnis_eintragen(f: &mut TrapFrame, ergebnis: SysErgebnis) {
    match ergebnis {
        Ok(wert) => {
            f.rax = Fehler::Ok.code();
            f.rdx = wert;
        }
        Err(fehler) => {
            f.rax = fehler.code();
            f.rdx = 0;
            // Nur seriell und nur knapp: Ein Prozess darf beliebig oft
            // Unsinn übergeben, das ist kein Kernel-Ereignis.
            crate::serial_println!(
                "[syscall] PID {} -> Fehler {} ({})",
                scheduler::laufende_user_pid(),
                fehler.code(),
                fehler.meldung()
            );
        }
    }
}

/// Der Rust-Dispatcher: liest Nummer und Argumente aus dem gesicherten
/// Kontext, führt den Syscall aus, schreibt Fehlercode (rax) und Ergebnis
/// (rdx) zurück — und liefert den Rahmen, mit dem es weitergeht.
#[no_mangle]
extern "C" fn syscall_dispatch(rahmen: *mut TrapFrame) -> *mut TrapFrame {
    // unsafe: `rahmen` zeigt auf den eben gesicherten Register-Block auf dem
    // Kernel-Stack des aufrufenden Prozesses — gültig für die Dauer des
    // Handlers.
    let f = unsafe { &mut *rahmen };
    scheduler::syscall_zaehlen();

    let nummer = f.rax;
    let (a0, a1, a2, a3) = (f.rdi, f.rsi, f.rdx, f.r10);

    // Die drei Syscalls, die den PROZESS WECHSELN — sie brauchen den Rahmen
    // selbst und können deshalb nicht durch `ausfuehren` laufen.
    match nummer {
        SYS_EXIT => {
            f.rax = Fehler::Ok.code();
            f.rdx = 0;
            if let Some(neuer) = scheduler::syscall_beenden(rahmen, a0) {
                return neuer;
            }
            // Kein eingeplanter Prozess: der alte Einzelschuss-Pfad
            // (ring3test) kehrt in den Kernel zurück.
            ring3::einzelschuss_ruecksprung(f);
            return rahmen;
        }
        SYS_YIELD => {
            f.rax = Fehler::Ok.code();
            f.rdx = 0;
            return scheduler::syscall_abgeben(rahmen);
        }
        SYS_SCHLAFE => {
            f.rax = Fehler::Ok.code();
            f.rdx = 0;
            if let Some(neuer) = scheduler::syscall_schlafen(rahmen, a0) {
                return neuer;
            }
            return rahmen;
        }
        // DIE BLOCKIERENDEN SYSCALLS (Serie 6, Teil 6). Sie brauchen den
        // Rahmen, weil sie ihn beim Blockieren zurückstellen müssen — siehe
        // `blockierend_behandeln` und `scheduler::blockieren`.
        SYS_SCHREIBE => {
            let neu = blockierend_behandeln(rahmen, f, sys_schreibe(a0, a1, a2));
            return umplanen_falls_noetig(rahmen, neu);
        }
        SYS_LESE => {
            let neu = blockierend_behandeln(rahmen, f, prozess::sys_lese(a0, a1, a2));
            return umplanen_falls_noetig(rahmen, neu);
        }
        SYS_WARTE => {
            let neu = blockierend_behandeln(rahmen, f, prozess::sys_warte(a0));
            return umplanen_falls_noetig(rahmen, neu);
        }
        SYS_FENSTER_EREIGNIS => {
            let neu = blockierend_behandeln(rahmen, f, fenster::sys_ereignis(a0, a1, a2));
            return umplanen_falls_noetig(rahmen, neu);
        }

        SYS_KONTEXT_TEST => {
            // Nur Tests: den EINGEHENDEN Rahmen wegkopieren (beweist die
            // Kontext-Sicherung, siehe tests/scheduler.rs).
            ring3::kontext_test_sichern(*f);
            f.rax = ring3::KONTEXT_TEST_ANTWORT;
            f.rdx = 0;
            return rahmen;
        }
        _ => {}
    }

    ergebnis_eintragen(f, ausfuehren(nummer, a0, a1, a2, a3));
    umplanen_falls_noetig(rahmen, rahmen)
}

/// DER RESCHEDULE-PUNKT (Serie 7, Teil 0) am Rückweg jedes Syscalls, der
/// nicht ohnehin schon umgeschaltet hat.
///
/// `eingang` ist der Rahmen, mit dem wir hereinkamen, `ausgang` der, den der
/// Syscall zurückgibt. Sind sie VERSCHIEDEN, hat der Syscall selbst schon
/// gewechselt (exit, yield, blockieren) — dann ist hier nichts mehr zu tun,
/// und ein zweiter Wechsel wäre schlicht falsch (wir würden den Kontext eines
/// Prozesses anfassen, der schon läuft).
///
/// Sind sie GLEICH, hat der Syscall womöglich jemanden GEWECKT (in eine Pipe
/// geschrieben, ein Ende geschlossen, ein Kind beendet). Dann bekommt der
/// Scheduler jetzt die Gelegenheit umzuplanen — und zwar nach der normalen
/// Round-Robin-Regel, nicht als Übergabe an den Geweckten
/// (Begründung in scheduler.rs).
fn umplanen_falls_noetig(eingang: *mut TrapFrame, ausgang: *mut TrapFrame) -> *mut TrapFrame {
    if ausgang != eingang {
        return ausgang;
    }
    scheduler::umplanen_am_syscall_ende(ausgang)
}

// ---------------------------------------------------------------------------
// Tests der reinen Abbildungs- und Prüf-Logik
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{FsFehler, IoFehler};

    /// Die ABI-Zahlen sind ein VERSPRECHEN: Wenn sie sich ändern, ändert sich
    /// die Bedeutung schon compilierter Programme. Deshalb stehen sie hier
    /// festgenagelt — wer eine Nummer verschiebt, bricht diesen Test.
    #[test_case]
    fn test_abi_nummern_stabil() {
        assert_eq!(SYS_EXIT, 0);
        assert_eq!(SYS_YIELD, 1);
        assert_eq!(SYS_GETPID, 2);
        assert_eq!(SYS_SCHREIBE, 3);
        assert_eq!(SYS_SCHLAFE, 4);
        assert_eq!(SYS_ZEIT_JETZT, 5);
        assert_eq!(SYS_ZEIT_EPOCHE, 6);
        assert_eq!(SYS_LESE, 7);
        assert_eq!(SYS_WARTE, 8);
        assert_eq!(SYS_BEENDE, 9);
        assert_eq!(SYS_PIPE, 10);
        assert_eq!(SYS_STARTE, 11);
        assert_eq!(SYS_ZUFALL, 12);
        assert_eq!(SYS_ZEIT_GEPRUEFT, 13);
        assert_eq!(SYS_SPEICHER, 14);
        assert_eq!(SYS_OEFFNE, 16);
        assert_eq!(SYS_LESE_AT, 17);
        assert_eq!(SYS_SCHREIBE_AT, 18);
        assert_eq!(SYS_SCHLIESSE, 19);
        assert_eq!(SYS_STAT, 20);
        assert_eq!(SYS_LISTE, 21);
        assert_eq!(SYS_LOESCHE, 22);
        assert_eq!(SYS_UMBENENNE, 23);
        assert_eq!(SYS_MKDIR, 24);
        assert_eq!(SYS_SOCKET, 32);
        assert_eq!(SYS_VERBINDE, 33);
        assert_eq!(SYS_SENDE, 34);
        assert_eq!(SYS_EMPFANGE, 35);
        assert_eq!(SYS_AUFLOESEN, 36);
        assert_eq!(SYS_SOCKET_ZUSTAND, 37);
        assert_eq!(SYS_FENSTER_OEFFNEN, 48);
        assert_eq!(SYS_FENSTER_ZEICHNEN, 49);
        assert_eq!(SYS_FENSTER_EREIGNIS, 50);
        assert_eq!(SYS_FENSTER_TITEL, 51);
        assert_eq!(SYS_FENSTER_SCHLIESSEN, 52);
        assert_eq!(SYS_AUDIO_OEFFNEN, 53);
        assert_eq!(SYS_AUDIO_SCHREIBEN, 54);
        assert_eq!(SYS_AUDIO_STATUS, 55);
        assert_eq!(SYS_AUDIO_LAUTSTAERKE, 56);
        // Und die Fehlercodes.
        assert_eq!(Fehler::Ok.code(), 0);
        assert_eq!(Fehler::UnbekannterSyscall.code(), 1);
        assert_eq!(Fehler::UngueltigerZeiger.code(), 3);
        assert_eq!(Fehler::UngueltigerHandle.code(), 5);
        assert_eq!(Fehler::NichtGefunden.code(), 8);
        assert_eq!(Fehler::Belegt.code(), 24);
        assert_eq!(Fehler::NichtGesaet.code(), 25);
        assert_eq!(Fehler::ZeitUnplausibel.code(), 26);
    }

    /// Unbekannte Nummern liefern IMMER einen Fehler und panicken nie —
    /// auch die Lücken zwischen den Gruppen und u64::MAX.
    #[test_case]
    fn test_unbekannte_nummern() {
        // 53..56 sind seit Serie 10 die Audio-Syscalls und deshalb aus
        // dieser Liste ausgetragen — der Test hat das beim Ergaenzen
        // korrekt bemerkt. 57 ist die naechste freie Nummer.
        for nummer in [15u64, 25, 31, 38, 47, 57, 100, 239, 241, u64::MAX] {
            assert_eq!(
                ausfuehren(nummer, 0, 0, 0, 0),
                Err(Fehler::UnbekannterSyscall),
                "Nummer {} muesste unbekannt sein",
                nummer
            );
        }
    }

    /// Die Fehler-ABBILDUNG: Kein Kernel-Typ scheint nach außen durch, und
    /// jede Kernel-Variante hat ein Ziel.
    #[test_case]
    fn test_fehler_abbildung() {
        // Dateisystem
        assert_eq!(Fehler::von_fs(FsFehler::NichtGefunden), Fehler::NichtGefunden);
        assert_eq!(Fehler::von_fs(FsFehler::VerzeichnisNichtLeer), Fehler::NichtLeer);
        assert_eq!(Fehler::von_fs(FsFehler::Voll), Fehler::KeinPlatz);
        assert_eq!(Fehler::von_fs(FsFehler::DateiZuGross), Fehler::KeinPlatz);
        // Welches Format der Kernel erwartet, erfaehrt der Prozess NICHT:
        assert_eq!(Fehler::von_fs(FsFehler::KeinSpeedFs), Fehler::NichtKonfiguriert);
        assert_eq!(Fehler::von_fs(FsFehler::KeinFat32), Fehler::NichtKonfiguriert);
        // Geraete-Fehler wandern durch das VFS hindurch:
        assert_eq!(
            Fehler::von_fs(FsFehler::Io(IoFehler::Geraetefehler)),
            Fehler::Geraetefehler
        );
        assert_eq!(
            Fehler::von_fs(FsFehler::Io(IoFehler::Schreibgeschuetzt)),
            Fehler::NurLesen
        );
        assert_eq!(
            Fehler::von_fs(FsFehler::Io(IoFehler::Zeitueberschreitung)),
            Fehler::Zeitueberschreitung
        );
        // Sockets
        use crate::netz::socket::SocketFehler as S;
        assert_eq!(Fehler::von_socket(S::UngueltigerHandle), Fehler::UngueltigerHandle);
        assert_eq!(Fehler::von_socket(S::NichtVerbunden), Fehler::NichtVerbunden);
        assert_eq!(Fehler::von_socket(S::KeinGeraet), Fehler::KeinGeraet);
        // DNS
        use crate::netz::dns::DnsFehler as D;
        assert_eq!(Fehler::von_dns(D::NichtGefunden), Fehler::NichtGefunden);
        assert_eq!(Fehler::von_dns(D::KeinDnsServer), Fehler::NichtKonfiguriert);
        // Zeiger-Fehler werden GROB abgebildet (kein Kernel-Zustand nach
        // außen) — nur die Längen-Überschreitung ist unterscheidbar.
        assert_eq!(Fehler::von_copy(CopyFehler::ZuGross), Fehler::ZuGross);
        for fehler in [
            CopyFehler::Ueberlauf,
            CopyFehler::AusserhalbUserBereich,
            CopyFehler::NichtGemappt,
            CopyFehler::KernelSpeicher,
            CopyFehler::NichtBeschreibbar,
            CopyFehler::FalscherAdressraum,
        ] {
            assert_eq!(Fehler::von_copy(fehler), Fehler::UngueltigerZeiger);
        }
        // Jeder Fehler hat eine Meldung (keine leeren Strings).
        for fehler in [Fehler::Ok, Fehler::UnbekannterSyscall, Fehler::Belegt] {
            assert!(!fehler.meldung().is_empty());
        }
    }

    /// Pfad-Argumente: Deckel, Nullängen und ungültige Zeiger — alles Fehler,
    /// nie eine Panik. (Der Erfolgsfall braucht einen echten Prozess-
    /// Adressraum und steckt in tests/syscalls.rs.)
    #[test_case]
    fn test_pfad_lesen_boesartig() {
        // Länge 0 und Länge über dem Deckel:
        assert_eq!(pfad_lesen(crate::adressraum::USER_START, 0), Err(Fehler::UngueltigerPfad));
        assert_eq!(
            pfad_lesen(crate::adressraum::USER_START, MAX_PFAD as u64 + 1),
            Err(Fehler::ZuGross)
        );
        assert_eq!(pfad_lesen(crate::adressraum::USER_START, u64::MAX), Err(Fehler::ZuGross));
        // Kernel-Adresse als Pfad-Zeiger:
        assert_eq!(
            pfad_lesen(crate::allocator::HEAP_START as u64, 8),
            Err(Fehler::UngueltigerZeiger)
        );
        // Nullzeiger:
        assert_eq!(pfad_lesen(0, 8), Err(Fehler::UngueltigerZeiger));
        // Ungemappte User-Adresse (kein Prozess aktiv):
        crate::adressraum::kernel_aktivieren();
        assert_eq!(
            pfad_lesen(crate::adressraum::USER_START + 0x5000, 8),
            Err(Fehler::UngueltigerZeiger)
        );
        // Puffer-Deckel:
        assert_eq!(
            puffer_lesen(crate::adressraum::USER_START, MAX_PUFFER as u64 + 1),
            Err(Fehler::ZuGross)
        );
        assert_eq!(laenge_pruefen(MAX_PUFFER as u64), Ok(MAX_PUFFER));
        assert_eq!(laenge_pruefen(MAX_PUFFER as u64 + 1), Err(Fehler::ZuGross));
    }
}
