pub mod apr;
pub mod dfr;
pub mod icr;
pub mod id;
pub mod ldr;
pub mod sivr;
pub mod version;
pub mod registers;

pub use apr::*;
pub use dfr::*;
pub use icr::*;
pub use id::*;
pub use ldr::*;
pub use sivr::*;
pub use version::*;
pub use registers::*;

pub trait LocalApic {
    unsafe fn read_reg_32(&self, index: LocalApicRegisterIndex) -> u32;
    unsafe fn write_reg_32(&self, index: LocalApicRegisterIndex, value: u32);
}
