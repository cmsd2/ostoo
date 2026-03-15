
.PHONY: build run run-nodisk test clean disk

QEMU_ARGS := -machine q35 \
             -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin \
             -serial stdio \
             -display none

DISK_IMG := disk.img

build: target/x86_64-os/debug/bootimage-kernel.bin

target/x86_64-os/debug/bootimage-kernel.bin:
	cargo bootimage --manifest-path kernel/Cargo.toml

# Create a blank 64 MiB disk image if one does not already exist.
$(DISK_IMG):
	dd if=/dev/zero of=$(DISK_IMG) bs=1M count=64

disk: $(DISK_IMG)

# Run with the virtio-blk disk attached.
run: target/x86_64-os/debug/bootimage-kernel.bin $(DISK_IMG)
	qemu-system-x86_64 $(QEMU_ARGS) \
	    -drive file=$(DISK_IMG),format=raw,if=none,id=hd0 \
	    -device virtio-blk-pci,drive=hd0

# Run without the disk (for quick tests without virtio-blk).
run-nodisk: target/x86_64-os/debug/bootimage-kernel.bin
	qemu-system-x86_64 $(QEMU_ARGS)

test:
	cargo test --manifest-path kernel/Cargo.toml

clean:
	cargo clean
