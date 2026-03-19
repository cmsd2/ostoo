//! File handle abstraction for the per-process file descriptor table.

use alloc::sync::Arc;
use alloc::vec::Vec;
use snafu::Snafu;

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
// FD table helpers (on Process)

/// Maximum number of file descriptors per process.
pub const MAX_FDS: usize = 64;

/// Create the default fd table with stdin(0), stdout(1), stderr(2).
pub fn default_fd_table() -> Vec<Option<Arc<dyn FileHandle>>> {
    let mut table: Vec<Option<Arc<dyn FileHandle>>> = Vec::with_capacity(8);
    table.push(Some(Arc::new(ConsoleHandle { readable: true })));  // fd 0 = stdin
    table.push(Some(Arc::new(ConsoleHandle { readable: false }))); // fd 1 = stdout
    table.push(Some(Arc::new(ConsoleHandle { readable: false }))); // fd 2 = stderr
    table
}
