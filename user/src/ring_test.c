/*
 * ring_test — OP_RING_WAIT smoke test.
 *
 * Tests shared-memory + notification fd signaling through a completion port.
 *
 * Parent (consumer):
 *   1. Creates shmem, notify fd, and completion port
 *   2. mmaps shmem with MAP_SHARED
 *   3. Spawns child with shmem fd + notify fd as argv
 *   4. Submits OP_RING_WAIT on the notify fd
 *   5. io_wait blocks until child signals
 *   6. Reads magic from shared memory, verifies
 *
 * Child (producer):
 *   1. mmaps inherited shmem fd
 *   2. Writes magic pattern to shared memory
 *   3. Calls notify_signal(notify_fd) to wake parent
 *   4. Exits
 *
 * Expected output:
 *   ring_test: parent: shmem=N notify=M port=P
 *   ring_test: parent: mmap'd at 0x...
 *   ring_test: parent: spawning child
 *   ring_test: parent: waiting for OP_RING_WAIT...
 *   ring_test: child: mmap'd at 0x...
 *   ring_test: child: wrote DEADBEEF
 *   ring_test: child: signaled notify fd
 *   ring_test: parent: got completion, result=0
 *   ring_test: parent: shmem data OK
 *   ring_test: all tests passed
 */

#include <sys/mman.h>
#include <sys/wait.h>
#include <spawn.h>
#include <string.h>
#include "ostoo.h"

/* Magic pattern */
#define MAGIC 0xDEADBEEF

/* -- child mode --------------------------------------------------------- */

static int child_main(int shmem_fd, int notify_fd) {
    /* mmap the inherited shmem fd */
    void *ptr = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, shmem_fd, 0);
    if (ptr == MAP_FAILED) {
        puts_stdout("ring_test: child: mmap failed\n");
        return 1;
    }

    puts_stdout("ring_test: child: mmap'd at ");
    put_hex((unsigned long)ptr);
    put_char('\n');

    /* Write magic pattern */
    unsigned int *data = (unsigned int *)ptr;
    data[0] = MAGIC;
    puts_stdout("ring_test: child: wrote DEADBEEF\n");

    /* Signal the notify fd */
    long ret = notify_signal(notify_fd);
    if (ret < 0) {
        puts_stdout("ring_test: child: notify failed: ");
        put_dec(ret);
        put_char('\n');
        munmap(ptr, 4096);
        return 1;
    }
    puts_stdout("ring_test: child: signaled notify fd\n");

    munmap(ptr, 4096);
    return 0;
}

/* -- parent mode -------------------------------------------------------- */

int main(int argc, char **argv) {
    extern char **environ;

    /* Child mode: argv[1] = shmem fd, argv[2] = notify fd */
    if (argc > 2) {
        int shmem_fd = simple_atoi(argv[1]);
        int notify_fd = simple_atoi(argv[2]);
        _exit(child_main(shmem_fd, notify_fd));
        return 1;
    }

    /* -- Parent mode -- */

    /* Create shmem object */
    long shmem_fd = shmem_create(4096, 0);
    if (shmem_fd < 0) {
        puts_stdout("ring_test: shmem_create failed: ");
        put_dec(shmem_fd);
        put_char('\n');
        _exit(1);
    }

    /* Create notify fd */
    long nfd = notify_create(0);
    if (nfd < 0) {
        puts_stdout("ring_test: notify_create failed: ");
        put_dec(nfd);
        put_char('\n');
        _exit(1);
    }

    /* Create completion port */
    long port_fd = io_create(0);
    if (port_fd < 0) {
        puts_stdout("ring_test: io_create failed: ");
        put_dec(port_fd);
        put_char('\n');
        _exit(1);
    }

    puts_stdout("ring_test: parent: shmem=");
    put_dec(shmem_fd);
    puts_stdout(" notify=");
    put_dec(nfd);
    puts_stdout(" port=");
    put_dec(port_fd);
    put_char('\n');

    /* mmap the shmem */
    void *ptr = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, (int)shmem_fd, 0);
    if (ptr == MAP_FAILED) {
        puts_stdout("ring_test: parent: mmap failed\n");
        _exit(1);
    }

    puts_stdout("ring_test: parent: mmap'd at ");
    put_hex((unsigned long)ptr);
    put_char('\n');

    /* Clear data slot */
    unsigned int *data = (unsigned int *)ptr;
    data[0] = 0;

    /* Spawn child with shmem fd and notify fd as argv */
    char shmem_str[16], nfd_str[16];
    itoa_buf((int)shmem_fd, shmem_str, sizeof(shmem_str));
    itoa_buf((int)nfd, nfd_str, sizeof(nfd_str));

    puts_stdout("ring_test: parent: spawning child\n");

    const char *self_path = "/bin/ring_test";
    char *child_argv[] = { (char *)self_path, shmem_str, nfd_str, (char *)0 };
    pid_t child_pid;
    int err = posix_spawn(&child_pid, self_path, 0, 0, child_argv, environ);
    if (err != 0) {
        puts_stdout("ring_test: parent: spawn failed\n");
        _exit(1);
    }

    /* Submit OP_RING_WAIT on the notify fd */
    struct io_submission sub;
    memset(&sub, 0, sizeof(sub));
    sub.user_data = 42;
    sub.opcode = OP_RING_WAIT;
    sub.fd = (int)nfd;

    long ret = io_submit((int)port_fd, &sub, 1);
    if (ret < 0) {
        puts_stdout("ring_test: parent: io_submit failed: ");
        put_dec(ret);
        put_char('\n');
        _exit(1);
    }

    puts_stdout("ring_test: parent: waiting for OP_RING_WAIT...\n");

    /* Wait for completion */
    struct io_completion comp;
    ret = io_wait((int)port_fd, &comp, 1, 1, 0);
    if (ret < 0) {
        puts_stdout("ring_test: parent: io_wait failed: ");
        put_dec(ret);
        put_char('\n');
        _exit(1);
    }

    puts_stdout("ring_test: parent: got completion, result=");
    put_dec(comp.result);
    put_char('\n');

    /* Wait for child to exit */
    int status = 0;
    waitpid(child_pid, &status, 0);

    /* Verify the magic pattern in shared memory */
    if (data[0] != MAGIC) {
        puts_stdout("ring_test: parent: WRONG data: ");
        put_hex(data[0]);
        puts_stdout(" (expected ");
        put_hex(MAGIC);
        puts_stdout(")\n");
        munmap(ptr, 4096);
        _exit(1);
    }
    puts_stdout("ring_test: parent: shmem data OK\n");

    munmap(ptr, 4096);
    close((int)shmem_fd);
    close((int)nfd);
    close((int)port_fd);

    puts_stdout("ring_test: all tests passed\n");
    _exit(0);
    return 0;
}
