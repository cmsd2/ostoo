#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

mod font;

use alloc::vec;
use alloc::vec::Vec;
use ostoo_rt::compositor_proto::*;
use ostoo_rt::kbd_proto;
use ostoo_rt::ostoo::{
    self, unpack_mouse_event, FramebufferMem, IoRing, IpcRecv, IpcSend, IrqFd, NotifyFd, OsError,
    SharedMem,
};
use ostoo_rt::sys::{self, IoSubmission, IpcMessage};
use ostoo_rt::{eprintln, println};

// ── Constants ────────────────────────────────────────────────────────

const FB_WIDTH: usize = 1024;
const FB_HEIGHT: usize = 768;
const FB_STRIDE: usize = FB_WIDTH * 4;

const BG_COLOR: u32 = 0x00385070; // muted slate blue desktop

// Window decoration dimensions (CDE/Motif style)
const TITLE_H: usize = 24;
const BORDER_W: usize = 4;
const BEVEL: usize = 2; // 3D bevel thickness

// CDE palette (0x00RRGGBB — standard XRGB as u32 little-endian)
const CDE_BG: u32 = 0x007088A0;           // cool grey-blue base
const CDE_BG_LIGHT: u32 = 0x00A0B8C8;     // bevel highlight (top/left)
const CDE_BG_DARK: u32 = 0x00506070;      // bevel shadow (bottom/right)
const CDE_ACTIVE_TITLE: u32 = 0x004060B0;  // active title bar (blue)
const CDE_ACTIVE_LIGHT: u32 = 0x006888D0;  // active highlight
const CDE_ACTIVE_DARK: u32 = 0x00283880;   // active shadow
const CDE_INACTIVE_TITLE: u32 = CDE_BG;    // inactive = base grey-blue
const TITLE_TEXT: u32 = 0x00FFFFFF;         // white text
const TITLE_TEXT_INACTIVE: u32 = 0x00A0B8C8; // dimmed text

const MAX_WINDOWS: usize = 4;
// Completion user_data tag encoding:
const TAG_NEW_CLIENT: u64 = 0x1000;
const TAG_DAMAGE_BASE: u64 = 0x2000;
const TAG_CMD_BASE: u64 = 0x3000;
const TAG_KEYBOARD: u64 = 0x4000;
const TAG_MOUSE: u64 = 0x5000;

// CDE-style window button dimensions (square, inset in title bar)
const BTN_SIZE: usize = 18;
const BTN_MARGIN: usize = (TITLE_H - BTN_SIZE) / 2;

// Cursor bitmaps (16-bit wide rows, 1-bit per pixel)
const CURSOR_W: usize = 12;
const CURSOR_H: usize = 16;

#[derive(Clone, Copy, PartialEq)]
enum CursorStyle {
    Arrow,
    ResizeDiag,    // bottom-right / top-left diagonal
    ResizeHoriz,   // left-right horizontal
    ResizeVert,    // up-down vertical
}

// Arrow pointer
#[rustfmt::skip]
static CURSOR_ARROW: [u16; 16] = [
    0b1000_0000_0000_0000,
    0b1100_0000_0000_0000,
    0b1110_0000_0000_0000,
    0b1111_0000_0000_0000,
    0b1111_1000_0000_0000,
    0b1111_1100_0000_0000,
    0b1111_1110_0000_0000,
    0b1111_1111_0000_0000,
    0b1111_1111_1000_0000,
    0b1111_1100_0000_0000,
    0b1111_0000_0000_0000,
    0b1101_1000_0000_0000,
    0b1000_1100_0000_0000,
    0b0000_1100_0000_0000,
    0b0000_0110_0000_0000,
    0b0000_0000_0000_0000,
];

// Diagonal resize (NW-SE double arrow, 12x12 centered in 16-bit rows)
#[rustfmt::skip]
static CURSOR_RESIZE_DIAG: [u16; 16] = [
    0b1111_1000_0000_0000,
    0b1100_0000_0000_0000,
    0b1010_0000_0000_0000,
    0b1001_0000_0000_0000,
    0b1000_1000_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0010_0000_0000,
    0b0000_0001_0000_0000,
    0b0000_0000_1000_0000,
    0b0000_0100_0100_0000,
    0b0000_0010_0100_0000,
    0b0000_0001_0100_0000,
    0b0000_0000_1100_0000,
    0b0000_0011_1110_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
];

// Horizontal resize (left-right double arrow)
#[rustfmt::skip]
static CURSOR_RESIZE_HORIZ: [u16; 16] = [
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0001_0000_0100_0000,
    0b0011_0000_0110_0000,
    0b0111_1111_1110_0000,
    0b1111_1111_1111_0000,
    0b0111_1111_1110_0000,
    0b0011_0000_0110_0000,
    0b0001_0000_0100_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
    0b0000_0000_0000_0000,
];

// Vertical resize (up-down double arrow)
#[rustfmt::skip]
static CURSOR_RESIZE_VERT: [u16; 16] = [
    0b0000_0100_0000_0000,
    0b0000_1110_0000_0000,
    0b0001_1111_0000_0000,
    0b0011_1111_1000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0000_0100_0000_0000,
    0b0011_1111_1000_0000,
    0b0001_1111_0000_0000,
    0b0000_1110_0000_0000,
    0b0000_0100_0000_0000,
];

// ── Window state ─────────────────────────────────────────────────────

struct Window {
    id: u64,
    /// Top-left of the decorated frame (including title bar and borders).
    x: usize,
    y: usize,
    /// Client content dimensions (excluding decorations).
    w: usize,
    h: usize,
    /// Actual buffer dimensions — may differ from w/h during resize drag.
    buf_w: usize,
    buf_h: usize,
    buf_ptr: *const u8,
    buf_size: usize,
    dirty: bool,
    _buf: SharedMem,
    _notify: NotifyFd,
    _c2s_recv: IpcRecv,
    s2c_send: IpcSend,
    cmd_msg: IpcMessage,
}

impl Window {
    /// Full decorated width.
    fn dec_w(&self) -> usize {
        self.w + 2 * BORDER_W
    }
    /// Full decorated height.
    fn dec_h(&self) -> usize {
        self.h + TITLE_H + BORDER_W
    }
    /// X offset where client content starts in screen coords.
    fn content_x(&self) -> usize {
        self.x + BORDER_W
    }
    /// Y offset where client content starts in screen coords.
    fn content_y(&self) -> usize {
        self.y + TITLE_H
    }
}

// ── Hit testing ──────────────────────────────────────────────────────

const RESIZE_INNER: usize = 4;  // pixels inside the border that count as resize zone
const RESIZE_CORNER: usize = 16; // corner resize square size (from the outer edge)

#[derive(PartialEq)]
enum HitZone {
    TitleBar,
    CloseButton,
    Content,
    ResizeBottomRight,
    ResizeRight,
    ResizeBottom,
    None,
}

/// Hit-test mouse position against all windows (top to bottom z-order).
/// Windows at the end of the vec are on top (painter's algo).
fn hit_test(windows: &[Window], mx: usize, my: usize) -> (Option<usize>, HitZone) {
    for (idx, win) in windows.iter().enumerate().rev() {
        let x1 = win.x;
        let y1 = win.y;
        let x2 = x1 + win.dec_w();
        let y2 = y1 + win.dec_h();

        if mx >= x1 && mx < x2 && my >= y1 && my < y2 {
            // Resize zones: BORDER_W + RESIZE_INNER pixels from the outer edge
            // (i.e. the full border plus RESIZE_INNER pixels into the content).
            let edge_zone = BORDER_W + RESIZE_INNER;
            let near_right = mx + edge_zone >= x2;
            let near_bottom = my + edge_zone >= y2;
            // Corner uses a larger square measured from the outer edge.
            let corner_right = mx + RESIZE_CORNER >= x2;
            let corner_bottom = my + RESIZE_CORNER >= y2;
            if corner_right && corner_bottom {
                return (Some(idx), HitZone::ResizeBottomRight);
            }
            if near_right && my >= y1 + TITLE_H {
                return (Some(idx), HitZone::ResizeRight);
            }
            if near_bottom {
                return (Some(idx), HitZone::ResizeBottom);
            }

            // Close button: right side of title bar (CDE-style square button).
            let close_x1 = x2 - BORDER_W - BTN_MARGIN - BTN_SIZE;
            let close_y1 = y1 + BTN_MARGIN;
            if mx >= close_x1 && mx < close_x1 + BTN_SIZE
                && my >= close_y1 && my < close_y1 + BTN_SIZE
            {
                return (Some(idx), HitZone::CloseButton);
            }

            // Title bar (excluding close button).
            if my < y1 + TITLE_H {
                return (Some(idx), HitZone::TitleBar);
            }

            // Client content area.
            return (Some(idx), HitZone::Content);
        }
    }
    (None, HitZone::None)
}

// ── Drag state ───────────────────────────────────────────────────────

const MIN_WIN_W: usize = 128;
const MIN_WIN_H: usize = 64;

enum DragMode {
    None,
    Move {
        wid: u64,
        off_x: i32,
        off_y: i32,
    },
    Resize {
        wid: u64,
        start_w: usize,
        start_h: usize,
        start_mx: usize,
        start_my: usize,
        resize_x: bool,
        resize_y: bool,
    },
}

// ── Main ─────────────────────────────────────────────────────────────

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("compositor: fatal error (errno {})", e.errno());
            1
        }
    }
}

fn run() -> Result<(), OsError> {
    println!("compositor: starting");

    // 1. Open the framebuffer.
    let fb_mem = FramebufferMem::open()?;
    let fb_ptr = fb_mem.mmap()?;
    println!("compositor: framebuffer {}x{}", FB_WIDTH, FB_HEIGHT);

    // Allocate back buffer for flicker-free compositing.
    let mut back_buf = vec![0u8; FB_WIDTH * FB_HEIGHT * 4];
    let back_ptr = back_buf.as_mut_ptr();

    // Clear to background color.
    fill_rect(fb_ptr, 0, 0, FB_WIDTH, FB_HEIGHT, BG_COLOR);

    // 2. Create registration channel + IO ring.
    let (reg_send, reg_recv) = ostoo::ipc_channel(4, 0)?;
    let ring = IoRing::new(16, 32)?;

    // 3. Register the compositor service.
    ostoo::service_register(SERVICE_NAME, reg_send.fd())?;
    println!("compositor: registered as 'compositor'");

    // 4. Connect to keyboard service (if available).
    let mut kbd_msg = IpcMessage::default();
    let _kbd_recv: Option<IpcRecv> = match connect_keyboard(&ring, &mut kbd_msg) {
        Ok(recv) => Some(recv),
        Err(_) => {
            println!("compositor: keyboard service not found, no key input");
            None
        }
    };

    // 5. Claim mouse IRQ directly (no separate mouse driver).
    let mouse_irq = match IrqFd::new(12) {
        Ok(irq) => {
            ring.submit(&[IoSubmission::irq_wait(TAG_MOUSE, irq.fd())])?;
            println!("compositor: claimed mouse IRQ 12");
            Some(irq)
        }
        Err(_) => {
            println!("compositor: failed to claim mouse IRQ, no mouse input");
            None
        }
    };
    // 6. Arm OP_IPC_RECV on the registration channel.
    let mut reg_msg = IpcMessage::default();
    ring.submit(&[IoSubmission::ipc_recv(
        TAG_NEW_CLIENT,
        reg_recv.fd(),
        &mut reg_msg,
    )])?;

    // 7. State.
    let mut windows: Vec<Window> = Vec::new();
    let mut next_wid: u64 = 1;
    // Mouse & focus state.
    let mut cursor_x: usize = FB_WIDTH / 2;
    let mut cursor_y: usize = FB_HEIGHT / 2;
    let mut prev_cursor_x: usize = cursor_x;
    let mut prev_cursor_y: usize = cursor_y;
    let mut buttons: u8 = 0;
    let mut focused_wid: Option<u64> = None;
    let mut drag = DragMode::None;
    let mut cursor_style = CursorStyle::Arrow;

    // Initial paint.
    composite(fb_ptr, back_ptr, &windows, focused_wid);
    draw_cursor(fb_ptr, cursor_x, cursor_y, cursor_style);

    // 8. Event loop.
    loop {
        ring.enter(0, 1)?;
        let mut scene_dirty = false;
        let mut cursor_moved = false;

        while let Some(cqe) = ring.pop_cqe() {
            let ud = cqe.user_data;

            if ud == TAG_NEW_CLIENT {
                if cqe.result >= 0 {
                    handle_connect(
                        &ring,
                        &reg_msg,
                        &mut windows,
                        &mut next_wid,
                        &mut focused_wid,
                    );
                    scene_dirty = true;
                }
                reg_msg = IpcMessage::default();
                ring.submit(&[IoSubmission::ipc_recv(
                    TAG_NEW_CLIENT,
                    reg_recv.fd(),
                    &mut reg_msg,
                )])?;
            } else if ud >= TAG_DAMAGE_BASE && ud < TAG_CMD_BASE {
                let wid = ud - TAG_DAMAGE_BASE;
                if let Some(win) = windows.iter_mut().find(|w| w.id == wid) {
                    win.dirty = true;
                    scene_dirty = true;
                    ring.submit(&[IoSubmission::ring_wait(
                        TAG_DAMAGE_BASE + wid,
                        win._notify.fd(),
                    )])?;
                }
            } else if ud == TAG_KEYBOARD {
                if cqe.result >= 0 && kbd_msg.tag == kbd_proto::MSG_KB_KEY {
                    let byte = kbd_msg.data[0];
                    let modifiers = kbd_msg.data[1];
                    let key_type = kbd_msg.data[2];
                    // Forward to focused window.
                    if let Some(fwid) = focused_wid {
                        if let Some(win) = windows.iter().find(|w| w.id == fwid) {
                            let key_msg = IpcMessage {
                                tag: MSG_KEY_EVENT,
                                data: [byte, modifiers, key_type],
                                fds: [-1; 4],
                            };
                            let _ = sys::ipc_send(
                                win.s2c_send.fd(),
                                &key_msg,
                                sys::IPC_NONBLOCK,
                            );
                        }
                    }
                }
                kbd_msg = IpcMessage::default();
                if let Some(ref recv) = _kbd_recv {
                    ring.submit(&[IoSubmission::ipc_recv(
                        TAG_KEYBOARD,
                        recv.fd(),
                        &mut kbd_msg,
                    )])?;
                }
            } else if ud == TAG_MOUSE {
                if cqe.result >= 0 {
                    // Kernel delivers decoded mouse events (dx, dy, buttons).
                    let event = unpack_mouse_event(cqe.result);

                    cursor_x = (cursor_x as i32 + event.dx)
                        .max(0).min(FB_WIDTH as i32 - 1) as usize;
                    cursor_y = (cursor_y as i32 + event.dy)
                        .max(0).min(FB_HEIGHT as i32 - 1) as usize;
                    let new_buttons = event.buttons;

                    let left_down = new_buttons & 1 != 0 && buttons & 1 == 0;
                    let left_up = new_buttons & 1 == 0 && buttons & 1 != 0;

                    buttons = new_buttons;
                    cursor_moved = true;

                    // Handle drag.
                    match &drag {
                        DragMode::Move { wid, off_x, off_y } => {
                            if left_up {
                                drag = DragMode::None;
                            } else {
                                let wid = *wid;
                                let off_x = *off_x;
                                let off_y = *off_y;
                                if let Some(win) =
                                    windows.iter_mut().find(|w| w.id == wid)
                                {
                                    let new_x =
                                        (cursor_x as i32 + off_x).max(0) as usize;
                                    let new_y =
                                        (cursor_y as i32 + off_y).max(0) as usize;
                                    win.x = new_x;
                                    win.y = new_y;
                                }
                            }
                            scene_dirty = true;
                        }
                        DragMode::Resize {
                            wid,
                            start_w,
                            start_h,
                            start_mx,
                            start_my,
                            resize_x,
                            resize_y,
                        } => {
                            let wid = *wid;
                            let start_w = *start_w;
                            let start_h = *start_h;
                            let start_mx = *start_mx;
                            let start_my = *start_my;
                            let rx = *resize_x;
                            let ry = *resize_y;

                            if left_up {
                                // Finalize resize: allocate new buffer.
                                if let Some(win) =
                                    windows.iter_mut().find(|w| w.id == wid)
                                {
                                    let new_w = if rx {
                                        ((start_w as i32
                                            + cursor_x as i32
                                            - start_mx as i32)
                                            .max(MIN_WIN_W as i32)
                                            as usize)
                                            .min(FB_WIDTH)
                                    } else {
                                        win.w
                                    };
                                    let new_h = if ry {
                                        ((start_h as i32
                                            + cursor_y as i32
                                            - start_my as i32)
                                            .max(MIN_WIN_H as i32)
                                            as usize)
                                            .min(FB_HEIGHT)
                                    } else {
                                        win.h
                                    };

                                    if new_w != win.buf_w || new_h != win.buf_h {
                                        finalize_resize(win, new_w, new_h);
                                    } else {
                                        // Drag ended at original size — reset visual w/h.
                                        win.w = win.buf_w;
                                        win.h = win.buf_h;
                                    }
                                }
                                drag = DragMode::None;
                            } else if let Some(win) =
                                windows.iter_mut().find(|w| w.id == wid)
                            {
                                // Live preview: update visual size during drag.
                                if rx {
                                    let nw = (start_w as i32
                                        + cursor_x as i32
                                        - start_mx as i32)
                                        .max(MIN_WIN_W as i32)
                                        as usize;
                                    win.w = nw.min(FB_WIDTH);
                                }
                                if ry {
                                    let nh = (start_h as i32
                                        + cursor_y as i32
                                        - start_my as i32)
                                        .max(MIN_WIN_H as i32)
                                        as usize;
                                    win.h = nh.min(FB_HEIGHT);
                                }
                            }
                            scene_dirty = true;
                        }
                        DragMode::None => {
                            let (hit_idx, zone) =
                                hit_test(&windows, cursor_x, cursor_y);

                            // Update cursor style based on hover zone.
                            cursor_style = match zone {
                                HitZone::ResizeBottomRight => CursorStyle::ResizeDiag,
                                HitZone::ResizeRight => CursorStyle::ResizeHoriz,
                                HitZone::ResizeBottom => CursorStyle::ResizeVert,
                                _ => CursorStyle::Arrow,
                            };

                            if left_down {
                                if let Some(idx) = hit_idx {
                                    let wid = windows[idx].id;
                                    // Focus + raise to top.
                                    focused_wid = Some(wid);
                                    let win = windows.remove(idx);
                                    windows.push(win);

                                    match zone {
                                        HitZone::TitleBar => {
                                            let win = windows.last().unwrap();
                                            let off_x =
                                                win.x as i32 - cursor_x as i32;
                                            let off_y =
                                                win.y as i32 - cursor_y as i32;
                                            drag = DragMode::Move {
                                                wid,
                                                off_x,
                                                off_y,
                                            };
                                        }
                                        HitZone::CloseButton => {
                                            println!(
                                                "compositor: window {} closed",
                                                wid
                                            );
                                            windows.retain(|w| w.id != wid);
                                            if focused_wid == Some(wid) {
                                                focused_wid =
                                                    windows.last().map(|w| w.id);
                                            }
                                        }
                                        HitZone::ResizeBottomRight
                                        | HitZone::ResizeRight
                                        | HitZone::ResizeBottom => {
                                            let win = windows.last().unwrap();
                                            drag = DragMode::Resize {
                                                wid,
                                                start_w: win.w,
                                                start_h: win.h,
                                                start_mx: cursor_x,
                                                start_my: cursor_y,
                                                resize_x: zone
                                                    != HitZone::ResizeBottom,
                                                resize_y: zone
                                                    != HitZone::ResizeRight,
                                            };
                                        }
                                        HitZone::Content => {
                                            // Focus only, no drag.
                                        }
                                        HitZone::None => {}
                                    }
                                    scene_dirty = true;
                                }
                            }
                        }
                    }
                }
                // Re-arm IRQ wait.
                if let Some(ref irq) = mouse_irq {
                    ring.submit(&[IoSubmission::irq_wait(TAG_MOUSE, irq.fd())])?;
                }
            } else if ud >= TAG_CMD_BASE {
                let wid = ud - TAG_CMD_BASE;
                let tag = windows
                    .iter()
                    .find(|w| w.id == wid)
                    .map(|w| w.cmd_msg.tag);
                if let Some(tag) = tag {
                    if cqe.result >= 0 {
                        match tag {
                            MSG_PRESENT => {
                                if let Some(w) =
                                    windows.iter_mut().find(|w| w.id == wid)
                                {
                                    w.dirty = true;
                                    scene_dirty = true;
                                }
                            }
                            MSG_CLOSE => {
                                println!("compositor: window {} closed", wid);
                                windows.retain(|w| w.id != wid);
                                if focused_wid == Some(wid) {
                                    focused_wid = windows.last().map(|w| w.id);
                                }
                                scene_dirty = true;
                            }
                            _ => {}
                        }
                    }
                    if let Some(win) = windows.iter_mut().find(|w| w.id == wid) {
                        win.cmd_msg = IpcMessage::default();
                        ring.submit(&[IoSubmission::ipc_recv(
                            TAG_CMD_BASE + wid,
                            win._c2s_recv.fd(),
                            &mut win.cmd_msg,
                        )])?;
                    }
                }
            }
        }

        if scene_dirty {
            // Full recomposite: redraw scene to back buffer, copy to LFB, draw cursor.
            composite(fb_ptr, back_ptr, &windows, focused_wid);
            draw_cursor(fb_ptr, cursor_x, cursor_y, cursor_style);
            prev_cursor_x = cursor_x;
            prev_cursor_y = cursor_y;
        } else if cursor_moved {
            // Cursor-only: restore old cursor rect from back buffer, draw at new pos.
            restore_rect(fb_ptr, back_ptr, prev_cursor_x, prev_cursor_y, CURSOR_W, CURSOR_H);
            draw_cursor(fb_ptr, cursor_x, cursor_y, cursor_style);
            prev_cursor_x = cursor_x;
            prev_cursor_y = cursor_y;
        }
    }
}

// ── Resize ───────────────────────────────────────────────────────────

fn finalize_resize(win: &mut Window, new_w: usize, new_h: usize) {
    let new_size = new_w * new_h * 4;
    let new_buf = match SharedMem::new(new_size, 0) {
        Ok(b) => b,
        Err(_) => return,
    };
    let new_ptr = match new_buf.mmap() {
        Ok(p) => p,
        Err(_) => return,
    };

    // Send MSG_WINDOW_RESIZED to client with new buffer fd.
    let msg = IpcMessage {
        tag: MSG_WINDOW_RESIZED,
        data: [new_w as u64, new_h as u64, 0],
        fds: [new_buf.fd(), -1, -1, -1],
    };
    let _ = sys::ipc_send(win.s2c_send.fd(), &msg, 0);

    // Update window state.
    win.w = new_w;
    win.h = new_h;
    win.buf_w = new_w;
    win.buf_h = new_h;
    win.buf_ptr = new_ptr as *const u8;
    win.buf_size = new_size;
    win._buf = new_buf;
    win.dirty = true;
}

// ── Connection handling ──────────────────────────────────────────────

fn handle_connect(
    ring: &IoRing,
    msg: &IpcMessage,
    windows: &mut Vec<Window>,
    next_wid: &mut u64,
    focused_wid: &mut Option<u64>,
) {
    if msg.tag != MSG_CONNECT {
        eprintln!("compositor: unexpected tag {} on reg channel", msg.tag);
        return;
    }

    if windows.len() >= MAX_WINDOWS {
        eprintln!("compositor: max windows reached, rejecting client");
        return;
    }

    let req_w = msg.data[0] as usize;
    let req_h = msg.data[1] as usize;
    let c2s_recv_fd = msg.fds[0];
    let s2c_send_fd = msg.fds[1];

    let w = req_w.min(FB_WIDTH).max(64);
    let h = req_h.min(FB_HEIGHT).max(64);

    let wid = *next_wid;
    *next_wid += 1;

    let buf_size = w * h * 4;
    let buf = match SharedMem::new(buf_size, 0) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("compositor: shmem alloc failed (errno {})", e.errno());
            return;
        }
    };
    let notify = match NotifyFd::new(0) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("compositor: notify alloc failed (errno {})", e.errno());
            return;
        }
    };

    let buf_ptr = match buf.mmap() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("compositor: mmap failed (errno {})", e.errno());
            return;
        }
    };

    let reply = IpcMessage {
        tag: MSG_WINDOW_CREATED,
        data: [wid, w as u64, h as u64],
        fds: [buf.fd(), notify.fd(), -1, -1],
    };
    if sys::ipc_send(s2c_send_fd, &reply, 0) < 0 {
        eprintln!("compositor: failed to send WINDOW_CREATED");
        return;
    }

    // Position: tile with room for decorations.
    let idx = windows.len();
    let (x, y) = tile_position(idx);

    println!(
        "compositor: window {} created ({}x{} at {},{})",
        wid, w, h, x, y
    );

    let c2s_recv = IpcRecv::from_raw_fd(c2s_recv_fd);
    let s2c_send = IpcSend::from_raw_fd(s2c_send_fd);

    let mut cmd_msg = IpcMessage::default();

    let _ = ring.submit(&[IoSubmission::ring_wait(
        TAG_DAMAGE_BASE + wid,
        notify.fd(),
    )]);
    let _ = ring.submit(&[IoSubmission::ipc_recv(
        TAG_CMD_BASE + wid,
        c2s_recv.fd(),
        &mut cmd_msg,
    )]);

    windows.push(Window {
        id: wid,
        x,
        y,
        w,
        h,
        buf_w: w,
        buf_h: h,
        buf_ptr: buf_ptr as *const u8,
        buf_size,
        dirty: false,
        _buf: buf,
        _notify: notify,
        _c2s_recv: c2s_recv,
        s2c_send,
        cmd_msg,
    });

    // Auto-focus the new window.
    *focused_wid = Some(wid);
}

// ── Compositing ──────────────────────────────────────────────────────

/// Recomposite the scene (without cursor) into back_buf, then copy to LFB.
/// Caller draws the cursor on LFB afterwards.
fn composite(
    fb_ptr: *mut u8,
    back_ptr: *mut u8,
    windows: &[Window],
    focused_wid: Option<u64>,
) {
    fill_rect(back_ptr, 0, 0, FB_WIDTH, FB_HEIGHT, BG_COLOR);

    for win in windows.iter() {
        draw_decorations(back_ptr, win, focused_wid == Some(win.id));
        blit_window(back_ptr, win);
    }

    let total = FB_WIDTH * FB_HEIGHT * 4;
    unsafe {
        core::ptr::copy_nonoverlapping(back_ptr, fb_ptr, total);
    }
}

/// Restore a rectangle on the LFB from the back buffer (cursor erase).
fn restore_rect(fb_ptr: *mut u8, back_ptr: *mut u8, x: usize, y: usize, w: usize, h: usize) {
    for row in y..(y + h).min(FB_HEIGHT) {
        let x_end = (x + w).min(FB_WIDTH);
        if x >= x_end {
            continue;
        }
        let off = row * FB_STRIDE + x * 4;
        let bytes = (x_end - x) * 4;
        unsafe {
            core::ptr::copy_nonoverlapping(back_ptr.add(off), fb_ptr.add(off), bytes);
        }
    }
}

fn draw_decorations(fb_ptr: *mut u8, win: &Window, focused: bool) {
    let x = win.x;
    let y = win.y;
    let dw = win.dec_w();
    let dh = win.dec_h();

    let (base, light, dark) = if focused {
        (CDE_ACTIVE_TITLE, CDE_ACTIVE_LIGHT, CDE_ACTIVE_DARK)
    } else {
        (CDE_INACTIVE_TITLE, CDE_BG_LIGHT, CDE_BG_DARK)
    };
    let text_color = if focused { TITLE_TEXT } else { TITLE_TEXT_INACTIVE };

    // Fill entire decoration area with base color.
    fill_rect(fb_ptr, x, y, dw, dh, base);

    // Outer bevel: light top/left, dark bottom/right.
    draw_bevel(fb_ptr, x, y, dw, dh, BEVEL, light, dark);

    // Inner bevel around client area (sunken).
    let cx = x + BORDER_W;
    let cy = y + TITLE_H;
    let cw = dw - 2 * BORDER_W;
    let ch = dh - TITLE_H - BORDER_W;
    draw_bevel(fb_ptr, cx - 1, cy - 1, cw + 2, ch + 2, 1, dark, light);

    // Close button (right side, CDE-style raised square).
    let btn_x = x + dw - BORDER_W - BTN_MARGIN - BTN_SIZE;
    let btn_y = y + BTN_MARGIN;
    fill_rect(fb_ptr, btn_x, btn_y, BTN_SIZE, BTN_SIZE, base);
    draw_bevel(fb_ptr, btn_x, btn_y, BTN_SIZE, BTN_SIZE, 1, light, dark);
    // Small inner square (CDE close button motif).
    let inner = 4;
    let ix = btn_x + (BTN_SIZE - inner * 2) / 2;
    let iy = btn_y + (BTN_SIZE - inner * 2) / 2;
    fill_rect(fb_ptr, ix, iy, inner * 2, inner * 2, dark);
    draw_bevel(fb_ptr, ix, iy, inner * 2, inner * 2, 1, light, dark);

    // Window title text — centered in title bar.
    let title = window_title(win.id);
    let mut title_len = 0;
    for &ch in title.iter() {
        if ch == 0 { break; }
        title_len += 1;
    }
    let text_w = title_len * font::FONT_WIDTH;
    let title_x = x + (dw - text_w) / 2;
    let title_y = y + (TITLE_H - font::FONT_HEIGHT) / 2;
    for (i, &ch) in title.iter().enumerate() {
        if ch == 0 { break; }
        let px = title_x + i * font::FONT_WIDTH;
        if px + font::FONT_WIDTH > btn_x {
            break;
        }
        font::draw_char(fb_ptr, FB_STRIDE, FB_WIDTH, FB_HEIGHT, ch, px, title_y, text_color, base);
    }
}

/// Draw a 3D bevel (raised): light on top/left edges, dark on bottom/right.
fn draw_bevel(fb_ptr: *mut u8, x: usize, y: usize, w: usize, h: usize, thickness: usize, light: u32, dark: u32) {
    // Top edge (light).
    for t in 0..thickness {
        fill_rect(fb_ptr, x + t, y + t, w - 2 * t, 1, light);
    }
    // Left edge (light).
    for t in 0..thickness {
        fill_rect(fb_ptr, x + t, y + t, 1, h - 2 * t, light);
    }
    // Bottom edge (dark).
    for t in 0..thickness {
        fill_rect(fb_ptr, x + t, y + h - 1 - t, w - 2 * t, 1, dark);
    }
    // Right edge (dark).
    for t in 0..thickness {
        fill_rect(fb_ptr, x + w - 1 - t, y + t, 1, h - 2 * t, dark);
    }
}

/// Format "Win N" into a fixed buffer. N can be up to ~20 digits.
fn window_title(id: u64) -> [u8; 24] {
    let mut buf = [0u8; 24];
    buf[0] = b'W';
    buf[1] = b'i';
    buf[2] = b'n';
    buf[3] = b' ';
    // Convert id to decimal.
    if id == 0 {
        buf[4] = b'0';
    } else {
        let mut tmp = [0u8; 20];
        let mut n = id;
        let mut pos = 0;
        while n > 0 {
            tmp[pos] = b'0' + (n % 10) as u8;
            n /= 10;
            pos += 1;
        }
        for i in 0..pos {
            buf[4 + i] = tmp[pos - 1 - i];
        }
    }
    buf
}

fn blit_window(fb_ptr: *mut u8, win: &Window) {
    let src = win.buf_ptr;
    let src_stride = win.buf_w * 4; // use actual buffer width, not visual width
    let dst_x = win.content_x();
    let dst_y_start = win.content_y();

    // Blit rows up to min(visual height, buffer height).
    let blit_h = win.h.min(win.buf_h);
    let blit_w = win.w.min(win.buf_w);

    for row in 0..blit_h {
        let dst_y = dst_y_start + row;
        if dst_y >= FB_HEIGHT {
            break;
        }
        let copy_w = blit_w.min(FB_WIDTH.saturating_sub(dst_x));
        if copy_w == 0 {
            continue;
        }

        let src_off = row * src_stride;
        let dst_off = dst_y * FB_STRIDE + dst_x * 4;
        let bytes = copy_w * 4;

        unsafe {
            core::ptr::copy_nonoverlapping(src.add(src_off), fb_ptr.add(dst_off), bytes);
        }
    }
}

fn fill_rect(fb_ptr: *mut u8, x: usize, y: usize, w: usize, h: usize, color: u32) {
    let bytes = color.to_le_bytes();
    for row in y..(y + h).min(FB_HEIGHT) {
        for col in x..(x + w).min(FB_WIDTH) {
            let off = row * FB_STRIDE + col * 4;
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), fb_ptr.add(off), 4);
            }
        }
    }
}

fn draw_cursor(fb_ptr: *mut u8, cx: usize, cy: usize, style: CursorStyle) {
    let bitmap = match style {
        CursorStyle::Arrow => &CURSOR_ARROW,
        CursorStyle::ResizeDiag => &CURSOR_RESIZE_DIAG,
        CursorStyle::ResizeHoriz => &CURSOR_RESIZE_HORIZ,
        CursorStyle::ResizeVert => &CURSOR_RESIZE_VERT,
    };
    let white = 0x00FFFFFFu32.to_le_bytes();
    let black = 0x00000000u32.to_le_bytes();

    for row in 0..CURSOR_H {
        let dy = cy + row;
        if dy >= FB_HEIGHT {
            break;
        }
        let bits = bitmap[row];
        for col in 0..CURSOR_W {
            let dx = cx + col;
            if dx >= FB_WIDTH {
                break;
            }
            if bits & (1 << (15 - col)) != 0 {
                let off = dy * FB_STRIDE + dx * 4;
                unsafe {
                    let is_edge = col == 0
                        || row == 0
                        || (bits & (1 << (15 - col + 1)) == 0)
                        || (row + 1 < CURSOR_H
                            && bitmap[row + 1] & (1 << (15 - col)) == 0);
                    let color = if is_edge { &black } else { &white };
                    core::ptr::copy_nonoverlapping(color.as_ptr(), fb_ptr.add(off), 4);
                }
            }
        }
    }
}

// ── Service connections ──────────────────────────────────────────────

fn connect_keyboard(
    ring: &IoRing,
    kbd_msg: &mut IpcMessage,
) -> Result<IpcRecv, OsError> {
    let reg_send_fd = ostoo::service_lookup_retry(kbd_proto::SERVICE_NAME, 10)?;
    let (evt_send, evt_recv) = ostoo::ipc_channel(4, 0)?;

    let connect_msg = IpcMessage {
        tag: kbd_proto::MSG_KB_CONNECT,
        data: [0; 3],
        fds: [evt_send.fd(), -1, -1, -1],
    };
    if sys::ipc_send(reg_send_fd, &connect_msg, 0) < 0 {
        return Err(OsError(-1));
    }
    ostoo_rt::syscall::close(reg_send_fd as u32);
    drop(evt_send);

    ring.submit(&[IoSubmission::ipc_recv(TAG_KEYBOARD, evt_recv.fd(), kbd_msg)])?;

    println!("compositor: connected to keyboard service");
    Ok(evt_recv)
}


fn tile_position(idx: usize) -> (usize, usize) {
    let col = idx % 2;
    let row = idx / 2;
    let x = col * (FB_WIDTH / 2) + 20;
    let y = row * (FB_HEIGHT / 2) + 20;
    (x, y)
}
