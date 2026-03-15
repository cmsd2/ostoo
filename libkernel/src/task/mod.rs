use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::boxed::Box;

pub mod executor;

/// Poll a [`Stream`] for its next item without requiring `StreamExt` in scope.
///
/// Used by the `#[actor]` macro's generated run loop so that crates using the
/// macro do not need a direct `futures_util` dependency.
///
/// [`Stream`]: futures_util::stream::Stream
pub fn poll_stream_next<S>(
    stream: &mut S,
    cx:     &mut core::task::Context<'_>,
) -> core::task::Poll<core::option::Option<S::Item>>
where
    S: futures_util::stream::Stream + core::marker::Unpin,
{
    use futures_util::stream::StreamExt;
    stream.poll_next_unpin(cx)
}
pub mod simple_executor;
pub mod keyboard;
pub mod mailbox;
pub mod registry;
pub mod timer;
pub mod scheduler;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TaskId(u64);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Task {
    pub fn new(future: impl Future<Output = ()> + Send + 'static) -> Task {
        Task {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }

    pub fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}

