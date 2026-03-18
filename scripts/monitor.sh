#!/bin/bash

DISK_IMG=disk.img

cargo bootimage --manifest-path kernel/Cargo.toml

qemu-system-x86_64 \
    -d int \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -monitor stdio \
    -drive file=$DISK_IMG,format=raw,if=none,id=hd0 \
	-device virtio-blk-pci,drive=hd0
    # \
    #-no-reboot \
    #-no-shutdown
