//! Unified accessor for inline and off-heap binaries.

use crate::term::{Term, binary::Binary, boxed::ProcBin};

/// Borrowed binary accessor that hides whether bytes are inline or off-heap.
#[derive(Copy, Clone, Debug)]
pub enum BinaryRef {
    Inline(Binary),
    Refc(ProcBin),
}

impl BinaryRef {
    /// Creates a binary accessor for either inline binaries or ProcBins.
    pub fn new(term: Term) -> Option<Self> {
        if let Some(binary) = Binary::new(term) {
            return Some(Self::Inline(binary));
        }
        ProcBin::new(term).map(Self::Refc)
    }

    /// Returns binary bytes without copying the backing storage.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Inline(binary) => binary.as_bytes(),
            Self::Refc(proc_bin) => proc_bin.as_bytes(),
        }
    }

    /// Returns the number of bytes in this binary.
    pub fn len(&self) -> usize {
        match self {
            Self::Inline(binary) => binary.len(),
            Self::Refc(proc_bin) => proc_bin.len(),
        }
    }

    /// Returns true when this binary contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{
        binary::write_binary,
        shared_binary::{SharedBinary, write_proc_bin},
    };

    #[test]
    fn binary_ref_wraps_inline_binary() {
        let mut heap = [0_u64; 3];
        let term = write_binary(&mut heap, b"hello").expect("inline binary fits");
        let binary = BinaryRef::new(term).expect("binary ref");

        assert!(matches!(binary, BinaryRef::Inline(_)));
        assert_eq!(binary.len(), 5);
        assert_eq!(binary.as_bytes(), b"hello");
    }

    #[test]
    fn binary_ref_wraps_proc_bin() {
        let shared = SharedBinary::new(b"off-heap".to_vec());
        let mut heap = [0_u64; 3];
        let term = write_proc_bin(&mut heap, &shared).expect("proc bin fits");
        let binary = BinaryRef::new(term).expect("binary ref");

        assert!(matches!(binary, BinaryRef::Refc(_)));
        assert_eq!(binary.len(), 8);
        assert_eq!(binary.as_bytes(), b"off-heap");
    }

    #[test]
    fn binary_ref_rejects_non_binary_terms() {
        assert!(BinaryRef::new(Term::small_int(1)).is_none());
    }
}
