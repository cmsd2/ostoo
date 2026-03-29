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

#include <unistd.h>
#include <string.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <spawn.h>

/* ── helpers ─────────────────────────────────────────────────────────── */

static void print(const char *s) {
    write(1, s, strlen(s));
}

static void put_char(char c) {
    write(1, &c, 1);
}

static void put_num(long n) {
    char buf[20];
    int i = 0;
    int neg = 0;
    if (n < 0) { neg = 1; n = -n; }
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    if (neg) put_char('-');
    while (--i >= 0) put_char(buf[i]);
}

static void itoa_buf(int val, char *buf, int bufsz) {
    int i = 0;
    int neg = 0;
    if (val < 0) { neg = 1; val = -val; }
    char tmp[20];
    if (val == 0) { tmp[i++] = '0'; }
    while (val > 0 && i < 18) {
        tmp[i++] = '0' + (val % 10);
        val /= 10;
    }
    int pos = 0;
    if (neg && pos < bufsz - 1) buf[pos++] = '-';
    while (--i >= 0 && pos < bufsz - 1) buf[pos++] = tmp[i];
    buf[pos] = '\0';
}

/* ── IPC channel syscall wrappers ────────────────────────────────────── */

#define SYS_IPC_CREATE 505
#define SYS_IPC_SEND   506

struct ipc_message {
    unsigned long tag;
    unsigned long data[3];
    int           fds[4];
};

static long ipc_create(int fds[2], unsigned capacity, unsigned flags) {
    return syscall(SYS_IPC_CREATE, fds, capacity, flags);
}

static long ipc_send(int fd, const struct ipc_message *msg, unsigned flags) {
    return syscall(SYS_IPC_SEND, fd, msg, flags);
}

/* ── main ────────────────────────────────────────────────────────────── */

#define ROUNDS 3

int main(void) {
    int fds[2];
    long rc;

    /* Create sync channel (capacity=0) */
    rc = ipc_create(fds, 0, 0);
    if (rc < 0) {
        print("ipc_sync: ipc_create failed: ");
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
        print("ipc_sync: posix_spawn failed\n");
        _exit(1);
    }

    /* Close recv end in parent — child has it */
    close(recv_fd);

    print("ipc_sync: spawned child pid=");
    put_num(child);
    put_char('\n');

    /* Send ROUNDS messages — each blocks until child receives (rendezvous) */
    for (int i = 1; i <= ROUNDS; i++) {
        struct ipc_message msg = { .tag = i, .data = { i * 111, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        print("ipc_sync: sending msg ");
        put_num(i);
        put_char('\n');

        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            print("ipc_sync: send failed: ");
            put_num(rc);
            put_char('\n');
            _exit(1);
        }
        print("ipc_sync: send ");
        put_num(i);
        print(" done\n");
    }

    /* Close send end — child will get EPIPE on next recv */
    close(send_fd);
    print("ipc_sync: closed send end\n");

    /* Wait for child */
    int status = 0;
    waitpid(child, &status, 0);
    print("ipc_sync: child exited\n");

    print("PASS\n");
    _exit(0);
    return 0;
}
