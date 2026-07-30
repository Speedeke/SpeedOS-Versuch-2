// fenstertest — DER BEWEIS: ein Ring-3-Prozess mit einem eigenen Fenster
//                (Serie 8, Teil 1)
//
// Was hier passiert, ging bis eben nicht: Ein Programm, das NICHT Teil des
// Kernels ist, das in einem eigenen Adressraum unprivilegiert laeuft und
// SpeedOS nur ueber `int 0x80` erreicht, besitzt ein Fenster auf dem
// Desktop — mit Titelleiste, Taskleisten-Eintrag, Alt+Tab und Snap wie
// jedes Kernel-Fenster. Nur den INHALT malt es selbst.
//
// Es zeigt:
//   * einen Farbverlauf (der Grundfall: viele Pixel auf einmal),
//   * Punkte, wo geklickt wurde (Maus-Ereignisse mit fensterlokalen
//     Koordinaten),
//   * die zuletzt gedrueckte Taste (Tastatur-Ereignisse),
//   * einen Rahmen, der bei Fokus die Farbe wechselt,
//   * und es folgt Groessenaenderungen.
//
// Beim Schliessen-Wunsch beendet es sich SELBST — das ist der Unterschied
// zu einem Kernel-Fenster, das der Compositor einfach zumachen kann.
//
// ZWEI INSTANZEN LAUFEN UNABHAENGIG: Jede hat ihr eigenes Fenster, ihre
// eigene Ereignis-Warteschlange und ihren eigenen Adressraum. Genau das
// prueft tests/fenster.rs nach.
//
//     starte fenstertest
//     starte fenstertest --breite=300 --hoehe=200

#![no_std]
#![no_main]

extern crate alloc;

use libspeed::fenster::{Ereignis, Fenster};
use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

/// Nach so vielen Millisekunden ohne Ereignis endet der Test von selbst.
/// Ein Fenster-Programm, das in einem automatisierten Test laeuft, darf
/// nicht ewig warten — und ein Mensch, der zuschaut, hat nach einer
/// Minute genug gesehen.
const LEBENSDAUER_MS: u64 = 60_000;

fn haupt(argumente: &Argumente) -> i32 {
    let mut breite = 420usize;
    let mut hoehe = 280usize;
    let mut still = false;
    for index in 1..argumente.anzahl() {
        let Some(argument) = argumente.get(index) else {
            continue;
        };
        if let Some(wert) = argument.strip_prefix("--breite=") {
            breite = zahl(wert).max(64) as usize;
        } else if let Some(wert) = argument.strip_prefix("--hoehe=") {
            hoehe = zahl(wert).max(64) as usize;
        } else if argument == "--still" {
            still = true;
        }
    }

    let mut f = match Fenster::oeffnen("fenstertest", breite, hoehe) {
        Ok(f) => f,
        Err(fehler) => {
            println!("Fenster liess sich nicht oeffnen: {}", fehler.text());
            if fehler == libspeed::Fehler::NICHT_KONFIGURIERT {
                println!("Es laeuft kein Desktop — mit dem Befehl 'desktop' starten.");
            }
            return 3;
        }
    };
    if !still {
        println!("fenstertest: Fenster offen ({}x{}).", f.breite(), f.hoehe());
        println!("Klicken malt Punkte, Tasten erscheinen unten, X schliesst.");
    }

    let mut zustand = Zustand {
        klicks: alloc::vec::Vec::new(),
        letzte_taste: None,
        fokus: true,
        ereignisse: 0,
    };
    alles_malen(&mut f, &zustand);
    let _ = f.zeigen();

    let start = libspeed::zeit_jetzt();
    loop {
        // 100 ms Frist: Kommt nichts, laeuft die Schleife trotzdem einmal
        // durch — genau dort wuerde eine Animation weiterlaufen.
        let ereignis = match f.naechstes_ereignis(100) {
            Ok(ereignis) => ereignis,
            // Ungueltiges Handle heisst hier: Das Fenster ist weg (der
            // zweite Klick auf das X hat es erzwungen). Sauber enden.
            Err(fehler) => {
                if !still {
                    println!("fenstertest: Fenster weg ({}).", fehler.text());
                }
                return 0;
            }
        };
        if ereignis != Ereignis::Keins {
            zustand.ereignisse += 1;
        }

        match ereignis {
            Ereignis::Keins => {
                if libspeed::zeit_jetzt() - start > LEBENSDAUER_MS {
                    if !still {
                        println!("fenstertest: Lebensdauer erreicht, beende mich.");
                    }
                    return 0;
                }
                continue;
            }
            Ereignis::Schliessen => {
                if !still {
                    println!(
                        "fenstertest: Schliessen-Wunsch nach {} Ereignis(sen). Tschuess.",
                        zustand.ereignisse
                    );
                }
                return 0;
            }
            Ereignis::Groesse { breite, hoehe } => {
                // WICHTIG: Der Kernel hat seinen Puffer neu angelegt — er
                // ist leer. Wer hier nicht neu zeichnet, sieht nichts.
                f.groesse_uebernehmen(breite, hoehe);
                zustand.klicks.retain(|(x, y)| {
                    (*x as u32) < breite && (*y as u32) < hoehe
                });
                alles_malen(&mut f, &zustand);
                let _ = f.zeigen();
                continue;
            }
            Ereignis::Fokus(hat) => {
                zustand.fokus = hat;
                // Nur der RAHMEN aendert sich — also auch nur den senden.
                rahmen_malen(&mut f, &zustand);
                let _ = f.zeigen();
                continue;
            }
            Ereignis::MausAb { x, y, .. } => {
                if zustand.klicks.len() < 200 {
                    zustand.klicks.push((x, y));
                }
                // NUR DEN PUNKT senden — der Grund, warum der Bereich im
                // Syscall steht. Ein voller Rahmen waere hier 470 KiB,
                // dieser Streifen sind 900 Byte.
                punkt_malen(&mut f, x, y);
                let _ = f.zeigen_bereich(
                    (x - 8).max(0) as usize,
                    (y - 8).max(0) as usize,
                    17,
                    17,
                );
                continue;
            }
            Ereignis::Taste(zeichen) => {
                zustand.letzte_taste = Some(zeichen);
            }
            Ereignis::Sondertaste(code) => {
                zustand.letzte_taste = char::from_u32('0' as u32 + code as u32);
            }
            // Bewegungen und Rad interessieren diesen Test nicht — sie
            // werden gezaehlt und sonst verworfen.
            Ereignis::MausBewegt { .. } | Ereignis::MausAuf { .. } | Ereignis::MausRad { .. } => {
                continue;
            }
        }

        // Tastenanzeige: nur der Streifen unten.
        fussleiste_malen(&mut f, &zustand);
        let hoehe = f.hoehe();
        let _ = f.zeigen_bereich(0, hoehe.saturating_sub(24), f.breite(), 24);
    }
}

struct Zustand {
    klicks: alloc::vec::Vec<(i32, i32)>,
    letzte_taste: Option<char>,
    fokus: bool,
    ereignisse: u64,
}

/// Der volle Neuaufbau: Verlauf, Ueberschrift, alle Punkte, Rahmen, Fuss.
fn alles_malen(f: &mut Fenster, zustand: &Zustand) {
    let hoehe = f.hoehe();
    let breite = f.breite();
    // Ein Verlauf von dunkelblau nach violett — ganzzahlig gerechnet,
    // denn SpeedOS-Programme laufen ohne Fliesskomma (-sse/+soft-float,
    // damit der Kontext-Wechsel nur GP-Register sichern muss).
    for y in 0..hoehe {
        let t = (y * 255 / hoehe.max(1)) as u32;
        let farbe = Fenster::farbe(
            (20 + t * 90 / 255) as u8,
            (18 + t * 20 / 255) as u8,
            (48 + t * 120 / 255) as u8,
        );
        f.rechteck(0, y as i32, breite as i32, 1, farbe);
    }
    f.text(12, 12, "FENSTERTEST", Fenster::farbe(230, 235, 255), 2);
    f.text(
        12,
        34,
        "RING 3 MALT SELBST",
        Fenster::farbe(150, 200, 255),
        1,
    );
    for (x, y) in &zustand.klicks {
        punkt_malen(f, *x, *y);
    }
    rahmen_malen(f, zustand);
    fussleiste_malen(f, zustand);
}

/// Ein Klick-Punkt: ein Kreuz mit Kaestchen.
fn punkt_malen(f: &mut Fenster, x: i32, y: i32) {
    let gelb = Fenster::farbe(255, 210, 80);
    f.rechteck(x - 7, y, 15, 1, gelb);
    f.rechteck(x, y - 7, 1, 15, gelb);
    f.rechteck(x - 3, y - 3, 7, 7, Fenster::farbe(255, 120, 60));
}

/// Der Rahmen zeigt den FOKUS an — die einzige Stelle, an der ein
/// Prozess-Fenster den Zustand sichtbar macht, den sonst die (vom Kernel
/// gemalte) Titelleiste traegt.
fn rahmen_malen(f: &mut Fenster, zustand: &Zustand) {
    let farbe = if zustand.fokus {
        Fenster::farbe(120, 220, 160)
    } else {
        Fenster::farbe(90, 90, 110)
    };
    let (b, h) = (f.breite() as i32, f.hoehe() as i32);
    f.rechteck(0, 0, b, 2, farbe);
    f.rechteck(0, h - 2, b, 2, farbe);
    f.rechteck(0, 0, 2, h, farbe);
    f.rechteck(b - 2, 0, 2, h, farbe);
}

/// Unten: die letzte Taste und die Zahl der Ereignisse.
fn fussleiste_malen(f: &mut Fenster, zustand: &Zustand) {
    let (b, h) = (f.breite() as i32, f.hoehe() as i32);
    f.rechteck(2, h - 22, b - 4, 20, Fenster::farbe(12, 12, 24));
    let mut zeile = alloc::string::String::from("TASTE: ");
    match zustand.letzte_taste {
        Some(zeichen) if !zeichen.is_control() => zeile.push(zeichen),
        Some(_) => zeile.push('?'),
        None => zeile.push('-'),
    }
    zeile.push_str("   PUNKTE: ");
    zahl_anhaengen(&mut zeile, zustand.klicks.len() as u64);
    f.text(8, h - 17, &zeile, Fenster::farbe(200, 210, 230), 1);
    rahmen_malen(f, zustand);
}

fn zahl_anhaengen(text: &mut alloc::string::String, zahl: u64) {
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
        text.push(z as char);
    }
}

fn zahl(text: &str) -> u64 {
    text.bytes()
        .filter(|b| b.is_ascii_digit())
        .fold(0u64, |summe, ziffer| summe * 10 + (ziffer - b'0') as u64)
}
