//! Bochs Graphics Adapter (BGA) framebuffer support.
//!
//! Provides low-level BGA register access via I/O ports and a `Framebuffer`
//! struct for pixel-level rendering.  The BGA device is present in QEMU's
//! stdvga (Q35 default) and controlled entirely through I/O ports 0x01CE/0x01CF
//! — no PCI BAR access is needed for mode switching.
//!
//! PCI BAR0 reading (to locate the linear framebuffer physical address) is done
//! by the caller in `kernel/src/main.rs` via `devices::pci`, keeping `libkernel`
//! free of a `devices` dependency.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// LFB physical address (set during boot, read by framebuffer_open syscall)

static LFB_PHYS_ADDR: AtomicU64 = AtomicU64::new(0);
static LFB_PHYS_SIZE: AtomicU64 = AtomicU64::new(0);

/// Store the LFB physical address and size. Called once during BGA init.
pub fn set_lfb_phys(addr: u64, size: u64) {
    LFB_PHYS_ADDR.store(addr, Ordering::Release);
    LFB_PHYS_SIZE.store(size, Ordering::Release);
}

/// Retrieve the LFB physical address and size, or `None` if BGA not initialized.
pub fn get_lfb_phys() -> Option<(u64, u64)> {
    let addr = LFB_PHYS_ADDR.load(Ordering::Acquire);
    let size = LFB_PHYS_SIZE.load(Ordering::Acquire);
    if addr == 0 { None } else { Some((addr, size)) }
}

// ---------------------------------------------------------------------------
// BGA I/O ports

const VBE_DISPI_IOPORT_INDEX: u16 = 0x01CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x01CF;

// ---------------------------------------------------------------------------
// BGA register indices

const VBE_DISPI_INDEX_ID: u16 = 0;
const VBE_DISPI_INDEX_XRES: u16 = 1;
const VBE_DISPI_INDEX_YRES: u16 = 2;
const VBE_DISPI_INDEX_BPP: u16 = 3;
const VBE_DISPI_INDEX_ENABLE: u16 = 4;

// ---------------------------------------------------------------------------
// BGA enable flags

const VBE_DISPI_DISABLED: u16 = 0x00;
const VBE_DISPI_ENABLED: u16 = 0x01;
const VBE_DISPI_LFB_ENABLED: u16 = 0x20;

// ---------------------------------------------------------------------------
// Default mode

pub const FB_WIDTH: usize = 1024;
pub const FB_HEIGHT: usize = 768;
pub const FB_BPP: usize = 32;
pub const FB_STRIDE: usize = FB_WIDTH * (FB_BPP / 8);

// ---------------------------------------------------------------------------
// BGA register access

fn bga_write_register(index: u16, value: u16) {
    unsafe {
        Port::new(VBE_DISPI_IOPORT_INDEX).write(index);
        Port::new(VBE_DISPI_IOPORT_DATA).write(value);
    }
}

fn bga_read_register(index: u16) -> u16 {
    unsafe {
        Port::new(VBE_DISPI_IOPORT_INDEX).write(index);
        Port::new(VBE_DISPI_IOPORT_DATA).read()
    }
}

// ---------------------------------------------------------------------------
// Detection and mode-set

/// Check whether the BGA device is present by reading the ID register.
///
/// Valid BGA IDs are 0xB0C0 through 0xB0C5.
pub fn bga_is_present() -> bool {
    let id = bga_read_register(VBE_DISPI_INDEX_ID);
    (0xB0C0..=0xB0C5).contains(&id)
}

/// Disable the BGA display and set the resolution and bits-per-pixel.
///
/// The display remains disabled after this call so the caller can clear the
/// LFB before making it visible.  Call [`bga_enable`] to turn the display on.
pub fn bga_set_resolution(width: u16, height: u16, bpp: u16) {
    bga_write_register(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_DISABLED);
    bga_write_register(VBE_DISPI_INDEX_XRES, width);
    bga_write_register(VBE_DISPI_INDEX_YRES, height);
    bga_write_register(VBE_DISPI_INDEX_BPP, bpp);
}

/// Enable the BGA display with the linear framebuffer active.
///
/// Call after [`bga_set_resolution`] and after clearing the LFB to avoid
/// showing stale VRAM contents.
pub fn bga_enable() {
    bga_write_register(
        VBE_DISPI_INDEX_ENABLE,
        VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED,
    );
}

/// Read a PCI BAR value and determine the LFB physical address.
///
/// `bar0` is the raw 32-bit BAR0 value.  If bit 2 indicates a 64-bit BAR,
/// `bar1` supplies the upper 32 bits.
pub fn lfb_phys_from_bars(bar0: u32, bar1: u32) -> u64 {
    let is_64bit = (bar0 & 0x06) == 0x04;
    let low = (bar0 & 0xFFFF_FFF0) as u64;
    if is_64bit {
        low | ((bar1 as u64) << 32)
    } else {
        low
    }
}

// ---------------------------------------------------------------------------
// Framebuffer

/// Raw pixel framebuffer backed by the BGA linear framebuffer (LFB).
///
/// All pixel writes use volatile stores to ensure they reach the MMIO region.
pub struct Framebuffer {
    ptr: *mut u8,
    width: usize,
    height: usize,
    stride: usize, // bytes per scanline
}

unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// Create a new framebuffer.
    ///
    /// # Safety
    /// `ptr` must point to a valid, mapped framebuffer region of at least
    /// `height * stride` bytes that remains valid for the lifetime of this
    /// struct.
    pub unsafe fn new(ptr: *mut u8, width: usize, height: usize, stride: usize) -> Self {
        Framebuffer {
            ptr,
            width,
            height,
            stride,
        }
    }

    /// Write a single 32-bit BGRA pixel at (x, y).
    #[inline]
    pub fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            let offset = y * self.stride + x * 4;
            unsafe {
                (self.ptr.add(offset) as *mut u32).write_volatile(color);
            }
        }
    }

    /// Fill a rectangle with a solid color.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for row in y..y_end {
            for col in x..x_end {
                let offset = row * self.stride + col * 4;
                unsafe {
                    (self.ptr.add(offset) as *mut u32).write_volatile(color);
                }
            }
        }
    }

    /// Clear the entire screen to a solid color.
    pub fn clear(&mut self, color: u32) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Copy scanlines upward by `n_lines` pixel rows, starting from `src_y`.
    ///
    /// Used for fast scrolling: shifts pixel data up without redrawing each
    /// character individually.
    pub fn scroll_up(&mut self, dst_y: usize, src_y: usize, n_lines: usize) {
        if src_y >= self.height || dst_y >= self.height {
            return;
        }
        let lines = n_lines.min(self.height - src_y).min(self.height - dst_y);
        let bytes_per_line = self.width * 4;
        unsafe {
            core::ptr::copy(
                self.ptr.add(src_y * self.stride),
                self.ptr.add(dst_y * self.stride),
                lines * bytes_per_line,
            );
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn stride(&self) -> usize {
        self.stride
    }
    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }
}
