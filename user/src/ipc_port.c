/*
 * ipc_port — IPC channel + completion port integration demo.
 *
 * Demonstrates OP_IPC_RECV and OP_IPC_SEND: submit IPC operations via a
 * completion port.
 *
 * Tests:
 *   1. OP_IPC_RECV: arm, send, io_wait gets the message
 *   2. OP_IPC_RECV + OP_TIMEOUT: message arrives before timeout
 *   3. OP_IPC_SEND: submit send via port, recv synchronously
 *   4. OP_IPC_SEND + OP_IPC_RECV on same port: both complete
 *   5. OP_IPC_RECV then close send end → EPIPE
 *   6. OP_IPC_SEND then close recv end → EPIPE
 *   PASS
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

/* ── syscall wrappers ────────────────────────────────────────────────── */

#define SYS_IO_CREATE  501
#define SYS_IO_SUBMIT  502
#define SYS_IO_WAIT    503
#define SYS_IPC_CREATE 505
#define SYS_IPC_SEND   506

#define OP_TIMEOUT    1
#define OP_IPC_SEND   5
#define OP_IPC_RECV   6

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

struct io_completion {
    unsigned long user_data;
    long          result;
    unsigned int  flags;
    unsigned int  opcode;
};

struct ipc_message {
    unsigned long tag;
    unsigned long data[3];
    int           fds[4];
};

static long io_create(unsigned int flags) {
    return syscall(SYS_IO_CREATE, flags);
}

static long io_submit(int port_fd, struct io_submission *entries, unsigned count) {
    return syscall(SYS_IO_SUBMIT, port_fd, entries, count);
}

static long io_wait(int port_fd, struct io_completion *comps,
                    unsigned max, unsigned min, unsigned long timeout_ns) {
    return syscall(SYS_IO_WAIT, port_fd, comps, max, min, timeout_ns);
}

static long ipc_create(int fds[2], unsigned capacity, unsigned flags) {
    return syscall(SYS_IPC_CREATE, fds, capacity, flags);
}

static long ipc_send(int fd, const struct ipc_message *msg, unsigned flags) {
    return syscall(SYS_IPC_SEND, fd, msg, flags);
}

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    int pass = 1;

    /* Create completion port */
    int port_fd = (int)io_create(0);
    if (port_fd < 0) {
        print("ipc_port: io_create failed\n");
        _exit(1);
    }

    /* Create async IPC channel (capacity=4) */
    int fds[2];
    if (ipc_create(fds, 4, 0) < 0) {
        print("ipc_port: ipc_create failed\n");
        _exit(1);
    }
    int send_fd = fds[0];
    int recv_fd = fds[1];

    print("ipc_port: created port=");
    put_num(port_fd);
    print(" send_fd=");
    put_num(send_fd);
    print(" recv_fd=");
    put_num(recv_fd);
    put_char('\n');

    /* ── Test 1: arm OP_IPC_RECV, send, wait ──────────────────────── */
    {
        struct ipc_message recv_buf;
        memset(&recv_buf, 0, sizeof(recv_buf));

        struct io_submission sub;
        memset(&sub, 0, sizeof(sub));
        sub.opcode = OP_IPC_RECV;
        sub.fd = recv_fd;
        sub.buf_addr = (unsigned long)&recv_buf;
        sub.user_data = 100;

        print("test1: submit OP_IPC_RECV\n");
        io_submit(port_fd, &sub, 1);

        /* Send a message — should trigger the armed port */
        struct ipc_message msg = { .tag = 42, .data = { 1234, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        print("test1: send msg tag=42\n");
        ipc_send(send_fd, &msg, 0);

        /* Wait for completion */
        struct io_completion comp;
        long n = io_wait(port_fd, &comp, 1, 1, 0);
        if (n != 1) {
            print("test1: io_wait returned ");
            put_num(n);
            print(" -- FAIL\n");
            pass = 0;
        } else {
            print("test1: io_wait => opcode=");
            put_num(comp.opcode);
            print(" user_data=");
            put_num((long)comp.user_data);
            print(" result=");
            put_num(comp.result);
            put_char('\n');

            if (comp.opcode == OP_IPC_RECV && comp.user_data == 100
                && comp.result == 0
                && recv_buf.tag == 42 && recv_buf.data[0] == 1234) {
                print("test1: msg tag=42 data[0]=1234 -- correct\n");
            } else {
                print("test1: unexpected values -- FAIL\n");
                print("  recv_buf.tag=");
                put_num((long)recv_buf.tag);
                print(" recv_buf.data[0]=");
                put_num((long)recv_buf.data[0]);
                put_char('\n');
                pass = 0;
            }
        }
    }

    /* ── Test 2: OP_IPC_RECV + OP_TIMEOUT, message arrives first ── */
    {
        struct ipc_message recv_buf;
        memset(&recv_buf, 0, sizeof(recv_buf));

        struct io_submission subs[2];
        memset(subs, 0, sizeof(subs));
        subs[0].opcode = OP_IPC_RECV;
        subs[0].fd = recv_fd;
        subs[0].buf_addr = (unsigned long)&recv_buf;
        subs[0].user_data = 200;
        subs[1].opcode = OP_TIMEOUT;
        subs[1].timeout_ns = 5000000000UL;  /* 5 seconds */
        subs[1].user_data = 201;

        print("test2: submit OP_IPC_RECV + OP_TIMEOUT\n");
        io_submit(port_fd, subs, 2);

        /* Send immediately — should beat the timeout */
        struct ipc_message msg = { .tag = 99, .data = { 5678, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        print("test2: send msg tag=99\n");
        ipc_send(send_fd, &msg, 0);

        /* Wait for at least 1 completion */
        struct io_completion comp;
        long n = io_wait(port_fd, &comp, 1, 1, 0);
        if (n != 1) {
            print("test2: io_wait returned ");
            put_num(n);
            print(" -- FAIL\n");
            pass = 0;
        } else {
            print("test2: io_wait => opcode=");
            put_num(comp.opcode);
            print(" user_data=");
            put_num((long)comp.user_data);
            print(" result=");
            put_num(comp.result);
            put_char('\n');

            if (comp.opcode == OP_IPC_RECV && comp.user_data == 200
                && comp.result == 0
                && recv_buf.tag == 99 && recv_buf.data[0] == 5678) {
                print("test2: msg tag=99 data[0]=5678 -- correct\n");
            } else {
                print("test2: unexpected -- FAIL\n");
                pass = 0;
            }
        }
    }

    /* ── Test 3: OP_IPC_SEND via port, recv synchronously ────────── */
    {
        struct ipc_message send_msg = { .tag = 77, .data = { 9999, 0, 0 }, .fds = { -1, -1, -1, -1 } };

        struct io_submission sub;
        memset(&sub, 0, sizeof(sub));
        sub.opcode = OP_IPC_SEND;
        sub.fd = send_fd;
        sub.buf_addr = (unsigned long)&send_msg;
        sub.user_data = 300;

        print("test3: submit OP_IPC_SEND\n");
        io_submit(port_fd, &sub, 1);

        /* Recv synchronously — should get the message */
        struct ipc_message recv_buf;
        long rc = syscall(507, recv_fd, &recv_buf, 0);  /* SYS_IPC_RECV */

        /* Check the send completion */
        struct io_completion comp;
        long n = io_wait(port_fd, &comp, 1, 1, 0);

        print("test3: io_wait => opcode=");
        put_num(comp.opcode);
        print(" user_data=");
        put_num((long)comp.user_data);
        print(" result=");
        put_num(comp.result);
        put_char('\n');

        if (n == 1 && comp.opcode == OP_IPC_SEND && comp.user_data == 300
            && comp.result == 0
            && rc == 0 && recv_buf.tag == 77 && recv_buf.data[0] == 9999) {
            print("test3: send via port, recv tag=77 -- correct\n");
        } else {
            print("test3: unexpected -- FAIL\n");
            pass = 0;
        }
    }

    /* ── Test 4: OP_IPC_SEND + OP_IPC_RECV on same port ──────────── */
    {
        /* Create a second channel for this test */
        int fds2[2];
        if (ipc_create(fds2, 4, 0) < 0) {
            print("test4: ipc_create failed\n");
            pass = 0;
        } else {
            struct ipc_message send_msg = { .tag = 55, .data = { 1111, 0, 0 }, .fds = { -1, -1, -1, -1 } };
            struct ipc_message recv_buf;
            memset(&recv_buf, 0, sizeof(recv_buf));

            struct io_submission subs[2];
            memset(subs, 0, sizeof(subs));
            /* Submit send on fds2[0] */
            subs[0].opcode = OP_IPC_SEND;
            subs[0].fd = fds2[0];
            subs[0].buf_addr = (unsigned long)&send_msg;
            subs[0].user_data = 400;
            /* Submit recv on fds2[1] */
            subs[1].opcode = OP_IPC_RECV;
            subs[1].fd = fds2[1];
            subs[1].buf_addr = (unsigned long)&recv_buf;
            subs[1].user_data = 401;

            print("test4: submit OP_IPC_SEND + OP_IPC_RECV\n");
            io_submit(port_fd, subs, 2);

            /* Both should complete — wait for 2 */
            struct io_completion comps[2];
            long n = io_wait(port_fd, comps, 2, 2, 0);

            print("test4: io_wait returned ");
            put_num(n);
            print(" completions\n");

            int got_send = 0, got_recv = 0;
            for (int i = 0; i < n; i++) {
                print("  comp: opcode=");
                put_num(comps[i].opcode);
                print(" user_data=");
                put_num((long)comps[i].user_data);
                print(" result=");
                put_num(comps[i].result);
                put_char('\n');
                if (comps[i].opcode == OP_IPC_SEND && comps[i].user_data == 400
                    && comps[i].result == 0)
                    got_send = 1;
                if (comps[i].opcode == OP_IPC_RECV && comps[i].user_data == 401
                    && comps[i].result == 0)
                    got_recv = 1;
            }

            if (got_send && got_recv
                && recv_buf.tag == 55 && recv_buf.data[0] == 1111) {
                print("test4: both completions, msg correct -- correct\n");
            } else {
                print("test4: unexpected -- FAIL\n");
                pass = 0;
            }

            close(fds2[0]);
            close(fds2[1]);
        }
    }

    /* ── Test 5: OP_IPC_RECV then close send end → EPIPE ─────────── */
    {
        struct ipc_message recv_buf;
        memset(&recv_buf, 0, sizeof(recv_buf));

        struct io_submission sub;
        memset(&sub, 0, sizeof(sub));
        sub.opcode = OP_IPC_RECV;
        sub.fd = recv_fd;
        sub.buf_addr = (unsigned long)&recv_buf;
        sub.user_data = 500;

        print("test5: submit OP_IPC_RECV\n");
        io_submit(port_fd, &sub, 1);

        /* Close send end — should trigger EPIPE completion */
        close(send_fd);
        print("test5: close send end\n");

        struct io_completion comp;
        long n = io_wait(port_fd, &comp, 1, 1, 0);

        print("test5: io_wait => opcode=");
        put_num(comp.opcode);
        print(" user_data=");
        put_num((long)comp.user_data);
        print(" result=");
        put_num(comp.result);
        put_char('\n');

        if (n == 1 && comp.opcode == OP_IPC_RECV && comp.user_data == 500
            && comp.result == -32) {
            print("test5: EPIPE -- correct\n");
        } else {
            print("test5: unexpected -- FAIL\n");
            pass = 0;
        }
    }

    /* ── Test 6: OP_IPC_SEND then close recv end → EPIPE ─────────── */
    {
        /* Need a fresh channel since send_fd is closed */
        int fds3[2];
        if (ipc_create(fds3, 4, 0) < 0) {
            print("test6: ipc_create failed\n");
            pass = 0;
        } else {
            struct ipc_message send_msg = { .tag = 1, .data = { 0, 0, 0 }, .fds = { -1, -1, -1, -1 } };

            struct io_submission sub;
            memset(&sub, 0, sizeof(sub));
            sub.opcode = OP_IPC_SEND;
            sub.fd = fds3[0];
            sub.buf_addr = (unsigned long)&send_msg;
            sub.user_data = 600;

            /* Fill the queue to force the send to pend */
            for (int i = 0; i < 4; i++) {
                struct ipc_message fill = { .tag = 0, .data = { 0, 0, 0 }, .fds = { -1, -1, -1, -1 } };
                ipc_send(fds3[0], &fill, 0);
            }

            print("test6: submit OP_IPC_SEND (queue full)\n");
            io_submit(port_fd, &sub, 1);

            /* Close recv end — should trigger EPIPE on the pending send */
            close(fds3[1]);
            print("test6: close recv end\n");

            struct io_completion comp;
            long n = io_wait(port_fd, &comp, 1, 1, 0);

            print("test6: io_wait => opcode=");
            put_num(comp.opcode);
            print(" user_data=");
            put_num((long)comp.user_data);
            print(" result=");
            put_num(comp.result);
            put_char('\n');

            if (n == 1 && comp.opcode == OP_IPC_SEND && comp.user_data == 600
                && comp.result == -32) {
                print("test6: EPIPE -- correct\n");
            } else {
                print("test6: unexpected -- FAIL\n");
                pass = 0;
            }

            close(fds3[0]);
        }
    }

    close(recv_fd);
    close(port_fd);

    if (pass) {
        print("PASS\n");
    } else {
        print("FAIL\n");
        _exit(1);
    }

    _exit(0);
    return 0;
}
