#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

use alloc::vec;
use alloc::vec::Vec;
use ostoo_rt::compositor_proto::*;
use ostoo_rt::kbd_proto;
use ostoo_rt::ostoo::{
    self, CompletionPort, FramebufferMem, IpcRecv, IpcSend, NotifyFd, OsError, SharedMem,
};
use ostoo_rt::sys::{self, IoCompletion, IoSubmission, IpcMessage};
use ostoo_rt::{eprintln, println};

// ── Constants ────────────────────────────────────────────────────────

const FB_WIDTH: usize = 1024;
const FB_HEIGHT: usize = 768;
const FB_STRIDE: usize = FB_WIDTH * 4; // bytes per scanline (BGRA)

const BG_COLOR: u32 = 0x00282828; // dark grey background

const MAX_WINDOWS: usize = 4;
const MAX_COMPLETIONS: usize = 16;

// Completion user_data tag encoding:
//   0x1000          = new client on registration channel
//   0x2000 + wid    = damage notification for window `wid`
//   0x3000 + wid    = command message on per-client channel for `wid`
//   0x4000          = keyboard event from keyboard service
const TAG_NEW_CLIENT: u64 = 0x1000;
const TAG_DAMAGE_BASE: u64 = 0x2000;
const TAG_CMD_BASE: u64 = 0x3000;
const TAG_KEYBOARD: u64 = 0x4000;

// ── Window state ─────────────────────────────────────────────────────

struct Window {
    id: u64,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    buf_ptr: *const u8,
    #[allow(dead_code)]
    buf_size: usize,
    dirty: bool,
    // Keep RAII handles alive so fds stay open.
    _buf: SharedMem,
    _notify: NotifyFd,
    _c2s_recv: IpcRecv,
    s2c_send: IpcSend,
    // IPC message buffer for re-arming OP_IPC_RECV (must live at stable address).
    cmd_msg: IpcMessage,
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

    // 2. Create registration channel + completion port.
    let (reg_send, reg_recv) = ostoo::ipc_channel(4, 0)?;
    let port = CompletionPort::new()?;

    // 3. Register the compositor service.
    ostoo::service_register(SERVICE_NAME, reg_send.fd())?;
    println!("compositor: registered as 'compositor'");

    // 4. Connect to keyboard service (if available).
    let mut kbd_msg = IpcMessage::default();
    let _kbd_recv: Option<IpcRecv> = match connect_keyboard(&port, &mut kbd_msg) {
        Ok(recv) => Some(recv),
        Err(_) => {
            println!("compositor: keyboard service not found, no key input");
            None
        }
    };

    // 5. Arm OP_IPC_RECV on the registration channel.
    let mut reg_msg = IpcMessage::default();
    port.submit(&[IoSubmission::ipc_recv(
        TAG_NEW_CLIENT,
        reg_recv.fd(),
        &mut reg_msg,
    )])?;

    // 6. Event loop.
    let mut windows: Vec<Window> = Vec::new();
    let mut next_wid: u64 = 1;
    let mut completions = [IoCompletion::default(); MAX_COMPLETIONS];

    loop {
        let n = port.wait(&mut completions, 1, 0)?;
        let mut any_dirty = false;

        for i in 0..n {
            let cqe = completions[i];
            let ud = cqe.user_data;

            if ud == TAG_NEW_CLIENT {
                // New client connection.
                if cqe.result >= 0 {
                    handle_connect(
                        &port,
                        &reg_msg,
                        &mut windows,
                        &mut next_wid,
                        fb_ptr,
                    );
                }
                // Re-arm registration channel.
                reg_msg = IpcMessage::default();
                port.submit(&[IoSubmission::ipc_recv(
                    TAG_NEW_CLIENT,
                    reg_recv.fd(),
                    &mut reg_msg,
                )])?;
            } else if ud >= TAG_DAMAGE_BASE && ud < TAG_CMD_BASE {
                // Damage notification.
                let wid = ud - TAG_DAMAGE_BASE;
                if let Some(win) = windows.iter_mut().find(|w| w.id == wid) {
                    win.dirty = true;
                    any_dirty = true;
                    // Re-arm OP_RING_WAIT.
                    port.submit(&[IoSubmission::ring_wait(
                        TAG_DAMAGE_BASE + wid,
                        win._notify.fd(),
                    )])?;
                }
            } else if ud == TAG_KEYBOARD {
                // Key event from keyboard service.
                if cqe.result >= 0 && kbd_msg.tag == kbd_proto::MSG_KB_KEY {
                    let byte = kbd_msg.data[0];
                    let modifiers = kbd_msg.data[1];
                    let key_type = kbd_msg.data[2];
                    // Forward to the first (focused) window.
                    if let Some(win) = windows.first() {
                        let key_msg = IpcMessage {
                            tag: MSG_KEY_EVENT,
                            data: [byte, modifiers, key_type],
                            fds: [-1; 4],
                        };
                        let _ = sys::ipc_send(win.s2c_send.fd(), &key_msg, sys::IPC_NONBLOCK);
                    }
                }
                // Re-arm keyboard recv.
                kbd_msg = IpcMessage::default();
                if let Some(ref recv) = _kbd_recv {
                    port.submit(&[IoSubmission::ipc_recv(
                        TAG_KEYBOARD,
                        recv.fd(),
                        &mut kbd_msg,
                    )])?;
                }
            } else if ud >= TAG_CMD_BASE {
                // Per-client command.
                let wid = ud - TAG_CMD_BASE;
                // Read the tag before borrowing mutably.
                let tag = windows.iter().find(|w| w.id == wid)
                    .map(|w| w.cmd_msg.tag);
                if let Some(tag) = tag {
                    if cqe.result >= 0 {
                        match tag {
                            MSG_PRESENT => {
                                if let Some(w) = windows.iter_mut().find(|w| w.id == wid) {
                                    w.dirty = true;
                                    any_dirty = true;
                                }
                            }
                            MSG_CLOSE => {
                                println!("compositor: window {} closed", wid);
                                windows.retain(|w| w.id != wid);
                                composite(fb_ptr, back_ptr, &mut windows);
                            }
                            _ => {}
                        }
                    }
                    // Re-arm OP_IPC_RECV on client channel (if window still exists).
                    if let Some(win) = windows.iter_mut().find(|w| w.id == wid) {
                        win.cmd_msg = IpcMessage::default();
                        port.submit(&[IoSubmission::ipc_recv(
                            TAG_CMD_BASE + wid,
                            win._c2s_recv.fd(),
                            &mut win.cmd_msg,
                        )])?;
                    }
                }
            }
        }

        if any_dirty {
            composite(fb_ptr, back_ptr, &mut windows);
        }
    }
}

// ── Connection handling ──────────────────────────────────────────────

fn handle_connect(
    port: &CompletionPort,
    msg: &IpcMessage,
    windows: &mut Vec<Window>,
    next_wid: &mut u64,
    _fb_ptr: *mut u8,
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

    // Clamp window size.
    let w = req_w.min(FB_WIDTH).max(64);
    let h = req_h.min(FB_HEIGHT).max(64);

    let wid = *next_wid;
    *next_wid += 1;

    // Allocate buffer + notification fd.
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

    // Map buffer in compositor's address space.
    let buf_ptr = match buf.mmap() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("compositor: mmap failed (errno {})", e.errno());
            return;
        }
    };

    // Send MSG_WINDOW_CREATED to client.
    let reply = IpcMessage {
        tag: MSG_WINDOW_CREATED,
        data: [wid, w as u64, h as u64],
        fds: [buf.fd(), notify.fd(), -1, -1],
    };
    // Use raw ipc_send with the s2c_send fd we received (not wrapped in RAII yet).
    if sys::ipc_send(s2c_send_fd, &reply, 0) < 0 {
        eprintln!("compositor: failed to send WINDOW_CREATED");
        return;
    }

    // Auto-tile: 2×2 grid.
    let idx = windows.len();
    let (x, y) = tile_position(idx, w, h);

    println!(
        "compositor: window {} created ({}x{} at {},{})",
        wid, w, h, x, y
    );

    // Wrap the received fds.
    let c2s_recv = IpcRecv::from_raw_fd(c2s_recv_fd);
    let s2c_send = IpcSend::from_raw_fd(s2c_send_fd);

    let mut cmd_msg = IpcMessage::default();

    // Arm OP_RING_WAIT on notify fd.
    let _ = port.submit(&[IoSubmission::ring_wait(
        TAG_DAMAGE_BASE + wid,
        notify.fd(),
    )]);

    // Arm OP_IPC_RECV on client channel.
    let _ = port.submit(&[IoSubmission::ipc_recv(
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
        buf_ptr: buf_ptr as *const u8,
        buf_size,
        dirty: false,
        _buf: buf,
        _notify: notify,
        _c2s_recv: c2s_recv,
        s2c_send: s2c_send,
        cmd_msg,
    });
}

// ── Compositing ──────────────────────────────────────────────────────

fn composite(fb_ptr: *mut u8, back_ptr: *mut u8, windows: &mut Vec<Window>) {
    // Composite into back buffer to avoid flicker.
    fill_rect(back_ptr, 0, 0, FB_WIDTH, FB_HEIGHT, BG_COLOR);

    // Painter's algorithm — back to front (first connected = bottom).
    for win in windows.iter_mut() {
        blit_window(back_ptr, win);
        win.dirty = false;
    }

    // Copy back buffer to framebuffer in one pass.
    let total = FB_WIDTH * FB_HEIGHT * 4;
    unsafe {
        core::ptr::copy_nonoverlapping(back_ptr, fb_ptr, total);
    }
}

fn blit_window(fb_ptr: *mut u8, win: &Window) {
    let src = win.buf_ptr;
    let src_stride = win.w * 4;

    for row in 0..win.h {
        let dst_y = win.y + row;
        if dst_y >= FB_HEIGHT {
            break;
        }
        let dst_x = win.x;
        let copy_w = win.w.min(FB_WIDTH.saturating_sub(dst_x));
        if copy_w == 0 {
            continue;
        }

        let src_off = row * src_stride;
        let dst_off = dst_y * FB_STRIDE + dst_x * 4;
        let bytes = copy_w * 4;

        unsafe {
            core::ptr::copy_nonoverlapping(
                src.add(src_off),
                fb_ptr.add(dst_off),
                bytes,
            );
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

// ── Keyboard service connection ──────────────────────────────────────

/// Connect to the userspace keyboard service.  Returns the IpcRecv end
/// for receiving key events.  The completion port is armed with TAG_KEYBOARD.
fn connect_keyboard(
    port: &CompletionPort,
    kbd_msg: &mut IpcMessage,
) -> Result<IpcRecv, OsError> {
    // Retry lookup — the keyboard driver may still be starting up.
    let reg_send_fd = ostoo::service_lookup_retry(kbd_proto::SERVICE_NAME, 10)?;

    // Create channel pair for key events: keyboard → compositor.
    let (evt_send, evt_recv) = ostoo::ipc_channel(4, 0)?;

    // Send MSG_KB_CONNECT with our send-end so the keyboard driver can push events.
    let connect_msg = IpcMessage {
        tag: kbd_proto::MSG_KB_CONNECT,
        data: [0; 3],
        fds: [evt_send.fd(), -1, -1, -1],
    };
    if sys::ipc_send(reg_send_fd, &connect_msg, 0) < 0 {
        return Err(OsError(-1));
    }
    // Close fds we no longer need.
    ostoo_rt::syscall::close(reg_send_fd as u32);
    drop(evt_send);

    // Arm OP_IPC_RECV on the event channel.
    port.submit(&[IoSubmission::ipc_recv(
        TAG_KEYBOARD,
        evt_recv.fd(),
        kbd_msg,
    )])?;

    println!("compositor: connected to keyboard service");
    Ok(evt_recv)
}

fn tile_position(idx: usize, _w: usize, _h: usize) -> (usize, usize) {
    let col = idx % 2;
    let row = idx / 2;
    let x = col * (FB_WIDTH / 2) + 20;
    let y = row * (FB_HEIGHT / 2) + 20;
    (x, y)
}
