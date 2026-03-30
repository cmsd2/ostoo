/*
 * io_pingpong — Two-process pipe ping-pong using completion ports.
 *
 * Parent creates two pipe pairs, spawns io_pong as a child,
 * then uses a completion port to submit OP_READ + OP_TIMEOUT
 * for each round.
 */

#include <string.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <spawn.h>
#include "ostoo.h"

/* ── main ────────────────────────────────────────────────────────────── */

#define TAG_READ    100
#define TAG_TIMER   200
#define ROUNDS      5

int main(void) {
    puts_stdout("io_pingpong: starting\n");

    /* Create two pipe pairs: parent→child and child→parent */
    int to_child[2];    /* [0]=read, [1]=write */
    int from_child[2];  /* [0]=read, [1]=write */

    if (syscall(293, to_child, 0) < 0) {
        puts_stdout("pipe2(to_child) failed\n");
        _exit(1);
    }
    if (syscall(293, from_child, 0) < 0) {
        puts_stdout("pipe2(from_child) failed\n");
        _exit(1);
    }

    /* Before spawning, dup child's fds to known numbers:
     * Child expects:  argv[1] = read fd (to_child[0])
     *                 argv[2] = write fd (from_child[1])
     */
    char rd_str[8], wr_str[8];
    itoa_buf(to_child[0], rd_str, sizeof(rd_str));
    itoa_buf(from_child[1], wr_str, sizeof(wr_str));

    /* Mark parent-side pipe ends as CLOEXEC so the child doesn't inherit
     * them.  Without this the child holds the write end of its own input
     * pipe, preventing EOF when the parent closes its copy. */
    fcntl(to_child[1], F_SETFD, FD_CLOEXEC);
    fcntl(from_child[0], F_SETFD, FD_CLOEXEC);

    /* Spawn io_pong child */
    pid_t child_pid;
    char *child_argv[] = { "/bin/io_pong", rd_str, wr_str, (char *)0 };
    int err = posix_spawn(&child_pid, "/bin/io_pong", 0, 0, child_argv, (char **)0);
    if (err != 0) {
        puts_stdout("posix_spawn(io_pong) failed\n");
        _exit(1);
    }

    puts_stdout("io_pingpong: child pid = ");
    put_num(child_pid);
    put_char('\n');

    /* Close child's ends in parent */
    close(to_child[0]);
    close(from_child[1]);

    /* Create completion port */
    long port_fd = io_create(0);
    if (port_fd < 0) {
        puts_stdout("io_create failed\n");
        _exit(1);
    }

    char send_buf[64];
    char recv_buf[64];

    for (int round = 0; round < ROUNDS; round++) {
        /* Format and send "ping N" */
        memset(send_buf, 0, sizeof(send_buf));
        const char *prefix = "ping ";
        int plen = (int)strlen(prefix);
        memcpy(send_buf, prefix, plen);
        /* append round number */
        char numstr[8];
        itoa_buf(round, numstr, sizeof(numstr));
        int nlen = (int)strlen(numstr);
        memcpy(send_buf + plen, numstr, nlen);
        int msg_len = plen + nlen;

        write(to_child[1], send_buf, msg_len);

        puts_stdout("  sent: ");
        write(1, send_buf, msg_len);
        put_char('\n');

        /* Submit OP_READ + OP_TIMEOUT to the port */
        struct io_submission subs[2];
        memset(subs, 0, sizeof(subs));

        memset(recv_buf, 0, sizeof(recv_buf));
        subs[0].user_data = TAG_READ;
        subs[0].opcode = OP_READ;
        subs[0].fd = from_child[0];
        subs[0].buf_addr = (unsigned long)recv_buf;
        subs[0].buf_len = sizeof(recv_buf) - 1;

        subs[1].user_data = TAG_TIMER;
        subs[1].opcode = OP_TIMEOUT;
        subs[1].timeout_ns = 1000000000UL; /* 1 second */

        io_submit((int)port_fd, subs, 2);

        /* Wait for at least 1 completion */
        struct io_completion comps[2];
        long got = io_wait((int)port_fd, comps, 2, 1, 0);

        for (long i = 0; i < got; i++) {
            if (comps[i].user_data == TAG_READ && comps[i].result > 0) {
                puts_stdout("  recv: ");
                write(1, recv_buf, (size_t)comps[i].result);
                put_char('\n');
            } else if (comps[i].user_data == TAG_TIMER) {
                puts_stdout("  (timer fired)\n");
            }
        }

        /* If we only got the timer, drain the read too (or vice versa) */
        if (got == 1) {
            long got2 = io_wait((int)port_fd, comps, 1, 1, 2000000000UL);
            for (long i = 0; i < got2; i++) {
                if (comps[i].user_data == TAG_READ && comps[i].result > 0) {
                    puts_stdout("  recv: ");
                    write(1, recv_buf, (size_t)comps[i].result);
                    put_char('\n');
                } else if (comps[i].user_data == TAG_TIMER) {
                    puts_stdout("  (timer fired)\n");
                }
            }
        }
    }

    /* Close pipes to signal EOF to child */
    close(to_child[1]);
    close(from_child[0]);
    close((int)port_fd);

    /* Wait for child */
    int status = 0;
    waitpid(child_pid, &status, 0);

    puts_stdout("io_pingpong: done\n");
    _exit(0);
    return 0;
}
