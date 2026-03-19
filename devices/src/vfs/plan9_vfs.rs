//! VFS adapter for 9P2000.L filesystems (host directory sharing via virtio-9p).

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{VfsDirEntry, VfsError};
use crate::virtio::p9::P9Client;
use crate::virtio::p9_proto::P9Error;

pub struct Plan9Vfs {
    client: Arc<P9Client>,
}

impl Plan9Vfs {
    pub fn new(client: Arc<P9Client>) -> Self {
        Self { client }
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        let entries = self.client.list_dir(path).map_err(map_err)?;
        Ok(entries.into_iter().map(|e| {
            // DT_DIR = 4
            let is_dir = e.dtype == 4 || (e.qid.qid_type & 0x80) != 0;
            VfsDirEntry {
                name: e.name,
                is_dir,
                size: 0, // readdir doesn't give us size
            }
        }).collect())
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        self.client.read_file(path).map_err(map_err)
    }
}

fn map_err(e: P9Error) -> VfsError {
    match e {
        P9Error::ServerError(2) => VfsError::NotFound,      // ENOENT
        P9Error::ServerError(20) => VfsError::NotADirectory, // ENOTDIR
        P9Error::ServerError(21) => VfsError::NotAFile,      // EISDIR
        P9Error::ServerError(_) | P9Error::DeviceError => VfsError::IoError,
        P9Error::BufferTooSmall | P9Error::InvalidResponse | P9Error::Utf8Error => VfsError::IoError,
    }
}
