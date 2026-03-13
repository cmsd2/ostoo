
.PHONY: build run test clean

build:
	cargo bootimage --manifest-path kernel/Cargo.toml

run:
	cargo bootimage run --manifest-path kernel/Cargo.toml

test:
	cargo test --manifest-path kernel/Cargo.toml

clean:
	cargo clean
