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
// Action — computed inside the mutex, executed outside

enum Action {
    Redraw,
    Submit(alloc::string::String),
    ClearScreen,
    Interrupt,
    Ignore,
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

    /// Copy a history entry into the edit buffer, avoiding aliasing issues.
    fn restore_from_history(&mut self, idx: usize) {
        let mut tmp = [0u8; MAX_LINE];
        let n = {
            let entry = self.history[idx].as_bytes();
            let n = entry.len().min(MAX_LINE);
            tmp[..n].copy_from_slice(&entry[..n]);
            n
        };
        self.buf[..n].copy_from_slice(&tmp[..n]);
        self.len = n;
        self.cursor = n;
    }

    // ── Key handlers ─────────────────────────────────────────────────────

    fn submit(&mut self) -> Action {
        let s = self.current_as_string();
        let trimmed = alloc::string::String::from(s.trim());
        self.len = 0; self.cursor = 0; self.hist_idx = None;
        if !trimmed.is_empty() {
            self.push_history(&trimmed);
            Action::Submit(trimmed)
        } else {
            Action::Redraw
        }
    }

    fn backspace(&mut self) -> Action {
        if self.cursor > 0 {
            let (src, dst) = (self.cursor, self.cursor - 1);
            let end = self.len;
            self.buf.copy_within(src..end, dst);
            self.cursor -= 1;
            self.len   -= 1;
            Action::Redraw
        } else { Action::Ignore }
    }

    fn delete_forward(&mut self) -> Action {
        if self.cursor < self.len {
            let (src, dst, end) = (self.cursor + 1, self.cursor, self.len);
            self.buf.copy_within(src..end, dst);
            self.len -= 1;
            Action::Redraw
        } else { Action::Ignore }
    }

    fn move_left(&mut self) -> Action {
        if self.cursor > 0 { self.cursor -= 1; Action::Redraw }
        else { Action::Ignore }
    }

    fn move_right(&mut self) -> Action {
        if self.cursor < self.len { self.cursor += 1; Action::Redraw }
        else { Action::Ignore }
    }

    fn move_home(&mut self) -> Action {
        self.cursor = 0; Action::Redraw
    }

    fn move_end(&mut self) -> Action {
        self.cursor = self.len; Action::Redraw
    }

    fn history_up(&mut self) -> Action {
        let hlen = self.history.len();
        if hlen == 0 {
            return Action::Ignore;
        }
        let new_idx = match self.hist_idx {
            None => {
                self.saved_buf = self.buf;
                self.saved_len = self.len;
                hlen - 1
            }
            Some(0)  => 0,
            Some(i)  => i - 1,
        };
        self.restore_from_history(new_idx);
        self.hist_idx = Some(new_idx);
        Action::Redraw
    }

    fn history_down(&mut self) -> Action {
        match self.hist_idx {
            None => Action::Ignore,
            Some(i) if i + 1 >= self.history.len() => {
                self.buf = self.saved_buf;
                self.len = self.saved_len;
                self.cursor = self.len;
                self.hist_idx = None;
                Action::Redraw
            }
            Some(i) => {
                let new_idx = i + 1;
                self.restore_from_history(new_idx);
                self.hist_idx = Some(new_idx);
                Action::Redraw
            }
        }
    }

    fn kill_to_end(&mut self) -> Action {
        self.len = self.cursor; Action::Redraw
    }

    fn kill_to_start(&mut self) -> Action {
        let (src, end) = (self.cursor, self.len);
        self.buf.copy_within(src..end, 0);
        self.len   -= src;
        self.cursor = 0;
        Action::Redraw
    }

    fn delete_word(&mut self) -> Action {
        let mut i = self.cursor;
        while i > 0 && self.buf[i - 1] == b' ' { i -= 1; }
        while i > 0 && self.buf[i - 1] != b' ' { i -= 1; }
        let removed = self.cursor - i;
        let (src, end) = (self.cursor, self.len);
        self.buf.copy_within(src..end, i);
        self.len   -= removed;
        self.cursor = i;
        Action::Redraw
    }

    fn interrupt(&mut self) -> Action {
        self.len = 0; self.cursor = 0; self.hist_idx = None;
        Action::Interrupt
    }

    fn insert_char(&mut self, c: char, max_input: usize) -> Action {
        if self.len < max_input {
            let b = c as u8;
            let cur = self.cursor;
            let end = self.len;
            if cur < end {
                self.buf.copy_within(cur..end, cur + 1);
            }
            self.buf[cur] = b;
            self.cursor += 1;
            self.len   += 1;
            Action::Redraw
        } else { Action::Ignore }
    }
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

        // If a userspace process is the foreground, send raw bytes to the
        // console input buffer instead of the kernel line editor.
        let fg = libkernel::console::foreground_pid();
        if fg != libkernel::process::ProcessId::KERNEL {
            match key {
                Key::Unicode('\n') | Key::Unicode('\r') => {
                    libkernel::console::push_input(b'\n');
                }
                Key::Unicode('\x08') => {
                    libkernel::console::push_input(0x7F); // DEL
                }
                Key::Unicode('\x03') => {
                    libkernel::console::push_input(0x03); // Ctrl+C
                }
                Key::Unicode('\x04') => {
                    libkernel::console::push_input(0x04); // Ctrl+D
                }
                Key::Unicode('\t') => {
                    libkernel::console::push_input(0x09); // Tab
                }
                Key::Unicode(c) if c.is_ascii() => {
                    libkernel::console::push_input(c as u8);
                }
                _ => {} // ignore non-ASCII for now
            }
            return;
        }

        // Read the prompt length before acquiring the line lock so we never
        // hold two locks simultaneously.
        let prompt_col = self.prompt.lock().len();
        let max_input  = (80usize.saturating_sub(prompt_col + 1)).min(MAX_LINE);

        let action = {
            let mut st = self.line.lock();
            match key {
                Key::Unicode('\n') | Key::Unicode('\r')                  => st.submit(),
                Key::Unicode('\x08')                                     => st.backspace(),
                Key::RawKey(KeyCode::Delete)                             => st.delete_forward(),
                Key::RawKey(KeyCode::ArrowLeft)  | Key::Unicode('\x02') => st.move_left(),
                Key::RawKey(KeyCode::ArrowRight) | Key::Unicode('\x06') => st.move_right(),
                Key::RawKey(KeyCode::Home)       | Key::Unicode('\x01') => st.move_home(),
                Key::RawKey(KeyCode::End)        | Key::Unicode('\x05') => st.move_end(),
                Key::RawKey(KeyCode::ArrowUp)    | Key::Unicode('\x10') => st.history_up(),
                Key::RawKey(KeyCode::ArrowDown)  | Key::Unicode('\x0E') => st.history_down(),
                Key::Unicode('\x0B')                                     => st.kill_to_end(),
                Key::Unicode('\x15')                                     => st.kill_to_start(),
                Key::Unicode('\x17')                                     => st.delete_word(),
                Key::Unicode('\x0C')                                     => Action::ClearScreen,
                Key::Unicode('\x03')                                     => st.interrupt(),
                Key::Unicode(c) if c.is_ascii() && !c.is_control()      => st.insert_char(c, max_input),
                _                                                        => Action::Ignore,
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
