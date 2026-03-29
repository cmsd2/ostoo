/*
 * sig_chld.c — SIGCHLD delivery demo.
 *
 * Installs a SIGCHLD handler, spawns a child that exits immediately,
 * then waits.  The handler should fire when the child exits.
 */
#include <signal.h>
#include <unistd.h>
#include <string.h>
#include <sys/wait.h>
#include <spawn.h>

static volatile int got_sigchld = 0;

static void handler(int sig) {
    (void)sig;
    got_sigchld = 1;
}

static void print(const char *s) {
    write(1, s, strlen(s));
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handler;

    print("sig_chld: installing SIGCHLD handler\n");
    if (sigaction(SIGCHLD, &sa, NULL) < 0) {
        print("sig_chld: sigaction failed\n");
        _exit(1);
    }

    /* Spawn a child that exits immediately. */
    print("sig_chld: spawning child (hello)\n");
    pid_t child;
    char *argv[] = { "hello", NULL };
    char *envp[] = { NULL };
    int rc = posix_spawn(&child, "/host/bin/hello", NULL, NULL, argv, envp);
    if (rc != 0) {
        print("sig_chld: posix_spawn failed\n");
        _exit(2);
    }

    /* Wait for the child to exit. */
    int status;
    waitpid(child, &status, 0);

    if (got_sigchld) {
        print("sig_chld: PASS - SIGCHLD handler fired\n");
    } else {
        print("sig_chld: FAIL - SIGCHLD handler did not fire\n");
        _exit(3);
    }

    return 0;
}
