#!/bin/bash

DISK_IMG=disk.img

qemu-system-x86_64 \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -serial stdio \
    -drive file=$DISK_IMG,format=raw,if=none,id=hd0 \
	-device virtio-blk-pci,drive=hd0
