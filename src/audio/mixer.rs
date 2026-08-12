// audio::mixer — mehrere Quellen zu einem Strom
//
// ===========================================================================
// DIE EINE REGEL, DIE ALLES ANDERE UEBERWIEGT: ES WIRD GEKLEMMT
//
// Zwei Quellen mit je 80 % Pegel ergeben 160 %. Ein `i16`, der dabei
// UEBERLAEUFT, klingt nicht etwa zu laut — er springt von +32767 auf
// -32768 und erzeugt ein KNACKEN bei voller Lautstaerke. Auf einem
// Kopfhoerer ist das nicht nur haesslich, sondern unangenehm laut.
//
// Deshalb: Zwischensumme in `i32`, am Ende auf den `i16`-Bereich
// GEKLEMMT. Das ist die klassische Uebersteuerung („Clipping") — sie
// klingt verzerrt, aber sie bleibt leise genug und stetig.
//
// `saturating_add` allein genuegt NICHT: Es klemmt je Addition, nicht
// am Ende. Bei drei Quellen (+30000, -30000, +30000) waere das Ergebnis
// mit Saettigung je Schritt 32767 + (-30000) + 30000 = 32767, richtig
// waere 30000. Also EINE Summe in `i32`, EINE Klemmung.
//
// ===========================================================================
// KEIN FLIESSKOMMA
//
// Der Kernel ist fliesskomma-frei (`-sse,+soft-float`, gemessen in
// Serie 6). Lautstaerke ist deshalb eine ganze Zahl in PROMILLE
// (0..=1000), und multipliziert wird in `i32`/`i64`. Dieselbe Loesung
// wie die UI-Skalierung in Halben und die CSS-Laengen in Tausendsteln.

use super::Sample;
use alloc::string::String;
use alloc::vec::Vec;

/// Lautstaerke in Promille. 1000 = unveraendert.
pub type Promille = u16;

/// Volle Lautstaerke.
pub const VOLL: Promille = 1000;

/// Wie viele Quellen gleichzeitig.
pub const MAX_QUELLEN: usize = 8;

/// Eine Tonquelle im Mixer.
pub struct Quelle {
    pub id: u32,
    pub name: String,
    pub lautstaerke: Promille,
    /// Die wartenden Samples (verschraenkt: L, R, L, R, …).
    puffer: Vec<Sample>,
    /// Wie weit daraus schon gemischt wurde.
    gelesen: usize,
}

impl Quelle {
    pub fn neu(id: u32, name: String) -> Quelle {
        Quelle {
            id,
            name,
            lautstaerke: VOLL,
            puffer: Vec::new(),
            gelesen: 0,
        }
    }

    /// Wie viele Samples noch bereitliegen.
    pub fn wartend(&self) -> usize {
        self.puffer.len().saturating_sub(self.gelesen)
    }

    /// Samples anhaengen.
    pub fn anhaengen(&mut self, samples: &[Sample]) {
        // Schon Gelesenes vorn wegwerfen, damit der Puffer nicht ewig
        // waechst. EINMAL je Anhaengen statt bei jedem Sample — ein
        // `Vec::drain(0..n)` je Frame waere ein memmove je Frame.
        if self.gelesen > 0 && self.gelesen == self.puffer.len() {
            self.puffer.clear();
            self.gelesen = 0;
        } else if self.gelesen > 4096 {
            self.puffer.drain(0..self.gelesen);
            self.gelesen = 0;
        }
        self.puffer.extend_from_slice(samples);
    }

    /// Ist die Quelle leergelaufen?
    pub fn leer(&self) -> bool {
        self.wartend() == 0
    }
}

/// Lautstaerke auf ein Sample anwenden.
///
/// In `i32` gerechnet und ZURUECKGEKLEMMT: Bei `lautstaerke > VOLL`
/// (Verstaerkung) kann das Ergebnis sonst ueberlaufen.
pub fn skalieren(sample: Sample, lautstaerke: Promille) -> Sample {
    let wert = (sample as i32 * lautstaerke as i32) / VOLL as i32;
    klemmen(wert)
}

/// Einen `i32` auf den `i16`-Bereich klemmen.
///
/// **Das Herz des Mixers.** Siehe Kopfkommentar: Wrappen klingt wie
/// ein Knacken bei voller Lautstaerke, Klemmen wie Verzerrung.
pub fn klemmen(wert: i32) -> Sample {
    if wert > Sample::MAX as i32 {
        Sample::MAX
    } else if wert < Sample::MIN as i32 {
        Sample::MIN
    } else {
        wert as Sample
    }
}

/// Der Mixer.
pub struct Mixer {
    quellen: Vec<Quelle>,
    /// Gesamtlautstaerke — das, was der Systray-Regler stellt.
    pub gesamt: Promille,
    /// Stumm? Getrennt von `gesamt`, damit Stummschalten die
    /// eingestellte Lautstaerke NICHT vergisst.
    pub stumm: bool,
    naechste_id: u32,
}

impl Default for Mixer {
    fn default() -> Self {
        Mixer {
            quellen: Vec::new(),
            gesamt: VOLL,
            stumm: false,
            naechste_id: 1,
        }
    }
}

impl Mixer {
    /// Eine Quelle anmelden. `None`, wenn kein Platz mehr ist.
    pub fn anmelden(&mut self, name: String) -> Option<u32> {
        if self.quellen.len() >= MAX_QUELLEN {
            return None;
        }
        let id = self.naechste_id;
        self.naechste_id += 1;
        self.quellen.push(Quelle::neu(id, name));
        Some(id)
    }

    pub fn abmelden(&mut self, id: u32) {
        self.quellen.retain(|q| q.id != id);
    }

    pub fn quelle_mut(&mut self, id: u32) -> Option<&mut Quelle> {
        self.quellen.iter_mut().find(|q| q.id == id)
    }

    pub fn quellen(&self) -> &[Quelle] {
        &self.quellen
    }

    pub fn anzahl(&self) -> usize {
        self.quellen.len()
    }

    /// **DAS MISCHEN.** Fuellt `ziel` mit der Summe aller Quellen.
    ///
    /// Liefert, wie viele Samples wirklich Ton enthielten — 0 heisst
    /// Stille, und der Aufrufer kann daran erkennen, dass er die
    /// Wiedergabe anhalten darf.
    ///
    /// `ziel` wird IMMER vollstaendig beschrieben (fehlende Samples
    /// werden zu Stille). Ein halb gefuellter Puffer waere sonst der
    /// Rest des vorigen Durchgangs — und das hoert man als Stottern.
    pub fn mischen(&mut self, ziel: &mut [Sample]) -> usize {
        let gesamt = if self.stumm { 0 } else { self.gesamt };
        let mut mit_ton = 0usize;

        for (i, platz) in ziel.iter_mut().enumerate() {
            // EINE Summe in i32 ueber alle Quellen — nicht je Schritt
            // saettigen (siehe Kopfkommentar).
            let mut summe: i32 = 0;
            let mut hatte_ton = false;
            for quelle in self.quellen.iter() {
                let pos = quelle.gelesen + i;
                if pos < quelle.puffer.len() {
                    let s = quelle.puffer[pos] as i32 * quelle.lautstaerke as i32 / VOLL as i32;
                    summe += s;
                    hatte_ton = true;
                }
            }
            if hatte_ton {
                mit_ton += 1;
            }
            summe = summe * gesamt as i32 / VOLL as i32;
            *platz = klemmen(summe);
        }

        // Die Leseposition aller Quellen vorruecken.
        for quelle in self.quellen.iter_mut() {
            quelle.gelesen = (quelle.gelesen + ziel.len()).min(quelle.puffer.len());
        }
        mit_ton
    }

    /// Quellen entfernen, die leergelaufen sind und nichts mehr
    /// nachliefern.
    pub fn leere_entfernen(&mut self) -> usize {
        let vorher = self.quellen.len();
        self.quellen.retain(|q| !q.leer());
        vorher - self.quellen.len()
    }
}

// ===========================================================================
// EINEN SINUS ERZEUGEN — ohne Fliesskomma
// ===========================================================================

/// Eine Viertelperiode Sinus, in 1/32767, an 65 Stuetzstellen.
///
/// ===================================================================
/// WARUM EINE TABELLE UND KEINE FORMEL
///
/// Es gibt kein Fliesskomma im Kernel. Einen Sinus per Taylor-Reihe in
/// Ganzzahlarithmetik zu rechnen ginge, waere aber deutlich mehr Code
/// als 65 Zahlen — und die Tabelle ist exakt nachpruefbar.
///
/// Gespeichert ist nur die ERSTE VIERTELPERIODE; die anderen drei
/// ergeben sich durch Spiegeln. Das ist die uebliche Loesung und spart
/// drei Viertel der Tabelle.
static SINUS_VIERTEL: [i16; 65] = [
    0, 804, 1608, 2410, 3212, 4011, 4808, 5602, 6393, 7179, 7962, 8739, 9512, 10278, 11039, 11793,
    12539, 13279, 14010, 14732, 15446, 16151, 16846, 17530, 18204, 18868, 19519, 20159, 20787,
    21403, 22005, 22594, 23170, 23731, 24279, 24811, 25329, 25832, 26319, 26790, 27245, 27683,
    28105, 28510, 28898, 29268, 29621, 29956, 30273, 30571, 30852, 31113, 31356, 31580, 31785,
    31971, 32137, 32285, 32412, 32521, 32609, 32678, 32728, 32757, 32767,
];

/// Sinus fuer einen Winkel in 1/256 Umdrehung. Ergebnis in 1/32767.
///
/// Der Winkel laeuft also 0..256 fuer eine volle Periode — eine
/// Zweierpotenz, damit der Umlauf ein `& 0xFF` ist und keine Division.
pub fn sinus(phase: u32) -> i16 {
    let p = (phase & 0xFF) as usize;
    match p {
        0..=63 => SINUS_VIERTEL[p],
        64..=127 => SINUS_VIERTEL[128 - p],
        128..=191 => -SINUS_VIERTEL[p - 128],
        _ => -SINUS_VIERTEL[256 - p],
    }
}

/// Einen Sinuston erzeugen (Stereo, verschraenkt).
///
/// `hz` ist die Frequenz, `frames` die Zahl der Stereo-Frames.
/// **Die Phase laeuft in 1/256-Schritten fest** — bei 48 kHz und 440 Hz
/// sind das 2,35 Schritte je Frame. Weil ganzzahlig gerechnet wird,
/// laeuft die Phase in 16.16-Festkomma, sonst waere jede Frequenz auf
/// ein Vielfaches von 187 Hz gerundet.
pub fn sinus_erzeugen(hz: u32, frames: usize, lautstaerke: Promille) -> Vec<Sample> {
    sinus_erzeugen_ab(hz, frames, lautstaerke, 0)
}

/// Wie `sinus_erzeugen`, aber ab einem bestimmten FRAME.
///
/// ===================================================================
/// WOFUER DER STARTPUNKT DA IST
///
/// Ein langer Ton wird stueckweise nachgefuellt (der Ringpuffer fasst
/// nur ~85 ms). Wuerde jedes Stueck bei Phase 0 anfangen, gaebe es an
/// JEDER Stueckgrenze einen Sprung in der Wellenform — und ein Sprung
/// klingt als Knacks. Mit dem Startframe laeuft die Phase durch.
pub fn sinus_erzeugen_ab(
    hz: u32,
    frames: usize,
    lautstaerke: Promille,
    ab_frame: u64,
) -> Vec<Sample> {
    let mut aus = Vec::with_capacity(frames * super::KANAELE);
    if hz == 0 {
        aus.resize(frames * super::KANAELE, 0);
        return aus;
    }
    // Phasenschritt je Frame in 16.16-Festkomma:
    // 256 Schritte je Periode, hz Perioden je Sekunde, ABTASTRATE
    // Frames je Sekunde.
    let schritt = ((256u64 << 16) * hz as u64 / super::ABTASTRATE as u64) as u32;
    // Der Startpunkt: Schritt mal Frame-Nummer, umlaufend.
    let mut phase: u32 = (schritt as u64).wrapping_mul(ab_frame) as u32;
    for _ in 0..frames {
        let wert = skalieren(sinus(phase >> 16), lautstaerke);
        // Beide Kanaele gleich — ein Ton, keine Stereo-Kunst.
        aus.push(wert);
        aus.push(wert);
        phase = phase.wrapping_add(schritt);
    }
    aus
}

// ===========================================================================
// TESTS
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    /// **DIE WICHTIGSTE ZUSAGE: es wird geklemmt, nicht gewrappt.**
    #[test_case]
    fn test_klemmen_statt_ueberlauf() {
        assert_eq!(klemmen(100_000), i16::MAX);
        assert_eq!(klemmen(-100_000), i16::MIN);
        assert_eq!(klemmen(0), 0);
        assert_eq!(klemmen(32767), 32767);
        assert_eq!(klemmen(32768), 32767, "genau eins darueber klemmt");
        assert_eq!(klemmen(-32768), -32768);
        assert_eq!(klemmen(-32769), -32768);
    }

    /// Zwei laute Quellen zusammen: verzerrt, aber nicht umgeklappt.
    #[test_case]
    fn test_zwei_laute_quellen_uebersteuern_ohne_knacken() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        let b = m.anmelden(String::from("b")).unwrap();
        m.quelle_mut(a).unwrap().anhaengen(&[30000, 30000]);
        m.quelle_mut(b).unwrap().anhaengen(&[30000, 30000]);
        let mut ziel = [0i16; 2];
        m.mischen(&mut ziel);
        // 60000 wuerde als i16 zu -5536 umklappen — DAS ist das Knacken.
        assert_eq!(ziel[0], i16::MAX);
        assert_eq!(ziel[1], i16::MAX);
    }

    /// Negativ genauso.
    #[test_case]
    fn test_negative_uebersteuerung() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        let b = m.anmelden(String::from("b")).unwrap();
        m.quelle_mut(a).unwrap().anhaengen(&[-30000]);
        m.quelle_mut(b).unwrap().anhaengen(&[-30000]);
        let mut ziel = [0i16; 1];
        m.mischen(&mut ziel);
        assert_eq!(ziel[0], i16::MIN);
    }

    /// **EINE Summe, nicht Saettigung je Schritt.** Drei Quellen, die
    /// sich gegenseitig aufheben, muessen das Ergebnis 30000 liefern —
    /// bei Saettigung je Addition kaeme 32767 heraus.
    #[test_case]
    fn test_summe_wird_nicht_je_schritt_gesaettigt() {
        let mut m = Mixer::default();
        for (name, wert) in [("a", 30000i16), ("b", -30000), ("c", 30000)] {
            let id = m.anmelden(String::from(name)).unwrap();
            m.quelle_mut(id).unwrap().anhaengen(&[wert]);
        }
        let mut ziel = [0i16; 1];
        m.mischen(&mut ziel);
        assert_eq!(ziel[0], 30000, "Zwischensumme in i32, EINE Klemmung");
    }

    #[test_case]
    fn test_lautstaerke_je_quelle() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        m.quelle_mut(a).unwrap().anhaengen(&[10000]);
        m.quelle_mut(a).unwrap().lautstaerke = 500; // 50 %
        let mut ziel = [0i16; 1];
        m.mischen(&mut ziel);
        assert_eq!(ziel[0], 5000);
    }

    #[test_case]
    fn test_gesamtlautstaerke_und_stumm() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        m.quelle_mut(a).unwrap().anhaengen(&[10000, 10000]);
        m.gesamt = 250; // 25 %
        let mut ziel = [0i16; 1];
        m.mischen(&mut ziel);
        assert_eq!(ziel[0], 2500);
        // Stumm schaltet aus, VERGISST aber die Lautstaerke nicht.
        m.stumm = true;
        let mut ziel = [0i16; 1];
        m.mischen(&mut ziel);
        assert_eq!(ziel[0], 0);
        assert_eq!(m.gesamt, 250, "die Einstellung bleibt erhalten");
    }

    /// Ohne Quellen ist es STILL — und der Puffer wird trotzdem ganz
    /// beschrieben. Ein halb gefuellter Puffer waere der Rest des
    /// vorigen Durchgangs, und das hoert man als Stottern.
    #[test_case]
    fn test_ohne_quellen_ist_es_still() {
        let mut m = Mixer::default();
        let mut ziel = [1234i16; 8];
        let ton = m.mischen(&mut ziel);
        assert_eq!(ton, 0);
        assert!(ziel.iter().all(|&s| s == 0), "alles genullt: {:?}", ziel);
    }

    /// Eine Quelle, die kuerzer ist als der Zielpuffer, fuellt den Rest
    /// mit Stille — nicht mit Wiederholung.
    #[test_case]
    fn test_kurze_quelle_fuellt_mit_stille() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        m.quelle_mut(a).unwrap().anhaengen(&[1000, 2000]);
        let mut ziel = [9999i16; 4];
        let ton = m.mischen(&mut ziel);
        assert_eq!(ton, 2);
        assert_eq!(ziel, [1000, 2000, 0, 0]);
    }

    #[test_case]
    fn test_quellen_grenze() {
        let mut m = Mixer::default();
        for i in 0..MAX_QUELLEN {
            assert!(m.anmelden(alloc::format!("q{}", i)).is_some());
        }
        assert!(m.anmelden(String::from("zuviel")).is_none());
        assert_eq!(m.anzahl(), MAX_QUELLEN);
    }

    #[test_case]
    fn test_abmelden_und_leere_entfernen() {
        let mut m = Mixer::default();
        let a = m.anmelden(String::from("a")).unwrap();
        let b = m.anmelden(String::from("b")).unwrap();
        m.quelle_mut(b).unwrap().anhaengen(&[1, 2, 3]);
        // `a` hat nie etwas geliefert -> leer.
        assert_eq!(m.leere_entfernen(), 1);
        assert_eq!(m.anzahl(), 1);
        m.abmelden(b);
        assert_eq!(m.anzahl(), 0);
        let _ = a;
    }

    // -------------------------------------------------------------------
    // SINUS
    // -------------------------------------------------------------------

    #[test_case]
    fn test_sinus_viertelperioden() {
        assert_eq!(sinus(0), 0, "Nulldurchgang");
        assert_eq!(sinus(64), 32767, "Maximum bei einer Viertelperiode");
        assert_eq!(sinus(128), 0, "Nulldurchgang bei der Haelfte");
        assert_eq!(sinus(192), -32767, "Minimum bei drei Vierteln");
        // Symmetrie: sin(x) == -sin(x + 128)
        for x in 0..128u32 {
            assert_eq!(sinus(x), -sinus(x + 128), "Symmetrie bei {}", x);
        }
    }

    #[test_case]
    fn test_sinus_laeuft_im_kreis() {
        for x in 0..256u32 {
            assert_eq!(sinus(x), sinus(x + 256));
            assert_eq!(sinus(x), sinus(x + 2560));
        }
    }

    #[test_case]
    fn test_sinus_erzeugen_laenge_und_stereo() {
        let s = sinus_erzeugen(440, 100, VOLL);
        assert_eq!(s.len(), 200, "100 Frames Stereo = 200 Samples");
        // Beide Kanaele gleich.
        for f in 0..100 {
            assert_eq!(s[f * 2], s[f * 2 + 1]);
        }
    }

    #[test_case]
    fn test_sinus_null_hertz_ist_stille() {
        let s = sinus_erzeugen(0, 10, VOLL);
        assert_eq!(s.len(), 20);
        assert!(s.iter().all(|&x| x == 0));
    }

    /// Eine hoehere Frequenz muss in derselben Zeit mehr Nulldurchgaenge
    /// haben — der einfachste Test, der wirklich etwas ueber die
    /// Frequenz aussagt.
    #[test_case]
    fn test_hoehere_frequenz_hat_mehr_nulldurchgaenge() {
        fn durchgaenge(hz: u32) -> usize {
            let s = sinus_erzeugen(hz, 4800, VOLL); // 100 ms
            s.chunks(2)
                .map(|c| c[0])
                .collect::<Vec<_>>()
                .windows(2)
                .filter(|w| (w[0] < 0) != (w[1] < 0))
                .count()
        }
        let tief = durchgaenge(220);
        let hoch = durchgaenge(880);
        assert!(hoch > tief * 3, "880 Hz: {} gegen 220 Hz: {}", hoch, tief);
    }

    #[test_case]
    fn test_sinus_lautstaerke_wirkt() {
        let laut = sinus_erzeugen(440, 100, VOLL);
        let leise = sinus_erzeugen(440, 100, 100); // 10 %
        let max_laut = laut.iter().map(|s| s.abs() as i32).max().unwrap();
        let max_leise = leise.iter().map(|s| s.abs() as i32).max().unwrap();
        assert!(max_leise * 5 < max_laut, "{} gegen {}", max_leise, max_laut);
    }
}
