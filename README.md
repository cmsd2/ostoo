# ostoo

A hobby x86-64 kernel written in Rust.

## Requirements

1. Rust dev environment (rustc, cargo, rustup)
2. QEMU (`qemu-system-x86_64`)

### Rustup components

```sh
rustup component add rust-src llvm-tools-preview
```

### Cargo packages

```sh
cargo install cargo-xbuild bootimage
```

## Build

```sh
make build
```

## Run

```sh
# Create a blank 64 MiB disk image (first time only):
make disk

# Build and boot with the virtio-blk disk attached:
make run

# Boot without a disk (quick smoke-test):
make run-nodisk
```

The kernel boots under QEMU's Q35 machine type, which provides PCIe and ECAM
support required by the virtio-blk driver.

## Shell commands

| Command | Description |
|---|---|
| `help` | List available commands |
| `driver list` | List registered drivers and status |
| `driver info <name>` | Print driver-specific info |
| `driver start/stop <name>` | Start or stop a driver |
| `blk info` | virtio-blk capacity and I/O counters |
| `blk read <sector>` | Read and hex-dump a 512-byte sector |
| `echo <text>` | Echo text to the console |

## Documentation

| Document | Description |
|---|---|
| [`docs/status.md`](docs/status.md) | Overall project status and feature list |
| [`docs/virtio-blk.md`](docs/virtio-blk.md) | virtio-blk PCI block device driver |
| [`docs/actors.md`](docs/actors.md) | Actor/driver framework and proc-macro reference |
| [`docs/apic-ioapic.md`](docs/apic-ioapic.md) | APIC and IO APIC initialization |
| [`docs/lapic-timer.md`](docs/lapic-timer.md) | LAPIC timer calibration |
| [`docs/scheduler.md`](docs/scheduler.md) | Cooperative async scheduler |

## License

This work is derived from blog_os by Philipp Oppermann: https://os.phil-opp.com/

Dual licensed Apache-2 and MIT.
