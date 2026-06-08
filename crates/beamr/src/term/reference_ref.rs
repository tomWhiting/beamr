//! Unified accessor for local and remote reference terms.

use crate::{
    atom::Atom,
    term::{
        Term,
        boxed::{ExternalReference, Reference},
    },
};

/// Borrowed reference accessor hiding compact-local versus boxed-remote layout.
#[derive(Copy, Clone, Debug)]
pub enum ReferenceRef {
    Local(Reference),
    Remote(ExternalReference),
}

impl ReferenceRef {
    /// Creates a reference accessor for local or remote boxed references.
    pub fn new(term: Term) -> Option<Self> {
        if let Some(reference) = Reference::new(term) {
            return Some(Self::Local(reference));
        }
        ExternalReference::new(term).map(Self::Remote)
    }

    /// Returns the reference id component.
    #[must_use]
    pub fn id(self) -> u64 {
        match self {
            Self::Local(reference) => reference.id(),
            Self::Remote(reference) => reference.id(),
        }
    }

    /// Returns the embedded remote node atom, or `None` for local references.
    #[must_use]
    pub fn node(self) -> Option<Atom> {
        match self {
            Self::Local(_) => None,
            Self::Remote(reference) => reference.node(),
        }
    }

    /// Returns true when the reference uses the compact local representation.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::{write_external_reference, write_reference};

    #[test]
    fn reference_ref_wraps_local_reference() {
        let mut heap = [0_u64; 2];
        let term = write_reference(&mut heap, 12).expect("reference fits");
        let reference = ReferenceRef::new(term).expect("local reference");

        assert!(reference.is_local());
        assert_eq!(reference.id(), 12);
        assert_eq!(reference.node(), None);
    }

    #[test]
    fn reference_ref_wraps_remote_reference() {
        let mut heap = [0_u64; 3];
        let term = write_external_reference(&mut heap, Atom::OK, 99).expect("external ref fits");
        let reference = ReferenceRef::new(term).expect("remote reference");

        assert!(!reference.is_local());
        assert_eq!(reference.id(), 99);
        assert_eq!(reference.node(), Some(Atom::OK));
    }
}
