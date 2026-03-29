//! Output capture — used by the kernel shell pager.
//!
//! When capture is active, `print!`/`println!` output is diverted into an
//! internal ring buffer instead of going to the VGA display.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::irq_mutex::IrqMutex;

use super::{WRITER, BUFFER_WIDTH};

/// Maximum number of lines the capture buffer can hold.
pub const MAX_CAPTURE_LINES: usize = 256;

pub(super) struct CaptureBuffer {
    data:    [[u8; BUFFER_WIDTH]; MAX_CAPTURE_LINES],
    lens:    [usize; MAX_CAPTURE_LINES],
    count:   usize,          // completed lines
    cur:     [u8; BUFFER_WIDTH],
    cur_len: usize,          // bytes in the current partial line
}

impl CaptureBuffer {
    pub(super) const fn new() -> Self {
        CaptureBuffer {
            data:    [[0u8; BUFFER_WIDTH]; MAX_CAPTURE_LINES],
            lens:    [0usize; MAX_CAPTURE_LINES],
            count:   0,
            cur:     [0u8; BUFFER_WIDTH],
            cur_len: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.count   = 0;
        self.cur_len = 0;
    }

    fn push_byte(&mut self, b: u8) {
        if b == b'\n' {
            self.commit_line();
        } else if self.cur_len < BUFFER_WIDTH {
            self.cur[self.cur_len] = if (0x20..=0x7e).contains(&b) { b } else { b'?' };
            self.cur_len += 1;
        }
    }

    fn commit_line(&mut self) {
        if self.count < MAX_CAPTURE_LINES {
            let l = self.cur_len;
            self.data[self.count][..l].copy_from_slice(&self.cur[..l]);
            self.lens[self.count] = l;
            self.count += 1;
        }
        self.cur_len = 0;
    }

    pub(super) fn flush_partial(&mut self) {
        if self.cur_len > 0 {
            self.commit_line();
        }
    }
}

impl fmt::Write for CaptureBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() { self.push_byte(b); }
        Ok(())
    }
}

pub(super) static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(super) static CAPTURE: IrqMutex<CaptureBuffer> = IrqMutex::new(CaptureBuffer::new());

/// Begin capturing all `print!`/`println!` output into an internal buffer
/// instead of writing directly to the VGA screen.
/// Call [`capture_end`] to stop and retrieve the line count.
pub fn capture_start() {
    CAPTURE.lock().reset();
    CAPTURE_ACTIVE.store(true, Ordering::Relaxed);
}

/// Stop capturing and return the number of captured lines.
pub fn capture_end() -> usize {
    CAPTURE_ACTIVE.store(false, Ordering::Relaxed);
    let mut c = CAPTURE.lock();
    c.flush_partial();
    c.count
}

/// Write captured line `i` to the VGA display followed by a newline.
/// No-op if `i` is out of range.
pub fn capture_print_line(i: usize) {
    let (data, len) = {
        let c = CAPTURE.lock();
        if i >= c.count { return; }
        let l = c.lens[i];
        let mut buf = [0u8; BUFFER_WIDTH];
        buf[..l].copy_from_slice(&c.data[i][..l]);
        (buf, l)
    };
    let mut w = WRITER.lock();
    for &b in &data[..len] { w.write_byte(b); }
    w.write_byte(b'\n');
}

/// Clear the bottom VGA row (the current writing line) and reset the column
/// to 0.  Used by the pager to erase the `-- More --` prompt after a keypress.
pub fn clear_current_line() {
    let mut w = WRITER.lock();
    let last = w.rows - 1;
    w.clear_row(last);
    w.column_position = 0;
}
