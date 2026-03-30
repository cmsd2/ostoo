CC      = x86_64-unknown-linux-musl-gcc
AR      = x86_64-unknown-linux-musl-ar
CFLAGS  = -static -Os -Wall -Wextra -Iinclude
SOURCES = $(wildcard src/*.c)
BINS    = $(patsubst src/%.c,bin/%,$(SOURCES))

all: bin lib/libostoo.a $(BINS)

bin:
	mkdir -p bin

lib:
	mkdir -p lib

lib/libostoo.a: lib/ostoo.o | lib
	$(AR) rcs $@ $<

lib/ostoo.o: lib/ostoo.c include/ostoo.h | lib
	$(CC) $(CFLAGS) -c -o $@ $<

bin/%: src/%.c lib/libostoo.a | bin
	$(CC) $(CFLAGS) -o $@ $< -Llib -lostoo

clean:
	rm -rf bin lib/libostoo.a lib/ostoo.o

.PHONY: all clean
