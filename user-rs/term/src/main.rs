#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

mod font;
mod vt100;

use ostoo_rt::compositor_proto::*;
use ostoo_rt::ostoo::{self, CompletionPort, NotifyFd, OsError, SharedMem};
use ostoo_rt::sys::{self, IoCompletion, IoSubmission, IpcMessage};
use ostoo_rt::syscall;
use ostoo_rt::{eprintln, println};

// ── Constants ────────────────────────────────────────────────────────

/// Initial terminal window size in cells.
const TERM_COLS: usize = 80;
const TERM_ROWS: usize = 24;

/// Pixel dimensions (8x16 font).
const WIN_W: usize = TERM_COLS * font::FONT_WIDTH;  // 640
const WIN_H: usize = TERM_ROWS * font::FONT_HEIGHT; // 384

const MAX_COMPLETIONS: usize = 16;

// Completion tags
const TAG_KEY: u64 = 0x1000;      // MSG_KEY_EVENT from compositor
const TAG_SHELL_OUT: u64 = 0x2000; // bytes from shell's stdout

// clone flags
const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const SIGCHLD: u64 = 17;

// ── Terminal state ───────────────────────────────────────────────────

struct Terminal {
    buf_ptr: *mut u8,
    buf_w: usize,
    buf_h: usize,
    stride: usize,
    cols: usize,
    rows: usize,
    cursor_row: usize,
    cursor_col: usize,
    fg: u32,
    bg: u32,
    parser: vt100::Vt100Parser,
}

impl Terminal {
    fn new(buf_ptr: *mut u8, w: usize, h: usize) -> Self {
        let mut t = Terminal {
            buf_ptr,
            buf_w: w,
            buf_h: h,
            stride: w * 4,
            cols: w / font::FONT_WIDTH,
            rows: h / font::FONT_HEIGHT,
            cursor_row: 0,
            cursor_col: 0,
            fg: vt100::DEFAULT_FG,
            bg: vt100::DEFAULT_BG,
            parser: vt100::Vt100Parser::new(),
        };
        t.clear_screen();
        t
    }

    fn resize(&mut self, buf_ptr: *mut u8, w: usize, h: usize) {
        self.buf_ptr = buf_ptr;
        self.buf_w = w;
        self.buf_h = h;
        self.stride = w * 4;
        self.cols = w / font::FONT_WIDTH;
        self.rows = h / font::FONT_HEIGHT;
        // Clamp cursor.
        if self.cursor_col >= self.cols {
            self.cursor_col = self.cols.saturating_sub(1);
        }
        if self.cursor_row >= self.rows {
            self.cursor_row = self.rows.saturating_sub(1);
        }
        self.clear_screen();
    }

    fn process_byte(&mut self, byte: u8) {
        let action = self.parser.feed(byte);
        match action {
            vt100::Action::Print(ch) => {
                self.put_char(ch);
                self.cursor_col += 1;
                if self.cursor_col >= self.cols {
                    self.cursor_col = 0;
                    self.advance_row();
                }
            }
            vt100::Action::Newline => {
                self.cursor_col = 0;
                self.advance_row();
            }
            vt100::Action::CarriageReturn => {
                self.cursor_col = 0;
            }
            vt100::Action::Backspace => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    self.put_char(b' ');
                }
            }
            vt100::Action::CursorHome => {
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            vt100::Action::ClearScreen => {
                self.clear_screen();
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            vt100::Action::EraseToEol => {
                for col in self.cursor_col..self.cols {
                    self.draw_char_at(self.cursor_row, col, b' ');
                }
            }
            vt100::Action::CursorUp(n) => {
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            vt100::Action::CursorDown(n) => {
                self.cursor_row = (self.cursor_row + n).min(self.rows.saturating_sub(1));
            }
            vt100::Action::CursorRight(n) => {
                self.cursor_col = (self.cursor_col + n).min(self.cols.saturating_sub(1));
            }
            vt100::Action::CursorLeft(n) => {
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            vt100::Action::SetFg(c) => self.fg = c,
            vt100::Action::SetBg(c) => self.bg = c,
            vt100::Action::ResetColors => {
                self.fg = vt100::DEFAULT_FG;
                self.bg = vt100::DEFAULT_BG;
            }
            vt100::Action::None => {}
        }
    }

    fn put_char(&mut self, ch: u8) {
        self.draw_char_at(self.cursor_row, self.cursor_col, ch);
    }

    fn draw_char_at(&self, row: usize, col: usize, ch: u8) {
        let px = col * font::FONT_WIDTH;
        let py = row * font::FONT_HEIGHT;
        font::draw_char(
            self.buf_ptr,
            self.stride,
            self.buf_w,
            self.buf_h,
            ch,
            px,
            py,
            self.fg,
            self.bg,
        );
    }

    fn advance_row(&mut self) {
        if self.cursor_row < self.rows.saturating_sub(1) {
            self.cursor_row += 1;
        } else {
            self.scroll_up();
        }
    }

    fn scroll_up(&mut self) {
        let row_bytes = font::FONT_HEIGHT * self.stride;
        let total_bytes = self.rows * row_bytes;
        unsafe {
            core::ptr::copy(
                self.buf_ptr.add(row_bytes),
                self.buf_ptr,
                total_bytes - row_bytes,
            );
        }
        let last_row_y = (self.rows - 1) * font::FONT_HEIGHT;
        let bg_bytes = self.bg.to_le_bytes();
        for y in last_row_y..last_row_y + font::FONT_HEIGHT {
            for x in 0..self.buf_w {
                let off = y * self.stride + x * 4;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bg_bytes.as_ptr(),
                        self.buf_ptr.add(off),
                        4,
                    );
                }
            }
        }
    }

    fn clear_screen(&mut self) {
        let bg_bytes = self.bg.to_le_bytes();
        for y in 0..self.buf_h {
            for x in 0..self.buf_w {
                let off = y * self.stride + x * 4;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bg_bytes.as_ptr(),
                        self.buf_ptr.add(off),
                        4,
                    );
                }
            }
        }
    }
}

// ── Main ─────────────────────────────────────────────────────────────

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("term: fatal error (errno {})", e.errno());
            1
        }
    }
}

fn run() -> Result<i32, OsError> {
    println!("term: starting terminal emulator");

    // 1. Connect to the compositor and get a window.
    // Retry lookup — the compositor may still be starting up.
    let reg_send_fd = ostoo::service_lookup_retry(SERVICE_NAME, 20)?;

    let (c2s_send, c2s_recv) = ostoo::ipc_channel(4, 0)?;
    let (s2c_send, s2c_recv) = ostoo::ipc_channel(4, 0)?;

    let connect_msg = IpcMessage {
        tag: MSG_CONNECT,
        data: [WIN_W as u64, WIN_H as u64, 0],
        fds: [c2s_recv.fd(), s2c_send.fd(), -1, -1],
    };
    if sys::ipc_send(reg_send_fd, &connect_msg, 0) < 0 {
        return Err(OsError(-1));
    }
    syscall::close(reg_send_fd as u32);
    drop(c2s_recv);
    drop(s2c_send);

    // Wait for MSG_WINDOW_CREATED.
    let reply = s2c_recv.recv(0)?;
    if reply.tag != MSG_WINDOW_CREATED {
        eprintln!("term: unexpected reply tag {}", reply.tag);
        return Err(OsError(-1));
    }

    let _wid = reply.data[0];
    let w = reply.data[1] as usize;
    let h = reply.data[2] as usize;
    let buf_fd = reply.fds[0];
    let notify_fd = reply.fds[1];

    let mut _buf = SharedMem::from_fd(buf_fd, w * h * 4);
    let buf_ptr = _buf.mmap()?;
    let notify = NotifyFd::from_raw_fd(notify_fd);

    println!("term: window {}x{}", w, h);

    // 2. Create pipes for shell stdin/stdout.
    let mut stdin_fds = [0i32; 2];   // [read_end, write_end]
    let mut stdout_fds = [0i32; 2];  // [read_end, write_end]
    if syscall::pipe2(&mut stdin_fds, 0) < 0 {
        return Err(OsError(-1));
    }
    if syscall::pipe2(&mut stdout_fds, 0) < 0 {
        return Err(OsError(-1));
    }

    // stdin_fds[0] = shell reads from, stdin_fds[1] = term writes to
    // stdout_fds[0] = term reads from, stdout_fds[1] = shell writes to

    // 3. Fork and exec the shell.
    let shell_path = b"/bin/shell\0";
    let clone_flags = CLONE_VM | CLONE_VFORK | SIGCHLD;
    let ret = syscall::clone(clone_flags);
    if ret < 0 {
        eprintln!("term: clone failed ({})", ret);
        return Err(OsError(ret));
    }

    if ret == 0 {
        // Child: set up fd redirections and exec.
        syscall::dup2(stdin_fds[0], 0);   // stdin = pipe read end
        syscall::dup2(stdout_fds[1], 1);  // stdout = pipe write end
        syscall::dup2(stdout_fds[1], 2);  // stderr = pipe write end

        // Close the pipe fds we don't need in the child.
        syscall::close(stdin_fds[0] as u32);
        syscall::close(stdin_fds[1] as u32);
        syscall::close(stdout_fds[0] as u32);
        syscall::close(stdout_fds[1] as u32);

        let argv: [*const u8; 2] = [shell_path.as_ptr(), core::ptr::null()];
        let envp: [*const u8; 5] = [
            b"PATH=/host/bin\0".as_ptr(),
            b"HOME=/\0".as_ptr(),
            b"TERM=dumb\0".as_ptr(),
            b"SHELL=/bin/shell\0".as_ptr(),
            core::ptr::null(),
        ];

        syscall::execve(shell_path.as_ptr(), argv.as_ptr(), envp.as_ptr());
        // If execve returns, it failed.
        syscall::exit(127);
    }

    // Parent: close the pipe ends used by the child.
    let shell_pid = ret;
    syscall::close(stdin_fds[0] as u32);
    syscall::close(stdout_fds[1] as u32);

    let stdin_write_fd = stdin_fds[1];
    let stdout_read_fd = stdout_fds[0];

    println!("term: spawned shell as pid {}", shell_pid);

    // 4. Set up event loop.
    let port = CompletionPort::new()?;
    let mut term = Terminal::new(buf_ptr, w, h);

    // Arm OP_IPC_RECV for key events from compositor.
    let mut key_msg = IpcMessage::default();
    port.submit(&[IoSubmission::ipc_recv(
        TAG_KEY,
        s2c_recv.fd(),
        &mut key_msg,
    )])?;

    // Arm OP_READ for shell stdout.
    let mut shell_buf = [0u8; 256];
    port.submit(&[IoSubmission::read(
        TAG_SHELL_OUT,
        stdout_read_fd,
        &mut shell_buf,
    )])?;

    let mut completions = [IoCompletion::default(); MAX_COMPLETIONS];

    // Signal initial damage so compositor shows our window.
    notify.signal()?;

    loop {
        let n = port.wait(&mut completions, 1, 0)?;
        let mut need_present = false;

        for i in 0..n {
            let cqe = completions[i];

            if cqe.user_data == TAG_KEY {
                if cqe.result >= 0 {
                    if key_msg.tag == MSG_KEY_EVENT {
                        // Key event from compositor.
                        let byte = key_msg.data[0] as u8;
                        let key_type = key_msg.data[2];

                        if key_type == 0 {
                            // ASCII key — write to shell's stdin pipe.
                            let b = [byte];
                            syscall::write(stdin_write_fd as u32, &b);
                        }
                        // TODO: handle special keys (arrows → ESC sequences)
                    } else if key_msg.tag == MSG_WINDOW_RESIZED {
                        // Window was resized by compositor.
                        let new_w = key_msg.data[0] as usize;
                        let new_h = key_msg.data[1] as usize;
                        let new_buf_fd = key_msg.fds[0];
                        if new_buf_fd >= 0 && new_w > 0 && new_h > 0 {
                            let new_buf = SharedMem::from_fd(new_buf_fd, new_w * new_h * 4);
                            if let Ok(new_ptr) = new_buf.mmap() {
                                _buf = new_buf;
                                term.resize(new_ptr, new_w, new_h);
                                need_present = true;
                                // Nudge the shell to redraw its prompt.
                                syscall::write(stdin_write_fd as u32, b"\n");
                            }
                        }
                    }
                }
                // Re-arm.
                key_msg = IpcMessage::default();
                port.submit(&[IoSubmission::ipc_recv(
                    TAG_KEY,
                    s2c_recv.fd(),
                    &mut key_msg,
                )])?;
            } else if cqe.user_data == TAG_SHELL_OUT {
                if cqe.result > 0 {
                    // Bytes from shell — process through VT100 parser.
                    let count = cqe.result as usize;
                    for j in 0..count {
                        term.process_byte(shell_buf[j]);
                    }
                    need_present = true;

                    // Re-arm read.
                    port.submit(&[IoSubmission::read(
                        TAG_SHELL_OUT,
                        stdout_read_fd,
                        &mut shell_buf,
                    )])?;
                } else {
                    // Shell closed stdout (exited).
                    println!("term: shell exited");
                    // Wait for shell to finish.
                    let mut status = 0u32;
                    syscall::wait4(shell_pid, &mut status, 0);
                    return Ok(status as i32);
                }
            }
        }

        if need_present {
            // Signal damage and send MSG_PRESENT.
            notify.signal()?;
            c2s_send.send(
                &IpcMessage {
                    tag: MSG_PRESENT,
                    data: [_wid, 0, 0],
                    fds: [-1; 4],
                },
                0,
            )?;
        }
    }
}
