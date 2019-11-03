use x86_64::{PhysAddr, VirtAddr};

mod msr;
mod register;

use register::LocalApicRegisters;

#[derive(Debug)]
pub enum LocalApiError {
    MissingCpuidFeatures
}

pub struct LocalApic {
    pub registers: LocalApicRegisters,
}

impl LocalApic {
    pub fn new(base_addr: VirtAddr) -> LocalApic {
        LocalApic {
            registers: LocalApicRegisters::new(base_addr),
        }
    }

    pub unsafe fn get_base_phys_addr() -> PhysAddr {
        LocalApicRegisters::get_base_phys_addr()
    }
}