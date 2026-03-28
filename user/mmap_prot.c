/*
 * mmap_prot — VMA / PROT flags smoke test.
 *
 * Tests:
 *   1. mmap PROT_READ|PROT_WRITE  — write succeeds
 *   2. mmap PROT_READ only        — write should page-fault (kernel kills us)
 *
 * Expected output when everything works:
 *   mmap_prot: RW region at 0x...
 *   mmap_prot: wrote 0xAA to RW region — OK
 *   mmap_prot: read back 0xAA — OK
 *   mmap_prot: RO region at 0x...
 *   mmap_prot: read 0x00 from RO region — OK
 *   mmap_prot: writing to RO region (should fault)...
 *   (page fault / process killed — no further output)
 */

#include <unistd.h>
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

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    /* Test 1: PROT_READ | PROT_WRITE — should succeed */
    void *rw = mmap(0, 4096, PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (rw == MAP_FAILED) {
        puts_stdout("mmap_prot: RW mmap failed\n");
        _exit(1);
    }
    puts_stdout("mmap_prot: RW region at ");
    put_hex((unsigned long)rw);
    put_char('\n');

    /* Write to the RW region */
    *(volatile unsigned char *)rw = 0xAA;
    puts_stdout("mmap_prot: wrote 0xAA to RW region — OK\n");

    /* Read it back */
    unsigned char val = *(volatile unsigned char *)rw;
    if (val == 0xAA) {
        puts_stdout("mmap_prot: read back 0xAA — OK\n");
    } else {
        puts_stdout("mmap_prot: read back wrong value!\n");
        _exit(1);
    }

    /* Test 2: PROT_READ only — write should page-fault */
    void *ro = mmap(0, 4096, PROT_READ,
                    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (ro == MAP_FAILED) {
        puts_stdout("mmap_prot: RO mmap failed\n");
        _exit(1);
    }
    puts_stdout("mmap_prot: RO region at ");
    put_hex((unsigned long)ro);
    put_char('\n');

    /* Read from RO region (should be zeroed) */
    unsigned char ro_val = *(volatile unsigned char *)ro;
    if (ro_val == 0) {
        puts_stdout("mmap_prot: read 0x00 from RO region — OK\n");
    } else {
        puts_stdout("mmap_prot: RO region not zeroed!\n");
        _exit(1);
    }

    /* This write should trigger a page fault */
    puts_stdout("mmap_prot: writing to RO region (should fault)...\n");
    *(volatile unsigned char *)ro = 0xBB;

    /* If we get here, PROT_READ enforcement failed */
    puts_stdout("mmap_prot: ERROR — write to RO region succeeded (should have faulted)\n");
    _exit(1);
    return 0;
}
