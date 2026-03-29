use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::fmt;
use core::future::Future;
use futures_util::task::AtomicWaker;
use core::pin::Pin;
use core::task::{Context, Poll};
use crate::spin_mutex::SpinMutex as Mutex;

// ---------------------------------------------------------------------------
// ActorMsg — generic envelope for all actor mailboxes

/// Response to an [`ActorMsg::Info`] or [`ActorMsg::ErasedInfo`] request.
///
/// `I` is the actor-specific detail type returned by `#[on_info]`.  For
/// type-erased registry queries ([`ActorMsg::ErasedInfo`]) `I` is `()`.
pub struct ActorStatus<I = ()> {
    /// Static name registered with the driver framework.
    pub name: &'static str,
    /// Always `true` when returned — the actor is running if it responds.
    pub running: bool,
    /// Actor-specific detail, populated by `#[on_info]`.
    pub info: I,
}

/// Envelope type for actor mailboxes.
///
/// Every actor's `Mailbox` is parameterised over `ActorMsg<M, I>` where `M`
/// is the actor-specific message type and `I` is the info detail type.
/// Type alias for the erased info carried by [`ActorMsg::ErasedInfo`].
///
/// Any concrete info type that implements [`fmt::Display`] can be boxed into
/// this type, preserving displayability without exposing the concrete type to
/// callers that don't know the actor's `Info` type.
pub type ErasedInfo = Box<dyn fmt::Debug + Send>;

pub enum ActorMsg<M, I: Send = ()> {
    /// Typed info request — reply carries the full [`ActorStatus<I>`].
    Info(Reply<ActorStatus<I>>),
    /// Type-erased info request used by the process registry — reply carries
    /// [`ActorStatus<ErasedInfo>`] so callers can display the detail without
    /// knowing the concrete info type.
    ErasedInfo(Reply<ActorStatus<ErasedInfo>>),
    /// An actor-specific message.
    Inner(M),
}

// ---------------------------------------------------------------------------
// RecvTimeout — outcome of a timed receive

pub enum RecvTimeout<M> {
    Message(M),
    Closed,
    Elapsed,
}

// ---------------------------------------------------------------------------
// Mailbox

struct MailboxInner<M> {
    queue:  VecDeque<M>,
    closed: bool,
}

/// Async message queue backed by a mutex.
///
/// - [`send`] enqueues a message under the lock.  If the mailbox is closed
///   the message is **dropped immediately**, which causes any embedded
///   [`Reply`] to close its reply channel and unblock the sender with `None`.
/// - [`recv`] suspends the caller until a message arrives or the mailbox is
///   [`close`]d, returning `Some(msg)` or `None` respectively.
/// - [`close`] drains and drops any queued messages (releasing embedded
///   [`Reply`] objects) and then unblocks any pending [`recv`].
pub struct Mailbox<M> {
    inner: Mutex<MailboxInner<M>>,
    waker: AtomicWaker,
}

impl<M: Send> Mailbox<M> {
    /// Allocate a mailbox.  `capacity` is used as an initial allocation hint
    /// for the internal queue.
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(MailboxInner {
                queue:  VecDeque::with_capacity(capacity),
                closed: false,
            }),
            waker: AtomicWaker::new(),
        })
    }

    /// Enqueue a message and wake the receiver.
    ///
    /// If the mailbox is closed the message is dropped immediately — any
    /// [`Reply`] embedded in it will have its `Drop` impl run, closing the
    /// reply channel and unblocking the sender with `None`.
    pub fn send(&self, msg: M) {
        {
            let mut inner = self.inner.lock();
            if inner.closed {
                return; // msg dropped here; Reply::drop closes the reply mailbox
            }
            inner.queue.push_back(msg);
        }
        self.waker.wake();
    }

    /// Signal that no more messages will be sent.
    ///
    /// Mark the mailbox as closed and wake any pending [`recv`].
    ///
    /// Messages already in the queue are **not** drained — [`recv`] will
    /// deliver them before returning `None`.  Any [`send`] call that arrives
    /// after `close` will drop the message immediately (including any embedded
    /// [`Reply`]), unblocking the corresponding sender with `None`.
    pub fn close(&self) {
        {
            let mut inner = self.inner.lock();
            inner.closed = true;
        }
        self.waker.wake();
    }

    /// Re-open a previously closed mailbox (e.g. before restarting a driver).
    pub fn reopen(&self) {
        self.inner.lock().closed = false;
    }

    /// Return a future that resolves to `Some(msg)` or `None` when closed.
    pub fn recv(&self) -> MailboxRecv<'_, M> {
        MailboxRecv { mailbox: self }
    }

    /// Return a future that resolves when a message arrives, the mailbox is
    /// closed, or `ticks` timer ticks have elapsed — whichever comes first.
    pub fn recv_timeout(&self, ticks: u64)
        -> impl core::future::Future<Output = RecvTimeout<M>> + '_
    {
        use crate::task::timer::Delay;
        let mut recv  = self.recv();
        let mut delay = Delay::new(ticks);
        core::future::poll_fn(move |cx| {
            if let Poll::Ready(opt) = Pin::new(&mut recv).poll(cx) {
                return Poll::Ready(match opt {
                    Some(msg) => RecvTimeout::Message(msg),
                    None      => RecvTimeout::Closed,
                });
            }
            if let Poll::Ready(()) = Pin::new(&mut delay).poll(cx) {
                return Poll::Ready(RecvTimeout::Elapsed);
            }
            Poll::Pending
        })
    }

    /// Non-blocking dequeue; returns `None` if the queue is empty.
    pub fn try_recv(&self) -> Option<M> {
        self.inner.lock().queue.pop_front()
    }

    /// Send a message and await a typed response — the "ask" pattern.
    ///
    /// Creates a one-shot [`Reply<R>`] mailbox, passes it to `make_msg` to
    /// construct the outgoing message, sends it, then suspends until the actor
    /// calls [`Reply::send`].
    ///
    /// Returns `None` if the actor dropped the [`Reply`] without responding
    /// (e.g. because it stopped mid-flight).
    pub fn ask<R, F>(&self, make_msg: F) -> impl Future<Output = Option<R>> + '_
    where
        R: Send + 'static,
        F: FnOnce(Reply<R>) -> M,
    {
        let (reply, rx) = Reply::new();
        self.send(make_msg(reply));
        async move { rx.recv().await }
    }
}

// ---------------------------------------------------------------------------
// Reply — one-shot response channel for request/response messaging.

/// The sending half of a one-shot request-response channel.
///
/// Include a `Reply<R>` in a message variant to let the sender await the
/// actor's response.  The actor calls [`Reply::send`] to deliver the response.
/// If the `Reply` is dropped without a send (e.g. the actor stopped),
/// [`Mailbox::recv`] on the receiving side returns `None` — the `Drop` impl
/// closes the mailbox automatically.
pub struct Reply<T: Send>(Arc<Mailbox<T>>);

impl<T: Send> Reply<T> {
    /// Create a linked (sender, receiver) pair.
    pub fn new() -> (Self, Arc<Mailbox<T>>) {
        let mb = Mailbox::new(1);
        (Reply(mb.clone()), mb)
    }

    /// Deliver the response, consuming this handle.
    ///
    /// The `Drop` impl closes the mailbox after the value is enqueued, so the
    /// receiver is always unblocked.
    pub fn send(self, value: T) {
        self.0.send(value);
        // Drop runs here → close() signals the receiver.
    }
}

impl<T: Send> Drop for Reply<T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

// ---------------------------------------------------------------------------
// MailboxRecv — the Future returned by Mailbox::recv

pub struct MailboxRecv<'a, M> {
    mailbox: &'a Mailbox<M>,
}

impl<M: Send> Future for MailboxRecv<'_, M> {
    type Output = Option<M>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<M>> {
        // First check under the lock.
        {
            let mut inner = self.mailbox.inner.lock();
            if let Some(m) = inner.queue.pop_front() {
                return Poll::Ready(Some(m));
            }
            if inner.closed {
                return Poll::Ready(None);
            }
        }
        // Register the waker while the lock is not held, then check once more
        // to close the window where a send or close could occur between the
        // first check and the registration.
        self.mailbox.waker.register(cx.waker());
        {
            let mut inner = self.mailbox.inner.lock();
            if let Some(m) = inner.queue.pop_front() {
                return Poll::Ready(Some(m));
            }
            if inner.closed {
                return Poll::Ready(None);
            }
        }
        Poll::Pending
    }
}
