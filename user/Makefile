CC      = x86_64-unknown-linux-musl-gcc
CFLAGS  = -static -Os -Wall -Wextra
SOURCES = $(wildcard src/*.c)
BINS    = $(patsubst src/%.c,bin/%,$(SOURCES))

all: bin $(BINS)

bin:
	mkdir -p bin

bin/%: src/%.c | bin
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -rf bin

.PHONY: all clean
