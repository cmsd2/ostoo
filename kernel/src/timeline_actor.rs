//! Timeline actor — drains context-switch events from the ISR queue and
//! renders them to VGA row 1 through the safe `WRITER` interface.

use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::vga_buffer::TimelineStream;

// ---------------------------------------------------------------------------
// Messages (none — this actor is stream-driven only)

pub enum TimelineMsg {}

// ---------------------------------------------------------------------------
// Info

#[derive(Debug)]
#[allow(dead_code)]
pub struct TimelineInfo {
    pub events_rendered: u64,
}

// ---------------------------------------------------------------------------
// Actor

pub struct TimelineActor {
    events_rendered: AtomicU64,
}

impl TimelineActor {
    pub fn new() -> Self {
        TimelineActor {
            events_rendered: AtomicU64::new(0),
        }
    }
}

#[devices::actor("timeline", TimelineMsg)]
impl TimelineActor {
    fn timeline_stream(&self) -> TimelineStream { TimelineStream::new() }

    #[on_stream(timeline_stream)]
    async fn on_event(&self, thread_idx: usize) {
        libkernel::vga_buffer::timeline_flush_one(thread_idx);
        self.events_rendered.fetch_add(1, Ordering::Relaxed);
    }

    #[on_info]
    async fn on_info(&self) -> TimelineInfo {
        TimelineInfo {
            events_rendered: self.events_rendered.load(Ordering::Relaxed),
        }
    }
}
