//! Embedded 8x16 VGA bitmap font (CP437) for title bar text rendering.
//!
//! Reuses the same font data as the terminal emulator.

pub const FONT_WIDTH: usize = 8;
pub const FONT_HEIGHT: usize = 16;

/// Render a single glyph into a raw BGRA pixel buffer.
#[inline]
pub fn draw_char(
    buf_ptr: *mut u8,
    stride: usize,
    buf_w: usize,
    buf_h: usize,
    ch: u8,
    px: usize,
    py: usize,
    fg: u32,
    bg: u32,
) {
    let base = (ch as usize) * FONT_HEIGHT;
    let fg_bytes = fg.to_le_bytes();
    let bg_bytes = bg.to_le_bytes();

    for row in 0..FONT_HEIGHT {
        let dy = py + row;
        if dy >= buf_h {
            break;
        }
        let bits = FONT_8X16[base + row];
        for col in 0..FONT_WIDTH {
            let dx = px + col;
            if dx >= buf_w {
                break;
            }
            let color = if bits & (0x80 >> col) != 0 {
                &fg_bytes
            } else {
                &bg_bytes
            };
            let off = dy * stride + dx * 4;
            unsafe {
                core::ptr::copy_nonoverlapping(color.as_ptr(), buf_ptr.add(off), 4);
            }
        }
    }
}

include!("../../term/src/font_data.rs");
