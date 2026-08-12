// audio — die Naht zwischen Tonerzeugern und Tonausgabe
//
// ===========================================================================
// DIESELBE DISZIPLIN WIE BEI BlockDevice, NetzGeraet UND usb::geraet
//
//   hda --[implementiert]--> AudioGeraet <--[benutzt]-- Mixer
//                                                          ^
//                                        Syscall / `ton` / `spielen`
//
// Der Treiber kennt keine Tonquellen, die Tonquellen keinen Treiber.
// Ein zweiter Treiber (AC97, USB-Audio) haengt sich an dasselbe Trait,
// ohne dass der Mixer angefasst wird — genau wie virtio-net damals den
// IP-Stack nicht angefasst hat.

pub mod dienst;
pub mod hda;
pub mod mixer;
pub mod wav;

use alloc::string::String;

/// Die Abtastrate, mit der SpeedOS arbeitet.
///
/// **EINE Rate, und zwar 48 kHz.** Jede Quelle wird darauf gebracht,
/// bevor sie den Mixer erreicht. Der Grund ist nicht Bequemlichkeit:
/// Mehrere Raten gleichzeitig hiessen Resampling IM Mixer, und ein
/// Resampler ohne Fliesskomma (soft-float!) ist ein eigenes Vorhaben.
/// 48 kHz, weil HDA das nativ kann und jede Hardware es beherrscht.
pub const ABTASTRATE: u32 = 48_000;

/// Kanaele. Stereo, fest.
pub const KANAELE: usize = 2;

/// Ein Sample ist 16 Bit mit Vorzeichen — das Format, in dem WAV
/// ueblicherweise vorliegt und das HDA direkt frisst.
pub type Sample = i16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFehler {
    KeinGeraet,
    NichtBereit,
    Zeitueberschreitung,
    KeinAusgabepfad,
    KeinSpeicher,
    PufferVoll,
}

impl AudioFehler {
    pub fn text(self) -> &'static str {
        match self {
            AudioFehler::KeinGeraet => "kein Audio-Geraet",
            AudioFehler::NichtBereit => "Audio-Geraet nicht bereit",
            AudioFehler::Zeitueberschreitung => "Frist abgelaufen",
            AudioFehler::KeinAusgabepfad => "kein Ausgabepfad gefunden",
            AudioFehler::KeinSpeicher => "kein Speicher fuer den Ringpuffer",
            AudioFehler::PufferVoll => "Puffer voll",
        }
    }
}

/// Was ein Tonausgabe-Geraet koennen muss.
///
/// Bewusst SCHMAL — dieselbe Ueberlegung wie bei `BlockDevice`: Je
/// weniger ein Trait verlangt, desto leichter faellt der zweite
/// Implementierer.
pub trait AudioGeraet: Send {
    /// Ein Name fuer die Anzeige.
    fn name(&self) -> String;
    /// Wie viele Frames der Ringpuffer fasst (ein Frame = alle Kanaele).
    fn puffer_frames(&self) -> usize;
    /// Wie viele Frames der Controller schon gespielt hat —
    /// **monoton wachsend, ueber Ringpuffer-Umlaeufe hinweg**.
    ///
    /// Die einzige Uhr, die zaehlt. Wer nachfuellt, ohne sie zu lesen,
    /// ueberschreibt entweder Ungespieltes oder laesst Luecken.
    ///
    /// `&mut self`, WEIL DIE HARDWARE NUR EINE POSITION IM PUFFER
    /// LIEFERT und die umlaeuft. Der Umlauf muss mitgezaehlt werden,
    /// und das ist Zustand. Ein `&self` mit innerer Veraenderlichkeit
    /// waere dieselbe Sache mit einem Deckel darauf.
    fn gespielte_frames(&mut self) -> u64;
    /// Wie viele Frames noch in den Ringpuffer passen.
    ///
    /// **ZUERST FRAGEN, DANN MISCHEN.** Der Mixer VERBRAUCHT beim
    /// Mischen die Samples seiner Quellen; wer erst mischt und dann
    /// merkt, dass die Hardware nichts nimmt, hat sie verloren. Diese
    /// Methode ist der Grund, warum das nicht passieren kann.
    fn freie_frames(&mut self) -> usize;
    /// Samples in den Ringpuffer schreiben. Liefert die Zahl der
    /// uebernommenen FRAMES (kann kleiner sein — dann ist er voll).
    fn schreiben(&mut self, frames: &[Sample]) -> usize;
    /// Wiedergabe starten.
    fn starten(&mut self) -> Result<(), AudioFehler>;
    /// Wiedergabe anhalten.
    fn stoppen(&mut self);
    fn laeuft(&self) -> bool;
}

/// Laeuft ein Audio-Geraet?
pub fn vorhanden() -> bool {
    hda::vorhanden()
}

/// Beim Boot aufrufen.
pub fn init() {
    hda::init();
}
