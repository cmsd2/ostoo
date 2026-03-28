//! IRQ fd creation — bridges libkernel IRQ infrastructure with the apic crate.

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

    // Find the IO APIC that handles this GSI and get its base address + offset.
    let (io_apic_base, gsi_offset) = match find_io_apic_for_gsi(gsi) {
        Some(v) => v,
        None => {
            libkernel::interrupts::free_vector(vector);
            return -errno::EINVAL;
        }
    };

    // Save the original IO APIC entry so we can restore it on close.
    let saved_entry = apic::read_gsi_entry(gsi).unwrap_or(0);

    // Program the IO APIC redirection entry (edge-triggered, active-high, masked).
    if !apic::route_gsi(gsi, vector) {
        libkernel::interrupts::free_vector(vector);
        return -errno::EINVAL;
    }

    let inner = Arc::new(IrqMutex::new(IrqInner::new(
        gsi, vector, slot, io_apic_base, gsi_offset, saved_entry,
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

/// Find the IO APIC that handles a GSI. Returns (base_virt_addr, gsi_offset).
fn find_io_apic_for_gsi(gsi: u32) -> Option<(u64, u32)> {
    let io_apics = apic::IO_APICS.lock();
    for apic in io_apics.iter() {
        let max = apic.max_redirect_entries();
        let end_gsi = apic.interrupt_base + max + 1;
        if gsi >= apic.interrupt_base && gsi < end_gsi {
            let offset = gsi - apic.interrupt_base;
            return Some((apic.base_addr().as_u64(), offset));
        }
    }
    None
}
