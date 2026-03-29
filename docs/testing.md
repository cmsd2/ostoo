# Testing

## Overview

The kernel uses Rust's `custom_test_frameworks` feature since the standard test
harness requires `std`.  Tests run inside QEMU in headless mode, communicate
results over the serial port, and signal pass/fail via an ISA debug-exit device.

```
# Kernel crate tests (kernel unit tests + integration tests)
cargo test --manifest-path kernel/Cargo.toml

# libkernel tests (allocator, VGA, path, timer, interrupts, etc.)
cargo test --manifest-path libkernel/Cargo.toml
```

Or via the Makefile:

```
make test
```

Note: `make test` currently runs only the kernel crate tests.  libkernel has
its own test binary (with heap initialization) that must be run separately.

---

## How It Works

### Custom test runner

`libkernel/src/lib.rs` defines the framework:

```rust
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
```

`test_runner` iterates over every `#[test_case]` function, runs it, and writes
results to serial.  On completion it writes to I/O port `0xf4` to exit QEMU:

| Exit code | Meaning |
|-----------|---------|
| `0x10`    | All tests passed |
| `0x11`    | A test panicked |

### QEMU configuration

`kernel/Cargo.toml` configures `bootimage` to launch QEMU with:

```toml
test-args = [
    "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    "-serial", "stdio",
    "-display", "none"
]
test-success-exit-code = 33
test-timeout = 30
```

- **isa-debug-exit** — writing to port `0xf4` terminates QEMU with an exit code
- **serial stdio** — test output appears on the host terminal
- **display none** — headless, no VGA window
- **timeout 30s** — kills stuck tests

### Serial output

Tests print to COM1 (`0x3F8`) via `serial_print!` / `serial_println!` from
`libkernel/src/serial.rs`.  Each test prints its name and `[ok]` on success;
the panic handler prints `[failed]` and the error before exiting.

---

## Test Types

### Unit tests (`#[test_case]`)

Standard tests that run inside the kernel.  When built with `cargo test`, the
kernel initialises GDT, IDT, heap, and memory, then calls `test_main()` which
invokes `test_runner` with all collected test cases.

### Integration tests (`kernel/tests/`)

Each file in `kernel/tests/` compiles as a separate kernel binary with its own
entry point.  `bootimage` boots each one independently in QEMU.

Two integration tests use `harness = false` because they need custom control
flow (e.g. verifying that a panic or exception fires correctly):

```toml
[[test]]
name = "should_panic"
harness = false

[[test]]
name = "stack_overflow"
harness = false
```

---

## Test Inventory

### Unit tests (37 tests)

All unit tests live in libkernel and run via
`cargo test --manifest-path libkernel/Cargo.toml`.

| File | Tests | What they cover |
|------|-------|-----------------|
| `libkernel/src/path.rs` | 13 | normalize and resolve: dots, dotdot, root, relative, absolute |
| `libkernel/src/vga_buffer/mod.rs` | 8 | println output, color encoding, FixedBuf formatting |
| `libkernel/src/md5.rs` | 7 | MD5 hash (RFC 1321 test vectors) |
| `libkernel/src/allocator/mod.rs` | 3 | align_up correctness and boundary conditions |
| `libkernel/src/memory/vmem_allocator.rs` | 3 | Virtual memory allocator state and page tracking |
| `libkernel/src/task/timer.rs` | 2 | Delay struct millisecond/second calculations |
| `libkernel/src/interrupts.rs` | 1 | Breakpoint exception (int3) handling |

### Integration tests (4 binaries)

| File | Harness | What it tests |
|------|---------|---------------|
| `basic_boot.rs` | standard | Kernel boots and VGA println works |
| `heap_allocation.rs` | standard | Box, Vec, and repeated allocation patterns |
| `should_panic.rs` | custom | Panic handler fires and exits correctly |
| `stack_overflow.rs` | custom | Double-fault handler catches stack overflow via IST |

---

## Execution Flow

```
cargo test
  |
  bootimage compiles test binary (kernel + bootloader)
  |
  QEMU boots with isa-debug-exit, serial stdio, no display
  |
  Kernel init: GDT, IDT, heap, memory
  |
  test_main() calls test_runner(&[...])
  |
  For each #[test_case]:
      run test
      serial_println!("test_name... [ok]")
  |
  exit_qemu(Success)  -->  write 0x10 to port 0xf4
  |
  bootimage reads exit code, reports result
```

For `harness = false` tests, the binary manages its own flow and exit.

---

## Key Files

| File | Role |
|------|------|
| `libkernel/src/lib.rs` | `test_runner`, `test_panic_handler`, `QemuExitCode` |
| `libkernel/src/serial.rs` | COM1 serial output (`serial_print!`, `serial_println!`) |
| `kernel/Cargo.toml` | bootimage test-args, exit codes, timeout |
| `.cargo/config.toml` | `bootimage runner`, build target |
| `kernel/tests/` | Integration test binaries |

---

## Adding a New Test

### Unit test

Add `#[test_case]` to a function in any file that has `#[cfg(test)]` access
to the test framework (libkernel modules or kernel crate modules):

```rust
#[test_case]
fn test_something() {
    serial_print!("test_something... ");
    assert_eq!(1 + 1, 2);
    serial_println!("[ok]");
}
```

### Integration test

Create `kernel/tests/my_test.rs` with its own entry point:

```rust
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(libkernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    libkernel::init();
    // ... any additional setup ...
    test_main();
    libkernel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    libkernel::test_panic_handler(info)
}

#[test_case]
fn my_test() {
    // ...
}
```

For tests that need custom panic/exception handling, add `harness = false` to
`kernel/Cargo.toml` and manage the entry point and exit manually.
