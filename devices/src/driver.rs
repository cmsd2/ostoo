use alloc::boxed::Box;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use libkernel::spin_mutex::SpinMutex as Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    Stopped,
    Running,
}

impl DriverState {
    pub fn as_str(self) -> &'static str {
        match self {
            DriverState::Running => "Running",
            DriverState::Stopped => "Stopped",
        }
    }
}

pub trait Driver: Send + Sync {
    fn name(&self) -> &'static str;
    fn state(&self) -> DriverState;
    fn start(&self);
    fn stop(&self);
}

lazy_static! {
    static ref DRIVER_REGISTRY: Mutex<Vec<Box<dyn Driver>>> = Mutex::new(Vec::new());
}

pub fn register(driver: Box<dyn Driver>) {
    DRIVER_REGISTRY.lock().push(driver);
}

pub fn start_driver(name: &str) -> Result<(), &'static str> {
    let reg = DRIVER_REGISTRY.lock();
    match reg.iter().find(|d| d.name() == name) {
        None => Err("driver not found"),
        Some(d) => {
            if d.state() == DriverState::Running {
                Err("already running")
            } else {
                d.start();
                Ok(())
            }
        }
    }
}

pub fn stop_driver(name: &str) -> Result<(), &'static str> {
    let reg = DRIVER_REGISTRY.lock();
    match reg.iter().find(|d| d.name() == name) {
        None => Err("driver not found"),
        Some(d) => {
            if d.state() == DriverState::Stopped {
                Err("already stopped")
            } else {
                d.stop();
                Ok(())
            }
        }
    }
}

pub fn with_drivers<F: FnMut(&str, DriverState)>(mut f: F) {
    let reg = DRIVER_REGISTRY.lock();
    for d in reg.iter() {
        f(d.name(), d.state());
    }
}

