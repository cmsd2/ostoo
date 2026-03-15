extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::task::keyboard::{Key, KeyStream};
use libkernel::task::mailbox::ActorMsg;
use libkernel::task::registry;
use libkernel::print;

use crate::shell::ShellMsg;

const MAX_LINE: usize = 80 - 7 - 1; // 80 cols − len("ostoo> ") − safety margin

// ---------------------------------------------------------------------------
// Messages

/// Control messages for the keyboard actor.
///
/// Currently empty — the actor is purely interrupt-driven.  Future variants
/// could add `SetEcho(bool)`, `SetPrompt(&'static str)`, etc.
pub enum KeyboardMsg {}

// ---------------------------------------------------------------------------
// Info

#[derive(Debug)]
pub struct KeyboardInfo {
    pub keys_processed:   u64,
    pub lines_dispatched: u64,
}

// ---------------------------------------------------------------------------
// Line buffer — stored in the actor, protected by spin::Mutex for Send + Sync.

struct LineBuf {
    buf: [u8; MAX_LINE],
    len: usize,
}

impl LineBuf {
    const fn new() -> Self { LineBuf { buf: [0; MAX_LINE], len: 0 } }
}

// ---------------------------------------------------------------------------
// Actor

pub struct KeyboardActor {
    keys_processed:   AtomicU64,
    lines_dispatched: AtomicU64,
    line:             spin::Mutex<LineBuf>,
}

impl KeyboardActor {
    pub fn new() -> Self {
        KeyboardActor {
            keys_processed:   AtomicU64::new(0),
            lines_dispatched: AtomicU64::new(0),
            line:             spin::Mutex::new(LineBuf::new()),
        }
    }
}

#[devices::actor("keyboard", KeyboardMsg)]
impl KeyboardActor {
    // ── Stream factory ────────────────────────────────────────────────────
    // Called once by the generated run loop before it enters the event loop.
    fn key_stream(&self) -> KeyStream { KeyStream::new() }

    // ── Interrupt stream handler ──────────────────────────────────────────
    #[on_stream(key_stream)]
    async fn on_key(&self, key: Key) {
        self.keys_processed.fetch_add(1, Ordering::Relaxed);
        match key {
            Key::Unicode('\n') | Key::Unicode('\r') => {
                let line_str = {
                    let lb = self.line.lock();
                    let s = core::str::from_utf8(&lb.buf[..lb.len]).unwrap_or("").trim();
                    alloc::string::String::from(s)
                };
                if !line_str.is_empty() {
                    if let Some(shell) = registry::get::<ShellMsg, ()>("shell") {
                        shell.send(ActorMsg::Inner(
                            ShellMsg::KeyLine(alloc::string::String::from(line_str)),
                        ));
                    }
                    self.lines_dispatched.fetch_add(1, Ordering::Relaxed);
                }
                self.line.lock().len = 0;
                // Print blank line; shell prints the next prompt.
                libkernel::println!();
            }

            Key::Unicode('\x08') => {
                let mut lb = self.line.lock();
                if lb.len > 0 {
                    lb.len -= 1;
                    libkernel::vga_buffer::backspace();
                }
            }

            Key::Unicode(c) if c.is_ascii() && !c.is_control() => {
                let mut lb = self.line.lock();
                if lb.len < MAX_LINE {
                    let idx = lb.len;
                    lb.buf[idx] = c as u8;
                    lb.len += 1;
                    print!("{}", c);
                }
            }

            _ => {}
        }
    }

    // ── Info query ────────────────────────────────────────────────────────
    #[on_info]
    async fn on_info(&self) -> KeyboardInfo {
        KeyboardInfo {
            keys_processed:   self.keys_processed.load(Ordering::Relaxed),
            lines_dispatched: self.lines_dispatched.load(Ordering::Relaxed),
        }
    }
}
