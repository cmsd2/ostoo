/*
 * sig_int.c — SIGINT (Ctrl+C) delivery demo.
 *
 * Installs a SIGINT handler, then reads from stdin.  When Ctrl+C is
 * pressed, the handler fires and read() returns -1 with errno == EINTR.
 *
 * Usage: run this program, then press Ctrl+C.
 */
#include <signal.h>
#include <string.h>
#include <errno.h>
#include "ostoo.h"

static volatile int got_sigint = 0;

static void handler(int sig) {
    (void)sig;
    got_sigint = 1;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;

    puts_stdout("sig_int: installing SIGINT handler\n");
    if (sigaction(SIGINT, &sa, NULL) < 0) {
        puts_stdout("sig_int: sigaction failed\n");
        _exit(1);
    }

    puts_stdout("sig_int: reading from stdin (press Ctrl+C)...\n");
    char buf[64];
    ssize_t n = read(0, buf, sizeof(buf));

    if (n < 0 && errno == EINTR) {
        puts_stdout("sig_int: read returned EINTR\n");
    } else if (n < 0) {
        puts_stdout("sig_int: read returned error (not EINTR)\n");
    } else {
        puts_stdout("sig_int: read returned data (no signal?)\n");
    }

    if (got_sigint) {
        puts_stdout("sig_int: PASS - SIGINT handler fired\n");
    } else {
        puts_stdout("sig_int: FAIL - SIGINT handler did not fire\n");
        _exit(2);
    }

    return 0;
}
