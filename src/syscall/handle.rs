// syscall/handle.rs — Die PER-PROZESS-Handle-Tabelle (Serie 6, Teil 4)
//
// Das war die letzte offene Lücke aus docs/serie6-bestandsaufnahme.md (b):
// Unsere Socket-Handles waren GLOBAL (eine monoton wachsende Zahl). Solange
// nur der Kernel sie benutzte, war das in Ordnung. Sobald aber mehrere
// unprivilegierte Prozesse laufen, ist es ein Loch: Prozess B könnte das
// Handle von Prozess A ERRATEN und benutzen.
//
// ==========================================================================
// DAS MODELL
//
// Jeder Prozess hat eine EIGENE kleine Tabelle. Ein Handle ist ein INDEX
// darin — eine kleine Zahl (0, 1, 2, 3, ...) wie ein POSIX-Dateideskriptor.
// Dieselbe Zahl bedeutet in jedem Prozess etwas ANDERES, und eine Zahl, die
// im eigenen Prozess nicht belegt ist, ist einfach `UngueltigerHandle`.
//
//   Prozess A: [0]=Eingabe [1]=Ausgabe [2]=Diagnose [3]=Datei(/x) [4]=Socket#7
//   Prozess B: [0]=Eingabe [1]=Ausgabe [2]=Diagnose [3]=Socket#9
//
// Handle 3 ist in A eine Datei und in B ein Socket. B kann A's Datei nicht
// erreichen — es gibt keine Zahl, die dorthin führt. Das GLOBALE Socket-Handle
// (#7) verlässt den Kernel NIE.
//
// Die drei ersten Plätze sind RESERVIERT und immer belegt (Eingabe, Ausgabe,
// Diagnose) — deshalb fängt das erste eigene Handle bei 3 an, und deshalb
// können sie nicht geschlossen werden: Sie gehören dem Kernel, nicht dem
// Prozess.
//
// AUFRÄUMEN BEIM PROZESS-ENDE: `Drop for HandleTabelle` schließt ALLES, was
// noch offen ist — insbesondere Sockets, die sonst als Kernel-Objekte
// weiterlebten (ein echtes Leck: TCP-Verbindungen, die niemand mehr abbaut).
// Weil die Tabelle IM `Prozess` steckt, passiert das automatisch, sobald der
// Aufräum-Task den Prozess fallen lässt. Der Beweis dafür ist ein Test:
// Sockets öffnen, Prozess töten, `socket::anzahl()` muss auf 0 zurückgehen.
// ==========================================================================

use super::{Fehler, SysErgebnis};
use crate::netz::socket;
use alloc::string::String;
use alloc::vec::Vec;

/// Handle 0 — Standard-EINGABE. Reserviert; Lesen ist noch nicht
/// implementiert (`NichtUnterstuetzt`), damit die Nummer nicht später
/// umgedeutet werden muss.
pub const HANDLE_EINGABE: u64 = 0;
/// Handle 1 — Standard-AUSGABE: Bildschirm UND seriell (Projektregel
/// „alle Ausgabe läuft doppelt").
pub const HANDLE_AUSGABE: u64 = 1;
/// Handle 2 — DIAGNOSE-Kanal: NUR seriell.
///
/// Warum getrennt von der Ausgabe: Ein Prozess, der zehntausend Zeilen
/// protokolliert, würde über Handle 1 den Compositor überschwemmen (jede
/// Terminal-Zeile ist ein Fenster-Update). Handle 2 geht am Bildschirm
/// vorbei — genau richtig für Zähler-Schleifen und Debug-Spuren.
pub const HANDLE_DIAGNOSE: u64 = 2;

/// Erstes frei vergebbares Handle.
pub const ERSTES_FREIES: u64 = 3;
/// Höchstzahl offener Handles je Prozess (inklusive der drei reservierten).
pub const MAX_HANDLES: usize = 32;

/// Modus-Bits für `oeffne`.
pub const MODUS_LESEN: u64 = 1;
pub const MODUS_SCHREIBEN: u64 = 2;
pub const MODUS_ANLEGEN: u64 = 4;
pub const MODUS_ABSCHNEIDEN: u64 = 8;
/// Alle definierten Bits — alles andere ist ein ungültiges Argument.
pub const MODUS_ALLE: u64 = MODUS_LESEN | MODUS_SCHREIBEN | MODUS_ANLEGEN | MODUS_ABSCHNEIDEN;

/// Ein Kernel-Objekt, auf das ein Handle zeigt.
///
/// DAS ist die Stelle, an der Kernel-Wissen endet: Der Prozess sieht eine
/// Zahl, der Kernel sieht diesen Enum. Ein Pfad, ein globales Socket-Handle
/// oder ein Kanal — für den Prozess alles „ein Handle".
pub enum KernelObjekt {
    /// Standard-Eingabe (Platzhalter, noch keine Quelle).
    Eingabe,
    /// Bildschirm + seriell.
    Ausgabe,
    /// Nur seriell.
    Diagnose,
    /// Eine offene Datei. Unser VFS ist PFADBASIERT (kein Inode-Handle),
    /// also merkt sich das Objekt den Pfad und das Recht. Die Folge ist
    /// ehrlich dokumentiert: Wird die Datei zwischenzeitlich umbenannt,
    /// zeigt das Handle danach ins Leere (`NichtGefunden`) — POSIX-Semantik
    /// („das Handle hält den Inode") gibt unser VFS nicht her.
    Datei { pfad: String, modus: u64 },
    /// Ein Socket. Das GLOBALE Handle bleibt im Kernel.
    Socket(socket::Handle),
    /// Das LESE-Ende einer Pipe (Serie 6, Teil 6).
    PipeLesen(crate::pipe::PipeId),
    /// Das SCHREIB-Ende einer Pipe.
    PipeSchreiben(crate::pipe::PipeId),
    /// EIN FENSTER, das diesem Prozess gehört (Serie 8).
    ///
    /// Dass es hier steht und nicht in einer eigenen Fenster-Tabelle, ist
    /// die ganze Pointe: Ein Fenster ist damit ein Handle wie jedes andere
    /// — es lässt sich mit `schliesse` schliessen, es ist aus einem
    /// FREMDEN Prozess nicht erreichbar (dort führt keine Zahl dorthin),
    /// und der `Drop` dieser Tabelle räumt es beim Prozess-Ende
    /// automatisch ab. Kein Pfad kann das vergessen.
    Fenster(crate::fenster::FensterId),
}

impl KernelObjekt {
    /// Kurzer Typ-Name für Diagnose-Ausgaben.
    pub fn typ_name(&self) -> &'static str {
        match self {
            KernelObjekt::Eingabe => "Eingabe",
            KernelObjekt::Ausgabe => "Ausgabe",
            KernelObjekt::Diagnose => "Diagnose",
            KernelObjekt::Datei { .. } => "Datei",
            KernelObjekt::Socket(_) => "Socket",
            KernelObjekt::PipeLesen(_) => "Pipe (lesen)",
            KernelObjekt::PipeSchreiben(_) => "Pipe (schreiben)",
            KernelObjekt::Fenster(_) => "Fenster",
        }
    }

    /// Schliesst das dahinterliegende Kernel-Objekt.
    ///
    /// EINE Stelle für alle Wege, auf denen ein Handle verschwindet:
    /// `schliesse`, Ersetzen beim Erben, und der `Drop` beim Prozess-Ende.
    /// Genau deshalb kann kein Pfad das Schliessen einer Pipe vergessen.
    pub fn schliessen(self) {
        match self {
            KernelObjekt::Socket(handle) => {
                let _ = socket::schliessen(handle);
            }
            KernelObjekt::PipeLesen(id) => {
                crate::pipe::ende_schliessen(id, crate::pipe::Ende::Lesen)
            }
            KernelObjekt::PipeSchreiben(id) => {
                crate::pipe::ende_schliessen(id, crate::pipe::Ende::Schreiben)
            }
            // Das Fenster verschwindet vom Bildschirm und sein Pixel-
            // Puffer vom Heap. Laeuft aus TASK-Kontext (der Aufraeum-Task
            // laesst den Prozess fallen), nie im Interrupt — der
            // MANAGER-Lock ist hier also erlaubt.
            KernelObjekt::Fenster(id) => crate::fenster::prozess_fenster_schliessen(id),
            // Dateien sind pfadbasiert, die Standard-Kanäle gehören dem
            // Kernel — da gibt es nichts zu tun.
            KernelObjekt::Eingabe | KernelObjekt::Ausgabe | KernelObjekt::Diagnose => {}
            KernelObjekt::Datei { .. } => {}
        }
    }
}

/// Die Handle-Tabelle eines Prozesses.
pub struct HandleTabelle {
    /// Index = Handle. `None` = freier Platz.
    eintraege: Vec<Option<KernelObjekt>>,
}

impl HandleTabelle {
    /// Eine frische Tabelle mit den drei reservierten Standard-Handles.
    pub fn neu() -> HandleTabelle {
        let mut eintraege = Vec::with_capacity(ERSTES_FREIES as usize);
        eintraege.push(Some(KernelObjekt::Eingabe));
        eintraege.push(Some(KernelObjekt::Ausgabe));
        eintraege.push(Some(KernelObjekt::Diagnose));
        HandleTabelle { eintraege }
    }

    /// Legt ein Objekt ab und liefert sein Handle (immer >= `ERSTES_FREIES`).
    pub fn einfuegen(&mut self, objekt: KernelObjekt) -> Result<u64, Fehler> {
        // Freien Platz wiederverwenden (Handles sind pro Prozess klein und
        // dürfen recyceln — anders als die globalen Socket-Handles, die
        // bewusst NIE recyceln).
        if let Some(index) = self
            .eintraege
            .iter()
            .enumerate()
            .skip(ERSTES_FREIES as usize)
            .find(|(_, platz)| platz.is_none())
            .map(|(index, _)| index)
        {
            self.eintraege[index] = Some(objekt);
            return Ok(index as u64);
        }
        if self.eintraege.len() >= MAX_HANDLES {
            return Err(Fehler::KeineHandlesFrei);
        }
        self.eintraege.push(Some(objekt));
        Ok((self.eintraege.len() - 1) as u64)
    }

    /// Schlägt ein Handle nach. Ein Handle außerhalb der Tabelle, ein
    /// geschlossenes und ein nie vergebenes sind ALLE derselbe Fehler — ein
    /// Prozess soll nicht durch Probieren herausfinden, welche Handles es in
    /// anderen Prozessen gibt.
    pub fn hole(&self, handle: u64) -> Result<&KernelObjekt, Fehler> {
        self.eintraege
            .get(handle as usize)
            .and_then(|platz| platz.as_ref())
            .ok_or(Fehler::UngueltigerHandle)
    }

    /// Entfernt ein Handle und liefert das Objekt zum Schließen.
    /// Die reservierten Standard-Handles gehören dem Kernel und lassen sich
    /// nicht schließen.
    pub fn entfernen(&mut self, handle: u64) -> Result<KernelObjekt, Fehler> {
        if handle < ERSTES_FREIES {
            return Err(Fehler::UngueltigesArgument);
        }
        self.eintraege
            .get_mut(handle as usize)
            .and_then(|platz| platz.take())
            .ok_or(Fehler::UngueltigerHandle)
    }

    /// Ersetzt einen Platz (auch einen reservierten) durch ein anderes
    /// Objekt. Das alte wird sauber geschlossen.
    ///
    /// NUR für die Handle-Weitergabe beim Prozess-Start gedacht: Ein
    /// laufender Prozess kann seine Standard-Handles nicht umbiegen (dafür
    /// gäbe es `dup2` — bewusst nicht in der ABI).
    pub fn ersetzen(&mut self, handle: u64, objekt: KernelObjekt) {
        let index = handle as usize;
        if index >= MAX_HANDLES {
            return;
        }
        while self.eintraege.len() <= index {
            self.eintraege.push(None);
        }
        if let Some(alt) = self.eintraege[index].replace(objekt) {
            alt.schliessen();
        }
    }

    /// Wie viele Handles sind offen (inklusive der drei reservierten)?
    pub fn anzahl_offen(&self) -> usize {
        self.eintraege.iter().filter(|platz| platz.is_some()).count()
    }

    /// Wie viele SOCKETS hält dieser Prozess? (für den Leak-Test)
    pub fn anzahl_sockets(&self) -> usize {
        self.eintraege
            .iter()
            .flatten()
            .filter(|objekt| matches!(objekt, KernelObjekt::Socket(_)))
            .count()
    }

    /// Wie viele FENSTER hält dieser Prozess? (für den Leak-Test)
    pub fn anzahl_fenster(&self) -> usize {
        self.eintraege
            .iter()
            .flatten()
            .filter(|objekt| matches!(objekt, KernelObjekt::Fenster(_)))
            .count()
    }
}

impl Default for HandleTabelle {
    fn default() -> Self {
        Self::neu()
    }
}

impl Drop for HandleTabelle {
    /// DAS AUTOMATISCHE AUFRÄUMEN: Beim Prozess-Ende wird ALLES geschlossen.
    ///
    /// Ohne das wäre jeder abgestürzte Prozess ein Leck — seine TCP-
    /// Verbindungen blieben als Kernel-Objekte liegen, würden Retransmits
    /// senden und irgendwann die Socket-Tabelle füllen. Genau das prüft
    /// `test_handle_leck_ueber_prozess_ende`.
    ///
    /// Läuft aus KERNEL-Kontext (der Aufräum-Task lässt den Prozess fallen),
    /// nie im Interrupt — `socket::schliessen` darf also Locks nehmen.
    fn drop(&mut self) {
        let mut geschlossen = 0usize;
        for objekt in self.eintraege.iter_mut().filter_map(|platz| platz.take()) {
            let zaehlt = matches!(
                objekt,
                KernelObjekt::Socket(_)
                    | KernelObjekt::PipeLesen(_)
                    | KernelObjekt::PipeSchreiben(_)
                    | KernelObjekt::Fenster(_)
            );
            objekt.schliessen();
            if zaehlt {
                geschlossen += 1;
            }
        }
        if geschlossen > 0 {
            crate::serial_println!(
                "[handles] {} Socket(s)/Pipe-Ende(n)/Fenster beim Prozess-Ende automatisch geschlossen.",
                geschlossen
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Der Zugriff aus einem Syscall
// ---------------------------------------------------------------------------

/// Wohin `schreibe` schreibt (aus dem Handle aufgelöst).
pub enum SchreibZiel {
    Konsole,
    Diagnose,
    Datei { pfad: String, modus: u64 },
    Socket(socket::Handle),
    Pipe(crate::pipe::PipeId),
}

/// Woraus `lese` liest (aus dem Handle aufgelöst).
pub enum LeseQuelle {
    /// Standard-Eingabe ohne Umleitung — dafür gibt es (noch) keine Quelle.
    Eingabe,
    Pipe(crate::pipe::PipeId),
    Datei { pfad: String, modus: u64 },
}

/// Löst ein Handle für `lese` auf.
pub fn lese_quelle(handle: u64) -> Result<LeseQuelle, Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(handle)? {
        KernelObjekt::Eingabe => Ok(LeseQuelle::Eingabe),
        KernelObjekt::PipeLesen(id) => Ok(LeseQuelle::Pipe(*id)),
        KernelObjekt::Datei { pfad, modus } => {
            if modus & MODUS_LESEN == 0 {
                return Err(Fehler::NichtUnterstuetzt);
            }
            Ok(LeseQuelle::Datei {
                pfad: pfad.clone(),
                modus: *modus,
            })
        }
        // Auf das SCHREIB-Ende einer Pipe oder auf die Ausgabe zu lesen ist
        // ein Typ-Fehler, kein „ungültig": Das Handle existiert ja.
        _ => Err(Fehler::FalscherHandleTyp),
    })?
}

/// Was ein Kind für einen seiner Standard-Kanäle erben soll.
///
/// `None` = nicht umleiten (das Kind bekommt den Kernel-Standard).
pub type Erbe = Option<KernelObjekt>;

/// Der ABI-Wert, der „nicht umleiten" bedeutet.
///
/// Bewusst `u64::MAX` und nicht 0: **0 ist ein gültiges Handle** (die
/// Standard-Eingabe). Ein Sonderwert, der zufällig auch eine echte Bedeutung
/// hat, ist genau die Sorte Falle, die man später einmal teuer bezahlt.
pub const ERBE_KEINS: u64 = u64::MAX;

/// Löst ein Handle auf, das an ein KIND weitergegeben werden soll.
///
/// Weitergegeben wird eine EIGENSTÄNDIGE Kopie: Bei einer Pipe wird das Ende
/// zusätzlich in Besitz genommen (`ende_uebernehmen`), damit es nicht
/// verschwindet, wenn der Elternteil sein eigenes Handle schliesst. Genau
/// das ist der Grund, warum die Pipe Besitz-ZÄHLER hat und keine Flags.
pub fn erbe_aufloesen(handle: u64) -> Result<Erbe, Fehler> {
    if handle == ERBE_KEINS {
        return Ok(None);
    }
    let objekt = crate::scheduler::mit_handles(|tabelle| match tabelle.hole(handle)? {
        KernelObjekt::PipeLesen(id) => Ok(KernelObjekt::PipeLesen(*id)),
        KernelObjekt::PipeSchreiben(id) => Ok(KernelObjekt::PipeSchreiben(*id)),
        KernelObjekt::Ausgabe => Ok(KernelObjekt::Ausgabe),
        KernelObjekt::Diagnose => Ok(KernelObjekt::Diagnose),
        KernelObjekt::Eingabe => Ok(KernelObjekt::Eingabe),
        // Dateien und Sockets zu vererben wäre möglich, ist aber nicht
        // gebraucht — und was nicht gebraucht wird, kommt nicht in die ABI.
        _ => Err(Fehler::FalscherHandleTyp),
    })??;
    besitz_uebernehmen(&objekt);
    Ok(Some(objekt))
}

/// Nimmt das Kernel-Objekt hinter einem geerbten Handle zusätzlich in Besitz.
pub fn besitz_uebernehmen(objekt: &KernelObjekt) {
    match objekt {
        KernelObjekt::PipeLesen(id) => {
            crate::pipe::ende_uebernehmen(*id, crate::pipe::Ende::Lesen);
        }
        KernelObjekt::PipeSchreiben(id) => {
            crate::pipe::ende_uebernehmen(*id, crate::pipe::Ende::Schreiben);
        }
        _ => {}
    }
}

/// Löst ein Handle für `schreibe` auf — und prüft dabei das Schreibrecht.
///
/// Der Rückgabewert trägt eine KOPIE des Pfades, nicht eine Referenz: Der
/// Tabellen-Lock ist danach wieder frei, und die eigentliche Dateioperation
/// läuft ohne ihn (sonst hielten wir zwei Locks über eine langsame
/// Platten-Operation).
pub fn schreib_ziel(handle: u64) -> Result<SchreibZiel, Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(handle)? {
        KernelObjekt::Ausgabe => Ok(SchreibZiel::Konsole),
        KernelObjekt::Diagnose => Ok(SchreibZiel::Diagnose),
        KernelObjekt::Eingabe => Err(Fehler::NichtUnterstuetzt),
        KernelObjekt::Datei { pfad, modus } => {
            if modus & MODUS_SCHREIBEN == 0 {
                return Err(Fehler::NurLesen);
            }
            Ok(SchreibZiel::Datei {
                pfad: pfad.clone(),
                modus: *modus,
            })
        }
        KernelObjekt::Socket(h) => Ok(SchreibZiel::Socket(*h)),
        KernelObjekt::PipeSchreiben(id) => Ok(SchreibZiel::Pipe(*id)),
        // Auf das LESE-Ende zu schreiben ist ein Typ-Fehler — und in ein
        // Fenster schreibt man mit `fenster_zeichnen`, nicht mit
        // `schreibe`: Pixel sind kein Byte-Strom.
        KernelObjekt::PipeLesen(_) | KernelObjekt::Fenster(_) => Err(Fehler::FalscherHandleTyp),
    })?
}

/// Ersetzt die Standard-Handles 0 und 1 einer FRISCHEN Tabelle durch geerbte
/// Objekte. Nur beim Prozess-Start aufzurufen (das Kind läuft noch nicht).
pub fn standard_erben(tabelle: &mut HandleTabelle, eingabe: Erbe, ausgabe: Erbe) {
    if let Some(objekt) = eingabe {
        tabelle.ersetzen(HANDLE_EINGABE, objekt);
    }
    if let Some(objekt) = ausgabe {
        tabelle.ersetzen(HANDLE_AUSGABE, objekt);
    }
}

/// Löst ein DATEI-Handle auf: liefert (Pfad-Kopie, Modus).
pub fn datei_handle(handle: u64) -> Result<(String, u64), Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(handle)? {
        KernelObjekt::Datei { pfad, modus } => Ok((pfad.clone(), *modus)),
        andere => {
            // Ein Socket ist kein Datei-Handle — und umgekehrt. Das ist ein
            // TYP-Fehler, nicht „ungültig": Das Handle existiert ja.
            let _ = andere;
            Err(Fehler::FalscherHandleTyp)
        }
    })?
}

/// Löst ein SOCKET-Handle auf: liefert das globale Kernel-Handle.
pub fn socket_handle(handle: u64) -> Result<socket::Handle, Fehler> {
    crate::scheduler::mit_handles(|tabelle| match tabelle.hole(handle)? {
        KernelObjekt::Socket(h) => Ok(*h),
        _ => Err(Fehler::FalscherHandleTyp),
    })?
}

/// Legt ein Objekt in der Tabelle des LAUFENDEN Prozesses ab.
pub fn einfuegen_aktuell(objekt: KernelObjekt) -> Result<u64, Fehler> {
    crate::scheduler::mit_handles(|tabelle| tabelle.einfuegen(objekt))?
}

/// `schliesse(handle)` — schließt JEDES Handle (Datei oder Socket).
///
/// Ein einziger Schließ-Syscall für alle Objekt-Arten ist bewusst so: Für
/// den Prozess ist ein Handle ein Handle. Was dahinter zu tun ist (bei einem
/// Socket ein geordneter TCP-Abbau, bei einer Datei nichts), weiß der Kernel.
pub fn sys_schliesse(handle: u64) -> SysErgebnis {
    let objekt = crate::scheduler::mit_handles(|tabelle| tabelle.entfernen(handle))??;
    // Was dahinter zu tun ist, weiss das Objekt selbst — dieselbe Stelle,
    // die auch der Drop der Tabelle benutzt.
    objekt.schliessen();
    Ok(0)
}

// ---------------------------------------------------------------------------
// Tests der reinen Tabellen-Logik
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// Vergabe, Nachschlagen, Schließen — und alle bösartigen Handles.
    #[test_case]
    fn test_handle_tabelle_grundlagen() {
        let mut tabelle = HandleTabelle::neu();
        // Die drei reservierten sind da ...
        assert_eq!(tabelle.anzahl_offen(), 3);
        assert!(matches!(tabelle.hole(HANDLE_EINGABE), Ok(KernelObjekt::Eingabe)));
        assert!(matches!(tabelle.hole(HANDLE_AUSGABE), Ok(KernelObjekt::Ausgabe)));
        assert!(matches!(tabelle.hole(HANDLE_DIAGNOSE), Ok(KernelObjekt::Diagnose)));
        // ... und lassen sich NICHT schließen (sie gehören dem Kernel).
        for reserviert in [HANDLE_EINGABE, HANDLE_AUSGABE, HANDLE_DIAGNOSE] {
            assert_eq!(
                tabelle.entfernen(reserviert).err(),
                Some(Fehler::UngueltigesArgument)
            );
        }

        // Erstes eigenes Handle ist 3.
        let h = tabelle
            .einfuegen(KernelObjekt::Datei {
                pfad: "/test.txt".to_string(),
                modus: MODUS_LESEN,
            })
            .expect("einfuegen");
        assert_eq!(h, ERSTES_FREIES);
        assert_eq!(tabelle.anzahl_offen(), 4);
        assert!(matches!(tabelle.hole(h), Ok(KernelObjekt::Datei { .. })));

        // BÖSARTIG: nie vergebene, weit entfernte und "negative" Handles
        // (als u64 ist -1 = u64::MAX) sind alle derselbe Fehler.
        for boese in [4u64, 31, 32, 1000, u64::MAX, u64::MAX - 1] {
            assert_eq!(tabelle.hole(boese).err(), Some(Fehler::UngueltigerHandle));
            assert_eq!(tabelle.entfernen(boese).err(), Some(Fehler::UngueltigerHandle));
        }

        // Schließen -> danach ist dasselbe Handle ungültig (kein Zugriff auf
        // ein bereits geschlossenes Objekt).
        assert!(tabelle.entfernen(h).is_ok());
        assert_eq!(tabelle.hole(h).err(), Some(Fehler::UngueltigerHandle));
        assert_eq!(tabelle.entfernen(h).err(), Some(Fehler::UngueltigerHandle));
        assert_eq!(tabelle.anzahl_offen(), 3);
    }

    /// Der freie Platz wird wiederverwendet, und bei MAX_HANDLES ist Schluss
    /// (kein unbegrenztes Wachsen durch einen Prozess in der Schleife).
    #[test_case]
    fn test_handle_tabelle_grenzen() {
        let mut tabelle = HandleTabelle::neu();
        let mut vergeben = Vec::new();
        loop {
            match tabelle.einfuegen(KernelObjekt::Diagnose) {
                Ok(h) => vergeben.push(h),
                Err(fehler) => {
                    assert_eq!(fehler, Fehler::KeineHandlesFrei);
                    break;
                }
            }
            assert!(vergeben.len() <= MAX_HANDLES, "Tabelle waechst unbegrenzt");
        }
        // 32 Plätze minus 3 reservierte:
        assert_eq!(vergeben.len(), MAX_HANDLES - ERSTES_FREIES as usize);
        assert_eq!(tabelle.anzahl_offen(), MAX_HANDLES);

        // Eines schließen -> genau dieser Platz wird wiederverwendet.
        let mittendrin = vergeben[5];
        tabelle.entfernen(mittendrin).expect("schliessen");
        let neu = tabelle.einfuegen(KernelObjekt::Diagnose).expect("wiederverwenden");
        assert_eq!(neu, mittendrin, "freier Platz muss wiederverwendet werden");
    }

    /// ISOLATION: Zwei Tabellen vergeben DIESELBEN Zahlen für VERSCHIEDENE
    /// Objekte. Das ist der ganze Punkt der Per-Prozess-Tabelle.
    #[test_case]
    fn test_handles_sind_prozess_lokal() {
        let mut a = HandleTabelle::neu();
        let mut b = HandleTabelle::neu();
        let in_a = a
            .einfuegen(KernelObjekt::Datei {
                pfad: "/geheim-von-a.txt".to_string(),
                modus: MODUS_LESEN,
            })
            .expect("A");
        let in_b = b.einfuegen(KernelObjekt::Diagnose).expect("B");
        // Gleiche Zahl ...
        assert_eq!(in_a, in_b);
        // ... völlig verschiedene Objekte.
        assert_eq!(a.hole(in_a).unwrap().typ_name(), "Datei");
        assert_eq!(b.hole(in_b).unwrap().typ_name(), "Diagnose");
        // Und B kann A's Datei mit KEINER Zahl erreichen: B hat nur 0..3.
        for zahl in 4..MAX_HANDLES as u64 {
            assert_eq!(b.hole(zahl).err(), Some(Fehler::UngueltigerHandle));
        }
    }

    /// Die Modus-Bits: nur definierte Kombinationen sind gültig.
    #[test_case]
    fn test_modus_bits() {
        assert_eq!(MODUS_ALLE, 0b1111);
        // Jede Kombination der definierten Bits ist erlaubt ...
        for modus in 0..=MODUS_ALLE {
            assert_eq!(modus & !MODUS_ALLE, 0);
        }
        // ... alles darüber nicht (das prüft datei::sys_oeffne).
        assert_ne!(16u64 & !MODUS_ALLE, 0);
        assert_ne!(32u64 & !MODUS_ALLE, 0);
    }
}
