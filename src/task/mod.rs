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

use alloc::boxed::Box;
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

/// Eindeutige Nummer für jeden Task (fürs Waker-Caching im Executor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// Vergibt beim Erzeugen einfach die nächste freie Nummer —
    /// ein atomarer Zähler reicht dafür völlig.
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Ein Task: eine ins Heap verfrachtete Future ohne Rückgabewert.
///
/// - `dyn Future`: Jede async fn hat ihren eigenen anonymen Typ —
///   über das Trait-Objekt können wir sie alle einheitlich lagern.
/// - `Pin<Box<...>>`: Futures dürfen nach dem ersten poll() nicht
///   mehr im Speicher verschoben werden (sie enthalten oft Zeiger
///   auf sich selbst). Pin garantiert genau das.
pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()>>>,
}

impl Task {
    /// Verpackt eine beliebige `async fn`-Future in einen Task.
    /// `'static`: Die Future darf keine Referenzen auf Kurzlebiges
    /// enthalten — sie läuft ja beliebig lange weiter.
    pub fn new(future: impl Future<Output = ()> + 'static) -> Task {
        Task {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }

    /// Die Future ein Stück weiterlaufen lassen. Der Context enthält
    /// den Waker, mit dem der Task sich später zurückmelden kann.
    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
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
