//! ostoo userspace runtime — `#![no_std]` entry point, syscall wrappers,
//! panic handler, print macros, and a brk-based global allocator.

#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod syscall;
pub mod io;
mod alloc_impl;

// ---- Global allocator ----

#[global_allocator]
static ALLOCATOR: alloc_impl::BrkAllocator = alloc_impl::BrkAllocator::new();

// ---- Entry point ----

/// ELF entry point. Reads argc/argv from the stack per the Linux/ostoo ABI
/// (matches `build_initial_stack` in osl/src/spawn.rs), then calls the
/// user-defined `main() -> i32`.
#[no_mangle]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // argc is at [rsp], argv is at [rsp+8]
        "mov rdi, [rsp]",       // rdi = argc
        "lea rsi, [rsp + 8]",   // rsi = &argv[0]
        "and rsp, -16",         // align stack to 16 bytes
        "call {start_rust}",
        "ud2",
        start_rust = sym _start_rust,
    );
}

extern "C" {
    /// User-provided main function.
    fn main() -> i32;
}

#[no_mangle]
unsafe extern "C" fn _start_rust(_argc: u64, _argv: *const *const u8) -> ! {
    let code = main();
    syscall::exit(code);
}

// ---- Panic handler ----

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    eprintln!("panic: {}", info);
    syscall::exit(101);
}

// ---- OOM handler ----

#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
    eprintln!("out of memory");
    syscall::exit(102);
}
