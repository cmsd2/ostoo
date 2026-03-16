/* High-half kernel linker script.
 *
 * Links the kernel at PML4 entry 510 (0xFFFF_FF00_0000_0000).
 * The bootloader maps each LOAD segment at its p_vaddr (high-half virtual)
 * backed by physical = kernel_start(0x400000) + p_offset (file offset),
 * so no AT() override is needed.
 *
 * Entry 510 is within the 256–510 range copied verbatim into every
 * per-process PML4 by create_user_page_table, making SYSCALL and
 * interrupt handlers reachable after a CR3 switch to an isolated PML4.
 */

OUTPUT_FORMAT("elf64-x86-64")
ENTRY(_start)

KERNEL_BASE = 0xFFFFFF0000000000;

SECTIONS
{
    . = KERNEL_BASE + 0x100000;

    .text : {
        *(.text .text.*)
    }

    . = ALIGN(0x1000);

    .rodata : {
        *(.rodata .rodata.*)
        *(.data.rel.ro .data.rel.ro.*)
    }

    . = ALIGN(0x1000);

    .data : {
        *(.data .data.*)
        *(.got .got.*)
        *(.got.plt)
    }

    . = ALIGN(0x1000);

    .bss (NOLOAD) : {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note*)
        *(.comment*)
        *(.debug*)
    }
}
