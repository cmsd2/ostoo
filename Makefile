
.PHONY: build run test

build:
	cargo bootimage

run:
	cargo xrun

test:
	cargo xtest
