# APIC and IO APIC Initialization

## Background

The x86/x86_64 interrupt subsystem has two generations:

- **8259 PIC (Programmable Interrupt Controller)** — the legacy two-chip design.
  Master (IRQs 0–7) and slave (IRQs 8–15) are chained. Vectors are remapped to
  0x20–0x2F to avoid conflicts with CPU exceptions.
- **APIC (Advanced Programmable Interrupt Controller)** — the modern design,
  required for SMP. Consists of a Local APIC (LAPIC) per CPU core and one or
  more IO APICs for external devices.

ACPI describes which model the firmware uses via the MADT (Multiple APIC
Description Table). On QEMU with default settings, the MADT reports
`InterruptModel::Apic`, meaning APIC mode is required.

## Architecture

```
   Device
     │
     ▼
 IO APIC  ──── Redirection Table ───► Local APIC ──► CPU
(external                              (per core)
 IRQs)                                  LAPIC ID
```

### Local APIC (LAPIC)

- One per CPU core, memory-mapped at physical address `0xFEE00000` by default.
- Handles inter-processor interrupts (IPIs) and LAPIC-local sources (timer,
  thermal, etc.).
- Must be **enabled** by writing to the Spurious Interrupt Vector Register (SIVR)
  at offset `0xF0`. Setting bit 8 (`APIC_ENABLE`) activates the LAPIC. Bits 0–7
  set the spurious interrupt vector (conventionally `0xFF`).
- **EOI** (End of Interrupt) is signalled by writing `0` to the EOI register at
  offset `0xB0`. Unlike the PIC, no interrupt number is needed — the write itself
  is the acknowledgement.

### IO APIC

- Handles external hardware interrupts (ISA IRQs, PCI interrupts).
- Accessed via two MMIO registers: `IOREGSEL` (write selector) and `IOWIN`
  (read/write data window), both at the IO APIC base address.
- Contains a **Redirection Table** with one 64-bit entry per input pin:

  | Bits  | Field              | Notes                                    |
  |-------|--------------------|------------------------------------------|
  | 0–7   | Vector             | IDT vector to deliver                    |
  | 8–10  | Delivery mode      | 0 = fixed                                |
  | 11    | Destination mode   | 0 = physical (LAPIC ID), 1 = logical     |
  | 13    | Pin polarity       | 0 = active high, 1 = active low          |
  | 15    | Trigger mode       | 0 = edge, 1 = level                      |
  | 16    | Mask               | 1 = masked (disabled)                    |
  | 56–63 | Destination        | Physical: target LAPIC ID                |

## ACPI and Interrupt Source Overrides

ISA IRQs are conventionally edge-triggered, active-high. However, some IRQs are
remapped: QEMU reports that ISA IRQ 0 (the PIT timer) is redirected to GSI 2
with edge/active-high signalling. The ACPI `InterruptSourceOverride` table
entries describe these remappings:

| ISA IRQ | Default GSI | Override GSI | Override Polarity | Override Trigger |
|---------|-------------|--------------|-------------------|------------------|
| 0       | 0           | 2 (QEMU)     | Same as bus       | Same as bus      |
| 1       | 1           | —            | —                 | —                |

The `init_io` function reads these overrides from `apic_info.interrupt_source_overrides`
and uses the correct GSI, polarity, and trigger mode when programming each
redirection entry.

## Initialization Sequence

### 1. Map Local APIC (`libkernel::apic::init_local`)

The LAPIC physical address is read from the `IA32_APIC_BASE` MSR. A virtual
page at `APIC_BASE` is mapped to this physical frame (with `NO_CACHE` flag, as
MMIO must not be cached):

```
Physical 0xFEE00000  →  Virtual 0xFFFF_8001_0000_0000
```

After mapping:
- `init()` logs the LAPIC ID, version, and LVT register state.
- `enable()` writes the SIVR: `APIC_ENABLE | 0xFF` (enable + spurious vector).
- The EOI virtual address (`APIC_BASE + 0xB0`) is stored in
  `libkernel::interrupts::LAPIC_EOI_ADDR` so interrupt handlers can issue EOI
  without needing a reference to the `apic` module.

### 2. Map IO APIC(s) (`libkernel::apic::init_io`)

Each IO APIC listed in the ACPI MADT is mapped to consecutive virtual pages
starting at `APIC_BASE + 4KiB`. The `global_system_interrupt_base` field records
which GSIs this IO APIC handles (typically 0 for the first IO APIC).

After mapping all IO APICs:
- **Mask all entries** — every redirection table slot is masked before
  programming, preventing spurious interrupts during setup.
- **Route ISA IRQs** — IRQ 0 (timer) and IRQ 1 (keyboard) are routed to IDT
  vectors `0x20` and `0x21` respectively, targeting the BSP's LAPIC ID. Source
  overrides are applied (e.g. timer GSI 2 on QEMU).

### 3. Update IDT and EOI (`libkernel::interrupts`)

The IDT is extended with a **spurious interrupt handler** at vector `0xFF`.
Spurious LAPIC interrupts must not receive an EOI.

The timer and keyboard handlers are updated to call `send_eoi()` instead of
`PICS.notify_end_of_interrupt()`. `send_eoi()` checks `LAPIC_EOI_ADDR`: if
non-zero (APIC mode), it writes `0` to the LAPIC EOI register; otherwise it
falls back to the PIC path. This allows the same IDT to work in both PIC and
APIC modes.

### 4. Disable the 8259 PIC (`libkernel::interrupts::disable_pic`)

After the IO APIC is programmed, the PIC is disabled by masking all IRQs:

```rust
Port::<u8>::new(0x21).write(0xFF);  // mask master PIC
Port::<u8>::new(0xA1).write(0xFF);  // mask slave PIC
```

This prevents the PIC from delivering interrupts that would arrive at the wrong
vectors or cause double-delivery with the IO APIC.

## Key Constants

| Symbol              | Value      | Description                           |
|---------------------|------------|---------------------------------------|
| `APIC_BASE`         | `0xFFFF_8001_0000_0000` | Virtual base for LAPIC mapping |
| `LAPIC_EOI_OFFSET`  | `0xB0`     | Offset of EOI register in LAPIC       |
| `LAPIC_SIVR_OFFSET` | `0xF0`     | Offset of SIVR in LAPIC               |
| `SPURIOUS_VECTOR`   | `0xFF`     | IDT vector for LAPIC spurious IRQs    |
| `TIMER_VECTOR`      | `0x20`     | IDT vector for timer (ISA IRQ 0)      |
| `KEYBOARD_VECTOR`   | `0x21`     | IDT vector for keyboard (ISA IRQ 1)   |

## Crate Location

The APIC code lives in `libkernel::apic` (module `libkernel/src/apic/`). It was
originally a separate `apic` crate but was merged into `libkernel` so that
`libkernel::irq_handle` can call IO APIC mask/unmask/write functions directly
without duplicating raw MMIO code.

The LAPIC EOI address is communicated via a single `AtomicU64` in
`libkernel::interrupts`: the `apic` module writes the address after mapping the
LAPIC, and interrupt handlers read it to perform EOI.

## References

- Intel SDM Vol. 3A, Chapter 10: Advanced Programmable Interrupt Controller
  (APIC)
- OSDev Wiki: [APIC](https://wiki.osdev.org/APIC),
  [IO APIC](https://wiki.osdev.org/IO_APIC),
  [MADT](https://wiki.osdev.org/MADT)
- ACPI Specification, Section 5.2.12: Multiple APIC Description Table
