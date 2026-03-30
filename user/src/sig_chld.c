/*
 * sig_chld.c — SIGCHLD delivery demo.
 *
 * Installs a SIGCHLD handler, spawns a child that exits immediately,
 * then waits.  The handler should fire when the child exits.
 */
#include <signal.h>
#include <string.h>
#include <sys/wait.h>
#include <spawn.h>
#include "ostoo.h"

static volatile int got_sigchld = 0;

static void handler(int sig) {
    (void)sig;
    got_sigchld = 1;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;

    puts_stdout("sig_chld: installing SIGCHLD handler\n");
    if (sigaction(SIGCHLD, &sa, NULL) < 0) {
        puts_stdout("sig_chld: sigaction failed\n");
        _exit(1);
    }

    /* Spawn a child that exits immediately. */
    puts_stdout("sig_chld: spawning child (hello)\n");
    pid_t child;
    char *argv[] = { "hello", NULL };
    char *envp[] = { NULL };
    int rc = posix_spawn(&child, "/host/bin/hello", NULL, NULL, argv, envp);
    if (rc != 0) {
        puts_stdout("sig_chld: posix_spawn failed\n");
        _exit(2);
    }

    /* Wait for the child to exit. */
    int status;
    waitpid(child, &status, 0);

    if (got_sigchld) {
        puts_stdout("sig_chld: PASS - SIGCHLD handler fired\n");
    } else {
        puts_stdout("sig_chld: FAIL - SIGCHLD handler did not fire\n");
        _exit(3);
    }

    return 0;
}
