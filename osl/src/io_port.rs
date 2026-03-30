//! Completion port syscall implementations: io_create, io_submit, io_wait.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll};

use libkernel::completion_port::{
    CompletionPort, Completion,
    OP_NOP, OP_TIMEOUT, OP_READ, OP_WRITE, OP_IRQ_WAIT, OP_IPC_RECV, OP_IPC_SEND,
    OP_RING_WAIT,
};
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

// ---------------------------------------------------------------------------
// Userspace structures (repr(C), matching the C header)

/// Submission entry read from user memory.
#[repr(C)]
#[derive(Clone, Copy)]
struct IoSubmission {
    user_data: u64,
    opcode: u32,
    flags: u32,
    fd: i32,
    _pad: i32,
    buf_addr: u64,
    buf_len: u32,
    offset: u32,
    timeout_ns: u64,
}

/// Completion entry written to user memory.
#[repr(C)]
#[derive(Clone, Copy)]
struct IoCompletion {
    user_data: u64,
    result: i64,
    flags: u32,
    opcode: u32,
}

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
                    scheduler::yield_now();
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
                // Resolve handle eagerly; post EBADF immediately on bad fd.
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
                        processed += 1;
                        continue;
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
                    // Spawn async task: read into kernel buffer, post
                    // completion with the buffer attached. io_wait will
                    // copy it to user space.
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
                        processed += 1;
                        continue;
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
                    // Copy user data to kernel buffer while page table is
                    // active, then spawn async task for the actual write.
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
                        processed += 1;
                        continue;
                    }
                };
                // Arm: register port + user_data, unmask the GSI.
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
                        processed += 1;
                        continue;
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
                    processed += 1;
                    continue;
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
                            scheduler::yield_now();
                        }
                    }
                    ArmRecvAction::ReadyAndNotifySendPort(mut env, send_port, send_ud) => {
                        // Post the message to the recv port.
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
                        // Post success to the send port.
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
                            scheduler::yield_now();
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
                    ArmRecvAction::Armed => {
                        // Port registered on channel; completion will be posted
                        // when a sender sends a message.
                    }
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
                        processed += 1;
                        continue;
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
                    processed += 1;
                    continue;
                }

                // Copy message from user memory.
                let msg = unsafe { *(sub.buf_addr as *const IpcMessage) };

                // Extract transferred fd objects from sender's fd table.
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
                        processed += 1;
                        continue;
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
                            scheduler::yield_now();
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
                        // Donate to the receiver that was unblocked.
                        let target = woken.unwrap_or(recv_thread);
                        scheduler::set_donate_target(target);
                        scheduler::yield_now();
                    }
                    ArmSendAction::ReadyToRecvPort(pr, mut env) => {
                        // Both send and recv were port-based.
                        // Post the message to the recv port.
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
                        // Post success to the send port.
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
                            scheduler::yield_now();
                        }
                    }
                    ArmSendAction::Armed => {
                        // Port+message stored in channel; completion will be
                        // posted when a receiver drains space.
                    }
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
                        processed += 1;
                        continue;
                    }
                };
                // Arm: register port + user_data, or satisfy immediately if notified.
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

        processed += 1;
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

    loop {
        {
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

            // Not enough completions yet — register as waiter and block
            p.set_waiter(thread_idx);
        }
        scheduler::block_current_thread();
        // Woken — either by post() or by timeout timer. Loop back to check.
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
