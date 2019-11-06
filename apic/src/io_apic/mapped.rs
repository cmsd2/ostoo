use x86_64::VirtAddr;
use apic_types::io::{IoApic, IoApicRegister, IoApic32BitRegisterIndex, IoApic64BitRegisterIndex};
use apic_types::io::{ArbitrationIdRegister, IdRegister, VersionRegister, VersionFlags};

pub struct MappedIoApic {
    pub id: u8,
    pub base_addr: VirtAddr,
    pub interrupt_base: u32,
}

impl MappedIoApic {
    fn io_reg_sel_mut(&self) -> *mut u32 {
        self.base_addr.as_mut_ptr::<u32>()
    }

    fn io_reg_win_mut(&self) -> *mut u32 {
        (self.base_addr + 0x10u64).as_mut_ptr::<u32>()
    }

    fn io_reg_win(&self) -> *const u32 {
        (self.base_addr + 0x10u64).as_mut_ptr::<u32>()
    }

    fn indexes_for_64bit_registers(&self, index: IoApic64BitRegisterIndex) -> (u32, u32) {
        match index {
            IoApic64BitRegisterIndex::RedirectionEntry(irq) => {
                (0x10 + irq * 2, 0x11 + irq * 2)
            }
        }
    }
    
    pub fn init(&self) {
        let id = unsafe { IdRegister.read(self) };
        let arb = unsafe { ArbitrationIdRegister.read(self) };
        let ver_reg = unsafe { VersionRegister.read(self) };
        let ver = ver_reg.version();
        let max_reds = ver_reg.max_redirect_entry();

        info!("[apic] init io_apic id={:?} arb={:?} ver={:?} max_reds={:?}", id, arb, ver, max_reds);
    }
}

impl IoApic for MappedIoApic {
    unsafe fn read_reg_32(&self, index: IoApic32BitRegisterIndex) -> u32 {
        *self.io_reg_sel_mut() = index.as_u32();
        let value = *self.io_reg_win();
        info!("[apic] read sel={:?} win={:?}", index.as_u32(), value);
        value
    }

    unsafe fn write_reg_32(&self, index: IoApic32BitRegisterIndex, value: u32) {
        *self.io_reg_sel_mut() = index.as_u32();
        *self.io_reg_win_mut() = value;
        info!("[apic] write sel={:?} win={:?}", index.as_u32(), value);
    }

    unsafe fn read_reg_64(&self, index: IoApic64BitRegisterIndex) -> u64 {
        let (low_addr, high_addr) = self.indexes_for_64bit_registers(index);

        *self.io_reg_sel_mut() = low_addr;
        let low = *self.io_reg_win();

        *self.io_reg_sel_mut() = high_addr;
        let high = *self.io_reg_win();

        ((high as u64) << 32) | low as u64
    }

    unsafe fn write_reg_64(&self, index: IoApic64BitRegisterIndex, value: u64) {
        let (low_addr, high_addr) = self.indexes_for_64bit_registers(index);
        let low = value as u32;
        let high = (value >> 32) as u32;

        *self.io_reg_sel_mut() = low_addr;
        *self.io_reg_win_mut() = low;

        *self.io_reg_sel_mut() = high_addr;
        *self.io_reg_win_mut() = high;
    }
}