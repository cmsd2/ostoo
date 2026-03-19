//! Kernel-wide constants.

pub const PAGE_SIZE: u64 = 0x1000;
pub const PAGE_MASK: u64 = 0xFFF;
pub const KERNEL_STACK_SIZE: usize = 64 * 1024;

/// Zero a single page at the given virtual address.
///
/// # Safety
/// `addr` must point to a valid, mapped, writable page of at least PAGE_SIZE bytes.
pub unsafe fn clear_page(addr: *mut u8) {
    core::ptr::write_bytes(addr, 0, PAGE_SIZE as usize);
}
