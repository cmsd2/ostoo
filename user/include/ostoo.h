/*
 * ostoo.h — Userspace library for ostoo custom syscalls.
 *
 * Provides struct definitions, constants, syscall wrappers, and helpers
 * for the ostoo completion port, IPC, shared memory, and notification APIs.
 *
 * All struct layouts must match the kernel's repr(C) definitions in:
 *   libkernel/src/completion_port.rs  (IoSubmission, IoCompletion, RingHeader)
 *   libkernel/src/channel.rs          (IpcMessage)
 *   osl/src/io_port.rs               (IoRingParams)
 */

#ifndef OSTOO_H
#define OSTOO_H

#include <unistd.h>
#include <sys/syscall.h>

/* ═══════════════════════════════════════════════════════════════════════
 * Syscall numbers (must match osl/src/syscall_nr.rs)
 * ═══════════════════════════════════════════════════════════════════════ */

#define SYS_IO_CREATE       501
#define SYS_IO_SUBMIT       502
#define SYS_IO_WAIT         503
#define SYS_IRQ_CREATE      504
#define SYS_IPC_CREATE      505
#define SYS_IPC_SEND        506
#define SYS_IPC_RECV        507
#define SYS_SHMEM_CREATE    508
#define SYS_NOTIFY_CREATE   509
#define SYS_NOTIFY          510
#define SYS_IO_SETUP_RINGS  511
#define SYS_IO_RING_ENTER   512
#define SYS_SVC_REGISTER    513
#define SYS_SVC_LOOKUP      514
#define SYS_FRAMEBUFFER_OPEN 515

/* ═══════════════════════════════════════════════════════════════════════
 * Completion port opcodes (must match libkernel/src/completion_port.rs)
 * ═══════════════════════════════════════════════════════════════════════ */

#define OP_NOP       0
#define OP_TIMEOUT   1
#define OP_READ      2
#define OP_WRITE     3
#define OP_IRQ_WAIT  4
#define OP_IPC_SEND  5
#define OP_IPC_RECV  6
#define OP_RING_WAIT 7

/* ═══════════════════════════════════════════════════════════════════════
 * Flags
 * ═══════════════════════════════════════════════════════════════════════ */

#define IPC_NONBLOCK    0x1
#define IPC_CLOEXEC     0x1
#define SHM_CLOEXEC     0x01
#define NOTIFY_CLOEXEC  0x01

/* ═══════════════════════════════════════════════════════════════════════
 * Ring layout constants
 * ═══════════════════════════════════════════════════════════════════════ */

#define RING_ENTRIES_OFFSET 64

/* ═══════════════════════════════════════════════════════════════════════
 * Struct definitions — must match kernel repr(C) layouts exactly
 * ═══════════════════════════════════════════════════════════════════════ */

/* IoSubmission: 48 bytes (libkernel::completion_port::IoSubmission) */
struct io_submission {
    unsigned long user_data;     /* 8 */
    unsigned int  opcode;        /* 4 */
    unsigned int  flags;         /* 4 */
    int           fd;            /* 4 */
    int           _pad;          /* 4 */
    unsigned long buf_addr;      /* 8 */
    unsigned int  buf_len;       /* 4 */
    unsigned int  offset;        /* 4 */
    unsigned long timeout_ns;    /* 8 */
};

/* IoCompletion: 24 bytes (libkernel::completion_port::IoCompletion) */
struct io_completion {
    unsigned long user_data;     /* 8 */
    long          result;        /* 8 */
    unsigned int  flags;         /* 4 */
    unsigned int  opcode;        /* 4 */
};

/* IpcMessage: 48 bytes (libkernel::channel::IpcMessage) */
struct ipc_message {
    unsigned long tag;           /* 8 */
    unsigned long data[3];       /* 24 */
    int           fds[4];        /* 16 */
};

/* RingHeader: 16 bytes (libkernel::completion_port::RingHeader) */
struct ring_header {
    unsigned int head;           /* 4 — atomic, consumer advances */
    unsigned int tail;           /* 4 — atomic, producer advances */
    unsigned int mask;           /* 4 */
    unsigned int flags;          /* 4 */
};

/* IoRingParams: 16 bytes (osl::io_port::IoRingParams) */
struct io_ring_params {
    unsigned int sq_entries;     /* IN/OUT */
    unsigned int cq_entries;     /* IN/OUT */
    int          sq_fd;          /* OUT */
    int          cq_fd;          /* OUT */
};

/* ABI safety checks */
_Static_assert(sizeof(struct io_submission) == 48, "io_submission size mismatch");
_Static_assert(sizeof(struct io_completion) == 24, "io_completion size mismatch");
_Static_assert(sizeof(struct ipc_message)   == 48, "ipc_message size mismatch");
_Static_assert(sizeof(struct ring_header)   == 16, "ring_header size mismatch");
_Static_assert(sizeof(struct io_ring_params)== 16, "io_ring_params size mismatch");

/* ═══════════════════════════════════════════════════════════════════════
 * Syscall wrappers
 * ═══════════════════════════════════════════════════════════════════════ */

long io_create(unsigned int flags);
long io_submit(int port_fd, const struct io_submission *entries,
               unsigned int count);
long io_wait(int port_fd, struct io_completion *completions,
             unsigned int max, unsigned int min, unsigned long timeout_ns);
long irq_create(unsigned int gsi);
long ipc_create(int fds[2], unsigned int capacity, unsigned int flags);
long ipc_send(int fd, const struct ipc_message *msg, unsigned int flags);
long ipc_recv(int fd, struct ipc_message *msg, unsigned int flags);
long shmem_create(unsigned long size, unsigned int flags);
long notify_create(unsigned int flags);
long notify_signal(int fd);
long io_setup_rings(int port_fd, struct io_ring_params *params);
long io_ring_enter(int port_fd, unsigned int to_submit,
                   unsigned int min_complete, unsigned int flags);
long svc_register(const char *name, int fd);
long svc_lookup(const char *name);
long framebuffer_open(unsigned int flags);

/* ═══════════════════════════════════════════════════════════════════════
 * Output helpers
 * ═══════════════════════════════════════════════════════════════════════ */

void puts_fd(int fd, const char *s);
void puts_stdout(const char *s);
void put_char(char c);
void put_num(long n);
void put_hex(unsigned long n);
void put_dec(long n);

/* ═══════════════════════════════════════════════════════════════════════
 * Conversion helpers
 * ═══════════════════════════════════════════════════════════════════════ */

void itoa_buf(int val, char *buf, int bufsz);
int  simple_atoi(const char *s);

/* ═══════════════════════════════════════════════════════════════════════
 * Ring buffer access helpers
 * ═══════════════════════════════════════════════════════════════════════ */

struct io_submission *sq_entry(void *sq_base, unsigned int index,
                               unsigned int mask);
struct io_completion *cq_entry(void *cq_base, unsigned int index,
                               unsigned int mask);

/*
 * Higher-level ring operations — handle atomic ordering internally
 * so callers never need to use __atomic_* builtins.
 */

/* Push a submission to the SQ ring.  Returns 0 on success, -1 if full. */
int sq_push(void *sq_base, const struct io_submission *sqe);

/* Pop a completion from the CQ ring.  Returns 0 on success, -1 if empty. */
int cq_pop(void *cq_base, struct io_completion *cqe_out);

/* Number of completions available in the CQ ring. */
unsigned int cq_ready(void *cq_base);

#endif /* OSTOO_H */
