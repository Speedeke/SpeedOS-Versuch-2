// libspeed::fenster — Ein Fenster aus Ring 3 (Serie 8, Teil 1)
//
// ==========================================================================
// WAS EIN PROGRAMM HIER BEKOMMT
//
// Ein `Fenster` mit einem eigenen Pixelpuffer im PROZESS-Speicher, eine
// Handvoll Zeichenhilfen darauf, und `naechstes_ereignis()`. Mehr braucht
// eine Ereignisschleife nicht:
//
//     let mut f = Fenster::oeffnen("Titel", 480, 320)?;
//     loop {
//         f.malen(...);          // in den eigenen Puffer
//         f.zeigen()?;           // EIN Syscall: Puffer -> Fenster
//         match f.naechstes_ereignis(16)? {
//             Ereignis::Schliessen => break,
//             ...
//         }
//     }
//
// DER PUFFER LIEGT HIER, NICHT IM KERNEL. Das ist der Kern des Entwurfs
// „Pixelpuffer per Syscall": Ein Programm zeichnet in gewoehnlichen
// Speicher, den es besitzt, und uebergibt ihn erst beim `zeigen`. Der
// Kernel prueft und KOPIERT (Dauerregel I) — deshalb kann kein Programm
// dem Compositor die Pixel unter den Haenden wegaendern.
//
// ==========================================================================
// DAS PIXELFORMAT (ABI, docs/syscalls.md)
//
// 4 Byte je Pixel: Byte 0 = Blau, 1 = Gruen, 2 = Rot, 3 = ungenutzt.
// Als Little-Endian-`u32` gelesen ist das `0x00RRGGBB` — also die
// Schreibweise, die man aus HTML kennt. Hier wird deshalb mit `u32`
// gerechnet und erst beim Senden als Bytes gelesen.
//
// ==========================================================================
// TEILWEISE ZEICHNEN IST DER NORMALFALL, nicht die Optimierung
//
// `zeigen()` schickt das ganze Fenster, `zeigen_bereich()` nur einen
// Streifen. Der Kernel meldet genau diesen Streifen als Schaden an den
// Compositor — bei 4K ist der Unterschied zwischen beidem der Unterschied
// zwischen einer fluessigen und einer zaehen Oberflaeche
// (docs/fenster-syscalls.md §4).

use crate::{syscall, Ergebnis, Fehler};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Die ABI (Abschrift aus docs/syscalls.md — bewusst NICHT geteilt)
// ---------------------------------------------------------------------------

pub const SYS_FENSTER_OEFFNEN: u64 = 48;
pub const SYS_FENSTER_ZEICHNEN: u64 = 49;
pub const SYS_FENSTER_EREIGNIS: u64 = 50;
pub const SYS_FENSTER_TITEL: u64 = 51;
pub const SYS_FENSTER_SCHLIESSEN: u64 = 52;

/// Ereignis-Arten (die Zahlen sind ABI).
pub const ART_KEINS: u32 = 0;
pub const ART_MAUS_BEWEGT: u32 = 1;
pub const ART_MAUS_AB: u32 = 2;
pub const ART_MAUS_AUF: u32 = 3;
pub const ART_MAUS_RAD: u32 = 4;
pub const ART_TASTE: u32 = 5;
pub const ART_SONDERTASTE: u32 = 6;
pub const ART_FOKUS: u32 = 7;
pub const ART_GROESSE: u32 = 8;
pub const ART_SCHLIESSEN: u32 = 9;

/// Sondertasten (ABI).
pub const SONDER_HOCH: i32 = 1;
pub const SONDER_RUNTER: i32 = 2;
pub const SONDER_LINKS: i32 = 3;
pub const SONDER_RECHTS: i32 = 4;
pub const SONDER_POS1: i32 = 5;
pub const SONDER_ENDE: i32 = 6;
pub const SONDER_BILD_HOCH: i32 = 7;
pub const SONDER_BILD_RUNTER: i32 = 8;
pub const SONDER_ENTF: i32 = 9;
pub const SONDER_F1: i32 = 20;

/// Maustasten (ABI).
pub const KNOPF_LINKS: i32 = 0;
pub const KNOPF_RECHTS: i32 = 1;
pub const KNOPF_MITTE: i32 = 2;

/// Groesse der Ereignis-Struktur in Bytes (ABI).
pub const EREIGNIS_BYTES: usize = 16;

/// Packt ein Rechteck in ein u64 — je 16 Bit x, y, Breite, Hoehe.
///
/// Warum gepackt: `fenster_zeichnen` hat vier Argument-Register und
/// braucht fuenf Zahlen. Die Alternative waere ein Zeiger auf ein Struct
/// gewesen — eine Bereichspruefung und eine Fehlerquelle mehr fuer vier
/// kleine Zahlen (dasselbe Argument wie bei `pipe()`).
pub fn rechteck_packen(x: u16, y: u16, breite: u16, hoehe: u16) -> u64 {
    ((x as u64) << 48) | ((y as u64) << 32) | ((breite as u64) << 16) | hoehe as u64
}

// ---------------------------------------------------------------------------
// Das Ereignis
// ---------------------------------------------------------------------------

/// Ein Eingabe-Ereignis, schon ausgepackt.
///
/// Ein Enum und nicht die rohe 16-Byte-Struktur: Die ABI ist vier Zahlen,
/// deren Bedeutung an der Art haengt — genau das, was ein Rust-Enum besser
/// ausdrueckt als jeder Kommentar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ereignis {
    /// Nichts passiert (die Frist ist abgelaufen). **Kein Fehler** — eine
    /// Ereignisschleife darf hier animieren und weitermachen.
    Keins,
    MausBewegt { x: i32, y: i32 },
    MausAb { x: i32, y: i32, knopf: i32 },
    MausAuf { x: i32, y: i32, knopf: i32 },
    MausRad { x: i32, y: i32, delta: i32 },
    /// Ein Zeichen (schon Unicode — der Kernel dekodiert die Tastatur).
    Taste(char),
    /// Eine Taste ohne Zeichen (Pfeile, Funktionstasten — `SONDER_*`).
    Sondertaste(i32),
    /// Fokus bekommen (`true`) oder verloren (`false`).
    Fokus(bool),
    /// Das Fenster hat eine neue Inhaltsgroesse. **Der Puffer des Kernels
    /// ist dabei neu und leer** — wer nicht neu zeichnet, sieht nichts.
    Groesse { breite: u32, hoehe: u32 },
    /// Der Benutzer moechte das Fenster schliessen. Ein WUNSCH, kein
    /// Befehl: Das Programm darf noch aufraeumen. Reagiert es nicht,
    /// schliesst der zweite Klick auf das X das Fenster.
    Schliessen,
}

impl Ereignis {
    /// Baut das Ereignis aus den 16 ABI-Bytes.
    fn aus_bytes(b: &[u8; EREIGNIS_BYTES]) -> Ereignis {
        let art = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let x = i32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let y = i32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        let wert = i32::from_le_bytes([b[12], b[13], b[14], b[15]]);
        match art {
            ART_MAUS_BEWEGT => Ereignis::MausBewegt { x, y },
            ART_MAUS_AB => Ereignis::MausAb { x, y, knopf: wert },
            ART_MAUS_AUF => Ereignis::MausAuf { x, y, knopf: wert },
            ART_MAUS_RAD => Ereignis::MausRad { x, y, delta: wert },
            // Ein Zeichen, das kein gueltiger Unicode-Skalarwert ist, kann
            // nicht entstehen (der Kernel schickt ein `char`) — sollte es
            // doch, wird es verworfen statt geraten.
            ART_TASTE => match char::from_u32(wert as u32) {
                Some(zeichen) => Ereignis::Taste(zeichen),
                None => Ereignis::Keins,
            },
            ART_SONDERTASTE => Ereignis::Sondertaste(wert),
            ART_FOKUS => Ereignis::Fokus(wert != 0),
            ART_GROESSE => Ereignis::Groesse {
                breite: x.max(0) as u32,
                hoehe: y.max(0) as u32,
            },
            ART_SCHLIESSEN => Ereignis::Schliessen,
            _ => Ereignis::Keins,
        }
    }
}

// ---------------------------------------------------------------------------
// Das Fenster
// ---------------------------------------------------------------------------

/// Ein Fenster mit eigenem Pixelpuffer.
pub struct Fenster {
    handle: u64,
    breite: usize,
    hoehe: usize,
    /// Der Puffer im EIGENEN Speicher. `0x00RRGGBB` je Pixel.
    pixel: Vec<u32>,
}

impl Fenster {
    /// Oeffnet ein Fenster. Der Kernel legt Puffer und Deko an; der Titel
    /// steht in einer Titelleiste, die dem Kernel gehoert.
    pub fn oeffnen(titel: &str, breite: usize, hoehe: usize) -> Result<Fenster, Fehler> {
        let handle = unsafe {
            syscall(
                SYS_FENSTER_OEFFNEN,
                titel.as_ptr() as u64,
                titel.len() as u64,
                breite as u64,
                hoehe as u64,
            )
        }?;
        Ok(Fenster {
            handle,
            breite,
            hoehe,
            pixel: vec![0u32; breite * hoehe],
        })
    }

    pub fn breite(&self) -> usize {
        self.breite
    }
    pub fn hoehe(&self) -> usize {
        self.hoehe
    }
    pub fn handle(&self) -> u64 {
        self.handle
    }

    /// Setzt EIN Pixel (ausserhalb: wird ignoriert, nie ein Absturz).
    #[inline]
    pub fn punkt(&mut self, x: i32, y: i32, farbe: u32) {
        if x < 0 || y < 0 || x as usize >= self.breite || y as usize >= self.hoehe {
            return;
        }
        self.pixel[y as usize * self.breite + x as usize] = farbe;
    }

    /// Fuellt ein Rechteck (geklemmt).
    pub fn rechteck(&mut self, x: i32, y: i32, breite: i32, hoehe: i32, farbe: u32) {
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = ((x + breite).max(0) as usize).min(self.breite);
        let y1 = ((y + hoehe).max(0) as usize).min(self.hoehe);
        for zeile in y0..y1 {
            let ab = zeile * self.breite;
            self.pixel[ab + x0..ab + x1].fill(farbe);
        }
    }

    /// Fuellt die ganze Flaeche.
    pub fn fuellen(&mut self, farbe: u32) {
        self.pixel.fill(farbe);
    }

    /// Baut eine Farbe aus den drei Kanaelen.
    pub fn farbe(rot: u8, gruen: u8, blau: u8) -> u32 {
        ((rot as u32) << 16) | ((gruen as u32) << 8) | blau as u32
    }

    /// Verschiebt den EIGENEN Puffer senkrecht um `dy` Pixel.
    ///
    /// =====================================================================
    /// DER SCHNELLPFAD DES SCROLLENS (Serie 8, Teil 7)
    ///
    /// Beim Scrollen um `dy` bleibt fast der ganze Inhalt derselbe — er
    /// steht nur woanders. Ihn zu VERSCHIEBEN statt neu zu malen, spart
    /// das Zeichnen aller sichtbaren Befehle; danach muss nur der neu
    /// freigewordene Streifen gemalt werden (`speedpaint::Sicht` rechnet
    /// aus, welcher).
    ///
    /// `dy > 0` heisst: der Inhalt wandert nach OBEN (man scrollt nach
    /// unten weiter). Der freigewordene Streifen liegt dann UNTEN und
    /// wird NICHT geleert — der Maler fuellt ihn ohnehin als Erstes mit
    /// dem Seitenhintergrund, und ihn hier zusaetzlich zu leeren waere
    /// derselbe Speicher zweimal angefasst.
    ///
    /// WAS DAS **NICHT** SPART: die Uebertragung. Der Kernel hat eine
    /// eigene Kopie des Fensterpuffers und weiss von dieser Verschiebung
    /// nichts — es muss anschliessend trotzdem die ganze Flaeche
    /// uebertragen werden. Gemessen und eingeordnet in
    /// docs/browser-rendern.md; es ist genau die Zahl, an der das
    /// Umstiegskriterium aus docs/fenster-syscalls.md haengt.
    pub fn senkrecht_verschieben(&mut self, dy: i32) {
        self.senkrecht_verschieben_bereich(0, self.hoehe, dy);
    }

    /// Verschiebt NUR ein waagerechtes Band des Puffers.
    ///
    /// =====================================================================
    /// WARUM ES DAS BRAUCHT (Serie-8-Abschluss)
    ///
    /// Ein Browser-Fenster besteht aus zwei Teilen, die sich verschieden
    /// verhalten: Die Bedienleiste steht STILL, und nur der Inhalt
    /// scrollt. `senkrecht_verschieben` bewegte den GANZEN Puffer — und
    /// zog die Leiste damit jedes Mal mit aus dem Bild. Sie musste
    /// deshalb bei JEDEM Scroll-Frame neu gezeichnet werden.
    ///
    /// Gemessen hat das bei 4K **2 200 us je Frame** gekostet (Tab-Leiste,
    /// Knoepfe, Adressfeld, Statuszeile — alles fuer nichts) und den
    /// Scroll-Frame von 7 050 auf 9 900 us gehoben. Das ist die
    /// 8-ms-Schwelle des Umstiegskriteriums aus docs/fenster-syscalls.md,
    /// und sie waere damit an einer selbstgemachten Verschwendung
    /// gerissen — nicht an der Architektur.
    ///
    /// Mit dem Band bleibt die Leiste unberuehrt und muss nicht neu
    /// gemalt werden.
    pub fn senkrecht_verschieben_bereich(&mut self, y: usize, hoehe: usize, dy: i32) {
        let y = y.min(self.hoehe);
        let hoehe = hoehe.min(self.hoehe - y);
        let weite = dy.unsigned_abs() as usize;
        if dy == 0 || hoehe == 0 || weite >= hoehe {
            return;
        }
        let oben = y * self.breite;
        let unten = (y + hoehe) * self.breite;
        let versatz = weite * self.breite;
        if dy > 0 {
            // Inhalt nach oben: Ziel liegt VOR der Quelle, also vorwaerts
            // kopieren. `copy_within` beherrscht die Ueberlappung selbst
            // (es ist ein memmove) — von Hand waere hier die klassische
            // Stelle fuer einen Ueberlappungsfehler.
            self.pixel.copy_within(oben + versatz..unten, oben);
        } else {
            self.pixel.copy_within(oben..unten - versatz, oben + versatz);
        }
    }

    /// Uebertraegt den GANZEN Puffer ins Fenster. Liefert die Zahl der
    /// uebernommenen Pixel (kleiner als erwartet heisst: das Fenster ist
    /// gerade kleiner geworden — dann kommt gleich ein `Groesse`-Ereignis).
    pub fn zeigen(&self) -> Ergebnis {
        self.zeigen_bereich(0, 0, self.breite, self.hoehe)
    }

    /// Uebertraegt nur einen BEREICH — der Normalfall fuer alles, was sich
    /// haeufig aendert (ein Cursor, eine Statuszeile, eine Scroll-Spalte).
    ///
    /// Der Bereich muss zusammenhaengend im Puffer liegen, deshalb wird er
    /// zeilenweise in einen Sendepuffer kopiert. Ist er so breit wie das
    /// Fenster, faellt das Kopieren weg (der Ausschnitt IST dann schon
    /// zusammenhaengend) — deswegen sind volle Zeilen der guenstigste
    /// Teilbereich.
    pub fn zeigen_bereich(&self, x: usize, y: usize, breite: usize, hoehe: usize) -> Ergebnis {
        if breite == 0 || hoehe == 0 {
            return Ok(0);
        }
        let x = x.min(self.breite);
        let y = y.min(self.hoehe);
        let breite = breite.min(self.breite - x);
        let hoehe = hoehe.min(self.hoehe - y);
        if breite == 0 || hoehe == 0 {
            return Ok(0);
        }
        let rechteck = rechteck_packen(x as u16, y as u16, breite as u16, hoehe as u16);

        if breite == self.breite {
            // Volle Zeilen: der Ausschnitt liegt schon zusammenhaengend da.
            let ab = y * self.breite;
            let bis = ab + hoehe * self.breite;
            return self.senden(&self.pixel[ab..bis], rechteck);
        }
        let mut streifen: Vec<u32> = Vec::with_capacity(breite * hoehe);
        for zeile in 0..hoehe {
            let ab = (y + zeile) * self.breite + x;
            streifen.extend_from_slice(&self.pixel[ab..ab + breite]);
        }
        self.senden(&streifen, rechteck)
    }

    fn senden(&self, pixel: &[u32], rechteck: u64) -> Ergebnis {
        unsafe {
            syscall(
                SYS_FENSTER_ZEICHNEN,
                self.handle,
                pixel.as_ptr() as u64,
                (pixel.len() * 4) as u64,
                rechteck,
            )
        }
    }

    /// Holt das naechste Ereignis. BLOCKIERT bis zu `frist_ms`
    /// Millisekunden; danach kommt `Ereignis::Keins` zurueck.
    ///
    /// `frist_ms == 0` heisst NICHT „ewig" — der Kernel setzt dann seine
    /// Standardfrist. Ewiges Warten waere ein Haenger ohne Meldung, und
    /// die Sorte gibt es in SpeedOS nicht (dieselbe Entscheidung wie beim
    /// `zufall`-Syscall).
    ///
    /// **Auf `Groesse` MUSS reagiert werden**: Der Kernel hat den Puffer
    /// dabei neu angelegt, er ist leer. `groesse_uebernehmen` passt den
    /// eigenen Puffer an.
    pub fn naechstes_ereignis(&self, frist_ms: u64) -> Result<Ereignis, Fehler> {
        let mut roh = [0u8; EREIGNIS_BYTES];
        unsafe {
            syscall(
                SYS_FENSTER_EREIGNIS,
                self.handle,
                roh.as_mut_ptr() as u64,
                frist_ms,
                0,
            )
        }?;
        Ok(Ereignis::aus_bytes(&roh))
    }

    /// Passt den eigenen Puffer an eine neue Groesse an (nach
    /// `Ereignis::Groesse`). Der Inhalt geht dabei verloren — der Kernel
    /// hat seinen Puffer ohnehin schon neu angelegt.
    pub fn groesse_uebernehmen(&mut self, breite: u32, hoehe: u32) {
        self.breite = breite as usize;
        self.hoehe = hoehe as usize;
        self.pixel = vec![0u32; self.breite * self.hoehe];
    }

    /// Setzt den Fenstertitel neu.
    pub fn titel_setzen(&self, titel: &str) -> Ergebnis {
        unsafe {
            syscall(
                SYS_FENSTER_TITEL,
                self.handle,
                titel.as_ptr() as u64,
                titel.len() as u64,
                0,
            )
        }
    }

    /// Schliesst das Fenster. Passiert beim Prozess-Ende ohnehin
    /// automatisch (die Handle-Tabelle des Kernels raeumt auf) — hier
    /// steht es fuer den Fall, dass ein Programm weiterlaeuft.
    pub fn schliessen(self) -> Ergebnis {
        unsafe { syscall(SYS_FENSTER_SCHLIESSEN, self.handle, 0, 0, 0) }
    }

    /// Zeichnet Text mit einem eingebauten 5x7-Raster.
    ///
    /// EIN MINIMALER ZEICHENSATZ, und das ist Absicht: Die Schriften des
    /// Kernels sind Kernel-Daten, und es gibt (noch) keinen Syscall, der
    /// sie herausgibt. Fuer ein Beweis-Programm reicht ein Raster; was ein
    /// HTML-Renderer wirklich braeuchte, steht in
    /// docs/serie8-bestandsaufnahme.md.
    pub fn text(&mut self, x: i32, y: i32, text: &str, farbe: u32, skala: i32) {
        let skala = skala.max(1);
        let mut stift = x;
        for zeichen in text.chars() {
            let muster = raster(zeichen);
            for (spalte, saeule) in muster.iter().enumerate() {
                for reihe in 0..7 {
                    if saeule & (1 << reihe) != 0 {
                        self.rechteck(
                            stift + spalte as i32 * skala,
                            y + reihe * skala,
                            skala,
                            skala,
                            farbe,
                        );
                    }
                }
            }
            stift += 6 * skala;
        }
    }
}

/// Der 5x7-Zeichensatz: je Zeichen fuenf Spalten, je Spalte sieben Bits
/// (Bit 0 = oben). Nur was gebraucht wird — Unbekanntes wird zu einem
/// vollen Kaesten, damit ein fehlendes Zeichen SICHTBAR ist (dasselbe
/// Prinzip wie das Magenta bei den Kernel-Icons).
fn raster(zeichen: char) -> [u8; 5] {
    match zeichen.to_ascii_uppercase() {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00],
        'A' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        'B' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        'C' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        'D' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        'E' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        'F' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        'G' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'I' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        'J' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        'K' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        'L' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        'M' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        'N' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        'O' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        'P' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        'Q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        'R' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        'T' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        'U' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        'V' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        'W' => [0x7F, 0x20, 0x18, 0x20, 0x7F],
        'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' => [0x03, 0x04, 0x78, 0x04, 0x03],
        'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        '1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46],
        '3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        '9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        '.' => [0x00, 0x40, 0x60, 0x00, 0x00],
        ',' => [0x00, 0x50, 0x30, 0x00, 0x00],
        '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '+' => [0x08, 0x08, 0x3E, 0x08, 0x08],
        '/' => [0x20, 0x10, 0x08, 0x04, 0x02],
        '(' => [0x00, 0x1C, 0x22, 0x41, 0x00],
        ')' => [0x00, 0x41, 0x22, 0x1C, 0x00],
        '!' => [0x00, 0x00, 0x5F, 0x00, 0x00],
        '?' => [0x02, 0x01, 0x51, 0x09, 0x06],
        '=' => [0x14, 0x14, 0x14, 0x14, 0x14],
        '#' => [0x14, 0x7F, 0x14, 0x7F, 0x14],
        '*' => [0x14, 0x08, 0x3E, 0x08, 0x14],
        // SATZZEICHEN AUS ECHTEM TEXT (Serie 8, Teil 7). Sie fehlten, und
        // der erste gerenderte Seitentext hat es sofort gezeigt: Aus
        // „WORLD'S" wurde „WORLD<Kasten>S" — der Apostroph ist in
        // englischer Prosa haeufiger als jedes `#` oder `*`, die hier von
        // Anfang an standen. Ein fehlendes Zeichen SICHTBAR zu machen war
        // richtig; es dann auch zu ergaenzen, ist der zweite Schritt.
        '\'' => [0x00, 0x00, 0x07, 0x00, 0x00],
        '"' => [0x00, 0x07, 0x00, 0x07, 0x00],
        ';' => [0x00, 0x56, 0x36, 0x00, 0x00],
        '<' => [0x08, 0x14, 0x22, 0x41, 0x00],
        '>' => [0x00, 0x41, 0x22, 0x14, 0x08],
        '[' => [0x00, 0x7F, 0x41, 0x41, 0x00],
        ']' => [0x00, 0x41, 0x41, 0x7F, 0x00],
        '{' => [0x00, 0x08, 0x36, 0x41, 0x00],
        '}' => [0x00, 0x41, 0x36, 0x08, 0x00],
        '%' => [0x23, 0x13, 0x08, 0x64, 0x62],
        '&' => [0x36, 0x49, 0x55, 0x22, 0x50],
        '_' => [0x40, 0x40, 0x40, 0x40, 0x40],
        '|' => [0x00, 0x00, 0x7F, 0x00, 0x00],
        '@' => [0x3E, 0x41, 0x5D, 0x55, 0x1E],
        '$' => [0x24, 0x2A, 0x7F, 0x2A, 0x12],
        '~' => [0x08, 0x04, 0x08, 0x10, 0x08],
        '^' => [0x04, 0x02, 0x01, 0x02, 0x04],
        '`' => [0x00, 0x01, 0x02, 0x00, 0x00],
        '\\' => [0x02, 0x04, 0x08, 0x10, 0x20],
        _ => [0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
    }
}

/// Eine kleine Hilfe fuer Titel wie "fenstertest (3 Klicks)".
pub fn titel_mit_zahl(name: &str, zahl: u64) -> String {
    let mut aus = String::from(name);
    aus.push_str(" (");
    let mut ziffern = [0u8; 20];
    let mut i = 20;
    let mut rest = zahl;
    loop {
        i -= 1;
        ziffern[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    for &z in &ziffern[i..] {
        aus.push(z as char);
    }
    aus.push(')');
    aus
}
