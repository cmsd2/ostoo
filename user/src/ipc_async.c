/*
 * ipc_async — Async (buffered) IPC channel demo.
 *
 * Creates an async channel with capacity 4, fills the buffer, drains it,
 * then tests IPC_NONBLOCK on empty receive.
 *
 * Expected output:
 *   ipc_async: created channel send_fd=N recv_fd=M
 *   ipc_async: sent 4 messages
 *   recv tag=1 data[0]=100
 *   recv tag=2 data[0]=200
 *   recv tag=3 data[0]=300
 *   recv tag=4 data[0]=400
 *   ipc_async: drained 4 messages
 *   ipc_async: nonblock recv => EAGAIN -- correct
 *   ipc_async: closed send end
 *   ipc_async: recv after close => EPIPE -- correct
 *   PASS
 */

#include <unistd.h>
#include <string.h>
#include <errno.h>
#include <sys/syscall.h>

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

/* ── IPC channel syscall wrappers ────────────────────────────────────── */

#define SYS_IPC_CREATE 505
#define SYS_IPC_SEND   506
#define SYS_IPC_RECV   507

#define IPC_CLOEXEC  0x1
#define IPC_NONBLOCK 0x1

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

static long ipc_recv(int fd, struct ipc_message *msg, unsigned flags) {
    return syscall(SYS_IPC_RECV, fd, msg, flags);
}

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    int fds[2];
    long rc;
    int pass = 1;

    /* Create async channel with capacity 4 */
    rc = ipc_create(fds, 4, 0);
    if (rc < 0) {
        print("ipc_async: ipc_create failed: ");
        put_num(rc);
        put_char('\n');
        _exit(1);
    }
    int send_fd = fds[0];
    int recv_fd = fds[1];

    print("ipc_async: created channel send_fd=");
    put_num(send_fd);
    print(" recv_fd=");
    put_num(recv_fd);
    put_char('\n');

    /* Fill the buffer with 4 messages (should not block) */
    for (int i = 1; i <= 4; i++) {
        struct ipc_message msg = { .tag = i, .data = { i * 100, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            print("ipc_async: send failed at i=");
            put_num(i);
            print(" errno=");
            put_num(errno);
            put_char('\n');
            pass = 0;
            break;
        }
    }
    print("ipc_async: sent 4 messages\n");

    /* Drain all 4 messages */
    int count = 0;
    for (int i = 0; i < 4; i++) {
        struct ipc_message msg;
        rc = ipc_recv(recv_fd, &msg, 0);
        if (rc < 0) {
            print("ipc_async: recv failed errno=");
            put_num(errno);
            put_char('\n');
            pass = 0;
            break;
        }
        print("  recv tag=");
        put_num((long)msg.tag);
        print(" data[0]=");
        put_num((long)msg.data[0]);
        put_char('\n');
        count++;
    }
    print("ipc_async: drained ");
    put_num(count);
    print(" messages\n");

    /* Test IPC_NONBLOCK on empty channel */
    {
        struct ipc_message msg;
        errno = 0;
        rc = ipc_recv(recv_fd, &msg, IPC_NONBLOCK);
        print("ipc_async: nonblock recv => ");
        if (rc == -1 && errno == EAGAIN) {
            print("EAGAIN -- correct\n");
        } else {
            print("rc=");
            put_num(rc);
            print(" errno=");
            put_num(errno);
            print(" -- UNEXPECTED\n");
            pass = 0;
        }
    }

    /* Close send end, then try recv => should get EPIPE */
    close(send_fd);
    print("ipc_async: closed send end\n");
    {
        struct ipc_message msg;
        errno = 0;
        rc = ipc_recv(recv_fd, &msg, 0);
        print("ipc_async: recv after close => ");
        if (rc == -1 && errno == EPIPE) {
            print("EPIPE -- correct\n");
        } else {
            print("rc=");
            put_num(rc);
            print(" errno=");
            put_num(errno);
            print(" -- UNEXPECTED\n");
            pass = 0;
        }
    }

    close(recv_fd);

    if (pass) {
        print("PASS\n");
    } else {
        print("FAIL\n");
        _exit(1);
    }

    _exit(0);
    return 0;
}
