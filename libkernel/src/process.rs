use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::structures::paging::PageTableFlags;

use crate::file::{FileError, FdEntry, FdObject, FD_CLOEXEC};

// ---------------------------------------------------------------------------
// VMA (Virtual Memory Area)

/// Linux mmap protection flags.
pub const PROT_NONE:  u32 = 0x0;
pub const PROT_READ:  u32 = 0x1;
pub const PROT_WRITE: u32 = 0x2;
pub const PROT_EXEC:  u32 = 0x4;

/// Linux mmap flags.
pub const MAP_PRIVATE:   u32 = 0x02;
pub const MAP_FIXED:     u32 = 0x10;
pub const MAP_ANONYMOUS: u32 = 0x20;

/// A virtual memory area tracked per-process.
#[derive(Debug, Clone)]
pub struct Vma {
    pub start: u64,        // page-aligned start address
    pub len: u64,          // page-aligned length
    pub prot: u32,         // PROT_READ | PROT_WRITE | PROT_EXEC
    pub flags: u32,        // MAP_PRIVATE | MAP_ANONYMOUS etc.
    pub fd: Option<usize>, // file descriptor (Phase 5)
    pub offset: u64,       // file offset (Phase 5)
}

impl Vma {
    /// Translate VMA protection flags to x86-64 page table flags.
    pub fn page_table_flags(&self) -> PageTableFlags {
        if self.prot == PROT_NONE {
            // No PRESENT bit — any access will fault.
            return PageTableFlags::USER_ACCESSIBLE;
        }
        let mut f = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if self.prot & PROT_WRITE != 0 {
            f |= PageTableFlags::WRITABLE;
        }
        if self.prot & PROT_EXEC == 0 {
            f |= PageTableFlags::NO_EXECUTE;
        }
        f
    }
}

// ---------------------------------------------------------------------------
// ProcessId

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(u64);

impl ProcessId {
    /// Sentinel value representing kernel threads (no process).
    pub const KERNEL: ProcessId = ProcessId(0);

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn from_raw(val: u64) -> Self {
        ProcessId(val)
    }
}

// ---------------------------------------------------------------------------
// Process state

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Zombie,
}

// ---------------------------------------------------------------------------
// Process struct

pub struct Process {
    pub pid: ProcessId,
    pub state: ProcessState,
    pub pml4_phys: PhysAddr,
    /// Heap-allocated kernel stack (64 KiB). Owned here, not by the scheduler.
    /// Kept alive so the memory isn't freed; the stack is accessed via raw pointer.
    #[allow(dead_code)]
    kernel_stack: Vec<u8>,
    /// Cached top of `kernel_stack`, 16-byte aligned.
    pub kernel_stack_top: u64,
    pub entry_point: u64,
    pub user_stack_top: u64,
    /// Index of this process's thread in the scheduler's thread vec.
    pub thread_idx: Option<usize>,
    pub exit_code: Option<i32>,
    /// Page-aligned end of the highest PT_LOAD segment (initial program break).
    pub brk_base: u64,
    /// Current program break (starts == brk_base).
    pub brk_current: u64,
    /// Bump-down pointer for anonymous mmap allocations.
    pub mmap_next: u64,
    /// VMA map: base address → VMA descriptor.
    pub vma_map: BTreeMap<u64, Vma>,
    /// Per-process file descriptor table.
    pub fd_table: Vec<Option<FdEntry>>,
    /// Current working directory (absolute path).
    pub cwd: String,
    /// Parent process ID (KERNEL for top-level processes).
    pub parent_pid: ProcessId,
    /// Scheduler thread index to wake when a child exits (for waitpid).
    pub wait_thread: Option<usize>,
    /// For vfork children: parent's thread index to unblock on execve/_exit.
    pub vfork_parent_thread: Option<usize>,
    /// True when this process shares its PML4 with the parent (CLONE_VM).
    /// Cleanup must not free the PML4 or its pages in this case.
    pub pml4_shared: bool,
}

const PROCESS_KERNEL_STACK_SIZE: usize = crate::consts::KERNEL_STACK_SIZE;

/// Default mmap region start (bump-down from here).
const MMAP_BASE: u64 = 0x0000_4000_0000_0000;

impl Process {
    pub fn new(pml4_phys: PhysAddr, entry_point: u64, user_stack_top: u64, brk_base: u64) -> Self {
        let pid = PROCESSES.alloc_pid();
        let mut kernel_stack = Vec::with_capacity(PROCESS_KERNEL_STACK_SIZE);
        kernel_stack.resize(PROCESS_KERNEL_STACK_SIZE, 0u8);
        let stack_top =
            (kernel_stack.as_ptr() as u64 + kernel_stack.len() as u64) & !0xF;
        Process {
            pid,
            state: ProcessState::Running,
            pml4_phys,
            kernel_stack,
            kernel_stack_top: stack_top,
            entry_point,
            user_stack_top,
            thread_idx: None,
            exit_code: None,
            brk_base,
            brk_current: brk_base,
            mmap_next: MMAP_BASE,
            vma_map: BTreeMap::new(),
            fd_table: crate::file::default_fd_table(),
            cwd: String::from("/"),
            parent_pid: ProcessId::KERNEL,
            wait_thread: None,
            vfork_parent_thread: None,
            pml4_shared: false,
        }
    }

    /// Allocate the lowest available file descriptor for the given object.
    pub fn alloc_fd(&mut self, object: FdObject) -> Result<usize, FileError> {
        self.alloc_fd_with_flags(object, 0)
    }

    /// Allocate the lowest available file descriptor with the given flags.
    pub fn alloc_fd_with_flags(&mut self, object: FdObject, flags: u32) -> Result<usize, FileError> {
        // Search for the first None slot.
        for (i, slot) in self.fd_table.iter().enumerate() {
            if slot.is_none() {
                self.fd_table[i] = Some(FdEntry::from_object(object, flags));
                return Ok(i);
            }
        }
        // No free slot — extend if under limit.
        if self.fd_table.len() < crate::file::MAX_FDS {
            let fd = self.fd_table.len();
            self.fd_table.push(Some(FdEntry::from_object(object, flags)));
            Ok(fd)
        } else {
            Err(FileError::TooManyOpenFiles)
        }
    }

    /// Close a file descriptor.
    pub fn close_fd(&mut self, fd: usize) -> Result<(), FileError> {
        if fd >= self.fd_table.len() {
            return Err(FileError::BadFd);
        }
        match self.fd_table[fd].take() {
            Some(entry) => { entry.object.close(); Ok(()) }
            None => Err(FileError::BadFd),
        }
    }

    /// Close all open file descriptors.  Called during process exit to release
    /// resources (IRQ handles, completion ports, pipes, etc.) promptly.
    pub fn close_all_fds(&mut self) {
        for slot in self.fd_table.iter_mut() {
            if let Some(entry) = slot.take() {
                entry.object.close();
            }
        }
    }

    /// Get the object for an open file descriptor.
    pub fn get_fd(&self, fd: usize) -> Result<FdObject, FileError> {
        self.fd_table.get(fd)
            .and_then(|slot| slot.as_ref().map(|e| e.object.clone()))
            .ok_or(FileError::BadFd)
    }

    /// Get FD flags (e.g. FD_CLOEXEC).
    pub fn get_fd_flags(&self, fd: usize) -> Result<u32, FileError> {
        self.fd_table.get(fd)
            .and_then(|slot| slot.as_ref().map(|e| e.flags))
            .ok_or(FileError::BadFd)
    }

    /// Set FD flags (e.g. FD_CLOEXEC).
    pub fn set_fd_flags(&mut self, fd: usize, flags: u32) -> Result<(), FileError> {
        match self.fd_table.get_mut(fd) {
            Some(Some(entry)) => { entry.flags = flags; Ok(()) }
            _ => Err(FileError::BadFd),
        }
    }

    /// Insert an FdEntry at a specific fd slot, closing any existing fd there.
    /// Extends the table if needed.
    pub fn set_fd(&mut self, fd: usize, entry: FdEntry) {
        // Extend table if necessary.
        while self.fd_table.len() <= fd {
            self.fd_table.push(None);
        }
        // Close existing fd silently.
        if let Some(old) = self.fd_table[fd].take() {
            old.object.close();
        }
        self.fd_table[fd] = Some(entry);
    }

    /// Get a clone of the FdEntry at `fd`.
    pub fn get_fd_entry(&self, fd: usize) -> Result<FdEntry, FileError> {
        self.fd_table.get(fd)
            .and_then(|slot| slot.clone())
            .ok_or(FileError::BadFd)
    }

    /// Remove or split VMAs that overlap `[start .. start+len)`.
    ///
    /// Returns a list of `(page_base, page_count)` ranges whose pages should
    /// be unmapped and freed by the caller (with MEMORY lock held separately).
    pub fn munmap_vmas(&mut self, start: u64, len: u64) -> Vec<(u64, usize)> {
        let end = start + len;
        let page_size = crate::consts::PAGE_SIZE;

        // Collect keys of VMAs that overlap [start, end).
        let overlapping: Vec<u64> = self.vma_map.range(..end)
            .filter(|(_, vma)| vma.start + vma.len > start)
            .map(|(&k, _)| k)
            .collect();

        let mut pages_to_free: Vec<(u64, usize)> = Vec::new();
        let mut to_remove: Vec<u64> = Vec::new();
        let mut to_insert: Vec<(u64, Vma)> = Vec::new();

        for key in &overlapping {
            let vma = &self.vma_map[key];
            let vma_start = vma.start;
            let vma_end = vma.start + vma.len;

            if start <= vma_start && end >= vma_end {
                // Case 1: entire VMA consumed
                let count = (vma.len / page_size) as usize;
                pages_to_free.push((vma_start, count));
                to_remove.push(*key);
            } else if start <= vma_start && end < vma_end {
                // Case 2: front consumed — shrink start
                let removed = end - vma_start;
                let count = (removed / page_size) as usize;
                pages_to_free.push((vma_start, count));
                let mut new_vma = vma.clone();
                new_vma.start = end;
                new_vma.len = vma.len - removed;
                to_remove.push(*key);
                to_insert.push((end, new_vma));
            } else if start > vma_start && end >= vma_end {
                // Case 3: tail consumed — shrink len
                let removed = vma_end - start;
                let count = (removed / page_size) as usize;
                pages_to_free.push((start, count));
                let mut new_vma = vma.clone();
                new_vma.len = start - vma_start;
                to_remove.push(*key);
                to_insert.push((vma_start, new_vma));
            } else {
                // Case 4: middle consumed — split into two fragments
                let removed = end - start;
                let count = (removed / page_size) as usize;
                pages_to_free.push((start, count));

                // Left fragment: [vma_start, start)
                let mut left = vma.clone();
                left.len = start - vma_start;

                // Right fragment: [end, vma_end)
                let mut right = vma.clone();
                right.start = end;
                right.len = vma_end - end;

                to_remove.push(*key);
                to_insert.push((vma_start, left));
                to_insert.push((end, right));
            }
        }

        // Apply mutations
        for key in to_remove {
            self.vma_map.remove(&key);
        }
        for (key, vma) in to_insert {
            self.vma_map.insert(key, vma);
        }

        pages_to_free
    }

    /// Update VMA prot flags in `[start .. start+len)`, splitting VMAs as needed.
    ///
    /// Returns a list of `(page_base, page_count)` ranges whose page table flags
    /// need to be updated by the caller (with MEMORY lock held separately).
    pub fn mprotect_vmas(&mut self, start: u64, len: u64, new_prot: u32) -> Vec<(u64, usize)> {
        let end = start + len;
        let page_size = crate::consts::PAGE_SIZE;

        // Collect keys of VMAs that overlap [start, end).
        let overlapping: Vec<u64> = self.vma_map.range(..end)
            .filter(|(_, vma)| vma.start + vma.len > start)
            .map(|(&k, _)| k)
            .collect();

        let mut pages_to_update: Vec<(u64, usize)> = Vec::new();
        let mut to_remove: Vec<u64> = Vec::new();
        let mut to_insert: Vec<(u64, Vma)> = Vec::new();

        for key in &overlapping {
            let vma = &self.vma_map[key];
            let vma_start = vma.start;
            let vma_end = vma.start + vma.len;

            if start <= vma_start && end >= vma_end {
                // Case 1: entire VMA — update prot in place
                let count = (vma.len / page_size) as usize;
                pages_to_update.push((vma_start, count));
                let mut updated = vma.clone();
                updated.prot = new_prot;
                to_remove.push(*key);
                to_insert.push((vma_start, updated));
            } else if start <= vma_start && end < vma_end {
                // Case 2: front overlap — split front with new_prot, remainder keeps old
                let front_len = end - vma_start;
                let count = (front_len / page_size) as usize;
                pages_to_update.push((vma_start, count));

                let mut front = vma.clone();
                front.len = front_len;
                front.prot = new_prot;

                let mut tail = vma.clone();
                tail.start = end;
                tail.len = vma_end - end;

                to_remove.push(*key);
                to_insert.push((vma_start, front));
                to_insert.push((end, tail));
            } else if start > vma_start && end >= vma_end {
                // Case 3: tail overlap — original shrinks, tail gets new_prot
                let tail_len = vma_end - start;
                let count = (tail_len / page_size) as usize;
                pages_to_update.push((start, count));

                let mut head = vma.clone();
                head.len = start - vma_start;

                let mut tail = vma.clone();
                tail.start = start;
                tail.len = tail_len;
                tail.prot = new_prot;

                to_remove.push(*key);
                to_insert.push((vma_start, head));
                to_insert.push((start, tail));
            } else {
                // Case 4: middle overlap — split into three
                let mid_len = end - start;
                let count = (mid_len / page_size) as usize;
                pages_to_update.push((start, count));

                let mut left = vma.clone();
                left.len = start - vma_start;

                let mut mid = vma.clone();
                mid.start = start;
                mid.len = mid_len;
                mid.prot = new_prot;

                let mut right = vma.clone();
                right.start = end;
                right.len = vma_end - end;

                to_remove.push(*key);
                to_insert.push((vma_start, left));
                to_insert.push((start, mid));
                to_insert.push((end, right));
            }
        }

        // Apply mutations
        for key in to_remove {
            self.vma_map.remove(&key);
        }
        for (key, vma) in to_insert {
            self.vma_map.insert(key, vma);
        }

        pages_to_update
    }

    /// Close all file descriptors that have FD_CLOEXEC set.
    pub fn close_cloexec_fds(&mut self) {
        for slot in self.fd_table.iter_mut() {
            if let Some(entry) = slot {
                if entry.flags & FD_CLOEXEC != 0 {
                    entry.object.close();
                    *slot = None;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ProcessManager — encapsulates the global process table and PID tracking

pub struct ProcessManager {
    table: Mutex<BTreeMap<ProcessId, Process>>,
    current_pid: AtomicU64,
    next_pid: AtomicU64,
}

pub static PROCESSES: ProcessManager = ProcessManager {
    table: Mutex::new(BTreeMap::new()),
    current_pid: AtomicU64::new(0),
    next_pid: AtomicU64::new(1),
};

impl ProcessManager {
    fn alloc_pid(&self) -> ProcessId {
        ProcessId(self.next_pid.fetch_add(1, Ordering::Relaxed))
    }

    pub fn insert(&self, mut proc: Process) -> ProcessId {
        let pid = proc.pid;
        // Ensure state is Running on insert.
        proc.state = ProcessState::Running;
        self.table.lock().insert(pid, proc);
        pid
    }

    pub fn current_pid(&self) -> ProcessId {
        ProcessId(self.current_pid.load(Ordering::Relaxed))
    }

    pub fn set_current_pid(&self, pid: ProcessId) {
        self.current_pid.store(pid.0, Ordering::Relaxed);
    }

    /// Run `f` with a mutable reference to the process. Returns `None` if not found.
    pub fn with_process<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Process) -> R,
    {
        let mut table = self.table.lock();
        table.get_mut(&pid).map(f)
    }

    /// Run `f` with an immutable reference to the process.
    pub fn with_process_ref<F, R>(&self, pid: ProcessId, f: F) -> Option<R>
    where
        F: FnOnce(&Process) -> R,
    {
        let table = self.table.lock();
        table.get(&pid).map(f)
    }

    /// Check whether a process exists and is a zombie.
    pub fn is_zombie(&self, pid: ProcessId) -> bool {
        self.table.lock().get(&pid).map_or(false, |p| p.state == ProcessState::Zombie)
    }

    /// Mark the process as a zombie with the given exit code.
    pub fn mark_zombie(&self, pid: ProcessId, code: i32) {
        if let Some(proc) = self.table.lock().get_mut(&pid) {
            proc.state = ProcessState::Zombie;
            proc.exit_code = Some(code);
        }
    }

    /// Remove the process from the table entirely, freeing its kernel stack.
    /// In the future this is where we'd deallocate PML4 and user-space frames.
    pub fn reap(&self, pid: ProcessId) {
        self.table.lock().remove(&pid);
    }

    /// Reap all zombie processes whose scheduler threads are Dead.
    ///
    /// Safe to call from normal kernel context (not ISR).  Frees kernel stacks
    /// and process table entries for processes that have fully exited.
    pub fn reap_zombies(&self) {
        use crate::task::scheduler;

        let zombie_pids: Vec<ProcessId> = {
            let table = self.table.lock();
            table.values()
                .filter(|p| p.state == ProcessState::Zombie)
                .filter(|p| {
                    // Only reap if the scheduler thread is actually Dead,
                    // meaning it has been fully preempted away from.
                    p.thread_idx.map_or(true, |idx| scheduler::is_thread_dead(idx))
                })
                .map(|p| p.pid)
                .collect()
        };
        // Drop the table lock before reaping (reap takes the lock itself).
        for pid in zombie_pids {
            self.reap(pid);
        }
    }

    /// Find a zombie child of `parent_pid` matching `target_pid` (or any if target == -1).
    /// Returns (child_pid, exit_code) if found.
    pub fn find_zombie_child(&self, parent_pid: ProcessId, target_pid: i64) -> Option<(ProcessId, i32)> {
        let table = self.table.lock();
        for p in table.values() {
            if p.parent_pid != parent_pid || p.state != ProcessState::Zombie {
                continue;
            }
            if target_pid == -1 || p.pid.as_u64() == target_pid as u64 {
                return Some((p.pid, p.exit_code.unwrap_or(0)));
            }
        }
        None
    }

    /// Check whether `parent_pid` has any children (zombie or not).
    pub fn has_children(&self, parent_pid: ProcessId) -> bool {
        let table = self.table.lock();
        table.values().any(|p| p.parent_pid == parent_pid)
    }
}

// ---------------------------------------------------------------------------
// Free-function wrappers — delegate to PROCESSES for backward compatibility

pub fn insert(proc: Process) -> ProcessId { PROCESSES.insert(proc) }
pub fn current_pid() -> ProcessId { PROCESSES.current_pid() }
pub fn set_current_pid(pid: ProcessId) { PROCESSES.set_current_pid(pid) }

pub fn with_process<F, R>(pid: ProcessId, f: F) -> Option<R>
where F: FnOnce(&mut Process) -> R {
    PROCESSES.with_process(pid, f)
}

pub fn with_process_ref<F, R>(pid: ProcessId, f: F) -> Option<R>
where F: FnOnce(&Process) -> R {
    PROCESSES.with_process_ref(pid, f)
}

pub fn is_zombie(pid: ProcessId) -> bool { PROCESSES.is_zombie(pid) }
pub fn mark_zombie(pid: ProcessId, code: i32) { PROCESSES.mark_zombie(pid, code) }
pub fn reap(pid: ProcessId) { PROCESSES.reap(pid) }
pub fn reap_zombies() { PROCESSES.reap_zombies() }

pub fn find_zombie_child(parent_pid: ProcessId, target_pid: i64) -> Option<(ProcessId, i32)> {
    PROCESSES.find_zombie_child(parent_pid, target_pid)
}

pub fn has_children(parent_pid: ProcessId) -> bool {
    PROCESSES.has_children(parent_pid)
}
