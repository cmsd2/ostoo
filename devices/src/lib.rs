#![no_std]
extern crate alloc;
#[macro_use] extern crate log;

pub use devices_macros::{actor, on_info, on_message, on_start, on_tick, on_stream};
#[macro_use] pub mod macros;
pub mod driver;
pub mod dummy;
pub mod pci;
pub mod task_driver;
