// bilder — DER BILDBETRACHTER (Serie 8, Teil 3)
//
// ===========================================================================
// WAS ER BEWEIST
//
// Die ganze Kette in einem Programm: Datei von /platte -> Dekoder in Ring 3
// (`libspeed::bild`) -> RGBA-Puffer -> Fenster-Syscall -> Compositor. Kein
// Stueck davon liegt im Kernel; der Kernel sieht Pixel und sonst nichts.
//
// ===========================================================================
// BEDIENUNG
//
//     starte bilder /platte/bilder/verlauf.png &
//
//     +/-        zoomen (ganzzahlig: ... 1:4, 1:3, 1:2, 1:1, 2:1, 3:1 ...)
//     0          zurueck auf „ganz sichtbar"
//     1          zurueck auf 1:1
//     Pfeile     verschieben
//     Mausrad    zoomen (um die Mausposition herum)
//     Ziehen     verschieben
//     i          Info ein/aus
//     q / ESC    Schluss
//
// DAS `&` IST PFLICHT (docs/fenster-syscalls.md): Solange ein Shell-Befehl
// synchron laeuft, kommt kein anderer Kernel-Task dran — auch der
// Compositor nicht. Ohne `&` zeichnet das Programm brav, und niemand sieht
// es.
//
// ===========================================================================
// WARUM GANZZAHLIGER ZOOM
//
// Unser Target hat `-sse,+soft-float` — Fliesskomma gibt es hier nicht. Der
// Zoom ist deshalb ein BRUCH aus zwei kleinen ganzen Zahlen (`zaehler`
// zu `nenner`), und die Abbildung Bildschirm -> Bild ist eine
// Ganzzahl-Division. Das ist keine Notloesung: Nearest-Neighbour mit
// ganzzahligen Faktoren ist bei 2:1 und 3:1 sogar das RICHTIGE — ein
// weichgezeichnetes Pixelbild waere schlechter.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use libspeed::bild::{self, Bild, BildFehler, Grenzen};
use libspeed::fenster::{
    Ereignis, Fenster, KNOPF_LINKS, SONDER_ENDE, SONDER_HOCH, SONDER_LINKS, SONDER_POS1,
    SONDER_RECHTS, SONDER_RUNTER,
};
use libspeed::{hauptprogramm, println, Argumente};

/// Der Hintergrund hinter durchsichtigen Bildteilen und neben dem Bild.
const HINTERGRUND: u32 = 0x0012_1422;
/// Das Schachbrett, das Durchsichtigkeit sichtbar macht.
const KARO_A: u32 = 0x0020_2434;
const KARO_B: u32 = 0x0018_1B28;
const KARO_KANTE: i32 = 8;

const TEXT: u32 = 0x00C8_D0E4;
const TEXT_STARK: u32 = 0x00F0_F4FF;
const FEHLER_FARBE: u32 = 0x00FF_6B6B;
const BALKEN: u32 = 0x001A_1E2E;

/// Anfangsgroesse des Fensters.
const FENSTER_BREITE: usize = 720;
const FENSTER_HOEHE: usize = 520;

/// Hoehe der Statuszeile am unteren Rand.
const LEISTE: i32 = 18;

// ---------------------------------------------------------------------------
// Der Zustand
// ---------------------------------------------------------------------------

/// Was gerade angezeigt wird.
enum Inhalt {
    /// Ein dekodiertes Bild.
    Bild(Bild),
    /// Es ging schief — der Grund wird ANGEZEIGT, nicht verschwiegen
    /// (Daten-Integritaets-Regel: die Oberflaeche zeigt den Fehler).
    Fehler(BildFehler),
}

struct Betrachter {
    inhalt: Inhalt,
    pfad: Vec<u8>,
    /// Zoom als Bruch `zaehler / nenner` — hoechstens einer von beiden > 1.
    zaehler: i32,
    nenner: i32,
    /// Linke obere Ecke des Bildes in Fenster-Koordinaten.
    ab_x: i32,
    ab_y: i32,
    /// Wird gerade gezogen? Dann die letzte Mausposition.
    zieht: Option<(i32, i32)>,
    info: bool,
    /// Bytes der Datei — fuer die Info-Zeile.
    datei_bytes: usize,
}

impl Betrachter {
    /// Bildbreite/-hoehe auf dem Bildschirm bei aktuellem Zoom.
    fn anzeige_masse(&self) -> (i32, i32) {
        match &self.inhalt {
            Inhalt::Bild(b) => (
                (b.breite() as i32 * self.zaehler / self.nenner).max(1),
                (b.hoehe() as i32 * self.zaehler / self.nenner).max(1),
            ),
            Inhalt::Fehler(_) => (0, 0),
        }
    }

    /// Zoom so waehlen, dass das ganze Bild in die Flaeche passt.
    ///
    /// GANZZAHLIG UND NIE GROESSER ALS 1:1 — ein 8x8-Icon auf Fenstergroesse
    /// aufzublasen ist selten das, was jemand will, und sieht nach einem
    /// Fehler aus.
    fn einpassen(&mut self, breite: i32, hoehe: i32) {
        let (bb, bh) = match &self.inhalt {
            Inhalt::Bild(b) => (b.breite() as i32, b.hoehe() as i32),
            Inhalt::Fehler(_) => return,
        };
        let flaeche_h = (hoehe - LEISTE).max(1);
        let breite = breite.max(1);
        self.zaehler = 1;
        self.nenner = 1;
        if bb > breite || bh > flaeche_h {
            // Kleinster Nenner, mit dem beides passt.
            let mut n = 2;
            while n < 64 && (bb / n > breite || bh / n > flaeche_h) {
                n += 1;
            }
            self.nenner = n;
        }
        self.zentrieren(breite, hoehe);
    }

    fn zentrieren(&mut self, breite: i32, hoehe: i32) {
        let (aw, ah) = self.anzeige_masse();
        self.ab_x = (breite - aw) / 2;
        self.ab_y = (hoehe - LEISTE - ah) / 2;
    }

    /// Einen Schritt hinein- oder herauszoomen, um den Punkt (`mx`,`my`).
    ///
    /// DER PUNKT UNTER DER MAUS BLEIBT STEHEN — das ist der ganze
    /// Unterschied zwischen brauchbarem und aergerlichem Zoom. Die Rechnung
    /// ist: Bildkoordinate unter der Maus vorher bestimmen, Zoom aendern,
    /// Ursprung so verschieben, dass dieselbe Bildkoordinate wieder unter
    /// der Maus liegt.
    fn zoomen(&mut self, hinein: bool, mx: i32, my: i32) {
        // Bildkoordinate unter der Maus, VOR der Aenderung.
        let bx = (mx - self.ab_x) * self.nenner / self.zaehler;
        let by = (my - self.ab_y) * self.nenner / self.zaehler;

        if hinein {
            if self.nenner > 1 {
                self.nenner -= 1;
            } else if self.zaehler < 16 {
                self.zaehler += 1;
            }
        } else if self.zaehler > 1 {
            self.zaehler -= 1;
        } else if self.nenner < 32 {
            self.nenner += 1;
        }

        // Ursprung so setzen, dass (bx,by) wieder unter (mx,my) liegt.
        self.ab_x = mx - bx * self.zaehler / self.nenner;
        self.ab_y = my - by * self.zaehler / self.nenner;
    }

    /// Malt alles in den Fensterpuffer.
    fn zeichnen(&self, f: &mut Fenster) {
        let breite = f.breite() as i32;
        let hoehe = f.hoehe() as i32;
        let flaeche_h = hoehe - LEISTE;

        match &self.inhalt {
            Inhalt::Bild(b) => self.bild_zeichnen(f, b, breite, flaeche_h),
            Inhalt::Fehler(fehler) => {
                f.fuellen(HINTERGRUND);
                let mitte = flaeche_h / 2;
                f.text(16, mitte - 24, "Bild kann nicht angezeigt werden", FEHLER_FARBE, 2);
                f.text(16, mitte + 4, fehler.text(), TEXT, 1);
                f.text(16, mitte + 20, fehler.kurz(), TEXT, 1);
            }
        }

        // --- Statuszeile ---
        f.rechteck(0, flaeche_h, breite, LEISTE, BALKEN);
        let mut zeile = [0u8; 160];
        let n = self.statuszeile(&mut zeile);
        if let Ok(s) = core::str::from_utf8(&zeile[..n]) {
            f.text(6, flaeche_h + 5, s, TEXT_STARK, 1);
        }
    }

    /// Das Bild selbst — Nearest-Neighbour, Zeile fuer Zeile.
    ///
    /// ES WIRD UEBER DIE ZIELPIXEL GELAUFEN, nicht ueber die Quellpixel.
    /// Andersherum ginge auch und waere bei starker Verkleinerung sogar
    /// schneller — aber dann bleiben beim Vergroessern Luecken, und man
    /// muesste sie mit einer zweiten Schleife fuellen. Ueber das Ziel zu
    /// laufen trifft JEDES Pixel genau einmal, in beiden Richtungen.
    fn bild_zeichnen(&self, f: &mut Fenster, b: &Bild, breite: i32, flaeche_h: i32) {
        let (aw, ah) = self.anzeige_masse();

        // Rundherum das Schachbrett (es zeigt auch, wo das Bild AUFHOERT).
        self.karo(f, breite, flaeche_h);

        // Nur der Teil, der sichtbar ist — bei 16:1 auf einem 4K-Schirm
        // waere alles andere Rechenzeit fuer nichts.
        let x0 = self.ab_x.max(0);
        let y0 = self.ab_y.max(0);
        let x1 = (self.ab_x + aw).min(breite);
        let y1 = (self.ab_y + ah).min(flaeche_h);
        if x1 <= x0 || y1 <= y0 {
            return; // ganz herausgeschoben
        }

        for y in y0..y1 {
            let by = ((y - self.ab_y) * self.nenner / self.zaehler) as u32;
            for x in x0..x1 {
                let bx = ((x - self.ab_x) * self.nenner / self.zaehler) as u32;
                // Der Untergrund ist das Karo — durchsichtige Bildstellen
                // zeigen es, und genau daran erkennt man sie.
                let unter = Self::karo_farbe(x, y);
                f.punkt(x, y, b.pixel_auf(bx, by, unter));
            }
        }
    }

    #[inline]
    fn karo_farbe(x: i32, y: i32) -> u32 {
        if ((x / KARO_KANTE) + (y / KARO_KANTE)) % 2 == 0 {
            KARO_A
        } else {
            KARO_B
        }
    }

    fn karo(&self, f: &mut Fenster, breite: i32, hoehe: i32) {
        let mut y = 0;
        while y < hoehe {
            let mut x = 0;
            while x < breite {
                f.rechteck(x, y, KARO_KANTE, KARO_KANTE, Self::karo_farbe(x, y));
                x += KARO_KANTE;
            }
            y += KARO_KANTE;
        }
    }

    /// Die Statuszeile, von Hand zusammengesetzt.
    ///
    /// VON HAND UND NICHT MIT `format!`: Ein `format!` je Bild-Neuzeichnung
    /// waere eine Heap-Allokation im heissen Pfad — und der Heap ist hier
    /// die knappe Ressource (das ganze Bild liegt darin).
    fn statuszeile(&self, ziel: &mut [u8]) -> usize {
        let mut n = 0usize;

        fn schreib(ziel: &mut [u8], n: &mut usize, s: &str) {
            for &b in s.as_bytes() {
                if *n < ziel.len() {
                    ziel[*n] = b;
                    *n += 1;
                }
            }
        }
        fn zahl(ziel: &mut [u8], n: &mut usize, mut v: u64) {
            let mut puffer = [0u8; 20];
            let mut i = puffer.len();
            loop {
                i -= 1;
                puffer[i] = b'0' + (v % 10) as u8;
                v /= 10;
                if v == 0 || i == 0 {
                    break;
                }
            }
            for &b in &puffer[i..] {
                if *n < ziel.len() {
                    ziel[*n] = b;
                    *n += 1;
                }
            }
        }

        match &self.inhalt {
            Inhalt::Bild(b) => {
                zahl(ziel, &mut n, b.breite() as u64);
                schreib(ziel, &mut n, "x");
                zahl(ziel, &mut n, b.hoehe() as u64);
                schreib(ziel, &mut n, "  Zoom ");
                zahl(ziel, &mut n, self.zaehler as u64);
                schreib(ziel, &mut n, ":");
                zahl(ziel, &mut n, self.nenner as u64);
                if self.info {
                    schreib(ziel, &mut n, "  Datei ");
                    zahl(ziel, &mut n, self.datei_bytes as u64);
                    schreib(ziel, &mut n, " B  RGBA ");
                    zahl(ziel, &mut n, b.bytes() as u64);
                    schreib(ziel, &mut n, " B  Heap-Spitze ");
                    let (_, _, spitze) = libspeed::heap::heap_stand();
                    zahl(ziel, &mut n, spitze as u64);
                    schreib(ziel, &mut n, " B");
                } else {
                    schreib(ziel, &mut n, "   +/- 0 1 Pfeile  i=Info  q=Ende");
                }
            }
            Inhalt::Fehler(f) => {
                schreib(ziel, &mut n, "FEHLER: ");
                schreib(ziel, &mut n, f.kurz());
                schreib(ziel, &mut n, "   q=Ende");
            }
        }
        n
    }
}

// ---------------------------------------------------------------------------
// Datei lesen
// ---------------------------------------------------------------------------

/// Liest eine Datei auf den Heap, hoechstens `max` Bytes.
///
/// Wird die Grenze ueberschritten, bricht das Lesen SOFORT ab — die Datei
/// erst ganz einzulesen und dann abzulehnen waere die teuerste Art, Nein
/// zu sagen. Der Dekoder sieht dann mehr Bytes als erlaubt und lehnt ab.
fn datei_lesen(pfad: &str, max: usize) -> Result<Vec<u8>, libspeed::Fehler> {
    let handle = libspeed::oeffne(pfad, libspeed::LESEN)?;
    let mut daten: Vec<u8> = Vec::new();
    let mut puffer = [0u8; 32 * 1024];
    loop {
        match libspeed::lese_at(handle, daten.len() as u64, &mut puffer) {
            Ok(0) => break,
            Ok(gelesen) => {
                daten.extend_from_slice(&puffer[..gelesen as usize]);
                if daten.len() > max {
                    break;
                }
            }
            Err(fehler) => {
                let _ = libspeed::schliesse(handle);
                return Err(fehler);
            }
        }
    }
    let _ = libspeed::schliesse(handle);
    Ok(daten)
}

// ---------------------------------------------------------------------------
// Hauptprogramm
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Der Pruefmodus — kein Fenster, eine Zeile Ausgabe
// ---------------------------------------------------------------------------

/// `bilder --pruefen <datei>`: dekodieren, EINE Zeile schreiben, beenden.
///
/// ===================================================================
/// WOZU EIN ZWEITER BETRIEBSMODUS
///
/// Der Dekoder liegt in Ring 3. Ein Kernel-Test kann ihn deshalb nicht
/// aufrufen — er muss einen PROZESS starten und dessen Ausgabe lesen.
/// Genau dieses Muster benutzt schon `tests/sicherheit.rs` mit
/// `angreifer` und `tests/netz_klient.rs` mit `holes`.
///
/// Ein Fenster darf dabei nicht aufgehen: Im Testkernel laeuft kein
/// Compositor, und ein Programm, das auf Fenster-Ereignisse wartet,
/// haengt bis zur Frist.
///
/// DIE AUSGABE IST EINE ZEILE UND MASCHINENLESBAR:
///
///     ok <breite> <hoehe> <rgba-bytes> <pixel(0,0)> <pixel(w-1,h-1)> <spitze>
///     fehler <kurz>
///
/// Die zwei Pixel sind der Grund, warum der Test mehr prueft als „ist
/// nicht abgestuerzt": Sie werden gegen die FORMEL aus
/// tools/testbilder_erzeugen.py verglichen. Ein Dekoder, der ein Bild in
/// der richtigen Groesse, aber mit vertauschten Farbkanaelen liefert,
/// faellt sonst nicht auf.
///
/// DER EXIT-CODE trennt die drei Faelle, auf die es ankommt:
///   0 = dekodiert, 1 = SAUBER abgelehnt, 3 = Datei unlesbar,
///   101 = PANIK (der Rust-Panic-Handler von libspeed) — und genau die
///   darf nie vorkommen.
fn pruefen(pfad: &str) -> i32 {
    let grenzen = Grenzen::standard();
    let daten = match datei_lesen(pfad, grenzen.max_datei_bytes) {
        Ok(d) => d,
        Err(f) => {
            println!("unlesbar {}", f.text());
            return 3;
        }
    };
    match bild::dekodieren_mit(&daten, grenzen) {
        Ok(b) => {
            let (_, _, spitze) = libspeed::heap::heap_stand();
            let letztes = b.pixel(b.breite() - 1, b.hoehe() - 1);
            println!(
                "ok {} {} {} {:08x} {:08x} {}",
                b.breite(),
                b.hoehe(),
                b.bytes(),
                b.pixel(0, 0),
                letztes,
                spitze
            );
            0
        }
        Err(f) => {
            println!("fehler {}", f.kurz());
            1
        }
    }
}

fn haupt(argumente: &Argumente) -> i32 {
    // `--pruefen <datei>` laeuft ohne Fenster (siehe `pruefen`).
    if argumente.get(1) == Some("--pruefen") {
        return match argumente.get(2) {
            Some(p) => pruefen(p),
            None => {
                println!("fehler kein-pfad");
                2
            }
        };
    }

    let pfad = match argumente.get(1) {
        Some(p) => p,
        None => {
            println!("Aufruf: starte bilder <datei.png|datei.jpg> &");
            println!();
            println!("  Das & ist Pflicht — ein synchroner Shell-Befehl laesst");
            println!("  den Compositor nicht laufen (docs/fenster-syscalls.md).");
            return 2;
        }
    };

    let grenzen = Grenzen::standard();

    // --- Lesen und dekodieren, BEVOR das Fenster aufgeht ---
    //
    // Erst das Fenster zu oeffnen und dann zu scheitern hiesse: Ein leeres
    // Fenster blitzt auf und verschwindet. So kann die Fehlermeldung IM
    // Fenster stehen — der Betrachter zeigt sie an, statt sie nur auf die
    // Konsole zu schreiben (Daten-Integritaets-Regel).
    let (inhalt, datei_bytes) = match datei_lesen(pfad, grenzen.max_datei_bytes) {
        Ok(daten) => {
            let n = daten.len();
            match bild::dekodieren_mit(&daten, grenzen) {
                Ok(b) => (Inhalt::Bild(b), n),
                Err(f) => {
                    println!("{}: {}", pfad, f.text());
                    (Inhalt::Fehler(f), n)
                }
            }
        }
        Err(f) => {
            println!("{}: {}", pfad, f.text());
            return 3;
        }
    };
    let war_fehler = matches!(inhalt, Inhalt::Fehler(_));

    let mut fenster = match Fenster::oeffnen("Bilder", FENSTER_BREITE, FENSTER_HOEHE) {
        Ok(f) => f,
        Err(fehler) => {
            println!("Fenster ging nicht auf: {}", fehler.text());
            return 4;
        }
    };

    let mut b = Betrachter {
        inhalt,
        pfad: pfad.as_bytes().to_vec(),
        zaehler: 1,
        nenner: 1,
        ab_x: 0,
        ab_y: 0,
        zieht: None,
        info: false,
        datei_bytes,
    };
    b.einpassen(fenster.breite() as i32, fenster.hoehe() as i32);
    titel_setzen(&fenster, &b);

    b.zeichnen(&mut fenster);
    let _ = fenster.zeigen();

    // --- Die Ereignisschleife ---
    //
    // Ein Fehler beim Abholen beendet sie: Er heisst, dass es das Fenster
    // nicht mehr gibt (der Handle ist zu). Weiterzuzeichnen waere ein
    // Prozess, der ins Leere malt.
    while let Ok(ereignis) = fenster.naechstes_ereignis(200) {
        let mut neu = true;
        match ereignis {
            Ereignis::Keins => neu = false,
            Ereignis::Schliessen => break,
            Ereignis::Groesse { breite, hoehe } => {
                fenster.groesse_uebernehmen(breite, hoehe);
                b.einpassen(breite as i32, hoehe as i32);
            }
            Ereignis::Taste(z) => match z {
                'q' | 'Q' | '\x1b' => break,
                '+' => b.zoomen(true, fenster.breite() as i32 / 2, fenster.hoehe() as i32 / 2),
                '-' => b.zoomen(false, fenster.breite() as i32 / 2, fenster.hoehe() as i32 / 2),
                '0' => b.einpassen(fenster.breite() as i32, fenster.hoehe() as i32),
                '1' => {
                    b.zaehler = 1;
                    b.nenner = 1;
                    b.zentrieren(fenster.breite() as i32, fenster.hoehe() as i32);
                }
                'i' | 'I' => b.info = !b.info,
                _ => neu = false,
            },
            Ereignis::Sondertaste(t) => match t {
                SONDER_LINKS => b.ab_x += 32,
                SONDER_RECHTS => b.ab_x -= 32,
                SONDER_HOCH => b.ab_y += 32,
                SONDER_RUNTER => b.ab_y -= 32,
                SONDER_POS1 => b.einpassen(fenster.breite() as i32, fenster.hoehe() as i32),
                SONDER_ENDE => {
                    b.zaehler = 1;
                    b.nenner = 1;
                    b.zentrieren(fenster.breite() as i32, fenster.hoehe() as i32);
                }
                _ => neu = false,
            },
            Ereignis::MausRad { x, y, delta } => b.zoomen(delta > 0, x, y),
            Ereignis::MausAb { x, y, knopf } if knopf == KNOPF_LINKS => {
                b.zieht = Some((x, y));
                neu = false;
            }
            Ereignis::MausAuf { .. } => {
                b.zieht = None;
                neu = false;
            }
            Ereignis::MausBewegt { x, y } => match b.zieht {
                Some((vx, vy)) => {
                    b.ab_x += x - vx;
                    b.ab_y += y - vy;
                    b.zieht = Some((x, y));
                }
                None => neu = false,
            },
            _ => neu = false,
        }

        if neu {
            titel_setzen(&fenster, &b);
            b.zeichnen(&mut fenster);
            let _ = fenster.zeigen();
        }
    }

    if war_fehler {
        1
    } else {
        0
    }
}

/// Titel = Dateiname (ohne Pfad) — der Kernel besitzt die Titelleiste, wir
/// liefern nur den Text.
fn titel_setzen(f: &Fenster, b: &Betrachter) {
    let name = match b.pfad.iter().rposition(|&c| c == b'/') {
        Some(i) => &b.pfad[i + 1..],
        None => &b.pfad[..],
    };
    if let Ok(s) = core::str::from_utf8(name) {
        let _ = f.titel_setzen(s);
    }
}

hauptprogramm!(haupt);
