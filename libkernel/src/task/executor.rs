use super::{Task, TaskId};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::task::Wake;
use core::task::Waker;
use core::task::{Context, Poll};
use crossbeam_queue::ArrayQueue;
use lazy_static::lazy_static;
use crate::spin_mutex::SpinMutex as Mutex;

// ---------------------------------------------------------------------------
// Global executor state
//
// Tasks ready to be polled on the next call to run_ready_tasks().
static TASK_QUEUE: Mutex<VecDeque<Task>> = Mutex::new(VecDeque::new());

// Tasks that returned Poll::Pending and are waiting for a Waker to fire.
static WAIT_MAP: Mutex<BTreeMap<TaskId, Task>> = Mutex::new(BTreeMap::new());

// Lock-free queue of task IDs whose wakers have fired.  Populated from ISR
// context (timer tick → wake), so it must be lock-free.
lazy_static! {
    // 256 slots: a task can be woken at most once per LAPIC tick (1000/s) so
    // this covers 256 ms of executor starvation before overflowing.
    static ref WAKE_QUEUE: Arc<ArrayQueue<TaskId>> = Arc::new(ArrayQueue::new(256));
}

// One Waker per live task.
//
// This is critical for ISR safety.  When timer::tick() or the keyboard ISR
// calls Waker::wake(), it consumes the Waker stored in timer::WAKERS or the
// AtomicWaker.  If that were the *last* Arc reference, the drop would call
// into the global heap allocator, which might be spin-locked by the preempted
// thread → deadlock.
//
// By keeping a second reference here, the ISR's drop never hits zero.
// Deallocation only happens from executor context (Poll::Ready cleanup), which
// is always safe.
static WAKER_CACHE: Mutex<BTreeMap<TaskId, Waker>> = Mutex::new(BTreeMap::new());

// Task IDs whose wakeup signal was consumed by wake_tasks() while the task was
// "in flight" (popped from TASK_QUEUE, currently being polled, not yet in
// WAIT_MAP).  Checked immediately after WAIT_MAP.insert so the task is
// re-queued without ever sleeping.
//
// Why this is needed (two-thread executor race):
//
//   Thread 0: task.poll() returns Poll::Pending
//   ← LAPIC fires → tick() pushes task_id to WAKE_QUEUE; preempts to Thread 1
//   Thread 1: wake_tasks() pops task_id — not in WAIT_MAP → signal gone
//   Thread 0 resumes: WAIT_MAP.insert(task_id) — no pending wake, task stuck
//
// The original single-thread fix (wake_tasks after insert) only catches wakeups
// from on-core interrupts that fire after the insert.  It cannot catch the case
// above where Thread 1 consumed the signal before Thread 0 reached the insert.
//
// Locking WAIT_MAP across poll() would fix the race but deadlocks any task body
// that calls wait_count() (which also locks WAIT_MAP).  DEFERRED_WAKES avoids
// holding any lock during poll().
static DEFERRED_WAKES: Mutex<BTreeSet<TaskId>> = Mutex::new(BTreeSet::new());

/// Enqueue a new task.  Safe to call from any kernel thread.
pub fn spawn(task: Task) {
    TASK_QUEUE.lock().push_back(task);
}

/// Number of tasks currently ready to poll.
pub fn ready_count() -> usize { TASK_QUEUE.lock().len() }

/// Number of tasks currently waiting for a waker.
pub fn wait_count() -> usize { WAIT_MAP.lock().len() }

/// Run the async executor loop.  Never returns.
///
/// Multiple kernel threads may call this concurrently; they will all compete
/// to pull tasks from the shared TASK_QUEUE.
pub fn run_worker() -> ! {
    loop {
        wake_tasks();
        run_ready_tasks();
        sleep_if_idle();
    }
}

// ---------------------------------------------------------------------------
// Internal helpers

fn run_ready_tasks() {
    loop {
        // Take one task from the queue, releasing the lock before polling so
        // that poll() can call spawn() without deadlocking.
        let mut task = {
            let mut queue = TASK_QUEUE.lock();
            match queue.pop_front() {
                Some(t) => t,
                None => break,
            }
        };

        let task_id = task.id;

        // Get-or-create a cached Waker for this task, then clone it.
        // Keeping the original in WAKER_CACHE ensures the Arc<TaskWaker>
        // always has at least one reference outside the ISR, preventing
        // deallocation inside the timer/keyboard ISR context.
        let waker = {
            let mut cache = WAKER_CACHE.lock();
            cache.entry(task_id)
                .or_insert_with(|| create_waker(task_id))
                .clone()
        };

        let mut context = Context::from_waker(&waker);
        match task.poll(&mut context) {
            Poll::Ready(()) => {
                // Task done: remove cached waker and any deferred wake entry.
                WAKER_CACHE.lock().remove(&task_id);
                DEFERRED_WAKES.lock().remove(&task_id);
            }
            Poll::Pending => {
                WAIT_MAP.lock().insert(task_id, task);

                // Case 1 — on-core interrupt: a timer tick may have fired
                // between poll() returning Pending and the insert above.
                // wake_tasks() will drain the WAKE_QUEUE and re-queue the task.
                //
                // Case 2 — cross-thread race: the LAPIC may have fired tick()
                // AND preempted to Thread 1, which ran wake_tasks() and
                // consumed this task's ID from WAKE_QUEUE before the insert.
                // That path records the task_id in DEFERRED_WAKES; we handle
                // it here by immediately moving the task back to TASK_QUEUE.
                if DEFERRED_WAKES.lock().remove(&task_id) {
                    if let Some(t) = WAIT_MAP.lock().remove(&task_id) {
                        TASK_QUEUE.lock().push_back(t);
                    }
                } else {
                    wake_tasks();
                }
            }
        }
    }
}

fn create_waker(task_id: TaskId) -> Waker {
    Waker::from(Arc::new(TaskWaker { task_id }))
}

fn wake_tasks() {
    while let Some(task_id) = WAKE_QUEUE.pop() {
        let mut wait_map = WAIT_MAP.lock();
        if let Some(task) = wait_map.remove(&task_id) {
            drop(wait_map); // release before locking TASK_QUEUE
            TASK_QUEUE.lock().push_back(task);
        } else {
            drop(wait_map);
            // Task is not in WAIT_MAP: it is either in TASK_QUEUE, currently
            // in-flight (being polled by another thread), or completed.
            //
            // If in-flight, the waker has been consumed from timer::WAKERS so
            // no future tick will re-wake the task.  Record a deferred wake;
            // run_ready_tasks() checks this set after WAIT_MAP.insert and
            // immediately re-queues if found.
            //
            // We distinguish "in-flight / queued" (still in WAKER_CACHE) from
            // "completed" (removed from WAKER_CACHE on Poll::Ready) to avoid
            // leaking entries for finished tasks.
            if WAKER_CACHE.lock().contains_key(&task_id) {
                DEFERRED_WAKES.lock().insert(task_id);
            }
        }
    }
}

fn sleep_if_idle() {
    use x86_64::instructions::interrupts;

    if !WAKE_QUEUE.is_empty() {
        return;
    }

    // Disable interrupts, re-check, then atomically re-enable + HLT.
    // This prevents the missed-wakeup race: if a timer tick fires between
    // the is_empty() check and HLT, the STI takes effect after the next
    // instruction (HLT), guaranteeing the interrupt is handled.
    interrupts::disable();
    if WAKE_QUEUE.is_empty() {
        interrupts::enable_and_hlt();
    } else {
        interrupts::enable();
    }
}

// ---------------------------------------------------------------------------
// Waker implementation

struct TaskWaker {
    task_id: TaskId,
}

impl TaskWaker {
    fn wake_task(&self) {
        // Silently drop if full: the task is already queued for wakeup if a
        // previous push succeeded, so no work is lost.
        let _ = WAKE_QUEUE.push(self.task_id);
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
