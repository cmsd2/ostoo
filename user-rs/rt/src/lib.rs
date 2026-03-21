//! ostoo userspace runtime — entry point, syscall wrappers, and print macros.
//!
//! With the default `no_std` feature: also provides a panic handler,
//! brk-based global allocator, and OOM handler. `_start` calls `main()`
//! directly.
//!
//! Without `no_std` (for `std` programs): `_start` calls musl's
//! `__libc_start_main` to initialize TLS, stdio, etc. before `main()`.

#![cfg_attr(feature = "no_std", no_std)]
#![cfg_attr(feature = "no_std", feature(alloc_error_handler))]

#[cfg(feature = "no_std")]
extern crate alloc;

pub mod syscall;
pub mod io;

#[cfg(feature = "no_std")]
mod alloc_impl;

// ---- Global allocator (no_std only) ----

#[cfg(feature = "no_std")]
#[global_allocator]
static ALLOCATOR: alloc_impl::BrkAllocator = alloc_impl::BrkAllocator::new();

// ---- Entry point ----

/// ELF entry point.
///
/// - `no_std` mode: reads argc/argv from stack, calls user's `main() -> i32`.
/// - `std` mode: passes initial RSP to musl's `__libc_start_main` which
///   initializes libc (TLS, stdio, locale), then calls `main(argc, argv, envp)`.
#[cfg(feature = "no_std")]
#[no_mangle]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov rdi, [rsp]",       // rdi = argc
        "lea rsi, [rsp + 8]",   // rsi = &argv[0]
        "and rsp, -16",         // align stack to 16 bytes
        "call {start_rust}",
        "ud2",
        start_rust = sym _start_rust,
    );
}

#[cfg(not(feature = "no_std"))]
#[no_mangle]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // musl's __libc_start_main has the glibc-compatible signature:
        //   (main_fn, argc, argv, init, fini, rtld_fini)
        // Extract argc/argv from the stack and pass main as a function pointer.
        "mov rsi, [rsp]",       // rsi = argc
        "lea rdx, [rsp + 8]",   // rdx = argv
        "lea rdi, [rip + {main}]", // rdi = &main (function pointer)
        "xor ecx, ecx",         // rcx = init (NULL)
        "xor r8d, r8d",         // r8  = fini (NULL)
        "xor r9d, r9d",         // r9  = rtld_fini (NULL)
        "and rsp, -16",
        "call {libc_start}",
        "ud2",
        main = sym main,
        libc_start = sym __libc_start_main,
    );
}

#[cfg(feature = "no_std")]
extern "C" {
    /// User-provided main function (no_std signature).
    fn main() -> i32;
}

#[cfg(not(feature = "no_std"))]
extern "C" {
    /// User-provided main function (std/libc signature).
    fn main(argc: i32, argv: *const *const u8, envp: *const *const u8) -> i32;
    /// musl libc startup — initializes TLS, stdio, then calls main(argc, argv, envp).
    fn __libc_start_main(
        main: unsafe extern "C" fn(i32, *const *const u8, *const *const u8) -> i32,
        argc: i32,
        argv: *const *const u8,
        init: usize,
        fini: usize,
        rtld_fini: usize,
    ) -> !;
}

#[cfg(feature = "no_std")]
#[no_mangle]
unsafe extern "C" fn _start_rust(_argc: u64, _argv: *const *const u8) -> ! {
    let code = main();
    syscall::exit(code);
}

// ---- Panic handler (no_std only) ----

#[cfg(feature = "no_std")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    eprintln!("panic: {}", info);
    syscall::exit(101);
}

// ---- OOM handler (no_std only) ----

#[cfg(feature = "no_std")]
#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
    eprintln!("out of memory");
    syscall::exit(102);
}
