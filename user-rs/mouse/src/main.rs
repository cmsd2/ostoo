#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

use alloc::vec::Vec;
use ostoo_rt::mouse_proto::*;
use ostoo_rt::ostoo::{self, CompletionPort, IpcSend, IrqFd, OsError};
use ostoo_rt::sys::{self, IoCompletion, IoSubmission, IpcMessage};
use ostoo_rt::{eprintln, println};

// ── Constants ────────────────────────────────────────────────────────

const FB_WIDTH: usize = 1024;
const FB_HEIGHT: usize = 768;

const MAX_CLIENTS: usize = 4;
const MAX_COMPLETIONS: usize = 16;

const TAG_IRQ: u64 = 0x1000;
const TAG_NEW_CLIENT: u64 = 0x2000;
const TAG_TIMEOUT: u64 = 0x3000;

const CLIENT_TIMEOUT_NS: u64 = 2_000_000_000;

// ── Client state ─────────────────────────────────────────────────────

struct Client {
    send: IpcSend,
}

// ── PS/2 mouse packet decoder ────────────────────────────────────────

struct MouseDecoder {
    buf: [u8; 3],
    idx: usize,
}

struct MouseEvent {
    dx: i32,
    dy: i32,
    buttons: u8,
}

impl MouseDecoder {
    fn new() -> Self {
        MouseDecoder {
            buf: [0; 3],
            idx: 0,
        }
    }

    fn feed(&mut self, byte: u8) -> Option<MouseEvent> {
        if self.idx == 0 {
            // Byte 0: bit 3 must always be 1 (sync bit).
            if byte & 0x08 == 0 {
                // Out of sync — skip this byte.
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
        let buttons = state as u8 & 0x07; // bits 0-2: left, right, middle

        // Check overflow bits — discard if set.
        if state & 0xC0 != 0 {
            return None;
        }

        // Signed deltas per OSDev wiki formula.
        let dx = self.buf[1] as i32 - ((state << 4) & 0x100);
        let dy = self.buf[2] as i32 - ((state << 3) & 0x100);

        Some(MouseEvent {
            dx,
            dy: -dy, // flip Y: PS/2 up = positive, screen down = positive
            buttons,
        })
    }
}

// ── Main ─────────────────────────────────────────────────────────────

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mouse: fatal error (errno {})", e.errno());
            1
        }
    }
}

fn run() -> Result<(), OsError> {
    println!("mouse: starting mouse driver");

    // 1. Claim mouse IRQ (GSI 12).
    let irq = IrqFd::new(12)?;
    println!("mouse: claimed IRQ 12 (fd={})", irq.fd());

    // 2. Create registration channel and completion port.
    let (reg_send, reg_recv) = ostoo::ipc_channel(4, 0)?;
    let port = CompletionPort::new()?;

    // 3. Register as "mouse" service.
    ostoo::service_register(SERVICE_NAME, reg_send.fd())?;
    println!("mouse: registered as 'mouse'");

    // 4. Arm initial operations.
    port.submit(&[IoSubmission::irq_wait(TAG_IRQ, irq.fd())])?;

    let mut reg_msg = IpcMessage::default();
    port.submit(&[IoSubmission::ipc_recv(
        TAG_NEW_CLIENT,
        reg_recv.fd(),
        &mut reg_msg,
    )])?;

    port.submit(&[IoSubmission::timeout(TAG_TIMEOUT, CLIENT_TIMEOUT_NS)])?;

    // 5. Event loop.
    let mut clients: Vec<Client> = Vec::new();
    let mut decoder = MouseDecoder::new();
    let mut completions = [IoCompletion::default(); MAX_COMPLETIONS];
    let mut timeout_armed = true;

    // Absolute cursor position.
    let mut cursor_x: i32 = (FB_WIDTH / 2) as i32;
    let mut cursor_y: i32 = (FB_HEIGHT / 2) as i32;

    let mut last_buttons: u8 = 0;

    loop {
        let n = port.wait(&mut completions, 1, 0)?;
        let mut have_update = false;

        for i in 0..n {
            let cqe = completions[i];
            let ud = cqe.user_data;

            if ud == TAG_IRQ {
                if cqe.result >= 0 {
                    let byte = cqe.result as u8;
                    if let Some(event) = decoder.feed(byte) {
                        // Update absolute position.
                        cursor_x = (cursor_x + event.dx).clamp(0, FB_WIDTH as i32 - 1);
                        cursor_y = (cursor_y + event.dy).clamp(0, FB_HEIGHT as i32 - 1);
                        last_buttons = event.buttons;
                        have_update = true;
                    }
                }
                // Re-arm IRQ wait.
                port.submit(&[IoSubmission::irq_wait(TAG_IRQ, irq.fd())])?;
            } else if ud == TAG_NEW_CLIENT {
                if cqe.result >= 0 && reg_msg.tag == MSG_MOUSE_CONNECT {
                    if clients.len() < MAX_CLIENTS {
                        let send_fd = reg_msg.fds[0];
                        if send_fd >= 0 {
                            clients.push(Client {
                                send: IpcSend::from_raw_fd(send_fd),
                            });
                            println!("mouse: client connected (total {})", clients.len());
                        }
                    }
                }
                reg_msg = IpcMessage::default();
                port.submit(&[IoSubmission::ipc_recv(
                    TAG_NEW_CLIENT,
                    reg_recv.fd(),
                    &mut reg_msg,
                )])?;

                if timeout_armed && !clients.is_empty() {
                    timeout_armed = false;
                }
            } else if ud == TAG_TIMEOUT {
                if clients.is_empty() {
                    println!("mouse: no clients after timeout, exiting");
                    return Ok(());
                }
                timeout_armed = false;
            }
        }

        // Send one batched update per io_wait round (collapses intermediate positions).
        if have_update && !clients.is_empty() {
            let msg = IpcMessage {
                tag: MSG_MOUSE_MOVE,
                data: [cursor_x as u64, cursor_y as u64, last_buttons as u64],
                fds: [-1; 4],
            };
            clients.retain(|c| sys::ipc_send(c.send.fd(), &msg, sys::IPC_NONBLOCK) >= 0);
        }
    }
}
