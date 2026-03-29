# Cross-Compiling C Programs for ostoo Userspace

This document explains how to compile static musl-linked x86_64 ELF binaries that
can run as ostoo user-space processes, using the crosstool-ng toolchain inside Docker.

## Prerequisites

- Docker
- The `ctng` Docker image (built from `crosstool/Dockerfile`)
- The compiled toolchain at `/Volumes/crosstool-ng/x-tools/x86_64-unknown-linux-musl`

## Toolchain details

| Component  | Version |
|------------|---------|
| GCC        | 15.2.0  |
| musl       | 1.2.5   |
| binutils   | 2.46.0  |
| Linux headers | 6.18.3 |

Target triple: `x86_64-unknown-linux-musl`

The toolchain produces fully static-linked ELF binaries with no runtime dependencies
(no dynamic linker, no shared libraries).

## Building the toolchain from scratch

If you need to rebuild the toolchain:

```bash
cd crosstool

# Build the Docker image (includes crosstool-ng and the .config)
docker build . -t ctng

# Run the build (output goes to /Volumes/crosstool-ng/x-tools)
./run.sh
```

The build runs inside Docker's case-sensitive overlay filesystem to avoid macOS
case-sensitivity issues with the Linux kernel tarball extraction. Only the output
(`x-tools`) and download cache (`src`) directories are mounted from the host.

## Compiling user programs

### Using the build script (recommended)

The `scripts/user-build.sh` wrapper handles the Docker invocation for you.
Arguments are passed through to `make`:

```bash
./scripts/user-build.sh          # build all .c files in user/src/ → user/bin/
./scripts/user-build.sh clean    # clean build artifacts
./scripts/user-build.sh bin/hello  # build a single target
```

### Manual Docker invocation

If you need to run compiler commands directly:

```bash
docker run --rm \
  -v /Volumes/crosstool-ng/x-tools:/home/ctng/x-tools \
  -v "$(pwd)/user":/home/ctng/user \
  ctng bash -c '
    export PATH="/home/ctng/x-tools/x86_64-unknown-linux-musl/bin:$PATH"
    cd /home/ctng/user
    x86_64-unknown-linux-musl-gcc -static -Os -Wall -Wextra -o bin/hello src/hello.c
  '
```

### Compiler flags

The recommended flags for ostoo user-space binaries:

| Flag | Purpose |
|------|---------|
| `-static` | Produce a fully static binary (required — ostoo has no dynamic linker) |
| `-Os` | Optimize for size (keeps binaries small for the FAT filesystem image) |
| `-Wall -Wextra` | Enable warnings |
| `-nostdlib` | Skip libc entirely (for minimal programs that use only raw syscalls) |

## Verifying the output

You can inspect a compiled binary without Docker using the host `file` command:

```bash
file user/bin/hello
# hello: ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, ...
```

Or use `readelf` from the toolchain:

```bash
docker run --rm \
  -v /Volumes/crosstool-ng/x-tools:/home/ctng/x-tools \
  -v "$(pwd)/user":/home/ctng/user \
  ctng bash -c '
    export PATH="/home/ctng/x-tools/x86_64-unknown-linux-musl/bin:$PATH"
    x86_64-unknown-linux-musl-readelf -h /home/ctng/user/bin/hello
  '
```

Confirm: `Type: EXEC`, `Machine: Advanced Micro Devices X86-64`.

## Running on ostoo

1. Copy the compiled binary onto the FAT filesystem image
2. Boot ostoo in QEMU
3. From the shell: `exec /hello`

The kernel's ELF loader parses the binary and spawns it as a ring-3 process with
the syscall layer providing `write`, `brk`, `mmap`, `exit`, and other calls needed
by musl's startup code.

## Available toolchain binaries

All prefixed with `x86_64-unknown-linux-musl-`:

| Binary | Purpose |
|--------|---------|
| `gcc` / `cc` | C compiler |
| `g++` / `c++` | C++ compiler |
| `as` | Assembler |
| `ld` / `ld.bfd` | Linker |
| `ar` | Archive tool |
| `objcopy` | Binary manipulation |
| `objdump` | Disassembler |
| `readelf` | ELF inspector |
| `strip` | Strip symbols |
| `nm` | Symbol table viewer |
| `gdb` | Debugger |
