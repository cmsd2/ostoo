use alloc::string::ToString;
use alloc::vec::Vec;

use super::{VfsDirEntry, VfsError};

mod cpuinfo;
mod drivers;
mod idt;
mod ioapic;
mod lapic;
mod maps;
mod meminfo;
mod memmap;
mod pci;
mod pmap;
mod tasks;
mod threads;
mod uptime;

pub struct ProcVfs;

impl ProcVfs {
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsDirEntry>, VfsError> {
        match path {
            "/" => Ok(alloc::vec![
                VfsDirEntry { name: "cpuinfo".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "drivers".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "idt".to_string(),      is_dir: false, size: 0 },
                VfsDirEntry { name: "ioapic".to_string(),   is_dir: false, size: 0 },
                VfsDirEntry { name: "lapic".to_string(),    is_dir: false, size: 0 },
                VfsDirEntry { name: "maps".to_string(),     is_dir: false, size: 0 },
                VfsDirEntry { name: "meminfo".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "memmap".to_string(),   is_dir: false, size: 0 },
                VfsDirEntry { name: "pci".to_string(),      is_dir: false, size: 0 },
                VfsDirEntry { name: "pmap".to_string(),     is_dir: false, size: 0 },
                VfsDirEntry { name: "tasks".to_string(),    is_dir: false, size: 0 },
                VfsDirEntry { name: "threads".to_string(),  is_dir: false, size: 0 },
                VfsDirEntry { name: "uptime".to_string(),   is_dir: false, size: 0 },
            ]),
            _ => Err(VfsError::NotFound),
        }
    }

    pub async fn read_file(&self, path: &str, caller_pid: libkernel::process::ProcessId) -> Result<Vec<u8>, VfsError> {
        match path {
            "/tasks"   => Ok(tasks::generate().into_bytes()),
            "/uptime"  => Ok(uptime::generate().into_bytes()),
            "/drivers" => Ok(drivers::generate().into_bytes()),
            "/threads" => Ok(threads::generate().into_bytes()),
            "/meminfo" => Ok(meminfo::generate().into_bytes()),
            "/memmap"  => Ok(memmap::generate().into_bytes()),
            "/cpuinfo" => Ok(cpuinfo::generate().into_bytes()),
            "/pmap"    => Ok(pmap::generate().into_bytes()),
            "/idt"     => Ok(idt::generate().into_bytes()),
            "/pci"     => Ok(pci::generate().into_bytes()),
            "/lapic"   => Ok(lapic::generate().into_bytes()),
            "/maps"    => Ok(maps::generate(caller_pid).into_bytes()),
            "/ioapic"  => Ok(ioapic::generate().into_bytes()),
            _ => Err(VfsError::NotFound),
        }
    }
}
