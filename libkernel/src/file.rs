//! File handle abstraction for the per-process file descriptor table.

use alloc::sync::Arc;
use alloc::vec::Vec;
use snafu::Snafu;
use crate::spin_mutex::SpinMutex as Mutex;

use crate::channel::{ChannelInner, CloseRecvAction, CloseSendAction, PendingPortRecv, PendingPortSend};
use crate::completion_port::CompletionPort;
use crate::irq_handle::IrqInner;
use crate::irq_mutex::IrqMutex;
use crate::notify::NotifyInner;
use crate::shmem::SharedMemInner;

// ---------------------------------------------------------------------------
// FD flags

/// File descriptor flag: close-on-exec.
pub const FD_CLOEXEC: u32 = 1;

// ---------------------------------------------------------------------------
// FdObject — what a file descriptor actually refers to

/// Result of closing an fd object.
pub enum CloseResult {
    /// Nothing to do.
    None,
    /// A blocked thread was woken; caller should donate to it.
    WakeThread(usize),
    /// An armed OP_IPC_RECV port needs a peer-closed notification.
    NotifyRecvPort(PendingPortRecv),
    /// An armed OP_IPC_SEND port needs a peer-closed notification.
    NotifySendPort(PendingPortSend),
}

/// Which end of an IPC channel a file descriptor refers to.
#[derive(Clone)]
pub enum ChannelFd {
    Send(Arc<IrqMutex<ChannelInner>>),
    Recv(Arc<IrqMutex<ChannelInner>>),
}

/// The kernel object referenced by a file descriptor.
pub enum FdObject {
    /// A regular I/O endpoint (console, pipe, VFS file, directory).
    File(Arc<dyn FileHandle>),
    /// A completion port for async I/O notification.
    Port(Arc<IrqMutex<CompletionPort>>),
    /// An IRQ file descriptor for userspace interrupt delivery.
    Irq(Arc<IrqMutex<IrqInner>>),
    /// An IPC channel endpoint (send or receive end).
    Channel(ChannelFd),
    /// A shared memory object (for MAP_SHARED anonymous mappings).
    SharedMem(Arc<SharedMemInner>),
    /// A notification fd for inter-process signaling (OP_RING_WAIT).
    Notify(Arc<IrqMutex<NotifyInner>>),
}

impl Clone for FdObject {
    fn clone(&self) -> Self {
        match self {
            FdObject::File(h) => FdObject::File(h.clone()),
            FdObject::Port(p) => FdObject::Port(p.clone()),
            FdObject::Irq(i) => FdObject::Irq(i.clone()),
            FdObject::Channel(c) => FdObject::Channel(c.clone()),
            FdObject::SharedMem(s) => FdObject::SharedMem(s.clone()),
            FdObject::Notify(n) => FdObject::Notify(n.clone()),
        }
    }
}

impl FdObject {
    /// Notify the underlying handle that a new fd reference was created.
    /// Call this when actually duplicating an fd (dup2, fork/clone), NOT
    /// for temporary access via get_fd().
    pub fn notify_dup(&self) {
        match self {
            FdObject::File(h) => h.on_dup(),
            FdObject::Channel(c) => match c {
                ChannelFd::Send(inner) => inner.lock().dup_send(),
                ChannelFd::Recv(inner) => inner.lock().dup_recv(),
            },
            // SharedMem, Notify: Arc clone is sufficient, no extra bookkeeping.
            _ => {}
        }
    }

    /// Close the underlying object.
    pub fn close(&self) -> CloseResult {
        match self {
            FdObject::File(h) => match h.close() {
                Some(idx) => CloseResult::WakeThread(idx),
                None => CloseResult::None,
            },
            FdObject::Port(_) => CloseResult::None,
            FdObject::Irq(i) => {
                let inner = i.lock();
                crate::irq_handle::close_irq(&inner);
                CloseResult::None
            }
            FdObject::Channel(c) => match c {
                ChannelFd::Send(inner) => match inner.lock().close_send() {
                    CloseSendAction::None => CloseResult::None,
                    CloseSendAction::WakeThread(idx) => CloseResult::WakeThread(idx),
                    CloseSendAction::NotifyPort(pr) => CloseResult::NotifyRecvPort(pr),
                },
                ChannelFd::Recv(inner) => match inner.lock().close_recv() {
                    CloseRecvAction::None => CloseResult::None,
                    CloseRecvAction::WakeThread(idx) => CloseResult::WakeThread(idx),
                    CloseRecvAction::NotifyPort(ps) => CloseResult::NotifySendPort(ps),
                },
            },
            // SharedMem: closing drops Arc ref; Drop impl handles frame release.
            FdObject::SharedMem(_) => CloseResult::None,
            // Notify: pending registration dropped with the Arc.
            FdObject::Notify(_) => CloseResult::None,
        }
    }

    /// Get the inner FileHandle, if this is a File.
    pub fn as_file(&self) -> Option<&Arc<dyn FileHandle>> {
        match self {
            FdObject::File(h) => Some(h),
            _ => None,
        }
    }

    /// Get the inner CompletionPort, if this is a Port.
    pub fn as_port(&self) -> Option<&Arc<IrqMutex<CompletionPort>>> {
        match self {
            FdObject::Port(p) => Some(p),
            _ => None,
        }
    }

    /// Get the inner IrqInner, if this is an Irq.
    pub fn as_irq(&self) -> Option<&Arc<IrqMutex<IrqInner>>> {
        match self {
            FdObject::Irq(i) => Some(i),
            _ => None,
        }
    }

    /// Get the channel fd, if this is a Channel.
    pub fn as_channel(&self) -> Option<&ChannelFd> {
        match self {
            FdObject::Channel(c) => Some(c),
            _ => None,
        }
    }

    /// Get the shared memory object, if this is a SharedMem.
    pub fn as_shmem(&self) -> Option<&Arc<SharedMemInner>> {
        match self {
            FdObject::SharedMem(s) => Some(s),
            _ => None,
        }
    }

    /// Get the notification inner, if this is a Notify.
    pub fn as_notify(&self) -> Option<&Arc<IrqMutex<NotifyInner>>> {
        match self {
            FdObject::Notify(n) => Some(n),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// FdEntry — wraps an FdObject + per-FD flags

/// A file descriptor table entry: the object plus per-FD flags (e.g. CLOEXEC).
pub struct FdEntry {
    pub object: FdObject,
    pub flags: u32,
}

impl FdEntry {
    pub fn new(handle: Arc<dyn FileHandle>) -> Self {
        FdEntry { object: FdObject::File(handle), flags: 0 }
    }

    pub fn with_flags(handle: Arc<dyn FileHandle>, flags: u32) -> Self {
        FdEntry { object: FdObject::File(handle), flags }
    }

    pub fn new_port(port: Arc<IrqMutex<CompletionPort>>) -> Self {
        FdEntry { object: FdObject::Port(port), flags: 0 }
    }

    pub fn port_with_flags(port: Arc<IrqMutex<CompletionPort>>, flags: u32) -> Self {
        FdEntry { object: FdObject::Port(port), flags }
    }

    pub fn from_object(object: FdObject, flags: u32) -> Self {
        FdEntry { object, flags }
    }
}

impl Clone for FdEntry {
    fn clone(&self) -> Self {
        FdEntry {
            object: self.object.clone(),
            flags: self.flags,
        }
    }
}

// ---------------------------------------------------------------------------
// FileError

#[derive(Debug, Clone, Copy, Snafu)]
pub enum FileError {
    #[snafu(display("bad file descriptor"))]
    BadFd,
    #[snafu(display("is a directory"))]
    IsDirectory,
    #[snafu(display("inappropriate ioctl for device"))]
    NotATty,
    #[snafu(display("too many open files"))]
    TooManyOpenFiles,
    #[snafu(display("interrupted system call"))]
    Interrupted,
}

// ---------------------------------------------------------------------------
// FileHandle trait

pub trait FileHandle: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError>;
    fn write(&self, buf: &[u8]) -> Result<usize, FileError>;
    /// Close the handle.  Returns the thread index of a woken reader (if
    /// this was the last PipeWriter), so the caller can yield to it after
    /// releasing any outer locks.
    fn close(&self) -> Option<usize> { None }
    /// Called when the fd referencing this handle is duplicated (dup2, fork,
    /// posix_spawn).  Used by PipeWriter to track writer reference count.
    fn on_dup(&self) {}
    /// Return a name for downcasting purposes.
    fn kind(&self) -> &'static str;
    /// For directory handles: serialize entries as linux_dirent64 into buf.
    fn getdents64(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::NotATty)
    }

    /// Return the full file content as a byte slice, if available.
    /// Used by mmap to copy file data into mapped pages.
    fn content_bytes(&self) -> Option<&[u8]> { None }

    /// Async-capable read. Default delegates to sync `read()`.
    /// Handles that may block (pipe, console) should override to register
    /// the waker and return `Pending` instead of blocking a thread.
    fn poll_read(&self, _cx: &mut core::task::Context<'_>, buf: &mut [u8])
        -> core::task::Poll<Result<usize, FileError>>
    {
        core::task::Poll::Ready(self.read(buf))
    }

    /// Async-capable write. Default delegates to sync `write()`.
    fn poll_write(&self, _cx: &mut core::task::Context<'_>, buf: &[u8])
        -> core::task::Poll<Result<usize, FileError>>
    {
        core::task::Poll::Ready(self.write(buf))
    }
}

// ---------------------------------------------------------------------------
// ConsoleHandle — stdin/stdout/stderr

pub struct ConsoleHandle {
    pub readable: bool,
}

impl FileHandle for ConsoleHandle {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        if !self.readable {
            return Err(FileError::BadFd);
        }
        match crate::console::read_input(buf) {
            crate::console::ReadResult::Data(n) => Ok(n),
            crate::console::ReadResult::Interrupted => Err(FileError::Interrupted),
        }
    }

    fn poll_read(&self, cx: &mut core::task::Context<'_>, buf: &mut [u8])
        -> core::task::Poll<Result<usize, FileError>>
    {
        if !self.readable {
            return core::task::Poll::Ready(Err(FileError::BadFd));
        }
        match crate::console::poll_read_input(cx, buf) {
            core::task::Poll::Ready(n) => core::task::Poll::Ready(Ok(n)),
            core::task::Poll::Pending => core::task::Poll::Pending,
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, FileError> {
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::print!("{}", s);
        } else {
            for &b in buf {
                if (0x20..0x7F).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                    crate::print!("{}", b as char);
                }
            }
        }
        Ok(buf.len())
    }

    fn kind(&self) -> &'static str { "console" }
}

// ---------------------------------------------------------------------------
// Pipe — in-kernel pipe for IPC

use alloc::collections::VecDeque;

struct PipeInner {
    buffer: VecDeque<u8>,
    write_closed: bool,
    /// Number of open writer file descriptors.  Decremented by
    /// `PipeWriter::close()`, incremented by `PipeWriter::on_dup()`.
    /// When this reaches 0, `write_closed` is set and readers see EOF.
    writer_count: usize,
    reader_thread: Option<usize>,
    /// Waker for async readers (completion port OP_READ).
    reader_waker: Option<core::task::Waker>,
}

/// Read end of a pipe.
pub struct PipeReader(Arc<Mutex<PipeInner>>);

/// Write end of a pipe.
pub struct PipeWriter(Arc<Mutex<PipeInner>>);

/// Create a connected (reader, writer) pipe pair.
pub fn make_pipe() -> (PipeReader, PipeWriter) {
    let inner = Arc::new(Mutex::new(PipeInner {
        buffer: VecDeque::new(),
        write_closed: false,
        writer_count: 1,
        reader_thread: None,
        reader_waker: None,
    }));
    (PipeReader(inner.clone()), PipeWriter(inner))
}

impl FileHandle for PipeReader {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        let pid = crate::process::current_pid();
        loop {
            let mut inner = self.0.lock();
            if !inner.buffer.is_empty() {
                let count = buf.len().min(inner.buffer.len());
                for i in 0..count {
                    buf[i] = inner.buffer.pop_front().unwrap();
                }
                return Ok(count);
            }
            if inner.write_closed {
                return Ok(0); // EOF
            }

            // Check for pending signals before blocking.
            if pid != crate::process::ProcessId::KERNEL {
                let has_signal = crate::process::with_process_ref(pid, |p| {
                    (p.signal.pending & !p.signal.blocked) != 0
                }).unwrap_or(false);
                if has_signal {
                    return Err(FileError::Interrupted);
                }
            }

            // Register as blocked reader and mark blocked under the pipe lock
            // so that unblock() is guaranteed to find ThreadState::Blocked.
            // [spec: completion_port_fixed.tla MarkBlocked — under caller's lock]
            let thread_idx = crate::task::scheduler::current_thread_idx();
            inner.reader_thread = Some(thread_idx);
            crate::task::scheduler::mark_blocked();
            drop(inner);
            // Register signal_thread after mark_blocked (best-effort for Ctrl+C).
            if pid != crate::process::ProcessId::KERNEL {
                crate::process::with_process(pid, |p| {
                    p.signal_thread = Some(thread_idx);
                });
            }
            crate::task::scheduler::yield_now();
            // Clear signal_thread after waking.
            if pid != crate::process::ProcessId::KERNEL {
                crate::process::with_process(pid, |p| {
                    p.signal_thread = None;
                });
            }
        }
    }

    fn poll_read(&self, cx: &mut core::task::Context<'_>, buf: &mut [u8])
        -> core::task::Poll<Result<usize, FileError>>
    {
        let mut inner = self.0.lock();
        if !inner.buffer.is_empty() {
            let count = buf.len().min(inner.buffer.len());
            for i in 0..count {
                buf[i] = inner.buffer.pop_front().unwrap();
            }
            return core::task::Poll::Ready(Ok(count));
        }
        if inner.write_closed {
            return core::task::Poll::Ready(Ok(0)); // EOF
        }
        inner.reader_waker = Some(cx.waker().clone());
        core::task::Poll::Pending
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd)
    }

    fn kind(&self) -> &'static str { "pipe_r" }
}

/// Helper: wake both the scheduler thread and the async waker on a pipe.
///
/// Returns the thread index that was unblocked (if any), so the caller can
/// use it for scheduler donate after dropping the pipe lock.
fn pipe_wake_reader(inner: &mut PipeInner) -> Option<usize> {
    let thread_idx = inner.reader_thread.take();
    if let Some(idx) = thread_idx {
        crate::task::scheduler::unblock(idx);
    }
    if let Some(waker) = inner.reader_waker.take() {
        waker.wake();
    }
    thread_idx
}

impl FileHandle for PipeWriter {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd)
    }

    fn write(&self, buf: &[u8]) -> Result<usize, FileError> {
        let woken = {
            let mut inner = self.0.lock();
            inner.buffer.extend(buf.iter());
            pipe_wake_reader(&mut inner)
        }; // pipe lock dropped before yield
        if let Some(thread_idx) = woken {
            crate::task::scheduler::set_donate_target(thread_idx);
            crate::task::scheduler::yield_now();
        }
        Ok(buf.len())
    }

    fn close(&self) -> Option<usize> {
        let mut inner = self.0.lock();
        inner.writer_count -= 1;
        if inner.writer_count == 0 {
            inner.write_closed = true;
            pipe_wake_reader(&mut inner)
        } else {
            None
        }
    }

    fn on_dup(&self) {
        let mut inner = self.0.lock();
        inner.writer_count += 1;
    }

    fn kind(&self) -> &'static str { "pipe_w" }
}

// ---------------------------------------------------------------------------
// FD table helpers (on Process)

/// Maximum number of file descriptors per process.
pub const MAX_FDS: usize = 64;

/// Create the default fd table with stdin(0), stdout(1), stderr(2).
pub fn default_fd_table() -> Vec<Option<FdEntry>> {
    let mut table: Vec<Option<FdEntry>> = Vec::with_capacity(8);
    table.push(Some(FdEntry::new(Arc::new(ConsoleHandle { readable: true }))));  // fd 0 = stdin
    table.push(Some(FdEntry::new(Arc::new(ConsoleHandle { readable: false })))); // fd 1 = stdout
    table.push(Some(FdEntry::new(Arc::new(ConsoleHandle { readable: false })))); // fd 2 = stderr
    table
}
