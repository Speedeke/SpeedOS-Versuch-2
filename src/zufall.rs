// zufall.rs — Der Zufallsgenerator von SpeedOS (Serie 7, Teil 1)
//
// Der Entwurf steht VOR diesem Code in docs/zufall.md — dort ist begruendet,
// WARUM es so aussieht, wie viel Entropie wir welcher Quelle zubilligen und
// was im unangenehmen Fall passiert. Hier steht, WIE.
//
// ==========================================================================
// DIE KETTE
//
//   IRQ-Handler (Timer, Tastatur, Maus, Netz, Platte)
//        |  einspeisen(Quelle)   <- nur Atomics: kein Lock, keine Allokation
//        v
//   ENTROPIE-POOL  [AtomicU64; 32]   (fetch_xor in einen Ring)
//        |  pool_falten()  <- XOR-Faltung auf 48 Byte, dann EINE ChaCha20-Runde
//        v
//   DRBG  Schluessel [u8;32] + Zaehler u64        <- BLATT-Lock
//        |  fast key erasure: die ersten 32 Byte jedes Aufrufs
//        |                    sind der NEUE Schluessel
//        v
//   fuellen(&mut [u8])  ->  Syscall zufall(ptr,len)  /  Shell `zufall`
//
// ==========================================================================
// DREI REGELN, DIE HIER NICHT VERHANDELBAR SIND
//
//  (1) NIE AUS EINER QUELLE ALLEIN. Auch wenn RDSEED vorhanden ist, wird
//      gemischt. Der Grund ist nicht Misstrauen um seiner selbst willen: Was
//      der Rauschgenerator einer CPU tut, koennen wir nicht nachpruefen, und
//      es gab reale Errata (AMD nach S3: dauerhaft 0xFFFFFFFF, mit gesetztem
//      Carry-Flag, also als "gueltig" gemeldet). Weil per XOR eingemischt
//      wird, kann eine defekte Quelle die anderen nicht verschlechtern —
//      sie traegt dann eben nichts bei.
//
//  (2) SALZ IST KEINE ENTROPIE. RTC-Zeit, Boot-TSC und Speicher-Layout
//      werden eingemischt, aber mit NULL angerechneten Bits: Ein Angreifer
//      kennt sie (die Bootzeit steht in jeder Logzeile). Sie trennen zwei
//      identische Rechner voneinander — mehr nicht, und das ist genau die
//      Definition von Salz.
//
//  (3) LIEBER WARTEN ALS SCHWACH. Ist der Pool nicht gesaet, gibt es KEINE
//      Bytes. Ein "geht schon irgendwie"-Fallback waere die schlimmste
//      Loesung: Der Aufrufer kann Zufall nicht auf Qualitaet pruefen, baut
//      daraus einen Schluessel, und der Fehler bleibt fuer immer still.
//
// ==========================================================================
// LOCK-DISZIPLIN
//
// Der DRBG-Mutex ist ein BLATT-Lock (nimmt keinen weiteren) und wird
// AUSSCHLIESSLICH mit ausgeschalteten Interrupts gehalten (`mit_drbg`).
// Damit ist er aus einem Syscall gefahrlos benutzbar — wenn der Syscall
// laeuft, haelt ihn niemand (Lock-Disziplin in syscall/mod.rs (a)).
// Die INTERRUPT-Seite (`einspeisen`) fasst ihn NIE an: Sie schreibt nur
// Atomics. Genau deshalb darf sie in jedem Handler stehen.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

// ===========================================================================
// (A) ChaCha20 — die Blockfunktion nach RFC 8439
//
// SELBST GESCHRIEBEN, und das ist KEIN Widerspruch zur Absage an
// Eigenbau-Krypto aus docs/serie7-bestandsaufnahme.md. Der Unterschied ist
// die PRUEFBARKEIT: TLS kann man nicht testen, weil der Test ein Angreifer
// waere. Diese Permutation dagegen ist 40 Zeilen, auf einer RFC-Seite
// vollstaendig spezifiziert und bitgenau gegen Testvektoren pruefbar — ein
// Fehler in einer Rotation, einer Addition oder der Byte-Reihenfolge laesst
// KEIN EINZIGES Byte stimmen (siehe test_chacha20_rfc8439_vektoren).
//
// Und sie ist von selbst SEITENKANALFREI: nur Add/XOR/Rotate auf FESTEN
// Indizes. Kein Zweig und kein Speicherzugriff haengt vom Schluessel ab.
// (Eine AES-Software-Implementierung mit S-Box-Tabellen haette genau dort
// die klassische Cache-Timing-Luecke.)
// ===========================================================================

/// Die vier Konstanten "expand 32-byte k" (RFC 8439 §2.3).
const CHACHA_KONSTANTEN: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// Die QUARTER ROUND (RFC 8439 §2.1) — der einzige Baustein von ChaCha20.
/// Vier Additionen, vier XOR, vier Rotationen. Mehr ist es nicht.
#[inline(always)]
fn viertelrunde(z: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    z[a] = z[a].wrapping_add(z[b]);
    z[d] = (z[d] ^ z[a]).rotate_left(16);
    z[c] = z[c].wrapping_add(z[d]);
    z[b] = (z[b] ^ z[c]).rotate_left(12);
    z[a] = z[a].wrapping_add(z[b]);
    z[d] = (z[d] ^ z[a]).rotate_left(8);
    z[c] = z[c].wrapping_add(z[d]);
    z[b] = (z[b] ^ z[c]).rotate_left(7);
}

/// Der ChaCha20-Zustand als 16 Worte (RFC 8439 §2.3):
/// 4 Konstanten | 8 Schluessel | 1 Blockzaehler | 3 Nonce.
fn chacha20_zustand(schluessel: &[u8; 32], zaehler: u32, nonce: &[u8; 12]) -> [u32; 16] {
    let mut z = [0u32; 16];
    z[..4].copy_from_slice(&CHACHA_KONSTANTEN);
    for i in 0..8 {
        z[4 + i] = u32::from_le_bytes([
            schluessel[i * 4],
            schluessel[i * 4 + 1],
            schluessel[i * 4 + 2],
            schluessel[i * 4 + 3],
        ]);
    }
    z[12] = zaehler;
    for i in 0..3 {
        z[13 + i] = u32::from_le_bytes([
            nonce[i * 4],
            nonce[i * 4 + 1],
            nonce[i * 4 + 2],
            nonce[i * 4 + 3],
        ]);
    }
    z
}

/// 20 Runden = 10 Doppelrunden (je 4 Spalten- und 4 Diagonal-Viertelrunden).
fn chacha20_runden(zustand: &[u32; 16]) -> [u32; 16] {
    let mut w = *zustand;
    for _ in 0..10 {
        // Spalten
        viertelrunde(&mut w, 0, 4, 8, 12);
        viertelrunde(&mut w, 1, 5, 9, 13);
        viertelrunde(&mut w, 2, 6, 10, 14);
        viertelrunde(&mut w, 3, 7, 11, 15);
        // Diagonalen
        viertelrunde(&mut w, 0, 5, 10, 15);
        viertelrunde(&mut w, 1, 6, 11, 12);
        viertelrunde(&mut w, 2, 7, 8, 13);
        viertelrunde(&mut w, 3, 4, 9, 14);
    }
    // Der Ausgangszustand wird ADDIERT — ohne das waere die Funktion
    // umkehrbar und damit als Generator wertlos.
    let mut aus = [0u32; 16];
    for i in 0..16 {
        aus[i] = w[i].wrapping_add(zustand[i]);
    }
    aus
}

/// Ein ChaCha20-Block: 64 Byte Keystream, little-endian serialisiert.
fn chacha20_block(schluessel: &[u8; 32], zaehler: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let worte = chacha20_runden(&chacha20_zustand(schluessel, zaehler, nonce));
    let mut aus = [0u8; 64];
    for (i, wort) in worte.iter().enumerate() {
        aus[i * 4..i * 4 + 4].copy_from_slice(&wort.to_le_bytes());
    }
    aus
}

/// Ueberschreibt einen Puffer WIRKLICH — `write_volatile`, damit der
/// Optimierer den Schreibvorgang nicht als "wird nie gelesen" wegwirft.
/// Ohne das bliebe ein verbrauchter Schluessel im Speicher stehen, und die
/// Vorwaerts-Sicherheit der Key-Erasure waere nur behauptet.
fn loeschen(puffer: &mut [u8]) {
    for byte in puffer.iter_mut() {
        // unsafe: `byte` ist eine gueltige, ausgerichtete, exklusiv gehaltene
        // Referenz auf ein u8 — write_volatile darauf ist immer erlaubt.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
}

// ===========================================================================
// (B) DIE QUELLEN
// ===========================================================================

/// Woher eine Entropie-Probe stammt. Die Reihenfolge ist der Index in den
/// Statistik-Feldern — Anhaengen ist in Ordnung, Umsortieren nicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quelle {
    /// PIT-Timer (IRQ 0) — die SCHWAECHSTE Quelle, siehe `bits_je_probe`.
    Pit,
    /// Tastatur (IRQ 1) — ein Mensch tippt.
    Tastatur,
    /// PS/2-Maus (IRQ 12) — ein Mensch bewegt.
    Maus,
    /// Netz-Empfang (virtio-net) — eine fremde Gegenstelle sendet.
    Netz,
    /// Platten-Antwort (ATA/virtio-blk).
    Platte,
    /// RDSEED/RDRAND.
    Hardware,
    /// Boot-Salz (RTC, TSC, Layout) — traegt NULL Bits bei.
    Salz,
}

/// Wie viele Quellen es gibt (Groesse der Statistik-Felder).
pub const ANZAHL_QUELLEN: usize = 7;

impl Quelle {
    /// Index in den Statistik-Feldern.
    pub fn index(self) -> usize {
        match self {
            Quelle::Pit => 0,
            Quelle::Tastatur => 1,
            Quelle::Maus => 2,
            Quelle::Netz => 3,
            Quelle::Platte => 4,
            Quelle::Hardware => 5,
            Quelle::Salz => 6,
        }
    }

    /// Alle Quellen in Index-Reihenfolge (fuer die Anzeige).
    pub fn alle() -> [Quelle; ANZAHL_QUELLEN] {
        [
            Quelle::Pit,
            Quelle::Tastatur,
            Quelle::Maus,
            Quelle::Netz,
            Quelle::Platte,
            Quelle::Hardware,
            Quelle::Salz,
        ]
    }

    /// Anzeigename (Shell).
    pub fn name(self) -> &'static str {
        match self {
            Quelle::Pit => "PIT-Timer",
            Quelle::Tastatur => "Tastatur",
            Quelle::Maus => "Maus",
            Quelle::Netz => "Netz-RX",
            Quelle::Platte => "Platte",
            Quelle::Hardware => "RDSEED/RDRAND",
            Quelle::Salz => "Boot-Salz",
        }
    }

    /// WIE VIELE BITS EINE PROBE DIESER QUELLE WERT IST.
    ///
    /// Diese Zahlen sind BEGRUENDETE UNTERTREIBUNGEN, keine Messungen —
    /// docs/zufall.md §3 rechnet sie vor. Die Regel dahinter: Zu wenig
    /// anzurechnen kostet Wartezeit beim Boot, zu viel anzurechnen kostet
    /// Sicherheit, ohne dass es jemand merkt.
    ///
    /// Der PIT bekommt hier 1 Bit, wird aber zusaetzlich nur bei JEDER
    /// ACHTEN Probe angerechnet (siehe `einspeisen`): Ein 250-Hz-Timer
    /// tickt regelmaessig; unvorhersagbar ist allein die Interrupt-Latenz.
    /// Mit voller Anrechnung waere die Schwelle nach einer Sekunde erreicht
    /// — eine Zahl, die gut aussieht und nichts bedeutet.
    pub fn bits_je_probe(self) -> u32 {
        match self {
            Quelle::Tastatur => 4,
            Quelle::Maus => 3,
            Quelle::Netz => 2,
            Quelle::Platte => 2,
            Quelle::Pit => 1,
            // Ein 64-Bit-Hardwarewert waere voll anrechenbar; der Deckel in
            // `hardware_einmischen` sorgt dafuer, dass NIE allein aus der
            // Hardware gesaet wird.
            Quelle::Hardware => 64,
            // SALZ IST KEINE ENTROPIE (Regel 2 im Kopf dieser Datei).
            Quelle::Salz => 0,
        }
    }

    /// Nur jede n-te Probe wird angerechnet (1 = jede).
    fn anrechnung_jede(self) -> u32 {
        match self {
            Quelle::Pit => 8,
            _ => 1,
        }
    }
}

// ===========================================================================
// (C) DER ENTROPIE-POOL
// ===========================================================================

/// Groesse des Pools in 64-Bit-Worten (256 Byte).
const POOL_WORTE: usize = 32;

/// Der Pool. `fetch_xor` in einen Ring: lock-frei, allokationsfrei, und
/// AKKUMULIEREND statt ueberschreibend — zwei Proben auf demselben Platz
/// loeschen einander nicht aus, sie mischen sich.
static POOL: [AtomicU64; POOL_WORTE] = [const { AtomicU64::new(0) }; POOL_WORTE];
/// Schreibposition im Ring.
static POOL_INDEX: AtomicUsize = AtomicUsize::new(0);

/// Geschaetzte Entropie im Pool, in Bit.
static ENTROPIE_BITS: AtomicU32 = AtomicU32::new(0);
/// Bits, die seit dem letzten Nachsaeen dazugekommen sind.
static BITS_SEIT_NACHSAAT: AtomicU32 = AtomicU32::new(0);

/// Ab hier gilt der Generator als GESAET. 256 Bit, weil der DRBG-Schluessel
/// 256 Bit hat — mehr zu sammeln braeuchte niemand, weniger waere ein
/// kuerzerer Schluessel als beworben.
pub const SCHWELLE_BITS: u32 = 256;
/// Deckel fuer die Schaetzung. Ein Mausbewegungs-Sturm soll nicht den
/// Eindruck erwecken, wir haetten Kilobit an Entropie.
const MAX_BITS: u32 = 4096;
/// So viele neue Bits loesen ein Nachsaeen vor dem naechsten `fuellen` aus.
const NACHSAAT_BITS: u32 = 64;

/// Letzter TSC-Wert je Quelle (fuer die Differenz).
static LETZTER_TSC: [AtomicU64; ANZAHL_QUELLEN] =
    [const { AtomicU64::new(0) }; ANZAHL_QUELLEN];
/// Letzte DIFFERENZ je Quelle (fuer den Wiederholungs-Test).
static LETZTE_DIFFERENZ: [AtomicU64; ANZAHL_QUELLEN] =
    [const { AtomicU64::new(0) }; ANZAHL_QUELLEN];
/// Proben je Quelle (Statistik + Anrechnungs-Takt).
static PROBEN: [AtomicU32; ANZAHL_QUELLEN] = [const { AtomicU32::new(0) }; ANZAHL_QUELLEN];

/// Ist der Generator gesaet?
static GESAET: AtomicBool = AtomicBool::new(false);
/// Wie oft wurde nachgesaet.
static NACHSAATEN: AtomicU64 = AtomicU64::new(0);
/// Wie viele Bytes wurden insgesamt ausgegeben.
static AUSGEGEBEN: AtomicU64 = AtomicU64::new(0);

/// SPEIST EINE PROBE EIN — aufrufbar aus JEDEM Interrupt-Handler.
///
/// Was hier passiert und was NICHT: Es werden ausschliesslich Atomics
/// angefasst. Kein Lock, keine Allokation, keine Ausgabe — die
/// Interrupt-Handler-Regel dieses Projekts (Deadlock-Regel 2). Die Kosten
/// sind ein `rdtsc` (~20 Zyklen) und drei atomare Operationen.
///
/// DER WIEDERHOLUNGS-TEST: Liefert eine Quelle zweimal hintereinander
/// EXAKT dieselbe TSC-Differenz, ist sie in diesem Moment ein Zaehler und
/// kein Rauschen. Die Probe wird trotzdem eingemischt (schaden kann sie
/// nicht), aber NICHT angerechnet. Das ist der einfachste denkbare
/// Gesundheitstest, angelehnt an den Repetition Count Test aus
/// NIST SP 800-90B.
pub fn einspeisen(quelle: Quelle) {
    let jetzt = crate::zeit::tsc_roh();
    einspeisen_wert(quelle, jetzt);
}

/// Wie `einspeisen`, aber mit einem ausdruecklichen Wert (Hardware-Quelle,
/// Salz, Tests).
fn einspeisen_wert(quelle: Quelle, wert: u64) {
    let index = quelle.index();
    let vorher = LETZTER_TSC[index].swap(wert, Ordering::Relaxed);
    let differenz = wert.wrapping_sub(vorher);
    let proben = PROBEN[index].fetch_add(1, Ordering::Relaxed);

    // Ins Pool-Wort mischen. Die Quelle geht in die oberen Bits ein, damit
    // zwei Quellen mit zufaellig gleicher Differenz sich nicht ausloeschen.
    let platz = POOL_INDEX.fetch_add(1, Ordering::Relaxed) % POOL_WORTE;
    let beitrag = differenz ^ ((index as u64) << 56) ^ wert.rotate_left(17);
    POOL[platz].fetch_xor(beitrag, Ordering::Relaxed);

    // --- Anrechnung, konservativ ---
    // Erste Probe einer Quelle: Es gibt noch keine Differenz, nur einen
    // absoluten Wert. Nicht anrechnen.
    if vorher == 0 {
        return;
    }
    // Wiederholungs-Test.
    let letzte = LETZTE_DIFFERENZ[index].swap(differenz, Ordering::Relaxed);
    if letzte == differenz {
        return;
    }
    // Anrechnungs-Takt (der PIT nur jede 8. Probe).
    let takt = quelle.anrechnung_jede();
    if takt > 1 && !proben.is_multiple_of(takt) {
        return;
    }
    bits_gutschreiben(quelle.bits_je_probe());
}

/// Schreibt Entropie-Bits gut (gedeckelt) und weckt den Zustandswechsel
/// "gesaet", sobald die Schwelle erreicht ist.
fn bits_gutschreiben(bits: u32) {
    if bits == 0 {
        return;
    }
    // `fetch_add` + `fetch_min` statt einer Lese-Aendere-Schreibe-Schleife:
    // Beides sind einzelne atomare Operationen, also im Interrupt-Handler
    // erlaubt und garantiert fortschrittssicher (eine CAS-Schleife koennte
    // theoretisch drehen). Der Deckel greift eine Anweisung spaeter — ein
    // kurzzeitiges Ueberschiessen um wenige Bit ist bedeutungslos.
    ENTROPIE_BITS.fetch_add(bits, Ordering::Relaxed);
    ENTROPIE_BITS.fetch_min(MAX_BITS, Ordering::Relaxed);
    BITS_SEIT_NACHSAAT.fetch_add(bits, Ordering::Relaxed);
    BITS_SEIT_NACHSAAT.fetch_min(MAX_BITS, Ordering::Relaxed);
}

/// Mischt beliebige Bytes ein — OHNE Anrechnung, wenn `quelle` `Salz` ist.
///
/// Das ist der Weg fuer Boot-Salz und Hardware-Werte: Beides sind fertige
/// Bytes, keine Zeitstempel.
pub fn einspeisen_bytes(quelle: Quelle, daten: &[u8]) {
    for brocken in daten.chunks(8) {
        let mut wort = [0u8; 8];
        wort[..brocken.len()].copy_from_slice(brocken);
        let wert = u64::from_le_bytes(wort);
        let platz = POOL_INDEX.fetch_add(1, Ordering::Relaxed) % POOL_WORTE;
        POOL[platz].fetch_xor(wert ^ ((quelle.index() as u64) << 56), Ordering::Relaxed);
    }
    PROBEN[quelle.index()].fetch_add(1, Ordering::Relaxed);
}

/// FALTET DEN POOL auf 48 Byte und verdichtet sie mit einer ChaCha20-Runde
/// zu 32 Byte Saat-Material.
///
/// Die Faltung ist ein XOR mit index-abhaengiger Rotation: Sie kann keine
/// Entropie erzeugen, verliert aber auch keine, solange das Ergebnis breiter
/// ist als die enthaltene Entropie (48 Byte gegen hoechstens 4096 gedeckelte
/// Bit — passt). Die Rotation sorgt dafuer, dass zwei gleiche Worte an
/// verschiedenen Positionen sich nicht ausloeschen.
///
/// Die ChaCha20-Runde danach ist die eigentliche Verdichtung: Sie verteilt
/// jedes Eingangsbit ueber den ganzen Ausgang. Sie ist KEINE Hashfunktion im
/// strengen Sinn — fuer die Aufgabe (Saat fuer den eigenen DRBG, nicht
/// Ausgabe nach aussen) reicht die Diffusion der Permutation.
fn pool_falten() -> [u8; 32] {
    let mut kern = [0u64; 6]; // 48 Byte: 32 Schluessel + 12 Nonce + 4 Zaehler
    for i in 0..POOL_WORTE {
        let wort = POOL[i].load(Ordering::Relaxed);
        kern[i % 6] ^= wort.rotate_left((i as u32 * 7) % 64);
    }
    let mut roh = [0u8; 48];
    for (i, wort) in kern.iter().enumerate() {
        roh[i * 8..i * 8 + 8].copy_from_slice(&wort.to_le_bytes());
    }
    let mut schluessel = [0u8; 32];
    schluessel.copy_from_slice(&roh[..32]);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&roh[32..44]);
    let zaehler = u32::from_le_bytes([roh[44], roh[45], roh[46], roh[47]]);

    let block = chacha20_block(&schluessel, zaehler, &nonce);
    let mut saat = [0u8; 32];
    saat.copy_from_slice(&block[..32]);
    loeschen(&mut schluessel);
    loeschen(&mut roh);
    saat
}

// ===========================================================================
// (D) DIE HARDWARE-QUELLE
// ===========================================================================

/// Ist RDSEED verfuegbar (CPUID.07H:EBX[18])?
static HAT_RDSEED: AtomicBool = AtomicBool::new(false);
/// Ist RDRAND verfuegbar (CPUID.01H:ECX[30])?
static HAT_RDRAND: AtomicBool = AtomicBool::new(false);
/// Wurde die Hardware-Quelle als DEFEKT erkannt und abgeschaltet?
static HARDWARE_DEFEKT: AtomicBool = AtomicBool::new(false);

/// Wie oft eine Hardware-Instruktion hoechstens wiederholt wird, bevor wir
/// aufgeben. RDSEED/RDRAND setzen CF=0, wenn gerade kein Wert bereitsteht —
/// wer den Rueckgabewert NICHT prueft, liest den unveraenderten
/// Registerinhalt und haelt ihn fuer Zufall.
const HARDWARE_VERSUCHE: u32 = 16;

/// Prueft die CPUID-Bits fuer RDSEED und RDRAND.
///
/// `__cpuid` ist auf x86_64 eine sichere Funktion (CPUID gibt es seit dem
/// 486, das Ziel-Target garantiert sie) — deshalb steht hier kein
/// unsafe-Block.
fn hardware_erkennen() {
    let (rdrand, rdseed) = {
        let blatt1 = core::arch::x86_64::__cpuid(1);
        let rdrand = blatt1.ecx & (1 << 30) != 0;
        // Blatt 7 gibt es erst ab einer Mindest-CPUID-Stufe — sonst liefert
        // es Muell. Also erst das Maximum abfragen.
        let max = core::arch::x86_64::__cpuid(0).eax;
        let rdseed = if max >= 7 {
            core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0
        } else {
            false
        };
        (rdrand, rdseed)
    };
    HAT_RDRAND.store(rdrand, Ordering::Relaxed);
    HAT_RDSEED.store(rdseed, Ordering::Relaxed);
}

/// Ein 64-Bit-Wert aus RDSEED (rohes Rauschen). `None`, wenn nicht
/// verfuegbar oder wenn die Hardware nach `HARDWARE_VERSUCHE` nichts liefert.
fn rdseed64() -> Option<u64> {
    if !HAT_RDSEED.load(Ordering::Relaxed) {
        return None;
    }
    for _ in 0..HARDWARE_VERSUCHE {
        let mut wert: u64 = 0;
        // unsafe: `rdseed64_step` ist genau fuer diesen Zweck da; wir haben
        // das CPUID-Bit geprueft. Es schreibt nur in `wert` und liefert 1
        // bei Erfolg — der Rueckgabewert MUSS geprueft werden.
        let ok = unsafe { core::arch::x86_64::_rdseed64_step(&mut wert) };
        if ok == 1 {
            return Some(wert);
        }
        core::hint::spin_loop();
    }
    None
}

/// Ein 64-Bit-Wert aus RDRAND (hardware-DRBG, aus RDSEED gesaet).
fn rdrand64() -> Option<u64> {
    if !HAT_RDRAND.load(Ordering::Relaxed) {
        return None;
    }
    for _ in 0..HARDWARE_VERSUCHE {
        let mut wert: u64 = 0;
        // unsafe: siehe rdseed64 — CPUID-Bit geprueft, Rueckgabewert geprueft.
        let ok = unsafe { core::arch::x86_64::_rdrand64_step(&mut wert) };
        if ok == 1 {
            return Some(wert);
        }
        core::hint::spin_loop();
    }
    None
}

/// Holt einen Hardware-Wert: RDSEED bevorzugt (rohes Rauschen), sonst RDRAND.
fn hardware_wert() -> Option<u64> {
    if HARDWARE_DEFEKT.load(Ordering::Relaxed) {
        return None;
    }
    rdseed64().or_else(rdrand64)
}

/// GESUNDHEITSPRUEFUNG der Hardware-Quelle.
///
/// Zieht acht Werte und lehnt die Quelle dauerhaft ab, wenn sie
/// offensichtlich kaputt ist. Der konkrete Anlass ist ein reales Erratum:
/// AMD-CPUs lieferten nach dem Aufwachen aus S3 dauerhaft `0xFFFF_FFFF` —
/// MIT gesetztem Carry-Flag, also als „gueltig" gemeldet. Ohne diese
/// Pruefung haette ein System das als Zufall verarbeitet.
///
/// Liefert `true`, wenn die Quelle brauchbar aussieht.
fn hardware_gesund() -> bool {
    let mut werte = [0u64; 8];
    for platz in werte.iter_mut() {
        match hardware_wert() {
            Some(wert) => *platz = wert,
            // Liefert die Hardware nicht einmal acht Werte, ist sie fuer uns
            // keine Quelle.
            None => return false,
        }
    }
    // Alle gleich? Dann ist es keine Quelle, sondern eine Konstante.
    if werte.iter().all(|w| *w == werte[0]) {
        return false;
    }
    // Die beiden bekannten Ausfall-Muster duerfen nicht mehrfach auftreten.
    let nullen = werte.iter().filter(|w| **w == 0).count();
    let einsen = werte.iter().filter(|w| **w == u64::MAX).count();
    if nullen > 1 || einsen > 1 {
        return false;
    }
    true
}

/// Mischt Hardware-Zufall in den Pool.
///
/// DER DECKEL IST DER PUNKT: Angerechnet wird hoechstens die HALBE Schwelle.
/// Dadurch kann der Generator NIE allein aus der Hardware gesaet werden — es
/// muessen immer mindestens 128 Bit aus Interrupt-Jitter dazukommen. Genau
/// das ist Regel 1 im Kopf dieser Datei, in eine Zahl gegossen.
fn hardware_einmischen() -> u32 {
    let mut angerechnet = 0u32;
    let deckel = SCHWELLE_BITS / 2;
    for _ in 0..8 {
        match hardware_wert() {
            Some(wert) => {
                einspeisen_bytes(Quelle::Hardware, &wert.to_le_bytes());
                if angerechnet < deckel {
                    let bits = Quelle::Hardware.bits_je_probe().min(deckel - angerechnet);
                    angerechnet += bits;
                }
            }
            None => break,
        }
    }
    if angerechnet > 0 {
        bits_gutschreiben(angerechnet);
    }
    angerechnet
}

// ===========================================================================
// (E) DER DRBG
// ===========================================================================

/// Der Generator-Zustand: ein 256-Bit-Schluessel und ein Aufruf-Zaehler.
struct Drbg {
    schluessel: [u8; 32],
    zaehler: u64,
}

static DRBG: Mutex<Drbg> = Mutex::new(Drbg {
    schluessel: [0; 32],
    zaehler: 0,
});

/// Zugriff auf den DRBG — IMMER mit ausgeschalteten Interrupts.
///
/// Das ist nicht Kosmetik, sondern die Bedingung dafuer, dass ein SYSCALL
/// diesen Lock gefahrlos nehmen darf: Wenn der Syscall laeuft (Interrupts
/// aus), kann ihn niemand halten (syscall/mod.rs, Lock-Disziplin (a)).
fn mit_drbg<T>(f: impl FnOnce(&mut Drbg) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut DRBG.lock()))
}

/// Die Nonce eines Aufrufs: der Aufrufzaehler, little-endian, mit vier
/// Null-Bytes aufgefuellt.
fn nonce_aus(zaehler: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&zaehler.to_le_bytes());
    nonce
}

impl Drbg {
    /// FAST KEY ERASURE — der Kern der Vorwaerts-Sicherheit.
    ///
    /// Aus dem aktuellen Schluessel wird ein Keystream erzeugt; dessen
    /// ERSTE 32 BYTE werden der NEUE Schluessel, der Rest ist Ausgabe. Der
    /// alte Schluessel wird ueberschrieben.
    ///
    /// Was das kauft: Wer den Kernel-Speicher SPAETER liest, kann FRUEHERE
    /// Ausgaben nicht rekonstruieren — die Schluessel dafuer existieren
    /// nicht mehr. Ohne diesen Schritt koennte ein einziger spaeterer
    /// Speicherabzug rueckwirkend jeden je erzeugten Sitzungsschluessel
    /// offenlegen.
    fn erzeugen(&mut self, ziel: &mut [u8]) {
        self.zaehler = self.zaehler.wrapping_add(1);
        let nonce = nonce_aus(self.zaehler);
        // Der ALTE Schluessel erzeugt den gesamten Keystream dieses Aufrufs.
        let mut alt = self.schluessel;

        // Block 0: erste Haelfte = neuer Schluessel, zweite Haelfte = Ausgabe.
        let block0 = chacha20_block(&alt, 0, &nonce);
        self.schluessel.copy_from_slice(&block0[..32]);

        let erste = ziel.len().min(32);
        ziel[..erste].copy_from_slice(&block0[32..32 + erste]);
        let mut geschrieben = erste;

        // Weitere Bloecke, falls mehr als 32 Byte gefragt sind.
        let mut block_nr: u32 = 1;
        while geschrieben < ziel.len() {
            let block = chacha20_block(&alt, block_nr, &nonce);
            let rest = (ziel.len() - geschrieben).min(64);
            ziel[geschrieben..geschrieben + rest].copy_from_slice(&block[..rest]);
            geschrieben += rest;
            block_nr = block_nr.wrapping_add(1);
        }
        loeschen(&mut alt);
    }

    /// Mischt Saat-Material in den Schluessel.
    ///
    /// XOR und nicht "ersetzen" — DAS ist die Eigenschaft, auf der die ganze
    /// "nie nur eine Quelle"-Regel steht: Eine SCHLECHTE neue Saat kann den
    /// bestehenden Zustand nicht verschlechtern. Sie traegt dann eben nichts
    /// bei, statt etwas kaputtzumachen.
    fn saeen(&mut self, material: &[u8; 32]) {
        for (byte, frisch) in self.schluessel.iter_mut().zip(material.iter()) {
            *byte ^= *frisch;
        }
        // Ein Key-Erasure-Schritt zum Diffundieren: Danach haengt jedes Bit
        // des Schluessels von jedem Bit des Materials ab.
        let mut weg = [0u8; 32];
        self.erzeugen(&mut weg);
        loeschen(&mut weg);
    }
}

/// NACHSAEEN: Pool falten, in den DRBG mischen, Zaehler zuruecksetzen.
///
/// Liefert `true`, wenn der Generator dadurch (erstmals) gesaet ist.
pub fn nachsaeen() -> bool {
    // Hardware IMMER mitnehmen, auch wenn der Pool schon voll ist: Zwei
    // Quellen sind besser als eine, und es kostet nichts.
    hardware_einmischen();

    let saat = pool_falten();
    mit_drbg(|drbg| drbg.saeen(&saat));
    let mut saat = saat;
    loeschen(&mut saat);

    NACHSAATEN.fetch_add(1, Ordering::Relaxed);
    BITS_SEIT_NACHSAAT.store(0, Ordering::Relaxed);

    let bits = ENTROPIE_BITS.load(Ordering::Relaxed);
    if bits >= SCHWELLE_BITS && !GESAET.swap(true, Ordering::SeqCst) {
        crate::serial_println!(
            "[ZUFALL] gesaet: {} Bit Entropie beisammen (Schwelle {}).",
            bits,
            SCHWELLE_BITS
        );
        return true;
    }
    GESAET.load(Ordering::Relaxed)
}

// ===========================================================================
// (F) DIE OEFFENTLICHE API
// ===========================================================================

/// Warum `fuellen` fehlgeschlagen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZufallFehler {
    /// Der Pool ist noch nicht ausreichend gesaet. **KEIN Fallback** — es
    /// gibt in diesem Zustand keine Bytes, siehe Regel 3 im Kopf.
    NichtGesaet,
}

impl ZufallFehler {
    pub fn meldung(self) -> &'static str {
        match self {
            ZufallFehler::NichtGesaet => "Zufallsgenerator noch nicht ausreichend gesaet",
        }
    }
}

/// Ist der Generator gesaet?
pub fn bereit() -> bool {
    GESAET.load(Ordering::Relaxed)
}

/// FUELLT einen Puffer mit kryptographisch brauchbarem Zufall.
///
/// `Err(NichtGesaet)`, solange die Schwelle nicht erreicht ist — und dann
/// bleibt der Puffer UNVERAENDERT. Wer hier im Fehlerfall etwas
/// zurueckliesse, haette den Fallback wieder eingebaut, den Regel 3
/// ausschliesst.
pub fn fuellen(ziel: &mut [u8]) -> Result<(), ZufallFehler> {
    if !bereit() {
        // Vielleicht ist die Schwelle inzwischen erreicht und es fehlte nur
        // das Nachsaeen — einmal versuchen, dann ehrlich absagen.
        if ENTROPIE_BITS.load(Ordering::Relaxed) < SCHWELLE_BITS || !nachsaeen() {
            return Err(ZufallFehler::NichtGesaet);
        }
    }
    // Genug Neues angefallen? Dann vorher nachsaeen.
    if BITS_SEIT_NACHSAAT.load(Ordering::Relaxed) >= NACHSAAT_BITS {
        nachsaeen();
    }
    mit_drbg(|drbg| drbg.erzeugen(ziel));
    AUSGEGEBEN.fetch_add(ziel.len() as u64, Ordering::Relaxed);
    Ok(())
}

/// Eine einzelne Zufallszahl (Bequemlichkeit fuer Kernel-Aufrufer wie
/// TCP-Sequenznummern und ephemere Ports).
pub fn u64_zufall() -> Result<u64, ZufallFehler> {
    let mut bytes = [0u8; 8];
    fuellen(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Momentaufnahme fuer Shell und Tests.
#[derive(Debug, Clone, Copy)]
pub struct ZufallStatus {
    pub gesaet: bool,
    pub entropie_bits: u32,
    pub schwelle_bits: u32,
    pub rdseed: bool,
    pub rdrand: bool,
    pub hardware_defekt: bool,
    /// Proben je Quelle, in der Reihenfolge von `Quelle::alle()`.
    pub proben: [u32; ANZAHL_QUELLEN],
    pub nachsaaten: u64,
    pub ausgegebene_bytes: u64,
}

/// DER STARTBEFUND: der Zustand am Ende von `init()`, festgehalten.
///
/// Warum eine Momentaufnahme und nicht einfach `status()`: Der interessante
/// Zustand („noch nicht gesaet") ist nach wenigen Sekunden nicht mehr
/// herstellbar — der PIT tickt ja weiter. Ein Test, der ihn spaeter pruefen
/// will, findet ihn nie und wuerde stillschweigend nichts pruefen. Also
/// wird er zum einzig richtigen Zeitpunkt festgehalten.
static START_BITS: AtomicU32 = AtomicU32::new(0);
static START_GESAET: AtomicBool = AtomicBool::new(false);
static START_ERFASST: AtomicBool = AtomicBool::new(false);

/// `(Bits, gesaet)` unmittelbar nach `init()`. `None`, wenn `init()` nie lief.
pub fn startbefund() -> Option<(u32, bool)> {
    if !START_ERFASST.load(Ordering::Relaxed) {
        return None;
    }
    Some((
        START_BITS.load(Ordering::Relaxed),
        START_GESAET.load(Ordering::Relaxed),
    ))
}

/// Der aktuelle Zustand des Generators.
pub fn status() -> ZufallStatus {
    let mut proben = [0u32; ANZAHL_QUELLEN];
    for (i, platz) in proben.iter_mut().enumerate() {
        *platz = PROBEN[i].load(Ordering::Relaxed);
    }
    ZufallStatus {
        gesaet: bereit(),
        entropie_bits: ENTROPIE_BITS.load(Ordering::Relaxed),
        schwelle_bits: SCHWELLE_BITS,
        rdseed: HAT_RDSEED.load(Ordering::Relaxed),
        rdrand: HAT_RDRAND.load(Ordering::Relaxed),
        hardware_defekt: HARDWARE_DEFEKT.load(Ordering::Relaxed),
        proben,
        nachsaaten: NACHSAATEN.load(Ordering::Relaxed),
        ausgegebene_bytes: AUSGEGEBEN.load(Ordering::Relaxed),
    }
}

/// RICHTET DEN GENERATOR EIN.
///
/// Gehoert in main.rs NACH `zeit::init()` (wir brauchen die TSC) und VOR
/// allem, was Zufall will. Der Aufruf saet NICHT — er stellt nur fest, was
/// da ist, und legt das Salz hinein.
pub fn init() {
    hardware_erkennen();

    // Hardware-Gesundheit pruefen, BEVOR wir ihr etwas glauben.
    if HAT_RDSEED.load(Ordering::Relaxed) || HAT_RDRAND.load(Ordering::Relaxed) {
        if hardware_gesund() {
            hardware_einmischen();
        } else {
            HARDWARE_DEFEKT.store(true, Ordering::Relaxed);
            crate::serial_println!(
                "[ZUFALL] WARNUNG: RDSEED/RDRAND vorhanden, aber die \
                 Gesundheitspruefung ist fehlgeschlagen — Quelle abgeschaltet."
            );
        }
    }

    // BOOT-SALZ: RTC-Zeit, TSC-Stand, Heap-Adresse. NULL angerechnete Bits
    // (Quelle::Salz), denn ein Angreifer kennt das alles — es trennt nur
    // zwei sonst identische Rechner voneinander (docs/zufall.md §1c).
    let jetzt = crate::zeit::jetzt();
    let salz: [u64; 3] = [
        crate::zeit::sekunden_seit_2000(&jetzt),
        crate::zeit::tsc_roh(),
        crate::allocator::HEAP_START as u64,
    ];
    for wert in salz {
        einspeisen_bytes(Quelle::Salz, &wert.to_le_bytes());
    }

    let s = status();
    crate::serial_println!(
        "[ZUFALL] RDSEED: {}, RDRAND: {}{} — Start mit {} von {} Bit.",
        if s.rdseed { "ja" } else { "nein" },
        if s.rdrand { "ja" } else { "nein" },
        if s.hardware_defekt { " (DEFEKT)" } else { "" },
        s.entropie_bits,
        s.schwelle_bits
    );
    if s.entropie_bits >= SCHWELLE_BITS {
        nachsaeen();
    } else {
        crate::serial_println!(
            "[ZUFALL] Noch nicht gesaet — es fehlen {} Bit aus Interrupt-Jitter. \
             `zufall` zeigt den Stand.",
            SCHWELLE_BITS - s.entropie_bits
        );
    }

    // Den Startbefund festhalten — JETZT, weil der ungesaete Zustand gleich
    // nicht mehr herstellbar ist (siehe `startbefund`).
    START_BITS.store(ENTROPIE_BITS.load(Ordering::Relaxed), Ordering::Relaxed);
    START_GESAET.store(bereit(), Ordering::Relaxed);
    START_ERFASST.store(true, Ordering::SeqCst);
}

/// DER NACHSAAT-TASK: mischt regelmaessig frische Entropie in den DRBG.
///
/// Fuenf Sekunden sind ein Kompromiss: oft genug, dass ein
/// Zustands-Kompromiss nicht lange nutzbar bleibt, selten genug, dass es
/// nicht auffaellt. Nachgesaet wird nur, wenn seither ueberhaupt etwas
/// angefallen ist — sonst waere es Arbeit ohne Gewinn.
pub async fn nachsaat_task() {
    loop {
        crate::zeit::warte_ms(5_000).await;
        if BITS_SEIT_NACHSAAT.load(Ordering::Relaxed) > 0 || !bereit() {
            nachsaeen();
        }
    }
}

// ===========================================================================
// TESTS
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// DER BELASTBARE TEIL: die Blockfunktion gegen RFC 8439.
    ///
    /// Alles andere in dieser Datei ist Argumentation — DIESER Test ist ein
    /// Beweis. Waere auch nur eine Rotation, eine Addition oder die
    /// Little-Endian-Serialisierung falsch, stimmte kein einziges Byte.
    ///
    /// Die Vektoren wurden zusaetzlich UNABHAENGIG gegengeprueft: mit einer
    /// aus der Spezifikation heraus geschriebenen Python-Referenz, die
    /// dieselben Werte liefert (docs/zufall.md §7).
    #[test_case]
    fn test_chacha20_rfc8439_vektoren() {
        // --- RFC 8439 §2.1.1: die Quarter Round einzeln ---
        let mut z = [0u32; 16];
        z[0] = 0x1111_1111;
        z[1] = 0x0102_0304;
        z[2] = 0x9b8d_6f43;
        z[3] = 0x0123_4567;
        viertelrunde(&mut z, 0, 1, 2, 3);
        assert_eq!(z[0], 0xea2a_92f4, "QR a");
        assert_eq!(z[1], 0xcb1c_f8ce, "QR b");
        assert_eq!(z[2], 0x4581_472e, "QR c");
        assert_eq!(z[3], 0x5881_c4bb, "QR d");

        // --- RFC 8439 §2.3.2: die volle Blockfunktion ---
        let mut schluessel = [0u8; 32];
        for (i, byte) in schluessel.iter_mut().enumerate() {
            *byte = i as u8; // 00 01 02 ... 1f
        }
        let nonce: [u8; 12] = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let zaehler = 1u32;

        // Der Zustand am Ende (die 16 Worte aus dem RFC).
        let worte = chacha20_runden(&chacha20_zustand(&schluessel, zaehler, &nonce));
        let erwartete_worte: [u32; 16] = [
            0xe4e7_f110, 0x1559_3bd1, 0x1fdd_0f50, 0xc471_20a3, 0xc7f4_d1c7, 0x0368_c033,
            0x9aaa_2204, 0x4e6c_d4c3, 0x4664_82d2, 0x09aa_9f07, 0x05d7_c214, 0xa202_8bd9,
            0xd19c_12b5, 0xb94e_16de, 0xe883_d0cb, 0x4e3c_50a2,
        ];
        assert_eq!(worte, erwartete_worte, "RFC 8439 2.3.2: Zustand");

        // Und die serialisierten 64 Byte (prueft zusaetzlich die
        // Byte-Reihenfolge — ein Big-Endian-Fehler faellt NUR hier auf).
        let block = chacha20_block(&schluessel, zaehler, &nonce);
        let erwartete_bytes: [u8; 64] = [
            0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20,
            0x71, 0xc4, 0xc7, 0xd1, 0xf4, 0xc7, 0x33, 0xc0, 0x68, 0x03, 0x04, 0x22, 0xaa, 0x9a,
            0xc3, 0xd4, 0x6c, 0x4e, 0xd2, 0x82, 0x64, 0x46, 0x07, 0x9f, 0xaa, 0x09, 0x14, 0xc2,
            0xd7, 0x05, 0xd9, 0x8b, 0x02, 0xa2, 0xb5, 0x12, 0x9c, 0xd1, 0xde, 0x16, 0x4e, 0xb9,
            0xcb, 0xd0, 0x83, 0xe8, 0xa2, 0x50, 0x3c, 0x4e,
        ];
        assert_eq!(block, erwartete_bytes, "RFC 8439 2.3.2: serialisierte Bytes");
    }

    /// Die Blockfunktion muss auf VERSCHIEDENE Eingaben verschieden
    /// reagieren — und der Ausgangszustand muss addiert werden.
    ///
    /// Der zweite Punkt ist der interessante: Ohne die Addition am Ende
    /// waeren die 20 Runden eine PERMUTATION und damit umkehrbar. Wer den
    /// Keystream sieht, koennte den Schluessel zurueckrechnen. Der Test
    /// prueft das indirekt — mit Nullschluessel und Nullnonce muesste die
    /// Ausgabe sonst die Konstanten enthalten.
    #[test_case]
    fn test_chacha20_reagiert_auf_eingaben() {
        let null = [0u8; 32];
        let nonce = [0u8; 12];
        let a = chacha20_block(&null, 0, &nonce);
        let b = chacha20_block(&null, 1, &nonce);
        assert_ne!(a, b, "verschiedene Zaehler muessen verschiedene Bloecke geben");

        let mut eins = [0u8; 32];
        eins[0] = 1;
        let c = chacha20_block(&eins, 0, &nonce);
        assert_ne!(a, c, "ein Bit im Schluessel muss den Block aendern");

        let mut nonce2 = [0u8; 12];
        nonce2[11] = 1;
        let d = chacha20_block(&null, 0, &nonce2);
        assert_ne!(a, d, "ein Bit in der Nonce muss den Block aendern");

        // Die Konstanten duerfen NICHT unveraendert im Ausgang stehen (das
        // waere der Fall, wenn die Runden gar nicht liefen).
        assert_ne!(
            &a[..16],
            &[
                0x65, 0x78, 0x70, 0x61, 0x6e, 0x64, 0x20, 0x33, 0x32, 0x2d, 0x62, 0x79, 0x74,
                0x65, 0x20, 0x6b
            ][..],
            "die Runden wurden gar nicht ausgefuehrt"
        );
    }

    /// FAST KEY ERASURE: Nach jedem `erzeugen` ist der Schluessel ein
    /// anderer — DAS ist die Vorwaerts-Sicherheit, und sie ist pruefbar.
    #[test_case]
    fn test_drbg_key_erasure() {
        let mut drbg = Drbg {
            schluessel: [0x42; 32],
            zaehler: 0,
        };
        let vorher = drbg.schluessel;
        let mut a = [0u8; 16];
        drbg.erzeugen(&mut a);
        assert_ne!(drbg.schluessel, vorher, "der Schluessel wurde nicht erneuert");

        let nach_erstem = drbg.schluessel;
        let mut b = [0u8; 16];
        drbg.erzeugen(&mut b);
        assert_ne!(drbg.schluessel, nach_erstem, "zweiter Aufruf erneuert nicht");
        assert_ne!(a, b, "zwei Aufrufe liefern dieselben Bytes");

        // Und der Zaehler laeuft mit (sonst waere die Nonce wiederverwendet —
        // bei einem Stromchiffre-Kern der klassische Totalschaden).
        assert_eq!(drbg.zaehler, 2);
    }

    /// Der DRBG ist DETERMINISTISCH bei gleichem Zustand — sonst waere er
    /// kein DRBG, sondern Raten. Gleichzeitig zeigt der Test, dass `saeen`
    /// den Zustand wirklich veraendert.
    #[test_case]
    fn test_drbg_deterministisch_und_saeen_wirkt() {
        let bauen = || Drbg {
            schluessel: [7; 32],
            zaehler: 0,
        };
        let mut a = bauen();
        let mut b = bauen();
        let (mut x, mut y) = ([0u8; 96], [0u8; 96]);
        a.erzeugen(&mut x);
        b.erzeugen(&mut y);
        assert_eq!(x, y, "gleicher Zustand muss gleiche Folge liefern");

        // Laengen ueber 32 Byte brauchen mehrere Bloecke — der Uebergang ist
        // die Stelle, an der ein Off-by-one wohnen wuerde.
        let mut c = bauen();
        let mut kurz = [0u8; 32];
        c.erzeugen(&mut kurz);
        assert_eq!(&x[..32], &kurz[..], "die ersten 32 Byte muessen gleich sein");

        // Saeen aendert die Folge.
        let mut d = bauen();
        d.saeen(&[0xAA; 32]);
        let mut z = [0u8; 96];
        d.erzeugen(&mut z);
        assert_ne!(x, z, "saeen hat den Zustand nicht veraendert");

        // Und SAEEN IST XOR: Zweimal dasselbe Material auf einen frischen
        // Zustand ergibt denselben Folgezustand (Reproduzierbarkeit der
        // Konstruktion — nicht der Ausgabe nach aussen).
        let mut e = bauen();
        e.saeen(&[0xAA; 32]);
        let mut z2 = [0u8; 96];
        e.erzeugen(&mut z2);
        assert_eq!(z, z2, "saeen ist nicht reproduzierbar");
    }

    /// Der Pool nimmt Proben auf, und die Faltung liefert bei
    /// unterschiedlichem Pool unterschiedliches Material.
    #[test_case]
    fn test_pool_faltung_aendert_sich() {
        let a = pool_falten();
        einspeisen_bytes(Quelle::Salz, b"eine Probe, die den Pool aendert");
        let b = pool_falten();
        assert_ne!(a, b, "der gefaltete Pool aendert sich nicht mit dem Inhalt");
        // Zweimal falten ohne Aenderung muss gleich sein (die Faltung selbst
        // ist zustandslos).
        assert_eq!(b, pool_falten(), "pool_falten ist nicht zustandslos");
    }

    /// ENTROPIE-BUCHFUEHRUNG: Salz zaehlt NICHT, der Wiederholungs-Test
    /// greift, und der PIT wird abgewertet.
    ///
    /// Das ist der Test, der die unbequeme Rechnung aus docs/zufall.md §3
    /// festnagelt — nicht die Qualitaet des Zufalls, sondern die EHRLICHKEIT
    /// der Schaetzung.
    #[test_case]
    fn test_entropie_buchfuehrung() {
        // Salz traegt null Bit bei — nachgerechnet an der Tabelle.
        assert_eq!(Quelle::Salz.bits_je_probe(), 0);
        // Der PIT ist die schwaechste angerechnete Quelle ...
        assert_eq!(Quelle::Pit.bits_je_probe(), 1);
        // ... und wird zusaetzlich nur jede 8. Probe angerechnet.
        assert_eq!(Quelle::Pit.anrechnung_jede(), 8);
        // Menschliche Quellen liegen darueber.
        assert!(Quelle::Tastatur.bits_je_probe() > Quelle::Netz.bits_je_probe());
        assert!(Quelle::Netz.bits_je_probe() >= Quelle::Pit.bits_je_probe());

        // Alle Quellen haben einen eindeutigen Index und einen Namen.
        let mut gesehen = [false; ANZAHL_QUELLEN];
        for quelle in Quelle::alle() {
            assert!(!gesehen[quelle.index()], "doppelter Index: {}", quelle.name());
            gesehen[quelle.index()] = true;
            assert!(!quelle.name().is_empty());
        }
        assert!(gesehen.iter().all(|g| *g), "eine Quelle hat keinen Index");

        // WIEDERHOLUNGS-TEST: Eine Quelle, die zweimal dieselbe Differenz
        // liefert, ist ein Zaehler — die zweite Probe darf nichts einbringen.
        let vorher = ENTROPIE_BITS.load(Ordering::Relaxed);
        // Erste Probe setzt nur den Anker (keine Differenz vorhanden).
        einspeisen_wert(Quelle::Platte, 1_000);
        einspeisen_wert(Quelle::Platte, 2_000); // Differenz 1000 -> zaehlt
        let nach_erster = ENTROPIE_BITS.load(Ordering::Relaxed);
        einspeisen_wert(Quelle::Platte, 3_000); // Differenz WIEDER 1000
        let nach_zweiter = ENTROPIE_BITS.load(Ordering::Relaxed);
        assert!(
            nach_erster > vorher || vorher >= MAX_BITS,
            "eine echte Differenz muss angerechnet werden"
        );
        assert_eq!(
            nach_zweiter, nach_erster,
            "eine WIEDERHOLTE Differenz darf nicht angerechnet werden"
        );

        // Der Deckel haelt.
        for i in 0..2000u64 {
            einspeisen_wert(Quelle::Tastatur, i * i + i);
        }
        assert!(
            ENTROPIE_BITS.load(Ordering::Relaxed) <= MAX_BITS,
            "die Entropie-Schaetzung laeuft ueber den Deckel"
        );
    }

    /// Die Schwelle ist die Schluessellaenge — und die Hardware allein darf
    /// sie nie erreichen.
    #[test_case]
    fn test_schwellen_und_hardware_deckel() {
        assert_eq!(SCHWELLE_BITS, 256, "die Schwelle ist die Schluessellaenge");
        // Der Hardware-Deckel ist die HALBE Schwelle: Es muessen immer
        // mindestens 128 Bit aus Interrupt-Jitter dazukommen.
        assert_eq!(SCHWELLE_BITS / 2, 128);
        // Der Deckel MUSS echt kleiner als die Schwelle sein — sonst
        // koennte die Hardware allein saeen (Regel 1 verletzt). Ueber eine
        // Laufzeit-Variable, damit der Vergleich nicht zur Compilezeit
        // wegfaellt und der Test etwas prueft statt nur dazustehen.
        let deckel = core::hint::black_box(SCHWELLE_BITS / 2);
        assert!(deckel < SCHWELLE_BITS, "die Hardware koennte allein saeen");
        // Und die Wiederholungsgrenze der Hardware-Instruktionen ist gesetzt
        // (ohne sie waere eine haengende Quelle ein Kernel-Haenger).
        let versuche = core::hint::black_box(HARDWARE_VERSUCHE);
        assert!(versuche > 0 && versuche <= 64);
    }

    /// Was die Hardware in DIESER Umgebung hergibt — ein BERICHTS-Test.
    /// Er kann nicht fehlschlagen (beide Antworten sind gueltig), aber er
    /// schreibt die Zahl in den Testlauf, aus der docs/zufall.md §2 gefuellt
    /// wird.
    #[test_case]
    fn test_hardware_bericht() {
        // ERKENNUNG SELBST ANSTOSSEN: Der Lib-Testkernel ruft `zufall::init()`
        // nicht (das tut nur main.rs und tests/zufall.rs). Ohne diese Zeile
        // meldete der Bericht schlicht „keine Hardware" — und zwar unabhaengig
        // davon, was die Maschine kann. Genau dieser Fehler stand hier in der
        // ersten Fassung und haette beinahe eine falsche Zahl in
        // docs/zufall.md §2 geschrieben.
        hardware_erkennen();
        let s = status();
        crate::serial_println!(
            "  [Bericht] RDSEED: {}, RDRAND: {}, defekt: {}",
            s.rdseed,
            s.rdrand,
            s.hardware_defekt
        );
        if s.rdseed || s.rdrand {
            // Ist eine Quelle da, muss sie auch etwas liefern — und zwei
            // Werte hintereinander duerfen nicht gleich sein.
            let a = hardware_wert();
            let b = hardware_wert();
            crate::serial_println!("  [Bericht] zwei Hardware-Werte: {:?} / {:?}", a, b);
            if let (Some(a), Some(b)) = (a, b) {
                assert_ne!(a, b, "die Hardware-Quelle liefert einen konstanten Wert");
            }
        } else {
            crate::serial_println!(
                "  [Bericht] keine Hardware-Quelle — der Pool haengt allein am \
                 Interrupt-Jitter (docs/zufall.md §4)."
            );
        }
    }

    /// `fuellen` liefert im ungesaeten Zustand NICHTS — und laesst den
    /// Puffer in Ruhe.
    ///
    /// Der zweite Teil ist der wichtige: Ein halb gefuellter Puffer im
    /// Fehlerfall waere genau der Fallback, den Regel 3 ausschliesst.
    #[test_case]
    fn test_nicht_gesaet_liefert_nichts() {
        // ABBRUCH-BEDINGUNG ist die BIT-ZAHL, nicht `bereit()` — genau hier
        // ist die erste Fassung dieses Tests hereingefallen: `bereit()` war
        // noch false, aber die Schwelle laengst ueberschritten, und `fuellen`
        // hat daraufhin korrekt nachgesaet und Bytes geliefert. Der Test hat
        // also nicht den Kernel gefunden, sondern seine eigene falsche
        // Annahme. (Im Testkernel tickt der PIT, und der Test davor speist
        // absichtlich viel ein — nach wenigen Sekunden ist der Pool voll.)
        if ENTROPIE_BITS.load(Ordering::Relaxed) >= SCHWELLE_BITS {
            crate::serial_println!(
                "  (uebersprungen: der Pool hat die Schwelle bereits erreicht — \
                 der ungesaete Zustand ist nicht mehr herstellbar)"
            );
            return;
        }
        let mut puffer = [0x5Au8; 32];
        assert_eq!(fuellen(&mut puffer), Err(ZufallFehler::NichtGesaet));
        assert_eq!(puffer, [0x5Au8; 32], "der Puffer wurde trotz Fehler angefasst");
    }
}
