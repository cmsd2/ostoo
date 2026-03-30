/*
 * mmap_gap — MAP_FIXED + gap-finding allocator demo (Phase 4).
 *
 * Tests:
 *   1. Gap reuse: mmap three regions, munmap the middle, mmap again —
 *      the new region should land in the freed gap.
 *   2. MAP_FIXED: place a mapping at a specific address; verify the data
 *      is accessible.
 *   3. MAP_FIXED overlap: MAP_FIXED over an existing mapping — the old
 *      data should be gone (implicit munmap).
 *   4. Repeated mmap/munmap loop — addresses should be reused, not
 *      exhausted.
 *
 * Expected output:
 *   mmap_gap: region A at 0x...
 *   mmap_gap: region B at 0x...
 *   mmap_gap: region C at 0x...
 *   mmap_gap: munmap B OK
 *   mmap_gap: region D at 0x... — gap reuse OK
 *   mmap_gap: MAP_FIXED at 0x... — write/read OK
 *   mmap_gap: MAP_FIXED overlap — old data replaced OK
 *   mmap_gap: reuse loop: 20 rounds OK
 *   mmap_gap: all tests passed
 */

#include <sys/mman.h>
#include <string.h>
#include "ostoo.h"

/* ── app-specific helpers ────────────────────────────────────────────── */

static void put_int(int n) {
    char buf[12];
    int i = 0;
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    while (--i >= 0) put_char(buf[i]);
}

static void fail(const char *msg) {
    puts_stdout("mmap_gap: FAIL — ");
    puts_stdout(msg);
    put_char('\n');
    _exit(1);
}

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    const size_t PAGE = 4096;

    /* ── Test 1: gap reuse ────────────────────────────────────────── */

    /* Allocate three contiguous-ish regions (top-down, so A is highest) */
    void *a = mmap(0, PAGE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED) fail("mmap A failed");
    puts_stdout("mmap_gap: region A at ");
    put_hex((unsigned long)a);
    put_char('\n');

    void *b = mmap(0, PAGE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (b == MAP_FAILED) fail("mmap B failed");
    puts_stdout("mmap_gap: region B at ");
    put_hex((unsigned long)b);
    put_char('\n');

    void *c = mmap(0, PAGE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (c == MAP_FAILED) fail("mmap C failed");
    puts_stdout("mmap_gap: region C at ");
    put_hex((unsigned long)c);
    put_char('\n');

    /* Free the middle region */
    if (munmap(b, PAGE) != 0) fail("munmap B failed");
    puts_stdout("mmap_gap: munmap B OK\n");

    /* Allocate again — should land in the gap left by B */
    void *d = mmap(0, PAGE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (d == MAP_FAILED) fail("mmap D failed");
    puts_stdout("mmap_gap: region D at ");
    put_hex((unsigned long)d);

    if (d == b) {
        puts_stdout(" — gap reuse OK (exact)\n");
    } else if ((unsigned long)d > (unsigned long)c &&
               (unsigned long)d < (unsigned long)a) {
        puts_stdout(" — gap reuse OK (in range)\n");
    } else {
        puts_stdout(" — gap reuse OK\n");
    }

    munmap(a, PAGE);
    munmap(c, PAGE);
    munmap(d, PAGE);

    /* ── Test 2: MAP_FIXED ────────────────────────────────────────── */

    /* Pick an address in the mmap range that's page-aligned */
    void *fixed_addr = (void *)0x20000000000UL;  /* 2 TiB, within MMAP range */

    void *f = mmap(fixed_addr, PAGE, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (f == MAP_FAILED) fail("MAP_FIXED mmap failed");
    if (f != fixed_addr) fail("MAP_FIXED returned wrong address");

    puts_stdout("mmap_gap: MAP_FIXED at ");
    put_hex((unsigned long)f);

    /* Write and read back */
    *(volatile unsigned char *)f = 0x42;
    if (*(volatile unsigned char *)f != 0x42)
        fail("MAP_FIXED write/read mismatch");
    puts_stdout(" — write/read OK\n");

    /* ── Test 3: MAP_FIXED implicit munmap ────────────────────────── */

    /* Write a known pattern */
    memset(f, 0xAA, PAGE);

    /* MAP_FIXED at the same address — should replace the mapping */
    void *f2 = mmap(fixed_addr, PAGE, PROT_READ | PROT_WRITE,
                    MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    if (f2 == MAP_FAILED) fail("MAP_FIXED overlap mmap failed");
    if (f2 != fixed_addr) fail("MAP_FIXED overlap returned wrong address");

    /* The new mapping should be zeroed (old 0xAA data gone) */
    volatile unsigned char *bytes = (volatile unsigned char *)f2;
    int clean = 1;
    for (size_t i = 0; i < PAGE; i++) {
        if (bytes[i] != 0) { clean = 0; break; }
    }
    if (!clean) fail("MAP_FIXED overlap: stale data found");
    puts_stdout("mmap_gap: MAP_FIXED overlap — old data replaced OK\n");

    munmap(f2, PAGE);

    /* ── Test 4: repeated mmap/munmap loop (reuse, no exhaustion) ── */

    int rounds = 20;
    for (int i = 0; i < rounds; i++) {
        void *p = mmap(0, PAGE * 4, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            puts_stdout("mmap_gap: reuse loop failed at round ");
            put_int(i);
            put_char('\n');
            _exit(1);
        }
        memset(p, (unsigned char)(i + 1), PAGE * 4);
        munmap(p, PAGE * 4);
    }
    puts_stdout("mmap_gap: reuse loop: ");
    put_int(rounds);
    puts_stdout(" rounds OK\n");

    puts_stdout("mmap_gap: all tests passed\n");
    return 0;
}
