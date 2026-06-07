//! Off-heap reference-counted binary storage and ProcBin writer.
//!
//! Large binaries can live outside a process heap and be referenced by small
//! ProcBin heap objects. Sharing the off-heap buffer is an optimisation: process
//! isolation is preserved because the bytes are immutable through this API.

use std::sync::Arc;

use crate::term::Term;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::boxed::{BoxedHeader, BoxedTag};

/// Maximum byte length stored as an inline heap binary.
///
/// BEAM-compatible binary allocation keeps binaries with length less than or
/// equal to this threshold in the process heap. Larger binaries are promoted to
/// ProcBin terms that reference immutable off-heap storage.
pub const REFC_BINARY_THRESHOLD: usize = 64;

const PROC_BIN_PAYLOAD_WORDS: usize = 2;
const PROC_BIN_WORDS: usize = 1 + PROC_BIN_PAYLOAD_WORDS;
const PROC_BIN_FLAGS: u64 = 0;

/// Number of heap words required for threshold-aware binary allocation.
pub const fn alloc_binary_word_count(byte_len: usize) -> usize {
    if byte_len <= REFC_BINARY_THRESHOLD {
        2 + packed_word_count(byte_len)
    } else {
        PROC_BIN_WORDS
    }
}

/// Off-heap immutable binary bytes shared by reference count.
#[derive(Clone, Debug)]
pub struct SharedBinary {
    inner: Arc<Vec<u8>>,
}

impl SharedBinary {
    /// Creates a new off-heap binary buffer.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(bytes),
        }
    }

    /// Returns the bytes stored in this shared binary.
    pub fn as_bytes(&self) -> &[u8] {
        self.inner.as_slice()
    }

    /// Returns the byte length of this shared binary.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true when this shared binary contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the number of strong references to this off-heap buffer.
    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    pub(crate) fn clone_from_raw_word(raw: u64) -> Self {
        let ptr = raw as *const Vec<u8>;
        // SAFETY: ProcBin writers store pointers produced by `Arc::into_raw` for
        // `Arc<Vec<u8>>`. Reconstitute the heap-owned strong reference only long
        // enough to clone it, then convert it back to raw so ownership remains in
        // the heap word.
        let arc = unsafe { Arc::from_raw(ptr) };
        let cloned = Arc::clone(&arc);
        let _raw = Arc::into_raw(arc);
        Self { inner: cloned }
    }

    pub(crate) fn bytes_from_raw_word(raw: u64) -> &'static [u8] {
        let ptr = raw as *const Vec<u8>;
        // SAFETY: ProcBin heap words own a strong `Arc<Vec<u8>>` reference, so
        // the pointed-to `Vec` remains live while the ProcBin object is live.
        // Access is read-only through shared references.
        unsafe { (*ptr).as_slice() }
    }

    pub(crate) fn retained_raw_word(&self) -> u64 {
        Arc::into_raw(Arc::clone(&self.inner)) as u64
    }
}

/// Writes a ProcBin layout (`header, flags, raw Arc pointer`) into `heap`.
pub fn write_proc_bin(heap: &mut [u64], shared: &SharedBinary) -> Option<Term> {
    if heap.len() < PROC_BIN_WORDS {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::ProcBin, PROC_BIN_PAYLOAD_WORDS);
    heap[1] = PROC_BIN_FLAGS;
    heap[2] = shared.retained_raw_word();

    Some(Term::boxed_ptr(heap.as_ptr()))
}

/// Allocates binary bytes using inline storage up to [`REFC_BINARY_THRESHOLD`]
/// and ProcBin/refc storage above it.
pub fn alloc_binary(heap: &mut [u64], bytes: &[u8]) -> Option<Term> {
    if bytes.len() <= REFC_BINARY_THRESHOLD {
        write_binary(heap, bytes)
    } else {
        let shared = SharedBinary::new(bytes.to_vec());
        write_proc_bin(heap, &shared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn shared_binary_clone_shares_data_and_counts_refs() {
        assert_send_sync::<SharedBinary>();
        let shared = SharedBinary::new(b"shared".to_vec());
        let cloned = shared.clone();

        assert_eq!(shared.as_bytes(), b"shared");
        assert_eq!(cloned.as_bytes(), b"shared");
        assert_eq!(shared.len(), 6);
        assert!(!shared.is_empty());
        assert_eq!(shared.ref_count(), 2);
        assert_eq!(cloned.ref_count(), 2);
    }

    #[test]
    fn write_proc_bin_uses_three_words_for_large_binary() {
        let shared = SharedBinary::new(vec![0xAB; 100 * 1024]);
        let mut heap = [0_u64; PROC_BIN_WORDS];
        let term = write_proc_bin(&mut heap, &shared).expect("proc bin should fit");

        assert!(term.is_boxed());
        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::ProcBin));
        assert_eq!(BoxedHeader::size(heap[0]), PROC_BIN_PAYLOAD_WORDS);
        assert_eq!(heap[1], PROC_BIN_FLAGS);
        assert_ne!(heap[2], 0);
        assert_eq!(heap.len(), 3);
        assert_eq!(shared.ref_count(), 2);
    }

    #[test]
    fn write_proc_bin_rejects_too_small_heap_slice() {
        let shared = SharedBinary::new(b"binary".to_vec());
        let mut heap = [0_u64; 2];

        assert_eq!(write_proc_bin(&mut heap, &shared), None);
    }

    #[test]
    fn proc_bin_accessor_reads_bytes_and_clones_arc_only() {
        let shared = SharedBinary::new(b"proc-bin".to_vec());
        let mut heap = [0_u64; PROC_BIN_WORDS];
        let term = write_proc_bin(&mut heap, &shared).expect("proc bin should fit");
        let proc_bin = crate::term::boxed::ProcBin::new(term).expect("proc bin accessor");

        assert_eq!(proc_bin.len(), 8);
        assert_eq!(proc_bin.as_bytes(), b"proc-bin");
        let cloned = proc_bin.shared_binary();
        assert_eq!(cloned.as_bytes(), b"proc-bin");
        assert_eq!(shared.ref_count(), 3);
    }

    #[test]
    fn alloc_binary_stores_empty_and_threshold_sized_binaries_inline() {
        for bytes in [Vec::new(), vec![0xAB; REFC_BINARY_THRESHOLD]] {
            let mut heap = vec![0_u64; alloc_binary_word_count(bytes.len())];
            let term = alloc_binary(&mut heap, &bytes).expect("binary should fit");

            assert!(term.is_boxed());
            assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::Binary));
            assert_eq!(
                crate::term::binary::Binary::new(term)
                    .expect("inline binary")
                    .as_bytes(),
                bytes.as_slice()
            );
        }
    }

    #[test]
    fn alloc_binary_promotes_above_threshold_to_proc_bin() {
        let bytes = [0xCD; REFC_BINARY_THRESHOLD + 1];
        let mut heap = vec![0_u64; alloc_binary_word_count(bytes.len())];
        let term = alloc_binary(&mut heap, &bytes).expect("binary should fit");

        assert!(term.is_boxed());
        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::ProcBin));
        assert_eq!(
            crate::term::boxed::ProcBin::new(term)
                .expect("proc bin")
                .as_bytes(),
            bytes
        );
        assert_eq!(heap.len(), PROC_BIN_WORDS);
    }
}
