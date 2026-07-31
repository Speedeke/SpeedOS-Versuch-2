// explorer.rs — Der SpeedOS-Explorer: die erste echte App auf dem
// UI-Toolkit (ui::App-Trait, gestartet über die Registry).
//
// AUFBAU (aufbau() baut alles aus dem App-Zustand):
//   Werkzeugleiste: [<] [>] [^] + Breadcrumbs (klickbar) bzw. im
//     Eingabemodus die getippte Adresse; Klick auf den freien
//     Bereich (KlickFlaeche) startet den Eingabemodus.
//   Mitte: links der Ordnerbaum (aufklappbar ab /), rechts die
//     Dateiliste (Ordner zuerst, dann alphabetisch; Icons nach Typ;
//     Größen lesbar formatiert).
//   Statusleiste: Eintragszahl + Auswahl-Info (oder Fehlermeldung).
//
// ZUSTAND vs. WIDGETS: Die App hält Pfad, Verlauf, Auswahl und die
// abgeleiteten Listen (neu_laden); die Widgets werden nach jeder
// Nachricht neu aufgebaut. Welcher Listeneintrag gemeint war, steckt
// in der Nachricht selbst: ScrollListe::mit_index_nachrichten
// kodiert BASIS + Index (die Basen liegen weit auseinander).
//
// Der Adress-Eingabemodus läuft über den App-Tasten-Hook (ui::App::
// taste): Die App puffert die Zeichen selbst, Enter navigiert,
// Esc bricht ab — kein geteilter Widget-Zustand nötig.
//
// Noch KEINE Dateioperationen (Teil 2) — nur Navigation.

use crate::fs::{self, NodeTyp};
use crate::grafik::{Icon, Rechteck};
use crate::ui::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::ui::{
    hbox, vbox, App, AppReaktion, Fueller, Maler, UiEreignis, UiKontext, UiReaktion, Widget,
};
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use pc_keyboard::DecodedKey;

// ----- Nachricht-IDs (Basen weit genug auseinander für die Indizes) -----
const N_ZURUECK: u32 = 1;
const N_VOR: u32 = 2;
const N_HOCH: u32 = 3;
const N_ADRESSE: u32 = 4; // Klick auf den freien Adressleisten-Bereich
const N_RECHTS_LEER: u32 = 5; // Rechtsklick auf freie Listenfläche
const N_UMBENENNEN: u32 = 6; // F2 / Kontextmenü (wirkt auf Auswahl)
const N_KOPIEREN: u32 = 7;
const N_AUSSCHNEIDEN: u32 = 8;
const N_LOESCHEN: u32 = 9;
const N_EINFUEGEN: u32 = 10;
const N_NEU_ORDNER: u32 = 11;
const N_NEU_DATEI: u32 = 12;
const N_AKTUALISIEREN: u32 = 13;
const N_WIEDERHERSTELLEN: u32 = 14;
const N_ENDGUELTIG: u32 = 15;
const N_OEFFNEN: u32 = 16; // Kontextmenü "Oeffnen" (wirkt auf Auswahl)
const N_BREADCRUMB: u32 = 100; // + Segment-Index
const N_LISTE_AUSWAHL: u32 = 1000; // + Eintrag-Index
const N_LISTE_OEFFNEN: u32 = 100_000; // + Eintrag-Index (Doppelklick/Enter)
const N_BAUM: u32 = 200_000; // + Baumzeilen-Index
const N_RECHTSKLICK: u32 = 300_000; // + Eintrag-Index (Kontextmenü)

/// Startet ein per Doppelklick geöffnetes Programm — AUSSERHALB des
/// MANAGER-Locks (aus `AppReaktion::danach`), denn `prozess_starten` nimmt
/// das VFS und die Prozess-Tabelle.
///
/// Der Prozess läuft NEBENHER: Anders als beim Shell-Befehl `starte` wird
/// hier NICHT auf sein Ende gewartet — der Explorer soll bedienbar bleiben
/// und der Compositor darf nicht stehen. Die Ausgabe des Programms landet
/// deshalb im HAUPT-Terminal (dorthin leitet `konsole::ausgabe_ziel()`
/// alles, was nicht zu einer gerade laufenden Shell-Sitzung gehört), und
/// der Aufräum-Task holt den Prozess ab, wenn er fertig ist.
fn programm_starten_aus_explorer(pfad: &str) {
    // argv[0] ist der Programmname — dieselbe Konvention wie bei `starte`.
    let name = pfad.rsplit('/').next().unwrap_or(pfad);
    match crate::prozess::prozess_starten(pfad, &[name]) {
        Ok(pid) => crate::println!("[Explorer] '{}' gestartet (PID {}).", pfad, pid),
        Err(fehler) => crate::println!(
            "[Explorer] '{}' konnte nicht gestartet werden: {}",
            pfad,
            fehler.meldung()
        ),
    }
}

/// Der Papierkorb-Ordner im VFS.
/// Der Papierkorb wohnt auf der DATEN-PLATTE, wenn sie gemountet
/// ist, sonst im RAM — dieselbe zentrale Wahl wie bei den
/// Einstellungen (fs::persistenter_pfad, kein if-Wildwuchs).
pub fn papierkorb() -> &'static str {
    fs::persistenter_pfad("/platte/papierkorb", "/papierkorb")
}

/// Startordner neuer Explorer-Fenster und der Datei-Dialoge:
/// das Zuhause /platte/heim, ohne Platte die Wurzel.
pub fn start_ordner() -> &'static str {
    fs::persistenter_pfad("/platte/heim", "/")
}

// ---------------------------------------------------------------------------
// Papierkorb — Entwurfsentscheidung: Der Ursprungspfad wird in einer
// METADATEN-DATEI gemerkt (`<name>.herkunft` neben dem Eintrag im
// Papierkorb) statt im Dateinamen kodiert. Gründe: (1) das VFS kann
// sie mit normalen Mitteln lesen/schreiben (kein Namens-Parser),
// (2) der angezeigte Name bleibt der echte Name, (3) die Ansicht
// filtert `.herkunft` schlicht aus. Preis: zwei Einträge pro Objekt —
// für ein Lern-OS die klarste Lösung.
// ---------------------------------------------------------------------------

const HERKUNFT_ENDUNG: &str = ".herkunft";

/// Findet einen freien Namen: "wunsch", sonst "wunsch (2)", ... —
/// reine Funktion (Namenskonflikt-Regel fürs Einfügen/Papierkorb).
pub fn eindeutiger_name(vorhandene: &[String], wunsch: &str) -> String {
    if !vorhandene.iter().any(|n| n == wunsch) {
        return String::from(wunsch);
    }
    let mut nummer = 2;
    loop {
        let kandidat = format!("{} ({})", wunsch, nummer);
        if !vorhandene.iter().any(|n| n == &kandidat) {
            return kandidat;
        }
        nummer += 1;
    }
}

/// Die Namen in einem Ordner (leer bei Fehler).
fn namen_in(pfad: &str) -> Vec<String> {
    fs::mit_fs(|f| f.liste(pfad))
        .map(|liste| liste.into_iter().map(|e| e.name).collect())
        .unwrap_or_default()
}

/// Verschiebt einen Pfad in den Papierkorb und merkt die Herkunft
/// (Ursprungs-ORDNER) in der Metadaten-Datei. Liefert den Namen im
/// Papierkorb (bei Konflikt mit " (2)"-Suffix).
pub fn in_papierkorb(pfad: &str) -> Result<String, fs::FsFehler> {
    // papierkorb() nimmt den VFS-Lock — VOR mit_fs auswerten:
    let korb = papierkorb();
    let _ = fs::mit_fs(|f| f.mkdir(korb)); // existiert ggf. schon
    let name = pfad.rsplit('/').next().unwrap_or(pfad);
    let korb_name = eindeutiger_name(&namen_in(papierkorb()), name);
    let ziel = fs::pfad_anhaengen(papierkorb(), &korb_name);
    fs::verschieben_rekursiv(pfad, &ziel)?;
    let herkunft = eltern_pfad(pfad);
    fs::mit_fs(|f| {
        f.schreiben(&format!("{}{}", ziel, HERKUNFT_ENDUNG), herkunft.as_bytes())
    })?;
    Ok(korb_name)
}

/// Stellt einen Papierkorb-Eintrag an seinem Ursprungsort wieder her
/// (Konflikt -> " (2)"). Liefert den Zielpfad.
pub fn papierkorb_wiederherstellen(korb_name: &str) -> Result<String, fs::FsFehler> {
    let quelle = fs::pfad_anhaengen(papierkorb(), korb_name);
    let herkunft_datei = format!("{}{}", quelle, HERKUNFT_ENDUNG);
    // Herkunft lesen (fehlt sie: zurück nach /):
    let herkunft = fs::mit_fs(|f| f.lesen(&herkunft_datei))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|_| String::from("/"));
    let ziel_name = eindeutiger_name(&namen_in(&herkunft), korb_name);
    let ziel = fs::pfad_anhaengen(&herkunft, &ziel_name);
    fs::verschieben_rekursiv(&quelle, &ziel)?;
    let _ = fs::mit_fs(|f| f.loeschen(&herkunft_datei));
    Ok(ziel)
}

/// Löscht einen Papierkorb-Eintrag ENDGÜLTIG (samt Metadaten).
pub fn papierkorb_endgueltig(korb_name: &str) -> Result<(), fs::FsFehler> {
    let pfad = fs::pfad_anhaengen(papierkorb(), korb_name);
    fs::loeschen_rekursiv(&pfad)?;
    let _ = fs::mit_fs(|f| f.loeschen(&format!("{}{}", pfad, HERKUNFT_ENDUNG)));
    Ok(())
}

/// Ein Eintrag der Dateiliste (aus dem VFS geladen und sortiert).
#[derive(Clone)]
struct DateiEintrag {
    name: String,
    typ: NodeTyp,
    groesse: usize,
    /// Zuletzt geändert (Sekunden seit 2000, siehe fs::Metadaten).
    geaendert: u64,
}

/// Eine Zeile des Ordnerbaums (flachgeklopfte Hierarchie).
struct BaumZeile {
    pfad: String,
    tiefe: usize,
    aufgeklappt: bool,
}

// ---------------------------------------------------------------------------
// Reine, unit-getestete Bausteine
// ---------------------------------------------------------------------------

/// Zerlegt einen absoluten Pfad in Breadcrumbs: (Anzeigename, Pfad).
/// "/" -> [("/", "/")];  "/system/logs" -> [("/", "/"),
/// ("system", "/system"), ("logs", "/system/logs")].
pub fn breadcrumbs(pfad: &str) -> Vec<(String, String)> {
    let mut teile = vec![(String::from("/"), String::from("/"))];
    let mut bisher = String::new();
    for segment in pfad.split('/').filter(|s| !s.is_empty()) {
        bisher.push('/');
        bisher.push_str(segment);
        teile.push((String::from(segment), bisher.clone()));
    }
    teile
}

/// Der Eltern-Pfad ("/system/logs" -> "/system", "/x" -> "/", "/" -> "/").
pub fn eltern_pfad(pfad: &str) -> String {
    match pfad.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => String::from(&pfad[..index]),
    }
}

/// Größe lesbar formatieren: Bytes, KiB oder MiB mit einer
/// Nachkommastelle (ganzzahlig gerechnet — kein Fließkomma im Kernel).
pub fn groesse_formatieren(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        let zehntel = bytes * 10 / 1024;
        format!("{},{} KiB", zehntel / 10, zehntel % 10)
    } else {
        let zehntel = bytes * 10 / (1024 * 1024);
        format!("{},{} MiB", zehntel / 10, zehntel % 10)
    }
}

/// Sortiert: Ordner zuerst, innerhalb alphabetisch (Groß/Klein egal).
fn sortieren(eintraege: &mut [DateiEintrag]) {
    eintraege.sort_by(|a, b| {
        let ordnung = |e: &DateiEintrag| e.typ != NodeTyp::Verzeichnis; // false < true
        ordnung(a)
            .cmp(&ordnung(b))
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
}

/// Der Zurück/Vor-Verlauf (wie im Browser) — reine Logik, testbar.
pub struct Verlauf {
    eintraege: Vec<String>,
    position: usize,
}

impl Verlauf {
    pub fn neu(start: &str) -> Self {
        Verlauf { eintraege: vec![String::from(start)], position: 0 }
    }
    pub fn aktuell(&self) -> &str {
        &self.eintraege[self.position]
    }
    /// Neuen Ort besuchen: kappt die Vorwärts-Historie (wie Browser).
    pub fn besuchen(&mut self, pfad: &str) {
        if self.aktuell() == pfad {
            return;
        }
        self.eintraege.truncate(self.position + 1);
        self.eintraege.push(String::from(pfad));
        self.position += 1;
    }
    pub fn kann_zurueck(&self) -> bool {
        self.position > 0
    }
    pub fn kann_vor(&self) -> bool {
        self.position + 1 < self.eintraege.len()
    }
    pub fn zurueck(&mut self) -> Option<&str> {
        if self.kann_zurueck() {
            self.position -= 1;
            Some(self.aktuell())
        } else {
            None
        }
    }
    pub fn vor(&mut self) -> Option<&str> {
        if self.kann_vor() {
            self.position += 1;
            Some(self.aktuell())
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// KlickFlaeche: unsichtbares Flex-Widget, das Klicks meldet
// (der "freie Bereich" der Adressleiste)
// ---------------------------------------------------------------------------

struct KlickFlaeche {
    nachricht: u32,
}

impl Widget for KlickFlaeche {
    fn wunschgroesse(&self, _k: &UiKontext) -> (i32, i32) {
        (0, 0)
    }
    fn flex(&self) -> i32 {
        1
    }
    fn zeichnen(&self, _m: &mut Maler<'_>, _b: Rechteck) {}
    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck, _k: &UiKontext) -> UiReaktion {
        match ereignis {
            UiEreignis::Klick { x, y } if bereich.enthaelt(*x, *y) => {
                UiReaktion::nachricht(self.nachricht)
            }
            _ => UiReaktion::ignoriert(),
        }
    }
}

// ---------------------------------------------------------------------------
// Die Explorer-App
// ---------------------------------------------------------------------------

pub struct ExplorerApp {
    verlauf: Verlauf,
    /// Aufgeklappte Ordner im Baum (Pfade).
    aufgeklappt: BTreeSet<String>,
    /// Auswahl in der Dateiliste (Index in `eintraege`).
    auswahl: Option<usize>,
    /// Adress-Eingabemodus (App puffert die Tasten selbst).
    adress_modus: bool,
    adress_puffer: String,
    /// F2-Umbenennen: Some(Puffer) = der Auswahl-Eintrag wird gerade
    /// umbenannt (Anzeige als Eingabezeile in der Liste).
    umbenennen: Option<String>,
    /// Abgeleiteter Zustand (neu_laden):
    eintraege: Vec<DateiEintrag>,
    baum_zeilen: Vec<BaumZeile>,
    fehler: Option<String>,
}

impl ExplorerApp {
    pub fn neu() -> Self {
        let mut app = ExplorerApp {
            verlauf: Verlauf::neu(start_ordner()),
            aufgeklappt: BTreeSet::new(),
            auswahl: None,
            adress_modus: false,
            adress_puffer: String::new(),
            umbenennen: None,
            eintraege: Vec::new(),
            baum_zeilen: Vec::new(),
            fehler: None,
        };
        app.aufgeklappt.insert(String::from("/"));
        app.neu_laden();
        app
    }

    fn pfad(&self) -> &str {
        self.verlauf.aktuell()
    }

    /// Lädt Dateiliste und Ordnerbaum aus dem VFS neu (nach jeder
    /// Navigation). fs::mit_fs ist unter dem MANAGER-Lock erlaubt.
    fn neu_laden(&mut self) {
        self.fehler = None;
        let pfad = String::from(self.pfad());
        match fs::mit_fs(|f| f.liste(&pfad)) {
            Ok(liste) => {
                self.eintraege = liste
                    .into_iter()
                    // Im Papierkorb die .herkunft-Metadaten ausblenden:
                    .filter(|e| {
                        !(pfad == papierkorb() && e.name.ends_with(HERKUNFT_ENDUNG))
                    })
                    .map(|e| DateiEintrag {
                        name: e.name,
                        typ: e.typ,
                        groesse: e.groesse,
                        geaendert: e.geaendert,
                    })
                    .collect();
                sortieren(&mut self.eintraege);
            }
            Err(f) => {
                self.eintraege = Vec::new();
                self.fehler = Some(String::from(f.meldung()));
            }
        }
        self.auswahl = self.auswahl.filter(|&i| i < self.eintraege.len());
        self.baum_laden();
    }

    /// Baut die flache Baum-Ansicht: Wurzel + aufgeklappte Ordner.
    fn baum_laden(&mut self) {
        let mut zeilen = Vec::new();
        Self::baum_ast(&self.aufgeklappt, "/", 0, &mut zeilen);
        self.baum_zeilen = zeilen;
    }

    fn baum_ast(aufgeklappt: &BTreeSet<String>, pfad: &str, tiefe: usize, zeilen: &mut Vec<BaumZeile>) {
        let offen = aufgeklappt.contains(pfad);
        zeilen.push(BaumZeile { pfad: String::from(pfad), tiefe, aufgeklappt: offen });
        if !offen {
            return;
        }
        // Unterordner einsammeln (Dateien gehören nicht in den Baum).
        // WICHTIG: liste() VOR dem Abstieg abschließen (mit_fs nie
        // verschachteln — Deadlock-Regel).
        let unterordner: Vec<String> = fs::mit_fs(|f| f.liste(pfad))
            .map(|liste| {
                let mut ordner: Vec<String> = liste
                    .into_iter()
                    .filter(|e| e.typ == NodeTyp::Verzeichnis)
                    .map(|e| {
                        if pfad == "/" {
                            format!("/{}", e.name)
                        } else {
                            format!("{}/{}", pfad, e.name)
                        }
                    })
                    .collect();
                ordner.sort_by_key(|a| a.to_ascii_lowercase());
                ordner
            })
            .unwrap_or_default();
        for unter in unterordner {
            Self::baum_ast(aufgeklappt, &unter, tiefe + 1, zeilen);
        }
    }

    /// Navigiert zu einem (absoluten) Pfad — über den Verlauf.
    fn navigieren(&mut self, pfad: &str) {
        let ziel = fs::pfad_aufloesen("/", pfad);
        match fs::mit_fs(|f| f.node_typ(&ziel)) {
            Ok(NodeTyp::Verzeichnis) => {
                self.verlauf.besuchen(&ziel);
                self.auswahl = None;
                // Zielordner im Baum sichtbar machen (Eltern aufklappen):
                for (_, teil) in breadcrumbs(&ziel) {
                    self.aufgeklappt.insert(teil);
                }
                self.neu_laden();
            }
            Ok(NodeTyp::Datei) => self.fehler = Some(format!("{} ist eine Datei", ziel)),
            Err(f) => self.fehler = Some(String::from(f.meldung())),
        }
    }

    /// Darf im aktuellen Ordner geschrieben werden? Auf Nur-Lese-
    /// Mounts (FAT32) false — dann sind Neu/Löschen/Umbenennen/
    /// Einfügen ausgegraut bzw. gesperrt. Kopieren VON hier bleibt
    /// erlaubt (das schreibt ja woanders hin).
    fn beschreibbar(&self) -> bool {
        fs::pfad_beschreibbar(self.pfad())
    }

    /// Setzt eine "nur lesen"-Meldung in die Statusleiste (wenn ein
    /// Schreib-Versuch auf einem Nur-Lese-Mount abgefangen wird).
    fn nur_lesen_hinweis(&mut self) {
        self.fehler = Some(String::from("Dieses Laufwerk ist nur lesbar (z. B. FAT-Stick)."));
    }

    /// Der Icon fürs Dateilisten-/Baum-Element.
    fn eintrag_icon(typ: NodeTyp) -> &'static Icon {
        match typ {
            NodeTyp::Verzeichnis => &crate::grafik::ICON_ORDNER,
            NodeTyp::Datei => &crate::grafik::ICON_DATEI,
        }
    }

    fn im_papierkorb(&self) -> bool {
        self.pfad() == papierkorb()
    }

    /// Voller Pfad des ausgewählten Eintrags.
    fn auswahl_pfad(&self) -> Option<String> {
        let eintrag = self.eintraege.get(self.auswahl?)?;
        Some(fs::pfad_anhaengen(self.pfad(), &eintrag.name))
    }

    fn fehler_setzen(&mut self, ergebnis: Result<(), fs::FsFehler>) {
        if let Err(f) = ergebnis {
            self.fehler = Some(String::from(f.meldung()));
        }
    }

    /// Entf: außerhalb des Papierkorbs -> hinein verschieben; im
    /// Papierkorb -> endgültig löschen.
    fn loeschen_auswahl(&mut self) {
        let (pfad, name) = match (self.auswahl_pfad(), self.auswahl) {
            (Some(pfad), Some(index)) => (pfad, self.eintraege[index].name.clone()),
            _ => return,
        };
        let ergebnis = if self.im_papierkorb() {
            papierkorb_endgueltig(&name)
        } else {
            in_papierkorb(&pfad).map(|_| ())
        };
        self.fehler_setzen(ergebnis);
        self.auswahl = None;
        self.neu_laden();
    }

    /// Strg+V / Kontextmenü "Einfügen": aus der globalen Ablage in
    /// den aktuellen Ordner (Konflikt -> " (2)"). Ausschneiden ist
    /// seit SpeedFS ein ECHTES rename (fs::verschieben_rekursiv):
    /// atomar und ohne die Datei-Inhalte anzufassen — das frühere
    /// kopieren+löschen wäre auf der Platte untragbar (jedes Byte
    /// zweimal durch den Treiber). Nur über die Mount-Grenze fällt
    /// das VFS selbst auf kopieren+löschen zurück.
    fn einfuegen(&mut self) {
        let ablage = match crate::ablage::holen() {
            Some(ablage) => ablage,
            None => return,
        };
        let name = ablage.pfad.rsplit('/').next().unwrap_or(&ablage.pfad);
        let ziel_name = eindeutiger_name(&namen_in(self.pfad()), name);
        let ziel = fs::pfad_anhaengen(self.pfad(), &ziel_name);
        let ergebnis = if ablage.ausschneiden {
            crate::ablage::leeren();
            fs::verschieben_rekursiv(&ablage.pfad, &ziel)
        } else {
            fs::kopieren_rekursiv(&ablage.pfad, &ziel)
        };
        self.fehler_setzen(ergebnis);
        self.neu_laden();
    }

    /// Neu-Ordner/-Datei: mit eindeutigem Namen anlegen und sofort in
    /// den Umbenennen-Modus springen (wie bei den "Großen").
    fn neu_anlegen(&mut self, typ: NodeTyp) {
        let wunsch = match typ {
            NodeTyp::Verzeichnis => "Neuer Ordner",
            NodeTyp::Datei => "Neue Datei.txt",
        };
        let name = eindeutiger_name(&namen_in(self.pfad()), wunsch);
        let pfad = fs::pfad_anhaengen(self.pfad(), &name);
        let ergebnis = fs::mit_fs(|f| match typ {
            NodeTyp::Verzeichnis => f.mkdir(&pfad),
            NodeTyp::Datei => f.schreiben(&pfad, b""),
        });
        self.fehler_setzen(ergebnis);
        self.neu_laden();
        self.auswahl = self.eintraege.iter().position(|e| e.name == name);
        if self.auswahl.is_some() {
            self.umbenennen = Some(name);
        }
    }

    /// F2-Enter: Auswahl auf den getippten Namen umbenennen.
    fn umbenennen_ausfuehren(&mut self) {
        let (neuer_name, index) = match (self.umbenennen.take(), self.auswahl) {
            (Some(name), Some(index)) if !name.is_empty() => (name, index),
            _ => return,
        };
        let alter_name = self.eintraege[index].name.clone();
        if neuer_name == alter_name {
            return;
        }
        let quelle = fs::pfad_anhaengen(self.pfad(), &alter_name);
        let ziel_name = eindeutiger_name(&namen_in(self.pfad()), &neuer_name);
        let ziel = fs::pfad_anhaengen(self.pfad(), &ziel_name);
        let ergebnis = fs::verschieben_rekursiv(&quelle, &ziel);
        self.fehler_setzen(ergebnis);
        self.neu_laden();
        self.auswahl = self.eintraege.iter().position(|e| e.name == ziel_name);
    }

    /// Doppelklick/Enter/Kontextmenü-Öffnen auf einen Eintrag.
    fn eintrag_oeffnen(&mut self, index: usize) -> AppReaktion {
        let eintrag = match self.eintraege.get(index) {
            Some(eintrag) => eintrag,
            None => return AppReaktion::neu_aufbauen(),
        };
        let pfad = fs::pfad_anhaengen(self.pfad(), &eintrag.name);
        match eintrag.typ {
            NodeTyp::Verzeichnis => {
                self.navigieren(&pfad);
                AppReaktion::neu_aufbauen()
            }
            NodeTyp::Datei => {
                // AUSFÜHRBAR ODER TEXT? (Serie 6, Teil 5)
                //
                // Seit es echte Programme gibt, hat ein Doppelklick auf eine
                // Datei zwei mögliche Bedeutungen. Entschieden wird an den
                // ERSTEN BYTES der Datei, nicht an ihrem Namen: SpeedOS kennt
                // keine Dateiendungen, und eine Endung wäre auch nur eine
                // Behauptung. `ist_programm` liest 20 Byte und prüft die
                // ELF-Magie samt Architektur (das `fs` darf hier unter dem
                // MANAGER-Lock benutzt werden — siehe Lock-Regel in ui/app.rs).
                //
                // Die vollständige Prüfung macht dann der Loader; scheitert
                // sie, meldet `prozess_starten` es sauber.
                if crate::prozess::ist_programm(&pfad) {
                    AppReaktion::danach(move || {
                        programm_starten_aus_explorer(&pfad);
                    })
                } else {
                    // SpeedText öffnen — NACH dem Lock (danach trägt den
                    // Pfad per move in die Box, siehe AppReaktion-Doku).
                    // Der frühere Nur-Lese-Betrachter ist Geschichte:
                    // Doppelklick auf eine Datei heißt jetzt BEARBEITEN.
                    AppReaktion::danach(move || {
                        crate::speedtext::starten_mit(&pfad);
                    })
                }
            }
        }
    }
}

impl App for ExplorerApp {
    fn name(&self) -> &'static str {
        "Explorer"
    }

    fn icon(&self) -> &'static Icon {
        &crate::grafik::ICON_ORDNER
    }

    fn aufbau(&self) -> Box<dyn Widget> {
        // --- Werkzeugleiste ---
        // Neu-Ordner/-Datei nur, wo geschrieben werden darf (FAT = aus):
        let schreibbar = self.beschreibbar();
        let mut leiste: Vec<Box<dyn Widget>> = vec![
            Box::new(Button::neu("<", N_ZURUECK)),
            Box::new(Button::neu(">", N_VOR)),
            Box::new(Button::neu("^", N_HOCH)),
            Box::new(Button::neu("+O", N_NEU_ORDNER).mit_deaktiviert(!schreibbar)),
            Box::new(Button::neu("+D", N_NEU_DATEI).mit_deaktiviert(!schreibbar)),
            Box::new(Button::neu("@", N_AKTUALISIEREN)),
        ];
        if self.im_papierkorb() {
            leiste.push(Box::new(Button::neu("Wiederherstellen", N_WIEDERHERSTELLEN)));
            leiste.push(Box::new(Button::neu("Endg. loeschen", N_ENDGUELTIG)));
        }
        if self.adress_modus {
            leiste.push(Box::new(Label::neu(&format!("> {}_", self.adress_puffer))));
            leiste.push(Box::new(Fueller));
        } else {
            for (i, (name, _)) in breadcrumbs(self.pfad()).into_iter().enumerate() {
                leiste.push(Box::new(Button::neu(&name, N_BREADCRUMB + i as u32)));
            }
            leiste.push(Box::new(KlickFlaeche { nachricht: N_ADRESSE }));
        }

        // --- Ordnerbaum (links) ---
        let baum_eintraege = self
            .baum_zeilen
            .iter()
            .map(|zeile| {
                let name = if zeile.pfad == "/" {
                    "/"
                } else {
                    zeile.pfad.rsplit('/').next().unwrap_or("?")
                };
                let marker = if zeile.aufgeklappt { "-" } else { "+" };
                ListenEintrag {
                    icon: Some(&crate::grafik::ICON_ORDNER),
                    text: format!("{}{} {}", "  ".repeat(zeile.tiefe), marker, name),
                }
            })
            .collect();
        let baum = ScrollListe::mit_index_nachrichten(baum_eintraege, N_BAUM, N_BAUM)
            .mit_layout(170, 0);

        // --- Dateiliste (rechts) ---
        let listen_eintraege = self
            .eintraege
            .iter()
            .enumerate()
            .map(|(i, e)| ListenEintrag {
                icon: Some(Self::eintrag_icon(e.typ)),
                // Im F2-Modus wird der Auswahl-Eintrag zur Eingabezeile:
                text: match (&self.umbenennen, self.auswahl) {
                    (Some(puffer), Some(auswahl)) if i == auswahl => {
                        format!("> {}_", puffer)
                    }
                    _ => match e.typ {
                        NodeTyp::Verzeichnis => e.name.clone(),
                        NodeTyp::Datei => {
                            format!("{}  ({})", e.name, groesse_formatieren(e.groesse))
                        }
                    },
                },
            })
            .collect();
        let liste = ScrollListe::mit_index_nachrichten(listen_eintraege, N_LISTE_AUSWAHL, N_LISTE_OEFFNEN)
            .mit_rechtsklick(N_RECHTSKLICK, N_RECHTS_LEER)
            .mit_auswahl(self.auswahl)
            .mit_fokus(!self.adress_modus && self.umbenennen.is_none());

        // --- Statusleiste (bei Auswahl mit ECHTEM Zeitstempel aus
        // --- dem VFS, im selben Anzeige-Offset wie die Systray-Uhr) ---
        let status = match (&self.fehler, self.auswahl) {
            (Some(fehler), _) => format!("Fehler: {}", fehler),
            (None, Some(index)) => {
                let e = &self.eintraege[index];
                let stempel = crate::einstellungen::stempel_text(e.geaendert);
                match e.typ {
                    NodeTyp::Verzeichnis => format!(
                        "{} Eintraege  |  {} (Ordner, geaendert {})",
                        self.eintraege.len(),
                        e.name,
                        stempel
                    ),
                    NodeTyp::Datei => format!(
                        "{} Eintraege  |  {} ({}, geaendert {})",
                        self.eintraege.len(),
                        e.name,
                        groesse_formatieren(e.groesse),
                        stempel
                    ),
                }
            }
            (None, None) => format!("{} Eintraege", self.eintraege.len()),
        };

        Box::new(vbox(vec![
            Box::new(hbox(leiste)) as Box<dyn Widget>,
            Box::new(hbox(vec![
                Box::new(baum) as Box<dyn Widget>,
                Box::new(liste),
            ])
            .mit_flex(1)),
            Box::new(Trennlinie),
            Box::new(Label::sekundaer(&status)),
        ]))
    }

    fn nachricht(&mut self, id: u32) -> AppReaktion {
        // Alle SCHREIBENDEN Aktionen auf einem Nur-Lese-Mount abfangen —
        // die Knöpfe sind zwar ausgegraut, aber Tastenkürzel könnten
        // sie sonst noch auslösen. Ausschneiden (= Verschieben von HIER
        // weg) verändert diesen Ordner ebenfalls, zählt also mit;
        // Kopieren nach woanders bleibt erlaubt.
        let schreibt_hier = matches!(
            id,
            N_UMBENENNEN | N_AUSSCHNEIDEN | N_LOESCHEN | N_EINFUEGEN | N_NEU_ORDNER | N_NEU_DATEI
        );
        if schreibt_hier && !self.beschreibbar() {
            self.nur_lesen_hinweis();
            return AppReaktion::neu_aufbauen();
        }
        match id {
            // --- Operationen (wirken auf die Auswahl) ---
            N_UMBENENNEN => {
                if let Some(index) = self.auswahl {
                    self.umbenennen = Some(self.eintraege[index].name.clone());
                }
            }
            N_KOPIEREN | N_AUSSCHNEIDEN => {
                if let Some(pfad) = self.auswahl_pfad() {
                    crate::ablage::setzen(&pfad, id == N_AUSSCHNEIDEN);
                }
            }
            N_LOESCHEN => self.loeschen_auswahl(),
            N_EINFUEGEN => self.einfuegen(),
            N_NEU_ORDNER => self.neu_anlegen(NodeTyp::Verzeichnis),
            N_NEU_DATEI => self.neu_anlegen(NodeTyp::Datei),
            N_AKTUALISIEREN => self.neu_laden(),
            N_WIEDERHERSTELLEN => {
                if let Some(index) = self.auswahl {
                    let name = self.eintraege[index].name.clone();
                    let ergebnis = papierkorb_wiederherstellen(&name).map(|_| ());
                    self.fehler_setzen(ergebnis);
                    self.auswahl = None;
                    self.neu_laden();
                }
            }
            N_ENDGUELTIG => {
                if let Some(index) = self.auswahl {
                    let name = self.eintraege[index].name.clone();
                    let ergebnis = papierkorb_endgueltig(&name);
                    self.fehler_setzen(ergebnis);
                    self.auswahl = None;
                    self.neu_laden();
                }
            }
            N_OEFFNEN => {
                if let Some(index) = self.auswahl {
                    return self.eintrag_oeffnen(index);
                }
            }
            // --- Kontextmenüs (Rechtsklick) ---
            // Auf Nur-Lese-Mounts (FAT) fehlen die schreibenden
            // Einträge komplett — der Nutzer soll gar nicht erst
            // ins Leere klicken.
            N_RECHTS_LEER => {
                let mut eintraege = Vec::new();
                if self.beschreibbar() {
                    eintraege.push((String::from("Einfuegen"), N_EINFUEGEN));
                    eintraege.push((String::from("Neuer Ordner"), N_NEU_ORDNER));
                    eintraege.push((String::from("Neue Datei"), N_NEU_DATEI));
                }
                eintraege.push((String::from("Aktualisieren"), N_AKTUALISIEREN));
                return AppReaktion::menue(eintraege);
            }
            id if id >= N_RECHTSKLICK => {
                self.auswahl = Some((id - N_RECHTSKLICK) as usize);
                let schreibbar = self.beschreibbar();
                let mut eintraege = vec![
                    (String::from("Oeffnen"), N_OEFFNEN),
                    // Kopieren geht immer (Ziel bestimmt der Nutzer):
                    (String::from("Kopieren (Strg+C)"), N_KOPIEREN),
                ];
                if schreibbar {
                    eintraege.insert(1, (String::from("Umbenennen (F2)"), N_UMBENENNEN));
                    eintraege.push((String::from("Ausschneiden (Strg+X)"), N_AUSSCHNEIDEN));
                    eintraege.push((String::from("Loeschen (Entf)"), N_LOESCHEN));
                }
                if self.im_papierkorb() {
                    eintraege.push((String::from("Wiederherstellen"), N_WIEDERHERSTELLEN));
                }
                return AppReaktion::menue(eintraege);
            }
            N_ZURUECK => {
                if self.verlauf.zurueck().is_some() {
                    self.auswahl = None;
                    self.neu_laden();
                }
            }
            N_VOR => {
                if self.verlauf.vor().is_some() {
                    self.auswahl = None;
                    self.neu_laden();
                }
            }
            N_HOCH => {
                let eltern = eltern_pfad(self.pfad());
                self.navigieren(&eltern);
            }
            N_ADRESSE => {
                self.adress_modus = true;
                self.adress_puffer = String::from(self.pfad());
            }
            id if (N_BREADCRUMB..N_LISTE_AUSWAHL).contains(&id) => {
                let krumen = breadcrumbs(self.pfad());
                if let Some((_, ziel)) = krumen.get((id - N_BREADCRUMB) as usize) {
                    let ziel = ziel.clone();
                    self.navigieren(&ziel);
                }
            }
            id if (N_LISTE_AUSWAHL..N_LISTE_OEFFNEN).contains(&id) => {
                self.auswahl = Some((id - N_LISTE_AUSWAHL) as usize);
            }
            id if (N_LISTE_OEFFNEN..N_BAUM).contains(&id) => {
                return self.eintrag_oeffnen((id - N_LISTE_OEFFNEN) as usize);
            }
            id if (N_BAUM..N_RECHTSKLICK).contains(&id) => {
                let index = (id - N_BAUM) as usize;
                if let Some(zeile) = self.baum_zeilen.get(index) {
                    let pfad = zeile.pfad.clone();
                    // Klick klappt auf/zu UND navigiert dorthin.
                    if zeile.aufgeklappt && pfad != *self.pfad() {
                        // war offen, aber nicht aktuell: nur hin.
                    } else if zeile.aufgeklappt {
                        self.aufgeklappt.remove(&pfad);
                    } else {
                        self.aufgeklappt.insert(pfad.clone());
                    }
                    self.navigieren(&pfad);
                }
            }
            _ => return AppReaktion::keine(),
        }
        AppReaktion::neu_aufbauen()
    }

    /// App-Shortcuts + Eingabemodi (siehe Kopfkommentar).
    fn taste(&mut self, taste: DecodedKey) -> Option<AppReaktion> {
        // F2-Umbenennen-Modus: die App puffert die Zeichen selbst.
        if self.umbenennen.is_some() {
            match taste {
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                    self.umbenennen_ausfuehren();
                }
                DecodedKey::Unicode('\u{1b}') => self.umbenennen = None,
                DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                    if let Some(puffer) = &mut self.umbenennen {
                        puffer.pop();
                    }
                }
                DecodedKey::Unicode(zeichen) if zeichen >= ' ' => {
                    if let Some(puffer) = &mut self.umbenennen {
                        if puffer.chars().count() < 40 {
                            puffer.push(zeichen);
                        }
                    }
                }
                _ => return Some(AppReaktion::keine()),
            }
            return Some(AppReaktion::neu_aufbauen());
        }

        if self.adress_modus {
            match taste {
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                    self.adress_modus = false;
                    let ziel = self.adress_puffer.clone();
                    self.navigieren(&ziel);
                }
                DecodedKey::Unicode('\u{1b}') => self.adress_modus = false,
                DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                    self.adress_puffer.pop();
                }
                DecodedKey::Unicode(zeichen) if zeichen >= ' ' => {
                    if self.adress_puffer.chars().count() < 60 {
                        self.adress_puffer.push(zeichen);
                    }
                }
                _ => return Some(AppReaktion::keine()),
            }
            return Some(AppReaktion::neu_aufbauen());
        }
        // App-Shortcuts (Kürzel dank HandleControl::MapLettersToUnicode
        // als Steuerzeichen: Strg+C = U+0003, X = U+0018, V = U+0016).
        match taste {
            // Backspace = einen Ordner hoch (klassischer Shortcut).
            DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}') => {
                let eltern = eltern_pfad(self.pfad());
                self.navigieren(&eltern);
                Some(AppReaktion::neu_aufbauen())
            }
            DecodedKey::RawKey(pc_keyboard::KeyCode::F2) => {
                Some(self.nachricht(N_UMBENENNEN))
            }
            DecodedKey::RawKey(pc_keyboard::KeyCode::Delete) => {
                Some(self.nachricht(N_LOESCHEN))
            }
            DecodedKey::Unicode('\u{3}') => Some(self.nachricht(N_KOPIEREN)),
            DecodedKey::Unicode('\u{18}') => Some(self.nachricht(N_AUSSCHNEIDEN)),
            DecodedKey::Unicode('\u{16}') => Some(self.nachricht(N_EINFUEGEN)),
            // Alles andere an die Widgets (Pfeile/Enter -> Dateiliste).
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — reine Logik, ohne Fenster
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_breadcrumbs_zerlegung() {
        assert_eq!(breadcrumbs("/"), vec![(String::from("/"), String::from("/"))]);
        let krumen = breadcrumbs("/system/logs");
        assert_eq!(krumen.len(), 3);
        assert_eq!(krumen[1], (String::from("system"), String::from("/system")));
        assert_eq!(krumen[2], (String::from("logs"), String::from("/system/logs")));

        assert_eq!(eltern_pfad("/system/logs"), "/system");
        assert_eq!(eltern_pfad("/system"), "/");
        assert_eq!(eltern_pfad("/"), "/");
    }

    #[test_case]
    fn test_groesse_formatieren() {
        assert_eq!(groesse_formatieren(0), "0 B");
        assert_eq!(groesse_formatieren(1023), "1023 B");
        assert_eq!(groesse_formatieren(1024), "1,0 KiB");
        assert_eq!(groesse_formatieren(1536), "1,5 KiB");
        assert_eq!(groesse_formatieren(1024 * 1024), "1,0 MiB");
        assert_eq!(groesse_formatieren(5 * 1024 * 1024 + 512 * 1024), "5,5 MiB");
    }

    #[test_case]
    fn test_sortierung_ordner_zuerst() {
        let mut eintraege = vec![
            DateiEintrag { name: String::from("zebra.txt"), typ: NodeTyp::Datei, groesse: 1, geaendert: 0 },
            DateiEintrag { name: String::from("Beta"), typ: NodeTyp::Verzeichnis, groesse: 0, geaendert: 0 },
            DateiEintrag { name: String::from("alpha.txt"), typ: NodeTyp::Datei, groesse: 1, geaendert: 0 },
            DateiEintrag { name: String::from("anton"), typ: NodeTyp::Verzeichnis, groesse: 0, geaendert: 0 },
        ];
        sortieren(&mut eintraege);
        let namen: Vec<&str> = eintraege.iter().map(|e| e.name.as_str()).collect();
        // Ordner zuerst (anton < Beta, case-insensitiv), dann Dateien:
        assert_eq!(namen, vec!["anton", "Beta", "alpha.txt", "zebra.txt"]);
    }

    /// Namenskonflikte: frei -> unverändert, belegt -> " (2)", " (3)".
    #[test_case]
    fn test_eindeutiger_name() {
        let keine: Vec<String> = Vec::new();
        assert_eq!(eindeutiger_name(&keine, "test.txt"), "test.txt");
        let belegt = vec![String::from("test.txt"), String::from("test.txt (2)")];
        assert_eq!(eindeutiger_name(&belegt, "test.txt"), "test.txt (3)");
        assert_eq!(eindeutiger_name(&belegt, "anders"), "anders");
    }

    /// Rekursives Kopieren: Ordner mit Unterordner + Dateien landet
    /// vollständig am Ziel (läuft gegen das echte Test-VFS).
    #[test_case]
    fn test_kopieren_rekursiv() {
        let _ = fs::mit_fs(|f| f.mkdir("/kopiertest"));
        let _ = fs::mit_fs(|f| f.mkdir("/kopiertest/unten"));
        fs::mit_fs(|f| f.schreiben("/kopiertest/a.txt", b"A")).unwrap();
        fs::mit_fs(|f| f.schreiben("/kopiertest/unten/b.txt", b"BB")).unwrap();

        fs::kopieren_rekursiv("/kopiertest", "/kopie").unwrap();
        assert_eq!(fs::mit_fs(|f| f.lesen("/kopie/a.txt")).unwrap(), b"A");
        assert_eq!(fs::mit_fs(|f| f.lesen("/kopie/unten/b.txt")).unwrap(), b"BB");
        // Quelle unangetastet:
        assert!(fs::mit_fs(|f| f.lesen("/kopiertest/a.txt")).is_ok());

        // Aufräumen (rekursives Löschen gleich mitgetestet):
        fs::loeschen_rekursiv("/kopie").unwrap();
        fs::loeschen_rekursiv("/kopiertest").unwrap();
        assert!(fs::mit_fs(|f| f.node_typ("/kopie")).is_err());
    }

    /// Papierkorb-Roundtrip: löschen merkt die Herkunft, Wieder-
    /// herstellen bringt die Datei an den Ursprungsort zurück,
    /// Endgültig-Löschen räumt samt Metadaten auf.
    #[test_case]
    fn test_papierkorb_roundtrip() {
        let _ = fs::mit_fs(|f| f.mkdir("/korbtest"));
        fs::mit_fs(|f| f.schreiben("/korbtest/opfer.txt", b"weg?")).unwrap();

        // In den Papierkorb (Herkunft = /korbtest):
        let korb_name = in_papierkorb("/korbtest/opfer.txt").unwrap();
        assert!(fs::mit_fs(|f| f.node_typ("/korbtest/opfer.txt")).is_err());
        let korb_pfad = fs::pfad_anhaengen(papierkorb(), &korb_name);
        assert_eq!(fs::mit_fs(|f| f.lesen(&korb_pfad)).unwrap(), b"weg?");
        let herkunft = fs::mit_fs(|f| f.lesen(&format!("{}{}", korb_pfad, HERKUNFT_ENDUNG))).unwrap();
        assert_eq!(herkunft, b"/korbtest");

        // Wiederherstellen -> zurück am Ursprungsort, Metadaten weg:
        let ziel = papierkorb_wiederherstellen(&korb_name).unwrap();
        assert_eq!(ziel, "/korbtest/opfer.txt");
        assert_eq!(fs::mit_fs(|f| f.lesen(&ziel)).unwrap(), b"weg?");
        assert!(fs::mit_fs(|f| f.node_typ(&korb_pfad)).is_err());

        // Nochmal löschen + endgültig:
        let korb_name = in_papierkorb("/korbtest/opfer.txt").unwrap();
        papierkorb_endgueltig(&korb_name).unwrap();
        let korb_pfad = fs::pfad_anhaengen(papierkorb(), &korb_name);
        assert!(fs::mit_fs(|f| f.node_typ(&korb_pfad)).is_err());

        fs::loeschen_rekursiv("/korbtest").unwrap();
    }

    #[test_case]
    fn test_verlauf_wie_browser() {
        let mut verlauf = Verlauf::neu("/");
        assert!(!verlauf.kann_zurueck());
        verlauf.besuchen("/system");
        verlauf.besuchen("/system/logs");
        assert_eq!(verlauf.aktuell(), "/system/logs");

        assert_eq!(verlauf.zurueck(), Some("/system"));
        assert!(verlauf.kann_vor());
        assert_eq!(verlauf.vor(), Some("/system/logs"));

        // Neuer Besuch nach Zurück KAPPT die Vorwärts-Historie:
        verlauf.zurueck();
        verlauf.besuchen("/anders");
        assert!(!verlauf.kann_vor());
        assert_eq!(verlauf.aktuell(), "/anders");
        assert_eq!(verlauf.zurueck(), Some("/system"));

        // Doppelt denselben Ort besuchen erzeugt keinen Eintrag:
        let mut doppelt = Verlauf::neu("/");
        doppelt.besuchen("/");
        assert!(!doppelt.kann_zurueck());
    }
}
