use alloc::boxed::Box;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use libkernel::task::timer::Delay;
use super::task_driver::{DriverTask, StopToken, TaskDriver};

/// Zero-sized marker type; all lifecycle state lives in [`TaskDriver<Dummy>`].
pub struct Dummy;

impl DriverTask for Dummy {
    fn name(&self) -> &'static str { "dummy" }

    fn info(&self, out: &mut dyn FnMut(&str, &str)) {
        out("heartbeat_interval", "5s");
    }

    fn run(_handle: Arc<Self>, stop: StopToken) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            info!("[dummy] started");
            loop {
                if stop.is_stopped() { break; }
                Delay::from_secs(5).await;
                if stop.is_stopped() { break; }
                info!("[dummy] heartbeat");
            }
            info!("[dummy] stopped");
        })
    }
}

pub type DummyDriver = TaskDriver<Dummy>;
