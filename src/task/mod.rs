// task/mod.rs — Kooperatives Multitasking: der Task-Typ
//
// Ein Task ist bei uns einfach eine Future mit einer ID. Futures sind
// Rusts eingebautes Konzept für "Arbeit, die noch nicht fertig ist":
// Der Compiler übersetzt jede `async fn` in eine Zustandsmaschine,
// die man mit poll() anstoßen kann. Antwortet sie Poll::Pending,
// hat sie gerade nichts zu tun und gibt die CPU FREIWILLIG ab —
// deshalb "kooperativ". Der Executor (executor.rs) ruft poll()
// wieder auf, sobald der Waker des Tasks sagt: "Es gibt Neues!"

pub mod executor;
pub mod keyboard;
pub mod uebersicht;

pub use uebersicht::TaskArt;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};
use uebersicht::TaskZaehler;

/// Eindeutige Nummer für jeden Task (fürs Waker-Caching im Executor
/// und als Anzeige-Id im Task-Manager).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// Vergibt beim Erzeugen einfach die nächste freie Nummer —
    /// ein atomarer Zähler reicht dafür völlig.
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Die rohe Nummer — der Schlüssel in der Task-Übersicht.
    pub fn als_zahl(self) -> u64 {
        self.0
    }

    /// Rückweg für den Executor (Beenden-Anforderungen kommen als
    /// rohe Nummern aus der Übersicht zurück).
    pub(crate) fn aus_zahl(zahl: u64) -> Self {
        TaskId(zahl)
    }
}

/// Ein Task: eine ins Heap verfrachtete Future ohne Rückgabewert,
/// plus Steckbrief (Name, Art, beendbar?) für die Task-Übersicht.
///
/// - `dyn Future`: Jede async fn hat ihren eigenen anonymen Typ —
///   über das Trait-Objekt können wir sie alle einheitlich lagern.
/// - `Pin<Box<...>>`: Futures dürfen nach dem ersten poll() nicht
///   mehr im Speicher verschoben werden (sie enthalten oft Zeiger
///   auf sich selbst). Pin garantiert genau das.
/// - `+ Send`: nötig, damit Tasks durch die globale Spawn-Queue
///   wandern dürfen (statische Datenstrukturen verlangen Send).
pub struct Task {
    id: TaskId,
    /// Anzeigename für den Task-Manager (String, damit auch
    /// nummerierte Namen wie "Demo-Zaehler 3" möglich sind).
    name: String,
    art: TaskArt,
    /// Darf der Task-Manager diesen Task beenden? Nur Wegwerf-Tasks
    /// (Demos) — Kernel-Tasks sind geschützt.
    beendbar: bool,
    /// Die heißen Zähler — geteilt mit Waker und Übersicht (Arc).
    zaehler: Arc<TaskZaehler>,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Task {
    /// Verpackt eine beliebige `async fn`-Future in einen Task.
    /// Jeder Task bekommt einen NAMEN — im Task-Manager erscheint er
    /// damit erkennbar statt als anonyme Nummer.
    /// `'static`: Die Future darf keine Referenzen auf Kurzlebiges
    /// enthalten — sie läuft ja beliebig lange weiter.
    pub fn new(name: impl Into<String>, future: impl Future<Output = ()> + Send + 'static) -> Task {
        Task {
            id: TaskId::new(),
            name: name.into(),
            art: TaskArt::Kernel,
            beendbar: false,
            zaehler: Arc::new(TaskZaehler::default()),
            future: Box::pin(future),
        }
    }

    /// Builder: Art fürs Task-Manager-Icon (Standard: Kernel).
    pub fn mit_art(mut self, art: TaskArt) -> Task {
        self.art = art;
        self
    }

    /// Builder: als beendbar markieren (Demo-/Wegwerf-Tasks) — nur
    /// solche darf der Task-Manager fallen lassen.
    pub fn als_beendbar(mut self) -> Task {
        self.beendbar = true;
        self
    }

    /// Die Future ein Stück weiterlaufen lassen. Der Context enthält
    /// den Waker, mit dem der Task sich später zurückmelden kann.
    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}

/// Reiht einen neuen Task ein, OHNE eine Referenz auf den Executor zu
/// brauchen — damit können laufende Tasks selbst neue Tasks starten!
/// Der Executor übernimmt die Neuzugänge zu Beginn jeder Runde aus
/// der globalen Spawn-Queue.
///
/// NICHT aus Interrupt-Handlern benutzen: Task::new alloziert auf dem
/// Heap, und Allokationen sind im Interrupt-Kontext verboten.
///
/// Gibt den Task zurück (Err), wenn die Spawn-Queue voll ist oder
/// noch kein Executor existiert — der Aufrufer entscheidet dann,
/// ob er es später nochmal versucht.
pub fn spawn(task: Task) -> Result<(), Task> {
    executor::neuen_task_einreihen(task)
}

/// Gibt die CPU einmal freiwillig ab: weckt sich selbst sofort wieder
/// und antwortet Pending. Der Executor stellt den Task dadurch hinten
/// in die Warteschlange — andere Tasks kommen an die Reihe.
/// DAS ist Kooperation in Reinform. Benutzung: `yield_now().await;`
pub fn yield_now() -> impl Future<Output = ()> {
    struct YieldNow {
        schon_abgegeben: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.schon_abgegeben {
                Poll::Ready(())
            } else {
                self.schon_abgegeben = true;
                cx.waker().wake_by_ref(); // gleich wieder einreihen
                Poll::Pending
            }
        }
    }

    YieldNow {
        schon_abgegeben: false,
    }
}
