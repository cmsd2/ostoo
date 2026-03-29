/*
 * mmap_file — file-backed MAP_PRIVATE mmap demo.
 *
 * Tests:
 *   1. Open a file, read first 64 bytes via read()
 *   2. mmap the same file with MAP_PRIVATE, PROT_READ
 *   3. Compare the mmap'd bytes with the read() bytes
 *   4. Verify bytes past file length are zero (if mapping is larger)
 *   5. munmap and exit cleanly
 *
 * Expected output:
 *   mmap_file: opened fd=N, read M bytes via read()
 *   mmap_file: mmap'd at 0x...
 *   mmap_file: first 64 bytes match read() — OK
 *   mmap_file: munmap OK
 *   mmap_file: all tests passed
 */

#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <string.h>

/* ── helpers ─────────────────────────────────────────────────────────── */

static void puts_stdout(const char *s) {
    write(1, s, strlen(s));
}

static void put_char(char c) {
    write(1, &c, 1);
}

static void put_hex(unsigned long n) {
    char buf[17];
    int i = 0;
    if (n == 0) { puts_stdout("0x0"); return; }
    while (n > 0) {
        int d = n & 0xF;
        buf[i++] = d < 10 ? '0' + d : 'a' + d - 10;
        n >>= 4;
    }
    puts_stdout("0x");
    while (--i >= 0) put_char(buf[i]);
}

static void put_dec(unsigned long n) {
    char buf[21];
    int i = 0;
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    while (--i >= 0) put_char(buf[i]);
}

/* ── main ────────────────────────────────────────────────────────────── */

/* Use /bin/shell as the test file — it's always present on the VFS. */
static const char *test_path = "/bin/shell";

int main(void) {
    /* Step 1: open the file and read the first 64 bytes via read(). */
    int fd = open(test_path, 0 /* O_RDONLY */, 0);
    if (fd < 0) {
        puts_stdout("mmap_file: open failed\n");
        _exit(1);
    }

    unsigned char read_buf[64];
    long nread = read(fd, read_buf, sizeof(read_buf));
    if (nread <= 0) {
        puts_stdout("mmap_file: read failed\n");
        _exit(1);
    }

    puts_stdout("mmap_file: opened fd=");
    put_dec((unsigned long)fd);
    puts_stdout(", read ");
    put_dec((unsigned long)nread);
    puts_stdout(" bytes via read()\n");

    /* Step 2: mmap the file — need to reopen since read() advanced the pos. */
    int fd2 = open(test_path, 0 /* O_RDONLY */, 0);
    if (fd2 < 0) {
        puts_stdout("mmap_file: second open failed\n");
        _exit(1);
    }

    /* Map one page. */
    void *mapped = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd2, 0);
    if (mapped == MAP_FAILED) {
        puts_stdout("mmap_file: mmap failed\n");
        _exit(1);
    }

    puts_stdout("mmap_file: mmap'd at ");
    put_hex((unsigned long)mapped);
    put_char('\n');

    /* Step 3: compare the first nread bytes. */
    unsigned char *mp = (unsigned char *)mapped;
    int mismatch = 0;
    for (long i = 0; i < nread; i++) {
        if (mp[i] != read_buf[i]) {
            puts_stdout("mmap_file: MISMATCH at byte ");
            put_dec((unsigned long)i);
            puts_stdout(": read=");
            put_hex(read_buf[i]);
            puts_stdout(" mmap=");
            put_hex(mp[i]);
            put_char('\n');
            mismatch = 1;
            break;
        }
    }

    if (mismatch) {
        _exit(1);
    }
    puts_stdout("mmap_file: first ");
    put_dec((unsigned long)nread);
    puts_stdout(" bytes match read() — OK\n");

    /* Step 4: munmap */
    if (munmap(mapped, 4096) != 0) {
        puts_stdout("mmap_file: munmap failed\n");
        _exit(1);
    }
    puts_stdout("mmap_file: munmap OK\n");

    close(fd);
    close(fd2);

    puts_stdout("mmap_file: all tests passed\n");
    _exit(0);
    return 0;
}
