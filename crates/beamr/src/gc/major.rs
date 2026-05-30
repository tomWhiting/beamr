//! Full heap compaction.
//!
//! When the old generation fills, copies all live data (young and
//! old) to a fresh heap, defragmenting as a side effect. More
//! expensive than minor GC but runs rarely. Triggered when the
//! old generation exceeds its growth threshold.

pub(crate) fn _scaffold() {}
