// virtio/virtqueue.rs — Die Split-Virtqueue (DIE Blaupause für virtio-net)
//
// Eine Virtqueue ist der Ringpuffer-Mechanismus, über den ALLE virtio-
// Geräte mit dem Treiber sprechen — egal ob Block (virtio-blk),
// Netzwerk (virtio-net), Konsole oder GPU. Dieses Modul ist deshalb
// bewusst GERÄTE- UND TRANSPORT-UNABHÄNGIG: Es verwaltet nur die drei
// Ringe im Speicher. WIE die Register des Geräts angesprochen werden
// (Legacy-Port-I/O oder Modern-MMIO) und WIE man das Gerät "anstößt"
// (Queue-Notify) macht der jeweilige Treiber — hier gibt es davon
// nichts. virtio-net (Serie 5) benutzt exakt dieses Modul weiter.
//
// AUFBAU einer "Split-Virtqueue" (die klassische Variante) — drei
// Teile in EINEM physisch zusammenhängenden Speicherblock:
//
//   1. DESKRIPTOR-TABELLE (groesse Einträge à 16 Byte):
//      Jeder Deskriptor beschreibt EINEN Puffer (physische Adresse +
//      Länge + Flags + Verkettung). Mehrere Deskriptoren bilden per
//      `next`-Feld eine KETTE (z. B. Header + Daten + Status bei
//      virtio-blk).
//
//   2. AVAILABLE-RING (Treiber -> Gerät):
//      Hier legt der TREIBER die Köpfe der Deskriptor-Ketten ab, die
//      das Gerät bearbeiten soll ("hier ist Arbeit für dich").
//
//   3. USED-RING (Gerät -> Treiber):
//      Hier legt das GERÄT die erledigten Ketten ab ("fertig, das
//      Ergebnis steht in deinem Puffer"). Muss auf 4096 ausgerichtet
//      sein (Legacy-Vorgabe).
//
// DER ABLAUF eines Requests (Treiber-Sicht):
//   a) Puffer-Ketten in freie Deskriptoren schreiben (kette_anlegen),
//   b) Kopf in den Available-Ring legen + avail.idx erhöhen
//      (verfuegbar_machen),
//   c) [Treiber stößt das Gerät an — NICHT hier, transportabhängig],
//   d) warten, bis used.idx sich bewegt, Ergebnis abholen und die
//      Deskriptoren wieder freigeben (used_abholen).
//
// SPEICHER-BARRIEREN: Da Treiber UND Gerät gleichzeitig auf diese
// Ringe zugreifen, brauchen wir Fences an den Übergabepunkten — sonst
// könnte die CPU (oder der Compiler) Schreib-/Lese-Reihenfolgen
// vertauschen und das Gerät liest halbfertige Ringe. Auf x86 sind das
// vor allem Compiler-Barrieren, aber wir setzen sie explizit und
// korrekt (das gilt dann auch auf schwächeren Architekturen).

use crate::memory;
use core::sync::atomic::{fence, Ordering};
use x86_64::{PhysAddr, VirtAddr};

/// Deskriptor-Flag: Es folgt ein weiterer Deskriptor (`next` gilt).
pub const F_NEXT: u16 = 1;
/// Deskriptor-Flag: Das GERÄT schreibt in diesen Puffer (sonst liest es).
pub const F_WRITE: u16 = 2;

/// Legacy-Ausrichtung: Der Used-Ring beginnt an einer 4096er-Grenze.
const AUSRICHTUNG: usize = 4096;

/// Ein einzelner Deskriptor (16 Byte, `#[repr(C)]` = exakt das
/// On-Wire-Layout, das die Hardware erwartet).
#[repr(C)]
#[derive(Clone, Copy)]
struct Deskriptor {
    /// Physische Adresse des Puffers (die Hardware liest per DMA).
    addr: u64,
    laenge: u32,
    flags: u16,
    /// Nächster Deskriptor in der Kette (nur gültig bei F_NEXT).
    next: u16,
}

/// Rundet `wert` auf das nächste Vielfache von `ausrichtung` auf.
/// Reine Funktion — unten unit-getestet.
fn aufrunden(wert: usize, ausrichtung: usize) -> usize {
    wert.div_ceil(ausrichtung) * ausrichtung
}

/// Byte-Offset des Available-Rings (direkt hinter der Deskriptor-Tabelle).
fn avail_offset(groesse: u16) -> usize {
    16 * groesse as usize
}

/// Byte-Offset des Used-Rings: hinter Deskriptoren + Available-Ring,
/// aufgerundet auf die 4096er-Grenze (Legacy-Vorgabe). Der Available-
/// Ring ist 6 + 2*groesse Bytes groß (flags, idx, ring[groesse],
/// used_event — alles u16).
fn used_offset(groesse: u16) -> usize {
    aufrunden(avail_offset(groesse) + 6 + 2 * groesse as usize, AUSRICHTUNG)
}

/// Gesamtgröße der Virtqueue in Bytes. Der Used-Ring ist
/// 6 + 8*groesse Bytes groß (flags, idx, ring[groesse] à 8 Byte,
/// avail_event).
fn gesamt_groesse(groesse: u16) -> usize {
    used_offset(groesse) + aufrunden(6 + 8 * groesse as usize, AUSRICHTUNG)
}

/// Eine fertig aufgesetzte Split-Virtqueue.
pub struct Virtqueue {
    /// Virtuelle Basis des zusammenhängenden Speicherblocks.
    basis: VirtAddr,
    /// Physische Basis — das braucht das Gerät (Queue-Address-Register).
    phys: PhysAddr,
    /// Anzahl Deskriptoren (= vom Gerät gemeldete Queue-Größe).
    groesse: u16,
    /// Kopf der Freiliste (Index eines freien Deskriptors) bzw.
    /// `groesse`, wenn nichts mehr frei ist.
    frei_kopf: u16,
    /// Wie viele Deskriptoren gerade frei sind.
    anzahl_frei: u16,
    /// Zuletzt verarbeiteter Used-Ring-Index (steigt monoton, wird
    /// per groesse "gewickelt" beim Ring-Zugriff).
    letzter_used: u16,
}

impl Virtqueue {
    /// Legt eine Virtqueue der gegebenen Größe an (physisch
    /// zusammenhängend, genullt). `groesse` muss eine Zweierpotenz
    /// sein (virtio-Vorgabe) — das prüft der Aufrufer (der Treiber
    /// liest sie vom Gerät). Die Freiliste verkettet anfangs alle
    /// Deskriptoren.
    pub fn neu(groesse: u16) -> Virtqueue {
        assert!(groesse.is_power_of_two(), "Queue-Groesse muss Zweierpotenz sein");
        let bytes = gesamt_groesse(groesse);
        let pages = bytes.div_ceil(4096);
        let basis = memory::allocate_pages(pages).expect("Virtqueue-Speicher");
        let phys = memory::uebersetzen(basis).expect("Virtqueue-Physik");

        // Frisch allozierte Frames können Müll enthalten — die Ringe
        // MÜSSEN genullt starten (avail.idx=0, used.idx=0, flags=0):
        // unsafe: `basis` gehört uns exklusiv (frisch alloziert),
        // `bytes` liegt vollständig im allozierten Bereich.
        unsafe {
            core::ptr::write_bytes(basis.as_mut_ptr::<u8>(), 0, bytes);
        }

        let vq = Virtqueue {
            basis,
            phys,
            groesse,
            frei_kopf: 0,
            anzahl_frei: groesse,
            letzter_used: 0,
        };
        // Freiliste aufbauen: Deskriptor i zeigt per `next` auf i+1,
        // der letzte auf `groesse` (= "Ende").
        for i in 0..groesse {
            let d = vq.deskriptor_ptr(i);
            // unsafe: i < groesse, der Zeiger liegt in der Tabelle.
            unsafe {
                (*d).next = if i + 1 < groesse { i + 1 } else { groesse };
            }
        }
        vq
    }

    /// Physische Basis-Adresse (für das Queue-Address-Register des
    /// Geräts). Bei Legacy-virtio gibt der Treiber davon die
    /// Page-Nummer (phys >> 12) ins Register.
    pub fn phys_basis(&self) -> PhysAddr {
        self.phys
    }

    pub fn groesse(&self) -> u16 {
        self.groesse
    }

    // --- Roh-Zeiger auf die einzelnen Ring-Felder ------------------------
    // Alle Zugriffe laufen über read_volatile/write_volatile, weil das
    // Gerät dieselben Bytes per DMA liest/schreibt — der Compiler darf
    // hier NICHTS wegoptimieren oder umordnen.

    fn deskriptor_ptr(&self, index: u16) -> *mut Deskriptor {
        // unsafe: index < groesse (Aufrufer), Zeiger bleibt in der Tabelle.
        unsafe {
            self.basis
                .as_mut_ptr::<Deskriptor>()
                .add(index as usize)
        }
    }

    fn avail_idx_ptr(&self) -> *mut u16 {
        // unsafe: idx-Feld liegt 2 Byte hinter flags.
        unsafe { self.basis.as_mut_ptr::<u8>().add(avail_offset(self.groesse) + 2) as *mut u16 }
    }

    fn avail_ring_ptr(&self, slot: u16) -> *mut u16 {
        // ring[] beginnt 4 Byte hinter avail_offset (flags + idx).
        // unsafe: slot < groesse -> im Available-Ring.
        unsafe {
            self.basis
                .as_mut_ptr::<u8>()
                .add(avail_offset(self.groesse) + 4 + 2 * slot as usize) as *mut u16
        }
    }

    fn used_idx_ptr(&self) -> *mut u16 {
        // unsafe: used_offset + 2 (hinter used.flags).
        unsafe { self.basis.as_mut_ptr::<u8>().add(used_offset(self.groesse) + 2) as *mut u16 }
    }

    /// Zeiger auf das Used-Ring-Element `slot`: {id: u32, laenge: u32}.
    fn used_element_ptr(&self, slot: u16) -> *mut u32 {
        // ring[] beginnt 4 Byte hinter used_offset (flags + idx),
        // jedes Element ist 8 Byte groß.
        // unsafe: slot < groesse -> im Used-Ring.
        unsafe {
            self.basis
                .as_mut_ptr::<u8>()
                .add(used_offset(self.groesse) + 4 + 8 * slot as usize) as *mut u32
        }
    }

    /// Legt eine DESKRIPTOR-KETTE aus den gegebenen Puffern an und
    /// liefert den Kopf-Index. Jeder Puffer ist (physische Adresse,
    /// Länge, geraet_schreibt): geraet_schreibt=true setzt F_WRITE
    /// (das Gerät schreibt in diesen Puffer, z. B. gelesene Daten
    /// oder das Status-Byte). Reicht die Freiliste nicht, kommt None
    /// zurück (der Treiber wartet dann auf freie Deskriptoren).
    pub fn kette_anlegen(&mut self, puffer: &[(PhysAddr, u32, bool)]) -> Option<u16> {
        if puffer.is_empty() || puffer.len() > self.anzahl_frei as usize {
            return None;
        }
        let kopf = self.frei_kopf;
        let mut aktuell = kopf;
        for (i, &(addr, laenge, geraet_schreibt)) in puffer.iter().enumerate() {
            let d = self.deskriptor_ptr(aktuell);
            let letzter = i + 1 == puffer.len();
            let mut flags = 0u16;
            if geraet_schreibt {
                flags |= F_WRITE;
            }
            if !letzter {
                flags |= F_NEXT;
            }
            // unsafe: `aktuell` ist ein gültiger, freier Deskriptor-Index.
            let naechster_frei = unsafe { (*d).next };
            // unsafe: Wir beschreiben genau diesen Deskriptor. Volatile,
            // weil das Gerät ihn später per DMA liest.
            unsafe {
                core::ptr::write_volatile(&mut (*d).addr, addr.as_u64());
                core::ptr::write_volatile(&mut (*d).laenge, laenge);
                core::ptr::write_volatile(&mut (*d).flags, flags);
                if !letzter {
                    core::ptr::write_volatile(&mut (*d).next, naechster_frei);
                }
            }
            if !letzter {
                aktuell = naechster_frei;
            } else {
                // Freiliste hinter dem letzten Kettenglied fortsetzen:
                self.frei_kopf = naechster_frei;
            }
        }
        self.anzahl_frei -= puffer.len() as u16;
        Some(kopf)
    }

    /// Macht die Kette mit Kopf `kopf` für das Gerät VERFÜGBAR: Kopf in
    /// den Available-Ring legen und avail.idx erhöhen. Danach muss der
    /// Treiber das Gerät anstoßen (Queue-Notify, transportabhängig).
    pub fn verfuegbar_machen(&mut self, kopf: u16) {
        // unsafe: idx volatil lesen (Gerät könnte es beobachten).
        let idx = unsafe { core::ptr::read_volatile(self.avail_idx_ptr()) };
        let slot = idx % self.groesse;
        // unsafe: Kopf in ring[slot] schreiben.
        unsafe {
            core::ptr::write_volatile(self.avail_ring_ptr(slot), kopf);
        }
        // BARRIERE: Der Ring-Eintrag MUSS geschrieben sein, BEVOR wir
        // avail.idx erhöhen — sonst sieht das Gerät den neuen Index,
        // aber noch den alten Ring-Eintrag.
        fence(Ordering::SeqCst);
        // unsafe: avail.idx erhöhen (mit Wrap durch u16-Überlauf — das
        // ist so gewollt, das Gerät rechnet modulo groesse).
        unsafe {
            core::ptr::write_volatile(self.avail_idx_ptr(), idx.wrapping_add(1));
        }
        fence(Ordering::SeqCst);
    }

    /// Holt das NÄCHSTE erledigte Element aus dem Used-Ring, wenn das
    /// Gerät eines abgelegt hat. Liefert (Kopf-Index der Kette,
    /// geschriebene Bytes) und gibt die Deskriptor-Kette zurück in die
    /// Freiliste. None = das Gerät ist noch nicht fertig.
    pub fn used_abholen(&mut self) -> Option<(u16, u32)> {
        // unsafe: used.idx volatil lesen (das Gerät erhöht es per DMA).
        let used_idx = unsafe { core::ptr::read_volatile(self.used_idx_ptr()) };
        if used_idx == self.letzter_used {
            return None; // nichts Neues
        }
        // BARRIERE: erst den neuen Index sehen, DANN das Element lesen.
        fence(Ordering::SeqCst);
        let slot = self.letzter_used % self.groesse;
        let element = self.used_element_ptr(slot);
        // unsafe: element[0] = id (Kopf-Deskriptor), element[1] = Länge.
        let (kopf, laenge) = unsafe {
            (
                core::ptr::read_volatile(element) as u16,
                core::ptr::read_volatile(element.add(1)),
            )
        };
        self.letzter_used = self.letzter_used.wrapping_add(1);
        self.kette_freigeben(kopf);
        Some((kopf, laenge))
    }

    /// Gibt eine komplette Deskriptor-Kette (ab `kopf`) in die
    /// Freiliste zurück. Folgt der `next`-Verkettung, bis kein
    /// F_NEXT-Flag mehr gesetzt ist.
    fn kette_freigeben(&mut self, kopf: u16) {
        let mut aktuell = kopf;
        let mut anzahl = 0u16;
        loop {
            let d = self.deskriptor_ptr(aktuell);
            // unsafe: gültiger Deskriptor-Index in der Kette.
            let flags = unsafe { core::ptr::read_volatile(&(*d).flags) };
            let hat_next = flags & F_NEXT != 0;
            let next = unsafe { core::ptr::read_volatile(&(*d).next) };
            anzahl += 1;
            // Schleifen-Schutz: Eine kaputte Kette darf uns nie hängen
            // lassen — mehr Glieder als Deskriptoren gibt es nicht.
            if anzahl > self.groesse {
                break;
            }
            if hat_next {
                aktuell = next;
            } else {
                // Ende der Kette: den letzten Deskriptor vor die
                // aktuelle Freiliste hängen.
                // unsafe: aktuell ist der letzte Kettenknoten.
                unsafe {
                    core::ptr::write_volatile(&mut (*d).next, self.frei_kopf);
                }
                break;
            }
        }
        self.frei_kopf = kopf;
        self.anzahl_frei += anzahl;
    }
}

// ---------------------------------------------------------------------------
// Tests — die reinen Layout-Funktionen (kein Gerät, kein Speicher nötig)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_virtqueue_aufrunden() {
        assert_eq!(aufrunden(0, 4096), 0);
        assert_eq!(aufrunden(1, 4096), 4096);
        assert_eq!(aufrunden(4096, 4096), 4096);
        assert_eq!(aufrunden(4097, 4096), 8192);
    }

    #[test_case]
    fn test_virtqueue_layout() {
        // Für Queue-Größe 128 (QEMU-typisch):
        let g = 128u16;
        // Deskriptoren: 128 * 16 = 2048.
        assert_eq!(avail_offset(g), 2048);
        // Available-Ring: 2048 + 6 + 256 = 2310 -> aufgerundet auf 4096.
        assert_eq!(used_offset(g), 4096);
        // Used-Ring: 6 + 8*128 = 1030 -> aufgerundet auf 4096.
        // Gesamt: 4096 + 4096 = 8192.
        assert_eq!(gesamt_groesse(g), 8192);
    }

    /// Eine echte Virtqueue anlegen (braucht Heap + Frame-Allocator)
    /// und die Ring-Verwaltung OHNE Gerät durchspielen: Kette anlegen,
    /// verfügbar machen, ein Used-Element von Hand einlegen, abholen.
    #[test_case]
    fn test_virtqueue_ringe() {
        let mut vq = Virtqueue::neu(16);
        assert_eq!(vq.anzahl_frei, 16);

        // Eine 3er-Kette (Header lesen / Daten schreiben / Status):
        let kopf = vq
            .kette_anlegen(&[
                (PhysAddr::new(0x1000), 16, false),
                (PhysAddr::new(0x2000), 512, true),
                (PhysAddr::new(0x3000), 1, true),
            ])
            .unwrap();
        assert_eq!(kopf, 0);
        assert_eq!(vq.anzahl_frei, 13); // 3 belegt

        vq.verfuegbar_machen(kopf);
        // avail.idx muss jetzt 1 sein:
        let idx = unsafe { core::ptr::read_volatile(vq.avail_idx_ptr()) };
        assert_eq!(idx, 1);
        // ring[0] trägt den Kopf:
        let ring0 = unsafe { core::ptr::read_volatile(vq.avail_ring_ptr(0)) };
        assert_eq!(ring0, kopf);

        // Das "Gerät" spielen: Element in used[0] legen, used.idx = 1.
        unsafe {
            let el = vq.used_element_ptr(0);
            core::ptr::write_volatile(el, kopf as u32);
            core::ptr::write_volatile(el.add(1), 513); // 512 Daten + 1 Status
            core::ptr::write_volatile(vq.used_idx_ptr(), 1);
        }
        let (fertig_kopf, laenge) = vq.used_abholen().unwrap();
        assert_eq!(fertig_kopf, kopf);
        assert_eq!(laenge, 513);
        // Die Kette ist zurück in der Freiliste:
        assert_eq!(vq.anzahl_frei, 16);
        // Nichts mehr abzuholen:
        assert_eq!(vq.used_abholen(), None);

        // Nach dem Freigeben lässt sich erneut eine Kette anlegen:
        assert!(vq.kette_anlegen(&[(PhysAddr::new(0x4000), 8, false)]).is_some());
    }
}
