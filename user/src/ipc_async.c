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

#include <string.h>
#include <errno.h>
#include "ostoo.h"

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    int fds[2];
    long rc;
    int pass = 1;

    /* Create async channel with capacity 4 */
    rc = ipc_create(fds, 4, 0);
    if (rc < 0) {
        puts_stdout("ipc_async: ipc_create failed: ");
        put_num(rc);
        put_char('\n');
        _exit(1);
    }
    int send_fd = fds[0];
    int recv_fd = fds[1];

    puts_stdout("ipc_async: created channel send_fd=");
    put_num(send_fd);
    puts_stdout(" recv_fd=");
    put_num(recv_fd);
    put_char('\n');

    /* Fill the buffer with 4 messages (should not block) */
    for (int i = 1; i <= 4; i++) {
        struct ipc_message msg = { .tag = i, .data = { i * 100, 0, 0 }, .fds = { -1, -1, -1, -1 } };
        rc = ipc_send(send_fd, &msg, 0);
        if (rc < 0) {
            puts_stdout("ipc_async: send failed at i=");
            put_num(i);
            puts_stdout(" errno=");
            put_num(errno);
            put_char('\n');
            pass = 0;
            break;
        }
    }
    puts_stdout("ipc_async: sent 4 messages\n");

    /* Drain all 4 messages */
    int count = 0;
    for (int i = 0; i < 4; i++) {
        struct ipc_message msg;
        rc = ipc_recv(recv_fd, &msg, 0);
        if (rc < 0) {
            puts_stdout("ipc_async: recv failed errno=");
            put_num(errno);
            put_char('\n');
            pass = 0;
            break;
        }
        puts_stdout("  recv tag=");
        put_num((long)msg.tag);
        puts_stdout(" data[0]=");
        put_num((long)msg.data[0]);
        put_char('\n');
        count++;
    }
    puts_stdout("ipc_async: drained ");
    put_num(count);
    puts_stdout(" messages\n");

    /* Test IPC_NONBLOCK on empty channel */
    {
        struct ipc_message msg;
        errno = 0;
        rc = ipc_recv(recv_fd, &msg, IPC_NONBLOCK);
        puts_stdout("ipc_async: nonblock recv => ");
        if (rc == -1 && errno == EAGAIN) {
            puts_stdout("EAGAIN -- correct\n");
        } else {
            puts_stdout("rc=");
            put_num(rc);
            puts_stdout(" errno=");
            put_num(errno);
            puts_stdout(" -- UNEXPECTED\n");
            pass = 0;
        }
    }

    /* Close send end, then try recv => should get EPIPE */
    close(send_fd);
    puts_stdout("ipc_async: closed send end\n");
    {
        struct ipc_message msg;
        errno = 0;
        rc = ipc_recv(recv_fd, &msg, 0);
        puts_stdout("ipc_async: recv after close => ");
        if (rc == -1 && errno == EPIPE) {
            puts_stdout("EPIPE -- correct\n");
        } else {
            puts_stdout("rc=");
            put_num(rc);
            puts_stdout(" errno=");
            put_num(errno);
            puts_stdout(" -- UNEXPECTED\n");
            pass = 0;
        }
    }

    close(recv_fd);

    if (pass) {
        puts_stdout("PASS\n");
    } else {
        puts_stdout("FAIL\n");
        _exit(1);
    }

    _exit(0);
    return 0;
}
