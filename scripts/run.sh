#!/bin/bash

DISK_IMG=disk.img

cargo bootimage --manifest-path kernel/Cargo.toml

qemu-system-x86_64 \
    -d int \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -serial stdio \
    -fsdev local,id=fsdev0,path=./user,security_model=none \
    -device virtio-9p-pci,fsdev=fsdev0,mount_tag=hostfs
    # -drive file=$DISK_IMG,format=raw,if=none,id=hd0 \
    # -device virtio-blk-pci,drive=hd0 \
    #-no-reboot \
    #-no-shutdown
