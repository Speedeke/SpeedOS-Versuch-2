// spielen — eine WAV-Datei abspielen
//
// ===========================================================================
// DIE GANZE KETTE IN EINEM PROGRAMM
//
//   hole http://.../ton.wav /platte/ton.wav      (Serie 7)
//   spielen /platte/ton.wav                      (hier)
//
// Datei lesen -> Kopf pruefen -> PCM umrechnen -> an den Mixer -> Ton.
// Jeder Schritt davon existierte schon; dieses Programm ist der Beweis,
// dass die Naehte zusammenpassen.
//
// ===========================================================================
// DER WAV-PARSER LIEGT IM KERNEL UND NICHT HIER — und das ist eine
// Entscheidung, keine Bequemlichkeit
//
// Auf den ersten Blick gehoert ein Parser fuer fremde Dateien nach Ring 3
// (so wie `pem.rs` und der Bilddekoder). Der Unterschied: `audio::wav`
// ist eine REINE FUNKTION auf `&[u8]` ohne unsafe, ohne Allokation im
// Fehlerfall und ohne jede Ressource — sie kann nichts kaputtmachen,
// was ein Fehler in ihr nicht ohnehin nur bei sich selbst anrichtet.
// Und sie wird an ZWEI Stellen gebraucht: hier und spaeter im
// Datei-Explorer fuer die Vorschau. Zweimal derselbe Parser waere
// zweimal derselbe Off-by-one.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use libspeed::{audio, hauptprogramm, print, println, Argumente};

hauptprogramm!(haupt);

const OK: i32 = 0;
const FEHLER_ARGUMENTE: i32 = 1;
const FEHLER_DATEI: i32 = 2;
const FEHLER_FORMAT: i32 = 3;
const FEHLER_AUDIO: i32 = 4;

/// Wie viele Samples je Durchgang an den Kernel gehen.
///
/// 16 384 Samples sind rund 170 ms — genau die Syscall-Grenze. Groesser
/// waere ein `UngueltigesArgument`, kleiner nur mehr Syscalls.
const STUECK: usize = 16_384;

fn haupt(argumente: &Argumente) -> i32 {
    let Some(pfad) = argumente.get(1) else {
        println!("Aufruf: spielen <datei.wav>");
        println!("");
        println!("Spielt eine unkomprimierte WAV-Datei ab (PCM, 8 oder 16 Bit).");
        println!("Beispiel:");
        println!("  hole http://example.com/ton.wav /platte/ton.wav");
        println!("  spielen /platte/ton.wav");
        return FEHLER_ARGUMENTE;
    };

    // --- 1. Datei lesen ---
    let daten = match libspeed::netz::datei_lesen(pfad) {
        Ok(d) => d,
        Err(f) => {
            println!("Datei nicht lesbar: {}", f.text());
            return FEHLER_DATEI;
        }
    };
    println!("{} — {} Byte", pfad, daten.len());

    // --- 2. Kopf pruefen ---
    //
    // JEDE ZAHL IN DER DATEI IST EINE BEHAUPTUNG. Der Parser klemmt
    // Laengen gegen die echte Dateigroesse und panickt nie; wir muessen
    // hier nur noch den Fehlerfall ANZEIGEN statt ihn zu verschlucken
    // (Daten-Integritaets-Regel).
    let info = match speed_os_wav::kopf_lesen(&daten) {
        Ok(i) => i,
        Err(f) => {
            println!("Keine brauchbare WAV-Datei: {}", f.text());
            return FEHLER_FORMAT;
        }
    };
    println!(
        "  {} Hz, {} Kanal/Kanaele, {} Bit — {} Frames, {} ms",
        info.rate,
        info.kanaele,
        info.bits,
        info.frames(),
        info.dauer_ms()
    );
    if info.gekuerzt {
        // EIN ABGESCHNITTENER DOWNLOAD WIRD GENANNT, nicht verschwiegen.
        // Die Datei behauptet mehr, als sie hergibt — wir spielen, was
        // da ist, und sagen es.
        println!("  ACHTUNG: Die Datei ist kuerzer, als ihr Kopf behauptet.");
    }
    // DIE ABTASTRATE WIRD NICHT UMGERECHNET (docs/grenzen.md). Statt
    // stillschweigend zu schnell zu spielen, wird es GESAGT.
    if info.rate != audio::ABTASTRATE {
        println!(
            "  ACHTUNG: {} Hz statt {} Hz — klingt {} zu {}.",
            info.rate,
            audio::ABTASTRATE,
            if info.rate < audio::ABTASTRATE { "hoeher" } else { "tiefer" },
            if info.rate < audio::ABTASTRATE { "schnell" } else { "langsam" }
        );
    }

    // --- 3. In unser Format bringen: 16 Bit, Stereo, verschraenkt ---
    let samples = speed_os_wav::samples_lesen(&daten, &info);
    if samples.is_empty() {
        println!("Keine Samples in der Datei.");
        return FEHLER_FORMAT;
    }

    // --- 4. Abspielen ---
    let mut strom = match audio::Strom::oeffnen() {
        Ok(s) => s,
        Err(f) if f == libspeed::Fehler::NICHT_KONFIGURIERT => {
            println!("Kein Audio-Geraet vorhanden.");
            return FEHLER_AUDIO;
        }
        Err(f) => {
            println!("Tonquelle liess sich nicht oeffnen: {}", f.text());
            return FEHLER_AUDIO;
        }
    };

    let gesamt = samples.len();
    let mut geschickt = 0usize;
    let mut letzte_anzeige = 101usize;

    while geschickt < gesamt {
        let rest = &samples[geschickt..];
        let stueck = &rest[..rest.len().min(STUECK)];
        match strom.nachfuellen(stueck) {
            // 0 heisst „Vorlauf voll" — warten, nicht abbrechen.
            Ok(0) => {
                libspeed::abgeben();
            }
            Ok(n) => {
                geschickt += n;
                // FORTSCHRITT NUR BEI ECHTER AENDERUNG. Bei jedem
                // Durchgang zu drucken hiesse hunderte Zeilen fuer ein
                // paar Sekunden Ton.
                let prozent = geschickt * 100 / gesamt;
                if prozent != letzte_anzeige {
                    fortschritt(prozent);
                    letzte_anzeige = prozent;
                }
            }
            Err(f) => {
                println!("");
                println!("Fehler beim Abspielen: {}", f.text());
                return FEHLER_AUDIO;
            }
        }
    }

    // --- 5. Auslaufen lassen ---
    //
    // Alles ist UEBERGEBEN, aber noch nicht GESPIELT: Im Mixer stehen
    // bis zu fuenf Sekunden Vorlauf. Wer hier endet, schneidet den Ton
    // ab — der Prozess stirbt, sein Handle faellt, und die Quelle wird
    // abgemeldet.
    while let Ok(wartend) = strom.wartend() {
        if wartend == 0 {
            break;
        }
        libspeed::abgeben();
    }
    fortschritt(100);
    println!("");
    println!("Fertig.");
    OK
}

/// Eine Fortschrittszeile, die sich selbst ueberschreibt.
fn fortschritt(prozent: usize) {
    let breite = 30usize;
    let voll = prozent * breite / 100;
    print!("\r  [");
    for i in 0..breite {
        if i < voll {
            print!("#");
        } else {
            print!("-");
        }
    }
    print!("] {:>3} %", prozent);
}

// ===========================================================================
// DER WAV-PARSER
//
// Er lebt im Kernel (`speed_os::audio::wav`) und ist eine reine Funktion
// auf `&[u8]`. Weil userland/ KEINE Kernel-Abhaengigkeit haben darf (die
// ABI ist ein Vertrag, kein geteilter Header — Serie 6, Teil 5), steht
// hier eine schlanke Kopie der Schnittstelle, die dieselben Regeln
// befolgt.
//
// **Das ist die einzige Stelle im Programm, an der etwas doppelt
// existiert**, und es ist dieselbe bewusste Doppelung wie bei den
// ABI-Konstanten und der Tastatur-Uebersetzung.
// ===========================================================================
mod speed_os_wav {
    use alloc::vec::Vec;

    pub const MAX_CHUNKS: usize = 64;
    const FORMAT_PCM: u16 = 1;
    const FORMAT_ERWEITERT: u16 = 0xFFFE;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WavFehler {
        ZuKurz,
        KeinWav,
        KeinFormat,
        KeineDaten,
        NichtPcm,
        BittiefeNichtUnterstuetzt,
        UnsinnigeWerte,
    }

    impl WavFehler {
        pub fn text(self) -> &'static str {
            match self {
                WavFehler::ZuKurz => "Datei zu kurz fuer ein WAV",
                WavFehler::KeinWav => "kein RIFF/WAVE-Kopf",
                WavFehler::KeinFormat => "kein fmt-Chunk",
                WavFehler::KeineDaten => "kein data-Chunk",
                WavFehler::NichtPcm => "kein unkomprimiertes PCM",
                WavFehler::BittiefeNichtUnterstuetzt => "nur 8 oder 16 Bit",
                WavFehler::UnsinnigeWerte => "unsinnige Format-Angaben",
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WavInfo {
        pub kanaele: u16,
        pub rate: u32,
        pub bits: u16,
        pub daten_start: usize,
        pub daten_bytes: usize,
        pub gekuerzt: bool,
    }

    impl WavInfo {
        pub fn frames(&self) -> usize {
            let je_frame = self.kanaele as usize * (self.bits as usize / 8);
            if je_frame == 0 {
                return 0;
            }
            self.daten_bytes / je_frame
        }
        pub fn dauer_ms(&self) -> u64 {
            if self.rate == 0 {
                return 0;
            }
            self.frames() as u64 * 1000 / self.rate as u64
        }
    }

    pub fn kopf_lesen(daten: &[u8]) -> Result<WavInfo, WavFehler> {
        if daten.len() < 20 {
            return Err(WavFehler::ZuKurz);
        }
        if &daten[0..4] != b"RIFF" || &daten[8..12] != b"WAVE" {
            return Err(WavFehler::KeinWav);
        }
        let mut kanaele = 0u16;
        let mut rate = 0u32;
        let mut bits = 0u16;
        let mut format = 0u16;
        let mut daten_start = 0usize;
        let mut daten_bytes = 0usize;
        let mut gekuerzt = false;
        let mut format_gefunden = false;

        let mut pos = 12usize;
        let mut runden = 0usize;
        while pos + 8 <= daten.len() {
            runden += 1;
            if runden > MAX_CHUNKS {
                break;
            }
            let kennung = &daten[pos..pos + 4];
            let groesse = u32le(daten, pos + 4) as usize;
            match kennung {
                b"fmt " => {
                    if groesse >= 16 && pos + 8 + 16 <= daten.len() {
                        let f = pos + 8;
                        format = u16le(daten, f);
                        kanaele = u16le(daten, f + 2);
                        rate = u32le(daten, f + 4);
                        bits = u16le(daten, f + 14);
                        if format == FORMAT_ERWEITERT
                            && groesse >= 26
                            && pos + 8 + 26 <= daten.len()
                        {
                            format = u16le(daten, f + 24);
                        }
                        format_gefunden = true;
                    }
                }
                b"data" => {
                    daten_start = pos + 8;
                    let verfuegbar = daten.len().saturating_sub(daten_start);
                    if groesse > verfuegbar {
                        gekuerzt = true;
                        daten_bytes = verfuegbar;
                    } else {
                        daten_bytes = groesse;
                    }
                }
                _ => {}
            }
            let schritt = groesse + (groesse & 1);
            pos = match pos.checked_add(8).and_then(|p| p.checked_add(schritt)) {
                Some(p) => p,
                None => break,
            };
        }

        if !format_gefunden {
            return Err(WavFehler::KeinFormat);
        }
        if daten_start == 0 {
            return Err(WavFehler::KeineDaten);
        }
        if format != FORMAT_PCM {
            return Err(WavFehler::NichtPcm);
        }
        if kanaele == 0 || kanaele > 8 || !(1000..=384_000).contains(&rate) {
            return Err(WavFehler::UnsinnigeWerte);
        }
        if bits != 8 && bits != 16 {
            return Err(WavFehler::BittiefeNichtUnterstuetzt);
        }
        Ok(WavInfo {
            kanaele,
            rate,
            bits,
            daten_start,
            daten_bytes,
            gekuerzt,
        })
    }

    pub fn samples_lesen(daten: &[u8], info: &WavInfo) -> Vec<i16> {
        let mut aus = Vec::new();
        let ende = (info.daten_start + info.daten_bytes).min(daten.len());
        if info.daten_start >= ende {
            return aus;
        }
        let roh = &daten[info.daten_start..ende];
        let kanaele = info.kanaele as usize;
        let bytes_je_sample = info.bits as usize / 8;
        let je_frame = kanaele * bytes_je_sample;
        if je_frame == 0 {
            return aus;
        }
        let frames = roh.len() / je_frame;
        aus.reserve(frames * 2);
        for f in 0..frames {
            let basis = f * je_frame;
            let hole = |kanal: usize| -> i16 {
                let k = kanal.min(kanaele - 1);
                let p = basis + k * bytes_je_sample;
                match bytes_je_sample {
                    // 8-Bit-WAV ist UNSIGNED mit Mitte 128.
                    1 => ((roh[p] as i16) - 128) << 8,
                    _ => i16::from_le_bytes([roh[p], roh[p + 1]]),
                }
            };
            let links = hole(0);
            let rechts = if kanaele >= 2 { hole(1) } else { links };
            aus.push(links);
            aus.push(rechts);
        }
        aus
    }

    fn u16le(d: &[u8], p: usize) -> u16 {
        u16::from_le_bytes([d[p], d[p + 1]])
    }
    fn u32le(d: &[u8], p: usize) -> u32 {
        u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]])
    }
}
