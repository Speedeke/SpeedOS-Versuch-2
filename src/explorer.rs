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
use crate::grafik::{Icon, Rechteck, Zeichner};
use crate::ui::widgets::{Button, Label, ListenEintrag, ScrollListe, Trennlinie};
use crate::ui::{hbox, vbox, App, AppReaktion, Fueller, UiEreignis, UiReaktion, Widget};
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
const N_BREADCRUMB: u32 = 100; // + Segment-Index
const N_LISTE_AUSWAHL: u32 = 1000; // + Eintrag-Index
const N_LISTE_OEFFNEN: u32 = 100_000; // + Eintrag-Index (Doppelklick/Enter)
const N_BAUM: u32 = 200_000; // + Baumzeilen-Index

/// Ein Eintrag der Dateiliste (aus dem VFS geladen und sortiert).
#[derive(Clone)]
struct DateiEintrag {
    name: String,
    typ: NodeTyp,
    groesse: usize,
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
    fn wunschgroesse(&self) -> (i32, i32) {
        (0, 0)
    }
    fn flex(&self) -> i32 {
        1
    }
    fn zeichnen(&self, _z: &mut Zeichner<'_, crate::fenster::FensterPuffer>, _b: Rechteck) {}
    fn ereignis(&mut self, ereignis: &UiEreignis, bereich: Rechteck) -> UiReaktion {
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
    /// Abgeleiteter Zustand (neu_laden):
    eintraege: Vec<DateiEintrag>,
    baum_zeilen: Vec<BaumZeile>,
    fehler: Option<String>,
}

impl ExplorerApp {
    pub fn neu() -> Self {
        let mut app = ExplorerApp {
            verlauf: Verlauf::neu("/"),
            aufgeklappt: BTreeSet::new(),
            auswahl: None,
            adress_modus: false,
            adress_puffer: String::new(),
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
                    .map(|e| DateiEintrag { name: e.name, typ: e.typ, groesse: e.groesse })
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

    /// Der Icon fürs Dateilisten-/Baum-Element.
    fn eintrag_icon(typ: NodeTyp) -> &'static Icon {
        match typ {
            NodeTyp::Verzeichnis => &crate::grafik::ICON_ORDNER,
            NodeTyp::Datei => &crate::grafik::ICON_DATEI,
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
        let mut leiste: Vec<Box<dyn Widget>> = vec![
            Box::new(Button::neu("<", N_ZURUECK)),
            Box::new(Button::neu(">", N_VOR)),
            Box::new(Button::neu("^", N_HOCH)),
        ];
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
            .map(|e| ListenEintrag {
                icon: Some(Self::eintrag_icon(e.typ)),
                text: match e.typ {
                    NodeTyp::Verzeichnis => e.name.clone(),
                    NodeTyp::Datei => format!("{}  ({})", e.name, groesse_formatieren(e.groesse)),
                },
            })
            .collect();
        let liste = ScrollListe::mit_index_nachrichten(listen_eintraege, N_LISTE_AUSWAHL, N_LISTE_OEFFNEN)
            .mit_auswahl(self.auswahl)
            .mit_fokus(!self.adress_modus);

        // --- Statusleiste ---
        let status = match (&self.fehler, self.auswahl) {
            (Some(fehler), _) => format!("Fehler: {}", fehler),
            (None, Some(index)) => {
                let e = &self.eintraege[index];
                match e.typ {
                    NodeTyp::Verzeichnis => {
                        format!("{} Eintraege  |  {} (Ordner)", self.eintraege.len(), e.name)
                    }
                    NodeTyp::Datei => format!(
                        "{} Eintraege  |  {} ({})",
                        self.eintraege.len(),
                        e.name,
                        groesse_formatieren(e.groesse)
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
        match id {
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
                let index = (id - N_LISTE_OEFFNEN) as usize;
                if let Some(eintrag) = self.eintraege.get(index) {
                    if eintrag.typ == NodeTyp::Verzeichnis {
                        let ziel = if self.pfad() == "/" {
                            format!("/{}", eintrag.name)
                        } else {
                            format!("{}/{}", self.pfad(), eintrag.name)
                        };
                        self.navigieren(&ziel);
                    }
                    // Dateien öffnen kommt in Teil 2.
                }
            }
            id if id >= N_BAUM => {
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

    /// App-Shortcuts + Adress-Eingabemodus (siehe Kopfkommentar).
    fn taste(&mut self, taste: DecodedKey) -> Option<AppReaktion> {
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
        // Backspace = einen Ordner hoch (klassischer Explorer-Shortcut).
        if matches!(taste, DecodedKey::Unicode('\u{8}') | DecodedKey::Unicode('\u{7f}')) {
            let eltern = eltern_pfad(self.pfad());
            self.navigieren(&eltern);
            return Some(AppReaktion::neu_aufbauen());
        }
        None // alles andere an die Widgets (Pfeile/Enter -> Dateiliste)
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
            DateiEintrag { name: String::from("zebra.txt"), typ: NodeTyp::Datei, groesse: 1 },
            DateiEintrag { name: String::from("Beta"), typ: NodeTyp::Verzeichnis, groesse: 0 },
            DateiEintrag { name: String::from("alpha.txt"), typ: NodeTyp::Datei, groesse: 1 },
            DateiEintrag { name: String::from("anton"), typ: NodeTyp::Verzeichnis, groesse: 0 },
        ];
        sortieren(&mut eintraege);
        let namen: Vec<&str> = eintraege.iter().map(|e| e.name.as_str()).collect();
        // Ordner zuerst (anton < Beta, case-insensitiv), dann Dateien:
        assert_eq!(namen, vec!["anton", "Beta", "alpha.txt", "zebra.txt"]);
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
