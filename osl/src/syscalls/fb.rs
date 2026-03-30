//! Framebuffer access syscall (515).

use alloc::sync::Arc;
use alloc::vec::Vec;

use libkernel::consts::PAGE_SIZE;
use libkernel::file::FdObject;
use libkernel::framebuffer;
use libkernel::shmem::SharedMemInner;
use x86_64::PhysAddr;

use crate::errno;
use crate::fd_helpers;

/// `framebuffer_open(flags) → fd or -errno`
///
/// Creates a shared-memory fd wrapping the BGA linear framebuffer's physical
/// frames. The caller can `mmap(MAP_SHARED, fd)` to get a user-accessible
/// pointer to the LFB.
///
/// The frames are non-owning (MMIO frames are never freed).
pub(crate) fn sys_framebuffer_open(flags: u32) -> i64 {
    if flags != 0 {
        return -errno::EINVAL;
    }

    let (lfb_phys, lfb_size) = match framebuffer::get_lfb_phys() {
        Some(v) => v,
        None => return -errno::ENODEV,
    };

    let page_size = PAGE_SIZE as u64;
    let num_pages = (lfb_size + page_size - 1) / page_size;

    let mut frames = Vec::with_capacity(num_pages as usize);
    for i in 0..num_pages {
        frames.push(PhysAddr::new(lfb_phys + i * page_size));
    }

    let inner = SharedMemInner::from_existing(frames, lfb_size as usize);
    let obj = FdObject::SharedMem(Arc::new(inner));

    match fd_helpers::alloc_fd(obj) {
        Ok(fd) => {
            // Suppress kernel display output — the calling process now owns the LFB.
            let pid = libkernel::process::current_pid();
            libkernel::vga_buffer::suppress_display(pid);
            fd as i64
        }
        Err(e) => e,
    }
}
