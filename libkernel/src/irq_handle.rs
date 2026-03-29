//! IRQ fd infrastructure — allows userspace to receive hardware interrupts
//! through file descriptors and completion ports.

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
    pub fn new(gsi: u32, vector: u8, slot: usize, saved_entry: u64) -> Self {
        Self {
            gsi, vector, slot,
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
    crate::apic::mask_gsi(inner.gsi);

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
            transfer_fds: None,
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
            transfer_fds: None,
        });
        return;
    }

    guard.pending = Some((port, user_data));
    crate::apic::unmask_gsi(guard.gsi);
}

/// Close an IRQ fd: restore the original IO APIC redirection entry, free the
/// dynamic vector, and remove from the slot table.
pub fn close_irq(inner: &IrqInner) {
    crate::apic::write_gsi_entry(inner.gsi, inner.saved_entry);
    crate::interrupts::free_vector(inner.vector);
    take_slot(inner.slot);
}
