/*
 * sig_segv.c — SIGSEGV signal delivery demo.
 *
 * Installs a SIGSEGV handler with SA_SIGINFO, then dereferences NULL.
 * The handler receives si_addr == 0 (the faulting address) and the
 * program exits cleanly instead of being killed.
 */
#include <signal.h>
#include <string.h>
#include <setjmp.h>
#include "ostoo.h"

static sigjmp_buf jump_buf;
static volatile int got_sigsegv = 0;
static volatile void *fault_addr = (void *)1; /* sentinel */

static void handler(int sig, siginfo_t *info, void *ucontext) {
    (void)sig;
    (void)ucontext;
    got_sigsegv = 1;
    fault_addr = info->si_addr;
    siglongjmp(jump_buf, 1);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO;

    puts_stdout("sig_segv: installing SIGSEGV handler (SA_SIGINFO)\n");
    if (sigaction(SIGSEGV, &sa, NULL) < 0) {
        puts_stdout("sig_segv: sigaction failed\n");
        _exit(1);
    }

    if (sigsetjmp(jump_buf, 1) == 0) {
        puts_stdout("sig_segv: dereferencing NULL...\n");
        volatile int *p = (volatile int *)0;
        (void)*p;  /* should trigger SIGSEGV */
        puts_stdout("sig_segv: FAIL - no fault occurred\n");
        _exit(2);
    }

    /* Landed here from siglongjmp in the handler. */
    if (got_sigsegv) {
        puts_stdout("sig_segv: PASS - SIGSEGV handler fired");
        if (fault_addr == (void *)0) {
            puts_stdout(", si_addr=0 (correct)\n");
        } else {
            puts_stdout(", si_addr != 0 (unexpected)\n");
        }
    } else {
        puts_stdout("sig_segv: FAIL - handler did not fire\n");
        _exit(3);
    }

    return 0;
}
