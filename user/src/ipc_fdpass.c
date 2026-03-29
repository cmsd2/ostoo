/*
 * ipc_fdpass — IPC fd-passing (capability transfer) demo.
 *
 * Demonstrates sending a file descriptor through an IPC channel:
 *   1. Create a pipe and an IPC channel
 *   2. Send the pipe write-end fd through the IPC channel
 *   3. Receive the message — kernel allocates a new fd for the pipe write-end
 *   4. Write "hello" through the received fd
 *   5. Read from the pipe read-end to verify
 *   6. Send stdout (fd 1) through the channel, write through received fd
 *
 * Expected output:
 *   ipc_fdpass: created pipe read_fd=N write_fd=M
 *   ipc_fdpass: created channel send_fd=N recv_fd=M
 *   ipc_fdpass: sent write_fd=M through channel
 *   ipc_fdpass: received new_fd=N
 *   ipc_fdpass: wrote 5 bytes through new_fd
 *   ipc_fdpass: read 5 bytes from pipe: hello
 *   test1: fd transfer -- correct
 *   ipc_fdpass: sent stdout through channel
 *   ipc_fdpass: received new_stdout=N
 *   test2: stdout transfer -- correct
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
    int pass = 1;
    long rc;

    /* Create a pipe */
    int pipe_fds[2];
    if (pipe(pipe_fds) < 0) {
        print("ipc_fdpass: pipe failed\n");
        _exit(1);
    }
    int pipe_read = pipe_fds[0];
    int pipe_write = pipe_fds[1];

    print("ipc_fdpass: created pipe read_fd=");
    put_num(pipe_read);
    print(" write_fd=");
    put_num(pipe_write);
    put_char('\n');

    /* Create an async IPC channel */
    int ch_fds[2];
    if (ipc_create(ch_fds, 4, 0) < 0) {
        print("ipc_fdpass: ipc_create failed\n");
        _exit(1);
    }
    int send_fd = ch_fds[0];
    int recv_fd = ch_fds[1];

    print("ipc_fdpass: created channel send_fd=");
    put_num(send_fd);
    print(" recv_fd=");
    put_num(recv_fd);
    put_char('\n');

    /* ── Test 1: Send pipe write-end through channel ────────────────── */
    {
        struct ipc_message msg = {
            .tag = 1,
            .data = { 0, 0, 0 },
            .fds = { pipe_write, -1, -1, -1 },
        };

        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            print("ipc_fdpass: send failed: ");
            put_num(rc);
            put_char('\n');
            pass = 0;
        } else {
            print("ipc_fdpass: sent write_fd=");
            put_num(pipe_write);
            print(" through channel\n");
        }

        /* Receive — should get a NEW fd number for the pipe write-end */
        struct ipc_message recv_msg;
        rc = ipc_recv(recv_fd, &recv_msg, 0);
        if (rc < 0) {
            print("ipc_fdpass: recv failed: ");
            put_num(rc);
            put_char('\n');
            pass = 0;
        } else {
            int new_fd = recv_msg.fds[0];
            print("ipc_fdpass: received new_fd=");
            put_num(new_fd);
            put_char('\n');

            if (new_fd < 0 || new_fd == pipe_write) {
                print("test1: bad fd -- FAIL\n");
                pass = 0;
            } else {
                /* Write through the received fd */
                const char *hello = "hello";
                long written = write(new_fd, hello, 5);
                print("ipc_fdpass: wrote ");
                put_num(written);
                print(" bytes through new_fd\n");

                /* Read from pipe read-end */
                char buf[16];
                long nread = read(pipe_read, buf, sizeof(buf));
                print("ipc_fdpass: read ");
                put_num(nread);
                print(" bytes from pipe: ");
                if (nread > 0) write(1, buf, nread);
                put_char('\n');

                if (nread == 5 && buf[0] == 'h' && buf[4] == 'o') {
                    print("test1: fd transfer -- correct\n");
                } else {
                    print("test1: data mismatch -- FAIL\n");
                    pass = 0;
                }

                close(new_fd);
            }
        }
    }

    /* ── Test 2: Send stdout (fd 1) through channel ─────────────────── */
    {
        struct ipc_message msg = {
            .tag = 2,
            .data = { 0, 0, 0 },
            .fds = { 1, -1, -1, -1 },  /* send stdout */
        };

        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            print("ipc_fdpass: send stdout failed: ");
            put_num(rc);
            put_char('\n');
            pass = 0;
        } else {
            print("ipc_fdpass: sent stdout through channel\n");
        }

        struct ipc_message recv_msg;
        rc = ipc_recv(recv_fd, &recv_msg, 0);
        if (rc < 0) {
            print("ipc_fdpass: recv stdout failed: ");
            put_num(rc);
            put_char('\n');
            pass = 0;
        } else {
            int new_stdout = recv_msg.fds[0];
            print("ipc_fdpass: received new_stdout=");
            put_num(new_stdout);
            put_char('\n');

            if (new_stdout > 0 && new_stdout != 1) {
                /* Write directly through the new fd */
                const char *test_msg = "test2: stdout transfer -- correct\n";
                write(new_stdout, test_msg, strlen(test_msg));
                close(new_stdout);
            } else {
                print("test2: bad stdout fd -- FAIL\n");
                pass = 0;
            }
        }
    }

    close(pipe_read);
    close(pipe_write);
    close(send_fd);
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
