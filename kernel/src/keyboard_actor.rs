extern crate alloc;

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use libkernel::task::keyboard::{Key, KeyCode, KeyStream};
use libkernel::task::mailbox::ActorMsg;
use libkernel::task::registry;
use libkernel::print;

use crate::shell::ShellMsg;

// ---------------------------------------------------------------------------
// Layout constants

/// Buffer size for the current input line.  Must fit in an 80-column terminal
/// even for the longest reasonable prompt; the actual insertion limit is
/// computed at runtime from the stored prompt length.
const MAX_LINE: usize    = 72;
const MAX_HISTORY: usize = 50;

// ---------------------------------------------------------------------------
// Messages

/// Control messages for the keyboard actor.
pub enum KeyboardMsg {
    /// Update the stored prompt string.  The shell sends this each time the
    /// prompt changes (e.g. after `cd`) so the actor can reprint it correctly
    /// on Ctrl+C / Ctrl+L and position the hardware cursor accurately.
    SetPrompt(alloc::string::String),
}

// ---------------------------------------------------------------------------
// Info

#[derive(Debug)]
#[allow(dead_code)]
pub struct KeyboardInfo {
    pub keys_processed:   u64,
    pub lines_dispatched: u64,
}

// ---------------------------------------------------------------------------
// Line editor state

struct LineState {
    buf:       [u8; MAX_LINE],
    len:       usize,
    cursor:    usize,
    history:   VecDeque<alloc::string::String>,  // oldest[0] … newest[last]
    hist_idx:  Option<usize>,                    // None = live; Some(i) = history[i]
    saved_buf: [u8; MAX_LINE],
    saved_len: usize,
}

impl LineState {
    const fn new() -> Self {
        LineState {
            buf:       [0; MAX_LINE],
            len:       0,
            cursor:    0,
            history:   VecDeque::new(),
            hist_idx:  None,
            saved_buf: [0; MAX_LINE],
            saved_len: 0,
        }
    }

    fn push_history(&mut self, s: &str) {
        // Don't duplicate the most-recent entry.
        if self.history.back().map(|e| e.as_str()) == Some(s) { return; }
        if self.history.len() == MAX_HISTORY { self.history.pop_front(); }
        self.history.push_back(alloc::string::String::from(s));
    }

    fn current_as_string(&self) -> alloc::string::String {
        alloc::string::String::from(
            core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
        )
    }
}

// ---------------------------------------------------------------------------
// Action — computed inside the mutex, executed outside

enum Action {
    Redraw,
    Submit(alloc::string::String),
    ClearScreen,
    Interrupt,
    Ignore,
}

// ---------------------------------------------------------------------------
// Actor

pub struct KeyboardActor {
    keys_processed:   AtomicU64,
    lines_dispatched: AtomicU64,
    line:             spin::Mutex<LineState>,
    /// Current prompt string, kept in sync with the shell via `SetPrompt`.
    prompt:           spin::Mutex<alloc::string::String>,
}

impl KeyboardActor {
    pub fn new() -> Self {
        KeyboardActor {
            keys_processed:   AtomicU64::new(0),
            lines_dispatched: AtomicU64::new(0),
            line:             spin::Mutex::new(LineState::new()),
            // Initial value matches Shell::new() CWD = "/".
            prompt:           spin::Mutex::new(alloc::string::String::from("ostoo:/> ")),
        }
    }
}

#[devices::actor("keyboard", KeyboardMsg)]
impl KeyboardActor {
    // ── Stream factory ────────────────────────────────────────────────────
    fn key_stream(&self) -> KeyStream { KeyStream::new() }

    // ── Interrupt stream handler ──────────────────────────────────────────
    #[on_stream(key_stream)]
    async fn on_key(&self, key: Key) {
        self.keys_processed.fetch_add(1, Ordering::Relaxed);

        // Read the prompt length before acquiring the line lock so we never
        // hold two locks simultaneously.
        let prompt_col = self.prompt.lock().len();
        let max_input  = (80usize.saturating_sub(prompt_col + 1)).min(MAX_LINE);

        let action = {
            let mut st = self.line.lock();
            match key {
                // ── Submit ───────────────────────────────────────────
                Key::Unicode('\n') | Key::Unicode('\r') => {
                    let s = st.current_as_string();
                    let trimmed = alloc::string::String::from(s.trim());
                    st.len = 0; st.cursor = 0; st.hist_idx = None;
                    if !trimmed.is_empty() {
                        st.push_history(&trimmed);
                        Action::Submit(trimmed)
                    } else {
                        Action::Redraw
                    }
                }

                // ── Backspace / Ctrl+H ───────────────────────────────
                Key::Unicode('\x08') => {
                    if st.cursor > 0 {
                        let (src, dst) = (st.cursor, st.cursor - 1);
                        let end = st.len;
                        st.buf.copy_within(src..end, dst);
                        st.cursor -= 1;
                        st.len   -= 1;
                        Action::Redraw
                    } else { Action::Ignore }
                }

                // ── Delete (forward) ─────────────────────────────────
                Key::RawKey(KeyCode::Delete) => {
                    if st.cursor < st.len {
                        let (src, dst, end) = (st.cursor + 1, st.cursor, st.len);
                        st.buf.copy_within(src..end, dst);
                        st.len -= 1;
                        Action::Redraw
                    } else { Action::Ignore }
                }

                // ── Arrow left / Ctrl+B ──────────────────────────────
                Key::RawKey(KeyCode::ArrowLeft) | Key::Unicode('\x02') => {
                    if st.cursor > 0 { st.cursor -= 1; Action::Redraw }
                    else { Action::Ignore }
                }

                // ── Arrow right / Ctrl+F ─────────────────────────────
                Key::RawKey(KeyCode::ArrowRight) | Key::Unicode('\x06') => {
                    if st.cursor < st.len { st.cursor += 1; Action::Redraw }
                    else { Action::Ignore }
                }

                // ── Home / Ctrl+A ────────────────────────────────────
                Key::RawKey(KeyCode::Home) | Key::Unicode('\x01') => {
                    st.cursor = 0; Action::Redraw
                }

                // ── End / Ctrl+E ─────────────────────────────────────
                Key::RawKey(KeyCode::End) | Key::Unicode('\x05') => {
                    st.cursor = st.len; Action::Redraw
                }

                // ── History up / Ctrl+P ──────────────────────────────
                Key::RawKey(KeyCode::ArrowUp) | Key::Unicode('\x10') => {
                    let hlen = st.history.len();
                    if hlen == 0 {
                        Action::Ignore
                    } else {
                        let new_idx = match st.hist_idx {
                            None => {
                                st.saved_buf = st.buf;
                                st.saved_len = st.len;
                                hlen - 1
                            }
                            Some(0)  => 0,
                            Some(i)  => i - 1,
                        };
                        // Copy history entry into a temporary to avoid aliasing.
                        let mut tmp = [0u8; MAX_LINE];
                        let n = {
                            let entry = st.history[new_idx].as_bytes();
                            let n = entry.len().min(MAX_LINE);
                            tmp[..n].copy_from_slice(&entry[..n]);
                            n
                        };
                        st.buf[..n].copy_from_slice(&tmp[..n]);
                        st.len = n; st.cursor = n;
                        st.hist_idx = Some(new_idx);
                        Action::Redraw
                    }
                }

                // ── History down / Ctrl+N ────────────────────────────
                Key::RawKey(KeyCode::ArrowDown) | Key::Unicode('\x0E') => {
                    match st.hist_idx {
                        None => Action::Ignore,
                        Some(i) if i + 1 >= st.history.len() => {
                            st.buf = st.saved_buf;
                            st.len = st.saved_len;
                            st.cursor = st.len;
                            st.hist_idx = None;
                            Action::Redraw
                        }
                        Some(i) => {
                            let new_idx = i + 1;
                            let mut tmp = [0u8; MAX_LINE];
                            let n = {
                                let entry = st.history[new_idx].as_bytes();
                                let n = entry.len().min(MAX_LINE);
                                tmp[..n].copy_from_slice(&entry[..n]);
                                n
                            };
                            st.buf[..n].copy_from_slice(&tmp[..n]);
                            st.len = n; st.cursor = n;
                            st.hist_idx = Some(new_idx);
                            Action::Redraw
                        }
                    }
                }

                // ── Kill to end / Ctrl+K ─────────────────────────────
                Key::Unicode('\x0B') => {
                    st.len = st.cursor; Action::Redraw
                }

                // ── Kill to start / Ctrl+U ───────────────────────────
                Key::Unicode('\x15') => {
                    let (src, end) = (st.cursor, st.len);
                    st.buf.copy_within(src..end, 0);
                    st.len   -= src;
                    st.cursor = 0;
                    Action::Redraw
                }

                // ── Delete previous word / Ctrl+W ────────────────────
                Key::Unicode('\x17') => {
                    let mut i = st.cursor;
                    while i > 0 && st.buf[i - 1] == b' ' { i -= 1; }
                    while i > 0 && st.buf[i - 1] != b' ' { i -= 1; }
                    let removed = st.cursor - i;
                    let (src, end) = (st.cursor, st.len);
                    st.buf.copy_within(src..end, i);
                    st.len   -= removed;
                    st.cursor = i;
                    Action::Redraw
                }

                // ── Ctrl+L (clear screen) ────────────────────────────
                Key::Unicode('\x0C') => Action::ClearScreen,

                // ── Ctrl+C (interrupt / clear line) ──────────────────
                Key::Unicode('\x03') => {
                    st.len = 0; st.cursor = 0; st.hist_idx = None;
                    Action::Interrupt
                }

                // ── Printable ASCII (with insertion) ─────────────────
                Key::Unicode(c) if c.is_ascii() && !c.is_control() => {
                    if st.len < max_input {
                        let b = c as u8;
                        let cur = st.cursor;
                        let end = st.len;
                        if cur < end {
                            st.buf.copy_within(cur..end, cur + 1);
                        }
                        st.buf[cur] = b;
                        st.cursor += 1;
                        st.len   += 1;
                        Action::Redraw
                    } else { Action::Ignore }
                }

                _ => Action::Ignore,
            }
        }; // mutex released here

        // ── Act outside the mutex ─────────────────────────────────────
        match action {
            Action::Submit(line) => {
                libkernel::println!();
                self.lines_dispatched.fetch_add(1, Ordering::Relaxed);
                if let Some(shell) = registry::get::<ShellMsg, ()>("shell") {
                    shell.send(ActorMsg::Inner(ShellMsg::KeyLine(line)));
                }
                // Shell prints the next prompt after handling the command.
            }
            Action::Redraw => {
                let st = self.line.lock();
                libkernel::vga_buffer::redraw_line(prompt_col, &st.buf, st.len, st.cursor);
            }
            Action::ClearScreen => {
                libkernel::vga_buffer::clear_content();
                let prompt_str = self.prompt.lock().clone();
                print!("{}", prompt_str);
                let st = self.line.lock();
                libkernel::vga_buffer::redraw_line(prompt_str.len(), &st.buf, st.len, st.cursor);
            }
            Action::Interrupt => {
                libkernel::println!("^C");
                let prompt_str = self.prompt.lock().clone();
                print!("{}", prompt_str);
            }
            Action::Ignore => {}
        }
    }

    // ── SetPrompt message ─────────────────────────────────────────────────
    #[on_message(SetPrompt)]
    async fn on_set_prompt(&self, prompt: alloc::string::String) {
        *self.prompt.lock() = prompt;
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
