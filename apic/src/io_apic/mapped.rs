use x86_64::VirtAddr;
use apic_types::io::{IoApic, IoApicRegister, IoApic32BitRegisterIndex, IoApic64BitRegisterIndex};
use apic_types::io::{ArbitrationIdRegister, IdRegister, VersionRegister};

pub struct MappedIoApic {
    pub id: u8,
    base_addr: VirtAddr,
    pub interrupt_base: u32,
}

impl MappedIoApic {
    /// Create a new `MappedIoApic`.
    ///
    /// # Safety
    /// `base_addr` must point to a valid, permanently mapped IO APIC MMIO
    /// region.  The caller must ensure the mapping lives for the lifetime of
    /// the returned value.
    pub unsafe fn new(id: u8, base_addr: VirtAddr, interrupt_base: u32) -> Self {
        MappedIoApic { id, base_addr, interrupt_base }
    }

    pub fn base_addr(&self) -> VirtAddr {
        self.base_addr
    }

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
    
    pub fn max_redirect_entries(&self) -> u32 {
        unsafe { VersionRegister.read(self) }.max_redirect_entry()
    }

    /// Raw IO APIC Version register (bits 0–7 = version, 16–23 = max redir entry).
    pub fn read_version_raw(&self) -> u32 {
        unsafe { self.read_reg_32(IoApic32BitRegisterIndex::Version) }
    }

    /// Read a single 64-bit redirection entry. `gsi_offset` = gsi − `self.interrupt_base`.
    pub fn read_redirect_entry(&self, gsi_offset: u32) -> u64 {
        unsafe { self.read_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(gsi_offset)) }
    }

    pub fn mask_all(&self) {
        let max = self.max_redirect_entries();
        for i in 0..=max {
            let entry = unsafe { self.read_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(i)) };
            unsafe { self.write_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(i), entry | (1 << 16)); }
        }
    }

    /// Mask a single redirection entry. `gsi_offset` = gsi - `self.interrupt_base`.
    pub fn mask_entry(&self, gsi_offset: u32) {
        let entry = unsafe { self.read_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(gsi_offset)) };
        unsafe { self.write_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(gsi_offset), entry | (1 << 16)); }
    }

    /// Program a redirection entry. `gsi_offset` = gsi - `self.interrupt_base`.
    pub fn set_irq(&self, gsi_offset: u32, vector: u8, lapic_id: u8, active_low: bool, level_triggered: bool) {
        let mut entry: u64 = vector as u64;        // bits 0-7: vector
        if active_low      { entry |= 1 << 13; }   // bit 13: pin polarity (1=active low)
        if level_triggered { entry |= 1 << 15; }   // bit 15: trigger mode (1=level)
        entry |= (lapic_id as u64) << 56;          // bits 56-63: destination LAPIC ID (physical)
        // bit 16 (mask) = 0 → unmasked
        unsafe { self.write_reg_64(IoApic64BitRegisterIndex::RedirectionEntry(gsi_offset), entry); }
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
        core::ptr::write_volatile(self.io_reg_sel_mut(), index.as_u32());
        let value = core::ptr::read_volatile(self.io_reg_win());
        info!("[apic] read sel={:?} win={:?}", index.as_u32(), value);
        value
    }

    unsafe fn write_reg_32(&self, index: IoApic32BitRegisterIndex, value: u32) {
        core::ptr::write_volatile(self.io_reg_sel_mut(), index.as_u32());
        core::ptr::write_volatile(self.io_reg_win_mut(), value);
        info!("[apic] write sel={:?} win={:?}", index.as_u32(), value);
    }

    unsafe fn read_reg_64(&self, index: IoApic64BitRegisterIndex) -> u64 {
        let (low_addr, high_addr) = self.indexes_for_64bit_registers(index);

        core::ptr::write_volatile(self.io_reg_sel_mut(), low_addr);
        let low = core::ptr::read_volatile(self.io_reg_win());

        core::ptr::write_volatile(self.io_reg_sel_mut(), high_addr);
        let high = core::ptr::read_volatile(self.io_reg_win());

        ((high as u64) << 32) | low as u64
    }

    unsafe fn write_reg_64(&self, index: IoApic64BitRegisterIndex, value: u64) {
        let (low_addr, high_addr) = self.indexes_for_64bit_registers(index);
        let low = value as u32;
        let high = (value >> 32) as u32;

        core::ptr::write_volatile(self.io_reg_sel_mut(), low_addr);
        core::ptr::write_volatile(self.io_reg_win_mut(), low);

        core::ptr::write_volatile(self.io_reg_sel_mut(), high_addr);
        core::ptr::write_volatile(self.io_reg_win_mut(), high);
    }
}