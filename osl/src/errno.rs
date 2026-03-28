//! Linux errno constants and converters from libkernel error types.

use libkernel::file::FileError;

pub const EPERM:   i64 = 1;
pub const ENOENT:  i64 = 2;
pub const ESRCH:   i64 = 3;
pub const EIO:     i64 = 5;
pub const ENOEXEC: i64 = 8;
pub const EBADF:   i64 = 9;
pub const ECHILD:  i64 = 10;
pub const ENOMEM:  i64 = 12;
pub const EFAULT:  i64 = 14;
pub const ENODEV:  i64 = 19;
pub const ENOTDIR: i64 = 20;
pub const EISDIR:  i64 = 21;
pub const EINVAL:  i64 = 22;
pub const EMFILE:  i64 = 24;
pub const ENOTTY:  i64 = 25;
pub const ESPIPE:  i64 = 29;
pub const ERANGE:  i64 = 34;
pub const ENOSYS:  i64 = 38;

pub fn file_errno(e: FileError) -> i64 {
    -(match e {
        FileError::BadFd => EBADF,
        FileError::IsDirectory => EISDIR,
        FileError::NotATty => ENOTTY,
        FileError::TooManyOpenFiles => EMFILE,
    })
}

pub fn vfs_errno(e: &devices::vfs::VfsError) -> i64 {
    -(match e {
        devices::vfs::VfsError::NotFound => ENOENT,
        devices::vfs::VfsError::NotAFile => EISDIR,
        devices::vfs::VfsError::NotADirectory => ENOTDIR,
        _ => EIO,
    })
}
