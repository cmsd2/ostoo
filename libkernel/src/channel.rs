//! Capability-based IPC channel.
//!
//! A channel is a unidirectional message conduit with configurable buffer
//! capacity.  Channels come in pairs: a send end and a receive end, each
//! exposed as a file descriptor (capability).
//!
//! - **capacity = 0**: Synchronous rendezvous — sender blocks until a receiver
//!   calls recv, then the message is transferred directly with scheduler
//!   donate for minimal latency.
//! - **capacity > 0**: Asynchronous buffered — sender enqueues and returns
//!   immediately (blocks only if the buffer is full).

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use crate::completion_port::CompletionPort;
use crate::file::FdObject;
use crate::irq_mutex::IrqMutex;
use crate::task::scheduler;

/// A fixed-size IPC message (48 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcMessage {
    /// User-defined message type / discriminator.
    pub tag: u64,
    /// Inline payload (24 bytes).
    pub data: [u64; 3],
    /// File descriptors for capability passing (-1 = unused).
    pub fds: [i32; 4],
}

impl IpcMessage {
    pub const ZERO: Self = IpcMessage { tag: 0, data: [0; 3], fds: [-1; 4] };
}

/// Transferred file descriptor objects, parallel to `IpcMessage::fds`.
/// Each `Some(obj)` corresponds to a valid fd slot; `None` means that
/// slot was -1 (unused).
pub type TransferFds = [Option<FdObject>; 4];

/// An IPC message plus any transferred fd objects.
///
/// The `fds` field in `msg` contains the *sender's* fd numbers (or -1).
/// The `transfer_fds` field holds the actual kernel objects extracted from
/// the sender's fd table.  At receive time, osl allocates new fds in the
/// receiver's table and rewrites `msg.fds` with the new numbers.
pub struct EnvelopedMessage {
    pub msg: IpcMessage,
    /// Transferred fd objects, or `None` if no fds are being passed.
    pub transfer_fds: Option<TransferFds>,
}

impl EnvelopedMessage {
    /// Wrap a plain message with no fd transfer.
    pub fn plain(msg: IpcMessage) -> Self {
        EnvelopedMessage { msg, transfer_fds: None }
    }
}

impl Drop for EnvelopedMessage {
    fn drop(&mut self) {
        // Close any fd objects that were never delivered to a receiver.
        if let Some(ref mut fds) = self.transfer_fds {
            for slot in fds.iter_mut() {
                if let Some(obj) = slot.take() {
                    obj.close();
                }
            }
        }
    }
}

/// Result of a channel send or receive operation.
pub enum ChannelResult {
    /// Operation completed successfully.
    Ok,
    /// Would block, but IPC_NONBLOCK was set.
    WouldBlock,
    /// The peer end is closed.
    PeerClosed,
    /// A blocked thread was woken — caller should donate to this thread index.
    OkDonate(usize),
}

/// Registration info for an `OP_IPC_RECV` submitted via a completion port.
pub struct PendingPortRecv {
    pub port: Arc<IrqMutex<CompletionPort>>,
    pub user_data: u64,
    pub buf_dest: u64,
}

/// Registration info for an `OP_IPC_SEND` submitted via a completion port.
pub struct PendingPortSend {
    pub port: Arc<IrqMutex<CompletionPort>>,
    pub user_data: u64,
    pub envelope: EnvelopedMessage,
}

/// Action for the caller after `arm_send`.
pub enum ArmSendAction {
    /// Message delivered or enqueued immediately; caller should post success.
    Ready,
    /// Message delivered to a blocked receiver; caller should post success and
    /// donate to the given thread.
    ReadyDonate(usize),
    /// Message delivered to an armed recv port; caller should post the message
    /// to the recv port and post success to the send port.
    ReadyToRecvPort(PendingPortRecv, EnvelopedMessage),
    /// Cannot send now; port info stored in the channel for future delivery.
    Armed,
    /// Receive end is closed.
    PeerClosed,
}

/// Action for the caller after `arm_recv`.
pub enum ArmRecvAction {
    /// A message was already available. Caller should post it to the recv port.
    Ready(EnvelopedMessage),
    /// A message was taken from a port-based sender. Caller should post the
    /// message to the recv port AND post success to the send port.
    ReadyAndNotifySendPort(EnvelopedMessage, Arc<IrqMutex<CompletionPort>>, u64),
    /// No message; port info stored in the channel for future notification.
    Armed,
    /// Send end is closed and no messages remain.
    PeerClosed,
}

/// Shared state between the send and receive ends of a channel.
pub struct ChannelInner {
    /// Buffered messages (for capacity > 0).
    queue: VecDeque<EnvelopedMessage>,
    /// Buffer capacity.  0 = synchronous rendezvous.
    capacity: usize,

    // --- Sync rendezvous state (capacity = 0) ---
    /// Message deposited by a blocked sender, waiting for a receiver.
    pending_send: Option<EnvelopedMessage>,
    /// Thread index of a sender blocked waiting for a receiver.
    blocked_sender: Option<usize>,

    // --- Receiver blocking (both sync and async) ---
    /// Thread index of a receiver blocked waiting for a message.
    blocked_receiver: Option<usize>,

    // --- Reference counts (like PipeWriter::writer_count) ---
    /// Number of open send-end file descriptors.
    send_count: usize,
    /// Number of open receive-end file descriptors.
    recv_count: usize,

    // --- Completion port integration ---
    /// When OP_IPC_RECV is submitted via a completion port, stores the port
    /// registration so that try_send can post the message directly.
    pending_port: Option<PendingPortRecv>,
    /// When OP_IPC_SEND is submitted via a completion port, stores the port
    /// registration + message so that try_recv can deliver it.
    pending_send_port: Option<PendingPortSend>,

    // --- Peer lifetime ---
    send_closed: bool,
    recv_closed: bool,
}

impl ChannelInner {
    /// Create a new channel with the given buffer capacity.
    pub fn new(capacity: usize) -> Self {
        ChannelInner {
            queue: VecDeque::with_capacity(capacity.min(64)),
            capacity,
            pending_send: None,
            blocked_sender: None,
            blocked_receiver: None,
            send_count: 1,
            recv_count: 1,
            pending_port: None,
            pending_send_port: None,
            send_closed: false,
            recv_closed: false,
        }
    }

    /// Increment the send-end reference count (called on dup/fork).
    pub fn dup_send(&mut self) {
        self.send_count += 1;
    }

    /// Increment the receive-end reference count (called on dup/fork).
    pub fn dup_recv(&mut self) {
        self.recv_count += 1;
    }

    /// Attempt to send a message (called with lock held).
    ///
    /// Returns the action the caller must take after releasing the lock.
    pub fn try_send(&mut self, env: EnvelopedMessage, nonblock: bool) -> SendAction {
        if self.recv_closed {
            return SendAction::PeerClosed(env);
        }

        if self.capacity == 0 {
            // Synchronous rendezvous.
            if let Some(recv_thread) = self.blocked_receiver.take() {
                // A receiver is waiting — hand off the message directly.
                // The receiver will read it from `pending_send` when it wakes.
                self.pending_send = Some(env);
                scheduler::unblock(recv_thread);
                SendAction::Donated(recv_thread)
            } else if let Some(pr) = self.pending_port.take() {
                // A completion port is armed for OP_IPC_RECV — deliver directly.
                SendAction::PostToPort(pr, env)
            } else {
                // No receiver waiting — sender must block.
                if nonblock {
                    return SendAction::WouldBlock(env);
                }
                let thread_idx = scheduler::current_thread_idx();
                self.pending_send = Some(env);
                self.blocked_sender = Some(thread_idx);
                // [spec: completion_port.tla MarkBlocked — under channel lock]
                scheduler::mark_blocked();
                SendAction::Block
            }
        } else {
            // Async buffered.
            if self.queue.len() < self.capacity {
                self.queue.push_back(env);
                if let Some(recv_thread) = self.blocked_receiver.take() {
                    scheduler::unblock(recv_thread);
                    SendAction::Donated(recv_thread)
                } else if let Some(pr) = self.pending_port.take() {
                    // Completion port armed — pop the message we just pushed.
                    let env = self.queue.pop_front().unwrap();
                    SendAction::PostToPort(pr, env)
                } else {
                    SendAction::Done
                }
            } else {
                // Queue full — sender must block.
                if nonblock {
                    return SendAction::WouldBlock(env);
                }
                let thread_idx = scheduler::current_thread_idx();
                self.blocked_sender = Some(thread_idx);
                // [spec: completion_port.tla MarkBlocked — under channel lock]
                scheduler::mark_blocked();
                SendAction::BlockWithMsg(env)
            }
        }
    }

    /// Attempt to receive a message (called with lock held).
    ///
    /// Returns the action the caller must take after releasing the lock.
    pub fn try_recv(&mut self, nonblock: bool) -> RecvAction {
        if self.capacity == 0 {
            // Synchronous rendezvous.
            if let Some(env) = self.pending_send.take() {
                // A thread-blocked sender deposited a message — take it.
                if let Some(sender_thread) = self.blocked_sender.take() {
                    scheduler::unblock(sender_thread);
                }
                RecvAction::Message(env)
            } else if let Some(ps) = self.pending_send_port.take() {
                // A port-based sender deposited a message — take it.
                RecvAction::MessageAndNotifySendPort(ps.envelope, ps.port, ps.user_data)
            } else if self.send_closed {
                RecvAction::PeerClosed
            } else {
                if nonblock {
                    return RecvAction::WouldBlock;
                }
                let thread_idx = scheduler::current_thread_idx();
                self.blocked_receiver = Some(thread_idx);
                // [spec: completion_port.tla MarkBlocked — under channel lock]
                scheduler::mark_blocked();
                RecvAction::Block
            }
        } else {
            // Async buffered.
            if let Some(env) = self.queue.pop_front() {
                // Wake a blocked sender if the queue was full.
                if let Some(sender_thread) = self.blocked_sender.take() {
                    scheduler::unblock(sender_thread);
                    RecvAction::Message(env)
                } else if let Some(ps) = self.pending_send_port.take() {
                    // Port-based sender was waiting for space — push its message.
                    self.queue.push_back(ps.envelope);
                    RecvAction::MessageAndNotifySendPort(env, ps.port, ps.user_data)
                } else {
                    RecvAction::Message(env)
                }
            } else if self.send_closed {
                RecvAction::PeerClosed
            } else {
                if nonblock {
                    return RecvAction::WouldBlock;
                }
                let thread_idx = scheduler::current_thread_idx();
                self.blocked_receiver = Some(thread_idx);
                // [spec: completion_port.tla MarkBlocked — under channel lock]
                scheduler::mark_blocked();
                RecvAction::Block
            }
        }
    }

    /// Returns true if the receive end is closed.
    pub fn is_recv_closed(&self) -> bool {
        self.recv_closed
    }

    /// Returns true if the send end is closed.
    pub fn is_send_closed(&self) -> bool {
        self.send_closed
    }

    /// Arm the channel for an `OP_IPC_SEND` completion port operation.
    ///
    /// If the message can be delivered immediately (queue not full, or receiver
    /// waiting), returns a Ready variant.  Otherwise stores the port + message
    /// for future delivery when a receiver drains space.
    pub fn arm_send(&mut self, info: PendingPortSend) -> ArmSendAction {
        if self.recv_closed {
            return ArmSendAction::PeerClosed;
        }

        if self.capacity == 0 {
            // Synchronous rendezvous.
            if let Some(recv_thread) = self.blocked_receiver.take() {
                self.pending_send = Some(info.envelope);
                scheduler::unblock(recv_thread);
                ArmSendAction::ReadyDonate(recv_thread)
            } else if let Some(pr) = self.pending_port.take() {
                // A recv port is armed — deliver directly to it.
                ArmSendAction::ReadyToRecvPort(pr, info.envelope)
            } else {
                self.pending_send_port = Some(info);
                ArmSendAction::Armed
            }
        } else {
            // Async buffered.
            if self.queue.len() < self.capacity {
                self.queue.push_back(info.envelope);
                if let Some(recv_thread) = self.blocked_receiver.take() {
                    scheduler::unblock(recv_thread);
                    ArmSendAction::ReadyDonate(recv_thread)
                } else if let Some(pr) = self.pending_port.take() {
                    let env = self.queue.pop_front().unwrap();
                    ArmSendAction::ReadyToRecvPort(pr, env)
                } else {
                    ArmSendAction::Ready
                }
            } else {
                // Queue full.
                self.pending_send_port = Some(info);
                ArmSendAction::Armed
            }
        }
    }

    /// Arm the channel for an `OP_IPC_RECV` completion port operation.
    ///
    /// If a message is already available (in queue or pending_send), returns
    /// it immediately.  Otherwise stores the port registration for future
    /// notification when a sender calls `try_send`.
    pub fn arm_recv(&mut self, info: PendingPortRecv) -> ArmRecvAction {
        if self.capacity == 0 {
            // Sync: check thread-blocked sender first, then port-based sender.
            if let Some(env) = self.pending_send.take() {
                if let Some(sender_thread) = self.blocked_sender.take() {
                    scheduler::unblock(sender_thread);
                }
                return ArmRecvAction::Ready(env);
            }
            if let Some(ps) = self.pending_send_port.take() {
                return ArmRecvAction::ReadyAndNotifySendPort(
                    ps.envelope, ps.port, ps.user_data,
                );
            }
        } else {
            // Async: check queue, then port-based sender waiting for space.
            if let Some(env) = self.queue.pop_front() {
                if let Some(sender_thread) = self.blocked_sender.take() {
                    scheduler::unblock(sender_thread);
                } else if let Some(ps) = self.pending_send_port.take() {
                    // Port sender was waiting for space — push its message.
                    self.queue.push_back(ps.envelope);
                    return ArmRecvAction::ReadyAndNotifySendPort(
                        env, ps.port, ps.user_data,
                    );
                }
                return ArmRecvAction::Ready(env);
            }
        }
        if self.send_closed {
            return ArmRecvAction::PeerClosed;
        }
        self.pending_port = Some(info);
        ArmRecvAction::Armed
    }

    /// Decrement the send-end reference count.  When it reaches zero, marks
    /// the send end as closed and wakes any blocked receiver.
    ///
    /// Returns the thread index of a woken receiver (if any) so the caller
    /// can donate after releasing the lock.
    pub fn close_send(&mut self) -> CloseSendAction {
        self.send_count -= 1;
        if self.send_count > 0 {
            return CloseSendAction::None;
        }
        self.send_closed = true;
        if let Some(recv_thread) = self.blocked_receiver.take() {
            scheduler::unblock(recv_thread);
            CloseSendAction::WakeThread(recv_thread)
        } else if let Some(pr) = self.pending_port.take() {
            CloseSendAction::NotifyPort(pr)
        } else {
            CloseSendAction::None
        }
    }

    /// Decrement the receive-end reference count.  When it reaches zero,
    /// marks the receive end as closed and wakes any blocked sender.
    ///
    /// Returns the thread index of a woken sender (if any) so the caller
    /// can donate after releasing the lock.
    pub fn close_recv(&mut self) -> CloseRecvAction {
        self.recv_count -= 1;
        if self.recv_count > 0 {
            return CloseRecvAction::None;
        }
        self.recv_closed = true;
        if let Some(sender_thread) = self.blocked_sender.take() {
            scheduler::unblock(sender_thread);
            CloseRecvAction::WakeThread(sender_thread)
        } else if let Some(ps) = self.pending_send_port.take() {
            CloseRecvAction::NotifyPort(ps)
        } else {
            CloseRecvAction::None
        }
    }
}

/// Action for the caller after `try_send`.
pub enum SendAction {
    /// Message delivered or enqueued; no further action.
    Done,
    /// Message delivered; donate quantum to the given thread.
    Donated(usize),
    /// No receiver; caller must block (message stored in pending_send).
    Block,
    /// Queue full; caller must block and retry with this message.
    BlockWithMsg(EnvelopedMessage),
    /// Would block but IPC_NONBLOCK was set.  Returns the envelope so the
    /// caller can drop/close the transferred fds.
    WouldBlock(EnvelopedMessage),
    /// Receive end is closed.  Returns the envelope so the caller can
    /// drop/close the transferred fds.
    PeerClosed(EnvelopedMessage),
    /// A completion port is armed — caller should post the message to the port.
    PostToPort(PendingPortRecv, EnvelopedMessage),
}

/// Action for the caller after `close_send`.
pub enum CloseSendAction {
    /// Nothing to do.
    None,
    /// A blocked receiver was woken; caller should donate to this thread.
    WakeThread(usize),
    /// A completion port was armed for OP_IPC_RECV; caller should post a
    /// peer-closed error to the port.
    NotifyPort(PendingPortRecv),
}

/// Action for the caller after `close_recv`.
pub enum CloseRecvAction {
    /// Nothing to do.
    None,
    /// A blocked sender was woken; caller should donate to this thread.
    WakeThread(usize),
    /// A completion port was armed for OP_IPC_SEND; caller should post a
    /// peer-closed error to the port.
    NotifyPort(PendingPortSend),
}

/// Action for the caller after `try_recv`.
pub enum RecvAction {
    /// A message was received.
    Message(EnvelopedMessage),
    /// A message was received, and an armed OP_IPC_SEND port was satisfied.
    /// Caller should post a success completion to the send port.
    MessageAndNotifySendPort(EnvelopedMessage, Arc<IrqMutex<CompletionPort>>, u64),
    /// No message; caller must block.
    Block,
    /// Would block but IPC_NONBLOCK was set.
    WouldBlock,
    /// Send end is closed and no messages remain.
    PeerClosed,
}
