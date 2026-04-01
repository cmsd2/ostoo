//! SPSC ring buffer over caller-provided memory.
//!
//! Memory layout (per ring):
//! ```text
//! offset 0..64   RingHeader  (head, tail, mask, flags — cache-line aligned)
//! offset 64..    entry[0], entry[1], ...
//! ```

use core::marker::PhantomData;
use core::sync::atomic::Ordering;

use crate::types::RingHeader;

/// Offset (in bytes) where ring entries start, cache-line aligned.
pub const RING_ENTRIES_OFFSET: usize = 64;

/// Maximum SQ entries that fit in a single 4 KiB page:
/// (4096 - 64) / 48 = 84, rounded down to power of 2 = 64
pub const MAX_SQ_ENTRIES: u32 = 64;

/// Maximum CQ entries that fit in a single 4 KiB page:
/// (4096 - 64) / 24 = 168, rounded down to power of 2 = 128
pub const MAX_CQ_ENTRIES: u32 = 128;

/// View onto an SPSC ring in caller-provided memory.
///
/// `T` is the entry type (`IoSubmission` for SQ, `IoCompletion` for CQ).
/// First 64 bytes = `RingHeader`, entries start at offset 64.
///
/// # Safety
///
/// The caller must ensure:
/// - `base` points to a region of at least `RING_ENTRIES_OFFSET + entry_count * size_of::<T>()` bytes.
/// - `entry_count` is a power of 2.
/// - The memory remains valid for the lifetime of this struct.
/// - Only one producer and one consumer access the ring concurrently.
pub struct SpscRing<T: Copy> {
    base: *mut u8,
    entry_count: u32,
    _marker: PhantomData<T>,
}

// SpscRing is Send+Sync: the raw pointer is used under SPSC discipline
// and the caller guarantees the backing memory outlives the ring.
unsafe impl<T: Copy> Send for SpscRing<T> {}
unsafe impl<T: Copy> Sync for SpscRing<T> {}

impl<T: Copy> SpscRing<T> {
    /// Create a ring view from a raw pointer and entry count.
    ///
    /// # Safety
    ///
    /// - `base` must point to memory of at least
    ///   `RING_ENTRIES_OFFSET + entry_count * size_of::<T>()` bytes.
    /// - `entry_count` must be a power of 2.
    /// - The memory must remain valid for the lifetime of this struct.
    pub unsafe fn from_raw_parts(base: *mut u8, entry_count: u32) -> Self {
        debug_assert!(entry_count.is_power_of_two());
        SpscRing {
            base,
            entry_count,
            _marker: PhantomData,
        }
    }

    /// Initialise the ring header (zero head/tail, set mask).
    ///
    /// # Safety
    ///
    /// Must only be called once, before any concurrent access.
    pub unsafe fn init_header(&self) {
        let hdr = self.base as *mut RingHeader;
        (*hdr).head = core::sync::atomic::AtomicU32::new(0);
        (*hdr).tail = core::sync::atomic::AtomicU32::new(0);
        (*hdr).mask = self.entry_count - 1;
        (*hdr).flags = 0;
    }

    /// Access the ring header.
    pub fn header(&self) -> &RingHeader {
        unsafe { &*(self.base as *const RingHeader) }
    }

    /// Number of entries the ring was created with.
    pub fn entry_count(&self) -> u32 {
        self.entry_count
    }

    // -- Low-level index accessors --

    /// Read the head value with Relaxed ordering (used by the head's owner).
    pub fn head(&self) -> u32 {
        self.header().head.load(Ordering::Relaxed)
    }

    /// Read the tail value with Relaxed ordering (used by the tail's owner).
    pub fn tail(&self) -> u32 {
        self.header().tail.load(Ordering::Relaxed)
    }

    /// Read the head with Acquire ordering (used by the producer to see consumer progress).
    pub fn head_acquire(&self) -> u32 {
        self.header().head.load(Ordering::Acquire)
    }

    /// Read the tail with Acquire ordering (used by the consumer to see producer progress).
    pub fn tail_acquire(&self) -> u32 {
        self.header().tail.load(Ordering::Acquire)
    }

    /// Set the head with Release ordering (consumer advances head after reading).
    pub fn set_head(&self, val: u32) {
        self.header().head.store(val, Ordering::Release);
    }

    /// Set the tail with Release ordering (producer advances tail after writing).
    pub fn set_tail(&self, val: u32) {
        self.header().tail.store(val, Ordering::Release);
    }

    // -- Entry-level accessors --

    /// Read the entry at the given logical index (masked internally).
    pub fn read_entry(&self, index: u32) -> T {
        let slot = (index & (self.entry_count - 1)) as usize;
        let offset = RING_ENTRIES_OFFSET + slot * core::mem::size_of::<T>();
        unsafe { *((self.base as usize + offset) as *const T) }
    }

    /// Write an entry at the given logical index (masked internally).
    pub fn write_entry(&self, index: u32, entry: &T) {
        let slot = (index & (self.entry_count - 1)) as usize;
        let offset = RING_ENTRIES_OFFSET + slot * core::mem::size_of::<T>();
        unsafe { *((self.base as usize + offset) as *mut T) = *entry; }
    }

    // -- High-level SPSC operations --

    /// Push an entry (producer side). Returns `false` if the ring is full.
    ///
    /// Atomic ordering: Acquire on head (consumer's counter), Release on tail.
    // [spec: spsc_ring/spsc_ring.tla Producer — AcquireHead through ReleaseTail]
    pub fn push(&self, entry: &T) -> bool {
        let hdr = self.header();

        // [spec: spsc_ring/spsc_ring.tla AcquireHead]
        let head = hdr.head.load(Ordering::Acquire);
        let tail = hdr.tail.load(Ordering::Relaxed);

        // [spec: spsc_ring/spsc_ring.tla CheckFull]
        if tail.wrapping_sub(head) >= self.entry_count {
            return false;
        }

        // [spec: spsc_ring/spsc_ring.tla WriteSlot]
        self.write_entry(tail, entry);

        // [spec: spsc_ring/spsc_ring.tla ReleaseTail]
        hdr.tail.store(tail.wrapping_add(1), Ordering::Release);

        true
    }

    /// Pop an entry (consumer side). Returns `None` if the ring is empty.
    ///
    /// Atomic ordering: Acquire on tail (producer's counter), Release on head.
    // [spec: spsc_ring/spsc_ring.tla Consumer — AcquireTail through ReleaseHead]
    pub fn pop(&self) -> Option<T> {
        let hdr = self.header();

        // [spec: spsc_ring/spsc_ring.tla AcquireTail]
        let tail = hdr.tail.load(Ordering::Acquire);
        let head = hdr.head.load(Ordering::Relaxed);

        // [spec: spsc_ring/spsc_ring.tla CheckEmpty]
        if head == tail {
            return None;
        }

        // [spec: spsc_ring/spsc_ring.tla ReadSlot]
        let entry = self.read_entry(head);

        // [spec: spsc_ring/spsc_ring.tla ReleaseHead]
        hdr.head.store(head.wrapping_add(1), Ordering::Release);

        Some(entry)
    }

    /// Number of entries currently in the ring (tail - head, using the
    /// appropriate acquire orderings for cross-thread reads).
    ///
    /// When called by the consumer, this acquires the producer's tail.
    /// When called by the producer, this acquires the consumer's head.
    /// For a general-purpose "how many?" query, both sides are acquired.
    pub fn len(&self) -> u32 {
        let tail = self.tail_acquire();
        let head = self.head_acquire();
        tail.wrapping_sub(head)
    }

    /// Number of free slots in the ring.
    pub fn available(&self) -> u32 {
        self.entry_count - self.len()
    }
}
