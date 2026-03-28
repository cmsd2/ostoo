use x86_64::PhysAddr;
use x86_64::structures::paging::{
    PhysFrame,
    Size4KiB,
    FrameAllocator,
};

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};

/// A FrameAllocator that returns usable frames from the bootloader's memory map.
///
/// Freed frames are recycled via an intrusive singly-linked free list stored
/// in the first 8 bytes of each freed page (physical memory identity map).
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
    /// Physical address of the first free page (0 = empty list).
    free_head: u64,
    /// Cached virtual base for accessing physical memory (set once).
    phys_mem_offset: u64,
}

impl BootInfoFrameAllocator {
    /// Create a FrameAllocator from the passed memory map.
    ///
    /// This function is unsafe because the caller must guarantee that the passed
    /// memory map is valid. The main requirement is that all frames that are marked
    /// as `USABLE` in it are really unused.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
            free_head: 0,
            phys_mem_offset: 0,
        }
    }

    /// Number of frames that have been handed out by `allocate_frame`.
    pub fn frames_allocated(&self) -> usize {
        self.next
    }

    /// Replace the memory_map reference (e.g. after copying it to the heap).
    pub fn set_memory_map(&mut self, memory_map: &'static MemoryMap) {
        self.memory_map = memory_map;
    }

    /// Total number of usable (free-at-boot) frames in the memory map.
    pub fn total_usable_frames(&self) -> usize {
        self.usable_frames().count()
    }

    /// Set the physical memory offset for free-list access. Called once from
    /// `init_services` after the high-half identity map is established.
    pub fn set_phys_mem_offset(&mut self, offset: u64) {
        self.phys_mem_offset = offset;
    }

    /// Return a frame to the free list for reuse.
    ///
    /// # Safety contract
    /// The caller must ensure that `frame` is no longer mapped in any page
    /// table and that deallocating it will not cause a double-free.
    pub fn deallocate_frame(&mut self, frame: PhysFrame) {
        assert!(self.phys_mem_offset != 0, "deallocate_frame: phys_mem_offset not set");
        let phys_addr = frame.start_address().as_u64();
        let virt = (self.phys_mem_offset + phys_addr) as *mut u64;
        // Write the current head pointer into the first 8 bytes of the freed page.
        unsafe { virt.write(self.free_head); }
        self.free_head = phys_addr;
    }

    /// Number of frames currently on the free list.
    pub fn free_list_len(&self) -> usize {
        let mut count = 0;
        let mut addr = self.free_head;
        while addr != 0 {
            count += 1;
            let virt = (self.phys_mem_offset + addr) as *const u64;
            addr = unsafe { virt.read() };
        }
        count
    }

    /// Allocate a frame from the boot-time sequential iterator only,
    /// bypassing the free list. Use for contiguous DMA allocations.
    pub fn allocate_frame_sequential(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }

    /// Returns an iterator over the usable frames specified in the memory map.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable_regions = regions
            .filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable_regions
            .map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));
        frame_addresses
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        // Try the free list first (recycled frames).
        if self.free_head != 0 {
            let phys_addr = self.free_head;
            let virt = (self.phys_mem_offset + phys_addr) as *const u64;
            self.free_head = unsafe { virt.read() };
            return Some(PhysFrame::containing_address(PhysAddr::new(phys_addr)));
        }
        // Fall through to the boot-time sequential allocator.
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}
