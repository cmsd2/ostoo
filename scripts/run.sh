#!/bin/bash
# Run with virtio-9p host sharing (no disk image needed).
set -e

qemu-system-x86_64 \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -serial stdio \
    -fsdev local,id=fsdev0,path=./user,security_model=none \
    -device virtio-9p-pci,fsdev=fsdev0,mount_tag=hostfs
