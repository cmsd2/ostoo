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

use alloc::vec;
use alloc::vec::Vec;

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

// ── Cell buffer types ────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: u32,
    bg: u32,
}

impl Cell {
    fn blank() -> Self {
        Cell {
            ch: b' ',
            fg: vt100::DEFAULT_FG,
            bg: vt100::DEFAULT_BG,
        }
    }
}

struct LogicalLine {
    cells: Vec<Cell>,
}

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
    cells: Vec<Cell>,
    wrapped: Vec<bool>,
}

impl Terminal {
    fn new(buf_ptr: *mut u8, w: usize, h: usize) -> Self {
        let cols = w / font::FONT_WIDTH;
        let rows = h / font::FONT_HEIGHT;
        let mut t = Terminal {
            buf_ptr,
            buf_w: w,
            buf_h: h,
            stride: w * 4,
            cols,
            rows,
            cursor_row: 0,
            cursor_col: 0,
            fg: vt100::DEFAULT_FG,
            bg: vt100::DEFAULT_BG,
            parser: vt100::Vt100Parser::new(),
            cells: vec![Cell::blank(); rows * cols],
            wrapped: vec![false; rows],
        };
        t.clear_screen();
        t
    }

    fn resize(&mut self, buf_ptr: *mut u8, w: usize, h: usize) {
        // 1. Extract logical lines from current cell buffer.
        let lines = self.extract_logical_lines();
        // 2. Save cursor's logical position.
        let (cursor_line, cursor_off) = self.cursor_to_logical(&lines);

        // 3. Update dimensions.
        self.buf_ptr = buf_ptr;
        self.buf_w = w;
        self.buf_h = h;
        self.stride = w * 4;
        self.cols = w / font::FONT_WIDTH;
        self.rows = h / font::FONT_HEIGHT;

        // 4. Allocate new cells/wrapped.
        self.cells = vec![Cell::blank(); self.rows * self.cols];
        self.wrapped = vec![false; self.rows];

        // 5. Reflow lines into new grid.
        let (new_row, new_col) = self.reflow_lines(&lines, cursor_line, cursor_off);
        self.cursor_row = new_row;
        self.cursor_col = new_col;

        // 6. Redraw pixels from cells.
        self.redraw();
    }

    /// Redraw the entire pixel buffer from the cell buffer.
    fn redraw(&self) {
        // Clear pixel buffer first.
        let bg_bytes = vt100::DEFAULT_BG.to_le_bytes();
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
        // Draw each cell.
        for r in 0..self.rows {
            for c in 0..self.cols {
                let cell = self.cells[r * self.cols + c];
                let px = c * font::FONT_WIDTH;
                let py = r * font::FONT_HEIGHT;
                font::draw_char(
                    self.buf_ptr,
                    self.stride,
                    self.buf_w,
                    self.buf_h,
                    cell.ch,
                    px,
                    py,
                    cell.fg,
                    cell.bg,
                );
            }
        }
    }

    /// Extract logical lines from the current cell buffer.
    /// A new logical line starts wherever `wrapped[r] == false`.
    fn extract_logical_lines(&self) -> Vec<LogicalLine> {
        let mut lines: Vec<LogicalLine> = Vec::new();
        for r in 0..self.rows {
            let row_start = r * self.cols;
            let row_cells = &self.cells[row_start..row_start + self.cols];
            if !self.wrapped[r] || lines.is_empty() {
                // Start a new logical line.
                lines.push(LogicalLine {
                    cells: Vec::from(row_cells),
                });
            } else {
                // Continuation — append to the last logical line.
                let last = lines.last_mut().unwrap();
                last.cells.extend_from_slice(row_cells);
            }
        }
        // Trim trailing blanks from each logical line.
        for line in &mut lines {
            while line.cells.last().map_or(false, |c| c.ch == b' ') {
                line.cells.pop();
            }
        }
        lines
    }

    /// Convert current (cursor_row, cursor_col) to (logical_line_index, offset_within_line).
    fn cursor_to_logical(&self, lines: &[LogicalLine]) -> (usize, usize) {
        // Walk through rows to find which logical line the cursor is in.
        let mut line_idx = 0usize;
        let mut row_within_line = 0usize;
        for r in 0..self.rows {
            if r == self.cursor_row {
                let offset = row_within_line * self.cols + self.cursor_col;
                return (line_idx.min(lines.len().saturating_sub(1)), offset);
            }
            // Check if next row starts a new logical line.
            if r + 1 < self.rows && !self.wrapped[r + 1] {
                line_idx += 1;
                row_within_line = 0;
            } else {
                row_within_line += 1;
            }
        }
        // Fallback.
        (line_idx.min(lines.len().saturating_sub(1)), self.cursor_col)
    }

    /// Reflow logical lines into the current cells/wrapped grid at new dimensions.
    /// Returns the new (cursor_row, cursor_col).
    fn reflow_lines(
        &mut self,
        lines: &[LogicalLine],
        cursor_line: usize,
        cursor_offset: usize,
    ) -> (usize, usize) {
        let cols = self.cols;
        let rows = self.rows;

        // First, compute how many screen rows each logical line needs
        // and figure out the total.
        let mut total_rows = 0usize;
        let mut line_row_counts: Vec<usize> = Vec::with_capacity(lines.len());
        for line in lines {
            let n = if line.cells.is_empty() {
                1
            } else {
                (line.cells.len() + cols - 1) / cols
            };
            line_row_counts.push(n);
            total_rows += n;
        }

        // Determine how many rows to skip from the top if content overflows.
        let skip_rows = total_rows.saturating_sub(rows);

        // Compute the new cursor position.
        let mut cursor_new_row = 0usize;
        let mut cursor_new_col = 0usize;
        {
            let mut absolute_row = 0usize;
            for (i, _) in lines.iter().enumerate() {
                let n = line_row_counts[i];
                if i == cursor_line {
                    // cursor_offset is the character offset within this logical line.
                    let row_in_line = if cols > 0 { cursor_offset / cols } else { 0 };
                    let col_in_line = if cols > 0 { cursor_offset % cols } else { 0 };
                    let abs = absolute_row + row_in_line;
                    if abs >= skip_rows {
                        cursor_new_row = abs - skip_rows;
                        cursor_new_col = col_in_line;
                    }
                    // else: cursor's line was scrolled off; stays at (0,0).
                    break;
                }
                absolute_row += n;
            }
        }

        // Now place the lines into the grid.
        let mut screen_row = 0usize; // absolute row (before skip adjustment)
        for (i, line) in lines.iter().enumerate() {
            let n = line_row_counts[i];
            for sub_row in 0..n {
                if screen_row >= skip_rows && (screen_row - skip_rows) < rows {
                    let dest_row = screen_row - skip_rows;
                    // Mark wrap continuation.
                    self.wrapped[dest_row] = sub_row > 0;
                    // Copy cells for this sub-row.
                    let src_start = sub_row * cols;
                    let src_end = (src_start + cols).min(line.cells.len());
                    let dest_start = dest_row * cols;
                    for j in src_start..src_end {
                        self.cells[dest_start + (j - src_start)] = line.cells[j];
                    }
                    // Remaining cells in the row stay as blank (from initialization).
                }
                screen_row += 1;
            }
        }

        // Clamp cursor.
        cursor_new_row = cursor_new_row.min(rows.saturating_sub(1));
        cursor_new_col = cursor_new_col.min(cols.saturating_sub(1));

        (cursor_new_row, cursor_new_col)
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
                    if self.cursor_row < self.rows {
                        self.wrapped[self.cursor_row] = true;
                    }
                }
            }
            vt100::Action::Newline => {
                self.cursor_col = 0;
                self.advance_row();
                if self.cursor_row < self.rows {
                    self.wrapped[self.cursor_row] = false;
                }
            }
            vt100::Action::CarriageReturn => {
                self.cursor_col = 0;
            }
            vt100::Action::Backspace => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    // put_char writes the cell + draws the pixel
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
                    let idx = self.cursor_row * self.cols + col;
                    if idx < self.cells.len() {
                        self.cells[idx] = Cell { ch: b' ', fg: self.fg, bg: self.bg };
                    }
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
        let idx = self.cursor_row * self.cols + self.cursor_col;
        if idx < self.cells.len() {
            self.cells[idx] = Cell { ch, fg: self.fg, bg: self.bg };
        }
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
        // Shift pixel rows up.
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

        // Shift cell buffer up by one row.
        let cols = self.cols;
        self.cells.copy_within(cols.., 0);
        let last_start = (self.rows - 1) * cols;
        for c in &mut self.cells[last_start..] {
            *c = Cell::blank();
        }

        // Shift wrapped flags up by one row.
        for r in 1..self.rows {
            self.wrapped[r - 1] = self.wrapped[r];
        }
        if self.rows > 0 {
            self.wrapped[self.rows - 1] = false;
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
        for c in self.cells.iter_mut() {
            *c = Cell::blank();
        }
        for w in self.wrapped.iter_mut() {
            *w = false;
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
                            if byte == 0x03 {
                                // Ctrl+C — send SIGINT to the shell.
                                syscall::kill(shell_pid, 2); // SIGINT = 2
                            } else {
                                // ASCII key — write to shell's stdin pipe.
                                let b = [byte];
                                syscall::write(stdin_write_fd as u32, &b);
                            }
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
                                // Nudge the shell to redraw its prompt at the top.
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
