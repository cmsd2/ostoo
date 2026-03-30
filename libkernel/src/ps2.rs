//! PS/2 controller helpers — auxiliary (mouse) port initialization.

use x86_64::instructions::port::Port;

const CMD_PORT: u16 = 0x64;
const DATA_PORT: u16 = 0x60;

fn wait_input_ready() {
    unsafe {
        let mut status = Port::<u8>::new(CMD_PORT);
        for _ in 0..100_000 {
            if status.read() & 0x02 == 0 {
                return;
            }
        }
    }
}

fn wait_output_ready() {
    unsafe {
        let mut status = Port::<u8>::new(CMD_PORT);
        for _ in 0..100_000 {
            if status.read() & 0x01 != 0 {
                return;
            }
        }
    }
}

/// Drain any stale bytes from the i8042 output buffer.
fn flush_output() {
    unsafe {
        let mut status = Port::<u8>::new(CMD_PORT);
        let mut data = Port::<u8>::new(DATA_PORT);
        for _ in 0..16 {
            if status.read() & 0x01 == 0 {
                break;
            }
            let _ = data.read();
        }
    }
}

/// Initialize the PS/2 auxiliary (mouse) port so that IRQ 12 fires on mouse movement.
///
/// The sequence is carefully ordered to avoid IRQ 12 firing during init:
/// 1. Enable the auxiliary port
/// 2. Send "enable data reporting" to the mouse and read ACK — all BEFORE
///    enabling the aux interrupt in the command byte
/// 3. Flush any stale bytes
/// 4. THEN enable aux interrupt so IRQ 12 starts firing for real mouse data
pub fn aux_init() {
    unsafe {
        let mut cmd = Port::<u8>::new(CMD_PORT);
        let mut data = Port::<u8>::new(DATA_PORT);

        // Flush any stale data in the output buffer.
        flush_output();

        // 1. Enable auxiliary port.
        wait_input_ready();
        cmd.write(0xA8);

        // 2. Set sample rate to 20/sec (default is 100 — far too fast).
        //    Command 0xF3 followed by rate byte, each via 0xD4 prefix.
        wait_input_ready();
        cmd.write(0xD4); // route next byte to aux device
        wait_input_ready();
        data.write(0xF3); // "Set Sample Rate"
        wait_output_ready();
        let _ack1 = data.read(); // ACK

        wait_input_ready();
        cmd.write(0xD4);
        wait_input_ready();
        data.write(20); // 20 samples/sec
        wait_output_ready();
        let _ack2 = data.read(); // ACK

        // 3. Send "enable data reporting" (0xF4) to the mouse BEFORE enabling
        //    aux interrupts, so the ACK doesn't trigger IRQ 12.
        wait_input_ready();
        cmd.write(0xD4);
        wait_input_ready();
        data.write(0xF4);

        // 4. Read ACK (0xFA) from the mouse.
        wait_output_ready();
        let _ack3 = data.read();

        // 5. Flush any remaining bytes (e.g. mouse ID byte some devices send).
        flush_output();

        // 6. NOW enable aux interrupt in the command byte.
        wait_input_ready();
        cmd.write(0x20);
        wait_output_ready();
        let cb = data.read();

        let new_cb = cb | 0x02; // set bit 1 (auxiliary interrupt enable)
        wait_input_ready();
        cmd.write(0x60);
        wait_input_ready();
        data.write(new_cb);
    }
}
