use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll, Waker};
use spin::Mutex;
use x86_64::instructions::interrupts;

/// LAPIC timer is configured at 1000 Hz → 1 tick = 1 ms.
pub const TICKS_PER_SECOND: u64 = 1000;

const MAX_WAKERS: usize = 16;

static TICK_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKERS: Mutex<[Option<Waker>; MAX_WAKERS]> = Mutex::new([
    None, None, None, None, None, None, None, None,
    None, None, None, None, None, None, None, None,
]);

/// Called by the timer interrupt handler on every tick. Must not block or allocate.
/// Interrupts are already disabled by the CPU on IDT dispatch.
pub(crate) fn tick() {
    TICK_COUNT.fetch_add(1, Ordering::Release);
    let mut wakers = WAKERS.lock();
    for slot in wakers.iter_mut() {
        if let Some(w) = slot.take() {
            w.wake();
        }
    }
}

/// Returns the current raw tick count.
pub fn ticks() -> u64 {
    TICK_COUNT.load(Ordering::Acquire)
}

/// Busy-waits until `n` ticks have elapsed, using HLT between checks.
/// Requires interrupts to be enabled. Safe to call before the executor starts.
pub fn wait_ticks(n: u64) {
    let target = TICK_COUNT.load(Ordering::Acquire) + n;
    loop {
        if TICK_COUNT.load(Ordering::Acquire) >= target {
            return;
        }
        x86_64::instructions::hlt();
    }
}

/// A future that resolves after the given number of ticks.
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
        if TICK_COUNT.load(Ordering::Acquire) >= self.target {
            return Poll::Ready(());
        }
        // Clone waker in task context before disabling interrupts (clone may allocate).
        let waker = cx.waker().clone();
        interrupts::without_interrupts(|| {
            let mut wakers = WAKERS.lock();
            for slot in wakers.iter_mut() {
                if slot.is_none() {
                    *slot = Some(waker);
                    return;
                }
            }
            panic!("timer: WAKERS full — increase MAX_WAKERS");
        });
        // Re-check: ISR may have fired between the first check and registration.
        if TICK_COUNT.load(Ordering::Acquire) >= self.target {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{serial_print, serial_println};
    use core::sync::atomic::Ordering;
    use super::{Delay, TICK_COUNT, TICKS_PER_SECOND};

    /// Verify that `from_millis` sets `target` to current_tick + expected_ticks.
    /// We bracket the creation with two tick reads to account for a timer interrupt
    /// firing between the snapshot and the `Delay::new` read.
    fn check_millis(ms: u64, expected: u64) {
        let before = TICK_COUNT.load(Ordering::Acquire);
        let d = Delay::from_millis(ms);
        let after = TICK_COUNT.load(Ordering::Acquire);
        assert!(d.target >= before + expected,
            "from_millis({ms}): target {} < before {} + {expected}", d.target, before);
        assert!(d.target <= after + expected,
            "from_millis({ms}): target {} > after {} + {expected}", d.target, after);
    }

    #[test_case]
    fn test_delay_from_millis() {
        serial_print!("test_delay_from_millis... ");
        check_millis(0,    0);
        check_millis(1,    1);
        check_millis(500,  500);
        check_millis(999,  999);
        check_millis(1000, TICKS_PER_SECOND);
        check_millis(1001, 1001);
        check_millis(2000, 2 * TICKS_PER_SECOND);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_delay_from_secs() {
        serial_print!("test_delay_from_secs... ");
        let before = TICK_COUNT.load(Ordering::Acquire);
        let d0 = Delay::from_secs(0);
        let d1 = Delay::from_secs(1);
        let d5 = Delay::from_secs(5);
        let after = TICK_COUNT.load(Ordering::Acquire);

        assert!(d0.target >= before && d0.target <= after);
        assert!(d1.target >= before + TICKS_PER_SECOND
             && d1.target <= after + TICKS_PER_SECOND);
        assert!(d5.target >= before + 5 * TICKS_PER_SECOND
             && d5.target <= after + 5 * TICKS_PER_SECOND);
        serial_println!("[ok]");
    }
}
