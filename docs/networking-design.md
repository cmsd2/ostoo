# Networking Design

## Overview

This document describes the planned networking architecture for ostoo.  The
design adds TCP/IP networking via a VirtIO network device and the smoltcp
protocol stack.

The initial implementation runs entirely in kernel space, matching the existing
pattern where VFS and block I/O run in the `devices` crate.  For the
longer-term microkernel path where the NIC driver and TCP/IP stack move to
userspace, see [microkernel-design.md](microkernel-design.md).

---

## Architecture

### Kernel-Space (Initial)

```
Userspace programs (socket/connect/bind/listen/accept/send/recv)
        │
  osl/src/net.rs                  ← syscall → smoltcp socket mapping
        │
  smoltcp::iface::Interface       ← protocol processing (TCP/IP/ARP/DHCP/DNS)
        │
  devices/src/virtio/net.rs       ← smoltcp Device trait wrapping VirtIONet
        │
  VirtIONet<KernelHal, PciTransport>  ← raw Ethernet frame send/receive
        │
  QEMU virtio-net-pci             ← -device virtio-net-pci,netdev=net0
                                     -netdev user,id=net0
```

### Userspace (Future)

Once the microkernel primitives from [microkernel-design.md](microkernel-design.md)
are in place, networking can be restructured:

```
Userspace programs (socket syscalls, routed by kernel to TCP/IP server)
        │
  TCP/IP server process           ← smoltcp in a userspace daemon
        │
  shared memory ring buffers      ← zero-copy packet passing
        │
  NIC driver process              ← virtio-net via mapped MMIO + IRQ fd
        │
  virtio-net-pci hardware
```

The kernel's socket syscall handlers become thin IPC stubs that route requests
to the TCP/IP server, preserving POSIX compatibility for musl.  This
corresponds to Phase C/D in the microkernel migration path.

---

## Kernel-Space vs Userspace

### Decision: kernel-space first

The initial implementation runs in kernel space.  Reasons:

- **Simpler.**  No IPC overhead — smoltcp directly accesses the virtio-net
  driver.  No message-passing for every packet.
- **Lower latency.**  No user/kernel context switches per packet.
- **Matches existing patterns.**  VFS operations already run in kernel via the
  `devices` crate; networking follows the same model.
- **Proven.**  Hermit OS and Kerla both use smoltcp + virtio-net in kernel
  space successfully.

The microkernel path (Phases C-D in [microkernel-design.md](microkernel-design.md))
moves the NIC driver and TCP/IP stack to userspace once shared memory, IRQ
delivery, and device MMIO mapping primitives exist.

---

## Protocols

| Layer | Protocol | Priority | Notes |
|---|---|---|---|
| Link | Ethernet II | Required | virtio-net provides raw frames |
| Link | ARP | Required | Automatic in smoltcp with Ethernet medium |
| Network | IPv4 | Required | Core routing |
| Network | ICMP | Required | Ping, error reporting |
| Transport | TCP | Required | Streams (HTTP, SSH, etc.) |
| Transport | UDP | Required | Datagrams (DNS, NTP, etc.) |
| Application | DHCPv4 | Required | Auto-configure IP/gateway/DNS from QEMU |
| Application | DNS | High | Name resolution |
| Network | IPv6 | Deferred | smoltcp supports it when ready |

---

## Crates

### virtio-drivers 0.13 (already in workspace)

Provides `device::net::VirtIONet` with:

- `new(transport, buf_len)` — initialize with PCI transport
- `mac_address()` — read hardware MAC
- `receive()` / `send(tx_buf)` — raw Ethernet frame I/O
- `ack_interrupt()` / `enable_interrupts()` — IRQ support

The existing `KernelHal` and `create_pci_transport()` work unchanged.  Add
device constants for virtio-net PCI IDs (modern: 0x1041, legacy: 0x1000).

### smoltcp 0.12

`no_std` TCP/IP stack.  Works with `alloc` (ostoo already has a heap).

Suggested Cargo features:

```toml
smoltcp = { version = "0.12", default-features = false, features = [
    "alloc", "log", "medium-ethernet",
    "proto-ipv4", "proto-dhcpv4", "proto-dns",
    "socket-raw", "socket-udp", "socket-tcp",
    "socket-icmp", "socket-dhcpv4", "socket-dns",
] }
```

Provides:

- **`Interface`** — central type that drives all protocol processing
- **`phy::Device` trait** — integrate a NIC via `RxToken`/`TxToken`
  (zero-copy, token-based)
- **Socket types** — raw, ICMP, TCP, UDP, DHCPv4 client, DNS resolver
- **ARP / neighbor cache** — automatic with Ethernet medium

No additional crates needed.  embassy-net wraps smoltcp but is tied to the
Embassy async runtime — skip it.

---

## Integration Points

### NIC Driver (`devices/src/virtio/net.rs`)

Wrap `VirtIONet<KernelHal, PciTransport, 64>` in a struct implementing
smoltcp's `phy::Device` trait:

- `receive()` → `RxToken` (read raw frame from virtqueue)
- `transmit()` → `TxToken` (write raw frame to virtqueue)
- `capabilities()` → MTU 1514, `Medium::Ethernet`, no checksum offload

### Polling

smoltcp requires periodic `Interface::poll()` calls.  Two options:

1. **Timer-driven** — poll every 10ms from the scheduler tick (simple,
   higher latency).
2. **IRQ-driven** — virtio-net interrupt triggers poll (responsive, more
   complex).

Start with timer-driven.  Migrate to IRQ-driven once the basics work.

### Blocking Bridge

`osl::blocking::blocking()` already converts async → sync for VFS.  Same
pattern for socket operations: spawn an async task that polls smoltcp, block
the calling thread until data arrives or the operation completes.

### Socket File Descriptors

Create a `SocketHandle` struct implementing the `FileHandle` trait.  Store in
the process `fd_table` like pipes and files:

- `read()` on a TCP socket → recv from smoltcp TCP socket
- `write()` on a TCP socket → send to smoltcp TCP socket
- `close()` → release smoltcp socket handle

UDP sockets need `sendto`/`recvfrom` for the address parameter.

### DHCP at Boot

After virtio-net init, create a DHCPv4 socket, poll until configured, then
set the interface IP/gateway/DNS.  QEMU user-mode networking provides DHCP
at 10.0.2.2 with default subnet 10.0.2.0/24.

---

## Syscalls

New Linux-compatible syscall numbers to add in `osl/src/syscalls/mod.rs`:

| Nr | Name | Purpose |
|---|---|---|
| 41 | socket | Create AF_INET SOCK_STREAM/SOCK_DGRAM |
| 42 | connect | TCP connect to remote |
| 43 | accept | Accept incoming TCP connection |
| 44 | sendto | Send datagram with destination address |
| 45 | recvfrom | Receive datagram with source address |
| 46 | sendmsg | Scatter/gather send (needed by musl) |
| 47 | recvmsg | Scatter/gather receive (needed by musl) |
| 49 | bind | Bind to local address/port |
| 50 | listen | Mark socket as listening |
| 51 | getsockname | Get local address of socket |
| 54 | setsockopt | Set socket options (SO_REUSEADDR, etc.) |
| 55 | getsockopt | Get socket options |

Stubs returning `ENOSYS` for unsupported options are acceptable initially.

---

## QEMU Configuration

Add to `scripts/run.sh`:

```bash
-device virtio-net-pci,netdev=net0 \
-netdev user,id=net0
```

QEMU user-mode networking (SLIRP) provides:

- NAT — guest can reach the internet and the host
- Built-in DHCP server at 10.0.2.2
- Built-in DNS forwarder at 10.0.2.3
- No host-side configuration needed

For host → guest connections, add port forwards:
`-netdev user,id=net0,hostfwd=tcp::8080-:80`

---

## What This Enables

With TCP/UDP + DNS + DHCP, userspace programs compiled against musl can use
the standard POSIX socket API.  This opens the door to:

- Ping (`ICMP echo`)
- Simple network tools (netcat-like, wget-like)
- HTTP client/server
- Eventually SSH (requires more crypto infrastructure)
