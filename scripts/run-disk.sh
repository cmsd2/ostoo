#!/bin/bash
# Run with virtio-blk disk and virtio-9p host sharing.
set -e

DISK_IMG=disk.img

if [ ! -f "$DISK_IMG" ]; then
    echo "Creating blank disk image: $DISK_IMG"
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=64
fi

qemu-system-x86_64 \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -serial stdio \
    -drive file="$DISK_IMG",format=raw,if=none,id=hd0 \
    -device virtio-blk-pci,drive=hd0 \
    -fsdev local,id=fsdev0,path=./user,security_model=none \
    -device virtio-9p-pci,fsdev=fsdev0,mount_tag=hostfs
