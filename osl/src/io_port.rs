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
    OP_NOP, OP_TIMEOUT, OP_READ, OP_WRITE, OP_IRQ_WAIT,
};
use libkernel::irq_mutex::IrqMutex;
use libkernel::file::{FileHandle, FileError, FdObject};
use libkernel::process;
use libkernel::task::{executor, scheduler, timer, Task};

use crate::dispatch::validate_user_buf;
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

// ---------------------------------------------------------------------------
// Helper: extract the Arc<IrqMutex<CompletionPort>> from an fd

fn get_port(port_fd: i32) -> Result<Arc<IrqMutex<CompletionPort>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(port_fd as usize)) {
        Some(Ok(obj)) => match obj.as_port() {
            Some(p) => Ok(p.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Get a file handle from the current process's fd table.
/// Returns EBADF if the fd refers to a non-file object (e.g. a completion port).
fn get_file_handle(fd: i32) -> Result<Arc<dyn FileHandle>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
        Some(Ok(obj)) => match obj.as_file() {
            Some(h) => Ok(h.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Get an IRQ handle from the current process's fd table.
fn get_irq_handle(fd: i32) -> Result<Arc<libkernel::irq_mutex::IrqMutex<libkernel::irq_handle::IrqInner>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd as usize)) {
        Some(Ok(obj)) => match obj.as_irq() {
            Some(i) => Ok(i.clone()),
            None => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

// ---------------------------------------------------------------------------
// sys_io_create(flags) → fd

pub fn sys_io_create(flags: u32) -> i64 {
    if flags != 0 {
        return -errno::EINVAL;
    }

    let port = Arc::new(IrqMutex::new(CompletionPort::new()));

    let pid = process::current_pid();
    match process::with_process(pid, |p| p.alloc_fd(FdObject::Port(port))) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(e)) => crate::errno::file_errno(e),
        None => -errno::EBADF,
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

    let port = match get_port(port_fd) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let mut processed: u32 = 0;
    for i in 0..count {
        let sub_ptr = entries_ptr + (i as u64) * entry_size;
        let sub = unsafe { *(sub_ptr as *const IoSubmission) };

        match sub.opcode {
            OP_NOP => {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_NOP,
                    read_buf: None,
                    read_dest: 0,
                });
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
                    });
                }));
            }

            OP_READ => {
                // Resolve handle eagerly; post EBADF immediately on bad fd.
                let handle = match get_file_handle(sub.fd) {
                    Ok(h) => h,
                    Err(_) => {
                        port.lock().post(Completion {
                            user_data: sub.user_data,
                            result: -errno::EBADF,
                            flags: 0,
                            opcode: OP_READ,
                            read_buf: None,
                            read_dest: 0,
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
                    });
                } else if !validate_user_buf(sub.buf_addr, sub.buf_len as u64) {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EFAULT,
                        flags: 0,
                        opcode: OP_READ,
                        read_buf: None,
                        read_dest: 0,
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
                        });
                    }));
                }
            }

            OP_WRITE => {
                let handle = match get_file_handle(sub.fd) {
                    Ok(h) => h,
                    Err(_) => {
                        port.lock().post(Completion {
                            user_data: sub.user_data,
                            result: -errno::EBADF,
                            flags: 0,
                            opcode: OP_WRITE,
                            read_buf: None,
                            read_dest: 0,
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
                    });
                } else if !validate_user_buf(sub.buf_addr, sub.buf_len as u64) {
                    port.lock().post(Completion {
                        user_data: sub.user_data,
                        result: -errno::EFAULT,
                        flags: 0,
                        opcode: OP_WRITE,
                        read_buf: None,
                        read_dest: 0,
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
                        });
                    }));
                }
            }

            OP_IRQ_WAIT => {
                let irq = match get_irq_handle(sub.fd) {
                    Ok(i) => i,
                    Err(_) => {
                        port.lock().post(Completion {
                            user_data: sub.user_data,
                            result: -errno::EBADF,
                            flags: 0,
                            opcode: OP_IRQ_WAIT,
                            read_buf: None,
                            read_dest: 0,
                        });
                        processed += 1;
                        continue;
                    }
                };
                // Arm: register port + user_data, unmask the GSI.
                libkernel::irq_handle::arm_irq(&irq, port.clone(), sub.user_data);
            }

            _ => {
                port.lock().post(Completion {
                    user_data: sub.user_data,
                    result: -errno::EINVAL,
                    flags: 0,
                    opcode: sub.opcode,
                    read_buf: None,
                    read_dest: 0,
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

    let port = match get_port(port_fd) {
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
                for (i, c) in drained.iter().enumerate().take(count) {
                    // For async OP_READ: copy kernel read buffer to user space.
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
