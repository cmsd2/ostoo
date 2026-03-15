use x86_64::{PhysAddr, VirtAddr};
use x86_64::registers::model_specific::Msr;
use apic_types::local::*;
use super::msr::ApicBaseMsrFlags;
use libkernel::cpuid;

const IA32_APIC_BASE_MSR: u32 = 0x1b;

pub struct MappedLocalApic {
    pub base_addr: VirtAddr,
}

impl MappedLocalApic {
    pub fn new(base_addr: VirtAddr) -> Self {
        MappedLocalApic {
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

    pub unsafe fn id(&self) -> u8 {
        let apic_id = if cpuid::is_p4_or_xeon_or_later().expect("cpuid") {
            Id8BitRegister.read(self)
        } else {
            Id4BitRegister.read(self)
        };
        match apic_id {
            ApicId::Id8Bit(id) | ApicId::Id4Bit(id) => id as u8,
        }
    }

    pub unsafe fn enable(&self) {
        let mut sivr = SpuriousInterruptVectorRegister.read(self);
        sivr.insert(SivrFlags::APIC_ENABLE | SivrFlags::VECTOR);
        SpuriousInterruptVectorRegister.write(self, sivr);
    }

    pub unsafe fn eoi(&self) {
        self.write_reg_32(LocalApicRegisterIndex::EndOfInterrupt, 0);
    }

    /// Start LAPIC timer in one-shot mode (used for calibration).
    /// `divide_bits`: bits[3,1:0] of the divide configuration register (e.g. 0x3 = divide-by-16).
    pub unsafe fn start_oneshot_timer(&self, initial_count: u32, divide_bits: u8, vector: u8) {
        self.write_reg_32(LocalApicRegisterIndex::TimerDivideConfiguration, divide_bits as u32);
        // LVT: one-shot = bits[18:17] = 0b00, unmasked (bit 16 = 0)
        self.write_reg_32(LocalApicRegisterIndex::LvtTimer, vector as u32);
        self.write_reg_32(LocalApicRegisterIndex::TimerInitialCount, initial_count);
    }

    /// Start LAPIC timer in periodic mode.
    pub unsafe fn start_periodic_timer(&self, initial_count: u32, divide_bits: u8, vector: u8) {
        self.write_reg_32(LocalApicRegisterIndex::TimerDivideConfiguration, divide_bits as u32);
        // LVT: periodic = bit 17 set, unmasked (bit 16 = 0)
        self.write_reg_32(LocalApicRegisterIndex::LvtTimer, (vector as u32) | (1 << 17));
        self.write_reg_32(LocalApicRegisterIndex::TimerInitialCount, initial_count);
    }

    /// Mask the LAPIC timer (sets bit 16 in LVT Timer).
    pub unsafe fn stop_timer(&self) {
        let lvt = self.read_reg_32(LocalApicRegisterIndex::LvtTimer);
        self.write_reg_32(LocalApicRegisterIndex::LvtTimer, lvt | (1 << 16));
    }

    /// Read the current countdown value (decrements toward 0; reloads at 0 if periodic).
    pub unsafe fn read_current_count(&self) -> u32 {
        self.read_reg_32(LocalApicRegisterIndex::TimerCurrentCount)
    }

    /// Raw LVT Timer register (bits 0–7 = vector, 16 = mask, 17–18 = mode).
    pub unsafe fn read_lvt_timer(&self) -> u32 {
        self.read_reg_32(LocalApicRegisterIndex::LvtTimer)
    }

    /// Timer initial count register.
    pub unsafe fn read_timer_initial_count(&self) -> u32 {
        self.read_reg_32(LocalApicRegisterIndex::TimerInitialCount)
    }

    /// Raw LAPIC Version register (bits 0–7 = version, 16–23 = max LVT entry).
    pub unsafe fn read_version_raw(&self) -> u32 {
        // Version register is at LAPIC MMIO offset 0x030.
        *(self.base_addr.as_ptr::<u8>().add(0x030) as *const u32)
    }

    pub unsafe fn init(&self) {
        info!("[apic] init phys_addr={:?} enabled={}", Self::get_base_phys_addr(), self.is_global_enabled());

        let id = if cpuid::is_p4_or_xeon_or_later().expect("cpuid") {
            Id8BitRegister.read(self)
        } else {
            Id4BitRegister.read(self)
        };

        let version = VersionRegister.read(self);

        info!("[apic] init id={:?} version={:?}", id, version);

        let icr = InterruptCommandRegister.read(self);
        let ldr = LogicalDestinationRegister.read(self);
        let tpr = TaskPriorityRegister.read(self);
        info!("[apic] init icr={:?} ldr={:?} tpr={:?}", icr, ldr, tpr);

        let timer_ic = LvtTimerInitialCountRegister.read(self);
        let timer_cc = LvtTimerCurrentCountRegister.read(self);
        let timer_div = LvtTimerDivideConfigurationRegister.read(self);
        info!("[apic] init initial_count={:?} current_count={:?} divide_config={:?}", timer_ic, timer_cc, timer_div);

        let dfr = DestinationFormatRegister.read(self);
        info!("[apic] init dfr={:?}", dfr);

        let lvt_timer = LvtTimerRegister.read(self);
        let lvt_cmci = LvtCmciRegister.read(self);
        let lvt_lint0 = LvtLint0Register.read(self);
        let lvt_lint1 = LvtLint1Register.read(self);
        let lvt_error = LvtErrorRegister.read(self);
        let lvt_perf = LvtPerfCountersRegister.read(self);
        let lvt_thermal = LvtThermalSensorRegister.read(self);
        info!("[apic] init timer={:?} cmci={:?} lint0={:?} lint1={:?} error={:?} perf={:?} thermal={:?}",
            lvt_timer, lvt_cmci, lvt_lint0, lvt_lint1, lvt_error, lvt_perf, lvt_thermal);
    }
}

impl LocalApic for MappedLocalApic {
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
