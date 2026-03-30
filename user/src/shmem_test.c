/*
 * shmem_test — shared memory IPC test.
 *
 * Tests:
 *   1. Parent creates a shmem object via shmem_create(4096)
 *   2. Parent mmaps it with MAP_SHARED, writes a magic pattern
 *   3. Parent spawns itself with the fd number as argv[1]
 *   4. Child mmaps the same fd, verifies the parent's pattern
 *   5. Child writes a response pattern, exits
 *   6. Parent waits for child, verifies the response
 *
 * Expected output:
 *   shmem_test: parent: shmem fd=N
 *   shmem_test: parent: mmap'd at 0x...
 *   shmem_test: parent: spawning child with fd N
 *   shmem_test: child: mmap'd fd N at 0x...
 *   shmem_test: child: pattern OK
 *   shmem_test: child: wrote response
 *   shmem_test: parent: child exited
 *   shmem_test: parent: response OK
 *   shmem_test: all tests passed
 */

#include <unistd.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <spawn.h>
#include <string.h>

/* Custom syscall: shmem_create(size, flags) -> fd */
#define SYS_SHMEM_CREATE 508
#define SHM_CLOEXEC 0x01

static long shmem_create(unsigned long size, unsigned int flags) {
    return syscall(SYS_SHMEM_CREATE, size, flags);
}

/* -- helpers ------------------------------------------------------------ */

static void puts_stdout(const char *s) {
    write(1, s, strlen(s));
}

static void put_char(char c) {
    write(1, &c, 1);
}

static void put_hex(unsigned long n) {
    char buf[17];
    int i = 0;
    if (n == 0) { puts_stdout("0x0"); return; }
    while (n > 0) {
        int d = n & 0xF;
        buf[i++] = d < 10 ? '0' + d : 'a' + d - 10;
        n >>= 4;
    }
    puts_stdout("0x");
    while (--i >= 0) put_char(buf[i]);
}

static void put_dec(long n) {
    char buf[21];
    int i = 0;
    if (n < 0) { put_char('-'); n = -n; }
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    while (--i >= 0) put_char(buf[i]);
}

static int simple_atoi(const char *s) {
    int n = 0;
    while (*s >= '0' && *s <= '9') {
        n = n * 10 + (*s - '0');
        s++;
    }
    return n;
}

static void itoa_buf(int n, char *buf, int bufsz) {
    int i = 0;
    if (n == 0) { buf[0] = '0'; buf[1] = '\0'; return; }
    char tmp[16];
    while (n > 0 && i < 15) {
        tmp[i++] = '0' + (n % 10);
        n /= 10;
    }
    int j = 0;
    while (--i >= 0 && j < bufsz - 1) {
        buf[j++] = tmp[i];
    }
    buf[j] = '\0';
}

/* Magic patterns */
#define PARENT_MAGIC 0xDEADBEEF
#define CHILD_MAGIC  0xCAFEBABE

/* -- child mode --------------------------------------------------------- */

static int child_main(int fd) {
    /* mmap the inherited shmem fd */
    void *ptr = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (ptr == MAP_FAILED) {
        puts_stdout("shmem_test: child: mmap failed\n");
        return 1;
    }

    puts_stdout("shmem_test: child: mmap'd fd ");
    put_dec(fd);
    puts_stdout(" at ");
    put_hex((unsigned long)ptr);
    put_char('\n');

    /* Verify parent's pattern */
    unsigned int *data = (unsigned int *)ptr;
    if (data[0] != PARENT_MAGIC) {
        puts_stdout("shmem_test: child: WRONG pattern: ");
        put_hex(data[0]);
        puts_stdout(" (expected ");
        put_hex(PARENT_MAGIC);
        puts_stdout(")\n");
        munmap(ptr, 4096);
        return 1;
    }
    puts_stdout("shmem_test: child: pattern OK\n");

    /* Write response at offset 4 */
    data[1] = CHILD_MAGIC;
    puts_stdout("shmem_test: child: wrote response\n");

    munmap(ptr, 4096);
    return 0;
}

/* -- parent mode -------------------------------------------------------- */

int main(int argc, char **argv) {
    extern char **environ;

    /* Child mode: argv[1] is the shmem fd number */
    if (argc > 1) {
        int fd = simple_atoi(argv[1]);
        _exit(child_main(fd));
        return 1; /* unreachable */
    }

    /* -- Parent mode -- */

    /* Step 1: create shmem */
    long fd = shmem_create(4096, 0);
    if (fd < 0) {
        puts_stdout("shmem_test: shmem_create failed: ");
        put_dec(fd);
        put_char('\n');
        _exit(1);
    }

    puts_stdout("shmem_test: parent: shmem fd=");
    put_dec(fd);
    put_char('\n');

    /* Step 2: mmap it */
    void *ptr = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, (int)fd, 0);
    if (ptr == MAP_FAILED) {
        puts_stdout("shmem_test: parent: mmap failed\n");
        _exit(1);
    }

    puts_stdout("shmem_test: parent: mmap'd at ");
    put_hex((unsigned long)ptr);
    put_char('\n');

    /* Step 3: write parent magic */
    unsigned int *data = (unsigned int *)ptr;
    data[0] = PARENT_MAGIC;
    data[1] = 0; /* clear response slot */

    /* Step 4: spawn child with fd number as argument */
    char fd_str[16];
    itoa_buf((int)fd, fd_str, sizeof(fd_str));

    puts_stdout("shmem_test: parent: spawning child with fd ");
    puts_stdout(fd_str);
    put_char('\n');

    /* Find our own path. */
    const char *self_path = "/bin/shmem_test";

    char *child_argv[] = { (char *)self_path, fd_str, (char *)0 };
    pid_t child_pid;
    int err = posix_spawn(&child_pid, self_path, 0, 0, child_argv, environ);
    if (err != 0) {
        puts_stdout("shmem_test: parent: spawn failed\n");
        _exit(1);
    }

    /* Step 5: wait for child */
    int status = 0;
    waitpid(child_pid, &status, 0);
    puts_stdout("shmem_test: parent: child exited\n");

    /* Step 6: verify child's response */
    if (data[1] != CHILD_MAGIC) {
        puts_stdout("shmem_test: parent: WRONG response: ");
        put_hex(data[1]);
        puts_stdout(" (expected ");
        put_hex(CHILD_MAGIC);
        puts_stdout(")\n");
        munmap(ptr, 4096);
        _exit(1);
    }
    puts_stdout("shmem_test: parent: response OK\n");

    munmap(ptr, 4096);
    close((int)fd);

    puts_stdout("shmem_test: all tests passed\n");
    _exit(0);
    return 0;
}
