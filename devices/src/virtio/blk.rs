use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::task::{Context, Poll};

use futures_util::task::AtomicWaker;
use virtio_drivers::device::blk::{BlkReq, BlkResp, VirtIOBlk};
use virtio_drivers::transport::pci::PciTransport;

use libkernel::task::mailbox::Reply;

use crate::actor;
use super::KernelHal;

// ---------------------------------------------------------------------------
// IRQ state (one static for the single virtio-blk device)

static IRQ_PENDING: AtomicBool = AtomicBool::new(false);
static IRQ_WAKER: AtomicWaker = AtomicWaker::new();
static BLK_GSI: AtomicU32 = AtomicU32::new(u32::MAX); // MAX = not configured

/// Called from the interrupt handler registered for this device.
pub fn virtio_blk_irq_handler(_slot: usize) {
    IRQ_PENDING.store(true, Ordering::Release);
    // Mask the GSI to prevent an interrupt storm while we process.
    let gsi = BLK_GSI.load(Ordering::Relaxed);
    if gsi != u32::MAX {
        libkernel::apic::mask_gsi(gsi);
    }
    IRQ_WAKER.wake();
}

/// Initialise IRQ-driven I/O for the virtio-blk device.
///
/// `gsi` is the PCI interrupt line (GSI) read from config space.
/// Registers a dynamic interrupt vector, routes the GSI to it, and unmasks.
pub fn init_irq(gsi: u32) {
    BLK_GSI.store(gsi, Ordering::Relaxed);
    let vector = match super::register_blk_irq(virtio_blk_irq_handler) {
        Some(v) => v,
        None => {
            log::warn!("[virtio-blk] no free vector for IRQ; falling back to polling");
            return;
        }
    };
    libkernel::apic::route_gsi(gsi, vector);
    libkernel::apic::unmask_gsi(gsi);
    log::info!("[virtio-blk] IRQ: GSI {} -> vector {:#x}", gsi, vector);
}

// ---------------------------------------------------------------------------
// CompletionFuture — woken by IRQ via AtomicWaker.

struct CompletionFuture<'a> {
    device: &'a spin::Mutex<VirtIOBlk<KernelHal, PciTransport>>,
}

impl<'a> Future for CompletionFuture<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // Fast path: check if already complete.
        if self.device.lock().peek_used().is_some() {
            // Unmask GSI so the next request can be IRQ-driven.
            let gsi = BLK_GSI.load(Ordering::Relaxed);
            if gsi != u32::MAX {
                libkernel::apic::unmask_gsi(gsi);
            }
            return Poll::Ready(());
        }

        // Register waker before re-checking (standard check-register-recheck).
        IRQ_WAKER.register(cx.waker());

        // Re-check after registration to avoid missed wake.
        if self.device.lock().peek_used().is_some() {
            let gsi = BLK_GSI.load(Ordering::Relaxed);
            if gsi != u32::MAX {
                libkernel::apic::unmask_gsi(gsi);
            }
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

// ---------------------------------------------------------------------------
// Messages

pub enum VirtioBlkMsg {
    Read(u64, Reply<Result<Vec<u8>, ()>>),
    Write(u64, Vec<u8>, Reply<Result<(), ()>>),
}

// ---------------------------------------------------------------------------
// Info

#[derive(Debug)]
#[allow(dead_code)]
pub struct VirtioBlkInfo {
    pub capacity_sectors: u64,
    pub reads:  u64,
    pub writes: u64,
}

// ---------------------------------------------------------------------------
// Actor

pub struct VirtioBlkActor {
    device: spin::Mutex<VirtIOBlk<KernelHal, PciTransport>>,
    reads:  AtomicU64,
    writes: AtomicU64,
}

// VirtIOBlk contains raw pointers (queue DMA buffers).  Access is always
// serialised through the spin::Mutex so these impls are sound.
unsafe impl Send for VirtioBlkActor {}
unsafe impl Sync for VirtioBlkActor {}

impl VirtioBlkActor {
    pub fn new(transport: PciTransport) -> Self {
        let device = VirtIOBlk::<KernelHal, PciTransport>::new(transport)
            .expect("virtio-blk init failed");
        VirtioBlkActor {
            device: spin::Mutex::new(device),
            reads:  AtomicU64::new(0),
            writes: AtomicU64::new(0),
        }
    }
}

#[actor("virtio-blk", VirtioBlkMsg)]
impl VirtioBlkActor {
    // ── Read ─────────────────────────────────────────────────────────────────
    #[on_message(Read)]
    async fn on_read(&self, sector: u64, reply: Reply<Result<Vec<u8>, ()>>) {
        let mut buf  = alloc::vec![0u8; 512];
        let mut req  = BlkReq::default();
        let mut resp = BlkResp::default();

        let token = {
            let mut dev = self.device.lock();
            // Safety: buf lives for the duration of the I/O (stored in the
            // async state machine), and the device DMA range is valid.
            match unsafe {
                dev.read_blocks_nb(sector as usize, &mut req, buf.as_mut_slice(), &mut resp)
            } {
                Ok(t)  => t,
                Err(_) => { reply.send(Err(())); return; }
            }
        };

        // Wait for the device to signal completion.
        CompletionFuture { device: &self.device }.await;

        {
            let mut dev = self.device.lock();
            if unsafe {
                dev.complete_read_blocks(token, &req, buf.as_mut_slice(), &mut resp)
            }.is_err() {
                reply.send(Err(()));
                return;
            }
        }

        self.reads.fetch_add(1, Ordering::Relaxed);
        reply.send(Ok(buf));
    }

    // ── Write ────────────────────────────────────────────────────────────────
    #[on_message(Write)]
    async fn on_write(&self, sector: u64, data: Vec<u8>, reply: Reply<Result<(), ()>>) {
        let mut req  = BlkReq::default();
        let mut resp = BlkResp::default();

        let token = {
            let mut dev = self.device.lock();
            // Safety: data lives for the duration of the I/O.
            match unsafe {
                dev.write_blocks_nb(sector as usize, &mut req, data.as_slice(), &mut resp)
            } {
                Ok(t)  => t,
                Err(_) => { reply.send(Err(())); return; }
            }
        };

        CompletionFuture { device: &self.device }.await;

        {
            let mut dev = self.device.lock();
            if unsafe {
                dev.complete_write_blocks(token, &req, data.as_slice(), &mut resp)
            }.is_err() {
                reply.send(Err(()));
                return;
            }
        }

        self.writes.fetch_add(1, Ordering::Relaxed);
        reply.send(Ok(()));
    }

    // ── Info ──────────────────────────────────────────────────────────────────
    #[on_info]
    async fn on_info(&self) -> VirtioBlkInfo {
        let cap = self.device.lock().capacity();
        VirtioBlkInfo {
            capacity_sectors: cap,
            reads:  self.reads.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
        }
    }
}
