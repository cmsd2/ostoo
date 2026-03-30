/*
 * sig_demo.c — POSIX signal delivery smoke test.
 *
 * Installs a SIGUSR1 handler, sends SIGUSR1 to self, verifies the handler ran.
 */
#include <signal.h>
#include <string.h>
#include "ostoo.h"

static volatile int handler_count = 0;

static void handler(int sig) {
    (void)sig;
    handler_count++;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;
    sa.sa_flags = 0;  /* musl adds SA_RESTORER automatically */

    puts_stdout("sig_demo: installing SIGUSR1 handler\n");
    if (sigaction(SIGUSR1, &sa, NULL) < 0) {
        puts_stdout("sig_demo: sigaction failed\n");
        _exit(1);
    }

    puts_stdout("sig_demo: sending SIGUSR1 to self\n");
    if (kill(getpid(), SIGUSR1) < 0) {
        puts_stdout("sig_demo: kill failed\n");
        _exit(2);
    }

    if (handler_count == 1) {
        puts_stdout("sig_demo: PASS — handler ran once\n");
    } else {
        puts_stdout("sig_demo: FAIL — handler_count != 1\n");
        _exit(3);
    }

    puts_stdout("sig_demo: done\n");
    return 0;
}
