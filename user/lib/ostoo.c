/*
 * libostoo — Userspace library for ostoo custom syscalls.
 *
 * Built as a static library (libostoo.a) and linked into all demo programs.
 */

#include "ostoo.h"
#include <string.h>

/* ═══════════════════════════════════════════════════════════════════════
 * Syscall wrappers
 * ═══════════════════════════════════════════════════════════════════════ */

long io_create(unsigned int flags) {
    return syscall(SYS_IO_CREATE, flags);
}

long io_submit(int port_fd, const struct io_submission *entries,
               unsigned int count) {
    return syscall(SYS_IO_SUBMIT, port_fd, entries, count);
}

long io_wait(int port_fd, struct io_completion *completions,
             unsigned int max, unsigned int min, unsigned long timeout_ns) {
    return syscall(SYS_IO_WAIT, port_fd, completions, max, min, timeout_ns);
}

long irq_create(unsigned int gsi) {
    return syscall(SYS_IRQ_CREATE, gsi);
}

long ipc_create(int fds[2], unsigned int capacity, unsigned int flags) {
    return syscall(SYS_IPC_CREATE, fds, capacity, flags);
}

long ipc_send(int fd, const struct ipc_message *msg, unsigned int flags) {
    return syscall(SYS_IPC_SEND, fd, msg, flags);
}

long ipc_recv(int fd, struct ipc_message *msg, unsigned int flags) {
    return syscall(SYS_IPC_RECV, fd, msg, flags);
}

long shmem_create(unsigned long size, unsigned int flags) {
    return syscall(SYS_SHMEM_CREATE, size, flags);
}

long notify_create(unsigned int flags) {
    return syscall(SYS_NOTIFY_CREATE, flags);
}

long notify_signal(int fd) {
    return syscall(SYS_NOTIFY, fd);
}

long io_setup_rings(int port_fd, struct io_ring_params *params) {
    return syscall(SYS_IO_SETUP_RINGS, port_fd, params);
}

long io_ring_enter(int port_fd, unsigned int to_submit,
                   unsigned int min_complete, unsigned int flags) {
    return syscall(SYS_IO_RING_ENTER, port_fd, to_submit, min_complete, flags);
}

long svc_register(const char *name, int fd) {
    return syscall(SYS_SVC_REGISTER, name, fd);
}

long svc_lookup(const char *name) {
    return syscall(SYS_SVC_LOOKUP, name);
}

long framebuffer_open(unsigned int flags) {
    return syscall(SYS_FRAMEBUFFER_OPEN, flags);
}

/* ═══════════════════════════════════════════════════════════════════════
 * Output helpers
 * ═══════════════════════════════════════════════════════════════════════ */

void puts_fd(int fd, const char *s) {
    write(fd, s, strlen(s));
}

void puts_stdout(const char *s) {
    puts_fd(1, s);
}

void put_char(char c) {
    write(1, &c, 1);
}

void put_num(long n) {
    char buf[21];
    int i = 0;
    int neg = 0;
    if (n < 0) { neg = 1; n = -n; }
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    if (neg) put_char('-');
    while (--i >= 0) put_char(buf[i]);
}

void put_hex(unsigned long n) {
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

void put_dec(long n) {
    put_num(n);
}

/* ═══════════════════════════════════════════════════════════════════════
 * Conversion helpers
 * ═══════════════════════════════════════════════════════════════════════ */

void itoa_buf(int val, char *buf, int bufsz) {
    int i = 0;
    int neg = 0;
    if (val < 0) { neg = 1; val = -val; }
    char tmp[20];
    if (val == 0) { tmp[i++] = '0'; }
    while (val > 0 && i < 18) {
        tmp[i++] = '0' + (val % 10);
        val /= 10;
    }
    int pos = 0;
    if (neg && pos < bufsz - 1) buf[pos++] = '-';
    while (--i >= 0 && pos < bufsz - 1) buf[pos++] = tmp[i];
    buf[pos] = '\0';
}

int simple_atoi(const char *s) {
    int val = 0;
    int neg = 0;
    if (*s == '-') { neg = 1; s++; }
    while (*s >= '0' && *s <= '9') {
        val = val * 10 + (*s - '0');
        s++;
    }
    return neg ? -val : val;
}

/* ═══════════════════════════════════════════════════════════════════════
 * Ring buffer access helpers
 * ═══════════════════════════════════════════════════════════════════════ */

struct io_submission *sq_entry(void *sq_base, unsigned int index,
                               unsigned int mask) {
    unsigned int slot = index & mask;
    return (struct io_submission *)((char *)sq_base + RING_ENTRIES_OFFSET
                                    + slot * sizeof(struct io_submission));
}

struct io_completion *cq_entry(void *cq_base, unsigned int index,
                               unsigned int mask) {
    unsigned int slot = index & mask;
    return (struct io_completion *)((char *)cq_base + RING_ENTRIES_OFFSET
                                    + slot * sizeof(struct io_completion));
}

/* ═══════════════════════════════════════════════════════════════════════
 * Higher-level ring operations
 * ═══════════════════════════════════════════════════════════════════════ */

int sq_push(void *sq_base, const struct io_submission *sqe) {
    struct ring_header *hdr = (struct ring_header *)sq_base;
    unsigned int tail = __atomic_load_n(&hdr->tail, __ATOMIC_RELAXED);
    unsigned int head = __atomic_load_n(&hdr->head, __ATOMIC_ACQUIRE);
    if (tail - head > hdr->mask) {
        return -1;  /* full */
    }
    struct io_submission *dst = sq_entry(sq_base, tail, hdr->mask);
    *dst = *sqe;
    __atomic_store_n(&hdr->tail, tail + 1, __ATOMIC_RELEASE);
    return 0;
}

int cq_pop(void *cq_base, struct io_completion *cqe_out) {
    struct ring_header *hdr = (struct ring_header *)cq_base;
    unsigned int head = __atomic_load_n(&hdr->head, __ATOMIC_RELAXED);
    unsigned int tail = __atomic_load_n(&hdr->tail, __ATOMIC_ACQUIRE);
    if (head == tail) {
        return -1;  /* empty */
    }
    struct io_completion *src = cq_entry(cq_base, head, hdr->mask);
    *cqe_out = *src;
    __atomic_store_n(&hdr->head, head + 1, __ATOMIC_RELEASE);
    return 0;
}

unsigned int cq_ready(void *cq_base) {
    struct ring_header *hdr = (struct ring_header *)cq_base;
    unsigned int head = __atomic_load_n(&hdr->head, __ATOMIC_RELAXED);
    unsigned int tail = __atomic_load_n(&hdr->tail, __ATOMIC_ACQUIRE);
    return tail - head;
}
