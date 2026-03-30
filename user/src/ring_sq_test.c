/*
 * ring_sq_test — Shared-memory SQ/CQ ring smoke test.
 *
 * Tests the io_setup_rings / io_ring_enter fast path:
 *
 *   1. Create a completion port
 *   2. Set up shared-memory rings via io_setup_rings
 *   3. mmap the SQ and CQ rings
 *   4. Submit OP_NOP via the SQ ring (no syscall)
 *   5. io_ring_enter to process + wait
 *   6. Read CQE from the CQ ring (no syscall)
 *   7. Verify user_data and result
 *   8. Repeat with OP_TIMEOUT
 *
 * Expected output:
 *   ring_sq_test: port=N
 *   ring_sq_test: setup_rings: sq_fd=N cq_fd=N sq_entries=64 cq_entries=128
 *   ring_sq_test: SQ mapped at 0x...
 *   ring_sq_test: CQ mapped at 0x...
 *   ring_sq_test: OP_NOP: submitted via SQ ring
 *   ring_sq_test: OP_NOP: io_ring_enter returned N
 *   ring_sq_test: OP_NOP: CQE user_data=42 result=0 opcode=0 PASS
 *   ring_sq_test: OP_TIMEOUT: submitted via SQ ring
 *   ring_sq_test: OP_TIMEOUT: io_ring_enter returned N
 *   ring_sq_test: OP_TIMEOUT: CQE user_data=99 result=0 opcode=1 PASS
 *   ring_sq_test: all tests passed
 */

#include <unistd.h>
#include <sys/mman.h>
#include <string.h>

/* Custom syscalls */
#define SYS_IO_CREATE       501
#define SYS_IO_SUBMIT       502
#define SYS_IO_WAIT         503
#define SYS_IO_SETUP_RINGS  511
#define SYS_IO_RING_ENTER   512

/* Opcodes */
#define OP_NOP     0
#define OP_TIMEOUT 1

/* -- Ring layout constants ------------------------------------------------ */

/* Ring header at offset 0 */
struct ring_header {
    unsigned int head;
    unsigned int tail;
    unsigned int mask;
    unsigned int flags;
};

/* Entries start at offset 64 (cache-line aligned) */
#define RING_ENTRIES_OFFSET 64

/* IoSubmission: matches kernel repr(C) layout (48 bytes) */
struct io_submission {
    unsigned long user_data;
    unsigned int  opcode;
    unsigned int  flags;
    int           fd;
    int           _pad;
    unsigned long buf_addr;
    unsigned int  buf_len;
    unsigned int  offset;
    unsigned long timeout_ns;
};

/* IoCompletion: matches kernel repr(C) layout (24 bytes) */
struct io_completion {
    unsigned long user_data;
    long          result;
    unsigned int  flags;
    unsigned int  opcode;
};

/* IoRingParams: matches kernel repr(C) layout */
struct io_ring_params {
    unsigned int sq_entries;   /* IN/OUT */
    unsigned int cq_entries;   /* IN/OUT */
    int          sq_fd;        /* OUT */
    int          cq_fd;        /* OUT */
};

/* -- syscall wrappers ----------------------------------------------------- */

static long io_create(unsigned int flags) {
    return syscall(SYS_IO_CREATE, flags);
}

static long io_setup_rings(int port_fd, struct io_ring_params *params) {
    return syscall(SYS_IO_SETUP_RINGS, port_fd, params);
}

static long io_ring_enter(int port_fd, unsigned int to_submit,
                          unsigned int min_complete, unsigned int flags) {
    return syscall(SYS_IO_RING_ENTER, port_fd, to_submit, min_complete, flags);
}

/* -- helpers -------------------------------------------------------------- */

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

static void put_dec(long n) {
    char buf[21];
    int i = 0;
    if (n < 0) { put_char('-'); n = -n; }
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    while (--i >= 0) put_char(buf[i]);
}

/* -- SQ/CQ ring access --------------------------------------------------- */

static struct io_submission *sq_entry(void *sq_base, unsigned int index,
                                       unsigned int mask) {
    unsigned int slot = index & mask;
    return (struct io_submission *)((char *)sq_base + RING_ENTRIES_OFFSET
                                     + slot * sizeof(struct io_submission));
}

static struct io_completion *cq_entry(void *cq_base, unsigned int index,
                                       unsigned int mask) {
    unsigned int slot = index & mask;
    return (struct io_completion *)((char *)cq_base + RING_ENTRIES_OFFSET
                                     + slot * sizeof(struct io_completion));
}

/* -- main ----------------------------------------------------------------- */

int main(void) {
    /* 1. Create completion port */
    long port_fd = io_create(0);
    if (port_fd < 0) {
        puts_stdout("ring_sq_test: io_create failed: ");
        put_dec(port_fd);
        put_char('\n');
        _exit(1);
    }
    puts_stdout("ring_sq_test: port=");
    put_dec(port_fd);
    put_char('\n');

    /* 2. Set up shared-memory rings */
    struct io_ring_params params;
    memset(&params, 0, sizeof(params));
    params.sq_entries = 64;
    params.cq_entries = 128;

    long ret = io_setup_rings((int)port_fd, &params);
    if (ret < 0) {
        puts_stdout("ring_sq_test: io_setup_rings failed: ");
        put_dec(ret);
        put_char('\n');
        _exit(1);
    }

    puts_stdout("ring_sq_test: setup_rings: sq_fd=");
    put_dec(params.sq_fd);
    puts_stdout(" cq_fd=");
    put_dec(params.cq_fd);
    puts_stdout(" sq_entries=");
    put_dec(params.sq_entries);
    puts_stdout(" cq_entries=");
    put_dec(params.cq_entries);
    put_char('\n');

    /* 3. mmap both rings */
    void *sq = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED,
                    params.sq_fd, 0);
    if (sq == MAP_FAILED) {
        puts_stdout("ring_sq_test: SQ mmap failed\n");
        _exit(1);
    }
    puts_stdout("ring_sq_test: SQ mapped at ");
    put_hex((unsigned long)sq);
    put_char('\n');

    void *cq = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED,
                    params.cq_fd, 0);
    if (cq == MAP_FAILED) {
        puts_stdout("ring_sq_test: CQ mmap failed\n");
        _exit(1);
    }
    puts_stdout("ring_sq_test: CQ mapped at ");
    put_hex((unsigned long)cq);
    put_char('\n');

    struct ring_header *sqh = (struct ring_header *)sq;
    struct ring_header *cqh = (struct ring_header *)cq;

    /* ---- Test 1: OP_NOP via SQ ring ---- */

    /* 4. Write OP_NOP SQE to SQ ring */
    {
        unsigned int tail = __atomic_load_n(&sqh->tail, __ATOMIC_RELAXED);
        struct io_submission *sqe = sq_entry(sq, tail, sqh->mask);
        memset(sqe, 0, sizeof(*sqe));
        sqe->user_data = 42;
        sqe->opcode = OP_NOP;
        __atomic_store_n(&sqh->tail, tail + 1, __ATOMIC_RELEASE);
        puts_stdout("ring_sq_test: OP_NOP: submitted via SQ ring\n");
    }

    /* 5. io_ring_enter: process 1, wait for 1 */
    ret = io_ring_enter((int)port_fd, 1, 1, 0);
    puts_stdout("ring_sq_test: OP_NOP: io_ring_enter returned ");
    put_dec(ret);
    put_char('\n');

    if (ret < 0) {
        puts_stdout("ring_sq_test: OP_NOP: FAIL (io_ring_enter error)\n");
        _exit(1);
    }

    /* 6. Read CQE from CQ ring */
    {
        unsigned int head = __atomic_load_n(&cqh->head, __ATOMIC_RELAXED);
        unsigned int cq_tail = __atomic_load_n(&cqh->tail, __ATOMIC_ACQUIRE);

        if (head == cq_tail) {
            puts_stdout("ring_sq_test: OP_NOP: FAIL (CQ empty)\n");
            _exit(1);
        }

        struct io_completion *cqe = cq_entry(cq, head, cqh->mask);
        puts_stdout("ring_sq_test: OP_NOP: CQE user_data=");
        put_dec(cqe->user_data);
        puts_stdout(" result=");
        put_dec(cqe->result);
        puts_stdout(" opcode=");
        put_dec(cqe->opcode);

        if (cqe->user_data == 42 && cqe->result == 0 && cqe->opcode == OP_NOP) {
            puts_stdout(" PASS\n");
        } else {
            puts_stdout(" FAIL\n");
            _exit(1);
        }

        /* Advance CQ head */
        __atomic_store_n(&cqh->head, head + 1, __ATOMIC_RELEASE);
    }

    /* ---- Test 2: OP_TIMEOUT via SQ ring ---- */

    /* Submit OP_TIMEOUT (50ms) */
    {
        unsigned int tail = __atomic_load_n(&sqh->tail, __ATOMIC_RELAXED);
        struct io_submission *sqe = sq_entry(sq, tail, sqh->mask);
        memset(sqe, 0, sizeof(*sqe));
        sqe->user_data = 99;
        sqe->opcode = OP_TIMEOUT;
        sqe->timeout_ns = 50000000UL;  /* 50 ms */
        __atomic_store_n(&sqh->tail, tail + 1, __ATOMIC_RELEASE);
        puts_stdout("ring_sq_test: OP_TIMEOUT: submitted via SQ ring\n");
    }

    /* io_ring_enter: process 1, wait for 1 */
    ret = io_ring_enter((int)port_fd, 1, 1, 0);
    puts_stdout("ring_sq_test: OP_TIMEOUT: io_ring_enter returned ");
    put_dec(ret);
    put_char('\n');

    if (ret < 0) {
        puts_stdout("ring_sq_test: OP_TIMEOUT: FAIL (io_ring_enter error)\n");
        _exit(1);
    }

    /* Read CQE */
    {
        unsigned int head = __atomic_load_n(&cqh->head, __ATOMIC_RELAXED);
        unsigned int cq_tail = __atomic_load_n(&cqh->tail, __ATOMIC_ACQUIRE);

        if (head == cq_tail) {
            puts_stdout("ring_sq_test: OP_TIMEOUT: FAIL (CQ empty)\n");
            _exit(1);
        }

        struct io_completion *cqe = cq_entry(cq, head, cqh->mask);
        puts_stdout("ring_sq_test: OP_TIMEOUT: CQE user_data=");
        put_dec(cqe->user_data);
        puts_stdout(" result=");
        put_dec(cqe->result);
        puts_stdout(" opcode=");
        put_dec(cqe->opcode);

        if (cqe->user_data == 99 && cqe->result == 0 && cqe->opcode == OP_TIMEOUT) {
            puts_stdout(" PASS\n");
        } else {
            puts_stdout(" FAIL\n");
            _exit(1);
        }

        __atomic_store_n(&cqh->head, head + 1, __ATOMIC_RELEASE);
    }

    /* Cleanup */
    munmap(sq, 4096);
    munmap(cq, 4096);
    close(params.sq_fd);
    close(params.cq_fd);
    close((int)port_fd);

    puts_stdout("ring_sq_test: all tests passed\n");
    _exit(0);
    return 0;
}
