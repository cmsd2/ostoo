# Cross-Compiling for x86_64 on a Non-x86 Host

This project targets x86_64 bare metal but can be built and run on any host architecture
(including aarch64-apple-darwin, i.e. Apple Silicon Macs). This document explains how that
works.

## Overview

The kernel is compiled for a custom `x86_64-os` target using Rust's cross-compilation
support. QEMU provides x86_64 emulation at runtime. The host machine never executes the
kernel code directly.

## Toolchain (`rust-toolchain.toml`)

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "llvm-tools"]
```

- **nightly** is required for the `-Z build-std` unstable feature (see below).
- **rust-src** provides the standard library source, which is needed to compile `core`,
  `alloc`, and `compiler_builtins` from source for the custom target.
- **llvm-tools** provides `llvm-objcopy` and related tools used by `bootimage` when
  assembling the final disk image.

Rustup downloads a pre-built nightly toolchain for the *host* architecture. The host
toolchain is only used to drive the build; the kernel itself is compiled to x86_64 object
files by rustc's bundled LLVM backend regardless of host architecture.

## Custom Target Spec (`x86_64-os.json`)

Rust's built-in targets assume a host OS. For a bare-metal kernel we need a custom target.
The file `x86_64-os.json` at the workspace root defines it:

```json
{
    "llvm-target": "x86_64-unknown-none",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    "arch": "x86_64",
    "target-endian": "little",
    "target-pointer-width": 64,
    "target-c-int-width": 32,
    "os": "none",
    "executables": true,
    "linker-flavor": "ld.lld",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": true,
    "rustc-abi": "softfloat",
    "features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float"
}
```

Key fields:

| Field | Value | Reason |
|---|---|---|
| `llvm-target` | `x86_64-unknown-none` | Bare metal; no OS assumed by LLVM |
| `data-layout` | LLVM datalayout string | Must exactly match LLVM's own layout for this triple; confirmed via `rustc +nightly --print target-spec-json --target x86_64-unknown-none -Z unstable-options` |
| `linker-flavor` / `linker` | `ld.lld` / `rust-lld` | Uses LLVM's cross-capable linker bundled with rustc; no host `ld` or cross-linker needed |
| `disable-redzone` | `true` | Required for kernel interrupt handlers; the red zone is an x86_64 ABI optimisation that is unsafe when interrupts can fire at any stack pointer |
| `rustc-abi` | `softfloat` | Tells rustc that this target intentionally violates the standard x86_64 ABI's SSE requirement. Without this, rustc refuses to compile when SSE is disabled |
| `features` | `-mmx,-sse,...,+soft-float` | Disables SIMD/SSE in generated code (unsafe in kernel context without SSE state saving) and enables soft-float emulation instead |

### Why `rustc-abi: softfloat` is needed

The standard x86_64 System V ABI mandates SSE2 support. If you disable SSE features in a
custom target spec, rustc rejects the build with:

```
error: target feature 'sse2' is required by the ABI but gets disabled
```

The `rustc-abi: softfloat` field is an escape hatch for kernel targets: it tells rustc to
use a different ABI variant (one that does not assume SSE), suppressing the error. This is
the same mechanism used internally by Rust's `x86_64-unknown-none` tier-2 target.

## Cargo Configuration (`.cargo/config.toml`)

```toml
[build]
target = "x86_64-os.json"

[unstable]
build-std = ["core", "compiler_builtins", "alloc"]
build-std-features = ["compiler-builtins-mem"]
json-target-spec = true
```

- **`target`**: Makes every `cargo build` in this workspace default to the custom target.
  No `--target` flag is required on the command line.
- **`build-std`**: Compiles `core`, `compiler_builtins`, and `alloc` from source for the
  custom target. This is necessary because Cargo ships pre-compiled standard library crates
  only for known built-in targets; a custom JSON target has no pre-built sysroot.
- **`build-std-features = ["compiler-builtins-mem"]`**: Builds the memory intrinsics
  (`memcpy`, `memset`, etc.) into `compiler_builtins` rather than relying on a C runtime,
  which does not exist in a bare-metal environment.
- **`json-target-spec = true`**: Unlocks support for `.json` custom target files in current
  Cargo nightly. Without this flag, Cargo rejects `.json` target specs.

## Bootloader and Bootimage

The kernel ELF is combined with a real-mode x86 bootloader by the `bootimage` tool:

```
cargo bootimage --manifest-path kernel/Cargo.toml
```

This produces `target/x86_64-os/debug/bootimage-kernel.bin`, a raw x86 disk image.

The bootloader crate (`bootloader = "0.9.x"`) includes its own target spec
(`x86_64-bootloader.json`) and declares `build-std = "core"` in its own Cargo metadata.
`bootimage` picks this up and compiles the bootloader from source using `-Z build-std`,
just like the kernel — no separate cross-toolchain or `cargo-xbuild` is needed.

### Why bootloader 0.9.x and not 0.8.x

`bootloader 0.8.x` was released before `-Z build-std` became stable enough for the
bootloader's own build. It fell back to `cargo xbuild`, a now-deprecated wrapper tool.
The 0.9.x line added `build-std` to its metadata and has been actively maintained for
compatibility with current Rust nightly (data-layout changes, `rustc-abi: softfloat`,
integer target fields, `json-target-spec`). The kernel-facing API (`entry_point!`,
`BootInfo`) is the same in both series.

## Running Under QEMU

```
cargo bootimage run --manifest-path kernel/Cargo.toml
```

or directly:

```
qemu-system-x86_64 -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin -serial stdio
```

QEMU provides full x86_64 CPU emulation. The run-time arguments are configured in
`kernel/Cargo.toml` under `[package.metadata.bootimage]`.

## Summary

| Concern | Solution |
|---|---|
| Compiling x86_64 code on ARM | rustc's LLVM backend handles any target regardless of host |
| Linking for bare metal | `rust-lld` (cross-capable, bundled with rustc) |
| No pre-built sysroot for custom target | `-Z build-std` compiles `core`/`alloc` from source |
| No OS or C runtime | `compiler-builtins-mem` provides memory intrinsics |
| SSE disabled but ABI expects it | `rustc-abi: softfloat` in target spec |
| Bootable disk image | `bootimage` + `bootloader 0.9.x` (self-contained, no xbuild) |
| Running the kernel | `qemu-system-x86_64` on any host |
