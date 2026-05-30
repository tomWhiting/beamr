//! Per-process heap allocator.
//!
//! Each process starts with a small heap (233 words default). Boxed terms are
//! allocated from this fixed-capacity bump buffer until the interpreter asks GC
//! to reclaim or move live data. The heap is private — no other process can
//! read or write it.

use std::fmt;

/// Default per-process heap capacity, in machine words.
pub const DEFAULT_HEAP_SIZE: usize = 233;

/// Error returned when a heap allocation cannot be satisfied.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeapFull {
    requested: usize,
    available: usize,
}

impl HeapFull {
    /// Number of words requested by the failed allocation.
    #[must_use]
    pub const fn requested(self) -> usize {
        self.requested
    }

    /// Number of free words remaining when the allocation failed.
    #[must_use]
    pub const fn available(self) -> usize {
        self.available
    }
}

impl fmt::Display for HeapFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "heap full: requested {} words with {} available",
            self.requested, self.available
        )
    }
}

impl std::error::Error for HeapFull {}

/// Fixed-capacity bump allocator for one process heap.
#[derive(Debug)]
pub struct Heap {
    words: Vec<u64>,
    used: usize,
    high_water_mark: usize,
}

impl Heap {
    /// Create a heap with room for `capacity` machine words.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            words: vec![0; capacity],
            used: 0,
            high_water_mark: 0,
        }
    }

    /// Allocate `words` contiguous machine words from the bump pointer.
    ///
    /// Zero-word allocations return the current bump pointer and do not advance
    /// usage. The backing vector is pre-sized and never grown here, so pointers
    /// returned by earlier successful allocations remain stable until GC or heap
    /// destruction.
    pub fn alloc(&mut self, words: usize) -> Result<*mut u64, HeapFull> {
        let Some(end) = self.used.checked_add(words) else {
            return Err(HeapFull {
                requested: words,
                available: self.available(),
            });
        };

        if end > self.capacity() {
            return Err(HeapFull {
                requested: words,
                available: self.available(),
            });
        }

        let start = self.used;
        let ptr = self.words.as_mut_ptr().wrapping_add(start);
        self.used = end;
        self.high_water_mark = self.high_water_mark.max(self.used);
        Ok(ptr)
    }

    /// Number of words currently allocated.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    /// Total word capacity of this heap.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.words.len()
    }

    /// Maximum words that have been allocated at once since heap creation.
    #[must_use]
    pub const fn high_water_mark(&self) -> usize {
        self.high_water_mark
    }

    /// Number of words available before the heap reports [`HeapFull`].
    #[must_use]
    pub fn available(&self) -> usize {
        self.capacity().saturating_sub(self.used)
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new(DEFAULT_HEAP_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::{Heap, HeapFull};

    #[test]
    fn new_heap_reports_capacity_and_zero_used() {
        let heap = Heap::new(1024);

        assert_eq!(heap.capacity(), 1024);
        assert_eq!(heap.used(), 0);
        assert_eq!(heap.high_water_mark(), 0);
    }

    #[test]
    fn alloc_returns_pointer_and_advances_used() {
        let mut heap = Heap::new(8);

        let ptr = heap.alloc(3).expect("allocation should fit");

        assert!(!ptr.is_null());
        assert_eq!(heap.used(), 3);
        assert_eq!(heap.high_water_mark(), 3);
    }

    #[test]
    fn allocation_regions_do_not_overlap() {
        let mut heap = Heap::new(8);

        let first = heap.alloc(3).expect("first allocation should fit");
        let second = heap.alloc(2).expect("second allocation should fit");

        assert_eq!(second.addr() - first.addr(), 3 * std::mem::size_of::<u64>());
    }

    #[test]
    fn heap_full_preserves_usage() {
        let mut heap = Heap::new(4);
        let _ = heap.alloc(3).expect("initial allocation should fit");

        let error = heap
            .alloc(2)
            .expect_err("allocation should exceed capacity");

        assert_eq!(
            error,
            HeapFull {
                requested: 2,
                available: 1
            }
        );
        assert_eq!(heap.used(), 3);
        assert_eq!(heap.high_water_mark(), 3);
    }

    #[test]
    fn zero_word_allocation_does_not_advance_bump_pointer() {
        let mut heap = Heap::new(1);

        let first = heap.alloc(0).expect("zero word allocation should succeed");
        let second = heap.alloc(0).expect("zero word allocation should succeed");

        assert_eq!(first, second);
        assert_eq!(heap.used(), 0);
    }
}
