//! Generic top-down gap finder for non-overlapping occupied intervals.
//!
//! The `OccupiedRanges` trait abstracts iteration over occupied intervals
//! so the algorithm can be reused for mmap, MMIO windows, etc.

use alloc::collections::BTreeMap;
use crate::process::Vma;

/// Trait for collections of non-overlapping occupied intervals.
/// Implementors yield `(start, len)` pairs in ascending start-address order.
pub trait OccupiedRanges {
    /// Iterate occupied intervals in ascending start-address order.
    fn occupied_ranges(&self) -> impl DoubleEndedIterator<Item = (u64, u64)>;
}

/// Find the highest gap of at least `len` bytes within `[floor, ceiling)`.
/// Searches top-down (Linux default for mmap).
///
/// Returns the base address of the gap (aligned to `len`'s page alignment
/// is the caller's responsibility).
pub fn find_gap_topdown(
    ranges: &impl OccupiedRanges,
    floor: u64,
    ceiling: u64,
    len: u64,
) -> Option<u64> {
    if len == 0 || ceiling <= floor || len > ceiling - floor {
        return None;
    }

    let mut gap_end = ceiling;

    // Walk occupied ranges from highest to lowest.
    for (range_start, range_len) in ranges.occupied_ranges().rev() {
        let range_end = range_start + range_len;

        // Only consider ranges that are below our current gap_end.
        if range_end > gap_end {
            // Range extends past our gap_end — just update gap_end.
            if range_start < gap_end {
                gap_end = range_start;
            }
            continue;
        }

        // Gap is [range_end, gap_end). Check if it fits.
        if gap_end >= range_end + len && range_end >= floor {
            return Some(gap_end - len);
        }

        // Move gap_end down past this range.
        gap_end = range_start;
    }

    // Check the final gap [floor, gap_end).
    if gap_end >= floor + len {
        Some(gap_end - len)
    } else {
        None
    }
}

impl OccupiedRanges for BTreeMap<u64, Vma> {
    fn occupied_ranges(&self) -> impl DoubleEndedIterator<Item = (u64, u64)> {
        self.values().map(|vma| (vma.start, vma.len))
    }
}
