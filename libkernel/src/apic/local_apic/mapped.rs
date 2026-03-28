use x86_64::{PhysAddr, VirtAddr};
use x86_64::registers::model_specific::Msr;
use apic_types::local::*;
use super::msr::ApicBaseMsrFlags;
use crate::cpuid;

const IA32_APIC_BASE_MSR: u32 = 0x1b;

pub struct MappedLocalApic {
    base_addr: VirtAddr,
}

impl MappedLocalApic {
    /// Create a new `MappedLocalApic`.
    ///
    /// # Safety
    /// `base_addr` must point to a valid, permanently mapped LAPIC MMIO
    /// region (typically 4 KiB starting at the IA32_APIC_BASE physical
    /// address).  The caller must ensure the mapping lives for the lifetime
    /// of the returned value.
    pub unsafe fn new(base_addr: VirtAddr) -> Self {
        MappedLocalApic { base_addr }
    }

    pub fn base_addr(&self) -> VirtAddr {
        self.base_addr
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

    pub fn is_global_enabled(&self) -> bool {
        let value = unsafe { Self::read_base_msr() };
        let flags = ApicBaseMsrFlags::from_bits_truncate(value);
        flags.contains(ApicBaseMsrFlags::GLOBAL_ENABLE)
    }

    pub fn global_disable(&self) {
        unsafe {
            let value = Self::read_base_msr();
            let mut flags = ApicBaseMsrFlags::from_bits_unchecked(value);
            flags.set(ApicBaseMsrFlags::GLOBAL_ENABLE, false);
            Self::write_base_msr(flags.bits());
        }
    }

    pub fn id(&self) -> u8 {
        let apic_id = if cpuid::is_p4_or_xeon_or_later().expect("cpuid") {
            unsafe { Id8BitRegister.read(self) }
        } else {
            unsafe { Id4BitRegister.read(self) }
        };
        match apic_id {
            ApicId::Id8Bit(id) | ApicId::Id4Bit(id) => id as u8,
        }
    }

    pub fn enable(&self) {
        unsafe {
            let mut sivr = SpuriousInterruptVectorRegister.read(self);
            sivr.insert(SivrFlags::APIC_ENABLE | SivrFlags::VECTOR);
            SpuriousInterruptVectorRegister.write(self, sivr);
        }
    }

    pub fn eoi(&self) {
        unsafe { self.write_reg_32(LocalApicRegisterIndex::EndOfInterrupt, 0); }
    }

    /// Start LAPIC timer in one-shot mode (used for calibration).
    /// `divide_bits`: bits[3,1:0] of the divide configuration register (e.g. 0x3 = divide-by-16).
    pub fn start_oneshot_timer(&self, initial_count: u32, divide_bits: u8, vector: u8) {
        unsafe {
            self.write_reg_32(LocalApicRegisterIndex::TimerDivideConfiguration, divide_bits as u32);
            // LVT: one-shot = bits[18:17] = 0b00, unmasked (bit 16 = 0)
            self.write_reg_32(LocalApicRegisterIndex::LvtTimer, vector as u32);
            self.write_reg_32(LocalApicRegisterIndex::TimerInitialCount, initial_count);
        }
    }

    /// Start LAPIC timer in periodic mode.
    pub fn start_periodic_timer(&self, initial_count: u32, divide_bits: u8, vector: u8) {
        unsafe {
            self.write_reg_32(LocalApicRegisterIndex::TimerDivideConfiguration, divide_bits as u32);
            // LVT: periodic = bit 17 set, unmasked (bit 16 = 0)
            self.write_reg_32(LocalApicRegisterIndex::LvtTimer, (vector as u32) | (1 << 17));
            self.write_reg_32(LocalApicRegisterIndex::TimerInitialCount, initial_count);
        }
    }

    /// Mask the LAPIC timer (sets bit 16 in LVT Timer).
    pub fn stop_timer(&self) {
        unsafe {
            let lvt = self.read_reg_32(LocalApicRegisterIndex::LvtTimer);
            self.write_reg_32(LocalApicRegisterIndex::LvtTimer, lvt | (1 << 16));
        }
    }

    /// Read the current countdown value (decrements toward 0; reloads at 0 if periodic).
    pub fn read_current_count(&self) -> u32 {
        unsafe { self.read_reg_32(LocalApicRegisterIndex::TimerCurrentCount) }
    }

    /// Raw LVT Timer register (bits 0-7 = vector, 16 = mask, 17-18 = mode).
    pub fn read_lvt_timer(&self) -> u32 {
        unsafe { self.read_reg_32(LocalApicRegisterIndex::LvtTimer) }
    }

    /// Timer initial count register.
    pub fn read_timer_initial_count(&self) -> u32 {
        unsafe { self.read_reg_32(LocalApicRegisterIndex::TimerInitialCount) }
    }

    /// Raw LAPIC Version register (bits 0-7 = version, 16-23 = max LVT entry).
    pub fn read_version_raw(&self) -> u32 {
        unsafe { self.read_reg_32(LocalApicRegisterIndex::Version) }
    }

    pub fn init(&self) {
        unsafe {
            info!("[apic] init phys_addr={:?} enabled={}", Self::get_base_phys_addr(), self.is_global_enabled());
        }

        let id = if cpuid::is_p4_or_xeon_or_later().expect("cpuid") {
            unsafe { Id8BitRegister.read(self) }
        } else {
            unsafe { Id4BitRegister.read(self) }
        };

        let version = unsafe { VersionRegister.read(self) };

        info!("[apic] init id={:?} version={:?}", id, version);

        let icr = unsafe { InterruptCommandRegister.read(self) };
        let ldr = unsafe { LogicalDestinationRegister.read(self) };
        let tpr = unsafe { TaskPriorityRegister.read(self) };
        info!("[apic] init icr={:?} ldr={:?} tpr={:?}", icr, ldr, tpr);

        let timer_ic = unsafe { LvtTimerInitialCountRegister.read(self) };
        let timer_cc = unsafe { LvtTimerCurrentCountRegister.read(self) };
        let timer_div = unsafe { LvtTimerDivideConfigurationRegister.read(self) };
        info!("[apic] init initial_count={:?} current_count={:?} divide_config={:?}", timer_ic, timer_cc, timer_div);

        let dfr = unsafe { DestinationFormatRegister.read(self) };
        info!("[apic] init dfr={:?}", dfr);

        let lvt_timer = unsafe { LvtTimerRegister.read(self) };
        let lvt_cmci = unsafe { LvtCmciRegister.read(self) };
        let lvt_lint0 = unsafe { LvtLint0Register.read(self) };
        let lvt_lint1 = unsafe { LvtLint1Register.read(self) };
        let lvt_error = unsafe { LvtErrorRegister.read(self) };
        let lvt_perf = unsafe { LvtPerfCountersRegister.read(self) };
        let lvt_thermal = unsafe { LvtThermalSensorRegister.read(self) };
        info!("[apic] init timer={:?} cmci={:?} lint0={:?} lint1={:?} error={:?} perf={:?} thermal={:?}",
            lvt_timer, lvt_cmci, lvt_lint0, lvt_lint1, lvt_error, lvt_perf, lvt_thermal);
    }
}
impl LocalApic for MappedLocalApic {
    unsafe fn read_reg_32(&self, register: LocalApicRegisterIndex) -> u32 {
        let addr = self.base_addr + register.as_u64();
        let ptr = addr.as_ptr::<u32>();
        core::ptr::read_volatile(ptr)
    }

    unsafe fn write_reg_32(&self, register: LocalApicRegisterIndex, value: u32) {
        let addr = self.base_addr + register.as_u64();
        let ptr = addr.as_mut_ptr::<u32>();
        core::ptr::write_volatile(ptr, value);
    }
}
