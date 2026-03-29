/*
 * io_pong — Simple child for io_pingpong.
 *
 * Reads messages from a pipe, replaces "ping" with "pong", writes reply.
 * No completion port used — plain blocking read/write.
 *
 * Usage: io_pong <read_fd> <write_fd>
 */

#include <unistd.h>
#include <string.h>

static void puts_stderr(const char *s) {
    write(2, s, strlen(s));
}

/* Simple atoi (positive only) */
static int my_atoi(const char *s) {
    int n = 0;
    while (*s >= '0' && *s <= '9') {
        n = n * 10 + (*s - '0');
        s++;
    }
    return n;
}

int main(int argc, char *argv[]) {
    if (argc < 3) {
        puts_stderr("io_pong: usage: io_pong <read_fd> <write_fd>\n");
        _exit(1);
    }

    int rd_fd = my_atoi(argv[1]);
    int wr_fd = my_atoi(argv[2]);

    char buf[256];

    for (;;) {
        ssize_t n = read(rd_fd, buf, sizeof(buf) - 1);
        if (n <= 0) break; /* EOF or error */

        buf[n] = '\0';

        /* Replace "ping" with "pong" at start of message */
        if (n >= 4 && buf[0] == 'p' && buf[1] == 'i' && buf[2] == 'n' && buf[3] == 'g') {
            buf[1] = 'o';
            buf[2] = 'n';
            /* buf[3] is already 'g' */
        }

        write(wr_fd, buf, (size_t)n);
    }

    close(rd_fd);
    close(wr_fd);
    _exit(0);
    return 0;
}
