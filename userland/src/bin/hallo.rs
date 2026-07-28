// hallo — Das erste echte SpeedOS-Programm
//
// Es tut fast nichts, und genau deshalb ist es wichtig: Wenn `hallo` seinen
// Text ausgibt und mit Code 0 endet, dann hat die GESAMTE Kette funktioniert
// — userland-Crate uebersetzt, statisch gelinkt, ins Kernel-Image
// eingebettet, beim Boot auf /platte/programme geschrieben, vom VFS gelesen,
// als ELF geprueft, in einen frischen Adressraum gemappt, mit Argumenten
// versorgt, vom Scheduler eingeplant, in Ring 3 ausgefuehrt, per Syscall
// zurueck in den Kernel, sauber beendet und abgeraeumt.
//
// Alles, was dieses Programm anfassen kann, geht durch `int 0x80`. Es hat
// keinen Heap, keine Bibliothek, keinen Zugriff auf ein einziges Byte des
// Kernels.

#![no_std]
#![no_main]

use libspeed::{println, Argumente};

libspeed::hauptprogramm!(haupt);

fn haupt(argumente: &Argumente) -> i32 {
    println!("Hallo aus dem User-Space von SpeedOS!");
    println!();
    println!("Ich bin ein eigenstaendiges Programm:");
    println!("  * von /platte geladen (kein Teil des Kernel-Images)");
    println!("  * in meinem EIGENEN Adressraum ab 0x8000000000");
    println!("  * in Ring 3 — ich darf keinen einzigen Kernel-Befehl");
    println!("  * jeder Kontakt laeuft ueber int 0x80");
    println!();
    println!("Meine Prozess-Nummer ist {}.", libspeed::pid());
    println!("Das System laeuft seit {} ms.", libspeed::zeit_jetzt());

    // Die Argumente beweisen, dass argv ankommt (rdi/rsi aus dem
    // Start-TrapFrame -> Zeiger auf unseren eigenen Stack).
    println!();
    println!("Ich wurde mit {} Argument(en) gestartet:", argumente.anzahl());
    for index in 0..argumente.anzahl() {
        match argumente.get(index) {
            Some(text) => println!("  argv[{}] = \"{}\"", index, text),
            None => println!("  argv[{}] = <kein gueltiges UTF-8>", index),
        }
    }

    // Ein Argument darf den Exit-Code bestimmen — so kann man von der Shell
    // aus pruefen, dass Exit-Codes wirklich durchkommen.
    if let Some(wunsch) = argumente.get(1) {
        if let Some(rest) = wunsch.strip_prefix("--code=") {
            let code: i32 = rest.bytes().fold(0i32, |summe, ziffer| {
                if ziffer.is_ascii_digit() {
                    summe * 10 + (ziffer - b'0') as i32
                } else {
                    summe
                }
            });
            println!();
            println!("Beende mich auf Wunsch mit Code {}.", code);
            return code;
        }
    }

    0
}
