// audio::wav — eine WAV-Datei lesen
//
// ===========================================================================
// EINE FREMDE DATEI IST FEINDLICH — dieselbe Haltung wie ueberall
//
// Ein WAV kommt vom Netz oder vom USB-Stick. Es ist damit dieselbe Sorte
// Daten wie ein ELF-Header, ein PNG, ein HTTP-Rumpf oder ein
// USB-Deskriptor: **jede Zahl darin ist eine Behauptung.** Der Parser
// ist deshalb eine REINE FUNKTION auf `&[u8]`, ohne unsafe, ohne Locks,
// und er panickt nie.
//
// Die drei Regeln sind wieder dieselben:
//   (1) Jedes Laengenfeld gegen die TATSAECHLICHE Dateigroesse pruefen.
//   (2) Keine Schleife ohne Obergrenze (die Chunk-Kette!).
//   (3) Lieber kuerzen als ablehnen — ein halbes Lied ist besser als
//       eine Fehlermeldung.
//
// ===========================================================================
// DER AUFBAU
//
//   "RIFF" | Groesse (4) | "WAVE"
//   dann eine Kette aus Chunks:  Kennung (4) | Groesse (4) | Daten
//
// Gebraucht werden zwei davon: `fmt ` (Format) und `data` (Samples).
// Dazwischen koennen beliebige andere stehen (`LIST`, `fact`, `cue `) —
// die werden UEBERSPRUNGEN, nicht abgelehnt. Wer nur „fmt kommt zuerst,
// data kommt zweitens" annimmt, scheitert an jeder Datei, die ein
// Programm mit Metadaten geschrieben hat.

use alloc::vec::Vec;

/// Wie viele Chunks eine Kette haben darf, bevor abgebrochen wird.
/// Der zweite Riegel gegen Endlosschleifen (Regel 2).
pub const MAX_CHUNKS: usize = 64;

/// PCM-Format-Kennung.
const FORMAT_PCM: u16 = 1;
/// Erweiterte Kennung — traegt das echte Format in einem Extra-Feld.
const FORMAT_ERWEITERT: u16 = 0xFFFE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavFehler {
    /// Kleiner als der kleinste moegliche Header.
    ZuKurz,
    /// Kein „RIFF"/„WAVE".
    KeinWav,
    /// Kein `fmt `-Chunk gefunden.
    KeinFormat,
    /// Kein `data`-Chunk gefunden.
    KeineDaten,
    /// Kein PCM (z. B. MP3 in einem WAV-Mantel).
    NichtPcm,
    /// Bittiefe, die wir nicht koennen.
    BittiefeNichtUnterstuetzt,
    /// 0 Kanaele, 0 Hz oder absurde Werte.
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

/// Was in einer WAV-Datei steht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub kanaele: u16,
    pub rate: u32,
    pub bits: u16,
    /// Wo die Samples anfangen und wie lang sie sind — **schon gegen
    /// die echte Dateigroesse geklemmt**.
    pub daten_start: usize,
    pub daten_bytes: usize,
    /// Musste die Laenge gekuerzt werden, weil der Chunk mehr behauptet
    /// hat, als die Datei hergibt? (Abgeschnittene Downloads.)
    pub gekuerzt: bool,
}

impl WavInfo {
    /// Wie viele FRAMES (alle Kanaele zusammen) enthalten sind.
    pub fn frames(&self) -> usize {
        let je_frame = self.kanaele as usize * (self.bits as usize / 8);
        if je_frame == 0 {
            return 0;
        }
        self.daten_bytes / je_frame
    }

    /// Die Spieldauer in Millisekunden.
    pub fn dauer_ms(&self) -> u64 {
        if self.rate == 0 {
            return 0;
        }
        self.frames() as u64 * 1000 / self.rate as u64
    }
}

/// Den Kopf lesen. **Liest die Samples NICHT** — bei einer 40-MiB-Datei
/// waere das eine Kopie, die niemand bestellt hat.
pub fn kopf_lesen(daten: &[u8]) -> Result<WavInfo, WavFehler> {
    // 12 Byte RIFF-Kopf + mindestens ein Chunk-Kopf.
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

    // DIE CHUNK-KETTE. `pos` steht auf dem naechsten Chunk-Kopf.
    let mut pos = 12usize;
    let mut runden = 0usize;
    while pos + 8 <= daten.len() {
        runden += 1;
        // RIEGEL 2: der harte Zaehler — auch wenn jede Groesse > 0 ist,
        // koennte eine absurd lange Kette uns festhalten.
        if runden > MAX_CHUNKS {
            break;
        }
        let kennung = &daten[pos..pos + 4];
        let groesse = u32le(daten, pos + 4) as usize;

        match kennung {
            b"fmt " => {
                // Ein `fmt `-Chunk hat mindestens 16 Byte.
                if groesse >= 16 && pos + 8 + 16 <= daten.len() {
                    let f = pos + 8;
                    format = u16le(daten, f);
                    kanaele = u16le(daten, f + 2);
                    rate = u32le(daten, f + 4);
                    bits = u16le(daten, f + 14);
                    // Beim erweiterten Format steht das ECHTE Format im
                    // Extra-Block (Offset 24). Ohne diesen Zweig gaelte
                    // jede 24-Bit-Aufnahme als „kein PCM".
                    if format == FORMAT_ERWEITERT && groesse >= 26 && pos + 8 + 26 <= daten.len() {
                        format = u16le(daten, f + 24);
                    }
                    format_gefunden = true;
                }
            }
            b"data" => {
                daten_start = pos + 8;
                // DIE LAENGE IST EINE BEHAUPTUNG. Ein abgeschnittener
                // Download behauptet die volle Laenge und hat sie nicht
                // — dann wird GEKUERZT statt abgelehnt (Regel 3): Ein
                // halbes Lied ist besser als eine Fehlermeldung.
                let verfuegbar = daten.len().saturating_sub(daten_start);
                if groesse > verfuegbar {
                    gekuerzt = true;
                    daten_bytes = verfuegbar;
                } else {
                    daten_bytes = groesse;
                }
            }
            _ => {} // LIST, fact, cue … uebersprungen, nicht abgelehnt
        }

        // WEITER. Chunks sind auf GERADE Groessen ausgerichtet — ein
        // ungerader Chunk hat ein Fuellbyte dahinter. Wer das
        // uebersieht, liest ab dem ersten ungeraden Chunk Muell.
        let schritt = groesse + (groesse & 1);
        // RIEGEL 1: Ohne Fortschritt waere es eine Endlosschleife.
        // `groesse == 0` ist ein gueltiger (leerer) Chunk, deshalb
        // bringt der Kopf selbst die 8 Byte.
        pos = match pos.checked_add(8).and_then(|p| p.checked_add(schritt)) {
            Some(p) => p,
            None => break, // Ueberlauf: die Groesse war absurd
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
    // Plausibilitaet. 0 Kanaele waere eine Division durch Null, und
    // eine Rate von 4 Milliarden ist keine Aufnahme.
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

/// Die Samples in unser Format bringen: **16 Bit, Stereo, verschraenkt**.
///
/// ===================================================================
/// WAS HIER UMGERECHNET WIRD — UND WAS NICHT
///
///   * 8 Bit -> 16 Bit: 8-Bit-WAV ist UNSIGNED (0..255, Mitte 128),
///     16-Bit ist SIGNED. Wer das verwechselt, bekommt ein lautes
///     Rauschen mit einem Gleichanteil.
///   * Mono -> Stereo: jedes Sample verdoppelt.
///   * Mehr als zwei Kanaele: nur die ERSTEN ZWEI werden genommen.
///
/// **NICHT umgerechnet wird die ABTASTRATE.** Ein Resampler ohne
/// Fliesskomma ist ein eigenes Vorhaben (docs/audio.md §4); eine Datei
/// mit 44 100 Hz spielt auf unserem 48-kHz-Ausgang also um rund 9 %
/// zu schnell. Das ist eine bekannte Grenze und steht in grenzen.md —
/// sie wird GEMELDET, nicht stillschweigend hingenommen.
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
                1 => {
                    // 8-Bit-WAV ist UNSIGNED mit Mitte 128.
                    ((roh[p] as i16) - 128) << 8
                }
                _ => i16::from_le_bytes([roh[p], roh[p + 1]]),
            }
        };
        let links = hole(0);
        // Mono wird verdoppelt, Mehrkanal auf die ersten zwei gekuerzt.
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

// ===========================================================================
// TESTS
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein gueltiges 16-Bit-Stereo-WAV bauen.
    fn wav_bauen(kanaele: u16, rate: u32, bits: u16, samples: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&(36u32 + samples.len() as u32).to_le_bytes());
        d.extend_from_slice(b"WAVE");
        d.extend_from_slice(b"fmt ");
        d.extend_from_slice(&16u32.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes()); // PCM
        d.extend_from_slice(&kanaele.to_le_bytes());
        d.extend_from_slice(&rate.to_le_bytes());
        let block = kanaele * bits / 8;
        d.extend_from_slice(&(rate * block as u32).to_le_bytes());
        d.extend_from_slice(&block.to_le_bytes());
        d.extend_from_slice(&bits.to_le_bytes());
        d.extend_from_slice(b"data");
        d.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        d.extend_from_slice(samples);
        d
    }

    #[test_case]
    fn test_gueltiges_wav() {
        let d = wav_bauen(2, 48000, 16, &[0, 0, 0, 0, 1, 0, 1, 0]);
        let info = kopf_lesen(&d).expect("gueltig");
        assert_eq!(info.kanaele, 2);
        assert_eq!(info.rate, 48000);
        assert_eq!(info.bits, 16);
        assert_eq!(info.daten_bytes, 8);
        assert_eq!(info.frames(), 2);
        assert!(!info.gekuerzt);
    }

    #[test_case]
    fn test_kein_riff() {
        let mut d = wav_bauen(2, 48000, 16, &[0; 4]);
        d[0] = b'X';
        assert_eq!(kopf_lesen(&d), Err(WavFehler::KeinWav));
        let mut d = wav_bauen(2, 48000, 16, &[0; 4]);
        d[8] = b'X'; // kein "WAVE"
        assert_eq!(kopf_lesen(&d), Err(WavFehler::KeinWav));
    }

    #[test_case]
    fn test_abgeschnitten_an_jeder_stelle() {
        let d = wav_bauen(2, 48000, 16, &[0; 16]);
        for laenge in 0..d.len() {
            // Darf NIE panicken — Ergebnis egal.
            let _ = kopf_lesen(&d[..laenge]);
        }
    }

    /// **DIE LAENGE LUEGT NACH OBEN** (abgeschnittener Download): Es
    /// wird GEKUERZT, nicht abgelehnt — ein halbes Lied ist besser als
    /// eine Fehlermeldung.
    #[test_case]
    fn test_datenlaenge_luegt_wird_gekuerzt() {
        let mut d = wav_bauen(2, 48000, 16, &[0; 8]);
        let pos = d.len() - 8 - 4;
        d[pos..pos + 4].copy_from_slice(&999_999u32.to_le_bytes());
        let info = kopf_lesen(&d).expect("darf nicht ablehnen");
        assert!(info.gekuerzt, "die Luege muss vermerkt sein");
        assert_eq!(info.daten_bytes, 8, "auf das Vorhandene geklemmt");
    }

    /// Unbekannte Chunks zwischen `fmt ` und `data` werden
    /// UEBERSPRUNGEN. Wer „fmt zuerst, data zweitens" annimmt,
    /// scheitert an jeder Datei mit Metadaten.
    #[test_case]
    fn test_unbekannte_chunks_werden_uebersprungen() {
        let mut d = Vec::new();
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&100u32.to_le_bytes());
        d.extend_from_slice(b"WAVE");
        d.extend_from_slice(b"fmt ");
        d.extend_from_slice(&16u32.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&48000u32.to_le_bytes());
        d.extend_from_slice(&192000u32.to_le_bytes());
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&16u16.to_le_bytes());
        // Ein LIST-Chunk mit UNGERADER Groesse -> Fuellbyte!
        d.extend_from_slice(b"LIST");
        d.extend_from_slice(&3u32.to_le_bytes());
        d.extend_from_slice(&[1, 2, 3, 0]); // 3 Byte + 1 Fuellbyte
        d.extend_from_slice(b"data");
        d.extend_from_slice(&4u32.to_le_bytes());
        d.extend_from_slice(&[1, 0, 2, 0]);
        let info = kopf_lesen(&d).expect("gueltig");
        assert_eq!(info.daten_bytes, 4, "der ungerade Chunk hat ein Fuellbyte");
    }

    #[test_case]
    fn test_chunk_groesse_null_haengt_nicht() {
        let mut d = Vec::new();
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&100u32.to_le_bytes());
        d.extend_from_slice(b"WAVE");
        // Ein Chunk mit Groesse 0 — ohne die 8 Byte Kopfvorschub waere
        // das eine Endlosschleife.
        for _ in 0..5 {
            d.extend_from_slice(b"junk");
            d.extend_from_slice(&0u32.to_le_bytes());
        }
        // Der Test muss ZURUECKKOMMEN; das Ergebnis ist egal.
        let _ = kopf_lesen(&d);
    }

    #[test_case]
    fn test_absurde_chunkgroesse_laeuft_nicht_ueber() {
        let mut d = Vec::new();
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&100u32.to_le_bytes());
        d.extend_from_slice(b"WAVE");
        d.extend_from_slice(b"junk");
        d.extend_from_slice(&u32::MAX.to_le_bytes());
        d.extend_from_slice(&[0; 16]);
        let _ = kopf_lesen(&d); // kein Ueberlauf, keine Panik
    }

    #[test_case]
    fn test_nicht_pcm_wird_abgelehnt() {
        let mut d = wav_bauen(2, 48000, 16, &[0; 4]);
        // Format-Feld auf 85 (MP3) setzen.
        d[20] = 85;
        assert_eq!(kopf_lesen(&d), Err(WavFehler::NichtPcm));
    }

    #[test_case]
    fn test_unsinnige_werte() {
        assert_eq!(
            kopf_lesen(&wav_bauen(0, 48000, 16, &[0; 4])),
            Err(WavFehler::UnsinnigeWerte),
            "0 Kanaele waere eine Division durch Null"
        );
        assert_eq!(
            kopf_lesen(&wav_bauen(99, 48000, 16, &[0; 4])),
            Err(WavFehler::UnsinnigeWerte)
        );
        assert_eq!(
            kopf_lesen(&wav_bauen(2, 1, 16, &[0; 4])),
            Err(WavFehler::UnsinnigeWerte)
        );
    }

    #[test_case]
    fn test_bittiefe() {
        assert!(kopf_lesen(&wav_bauen(2, 48000, 8, &[0; 4])).is_ok());
        assert!(kopf_lesen(&wav_bauen(2, 48000, 16, &[0; 4])).is_ok());
        assert_eq!(
            kopf_lesen(&wav_bauen(2, 48000, 24, &[0; 6])),
            Err(WavFehler::BittiefeNichtUnterstuetzt)
        );
    }

    #[test_case]
    fn test_muell_panickt_nicht() {
        let mut wert: u32 = 0xDEAD_BEEF;
        for laenge in [0usize, 1, 11, 12, 20, 45, 200] {
            let mut muell = Vec::with_capacity(laenge);
            for _ in 0..laenge {
                // Reproduzierbarer LCG — TESTHILFE, kein Zufall.
                wert = wert.wrapping_mul(1103515245).wrapping_add(12345);
                muell.push((wert >> 16) as u8);
            }
            let _ = kopf_lesen(&muell);
        }
        // Und ein Muell-Rumpf hinter einem GUELTIGEN Kopf.
        let mut d = wav_bauen(2, 48000, 16, &[0; 8]);
        for b in d.iter_mut().skip(44) {
            *b = 0xFF;
        }
        if let Ok(info) = kopf_lesen(&d) {
            let _ = samples_lesen(&d, &info);
        }
    }

    // -------------------------------------------------------------------
    // UMRECHNUNG
    // -------------------------------------------------------------------

    #[test_case]
    fn test_stereo_16bit_unveraendert() {
        let samples: [u8; 8] = [0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x40];
        let d = wav_bauen(2, 48000, 16, &samples);
        let info = kopf_lesen(&d).unwrap();
        let s = samples_lesen(&d, &info);
        assert_eq!(s, alloc::vec![0x1000, 0x2000, 0x3000, 0x4000]);
    }

    /// Mono wird auf beide Kanaele verdoppelt.
    #[test_case]
    fn test_mono_wird_stereo() {
        let samples: [u8; 4] = [0x00, 0x10, 0x00, 0x20];
        let d = wav_bauen(1, 48000, 16, &samples);
        let info = kopf_lesen(&d).unwrap();
        let s = samples_lesen(&d, &info);
        assert_eq!(s, alloc::vec![0x1000, 0x1000, 0x2000, 0x2000]);
    }

    /// **8-BIT-WAV IST UNSIGNED.** 128 ist die Mitte, nicht 0 — wer das
    /// verwechselt, bekommt Rauschen mit Gleichanteil.
    #[test_case]
    fn test_acht_bit_ist_unsigned() {
        let samples: [u8; 4] = [128, 128, 255, 0];
        let d = wav_bauen(2, 48000, 8, &samples);
        let info = kopf_lesen(&d).unwrap();
        let s = samples_lesen(&d, &info);
        assert_eq!(s[0], 0, "128 ist die MITTE, also Stille");
        assert_eq!(s[1], 0);
        assert_eq!(s[2], 127 << 8, "255 ist der Maximalausschlag");
        assert_eq!(s[3], -128 << 8, "0 ist der Minimalausschlag");
    }

    #[test_case]
    fn test_mehrkanal_wird_auf_stereo_gekuerzt() {
        // 4 Kanaele, ein Frame.
        let samples: [u8; 8] = [0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x40];
        let d = wav_bauen(4, 48000, 16, &samples);
        let info = kopf_lesen(&d).unwrap();
        let s = samples_lesen(&d, &info);
        assert_eq!(s, alloc::vec![0x1000, 0x2000], "nur die ersten zwei");
    }

    #[test_case]
    fn test_dauer_und_frames() {
        // 48000 Frames Stereo 16 Bit = 1 Sekunde.
        let d = wav_bauen(2, 48000, 16, &alloc::vec![0u8; 48000 * 4]);
        let info = kopf_lesen(&d).unwrap();
        assert_eq!(info.frames(), 48000);
        assert_eq!(info.dauer_ms(), 1000);
    }
}
