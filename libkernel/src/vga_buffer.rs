use volatile::Volatile;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lazy_static::lazy_static;
use crate::irq_mutex::IrqMutex;

/// Physical VGA base as a kernel virtual address.  Initially the bootloader
/// identity address (0xb8000).  Call `remap_vga` after memory services are
/// ready to switch to the phys-mem-offset alias in the high half.
static VGA_BASE: AtomicU64 = AtomicU64::new(0xb8000);

lazy_static! {
    pub static ref WRITER: IrqMutex<Writer> = IrqMutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        // Safety: 0xb8000 is the standard VGA text-mode buffer address,
        // identity-mapped by the bootloader at startup.
        buffer: unsafe { VgaBuffer::new(0xb8000 as *mut Buffer) },
    });
}

/// Switch the VGA writer to a high-half virtual address for the VGA
/// framebuffer.
///
/// `vga_virt` must point to the VGA buffer (physical `0xb8000`) mapped into
/// the kernel high half (entries 256–510), so it is present in every
/// isolated user page table.  Call once, after `memory::init_services()`.
pub fn remap_vga(vga_virt: x86_64::VirtAddr) {
    let base = vga_virt.as_u64();
    VGA_BASE.store(base, Ordering::Relaxed);
    WRITER.lock().buffer.set_ptr(base as *mut Buffer);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// Write a formatted string to row 0 (the fixed status bar).
#[macro_export]
macro_rules! status_bar {
    ($($arg:tt)*) => ($crate::vga_buffer::print_status_bar(format_args!($($arg)*)));
}

// ---------------------------------------------------------------------------
// Output capture (used by the shell pager)

/// Maximum number of lines the capture buffer can hold.
pub const MAX_CAPTURE_LINES: usize = 256;

struct CaptureBuffer {
    data:    [[u8; BUFFER_WIDTH]; MAX_CAPTURE_LINES],
    lens:    [usize; MAX_CAPTURE_LINES],
    count:   usize,          // completed lines
    cur:     [u8; BUFFER_WIDTH],
    cur_len: usize,          // bytes in the current partial line
}

impl CaptureBuffer {
    const fn new() -> Self {
        CaptureBuffer {
            data:    [[0u8; BUFFER_WIDTH]; MAX_CAPTURE_LINES],
            lens:    [0usize; MAX_CAPTURE_LINES],
            count:   0,
            cur:     [0u8; BUFFER_WIDTH],
            cur_len: 0,
        }
    }

    fn reset(&mut self) {
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

    fn flush_partial(&mut self) {
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

static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE: IrqMutex<CaptureBuffer> = IrqMutex::new(CaptureBuffer::new());

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
    w.clear_row(BUFFER_HEIGHT - 1);
    w.column_position = 0;
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    if CAPTURE_ACTIVE.load(Ordering::Relaxed) {
        CAPTURE.lock().write_fmt(args).unwrap();
    } else {
        WRITER.lock().write_fmt(args).unwrap();
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

impl Color {
    /// Convert a `u8` (0–15) to a `Color`, returning `None` for out-of-range values.
    pub fn from_u8(val: u8) -> Option<Color> {
        match val {
            0  => Some(Color::Black),
            1  => Some(Color::Blue),
            2  => Some(Color::Green),
            3  => Some(Color::Cyan),
            4  => Some(Color::Red),
            5  => Some(Color::Magenta),
            6  => Some(Color::Brown),
            7  => Some(Color::LightGray),
            8  => Some(Color::DarkGray),
            9  => Some(Color::LightBlue),
            10 => Some(Color::LightGreen),
            11 => Some(Color::LightCyan),
            12 => Some(Color::LightRed),
            13 => Some(Color::Pink),
            14 => Some(Color::Yellow),
            15 => Some(Color::White),
            _  => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: ColorCode,
}

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

#[repr(transparent)]
struct Buffer {
    chars: [[Volatile<ScreenChar>; BUFFER_WIDTH]; BUFFER_HEIGHT],
}

/// Safe wrapper around the raw VGA MMIO buffer pointer.
///
/// # Safety contract (construction only)
/// The pointer passed to [`VgaBuffer::new`] must point to a valid, permanently
/// mapped VGA text-mode framebuffer that lives for the lifetime of the kernel.
/// All subsequent reads and writes go through `Volatile`, so individual
/// accesses do not require `unsafe`.
struct VgaBuffer {
    ptr: *mut Buffer,
}

impl VgaBuffer {
    /// Create a new `VgaBuffer`.
    ///
    /// # Safety
    /// `ptr` must point to a valid VGA text-mode buffer that is mapped for the
    /// entire lifetime of the kernel.  The caller must ensure exclusive access
    /// is serialised externally (e.g. via a `Mutex`).
    const unsafe fn new(ptr: *mut Buffer) -> Self {
        VgaBuffer { ptr }
    }

    /// Update the backing pointer (e.g. after remapping VGA to the high half).
    fn set_ptr(&mut self, ptr: *mut Buffer) {
        self.ptr = ptr;
    }

    fn read_cell(&self, row: usize, col: usize) -> ScreenChar {
        // Safety: the VgaBuffer invariant guarantees ptr is valid and the
        // Volatile wrapper ensures the compiler emits the load.
        unsafe { &*self.ptr }.chars[row][col].read()
    }

    fn write_cell(&mut self, row: usize, col: usize, ch: ScreenChar) {
        unsafe { &mut *self.ptr }.chars[row][col].write(ch);
    }

    /// Move the VGA hardware text cursor to `pos` (linear cell index: row*80+col).
    fn set_hw_cursor(&self, pos: u16) {
        use x86_64::instructions::port::Port;
        unsafe {
            let mut idx: Port<u8> = Port::new(0x3D4);
            let mut dat: Port<u8> = Port::new(0x3D5);
            idx.write(0x0E);
            dat.write((pos >> 8) as u8);
            idx.write(0x0F);
            dat.write(pos as u8);
        }
    }
}

// Safety: The raw pointer is always valid (either the bootloader identity map
// or the phys-mem-offset alias) and all access is serialised by the Mutex
// wrapping the Writer that owns this VgaBuffer.
unsafe impl Send for VgaBuffer {}

pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: VgaBuffer,
}

impl Writer {
    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // printable ASCII byte or newline
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                // not part of printable ASCII range
                _ => self.write_byte(0xfe),
            }

        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column_position >= BUFFER_WIDTH {
                    self.new_line();
                }

                let row = BUFFER_HEIGHT - 1;
                let col = self.column_position;

                let color_code = self.color_code;
                self.buffer.write_cell(row, col, ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
                let hw_col = self.column_position.min(BUFFER_WIDTH - 1);
                self.buffer.set_hw_cursor(((BUFFER_HEIGHT - 1) * BUFFER_WIDTH + hw_col) as u16);
            }
        }
    }

    fn new_line(&mut self) {
        // Rows 0 (status bar) and 1 (timeline) are fixed; scroll only rows 2..24.
        for row in 3..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.read_cell(row, col);
                self.buffer.write_cell(row - 1, col, character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
        self.buffer.set_hw_cursor(((BUFFER_HEIGHT - 1) * BUFFER_WIDTH) as u16);
    }

    /// Erase the last character on the current line (if any).
    fn do_backspace(&mut self) {
        if self.column_position > 0 {
            self.column_position -= 1;
            let row = BUFFER_HEIGHT - 1;
            let col = self.column_position;
            self.buffer.write_cell(row, col, ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            });
        }
    }

    /// Overwrite row 0 (status bar) with `data`, rendered white-on-blue.
    fn write_status_row(&mut self, data: &[u8; BUFFER_WIDTH]) {
        let color = ColorCode::new(Color::White, Color::Blue);
        for col in 0..BUFFER_WIDTH {
            self.buffer.write_cell(0, col, ScreenChar {
                ascii_character: data[col],
                color_code: color,
            });
        }
    }

    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..BUFFER_WIDTH {
            self.buffer.write_cell(row, col, blank);
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Status bar and timeline strip

/// A fixed-width byte buffer that implements `fmt::Write` without allocating.
struct FixedBuf {
    data: [u8; BUFFER_WIDTH],
    len: usize,
}

impl FixedBuf {
    fn new() -> Self {
        FixedBuf { data: [b' '; BUFFER_WIDTH], len: 0 }
    }
}

impl fmt::Write for FixedBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if self.len >= BUFFER_WIDTH { break; }
            self.data[self.len] = if (0x20..=0x7e).contains(&b) { b } else { b'?' };
            self.len += 1;
        }
        Ok(())
    }
}

/// Erase the last typed character on the current line.
/// No-op if the cursor is already at column 0.
pub fn backspace() {
    WRITER.lock().do_backspace();
}

/// Redraw the input region of the bottom row starting at `start_col`, filling
/// it with `buf[..len]` and blanking the rest.  Moves the hardware cursor to
/// `start_col + cursor`.
pub fn redraw_line(start_col: usize, buf: &[u8], len: usize, cursor: usize) {
    let mut w = WRITER.lock();
    let row = BUFFER_HEIGHT - 1;
    let color = w.color_code;
    for i in 0..(BUFFER_WIDTH - start_col) {
        let byte = if i < len { buf[i] } else { b' ' };
        w.buffer.write_cell(row, start_col + i, ScreenChar {
            ascii_character: byte,
            color_code: color,
        });
    }
    let new_col = (start_col + cursor).min(BUFFER_WIDTH - 1);
    w.column_position = new_col + 1;
    let hw_pos = (row * BUFFER_WIDTH + new_col) as u16;
    w.buffer.set_hw_cursor(hw_pos);
}

/// Clear all content rows (2–24), reset cursor to column 0.
pub fn clear_content() {
    let mut w = WRITER.lock();
    for row in 2..BUFFER_HEIGHT {
        w.clear_row(row);
    }
    w.column_position = 0;
}

/// Initialise rows 0 and 1: blue status bar and dark-grey timeline placeholder.
/// Call once before the first `println!` so the reserved rows look intentional.
pub fn init_display() {
    let mut w = WRITER.lock();
    let bar_color = ColorCode::new(Color::White, Color::Blue);
    for col in 0..BUFFER_WIDTH {
        w.buffer.write_cell(0, col, ScreenChar { ascii_character: b' ', color_code: bar_color });
    }
    let tl_color = ColorCode::new(Color::DarkGray, Color::Black);
    for col in 0..BUFFER_WIDTH {
        w.buffer.write_cell(1, col, ScreenChar { ascii_character: b' ', color_code: tl_color });
    }
}

/// Write a formatted message to the fixed status bar at row 0 (white-on-blue).
/// Safe to call from any kernel thread (acquires `WRITER` lock).
#[doc(hidden)]
pub fn print_status_bar(args: fmt::Arguments) {
    use core::fmt::Write;
    let mut buf = FixedBuf::new();
    let _ = buf.write_fmt(args);
    WRITER.lock().write_status_row(&buf.data);
}

/// Append a coloured block to the timeline strip at row 1, shifting old blocks
/// left.  Each call represents one context switch; colours cycle per thread.
///
/// # ISR Safety
/// Writes directly to VGA memory without acquiring any lock, so it is safe to
/// call from interrupt context (interrupts are already disabled by the CPU on
/// IDT dispatch).
pub fn timeline_append(thread_idx: usize) {
    const THREAD_BG: [Color; 6] = [
        Color::LightGreen, Color::LightCyan, Color::LightRed,
        Color::Pink,       Color::Yellow,    Color::LightGray,
    ];
    let bg = THREAD_BG[thread_idx % THREAD_BG.len()];
    let color_byte = ColorCode::new(Color::Black, bg).0;

    // VGA RAM: each cell is a u16 (low byte = character, high byte = colour).
    // Row 1 starts at offset BUFFER_WIDTH from the base address.
    let vga = VGA_BASE.load(Ordering::Relaxed) as *mut u16;
    let row1 = BUFFER_WIDTH; // offset in u16 units
    unsafe {
        // Shift the 80-column row left by one position.
        for col in 0..BUFFER_WIDTH - 1 {
            let val = core::ptr::read_volatile(vga.add(row1 + col + 1));
            core::ptr::write_volatile(vga.add(row1 + col), val);
        }
        // Append a space at the rightmost column; the background colour fills the cell.
        let entry: u16 = (color_byte as u16) << 8 | b' ' as u16;
        core::ptr::write_volatile(vga.add(row1 + BUFFER_WIDTH - 1), entry);
    }
}

#[cfg(test)]
mod test {
    use core::fmt::Write;
    use crate::{serial_print, serial_println};
    use super::{WRITER, BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, FixedBuf};

    #[test_case]
    fn test_println_simple() {
        serial_print!("test_println... ");
        println!("test_println_simple output");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_println_many() {
        serial_print!("test_println_many... ");
        for _ in 0..200 {
            println!("test_println_many output");
        }
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_println_output() {
        serial_print!("test_println_output... ");

        let s = "Some test string that fits on a single line";

        let mut writer = WRITER.lock();
        writeln!(writer, "\n{}", s).expect("writeln failed");
        for (i, c) in s.chars().enumerate() {
            let screen_char = writer.buffer.read_cell(BUFFER_HEIGHT - 2, i);
            assert_eq!(char::from(screen_char.ascii_character), c);
        }

        serial_println!("[ok]");
    }

    #[test_case]
    fn test_color_code_encoding() {
        serial_print!("test_color_code_encoding... ");
        // High nibble = background, low nibble = foreground.
        let cc = ColorCode::new(Color::White, Color::Blue);
        assert_eq!(cc.0, (Color::Blue as u8) << 4 | Color::White as u8);

        let cc = ColorCode::new(Color::Black, Color::Black);
        assert_eq!(cc.0, 0);

        let cc = ColorCode::new(Color::Yellow, Color::Black);
        assert_eq!(cc.0, Color::Yellow as u8);

        let cc = ColorCode::new(Color::Black, Color::White);
        assert_eq!(cc.0, (Color::White as u8) << 4);

        // All 16 foreground colours round-trip.
        for fg in 0u8..16 {
            for bg in 0u8..16 {
                let f = Color::from_u8(fg).expect("valid fg");
                let b = Color::from_u8(bg).expect("valid bg");
                let cc = ColorCode::new(f, b);
                assert_eq!(cc.0 >> 4, bg, "bg mismatch fg={fg} bg={bg}");
                assert_eq!(cc.0 & 0x0F, fg, "fg mismatch fg={fg} bg={bg}");
            }
        }
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_fixed_buf_write_printable() {
        serial_print!("test_fixed_buf_write_printable... ");
        let mut buf = FixedBuf::new();
        write!(buf, "hello").unwrap();
        assert_eq!(buf.len, 5);
        assert_eq!(&buf.data[..5], b"hello");
        // Remaining positions keep the initialised space fill.
        assert_eq!(buf.data[5], b' ');
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_fixed_buf_non_printable_replaced() {
        serial_print!("test_fixed_buf_non_printable_replaced... ");
        let mut buf = FixedBuf::new();
        write!(buf, "\x01\x1b\x7f").unwrap();
        assert_eq!(buf.len, 3);
        assert_eq!(&buf.data[..3], b"???");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_fixed_buf_truncates_at_buffer_width() {
        serial_print!("test_fixed_buf_truncates_at_buffer_width... ");
        let mut buf = FixedBuf::new();
        for _ in 0..BUFFER_WIDTH + 10 {
            write!(buf, "x").unwrap();
        }
        assert_eq!(buf.len, BUFFER_WIDTH);
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_fixed_buf_format_args() {
        serial_print!("test_fixed_buf_format_args... ");
        let mut buf = FixedBuf::new();
        write!(buf, "n={}", 42u32).unwrap();
        assert_eq!(&buf.data[..4], b"n=42");
        assert_eq!(buf.len, 4);
        serial_println!("[ok]");
    }
}