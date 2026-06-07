//! Ordered ETS key wrapper for BEAM term ordering.

use std::{cmp::Ordering, fmt, sync::Arc};

use crate::{
    atom::AtomTable,
    term::{Term, compare},
};

/// A term key ordered according to Erlang term ordering.
///
/// `Ord` cannot accept an atom table parameter, so ETS table implementations
/// that need name-stable atom ordering construct keys with
/// [`TermKey::with_atom_table`]. That stores the table handle in the key, keeping
/// B-tree comparisons on the same BEAM term ordering used by VM-visible term
/// comparison.
#[derive(Clone)]
pub struct TermKey {
    term: Term,
    atom_table: Option<Arc<AtomTable>>,
}

impl TermKey {
    #[must_use]
    pub const fn new(term: Term) -> Self {
        Self {
            term,
            atom_table: None,
        }
    }

    #[must_use]
    pub fn with_atom_table(term: Term, atom_table: Arc<AtomTable>) -> Self {
        Self {
            term,
            atom_table: Some(atom_table),
        }
    }

    #[must_use]
    pub const fn term(&self) -> Term {
        self.term
    }
}

impl PartialEq for TermKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TermKey {}

impl fmt::Debug for TermKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TermKey")
            .field("term", &self.term)
            .field("has_atom_table", &self.atom_table.is_some())
            .finish()
    }
}

impl PartialOrd for TermKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TermKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.atom_table.as_ref().or(other.atom_table.as_ref()) {
            Some(atom_table) => compare::cmp(self.term, other.term, atom_table),
            None => self.term.cmp(&other.term),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoms_order_by_name_even_when_interned_in_reverse() {
        let atom_table = Arc::new(AtomTable::new());
        let atom_b = Term::atom(atom_table.intern("b"));
        let atom_a = Term::atom(atom_table.intern("a"));

        assert!(
            TermKey::with_atom_table(atom_a, Arc::clone(&atom_table))
                < TermKey::with_atom_table(atom_b, Arc::clone(&atom_table))
        );
    }

    #[test]
    fn numbers_sort_before_atoms() {
        let atom_table = Arc::new(AtomTable::new());
        let atom_a = Term::atom(atom_table.intern("a"));

        assert!(TermKey::new(Term::small_int(1)) < TermKey::with_atom_table(atom_a, atom_table));
    }

    #[test]
    fn debug_does_not_require_debug_atom_table() {
        let atom_table = Arc::new(AtomTable::new());
        let debug = format!(
            "{:?}",
            TermKey::with_atom_table(Term::atom(atom_table.intern("a")), atom_table)
        );

        assert!(debug.contains("TermKey"));
        assert!(debug.contains("has_atom_table: true"));
    }
}
