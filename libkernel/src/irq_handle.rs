//! IRQ fd infrastructure — allows userspace to receive hardware interrupts
//! through file descriptors and completion ports.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::completion_port::{Completion, CompletionPort, OP_IRQ_WAIT};
use crate::irq_mutex::IrqMutex;

// ---------------------------------------------------------------------------
// Per-slot IRQ counters (lock-free, safe from ISR context)

struct IrqCounters {
    total: AtomicU64,
    delivered: AtomicU64,
    buffered: AtomicU64,
    spurious: AtomicU64,
    wrong_source: AtomicU64,
}

impl IrqCounters {
    const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            buffered: AtomicU64::new(0),
            spurious: AtomicU64::new(0),
            wrong_source: AtomicU64::new(0),
        }
    }
}

const fn new_counters_array() -> [IrqCounters; 16] {
    const C: IrqCounters = IrqCounters::new();
    [C, C, C, C, C, C, C, C, C, C, C, C, C, C, C, C]
}

static IRQ_COUNTERS: [IrqCounters; 16] = new_counters_array();

/// Print IRQ statistics for all active slots to serial.
pub fn print_irq_stats() {
    crate::serial_println!("{}", format_irq_stats());
}

/// Format IRQ statistics as a string (for /proc/irq_stats).
pub fn format_irq_stats() -> alloc::string::String {
    use core::fmt::Write;
    let mut s = alloc::string::String::new();

    let slots = IRQ_SLOTS.lock();
    writeln!(s, "{:<4} {:>5} {:>8} {:>8} {:>8} {:>8} {:>10}",
        "SLOT", "GSI", "TOTAL", "DELIVER", "BUFFER", "SPURIOUS", "WRONG_SRC").unwrap();
    for slot in 0..16 {
        let c = &IRQ_COUNTERS[slot];
        let total = c.total.load(Ordering::Relaxed);
        if total == 0 {
            continue;
        }
        let gsi = match &slots[slot] {
            Some(arc) => arc.lock().gsi,
            None => 0,
        };
        let delivered = c.delivered.load(Ordering::Relaxed);
        let buffered = c.buffered.load(Ordering::Relaxed);
        let spurious = c.spurious.load(Ordering::Relaxed);
        let wrong_src = c.wrong_source.load(Ordering::Relaxed);
        writeln!(s, "{:<4} {:>5} {:>8} {:>8} {:>8} {:>8} {:>10}",
            slot, gsi, total, delivered, buffered, spurious, wrong_src).unwrap();
    }
    s
}

// ---------------------------------------------------------------------------
// PS/2 mouse packet decoder — assembles 3-byte packets in the ISR

struct MouseDecoder {
    buf: [u8; 3],
    idx: usize,
}

impl MouseDecoder {
    const fn new() -> Self {
        MouseDecoder { buf: [0; 3], idx: 0 }
    }

    /// Feed a raw byte.  Returns a packed i64 when a complete 3-byte packet
    /// is assembled, or `None` for partial / invalid data.
    fn feed(&mut self, byte: u8) -> Option<i64> {
        if self.idx == 0 {
            // Byte 0: bit 3 must always be 1 (sync bit).
            if byte & 0x08 == 0 {
                return None;
            }
        }

        self.buf[self.idx] = byte;
        self.idx += 1;

        if self.idx < 3 {
            return None;
        }

        // Complete 3-byte packet.
        self.idx = 0;

        let state = self.buf[0] as i32;
        let buttons = state as u8 & 0x07;

        // Check overflow bits — discard if set.
        if state & 0xC0 != 0 {
            return None;
        }

        // Signed deltas per OSDev wiki formula.
        let dx = (self.buf[1] as i32 - ((state << 4) & 0x100)) as i16;
        let dy = -(self.buf[2] as i32 - ((state << 3) & 0x100)) as i16; // flip Y

        Some(pack_mouse_event(dx, dy, buttons))
    }
}

/// Pack a decoded mouse event into an i64 for delivery via OP_IRQ_WAIT.
///
/// Layout: bits 0-15 = dx (i16), bits 16-31 = dy (i16), bits 32-39 = buttons (u8).
pub fn pack_mouse_event(dx: i16, dy: i16, buttons: u8) -> i64 {
    (dx as u16 as u64 | ((dy as u16 as u64) << 16) | ((buttons as u64) << 32)) as i64
}

// ---------------------------------------------------------------------------
// IrqInner — per-IRQ-fd kernel state

const SCANCODE_BUF_SIZE: usize = 64;
const MOUSE_EVENT_BUF_SIZE: usize = 16;

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
    /// Mouse (GSI 12) decoder and decoded-event ring buffer.
    mouse_decoder: MouseDecoder,
    mouse_event_buf: [i64; MOUSE_EVENT_BUF_SIZE],
    mouse_event_head: usize,
    mouse_event_tail: usize,
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
            mouse_decoder: MouseDecoder::new(),
            mouse_event_buf: [0; MOUSE_EVENT_BUF_SIZE],
            mouse_event_head: 0,
            mouse_event_tail: 0,
        }
    }

    fn scancode_push(&mut self, code: u8) {
        let next = (self.scancode_tail + 1) % SCANCODE_BUF_SIZE;
        if next != self.scancode_head {
            self.scancode_buf[self.scancode_tail] = code;
            self.scancode_tail = next;
        }
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

    fn mouse_event_push(&mut self, event: i64) {
        let next = (self.mouse_event_tail + 1) % MOUSE_EVENT_BUF_SIZE;
        if next != self.mouse_event_head {
            self.mouse_event_buf[self.mouse_event_tail] = event;
            self.mouse_event_tail = next;
        }
    }

    fn mouse_event_pop(&mut self) -> Option<i64> {
        if self.mouse_event_head == self.mouse_event_tail {
            None
        } else {
            let event = self.mouse_event_buf[self.mouse_event_head];
            self.mouse_event_head = (self.mouse_event_head + 1) % MOUSE_EVENT_BUF_SIZE;
            Some(event)
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
    let counters = &IRQ_COUNTERS[slot];
    counters.total.fetch_add(1, Ordering::Relaxed);

    if inner.gsi == 1 || inner.gsi == 12 {
        // Drain all available bytes from the i8042 output buffer in one ISR.
        // The controller can queue multiple bytes; reading them all in one
        // interrupt avoids losing data to the scancode / mouse event buffers.
        unsafe {
            let mut status_port = x86_64::instructions::port::Port::<u8>::new(0x64);
            let mut port_60 = x86_64::instructions::port::Port::<u8>::new(0x60);

            for _ in 0..16 {
                let status = status_port.read();
                if status & 0x01 == 0 {
                    break; // Output buffer empty.
                }
                let byte = port_60.read();
                let from_aux = status & 0x20 != 0;

                // Only deliver if source matches this IRQ's device.
                if (inner.gsi == 1 && from_aux) || (inner.gsi == 12 && !from_aux) {
                    counters.wrong_source.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                if inner.gsi == 12 {
                    // Mouse: decode 3-byte PS/2 packets in-kernel, deliver
                    // packed (dx, dy, buttons) events instead of raw bytes.
                    if let Some(event) = inner.mouse_decoder.feed(byte) {
                        if let Some((port, user_data)) = inner.pending.take() {
                            counters.delivered.fetch_add(1, Ordering::Relaxed);
                            port.lock().post(Completion {
                                user_data,
                                result: event,
                                flags: 0,
                                opcode: OP_IRQ_WAIT,
                                read_buf: None,
                                read_dest: 0,
                                transfer_fds: None,
                            });
                        } else {
                            counters.buffered.fetch_add(1, Ordering::Relaxed);
                            inner.mouse_event_push(event);
                        }
                    }
                } else {
                    // Keyboard: deliver raw scancodes.
                    if let Some((port, user_data)) = inner.pending.take() {
                        counters.delivered.fetch_add(1, Ordering::Relaxed);
                        port.lock().post(Completion {
                            user_data,
                            result: byte as i64,
                            flags: 0,
                            opcode: OP_IRQ_WAIT,
                            read_buf: None,
                            read_dest: 0,
                            transfer_fds: None,
                        });
                    } else {
                        counters.buffered.fetch_add(1, Ordering::Relaxed);
                        inner.scancode_push(byte);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Arm and close — called from osl (io_submit and close_fd paths)

/// Arm an IRQ fd for OP_IRQ_WAIT: register the port to post to on interrupt,
/// then unmask the GSI.  If there are buffered events/scancodes, post
/// completions for ALL of them so userspace can drain them in one batch.
pub fn arm_irq(inner: &Arc<IrqMutex<IrqInner>>, port: Arc<IrqMutex<CompletionPort>>, user_data: u64) {
    let mut guard = inner.lock();

    // Drain entire buffer into completions.
    let mut posted = false;
    if guard.gsi == 12 {
        // Mouse: drain decoded events.
        while let Some(event) = guard.mouse_event_pop() {
            port.lock().post(Completion {
                user_data,
                result: event,
                flags: 0,
                opcode: OP_IRQ_WAIT,
                read_buf: None,
                read_dest: 0,
                transfer_fds: None,
            });
            posted = true;
        }
    } else {
        // Keyboard: drain raw scancodes.
        while let Some(code) = guard.scancode_pop() {
            port.lock().post(Completion {
                user_data,
                result: code as i64,
                flags: 0,
                opcode: OP_IRQ_WAIT,
                read_buf: None,
                read_dest: 0,
                transfer_fds: None,
            });
            posted = true;
        }
    }

    if posted {
        crate::apic::unmask_gsi(guard.gsi);
        return;
    }

    // Nothing buffered — wait for next IRQ.
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
