use alloc::string::String;
use core::fmt::Write;

pub(super) fn generate() -> String {
    use bootloader::bootinfo::MemoryRegionType;

    let mut s = String::new();
    let _ = writeln!(s, "Physical memory map:");
    let mut total_usable: u64 = 0;

    libkernel::memory::with_memory(|mem| {
        for r in mem.iter_memory_regions() {
            let start = r.range.start_addr();
            let end   = r.range.end_addr();
            let kib   = (end - start + 1023) / 1024;
            if r.region_type == MemoryRegionType::Usable {
                total_usable += end - start;
            }
            let _ = writeln!(s, "  {:#011x}-{:#011x}  {:22?}  {} KiB",
                start, end.saturating_sub(1), r.region_type, kib);
        }
    });

    let _ = writeln!(s, "  Usable total: {} KiB ({} MiB)",
        total_usable / 1024, total_usable / 1024 / 1024);

    s
}
