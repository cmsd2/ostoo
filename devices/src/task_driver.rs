use alloc::sync::Arc;
use core::future::Future;
use core::sync::atomic::{AtomicBool, Ordering};
use libkernel::task::{Task, executor};
use libkernel::task::mailbox::{ActorMsg, Mailbox};
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
///
/// `Message` is the actor-specific inner message type.  The mailbox is
/// parameterised over [`ActorMsg<Self::Message>`], which adds the generic
/// `Info` variant handled by the framework.  Use `()` if the driver does not
/// need actor-specific runtime messages.
pub trait DriverTask: Send + Sync + 'static {
    type Message: Send;

    fn name(&self) -> &'static str;

    fn run(
        handle: Arc<Self>,
        stop:   StopToken,
        inbox:  Arc<Mailbox<ActorMsg<Self::Message>>>,
    ) -> impl Future<Output = ()> + Send
    where Self: Sized;
}

/// Generic wrapper that adapts any [`DriverTask`] into a [`Driver`].
///
/// Owns the lifecycle atomics (`running`, `stop_flag`), the inner task behind
/// an `Arc`, and the driver's [`Mailbox`].
///
/// Construct with [`TaskDriver::new`], which returns `(TaskDriver<T>,
/// Arc<Mailbox<ActorMsg<T::Message>>>)`.  Hold onto the `Arc<Mailbox>` to
/// send typed messages to the running driver task.
pub struct TaskDriver<T: DriverTask> {
    task:      Arc<T>,
    running:   Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
    inbox:     Arc<Mailbox<ActorMsg<T::Message>>>,
}

impl<T: DriverTask> TaskDriver<T> {
    /// Create a new driver and return both the driver and a sender handle.
    ///
    /// The caller should keep the `Arc<Mailbox<ActorMsg<T::Message>>>` to send
    /// messages to the driver once it is started.  The `Driver` registry only
    /// holds the lifecycle-level `Box<dyn Driver>`; typed messaging stays
    /// out-of-band.
    pub fn new(task: T) -> (Self, Arc<Mailbox<ActorMsg<T::Message>>>) {
        let inbox = Mailbox::new(16);
        inbox.close(); // starts closed; opened by start() via reopen()
        let driver = TaskDriver {
            task:      Arc::new(task),
            running:   Arc::new(AtomicBool::new(false)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            inbox:     inbox.clone(),
        };
        (driver, inbox)
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
        self.inbox.reopen();
        self.running.store(true,  Ordering::Release);
        let handle  = self.task.clone();
        let stop    = StopToken(self.stop_flag.clone());
        let running = self.running.clone();
        let inbox   = self.inbox.clone();
        executor::spawn(Task::new(async move {
            T::run(handle, stop, inbox).await;
            running.store(false, Ordering::Release);
        }));
    }

    fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
        self.inbox.close();
    }
}
