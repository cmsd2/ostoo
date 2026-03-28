//! IRQ fd infrastructure — allows userspace to receive hardware interrupts
//! through file descriptors and completion ports.
//!
//! The ISR handler and arm/close operations live here (in libkernel) using
//! raw IO APIC MMIO writes to avoid depending on the `apic` crate.
//! The `create_irq` function lives in `osl` which has the `apic` dependency.

use alloc::sync::Arc;

use crate::completion_port::{Completion, CompletionPort, OP_IRQ_WAIT};
use crate::irq_mutex::IrqMutex;

// ---------------------------------------------------------------------------
// IrqInner — per-IRQ-fd kernel state

const SCANCODE_BUF_SIZE: usize = 16;

pub struct IrqInner {
    pub gsi: u32,
    pub vector: u8,
    pub slot: usize,
    /// IO APIC MMIO virtual base address (for raw mask/unmask).
    pub io_apic_base: u64,
    /// Offset within the IO APIC (gsi - interrupt_base).
    pub gsi_offset: u32,
    /// When OP_IRQ_WAIT is active: (port, user_data) to post on interrupt.
    pub pending: Option<(Arc<IrqMutex<CompletionPort>>, u64)>,
    /// Original IO APIC redirection entry, saved before route_gsi reprograms it.
    /// Restored on close to give the interrupt back to its previous handler.
    pub saved_entry: u64,
    /// Keyboard (GSI 1) scancode ring buffer — holds scancodes read by the
    /// ISR when no OP_IRQ_WAIT is pending (e.g. break codes between rearms).
    scancode_buf: [u8; SCANCODE_BUF_SIZE],
    scancode_head: usize,
    scancode_tail: usize,
}

impl IrqInner {
    pub fn new(gsi: u32, vector: u8, slot: usize, io_apic_base: u64, gsi_offset: u32, saved_entry: u64) -> Self {
        Self {
            gsi, vector, slot, io_apic_base, gsi_offset,
            pending: None,
            saved_entry,
            scancode_buf: [0; SCANCODE_BUF_SIZE],
            scancode_head: 0,
            scancode_tail: 0,
        }
    }

    fn scancode_push(&mut self, code: u8) {
        let next = (self.scancode_tail + 1) % SCANCODE_BUF_SIZE;
        if next != self.scancode_head {
            self.scancode_buf[self.scancode_tail] = code;
            self.scancode_tail = next;
        }
        // else: full, drop oldest would be better but keep it simple
    }

    fn scancode_pop(&mut self) -> Option<u8> {
        if self.scancode_head == self.scancode_tail {
            None
        } else {
            let code = self.scancode_buf[self.scancode_head];
            self.scancode_head = (self.scancode_head + 1) % SCANCODE_BUF_SIZE;
            Some(code)
        }
    }
}

// ---------------------------------------------------------------------------
// IRQ slot table — 16 slots matching the dynamic vector range

static IRQ_SLOTS: IrqMutex<[Option<Arc<IrqMutex<IrqInner>>>; 16]> = {
    // const array init
    const NONE: Option<Arc<IrqMutex<IrqInner>>> = None;
    IrqMutex::new([NONE; 16])
};

/// Store an IrqInner in the slot table. Called from osl::irq during creation.
pub fn store_slot(slot: usize, inner: Arc<IrqMutex<IrqInner>>) {
    let mut slots = IRQ_SLOTS.lock();
    slots[slot] = Some(inner);
}

/// Remove an IrqInner from the slot table and return it.
pub fn take_slot(slot: usize) -> Option<Arc<IrqMutex<IrqInner>>> {
    let mut slots = IRQ_SLOTS.lock();
    slots[slot].take()
}

// ---------------------------------------------------------------------------
// Raw IO APIC mask/unmask (no lock needed, ISR-safe)

/// Mask an IO APIC redirection entry via raw MMIO writes.
///
/// # Safety
/// `io_apic_base` must be a valid mapped IO APIC MMIO address.
unsafe fn raw_mask_entry(io_apic_base: u64, gsi_offset: u32) {
    let sel = io_apic_base as *mut u32;
    let win = (io_apic_base + 0x10) as *mut u32;
    let reg_idx = 0x10 + gsi_offset * 2;
    core::ptr::write_volatile(sel, reg_idx);
    let low = core::ptr::read_volatile(win as *const u32);
    core::ptr::write_volatile(sel, reg_idx);
    core::ptr::write_volatile(win, low | (1 << 16));
}

/// Unmask an IO APIC redirection entry via raw MMIO writes.
///
/// # Safety
/// `io_apic_base` must be a valid mapped IO APIC MMIO address.
unsafe fn raw_unmask_entry(io_apic_base: u64, gsi_offset: u32) {
    let sel = io_apic_base as *mut u32;
    let win = (io_apic_base + 0x10) as *mut u32;
    let reg_idx = 0x10 + gsi_offset * 2;
    core::ptr::write_volatile(sel, reg_idx);
    let low = core::ptr::read_volatile(win as *const u32);
    core::ptr::write_volatile(sel, reg_idx);
    core::ptr::write_volatile(win, low & !(1 << 16));
}

/// Write a full 64-bit IO APIC redirection entry via raw MMIO writes.
///
/// # Safety
/// `io_apic_base` must be a valid mapped IO APIC MMIO address.
unsafe fn raw_write_entry(io_apic_base: u64, gsi_offset: u32, entry: u64) {
    let sel = io_apic_base as *mut u32;
    let win = (io_apic_base + 0x10) as *mut u32;
    let low_reg = 0x10 + gsi_offset * 2;
    let high_reg = low_reg + 1;
    core::ptr::write_volatile(sel, low_reg);
    core::ptr::write_volatile(win, entry as u32);
    core::ptr::write_volatile(sel, high_reg);
    core::ptr::write_volatile(win, (entry >> 32) as u32);
}

// ---------------------------------------------------------------------------
// ISR handler — registered with register_handler() as fn(usize)

/// Dynamic interrupt handler for all IRQ fd slots.
/// Called from the IDT trampoline with interrupts disabled.
pub fn irq_fd_dispatch(slot: usize) {
    let slots = IRQ_SLOTS.lock();
    let inner_arc = match &slots[slot] {
        Some(arc) => arc.clone(),
        None => return,
    };
    drop(slots);

    let mut inner = inner_arc.lock();

    // Mask the GSI immediately to prevent interrupt storms.
    unsafe { raw_mask_entry(inner.io_apic_base, inner.gsi_offset); }

    // For keyboard (GSI 1): read the scancode to deassert the IRQ.
    let scancode = if inner.gsi == 1 {
        unsafe {
            let mut port_60 = x86_64::instructions::port::Port::<u8>::new(0x60);
            Some(port_60.read())
        }
    } else {
        None
    };

    if let Some((port, user_data)) = inner.pending.take() {
        let result = scancode.map(|s| s as i64).unwrap_or(0);
        port.lock().post(Completion {
            user_data,
            result,
            flags: 0,
            opcode: OP_IRQ_WAIT,
            read_buf: None,
            read_dest: 0,
        });
    } else {
        // No pending wait — buffer the scancode so it isn't lost.
        if let Some(code) = scancode {
            inner.scancode_push(code);
        }
    }
}

// ---------------------------------------------------------------------------
// Arm and close — called from osl (io_submit and close_fd paths)

/// Arm an IRQ fd for OP_IRQ_WAIT: register the port to post to on interrupt,
/// then unmask the GSI.  If there are buffered scancodes (keyboard), post a
/// completion immediately from the buffer without unmasking.
pub fn arm_irq(inner: &Arc<IrqMutex<IrqInner>>, port: Arc<IrqMutex<CompletionPort>>, user_data: u64) {
    let mut guard = inner.lock();

    // If there's a buffered scancode, satisfy the request immediately.
    if let Some(code) = guard.scancode_pop() {
        port.lock().post(Completion {
            user_data,
            result: code as i64,
            flags: 0,
            opcode: OP_IRQ_WAIT,
            read_buf: None,
            read_dest: 0,
        });
        return;
    }

    guard.pending = Some((port, user_data));
    unsafe { raw_unmask_entry(guard.io_apic_base, guard.gsi_offset); }
}

/// Close an IRQ fd: restore the original IO APIC redirection entry, free the
/// dynamic vector, and remove from the slot table.
pub fn close_irq(inner: &IrqInner) {
    unsafe { raw_write_entry(inner.io_apic_base, inner.gsi_offset, inner.saved_entry); }
    crate::interrupts::free_vector(inner.vector);
    take_slot(inner.slot);
}
