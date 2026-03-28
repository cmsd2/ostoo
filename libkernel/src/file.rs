//! File handle abstraction for the per-process file descriptor table.

use alloc::sync::Arc;
use alloc::vec::Vec;
use snafu::Snafu;
use spin::Mutex;

use crate::completion_port::CompletionPort;
use crate::irq_mutex::IrqMutex;

// ---------------------------------------------------------------------------
// FD flags

/// File descriptor flag: close-on-exec.
pub const FD_CLOEXEC: u32 = 1;

// ---------------------------------------------------------------------------
// FdObject — what a file descriptor actually refers to

/// The kernel object referenced by a file descriptor.
pub enum FdObject {
    /// A regular I/O endpoint (console, pipe, VFS file, directory).
    File(Arc<dyn FileHandle>),
    /// A completion port for async I/O notification.
    Port(Arc<IrqMutex<CompletionPort>>),
}

impl Clone for FdObject {
    fn clone(&self) -> Self {
        match self {
            FdObject::File(h) => FdObject::File(h.clone()),
            FdObject::Port(p) => FdObject::Port(p.clone()),
        }
    }
}

impl FdObject {
    /// Close the underlying object.
    pub fn close(&self) {
        match self {
            FdObject::File(h) => h.close(),
            FdObject::Port(_) => {} // no-op
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
}

// ---------------------------------------------------------------------------
// FileHandle trait

pub trait FileHandle: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError>;
    fn write(&self, buf: &[u8]) -> Result<usize, FileError>;
    fn close(&self) {}
    /// Return a name for downcasting purposes.
    fn kind(&self) -> &'static str;
    /// For directory handles: serialize entries as linux_dirent64 into buf.
    fn getdents64(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::NotATty)
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
        // Delegate to the console input buffer (Phase 3).
        Ok(crate::console::read_input(buf))
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
    reader_thread: Option<usize>,
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
        reader_thread: None,
    }));
    (PipeReader(inner.clone()), PipeWriter(inner))
}

impl FileHandle for PipeReader {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
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
            // Register as blocked reader and wait.
            let thread_idx = crate::task::scheduler::current_thread_idx();
            inner.reader_thread = Some(thread_idx);
            drop(inner);
            crate::task::scheduler::block_current_thread();
        }
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd)
    }

    fn kind(&self) -> &'static str { "pipe_r" }
}

impl FileHandle for PipeWriter {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd)
    }

    fn write(&self, buf: &[u8]) -> Result<usize, FileError> {
        let mut inner = self.0.lock();
        inner.buffer.extend(buf.iter());
        if let Some(thread_idx) = inner.reader_thread.take() {
            crate::task::scheduler::unblock(thread_idx);
        }
        Ok(buf.len())
    }

    fn close(&self) {
        let mut inner = self.0.lock();
        inner.write_closed = true;
        if let Some(thread_idx) = inner.reader_thread.take() {
            crate::task::scheduler::unblock(thread_idx);
        }
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
