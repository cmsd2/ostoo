use alloc::vec::Vec;

use super::{VfsDirEntry, VfsError};
use crate::virtio::exfat::{self, ExfatError};
pub use crate::virtio::exfat::BlkInbox;

// ---------------------------------------------------------------------------

pub struct ExfatVfs {
    inbox: BlkInbox,
}

impl ExfatVfs {
    pub fn new(inbox: BlkInbox) -> Self {
        Self { inbox }
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        let vol = exfat::open_exfat(&self.inbox).await.map_err(map_err)?;
        let entries = exfat::list_dir(&vol, &self.inbox, path).await.map_err(map_err)?;
        Ok(entries.into_iter().map(|e| VfsDirEntry {
            name:   e.name,
            is_dir: e.is_dir,
            size:   e.size,
        }).collect())
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let vol = exfat::open_exfat(&self.inbox).await.map_err(map_err)?;
        exfat::read_file(&vol, &self.inbox, path).await.map_err(map_err)
    }
}

fn map_err(e: ExfatError) -> VfsError {
    match e {
        ExfatError::NoDevice
        | ExfatError::IoError
        | ExfatError::NotExfat
        | ExfatError::UnknownPartitionLayout => VfsError::IoError,
        ExfatError::PathNotFound  => VfsError::NotFound,
        ExfatError::NotAFile      => VfsError::NotAFile,
        ExfatError::NotADirectory => VfsError::NotADirectory,
        ExfatError::FileTooLarge  => VfsError::FileTooLarge,
    }
}
