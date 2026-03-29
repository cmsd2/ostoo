/*
 * ipc_pong — Child process for ipc_sync demo.
 *
 * Receives IPC messages on the fd passed as argv[1] until EPIPE.
 * Prints each received message.
 */

#include <unistd.h>
#include <string.h>
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

static int atoi_simple(const char *s) {
    int val = 0;
    int neg = 0;
    if (*s == '-') { neg = 1; s++; }
    while (*s >= '0' && *s <= '9') {
        val = val * 10 + (*s - '0');
        s++;
    }
    return neg ? -val : val;
}

/* ── IPC channel syscall wrapper ─────────────────────────────────────── */

#define SYS_IPC_RECV 507

struct ipc_message {
    unsigned long tag;
    unsigned long data[3];
    int           fds[4];
};

static long ipc_recv(int fd, struct ipc_message *msg, unsigned flags) {
    return syscall(SYS_IPC_RECV, fd, msg, flags);
}

/* ── main ────────────────────────────────────────────────────────────── */

int main(int argc, char *argv[]) {
    if (argc < 2) {
        print("ipc_pong: usage: ipc_pong <recv_fd>\n");
        _exit(1);
    }

    int recv_fd = atoi_simple(argv[1]);

    print("  ipc_pong: listening on fd ");
    put_num(recv_fd);
    put_char('\n');

    int count = 0;
    for (;;) {
        struct ipc_message msg;
        long rc = ipc_recv(recv_fd, &msg, 0);
        if (rc == -32) {  /* EPIPE — sender closed */
            print("  ipc_pong: sender closed (EPIPE)\n");
            break;
        }
        if (rc < 0) {
            print("  ipc_pong: recv error: ");
            put_num(rc);
            put_char('\n');
            break;
        }
        count++;
        print("  ipc_pong: recv tag=");
        put_num((long)msg.tag);
        print(" data[0]=");
        put_num((long)msg.data[0]);
        put_char('\n');
    }

    print("  ipc_pong: received ");
    put_num(count);
    print(" messages total\n");

    close(recv_fd);
    _exit(0);
    return 0;
}
