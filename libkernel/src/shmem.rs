//! Shared memory objects for MAP_SHARED anonymous mappings.

use alloc::vec::Vec;
use x86_64::PhysAddr;

/// A shared memory object backed by physical frames.
///
/// Created via `shmem_create` syscall.  The fd can be passed to other
/// processes (via inheritance or IPC), and both sides `mmap(MAP_SHARED, fd)`
/// to map the same physical frames into their address spaces.
///
/// Lifetime is managed via `Arc<SharedMemInner>`.  The object owns one
/// reference to each backing frame (tracked in the global refcount table).
/// When the last `Arc` is dropped, `Drop` releases those references.
pub struct SharedMemInner {
    /// Physical addresses of backing frames (one per page).
    frames: Vec<PhysAddr>,
    /// Logical size in bytes (may be smaller than frames.len() * PAGE_SIZE).
    size: usize,
    /// Whether this object owns its frames and should release them on drop.
    ///
    /// `true` for objects created via `new()` (normal shmem_create path).
    /// `false` for objects created via `from_existing()` (wrapping IoRing
    /// frames — the IoRing is the true owner).
    owned: bool,
}

impl SharedMemInner {
    /// Allocate a new shared memory object with eagerly-allocated frames.
    ///
    /// Returns `None` if frame allocation fails.
    pub fn new(size: usize) -> Option<Self> {
        if size == 0 {
            return None;
        }
        let page_size = crate::consts::PAGE_SIZE as usize;
        let num_pages = (size + page_size - 1) / page_size;
        let mut frames = Vec::with_capacity(num_pages);

        crate::memory::with_memory(|mem| {
            for _ in 0..num_pages {
                let phys = mem.alloc_dma_pages(1)?;
                // Zero the page.
                let dst = mem.phys_mem_offset() + phys.as_u64();
                unsafe { crate::consts::clear_page(dst.as_mut_ptr::<u8>()); }
                frames.push(phys);
            }
            Some(())
        })?;

        Some(SharedMemInner { frames, size, owned: true })
    }

    /// Wrap existing physical frames as a shared memory object.
    ///
    /// The frames are NOT owned by this object — they will not be released
    /// on drop.  Used by `io_setup_rings` to expose IoRing pages as shmem
    /// fds that can be mmap'd by userspace.
    pub fn from_existing(frames: Vec<PhysAddr>, size: usize) -> Self {
        SharedMemInner { frames, size, owned: false }
    }

    /// Physical frame addresses backing this object.
    pub fn frames(&self) -> &[PhysAddr] {
        &self.frames
    }

    /// Logical size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }
}

impl Drop for SharedMemInner {
    fn drop(&mut self) {
        if self.owned {
            crate::memory::with_memory(|mem| {
                for &phys in &self.frames {
                    mem.release_shared_frame(phys);
                }
            });
        }
    }
}
