#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

mod scancode;

use alloc::vec::Vec;
use ostoo_rt::kbd_proto::*;
use ostoo_rt::ostoo::{self, CompletionPort, IpcSend, IrqFd, OsError};
use ostoo_rt::sys::{self, IoCompletion, IoSubmission, IpcMessage};
use ostoo_rt::{eprintln, println};

// ── Constants ────────────────────────────────────────────────────────

const MAX_CLIENTS: usize = 4;
const MAX_COMPLETIONS: usize = 16;

// Completion user_data tags
const TAG_IRQ: u64 = 0x1000;
const TAG_NEW_CLIENT: u64 = 0x2000;

// Timeout for exiting if no clients connect (2 seconds in nanoseconds).
const CLIENT_TIMEOUT_NS: u64 = 2_000_000_000;
const TAG_TIMEOUT: u64 = 0x3000;

// ── Client state ─────────────────────────────────────────────────────

struct Client {
    send: IpcSend,
}

// ── Main ─────────────────────────────────────────────────────────────

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kbd: fatal error (errno {})", e.errno());
            1
        }
    }
}

fn run() -> Result<(), OsError> {
    println!("kbd: starting keyboard driver");

    // 1. Claim keyboard IRQ (GSI 1).
    let irq = IrqFd::new(1)?;
    println!("kbd: claimed IRQ 1 (fd={})", irq.fd());

    // 2. Create registration channel and completion port.
    let (reg_send, reg_recv) = ostoo::ipc_channel(4, 0)?;
    let port = CompletionPort::new()?;

    // 3. Register as "keyboard" service.
    ostoo::service_register(SERVICE_NAME, reg_send.fd())?;
    println!("kbd: registered as 'keyboard'");

    // 4. Arm initial operations.
    // IRQ wait.
    port.submit(&[IoSubmission::irq_wait(TAG_IRQ, irq.fd())])?;

    // IPC recv on registration channel.
    let mut reg_msg = IpcMessage::default();
    port.submit(&[IoSubmission::ipc_recv(
        TAG_NEW_CLIENT,
        reg_recv.fd(),
        &mut reg_msg,
    )])?;

    // Timeout: exit if no clients connect within 2 seconds.
    port.submit(&[IoSubmission::timeout(TAG_TIMEOUT, CLIENT_TIMEOUT_NS)])?;

    // 5. Event loop.
    let mut clients: Vec<Client> = Vec::new();
    let mut decoder = scancode::ScancodeDecoder::new();
    let mut completions = [IoCompletion::default(); MAX_COMPLETIONS];
    let mut timeout_armed = true;

    loop {
        let n = port.wait(&mut completions, 1, 0)?;

        for i in 0..n {
            let cqe = completions[i];
            let ud = cqe.user_data;

            if ud == TAG_IRQ {
                // IRQ fired — scancode is in cqe.result.
                if cqe.result >= 0 {
                    let scancode = cqe.result as u8;
                    if let Some(event) = decoder.feed(scancode) {
                        // Broadcast key event to all clients.
                        let msg = IpcMessage {
                            tag: MSG_KB_KEY,
                            data: [event.byte as u64, event.modifiers, event.key_type],
                            fds: [-1; 4],
                        };
                        // Send to each client; remove any that fail.
                        clients.retain(|c| {
                            sys::ipc_send(c.send.fd(), &msg, sys::IPC_NONBLOCK) >= 0
                        });
                    }
                }
                // Re-arm IRQ wait.
                port.submit(&[IoSubmission::irq_wait(TAG_IRQ, irq.fd())])?;
            } else if ud == TAG_NEW_CLIENT {
                // New client connection.
                if cqe.result >= 0 && reg_msg.tag == MSG_KB_CONNECT {
                    if clients.len() < MAX_CLIENTS {
                        let send_fd = reg_msg.fds[0];
                        if send_fd >= 0 {
                            clients.push(Client {
                                send: IpcSend::from_raw_fd(send_fd),
                            });
                            println!("kbd: client connected (total {})", clients.len());
                        }
                    }
                }
                // Re-arm registration channel.
                reg_msg = IpcMessage::default();
                port.submit(&[IoSubmission::ipc_recv(
                    TAG_NEW_CLIENT,
                    reg_recv.fd(),
                    &mut reg_msg,
                )])?;

                // Cancel the timeout now that a client connected.
                if timeout_armed && !clients.is_empty() {
                    timeout_armed = false;
                }
            } else if ud == TAG_TIMEOUT {
                // Timeout fired with no clients — exit so kernel keyboard resumes.
                if clients.is_empty() {
                    println!("kbd: no clients after timeout, exiting");
                    return Ok(());
                }
                timeout_armed = false;
            }
        }
    }
}
