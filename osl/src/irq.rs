//! IRQ fd creation — bridges libkernel IRQ infrastructure with the apic module.

use alloc::sync::Arc;

use libkernel::file::FdObject;
use libkernel::irq_handle::{self, IrqInner};
use libkernel::irq_mutex::IrqMutex;

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

    // For mouse (GSI 12): initialize the PS/2 auxiliary port so IRQ 12 fires.
    if gsi == 12 {
        libkernel::ps2::aux_init();
    }

    let inner = Arc::new(IrqMutex::new(IrqInner::new(
        gsi, vector, slot, saved_entry,
    )));

    irq_handle::store_slot(slot, inner.clone());

    // Allocate an fd for this IRQ object.
    match crate::fd_helpers::alloc_fd(FdObject::Irq(inner)) {
        Ok(fd) => fd as i64,
        Err(e) => {
            // Clean up on fd allocation failure.
            irq_handle::take_slot(slot);
            libkernel::interrupts::free_vector(vector);
            e
        }
    }
}
