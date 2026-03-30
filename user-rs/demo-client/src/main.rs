#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

use ostoo_rt::compositor_proto::*;
use ostoo_rt::ostoo::{self, NotifyFd, OsError, SharedMem};
use ostoo_rt::sys::{self, IpcMessage};
use ostoo_rt::syscall;
use ostoo_rt::{eprintln, println};

const WIN_W: usize = 400;
const WIN_H: usize = 300;

#[no_mangle]
fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("demo-client: error (errno {})", e.errno());
            1
        }
    }
}

fn run() -> Result<(), OsError> {
    println!("demo-client: connecting to compositor");

    // 1. Look up the compositor service.
    let reg_send_fd = ostoo::service_lookup(SERVICE_NAME)?;

    // 2. Create our channel pairs.
    let (c2s_send, c2s_recv) = ostoo::ipc_channel(4, 0)?;
    let (s2c_send, s2c_recv) = ostoo::ipc_channel(4, 0)?;

    // 3. Send MSG_CONNECT with our channel ends.
    let connect_msg = IpcMessage {
        tag: MSG_CONNECT,
        data: [WIN_W as u64, WIN_H as u64, 0],
        fds: [c2s_recv.fd(), s2c_send.fd(), -1, -1],
    };
    if sys::ipc_send(reg_send_fd, &connect_msg, 0) < 0 {
        eprintln!("demo-client: failed to send MSG_CONNECT");
        return Err(OsError(-1));
    }

    // Close the reg_send fd (we don't need it anymore).
    syscall::close(reg_send_fd as u32);
    // Drop the ends we passed to the compositor (they now own them).
    drop(c2s_recv);
    drop(s2c_send);

    // 4. Wait for MSG_WINDOW_CREATED.
    let reply = s2c_recv.recv(0)?;
    if reply.tag != MSG_WINDOW_CREATED {
        eprintln!("demo-client: unexpected reply tag {}", reply.tag);
        return Err(OsError(-1));
    }

    let wid = reply.data[0];
    let w = reply.data[1] as usize;
    let h = reply.data[2] as usize;
    let buf_fd = reply.fds[0];
    let notify_fd = reply.fds[1];

    println!(
        "demo-client: window {} ({}x{}), buf_fd={}, notify_fd={}",
        wid, w, h, buf_fd, notify_fd
    );

    // 5. Map the shared buffer.
    let buf = SharedMem::from_fd(buf_fd, w * h * 4);
    let buf_ptr = buf.mmap()?;
    let notify = NotifyFd::from_raw_fd(notify_fd);

    // 6. Draw a gradient pattern.
    draw_gradient(buf_ptr, w, h);

    // 7. Signal damage.
    notify.signal()?;
    println!("demo-client: presented gradient");

    // 8. Send MSG_PRESENT.
    c2s_send.send(
        &IpcMessage {
            tag: MSG_PRESENT,
            data: [wid, 0, 0],
            fds: [-1; 4],
        },
        0,
    )?;

    // 9. Keep running (sleep loop) — in a real client we'd handle input.
    loop {
        // Sleep ~1 second via a simple busy-wait alternative.
        // In the real kernel, clock_gettime returns zero, so we just yield.
        syscall::exit(0);
    }
}

/// Draw a simple colour gradient: red increases left-to-right,
/// green increases top-to-bottom, blue is constant.
fn draw_gradient(ptr: *mut u8, w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let r = ((x * 255) / w) as u8;
            let g = ((y * 255) / h) as u8;
            let b: u8 = 80;
            let pixel: u32 = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16);
            let off = (y * w + x) * 4;
            unsafe {
                (ptr.add(off) as *mut u32).write(pixel);
            }
        }
    }
}
