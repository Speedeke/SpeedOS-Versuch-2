// ===========================================================================
// src/mtrr.rs — Memory Type Range Registers: den Framebuffer schnell machen
// ===========================================================================
//
// WOZU DIESE DATEI DA IST
//
// Ein Vollbild-Transfer bei 1080p sind 8,3 MB. Liegt der Framebuffer als
// UNGECACHT (UC) im Adressraum, geht jeder einzelne 32-Bit-Schreibzugriff
// als eigene Transaktion ueber den Bus — kein Zusammenfassen, kein
// Burst. Gemessen kostet ein Vollbild dann leicht 100 ms. Bei
// WRITE-COMBINING (WC) sammelt die CPU die Schreibzugriffe in
// Puffern und schiebt sie als volle Cache-Zeilen hinaus; derselbe
// Transfer kostet wenige Millisekunden.
//
// Der Unterschied ist der zwischen einem bedienbaren und einem
// unbedienbaren System: Jede gescrollte Zeile, jeder Mauszeiger-Schritt
// laeuft ueber diesen Weg.
//
// ---------------------------------------------------------------------------
// WARUM PAT ALLEIN NICHT REICHT — der Punkt dieser Datei
//
// `memory::write_combining_einrichten` legt in der PAT einen WC-Eintrag
// an und `bereich_write_combining` haengt die Framebuffer-Seiten daran.
// Das ist richtig und noetig, aber es ist nur die HALBE Miete:
//
// Der EFFEKTIVE Speichertyp einer Seite ergibt sich aus MTRR **und** PAT
// zusammen, und dabei gewinnt der RESTRIKTIVERE (Intel SDM Vol. 3,
// Tabelle „Effective Memory Type Depending on MTRR and PAT"). Sagt der
// MTRR fuer den Bereich UC, dann bleibt es UC — ganz gleich, was in der
// Seitentabelle steht. UC ist die staerkste Aussage, und die PAT darf
// sie nicht aufweichen.
//
// In QEMU faellt das nie auf: Dort ist der „Framebuffer" gewoehnlicher
// Hauptspeicher, und die Vorgabe-MTRR sagt WB. Auf echter Hardware
// laesst die Firmware den Bereich haeufig auf UC stehen (oder die
// Vorgabe ist UC und es gibt gar keinen passenden Eintrag). Genau
// deshalb lief SpeedOS in QEMU fluessig und auf dem Laptop zaeh.
//
// Deshalb programmieren wir hier einen VARIABLEN MTRR auf WC — dasselbe,
// was Linux frueher mit `mtrr_add()` fuer Grafikkarten getan hat.
//
// ---------------------------------------------------------------------------
// DIE HALTUNG: LIEBER NICHTS TUN ALS ETWAS KAPUTTMACHEN
//
// MTRRs sind Hardware-weit wirksam und ein falsch gesetzter Bereich kann
// Speicher stillschweigend beschaedigen (falscher Typ auf normalem RAM)
// oder die Maschine anhalten. Deshalb greift diese Datei NUR, wenn ALLE
// Bedingungen sauber erfuellt sind, und laesst sonst alles unberuehrt:
//
//   * MTRRs sind vorhanden (CPUID) und EINGESCHALTET,
//   * die CPU unterstuetzt den Typ WC (MTRRCAP Bit 10),
//   * der Bereich ist NICHT schon WC (dann waere jede Aenderung Unsinn),
//   * es gibt genug FREIE variable Register,
//   * der Bereich laesst sich mit hoechstens `MAX_REGISTER` Paaren
//     ausdruecken (MTRRs koennen nur ausgerichtete Zweierpotenzen).
//
// Ein bestehender Eintrag wird NIE ueberschrieben. Wir belegen
// ausschliesslich Register, deren Gueltig-Bit geloescht ist.
//
// ---------------------------------------------------------------------------
// DIE ANDERE HALTUNG: DER BEFUND WIRD BERICHTET, NICHT VERSCHLUCKT
//
// `bericht()` liefert in einem Satz, was passiert ist. Auf echter
// Hardware gibt es keine serielle Ausgabe — der Satz landet im
// Befund-Schirm beim Boot. Ein Mechanismus, dessen Wirkung man nicht
// nachsehen kann, ist eine Behauptung.

use core::sync::atomic::{AtomicU8, Ordering};

use x86_64::registers::model_specific::Msr;

// ---------------------------------------------------------------------------
// Register-Nummern (Intel SDM Vol. 4, „MTRRs")
// ---------------------------------------------------------------------------

/// IA32_MTRRCAP — was kann diese CPU?
const MSR_MTRRCAP: u32 = 0x0FE;
/// IA32_MTRR_DEF_TYPE — Vorgabe-Typ und die zwei Hauptschalter.
const MSR_MTRR_DEF_TYPE: u32 = 0x2FF;
/// IA32_MTRR_PHYSBASE0 — die variablen Paare liegen ab hier, je zwei
/// Register (Basis, Maske) hintereinander.
const MSR_MTRR_PHYSBASE0: u32 = 0x200;

/// Speichertyp „nicht gecacht".
const TYP_UC: u8 = 0x00;
/// Speichertyp „Write-Combining" — der, den wir wollen.
const TYP_WC: u8 = 0x01;
/// Speichertyp „Write-Back" (normaler RAM).
const TYP_WB: u8 = 0x06;

/// Hoechstens so viele variable Register nehmen wir fuer EINEN Bereich in
/// Anspruch. Ein Framebuffer laesst sich fast immer mit zwei
/// Zweierpotenzen ueberdecken (z. B. 8 MiB + 4 MiB fuer 1080p); mehr
/// waere Verschwendung an einer knappen Ressource (meist 8 Paare
/// insgesamt, von denen die Firmware schon einige belegt).
const MAX_REGISTER: usize = 2;

// ---------------------------------------------------------------------------
// Der Befund
// ---------------------------------------------------------------------------

/// Was beim letzten `framebuffer_beschleunigen` herauskam. Als Zahl,
/// damit `bericht()` ohne Heap auskommt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Befund {
    /// Noch nicht gelaufen.
    Unbekannt = 0,
    /// Keine MTRRs, oder sie sind abgeschaltet — nichts zu tun.
    NichtVerfuegbar = 1,
    /// Die CPU kann den Typ WC nicht.
    WcNichtUnterstuetzt = 2,
    /// Der Bereich war schon WC. Der beste Fall: nichts zu tun.
    SchonWc = 3,
    /// Wir haben einen (oder zwei) Bereiche auf WC gesetzt.
    Gesetzt = 4,
    /// Kein freies Registerpaar mehr.
    KeinRegisterFrei = 5,
    /// Der Bereich braucht mehr als `MAX_REGISTER` Zweierpotenzen.
    ZuZerklueftet = 6,
    /// Der Bereich ist nicht UC — er ist also gecacht und damit schon
    /// schnell. Wir fassen ihn NICHT an (siehe `beschleunigen_innen`).
    NichtNoetig = 7,
}

impl Befund {
    /// Ein kurzer deutscher Satz fuer den Befund-Schirm.
    pub fn text(self) -> &'static str {
        match self {
            Befund::Unbekannt => "nicht geprueft",
            Befund::NichtVerfuegbar => "keine MTRRs",
            Befund::WcNichtUnterstuetzt => "CPU kann kein WC",
            Befund::SchonWc => "war schon WC",
            Befund::Gesetzt => "auf WC gesetzt",
            Befund::KeinRegisterFrei => "kein Register frei",
            Befund::ZuZerklueftet => "Bereich zu zerklueftet",
            Befund::NichtNoetig => "nicht noetig (gecacht)",
        }
    }

    fn von_zahl(z: u8) -> Befund {
        match z {
            1 => Befund::NichtVerfuegbar,
            2 => Befund::WcNichtUnterstuetzt,
            3 => Befund::SchonWc,
            4 => Befund::Gesetzt,
            5 => Befund::KeinRegisterFrei,
            6 => Befund::ZuZerklueftet,
            7 => Befund::NichtNoetig,
            _ => Befund::Unbekannt,
        }
    }
}

static BEFUND: AtomicU8 = AtomicU8::new(Befund::Unbekannt as u8);
/// Der Speichertyp, den der Bereich VORHER hatte (fuer den Bericht).
static TYP_VORHER: AtomicU8 = AtomicU8::new(0xFF);

/// Was beim letzten Versuch herauskam.
pub fn befund() -> Befund {
    Befund::von_zahl(BEFUND.load(Ordering::Relaxed))
}

/// Der Speichertyp, den der Framebuffer VOR unserem Eingriff hatte —
/// `None`, wenn wir gar nicht nachgesehen haben.
pub fn typ_vorher() -> Option<u8> {
    match TYP_VORHER.load(Ordering::Relaxed) {
        0xFF => None,
        t => Some(t),
    }
}

/// Ein Speichertyp als kurzes Wort.
pub fn typ_text(typ: u8) -> &'static str {
    match typ {
        TYP_UC => "UC",
        TYP_WC => "WC",
        0x04 => "WT",
        0x05 => "WP",
        TYP_WB => "WB",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// Die reinen Funktionen — hier steckt die Denkarbeit, und sie ist testbar
// ---------------------------------------------------------------------------

/// Zerlegt `[basis, basis+laenge)` in AUSGERICHTETE ZWEIERPOTENZEN.
///
/// MTRRs koennen nichts anderes: Ein variables Paar beschreibt immer
/// einen Bereich, dessen Groesse eine Zweierpotenz ist UND dessen Basis
/// auf diese Groesse ausgerichtet liegt. Ein 1080p-Framebuffer ist
/// 8 294 400 Byte gross — keine Zweierpotenz. Also muss er ueberdeckt
/// werden.
///
/// WIR UEBERDECKEN NACH OBEN, wir schneiden nicht ab: Der letzte
/// Bereich darf ueber das Ende hinausragen. Das ist die richtige
/// Richtung, denn dahinter liegt weiterer Grafikspeicher desselben
/// Geraets und kein normaler RAM — waehrend ein zu KURZER Bereich
/// bedeutete, dass der Rest des Bildschirms weiterhin langsam bleibt
/// (und man den Fehler an einem Streifen im unteren Bilddrittel
/// merkte, der beim Scrollen ruckelt).
///
/// Der Algorithmus ist der uebliche gierige: Nimm am aktuellen Anfang
/// den groessten Block, der (a) zur Ausrichtung der Basis passt und
/// (b) nicht groesser ist als das, was noch fehlt — und wenn zum
/// Schluss ein Rest bleibt, runde ihn auf die naechste Zweierpotenz
/// auf.
///
/// Gibt `None`, wenn mehr als `MAX_REGISTER` Bloecke noetig waeren.
fn bereiche_zerlegen(basis: u64, laenge: u64) -> Option<[(u64, u64); MAX_REGISTER]> {
    if laenge == 0 || !basis.is_multiple_of(4096) {
        return None;
    }
    let mut ergebnis = [(0u64, 0u64); MAX_REGISTER];
    let mut anzahl = 0usize;
    let mut pos = basis;
    let ende = basis.checked_add(laenge)?;

    while pos < ende {
        // Die groesste Zweierpotenz, auf die `pos` ausgerichtet ist.
        // (`trailing_zeros` von 0 waere 64 — kann hier nicht auftreten,
        // weil pos >= basis > 0 ist, sobald der Framebuffer real ist.)
        let ausrichtung = if pos == 0 { 1u64 << 63 } else { 1u64 << pos.trailing_zeros() };
        let rest = ende - pos;
        // Der groesste Block <= rest ist die naechstkleinere
        // Zweierpotenz von `rest`.
        let passend = if rest.is_power_of_two() {
            rest
        } else {
            1u64 << (63 - rest.leading_zeros())
        };
        let mut groesse = core::cmp::min(ausrichtung, passend);

        // Ist das der LETZTE moegliche Block und bleibt danach noch
        // etwas uebrig, dann runden wir AUF — lieber etwas zu viel
        // ueberdecken als einen langsamen Streifen zu lassen.
        if anzahl == MAX_REGISTER - 1 && groesse < rest {
            let aufgerundet = rest.next_power_of_two();
            // Nur, wenn die Basis das auch traegt.
            if pos.is_multiple_of(aufgerundet) {
                groesse = aufgerundet;
            }
        }

        if anzahl >= MAX_REGISTER {
            return None;
        }
        ergebnis[anzahl] = (pos, groesse);
        anzahl += 1;
        pos = pos.checked_add(groesse)?;
    }
    if anzahl == 0 {
        return None;
    }
    Some(ergebnis)
}

/// Baut den Wert fuer IA32_MTRR_PHYSMASKn.
///
/// Die Maske sagt, welche Adressbits verglichen werden: Ein Bereich der
/// Groesse `groesse` ignoriert die unteren `log2(groesse)` Bits. Bit 11
/// ist das Gueltig-Bit.
fn maske_bauen(groesse: u64, physik_bits: u32) -> u64 {
    let voll: u64 = if physik_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << physik_bits) - 1
    };
    // Alle Bits oberhalb der Bereichsgroesse, begrenzt auf die
    // physikalische Adressbreite der CPU. Reservierte Bits darueber
    // MUESSEN null sein, sonst gibt es eine #GP.
    let m = (!(groesse - 1)) & voll & !0xFFFu64;
    m | (1 << 11)
}

/// Baut den Wert fuer IA32_MTRR_PHYSBASEn (Adresse + Typ).
fn basis_bauen(basis: u64, typ: u8) -> u64 {
    (basis & !0xFFFu64) | (typ as u64)
}

// ---------------------------------------------------------------------------
// Die Hardware-Seite
// ---------------------------------------------------------------------------

/// Wie viele physikalische Adressbits hat diese CPU? (CPUID 0x8000_0008)
fn physik_adressbits() -> u32 {
    // CPUID ist auf jeder x86_64-CPU vorhanden und ohne Seiteneffekt.
    // Vor Blatt 0x8000_0008 pruefen wir, dass es das erweiterte Blatt
    // ueberhaupt gibt.
    let hoechstes = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if hoechstes >= 0x8000_0008 {
        let bits = core::arch::x86_64::__cpuid(0x8000_0008).eax & 0xFF;
        if (32..=52).contains(&bits) {
            return bits;
        }
    }
    36 // Der konservative Rueckfall (P6-Zeitalter).
}

/// Hat die CPU ueberhaupt MTRRs? (CPUID.01H:EDX Bit 12)
fn mtrr_vorhanden() -> bool {
    let edx = core::arch::x86_64::__cpuid(1).edx;
    edx & (1 << 12) != 0
}

/// Liest den effektiven MTRR-Typ fuer eine physikalische Adresse —
/// so, wie die CPU ihn bestimmt.
///
/// Die Regeln (SDM 11.11.4.1) in der Reihenfolge, in der sie gelten:
///   1. Unter 1 MiB entscheiden die FESTEN MTRRs (die fassen wir nicht
///      an; ein Framebuffer liegt nie dort — wir liefern dann `None`).
///   2. Trifft GENAU EIN variabler Bereich zu, gilt dessen Typ.
///   3. Treffen MEHRERE zu und einer davon sagt UC, gilt UC.
///   4. Treffen mehrere zu und alle sagen WT/WB, gilt WT.
///   5. Trifft KEINER zu, gilt der Vorgabe-Typ.
fn typ_fuer_adresse(adresse: u64, vcnt: u8, vorgabe: u8) -> Option<u8> {
    if adresse < 0x10_0000 {
        return None; // Bereich der festen MTRRs — nicht unsere Baustelle.
    }
    let mut gefunden: Option<u8> = None;
    let mut mehrere = false;
    for i in 0..vcnt {
        let (basis_roh, maske_roh) = paar_lesen(i);
        if maske_roh & (1 << 11) == 0 {
            continue; // ungueltig = frei
        }
        let maske = maske_roh & !0xFFFu64;
        if (adresse & maske) == (basis_roh & maske & !0xFFFu64) {
            let typ = (basis_roh & 0xFF) as u8;
            match gefunden {
                None => gefunden = Some(typ),
                Some(bisher) => {
                    mehrere = true;
                    // Regel 3: UC gewinnt immer.
                    if typ == TYP_UC || bisher == TYP_UC {
                        gefunden = Some(TYP_UC);
                    } else if typ != bisher {
                        // Regel 4 (WT/WB) bzw. undefinierte Mischung:
                        // konservativ das Restriktivere annehmen.
                        gefunden = Some(core::cmp::min(typ, bisher));
                    }
                }
            }
        }
    }
    let _ = mehrere;
    Some(gefunden.unwrap_or(vorgabe))
}

/// Liest ein variables Registerpaar (Basis, Maske).
fn paar_lesen(index: u8) -> (u64, u64) {
    let basis_msr = Msr::new(MSR_MTRR_PHYSBASE0 + (index as u32) * 2);
    let maske_msr = Msr::new(MSR_MTRR_PHYSBASE0 + (index as u32) * 2 + 1);
    // SAFETY: Die Nummern liegen im vom SDM festgelegten Block, und wir
    // rufen die Funktion nur, wenn CPUID MTRRs meldet. Reines Lesen.
    unsafe { (basis_msr.read(), maske_msr.read()) }
}

// ---------------------------------------------------------------------------
// Die eigentliche Aktion
// ---------------------------------------------------------------------------

/// Setzt den Framebuffer-Bereich auf Write-Combining, wenn das noetig
/// UND gefahrlos moeglich ist.
///
/// `basis` ist die PHYSIKALISCHE Adresse des Framebuffers, `laenge`
/// seine Groesse in Byte.
///
/// Liefert den Befund; `befund()` merkt ihn sich fuer den Bericht.
pub fn framebuffer_beschleunigen(basis: u64, laenge: u64) -> Befund {
    let befund = beschleunigen_innen(basis, laenge);
    BEFUND.store(befund as u8, Ordering::Relaxed);
    crate::serial_println!(
        "[mtrr] Framebuffer 0x{:x} ({} KiB): {}",
        basis,
        laenge / 1024,
        befund.text()
    );
    befund
}

fn beschleunigen_innen(basis: u64, laenge: u64) -> Befund {
    if !mtrr_vorhanden() {
        return Befund::NichtVerfuegbar;
    }

    // SAFETY: MTRRCAP/DEF_TYPE existieren, sobald CPUID MTRRs meldet.
    let (cap, def) = unsafe { (Msr::new(MSR_MTRRCAP).read(), Msr::new(MSR_MTRR_DEF_TYPE).read()) };

    let vcnt = (cap & 0xFF) as u8;
    let kann_wc = cap & (1 << 10) != 0;
    let eingeschaltet = def & (1 << 11) != 0;
    let vorgabe = (def & 0xFF) as u8;

    crate::serial_println!(
        "[mtrr] VCNT={} WC={} E={} Vorgabe={}",
        vcnt,
        kann_wc,
        eingeschaltet,
        typ_text(vorgabe)
    );

    if vcnt == 0 || !eingeschaltet {
        return Befund::NichtVerfuegbar;
    }
    if !kann_wc {
        return Befund::WcNichtUnterstuetzt;
    }

    // Wie sieht es JETZT aus? Das ist zugleich die Antwort auf die
    // Frage, ob dieser ganze Aufwand ueberhaupt noetig ist.
    let Some(vorher) = typ_fuer_adresse(basis, vcnt, vorgabe) else {
        // Unter 1 MiB entscheiden die festen MTRRs — dort liegt kein
        // Framebuffer, und die fassen wir grundsaetzlich nicht an.
        return Befund::NichtNoetig;
    };
    TYP_VORHER.store(vorher, Ordering::Relaxed);
    if vorher == TYP_WC {
        return Befund::SchonWc;
    }

    // ===================================================================
    // DIE SICHERHEITSSCHRANKE: WIR GREIFEN NUR BEI **UC** EIN.
    //
    // Das ist keine Vorsicht um der Vorsicht willen, sondern die Stelle,
    // an der man echten Schaden anrichten koennte:
    //
    // Sagt der MTRR fuer den Bereich WB, dann ist es gewoehnlicher,
    // gecachter Speicher — genau der Fall in QEMU, wo der „Framebuffer"
    // schlicht Hauptspeicher ist. Ihn auf WC zu stellen wuerde ihn
    // LANGSAMER machen, und schlimmer: Weil wir nach oben UEBERDECKEN
    // (MTRRs koennen nur Zweierpotenzen), erwischten wir dabei
    // benachbarten RAM — der verloere seine Cache-Kohaerenz und seine
    // Schreib-Reihenfolge. Das waere ein Fehler, den man erst Wochen
    // spaeter als unerklaerliche Datenverfaelschung bemerkt.
    //
    // UC dagegen bedeutet in der Praxis: Geraetespeicher. Dahinter liegt
    // weiterer Grafikspeicher desselben Geraets und kein RAM — die
    // Ueberdeckung ist dort gefahrlos, und der Gewinn ist genau der,
    // um den es geht.
    //
    // Kurz: Wir reparieren den kaputten Fall und lassen den gesunden in
    // Ruhe.
    if vorher != TYP_UC {
        crate::serial_println!(
            "[mtrr] Framebuffer ist {} (nicht UC) — kein Eingriff noetig.",
            typ_text(vorher)
        );
        return Befund::NichtNoetig;
    }

    let Some(bereiche) = bereiche_zerlegen(basis, laenge) else {
        return Befund::ZuZerklueftet;
    };
    let noetig = bereiche.iter().filter(|(_, g)| *g != 0).count();

    // FREIE Register suchen — wir fassen NIE einen bestehenden Eintrag
    // an. Die Firmware hat ihre Gruende, und wir kennen sie nicht.
    let mut frei = [0u8; MAX_REGISTER];
    let mut gefunden = 0usize;
    for i in 0..vcnt {
        let (_, maske) = paar_lesen(i);
        if maske & (1 << 11) == 0 {
            if gefunden < MAX_REGISTER {
                frei[gefunden] = i;
            }
            gefunden += 1;
            if gefunden >= noetig {
                break;
            }
        }
    }
    if gefunden < noetig {
        return Befund::KeinRegisterFrei;
    }

    let bits = physik_adressbits();
    // SAFETY: Die Bedingungen der Funktion sind oben alle geprueft —
    // MTRRs vorhanden und aktiv, WC unterstuetzt, die Register sind
    // FREI (Gueltig-Bit geloescht), und die Bereiche sind ausgerichtete
    // Zweierpotenzen. Die vorgeschriebene Aenderungs-Reihenfolge
    // (SDM 11.11.8) steckt in `paare_schreiben`.
    unsafe {
        paare_schreiben(&frei[..noetig], &bereiche[..noetig], bits);
    }

    for (i, (b, g)) in bereiche[..noetig].iter().enumerate() {
        crate::serial_println!(
            "[mtrr] Register {} <- 0x{:x} .. 0x{:x} ({} KiB) = WC",
            frei[i],
            b,
            b + g,
            g / 1024
        );
    }
    Befund::Gesetzt
}

/// Schreibt die Registerpaare mit der vom SDM vorgeschriebenen
/// Reihenfolge (Vol. 3, „MTRR Considerations in MP Systems" bzw.
/// 11.11.8 „MTRR Maintenance Programming Interface").
///
/// # Safety
///
/// Der Aufrufer muss sichergestellt haben:
///   * die CPU hat MTRRs und unterstuetzt den geschriebenen Typ,
///   * `register` nennt ausschliesslich FREIE Paare (Gueltig-Bit aus),
///   * jeder Bereich ist eine auf sich selbst ausgerichtete
///     Zweierpotenz und liegt NICHT auf normalem RAM.
///
/// Waehrend der Aenderung sind die Caches abgeschaltet — die Prozedur
/// laeuft mit ausgeschalteten Interrupts und beruehrt so wenig Speicher
/// wie moeglich.
unsafe fn paare_schreiben(register: &[u8], bereiche: &[(u64, u64)], physik_bits: u32) {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4};
    use x86_64::instructions::tlb;

    x86_64::instructions::interrupts::without_interrupts(|| {
        // --- 1. Caches abschalten: CD=1, NW=0 ---------------------------
        let cr0_alt = Cr0::read();
        unsafe {
            Cr0::write(cr0_alt | Cr0Flags::CACHE_DISABLE);
            Cr0::update(|f| f.remove(Cr0Flags::NOT_WRITE_THROUGH));
        }

        // --- 2. Caches leeren -------------------------------------------
        // SAFETY: wbinvd ist in Ring 0 immer erlaubt.
        unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };

        // --- 3. TLB leeren ----------------------------------------------
        // Ueber CR3 neu laden — `tlb::flush_all` tut genau das.
        tlb::flush_all();

        // --- 4. MTRRs abschalten (DEF_TYPE.E = 0) ------------------------
        let mut def_msr = Msr::new(MSR_MTRR_DEF_TYPE);
        let def_alt = unsafe { def_msr.read() };
        unsafe { def_msr.write(def_alt & !(1 << 11)) };

        // --- 5. Die Paare schreiben --------------------------------------
        // WICHTIGE REIHENFOLGE: erst die BASIS (mit dem Typ), dann die
        // MASKE — das Gueltig-Bit sitzt in der Maske, der Bereich wird
        // also erst mit dem letzten Schreibzugriff scharf. Andersherum
        // gaebe es einen Augenblick, in dem ein gueltiger Bereich mit
        // einer alten Basis dasteht.
        for (i, (basis, groesse)) in register.iter().zip(bereiche.iter()) {
            let mut basis_msr = Msr::new(MSR_MTRR_PHYSBASE0 + (*i as u32) * 2);
            let mut maske_msr = Msr::new(MSR_MTRR_PHYSBASE0 + (*i as u32) * 2 + 1);
            unsafe {
                basis_msr.write(basis_bauen(*basis, TYP_WC));
                maske_msr.write(maske_bauen(*groesse, physik_bits));
            }
        }

        // --- 6. MTRRs wieder einschalten ---------------------------------
        unsafe { def_msr.write(def_alt | (1 << 11)) };

        // --- 7. Caches erneut leeren und TLB nachziehen -------------------
        unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
        tlb::flush_all();

        // --- 8. Caches wieder anschalten ---------------------------------
        unsafe { Cr0::write(cr0_alt) };

        // CR4 bleibt unberuehrt; PGE aus- und einzuschalten waere die
        // Lehrbuch-Variante fuer globale Seiten. Wir haben keine
        // globalen Seiten (bootloader_api setzt GLOBAL nicht), und ein
        // vollstaendiger CR3-Neuladen leert auch ohne das alles, was
        // nicht global ist.
        let _ = Cr4::read();
    });
}

// ===========================================================================
// Tests — die Zerlegung ist reine Rechnung und wird auch so geprueft
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein 1080p-Framebuffer (1920x1080x4 = 8 294 400 Byte) an einer
    /// typischen, auf 16 MiB ausgerichteten Adresse. Er ist KEINE
    /// Zweierpotenz — genau der Fall, fuer den es die Zerlegung gibt.
    #[test_case]
    fn test_zerlegung_1080p() {
        let basis = 0xC000_0000u64; // ueppig ausgerichtet, wie ueblich
        let laenge = 1920 * 1080 * 4;
        let teile = bereiche_zerlegen(basis, laenge).expect("muss gehen");
        let summe: u64 = teile.iter().map(|(_, g)| g).sum();
        assert!(
            summe >= laenge,
            "die Ueberdeckung darf nie KUERZER sein als der Framebuffer"
        );
        // Jeder Teil eine Zweierpotenz, auf sich selbst ausgerichtet:
        for (b, g) in teile.iter() {
            if *g == 0 {
                continue;
            }
            assert!(g.is_power_of_two(), "Groesse muss Zweierpotenz sein");
            assert_eq!(b % g, 0, "Basis muss auf die Groesse ausgerichtet sein");
        }
        // Und sie muessen lueckenlos aneinander liegen.
        assert_eq!(teile[0].0, basis);
        if teile[1].1 != 0 {
            assert_eq!(teile[1].0, teile[0].0 + teile[0].1, "keine Luecke");
        }
    }

    /// Eine Groesse, die schon eine Zweierpotenz ist, braucht genau EIN
    /// Register — und darf nicht kuenstlich zerlegt werden.
    #[test_case]
    fn test_zerlegung_zweierpotenz_braucht_ein_register() {
        let teile = bereiche_zerlegen(0x8000_0000, 16 * 1024 * 1024).unwrap();
        assert_eq!(teile[0], (0x8000_0000, 16 * 1024 * 1024));
        assert_eq!(teile[1].1, 0, "der zweite Platz bleibt leer");
    }

    /// Die Maske muss die unteren Bits des Bereichs ausblenden und das
    /// Gueltig-Bit setzen — und oberhalb der physikalischen Adressbreite
    /// NICHTS setzen (dort gaebe es sonst eine #GP).
    #[test_case]
    fn test_maske_bauen() {
        let m = maske_bauen(16 * 1024 * 1024, 39);
        assert!(m & (1 << 11) != 0, "Gueltig-Bit fehlt");
        assert_eq!(m & 0xFFF & !(1 << 11), 0, "unterhalb 4 KiB nichts setzen");
        // Bit 24 (16 MiB) und darueber muessen gesetzt sein, Bit 23 nicht.
        assert!(m & (1 << 24) != 0);
        assert_eq!(m & (1 << 23), 0);
        // Nichts oberhalb der physikalischen Breite:
        assert_eq!(m >> 39, 0, "reservierte Bits muessen null sein");
    }

    /// Der Typ steht in den unteren 8 Bit der Basis, die Adresse
    /// darueber — beides darf sich nicht vermischen.
    #[test_case]
    fn test_basis_bauen() {
        let b = basis_bauen(0xC000_0000, TYP_WC);
        assert_eq!(b & 0xFF, TYP_WC as u64);
        assert_eq!(b & !0xFFF, 0xC000_0000);
    }

    /// Eine unausgerichtete Basis wird ABGELEHNT statt krumm gerechnet.
    #[test_case]
    fn test_unausgerichtete_basis_abgelehnt() {
        assert!(bereiche_zerlegen(0xC000_0123, 4096).is_none());
        assert!(bereiche_zerlegen(0xC000_0000, 0).is_none());
    }

    /// Ein Bereich, der mehr als MAX_REGISTER Zweierpotenzen braucht,
    /// wird abgelehnt — lieber nichts tun als die knappen Register
    /// leerraeumen.
    #[test_case]
    fn test_zu_zerklueftet_wird_abgelehnt() {
        // Basis nur auf 4 KiB ausgerichtet, Laenge krumm: das braucht
        // viele Bloecke, und der Aufrundungs-Ausweg greift nicht, weil
        // die Basis die groessere Zweierpotenz nicht traegt.
        assert!(bereiche_zerlegen(0xC000_1000, 7 * 1024 * 1024).is_none());
    }

    /// Der Befund muss sich als Zahl hin und zurueck uebersetzen lassen
    /// — er reist ueber ein Atomic.
    #[test_case]
    fn test_befund_rundreise() {
        for b in [
            Befund::NichtVerfuegbar,
            Befund::WcNichtUnterstuetzt,
            Befund::SchonWc,
            Befund::Gesetzt,
            Befund::KeinRegisterFrei,
            Befund::ZuZerklueftet,
            Befund::NichtNoetig,
        ] {
            assert_eq!(Befund::von_zahl(b as u8), b);
            assert!(!b.text().is_empty());
        }
    }
}
