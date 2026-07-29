// zeit.rs — Die Zeit-API von SpeedOS
//
// Alle Stellen im Kernel, die Zeit brauchen, fragen NUR dieses Modul —
// niemals direkt den Tick-Zähler des Interrupt-Handlers. Diese
// API-Naht hat sich gerade ausgezahlt: Die Zeitquelle wurde vom PIT
// auf den TSC umgestellt, ohne dass sich ein Aufrufer ändern musste.
//
// ZWEI Uhren, zwei Aufgaben:
//   * TSC (Time Stamp Counter): zählt CPU-Takte, wird beim Boot gegen
//     den PIT kalibriert (zeit::init) und ist danach DIE Zeitquelle
//     für ms_seit_boot/us_seit_boot — mikrosekundengenau und
//     UNABHÄNGIG von Interrupts (kein Stillstand mehr unter
//     without_interrupts, die alte Mess-Falle ist tot).
//   * PIT (~250 Hz, Teiler 4773): nur noch der WECKGEBER — seine
//     Interrupts wecken warte_ms-Schläfer und den Executor aus hlt.
//     Vor der Kalibrierung (und falls sie je scheitert) dient er als
//     grobe Fallback-Uhr.
//
// Dazu die ECHTE Uhrzeit: rtc.rs liest beim Boot einmal die
// CMOS-Uhr; zeit::jetzt() = RTC-Anker + verstrichene TSC-Zeit.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};

/// Die Basisfrequenz des PIT-Chips in Hz (Quarz seit dem Ur-PC 1981).
pub(crate) const PIT_BASIS_HZ: u64 = 1_193_182;
/// UNSER PIT-Teiler (~250 Hz). Lebt hier, damit die Timer-
/// Programmierung (interrupts.rs) und die ms-Umrechnung (unten)
/// garantiert denselben Wert benutzen.
pub(crate) const PIT_TEILER: u64 = 4_773;

/// Timer-Ticks seit dem Boot (~250 pro Sekunde) — der Weckgeber.
pub fn ticks() -> u64 {
    crate::interrupts::timer_ticks()
}

/// Rechnet Ticks in Millisekunden um (reine Funktion, gut testbar):
/// ms = ticks * Teiler * 1000 / Basisfrequenz  (~4,0 ms pro Tick).
pub fn ms_von_ticks(ticks: u64) -> u64 {
    ticks * (PIT_TEILER * 1000) / PIT_BASIS_HZ
}

// ---------------------------------------------------------------------------
// TSC — die kalibrierte, monotone Zeitquelle
// ---------------------------------------------------------------------------

/// TSC-Takte pro Millisekunde (0 = noch nicht kalibriert -> Fallback
/// auf die PIT-Ticks). Nach zeit::init() konstant.
static TSC_PRO_MS: AtomicU64 = AtomicU64::new(0);
/// TSC-Stand am Ende der Kalibrierung (der "Anker").
static TSC_ANKER: AtomicU64 = AtomicU64::new(0);
/// Mikrosekunden seit Boot (aus PIT-Ticks) zum Anker-Zeitpunkt —
/// so läuft die Uhr nahtlos weiter, statt bei 0 neu anzufangen.
static ANKER_US: AtomicU64 = AtomicU64::new(0);

/// Liest den Time Stamp Counter der CPU.
fn tsc_lesen() -> u64 {
    // unsafe (Intrinsic): RDTSC liest nur ein CPU-Register, hat
    // keinerlei Speicher-Nebenwirkungen und ist im Ring 0 immer
    // erlaubt. (Die Out-of-Order-Unschärfe von ein paar Takten ist
    // für eine Uhr mit µs-Anspruch bedeutungslos.)
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Der ROHE TSC-Wert — die Entropie-Quelle des Zufallsgenerators.
///
/// Warum roh und nicht `us_seit_boot()`: Die Entropie steckt ausgerechnet in
/// den UNTERSTEN Bits (Interrupt-Latenz, Buslaufzeit, DRAM-Refresh). Die
/// Umrechnung in Mikrosekunden dividiert sie weg — sie ist für eine Uhr
/// richtig und für einen Zufallsgenerator das Gegenteil davon.
pub fn tsc_roh() -> u64 {
    tsc_lesen()
}

/// Ist der TSC laut CPUID "invariant" (tickt konstant, unabhängig
/// von Stromsparmodi)? Blatt 0x8000_0007, EDX Bit 8. (__cpuid ist
/// auf aktuellem Rust eine SICHERE Funktion — sie liest nur
/// CPU-Infos; die Blattnummer wird gegen das Maximum geprüft.)
fn tsc_invariant() -> bool {
    let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if max < 0x8000_0007 {
        return false;
    }
    core::arch::x86_64::__cpuid(0x8000_0007).edx & (1 << 8) != 0
}

/// Die kalibrierte TSC-Frequenz in Hz (0 = nicht kalibriert).
pub fn tsc_frequenz_hz() -> u64 {
    TSC_PRO_MS.load(core::sync::atomic::Ordering::Relaxed) * 1000
}

/// Misst den TSC über `mess_ticks` PIT-Ticks (an Tick-Flanken
/// ausgerichtet) und liefert die daraus errechnete Frequenz in Hz.
fn tsc_frequenz_messen(mess_ticks: u64) -> u64 {
    // An eine frische Tick-Flanke ausrichten, sonst ist der erste
    // "Tick" der Messung nur ein Bruchteil eines echten:
    let t = ticks();
    while ticks() == t {
        core::hint::spin_loop();
    }
    let tsc_start = tsc_lesen();
    let tick_start = ticks();
    while ticks() < tick_start + mess_ticks {
        core::hint::spin_loop();
    }
    let tsc_delta = tsc_lesen() - tsc_start;
    // Frequenz = Takte / Dauer;  Dauer = mess_ticks * Teiler / Basis.
    tsc_delta * PIT_BASIS_HZ / (mess_ticks * PIT_TEILER)
}

/// Kalibriert den TSC gegen den PIT und liest die RTC — beim Boot
/// aufrufen, NACHDEM Interrupts laufen (der PIT muss ticken!).
/// Loggt Dauer, Frequenz, Genauigkeit und den Invariant-Status.
pub fn init() {
    use core::sync::atomic::Ordering;

    let start_ticks = ticks();

    // Zwei Messungen: die erste kalibriert, die zweite verrät die
    // Wiederholgenauigkeit (Abweichung in Promille).
    const MESS_TICKS: u64 = 25; // ~100 ms pro Messung
    let hz_1 = tsc_frequenz_messen(MESS_TICKS);
    let hz_2 = tsc_frequenz_messen(MESS_TICKS);
    let abweichung_promille = hz_1.abs_diff(hz_2) * 1000 / hz_1.max(1);

    // Anker setzen: Ab HIER übernimmt der TSC die Uhr, nahtlos an
    // der bisherigen PIT-Zeit ausgerichtet.
    TSC_ANKER.store(tsc_lesen(), Ordering::Relaxed);
    ANKER_US.store(ms_von_ticks(ticks()) * 1000, Ordering::Relaxed);
    TSC_PRO_MS.store((hz_1 / 1000).max(1), Ordering::Relaxed);

    let dauer_ms = ms_von_ticks(ticks() - start_ticks);
    crate::serial_println!(
        "[ZEIT] TSC kalibriert: {},{:03} MHz (Kontrollmessung weicht {} Promille ab, \
         Dauer {} ms, invariant laut CPUID: {})",
        hz_1 / 1_000_000,
        (hz_1 / 1000) % 1000,
        abweichung_promille,
        dauer_ms,
        if tsc_invariant() { "ja" } else { "NEIN" }
    );

    // Echte Uhrzeit: RTC einmal lesen und als Anker merken.
    //
    // Die RTC-ZONE ist hier noch NICHT bekannt (sie steht in den
    // Einstellungen, und die brauchen das Dateisystem). Deshalb wird der
    // Rohwert zusätzlich aufbewahrt; `rtc_zone_setzen` rechnet den Anker
    // später um, ohne die Uhr ein zweites Mal zu lesen.
    match crate::rtc::lesen() {
        Some(datum) => {
            let roh = sekunden_seit_2000(&datum);
            ROH_RTC_S.store(roh, Ordering::Relaxed);
            EPOCH_ANKER_S.store(roh, Ordering::Relaxed);
            EPOCH_ANKER_US.store(us_seit_boot(), Ordering::Relaxed);
            crate::serial_println!(
                "[ZEIT] RTC gelesen: {:02}.{:02}.{} {:02}:{:02}:{:02} \
                 (zunaechst als UTC gedeutet — die RTC-Zone kommt aus den Einstellungen)",
                datum.tag, datum.monat, datum.jahr,
                datum.stunde, datum.minute, datum.sekunde
            );
        }
        None => crate::serial_println!(
            "[ZEIT] WARNUNG: RTC nicht lesbar — Uhrzeit startet am Platzhalter-Datum."
        ),
    }
    plausibilitaet_pruefen();
}

// ---------------------------------------------------------------------------
// (1) DIE RTC-ZONE — wie der Rohwert der Hardware-Uhr zu deuten ist
// ---------------------------------------------------------------------------

/// Deutet die beim Boot gelesene RTC-Zeit und setzt den UTC-Anker neu.
///
/// `zone_min` sind die Minuten, die der ROHWERT der UTC VORAUS ist
/// (Mitteleuropa im Sommer: +120). `0` heisst „die RTC läuft in UTC".
///
/// WANN AUFRUFEN: nachdem die Einstellungen geladen sind. Vorher gilt die
/// Annahme „RTC = UTC", was für die Boot-Meldungen genügt und für nichts
/// anderes benutzt wird.
///
/// WARUM NACHTRÄGLICH UND NICHT GLEICH RICHTIG: Die Zone steht in einer
/// Datei auf `/platte`, und das Dateisystem gibt es beim Lesen der RTC noch
/// nicht. Die Alternative — die RTC später ein zweites Mal lesen — wäre
/// schlechter: Zwischen beiden Lesungen liegt eine unbekannte Zeitspanne,
/// und der TSC-Anker müsste mit umgesetzt werden.
pub fn rtc_zone_setzen(zone_min: i64) {
    use core::sync::atomic::Ordering;
    let roh = ROH_RTC_S.load(Ordering::Relaxed);
    RTC_ZONE_MIN.store(zone_min, Ordering::Relaxed);
    if roh == 0 {
        return; // keine RTC gelesen — es gibt nichts umzudeuten
    }
    let utc = (roh as i64 - zone_min * 60).max(0) as u64;
    EPOCH_ANKER_S.store(utc, Ordering::Relaxed);
    if zone_min != 0 {
        crate::serial_println!(
            "[ZEIT] RTC laeuft in Lokalzeit ({:+} min) — UTC-Anker um {} s korrigiert.",
            zone_min,
            zone_min * 60
        );
    }
    plausibilitaet_pruefen();
}

/// Die aktuell angenommene RTC-Zone in Minuten (0 = die RTC läuft in UTC).
pub fn rtc_zone_min() -> i64 {
    RTC_ZONE_MIN.load(core::sync::atomic::Ordering::Relaxed)
}

/// STELLT DIE UHR VON HAND (Einstellungen).
///
/// Auf Hardware mit leerer Pufferbatterie ist das der einzige Weg zu einer
/// brauchbaren Zeit — und ohne brauchbare Zeit gibt es keine
/// Zertifikatsprüfung. `datum` ist **UTC**, nicht Lokalzeit: Die
/// Einstellungs-App rechnet die Anzeige-Zone vorher heraus (Ebene (3)
/// bleibt Ebene (3)).
///
/// Die CMOS-Uhr selbst wird NICHT geschrieben. Das wäre ein Schreibzugriff
/// auf Firmware-Zustand, den ein Lernsystem nicht braucht — die Korrektur
/// lebt in den Einstellungen und wird bei jedem Boot neu angewandt.
pub fn zeit_setzen(datum: &DatumUhrzeit) {
    use core::sync::atomic::Ordering;
    EPOCH_ANKER_S.store(sekunden_seit_2000(datum), Ordering::Relaxed);
    EPOCH_ANKER_US.store(us_seit_boot(), Ordering::Relaxed);
    crate::serial_println!(
        "[ZEIT] Uhr von Hand gestellt: {:02}.{:02}.{} {:02}:{:02}:{:02} UTC",
        datum.tag, datum.monat, datum.jahr, datum.stunde, datum.minute, datum.sekunde
    );
    plausibilitaet_pruefen();
}

// ---------------------------------------------------------------------------
// (2) DIE PLAUSIBILITÄT — was eine Uhr ohne zweite Quelle wissen kann
// ---------------------------------------------------------------------------

/// Das Bau-Datum dieses Kernels (Sekunden seit 2000), vom build.rs gesetzt.
///
/// 0 heisst „kein Bau-Datum bekannt" — dann wird nicht geprüft, statt eine
/// erfundene Grenze zu benutzen.
pub const BAU_EPOCHE_S: u64 = match u64::from_str_radix(env!("SPEEDOS_BAU_EPOCHE_S"), 10) {
    Ok(wert) => wert,
    Err(_) => 0,
};

/// Wie weit die Uhr NACH dem Bau-Datum liegen darf, bevor wir sie für
/// unplausibel halten: 30 Jahre.
///
/// Die Obergrenze ist die schwächere der beiden Grenzen und bewusst weit:
/// Ein Kernel, der zehn Jahre später noch läuft, soll nicht plötzlich seine
/// Uhr für kaputt erklären. Sie fängt trotzdem den zweiten klassischen
/// RTC-Ausfall — ein Register voller 0xFF wird zu einem Datum weit jenseits
/// jeder Nutzungsdauer.
pub const PLAUSIBEL_JAHRE: u64 = 30;

/// Ist die Uhr unplausibel? (Wird beim Boot und nach jeder Korrektur gesetzt.)
static ZEIT_UNPLAUSIBEL: AtomicBool = AtomicBool::new(false);

/// Warum eine Zeit nicht für Zertifikate taugt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeitFehler {
    /// Die Uhr steht VOR dem Bau-Datum dieses Kernels — nachweislich falsch.
    VorBauDatum,
    /// Die Uhr steht absurd weit in der Zukunft.
    ZuWeitInDerZukunft,
    /// Es wurde nie eine Uhr gelesen (RTC fehlt/defekt, nichts gestellt).
    KeineUhr,
}

impl ZeitFehler {
    pub fn meldung(self) -> &'static str {
        match self {
            ZeitFehler::VorBauDatum => {
                "die Uhr steht vor dem Bau-Datum dieses Kernels — sie ist nachweislich falsch"
            }
            ZeitFehler::ZuWeitInDerZukunft => "die Uhr steht absurd weit in der Zukunft",
            ZeitFehler::KeineUhr => "es konnte keine Uhrzeit ermittelt werden",
        }
    }
}

/// PRÜFT EINE ZEITANGABE gegen das Bau-Datum — reine Funktion.
///
/// DIE EINZIGE GRENZE, DIE EIN SYSTEM OHNE NETZ KENNEN KANN: Ein Kernel
/// kann nicht vor seinem eigenen Bau gelaufen sein. Das klingt trivial und
/// fängt genau den häufigsten Fall — die leere Pufferbatterie, die die Uhr
/// auf den 1.1.2000 (oder 1.1.1980) zurücksetzt, also weit vor jedes
/// Bau-Datum.
///
/// WAS SIE NICHT FINDET, und das gehört ausgesprochen: eine Uhr, die um
/// Stunden oder Tage falsch geht, und eine absichtlich VORGESTELLTE Uhr.
/// Gegen die zweite hilft nur eine unabhängige Quelle (NTP, docs/zeit.md).
/// Diese Prüfung ist ein Plausibilitäts-Filter, kein Sicherheitsmechanismus.
pub fn zeit_pruefen(sekunden_seit_2000: u64, bau_epoche: u64) -> Result<(), ZeitFehler> {
    if sekunden_seit_2000 == 0 {
        return Err(ZeitFehler::KeineUhr);
    }
    if bau_epoche == 0 {
        return Ok(()); // kein Bau-Datum bekannt -> nicht prüfbar, nicht ablehnen
    }
    if sekunden_seit_2000 < bau_epoche {
        return Err(ZeitFehler::VorBauDatum);
    }
    let obergrenze = bau_epoche.saturating_add(PLAUSIBEL_JAHRE * 365 * 24 * 60 * 60);
    if sekunden_seit_2000 > obergrenze {
        return Err(ZeitFehler::ZuWeitInDerZukunft);
    }
    Ok(())
}

/// Prüft die aktuelle Uhr und meldet das Ergebnis laut, wenn es schlecht ist.
fn plausibilitaet_pruefen() {
    use core::sync::atomic::Ordering;
    let jetzt_s = EPOCH_ANKER_S.load(Ordering::Relaxed);
    match zeit_pruefen(jetzt_s, BAU_EPOCHE_S) {
        Ok(()) => ZEIT_UNPLAUSIBEL.store(false, Ordering::Relaxed),
        Err(fehler) => {
            ZEIT_UNPLAUSIBEL.store(true, Ordering::Relaxed);
            let bau = datum_von_sekunden_seit_2000(BAU_EPOCHE_S);
            let ist = datum_von_sekunden_seit_2000(jetzt_s);
            // LAUT, und mit beiden Zahlen — wer das liest, soll sofort
            // sehen, was gegen was steht.
            crate::serial_println!(
                "[ZEIT] *** UHR UNPLAUSIBEL: {} ***",
                fehler.meldung()
            );
            crate::serial_println!(
                "[ZEIT] *** Uhr sagt {:02}.{:02}.{} {:02}:{:02} UTC, \
                 dieser Kernel wurde am {:02}.{:02}.{} gebaut. ***",
                ist.tag, ist.monat, ist.jahr, ist.stunde, ist.minute,
                bau.tag, bau.monat, bau.jahr
            );
            crate::serial_println!(
                "[ZEIT] *** Zertifikatspruefung wird VERWEIGERT, bis die Uhr \
                 gestellt ist (Einstellungen -> Zeit). ***"
            );
        }
    }
}

/// Ist die Uhr plausibel?
pub fn plausibel() -> bool {
    !ZEIT_UNPLAUSIBEL.load(core::sync::atomic::Ordering::Relaxed)
}

/// DIE ZEIT FÜR ZERTIFIKATE: UNIX-Sekunden, aber nur, wenn wir ihr trauen.
///
/// Das ist der Punkt, an dem aus „wir wissen, dass die Uhr falsch ist" eine
/// KONSEQUENZ wird. Ein TLS-Client, der hier einen Fehler bekommt, darf
/// nicht weiterprüfen und schon gar nicht ungeprüft verbinden — er hat
/// schlicht keine Zeitbasis.
///
/// Die Alternative wäre, die Gültigkeitsprüfung „ausnahmsweise" zu
/// überspringen. Das ist der Punkt, an dem TLS aufhört, etwas wert zu sein
/// (docs/serie7-bestandsaufnahme.md §c) — deshalb gibt es diesen Weg nicht.
pub fn zertifikatszeit() -> Result<u64, ZeitFehler> {
    let jetzt_s = sekunden_seit_2000(&jetzt());
    zeit_pruefen(jetzt_s, BAU_EPOCHE_S)?;
    Ok(jetzt_s + SEKUNDEN_1970_BIS_2000)
}

/// Sekunden zwischen dem 1.1.1970 und dem 1.1.2000 — die Brücke zwischen
/// unserer Epoche und der, in der X.509 und der Rest der Welt rechnen.
pub const SEKUNDEN_1970_BIS_2000: u64 = 946_684_800;

/// Die aktuelle UTC-Zeit als UNIX-Sekunden (ohne Plausibilitätsprüfung —
/// für Anzeige und Protokoll; wer prüft, nimmt `zertifikatszeit`).
pub fn unix_zeit() -> u64 {
    sekunden_seit_2000(&jetzt()) + SEKUNDEN_1970_BIS_2000
}

/// Mikrosekunden seit dem Boot — über den kalibrierten TSC, läuft
/// auch unter without_interrupts weiter. Vor der Kalibrierung:
/// PIT-Ticks (4-ms-Auflösung).
pub fn us_seit_boot() -> u64 {
    use core::sync::atomic::Ordering;

    let pro_ms = TSC_PRO_MS.load(Ordering::Relaxed);
    if pro_ms == 0 {
        return ms_von_ticks(ticks()) * 1000;
    }
    let delta = tsc_lesen().saturating_sub(TSC_ANKER.load(Ordering::Relaxed));
    ANKER_US.load(Ordering::Relaxed) + delta * 1000 / pro_ms
}

/// Millisekunden seit dem Boot (siehe us_seit_boot).
pub fn ms_seit_boot() -> u64 {
    us_seit_boot() / 1000
}

/// Schläft bis zum nächsten Interrupt — UND ZWAR EGAL, OB INTERRUPTS GERADE
/// AN ODER AUS SIND.
///
/// ==========================================================================
/// DIE FALLE, DIE DAHINTER STECKT (teuer gelernt in Serie 6, Teil 5)
///
/// Jede synchrone Warteschleife des Netz-Stacks (DNS, DHCP, HTTP) sah so aus:
///
///     loop { pumpen(); if fertig { break } if frist_um { break } hlt(); }
///
/// Das ist völlig richtig — für KERNEL-Kontext, wo Interrupts an sind: `hlt`
/// hält die CPU an, der nächste Timer- oder Netz-Interrupt weckt sie.
///
/// Sobald derselbe Code aber aus einem SYSCALL läuft, ist es ein TOTALAUSFALL.
/// `int 0x80` geht durch ein INTERRUPT-Gate, und das löscht IF — im Syscall
/// sind Interrupts also AUS. Ein `hlt` mit ausgeschalteten Interrupts hält die
/// CPU an, und es kann sie nichts mehr wecken. Kein Timer, kein Netz, keine
/// Tastatur. Die Maschine steht, für immer, ohne jede Meldung.
///
/// Genau das ist passiert, als `netzhole` (ein Ring-3-Programm) den Syscall
/// `aufloesen` rief: `dns::aufloesen` erreichte sein `hlt`, und SpeedOS blieb
/// mitten in der DNS-Auflösung stehen.
///
/// Diese Funktion ist die Antwort: Sie SIEHT NACH, in welchem Kontext sie
/// läuft. Sind Interrupts an, ist es das gewohnte `hlt`. Sind sie aus, öffnet
/// sie ein Wartefenster (`enable_and_hlt` + `disable`) — dieselbe Mechanik wie
/// `syscall::warte_fenster`, inklusive derselben eisernen Bedingung:
///
///   **Beim Aufruf darf KEIN Lock gehalten werden.**
///
/// Denn im Wartefenster darf der Scheduler den Prozess verdrängen. Alle
/// Aufrufstellen erfüllen das: Sie warten zwischen zwei Pump-Durchgängen und
/// halten dabei nichts.
///
/// ==========================================================================
/// SEIT DEM WECK-LATENZ-PASS (Serie 7, Teil 0): ERST ABGEBEN, DANN SCHLAFEN.
///
/// `hlt` heisst „ich habe nichts zu tun, weckt mich beim nächsten Interrupt"
/// — und genau das war die MESSFALLE aus dem Serie-6-Abschluss: Wer in einer
/// synchronen Schleife auf Daten eines anderen PROZESSES wartet und dabei
/// `hlt`-t, misst nicht dessen Geschwindigkeit, sondern die Tick-Rate. Er
/// blockiert den anderen sogar: Solange PID 0 seine Zeitscheibe „verschläft",
/// bekommt der Erzeuger die CPU erst, wenn sie abläuft (bis zu 20 ms).
///
/// Richtig ist: Kann ein anderer Prozess laufen, gehört ihm der Rest unserer
/// Scheibe. Erst wenn wirklich niemand lauffähig ist, wird geschlafen. Das
/// ist dieselbe Regel, nach der `Executor::sleep_if_idle` schon seit Teil 3
/// verfährt — sie gilt jetzt für JEDE synchrone Warteschleife des Kernels
/// (Pipe leeren, `scheduler::warten_auf`, DNS, HTTP), weil sie alle hier
/// hindurchlaufen.
/// ==========================================================================
pub fn warte_auf_interrupt() {
    use x86_64::instructions::interrupts;

    // Wartet jemand darauf, dass wir Platz machen? Dann nicht schlafen,
    // sondern abgeben. `abgeben()` geht über `int 0x80` und funktioniert
    // aus Ring 0 wie aus Ring 3, mit an- wie mit ausgeschalteten Interrupts.
    if crate::scheduler::umplanung_offen() && crate::scheduler::umplanen_im_kernel() {
        return;
    }
    if crate::scheduler::sofort_wecken_an() && crate::scheduler::andere_lauffaehig() {
        crate::scheduler::abgeben();
        return;
    }

    if interrupts::are_enabled() {
        // Kernel-Kontext: der gewohnte Weg.
        x86_64::instructions::hlt();
    } else {
        // Syscall-Kontext: `enable_and_hlt` als EIN Schritt, damit zwischen
        // "Interrupts an" und "schlafen" kein Interrupt verloren geht.
        interrupts::enable_and_hlt();
        interrupts::disable();
    }
}

// ---------------------------------------------------------------------------
// Datum und Uhrzeit — echte Zeit aus RTC-Anker + TSC
//
// Beim Boot liest zeit::init() einmal die CMOS-Uhr (rtc.rs) und
// merkt sich den Zeitpunkt als "Sekunden seit dem 1.1.2000" plus den
// damaligen us_seit_boot-Stand. jetzt() addiert einfach die seither
// verstrichene TSC-Zeit — die RTC wird nie wieder angefasst.
// ---------------------------------------------------------------------------

// ===========================================================================
// DIE EINE REGEL DIESER DATEI (Serie 7, Teil 2):
//
//   **`zeit::jetzt()` LIEFERT IMMER UTC.**
//
// Bis Serie 3 hiess es nur „das echte Datum" — und das war eine Unschärfe,
// die man erst bemerkt, wenn sie weh tut. Was die RTC liefert, ist nämlich
// NICHT festgelegt: Ein Windows-PC führt sie in LOKALZEIT, ein
// Linux-System in UTC, und QEMU tat, was der Runner ihm sagte
// (`-rtc base=localtime`). „Das echte Datum" war also je nach Maschine um
// bis zu 14 Stunden verschoben.
//
// Solange die Uhr nur die Taskleiste füllt, ist das gleichgültig. Sobald
// ZERTIFIKATE geprüft werden, ist es das nicht mehr: Gültigkeitszeiträume
// sind in UTC angegeben, und eine um Stunden verschobene Uhr macht die
// Prüfung entweder grundlos streng oder — schlimmer — zu lax.
//
// DESHALB DIE TRENNUNG IN DREI SAUBER GESCHIEDENE BEGRIFFE:
//
//   (1) DIE RTC-ZONE — eine Eigenschaft der HARDWARE: „Läuft die
//       CMOS-Uhr in UTC oder in Lokalzeit, und wenn Lokalzeit, in
//       welcher?" Sie wird EINMAL beim Anker-Setzen angewandt und
//       danach nie wieder (`rtc_zone_setzen`).
//   (2) UTC — die Wahrheit. `jetzt()`, `unix_zeit()`, alles, was rechnet
//       oder prüft, benutzt ausschliesslich das.
//   (3) DIE ANZEIGE-ZONE — reine Kosmetik, lebt in `einstellungen`
//       (`jetzt_lokal`, `utc_offset_min`). Sie darf NIE in eine
//       Berechnung geraten, nur in eine Ausgabe.
//
// Wer eine dieser drei Ebenen mit einer anderen verrechnet, baut genau den
// Fehler wieder ein, den dieser Abschnitt beseitigt hat.
// ===========================================================================

/// UTC-Anker: Sekunden seit dem 1.1.2000 00:00:00 **UTC** zum Lese-Zeitpunkt
/// (0 = keine RTC gelesen -> Platzhalter-Datum).
static EPOCH_ANKER_S: AtomicU64 = AtomicU64::new(0);
/// us_seit_boot zum RTC-Lese-Zeitpunkt.
static EPOCH_ANKER_US: AtomicU64 = AtomicU64::new(0);
/// Was die RTC WÖRTLICH gesagt hat (Sekunden seit 2000, uninterpretiert).
///
/// Getrennt vom Anker aufbewahrt, weil die RTC-Zone erst später bekannt ist:
/// `zeit::init()` läuft weit vor `einstellungen::laden()` (die braucht das
/// Dateisystem). Ohne diesen Rohwert müsste die RTC ein zweites Mal gelesen
/// werden — und die zweite Lesung wäre eine andere Sekunde.
static ROH_RTC_S: AtomicU64 = AtomicU64::new(0);
/// Minuten, die vom Rohwert ABGEZOGEN werden, um UTC zu erhalten.
/// 0 = die RTC läuft in UTC.
static RTC_ZONE_MIN: AtomicI64 = AtomicI64::new(0);

/// Platzhalter, falls die RTC nicht lesbar ist (11.07.2026 09:00) —
/// dann läuft die Uhr wenigstens korrekt WEITER, nur der Startpunkt
/// ist erfunden (der alte Zustand vor der RTC-Anbindung).
const FALLBACK_EPOCH_S: u64 = ((9_688) * 24 * 60 * 60) + 9 * 3600;

/// Ein aufgeschlüsselter Zeitpunkt (für Taskleiste, Uhr-Fenster, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatumUhrzeit {
    pub jahr: u64,
    pub monat: u64,
    pub tag: u64,
    pub stunde: u64,
    pub minute: u64,
    pub sekunde: u64,
}

/// Das aktuelle Datum samt Uhrzeit **in UTC** (UTC-Anker + TSC-Zeit).
///
/// **IMMER UTC** — siehe die Regel am Anfang dieses Abschnitts. Wer
/// Lokalzeit anzeigen will, nimmt `einstellungen::jetzt_lokal()`; wer
/// rechnet oder prüft, nimmt das hier.
pub fn jetzt() -> DatumUhrzeit {
    use core::sync::atomic::Ordering;

    let anker_s = EPOCH_ANKER_S.load(Ordering::Relaxed);
    let vergangen_s =
        us_seit_boot().saturating_sub(EPOCH_ANKER_US.load(Ordering::Relaxed)) / 1_000_000;
    if anker_s == 0 {
        return datum_von_sekunden_seit_2000(FALLBACK_EPOCH_S + ms_seit_boot() / 1000);
    }
    datum_von_sekunden_seit_2000(anker_s + vergangen_s)
}

fn schaltjahr(jahr: u64) -> bool {
    jahr.is_multiple_of(4) && (!jahr.is_multiple_of(100) || jahr.is_multiple_of(400))
}

fn tage_im_monat(jahr: u64, monat: u64) -> u64 {
    match monat {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if schaltjahr(jahr) {
                29
            } else {
                28
            }
        }
    }
}

/// Sekunden seit dem 1.1.2000 00:00:00 -> Kalenderdatum. Reine,
/// unit-getestete Funktion; läuft Jahr- und Monatsweise (schnell
/// genug — die Taskleiste ruft das einmal pro Sekunde).
pub fn datum_von_sekunden_seit_2000(gesamt: u64) -> DatumUhrzeit {
    let mut tage = gesamt / 86_400;
    let tages_sekunden = gesamt % 86_400;

    let mut jahr = 2000;
    loop {
        let jahres_tage = if schaltjahr(jahr) { 366 } else { 365 };
        if tage < jahres_tage {
            break;
        }
        tage -= jahres_tage;
        jahr += 1;
    }
    let mut monat = 1;
    loop {
        let monats_tage = tage_im_monat(jahr, monat);
        if tage < monats_tage {
            break;
        }
        tage -= monats_tage;
        monat += 1;
    }

    DatumUhrzeit {
        jahr,
        monat,
        tag: tage + 1,
        stunde: tages_sekunden / 3600,
        minute: (tages_sekunden / 60) % 60,
        sekunde: tages_sekunden % 60,
    }
}

/// Die Umkehrung: Kalenderdatum -> Sekunden seit dem 1.1.2000
/// (braucht der RTC-Anker; Roundtrip mit datum_von_sekunden_seit_2000
/// ist unit-getestet).
pub fn sekunden_seit_2000(datum: &DatumUhrzeit) -> u64 {
    let mut tage = 0;
    for jahr in 2000..datum.jahr {
        tage += if schaltjahr(jahr) { 366 } else { 365 };
    }
    for monat in 1..datum.monat {
        tage += tage_im_monat(datum.jahr, monat);
    }
    tage += datum.tag - 1;
    tage * 86_400 + datum.stunde * 3600 + datum.minute * 60 + datum.sekunde
}

// ---------------------------------------------------------------------------
// Async-Warten auf Timer-Ticks (Cursor-Blinken, Compositor, Uhr, ...)
//
// Ein async Task darf NICHT in einer Schleife pollen ("ist es schon
// soweit?") — mit yield_now wäre er immer "bereit", der Executor käme
// nie zum Schlafen, die CPU liefe auf 100 %. Stattdessen: Der Task
// deponiert seinen Waker hier, der Timer-Interrupt weckt ihn beim
// nächsten Tick. Zwischen den Ticks schläft die CPU per hlt.
//
// WICHTIG (Lektion vom Desktop-Bau): Ein einzelner AtomicWaker kann
// nur EINEN Warter halten — mit mehreren Tick-Wartern (Cursor,
// Compositor, Uhr) verhungern alle bis auf den zuletzt registrierten!
// Deshalb: eine feste Liste von Waker-SLOTS. Jede wartende Future
// belegt per lock-freiem compare_exchange einen Slot (und gibt ihn
// in Drop zurück); der Timer-Interrupt weckt ALLE belegten Slots —
// komplett ohne Locks, wie es sich für Interrupt-Pfade gehört.
// ---------------------------------------------------------------------------

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::Ordering;
use core::task::{Context, Poll};
use futures_util::task::AtomicWaker;

/// Wie viele Tasks GLEICHZEITIG auf Ticks warten können.
const MAX_TICK_WARTER: usize = 8;

/// Die Waker-Slots samt Belegt-Markierung.
static TICK_WARTER: [AtomicWaker; MAX_TICK_WARTER] =
    [const { AtomicWaker::new() }; MAX_TICK_WARTER];
static SLOT_BELEGT: [AtomicBool; MAX_TICK_WARTER] =
    [const { AtomicBool::new(false) }; MAX_TICK_WARTER];

/// Wird vom Timer-Interrupt-Handler gerufen: weckt ALLE Warter.
/// (AtomicWaker::wake ist lock-frei — interrupt-sicher.)
pub(crate) fn tick_waker_wecken() {
    for (slot, belegt) in TICK_WARTER.iter().zip(SLOT_BELEGT.iter()) {
        if belegt.load(Ordering::Acquire) {
            slot.wake();
        }
    }
}

/// Future, die beim NÄCHSTEN Timer-Tick fertig wird.
struct NaechsterTick {
    start_ticks: u64,
    /// Der belegte Waker-Slot (None = noch keiner).
    slot: Option<usize>,
}

impl NaechsterTick {
    fn slot_freigeben(&mut self) {
        if let Some(index) = self.slot.take() {
            SLOT_BELEGT[index].store(false, Ordering::Release);
        }
    }
}

impl Future for NaechsterTick {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if ticks() > self.start_ticks {
            self.slot_freigeben();
            return Poll::Ready(());
        }

        // Slot belegen (falls noch keiner): lock-freies compare_exchange.
        if self.slot.is_none() {
            for (index, belegt) in SLOT_BELEGT.iter().enumerate() {
                if belegt
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    self.slot = Some(index);
                    break;
                }
            }
        }

        match self.slot {
            Some(index) => {
                // Waker registrieren, dann NOCHMAL prüfen — schließt
                // die Race Condition, falls der Tick genau dazwischen
                // kam (gleiches Muster wie beim Tastatur-Stream).
                TICK_WARTER[index].register(cx.waker());
                if ticks() > self.start_ticks {
                    self.slot_freigeben();
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
            None => {
                // Alle Slots voll (mehr als 8 Warter): Notfall-Modus —
                // sich selbst sofort wieder einreihen (busy, aber
                // korrekt; besser als ewig zu schlafen).
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

/// Slot auch beim Abbruch der Future zurückgeben (z. B. wenn ein
/// Task mitten im warte_ms beendet wird).
impl Drop for NaechsterTick {
    fn drop(&mut self) {
        self.slot_freigeben();
    }
}

/// Wartet asynchron ungefähr `ms` Millisekunden (Auflösung: ~4 ms,
/// die PIT-Tick-Länge — für Cursor-Blinken völlig ausreichend).
pub async fn warte_ms(ms: u64) {
    let ziel = ms_seit_boot() + ms;
    while ms_seit_boot() < ziel {
        NaechsterTick {
            start_ticks: ticks(),
            slot: None,
        }
        .await;
    }
}

// ---------------------------------------------------------------------------
// Tests — laufen in QEMU über unser eigenes Test-Framework (cargo test)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Umrechnung stimmt (Werte von Hand nachgerechnet:
    /// 4.773.000 / 1.193.182 = 4,0002 ms pro Tick).
    #[test_case]
    fn test_ms_von_ticks() {
        assert_eq!(ms_von_ticks(0), 0);
        assert_eq!(ms_von_ticks(1), 4);
        assert_eq!(ms_von_ticks(100), 400);
        // ~250 Ticks sollten fast genau 1 Sekunde sein:
        assert_eq!(ms_von_ticks(250), 1000);
    }

    /// Datums-Arithmetik: bekannte Fixpunkte, Schaltjahre und der
    /// Roundtrip datum <-> Sekunden-seit-2000.
    #[test_case]
    fn test_datum_arithmetik() {
        // Der Nullpunkt der Epoche:
        let null = datum_von_sekunden_seit_2000(0);
        assert_eq!((null.jahr, null.monat, null.tag), (2000, 1, 1));
        assert_eq!((null.stunde, null.minute, null.sekunde), (0, 0, 0));

        // Tag 59 des Jahres 2000 ist der 29. Februar (Schaltjahr!):
        let schalt = datum_von_sekunden_seit_2000(59 * 86_400);
        assert_eq!((schalt.jahr, schalt.monat, schalt.tag), (2000, 2, 29));

        // 2028 ist ein Schaltjahr, 2100 keins, 2000 doch:
        assert!(schaltjahr(2028));
        assert!(!schaltjahr(2100));
        assert!(schaltjahr(2000));

        // Roundtrip über markante Daten (inkl. Schalttag und
        // Jahresende):
        for datum in [
            DatumUhrzeit { jahr: 2026, monat: 7, tag: 11, stunde: 9, minute: 30, sekunde: 59 },
            DatumUhrzeit { jahr: 2024, monat: 2, tag: 29, stunde: 23, minute: 59, sekunde: 59 },
            DatumUhrzeit { jahr: 2031, monat: 12, tag: 31, stunde: 0, minute: 0, sekunde: 1 },
        ] {
            let sekunden = sekunden_seit_2000(&datum);
            assert_eq!(datum_von_sekunden_seit_2000(sekunden), datum);
        }

        // Die Fallback-Konstante zeigt wirklich auf 11.07.2026 09:00:
        let fallback = datum_von_sekunden_seit_2000(FALLBACK_EPOCH_S);
        assert_eq!((fallback.jahr, fallback.monat, fallback.tag), (2026, 7, 11));
        assert_eq!(fallback.stunde, 9);
    }

    /// TSC-Kalibrierung: plausible Frequenz, und die TSC-Uhr misst
    /// eine PIT-Wartezeit (25 Ticks ~ 100 ms — dieselbe Dauer, die
    /// zwei warte_ms(50)-Aufrufe schlafen würden) auf +-20 % genau.
    #[test_case]
    fn test_tsc_kalibrierung_plausibel() {
        // test_kernel_main hat zeit::init() gerufen:
        assert!(tsc_frequenz_hz() > 100_000_000, "TSC-Frequenz unplausibel");

        // An eine Tick-Flanke ausrichten, dann 25 Ticks warten und
        // die Dauer mit der TSC-Uhr messen:
        let t = ticks();
        while ticks() == t {
            core::hint::spin_loop();
        }
        let start_us = us_seit_boot();
        let basis = ticks();
        while ticks() < basis + 25 {
            x86_64::instructions::hlt();
        }
        let dauer_us = us_seit_boot() - start_us;
        let erwartet_us = 25 * PIT_TEILER * 1_000_000 / PIT_BASIS_HZ; // ~100.017
        assert!(
            dauer_us > erwartet_us * 8 / 10 && dauer_us < erwartet_us * 12 / 10,
            "TSC-Messung {} us weicht zu weit von {} us ab",
            dauer_us,
            erwartet_us
        );
    }

    /// Die TSC-Uhr läuft auch unter without_interrupts weiter —
    /// die alte Mess-Falle ist tot.
    #[test_case]
    fn test_zeit_laeuft_unter_cli() {
        let vorher = us_seit_boot();
        x86_64::instructions::interrupts::without_interrupts(|| {
            // ~2M spin-Runden vergehen lassen:
            for _ in 0..2_000_000 {
                core::hint::spin_loop();
            }
            assert!(
                us_seit_boot() > vorher,
                "Zeit steht unter without_interrupts still"
            );
        });
    }

    /// Die Uhr läuft vorwärts: Nach ein paar hlt-Schlafrunden ist
    /// ms_seit_boot größer als vorher.
    #[test_case]
    fn test_zeit_laeuft_vorwaerts() {
        let vorher = ms_seit_boot();
        for _ in 0..3 {
            x86_64::instructions::hlt();
        }
        assert!(ms_seit_boot() > vorher);
    }
}
