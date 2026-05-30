//! Per-process generational heap allocator.
//!
//! Each process owns two private bump regions: a young generation (nursery) for
//! new allocations and an old generation for promoted/live data. Regions use
//! separate backing vectors so nursery and old objects are never mixed in the
//! same memory area. The backing vectors are pre-sized and never grow while live
//! pointers may refer into them; GC replaces regions only after rewriting roots.

use std::fmt;

/// Default per-process heap capacity, in machine words.
pub const DEFAULT_HEAP_SIZE: usize = 233;

const DEFAULT_OLD_HEAP_SIZE: usize = DEFAULT_HEAP_SIZE;

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

/// One fixed-capacity bump region inside a process heap.
#[derive(Debug)]
pub(crate) struct HeapRegion {
    words: Vec<u64>,
    used: usize,
    high_water_mark: usize,
}

impl HeapRegion {
    fn new(capacity: usize) -> Self {
        Self {
            words: vec![0; capacity],
            used: 0,
            high_water_mark: 0,
        }
    }

    fn with_used(words: Vec<u64>, used: usize) -> Self {
        debug_assert!(used <= words.len());
        Self {
            words,
            used,
            high_water_mark: used,
        }
    }

    fn alloc(&mut self, words: usize) -> Result<*mut u64, HeapFull> {
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

    const fn used(&self) -> usize {
        self.used
    }

    fn capacity(&self) -> usize {
        self.words.len()
    }

    const fn high_water_mark(&self) -> usize {
        self.high_water_mark
    }

    fn available(&self) -> usize {
        self.capacity().saturating_sub(self.used)
    }

    fn reset(&mut self) {
        self.words[..self.used].fill(0);
        self.used = 0;
    }

    fn contains(&self, ptr: *const u64) -> bool {
        let start = self.words.as_ptr().addr();
        let end = start.saturating_add(self.capacity() * std::mem::size_of::<u64>());
        let addr = ptr.addr();
        addr >= start && addr < end
    }
}

/// Generational bump allocator for one process heap.
#[derive(Debug)]
pub struct Heap {
    young: HeapRegion,
    old: HeapRegion,
    initial_capacity: usize,
    previous_capacity: usize,
}

impl Heap {
    /// Create a heap with room for `capacity` machine words in the nursery.
    ///
    /// `capacity()` reports nursery capacity because raw `alloc` targets the
    /// nursery. The old generation is a distinct region with its own capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let previous_capacity = fibonacci_previous(capacity);
        Self {
            young: HeapRegion::new(capacity),
            old: HeapRegion::new(DEFAULT_OLD_HEAP_SIZE.max(capacity)),
            initial_capacity: capacity,
            previous_capacity,
        }
    }

    /// Allocate `words` contiguous machine words from the young generation.
    pub fn alloc(&mut self, words: usize) -> Result<*mut u64, HeapFull> {
        self.young.alloc(words)
    }

    /// Allocate `words` contiguous machine words from the old generation.
    pub(crate) fn alloc_old(&mut self, words: usize) -> Result<*mut u64, HeapFull> {
        self.old.alloc(words)
    }

    /// Allocate from a standalone fresh old-space region used during major GC.
    pub(crate) fn alloc_in_region(
        region: &mut HeapRegion,
        words: usize,
    ) -> Result<*mut u64, HeapFull> {
        region.alloc(words)
    }

    /// Build a fresh old-space region for major compaction.
    pub(crate) fn fresh_old_region(&self, capacity: usize) -> HeapRegion {
        HeapRegion::new(capacity.max(self.initial_capacity))
    }

    /// Replace old generation with a compacted fresh region.
    pub(crate) fn replace_old(&mut self, region: HeapRegion) {
        self.old = region;
    }

    /// Number of words currently allocated in the young generation.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.young.used()
    }

    /// Total words currently allocated across young and old generations.
    #[must_use]
    pub const fn total_used(&self) -> usize {
        self.young.used() + self.old.used()
    }

    /// Young-generation word capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.young.capacity()
    }

    /// Total capacity across young and old generations.
    #[must_use]
    pub fn total_capacity(&self) -> usize {
        self.young_capacity() + self.old_capacity()
    }

    /// Young-generation word capacity.
    #[must_use]
    pub fn young_capacity(&self) -> usize {
        self.young.capacity()
    }

    /// Old-generation word capacity.
    #[must_use]
    pub fn old_capacity(&self) -> usize {
        self.old.capacity()
    }

    /// Words currently allocated in the young generation.
    #[must_use]
    pub const fn young_used(&self) -> usize {
        self.young.used()
    }

    /// Words currently allocated in the old generation.
    #[must_use]
    pub const fn old_used(&self) -> usize {
        self.old.used()
    }

    /// Maximum nursery words allocated at once since heap creation or growth.
    #[must_use]
    pub const fn high_water_mark(&self) -> usize {
        self.young.high_water_mark()
    }

    /// Number of words available before nursery allocation reports [`HeapFull`].
    #[must_use]
    pub fn available(&self) -> usize {
        self.young.available()
    }

    /// Number of words available in old space before promotion fails.
    #[must_use]
    pub fn old_available(&self) -> usize {
        self.old.available()
    }

    /// True if `ptr` points into the currently allocated young region storage.
    #[must_use]
    pub fn young_contains(&self, ptr: *const u64) -> bool {
        self.young.contains(ptr)
    }

    /// True if `ptr` points into the currently allocated old region storage.
    #[must_use]
    pub fn old_contains(&self, ptr: *const u64) -> bool {
        self.old.contains(ptr)
    }

    /// True if `ptr` points into any current heap region storage.
    #[must_use]
    pub fn contains(&self, ptr: *const u64) -> bool {
        self.young_contains(ptr) || self.old_contains(ptr)
    }

    /// Reclaim the nursery wholesale after all live young objects are promoted.
    pub(crate) fn reset_young(&mut self) {
        self.young.reset();
    }

    /// Grow young generation to the next Fibonacci-like capacity.
    pub fn grow_to_next_capacity(&mut self) {
        let current = self.young_capacity();
        let next = current
            .saturating_add(self.previous_capacity)
            .max(current + 1);
        self.previous_capacity = current;
        self.young = HeapRegion::new(next);
    }

    /// Ensure old space has enough free words for an upcoming promotion/copy.
    pub(crate) fn ensure_old_available(&mut self, needed: usize) {
        while self.old_available() < needed {
            let current = self.old_capacity();
            let next = current
                .saturating_add(fibonacci_previous(current))
                .max(current.saturating_add(needed));
            self.grow_old_to(next);
        }
    }

    /// Grow old generation capacity without moving existing words' base when not needed.
    pub(crate) fn grow_old_to(&mut self, capacity: usize) {
        if capacity <= self.old_capacity() {
            return;
        }

        let mut words = vec![0; capacity];
        words[..self.old.used()].copy_from_slice(&self.old.words[..self.old.used()]);
        self.old = HeapRegion::with_used(words, self.old.used());
    }

    /// Shrink old space after major GC when utilization is below `threshold`.
    ///
    /// The selected capacity is the smallest Fibonacci-like capacity that fits
    /// live data, is at least the initial heap size, and never exceeds current
    /// capacity when shrink is not warranted.
    pub(crate) fn shrink_old_after_major_if_underutilized(&mut self, threshold: f64) {
        let capacity = self.old_capacity();
        if capacity == 0 {
            return;
        }

        let utilization = self.old_used() as f64 / capacity as f64;
        if utilization >= threshold || capacity <= self.initial_capacity {
            return;
        }

        let target = fibonacci_capacity_for(self.old_used().max(self.initial_capacity));
        if target >= capacity {
            return;
        }

        let mut words = vec![0; target];
        words[..self.old.used()].copy_from_slice(&self.old.words[..self.old.used()]);
        self.old = HeapRegion::with_used(words, self.old.used());
    }

    pub(crate) fn copy_words_from_ptr(&self, src: *const u64, len: usize) -> Vec<u64> {
        // SAFETY: GC computes object sizes from valid object headers/cell tags;
        // `src..src+len` belongs to the source heap region while copying.
        unsafe { std::slice::from_raw_parts(src, len).to_vec() }
    }

    pub(crate) fn write_words(dst: *mut u64, words: &[u64]) {
        // SAFETY: destination is freshly allocated for exactly `words.len()`
        // words in a heap region and does not overlap the temporary source vec.
        unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), dst, words.len()) }
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new(DEFAULT_HEAP_SIZE)
    }
}

fn fibonacci_previous(capacity: usize) -> usize {
    let mut prev = 144;
    let mut current = DEFAULT_HEAP_SIZE;
    while current < capacity {
        let next = prev + current;
        prev = current;
        current = next;
    }
    prev.min(capacity)
}

fn fibonacci_capacity_for(needed: usize) -> usize {
    let mut previous = 144;
    let mut current = DEFAULT_HEAP_SIZE;
    while current < needed {
        let next = previous + current;
        previous = current;
        current = next;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_HEAP_SIZE, Heap, HeapFull};

    #[test]
    fn new_heap_reports_young_capacity_and_zero_used() {
        let heap = Heap::new(1024);

        assert_eq!(heap.capacity(), 1024);
        assert_eq!(heap.young_capacity(), 1024);
        assert_eq!(heap.used(), 0);
        assert_eq!(heap.young_used(), 0);
        assert_eq!(heap.old_used(), 0);
        assert_eq!(heap.high_water_mark(), 0);
    }

    #[test]
    fn alloc_returns_pointer_in_young_and_advances_used() {
        let mut heap = Heap::new(8);

        let ptr = heap.alloc(3).expect("allocation should fit");

        assert!(!ptr.is_null());
        assert!(heap.young_contains(ptr));
        assert!(!heap.old_contains(ptr));
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

    #[test]
    fn young_and_old_are_distinct_regions() {
        let mut heap = Heap::new(DEFAULT_HEAP_SIZE);
        let young = heap.alloc(1).expect("young allocation fits");
        let old = heap.alloc_old(1).expect("old allocation fits");

        assert!(heap.young_contains(young));
        assert!(heap.old_contains(old));
        assert!(!heap.old_contains(young));
        assert!(!heap.young_contains(old));
    }

    #[test]
    fn grows_follow_fibonacci_like_sequence() {
        let mut heap = Heap::new(DEFAULT_HEAP_SIZE);

        heap.grow_to_next_capacity();
        assert_eq!(heap.capacity(), 377);
        heap.grow_to_next_capacity();
        assert_eq!(heap.capacity(), 610);
        heap.grow_to_next_capacity();
        assert_eq!(heap.capacity(), 987);
    }

    #[test]
    fn shrink_never_goes_below_initial_size() {
        let mut heap = Heap::new(DEFAULT_HEAP_SIZE);
        heap.grow_old_to(987);
        heap.shrink_old_after_major_if_underutilized(0.25);

        assert_eq!(heap.old_capacity(), DEFAULT_HEAP_SIZE);
    }
}
