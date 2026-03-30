/*
 * ipc_pong — Child process for ipc_sync demo.
 *
 * Receives IPC messages on the fd passed as argv[1] until EPIPE.
 * Prints each received message.
 */

#include <string.h>
#include "ostoo.h"

/* ── main ────────────────────────────────────────────────────────────── */

int main(int argc, char *argv[]) {
    if (argc < 2) {
        puts_stdout("ipc_pong: usage: ipc_pong <recv_fd>\n");
        _exit(1);
    }

    int recv_fd = simple_atoi(argv[1]);

    puts_stdout("  ipc_pong: listening on fd ");
    put_num(recv_fd);
    put_char('\n');

    int count = 0;
    for (;;) {
        struct ipc_message msg;
        long rc = ipc_recv(recv_fd, &msg, 0);
        if (rc == -32) {  /* EPIPE — sender closed */
            puts_stdout("  ipc_pong: sender closed (EPIPE)\n");
            break;
        }
        if (rc < 0) {
            puts_stdout("  ipc_pong: recv error: ");
            put_num(rc);
            put_char('\n');
            break;
        }
        count++;
        puts_stdout("  ipc_pong: recv tag=");
        put_num((long)msg.tag);
        puts_stdout(" data[0]=");
        put_num((long)msg.data[0]);
        put_char('\n');
    }

    puts_stdout("  ipc_pong: received ");
    put_num(count);
    puts_stdout(" messages total\n");

    close(recv_fd);
    _exit(0);
    return 0;
}
