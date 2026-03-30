/*
 * mmap_free — munmap + frame free list demo.
 *
 * Tests:
 *   1. mmap a region, write a pattern, munmap it
 *   2. mmap a new region — should get recycled frames (zero-filled)
 *   3. Repeat in a loop to show frames are recycled (not leaked)
 *   4. cat /proc/meminfo before/after to see free list count
 *
 * Expected output:
 *   mmap_free: round 1: mmap at 0x... — write 0xAA — munmap OK — remapped at 0x... — zeroed OK
 *   mmap_free: round 2: mmap at 0x... — write 0xBB — munmap OK — remapped at 0x... — zeroed OK
 *   ...
 *   mmap_free: multi-page test: 4 pages at 0x... — write OK — munmap OK — remapped at 0x... — zeroed OK
 *   mmap_free: all tests passed
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

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    /* Test 1: repeated single-page mmap/munmap cycles */
    for (int round = 1; round <= 5; round++) {
        unsigned char pattern = 0xA0 + round;

        void *p = mmap(0, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            puts_stdout("mmap_free: mmap failed\n");
            _exit(1);
        }

        puts_stdout("mmap_free: round ");
        put_int(round);
        puts_stdout(": mmap at ");
        put_hex((unsigned long)p);

        /* Write a pattern to every byte in the page */
        memset(p, pattern, 4096);
        puts_stdout(" — write ");
        put_hex(pattern);

        /* munmap */
        int ret = munmap(p, 4096);
        if (ret != 0) {
            puts_stdout(" — munmap FAILED\n");
            _exit(1);
        }
        puts_stdout(" — munmap OK");

        /* mmap again — should get a recycled, zeroed frame */
        void *q = mmap(0, 4096, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (q == MAP_FAILED) {
            puts_stdout(" — remap failed\n");
            _exit(1);
        }
        puts_stdout(" — remapped at ");
        put_hex((unsigned long)q);

        /* Verify it's zeroed (not stale data from the freed frame) */
        volatile unsigned char *bytes = (volatile unsigned char *)q;
        int clean = 1;
        for (int i = 0; i < 4096; i++) {
            if (bytes[i] != 0) { clean = 0; break; }
        }
        if (clean) {
            puts_stdout(" — zeroed OK\n");
        } else {
            puts_stdout(" — ERROR: stale data!\n");
            _exit(1);
        }

        /* Clean up for next round */
        munmap(q, 4096);
    }

    /* Test 2: multi-page mmap/munmap */
    {
        int pages = 4;
        size_t len = pages * 4096;

        void *p = mmap(0, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            puts_stdout("mmap_free: multi-page mmap failed\n");
            _exit(1);
        }
        puts_stdout("mmap_free: multi-page test: ");
        put_int(pages);
        puts_stdout(" pages at ");
        put_hex((unsigned long)p);

        memset(p, 0xCC, len);
        puts_stdout(" — write OK");

        int ret = munmap(p, len);
        if (ret != 0) {
            puts_stdout(" — munmap FAILED\n");
            _exit(1);
        }
        puts_stdout(" — munmap OK");

        void *q = mmap(0, len, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (q == MAP_FAILED) {
            puts_stdout(" — remap failed\n");
            _exit(1);
        }
        puts_stdout(" — remapped at ");
        put_hex((unsigned long)q);

        volatile unsigned char *bytes = (volatile unsigned char *)q;
        int clean = 1;
        for (size_t i = 0; i < len; i++) {
            if (bytes[i] != 0) { clean = 0; break; }
        }
        if (clean) {
            puts_stdout(" — zeroed OK\n");
        } else {
            puts_stdout(" — ERROR: stale data!\n");
            _exit(1);
        }
        munmap(q, len);
    }

    puts_stdout("mmap_free: all tests passed\n");
    return 0;
}
