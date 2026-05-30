//! Binary and bitstring representation.
//!
//! Binaries can be standalone heap-allocated blobs or sub-binaries
//! that share the underlying bytes of a parent without copying.
//! Reference-counted shared storage for large binaries avoids
//! copying on message send while preserving the semantic model
//! of per-process isolation.

pub(crate) fn _scaffold() {}
