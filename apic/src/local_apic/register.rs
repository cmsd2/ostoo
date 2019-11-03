use x86_64::{PhysAddr, VirtAddr};
use x86_64::registers::model_specific::Msr;
use core::convert::TryFrom;
use core::result::Result;
use apic_types::local::*;
use super::msr::ApicBaseMsrFlags;

const IA32_APIC_BASE_MSR: u32 = 0x1b;


/*
    pub unsafe fn is_enabled(&self) -> bool {
        self.read_sivr().contains(SivrFlags::ENABLE)
    }

    pub unsafe fn enable(&self) {
        let mut flags = self.read_sivr();
        flags.set(SivrFlags::ENABLE, true);
        self.write_sivr(flags);
    }

    pub unsafe fn read_sivr(&self) -> SivrFlags {
        let sivr = self.read_reg_32(LocalApicRegister::Sivr);
        SivrFlags::from_bits_truncate(sivr)
    }

    pub unsafe fn write_sivr(&self, flags: SivrFlags) {
        self.write_reg_32(LocalApicRegister::Sivr, flags.bits());
    }
*/

pub struct LocalApicRegisters {
    pub base_addr: VirtAddr,
}

impl LocalApicRegisters {
    pub fn new(base_addr: VirtAddr) -> Self {
        LocalApicRegisters {
            base_addr: base_addr,
        }
    }

    pub fn msr() -> Msr {
        Msr::new(IA32_APIC_BASE_MSR)
    }

    pub unsafe fn read_base_msr() -> u64 {
        let msr = Self::msr();
        msr.read()
    }

    pub unsafe fn write_base_msr(value: u64) {
        let mut msr = Self::msr();
        msr.write(value)
    }

    pub unsafe fn get_base_phys_addr() -> PhysAddr {
        let value = Self::read_base_msr();
        PhysAddr::new(value & !0xfff)
    }

    pub unsafe fn is_global_enabled(&self) -> bool {
        let value = Self::read_base_msr();
        let flags = ApicBaseMsrFlags::from_bits_truncate(value);
        flags.contains(ApicBaseMsrFlags::GLOBAL_ENABLE)
    }

    pub unsafe fn global_disable(&self) {
        let value = Self::read_base_msr();
        let mut flags = ApicBaseMsrFlags::from_bits_unchecked(value);
        flags.set(ApicBaseMsrFlags::GLOBAL_ENABLE, false);
        Self::write_base_msr(flags.bits());
    }
}

impl LocalApic for LocalApicRegisters {
    unsafe fn read_reg_32(&self, register: LocalApicRegisterIndex) -> u32 {
        let addr = self.base_addr + register.as_u64();
        let ptr = addr.as_ptr::<u32>();
        *ptr
    }

    unsafe fn write_reg_32(&self, register: LocalApicRegisterIndex, value: u32) {
        let addr = self.base_addr + register.as_u64();
        let ptr = addr.as_mut_ptr::<u32>();
        *ptr = value;
    }
}
