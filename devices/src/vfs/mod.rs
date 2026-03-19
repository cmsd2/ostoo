use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::sync::Arc;
use lazy_static::lazy_static;
use spin::Mutex;

pub mod exfat_vfs;
pub mod plan9_vfs;
pub mod proc_vfs;

pub use exfat_vfs::ExfatVfs;
pub use plan9_vfs::Plan9Vfs;
pub use proc_vfs::ProcVfs;

// ---------------------------------------------------------------------------
// Public types

#[derive(Debug, Clone)]
pub struct VfsDirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   u64,
}

#[derive(Debug)]
pub enum VfsError {
    IoError,
    NotFound,
    NotAFile,
    NotADirectory,
    FileTooLarge,
    NoFilesystem,
}

// ---------------------------------------------------------------------------
// Enum dispatch — no Pin<Box<dyn Future>> needed

pub enum AnyVfs {
    Exfat(ExfatVfs),
    Plan9(Plan9Vfs),
    Proc(ProcVfs),
}

impl AnyVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match self {
            AnyVfs::Exfat(fs) => fs.list_dir(path).await,
            AnyVfs::Plan9(fs) => fs.list_dir(path).await,
            AnyVfs::Proc(fs)  => fs.list_dir(path).await,
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        match self {
            AnyVfs::Exfat(fs) => fs.read_file(path).await,
            AnyVfs::Plan9(fs) => fs.read_file(path).await,
            AnyVfs::Proc(fs)  => fs.read_file(path).await,
        }
    }

    pub fn fs_type(&self) -> &'static str {
        match self {
            AnyVfs::Exfat(_) => "exfat",
            AnyVfs::Plan9(_) => "9p",
            AnyVfs::Proc(_)  => "proc",
        }
    }
}

// ---------------------------------------------------------------------------
// Mount table — entries sorted longest-mountpoint-first

lazy_static! {
    static ref MOUNTS: Mutex<Vec<(String, Arc<AnyVfs>)>> = Mutex::new(Vec::new());
}

/// Register (or replace) a filesystem at `mountpoint`.
pub fn mount(mountpoint: &str, fs: AnyVfs) {
    let mut mounts = MOUNTS.lock();
    mounts.retain(|(mp, _)| mp != mountpoint);
    mounts.push((mountpoint.to_string(), Arc::new(fs)));
    // Longest mountpoint first so the linear scan finds the best match.
    mounts.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));
}

/// Resolve `path` to the filesystem that owns it, returning the filesystem
/// and the path relative to its mount root.
///
/// Lock is released before returning — it is never held across an await point.
fn resolve(path: &str) -> Option<(Arc<AnyVfs>, String)> {
    let mounts = MOUNTS.lock();
    for (mp, fs) in mounts.iter() {
        if mp == "/" {
            // Root mount: pass the full path through unchanged.
            return Some((Arc::clone(fs), path.to_string()));
        } else if path == mp.as_str() {
            // Exact match: the path names the mountpoint itself → fs root.
            return Some((Arc::clone(fs), "/".to_string()));
        } else if path.starts_with(mp.as_str())
            && path.as_bytes().get(mp.len()) == Some(&b'/')
        {
            // Prefix match: strip the mountpoint prefix.
            let rel = path[mp.len()..].to_string();
            return Some((Arc::clone(fs), rel));
        }
    }
    None
}

/// List a directory through the VFS.  `path` must be absolute.
pub async fn list_dir(path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
    let (fs, rel) = resolve(path).ok_or(VfsError::NoFilesystem)?;
    fs.list_dir(&rel).await
}

/// Read a file through the VFS.  `path` must be absolute.
pub async fn read_file(path: &str) -> Result<Vec<u8>, VfsError> {
    let (fs, rel) = resolve(path).ok_or(VfsError::NoFilesystem)?;
    fs.read_file(&rel).await
}

/// Invoke `f` with a snapshot of the current mount table (for listing).
pub fn with_mounts<F: FnOnce(&[(String, Arc<AnyVfs>)])>(f: F) {
    let mounts = MOUNTS.lock();
    f(&mounts);
}
