use crate::actor;

pub enum DummyMsg {
    SetInterval(u64),
}

pub struct Dummy;

#[actor("dummy", DummyMsg)]
impl Dummy {
    #[on_message(SetInterval)]
    async fn set_interval(&self, secs: u64) {
        info!("[dummy] interval set to {}s — takes effect next heartbeat", secs);
    }
}
