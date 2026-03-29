//! execve(2) syscall implementation.

use alloc::vec::Vec;

use crate::elf_loader;
use crate::errno;
use crate::user_mem::{read_user_string, read_user_string_array};
use libkernel::elf;
use libkernel::memory::with_memory;
use libkernel::process;
use libkernel::task::scheduler;
use x86_64::VirtAddr;

pub fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> i64 {
    // 1. Copy all arguments from userspace before we destroy the address space.
    let path = match read_user_string(path_ptr, 4096) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let argv = match read_user_string_array(argv_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let envp = match read_user_string_array(envp_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let resolved = crate::syscalls::resolve_user_path(&path);

    // 2. Read ELF from VFS.
    let pid = libkernel::process::current_pid();
    let elf_data = match crate::syscalls::vfs_read_file(&resolved, pid) {
        Ok(data) => data,
        Err(_) => return -errno::ENOENT,
    };

    // 3. Parse ELF.
    let info = match elf::parse(&elf_data) {
        Ok(info) => info,
        Err(_) => return -errno::ENOEXEC,
    };

    if info.segments.is_empty() {
        return -errno::ENOEXEC;
    }

    // 4a. Save old address space info before creating new one.
    let (old_pml4_phys, old_pml4_shared) = process::with_process_ref(pid, |p| {
        (p.pml4_phys, p.pml4_shared)
    }).unwrap_or((x86_64::PhysAddr::new(0), false));

    // 4. Create fresh PML4 and map segments + stack.
    let (new_pml4_phys, stack_kernel_base) = match elf_loader::load_elf_address_space(&elf_data, &info) {
        Ok(v) => v,
        Err(_) => return -errno::ENOMEM,
    };

    // 5. Compute brk_base.
    let brk_base = elf_loader::compute_brk_base(&info);

    // 6. Build initial stack with argv, envp, auxv.
    let argv_slices: Vec<Vec<u8>> = argv.iter().map(|s| s.as_bytes().to_vec()).collect();
    let envp_slices: Vec<Vec<u8>> = envp.iter().map(|s| s.as_bytes().to_vec()).collect();
    let argv_refs: Vec<&[u8]> = argv_slices.iter().map(|v| v.as_slice()).collect();
    let envp_refs: Vec<&[u8]> = envp_slices.iter().map(|v| v.as_slice()).collect();

    let user_rsp = crate::spawn::build_initial_stack(
        stack_kernel_base,
        elf_loader::ELF_STACK_VIRT,
        elf_loader::ELF_STACK_SIZE,
        &info,
        &argv_refs,
        &envp_refs,
    );

    // 7. Update Process.
    let pid = process::current_pid();
    let vfork_parent_thread = process::with_process(pid, |p| {
        p.pml4_phys = new_pml4_phys;
        p.entry_point = info.entry;
        p.user_stack_top = user_rsp;
        p.brk_base = brk_base;
        p.brk_current = brk_base;
        p.vma_map.clear();
        p.pml4_shared = false;
        p.close_cloexec_fds();
        p.vfork_parent_thread.take()
    });

    // 8. Switch to new address space and free old one.
    scheduler::set_current_cr3(new_pml4_phys.as_u64());
    unsafe { libkernel::memory::switch_address_space(new_pml4_phys); }

    // Free old address space (CR3 already points to new PML4, so flush_tlb=false).
    // Skip if the old PML4 was shared with the parent (CLONE_VM/vfork).
    if !old_pml4_shared && old_pml4_phys.as_u64() != 0 {
        with_memory(|mem| {
            mem.cleanup_user_address_space(old_pml4_phys, false);
        });
    }

    // 9. If vfork child, unblock parent and become the foreground process.
    if let Some(Some(thread_idx)) = vfork_parent_thread {
        scheduler::unblock(thread_idx);
        libkernel::console::set_foreground(pid);
    }

    libkernel::serial_println!("[execve] pid={} path={} entry={:#x} rsp={:#x} pml4={:#x}",
        pid.as_u64(), resolved, info.entry, user_rsp, new_pml4_phys.as_u64());

    // 10. Jump to new userspace — never returns.
    let user_cs = libkernel::gdt::user_code_selector().0 as u64;
    let user_ss = libkernel::gdt::user_data_selector().0 as u64;
    let per_cpu = libkernel::syscall::per_cpu_addr();
    let kernel_stack_top = process::with_process_ref(pid, |p| p.kernel_stack_top)
        .unwrap_or(0);

    // Reset FS_BASE (TLS) — the new program's libc will set it up.
    unsafe { libkernel::msr::write_fs_base(0); }

    // Set up TSS and PER_CPU for this process.
    libkernel::gdt::set_kernel_stack(VirtAddr::new(kernel_stack_top));
    libkernel::syscall::set_kernel_rsp(kernel_stack_top);

    unsafe {
        scheduler::jump_to_userspace(
            info.entry, user_rsp, new_pml4_phys.as_u64(),
            user_cs, user_ss, per_cpu, 0, 0,
        );
    }
}
