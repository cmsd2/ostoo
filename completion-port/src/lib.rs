//! Shared-memory SPSC ring buffer and generic completion port.
//!
//! `no_std` + `alloc`.  Pure ring protocol logic for a single-producer /
//! single-consumer queue laid out in shared memory, plus a bounded
//! completion queue with single-waiter wake.

#![no_std]

extern crate alloc;

mod types;
mod ring;
mod port;

pub use types::{IoSubmission, IoCompletion, RingHeader};
pub use ring::{SpscRing, RING_ENTRIES_OFFSET, MAX_SQ_ENTRIES, MAX_CQ_ENTRIES};
pub use port::{Waker, CompletionPort};
