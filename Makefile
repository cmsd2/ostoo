
.PHONY: build user run run-disk test clean

# Build userspace programs first, then the kernel bootimage.
build: user
	cargo bootimage --manifest-path kernel/Cargo.toml

# Build userspace programs via the Docker cross-compiler.
user:
	scripts/user-build.sh

# Run with virtio-9p host sharing (default, no disk image needed).
run: build
	scripts/run.sh

# Run with virtio-blk disk attached.
run-disk: build
	scripts/run-disk.sh

test:
	cargo test --manifest-path libkernel/Cargo.toml
	cargo test --manifest-path kernel/Cargo.toml

clean:
	cargo clean
