//! Timeline event queue — ISR producer, async actor consumer.
//!
//! The scheduler ISR pushes context-switch events into a lock-free queue.
//! The timeline actor pops them and draws coloured blocks on VGA row 1.

use core::pin::Pin;
use core::task::{Context, Poll};
use conquer_once::spin::OnceCell;
use crossbeam_queue::ArrayQueue;
use futures_util::stream::Stream;
use futures_util::task::AtomicWaker;

use super::{WRITER, Color, ColorCode, ScreenChar};

static TIMELINE_QUEUE: OnceCell<ArrayQueue<usize>> = OnceCell::uninit();
static TIMELINE_WAKER: AtomicWaker = AtomicWaker::new();

/// A stream of thread indices pushed by the scheduler ISR.
pub struct TimelineStream {
    _private: (),
}

impl TimelineStream {
    pub fn new() -> Self {
        let _ = TIMELINE_QUEUE.try_init_once(|| ArrayQueue::new(256));
        TimelineStream { _private: () }
    }
}

impl Stream for TimelineStream {
    type Item = usize;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<usize>> {
        let queue = TIMELINE_QUEUE.try_get().expect("timeline queue not initialized");

        if let Some(idx) = queue.pop() {
            return Poll::Ready(Some(idx));
        }

        TIMELINE_WAKER.register(cx.waker());

        match queue.pop() {
            Some(idx) => {
                TIMELINE_WAKER.take();
                Poll::Ready(Some(idx))
            }
            None => Poll::Pending,
        }
    }
}

/// Push a context-switch event into the timeline queue.
///
/// Called from the scheduler ISR (`preempt_tick`).  Lock-free and
/// allocation-free; safe to call with interrupts disabled.
pub fn timeline_append(thread_idx: usize) {
    if let Ok(queue) = TIMELINE_QUEUE.try_get() {
        // Drop the event if the queue is full — a few missed visual ticks
        // are invisible at screen refresh rates.
        let _ = queue.push(thread_idx);
        TIMELINE_WAKER.wake();
    }
}

/// Write one context-switch tick to VGA row 1, shifting old blocks left.
///
/// Called by the timeline actor from normal (non-ISR) context while holding
/// the `WRITER` lock.
pub fn timeline_flush_one(thread_idx: usize) {
    if super::DISPLAY_SUPPRESSED.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    const THREAD_BG: [Color; 6] = [
        Color::LightGreen, Color::LightCyan, Color::LightRed,
        Color::Pink,       Color::Yellow,    Color::LightGray,
    ];
    let bg = THREAD_BG[thread_idx % THREAD_BG.len()];
    let color = ColorCode::new(Color::Black, bg);

    let mut w = WRITER.lock();
    let cols = w.cols;
    // Shift row 1 left by one column.
    for col in 0..cols - 1 {
        let ch = w.read_cell(1, col + 1);
        w.write_cell(1, col, ch);
    }
    // Append a space with the thread's background colour.
    w.write_cell(1, cols - 1, ScreenChar {
        ascii_character: b' ',
        color_code: color,
    });
}
