# Rust Cross-Compilation for ostoo Userspace

Build Rust userspace programs natively on macOS, producing static x86_64 ELF
binaries that run on the ostoo kernel.

## Architecture

```
user-rs/                        # Separate Cargo workspace
├── Cargo.toml                  # workspace: rt, hello-rs, hello-std
├── .cargo/config.toml          # custom target, build-std
├── x86_64-ostoo-user.json      # custom target spec (no CRT objects)
├── rt/                         # ostoo-rt runtime crate
│   ├── Cargo.toml              # features: no_std (default)
│   └── src/
│       ├── lib.rs              # _start, panic handler, global allocator
│       ├── syscall.rs          # inline-asm SYSCALL wrappers
│       ├── io.rs               # print!/println!/eprint!/eprintln! macros
│       └── alloc_impl.rs       # brk-based bump allocator
├── hello-rs/                   # no_std + alloc example (~5 KiB)
│   ├── Cargo.toml
│   └── src/main.rs
└── hello-std/                  # full std example (~54 KiB)
    ├── Cargo.toml
    └── src/main.rs
sysroot/                        # musl sysroot (extracted, gitignored)
└── x86_64-ostoo-user/
    ├── lib/                    # libc.a, crt*.o, libunwind.a stub
    └── include/                # C headers
```

### Why a separate workspace?

The kernel uses a custom target (`x86_64-os.json`) that disables SSE and the
red zone. Userspace needs standard x86_64 ABI with SSE and red zone enabled.
A separate workspace with its own `.cargo/config.toml` avoids target conflicts.

### Custom target (`x86_64-ostoo-user.json`)

Based on `x86_64-unknown-linux-musl` but with empty `pre-link-objects` and
`post-link-objects` — we provide our own `_start` in `ostoo-rt` instead of
using musl's CRT startup files. Has `crt-static-default: true` so that the
`libc` crate links `libc.a` statically when building `std`.

## Building

```bash
# One-time: extract musl sysroot from the ostoo-compiler Docker image
scripts/extract-musl-sysroot.sh

# Build and deploy to user/ (visible at /host/ in guest via virtio-9p)
# (automatically calls extract-musl-sysroot.sh if needed)
scripts/user-rs-build.sh

# Or manually:
cd user-rs
cargo build --release
```

Uses `build-std` to compile `std` and `panic_abort` (and transitively `core`,
`alloc`, `compiler_builtins`, `libc`, `unwind`) from source. Links against
`libc.a` from the musl sysroot. Requires the nightly toolchain with `rust-src`
component (already in `rust-toolchain.toml`).

**Note:** packages must be built separately (`-p hello-rs`, then `-p hello-std`)
because Cargo feature unification would otherwise merge `ostoo-rt`'s `no_std`
feature across the workspace, causing duplicate `#[panic_handler]` errors. The
build script handles this automatically.

### Release profile

`opt-level = "s"`, `lto = true`, `panic = "abort"`, `strip = true` — produces
small binaries (the hello world example is ~4.6 KiB).

## Runtime crate (`ostoo-rt`)

`ostoo-rt` has a `no_std` feature (enabled by default). With `no_std`, it
provides a panic handler, global allocator, and OOM handler. Without it
(for `std` programs), it provides only `_start` and syscall wrappers.

### Tier 1: `no_std` + `alloc` programs

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

Depend on `ostoo-rt` with default features (includes `no_std`).

### Tier 2: `std` programs

```rust
#![feature(restricted_std)]
#![no_main]
extern crate ostoo_rt;

use std::collections::HashMap;

#[no_mangle]
fn main() -> i32 {
    println!("Hello from Rust std on ostoo!");
    let mut map = HashMap::new();
    map.insert("key", 42);
    println!("HashMap works: {:?}", map);
    0
}
```

Depend on `ostoo-rt` with `default-features = false` (disables `no_std` so
`std`'s panic handler and allocator are used instead). The
`#![feature(restricted_std)]` attribute is required for custom JSON targets.

### What `ostoo-rt` provides

- **`_start` entry point** (always, but behaviour differs by mode):
  - `no_std`: reads `argc`/`argv` from the stack, calls `_start_rust` → user's
    `main() -> i32` directly.
  - `std`: extracts `argc`/`argv` from the stack and calls musl's
    `__libc_start_main(main, argc, argv, ...)` which initializes libc
    (TLS via `arch_prctl`, stdio, locale, auxvec parsing) before calling
    `main(argc, argv, envp)`. This is essential — without libc init, musl's
    `write()` and other functions fault on uninitialised TLS.

- **Syscall wrappers** (always) — `syscall0` through `syscall4` via inline asm
  (SYSCALL instruction). Typed wrappers: `write`, `read`, `open`, `close`,
  `exit`, `brk`, `getcwd`, `chdir`, `getdents64`, `wait4`.

- **`print!`/`println!`/`eprint!`/`eprintln!` macros** (always) — write to
  fd 1/2 via `core::fmt::Write`. In `std` mode, prefer `std::println!` instead.

- **Global allocator** (`no_std` only) — brk-based bump allocator.

- **Panic handler** (`no_std` only) — prints panic info to stderr, exits 101.

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

## Musl sysroot

The musl sysroot provides `libc.a` for Rust's `std` to link against. It is
extracted from the `ostoo-compiler` Docker image (which builds musl 1.2.5 via
crosstool-ng).

```bash
# Extract sysroot (skips if already present)
scripts/extract-musl-sysroot.sh
```

The sysroot is placed at `sysroot/x86_64-ostoo-user/` and is gitignored. It
contains:
- `lib/libc.a` — musl static C library
- `lib/crt1.o`, `crti.o`, `crtn.o` — CRT objects (not linked by default; our
  target spec has empty `pre-link-objects`)
- `lib/libunwind.a` — empty stub (satisfies `unwind` crate's `#[link]`; musl's
  unwinder is in `libc.a`, and with `panic=abort` unwinding is never invoked)
- `include/` — C headers

The cargo config passes `-Lnative=../sysroot/x86_64-ostoo-user/lib` to the
linker so it can find `libc.a` and `libunwind.a`.

## Binary sizes

| Program | Mode | Size |
|---|---|---|
| hello-rs | no_std + alloc | ~5 KiB |
| hello-std | full std | ~55 KiB |

Both use `opt-level = "s"`, LTO, `panic = "abort"`, and stripping.
