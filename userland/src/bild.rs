// libspeed::bild — BILDER DEKODIEREN, IN RING 3 (Serie 8, Teil 3)
//
// ===========================================================================
// WARUM DAS HIER STEHT UND NICHT IM KERNEL
//
// Ein Bilddekoder ist ein PARSER FÜR FREMDE DATEN — dieselbe Sorte Code wie
// `pem.rs` und aus demselben Grund am selben Ort. Ein PNG kommt von einer
// Webseite, einem USB-Stick, einem Fremden. Es ist eine Folge von
// BEHAUPTUNGEN über Grössen, Offsets und Kompression, und jede einzelne kann
// gelogen sein.
//
// Läge der Dekoder im Kernel, wäre ein Fehler in ihm ein Fehler IM KERNEL.
// Hier trifft er einen Prozess, der stirbt und ersetzt wird — Dauerregel II.
// Es ist dieselbe Entscheidung wie bei rustls (Serie 7): 30 000 Zeilen
// Fremdcode gehören nicht in Ring 0.
//
// ===========================================================================
// DIE DREI SCHRITTE, UND WARUM ES DREI SIND
//
// Der naive Weg ist `dekodiere(bytes) -> Bild` in einem Rutsch. Der ist
// ANGREIFBAR, und der Testfall dazu liegt im Repository
// (`assets/testbilder/bombe.png`): 48 KiB Datei, die 4096x4096 deklariert
// und zu 50 MiB dekodiert. Sie ist FORMAL EINWANDFREI — sie scheitert an
// keiner Plausibilitätsprüfung, weil an ihr nichts unplausibel ist. Sie
// muss an einer GRENZE scheitern, und dafür muss man die Masse kennen,
// BEVOR man Speicher anfasst:
//
//   (1) `decode_headers()`  — nur der IHDR/SOF-Kopf, keine Bilddaten.
//   (2) GRENZEN PRÜFEN      — Kantenlänge, Pixelzahl, Puffergrösse. Hier
//                             stirbt die Bombe, mit einem Fehler und ohne
//                             ein einziges alloziertes Byte.
//   (3) `decode_into()`     — in einen Puffer, den WIR angelegt haben und
//                             dessen Grösse WIR bestimmt haben.
//
// Schritt 3 ist der zweite Gewinn: `decode_raw()` würde selbst allozieren
// (und zwar so viel, wie die Datei will). `decode_into` schreibt in unseren
// Puffer — der Dekoder bestimmt nicht mehr, wie viel Speicher er bekommt.
//
// ===========================================================================
// DIE SPEICHER-RECHNUNG (die eigentliche Grenze, und sie ist keine
// Format-Grenze)
//
// Ein Prozess hat 64 MiB Heap (`heap::HEAP_MAX_BYTES`; bis Serie 8,
// Teil 7 waren es 12 MiB, und die Rechnung unten stammt von damals — die
// Grenzen wurden BEWUSST nicht mit angehoben: Ein Bilddekoder soll so
// wenig annehmen wie moeglich, und `Grenzen` ist ohnehin ein Argument).
// Zur Spitze liegen gleichzeitig im Speicher:
//
//     Dateibytes  +  dekodiertes RGBA (breite * hoehe * 4)  +  Fensterpuffer
//
// Mit den Standard-Grenzen (4 MiB Datei, 1 Mi Pixel) sind das 4 + 4 = 8 MiB,
// und es bleiben 4 MiB für ein Fenster — bei 720p reicht das (3,5 MiB).
// 1 Mi Pixel sind 1024x1024 oder 1280x819; ein 1920x1080-Foto (2,07 Mi
// Pixel) wird ABGELEHNT, und das ist keine Formatgrenze, sondern die
// Prozess-Heap-Grenze. Sie steht in docs/grenzen.md, weil sie eine echte
// Einschränkung ist und keine Wahl.
//
// Deshalb ist `Grenzen` ein ARGUMENT und keine Konstante: Wächst das
// Prozess-Layout (ABI-Änderung, siehe grenzen.md), hebt ein Aufrufer sie an,
// ohne dass hier eine Zeile geändert werden muss.
//
// ===========================================================================
// DIE AUSGABE IST IMMER RGBA — auch wenn die Datei etwas anderes sagt
//
// Graustufen, Palette, RGB, RGBA, YCbCr: Was hier herauskommt, ist IMMER
// `breite * hoehe * 4` Byte in der Reihenfolge R, G, B, A. Ein Aufrufer
// (der Bildbetrachter heute, der HTML-Renderer morgen) soll KEINE Farbräume
// kennen müssen — sonst wandert die Fallunterscheidung in jeden Aufrufer,
// und einer vergisst sie.
//
// Warum `Vec<u8>` und nicht `Vec<u32>`: Ein `Vec<u8>` in RGBA-Reihenfolge
// ist die Form, in der jeder Dekoder liefert und jedes Format es meint —
// ein `Vec<u32>` bräuchte eine Umdeutung des Puffers und damit `unsafe`.
// `libspeed::pem`, `netz` und `tls` haben zusammen NULL unsafe-Blöcke
// (docs/unsafe-audit-serie7.md); dieser hier hat auch keinen. Die
// Umrechnung ins Fenster-Format passiert an EINER Stelle, in
// `nach_fenster`, und die ist gewöhnliches Rust.

use crate::heap;
use alloc::vec;
use alloc::vec::Vec;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

// ---------------------------------------------------------------------------
// GRENZEN
// ---------------------------------------------------------------------------

/// Was ein Dekodier-Auftrag höchstens kosten darf.
///
/// EIN STRUCT UND KEINE KONSTANTEN, weil die Zahlen nicht aus dem Format
/// kommen, sondern aus dem Prozess-Layout — und das ist nicht überall
/// gleich. Ein Betrachter mit einem kleinen Fenster darf grosszügiger sein
/// als ein Renderer, der zwanzig Bilder auf einer Seite hält.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grenzen {
    /// Grösste erlaubte Breite ODER Höhe in Pixeln.
    ///
    /// Fängt den Fall ab, in dem EINE Kante absurd ist (100000 x 1). Die
    /// Pixelzahl allein täte das nicht: 100000 x 1 sind nur 100 000 Pixel,
    /// aber die Zeilenpuffer eines Dekoders hängen an der BREITE.
    pub max_kante: u32,
    /// Grösste erlaubte Pixelzahl (breite * hoehe).
    ///
    /// DAS IST DIE GRENZE, AN DER DIE DEKOMPRESSIONSBOMBE STIRBT. Sie wird
    /// aus dem Kopf gerechnet, nicht aus der Dateigrösse — genau das ist
    /// der Punkt einer Bombe: Die Datei ist klein.
    pub max_pixel: u64,
    /// Grösste Eingabedatei in Bytes.
    pub max_datei_bytes: usize,
}

impl Grenzen {
    /// Die Vorgabe: 8192 Pixel Kante, 1 Mi Pixel, 4 MiB Datei.
    ///
    /// Die Zahlen kommen aus der Speicher-Rechnung im Kopfkommentar, nicht
    /// aus dem Bauch: 4 MiB Datei + 4 MiB RGBA lassen von 12 MiB Heap noch
    /// 4 MiB für ein Fenster übrig.
    pub const fn standard() -> Grenzen {
        Grenzen {
            max_kante: 8192,
            max_pixel: 1024 * 1024,
            max_datei_bytes: 4 * 1024 * 1024,
        }
    }

    /// Dieselben Grenzen mit einer anderen Pixelzahl.
    pub const fn mit_max_pixel(mut self, pixel: u64) -> Grenzen {
        self.max_pixel = pixel;
        self
    }

    /// Was ein Bild dieser Masse an RGBA-Bytes kostet — mit `checked`,
    /// weil die Masse aus einer fremden Datei stammen.
    fn rgba_bytes(breite: u32, hoehe: u32) -> Option<usize> {
        let pixel = (breite as u64).checked_mul(hoehe as u64)?;
        let bytes = pixel.checked_mul(4)?;
        usize::try_from(bytes).ok()
    }
}

impl Default for Grenzen {
    fn default() -> Self {
        Grenzen::standard()
    }
}

// ---------------------------------------------------------------------------
// FEHLER
// ---------------------------------------------------------------------------

/// Warum ein Bild nicht dekodiert werden konnte.
///
/// Getrennt nach dem, was ein Aufrufer unterschiedlich BEHANDELN würde,
/// nicht nach dem, was der Dekoder intern unterscheidet — dasselbe
/// Prinzip wie bei `AbrufFehler` (Serie 7, Teil 5). `ZuGross` ist deshalb
/// eine eigene Variante und kein `Kaputt`: Ein zu grosses Bild ist
/// EINWANDFREI, es passt nur nicht. Das ist eine andere Aussage, und ein
/// Betrachter darf sie anders anzeigen ("zu gross" statt "kaputt").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BildFehler {
    /// Null Bytes.
    Leer,
    /// Die Datei ist grösser als `Grenzen::max_datei_bytes`.
    DateiZuGross { bytes: usize, grenze: usize },
    /// Kein erkennbares Bildformat (Signatur passt zu nichts).
    UnbekanntesFormat,
    /// Format erkannt, aber nicht eingebaut (GIF, BMP, WebP, ...).
    NichtUnterstuetzt(Format),
    /// Der Kopf ist kaputt oder abgeschnitten.
    KaputterKopf,
    /// Die Bilddaten sind kaputt oder abgeschnitten.
    KaputteDaten,
    /// Breite oder Höhe ist 0.
    NullGross,
    /// Über einer Grenze. `pixel` ist die geforderte Pixelzahl.
    ZuGross {
        breite: u32,
        hoehe: u32,
        pixel: u64,
        grenze: u64,
    },
    /// Der Heap gab den Puffer nicht her.
    KeinSpeicher { bytes: usize },
    /// Der Dekoder lieferte einen Farbraum, den wir nicht nach RGBA
    /// bringen können. Sollte nicht vorkommen — steht hier, damit es
    /// ein FEHLER ist und keine Panik.
    Farbraum,
}

impl BildFehler {
    /// Ein deutscher Satz für den Benutzer.
    pub fn text(self) -> &'static str {
        match self {
            BildFehler::Leer => "Die Datei ist leer.",
            BildFehler::DateiZuGross { .. } => "Die Datei ist zu gross.",
            BildFehler::UnbekanntesFormat => "Das ist kein bekanntes Bildformat.",
            BildFehler::NichtUnterstuetzt(_) => "Dieses Bildformat kann SpeedOS (noch) nicht.",
            BildFehler::KaputterKopf => "Der Bildkopf ist kaputt oder abgeschnitten.",
            BildFehler::KaputteDaten => "Die Bilddaten sind kaputt oder abgeschnitten.",
            BildFehler::NullGross => "Das Bild ist 0 Pixel gross.",
            BildFehler::ZuGross { .. } => "Das Bild ist zu gross fuer den Speicher dieses Prozesses.",
            BildFehler::KeinSpeicher { .. } => "Kein Speicher fuer das Bild.",
            BildFehler::Farbraum => "Unbekannter Farbraum im Bild.",
        }
    }

    /// Das maschinenlesbare Schlagwort — daran hängen die Tests.
    ///
    /// Dasselbe Muster wie `TlsFehler::kurz()` und `AbrufFehler::kurz()`:
    /// Ein Test, der auf einen deutschen Satz prüft, bricht beim nächsten
    /// Tippfehler.
    pub fn kurz(self) -> &'static str {
        match self {
            BildFehler::Leer => "leer",
            BildFehler::DateiZuGross { .. } => "datei-zu-gross",
            BildFehler::UnbekanntesFormat => "unbekanntes-format",
            BildFehler::NichtUnterstuetzt(_) => "nicht-unterstuetzt",
            BildFehler::KaputterKopf => "kaputter-kopf",
            BildFehler::KaputteDaten => "kaputte-daten",
            BildFehler::NullGross => "null-gross",
            BildFehler::ZuGross { .. } => "zu-gross",
            BildFehler::KeinSpeicher { .. } => "kein-speicher",
            BildFehler::Farbraum => "farbraum",
        }
    }
}

// ---------------------------------------------------------------------------
// FORMAT-ERKENNUNG
// ---------------------------------------------------------------------------

/// Die Bildformate, die wir an der Signatur unterscheiden.
///
/// AN DEN ERSTEN BYTES, NIE AN DER ENDUNG — dasselbe Argument wie bei
/// `prozess::ist_programm` im Kernel: Unser VFS kennt keine Endungen, und
/// eine Datei, die `.png` heisst, ist deswegen keins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
    WebP,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::WebP => "WebP",
        }
    }

    /// Können wir es dekodieren?
    pub fn unterstuetzt(self) -> bool {
        matches!(self, Format::Png | Format::Jpeg)
    }
}

/// Das Format an der Signatur erkennen.
///
/// GIF/BMP/WebP werden ERKANNT, obwohl sie nicht dekodiert werden. Der
/// Grund ist die Fehlermeldung: „GIF kann SpeedOS nicht" ist eine Auskunft,
/// „unbekanntes Format" bei einer offensichtlichen GIF-Datei ist eine
/// Ratlosigkeit.
pub fn format_erkennen(daten: &[u8]) -> Option<Format> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if daten.starts_with(PNG) {
        return Some(Format::Png);
    }
    // JPEG: SOI-Marker FF D8, danach ein weiterer Marker FF xx.
    if daten.len() >= 3 && daten[0] == 0xFF && daten[1] == 0xD8 && daten[2] == 0xFF {
        return Some(Format::Jpeg);
    }
    if daten.starts_with(b"GIF87a") || daten.starts_with(b"GIF89a") {
        return Some(Format::Gif);
    }
    if daten.starts_with(b"BM") {
        return Some(Format::Bmp);
    }
    // RIFF....WEBP
    if daten.len() >= 12 && daten.starts_with(b"RIFF") && &daten[8..12] == b"WEBP" {
        return Some(Format::WebP);
    }
    None
}

// ---------------------------------------------------------------------------
// DAS BILD
// ---------------------------------------------------------------------------

/// Ein dekodiertes Bild: immer RGBA, immer `breite * hoehe * 4` Byte.
#[derive(Debug, Clone)]
pub struct Bild {
    breite: u32,
    hoehe: u32,
    /// R, G, B, A je Pixel, zeilenweise von oben nach unten.
    rgba: Vec<u8>,
}

impl Bild {
    /// Ein Bild aus fertigen RGBA-Bytes. Prüft die Länge — ein Bild, dessen
    /// Puffer nicht zu seinen Massen passt, darf es nicht geben.
    pub fn aus_rgba(breite: u32, hoehe: u32, rgba: Vec<u8>) -> Option<Bild> {
        let erwartet = Grenzen::rgba_bytes(breite, hoehe)?;
        if rgba.len() != erwartet || breite == 0 || hoehe == 0 {
            return None;
        }
        Some(Bild { breite, hoehe, rgba })
    }

    #[inline]
    pub fn breite(&self) -> u32 {
        self.breite
    }
    #[inline]
    pub fn hoehe(&self) -> u32 {
        self.hoehe
    }
    /// Die rohen RGBA-Bytes.
    #[inline]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
    /// Wie viele Bytes das Bild auf dem Heap belegt.
    #[inline]
    pub fn bytes(&self) -> usize {
        self.rgba.len()
    }

    /// Ein Pixel als `0xAARRGGBB`.
    ///
    /// Ausserhalb des Bildes: vollständig durchsichtig (0). KEIN Panik,
    /// KEIN Wrap-around — ein Renderer rechnet Koordinaten aus, und die
    /// dürfen danebenliegen.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        if x >= self.breite || y >= self.hoehe {
            return 0;
        }
        let i = ((y as usize) * (self.breite as usize) + x as usize) * 4;
        let r = self.rgba[i] as u32;
        let g = self.rgba[i + 1] as u32;
        let b = self.rgba[i + 2] as u32;
        let a = self.rgba[i + 3] as u32;
        (a << 24) | (r << 16) | (g << 8) | b
    }

    /// Das Bild in das Pixelformat des Fensters bringen (`0x00RRGGBB`),
    /// mit `hintergrund` hinter dem Alpha-Kanal.
    ///
    /// DIE EINE STELLE, an der aus RGBA-Bytes Fenster-Pixel werden. Das
    /// Fenster-ABI kennt kein Alpha (docs/syscalls.md §6b) — irgendwo MUSS
    /// also verrechnet werden, und lieber hier einmal als in jedem
    /// Aufrufer einmal falsch.
    pub fn nach_fenster(&self, hintergrund: u32) -> Vec<u32> {
        let (hr, hg, hb) = zerlegen(hintergrund);

        let mut ziel = Vec::with_capacity(self.rgba.len() / 4);
        for p in self.rgba.as_chunks::<4>().0 {
            ziel.push(mischen(p[0], p[1], p[2], p[3], hr, hg, hb));
        }
        ziel
    }

    /// Ein Pixel über den Hintergrund gemischt, als Fenster-Pixel.
    ///
    /// Für Aufrufer, die Pixel EINZELN holen (Zoom, Skalierung) und keinen
    /// zweiten Puffer wollen — der Bildbetrachter tut genau das.
    #[inline]
    pub fn pixel_auf(&self, x: u32, y: u32, hintergrund: u32) -> u32 {
        if x >= self.breite || y >= self.hoehe {
            return hintergrund;
        }
        let i = ((y as usize) * (self.breite as usize) + x as usize) * 4;
        let (hr, hg, hb) = zerlegen(hintergrund);
        mischen(
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
            hr,
            hg,
            hb,
        )
    }
}

/// Ein Fenster-Pixel (`0x00RRGGBB`) in seine drei Kanaele zerlegen.
#[inline]
fn zerlegen(farbe: u32) -> (u32, u32, u32) {
    ((farbe >> 16) & 0xFF, (farbe >> 8) & 0xFF, farbe & 0xFF)
}

/// Alpha-Mischung, ganzzahlig.
///
/// GANZZAHLIG, WEIL ES SEIN MUSS: Unser Target hat `-sse,+soft-float`
/// (userland/.cargo/config.toml) — Fliesskomma gibt es hier nicht, und
/// es bräuchte auch niemand. `a * v + (255 - a) * h` durch 255 ist genau
/// die Formel, die `grafik::Zeichner::pixel` im Kernel benutzt.
#[inline]
fn mischen(r: u8, g: u8, b: u8, a: u8, hr: u32, hg: u32, hb: u32) -> u32 {
    let a = a as u32;
    if a == 255 {
        return ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
    }
    if a == 0 {
        return (hr << 16) | (hg << 8) | hb;
    }
    let gegen = 255 - a;
    let mr = (a * r as u32 + gegen * hr) / 255;
    let mg = (a * g as u32 + gegen * hg) / 255;
    let mb = (a * b as u32 + gegen * hb) / 255;
    (mr << 16) | (mg << 8) | mb
}

// ---------------------------------------------------------------------------
// DEKODIEREN
// ---------------------------------------------------------------------------

/// Ein Bild dekodieren, mit den Standard-Grenzen.
pub fn dekodieren(daten: &[u8]) -> Result<Bild, BildFehler> {
    dekodieren_mit(daten, Grenzen::standard())
}

/// Ein Bild dekodieren, mit ausdrücklichen Grenzen.
///
/// PANICKT NIE — jede Kaputtheit ist ein `BildFehler`. Das ist keine
/// Höflichkeit: Ein Panic in Ring 3 beendet den Prozess (exit 101), und
/// ein Bildbetrachter, der bei einer kaputten Datei verschwindet statt
/// „kaputt" anzuzeigen, ist unbrauchbar. Beim HTML-Renderer wäre es
/// schlimmer — ein Bild auf einer Seite darf nicht die Seite abschiessen.
pub fn dekodieren_mit(daten: &[u8], grenzen: Grenzen) -> Result<Bild, BildFehler> {
    if daten.is_empty() {
        return Err(BildFehler::Leer);
    }
    if daten.len() > grenzen.max_datei_bytes {
        return Err(BildFehler::DateiZuGross {
            bytes: daten.len(),
            grenze: grenzen.max_datei_bytes,
        });
    }

    match format_erkennen(daten) {
        Some(Format::Png) => png_dekodieren(daten, grenzen),
        Some(Format::Jpeg) => jpeg_dekodieren(daten, grenzen),
        Some(anderes) => Err(BildFehler::NichtUnterstuetzt(anderes)),
        None => Err(BildFehler::UnbekanntesFormat),
    }
}

/// Die Grenzen an den Dekoder weiterreichen.
///
/// `max_width`/`max_height` sind der ERSTE Riegel — er greift schon im
/// Kopf-Parser der Kiste, also noch vor unserer eigenen Prüfung. Beide zu
/// haben ist kein Übereifer: Der Riegel der Kiste kennt unsere Pixelzahl
/// nicht (`assets/testbilder/bombe.png` läuft mit 4096x4096 glatt durch
/// ihn hindurch), und unsere Prüfung würde ohne ihn erst laufen, nachdem
/// der Kopf-Parser mit absurden Zahlen gearbeitet hat.
fn optionen(grenzen: Grenzen) -> DecoderOptions {
    DecoderOptions::default()
        .set_max_width(grenzen.max_kante as usize)
        .set_max_height(grenzen.max_kante as usize)
}

/// Schritt (2): Masse prüfen, bevor irgendetwas alloziert wird.
fn masse_pruefen(breite: usize, hoehe: usize, grenzen: Grenzen) -> Result<(u32, u32), BildFehler> {
    if breite == 0 || hoehe == 0 {
        return Err(BildFehler::NullGross);
    }
    let breite = u32::try_from(breite).map_err(|_| BildFehler::ZuGross {
        breite: u32::MAX,
        hoehe: 0,
        pixel: u64::MAX,
        grenze: grenzen.max_pixel,
    })?;
    let hoehe = u32::try_from(hoehe).map_err(|_| BildFehler::ZuGross {
        breite,
        hoehe: u32::MAX,
        pixel: u64::MAX,
        grenze: grenzen.max_pixel,
    })?;

    let pixel = (breite as u64) * (hoehe as u64); // beide u32 -> passt in u64
    if breite > grenzen.max_kante || hoehe > grenzen.max_kante || pixel > grenzen.max_pixel {
        return Err(BildFehler::ZuGross {
            breite,
            hoehe,
            pixel,
            grenze: grenzen.max_pixel,
        });
    }
    Ok((breite, hoehe))
}

/// Schritt (3): den RGBA-Puffer anlegen — und zwar so, dass der
/// Dekoder DIREKT HINEIN schreibt.
///
/// DER TRICK, DER DIE SPITZE HALBIERT: Der Puffer ist von Anfang an
/// `breite * hoehe * 4` gross, der Dekoder schreibt aber nur seine
/// `roh_bytes` (bei RGB 3/Pixel, bei Grau 1/Pixel) in den VORDEREN Teil.
/// Danach wird VON HINTEN NACH VORN auf 4 Byte je Pixel auseinandergezogen
/// — rückwärts, weil das Ziel jedes Pixels weiter hinten liegt als seine
/// Quelle und sich beide deshalb nie ins Gehege kommen.
///
/// Der naheliegende Weg (Dekoder-Vec + Umbau in einen zweiten Vec) hätte
/// beide gleichzeitig im Speicher — bei 1 Mi Pixeln 4 MiB statt 8 MiB
/// Spitze. Bei 12 MiB Heap ist das der Unterschied zwischen „geht" und
/// „geht nicht".
fn puffer_anlegen(bytes: usize) -> Result<Vec<u8>, BildFehler> {
    // `try_reserve` gibt es für Vec, aber der Allocator dieses Prozesses
    // fordert Seiten beim Kernel an (SYS_SPEICHER) und meldet Not über
    // seinen eigenen Weg — deshalb wird VORHER gefragt, ob es passt.
    if !heap::passt_noch(bytes) {
        return Err(BildFehler::KeinSpeicher { bytes });
    }
    let mut v = Vec::new();
    if v.try_reserve_exact(bytes).is_err() {
        return Err(BildFehler::KeinSpeicher { bytes });
    }
    v.resize(bytes, 0);
    Ok(v)
}

/// Wie viele Bytes je Pixel ein Farbraum liefert — und ob wir ihn kennen.
fn kanaele(cs: ColorSpace) -> Option<usize> {
    match cs {
        ColorSpace::Luma => Some(1),
        ColorSpace::LumaA => Some(2),
        ColorSpace::RGB | ColorSpace::BGR => Some(3),
        ColorSpace::RGBA | ColorSpace::BGRA | ColorSpace::ARGB => Some(4),
        _ => None,
    }
}

/// Den vorderen Teil des Puffers (roh, `kanaele` Byte je Pixel) an Ort und
/// Stelle auf RGBA auseinanderziehen.
///
/// RÜCKWÄRTS. Der Kommentar steht hier, weil die Richtung die ganze
/// Korrektheit trägt: Pixel `i` liegt roh bei `i*k` und soll nach `i*4`.
/// Für `k < 4` ist `i*4 >= i*k`, das Ziel liegt also NIE vor der Quelle —
/// wer vorwärts liefe, überschriebe die Quelle des nächsten Pixels.
fn nach_rgba_ausziehen(puffer: &mut [u8], pixel: usize, cs: ColorSpace) {
    let k = match kanaele(cs) {
        Some(k) => k,
        None => return,
    };
    for i in (0..pixel).rev() {
        let q = i * k;
        let z = i * 4;
        let (r, g, b, a) = match (k, cs) {
            (1, _) => {
                let v = puffer[q];
                (v, v, v, 255)
            }
            (2, _) => {
                let v = puffer[q];
                (v, v, v, puffer[q + 1])
            }
            (3, ColorSpace::BGR) => (puffer[q + 2], puffer[q + 1], puffer[q], 255),
            (3, _) => (puffer[q], puffer[q + 1], puffer[q + 2], 255),
            (4, ColorSpace::BGRA) => (puffer[q + 2], puffer[q + 1], puffer[q], puffer[q + 3]),
            (4, ColorSpace::ARGB) => (puffer[q + 1], puffer[q + 2], puffer[q + 3], puffer[q]),
            (4, _) => (puffer[q], puffer[q + 1], puffer[q + 2], puffer[q + 3]),
            _ => (0, 0, 0, 255),
        };
        puffer[z] = r;
        puffer[z + 1] = g;
        puffer[z + 2] = b;
        puffer[z + 3] = a;
    }
}

fn png_dekodieren(daten: &[u8], grenzen: Grenzen) -> Result<Bild, BildFehler> {
    let mut d = zune_png::PngDecoder::new_with_options(ZCursor::new(daten), optionen(grenzen));

    // (1) NUR DER KOPF.
    d.decode_headers().map_err(|_| BildFehler::KaputterKopf)?;
    let (b, h) = d.dimensions().ok_or(BildFehler::KaputterKopf)?;

    // (2) GRENZEN. Hier stirbt die Bombe.
    let (breite, hoehe) = masse_pruefen(b, h, grenzen)?;

    let roh_bytes = d.output_buffer_size().ok_or(BildFehler::KaputterKopf)?;
    let cs = d.colorspace().ok_or(BildFehler::Farbraum)?;
    if kanaele(cs).is_none() {
        return Err(BildFehler::Farbraum);
    }
    let rgba_bytes = Grenzen::rgba_bytes(breite, hoehe).ok_or(BildFehler::NullGross)?;
    // Der Dekoder darf nicht MEHR wollen, als das RGBA-Ziel hergibt —
    // sonst schriebe er über das Ende des vorderen Teils hinaus. Bei
    // <= 4 Kanälen kann das nicht passieren; geprüft wird es trotzdem,
    // weil die Zahl aus einer fremden Datei stammt.
    if roh_bytes > rgba_bytes {
        return Err(BildFehler::Farbraum);
    }

    // (3) UNSER Puffer, direkt in der Endgrösse.
    let mut puffer = puffer_anlegen(rgba_bytes)?;
    d.decode_into(&mut puffer[..roh_bytes])
        .map_err(|_| BildFehler::KaputteDaten)?;

    nach_rgba_ausziehen(&mut puffer, (breite as usize) * (hoehe as usize), cs);
    Bild::aus_rgba(breite, hoehe, puffer).ok_or(BildFehler::KaputteDaten)
}

fn jpeg_dekodieren(daten: &[u8], grenzen: Grenzen) -> Result<Bild, BildFehler> {
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(daten), optionen(grenzen));

    d.decode_headers().map_err(|_| BildFehler::KaputterKopf)?;
    let info = d.info().ok_or(BildFehler::KaputterKopf)?;
    let (breite, hoehe) = masse_pruefen(info.width as usize, info.height as usize, grenzen)?;

    let roh_bytes = d.output_buffer_size().ok_or(BildFehler::KaputterKopf)?;
    let cs = d.output_colorspace().ok_or(BildFehler::Farbraum)?;
    if kanaele(cs).is_none() {
        return Err(BildFehler::Farbraum);
    }
    let rgba_bytes = Grenzen::rgba_bytes(breite, hoehe).ok_or(BildFehler::NullGross)?;
    if roh_bytes > rgba_bytes {
        return Err(BildFehler::Farbraum);
    }

    let mut puffer = puffer_anlegen(rgba_bytes)?;
    d.decode_into(&mut puffer[..roh_bytes])
        .map_err(|_| BildFehler::KaputteDaten)?;

    nach_rgba_ausziehen(&mut puffer, (breite as usize) * (hoehe as usize), cs);
    Bild::aus_rgba(breite, hoehe, puffer).ok_or(BildFehler::KaputteDaten)
}

// ---------------------------------------------------------------------------
// Ein Ersatzbild, wenn nichts geht
// ---------------------------------------------------------------------------

/// Ein Schachbrett-Platzhalter in Magenta/Grau.
///
/// MAGENTA MIT ABSICHT — dasselbe Argument wie bei den Icon-Paletten im
/// Kernel: Eine Farbe, die in keinem echten Bild vorkommt, ist ein
/// sichtbarer Fehler. Ein grauer Kasten sähe aus wie ein Bild.
pub fn platzhalter(breite: u32, hoehe: u32) -> Bild {
    let breite = breite.max(1);
    let hoehe = hoehe.max(1);
    let mut rgba = vec![0u8; (breite as usize) * (hoehe as usize) * 4];
    for y in 0..hoehe as usize {
        for x in 0..breite as usize {
            let hell = ((x / 8) + (y / 8)) % 2 == 0;
            let i = (y * breite as usize + x) * 4;
            let (r, g, b) = if hell {
                (0xC0, 0x30, 0xC0)
            } else {
                (0x50, 0x50, 0x58)
            };
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 0xFF;
        }
    }
    Bild { breite, hoehe, rgba }
}
