//! VFS-backed file handles for the per-process file descriptor table.

use alloc::vec::Vec;
use libkernel::spin_mutex::SpinMutex as Mutex;

use devices::vfs::VfsDirEntry;
use libkernel::file::{FileHandle, FileError};

// ---------------------------------------------------------------------------
// VfsHandle — buffered file (entire content loaded at open)

pub struct VfsHandle {
    content: Vec<u8>,
    pos: Mutex<usize>,
}

impl VfsHandle {
    pub fn new(content: Vec<u8>) -> Self {
        VfsHandle { content, pos: Mutex::new(0) }
    }
}

impl FileHandle for VfsHandle {
    fn read(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        let mut pos = self.pos.lock();
        let remaining = self.content.len().saturating_sub(*pos);
        let count = buf.len().min(remaining);
        if count > 0 {
            buf[..count].copy_from_slice(&self.content[*pos..*pos + count]);
            *pos += count;
        }
        Ok(count)
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd) // read-only
    }

    fn kind(&self) -> &'static str { "vfs_file" }

    fn content_bytes(&self) -> Option<&[u8]> {
        Some(&self.content)
    }
}

// ---------------------------------------------------------------------------
// DirHandle — buffered directory listing

pub struct DirHandle {
    entries: Vec<VfsDirEntry>,
    cursor: Mutex<usize>,
}

impl DirHandle {
    pub fn new(entries: Vec<VfsDirEntry>) -> Self {
        DirHandle { entries, cursor: Mutex::new(0) }
    }

    /// Consume entries starting at cursor, serializing as linux_dirent64 into `buf`.
    /// Returns total bytes written.
    pub fn getdents64(&self, buf: &mut [u8]) -> usize {
        let mut cursor = self.cursor.lock();
        let mut offset = 0usize;

        while *cursor < self.entries.len() {
            let entry = &self.entries[*cursor];
            let name_bytes = entry.name.as_bytes();
            // linux_dirent64: d_ino(8) + d_off(8) + d_reclen(2) + d_type(1) + name + null
            let reclen_raw = 8 + 8 + 2 + 1 + name_bytes.len() + 1;
            let reclen = (reclen_raw + 7) & !7; // align to 8 bytes

            if offset + reclen > buf.len() {
                break;
            }

            // d_ino: fake inode = cursor + 1
            let ino = (*cursor + 1) as u64;
            buf[offset..offset + 8].copy_from_slice(&ino.to_le_bytes());
            // d_off: offset of *next* entry
            let d_off = (*cursor + 1) as u64;
            buf[offset + 8..offset + 16].copy_from_slice(&d_off.to_le_bytes());
            // d_reclen
            buf[offset + 16..offset + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
            // d_type: DT_DIR=4, DT_REG=8
            buf[offset + 18] = if entry.is_dir { 4 } else { 8 };
            // d_name (null-terminated)
            let name_start = offset + 19;
            buf[name_start..name_start + name_bytes.len()].copy_from_slice(name_bytes);
            buf[name_start + name_bytes.len()] = 0;
            // Zero padding
            let pad_start = name_start + name_bytes.len() + 1;
            for i in pad_start..offset + reclen {
                buf[i] = 0;
            }

            offset += reclen;
            *cursor += 1;
        }

        offset
    }
}

impl FileHandle for DirHandle {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, FileError> {
        Err(FileError::IsDirectory)
    }

    fn write(&self, _buf: &[u8]) -> Result<usize, FileError> {
        Err(FileError::BadFd)
    }

    fn kind(&self) -> &'static str { "dir" }

    fn getdents64(&self, buf: &mut [u8]) -> Result<usize, FileError> {
        Ok(DirHandle::getdents64(self, buf))
    }
}
