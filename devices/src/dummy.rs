use core::sync::atomic::{AtomicU64, Ordering};
use crate::actor;

#[derive(Debug)]
pub struct DummyInfo {
    pub interval_secs: u64,
}

pub enum DummyMsg {
    SetInterval(u64),
}

pub struct Dummy {
    interval_secs: AtomicU64,
}

impl Dummy {
    pub fn new() -> Self {
        Dummy { interval_secs: AtomicU64::new(1) }
    }
}

#[actor("dummy", DummyMsg)]
impl Dummy {
    #[on_info]
    async fn on_info(&self) -> DummyInfo {
        DummyInfo {
            interval_secs: self.interval_secs.load(Ordering::Relaxed),
        }
    }

    #[on_message(SetInterval)]
    async fn set_interval(&self, secs: u64) {
        self.interval_secs.store(secs, Ordering::Relaxed);
        info!("[dummy] interval set to {}s", secs);
    }
}
