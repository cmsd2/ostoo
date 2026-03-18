CC      = x86_64-unknown-linux-musl-gcc
CFLAGS  = -static -Os -Wall -Wextra
SOURCES = $(wildcard *.c)
BINS    = $(SOURCES:.c=)

all: $(BINS)

%: %.c
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -f $(BINS)

.PHONY: all clean
