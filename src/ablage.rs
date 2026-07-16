// ablage.rs — Die globale Ablage (Kopieren/Ausschneiden/Einfügen)
//
// Der Grundstein der System-Zwischenablage: EIN globaler Zustand,
// den alle Fenster teilen — deshalb funktioniert Strg+C in einem
// Explorer-Fenster und Strg+V in einem anderen. Aktuell trägt sie
// genau einen VFS-Pfad; später kommen weitere Inhaltsarten dazu
// (Text, Bild, ...) — dann wird der Inhalt ein Enum.
//
// LOCK-REGEL: Der ABLAGE-Mutex ist ein BLATT-Lock (nimmt selbst
// keine anderen Locks) — er darf deshalb auch unter dem MANAGER-Lock
// (App::nachricht/taste) genommen werden.

use alloc::string::String;
use spin::Mutex;

/// Was in der Ablage liegt.
#[derive(Clone)]
pub struct Ablage {
    /// Der (absolute) Quell-Pfad.
    pub pfad: String,
    /// true = Ausschneiden (nach dem Einfügen wird die Quelle
    /// gelöscht und die Ablage geleert).
    pub ausschneiden: bool,
}

static ABLAGE: Mutex<Option<Ablage>> = Mutex::new(None);

/// Legt einen Pfad in die Ablage (Strg+C / Strg+X).
pub fn setzen(pfad: &str, ausschneiden: bool) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        *ABLAGE.lock() = Some(Ablage { pfad: String::from(pfad), ausschneiden });
    });
}

/// Liest die Ablage (Kopie; None = leer).
pub fn holen() -> Option<Ablage> {
    x86_64::instructions::interrupts::without_interrupts(|| ABLAGE.lock().clone())
}

/// Leert die Ablage (nach eingefügtem Ausschneiden).
pub fn leeren() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        *ABLAGE.lock() = None;
    });
}
