/*
 * sig_int.c — SIGINT (Ctrl+C) delivery demo.
 *
 * Installs a SIGINT handler, then reads from stdin.  When Ctrl+C is
 * pressed, the handler fires and read() returns -1 with errno == EINTR.
 *
 * Usage: run this program, then press Ctrl+C.
 */
#include <signal.h>
#include <unistd.h>
#include <string.h>
#include <errno.h>

static volatile int got_sigint = 0;

static void handler(int sig) {
    (void)sig;
    got_sigint = 1;
}

static void print(const char *s) {
    write(1, s, strlen(s));
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;

    print("sig_int: installing SIGINT handler\n");
    if (sigaction(SIGINT, &sa, NULL) < 0) {
        print("sig_int: sigaction failed\n");
        _exit(1);
    }

    print("sig_int: reading from stdin (press Ctrl+C)...\n");
    char buf[64];
    ssize_t n = read(0, buf, sizeof(buf));

    if (n < 0 && errno == EINTR) {
        print("sig_int: read returned EINTR\n");
    } else if (n < 0) {
        print("sig_int: read returned error (not EINTR)\n");
    } else {
        print("sig_int: read returned data (no signal?)\n");
    }

    if (got_sigint) {
        print("sig_int: PASS - SIGINT handler fired\n");
    } else {
        print("sig_int: FAIL - SIGINT handler did not fire\n");
        _exit(2);
    }

    return 0;
}
