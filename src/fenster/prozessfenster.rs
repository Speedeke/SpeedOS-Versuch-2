// fenster/prozessfenster.rs — Ein Fenster, das einem PROZESS gehoert
// (Serie 8, Teil 1: die Naht fuer den Browser)
//
// Bis hierher lebte jedes Fenster im Kernel: Der Compositor rief eine
// Kernel-Funktion, die in den Fenster-Puffer malte. Ein Programm in Ring 3
// konnte gar kein Fenster besitzen — genau die Luecke, die
// docs/serie8-bestandsaufnahme.md als erste zu schliessen empfiehlt.
//
// ==========================================================================
// DIE ENTSCHEIDUNG: PIXELPUFFER PER SYSCALL
//
// Von den drei bewerteten Wegen (Pixelpuffer per Syscall / geteilter Speicher
// / Zeichenbefehle als Protokoll) ist dies der erste. Die Begruendung steht
// vollstaendig in docs/fenster-syscalls.md und kurz hier:
//
//   * Er kostet KEINE Sicherheitszusage. Der Prozess uebergibt Bytes, der
//     Kernel prueft sie mit demselben `copy_in`-Apparat wie jeden anderen
//     Zeiger (Dauerregel I). Bei geteiltem Speicher waere die Seite
//     GLEICHZEITIG in zwei Adressraeumen — dann gilt „pruefen, dann
//     kopieren" nicht mehr, denn der Prozess kann die Daten unter den
//     Haenden des Kernels aendern.
//   * Er verbaut geteilten Speicher NICHT. Die ABI redet ueber (Zeiger,
//     Laenge, Rechteck); ein spaeterer geteilter Puffer waere ein zweiter
//     Weg, dieselben Pixel zu liefern, kein anderer Vertrag.
//   * Das Umstiegskriterium steht VORHER fest (dieselbe Methodik wie die
//     TCP-Reissleine, docs/fenster-syscalls.md §5).
//
// DER PROZESS MALT NUR DEN INHALT. Titelleiste, Rahmen, Schatten, Snap,
// Alt+Tab und der Taskleisten-Eintrag bleiben Sache des Kernels — ein
// Prozess-Fenster verhaelt sich fuer den Benutzer exakt wie ein Kernel-
// Fenster, und ein Programm kann sich nicht als etwas anderes ausgeben
// (keine gefaelschte Titelleiste, kein Fenster ohne Schliessknopf).
//
// ==========================================================================
// DIE EREIGNIS-WARTESCHLANGE — und was daran die eigentliche Arbeit ist
//
// Eingaben entstehen im Interrupt-getriebenen Kernel und werden von einem
// Prozess abgeholt, der vielleicht gerade nicht laeuft. Dazwischen gehoert
// ein Puffer. Die Frage ist nicht „wie baut man eine Queue", sondern
// „WAS PASSIERT, WENN SIE VOLL IST" — und die Antwort darf nicht „das
// Aelteste faellt raus" lauten, denn dann verliert ein beschaeftigtes
// Programm ausgerechnet seinen Schliessen-Wunsch.
//
// Deshalb liegen hier DREI Sorten Zustand nebeneinander:
//
//   (1) DIE QUEUE (feste Kapazitaet) fuer Maus, Tastatur und Fokus. Laeuft
//       sie ueber, faellt das AELTESTE Ereignis heraus und wird GEZAEHLT
//       (`verworfen`) — verlorene Eingabe soll messbar sein, nicht heimlich.
//   (2) `groesse_neu` — ein FELD, kein Queue-Eintrag. Eine Groessenaenderung
//       ist ein ZUSTAND, kein Ereignis: Wer dreimal am Fensterrand zieht,
//       will nicht drei Meldungen, sondern die LETZTE Groesse. Sie wird
//       zusammengefasst und kann nie verloren gehen.
//   (3) `schliessen_gewuenscht` — ebenfalls ein Flag. Der Klick auf das X
//       darf unter keinen Umstaenden in einer vollen Queue verschwinden;
//       ein Fenster, das sich nicht schliessen laesst, ist genau die Sorte
//       Fehler, die ein Benutzer als „haengt" erlebt.
//
// MAUSBEWEGUNGEN WERDEN ZUSAMMENGEFASST: Steht am Ende der Queue schon eine
// Bewegung und kommt die naechste, wird sie ERSETZT. Eine Maus liefert bis
// zu 200 Pakete je Sekunde; ohne das Zusammenfassen bestuende die Queue aus
// Positionen, die niemanden mehr interessieren, und die Klicks dazwischen
// fielen heraus.

use alloc::collections::VecDeque;
use pc_keyboard::{DecodedKey, KeyCode};

// ---------------------------------------------------------------------------
// DIE ABI: wie ein Ereignis im Speicher des Prozesses aussieht
// ---------------------------------------------------------------------------

/// Die Ereignis-Arten. **Diese Zahlen sind ABI** (docs/syscalls.md) — sie
/// duerfen nie umgedeutet werden, nur wachsen.
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

/// Maustasten in der ABI.
pub const KNOPF_LINKS: i32 = 0;
pub const KNOPF_RECHTS: i32 = 1;
pub const KNOPF_MITTE: i32 = 2;

/// Sondertasten in der ABI (alles, was kein Unicode-Zeichen ist).
pub const SONDER_HOCH: i32 = 1;
pub const SONDER_RUNTER: i32 = 2;
pub const SONDER_LINKS: i32 = 3;
pub const SONDER_RECHTS: i32 = 4;
pub const SONDER_POS1: i32 = 5;
pub const SONDER_ENDE: i32 = 6;
pub const SONDER_BILD_HOCH: i32 = 7;
pub const SONDER_BILD_RUNTER: i32 = 8;
pub const SONDER_ENTF: i32 = 9;
/// F1..F12 liegen zusammenhaengend ab 20 — so bleibt Platz fuer weitere
/// Navigationstasten, ohne die Funktionstasten zu verschieben.
pub const SONDER_F1: i32 = 20;

/// Ein Ereignis, wie es im Speicher des Prozesses landet: 16 Byte,
/// vier 32-Bit-Felder, keine Ausrichtungs-Ueberraschungen.
///
/// Die Bedeutung von `x`, `y` und `wert` haengt an `art` — das ist die
/// bewusste Alternative zu einem grossen Struct mit vielen ungenutzten
/// Feldern. Was wo steht, sagt `docs/syscalls.md` und die Konstruktoren
/// unten (die EINZIGE Stelle, an der es zusammengesetzt wird).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EreignisDaten {
    /// Eine der `ART_*`-Konstanten.
    pub art: u32,
    /// Maus: X in FENSTERINHALT-Koordinaten. Groesse: die neue Breite.
    pub x: i32,
    /// Maus: Y in Fensterinhalt-Koordinaten. Groesse: die neue Hoehe.
    pub y: i32,
    /// Maustaste, Rad-Delta, Unicode-Codepoint, Sondertasten-Code oder
    /// Fokus-Flag (1 = bekommen, 0 = verloren).
    pub wert: i32,
}

/// Die Groesse der Ereignis-Struktur in Bytes — ABI, festgenagelt per Test.
pub const EREIGNIS_BYTES: usize = 16;

impl EreignisDaten {
    /// Die Bytes, wie sie der Prozess sieht (Little-Endian, wie x86-64).
    ///
    /// Von Hand statt per `transmute`: Ein `repr(C)`-Struct hat zwar ein
    /// festgelegtes Layout, aber der Kernel soll die Bytes, die er nach
    /// Ring 3 schreibt, AUSDRUECKLICH zusammensetzen — dann steht die ABI
    /// im Code und nicht in einer Annahme ueber den Compiler.
    pub fn bytes(&self) -> [u8; EREIGNIS_BYTES] {
        let mut aus = [0u8; EREIGNIS_BYTES];
        aus[0..4].copy_from_slice(&self.art.to_le_bytes());
        aus[4..8].copy_from_slice(&self.x.to_le_bytes());
        aus[8..12].copy_from_slice(&self.y.to_le_bytes());
        aus[12..16].copy_from_slice(&self.wert.to_le_bytes());
        aus
    }

    pub fn keins() -> Self {
        EreignisDaten::default()
    }

    pub fn maus(art: u32, x: i32, y: i32, wert: i32) -> Self {
        EreignisDaten { art, x, y, wert }
    }

    pub fn taste(zeichen: char) -> Self {
        EreignisDaten {
            art: ART_TASTE,
            x: 0,
            y: 0,
            wert: zeichen as u32 as i32,
        }
    }

    pub fn sondertaste(code: i32) -> Self {
        EreignisDaten {
            art: ART_SONDERTASTE,
            x: 0,
            y: 0,
            wert: code,
        }
    }

    pub fn fokus(bekommen: bool) -> Self {
        EreignisDaten {
            art: ART_FOKUS,
            x: 0,
            y: 0,
            wert: bekommen as i32,
        }
    }

    pub fn groesse(breite: i32, hoehe: i32) -> Self {
        EreignisDaten {
            art: ART_GROESSE,
            x: breite,
            y: hoehe,
            wert: 0,
        }
    }

    pub fn schliessen() -> Self {
        EreignisDaten {
            art: ART_SCHLIESSEN,
            x: 0,
            y: 0,
            wert: 0,
        }
    }
}

/// Uebersetzt eine dekodierte Taste in ein ABI-Ereignis.
///
/// `None` bedeutet „diese Taste hat in der ABI keine Entsprechung" — sie
/// wird dann schlicht nicht zugestellt. Das ist die ehrliche Variante:
/// Lieber gar nichts als eine erfundene Zahl, die spaeter jemand als
/// Bedeutung missversteht. Wer eine Taste ergaenzt, traegt sie HIER und in
/// docs/syscalls.md ein — die Codes sind ABI.
pub fn taste_uebersetzen(taste: DecodedKey) -> Option<EreignisDaten> {
    match taste {
        DecodedKey::Unicode(zeichen) => Some(EreignisDaten::taste(zeichen)),
        DecodedKey::RawKey(code) => {
            let sonder = match code {
                KeyCode::ArrowUp => SONDER_HOCH,
                KeyCode::ArrowDown => SONDER_RUNTER,
                KeyCode::ArrowLeft => SONDER_LINKS,
                KeyCode::ArrowRight => SONDER_RECHTS,
                KeyCode::Home => SONDER_POS1,
                KeyCode::End => SONDER_ENDE,
                KeyCode::PageUp => SONDER_BILD_HOCH,
                KeyCode::PageDown => SONDER_BILD_RUNTER,
                KeyCode::Delete => SONDER_ENTF,
                KeyCode::F1 => SONDER_F1,
                KeyCode::F2 => SONDER_F1 + 1,
                KeyCode::F3 => SONDER_F1 + 2,
                KeyCode::F4 => SONDER_F1 + 3,
                KeyCode::F5 => SONDER_F1 + 4,
                KeyCode::F6 => SONDER_F1 + 5,
                KeyCode::F7 => SONDER_F1 + 6,
                KeyCode::F8 => SONDER_F1 + 7,
                KeyCode::F9 => SONDER_F1 + 8,
                KeyCode::F10 => SONDER_F1 + 9,
                KeyCode::F11 => SONDER_F1 + 10,
                KeyCode::F12 => SONDER_F1 + 11,
                // Umschalt, Strg, Alt und alles Uebrige: nicht zustellen.
                _ => return None,
            };
            Some(EreignisDaten::sondertaste(sonder))
        }
    }
}

// ---------------------------------------------------------------------------
// Der Fenster-Zustand auf der Kernel-Seite
// ---------------------------------------------------------------------------

/// Wie viele Eingabe-Ereignisse hoechstens warten. Bei 200 Maus-Paketen je
/// Sekunde und zusammengefassten Bewegungen sind 64 reichlich: Ein Programm,
/// das das ueberlaeuft, holt seine Ereignisse laenger als eine Sekunde nicht
/// ab — dann ist ohnehin nicht die Queue das Problem.
pub const MAX_EREIGNISSE: usize = 64;

/// Ein Fenster, dessen Inhalt ein Ring-3-Prozess malt.
pub struct ProzessFenster {
    /// Wem gehoert es? Nur dieser Prozess darf hineinzeichnen — geprueft
    /// wird das ueber die Handle-Tabelle (er hat gar keine andere Zahl, mit
    /// der er es erreichen koennte), diese PID ist die Diagnose-Sicht.
    pub besitzer: crate::prozess::Pid,
    /// Eingabe-Ereignisse (Maus, Tastatur, Fokus).
    warteschlange: VecDeque<EreignisDaten>,
    /// Wie viele Ereignisse die volle Queue verworfen hat. Sichtbar, damit
    /// verlorene Eingabe messbar ist statt heimlich.
    pub verworfen: u64,
    /// Die zuletzt gesetzte Groesse, solange der Prozess sie nicht abgeholt
    /// hat. ZUSAMMENGEFASST — der letzte Wert gewinnt.
    groesse_neu: Option<(i32, i32)>,
    /// Der Benutzer will das Fenster schliessen (X-Knopf). Ein FLAG, damit
    /// es in keiner vollen Queue verloren geht.
    schliessen_gewuenscht: bool,
    /// Wurde ueberhaupt schon einmal um das Schliessen gebeten? Wird
    /// NICHT zurueckgesetzt, wenn das Ereignis abgeholt wird — daran
    /// erkennt der zweite Klick, dass er erzwingen darf.
    schliessen_gebeten: bool,
    /// Bis wann wartet der Besitzer gerade auf ein Ereignis (ms seit Boot,
    /// 0 = wartet nicht)?
    ///
    /// Der Wert liegt HIER und nicht im Syscall, weil ein blockierender
    /// Syscall bei SpeedOS NEU GESTARTET wird (Serie 6, Teil 6): Beim
    /// zweiten Durchlauf waere die Frist sonst wieder von vorn berechnet und
    /// eine Frist von 100 ms koennte ewig dauern. Gesetzt wird er nur, wenn
    /// er noch 0 ist — der Neustart aendert damit nichts, wie die
    /// Neustart-Regel es verlangt.
    pub frist_bis_ms: u64,
}

impl ProzessFenster {
    pub fn neu(besitzer: crate::prozess::Pid) -> Self {
        ProzessFenster {
            besitzer,
            warteschlange: VecDeque::new(),
            verworfen: 0,
            groesse_neu: None,
            schliessen_gewuenscht: false,
            schliessen_gebeten: false,
            frist_bis_ms: 0,
        }
    }

    /// Legt ein Eingabe-Ereignis ab.
    ///
    /// Zwei Sonderbehandlungen, beide oben begruendet: Bewegungen werden am
    /// Ende der Queue ZUSAMMENGEFASST, und beim Ueberlauf faellt das
    /// AELTESTE heraus (und wird gezaehlt).
    pub fn ereignis_ablegen(&mut self, ereignis: EreignisDaten) {
        if ereignis.art == ART_MAUS_BEWEGT {
            if let Some(letztes) = self.warteschlange.back_mut() {
                if letztes.art == ART_MAUS_BEWEGT {
                    *letztes = ereignis;
                    return;
                }
            }
        }
        if self.warteschlange.len() >= MAX_EREIGNISSE {
            self.warteschlange.pop_front();
            self.verworfen += 1;
        }
        self.warteschlange.push_back(ereignis);
    }

    /// Meldet eine neue Inhaltsgroesse (zusammengefasst).
    pub fn groesse_melden(&mut self, breite: i32, hoehe: i32) {
        self.groesse_neu = Some((breite, hoehe));
    }

    /// Der Benutzer hat auf das X geklickt. Liefert `true`, wenn das
    /// Schliessen ERZWUNGEN werden soll.
    ///
    /// EIN PROZESS-FENSTER WIRD NICHT EINFACH ZUGEMACHT: Der Prozess
    /// besitzt den Puffer und will vielleicht vorher noch etwas sichern.
    /// Also ist der erste Klick eine BITTE (das Ereignis `Schliessen`).
    ///
    /// Aber ein Fenster, das sich nicht schliessen laesst, weil sein
    /// Programm haengt oder die Bitte einfach ignoriert, ist fuer den
    /// Benutzer ein defektes System. Deshalb der ZWEITE Klick: Wurde
    /// schon einmal gebeten und das Fenster ist immer noch da, wird
    /// geschlossen. Kein Zeitgeber, keine Frist, die man erklaeren muss —
    /// „nochmal klicken" ist das, was ein Mensch ohnehin tut.
    pub fn schliessen_wuenschen(&mut self) -> bool {
        let erzwingen = self.schliessen_gebeten;
        self.schliessen_gebeten = true;
        self.schliessen_gewuenscht = true;
        erzwingen
    }

    /// Liegt irgendetwas an? (Der Weck-Test des Schedulers.)
    pub fn hat_ereignis(&self) -> bool {
        self.groesse_neu.is_some() || self.schliessen_gewuenscht || !self.warteschlange.is_empty()
    }

    /// Holt das naechste Ereignis ab — `None`, wenn nichts anliegt.
    ///
    /// DIE REIHENFOLGE IST EINE ENTSCHEIDUNG:
    ///   1. GROESSE zuerst. Wer danach zeichnet, zeichnet in der richtigen
    ///      Groesse; andersherum male ein Programm erst einmal falsch.
    ///   2. SCHLIESSEN als Zweites. Es soll ankommen, auch wenn dauernd
    ///      Eingaben nachlaufen — sonst waere „Fenster zu" von der
    ///      Maus-Aktivitaet abhaengig.
    ///   3. Dann die Eingaben in ihrer Reihenfolge.
    pub fn ereignis_holen(&mut self) -> Option<EreignisDaten> {
        if let Some((breite, hoehe)) = self.groesse_neu.take() {
            return Some(EreignisDaten::groesse(breite, hoehe));
        }
        if self.schliessen_gewuenscht {
            self.schliessen_gewuenscht = false;
            return Some(EreignisDaten::schliessen());
        }
        self.warteschlange.pop_front()
    }

    /// Wie viele Eingabe-Ereignisse warten? (Diagnose und Tests.)
    pub fn anzahl_wartend(&self) -> usize {
        self.warteschlange.len()
    }
}

// ---------------------------------------------------------------------------
// Tests der reinen Warteschlangen-Logik
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Die ABI-Zahlen und das Byte-Layout sind ein VERSPRECHEN an schon
    /// uebersetzte Programme — wer sie verschiebt, bricht diesen Test.
    #[test_case]
    fn test_ereignis_abi_stabil() {
        assert_eq!(EREIGNIS_BYTES, 16);
        assert_eq!(core::mem::size_of::<EreignisDaten>(), EREIGNIS_BYTES);
        assert_eq!(ART_KEINS, 0);
        assert_eq!(ART_MAUS_BEWEGT, 1);
        assert_eq!(ART_MAUS_AB, 2);
        assert_eq!(ART_MAUS_AUF, 3);
        assert_eq!(ART_MAUS_RAD, 4);
        assert_eq!(ART_TASTE, 5);
        assert_eq!(ART_SONDERTASTE, 6);
        assert_eq!(ART_FOKUS, 7);
        assert_eq!(ART_GROESSE, 8);
        assert_eq!(ART_SCHLIESSEN, 9);

        // Und die Bytes stehen wirklich da, wo die ABI sie verspricht:
        let e = EreignisDaten::maus(ART_MAUS_AB, 0x1234, -2, KNOPF_RECHTS);
        let b = e.bytes();
        assert_eq!(u32::from_le_bytes([b[0], b[1], b[2], b[3]]), ART_MAUS_AB);
        assert_eq!(i32::from_le_bytes([b[4], b[5], b[6], b[7]]), 0x1234);
        assert_eq!(i32::from_le_bytes([b[8], b[9], b[10], b[11]]), -2);
        assert_eq!(i32::from_le_bytes([b[12], b[13], b[14], b[15]]), KNOPF_RECHTS);
        // Ein Unicode-Zeichen ueberlebt die Runde (auch ueber ASCII hinaus):
        assert_eq!(EreignisDaten::taste('ä').wert, 'ä' as u32 as i32);
    }

    /// Mausbewegungen werden zusammengefasst — sonst besteht die Queue aus
    /// Positionen, die niemanden mehr interessieren, und die Klicks
    /// dazwischen fallen heraus.
    #[test_case]
    fn test_bewegungen_werden_zusammengefasst() {
        let mut f = ProzessFenster::neu(1);
        for i in 0..100 {
            f.ereignis_ablegen(EreignisDaten::maus(ART_MAUS_BEWEGT, i, i, 0));
        }
        assert_eq!(f.anzahl_wartend(), 1, "Bewegungen muessen verschmelzen");
        assert_eq!(f.verworfen, 0, "dabei darf nichts verworfen werden");

        // Ein Klick dazwischen trennt sie: davor eine Bewegung, dann der
        // Klick, dann wieder EINE Bewegung.
        f.ereignis_ablegen(EreignisDaten::maus(ART_MAUS_AB, 5, 5, KNOPF_LINKS));
        for i in 0..10 {
            f.ereignis_ablegen(EreignisDaten::maus(ART_MAUS_BEWEGT, i, i, 0));
        }
        assert_eq!(f.anzahl_wartend(), 3);
        assert_eq!(f.ereignis_holen().unwrap().art, ART_MAUS_BEWEGT);
        assert_eq!(f.ereignis_holen().unwrap().art, ART_MAUS_AB);
        let letzte = f.ereignis_holen().unwrap();
        assert_eq!(letzte.art, ART_MAUS_BEWEGT);
        assert_eq!(letzte.x, 9, "die JUENGSTE Bewegung muss gewinnen");
        assert!(f.ereignis_holen().is_none());
    }

    /// DIE WICHTIGE ZUSAGE: Eine volle Warteschlange darf den Schliessen-
    /// Wunsch und die Groesse NICHT verschlucken. Ein Fenster, das sich
    /// nicht schliessen laesst, erlebt ein Benutzer als „haengt".
    #[test_case]
    fn test_volle_queue_verliert_nie_schliessen_und_groesse() {
        let mut f = ProzessFenster::neu(1);
        // Weit ueber die Kapazitaet hinaus fluten (keine Bewegungen, die
        // wuerden ja verschmelzen).
        for i in 0..(MAX_EREIGNISSE * 3) {
            f.ereignis_ablegen(EreignisDaten::taste((b'a' + (i % 26) as u8) as char));
        }
        assert_eq!(f.anzahl_wartend(), MAX_EREIGNISSE, "die Queue ist gedeckelt");
        assert_eq!(
            f.verworfen,
            (MAX_EREIGNISSE * 3 - MAX_EREIGNISSE) as u64,
            "verlorene Eingabe muss GEZAEHLT werden, nicht heimlich sein"
        );

        // Jetzt Groesse und Schliessen — beide muessen ankommen, und zwar
        // VOR den aufgestauten Tasten.
        f.groesse_melden(320, 200);
        f.groesse_melden(640, 480);
        assert!(!f.schliessen_wuenschen(), "der ERSTE Klick bittet nur");

        let erstes = f.ereignis_holen().unwrap();
        assert_eq!(erstes.art, ART_GROESSE);
        assert_eq!((erstes.x, erstes.y), (640, 480), "die LETZTE Groesse gilt");
        assert_eq!(f.ereignis_holen().unwrap().art, ART_SCHLIESSEN);
        assert_eq!(f.ereignis_holen().unwrap().art, ART_TASTE);

        // Der Schliessen-Wunsch wird genau EINMAL geliefert.
        while let Some(e) = f.ereignis_holen() {
            assert_ne!(e.art, ART_SCHLIESSEN);
            assert_ne!(e.art, ART_GROESSE);
        }
        assert!(!f.hat_ereignis());

        // Und der ZWEITE Klick auf das X erzwingt: Ein Fenster, das sich
        // nicht schliessen laesst, waere fuer den Benutzer ein defektes
        // System — auch wenn der Prozess nur die Bitte ignoriert hat.
        assert!(f.schliessen_wuenschen(), "der zweite Klick muss erzwingen");
    }

    /// Leer heisst leer — und `hat_ereignis` ist genau dann wahr, wenn
    /// `ereignis_holen` etwas liefert (daran haengt der Weckruf).
    #[test_case]
    fn test_hat_ereignis_stimmt_mit_holen_ueberein() {
        let mut f = ProzessFenster::neu(7);
        assert!(!f.hat_ereignis());
        assert!(f.ereignis_holen().is_none());

        for aufbau in [0, 1, 2] {
            match aufbau {
                0 => f.ereignis_ablegen(EreignisDaten::fokus(true)),
                1 => f.groesse_melden(100, 100),
                _ => {
                    f.schliessen_wuenschen();
                }
            }
            assert!(f.hat_ereignis(), "Aufbau {} muss anliegen", aufbau);
            assert!(f.ereignis_holen().is_some());
            assert!(!f.hat_ereignis(), "Aufbau {} muss danach leer sein", aufbau);
        }
    }
}
