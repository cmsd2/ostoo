//! IPC channel syscall implementations.
//!
//! Provides `sys_ipc_create`, `sys_ipc_send`, and `sys_ipc_recv` which
//! expose capability-based IPC channels to userspace.

use alloc::sync::Arc;
use core::mem;

use alloc::vec::Vec;

use libkernel::channel::{ChannelInner, IpcMessage, RecvAction, SendAction};
use libkernel::completion_port::{Completion, OP_IPC_RECV, OP_IPC_SEND};
use libkernel::file::{ChannelFd, FdObject, FD_CLOEXEC};
use libkernel::irq_mutex::IrqMutex;
use libkernel::process;
use libkernel::task::scheduler;

use crate::errno;
use crate::user_mem::validate_user_buf;

/// Flag for `ipc_create`: set FD_CLOEXEC on both fds.
const IPC_CLOEXEC: u32 = 0x1;
/// Flag for `ipc_send` / `ipc_recv`: non-blocking mode.
const IPC_NONBLOCK: u32 = 0x1;

// -------------------------------------------------------------------------
// sys_ipc_create(fds_ptr, capacity, flags) → 0 or -errno
//
// Creates a channel pair and writes [send_fd, recv_fd] to user memory.

pub fn sys_ipc_create(fds_ptr: u64, capacity: u32, flags: u32) -> i64 {
    if !validate_user_buf(fds_ptr, 8) {
        return -errno::EFAULT;
    }
    if flags & !IPC_CLOEXEC != 0 {
        return -errno::EINVAL;
    }

    let inner = Arc::new(IrqMutex::new(ChannelInner::new(capacity as usize)));
    let send_obj = FdObject::Channel(ChannelFd::Send(inner.clone()));
    let recv_obj = FdObject::Channel(ChannelFd::Recv(inner));

    let fd_flags = if flags & IPC_CLOEXEC != 0 { FD_CLOEXEC } else { 0 };

    let pid = process::current_pid();
    match process::with_process(pid, |p| {
        let sfd = p.alloc_fd_with_flags(send_obj, fd_flags)?;
        let rfd = match p.alloc_fd_with_flags(recv_obj, fd_flags) {
            Ok(fd) => fd,
            Err(e) => {
                p.close_fd(sfd).ok();
                return Err(e);
            }
        };
        Ok((sfd, rfd))
    }) {
        Some(Ok((sfd, rfd))) => {
            let fds = unsafe { core::slice::from_raw_parts_mut(fds_ptr as *mut i32, 2) };
            fds[0] = sfd as i32;
            fds[1] = rfd as i32;
            0
        }
        Some(Err(e)) => errno::file_errno(e),
        None => -errno::EBADF,
    }
}

// -------------------------------------------------------------------------
// sys_ipc_send(fd, msg_ptr, flags) → 0 or -errno

pub fn sys_ipc_send(fd: i32, msg_ptr: u64, flags: u32) -> i64 {
    if !validate_user_buf(msg_ptr, mem::size_of::<IpcMessage>() as u64) {
        return -errno::EFAULT;
    }

    let nonblock = flags & IPC_NONBLOCK != 0;

    // Get the channel send-end from the fd table.
    let channel = match get_channel_send(fd as usize) {
        Ok(c) => c,
        Err(e) => return e,
    };

    // Copy the message from user memory.
    let msg = unsafe { *(msg_ptr as *const IpcMessage) };

    loop {
        let action = channel.lock().try_send(msg, nonblock);
        match action {
            SendAction::Done => return 0,
            SendAction::Donated(thread_idx) => {
                scheduler::set_donate_target(thread_idx);
                scheduler::yield_now();
                return 0;
            }
            SendAction::Block => {
                // Message stored in pending_send; block until receiver wakes us.
                scheduler::block_current_thread();
                // Check if peer closed while we were blocked.
                let recv_closed = channel.lock().is_recv_closed();
                if recv_closed {
                    return -errno::EPIPE;
                }
                return 0;
            }
            SendAction::BlockWithMsg(_retry_msg) => {
                // Queue was full; block until receiver drains and wakes us.
                scheduler::block_current_thread();
                // Retry — the receiver unblocked us, so there should be space.
                continue;
            }
            SendAction::WouldBlock => return -errno::EAGAIN,
            SendAction::PeerClosed => return -errno::EPIPE,
            SendAction::PostToPort(pr, msg) => {
                let bytes = ipc_msg_to_bytes(&msg);
                let woken = pr.port.lock().post(Completion {
                    user_data: pr.user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_IPC_RECV,
                    read_buf: Some(bytes),
                    read_dest: pr.buf_dest,
                });
                if let Some(thread_idx) = woken {
                    scheduler::set_donate_target(thread_idx);
                    scheduler::yield_now();
                }
                return 0;
            }
        }
    }
}

// -------------------------------------------------------------------------
// sys_ipc_recv(fd, msg_ptr, flags) → 0 or -errno

pub fn sys_ipc_recv(fd: i32, msg_ptr: u64, flags: u32) -> i64 {
    if !validate_user_buf(msg_ptr, mem::size_of::<IpcMessage>() as u64) {
        return -errno::EFAULT;
    }

    let nonblock = flags & IPC_NONBLOCK != 0;

    // Get the channel recv-end from the fd table.
    let channel = match get_channel_recv(fd as usize) {
        Ok(c) => c,
        Err(e) => return e,
    };

    loop {
        let action = channel.lock().try_recv(nonblock);
        match action {
            RecvAction::Message(msg) => {
                // Copy message to user memory.
                unsafe { *(msg_ptr as *mut IpcMessage) = msg; }
                return 0;
            }
            RecvAction::Block => {
                // No message; block until sender wakes us.
                scheduler::block_current_thread();
                // Retry — sender may have deposited a message or closed.
                continue;
            }
            RecvAction::MessageAndNotifySendPort(msg, port, user_data) => {
                // Copy message to user memory.
                unsafe { *(msg_ptr as *mut IpcMessage) = msg; }
                // Post success to the armed OP_IPC_SEND port.
                let woken = port.lock().post(Completion {
                    user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_IPC_SEND,
                    read_buf: None,
                    read_dest: 0,
                });
                if let Some(thread_idx) = woken {
                    scheduler::set_donate_target(thread_idx);
                    scheduler::yield_now();
                }
                return 0;
            }
            RecvAction::WouldBlock => return -errno::EAGAIN,
            RecvAction::PeerClosed => return -errno::EPIPE,
        }
    }
}

// -------------------------------------------------------------------------
// Helpers

fn get_channel_send(fd: usize) -> Result<Arc<IrqMutex<ChannelInner>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_channel() {
            Some(ChannelFd::Send(inner)) => Ok(inner.clone()),
            _ => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

fn get_channel_recv(fd: usize) -> Result<Arc<IrqMutex<ChannelInner>>, i64> {
    let pid = process::current_pid();
    match process::with_process_ref(pid, |p| p.get_fd(fd)) {
        Some(Ok(obj)) => match obj.as_channel() {
            Some(ChannelFd::Recv(inner)) => Ok(inner.clone()),
            _ => Err(-errno::EBADF),
        },
        _ => Err(-errno::EBADF),
    }
}

/// Serialize an IpcMessage to a byte Vec for Completion::read_buf.
fn ipc_msg_to_bytes(msg: &IpcMessage) -> Vec<u8> {
    unsafe {
        core::slice::from_raw_parts(
            msg as *const IpcMessage as *const u8,
            core::mem::size_of::<IpcMessage>(),
        )
    }.to_vec()
}
