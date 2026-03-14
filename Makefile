
.PHONY: build run test clean

build: target/x86_64-os/debug/bootimage-kernel.bin

target/x86_64-os/debug/bootimage-kernel.bin:
	cargo bootimage --manifest-path kernel/Cargo.toml

run: target/x86_64-os/debug/bootimage-kernel.bin
	qemu-system-x86_64 -drive format=raw,file=target/x86_64-os/debug/bootimage-kernel.bin -serial stdio -display none

test:
	cargo test --manifest-path kernel/Cargo.toml

clean:
	cargo clean
