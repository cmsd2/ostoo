use x86_64::VirtAddr;
use x86_64::structures::tss::TaskStateSegment;
use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Public selectors, indexed as described in the GDT layout below.
pub struct Selectors {
    pub kernel_code: SegmentSelector,
    pub kernel_data: SegmentSelector,
    pub user_data:   SegmentSelector,
    pub user_code:   SegmentSelector,
    pub tss:         SegmentSelector,
}

// GDT layout (required order for SYSCALL/SYSRET):
//   0x00  Null
//   0x08  Kernel code  (DPL=0) ← STAR[47:32]
//   0x10  Kernel data  (DPL=0) ← SS after SYSCALL (STAR[47:32] + 8)
//   0x18  User data    (DPL=3) ← SS after SYSRETQ  (STAR[63:48] + 8)
//   0x20  User code    (DPL=3) ← CS after SYSRETQ  (STAR[63:48] + 16)
//   0x28  TSS lo
//   0x30  TSS hi       (16-byte system descriptor occupies two slots)
//
// STAR[47:32] = 0x08  → SYSCALL: CS=0x08, SS=0x10
// STAR[63:48] = 0x10  → SYSRETQ: CS=(0x10+16)|3=0x23, SS=(0x10+8)|3=0x1B

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            let stack_end = stack_start + STACK_SIZE as u64;
            stack_end
        };
        tss
    };
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let kernel_code = gdt.append(Descriptor::kernel_code_segment());
        let kernel_data = gdt.append(Descriptor::kernel_data_segment());
        let user_data   = gdt.append(Descriptor::user_data_segment());
        let user_code   = gdt.append(Descriptor::user_code_segment());
        let tss         = gdt.append(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { kernel_code, kernel_data, user_data, user_code, tss })
    };
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, SS, DS, ES, Segment};
    use x86_64::instructions::tables::load_tss;
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::PrivilegeLevel;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kernel_code);
        SS::set_reg(GDT.1.kernel_data);
        // Clear the data segment registers so they don't hold stale values.
        DS::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        ES::set_reg(SegmentSelector::new(0, PrivilegeLevel::Ring0));
        load_tss(GDT.1.tss);
    }
}

/// Update `TSS.rsp0` (the kernel stack loaded on ring-3 → ring-0 transitions).
///
/// Must be called whenever the current process changes, before the next
/// hardware interrupt can arrive from ring 3.
pub fn set_kernel_stack(stack_top: VirtAddr) {
    // Safety: TSS is at a fixed static address; single-CPU, no concurrent
    // writers; we only touch the RSP0 field.
    unsafe {
        let tss = &*TSS as *const TaskStateSegment as *mut TaskStateSegment;
        (*tss).privilege_stack_table[0] = stack_top;
    }
}

/// The raw selector value for the kernel code segment.
pub fn kernel_code_selector() -> SegmentSelector { GDT.1.kernel_code }

/// The raw selector value for the kernel data segment.
pub fn kernel_data_selector() -> SegmentSelector { GDT.1.kernel_data }

/// The raw selector value for the user code segment (DPL already = 3).
pub fn user_code_selector() -> SegmentSelector { GDT.1.user_code }

/// The raw selector value for the user data segment (DPL already = 3).
pub fn user_data_selector() -> SegmentSelector { GDT.1.user_data }
