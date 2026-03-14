use volatile::Volatile;
use core::fmt;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    pub static ref WRITER: Mutex<Writer> = Mutex::new(Writer {
        column_position: 0,
        color_code: ColorCode::new(Color::Yellow, Color::Black),
        buffer: unsafe { &mut *(0xb8000 as *mut Buffer) },
    });
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
    WRITER.lock().write_fmt(args).unwrap();
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

pub struct Writer {
    column_position: usize,
    color_code: ColorCode,
    buffer: &'static mut Buffer,
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
                self.buffer.chars[row][col].write(ScreenChar {
                    ascii_character: byte,
                    color_code,
                });
                self.column_position += 1;
            }
        }
    }

    fn new_line(&mut self) {
        // Rows 0 (status bar) and 1 (timeline) are fixed; scroll only rows 2..24.
        for row in 3..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                let character = self.buffer.chars[row][col].read();
                self.buffer.chars[row - 1][col].write(character);
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column_position = 0;
    }

    /// Overwrite row 0 (status bar) with `data`, rendered white-on-blue.
    fn write_status_row(&mut self, data: &[u8; BUFFER_WIDTH]) {
        let color = ColorCode::new(Color::White, Color::Blue);
        for col in 0..BUFFER_WIDTH {
            self.buffer.chars[0][col].write(ScreenChar {
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
            self.buffer.chars[row][col].write(blank);
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

/// Initialise rows 0 and 1: blue status bar and dark-grey timeline placeholder.
/// Call once before the first `println!` so the reserved rows look intentional.
pub fn init_display() {
    let mut w = WRITER.lock();
    let bar_color = ColorCode::new(Color::White, Color::Blue);
    for col in 0..BUFFER_WIDTH {
        w.buffer.chars[0][col].write(ScreenChar { ascii_character: b' ', color_code: bar_color });
    }
    let tl_color = ColorCode::new(Color::DarkGray, Color::Black);
    for col in 0..BUFFER_WIDTH {
        w.buffer.chars[1][col].write(ScreenChar { ascii_character: b' ', color_code: tl_color });
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
    let vga = 0xb8000 as *mut u16;
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
    use super::{WRITER, BUFFER_HEIGHT};

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
            let screen_char = writer.buffer.chars[BUFFER_HEIGHT - 2][i].read();
            assert_eq!(char::from(screen_char.ascii_character), c);
        }

        serial_println!("[ok]");
    }
}