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

