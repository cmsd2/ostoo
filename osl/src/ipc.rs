//! IPC channel syscall implementations.
//!
//! Provides `sys_ipc_create`, `sys_ipc_send`, and `sys_ipc_recv` which
//! expose capability-based IPC channels to userspace.

use alloc::sync::Arc;
use core::mem;

use alloc::vec::Vec;

use libkernel::channel::{
    ChannelInner, EnvelopedMessage, IpcMessage, RecvAction, SendAction, TransferFds,
};
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

    // Extract transferred fd objects from sender's fd table.
    let transfer_fds = match extract_transfer_fds(&msg) {
        Ok(fds) => fds,
        Err(e) => return e,
    };

    let env = EnvelopedMessage { msg, transfer_fds };

    let mut retry_env = Some(env);
    loop {
        let env = retry_env.take().unwrap();
        let action = channel.lock().try_send(env, nonblock);
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
            SendAction::BlockWithMsg(env) => {
                // Queue was full; block until receiver drains and wakes us.
                retry_env = Some(env);
                scheduler::block_current_thread();
                // Retry — the receiver unblocked us, so there should be space.
                continue;
            }
            SendAction::WouldBlock(_env) => return -errno::EAGAIN,
            SendAction::PeerClosed(_env) => return -errno::EPIPE,
            SendAction::PostToPort(pr, mut env) => {
                let bytes = ipc_msg_to_bytes(&env.msg);
                // Take transfer_fds out so they travel with the Completion,
                // not dropped when env is dropped.
                let tfds = env.transfer_fds.take();
                let woken = pr.port.lock().post(Completion {
                    user_data: pr.user_data,
                    result: 0,
                    flags: 0,
                    opcode: OP_IPC_RECV,
                    read_buf: Some(bytes),
                    read_dest: pr.buf_dest,
                    transfer_fds: tfds,
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
            RecvAction::Message(env) => {
                return deliver_to_user(env, msg_ptr);
            }
            RecvAction::Block => {
                // No message; block until sender wakes us.
                scheduler::block_current_thread();
                // Retry — sender may have deposited a message or closed.
                continue;
            }
            RecvAction::MessageAndNotifySendPort(env, port, user_data) => {
                let rc = deliver_to_user(env, msg_ptr);
                // Post success to the armed OP_IPC_SEND port.
                let woken = port.lock().post(Completion {
                    user_data,
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
                return rc;
            }
            RecvAction::WouldBlock => return -errno::EAGAIN,
            RecvAction::PeerClosed => return -errno::EPIPE,
        }
    }
}

// -------------------------------------------------------------------------
// fd-passing helpers

/// Extract fd objects from the sender's fd table based on `msg.fds`.
///
/// Returns `None` if all fd slots are -1 (no transfer needed).
/// On error (bad fd), rolls back any already-duped objects and returns EBADF.
pub fn extract_transfer_fds(msg: &IpcMessage) -> Result<Option<TransferFds>, i64> {
    // Fast path: no fds to transfer.
    if msg.fds.iter().all(|&fd| fd == -1) {
        return Ok(None);
    }

    let pid = process::current_pid();
    let mut objects: TransferFds = [None, None, None, None];

    for (i, &fd) in msg.fds.iter().enumerate() {
        if fd == -1 {
            continue;
        }
        if fd < 0 {
            // Negative fds other than -1 are invalid.
            rollback_transfer_fds(&mut objects);
            return Err(-errno::EBADF);
        }
        match process::with_process(pid, |p| p.get_fd_entry(fd as usize)) {
            Some(Ok(entry)) => {
                entry.object.notify_dup();
                objects[i] = Some(entry.object);
            }
            _ => {
                rollback_transfer_fds(&mut objects);
                return Err(-errno::EBADF);
            }
        }
    }
    Ok(Some(objects))
}

/// Install transferred fd objects into the receiver's fd table.
///
/// Rewrites `msg.fds` with the new fd numbers.  On error (too many fds),
/// rolls back and returns EMFILE.
pub fn install_transfer_fds(msg: &mut IpcMessage, mut fds: TransferFds) -> Result<(), i64> {
    let pid = process::current_pid();
    let mut allocated: [i32; 4] = [-1; 4];

    for i in 0..4 {
        if let Some(object) = fds[i].take() {
            match process::with_process(pid, |p| p.alloc_fd(object)) {
                Some(Ok(new_fd)) => {
                    allocated[i] = new_fd as i32;
                }
                _ => {
                    // Rollback: close already-allocated fds.
                    for &afd in &allocated {
                        if afd != -1 {
                            process::with_process(pid, |p| {
                                p.close_fd(afd as usize).ok();
                            });
                        }
                    }
                    // Close remaining uninstalled objects.
                    for slot in fds.iter_mut().skip(i) {
                        if let Some(obj) = slot.take() {
                            obj.close();
                        }
                    }
                    return Err(-errno::EMFILE);
                }
            }
        }
    }
    msg.fds = allocated;
    Ok(())
}

/// Close any already-duped objects on send-side error.
fn rollback_transfer_fds(objects: &mut TransferFds) {
    for slot in objects.iter_mut() {
        if let Some(obj) = slot.take() {
            obj.close();
        }
    }
}

/// Deliver an enveloped message to user memory, installing any transferred fds.
fn deliver_to_user(mut env: EnvelopedMessage, msg_ptr: u64) -> i64 {
    if let Some(fds) = env.transfer_fds.take() {
        if let Err(e) = install_transfer_fds(&mut env.msg, fds) {
            return e;
        }
    }
    // Copy message to user memory.
    unsafe { *(msg_ptr as *mut IpcMessage) = env.msg; }
    0
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
pub fn ipc_msg_to_bytes(msg: &IpcMessage) -> Vec<u8> {
    unsafe {
        core::slice::from_raw_parts(
            msg as *const IpcMessage as *const u8,
            core::mem::size_of::<IpcMessage>(),
        )
    }
    .to_vec()
}
