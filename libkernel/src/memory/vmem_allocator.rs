use x86_64::VirtAddr;
use x86_64::structures::paging::{Page, PageSize};

pub trait VmemAllocator<S> where S: PageSize {
    fn alloc(&mut self, num_pages: u64) -> (Page<S>, Page<S>);
    fn dealloc(&mut self, start: Page<S>, end: Page<S>);
}

pub struct DumbVmemAllocator<S> where S: PageSize {
    base: Page<S>,
    next: Page<S>,
    end: Page<S>,
}

impl <S> DumbVmemAllocator<S> where S: PageSize {
    pub fn new(base_addr: VirtAddr, size_bytes: u64) -> Self {
        let base = Page::from_start_address(base_addr).expect("page");
        let end = Page::from_start_address((base_addr + size_bytes).align_up(S::SIZE)).expect("page");

        DumbVmemAllocator {
            base: base,
            next: base,
            end: end,
        }
    }

    pub fn available(&self) -> u64 {
        self.end - self.next
    }

    pub fn used(&self) -> u64 {
        self.next - self.base
    }
}

impl <S> VmemAllocator<S> for DumbVmemAllocator<S> where S: PageSize {
    fn alloc(&mut self, num_pages: u64) -> (Page<S>, Page<S>) {
        assert!(num_pages > 0);
        assert!(num_pages <= self.available());

        let start = self.next;
        self.next += num_pages;
        let end = self.next - 1;

        (start, end)
    }

    fn dealloc(&mut self, _start: Page<S>, _end: Page<S>) {
        // do nothing
    }
}

#[cfg(test)]
mod test {
    use crate::{serial_print, serial_println};
    use x86_64::VirtAddr;
    use x86_64::structures::paging::Size4KiB;
    use super::{DumbVmemAllocator, VmemAllocator};

    const BASE: u64 = 0x_4444_4444_0000;
    const PAGE: u64 = 4096;

    #[test_case]
    fn test_dumb_vmem_initial_state() {
        serial_print!("test_dumb_vmem_initial_state... ");
        let alloc: DumbVmemAllocator<Size4KiB> =
            DumbVmemAllocator::new(VirtAddr::new(BASE), 16 * PAGE);
        assert_eq!(alloc.available(), 16);
        assert_eq!(alloc.used(), 0);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_dumb_vmem_alloc_advances_cursor() {
        serial_print!("test_dumb_vmem_alloc_advances_cursor... ");
        let base = VirtAddr::new(BASE);
        let mut alloc: DumbVmemAllocator<Size4KiB> =
            DumbVmemAllocator::new(base, 16 * PAGE);

        let (s1, e1) = alloc.alloc(4);
        assert_eq!(s1.start_address(), base);
        assert_eq!(e1.start_address(), base + 3 * PAGE);
        assert_eq!(alloc.used(), 4);
        assert_eq!(alloc.available(), 12);

        let (s2, e2) = alloc.alloc(1);
        assert_eq!(s2.start_address(), base + 4 * PAGE);
        assert_eq!(e2.start_address(), base + 4 * PAGE);
        assert_eq!(alloc.used(), 5);
        assert_eq!(alloc.available(), 11);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_dumb_vmem_alloc_all_pages() {
        serial_print!("test_dumb_vmem_alloc_all_pages... ");
        let base = VirtAddr::new(BASE);
        let mut alloc: DumbVmemAllocator<Size4KiB> =
            DumbVmemAllocator::new(base, 8 * PAGE);

        let (s, e) = alloc.alloc(8);
        assert_eq!(s.start_address(), base);
        assert_eq!(e.start_address(), base + 7 * PAGE);
        assert_eq!(alloc.used(), 8);
        assert_eq!(alloc.available(), 0);
        serial_println!("[ok]");
    }
}

