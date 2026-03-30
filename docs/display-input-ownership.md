# Display & Input Ownership

How the framebuffer and keyboard transition from kernel to compositor
using the existing fd-passing and service-registry primitives.

## Problem

At boot, three components compete for the display and keyboard:

1. **Kernel WRITER** — `println!()` renders to the BGA framebuffer via an
   IrqMutex-protected `Framebuffer` struct.
2. **User shell** — reads from the console input buffer, writes to stdout
   (which goes through WRITER).
3. **Compositor** — mmaps the same LFB via `framebuffer_open`, composites
   client windows.

Today there is no ownership model. The kernel WRITER and compositor both
hold pointers to the same physical framebuffer memory and write
concurrently. Keyboard input routes to the user shell via
`FOREGROUND_PID` but the compositor has no way to receive it.

## Design: Capability-Based Handoff

Ownership is expressed through who holds which fds, matching the existing
IPC model.

### Display Ownership

```
BOOT                            COMPOSITOR RUNNING
────                            ──────────────────
WRITER ──▶ LFB (active)        WRITER ──▶ serial only
                                Compositor ──▶ LFB (exclusive)
```

When the compositor calls `framebuffer_open` (syscall 515), two things
happen:

1. The compositor gets an shmem fd wrapping the BGA LFB (existing
   behaviour).
2. **Side effect:** the kernel marks the WRITER backend as
   *suppressed*. All subsequent `println!()` / `log::info!()` output is
   redirected to serial only. The kernel no longer touches the LFB.
   The status bar and timeline strip are also suppressed.

If the compositor exits or crashes, the kernel detects this (via process
exit cleanup in `terminate_process`) and *unsuppresses* the WRITER,
calling `repaint_all()` to restore kernel display output.

Implementation: `DISPLAY_SUPPRESSED: AtomicBool` and `DISPLAY_OWNER_PID:
AtomicU64` in `libkernel/src/vga_buffer/mod.rs`.

### Input Ownership — Userspace Keyboard Driver

Instead of a kernel-level `input_acquire` syscall, the keyboard becomes
a **userspace service** (`/bin/kbd`). This requires no new kernel
interfaces — only existing primitives.

```
┌──────────┐  IRQ fd    ┌──────────┐  IPC channel  ┌────────────┐
│ IO APIC  │───────────▶│ /bin/kbd │──────────────▶│ Compositor │
│ (GSI 1)  │  scancode   │          │  key events    │            │
└──────────┘  in result  └──────────┘               └────────────┘
```

How it works:

1. `/bin/kbd` calls `irq_create(1)` — claims keyboard IRQ via the
   existing IRQ fd mechanism. This reroutes the keyboard interrupt from
   the hardwired kernel ISR (vector 33) to a dynamic vector handled by
   `irq_fd_dispatch`, which reads port 0x60 and delivers the scancode
   in `completion.result`. The GSI is kept unmasked between interrupts
   so that edge-triggered IRQ edges are never lost; scancodes that
   arrive between OP_IRQ_WAIT re-arms are buffered in a 16-entry ring.
2. Creates a registration channel and calls `svc_register("keyboard")`.
3. Event loop on CompletionPort:
   - `OP_IRQ_WAIT` → receives scancode → decodes via scancode set 1
     state machine → produces key events
   - `OP_IPC_RECV` on registration channel → new client connecting
     (compositor sends a channel send-end) → stores client
4. For each decoded key event, sends an `IpcMessage` to all connected
   clients.

When `/bin/kbd` exits, `close_irq` restores the original IO APIC entry,
and the kernel keyboard actor resumes automatically — providing fallback.

Safety: if no client connects within 2 seconds, kbd exits to avoid
capturing keyboard input with nobody listening.

### Keyboard Protocol

```
MSG_KB_CONNECT (tag=1): client → keyboard service
  data = [0, 0, 0]
  fds  = [event_send_fd, -1, -1, -1]

MSG_KB_KEY (tag=1): keyboard service → client (via passed channel)
  data = [byte, modifiers, key_type]
  fds  = [-1, -1, -1, -1]
```

- `key_type`: 0 = ASCII byte, 1 = special key (arrow, etc.)
- `modifiers`: bitmask (bit 0 = shift, bit 1 = ctrl, bit 2 = alt)

### Input Ownership — Userspace Mouse Driver

The mouse uses the same architecture as the keyboard: a userspace
service (`/bin/mouse`) claiming the PS/2 mouse IRQ.

```
┌──────────┐  IRQ fd    ┌──────────┐  IPC channel  ┌────────────┐
│ IO APIC  │───────────▶│ /bin/mouse│──────────────▶│ Compositor │
│ (GSI 12) │  byte       │          │  mouse events  │            │
└──────────┘  in result  └──────────┘               └────────────┘
```

How it works:

1. `/bin/mouse` calls `irq_create(12)` — claims mouse IRQ via the
   existing IRQ fd mechanism. The kernel automatically initializes
   the PS/2 auxiliary port (i8042 controller) when GSI 12 is claimed,
   enabling mouse data reporting and IRQ 12 delivery.
2. Creates a registration channel and calls `svc_register("mouse")`.
3. Event loop: collects 3-byte PS/2 packets (sync on byte 0 bit 3),
   decodes deltas and buttons, tracks absolute cursor position
   (clamped to screen bounds), broadcasts to connected clients.

### Mouse Protocol

```
MSG_MOUSE_CONNECT (tag=1): client → mouse service
  data = [0, 0, 0]
  fds  = [event_send_fd, -1, -1, -1]

MSG_MOUSE_MOVE (tag=1): mouse service → client (via passed channel)
  data = [x, y, buttons]
  fds  = [-1, -1, -1, -1]
```

- `buttons`: bitmask (bit 0 = left, bit 1 = right, bit 2 = middle)
- `x`, `y`: absolute screen coordinates

### Compositor Key & Mouse Forwarding

The compositor connects to both keyboard and mouse services on startup
using `svc_lookup_retry()`. Key events are forwarded to the focused
window's client via `MSG_KEY_EVENT` (tag 5). Mouse events drive the
cursor, focus, window movement, and resizing.

### Window Decorations (Server-Side)

The compositor draws server-side decorations around each client window:

```
┌─────────────────────────────┐ ─┐
│ [X] Win 1                   │  │ TITLE_H = 20px
├─────────────────────────────┤ ─┘
│                             │
│       Client Content        │  client buffer (w × h)
│                             │
└─────────────────────────────┘
  BORDER_W = 2px on all sides
```

- Title bar: highlighted when focused, dimmed when unfocused
- Close button: `[X]` in top-right of title bar
- Window title: "Win N" rendered with 8×16 CP437 font

### Window Management

**Focus**: Click anywhere in a window to focus it. The focused window
moves to the top of the Z-order and receives keyboard input.

**Move**: Drag the title bar to move a window.

**Resize**: Drag the bottom edge, right edge, or bottom-right corner
to resize. On mouse-up, the compositor allocates a new shared buffer
and sends `MSG_WINDOW_RESIZED` to the client. The terminal emulator
recalculates cols/rows and clears the screen.

**Close**: Click the `[X]` button to close a window.

### Compositor Double Buffering

The compositor uses an offscreen back buffer (heap-allocated, same
size as the framebuffer) to eliminate flicker. Each composite pass
clears and draws windows into the back buffer, then copies the
finished frame to the LFB in a single `memcpy`.

### Terminal Emulator and Shell

The terminal emulator (`/bin/term`) is a compositor client that spawns
the shell with pipe-connected stdin/stdout.

```
Compositor                      Terminal Emulator              Shell
──────────                      ─────────────────              ─────
         MSG_KEY_EVENT           stdin pipe
  ─────────────────▶  translate  ──────────────▶  read(0)
         s2c channel             stdout pipe
  ◀─────────────────  render    ◀──────────────  write(1)
         damage notify           (pipe pair)
  ◀─────────────────
```

The terminal emulator:
- Connects to compositor via `svc_lookup_retry("compositor")`
- Gets a 640×384 window (80×24 cells at 8×16 font)
- Creates pipe pairs for shell stdin/stdout
- Spawns `/bin/shell` via `clone(CLONE_VM|CLONE_VFORK)` + `execve`
  (`child_stack=0` shares the parent's stack — safe because the parent
  is blocked until the child calls `execve` or `_exit`)
- Event loop: key events → shell stdin pipe, shell stdout → VT100
  parser → glyph rendering → damage signal

#### Command interpreter (shell)

A plain stdin/stdout program with no knowledge of the compositor:
- Reads lines from stdin, writes output to stdout
- Works identically in both graphical and fallback modes

### Fallback Path

If the compositor binary is not present or fails to start:

- `framebuffer_open` is never called → WRITER stays active
- Keyboard IRQ stays with kernel actor → routes to console buffer
- User shell reads/writes via console as it does today

The system degrades gracefully to the current behaviour.

## Startup Sequence

```
Boot
 ├─ launch_keyboard_driver() [100ms VFS settle]
 │   └─ /bin/kbd: irq_create(1), svc_register("keyboard")
 │      keyboard IRQ rerouted → kernel actor dormant
 │
 ├─ launch_mouse_driver() [100ms VFS settle]
 │   └─ /bin/mouse: irq_create(12) → PS/2 aux init, svc_register("mouse")
 │      mouse IRQ 12 claimed → 3-byte packet decoding
 │
 ├─ launch_compositor() [100ms VFS settle]
 │   ├─ /bin/compositor: framebuffer_open → WRITER suppressed
 │   ├─ svc_lookup_retry("keyboard") → receive key events
 │   ├─ svc_lookup_retry("mouse") → receive mouse events
 │   ├─ svc_register("compositor")
 │   └─ kernel spawns /bin/term
 │       ├─ svc_lookup_retry("compositor") → get window
 │       ├─ pipe2 + clone/execve → spawn /bin/shell
 │       └─ event loop (keys → shell stdin, shell stdout → render)
 │
 └─ launch_userspace_shell() [polls DISPLAY_SUPPRESSED every 50ms, up to 1s]
     └─ if DISPLAY_SUPPRESSED → skip (compositor path active)
        else → launch /bin/shell directly (fallback)
```

Service readiness is coordinated via polling and retry loops rather
than hardcoded sleep timings:
- Userspace: `svc_lookup_retry()` retries service lookup with 50ms yields
- Kernel: `launch_userspace_shell` polls `DISPLAY_SUPPRESSED` every 50ms
  (up to 20 iterations / 1 second) before falling back to standalone shell

## Fallback Matrix

| kbd | mouse | compositor | term | Result |
|-----|-------|-----------|------|--------|
| yes | yes | yes | yes | Full graphical: kbd+mouse→compositor→term→shell |
| yes | no | yes | yes | Graphical, keyboard only (no cursor/mouse) |
| yes | yes | yes | no | Compositor up, no terminal (display-only) |
| no | no | no | - | Classic fallback: kernel kbd actor + shell on console |

## No New Syscalls

This design uses only existing kernel primitives:

| Primitive | Syscall | Use |
|-----------|---------|-----|
| `irq_create` | 504 | keyboard driver claims IRQ 1, mouse driver claims IRQ 12 |
| `svc_register` / `svc_lookup` | 513/514 | keyboard, mouse, and compositor service discovery |
| `ipc_create` / `ipc_send` / `ipc_recv` | 505-507 | key/mouse event delivery with fd passing |
| `shmem_create` | 508 | window buffers, resize buffer allocation |
| `framebuffer_open` | 515 | display ownership (with suppression side effect) |
| `pipe2` | 293 | terminal↔shell communication |
| `clone` / `execve` / `dup2` | 56/59/33 | terminal spawns shell |

The only kernel changes beyond the initial suppression flag are:
- PS/2 auxiliary port initialization on `irq_create(12)` (`libkernel/src/ps2.rs`)
- `irq_fd_dispatch` reads port 0x60 for GSI 12 (mouse) in addition to GSI 1 (keyboard)
