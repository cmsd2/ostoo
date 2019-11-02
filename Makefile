
.PHONY: build run test clean

build:
	cargo bootimage --manifest-path kernel/Cargo.toml

run:
	bootimage run --manifest-path kernel/Cargo.toml

test:
	cargo xtest

clean:
	cargo clean