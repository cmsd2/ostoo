# irq_create (nr 504)

Create a file descriptor for receiving hardware interrupts.

## Signature

```
irq_create(gsi: u32) → fd or -errno
```

## Arguments

| Arg | Register | Description |
|-----|----------|-------------|
| gsi | rdi | Global System Interrupt number |

## Return value

On success, returns a file descriptor for the IRQ object.

## Errors

| Error | Condition |
|-------|-----------|
| ENOMEM | No free dynamic interrupt vectors available |
| EINVAL | Failed to program the IO APIC for the given GSI |
| EMFILE | Process fd table is full |

## Description

Allocates a dynamic interrupt vector, programs the IO APIC to route the
specified GSI to that vector (edge-triggered, active-high, initially
masked), and returns an fd referring to the IRQ object.

The IRQ fd is used with `io_submit` `OP_IRQ_WAIT` to asynchronously wait
for interrupts via a completion port.  When an interrupt fires, the
registered completion port receives a completion with the user_data from the
submission.  The GSI is then re-masked until the next `OP_IRQ_WAIT` is
submitted.

When the fd is closed, the original IO APIC redirection entry is restored
and the dynamic vector is freed.

## Implementation

`osl/src/irq.rs` — `sys_irq_create`

## See also

- [io_submit (502)](io_submit.md) — `OP_IRQ_WAIT` opcode
- [Completion Port Design](../completion-port-design.md)
