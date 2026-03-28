//! IRQ fd creation — bridges libkernel IRQ infrastructure with the apic module.

use alloc::sync::Arc;

use libkernel::file::FdObject;
use libkernel::irq_handle::{self, IrqInner};
use libkernel::irq_mutex::IrqMutex;
use libkernel::process;

use crate::errno;

/// Syscall handler for `irq_create(gsi)` — creates an IRQ fd for the given GSI.
pub fn sys_irq_create(gsi: u32) -> i64 {
    // Allocate a dynamic vector and register the shared ISR handler.
    let vector = match libkernel::interrupts::register_handler(irq_handle::irq_fd_dispatch) {
        Some(v) => v,
        None => return -errno::ENOMEM,
    };

    let slot = (vector - libkernel::interrupts::DYNAMIC_BASE) as usize;

    // Save the original IO APIC entry so we can restore it on close.
    let saved_entry = libkernel::apic::read_gsi_entry(gsi).unwrap_or(0);

    // Program the IO APIC redirection entry (edge-triggered, active-high, masked).
    if !libkernel::apic::route_gsi(gsi, vector) {
        libkernel::interrupts::free_vector(vector);
        return -errno::EINVAL;
    }

    let inner = Arc::new(IrqMutex::new(IrqInner::new(
        gsi, vector, slot, saved_entry,
    )));

    irq_handle::store_slot(slot, inner.clone());

    // Allocate an fd for this IRQ object.
    let pid = process::current_pid();
    match process::with_process(pid, |p| p.alloc_fd(FdObject::Irq(inner))) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(e)) => {
            // Clean up on fd allocation failure.
            irq_handle::take_slot(slot);
            libkernel::interrupts::free_vector(vector);
            crate::errno::file_errno(e)
        }
        None => {
            irq_handle::take_slot(slot);
            libkernel::interrupts::free_vector(vector);
            -errno::EBADF
        }
    }
}
