//! Single-waiter condvar for kernel blocking.
//!
//! Encapsulates the check → register → mark_blocked → unlock → yield cycle.
//! The type ensures mark_blocked() runs while the caller's lock is held.

use crate::task::scheduler;

pub struct WaitCondition;

impl WaitCondition {
    /// If `guard` is `Some`, register the waiter, mark the current thread
    /// Blocked (while the guard/lock is held), drop the guard, and yield.
    /// If `guard` is `None`, return immediately (condition already met).
    ///
    /// The caller acquires a lock, checks a condition, and passes
    /// `Some(guard)` to block or `None` to skip:
    ///
    /// ```text
    /// WaitCondition::wait_while(
    ///     {
    ///         let guard = resource.lock();
    ///         if condition_met(&guard) { None } else { Some(guard) }
    ///     },
    ///     |guard, thread_idx| { guard.waiter = Some(thread_idx); },
    /// );
    /// ```
    // [spec: completion_port/completion_port.tla
    //   CheckAndAct (check + register + mark_blocked) = one atomic step
    //   WaitUnblocked (yield_now) = next step]
    pub fn wait_while<G>(guard: Option<G>, register: impl FnOnce(&mut G, usize)) {
        if let Some(mut guard) = guard {
            let thread_idx = scheduler::current_thread_idx();
            register(&mut guard, thread_idx);
            scheduler::mark_blocked();
            drop(guard);
            scheduler::yield_now();
        }
    }
}
