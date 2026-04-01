//! Generic completion port: bounded queue + single-waiter wake.

use alloc::collections::VecDeque;

/// Wake a blocked waiter by opaque token (e.g. thread index).
pub trait Waker {
    fn wake(&self, token: usize);
}

const DEFAULT_MAX_QUEUED: usize = 256;

/// A completion port: bounded queue of completions with single-waiter wake.
///
/// `C` is the completion type. `W` implements waking a blocked waiter.
pub struct CompletionPort<C, W: Waker> {
    queue: VecDeque<C>,
    waiter: Option<usize>,
    max_queued: usize,
    waker: W,
}

impl<C, W: Waker> CompletionPort<C, W> {
    /// Create a new completion port with default queue capacity.
    pub fn new(waker: W) -> Self {
        CompletionPort {
            queue: VecDeque::new(),
            waiter: None,
            max_queued: DEFAULT_MAX_QUEUED,
            waker,
        }
    }

    /// Create a new completion port with a custom max queued limit.
    pub fn with_max_queued(waker: W, max: usize) -> Self {
        CompletionPort {
            queue: VecDeque::new(),
            waiter: None,
            max_queued: max,
            waker,
        }
    }

    /// Post a completion to the queue. Wakes the waiter if one is registered.
    ///
    /// Returns the thread token that was woken (if any).
    pub fn post(&mut self, c: C) -> Option<usize> {
        if self.queue.len() < self.max_queued {
            self.queue.push_back(c);
        }
        self.wake_waiter()
    }

    /// Wake the registered waiter (if any) without posting a completion.
    ///
    /// Useful for ring fast-path where the CQE was written directly to shared
    /// memory and we only need to wake the blocked consumer.
    ///
    /// Returns the thread token that was woken (if any).
    pub fn wake_waiter(&mut self) -> Option<usize> {
        if let Some(t) = self.waiter.take() {
            self.waker.wake(t);
            Some(t)
        } else {
            None
        }
    }

    /// Drain up to `max` completions from the queue.
    pub fn drain(&mut self, max: usize) -> VecDeque<C> {
        let count = max.min(self.queue.len());
        self.queue.drain(..count).collect()
    }

    /// Register the current thread as waiter. Only one waiter at a time.
    pub fn set_waiter(&mut self, token: usize) {
        self.waiter = Some(token);
    }

    /// Number of pending completions in the queue.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}
