use alloc::string::ToString;
use alloc::vec::Vec;

use super::{VfsDirEntry, VfsError};

// ---------------------------------------------------------------------------

pub struct ProcVfs;

impl ProcVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match path {
            "/" => Ok(alloc::vec![
                VfsDirEntry { name: "tasks".to_string(),   is_dir: false, size: 0 },
                VfsDirEntry { name: "uptime".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "drivers".to_string(), is_dir: false, size: 0 },
            ]),
            _ => Err(VfsError::NotFound),
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        match path {
            "/tasks" => {
                let ready   = libkernel::task::executor::ready_count();
                let waiting = libkernel::task::executor::wait_count();
                let s = alloc::format!("ready: {}  waiting: {}\n", ready, waiting);
                Ok(s.into_bytes())
            }
            "/uptime" => {
                let secs =
                    libkernel::task::timer::ticks() / libkernel::task::timer::TICKS_PER_SECOND;
                let s = alloc::format!("{}s\n", secs);
                Ok(s.into_bytes())
            }
            "/drivers" => {
                let mut lines = alloc::string::String::new();
                crate::driver::with_drivers(|name, state| {
                    lines.push_str(name);
                    lines.push_str("  ");
                    lines.push_str(state.as_str());
                    lines.push('\n');
                });
                Ok(lines.into_bytes())
            }
            _ => Err(VfsError::NotFound),
        }
    }
}
