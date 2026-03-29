use alloc::string::String;
use core::fmt::Write;

/// Well-known virtual address where the Local APIC is mapped.
const APIC_VIRT_BASE: u64 = 0xFFFF_8001_0000_0000;

pub(super) fn generate() -> String {
    let mut s = String::new();

    // Heap
    let (heap_used, heap_free) = libkernel::allocator::heap_stats();
    let heap_total = heap_used + heap_free;
    let _ = writeln!(s, "Heap: {} used  {} free  ({} KiB total)",
        heap_used, heap_free, heap_total / 1024);

    // Frame allocator
    let (frames_alloc, frames_total, free_list) = libkernel::memory::with_memory(|m| m.frame_stats());
    let _ = writeln!(s, "Frames: {} allocated / {} usable ({} MiB usable), {} on free list",
        frames_alloc, frames_total, frames_total as u64 * 4 / 1024, free_list);

    // Known virtual regions
    let _ = writeln!(s, "Known virtual regions:");
    let _ = writeln!(s, "  {:#018x}  Heap ({} KiB)",
        libkernel::allocator::HEAP_START,
        libkernel::allocator::HEAP_SIZE / 1024);
    let _ = writeln!(s, "  {:#018x}  Local APIC registers", APIC_VIRT_BASE);
    let phys_off = libkernel::memory::with_memory(|m| m.phys_mem_offset().as_u64());
    let _ = writeln!(s, "  {:#018x}  Physical memory identity map", phys_off);

    s
}
