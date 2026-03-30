#\!/bin/bash
# Run with QEMU monitor on stdio and stop-on-fault for debugging.
set -e

qemu-system-x86_64 \
    -machine q35 \
    -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
    -monitor stdio \
    -no-reboot \
    -no-shutdown \
    -d int,cpu_reset \
    -fsdev local,id=fsdev0,path=./user,security_model=none \
    -device virtio-9p-pci,fsdev=fsdev0,mount_tag=hostfs
