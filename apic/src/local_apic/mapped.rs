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
