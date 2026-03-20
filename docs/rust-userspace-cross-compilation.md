# Rust Cross-Compilation for ostoo Userspace

Build Rust userspace programs natively on macOS, producing static x86_64 ELF
binaries that run on the ostoo kernel. No Docker, no musl libc dependency.

## Architecture

```
user-rs/                        # Separate Cargo workspace
├── Cargo.toml                  # workspace: rt, hello
├── .cargo/config.toml          # custom target, build-std
├── x86_64-ostoo-user.json      # custom target spec (no CRT objects)
├── rt/                         # ostoo-rt runtime crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # _start, panic handler, global allocator
│       ├── syscall.rs          # inline-asm SYSCALL wrappers
│       ├── io.rs               # print!/println!/eprint!/eprintln! macros
│       └── alloc_impl.rs       # brk-based bump allocator
└── hello-rs/                   # example program
    ├── Cargo.toml
    └── src/main.rs
```

### Why a separate workspace?

The kernel uses a custom target (`x86_64-os.json`) that disables SSE and the
red zone. Userspace needs standard x86_64 ABI with SSE and red zone enabled.
A separate workspace with its own `.cargo/config.toml` avoids target conflicts.

### Custom target (`x86_64-ostoo-user.json`)

Based on `x86_64-unknown-linux-musl` but with empty `pre-link-objects` and
`post-link-objects` — eliminates the CRT startup files (`crt1.o`, `crti.o`,
etc.) that don't exist on macOS cross-compilation. We provide our own `_start`
in `ostoo-rt`.

## Building

```bash
# Build and deploy to user/ (visible at /host/ in guest via virtio-9p)
scripts/user-rs-build.sh

# Or manually:
cd user-rs
cargo build --release
```

Uses `build-std` to compile `core`, `alloc`, and `compiler_builtins` from source
(with `compiler-builtins-mem` for `memcpy`/`memset`). Requires the nightly
toolchain with `rust-src` component (already in `rust-toolchain.toml`).

### Release profile

`opt-level = "s"`, `lto = true`, `panic = "abort"`, `strip = true` — produces
small binaries (the hello world example is ~4.6 KiB).

## Runtime crate (`ostoo-rt`)

Programs depend on `ostoo-rt` and use `#![no_std]` + `#![no_main]`:

```rust
#![no_std]
#![no_main]
extern crate ostoo_rt;
use ostoo_rt::println;

#[no_mangle]
fn main() -> i32 {
    println!("Hello from Rust on ostoo!");
    0
}
```

### What `ostoo-rt` provides

- **`_start` entry point** — naked function that reads `argc`/`argv` from the
  stack (matching `osl/src/spawn.rs:build_initial_stack()` layout), aligns RSP,
  and calls `_start_rust` which invokes the user's `main() -> i32`.

- **Syscall wrappers** — `syscall0` through `syscall4` via inline asm (SYSCALL
  instruction). Typed wrappers: `write`, `read`, `open`, `close`, `exit`, `brk`,
  `getcwd`, `chdir`, `getdents64`, `spawn`, `wait4`.

- **`print!`/`println!`/`eprint!`/`eprintln!` macros** — write to fd 1/2 via
  `core::fmt::Write`.

- **Global allocator** — brk-based bump allocator. Calls `brk(0)` to get the
  heap base, `brk(new_addr)` to grow. No deallocation (sufficient for simple
  programs; can upgrade to `linked_list_allocator` later).

- **Panic handler** — prints panic info to stderr, exits with code 101.

### Adding new programs

1. Create `user-rs/<name>/Cargo.toml` with `ostoo-rt` dependency
2. Add `"<name>"` to workspace members in `user-rs/Cargo.toml`
3. Add the binary name to the deploy loop in `scripts/user-rs-build.sh`

## Verification

```bash
# Binary format check
file user-rs/target/x86_64-ostoo-user/release/hello-rs
# → ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, stripped

# Entry point is _start (verify disassembly shows mov rdi,[rsp]; lea rsi,[rsp+8])
llvm-objdump -d --start-address=<entry> target/x86_64-ostoo-user/release/hello-rs

# No .interp section (no dynamic linker)
llvm-readobj -S target/x86_64-ostoo-user/release/hello-rs | grep -c .interp  # → 0
```

Boot ostoo, then:
```
> spawn /host/hello-rs
Hello from Rust on ostoo!
Heap works: 42
```

## Future: Tier 2 (`std` support)

The current approach is Tier 1: `#![no_std]` + `alloc`. For programs needing
full `std` (e.g., `HashMap`, networking stubs), a Tier 2 approach would build
`std` from source against a patched musl. This is not yet implemented.
