// task/executor.rs — Der Executor: Herzstück des kooperativen Multitaskings
//
// Der Executor verwaltet alle Tasks und arbeitet sie fair ab:
//
//   1. Er holt Task-IDs aus einer Warteschlange (FIFO — wer zuerst
//      geweckt wurde, läuft zuerst: das ist unsere Fairness).
//   2. Er pollt den jeweiligen Task. Antwortet der Ready, ist er
//      fertig und fliegt raus. Antwortet er Pending, passiert NICHTS
//      weiter — der Task wird erst wieder gepollt, wenn sein Waker
//      aufgerufen wird.
//   3. Ist die Warteschlange leer, legt der Executor die CPU mit hlt
//      schlafen. Der nächste Interrupt (Timer, Taste) weckt sie.
//
// Der Waker ist der Clou am ganzen System: ein "Rückruf-Ticket", das
// der Executor jedem Task beim Pollen mitgibt. Der Task deponiert es
// dort, wo seine Daten entstehen (z. B. beim Tastatur-Interrupt).
// Wird das Ticket eingelöst (wake), landet die Task-ID wieder in der
// Warteschlange. So verschwendet der Executor NIE Zeit damit, Tasks
// zu pollen, die sowieso nichts zu tun haben (kein "Busy Polling").

use super::{Task, TaskId};
use alloc::{collections::BTreeMap, sync::Arc, task::Wake};
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

/// Wie viele geweckte Tasks gleichzeitig anstehen können.
const TASK_QUEUE_GROESSE: usize = 100;

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
}

impl Executor {
    pub fn new() -> Self {
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(TASK_QUEUE_GROESSE)),
            waker_cache: BTreeMap::new(),
        }
    }

    /// Nimmt einen neuen Task auf und reiht ihn zum ersten Lauf ein.
    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task_id, task).is_some() {
            panic!("Task-ID {:?} existiert schon", task_id);
        }
        self.task_queue.push(task_id).expect("Task-Warteschlange voll");
    }

    /// Die Hauptschleife des Kernels: Tasks abarbeiten, dann schlafen.
    /// Kehrt nie zurück — sie IST ab jetzt unser Leerlauf.
    pub fn run(&mut self) -> ! {
        loop {
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }

    /// Arbeitet alle aktuell geweckten Tasks genau einmal ab.
    fn run_ready_tasks(&mut self) {
        while let Some(task_id) = self.task_queue.pop() {
            let task = match self.tasks.get_mut(&task_id) {
                Some(task) => task,
                None => continue, // Task existiert nicht mehr (fertig)
            };
            // Waker aus dem Cache holen oder einmalig erzeugen:
            let waker = self
                .waker_cache
                .entry(task_id)
                .or_insert_with(|| TaskWaker::waker(task_id, self.task_queue.clone()));
            let mut context = Context::from_waker(waker);
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    // Fertig! Task und seinen Waker entsorgen.
                    self.tasks.remove(&task_id);
                    self.waker_cache.remove(&task_id);
                }
                Poll::Pending => {
                    // Nichts tun: Der Task meldet sich per Waker zurück.
                }
            }
        }
    }

    /// Legt die CPU schlafen, wenn keine Arbeit ansteht.
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
        if self.task_queue.is_empty() {
            enable_and_hlt();
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
/// wake() = "ID wieder einreihen". Mehr ist es nicht!
struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
}

impl TaskWaker {
    /// Baut aus unserem TaskWaker einen offiziellen core::task::Waker.
    /// Das Wake-Trait aus alloc erledigt die hässliche Vtable-Arbeit.
    fn waker(task_id: TaskId, task_queue: Arc<ArrayQueue<TaskId>>) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
        }))
    }

    fn wake_task(&self) {
        // push auf die lock-freie Queue: darf auch mitten im
        // Interrupt-Handler passieren, blockiert nie.
        self.task_queue.push(self.task_id).expect("Task-Warteschlange voll");
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
