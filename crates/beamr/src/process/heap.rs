//! Per-process heap allocator.
//!
//! Each process starts with a small heap (233 words default) that
//! grows on demand. The heap is split into a young generation
//! (nursery) and an old generation for the GC. All boxed terms
//! are allocated here. The heap is private — no other process
//! can read or write it.

pub(crate) fn _scaffold() {}
