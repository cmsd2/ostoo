//! Brk-based bump allocator for `#[global_allocator]`.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};
use crate::syscall;

pub struct BrkAllocator {
    /// Current top of the heap (next allocation address).
    top: AtomicU64,
}

impl BrkAllocator {
    pub const fn new() -> Self {
        Self { top: AtomicU64::new(0) }
    }

    fn init_if_needed(&self) -> u64 {
        let mut top = self.top.load(Ordering::Relaxed);
        if top == 0 {
            // brk(0) returns the current break.
            let base = syscall::brk(0) as u64;
            // Try to set it; if another path already initialized, use theirs.
            match self.top.compare_exchange(0, base, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => top = base,
                Err(existing) => top = existing,
            }
        }
        top
    }
}

unsafe impl GlobalAlloc for BrkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let top = self.init_if_needed();
        let align = layout.align() as u64;
        let aligned = (top + align - 1) & !(align - 1);
        let new_top = aligned + layout.size() as u64;

        // Grow the heap via brk.
        let result = syscall::brk(new_top);
        if (result as u64) < new_top {
            // brk failed — out of memory.
            return core::ptr::null_mut();
        }

        self.top.store(new_top, Ordering::Relaxed);
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator — no deallocation.
    }
}
