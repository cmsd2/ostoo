extern crate alloc;

use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::Poll;
use futures_util::stream::StreamExt;
use libkernel::task::keyboard::{Key, KeyStream};
use libkernel::task::mailbox::{ActorMsg, ActorStatus, Mailbox};
use libkernel::task::registry;
use libkernel::{print, println};
use devices::task_driver::{DriverTask, StopToken};

use crate::shell::ShellMsg;

const PROMPT:   &str = "ostoo> ";
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
// Actor

pub struct KeyboardActor {
    keys_processed:   AtomicU64,
    lines_dispatched: AtomicU64,
}

impl KeyboardActor {
    pub fn new() -> Self {
        KeyboardActor {
            keys_processed:   AtomicU64::new(0),
            lines_dispatched: AtomicU64::new(0),
        }
    }

    fn info(&self) -> KeyboardInfo {
        KeyboardInfo {
            keys_processed:   self.keys_processed.load(Ordering::Relaxed),
            lines_dispatched: self.lines_dispatched.load(Ordering::Relaxed),
        }
    }
}

pub type KeyboardDriver = devices::task_driver::TaskDriver<KeyboardActor>;

// ---------------------------------------------------------------------------
// DriverTask — manual impl because the run loop races two event sources

impl DriverTask for KeyboardActor {
    type Message = KeyboardMsg;
    type Info    = KeyboardInfo;

    fn name(&self) -> &'static str { "keyboard" }

    fn run(
        handle: Arc<Self>,
        _stop:  StopToken,
        inbox:  Arc<Mailbox<ActorMsg<KeyboardMsg, KeyboardInfo>>>,
    ) -> impl Future<Output = ()> + Send {
        async move {
            log::info!("[keyboard] started");

            // KeyStream maintains PS/2 decoder state — must survive across
            // loop iterations, so it lives outside the loop.
            let mut keys = KeyStream::new();
            let mut buf  = [0u8; MAX_LINE];
            let mut len  = 0usize;

            println!();
            print!("{}", PROMPT);

            loop {
                // ── The core interrupt-driven actor pattern ────────────────
                //
                // The keyboard actor has two independent event sources:
                //
                //   1. Hardware interrupts → SCANCODE_QUEUE → KeyStream
                //      Waker: AtomicWaker in keyboard::WAKER
                //
                //   2. Actor mailbox → inbox.recv()
                //      Waker: AtomicWaker in Mailbox::waker
                //
                // poll_fn polls both on every task wakeup and returns
                // whichever is ready.  Both AtomicWakers register the *same*
                // task waker, so the task is rescheduled by either source.
                //
                // inbox.recv() is created fresh each iteration.  That is
                // intentional: MailboxRecv re-registers the waker on every
                // poll, so no wakeups are lost.
                // ──────────────────────────────────────────────────────────

                enum Event {
                    Key(Key),
                    Msg(ActorMsg<KeyboardMsg, KeyboardInfo>),
                    Stopped,
                }

                let mut recv = inbox.recv();

                let event = core::future::poll_fn(|cx| {
                    // Interrupt path — fires when an IRQ pushes a scancode.
                    // KeyStream is Unpin so poll_next_unpin avoids Pin::new.
                    match keys.poll_next_unpin(cx) {
                        Poll::Ready(Some(key)) => return Poll::Ready(Event::Key(key)),
                        Poll::Ready(None)      => return Poll::Ready(Event::Stopped),
                        Poll::Pending          => {}
                    }
                    // Message path — fires when another actor sends a message
                    // or when stop() closes the mailbox.
                    match Pin::new(&mut recv).poll(cx) {
                        Poll::Ready(Some(msg)) => return Poll::Ready(Event::Msg(msg)),
                        Poll::Ready(None)      => return Poll::Ready(Event::Stopped),
                        Poll::Pending          => {}
                    }
                    Poll::Pending
                }).await;

                match event {
                    Event::Stopped => break,

                    Event::Key(key) => {
                        handle.keys_processed.fetch_add(1, Ordering::Relaxed);
                        match key {
                            Key::Unicode('\n') | Key::Unicode('\r') => {
                                println!();
                                let line = core::str::from_utf8(&buf[..len])
                                    .unwrap_or("").trim();
                                if !line.is_empty() {
                                    let line_string = alloc::string::String::from(line);
                                    // Look up the shell dynamically so the
                                    // keyboard actor doesn't hold a direct Arc
                                    // reference to the shell mailbox.
                                    if let Some(shell) = registry::get::<ShellMsg, ()>("shell") {
                                        shell.ask(|r| ActorMsg::Inner(
                                            ShellMsg::KeyLine(line_string, r),
                                        )).await;
                                    }
                                    handle.lines_dispatched.fetch_add(1, Ordering::Relaxed);
                                }
                                len = 0;
                                print!("{}", PROMPT);
                            }

                            Key::Unicode('\x08') => {
                                if len > 0 {
                                    len -= 1;
                                    libkernel::vga_buffer::backspace();
                                }
                            }

                            Key::Unicode(c) if c.is_ascii() && !c.is_control() => {
                                if len < MAX_LINE {
                                    buf[len] = c as u8;
                                    len += 1;
                                    print!("{}", c);
                                }
                            }

                            _ => {}
                        }
                    }

                    Event::Msg(msg) => match msg {
                        ActorMsg::Info(reply) => {
                            reply.send(ActorStatus {
                                name:    "keyboard",
                                running: true,
                                info:    handle.info(),
                            });
                        }
                        ActorMsg::ErasedInfo(reply) => {
                            reply.send(ActorStatus {
                                name:    "keyboard",
                                running: true,
                                info:    alloc::boxed::Box::new(handle.info()),
                            });
                        }
                        ActorMsg::Inner(msg) => match msg {
                            // No messages yet — exhaustive match over empty enum
                        }
                    }
                }
            }

            log::info!("[keyboard] stopped");
        }
    }
}
