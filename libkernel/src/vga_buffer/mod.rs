use volatile::Volatile;
use core::fmt;
use core::sync::atomic::Ordering;
use lazy_static::lazy_static;
use crate::irq_mutex::IrqMutex;
use crate::framebuffer::Framebuffer;
use crate::font;

pub mod capture;
pub mod timeline;

// Re-export public items from submodules at the old paths.
pub use capture::{
    MAX_CAPTURE_LINES, capture_start, capture_end, capture_print_line, clear_current_line,
};
pub use timeline::{TimelineStream, timeline_append, timeline_flush_one};

// ---------------------------------------------------------------------------
// Text-mode constants (used during early boot and as defaults)

const BUFFER_HEIGHT: usize = 25;
const BUFFER_WIDTH: usize = 80;

// ---------------------------------------------------------------------------
// Framebuffer-mode constants

/// Text columns when using the graphical framebuffer (1024 / 8).
pub const FB_COLS: usize = crate::framebuffer::FB_WIDTH / font::FONT_WIDTH;
/// Text rows when using the graphical framebuffer (768 / 16).
pub const FB_ROWS: usize = crate::framebuffer::FB_HEIGHT / font::FONT_HEIGHT;

/// Maximum text width across both backends (used for formatting buffers).
const MAX_COLS: usize = FB_COLS; // 128 >= 80

/// VGA 16-colour palette mapped to 32-bit BGRA values (for BGA LFB which
/// uses BGRX byte order: blue in low byte, red in byte 2).
const VGA_PALETTE: [u32; 16] = [
    0x00000000, // Black
    0x00AA0000, // Blue
    0x0000AA00, // Green
    0x00AAAA00, // Cyan
    0x000000AA, // Red
    0x00AA00AA, // Magenta
    0x000055AA, // Brown
    0x00AAAAAA, // LightGray
    0x00555555, // DarkGray
    0x00FF5555, // LightBlue
    0x0055FF55, // LightGreen
    0x00FFFF55, // LightCyan
    0x005555FF, // LightRed
    0x00FF55FF, // Pink
    0x0055FFFF, // Yellow
    0x00FFFFFF, // White
];

// ---------------------------------------------------------------------------
// Display backend

/// Rendering backend: either legacy VGA text-mode MMIO or pixel framebuffer.
enum DisplayBackend {
    TextMode(VgaBuffer),
    Graphical {
        fb: Framebuffer,
        /// Shadow text buffer for scrolling / redrawing.
        /// Boxed to avoid putting 12 KiB on the 64 KiB kernel stack.
        cells: alloc::boxed::Box<[[ScreenChar; FB_COLS]; FB_ROWS]>,
    },
}

lazy_static! {
    pub static ref WRITER: IrqMutex<Writer> = IrqMutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        // Safety: 0xb8000 is the standard VGA text-mode buffer address,
        // identity-mapped by the bootloader at startup.
        backend: DisplayBackend::TextMode(unsafe { VgaBuffer::new(0xb8000 as *mut Buffer) }),
        cols: BUFFER_WIDTH,
        rows: BUFFER_HEIGHT,
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
    let mut w = WRITER.lock();
    if let DisplayBackend::TextMode(ref mut buf) = w.backend {
        buf.set_ptr(base as *mut Buffer);
    }
}

/// Snapshot the VGA text buffer (80×25) while still in text mode.
///
/// The BGA mode switch remaps VRAM, making the VGA buffer at 0xB8000
/// return garbage.  Call this *before* `bga_set_resolution()`.
///
/// The returned closure captures the snapshot and, when called with a
/// [`Framebuffer`], completes the backend switch.  This avoids exposing
/// the private `ScreenChar` type.
///
/// Both the snapshot and the shadow cell buffer are heap-allocated to
/// avoid overflowing the 64 KiB kernel stack.
pub fn snapshot_for_framebuffer() -> impl FnOnce(Framebuffer) {
    use alloc::boxed::Box;

    let w = WRITER.lock();
    let blank = ScreenChar {
        ascii_character: b' ',
        color_code: ColorCode::new(Color::Yellow, Color::Black),
    };
    let mut snap = Box::new([[blank; BUFFER_WIDTH]; BUFFER_HEIGHT]);
    if let DisplayBackend::TextMode(ref buf) = w.backend {
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                snap[row][col] = buf.read_cell(row, col);
            }
        }
    }
    drop(w);

    move |fb: Framebuffer| {
        let mut w = WRITER.lock();
        let mut cells = Box::new([[blank; FB_COLS]; FB_ROWS]);
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                cells[row][col] = snap[row][col];
            }
        }
        w.cols = FB_COLS;
        w.rows = FB_ROWS;
        w.backend = DisplayBackend::Graphical { fb, cells };
        w.repaint_all();
    }
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

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    if capture::CAPTURE_ACTIVE.load(Ordering::Relaxed) {
        capture::CAPTURE.lock().write_fmt(args).unwrap();
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
pub(crate) struct ColorCode(pub(crate) u8);

impl ColorCode {
    pub(crate) fn new(foreground: Color, background: Color) -> ColorCode {
        ColorCode((background as u8) << 4 | (foreground as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct ScreenChar {
    pub(crate) ascii_character: u8,
    pub(crate) color_code: ColorCode,
}

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
    pub(crate) column_position: usize,
    pub(crate) color_code: ColorCode,
    backend: DisplayBackend,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

impl Writer {
    // -----------------------------------------------------------------------
    // Backend-dispatch helpers

    pub(crate) fn read_cell(&self, row: usize, col: usize) -> ScreenChar {
        match &self.backend {
            DisplayBackend::TextMode(buf) => buf.read_cell(row, col),
            DisplayBackend::Graphical { cells, .. } => cells[row][col],
        }
    }

    pub(crate) fn write_cell(&mut self, row: usize, col: usize, ch: ScreenChar) {
        match &mut self.backend {
            DisplayBackend::TextMode(buf) => buf.write_cell(row, col, ch),
            DisplayBackend::Graphical { fb, cells } => {
                cells[row][col] = ch;
                let fg = VGA_PALETTE[(ch.color_code.0 & 0x0F) as usize];
                let bg = VGA_PALETTE[(ch.color_code.0 >> 4) as usize];
                font::draw_char(
                    fb,
                    ch.ascii_character,
                    col * font::FONT_WIDTH,
                    row * font::FONT_HEIGHT,
                    fg,
                    bg,
                );
            }
        }
    }

    fn set_cursor(&self, row: usize, col: usize) {
        if let DisplayBackend::TextMode(ref buf) = self.backend {
            buf.set_hw_cursor((row * BUFFER_WIDTH + col) as u16);
        }
        // In graphical mode: no hardware cursor (software cursor is a future enhancement).
    }

    /// Repaint the entire screen from the shadow cell buffer.
    fn repaint_all(&mut self) {
        if let DisplayBackend::Graphical { ref mut fb, ref cells } = self.backend {
            fb.clear(VGA_PALETTE[0]); // black
            for row in 0..self.rows {
                for col in 0..self.cols {
                    let ch = cells[row][col];
                    let fg = VGA_PALETTE[(ch.color_code.0 & 0x0F) as usize];
                    let bg = VGA_PALETTE[(ch.color_code.0 >> 4) as usize];
                    font::draw_char(
                        fb,
                        ch.ascii_character,
                        col * font::FONT_WIDTH,
                        row * font::FONT_HEIGHT,
                        fg,
                        bg,
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Text output

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                // printable ASCII byte or newline
                0x20..=0x7e | b'\n' | 0x08 => self.write_byte(byte),
                // not part of printable ASCII range
                _ => self.write_byte(0xfe),
            }

        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            0x08 => self.do_backspace(),
            byte => {
                if self.column_position >= self.cols {
                    self.new_line();
                }

                let row = self.rows - 1;
                let col = self.column_position;

                let color_code = self.color_code;
                self.write_cell(row, col, ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
                let hw_col = self.column_position.min(self.cols - 1);
                self.set_cursor(row, hw_col);
            }
        }
    }

    fn new_line(&mut self) {
        // Rows 0 (status bar) and 1 (timeline) are fixed; scroll only rows 2..last.
        // Fast path for graphical mode: shift pixel data then update shadow cells.
        if let DisplayBackend::Graphical { ref mut fb, ref mut cells } = self.backend {
            let src_y = 3 * font::FONT_HEIGHT;
            let dst_y = 2 * font::FONT_HEIGHT;
            let n_lines = (self.rows - 3) * font::FONT_HEIGHT;
            fb.scroll_up(dst_y, src_y, n_lines);

            for row in 3..self.rows {
                cells[row - 1] = cells[row];
            }
            // Clear the last row in the shadow buffer and on screen.
            let blank = ScreenChar {
                ascii_character: b' ',
                color_code: self.color_code,
            };
            let last = self.rows - 1;
            for col in 0..self.cols {
                cells[last][col] = blank;
            }
            let bg = VGA_PALETTE[(self.color_code.0 >> 4) as usize];
            fb.fill_rect(
                0,
                last * font::FONT_HEIGHT,
                self.cols * font::FONT_WIDTH,
                font::FONT_HEIGHT,
                bg,
            );
        } else {
            // Text-mode path
            for row in 3..self.rows {
                for col in 0..self.cols {
                    let character = self.read_cell(row, col);
                    self.write_cell(row - 1, col, character);
                }
            }
            self.clear_row(self.rows - 1);
        }

        self.column_position = 0;
        self.set_cursor(self.rows - 1, 0);
    }

    /// Move the cursor back one column (BS / 0x08).  Does not erase —
    /// the caller is expected to overwrite the cell explicitly (standard
    /// terminal semantics: `\b` is cursor-left, not erase).
    fn do_backspace(&mut self) {
        if self.column_position > 0 {
            self.column_position -= 1;
            let hw_col = self.column_position.min(self.cols - 1);
            self.set_cursor(self.rows - 1, hw_col);
        }
    }

    /// Overwrite row 0 (status bar) with `data`, rendered white-on-blue.
    fn write_status_row(&mut self, data: &[u8; MAX_COLS]) {
        let color = ColorCode::new(Color::White, Color::Blue);
        for col in 0..self.cols {
            self.write_cell(0, col, ScreenChar {
                ascii_character: data[col],
                color_code: color,
            });
        }
    }

    pub(crate) fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii_character: b' ',
            color_code: self.color_code,
        };
        for col in 0..self.cols {
            self.write_cell(row, col, blank);
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
    data: [u8; MAX_COLS],
    len: usize,
}

impl FixedBuf {
    fn new() -> Self {
        FixedBuf { data: [b' '; MAX_COLS], len: 0 }
    }
}

impl fmt::Write for FixedBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if self.len >= MAX_COLS { break; }
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
    let row = w.rows - 1;
    let cols = w.cols;
    let color = w.color_code;
    for i in 0..(cols - start_col) {
        let byte = if i < len { buf[i] } else { b' ' };
        w.write_cell(row, start_col + i, ScreenChar {
            ascii_character: byte,
            color_code: color,
        });
    }
    let new_col = (start_col + cursor).min(cols - 1);
    w.column_position = new_col + 1;
    w.set_cursor(row, new_col);
}

/// Clear all content rows (2–last), reset cursor to column 0.
pub fn clear_content() {
    let mut w = WRITER.lock();
    let rows = w.rows;
    for row in 2..rows {
        w.clear_row(row);
    }
    w.column_position = 0;
}

/// Initialise rows 0 and 1: blue status bar and dark-grey timeline placeholder.
/// Call once before the first `println!` so the reserved rows look intentional.
pub fn init_display() {
    let mut w = WRITER.lock();
    let cols = w.cols;
    let bar_color = ColorCode::new(Color::White, Color::Blue);
    for col in 0..cols {
        w.write_cell(0, col, ScreenChar { ascii_character: b' ', color_code: bar_color });
    }
    let tl_color = ColorCode::new(Color::DarkGray, Color::Black);
    for col in 0..cols {
        w.write_cell(1, col, ScreenChar { ascii_character: b' ', color_code: tl_color });
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

// ---------------------------------------------------------------------------
// Boot progress bar (row 2)

const PROGRESS_ROW: usize = 2;
const BAR_LEFT: usize = 2;   // " ["
const BAR_WIDTH: usize = 30;
const BAR_RIGHT: usize = BAR_LEFT + BAR_WIDTH; // "] "

/// Draw a boot progress bar on VGA row 2.
///
/// `step` is 1-based (1 = first step done).  The bar fills proportionally.
/// `label` describes the step just completed (or in progress).
/// Call with `step == total` on the final step; call [`boot_progress_done`]
/// afterwards to clear the row.
pub fn boot_progress(step: usize, total: usize, label: &str) {
    let mut w = WRITER.lock();
    let cols = w.cols;
    let label_start = BAR_RIGHT + 2;

    let filled = if total == 0 { 0 } else { (step * BAR_WIDTH) / total };
    let bar_fg = ColorCode::new(Color::LightGreen, Color::Black);
    let bar_bg = ColorCode::new(Color::DarkGray, Color::Black);
    let bracket_color = ColorCode::new(Color::White, Color::Black);
    let label_color = ColorCode::new(Color::LightGray, Color::Black);
    let blank = ScreenChar { ascii_character: b' ', color_code: label_color };

    // " ["
    w.write_cell(PROGRESS_ROW, 0, ScreenChar { ascii_character: b' ', color_code: bracket_color });
    w.write_cell(PROGRESS_ROW, 1, ScreenChar { ascii_character: b'[', color_code: bracket_color });

    // Bar: filled portion uses 0xDB (█), empty uses 0xB0 (░)
    for i in 0..BAR_WIDTH {
        let (ch, color) = if i < filled { (0xDB, bar_fg) } else { (0xB0, bar_bg) };
        w.write_cell(PROGRESS_ROW, BAR_LEFT + i, ScreenChar {
            ascii_character: ch,
            color_code: color,
        });
    }

    // "] "
    w.write_cell(PROGRESS_ROW, BAR_RIGHT, ScreenChar { ascii_character: b']', color_code: bracket_color });
    w.write_cell(PROGRESS_ROW, BAR_RIGHT + 1, ScreenChar { ascii_character: b' ', color_code: bracket_color });

    // Label text (pad/truncate to fill the rest of the row)
    let label_bytes = label.as_bytes();
    for col in label_start..cols {
        let i = col - label_start;
        if i < label_bytes.len() {
            let b = label_bytes[i];
            let ch = if (0x20..=0x7e).contains(&b) { b } else { b'?' };
            w.write_cell(PROGRESS_ROW, col, ScreenChar { ascii_character: ch, color_code: label_color });
        } else {
            w.write_cell(PROGRESS_ROW, col, blank);
        }
    }
}

/// Clear the boot progress row after init is complete.
pub fn boot_progress_done() {
    let mut w = WRITER.lock();
    let blank = ScreenChar {
        ascii_character: b' ',
        color_code: ColorCode::new(Color::Yellow, Color::Black),
    };
    let cols = w.cols;
    for col in 0..cols {
        w.write_cell(PROGRESS_ROW, col, blank);
    }
}

#[cfg(test)]
mod test {
    use core::fmt::Write;
    use crate::{serial_print, serial_println};
    use super::{WRITER, BUFFER_HEIGHT, BUFFER_WIDTH, Color, ColorCode, FixedBuf, MAX_COLS};

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
            let screen_char = writer.read_cell(BUFFER_HEIGHT - 2, i);
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
        for _ in 0..MAX_COLS + 10 {
            write!(buf, "x").unwrap();
        }
        assert_eq!(buf.len, MAX_COLS);
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
