//! Fixed-size arena allocator for 64 KiB kernel thread stacks.
//!
//! All kernel stacks (process kernel stacks, kernel thread stacks, boot thread)
//! are allocated from a dedicated virtual region right after the heap, avoiding
//! heap fragmentation from large 64 KiB allocations.

use crate::consts::{KERNEL_STACK_SIZE, PAGE_SIZE, STACK_ARENA_BASE, STACK_ARENA_CAPACITY};
use crate::spin_mutex::SpinMutex as Mutex;

struct ArenaInner {
    /// Bitmap of free slots (bit=1 → free).
    free_bitmap: u32,
    initialized: bool,
}

static ARENA: Mutex<ArenaInner> = Mutex::new(ArenaInner {
    free_bitmap: 0,
    initialized: false,
});

/// A handle to a 64 KiB stack slot in the arena.
///
/// Dropping the slot returns it to the arena for reuse.
/// Use `core::mem::forget` for permanent stacks (boot thread).
pub struct StackSlot {
    index: usize,
    base: *mut u8,
    size: usize,
}

// StackSlot contains a raw pointer but is only used from kernel context
// where all stacks are mapped in the shared kernel half of the address space.
unsafe impl Send for StackSlot {}

impl StackSlot {
    /// 16-byte-aligned top of the stack (highest usable address).
    pub fn top(&self) -> u64 {
        (self.base as u64 + self.size as u64) & !0xF
    }
}

impl Drop for StackSlot {
    fn drop(&mut self) {
        let mut arena = ARENA.lock();
        arena.free_bitmap |= 1 << self.index;
    }
}

/// Initialize the arena: allocate physical frames and map them.
///
/// Must be called after `memory::init_services()` and before any stack
/// allocation (i.e. before `scheduler::migrate_to_heap_stack`).
pub fn init() {
    let total_pages = (STACK_ARENA_CAPACITY * KERNEL_STACK_SIZE) / PAGE_SIZE as usize;
    crate::memory::with_memory(|mem| {
        mem.map_kernel_pages(STACK_ARENA_BASE, total_pages)
            .expect("stack arena: failed to map pages");
    });

    let mut arena = ARENA.lock();
    arena.free_bitmap = (1u32 << STACK_ARENA_CAPACITY) - 1;
    arena.initialized = true;
}

/// Allocate a zeroed 64 KiB stack slot. Returns `None` if the arena is full.
pub fn alloc() -> Option<StackSlot> {
    let mut arena = ARENA.lock();
    assert!(arena.initialized, "stack_arena::alloc before init");
    if arena.free_bitmap == 0 {
        return None;
    }
    let index = arena.free_bitmap.trailing_zeros() as usize;
    arena.free_bitmap &= !(1 << index);
    drop(arena);

    let base = (STACK_ARENA_BASE + (index as u64) * KERNEL_STACK_SIZE as u64) as *mut u8;
    unsafe { core::ptr::write_bytes(base, 0, KERNEL_STACK_SIZE); }

    Some(StackSlot { index, base, size: KERNEL_STACK_SIZE })
}
