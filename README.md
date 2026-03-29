# ostoo

[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue)](https://cmsd2.github.io/ostoo/)

A hobby x86-64 kernel written in Rust with a userspace shell, VFS, syscall
layer, and virtio drivers.

![Boot and shell demo](media/console.gif)

## Requirements

- Rust nightly (rustc, cargo, rustup)
- QEMU (`qemu-system-x86_64`)
- Docker (for building userspace programs)

### Rustup components

```sh
rustup component add rust-src llvm-tools-preview
```

### Cargo packages

```sh
cargo install cargo-xbuild bootimage
```

### Cross-compiler (one-time setup)

Userspace programs (the shell, etc.) are compiled with a musl-targeting
cross-compiler that runs inside a Docker container. Build the compiler image
once before your first `make`:

```sh
./crosstool/build.sh
```

This creates a Docker image called `ostoo-compiler`. You only need to re-run
this if the toolchain configuration changes.

## Build

```sh
make build
```

This builds the userspace programs (via the Docker cross-compiler) and then
the kernel bootimage.

To build just the userspace programs or just the kernel:

```sh
make user     # cross-compile userspace only
cargo bootimage --manifest-path kernel/Cargo.toml  # kernel only
```

## Run

```sh
# Build and boot with virtio-9p host sharing (default):
make run

# Build and boot with a virtio-blk disk image (created automatically):
make run-disk
```

The kernel boots under QEMU's Q35 machine type. By default, the host `user/`
directory is shared into the guest via virtio-9p, so changes to userspace
binaries are visible immediately without rebuilding a disk image.

## Test

```sh
make test
```

## Shell

The kernel auto-launches a userspace shell (`/shell`) if one is found on the
filesystem. If not, it falls back to a built-in kernel shell (prompt `kernel:#`
vs `$` for the userspace shell).

### Userspace shell commands

| Command | Description |
|---|---|
| `ls [path]` | List directory contents |
| `cat <file>` | Print file contents |
| `echo <text>` | Echo text |
| `pwd` | Print working directory |
| `cd <dir>` | Change directory |
| `exit` | Exit the shell |
| `help` | List available commands |

### Kernel shell commands

| Command | Description |
|---|---|
| `help` | List available commands |
| `ls [path]` | List directory / file info |
| `cat <file>` | Print file contents |
| `mount` | Show mounted filesystems |
| `driver list` | List registered drivers and status |
| `driver info <name>` | Print driver-specific info |
| `driver start/stop <name>` | Start or stop a driver |
| `blk info` | virtio-blk capacity and I/O counters |
| `blk read <sector>` | Read and hex-dump a 512-byte sector |

## Documentation

| Document | Description |
|---|---|
| [`docs/status.md`](docs/status.md) | Overall project status and feature list |
| [`docs/virtio-blk.md`](docs/virtio-blk.md) | virtio-blk PCI block device driver |
| [`docs/virtio-9p.md`](docs/virtio-9p.md) | virtio-9p host directory sharing |
| [`docs/vfs.md`](docs/vfs.md) | Virtual filesystem layer |
| [`docs/actors.md`](docs/actors.md) | Actor/driver framework and proc-macro reference |
| [`docs/scheduler.md`](docs/scheduler.md) | Async scheduler and preemption |
| [`docs/process-spawning.md`](docs/process-spawning.md) | ELF loading and process creation |
| [`docs/file-descriptors.md`](docs/file-descriptors.md) | File descriptor system and syscalls |
| [`docs/userspace-shell.md`](docs/userspace-shell.md) | Userspace shell design |
| [`docs/apic-ioapic.md`](docs/apic-ioapic.md) | APIC and IO APIC initialization |
| [`docs/lapic-timer.md`](docs/lapic-timer.md) | LAPIC timer calibration |
| [`docs/paging.md`](docs/paging.md) | Paging and memory management |
| [`docs/cross-compiling-c.md`](docs/cross-compiling-c.md) | Cross-compiling C for the kernel |
| [`docs/testing.md`](docs/testing.md) | Testing strategy and infrastructure |
| [`docs/code-audit.md`](docs/code-audit.md) | Code quality audit and improvement tracking |

## License

This work is derived from blog_os by Philipp Oppermann: https://os.phil-opp.com/

Dual licensed Apache-2 and MIT.
