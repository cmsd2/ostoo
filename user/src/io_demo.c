/*
 * io_demo — Phase 1+2 smoke test for completion ports.
 *
 * Tests: OP_NOP, OP_TIMEOUT, OP_READ (pipe), OP_WRITE (pipe).
 *
 * Expected output:
 *   comp 0: opcode=NOP      user_data=1  result=0
 *   comp 1: opcode=READ     user_data=2  result=5
 *   comp 2: opcode=WRITE    user_data=3  result=6
 *   comp 3: opcode=NOP      user_data=4  result=0
 *   comp 4: opcode=TIMEOUT  user_data=5  result=0
 *   All 5 completions received!
 */

#include <string.h>
#include "ostoo.h"

/* ── opcode name ─────────────────────────────────────────────────────── */

static const char *opcode_name(unsigned int op) {
    switch (op) {
    case OP_NOP:     return "NOP";
    case OP_TIMEOUT: return "TIMEOUT";
    case OP_READ:    return "READ";
    case OP_WRITE:   return "WRITE";
    default:         return "???";
    }
}

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    puts_stdout("io_demo: starting\n");

    /* Create a pipe for testing OP_READ/OP_WRITE */
    int pipefd[2];
    if (syscall(293 /* SYS_pipe2 */, pipefd, 0) < 0) {
        puts_stdout("io_demo: pipe2 failed\n");
        _exit(1);
    }
    /* Write "hello" into the pipe so the OP_READ has data */
    write(pipefd[1], "hello", 5);

    /* Create a completion port */
    long port_fd = io_create(0);
    if (port_fd < 0) {
        puts_stdout("io_demo: io_create failed: ");
        put_num(port_fd);
        put_char('\n');
        _exit(1);
    }
    puts_stdout("io_demo: port fd = ");
    put_num(port_fd);
    put_char('\n');

    /* Build 5 submissions */
    struct io_submission subs[5];
    memset(subs, 0, sizeof(subs));

    /* 0: OP_NOP */
    subs[0].user_data = 1;
    subs[0].opcode = OP_NOP;

    /* 1: OP_READ from pipe read end */
    char read_buf[64];
    memset(read_buf, 0, sizeof(read_buf));
    subs[1].user_data = 2;
    subs[1].opcode = OP_READ;
    subs[1].fd = pipefd[0];
    subs[1].buf_addr = (unsigned long)read_buf;
    subs[1].buf_len = sizeof(read_buf);

    /* 2: OP_WRITE to pipe write end */
    const char *msg = "world!";
    subs[2].user_data = 3;
    subs[2].opcode = OP_WRITE;
    subs[2].fd = pipefd[1];
    subs[2].buf_addr = (unsigned long)msg;
    subs[2].buf_len = 6;

    /* 3: OP_NOP */
    subs[3].user_data = 4;
    subs[3].opcode = OP_NOP;

    /* 4: OP_TIMEOUT — 200ms */
    subs[4].user_data = 5;
    subs[4].opcode = OP_TIMEOUT;
    subs[4].timeout_ns = 200000000UL; /* 200ms */

    /* Submit all 5 */
    long submitted = io_submit((int)port_fd, subs, 5);
    if (submitted < 0) {
        puts_stdout("io_demo: io_submit failed: ");
        put_num(submitted);
        put_char('\n');
        _exit(1);
    }
    puts_stdout("io_demo: submitted ");
    put_num(submitted);
    puts_stdout(" operations\n");

    /* Wait for all 5 completions (5s timeout) */
    struct io_completion comps[8];
    long got = io_wait((int)port_fd, comps, 8, 5, 5000000000UL);
    if (got < 0) {
        puts_stdout("io_demo: io_wait failed: ");
        put_num(got);
        put_char('\n');
        _exit(1);
    }

    for (long i = 0; i < got; i++) {
        puts_stdout("  comp ");
        put_num(i);
        puts_stdout(": opcode=");
        puts_stdout(opcode_name(comps[i].opcode));
        puts_stdout("  user_data=");
        put_num((long)comps[i].user_data);
        puts_stdout("  result=");
        put_num(comps[i].result);
        put_char('\n');
    }

    if (got == 5) {
        puts_stdout("All 5 completions received!\n");
    } else {
        puts_stdout("Expected 5 completions, got ");
        put_num(got);
        put_char('\n');
    }

    /* Show the data read by OP_READ */
    puts_stdout("io_demo: read_buf = \"");
    /* Find the READ completion and check how many bytes */
    for (long i = 0; i < got; i++) {
        if (comps[i].opcode == OP_READ && comps[i].result > 0) {
            write(1, read_buf, (size_t)comps[i].result);
        }
    }
    puts_stdout("\"\n");

    close(pipefd[0]);
    close(pipefd[1]);
    close((int)port_fd);

    puts_stdout("io_demo: done\n");
    _exit(0);
    return 0;
}
