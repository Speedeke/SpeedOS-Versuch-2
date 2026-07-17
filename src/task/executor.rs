// task/executor.rs — Der Executor: Herzstück des kooperativen Multitaskings
//
// Der Executor verwaltet alle Tasks und arbeitet sie fair ab:
//
//   1. Er übernimmt Neuzugänge aus der globalen Spawn-Queue
//      (task::spawn — damit können Tasks selbst Tasks starten).
//   2. Er holt Task-IDs aus der Weck-Warteschlange (FIFO — wer zuerst
//      geweckt wurde, läuft zuerst: das ist unsere Fairness) und pollt
//      sie. Ready = fertig, fliegt raus. Pending = schläft, bis sein
//      Waker ihn wieder einreiht.
//   3. Ist nirgends Arbeit, legt er die CPU mit hlt schlafen.
//
// Der Waker ist der Clou am ganzen System: ein "Rückruf-Ticket", das
// der Executor jedem Task beim Pollen mitgibt. Der Task deponiert es
// dort, wo seine Daten entstehen (z. B. beim Tastatur-Interrupt).
// Wird das Ticket eingelöst (wake), landet die Task-ID wieder in der
// Warteschlange. So verschwendet der Executor NIE Zeit damit, Tasks
// zu pollen, die sowieso nichts zu tun haben (kein "Busy Polling").
//
// Überlauf-Verhalten (statt Panic!): Ist die Weck-Warteschlange voll,
// geht das Wecken NICHT verloren — es wird nur ungenau: Ein Notfall-
// Flag wird gesetzt, und der Executor pollt daraufhin einmal ALLE
// Tasks. Das kostet kurz Leistung, aber kein Task bleibt je hängen.
// (Der Timer-Interrupt weckt die CPU spätestens nach ~4 ms aus dem
// hlt, selbst wenn das Flag mitten im Einschlafen gesetzt wurde.)

use super::uebersicht::{self, TaskZaehler};
use super::{Task, TaskId};
use alloc::{collections::BTreeMap, sync::Arc, task::Wake, vec::Vec};
use conquer_once::spin::OnceCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

/// Wie viele Tasks gerade leben — nur eine Anzeige-Statistik (die
/// Einstellungen-App zeigt sie auf der Info-Seite), deshalb ein
/// simples Atomic statt Zugriff auf den Executor selbst.
static TASK_ANZAHL: AtomicUsize = AtomicUsize::new(0);

/// Anzahl der aktuell lebenden Tasks.
pub fn task_anzahl() -> usize {
    TASK_ANZAHL.load(Ordering::Relaxed)
}

/// Standard-Kapazität der Warteschlangen (Weck- und Spawn-Queue).
pub const STANDARD_KAPAZITAET: usize = 128;

/// Die globale Spawn-Queue: Hierüber reicht task::spawn() neue Tasks
/// an den Executor durch — von überall, ohne &mut Executor.
static SPAWN_QUEUE: OnceCell<ArrayQueue<Task>> = OnceCell::uninit();

/// Interne Schnittstelle für task::spawn() (siehe task/mod.rs).
pub(super) fn neuen_task_einreihen(task: Task) -> Result<(), Task> {
    match SPAWN_QUEUE.try_get() {
        Ok(queue) => queue.push(task),
        Err(_) => Err(task), // Es gibt noch keinen Executor.
    }
}

pub struct Executor {
    /// Alle lebenden Tasks, auffindbar über ihre ID.
    tasks: BTreeMap<TaskId, Task>,
    /// Die IDs der geweckten Tasks — die eigentliche Arbeitsliste.
    /// Arc, weil auch alle Waker auf diese Queue zeigen; ArrayQueue,
    /// weil sie lock-frei ist und damit interrupt-sicher (ein Waker
    /// darf ja mitten aus einem Interrupt-Handler feuern!).
    task_queue: Arc<ArrayQueue<TaskId>>,
    /// Waker-Zwischenspeicher, damit nicht bei jedem poll() ein
    /// neuer Waker auf dem Heap entsteht.
    waker_cache: BTreeMap<TaskId, Waker>,
    /// Notfall-Flag: wurde ein Wecken wegen voller Queue verworfen?
    /// Dann pollt die nächste Runde ALLE Tasks (nichts geht verloren).
    ueberlauf: Arc<AtomicBool>,
}

impl Executor {
    /// Executor mit Standard-Kapazität (128 Einträge pro Queue).
    pub fn new() -> Self {
        Self::mit_kapazitaet(STANDARD_KAPAZITAET)
    }

    /// Executor mit konfigurierbarer Warteschlangen-Kapazität.
    /// Faustregel: mindestens 2x so groß wie die erwartete Task-Zahl,
    /// denn eine ID kann kurzzeitig mehrfach in der Queue stehen.
    pub fn mit_kapazitaet(kapazitaet: usize) -> Self {
        // Die globale Spawn-Queue beim ersten Executor anlegen
        // (ein zweiter Executor teilt sie sich einfach).
        let _ = SPAWN_QUEUE.try_init_once(|| ArrayQueue::new(kapazitaet));

        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(kapazitaet)),
            waker_cache: BTreeMap::new(),
            ueberlauf: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Nimmt einen neuen Task auf und reiht ihn zum ersten Lauf ein.
    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        // Steckbrief in die Task-Übersicht eintragen (Task-Manager);
        // die Zähler teilen sich Task, Waker und Übersicht per Arc.
        uebersicht::registrieren(
            task_id.als_zahl(),
            task.name.clone(),
            task.art,
            task.beendbar,
            task.zaehler.clone(),
        );
        if self.tasks.insert(task_id, task).is_some() {
            panic!("Task-ID {:?} existiert schon", task_id);
        }
        TASK_ANZAHL.store(self.tasks.len(), Ordering::Relaxed);
        if self.task_queue.push(task_id).is_err() {
            // Queue voll: kein Panic — Notfall-Flag setzen, die
            // nächste Runde pollt dann sowieso alle Tasks.
            self.ueberlauf.store(true, Ordering::Relaxed);
        }
    }

    /// Die Hauptschleife des Kernels: Tasks abarbeiten, dann schlafen.
    /// Kehrt nie zurück — sie IST ab jetzt unser Leerlauf.
    /// Nebenbei die CPU-METRIK: Die Zeit in run_ready_tasks ist
    /// ARBEIT, die Zeit im hlt (sleep_if_idle) ist RUHE — beides per
    /// TSC gemessen und ins Auslastungs-Gleitfenster verbucht.
    pub fn run(&mut self) -> ! {
        loop {
            let start_us = crate::zeit::us_seit_boot();
            self.run_ready_tasks();
            let arbeit_us = crate::zeit::us_seit_boot() - start_us;
            uebersicht::cpu_verbuchen(arbeit_us, 0);
            self.sleep_if_idle();
        }
    }

    /// Arbeitet alle aktuell anstehenden Aufgaben genau einmal ab.
    fn run_ready_tasks(&mut self) {
        // 0. Beenden-Anforderungen aus dem Task-Manager: Der Task
        //    wird FALLEN GELASSEN (Drop der Future) — kooperativ
        //    heißt: Er endet an seinem aktuellen await-Punkt, nie
        //    mitten in einer Anweisung. Drop räumt sauber auf.
        for id in uebersicht::beenden_abholen() {
            let task_id = TaskId::aus_zahl(id);
            if self.tasks.remove(&task_id).is_some() {
                self.waker_cache.remove(&task_id);
                uebersicht::austragen(id);
                TASK_ANZAHL.store(self.tasks.len(), Ordering::Relaxed);
                crate::serial_println!("[EXECUTOR] Task {} auf Anforderung beendet.", id);
            }
        }

        // 1. Neuzugänge aus der globalen Spawn-Queue übernehmen:
        if let Ok(spawn_queue) = SPAWN_QUEUE.try_get() {
            while let Some(task) = spawn_queue.pop() {
                self.spawn(task);
            }
        }

        // 2. Notfall-Modus nach Queue-Überlauf: ALLE Tasks pollen,
        //    damit garantiert kein verworfenes Wecken verloren geht.
        if self.ueberlauf.swap(false, Ordering::Relaxed) {
            let alle_ids: Vec<TaskId> = self.tasks.keys().copied().collect();
            for task_id in alle_ids {
                self.task_pollen(task_id);
            }
        }

        // 3. Normalfall: nur die geweckten Tasks pollen.
        while let Some(task_id) = self.task_queue.pop() {
            self.task_pollen(task_id);
        }
    }

    /// Pollt einen einzelnen Task (falls er noch existiert).
    fn task_pollen(&mut self, task_id: TaskId) {
        let task = match self.tasks.get_mut(&task_id) {
            Some(task) => task,
            None => return, // Task existiert nicht mehr (fertig)
        };
        // Aktivitätszähler: Der Task wird jetzt gepollt — VOR dem
        // Poll auf "schläft" stellen, damit ein Selbst-Wecken
        // WÄHREND des Polls (yield_now) wieder "wach" setzen kann.
        task.zaehler.polls.fetch_add(1, Ordering::Relaxed);
        task.zaehler.wach.store(false, Ordering::Relaxed);
        let zaehler = task.zaehler.clone();
        // Waker aus dem Cache holen oder einmalig erzeugen:
        let waker = self.waker_cache.entry(task_id).or_insert_with(|| {
            TaskWaker::waker(task_id, self.task_queue.clone(), self.ueberlauf.clone(), zaehler)
        });
        let mut context = Context::from_waker(waker);
        match task.poll(&mut context) {
            Poll::Ready(()) => {
                // Fertig! Task und seinen Waker entsorgen.
                self.tasks.remove(&task_id);
                self.waker_cache.remove(&task_id);
                uebersicht::austragen(task_id.als_zahl());
                TASK_ANZAHL.store(self.tasks.len(), Ordering::Relaxed);
            }
            Poll::Pending => {
                // Nichts tun: Der Task meldet sich per Waker zurück.
            }
        }
    }

    /// Legt die CPU schlafen, wenn wirklich nirgends Arbeit wartet.
    ///
    /// WICHTIG, subtil: Erst Interrupts VERBIETEN, dann prüfen, dann
    /// atomar "erlauben + hlt" (enable_and_hlt). Ohne das gäbe es eine
    /// Race Condition: Käme der Tastatur-Interrupt GENAU zwischen
    /// unserer Prüfung ("Queue leer") und dem hlt, würde die CPU
    /// schlafen gehen, obwohl gerade Arbeit hereingekommen ist — bis
    /// zum nächsten Timer-Tick wäre die Eingabe verzögert.
    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts::{self, enable_and_hlt};

        interrupts::disable();
        let spawn_queue_leer = SPAWN_QUEUE
            .try_get()
            .map(|q| q.is_empty())
            .unwrap_or(true);
        if self.task_queue.is_empty()
            && spawn_queue_leer
            && !self.ueberlauf.load(Ordering::Relaxed)
        {
            // RUHE messen: Der TSC läuft auch mit Interrupts aus
            // weiter; die Handler-Zeit nach dem Aufwachen zählt mit
            // zur Ruhe — Handler sind minimal, das ist ehrlich genug.
            let start_us = crate::zeit::us_seit_boot();
            enable_and_hlt();
            uebersicht::cpu_verbuchen(0, crate::zeit::us_seit_boot() - start_us);
        } else {
            interrupts::enable();
        }
    }
}

/// Default = leerer Executor (wie new).
impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

/// Der Waker eines Tasks: kennt die Task-ID und die Warteschlange.
/// wake() = "ID wieder einreihen" — plus Zählerpflege für die
/// Task-Übersicht (Atomics, denn wake feuert aus Interrupt-Handlern).
struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
    ueberlauf: Arc<AtomicBool>,
    zaehler: Arc<TaskZaehler>,
}

impl TaskWaker {
    /// Baut aus unserem TaskWaker einen offiziellen core::task::Waker.
    /// Das Wake-Trait aus alloc erledigt die hässliche Vtable-Arbeit.
    fn waker(
        task_id: TaskId,
        task_queue: Arc<ArrayQueue<TaskId>>,
        ueberlauf: Arc<AtomicBool>,
        zaehler: Arc<TaskZaehler>,
    ) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
            ueberlauf,
            zaehler,
        }))
    }

    fn wake_task(&self) {
        // Übersicht: geweckt -> Zähler hoch, Status "wachend".
        self.zaehler.wecken.fetch_add(1, Ordering::Relaxed);
        self.zaehler.wach.store(true, Ordering::Relaxed);
        // push auf die lock-freie Queue: darf auch mitten im
        // Interrupt-Handler passieren, blockiert nie.
        if self.task_queue.push(self.task_id).is_err() {
            // Queue voll: KEIN Panic. Das Flag sorgt dafür, dass die
            // nächste Executor-Runde alle Tasks pollt — dieses Wecken
            // kommt also garantiert an, nur eben etwas gröber.
            self.ueberlauf.store(true, Ordering::Relaxed);
        }
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}
