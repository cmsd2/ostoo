# Compositor Design

A Wayland-style userspace compositor for ostoo.

## Overview

The compositor takes ownership of the BGA framebuffer, accepts client
connections via a service registry, allocates shared-memory pixel buffers
for clients, and composites their output to the screen.

**MVP scope**: window creation, buffer allocation, damage signaling,
compositing. No input routing or window management.

## Architecture

```
┌──────────┐  svc_lookup   ┌────────────┐  framebuffer_open  ┌─────┐
│  Client   │──────────────▶│ Compositor │──────────────────▶│ BGA │
│           │  IPC channels │            │  MAP_SHARED mmap   │ LFB │
│  shmem    │◀─────────────▶│  shmem     │                    └─────┘
│  buffer   │  notify fds   │  event     │
└──────────┘               │  loop      │
                            └────────────┘
```

## Kernel Primitives Used

| Syscall | Nr | Purpose |
|---|---|---|
| `svc_register` | 513 | Compositor registers itself under `"compositor"` |
| `svc_lookup` | 514 | Client finds the compositor's registration channel |
| `framebuffer_open` | 515 | Compositor gets an shmem fd wrapping the BGA LFB |
| `ipc_create` | 505 | Channel pairs for registration and per-client comms |
| `ipc_send`/`ipc_recv` | 506/507 | Message passing with fd-passing |
| `shmem_create` | 508 | Per-window pixel buffer allocation |
| `notify_create` | 509 | Per-window damage notification fd |
| `notify` | 510 | Client signals "buffer is ready" |
| `io_create` | 501 | Compositor's completion port |
| `io_submit` | 502 | Arm OP_IPC_RECV and OP_RING_WAIT |
| `io_wait` | 503 | Block until events arrive |

## Service Registry (syscalls 513–514)

A kernel-global `BTreeMap<String, FdObject>` keyed by null-terminated name.

- `svc_register(name, fd)`: clones the fd object + `notify_dup()`, inserts
  under name. Returns `-EBUSY` if taken.
- `svc_lookup(name)`: clones + `notify_dup()`s the stored object, allocates
  fd in caller's table. Returns `-ENOENT` if not found.

Max name length: 128 bytes.

## Framebuffer Access (syscall 515)

`framebuffer_open(flags)` creates a `SharedMemInner::from_existing()` wrapping
the BGA LFB physical frames. The frames are non-owning (MMIO memory is never
freed). The caller mmaps with `MAP_SHARED` to get a user-accessible pointer.

The LFB physical address and size are stored in atomics during BGA init and
read by the syscall handler.

## Connection Protocol

Uses existing IPC fd-passing — no new kernel primitives needed.

### Compositor Setup
1. `ipc_create()` → `[reg_send, reg_recv]`
2. `svc_register("compositor\0", reg_send)`
3. Create `CompletionPort`, submit `OP_IPC_RECV` on `reg_recv`

### Client Connects
1. `svc_lookup("compositor\0")` → dup of `reg_send`
2. Create two channel pairs: `c2s` (client→server) and `s2c` (server→client)
3. `ipc_send(reg_send, MSG_CONNECT { w, h, fds=[c2s_recv, s2c_send] })`
4. `ipc_recv(s2c_recv)` → `MSG_WINDOW_CREATED { id, w, h, fds=[buf_fd, notify_fd] }`
5. `mmap(MAP_SHARED, buf_fd)` → pixel buffer
6. Draw, then `notify_signal(notify_fd)`

### Compositor Accepts
1. Extract `c2s_recv`, `s2c_send` from message
2. Allocate shmem buffer + notify fd
3. `ipc_send(s2c_send, MSG_WINDOW_CREATED { fds=[buf_fd, notify_fd] })`
4. Arm `OP_RING_WAIT` on notify fd, `OP_IPC_RECV` on `c2s_recv`
5. Re-arm `OP_IPC_RECV` on `reg_recv` for next client

## Wire Protocol

| Tag | Name | Direction | data[] | fds[] |
|-----|------|-----------|--------|-------|
| 1 | MSG_CONNECT | client→compositor | [w, h, 0] | [c2s_recv, s2c_send] |
| 2 | MSG_WINDOW_CREATED | compositor→client | [wid, w, h] | [buf_fd, notify_fd] |
| 3 | MSG_PRESENT | client→compositor | [wid, 0, 0] | — |
| 4 | MSG_CLOSE | client→compositor | [wid, 0, 0] | — |

## Compositor Event Loop

```
port.wait(min=1) → completions
  TAG_NEW_CLIENT    → handle_connect(), re-arm OP_IPC_RECV on reg_recv
  TAG_DAMAGE(wid)   → mark dirty, re-arm OP_RING_WAIT
  TAG_CMD(wid)      → handle MSG_PRESENT/MSG_CLOSE, re-arm OP_IPC_RECV
if any dirty → composite()
```

## Compositing

- BGRA throughout (matches BGA native format, zero conversion)
- Background: solid dark grey (`0x00282828`)
- Window placement: auto-tile (2×2 grid)
- Full-screen repaint on any damage (acceptable at 1024×768)
- Painter's algorithm, back-to-front

## Files

| File | Role |
|------|------|
| `libkernel/src/service.rs` | Service registry (`register`, `lookup`) |
| `libkernel/src/framebuffer.rs` | LFB phys addr globals (`set_lfb_phys`, `get_lfb_phys`) |
| `osl/src/syscalls/service.rs` | `sys_svc_register`, `sys_svc_lookup` |
| `osl/src/syscalls/fb.rs` | `sys_framebuffer_open` |
| `user-rs/rt/src/compositor_proto.rs` | Protocol constants |
| `user-rs/compositor/` | Compositor binary |
| `user-rs/demo-client/` | Demo client binary |

## Usage

```sh
# Build kernel
cargo bootimage --manifest-path kernel/Cargo.toml

# Build and deploy Rust userspace (compositor + demo-client)
scripts/user-rs-build.sh

# Run
scripts/run.sh
```

The compositor is auto-launched by the kernel at boot (see `launch_compositor`
in `kernel/src/main.rs`).  Run `demo-client` from the shell to display a
test gradient.

## Future Work

- Input routing: keyboard events → focused window
- Window management: move, resize, focus, Z-order
- Double buffering: back-buffer swap
- Alpha blending
- Dirty rect optimization
- Write-combining PAT entries for LFB pages
- Service auto-cleanup on process exit
