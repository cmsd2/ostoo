use alloc::boxed::Box;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use libkernel::task::{Task, executor};
use super::driver::{Driver, DriverState};

/// Passed into [`DriverTask::run`]; poll [`StopToken::is_stopped`] to detect
/// a [`Driver::stop`] request.
pub struct StopToken(Arc<AtomicBool>);

impl StopToken {
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// The per-driver logic.  Implement this trait to define what a driver does;
/// [`TaskDriver<T>`] supplies all the lifecycle boilerplate.
///
/// `run` is a static method (not `&self`) so the future can be `'static`:
/// the driver's own state is reachable via the `Arc<Self>` handle.
pub trait DriverTask: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    fn run(handle: Arc<Self>, stop: StopToken)
        -> Pin<Box<dyn Future<Output = ()> + Send>>
    where Self: Sized;

    /// Emit driver-specific info fields.  Override to expose internal state
    /// through `driver info <name>`.  Default: nothing.
    fn info(&self, _out: &mut dyn FnMut(&str, &str)) {}
}

/// Generic wrapper that adapts any [`DriverTask`] into a [`Driver`].
///
/// Owns the lifecycle atomics (`running`, `stop_flag`) and the inner task
/// behind an `Arc` so `start()` can hand a clone to the spawned future.
pub struct TaskDriver<T> {
    task:      Arc<T>,
    running:   Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
}

impl<T: DriverTask> TaskDriver<T> {
    pub fn new(task: T) -> Self {
        TaskDriver {
            task:      Arc::new(task),
            running:   Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl<T: DriverTask> Driver for TaskDriver<T> {
    fn name(&self) -> &'static str {
        self.task.name()
    }

    fn state(&self) -> DriverState {
        if self.running.load(Ordering::Acquire) {
            DriverState::Running
        } else {
            DriverState::Stopped
        }
    }

    fn start(&self) {
        if self.running.load(Ordering::Acquire) { return; }
        self.stop_flag.store(false, Ordering::Release);
        self.running.store(true,  Ordering::Release);
        let handle  = self.task.clone();
        let stop    = StopToken(self.stop_flag.clone());
        let running = self.running.clone();
        executor::spawn(Task::new(async move {
            T::run(handle, stop).await;
            running.store(false, Ordering::Release);
        }));
    }

    fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }

    fn info(&self, out: &mut dyn FnMut(&str, &str)) {
        out("stop_flag", if self.stop_flag.load(Ordering::Acquire) { "set" } else { "clear" });
        self.task.info(out);
    }
}
