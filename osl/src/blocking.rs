//! Async-to-sync bridge: run an async future to completion from a blocking
//! syscall context.

use alloc::sync::Arc;
use core::future::Future;
use libkernel::spin_mutex::SpinMutex as Mutex;

use libkernel::task::scheduler;
use libkernel::task::executor;
use libkernel::task::Task;
use libkernel::wait_condition::WaitCondition;

/// Run an async future to completion, blocking the current scheduler thread.
///
/// Spawns the future as a kernel async task. When it completes, the blocked
/// thread is unblocked and the result is returned.
// [spec: completion_port/completion_port.tla — atomic check + mark_blocked]
pub fn blocking<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    let result: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let thread_idx = scheduler::current_thread_idx();
    let r = result.clone();

    executor::spawn(Task::new(async move {
        let val = future.await;
        *r.lock() = Some(val);
        scheduler::unblock(thread_idx);
    }));

    WaitCondition::wait_while(
        {
            let guard = result.lock();
            if guard.is_some() { None } else { Some(guard) }
        },
        |_guard, _idx| {},
    );

    let val = result.lock().take().expect("blocking: result missing after unblock");
    val
}
