// einstellungen.rs — Persistente System-Einstellungen + Einstellungen-App
//
// ZWEI Dinge leben hier:
//
//   1. DER EINSTELLUNGS-SPEICHER: ein globaler Schlüssel=Wert-Store
//      mit typisiertem Zugriff (hole_zahl/hole_bool/hole_text und die
//      setze_*-Gegenstücke). Er wird beim Boot aus dem VFS geladen
//      (/system/einstellungen.txt, simples Schlüssel=Wert-Format) und
//      bei JEDER Änderung sofort zurückgeschrieben. Das ist die
//      API-NAHT für Serie 4: Wenn das Disk-Dateisystem kommt, wird
//      nur das gemountete VFS ausgetauscht — kein Aufrufer ändert
//      sich, die Einstellungen überleben dann echte Neustarts.
//      (Heute liegt darunter das RamFs: Die Werte überleben das
//      Schließen/Öffnen der App und den Desktop-Modus, aber noch
//      keinen Reboot — das ist die bekannte RamFs-Grenze.)
//
//   2. DIE EINSTELLUNGEN-APP (ui::App): Kategorien-Navigation links
//      (ScrollListe), Inhaltsbereich rechts — das Zuhause aller
//      künftigen Optionen. Alle Optionen wirken SOFORT (Theme/Akzent/
//      Hintergrund/Skalierung über die lock-freien theme-Atomics,
//      Neuzeichnen über AppReaktion.danach -> fenster::
//      alles_neu_zeichnen) und werden sofort persistiert.
//
// LOCK-REGEL: Der SPEICHER-Mutex ist ein BLATT-Lock (wie ablage.rs) —
// er darf unter dem MANAGER-Lock (App::nachricht) genommen werden.
// speichern() ruft fs::mit_fs NACH dem Loslassen des Speicher-Locks
// (nie verschachtelt, und fs unter dem MANAGER-Lock ist erlaubt).
//
// ZEITZONEN-ANNAHME (dokumentiert, weil sie leicht vergessen wird):
// Die RTC liefert in QEMU die HOST-LOKALZEIT (der boot/-Runner
// startet mit `-rtc base=localtime`). zeit::jetzt() ist also schon
// Lokalzeit — der UTC-Offset hier ist eine reine ANZEIGE-Verschiebung
// relativ zur RTC-Zeit, Standard 00:00 = unverändert. Echte
// Zeitzonen-Logik (RTC in UTC + Zonendatenbank) kommt erst mit
// echter Hardware-Erfahrung.

use crate::fs;
use crate::zeit::{self, DatumUhrzeit};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

/// Die Einstellungs-Datei im VFS.
pub const PFAD: &str = "/system/einstellungen.txt";

// ----- Die Schlüssel (zentral, damit sich niemand vertippt) -----
pub const S_THEME_HELL: &str = "theme.hell";
pub const S_AKZENT: &str = "theme.akzent";
pub const S_HINTERGRUND: &str = "desktop.hintergrund";
pub const S_SKALA: &str = "ui.skala";
pub const S_BLINK_MS: &str = "cursor.blink_ms";
pub const S_FORMAT24: &str = "zeit.format24";
pub const S_UTC_OFFSET: &str = "zeit.utc_offset_min";

// ---------------------------------------------------------------------------
// Der Speicher: BTreeMap hinter einem Blatt-Lock
// ---------------------------------------------------------------------------

static SPEICHER: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Blatt-Lock-Zugriff (Interrupts aus, wie bei der Ablage) — die
/// Closure darf KEINE anderen Locks nehmen.
fn mit_werten<T>(f: impl FnOnce(&mut BTreeMap<String, String>) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut SPEICHER.lock()))
}

// ---------------------------------------------------------------------------
// Format: eine Zeile pro Einstellung, `schluessel=wert`.
// Leerzeilen und #-Kommentare sind erlaubt, Unbekanntes wird beim
// Laden schlicht ignoriert (robust gegen Handedits und alte Dateien).
// Reine Funktionen — unit-getestet, ganz ohne VFS.
// ---------------------------------------------------------------------------

/// Parst den Dateiinhalt in eine Schlüssel=Wert-Tabelle.
pub fn parsen(text: &str) -> BTreeMap<String, String> {
    let mut werte = BTreeMap::new();
    for zeile in text.lines() {
        let zeile = zeile.trim();
        if zeile.is_empty() || zeile.starts_with('#') {
            continue;
        }
        // Nur am ERSTEN '=' trennen — der Wert darf selbst '=' tragen.
        if let Some((schluessel, wert)) = zeile.split_once('=') {
            let schluessel = schluessel.trim();
            if !schluessel.is_empty() {
                werte.insert(String::from(schluessel), String::from(wert.trim()));
            }
        }
        // Zeilen ohne '=' werden ignoriert (kein Abbruch beim Laden).
    }
    werte
}

/// Die Umkehrung: Tabelle -> Dateiinhalt (sortiert, da BTreeMap).
pub fn serialisieren(werte: &BTreeMap<String, String>) -> String {
    let mut text = String::from("# SpeedOS-Einstellungen (schluessel=wert)\n");
    for (schluessel, wert) in werte {
        text.push_str(schluessel);
        text.push('=');
        text.push_str(wert);
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// Laden / Speichern über das VFS
// ---------------------------------------------------------------------------

/// Lädt die Einstellungen aus dem VFS (beim Boot, nach fs::init) und
/// wendet sie an. Fehlt die Datei, gelten die Standardwerte.
pub fn laden() {
    let text = fs::mit_fs(|f| f.lesen(PFAD))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let werte = parsen(&text);
    let anzahl = werte.len();
    mit_werten(|speicher| *speicher = werte);
    anwenden();
    crate::serial_println!("[EINSTELLUNGEN] {} Wert(e) aus {} geladen.", anzahl, PFAD);
}

/// Schreibt den kompletten Stand zurück ins VFS (nach jeder Änderung —
/// die Datei ist winzig, das ist billiger als jede Schlau-Logik).
/// Danach fs::sync(): Einstellungen sollen auch dann auf dem Medium
/// sein, wenn unter dem VFS mal ein Disk-FS mit Schreib-Cache liegt
/// (heute, beim RamFs, ist sync ein No-Op).
pub fn speichern() {
    let text = mit_werten(|werte| serialisieren(werte));
    let _ = fs::mit_fs(|f| f.mkdir("/system")); // existiert normalerweise
    if let Err(fehler) =
        fs::mit_fs(|f| f.schreiben(PFAD, text.as_bytes())).and_then(|()| fs::sync())
    {
        crate::serial_println!("[EINSTELLUNGEN] Speichern fehlgeschlagen: {}", fehler.meldung());
    }
}

/// Wendet die geladenen Werte auf die lock-freien theme-Atomics an
/// (Hell/Dunkel, Akzent, Hintergrund). Die UI-Skalierung wendet
/// fenster::desktop_starten selbst an — sie hängt von der Auflösung
/// ab (Auto-Wahl, wenn kein Wert gespeichert ist).
fn anwenden() {
    crate::theme::hell_setzen(hole_bool(S_THEME_HELL, false));
    crate::theme::akzent_setzen(hole_zahl(S_AKZENT, 0).max(0) as usize);
    crate::theme::hintergrund_setzen(hole_zahl(S_HINTERGRUND, 0).max(0) as usize);
}

// ---------------------------------------------------------------------------
// Typisierter Zugriff — DIE API für alle Aufrufer
// ---------------------------------------------------------------------------

/// Roher Wert (None = nicht gesetzt).
pub fn hole_opt(schluessel: &str) -> Option<String> {
    mit_werten(|werte| werte.get(schluessel).cloned())
}

pub fn hole_text(schluessel: &str, standard: &str) -> String {
    hole_opt(schluessel).unwrap_or_else(|| String::from(standard))
}

/// Zahl (i64); nicht gesetzt ODER unparsbar -> Standard.
pub fn hole_zahl(schluessel: &str, standard: i64) -> i64 {
    hole_opt(schluessel)
        .and_then(|wert| wert.parse().ok())
        .unwrap_or(standard)
}

/// Bool: gespeichert als "1"/"0".
pub fn hole_bool(schluessel: &str, standard: bool) -> bool {
    match hole_opt(schluessel).as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => standard,
    }
}

/// Setzt einen Wert und speichert SOFORT (siehe speichern()).
pub fn setze_text(schluessel: &str, wert: &str) {
    mit_werten(|werte| {
        werte.insert(String::from(schluessel), String::from(wert));
    });
    speichern();
}

pub fn setze_zahl(schluessel: &str, wert: i64) {
    setze_text(schluessel, &format!("{}", wert));
}

pub fn setze_bool(schluessel: &str, wert: bool) {
    setze_text(schluessel, if wert { "1" } else { "0" });
}

// ---------------------------------------------------------------------------
// Abgeleitete Helfer für die Verbraucher (Systray-Uhr, Textfeld-Cursor)
// ---------------------------------------------------------------------------

/// Cursor-Blink-Halbperiode in Millisekunden (geklemmt — 0 würde
/// durch Null teilen, Riesenwerte sähen wie "kaputt" aus).
pub fn cursor_blink_ms() -> u64 {
    hole_zahl(S_BLINK_MS, 500).clamp(100, 2000) as u64
}

/// Dasselbe in Mikrosekunden (fürs Textfeld: us_seit_boot-Takt).
pub fn cursor_blink_us() -> u64 {
    cursor_blink_ms() * 1000
}

/// Der Anzeige-Offset zur RTC-Zeit in Minuten (siehe Kopfkommentar:
/// die RTC liefert bereits Lokalzeit, Standard ist deshalb 0).
pub fn utc_offset_min() -> i64 {
    hole_zahl(S_UTC_OFFSET, 0).clamp(-12 * 60, 14 * 60)
}

/// Die anzuzeigende Zeit: RTC/TSC-Zeit plus Offset.
pub fn jetzt_lokal() -> DatumUhrzeit {
    zeit_verschieben(&zeit::jetzt(), utc_offset_min())
}

/// Verschiebt einen Zeitpunkt um Minuten (reine Funktion — über die
/// Sekunden-seit-2000-Arithmetik, damit Tages-/Monatsgrenzen stimmen).
pub fn zeit_verschieben(datum: &DatumUhrzeit, minuten: i64) -> DatumUhrzeit {
    let sekunden = zeit::sekunden_seit_2000(datum) as i64 + minuten * 60;
    zeit::datum_von_sekunden_seit_2000(sekunden.max(0) as u64)
}

/// Formatiert die Uhrzeit im 24h- oder 12h-Format (reine Funktion).
pub fn uhrzeit_formatieren(datum: &DatumUhrzeit, format24: bool) -> String {
    if format24 {
        return format!("{:02}:{:02}:{:02}", datum.stunde, datum.minute, datum.sekunde);
    }
    let anzeige_stunde = match datum.stunde % 12 {
        0 => 12,
        stunde => stunde,
    };
    let suffix = if datum.stunde < 12 { "AM" } else { "PM" };
    format!("{:02}:{:02}:{:02} {}", anzeige_stunde, datum.minute, datum.sekunde, suffix)
}

/// Die Uhrzeit im EINGESTELLTEN Format (für Systray + Uhr-Seite).
pub fn uhrzeit_text(datum: &DatumUhrzeit) -> String {
    uhrzeit_formatieren(datum, hole_bool(S_FORMAT24, true))
}

/// Ein Datei-Zeitstempel (Sekunden seit 2000 aus fs::Metadaten bzw.
/// DirEintrag.geaendert) als LOKALER Anzeigetext "17.07.2026 14:32" —
/// mit demselben Anzeige-Offset wie die Systray-Uhr, damit dir und
/// Explorer dieselbe Zeit zeigen wie die Uhr unten rechts.
pub fn stempel_text(stempel: u64) -> String {
    let datum = zeit_verschieben(
        &zeit::datum_von_sekunden_seit_2000(stempel),
        utc_offset_min(),
    );
    format!(
        "{:02}.{:02}.{:04} {:02}:{:02}",
        datum.tag, datum.monat, datum.jahr, datum.stunde, datum.minute
    )
}

/// Offset als Anzeigetext: "UTC+02:00", "UTC-04:30".
pub fn offset_text(minuten: i64) -> String {
    let vorzeichen = if minuten < 0 { '-' } else { '+' };
    let betrag = minuten.unsigned_abs();
    format!("UTC{}{:02}:{:02}", vorzeichen, betrag / 60, betrag % 60)
}

// ===========================================================================
// Die Einstellungen-App — Kategorien links, Inhalt rechts
// ===========================================================================

use crate::grafik::{Icon, Rechteck, Rgba, Zeichner};
use crate::theme::{self, metrik};
use crate::ui::widgets::{Button, Checkbox, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::ui::{hbox, vbox, App, AppReaktion, Fueller, UiEreignis, UiReaktion, Widget};

// ----- Nachricht-IDs (Basen weit genug auseinander für die Indizes) -----
const N_NICHTS: u32 = 0; // Anzeige-Chips ohne Wirkung (Offset-Text)
const N_THEME_DUNKEL: u32 = 1;
const N_THEME_HELL: u32 = 2;
const N_SKALA_BASIS: u32 = 10; // +0/+1/+2 -> Halbe 2/3/4
const N_BLINK_BASIS: u32 = 20; // +0/+1/+2 -> langsam/normal/schnell
const N_OFFSET_MINUS: u32 = 30;
const N_OFFSET_PLUS: u32 = 31;
const N_FORMAT24: u32 = 32;
const N_AKZENT: u32 = 100; // + Palette-Index
const N_HINTERGRUND: u32 = 200; // + Preset-Index
const N_KATEGORIE: u32 = 10_000; // + Kategorie-Index

/// Die Blink-Auswahl (Beschriftung, Halbperiode in ms).
const BLINK_STUFEN: [(&str, i64); 3] = [("Langsam", 800), ("Normal", 500), ("Schnell", 250)];

const KAT_PERSONALISIERUNG: usize = 0;
const KAT_ANZEIGE: usize = 1;
const KAT_ZEIT: usize = 2;
const KAT_INFO: usize = 3;
const KATEGORIEN: [(&str, &Icon); 4] = [
    ("Personalisierung", &crate::grafik::ICON_THEME),
    ("Anzeige", &crate::grafik::ICON_ZAHNRAD),
    ("Datum & Uhrzeit", &crate::grafik::ICON_UHR),
    ("Info", &crate::grafik::ICON_LOGO),
];

pub struct EinstellungenApp {
    kategorie: usize,
    /// Beim App-Start EINMAL erfasst: mit_framebuffer darf NICHT
    /// unter dem MANAGER-Lock laufen (Lock-Ordnung FRAMEBUFFER ->
    /// MANAGER!), aufbau()/tick() aber schon — deshalb der Cache.
    /// Die Auflösung ändert sich zur Laufzeit sowieso nie.
    aufloesung: (usize, usize),
    /// Live-Seiten (Uhr, Info) nur beim Sekundenwechsel neu bauen —
    /// der geteilte Toolkit-Baustein (Serie-3-Review).
    sekunden_tick: crate::ui::app::SekundenTick,
}

impl EinstellungenApp {
    pub fn neu() -> Self {
        let aufloesung = crate::framebuffer::mit_framebuffer(|fb| {
            let info = fb.info();
            (info.width, info.height)
        })
        .unwrap_or((0, 0));
        EinstellungenApp {
            kategorie: KAT_PERSONALISIERUNG,
            aufloesung,
            sekunden_tick: crate::ui::app::SekundenTick::neu(),
        }
    }

    /// Reaktion für Optionen, die den GANZEN Desktop umfärben:
    /// eigenen Baum neu bauen UND (nach dem Lock!) alle Fenster,
    /// Hintergrund-Cache und Taskleiste neu zeichnen lassen.
    fn desktop_neu_zeichnen() -> AppReaktion {
        AppReaktion::neu_aufbauen().mit_danach(crate::fenster::alles_neu_zeichnen)
    }

    // ----- Die vier Inhaltsseiten (rein aus dem Zustand gebaut) -----

    fn seite_personalisierung(&self) -> Vec<Box<dyn Widget>> {
        let hell = theme::hell_aktiv();
        let akzent_index = theme::akzent_index();
        let hintergrund_index = theme::hintergrund_index();

        // Akzent-Palette als Farbfelder:
        let mut felder: Vec<Box<dyn Widget>> = theme::AKZENTE
            .iter()
            .enumerate()
            .map(|(i, akzent)| {
                Box::new(FarbFeld {
                    farbe: if hell { akzent.hell } else { akzent.dunkel },
                    aktiv: i == akzent_index,
                    nachricht: N_AKZENT + i as u32,
                }) as Box<dyn Widget>
            })
            .collect();
        felder.push(Box::new(Fueller));

        let presets = theme::HINTERGRUENDE
            .iter()
            .map(|preset| ListenEintrag { icon: Some(&crate::grafik::ICON_THEME), text: String::from(preset.name) })
            .collect();

        vec![
            Box::new(Label::neu("Theme")) as Box<dyn Widget>,
            Box::new(hbox(vec![
                Box::new(Button::neu("Aurora Dunkel", N_THEME_DUNKEL).mit_aktiv(!hell)) as Box<dyn Widget>,
                Box::new(Button::neu("Aurora Hell", N_THEME_HELL).mit_aktiv(hell)),
                Box::new(Fueller),
            ])),
            Box::new(Trennlinie),
            Box::new(Label::neu("Akzentfarbe")),
            Box::new(Label::sekundaer(&format!(
                "Gilt in Hell UND Dunkel - aktuell: {}",
                theme::AKZENTE[akzent_index].name
            ))),
            Box::new(hbox(felder)),
            Box::new(Trennlinie),
            Box::new(Label::neu("Desktop-Hintergrund")),
            Box::new(
                ScrollListe::mit_index_nachrichten(presets, N_HINTERGRUND, N_HINTERGRUND)
                    .mit_auswahl(Some(hintergrund_index)),
            ),
        ]
    }

    fn seite_anzeige(&self) -> Vec<Box<dyn Widget>> {
        let halbe = theme::skala_halbe();
        let blink_ms = hole_zahl(S_BLINK_MS, 500);

        let blink_knoepfe: Vec<Box<dyn Widget>> = BLINK_STUFEN
            .iter()
            .enumerate()
            .map(|(i, (name, ms))| {
                Box::new(Button::neu(name, N_BLINK_BASIS + i as u32).mit_aktiv(blink_ms == *ms))
                    as Box<dyn Widget>
            })
            .chain(core::iter::once(Box::new(Fueller) as Box<dyn Widget>))
            .collect();

        vec![
            Box::new(Label::neu("Aufloesung")) as Box<dyn Widget>,
            Box::new(Label::neu(&format!("{} x {} Pixel", self.aufloesung.0, self.aufloesung.1))),
            Box::new(Label::sekundaer(
                "Nur Anzeige: Die Aufloesung waehlt der Bootloader beim\n\
                 Start. Wunsch per Umgebungsvariable SPEEDOS_AUFLOESUNG\n\
                 (720p, 1080p, 2k, 4k oder BREITExHOEHE) vor cargo run.",
            )),
            Box::new(Trennlinie),
            Box::new(Label::neu("UI-Skalierung")),
            Box::new(hbox(vec![
                Box::new(Button::neu("1.0", N_SKALA_BASIS).mit_aktiv(halbe == 2)) as Box<dyn Widget>,
                Box::new(Button::neu("1.5", N_SKALA_BASIS + 1).mit_aktiv(halbe == 3)),
                Box::new(Button::neu("2.0", N_SKALA_BASIS + 2).mit_aktiv(halbe == 4)),
                Box::new(Fueller),
            ])),
            Box::new(Trennlinie),
            Box::new(Label::neu("Cursor-Blinkgeschwindigkeit")),
            Box::new(hbox(blink_knoepfe)),
        ]
    }

    fn seite_zeit(&self) -> Vec<Box<dyn Widget>> {
        let jetzt = jetzt_lokal();
        let offset = utc_offset_min();
        let format24 = hole_bool(S_FORMAT24, true);

        vec![
            Box::new(Label::neu("Aktuelle Zeit")) as Box<dyn Widget>,
            Box::new(Label::neu(&format!(
                "{}   {:02}.{:02}.{}",
                uhrzeit_formatieren(&jetzt, format24),
                jetzt.tag,
                jetzt.monat,
                jetzt.jahr
            ))),
            Box::new(Trennlinie),
            Box::new(Label::neu("Zeitzone (Anzeige-Offset)")),
            Box::new(hbox(vec![
                Box::new(Button::neu("-", N_OFFSET_MINUS)) as Box<dyn Widget>,
                Box::new(Button::neu(&offset_text(offset), N_NICHTS)),
                Box::new(Button::neu("+", N_OFFSET_PLUS)),
                Box::new(Fueller),
            ])),
            Box::new(Label::sekundaer(
                "Annahme: Die RTC liefert in QEMU bereits die Host-\n\
                 LOKALZEIT (Runner: -rtc base=localtime). Der Offset\n\
                 verschiebt nur die ANZEIGE - 00:00 = RTC-Zeit pur.",
            )),
            Box::new(Trennlinie),
            Box::new(Checkbox::neu("24-Stunden-Format (Systray-Uhr)", format24, N_FORMAT24)),
        ]
    }

    fn seite_info(&self) -> Vec<Box<dyn Widget>> {
        use crate::explorer::groesse_formatieren;

        let (frames_frei, frames_gesamt) = crate::memory::frame_statistik();
        let tsc_hz = zeit::tsc_frequenz_hz();
        let sekunden = zeit::ms_seit_boot() / 1000;
        let zeilen = format!(
            "Aufloesung:   {} x {} Pixel\n\
             Speicher:     {} frei von {}\n\
             TSC-Frequenz: {},{:03} MHz\n\
             Uptime:       {:02}:{:02}:{:02}\n\
             Tasks:        {}",
            self.aufloesung.0,
            self.aufloesung.1,
            groesse_formatieren(frames_frei * 4096),
            groesse_formatieren(frames_gesamt * 4096),
            tsc_hz / 1_000_000,
            (tsc_hz / 1000) % 1000,
            sekunden / 3600,
            (sekunden / 60) % 60,
            sekunden % 60,
            crate::task::executor::task_anzahl(),
        );

        vec![
            Box::new(hbox(vec![
                Box::new(IconBild { icon: &crate::grafik::ICON_LOGO, skala: 3 }) as Box<dyn Widget>,
                Box::new(vbox(vec![
                    Box::new(Label::neu("SpeedOS")) as Box<dyn Widget>,
                    // Die Version kommt zur COMPILE-ZEIT aus Cargo.toml:
                    Box::new(Label::sekundaer(concat!("Version ", env!("CARGO_PKG_VERSION")))),
                    Box::new(Label::sekundaer("Rust nightly, no_std, x86_64")),
                ])),
            ])) as Box<dyn Widget>,
            Box::new(Trennlinie),
            Box::new(Label::neu(&zeilen)),
        ]
    }
}

impl App for EinstellungenApp {
    fn name(&self) -> &'static str {
        "Einstellungen"
    }

    fn icon(&self) -> &'static Icon {
        &crate::grafik::ICON_ZAHNRAD
    }

    fn aufbau(&self) -> Box<dyn Widget> {
        // Kategorien-Navigation links:
        let eintraege = KATEGORIEN
            .iter()
            .map(|(name, icon)| ListenEintrag { icon: Some(icon), text: String::from(*name) })
            .collect();
        // Spaltenbreite mit der UI-Skala wachsen lassen (sonst
        // schneidet die 1.5/2.0-Schrift die Kategorienamen ab):
        let navigation = ScrollListe::mit_index_nachrichten(eintraege, N_KATEGORIE, N_KATEGORIE)
            .mit_auswahl(Some(self.kategorie))
            .mit_layout(180 * theme::skala_halbe() / 2, 0)
            .mit_fokus(true);

        // Inhaltsbereich rechts (je nach Kategorie):
        let inhalt = match self.kategorie {
            KAT_ANZEIGE => self.seite_anzeige(),
            KAT_ZEIT => self.seite_zeit(),
            KAT_INFO => self.seite_info(),
            _ => self.seite_personalisierung(),
        };

        Box::new(
            hbox(vec![
                Box::new(navigation) as Box<dyn Widget>,
                Box::new(vbox(inhalt).mit_flex(1)),
            ])
            .mit_flex(1),
        )
    }

    fn nachricht(&mut self, id: u32) -> AppReaktion {
        match id {
            // --- Personalisierung ---
            N_THEME_DUNKEL | N_THEME_HELL => {
                let hell = id == N_THEME_HELL;
                if theme::hell_aktiv() == hell {
                    return AppReaktion::keine();
                }
                theme::hell_setzen(hell);
                setze_bool(S_THEME_HELL, hell);
                return Self::desktop_neu_zeichnen();
            }
            id if (N_AKZENT..N_HINTERGRUND).contains(&id) => {
                let index = (id - N_AKZENT) as usize;
                theme::akzent_setzen(index);
                setze_zahl(S_AKZENT, index as i64);
                return Self::desktop_neu_zeichnen();
            }
            id if (N_HINTERGRUND..N_KATEGORIE).contains(&id) => {
                let index = (id - N_HINTERGRUND) as usize;
                theme::hintergrund_setzen(index);
                setze_zahl(S_HINTERGRUND, index as i64);
                // alles_neu_zeichnen setzt hintergrund_neu — der
                // Compositor rendert den Verlauf-CACHE dadurch neu
                // (sonst bliebe der alte Verlauf einfach stehen).
                return Self::desktop_neu_zeichnen();
            }
            // --- Anzeige ---
            id if (N_SKALA_BASIS..N_SKALA_BASIS + 3).contains(&id) => {
                let halbe = 2 + (id - N_SKALA_BASIS) as i32;
                if theme::skala_halbe() == halbe {
                    return AppReaktion::keine();
                }
                // Die Skala SOFORT setzen (lock-freies Atomic, unterm
                // MANAGER-Lock erlaubt) — der direkt folgende
                // Neu-Aufbau markiert dann schon den richtigen Knopf.
                // Das Neuzeichnen ALLER Fenster kommt nach dem Lock.
                theme::skala_setzen_halbe(halbe);
                setze_zahl(S_SKALA, halbe as i64);
                return Self::desktop_neu_zeichnen();
            }
            id if (N_BLINK_BASIS..N_BLINK_BASIS + 3).contains(&id) => {
                let (_, ms) = BLINK_STUFEN[(id - N_BLINK_BASIS) as usize];
                setze_zahl(S_BLINK_MS, ms);
                // Wirkt sofort: Textfeld + Konsolen-Cursor lesen den
                // Wert live — kein Desktop-Neuzeichnen nötig.
            }
            // --- Datum & Uhrzeit ---
            N_OFFSET_MINUS | N_OFFSET_PLUS => {
                let schritt = if id == N_OFFSET_PLUS { 30 } else { -30 };
                let neu = (utc_offset_min() + schritt).clamp(-12 * 60, 14 * 60);
                setze_zahl(S_UTC_OFFSET, neu);
                // Die Systray-Uhr zieht beim nächsten Sekundenwechsel
                // automatisch nach (sie liest jetzt_lokal live).
            }
            N_FORMAT24 => {
                setze_bool(S_FORMAT24, !hole_bool(S_FORMAT24, true));
            }
            // --- Navigation ---
            id if id >= N_KATEGORIE => {
                self.kategorie = ((id - N_KATEGORIE) as usize).min(KATEGORIEN.len() - 1);
            }
            _ => return AppReaktion::keine(),
        }
        AppReaktion::neu_aufbauen()
    }

    /// Live-Seiten (Uhrzeit, Uptime) beim Sekundenwechsel neu bauen.
    fn tick(&mut self) -> bool {
        (self.kategorie == KAT_ZEIT || self.kategorie == KAT_INFO)
            && self.sekunden_tick.faellig()
    }
}

/// Start-Funktion für die App-Registry.
pub fn starten() {
    crate::fenster::app_starten(Box::new(EinstellungenApp::neu()), 620, 440);
}

// ---------------------------------------------------------------------------
// Kleine Spezial-Widgets der Einstellungen-App
// ---------------------------------------------------------------------------

/// Ein klickbares Farbfeld (Akzent-Palette). Das aktive Feld trägt
/// einen doppelten Rahmen in Text-Farbe (ein Akzent-Rahmen wäre
/// unsichtbar — er hätte ja gerade die Feld-Farbe).
struct FarbFeld {
    farbe: Rgba,
    aktiv: bool,
    nachricht: u32,
}

impl Widget for FarbFeld {
    fn wunschgroesse(&self) -> (i32, i32) {
        (metrik().ui_element_hoehe, metrik().ui_element_hoehe)
    }

    fn zeichnen(&self, z: &mut Zeichner<'_, crate::fenster::FensterPuffer>, bereich: Rechteck) {
        let thema = theme::aktuell();
        z.rechteck_abgerundet(bereich, metrik().radius_klein, self.farbe);
        if self.aktiv {
            z.rechteck_rahmen(bereich, thema.text_stark);
            z.rechteck_rahmen(
                Rechteck::neu(bereich.x + 1, bereich.y + 1, bereich.breite - 2, bereich.hoehe - 2),
                thema.text_stark,
            );
        } else {
            z.rechteck_rahmen(bereich, thema.rahmen_passiv);
        }
    }

    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
        match ereignis {
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                UiReaktion::nachricht(self.nachricht)
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

/// Zeigt ein Icon vergrößert an (das Logo auf der Info-Seite).
struct IconBild {
    icon: &'static Icon,
    skala: i32,
}

impl Widget for IconBild {
    fn wunschgroesse(&self) -> (i32, i32) {
        (16 * self.skala + metrik().abstand, 16 * self.skala)
    }
    fn zeichnen(&self, z: &mut Zeichner<'_, crate::fenster::FensterPuffer>, bereich: Rechteck) {
        z.icon(bereich.x, bereich.y, self.icon, self.skala);
    }
    fn ereignis(&mut self, _e: &UiEreignis, _bereich: Rechteck) -> UiReaktion {
        UiReaktion::ignoriert()
    }
}

// ---------------------------------------------------------------------------
// Tests — Parsing, Formatierung und der Speicher-Roundtrip
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Das Datei-Format: Kommentare, Leerzeilen, Leerzeichen um '=',
    /// '=' im Wert, kaputte Zeilen — alles muss robust durchlaufen.
    #[test_case]
    fn test_parsen_schluessel_wert() {
        let text = "# Kommentar\n\
                    \n\
                    theme.hell=1\n\
                    ui.skala = 3 \n\
                    formel=a=b\n\
                    kaputte zeile ohne gleichheitszeichen\n\
                    =wert ohne schluessel\n";
        let werte = parsen(text);
        assert_eq!(werte.len(), 3);
        assert_eq!(werte.get("theme.hell").map(String::as_str), Some("1"));
        assert_eq!(werte.get("ui.skala").map(String::as_str), Some("3"));
        // Nur am ERSTEN '=' getrennt — der Rest gehört zum Wert:
        assert_eq!(werte.get("formel").map(String::as_str), Some("a=b"));
    }

    /// serialisieren -> parsen ist verlustfrei (der Persistenz-Kern).
    #[test_case]
    fn test_serialisieren_roundtrip() {
        let mut werte = BTreeMap::new();
        werte.insert(String::from("a.b"), String::from("42"));
        werte.insert(String::from("x"), String::from("hallo welt"));
        assert_eq!(parsen(&serialisieren(&werte)), werte);

        // Leere Tabelle: nur die Kommentar-Kopfzeile, parst zu leer.
        assert!(parsen(&serialisieren(&BTreeMap::new())).is_empty());
    }

    /// 12h/24h-Formatierung inklusive der Mitternacht/Mittag-Kanten
    /// (00:xx -> 12 AM, 12:xx -> 12 PM — der Klassiker).
    #[test_case]
    fn test_uhrzeit_formatieren() {
        let zeit = |stunde| DatumUhrzeit { jahr: 2026, monat: 7, tag: 16, stunde, minute: 5, sekunde: 9 };
        assert_eq!(uhrzeit_formatieren(&zeit(14), true), "14:05:09");
        assert_eq!(uhrzeit_formatieren(&zeit(14), false), "02:05:09 PM");
        assert_eq!(uhrzeit_formatieren(&zeit(0), false), "12:05:09 AM");
        assert_eq!(uhrzeit_formatieren(&zeit(12), false), "12:05:09 PM");
        assert_eq!(uhrzeit_formatieren(&zeit(23), false), "11:05:09 PM");
    }

    /// Offset-Verschiebung über Tagesgrenzen (vor und zurück) und
    /// der Anzeigetext mit halben Stunden.
    #[test_case]
    fn test_zeit_verschieben_und_offset_text() {
        let datum = DatumUhrzeit { jahr: 2026, monat: 12, tag: 31, stunde: 23, minute: 45, sekunde: 0 };
        let vor = zeit_verschieben(&datum, 30);
        assert_eq!((vor.jahr, vor.monat, vor.tag, vor.stunde, vor.minute), (2027, 1, 1, 0, 15));

        let anfang = DatumUhrzeit { jahr: 2026, monat: 1, tag: 1, stunde: 0, minute: 15, sekunde: 0 };
        let zurueck = zeit_verschieben(&anfang, -60);
        assert_eq!((zurueck.jahr, zurueck.monat, zurueck.tag, zurueck.stunde, zurueck.minute), (2025, 12, 31, 23, 15));

        assert_eq!(offset_text(0), "UTC+00:00");
        assert_eq!(offset_text(120), "UTC+02:00");
        assert_eq!(offset_text(330), "UTC+05:30");
        assert_eq!(offset_text(-270), "UTC-04:30");
    }

    /// Der volle Persistenz-Roundtrip über das echte Test-VFS:
    /// setzen speichert sofort; ein leerer Speicher + laden() holt
    /// den Wert zurück (so überlebt eine Einstellung den "Neustart").
    #[test_case]
    fn test_setzen_speichern_laden_roundtrip() {
        setze_zahl("test.antwort", 42);
        setze_bool("test.an", true);
        setze_text("test.name", "aurora");

        // Speicher künstlich leeren (als wäre frisch gebootet):
        mit_werten(|werte| werte.clear());
        assert_eq!(hole_zahl("test.antwort", 0), 0);

        laden();
        assert_eq!(hole_zahl("test.antwort", 0), 42);
        assert!(hole_bool("test.an", false));
        assert_eq!(hole_text("test.name", ""), "aurora");
        // Unparsbare Zahl fällt auf den Standard zurück:
        assert_eq!(hole_zahl("test.name", 7), 7);

        // Aufräumen: Testschlüssel wieder entfernen.
        mit_werten(|werte| {
            werte.retain(|schluessel, _| !schluessel.starts_with("test."));
        });
        speichern();
    }
}
