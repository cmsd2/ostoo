/*
 * ipc_sync — Sync (rendezvous) IPC channel demo with parent + child.
 *
 * Creates a sync channel (capacity=0), spawns ipc_pong as a child
 * that receives on the recv end, then sends 3 messages.  Each send
 * blocks until the child calls recv (rendezvous).
 *
 * Expected output:
 *   ipc_sync: spawned child pid=N
 *   ipc_sync: sending msg 1
 *   ipc_sync: send 1 done
 *   ipc_sync: sending msg 2
 *   ipc_sync: send 2 done
 *   ipc_sync: sending msg 3
 *   ipc_sync: send 3 done
 *   ipc_sync: closed send end
 *   ipc_sync: child exited
 *   PASS
 */

#include <string.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <spawn.h>
#include "ostoo.h"

/* ── main ────────────────────────────────────────────────────────────── */

#define ROUNDS 3

int main(void) {
    int fds[2];
    long rc;

    /* Create sync channel (capacity=0) */
    rc = ipc_create(fds, 0, 0);
    if (rc < 0) {
        puts_stdout("ipc_sync: ipc_create failed: ");
        put_num(rc);
        put_char('\n');
        _exit(1);
    }
    int send_fd = fds[0];
    int recv_fd = fds[1];

    /* Mark send_fd as CLOEXEC so child doesn't inherit it */
    fcntl(send_fd, F_SETFD, FD_CLOEXEC);

    /* Spawn ipc_pong child, passing recv_fd as argv[1] */
    char recv_str[8];
    itoa_buf(recv_fd, recv_str, sizeof(recv_str));

    pid_t child;
    char *argv[] = { "ipc_pong", recv_str, (char *)0 };
    int err = posix_spawn(&child, "/bin/ipc_pong", 0, 0, argv, (char **)0);
    if (err != 0) {
        puts_stdout("ipc_sync: posix_spawn failed\n");
        _exit(1);
    }

    /* Close recv end in parent — child has it */
    close(recv_fd);

    puts_stdout("ipc_sync: spawned child pid=");
    put_num(child);
    put_char('\n');

    /* Send ROUNDS messages — each blocks until child receives (rendezvous) */
    for (int i = 1; i <= ROUNDS; i++) {
        struct ipc_message msg = { .tag = i, .data = { i * 111, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        puts_stdout("ipc_sync: sending msg ");
        put_num(i);
        put_char('\n');

        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            puts_stdout("ipc_sync: send failed: ");
            put_num(rc);
            put_char('\n');
            _exit(1);
        }
        puts_stdout("ipc_sync: send ");
        put_num(i);
        puts_stdout(" done\n");
    }

    /* Close send end — child will get EPIPE on next recv */
    close(send_fd);
    puts_stdout("ipc_sync: closed send end\n");

    /* Wait for child */
    int status = 0;
    waitpid(child, &status, 0);
    puts_stdout("ipc_sync: child exited\n");

    puts_stdout("PASS\n");
    _exit(0);
    return 0;
}
