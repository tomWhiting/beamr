//! On-heap sub-binary views into parent binaries.
//!
//! Sub-binaries store a parent binary term plus byte offset/length metadata.
//! They do not own or copy bytes; accessors resolve the parent when reading.

use crate::term::{
    Term,
    boxed::{BoxedHeader, BoxedTag},
};

pub const SUB_BINARY_PAYLOAD_WORDS: usize = 4;
pub const SUB_BINARY_WORDS: usize = 1 + SUB_BINARY_PAYLOAD_WORDS;
const SUB_BINARY_FLAGS: u64 = 0;

/// Writes a SubBinary layout (`header, parent, offset, length, flags`) into `heap`.
pub fn write_sub_binary(
    heap: &mut [u64],
    parent_term: Term,
    offset: usize,
    length: usize,
) -> Option<Term> {
    if heap.len() < SUB_BINARY_WORDS {
        return None;
    }

    heap[0] = BoxedHeader::new(BoxedTag::SubBinary, SUB_BINARY_PAYLOAD_WORDS);
    heap[1] = parent_term.raw();
    heap[2] = offset as u64;
    heap[3] = length as u64;
    heap[4] = SUB_BINARY_FLAGS;

    Some(Term::boxed_ptr(heap.as_ptr()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::{ProcBin, SubBinary};
    use crate::term::shared_binary::{SharedBinary, write_proc_bin};

    #[test]
    fn sub_binary_views_proc_bin_with_fixed_five_word_layout() {
        let bytes: Vec<u8> = (0_u8..32).collect();
        let shared = SharedBinary::new(bytes);
        let mut proc_heap = [0_u64; 3];
        let parent = write_proc_bin(&mut proc_heap, &shared).expect("proc bin fits");
        assert!(ProcBin::new(parent).is_some());

        let mut heap = [0_u64; SUB_BINARY_WORDS];
        let term = write_sub_binary(&mut heap, parent, 10, 10).expect("sub binary fits");

        assert!(term.is_boxed());
        assert_eq!(BoxedHeader::tag(heap[0]), Some(BoxedTag::SubBinary));
        assert_eq!(BoxedHeader::size(heap[0]), SUB_BINARY_PAYLOAD_WORDS);
        assert_eq!(heap[1], parent.raw());
        assert_eq!(heap[2], 10);
        assert_eq!(heap[3], 10);
        assert_eq!(heap[4], SUB_BINARY_FLAGS);
        assert_eq!(heap.len(), 5);
        let sub_binary = SubBinary::new(term).expect("sub binary accessor");
        assert_eq!(sub_binary.parent(), parent);
        assert_eq!(sub_binary.len(), 10);
        assert_eq!(sub_binary.as_bytes(), &shared.as_bytes()[10..20]);
        assert_eq!(shared.ref_count(), 2);
    }

    #[test]
    fn sub_binary_writer_rejects_too_small_heap_slice() {
        let mut heap = [0_u64; SUB_BINARY_WORDS - 1];
        assert_eq!(write_sub_binary(&mut heap, Term::NIL, 0, 0), None);
    }
}
