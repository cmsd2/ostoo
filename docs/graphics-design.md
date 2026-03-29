# Graphics Subsystem Design

## Overview

The kernel migrates from VGA text mode (80x25) to a pixel framebuffer during
boot.  Two hardware paths are covered: the **Bochs Graphics Adapter (BGA)**
for the initial implementation, and **virtio-gpu** as future work.

After the switch, all existing output (`println!`, `status_bar!`, timeline
strip, boot progress bar) renders via an 8x16 bitmap font onto the
framebuffer.  The text grid expands from 80x25 to 128x48 characters at
1024x768 resolution.

## Architecture

```
  Early boot (text mode)          After PCI scan (graphical mode)
  ========================        ================================

  print!/status_bar!              print!/status_bar!
       |                               |
    Writer                          Writer
       |                               |
  DisplayBackend::TextMode        DisplayBackend::Graphical
       |                               |
  VgaBuffer (0xB8000 MMIO)       Framebuffer (BGA LFB MMIO)
                                       |
                                  font::draw_char() -> pixels
```

The `Writer` struct contains a `DisplayBackend` enum that dispatches all
cell reads/writes to either the legacy VGA text buffer or the pixel
framebuffer.  The switch happens once during boot after PCI enumeration
detects the BGA device.

## BGA (Bochs Graphics Adapter) -- Implemented

### Hardware Interface

The BGA device is QEMU's default VGA adapter on Q35 machines (`-vga std`).
It is controlled via two I/O ports:

| Port    | Direction | Description        |
|---------|-----------|--------------------|
| 0x01CE  | Write     | Register index     |
| 0x01CF  | R/W       | Register data      |

#### Register Map

| Index | Name         | Description                     |
|-------|--------------|---------------------------------|
| 0     | ID           | Version ID (0xB0C0..0xB0C5)    |
| 1     | XRES         | Horizontal resolution           |
| 2     | YRES         | Vertical resolution             |
| 3     | BPP          | Bits per pixel (8/15/16/24/32)  |
| 4     | ENABLE       | Display enable + LFB enable     |
| 5     | BANK         | VGA bank (legacy, not used)     |
| 6     | VIRT_WIDTH   | Virtual width (scrolling)       |
| 7     | VIRT_HEIGHT  | Virtual height (scrolling)      |
| 8     | X_OFFSET     | Display X offset                |
| 9     | Y_OFFSET     | Display Y offset                |

#### Mode Switch Sequence

1. Write `ENABLE = 0` (disable display)
2. Write `XRES = 1024`, `YRES = 768`, `BPP = 32`
3. Write `ENABLE = 0x01 | 0x20` (enabled + LFB enabled)

### Linear Framebuffer (LFB)

The LFB is located at **PCI BAR0** of the BGA device:

- **PCI Vendor**: 0x1234
- **PCI Device**: 0x1111
- **BAR0**: Physical base address of the LFB (typically 0xFD000000 on Q35)
- **Size**: `width * height * (bpp/8)` = 1024 * 768 * 4 = 3,145,728 bytes
- **Pixel format**: BGRX (blue in byte 0, green in byte 1, red in byte 2, byte 3 unused)

The kernel maps the LFB into the kernel MMIO virtual window (0xFFFF_8002_...)
using `map_mmio_region()`.  This region is present in all user page tables
(via shared PML4 entries 256-510).

### Software Text Rendering

Characters are rendered using an embedded 8x16 bitmap font (standard IBM VGA
ROM font, CP437 character set, 256 glyphs, 4096 bytes).

- **Text grid**: 128 columns x 48 rows (1024/8 x 768/16)
- **Font**: `libkernel/src/font.rs` -- `FONT_8X16` static array + `draw_char()`
- **Shadow buffer**: `[[ScreenChar; 128]; 48]` inside `DisplayBackend::Graphical`
  enables scrolling without reading back from MMIO

#### Color Mapping

The VGA 16-color palette is mapped to 32-bit BGRA values:

| VGA Color   | BGRA Value   |
|-------------|-------------|
| Black       | 0x00000000  |
| Blue        | 0x00AA0000  |
| Green       | 0x0000AA00  |
| ...         | ...         |
| White       | 0x00FFFFFF  |

#### Row Layout (preserved from text mode)

| Row(s)  | Purpose                          |
|---------|----------------------------------|
| 0       | Status bar (white on blue)       |
| 1       | Timeline strip (colored blocks)  |
| 2       | Boot progress bar (during init)  |
| 3-47    | Scrolling text output            |

### Scrolling

The graphical backend uses a fast scroll path:
1. `Framebuffer::scroll_up()` uses `core::ptr::copy()` to shift pixel data
   up by one character row (16 scanlines) in a single memcpy operation
2. The shadow `cells` array is shifted correspondingly
3. Only the new blank bottom row is cleared with `fill_rect()`

This avoids the naive approach of redrawing every character cell on scroll.

### Boot Sequence

1. Kernel boots in VGA text mode -- early `println!` works before PCI scan
2. After PCI scan: detect BGA via I/O port ID register
3. Find BGA PCI device, read BAR0 for LFB physical address
4. Map LFB into kernel virtual space
5. Call `bga_set_mode(1024, 768, 32)` to switch hardware
6. Call `switch_to_framebuffer()` -- copies current text content into shadow
   buffer, repaints entire screen
7. All subsequent output renders as pixels

If BGA is not detected (e.g. `-vga none`), the kernel stays in text mode.

### QEMU Configuration

Q35 machine includes stdvga (BGA-compatible) by default.  No changes to
`run.sh` required.  To be explicit: `-vga std`.

### Limitations

- No hardware cursor in graphical mode (software cursor is a future enhancement)
- LFB mapped with NO_CACHE (not write-combining); acceptable for text console
  but suboptimal for heavy graphics.  A future optimization would configure
  PAT for write-combining on the LFB region.
- Only works in QEMU/Bochs (BGA is not present on real hardware)

## VirtIO-GPU -- Future Work

### Motivation

- Standard virtio device, works with the `virtio-drivers` crate already in use
- Supports hardware-accelerated 2D operations (TRANSFER_TO_HOST_2D)
- Better fit for the existing virtio infrastructure (virtio-blk, virtio-9p)
- Portable across any hypervisor supporting virtio-gpu (not just QEMU)

### Hardware Interface

| Field         | Value                              |
|---------------|------------------------------------|
| PCI Vendor    | 0x1AF4                             |
| PCI Device    | 0x1050 (modern) / 0x1010 (legacy)  |
| Device class  | Display controller                 |
| Virtqueues    | controlq (commands), cursorq       |

### Command Protocol

Unlike BGA's simple I/O port registers, virtio-gpu uses a request/response
protocol over virtqueues:

1. **RESOURCE_CREATE_2D** -- allocate a 2D resource (the framebuffer)
2. **RESOURCE_ATTACH_BACKING** -- attach DMA pages as backing store
3. **SET_SCANOUT** -- assign the resource to a display output
4. **TRANSFER_TO_HOST_2D** -- copy dirty rectangles from guest to host
5. **RESOURCE_FLUSH** -- tell the host to display the updated region

### Design Sketch

- `VirtioGpuActor` following the existing `VirtioBlkActor` pattern
- Scanout = framebuffer resource backed by DMA pages from `alloc_dma_pages()`
- Periodic `TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH` to update display
- Dirty-rect tracking to minimize transfer size
- Could share the same `Framebuffer` abstraction used by BGA

### Why BGA First

- Simpler: I/O port registers + direct MMIO framebuffer writes
- No virtqueue setup, no command protocol
- QEMU Q35 has it by default (stdvga)
- Sufficient for a text console

## Key Files

| File | Description |
|------|-------------|
| `libkernel/src/framebuffer.rs` | BGA register access, Framebuffer struct |
| `libkernel/src/font.rs` | Embedded 8x16 bitmap font + draw_char() |
| `libkernel/src/vga_buffer/` | DisplayBackend abstraction, Writer refactoring (mod.rs, capture.rs, timeline.rs) |
| `kernel/src/main.rs` | init_bga_framebuffer() boot integration |

## Status

- [x] BGA detection and mode switching
- [x] Linear framebuffer mapping and pixel rendering
- [x] 8x16 bitmap font (full CP437 character set)
- [x] DisplayBackend abstraction with text-mode fallback
- [x] Fast pixel scrolling
- [x] Status bar, timeline, progress bar all work in graphical mode
- [ ] Software cursor (underline/block at cursor position)
- [ ] Write-combining for LFB pages (PAT configuration)
- [ ] Virtio-GPU backend
