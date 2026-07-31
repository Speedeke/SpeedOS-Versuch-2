// uidemo — DAS WIDGET-TOOLKIT IN EINEM RING-3-PROZESS
//          (Serie 8, Teil 2: der Beweis der Trennung)
//
// ==========================================================================
// WAS HIER PASSIERT
//
// Dieselben Widgets, die im Kernel den Explorer und die Einstellungen
// bauen, laufen hier in einem unprivilegierten Prozess — in einem eigenen
// Adressraum, ohne eine Zeile Kernel-Code, ohne `speed_os` als
// Abhaengigkeit. Der Prozess ist der ZWEITE WIRT von `speedui`:
//
//   * `DemoThema`   liefert feste Farben (kein Theme-System, keine
//                   Einstellungen, keine Atomics).
//   * `DemoSchrift` liefert die Masse des 5x7-Rasters aus
//                   `libspeed::fenster` — der Kernel-Font bleibt im
//                   Kernel (es gibt keinen Schrift-Syscall).
//   * `DemoUhr`     liefert `zeit_jetzt() * 1000` (Syscall 5).
//   * `PixelLeinwand` malt in den eigenen Pixelpuffer.
//
// Dass dieselben Widgets mit so verschiedenen Wirten laufen, IST die
// Aussage dieses Programms. Es sieht anders aus als der Kernel-Desktop,
// und das ist ehrlich so: Die Schrift ist eine andere.
//
// ==========================================================================
// UND DIE TEIL-RECHTECKE GEHEN UEBER DIE PROZESSGRENZE
//
// Die Schadensmeldungen der Widgets (`UiReaktion.schaden`, seit Serie 4)
// werden hier zu `fenster_zeichnen`-Rechtecken (Serie 8, Teil 1). Ein
// Klick auf einen Knopf uebertraegt deshalb ~2 KiB statt der ganzen
// Fensterflaeche — dieselbe Mechanik wie im Kernel, nur dass dazwischen
// ein Syscall liegt.
//
//     starte uidemo &

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use libspeed::fenster::{Ereignis, Fenster};
use libspeed::{println, Argumente};
use speedui::{
    hbox, vbox, w, widgets, Farbe, Farbrolle, Icon, Leinwand, Mass, Rechteck, Schrift, Taste,
    Thema, Uhr, UiEreignis, UiFenster, UiKontext, UiReaktion,
};

libspeed::hauptprogramm!(haupt);

// ---------------------------------------------------------------------------
// (1) DAS THEMA — feste Farben, kein Theme-System
// ---------------------------------------------------------------------------

struct DemoThema;

impl Thema for DemoThema {
    fn farbe(&self, rolle: Farbrolle) -> Farbe {
        match rolle {
            Farbrolle::Flaeche => Farbe::neu(0x1a, 0x1e, 0x2e),
            Farbrolle::InhaltHintergrund => Farbe::neu(0x12, 0x14, 0x22),
            Farbrolle::Rahmen => Farbe::neu(0x3a, 0x40, 0x58),
            Farbrolle::Akzent => Farbe::neu(0x7c, 0x3a, 0xed),
            Farbrolle::Auswahl => Farbe::neu(0x3b, 0x2a, 0x6e),
            Farbrolle::Eingabefeld => Farbe::neu(0x20, 0x24, 0x38),
            Farbrolle::KnopfFlaeche => Farbe::neu(0x26, 0x2b, 0x40),
            Farbrolle::KnopfAktiv => Farbe::neu(0x34, 0x3b, 0x56),
            Farbrolle::TextStark => Farbe::neu(0xf0, 0xf4, 0xff),
            Farbrolle::TextNormal => Farbe::neu(0xc8, 0xd0, 0xe4),
            Farbrolle::TextSekundaer => Farbe::neu(0x94, 0xa0, 0xbc),
            Farbrolle::TextGedimmt => Farbe::neu(0x60, 0x6a, 0x84),
            Farbrolle::TextAufAkzent => Farbe::neu(0xff, 0xff, 0xff),
        }
    }

    fn mass(&self, mass: Mass) -> i32 {
        match mass {
            Mass::Abstand => 6,
            Mass::UiRand => 8,
            Mass::ElementHoehe => 22,
            Mass::ListenEintragHoehe => 18,
            Mass::ScrollbalkenBreite => 8,
            Mass::RadiusKlein => 4,
            // Die 5x7-Schrift wird mit Faktor 1 gezeichnet — „Groesse 8"
            // ist hier eine Zeilenhoehe, keine Punktgroesse.
            Mass::SchriftUi => 8,
            Mass::ZeilenHoehe => 10,
            Mass::CursorBlinkUs => 500_000,
        }
    }
}

// ---------------------------------------------------------------------------
// (2) DIE SCHRIFT — das 5x7-Raster aus libspeed
// ---------------------------------------------------------------------------

/// DIE EHRLICHE FOLGE DER TRENNUNG: Ein Prozess bekommt die
/// vorgerasterten Kernel-Schriften nicht (es gibt keinen Syscall dafuer,
/// siehe docs/grenzen.md). Also bringt er seine eigene mit — hier das
/// 5x7-Raster, das `libspeed::fenster` fuer `fenstertest` schon hatte.
/// Ein Zeichen ist 5 Pixel breit plus 1 Pixel Abstand.
struct DemoSchrift;

const ZEICHEN_BREITE: i32 = 6;
const ZEICHEN_HOEHE: i32 = 7;

impl Schrift for DemoSchrift {
    fn zeichen_breite(&self, _groesse: i32) -> i32 {
        ZEICHEN_BREITE
    }
    fn zeilen_hoehe(&self, _groesse: i32) -> i32 {
        ZEICHEN_HOEHE + 3
    }
}

// ---------------------------------------------------------------------------
// (3) DIE UHR — der Zeit-Syscall
// ---------------------------------------------------------------------------

struct DemoUhr;

impl Uhr for DemoUhr {
    fn us(&self) -> u64 {
        // `zeit_jetzt` liefert Millisekunden; das Toolkit will
        // Mikrosekunden. Die groebere Aufloesung reicht: Sie steuert nur
        // Cursor-Blinken (500 ms) und Doppelklick (500 ms).
        libspeed::zeit_jetzt().saturating_mul(1000)
    }
}

// ---------------------------------------------------------------------------
// (4) DIE LEINWAND — der eigene Pixelpuffer
// ---------------------------------------------------------------------------

/// Alle neun Operationen des `Leinwand`-Traits auf dem Prozess-eigenen
/// Puffer. Was der Kernel mit Bresenham und Zeilen-Schnellpfaden macht,
/// steht hier schlicht und einfach da — auch das ist eine Aussage der
/// Trennung: Der Wirt entscheidet, wie gut er malt.
struct PixelLeinwand<'a> {
    f: &'a mut Fenster,
    clip: Option<Rechteck>,
}

impl<'a> PixelLeinwand<'a> {
    fn neu(f: &'a mut Fenster) -> Self {
        PixelLeinwand { f, clip: None }
    }

    /// Ein Rechteck auf das Clip schneiden (None = nichts sichtbar).
    fn sichtbar(&self, r: Rechteck) -> Option<Rechteck> {
        match self.clip {
            Some(c) => r.schneiden(&c),
            None => Some(r),
        }
    }

    fn farbwert(farbe: Farbe) -> u32 {
        Fenster::farbe(farbe.r, farbe.g, farbe.b)
    }
}

impl Leinwand for PixelLeinwand<'_> {
    fn masse(&self) -> (i32, i32) {
        (self.f.breite() as i32, self.f.hoehe() as i32)
    }
    fn clip(&self) -> Option<Rechteck> {
        self.clip
    }
    fn clip_setzen(&mut self, clip: Option<Rechteck>) {
        self.clip = clip;
    }
    fn fuellen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        if let Some(r) = self.sichtbar(rechteck) {
            self.f
                .rechteck(r.x, r.y, r.breite, r.hoehe, Self::farbwert(farbe));
        }
    }
    fn abgerundet(&mut self, rechteck: Rechteck, radius: i32, farbe: Farbe) {
        // Runde Ecken sparen wir uns: Das Toolkit verlangt nur, DASS
        // gefuellt wird — WIE gut, entscheidet der Wirt. Die Ecken werden
        // um `radius` eingerueckt, das reicht als Andeutung.
        let r = radius.min(rechteck.breite / 2).min(rechteck.hoehe / 2);
        self.fuellen(
            Rechteck::neu(rechteck.x + r, rechteck.y, rechteck.breite - 2 * r, rechteck.hoehe),
            farbe,
        );
        self.fuellen(
            Rechteck::neu(rechteck.x, rechteck.y + r, rechteck.breite, rechteck.hoehe - 2 * r),
            farbe,
        );
    }
    fn rahmen(&mut self, rechteck: Rechteck, farbe: Farbe) {
        self.fuellen(Rechteck::neu(rechteck.x, rechteck.y, rechteck.breite, 1), farbe);
        self.fuellen(
            Rechteck::neu(rechteck.x, rechteck.y + rechteck.hoehe - 1, rechteck.breite, 1),
            farbe,
        );
        self.fuellen(Rechteck::neu(rechteck.x, rechteck.y, 1, rechteck.hoehe), farbe);
        self.fuellen(
            Rechteck::neu(rechteck.x + rechteck.breite - 1, rechteck.y, 1, rechteck.hoehe),
            farbe,
        );
    }
    fn linie(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, farbe: Farbe) {
        // Nur waagerecht/senkrecht — mehr braucht kein Widget (Trennlinie
        // und Checkbox-Haken; letzterer wird dadurch eckig).
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        self.fuellen(
            Rechteck::neu(x0, y0, (x1 - x0 + 1).max(1), (y1 - y0 + 1).max(1)),
            farbe,
        );
    }
    fn text(&mut self, x: i32, y: i32, text: &str, _groesse: i32, _fett: bool, farbe: Farbe) {
        // Geclippt zeichnen: Das `text` von libspeed kann das nicht, also
        // wird ZEICHENWEISE geprueft (ein Zeichen ist 6x7 Pixel).
        let farbwert = Self::farbwert(farbe);
        for (i, zeichen) in text.chars().enumerate() {
            let zx = x + i as i32 * ZEICHEN_BREITE;
            let kaesten = Rechteck::neu(zx, y, ZEICHEN_BREITE, ZEICHEN_HOEHE);
            if self.sichtbar(kaesten).is_none() {
                continue;
            }
            let mut eins = String::new();
            eins.push(zeichen);
            self.f.text(zx, y, &eins, farbwert, 1);
        }
    }
    fn icon(&mut self, x: i32, y: i32, icon: &Icon, skalierung: i32) {
        // Icons zeichnet der Prozess selbst — mit DERSELBEN Palette wie
        // der Kernel (`speedui::icon_farbe`), damit dasselbe Icon nicht in
        // zwei Wirten verschieden aussieht.
        for (zeile, muster) in icon.zeilen.iter().enumerate() {
            for (spalte, zeichen) in muster.chars().enumerate() {
                let Some(farbe) = speedui::icon_farbe(zeichen) else {
                    continue;
                };
                self.fuellen(
                    Rechteck::neu(
                        x + spalte as i32 * skalierung,
                        y + zeile as i32 * skalierung,
                        skalierung,
                        skalierung,
                    ),
                    farbe,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Die Nachrichten der Demo
// ---------------------------------------------------------------------------

const N_KNOPF_A: u32 = 1;
const N_KNOPF_B: u32 = 2;
const N_HAKEN: u32 = 3;
const N_FELD: u32 = 4;
const N_LISTE: u32 = 1000; // + Index

fn haupt(_argumente: &Argumente) -> i32 {
    let mut f = match // KEIN Gedankenstrich: Die Titelleiste zeichnet der Kernel mit
    // seiner Latin-1-Schrift, und ein "—" wird dort zu "?".
    Fenster::oeffnen("uidemo - speedui in Ring 3", 460, 320) {
        Ok(f) => f,
        Err(fehler) => {
            println!("Fenster liess sich nicht oeffnen: {}", fehler.text());
            if fehler == libspeed::Fehler::NICHT_KONFIGURIERT {
                println!("Es laeuft kein Desktop — mit dem Befehl 'desktop' starten.");
            }
            return 3;
        }
    };
    println!("uidemo: {}x{} — dieselben Widgets wie im Kernel.", f.breite(), f.hoehe());

    let thema = DemoThema;
    let schrift = DemoSchrift;
    let uhr = DemoUhr;
    let k = UiKontext::neu(&thema, &schrift, &uhr);

    let mut zaehler = 0u32;
    let mut ui = UiFenster::neu(baum(zaehler), |_| {}, &LEER_ICON);
    ui.fokus_initial();

    // Erster Aufbau: alles.
    voll_zeichnen(&mut f, &ui, &k);

    loop {
        let masse = (f.breite() as i32, f.hoehe() as i32);
        let ereignis = match f.naechstes_ereignis(200) {
            Ok(e) => e,
            Err(_) => return 0, // Fenster weg
        };

        // Fenster-Ereignis -> UiEreignis. DAS ist die Uebersetzung, die
        // im Kernel der Fenster-Manager macht.
        let ui_ereignis = match ereignis {
            Ereignis::Keins => continue,
            Ereignis::Schliessen => {
                println!("uidemo: Tschuess.");
                return 0;
            }
            Ereignis::Groesse { breite, hoehe } => {
                f.groesse_uebernehmen(breite, hoehe);
                voll_zeichnen(&mut f, &ui, &k);
                continue;
            }
            Ereignis::Fokus(_) => continue,
            Ereignis::MausAb { x, y, .. } => UiEreignis::Klick { x, y },
            Ereignis::MausAuf { x, y, .. } => UiEreignis::Losgelassen { x, y },
            Ereignis::MausBewegt { x, y } => UiEreignis::Bewegt { x, y },
            Ereignis::MausRad { x, y, delta } => UiEreignis::Scroll {
                delta: delta as i8,
                x,
                y,
            },
            Ereignis::Taste(zeichen) => UiEreignis::Taste(Taste::Zeichen(zeichen)),
            Ereignis::Sondertaste(code) => match sondertaste(code) {
                Some(taste) => UiEreignis::Taste(taste),
                None => continue,
            },
        };

        let reaktion = ui.maus_oder_taste(ui_ereignis, masse, &k);

        // Nachricht verarbeiten -> Baum neu bauen (das App-Muster).
        if let Some(id) = reaktion.nachricht {
            if matches!(id, N_KNOPF_A | N_KNOPF_B | N_HAKEN | N_FELD) || id >= N_LISTE {
                zaehler += 1;
                ui.wurzel_setzen(baum(zaehler));
                ui.fokus_initial();
                voll_zeichnen(&mut f, &ui, &k);
                continue;
            }
        }

        // ==================================================================
        // HIER GEHT DIE TEIL-RECHTECK-MECHANIK UEBER DIE PROZESSGRENZE:
        // Das Widget hat gemeldet, WELCHE Flaeche sich geaendert hat. Statt
        // das ganze Fenster zu senden, wird genau dieser Streifen gezeichnet
        // UND genau dieser Streifen per Syscall uebertragen.
        // ==================================================================
        if reaktion.neu_zeichnen {
            match reaktion.schaden {
                Some(schaden) => {
                    {
                        let mut leinwand = PixelLeinwand::neu(&mut f);
                        ui.zeichnen_bereich(&mut leinwand, schaden, &k);
                    }
                    let _ = f.zeigen_bereich(
                        schaden.x.max(0) as usize,
                        schaden.y.max(0) as usize,
                        schaden.breite.max(0) as usize,
                        schaden.hoehe.max(0) as usize,
                    );
                }
                None => voll_zeichnen(&mut f, &ui, &k),
            }
        }
    }
}

/// Zeichnet den ganzen Baum und uebertraegt das ganze Fenster.
fn voll_zeichnen(f: &mut Fenster, ui: &UiFenster, k: &UiKontext) {
    {
        let mut leinwand = PixelLeinwand::neu(f);
        ui.zeichnen(&mut leinwand, k);
    }
    let _ = f.zeigen();
}

/// Der Widget-Baum — die Galerie aus Serie 3, unveraendert im Aufbau.
fn baum(zaehler: u32) -> alloc::boxed::Box<dyn speedui::Widget> {
    let eintraege = (0..12)
        .map(|i| widgets::ListenEintrag {
            icon: None,
            text: zahl_text("Eintrag ", i),
        })
        .collect();

    w(vbox(vec![
        w(widgets::Label::neu("SPEEDUI IN RING 3")),
        w(widgets::Label::sekundaer(&zahl_text(
            "DIESELBEN WIDGETS WIE IM KERNEL - EREIGNISSE: ",
            zaehler,
        ))),
        w(widgets::Trennlinie),
        w(hbox(vec![
            w(widgets::Button::neu("KNOPF A", N_KNOPF_A)),
            w(widgets::Button::neu("KNOPF B", N_KNOPF_B)),
            w(speedui::Fueller),
        ])),
        w(widgets::Checkbox::neu("EIN HAKEN", zaehler % 2 == 1, N_HAKEN)),
        w(widgets::Textfeld::neu(N_FELD)),
        w(widgets::ScrollListe::mit_index_nachrichten(
            eintraege, N_LISTE, N_LISTE,
        )),
    ]))
}

/// Ein leeres Icon — `UiFenster` verlangt eines, und der Prozess malt
/// seine Titelleiste ohnehin nicht selbst (das tut der Kernel).
static LEER_ICON: Icon = Icon {
    zeilen: [
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
        "................",
    ],
};

/// ABI-Sondertaste -> Toolkit-Taste (die zweite Haelfte der Uebersetzung,
/// die im Kernel `ui::taste_von` macht).
fn sondertaste(code: i32) -> Option<Taste> {
    use libspeed::fenster as fw;
    Some(match code {
        fw::SONDER_HOCH => Taste::Hoch,
        fw::SONDER_RUNTER => Taste::Runter,
        fw::SONDER_LINKS => Taste::Links,
        fw::SONDER_RECHTS => Taste::Rechts,
        fw::SONDER_POS1 => Taste::Pos1,
        fw::SONDER_ENDE => Taste::Ende,
        fw::SONDER_BILD_HOCH => Taste::BildHoch,
        fw::SONDER_BILD_RUNTER => Taste::BildRunter,
        fw::SONDER_ENTF => Taste::Entf,
        _ => return None,
    })
}

fn zahl_text(vorne: &str, zahl: u32) -> String {
    let mut aus = String::from(vorne);
    let mut ziffern = [0u8; 12];
    let mut i = 12;
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
    aus
}

/// Kleine Hilfe: Maus- und Tasten-Ereignisse gehen an verschiedene
/// UiFenster-Methoden — hier zusammengefasst, damit die Schleife oben
/// lesbar bleibt.
trait EreignisWeiche {
    fn maus_oder_taste(
        &mut self,
        ereignis: UiEreignis,
        masse: (i32, i32),
        k: &UiKontext,
    ) -> UiReaktion;
}

impl EreignisWeiche for UiFenster {
    fn maus_oder_taste(
        &mut self,
        ereignis: UiEreignis,
        masse: (i32, i32),
        k: &UiKontext,
    ) -> UiReaktion {
        match ereignis {
            UiEreignis::Taste(taste) => self.taste(taste, masse, k),
            andere => self.maus(andere, masse, k),
        }
    }
}
