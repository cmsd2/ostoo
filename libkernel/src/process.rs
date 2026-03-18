use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

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

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

fn alloc_pid() -> ProcessId {
    ProcessId(NEXT_PID.fetch_add(1, Ordering::Relaxed))
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
    /// Tracked (vaddr, len) pairs for mmap regions.
    pub mmap_regions: Vec<(u64, u64)>,
}

const PROCESS_KERNEL_STACK_SIZE: usize = 64 * 1024;

/// Default mmap region start (bump-down from here).
const MMAP_BASE: u64 = 0x0000_4000_0000_0000;

impl Process {
    pub fn new(pml4_phys: PhysAddr, entry_point: u64, user_stack_top: u64, brk_base: u64) -> Self {
        let pid = alloc_pid();
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
            mmap_regions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Global process table

static PROCESS_TABLE: Mutex<BTreeMap<ProcessId, Process>> =
    Mutex::new(BTreeMap::new());

/// PID of the currently running process (0 = kernel thread).
static CURRENT_PID: AtomicU64 = AtomicU64::new(0);

pub fn insert(mut proc: Process) -> ProcessId {
    let pid = proc.pid;
    // Ensure state is Running on insert.
    proc.state = ProcessState::Running;
    PROCESS_TABLE.lock().insert(pid, proc);
    pid
}

pub fn current_pid() -> ProcessId {
    ProcessId(CURRENT_PID.load(Ordering::Relaxed))
}

pub fn set_current_pid(pid: ProcessId) {
    CURRENT_PID.store(pid.0, Ordering::Relaxed);
}

/// Run `f` with a mutable reference to the process. Returns `None` if not found.
pub fn with_process<F, R>(pid: ProcessId, f: F) -> Option<R>
where
    F: FnOnce(&mut Process) -> R,
{
    let mut table = PROCESS_TABLE.lock();
    table.get_mut(&pid).map(f)
}

/// Run `f` with an immutable reference to the process.
pub fn with_process_ref<F, R>(pid: ProcessId, f: F) -> Option<R>
where
    F: FnOnce(&Process) -> R,
{
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(f)
}

/// Mark the process as a zombie with the given exit code.
pub fn mark_zombie(pid: ProcessId, code: i32) {
    if let Some(proc) = PROCESS_TABLE.lock().get_mut(&pid) {
        proc.state = ProcessState::Zombie;
        proc.exit_code = Some(code);
    }
}

/// Remove the process from the table entirely, freeing its kernel stack.
/// In the future this is where we'd deallocate PML4 and user-space frames.
pub fn reap(pid: ProcessId) {
    PROCESS_TABLE.lock().remove(&pid);
}

/// Reap all zombie processes whose scheduler threads are Dead.
///
/// Safe to call from normal kernel context (not ISR).  Frees kernel stacks
/// and process table entries for processes that have fully exited.
pub fn reap_zombies() {
    use crate::task::scheduler;

    let zombie_pids: Vec<ProcessId> = {
        let table = PROCESS_TABLE.lock();
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
        reap(pid);
    }
}
