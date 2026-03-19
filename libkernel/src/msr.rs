//! x86-64 Model-Specific Register (MSR) addresses.

pub const IA32_EFER: u32 = 0xC000_0080;
pub const IA32_STAR: u32 = 0xC000_0081;
pub const IA32_LSTAR: u32 = 0xC000_0082;
pub const IA32_FMASK: u32 = 0xC000_0084;
pub const IA32_FS_BASE: u32 = 0xC000_0100;
pub const IA32_GS_BASE: u32 = 0xC000_0101;
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// Read the IA32_FS_BASE MSR (TLS base pointer).
///
/// Safe because FS_BASE only affects FS-relative addressing for the current
/// core — it cannot corrupt memory, disable protections, or cause UB.
pub fn read_fs_base() -> u64 {
    unsafe { x86_64::registers::model_specific::Msr::new(IA32_FS_BASE).read() }
}

/// Write the IA32_FS_BASE MSR (TLS base pointer).
///
/// # Safety
/// The caller must ensure `val` is a valid base address for FS-relative
/// accesses on this core (e.g. a TLS block allocated for the current thread).
pub unsafe fn write_fs_base(val: u64) {
    x86_64::registers::model_specific::Msr::new(IA32_FS_BASE).write(val);
}
