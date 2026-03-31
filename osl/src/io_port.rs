//! Completion port syscall implementations: io_create, io_submit, io_wait.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll};

use libkernel::completion_port::{
    CompletionPort, Completion, IoSubmission, IoCompletion, IoRing,
    OP_NOP, OP_TIMEOUT, OP_READ, OP_WRITE, OP_IRQ_WAIT, OP_IPC_RECV, OP_IPC_SEND,
    OP_RING_WAIT, MAX_SQ_ENTRIES, MAX_CQ_ENTRIES,
};
use libkernel::wait_condition::WaitCondition;
use libkernel::shmem::SharedMemInner;
use libkernel::irq_mutex::IrqMutex;
use libkernel::channel::{ArmRecvAction, ArmSendAction, EnvelopedMessage, IpcMessage, PendingPortRecv, PendingPortSend};
use libkernel::file::{ChannelFd, FileHandle, FileError, FdObject};
use libkernel::task::{executor, scheduler, timer, Task};

use crate::fd_helpers;
use crate::user_mem::validate_user_buf;
use crate::errno;

// ---------------------------------------------------------------------------
// Async futures for FileHandle poll_read / poll_write

/// Future that calls `FileHandle::poll_read` into a kernel buffer.
struct HandleReadFuture {
    handle: Arc<dyn FileHandle>,
    buf: Vec<u8>,
}

impl Future for HandleReadFuture {
    type Output = (Result<usize, FileError>, Vec<u8>);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.handle.poll_read(cx, &mut this.buf) {
            Poll::Ready(result) => Poll::Ready((result, core::mem::take(&mut this.buf))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future that calls `FileHandle::poll_write` from a kernel buffer.
struct HandleWriteFuture {
    handle: Arc<dyn FileHandle>,
    buf: Vec<u8>,
}

impl Future for HandleWriteFuture {
    type Output = Result<usize, FileError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.handle.poll_write(cx, &self.buf)
    }
}

// ---------------------------------------------------------------------------
// CancellableDelay — a Delay that completes early when a cancel flag is set

struct CancellableDelay {
    delay: timer::Delay,
    cancel: Arc<AtomicBool>,
}

impl Future for CancellableDelay {
    /// Returns `true` if the delay expired naturally, `false` if cancelled.
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
        if self.cancel.load(Ordering::Acquire) {
            return Poll::Ready(false);
        }
        let this = self.get_mut();
        match Pin::new(&mut this.delay).poll(cx) {
            Poll::Ready(()) => Poll::Ready(true),
            Poll::Pending => Poll::Pending,
        }
    }
}

// IoSubmission and IoCompletion are imported from libkernel::completion_port.
// FD helpers are in crate::fd_helpers (get_fd_file, get_fd_port, get_fd_irq).

// ---------------------------------------------------------------------------
// sys_io_create(flags) → fd

pub fn sys_io_create(flags: u32) -> i64 {
    if flags != 0 {
        return -errno::EINVAL;
    }

    let port = Arc::new(IrqMutex::new(CompletionPort::new()));

    match fd_helpers::alloc_fd(FdObject::Port(port)) {
        Ok(fd) => fd as i64,
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// process_submission — shared logic for sys_io_submit and sys_io_ring_enter

/// Process a single submission entry.
///
/// Returns `true` if a thread was woken and the caller should donate/yield.
fn process_submission(
    port: &Arc<IrqMutex<CompletionPort>>,
    sub: &IoSubmission,
) -> bool {
    match sub.opcode {
        OP_NOP => {
            let woken = port.lock().post(Completion {
                user_data: sub.user_data,
                result: 0,
                flags: 0,
                opcode: OP_NOP,
                read_buf: None,
                read_dest: 0,
                transfer_fds: None,
            });
            if let Some(thread_idx) = woken {
                scheduler::set_donate_target(thread_idx);
                return true;
            }
        }

        OP_TIMEOUT => {
            let port_clone = port.clone();
            let user_data = sub.user_data;
            let timeout_ns = sub.timeout_ns;
            let ms = (timeout_ns + 999_999) / 1_000_000;

            executor::spawn(Task::new(async move {
                timer::Delay::from_millis(ms).await;
                port_clone.lock().post(Completion {
                    user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_TIMEOUT,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
            }));
        }

        OP_READ => {
            let handle = match fd_helpers::get_fd_file(sub.fd as usize) {
                Ok(h) => h,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_READ,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };

            if sub.buf_len == 0 {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_READ,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
            } else if !validate_user_buf(sub.buf_addr, sub.buf_len as u64) {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: -errno::EFAULT,
                    flags: 0,
                    opcode: OP_READ,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
            } else {
                let port_clone = port.clone();
                let user_data = sub.user_data;
                let buf_len = sub.buf_len as usize;
                let buf_addr = sub.buf_addr;

                executor::spawn(Task::new(async move {
                    let kernel_buf = vec![0u8; buf_len];
                    let fut = HandleReadFuture { handle, buf: kernel_buf };
                    let (result, mut buf) = fut.await;
                    let result = match result {
                        Ok(n) => { buf.truncate(n); n as i64 }
                        Err(e) => { buf.clear(); crate::errno::file_errno(e) }
                    };
                    port_clone.lock().post(Completion {
                        user_data,
                        result,
                        flags: 0,
                        opcode: OP_READ,
                        read_buf: Some(buf),
                        read_dest: buf_addr,
                        transfer_fds: None,
                    });
                }));
            }
        }

        OP_WRITE => {
            let handle = match fd_helpers::get_fd_file(sub.fd as usize) {
                Ok(h) => h,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_WRITE,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };

            if sub.buf_len == 0 {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_WRITE,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
            } else if !validate_user_buf(sub.buf_addr, sub.buf_len as u64) {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: -errno::EFAULT,
                    flags: 0,
                    opcode: OP_WRITE,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
            } else {
                let kernel_buf: Vec<u8> = unsafe {
                    core::slice::from_raw_parts(
                        sub.buf_addr as *const u8,
                        sub.buf_len as usize,
                    )
                }.to_vec();

                let port_clone = port.clone();
                let user_data = sub.user_data;

                executor::spawn(Task::new(async move {
                    let fut = HandleWriteFuture { handle, buf: kernel_buf };
                    let result = match fut.await {
                        Ok(n) => n as i64,
                        Err(e) => crate::errno::file_errno(e),
                    };
                    port_clone.lock().post(Completion {
                        user_data,
                        result,
                        flags: 0,
                        opcode: OP_WRITE,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                }));
            }
        }

        OP_IRQ_WAIT => {
            let irq = match fd_helpers::get_fd_irq(sub.fd as usize) {
                Ok(i) => i,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_IRQ_WAIT,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };
            libkernel::irq_handle::arm_irq(&irq, port.clone(), sub.user_data);
        }

        OP_IPC_RECV => {
            let channel = match get_ipc_recv_channel(sub.fd as usize) {
                Ok(c) => c,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_IPC_RECV,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };

            let msg_size = core::mem::size_of::<IpcMessage>() as u64;
            if sub.buf_addr != 0 && !validate_user_buf(sub.buf_addr, msg_size) {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: -errno::EFAULT,
                    flags: 0,
                    opcode: OP_IPC_RECV,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
                return false;
            }

            let info = PendingPortRecv {
                port: port.clone(),
                user_data: sub.user_data,
                buf_dest: sub.buf_addr,
            };
            let action = channel.lock().arm_recv(info);
            match action {
                ArmRecvAction::Ready(mut env) => {
                    let bytes = ipc_msg_to_bytes(&env.msg);
                    let tfds = env.transfer_fds.take();
                    let woken = port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_RECV,
                        read_buf: Some(bytes),
                        read_dest: sub.buf_addr,
                        transfer_fds: tfds,
                    });
                    if let Some(thread_idx) = woken {
                        scheduler::set_donate_target(thread_idx);
                        return true;
                    }
                }
                ArmRecvAction::ReadyAndNotifySendPort(mut env, send_port, send_ud) => {
                    let bytes = ipc_msg_to_bytes(&env.msg);
                    let tfds = env.transfer_fds.take();
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_RECV,
                        read_buf: Some(bytes),
                        read_dest: sub.buf_addr,
                        transfer_fds: tfds,
                    });
                    let woken = send_port.lock().post(Completion {
                        user_data: send_ud,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    if let Some(thread_idx) = woken {
                        scheduler::set_donate_target(thread_idx);
                        return true;
                    }
                }
                ArmRecvAction::PeerClosed => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EPIPE,
                        flags: 0,
                        opcode: OP_IPC_RECV,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                }
                ArmRecvAction::Armed => {}
            }
        }

        OP_IPC_SEND => {
            let channel = match get_ipc_send_channel(sub.fd as usize) {
                Ok(c) => c,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };

            let msg_size = core::mem::size_of::<IpcMessage>() as u64;
            if !validate_user_buf(sub.buf_addr, msg_size) {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: -errno::EFAULT,
                    flags: 0,
                    opcode: OP_IPC_SEND,
                    read_buf: None,
                    read_dest: 0,
                    transfer_fds: None,
                });
                return false;
            }

            let msg = unsafe { *(sub.buf_addr as *const IpcMessage) };

            let transfer_fds = match crate::ipc::extract_transfer_fds(&msg) {
                Ok(fds) => fds,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };

            let info = PendingPortSend {
                port: port.clone(),
                user_data: sub.user_data,
                envelope: EnvelopedMessage { msg, transfer_fds },
            };
            let action = channel.lock().arm_send(info);
            match action {
                ArmSendAction::Ready => {
                    let woken = port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    if let Some(thread_idx) = woken {
                        scheduler::set_donate_target(thread_idx);
                        return true;
                    }
                }
                ArmSendAction::ReadyDonate(recv_thread) => {
                    let woken = port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    let target = woken.unwrap_or(recv_thread);
                    scheduler::set_donate_target(target);
                    return true;
                }
                ArmSendAction::ReadyToRecvPort(pr, mut env) => {
                    let bytes = ipc_msg_to_bytes(&env.msg);
                    let tfds = env.transfer_fds.take();
                    pr.port.lock().post(Completion {
                        user_data: pr.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_RECV,
                        read_buf: Some(bytes),
                        read_dest: pr.buf_dest,
                        transfer_fds: tfds,
                    });
                    let woken = port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: 0,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    if let Some(thread_idx) = woken {
                        scheduler::set_donate_target(thread_idx);
                        return true;
                    }
                }
                ArmSendAction::Armed => {}
                ArmSendAction::PeerClosed => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EPIPE,
                        flags: 0,
                        opcode: OP_IPC_SEND,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                }
            }
        }

        OP_RING_WAIT => {
            let notify = match fd_helpers::get_fd_notify(sub.fd as usize) {
                Ok(n) => n,
                Err(_) => {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EBADF,
                        flags: 0,
                        opcode: OP_RING_WAIT,
                        read_buf: None,
                        read_dest: 0,
                        transfer_fds: None,
                    });
                    return false;
                }
            };
            libkernel::notify::arm_notify(&notify, port.clone(), sub.user_data);
        }

        _ => {
            port.lock().post(Completion {
                user_data: sub.user_data,
                result: -errno::EINVAL,
                flags: 0,
                opcode: sub.opcode,
                read_buf: None,
                read_dest: 0,
                transfer_fds: None,
            });
        }
    }

    false
}

// ---------------------------------------------------------------------------
// sys_io_submit(port_fd, entries_ptr, count) → i64

pub fn sys_io_submit(port_fd: i32, entries_ptr: u64, count: u32) -> i64 {
    let entry_size = core::mem::size_of::<IoSubmission>() as u64;
    let total_size = entry_size * count as u64;

    if count == 0 {
        return 0;
    }
    if !validate_user_buf(entries_ptr, total_size) {
        return -errno::EFAULT;
    }

    let port = match fd_helpers::get_fd_port(port_fd as usize) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let mut processed: u32 = 0;
    for i in 0..count {
        let sub_ptr = entries_ptr + (i as u64) * entry_size;
        let sub = unsafe { *(sub_ptr as *const IoSubmission) };

        let should_yield = process_submission(&port, &sub);
        processed += 1;
        if should_yield {
            scheduler::yield_now();
        }
    }

    processed as i64
}

// ---------------------------------------------------------------------------
// sys_io_wait(port_fd, completions_ptr, max, min, timeout_ns) → i64

pub fn sys_io_wait(port_fd: i32, completions_ptr: u64, max: u32, min: u32, timeout_ns: u64) -> i64 {
    let comp_size = core::mem::size_of::<IoCompletion>() as u64;
    let total_size = comp_size * max as u64;

    if max == 0 {
        return 0;
    }
    if !validate_user_buf(completions_ptr, total_size) {
        return -errno::EFAULT;
    }

    let port = match fd_helpers::get_fd_port(port_fd as usize) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Ring mode: io_wait is replaced by io_ring_enter
    if port.lock().has_ring() {
        return -errno::EINVAL;
    }

    let min = min.min(max) as usize;
    let max = max as usize;

    // Calculate deadline tick if timeout specified
    let deadline = if timeout_ns > 0 {
        let ms = (timeout_ns + 999_999) / 1_000_000;
        Some(timer::ticks() + ms)
    } else if timeout_ns == 0 {
        // timeout_ns == 0 means infinite wait (no timeout)
        None
    } else {
        None
    };

    // If timeout > 0, spawn a cancellable timer task to wake us on deadline.
    // The cancel flag is set when io_wait returns early, so the timer task
    // stops re-registering wakers and frees its WAKERS slot promptly.
    let thread_idx = scheduler::current_thread_idx();
    let cancel = Arc::new(AtomicBool::new(false));
    if let Some(deadline_tick) = deadline {
        let ms = deadline_tick.saturating_sub(timer::ticks());
        let cancel_clone = cancel.clone();
        executor::spawn(Task::new(async move {
            let expired = CancellableDelay {
                delay: timer::Delay::from_millis(ms),
                cancel: cancel_clone,
            }.await;
            if expired {
                // Spurious wakeup if already unblocked — unblock on non-Blocked is a no-op
                scheduler::unblock(thread_idx);
            }
        }));
    }

    // [spec: completion_port.tla WaitLoop]
    loop {
        // [spec: completion_port.tla CheckAndAct — WaitCondition]
        let mut p = port.lock();
        let timed_out = deadline.is_some() && timer::ticks() >= deadline.unwrap();

        if p.pending() >= min || timed_out {
            // Ready to return — drain and copy to user memory
            let drained = p.drain(max);
            drop(p);

            let count = drained.len().min(max);
            let user_comps = unsafe {
                core::slice::from_raw_parts_mut(
                    completions_ptr as *mut IoCompletion,
                    count,
                )
            };
            for (i, mut c) in drained.into_iter().enumerate().take(count) {
                // For OP_IPC_RECV: install transferred fds into receiver's
                // fd table and rewrite the serialized message bytes.
                if let Some(tfds) = c.transfer_fds.take() {
                    if let Some(ref mut buf) = c.read_buf {
                        if buf.len() == core::mem::size_of::<IpcMessage>() {
                            let msg_ptr = buf.as_mut_ptr() as *mut IpcMessage;
                            let msg = unsafe { &mut *msg_ptr };
                            // Best-effort: if install fails, the fds field
                            // keeps -1 values and the completion still posts.
                            let _ = crate::ipc::install_transfer_fds(msg, tfds);
                        }
                    }
                }
                // For async OP_READ / OP_IPC_RECV: copy kernel buffer to user space.
                // We're in the process's syscall context so page tables are correct.
                if let Some(ref buf) = c.read_buf {
                    if !buf.is_empty() && c.read_dest != 0
                        && validate_user_buf(c.read_dest, buf.len() as u64)
                    {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                buf.as_ptr(),
                                c.read_dest as *mut u8,
                                buf.len(),
                            );
                        }
                    }
                }
                user_comps[i] = IoCompletion {
                    user_data: c.user_data,
                    result: c.result,
                    flags: c.flags,
                    opcode: c.opcode,
                };
            }
            // Cancel the timeout task so it stops occupying a WAKERS slot.
            cancel.store(true, Ordering::Release);
            return count as i64;
        }

        // Not enough completions yet — register as waiter and block.
        WaitCondition::wait_while(Some(p), |p, thread_idx| {
            p.set_waiter(thread_idx);
        });
        // Woken — either by post() or by timeout timer. Loop back to check.
    }
}

// ---------------------------------------------------------------------------
// sys_io_setup_rings(port_fd, params_ptr) → 0 or -errno

/// User-visible params struct for io_setup_rings.
#[repr(C)]
struct IoRingParams {
    sq_entries: u32,   // IN: requested (rounded to pow2, max 64)
    cq_entries: u32,   // IN: requested (rounded to pow2, max 128)
    sq_fd: i32,        // OUT: shmem fd for SQ ring
    cq_fd: i32,        // OUT: shmem fd for CQ ring
}

pub fn sys_io_setup_rings(port_fd: i32, params_ptr: u64) -> i64 {
    let params_size = core::mem::size_of::<IoRingParams>() as u64;
    if !validate_user_buf(params_ptr, params_size) {
        return -errno::EFAULT;
    }

    let port = match fd_helpers::get_fd_port(port_fd as usize) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Check no ring already set up
    if port.lock().has_ring() {
        return -errno::EBUSY;
    }

    // Read params from user memory
    let params = unsafe { &mut *(params_ptr as *mut IoRingParams) };
    let mut sq_entries = params.sq_entries;
    let mut cq_entries = params.cq_entries;

    // Round up to next power of 2, clamp to max
    sq_entries = sq_entries.max(1).next_power_of_two().min(MAX_SQ_ENTRIES);
    cq_entries = cq_entries.max(1).next_power_of_two().min(MAX_CQ_ENTRIES);

    // Create IoRing
    let ring = match IoRing::new(sq_entries, cq_entries) {
        Some(r) => r,
        None => return -errno::ENOMEM,
    };

    // Create SharedMemInner objects wrapping the ring's physical frames
    // (non-owning — IoRing owns the frames)
    let sq_shmem = Arc::new(SharedMemInner::from_existing(
        ring.sq_frames().to_vec(),
        libkernel::consts::PAGE_SIZE as usize,
    ));
    let cq_shmem = Arc::new(SharedMemInner::from_existing(
        ring.cq_frames().to_vec(),
        libkernel::consts::PAGE_SIZE as usize,
    ));

    // Allocate shmem fds
    let sq_fd = match fd_helpers::alloc_fd(FdObject::SharedMem(sq_shmem)) {
        Ok(fd) => fd as i32,
        Err(_) => return -errno::EMFILE,
    };
    let cq_fd = match fd_helpers::alloc_fd(FdObject::SharedMem(cq_shmem)) {
        Ok(fd) => fd as i32,
        Err(e) => {
            // Close sq_fd on failure
            let pid = libkernel::process::current_pid();
            let _ = libkernel::process::with_process(pid, |p| p.close_fd(sq_fd as usize));
            return e;
        }
    };

    // Install ring on the port
    port.lock().setup_ring(ring);

    // Write results back to user params
    params.sq_entries = sq_entries;
    params.cq_entries = cq_entries;
    params.sq_fd = sq_fd;
    params.cq_fd = cq_fd;

    0
}

// ---------------------------------------------------------------------------
// sys_io_ring_enter(port_fd, to_submit, min_complete, flags) → i64

pub fn sys_io_ring_enter(port_fd: i32, to_submit: u32, min_complete: u32, flags: u32) -> i64 {
    if flags != 0 {
        return -errno::EINVAL;
    }

    let port = match fd_helpers::get_fd_port(port_fd as usize) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Must be in ring mode
    if !port.lock().has_ring() {
        return -errno::EINVAL;
    }

    // Phase 1: Drain SQ entries
    if to_submit > 0 {
        let (sq_head, sq_tail) = {
            let p = port.lock();
            let ring = p.ring().unwrap();
            (ring.sq_head(), ring.sq_tail())
        };

        let pending = sq_tail.wrapping_sub(sq_head);
        let count = pending.min(to_submit);

        for i in 0..count {
            let idx = sq_head.wrapping_add(i);
            let sub = {
                let p = port.lock();
                let ring = p.ring().unwrap();
                ring.read_sqe(idx)
            };

            let should_yield = process_submission(&port, &sub);
            if should_yield {
                scheduler::yield_now();
            }
        }

        // Advance SQ head
        if count > 0 {
            let p = port.lock();
            let ring = p.ring().unwrap();
            ring.advance_sq_head(count);
        }
    }

    // Phase 2: Flush deferred completions from the kernel queue
    // (for OP_READ/OP_IPC_RECV that need data copy in syscall context)
    {
        let mut p = port.lock();
        let n = p.pending();
        let drained = p.drain(n);
        let has_ring = p.has_ring();
        drop(p);

        if has_ring {
            for mut c in drained {
                // Install transferred fds (OP_IPC_RECV)
                if let Some(tfds) = c.transfer_fds.take() {
                    if let Some(ref mut buf) = c.read_buf {
                        if buf.len() == core::mem::size_of::<IpcMessage>() {
                            let msg_ptr = buf.as_mut_ptr() as *mut IpcMessage;
                            let msg = unsafe { &mut *msg_ptr };
                            let _ = crate::ipc::install_transfer_fds(msg, tfds);
                        }
                    }
                }
                // Copy kernel buffer to user space (OP_READ, OP_IPC_RECV)
                if let Some(ref buf) = c.read_buf {
                    if !buf.is_empty() && c.read_dest != 0
                        && validate_user_buf(c.read_dest, buf.len() as u64)
                    {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                buf.as_ptr(),
                                c.read_dest as *mut u8,
                                buf.len(),
                            );
                        }
                    }
                }
                // Post CQE to ring
                let p = port.lock();
                if let Some(ring) = p.ring() {
                    ring.post_cqe(IoCompletion {
                        user_data: c.user_data,
                        result: c.result,
                        flags: c.flags,
                        opcode: c.opcode,
                    });
                }
            }
        }
    }

    // Phase 3: Wait for min_complete CQ entries
    // [spec: completion_port.tla WaitLoop — WaitCondition]
    if min_complete > 0 {
        loop {
            let p = port.lock();
            if let Some(ring) = p.ring() {
                let avail = ring.cq_available();
                if avail >= min_complete {
                    return avail as i64;
                }
            }
            WaitCondition::wait_while(Some(p), |p, thread_idx| {
                p.set_waiter(thread_idx);
            });
            // Woken by post() — flush deferred completions before re-checking.
            {
                let mut p = port.lock();
                let n = p.pending();
                let drained = p.drain(n);
                let has_ring = p.has_ring();
                drop(p);

                if has_ring {
                    for mut c in drained {
                        if let Some(tfds) = c.transfer_fds.take() {
                            if let Some(ref mut buf) = c.read_buf {
                                if buf.len() == core::mem::size_of::<IpcMessage>() {
                                    let msg_ptr = buf.as_mut_ptr() as *mut IpcMessage;
                                    let msg = unsafe { &mut *msg_ptr };
                                    let _ = crate::ipc::install_transfer_fds(msg, tfds);
                                }
                            }
                        }
                        if let Some(ref buf) = c.read_buf {
                            if !buf.is_empty() && c.read_dest != 0
                                && validate_user_buf(c.read_dest, buf.len() as u64)
                            {
                                unsafe {
                                    core::ptr::copy_nonoverlapping(
                                        buf.as_ptr(),
                                        c.read_dest as *mut u8,
                                        buf.len(),
                                    );
                                }
                            }
                        }
                        let p = port.lock();
                        if let Some(ring) = p.ring() {
                            ring.post_cqe(IoCompletion {
                                user_data: c.user_data,
                                result: c.result,
                                flags: c.flags,
                                opcode: c.opcode,
                            });
                        }
                    }
                }
            }
        }
    }

    // Return CQ available count
    let p = port.lock();
    if let Some(ring) = p.ring() {
        ring.cq_available() as i64
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// IPC channel helpers for OP_IPC_RECV

use libkernel::irq_mutex::IrqMutex as IrqMutex2;
use libkernel::channel::ChannelInner;

fn get_ipc_send_channel(fd: usize) -> Result<Arc<IrqMutex2<ChannelInner>>, i64> {
    let pid = libkernel::process::current_pid();
    match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_channel() {
            Some(ChannelFd::Send(inner)) => Ok(inner.clone()),
            _ => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

fn get_ipc_recv_channel(fd: usize) -> Result<Arc<IrqMutex2<ChannelInner>>, i64> {
    let pid = libkernel::process::current_pid();
    match libkernel::process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_channel() {
            Some(ChannelFd::Recv(inner)) => Ok(inner.clone()),
            _ => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Serialize an IpcMessage to a byte Vec for use as Completion::read_buf.
fn ipc_msg_to_bytes(msg: &IpcMessage) -> Vec<u8> {
    unsafe {
        core::slice::from_raw_parts(
            msg as *const IpcMessage as *const u8,
            core::mem::size_of::<IpcMessage>(),
        )
    }.to_vec()
}
