use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};

use virtio_drivers::{BufferDirection, Hal, PhysAddr};
use virtio_drivers::transport::pci::bus::{Cam, MmioCam, PciRoot};
use virtio_drivers::transport::pci::PciTransport;

use libkernel::memory;

pub mod blk;
pub mod exfat;
pub mod p9_proto;
pub mod p9;
pub use exfat::{BlkInbox, DirEntry, ExfatError, ExfatVol,
                open_exfat, list_dir, read_file};

// ---------------------------------------------------------------------------
// Physical-memory offset cache (mirrors the one in libkernel::memory)

fn phys_mem_offset() -> usize {
    libkernel::memory::phys_mem_offset() as usize
}

// ---------------------------------------------------------------------------
// KernelHal — implements virtio_drivers::Hal for this kernel

pub struct KernelHal;

unsafe impl Hal for KernelHal {
    /// Allocate physically-contiguous DMA pages and return (paddr, vaddr).
    fn dma_alloc(pages: usize, _dir: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let paddr = memory::with_memory(|mem| {
            mem.alloc_dma_pages(pages)
                .expect("dma_alloc: out of physical frames")
        });

        let vaddr = phys_mem_offset() + paddr.as_u64() as usize;
        // Zero the allocation.
        unsafe { core::ptr::write_bytes(vaddr as *mut u8, 0, pages * libkernel::consts::PAGE_SIZE as usize); }

        (paddr.as_u64(), NonNull::new(vaddr as *mut u8).unwrap())
    }

    /// Deallocate DMA pages.  Our frame allocator has no free; leak is OK for MVP.
    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        0
    }

    /// Convert a physical MMIO address to a virtual address, mapping the pages
    /// if they are not already present in the kernel page table.
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        let virt = memory::with_memory(|mem| {
            mem.map_mmio_region(
                x86_64::PhysAddr::new(paddr as u64),
                size,
            )
        });
        NonNull::new(virt.as_u64() as *mut u8).unwrap()
    }

    /// Share a buffer with the device.  x86 is cache-coherent.
    ///
    /// We must do a page-table walk because the buffer may live in the kernel
    /// heap (mapped at HEAP_START) rather than the linear physical-memory
    /// window (mapped at phys_mem_offset), so a plain subtraction is wrong.
    unsafe fn share(buffer: NonNull<[u8]>, _dir: BufferDirection) -> PhysAddr {
        let vaddr = x86_64::VirtAddr::new(buffer.as_ptr() as *const u8 as u64);
        memory::with_memory(|mem| {
            mem.translate_virt(vaddr)
                .expect("virtio share: buffer virtual address not mapped")
                .as_u64()
        })
    }

    /// Unshare — nothing to do on a cache-coherent architecture.
    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _dir: BufferDirection) {}
}

// ---------------------------------------------------------------------------
// ECAM virtual base — set once from kernel init, then read-only

static ECAM_VIRT_BASE: AtomicU64 = AtomicU64::new(0);

/// Store the virtual address at which the PCIe ECAM region (0xB0000000 phys
/// for QEMU Q35) has been mapped.  Must be called before any virtio probe.
pub fn set_ecam_base(virt_base: u64) {
    ECAM_VIRT_BASE.store(virt_base, Ordering::Relaxed);
}

/// Create a `PciRoot` pointing at the ECAM region.
///
/// # Safety
/// `ECAM_VIRT_BASE` must have been set by `set_ecam_base` and the region
/// must be fully mapped and valid.
unsafe fn create_pci_root() -> PciRoot<MmioCam<'static>> {
    let base = ECAM_VIRT_BASE.load(Ordering::Relaxed);
    // Safety: caller guarantees the mapping is valid and the ECAM region
    // lives for the lifetime of the kernel ('static).
    PciRoot::new(MmioCam::new(base as *mut u8, Cam::Ecam))
}

// ---------------------------------------------------------------------------
// Public helper: probe bus/device/function and create a PCI transport

/// Attempt to create a VirtIO PCI transport for the device at the given
/// bus/device/function coordinates.  Returns `None` if the transport cannot
/// be initialised (capability parsing failed, etc.).
pub fn create_pci_transport(bus: u8, device: u8, function: u8) -> Option<PciTransport> {
    use virtio_drivers::transport::pci::bus::DeviceFunction;
    let df = DeviceFunction { bus, device, function };
    let mut root = unsafe { create_pci_root() };
    PciTransport::new::<KernelHal, _>(&mut root, df).ok()
}

/// Legacy alias for `create_pci_transport`.
pub fn create_blk_transport(bus: u8, device: u8, function: u8) -> Option<PciTransport> {
    create_pci_transport(bus, device, function)
}

// ---------------------------------------------------------------------------
// IRQ registration (simplified: no MSI wiring for MVP)

/// Register a dynamic interrupt handler for the virtio-blk device.
/// Returns the assigned vector, or `None` if all dynamic slots are in use.
pub fn register_blk_irq(handler: fn()) -> Option<u8> {
    libkernel::interrupts::register_handler(handler)
}
