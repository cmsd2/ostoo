/*
 * ring_sq_test — Shared-memory SQ/CQ ring smoke test.
 *
 * Tests the io_setup_rings / io_ring_enter fast path:
 *
 *   1. Create a completion port
 *   2. Set up shared-memory rings via io_setup_rings
 *   3. mmap the SQ and CQ rings
 *   4. Submit OP_NOP via sq_push (no syscall)
 *   5. io_ring_enter to process + wait
 *   6. Read CQE via cq_pop (no syscall)
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

#include <sys/mman.h>
#include <string.h>
#include "ostoo.h"

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

    /* ---- Test 1: OP_NOP via SQ ring ---- */

    {
        struct io_submission sqe;
        memset(&sqe, 0, sizeof(sqe));
        sqe.user_data = 42;
        sqe.opcode = OP_NOP;

        if (sq_push(sq, &sqe) < 0) {
            puts_stdout("ring_sq_test: OP_NOP: sq_push failed (full)\n");
            _exit(1);
        }
        puts_stdout("ring_sq_test: OP_NOP: submitted via SQ ring\n");
    }

    /* io_ring_enter: process 1, wait for 1 */
    ret = io_ring_enter((int)port_fd, 1, 1, 0);
    puts_stdout("ring_sq_test: OP_NOP: io_ring_enter returned ");
    put_dec(ret);
    put_char('\n');

    if (ret < 0) {
        puts_stdout("ring_sq_test: OP_NOP: FAIL (io_ring_enter error)\n");
        _exit(1);
    }

    /* Read CQE */
    {
        struct io_completion cqe;
        if (cq_pop(cq, &cqe) < 0) {
            puts_stdout("ring_sq_test: OP_NOP: FAIL (CQ empty)\n");
            _exit(1);
        }

        puts_stdout("ring_sq_test: OP_NOP: CQE user_data=");
        put_dec(cqe.user_data);
        puts_stdout(" result=");
        put_dec(cqe.result);
        puts_stdout(" opcode=");
        put_dec(cqe.opcode);

        if (cqe.user_data == 42 && cqe.result == 0 && cqe.opcode == OP_NOP) {
            puts_stdout(" PASS\n");
        } else {
            puts_stdout(" FAIL\n");
            _exit(1);
        }
    }

    /* ---- Test 2: OP_TIMEOUT via SQ ring ---- */

    {
        struct io_submission sqe;
        memset(&sqe, 0, sizeof(sqe));
        sqe.user_data = 99;
        sqe.opcode = OP_TIMEOUT;
        sqe.timeout_ns = 50000000UL;  /* 50 ms */

        if (sq_push(sq, &sqe) < 0) {
            puts_stdout("ring_sq_test: OP_TIMEOUT: sq_push failed (full)\n");
            _exit(1);
        }
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
        struct io_completion cqe;
        if (cq_pop(cq, &cqe) < 0) {
            puts_stdout("ring_sq_test: OP_TIMEOUT: FAIL (CQ empty)\n");
            _exit(1);
        }

        puts_stdout("ring_sq_test: OP_TIMEOUT: CQE user_data=");
        put_dec(cqe.user_data);
        puts_stdout(" result=");
        put_dec(cqe.result);
        puts_stdout(" opcode=");
        put_dec(cqe.opcode);

        if (cqe.user_data == 99 && cqe.result == 0 && cqe.opcode == OP_TIMEOUT) {
            puts_stdout(" PASS\n");
        } else {
            puts_stdout(" FAIL\n");
            _exit(1);
        }
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
