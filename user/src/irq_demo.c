/*
 * irq_demo — OP_IRQ_WAIT smoke test for IRQ fd + completion port.
 *
 * Creates an IRQ fd for the keyboard (GSI 1), submits OP_IRQ_WAIT,
 * waits for a keypress, and prints the completion (including scancode
 * in result).  Repeats 5 times, then exits.
 *
 * Expected output:
 *   irq_demo: port fd = N
 *   irq_demo: irq fd = M
 *   Press a key...
 *   irq_demo: got IRQ! opcode=IRQ_WAIT user_data=100 result=XX (scancode)
 *   Press a key...
 *   ...
 *   irq_demo: done (5 IRQs received)
 */

#include <string.h>
#include "ostoo.h"

/* ── main ────────────────────────────────────────────────────────────── */

int main(void) {
    puts_stdout("irq_demo: starting\n");

    /* Create completion port */
    long port_fd = io_create(0);
    if (port_fd < 0) {
        puts_stdout("irq_demo: io_create failed: ");
        put_num(port_fd);
        put_char('\n');
        _exit(1);
    }
    puts_stdout("irq_demo: port fd = ");
    put_num(port_fd);
    put_char('\n');

    /* Create IRQ fd for keyboard (GSI 1) */
    long irq_fd = irq_create(1);
    if (irq_fd < 0) {
        puts_stdout("irq_demo: irq_create failed: ");
        put_num(irq_fd);
        put_char('\n');
        _exit(1);
    }
    puts_stdout("irq_demo: irq fd = ");
    put_num(irq_fd);
    put_char('\n');

    /* Receive 5 keyboard interrupts */
    int count = 5;
    for (int i = 0; i < count; i++) {
        puts_stdout("Press a key...\n");

        /* Submit OP_IRQ_WAIT */
        struct io_submission sub;
        memset(&sub, 0, sizeof(sub));
        sub.user_data = 100 + i;
        sub.opcode = OP_IRQ_WAIT;
        sub.fd = (int)irq_fd;

        long ret = io_submit((int)port_fd, &sub, 1);
        if (ret < 0) {
            puts_stdout("irq_demo: io_submit failed: ");
            put_num(ret);
            put_char('\n');
            _exit(1);
        }

        /* Wait for the IRQ completion (10s timeout) */
        struct io_completion comp;
        long got = io_wait((int)port_fd, &comp, 1, 1, 10000000000UL);
        if (got < 0) {
            puts_stdout("irq_demo: io_wait failed: ");
            put_num(got);
            put_char('\n');
            _exit(1);
        }
        if (got == 0) {
            puts_stdout("irq_demo: timeout waiting for IRQ\n");
            continue;
        }

        puts_stdout("  got IRQ! opcode=");
        if (comp.opcode == OP_IRQ_WAIT) {
            puts_stdout("IRQ_WAIT");
        } else {
            put_num(comp.opcode);
        }
        puts_stdout(" user_data=");
        put_num((long)comp.user_data);
        puts_stdout(" result=");
        put_num(comp.result);
        puts_stdout(" (scancode ");
        put_hex((unsigned long)comp.result);
        puts_stdout(")\n");
    }

    close((int)irq_fd);
    close((int)port_fd);

    puts_stdout("irq_demo: done (");
    put_num(count);
    puts_stdout(" IRQs received)\n");

    _exit(0);
    return 0;
}
