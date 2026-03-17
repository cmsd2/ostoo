use alloc::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

pub mod bump;

pub const HEAP_START: usize = 0xFFFF_8000_0000_0000;
pub const HEAP_SIZE: usize = 512 * 1024; // 512 KiB

/// Align the given address `addr` upwards to alignment `align`.
pub fn align_up(addr: usize, align: usize) -> usize {
    let remainder = addr % align;
    if remainder == 0 {
        addr // addr already aligned
    } else {
        addr - remainder + align
    }
}

pub fn align_up_pow_2(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

pub struct Locked<A> {
    inner: spin::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked {
            inner: spin::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, A> {
        self.inner.lock()
    }
}

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }

    unsafe {
        super::ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}

/// Returns `(used_bytes, free_bytes)` for the kernel heap.
pub fn heap_stats() -> (usize, usize) {
    let heap = crate::ALLOCATOR.lock();
    (heap.used(), heap.free())
}

pub struct Dummy;

unsafe impl GlobalAlloc for Dummy {
    unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
        null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        panic!("dealloc should be never called")
    }
}

#[cfg(test)]
mod test {
    use crate::{serial_print, serial_println};
    use super::{align_up, align_up_pow_2};

    #[test_case]
    fn test_align_up() {
        serial_print!("test_align_up... ");
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(0, 1), 0);
        assert_eq!(align_up(7, 1), 7);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_align_up_pow2() {
        serial_print!("test_align_up_pow2... ");
        assert_eq!(align_up_pow_2(0, 4), 0);
        assert_eq!(align_up_pow_2(1, 4), 4);
        assert_eq!(align_up_pow_2(4, 4), 4);
        assert_eq!(align_up_pow_2(5, 8), 8);
        assert_eq!(align_up_pow_2(8, 8), 8);
        assert_eq!(align_up_pow_2(9, 8), 16);
        assert_eq!(align_up_pow_2(0, 1), 0);
        assert_eq!(align_up_pow_2(7, 1), 7);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_align_up_matches_pow2() {
        serial_print!("test_align_up_matches_pow2... ");
        // Both implementations must agree on power-of-two alignments.
        for align in [1usize, 2, 4, 8, 16, 64, 4096] {
            for addr in [0usize, 1, 3, 4, 7, 8, 15, 16, 100, 1023, 1024, 4095, 4096, 8191] {
                assert_eq!(align_up(addr, align), align_up_pow_2(addr, align),
                    "mismatch at addr={addr} align={align}");
            }
        }
        serial_println!("[ok]");
    }
}