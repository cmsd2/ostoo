# kernel

The top-level kernel binary — entry point, ties everything together.

## Requirements

```sh
cargo install cargo-xbuild bootimage
rustup component add rust-src llvm-tools-preview
```

## Build

```sh
make build
```

## Run

```sh
# Create disk image (first time only):
make disk

# Run with virtio-blk disk:
make run

# Run without disk:
make run-nodisk
```

## Shell commands

| Command | Description |
|---|---|
| `help` | List commands |
| `driver list` | List drivers |
| `driver info <name>` | Print driver info |
| `driver start/stop <name>` | Manage a driver |
| `blk info` | virtio-blk capacity and I/O counters |
| `blk read <sector>` | Hex-dump a 512-byte sector |
| `echo <text>` | Echo text |
