use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll};
use futures_util::task::AtomicWaker;

/// PIT is configured at 100 Hz → 1 tick = 10 ms.
pub const TICKS_PER_SECOND: u64 = 100;

static WAKER: AtomicWaker = AtomicWaker::new();
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Called by the timer interrupt handler on every PIT tick. Must not block or allocate.
pub(crate) fn tick() {
    TICK_COUNT.fetch_add(1, Ordering::Release);
    WAKER.wake();
}

/// Returns the current raw tick count.
pub fn ticks() -> u64 {
    TICK_COUNT.load(Ordering::Acquire)
}

/// A future that resolves after `ticks` PIT ticks (1 tick = 10 ms at 100 Hz).
pub struct Delay {
    target: u64,
}

impl Delay {
    pub fn new(ticks: u64) -> Self {
        Delay { target: TICK_COUNT.load(Ordering::Acquire) + ticks }
    }

    pub fn from_millis(ms: u64) -> Self {
        Self::new((ms * TICKS_PER_SECOND + 999) / 1000)
    }

    pub fn from_secs(secs: u64) -> Self {
        Self::new(secs * TICKS_PER_SECOND)
    }
}

impl Future for Delay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let current = TICK_COUNT.load(Ordering::Acquire);
        if current >= self.target {
            return Poll::Ready(());
        }
        // Register waker before re-checking to avoid a race with the ISR.
        WAKER.register(cx.waker());
        if TICK_COUNT.load(Ordering::Acquire) >= self.target {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
