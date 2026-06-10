//! Arena allocator for ETS match-spec result terms.
//!
//! [`MatchArena`] owns every heap block used for constructed match-spec body
//! results. Terms allocated from the arena contain raw pointers into these
//! blocks and remain valid until the arena is dropped.

use crate::atom::Atom;
use crate::ets::match_spec::TermAllocator;
use crate::term::Term;
use crate::term::boxed::{write_cons, write_tuple};

/// Owns match-spec result allocations for one match/select operation.
#[derive(Debug, Default)]
pub struct MatchArena {
    blocks: Vec<Box<[u64]>>,
}

impl MatchArena {
    /// Create an empty match arena.
    #[must_use]
    pub const fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    /// Number of heap blocks currently owned by the arena.
    #[must_use]
    pub fn allocation_count(&self) -> usize {
        self.blocks.len()
    }
}

impl TermAllocator for MatchArena {
    fn alloc_tuple(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let mut words = vec![0_u64; 1 + elements.len()].into_boxed_slice();
        let term = write_tuple(&mut words, elements).ok_or_else(|| Term::atom(Atom::BADARG))?;
        self.blocks.push(words);
        Ok(term)
    }

    fn alloc_list(&mut self, elements: &[Term]) -> Result<Term, Term> {
        let mut tail = Term::NIL;
        for element in elements.iter().rev().copied() {
            let mut words = vec![0_u64; 2].into_boxed_slice();
            tail = write_cons(&mut words, element, tail).ok_or_else(|| Term::atom(Atom::BADARG))?;
            self.blocks.push(words);
        }
        Ok(tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::{Cons, Tuple};

    #[test]
    fn alloc_tuple_produces_valid_tuple_term() {
        let mut arena = MatchArena::new();
        let term = arena
            .alloc_tuple(&[Term::small_int(1), Term::small_int(2)])
            .expect("tuple allocation succeeds");

        let tuple = Tuple::new(term).expect("arena term is a tuple");
        assert_eq!(tuple.arity(), 2);
        assert_eq!(tuple.get(0), Some(Term::small_int(1)));
        assert_eq!(tuple.get(1), Some(Term::small_int(2)));
        assert_eq!(arena.allocation_count(), 1);
    }

    #[test]
    fn alloc_list_produces_valid_list_term() {
        let mut arena = MatchArena::new();
        let term = arena
            .alloc_list(&[Term::small_int(1), Term::small_int(2)])
            .expect("list allocation succeeds");

        let first = Cons::new(term).expect("first cons cell");
        assert_eq!(first.head(), Term::small_int(1));
        let second = Cons::new(first.tail()).expect("second cons cell");
        assert_eq!(second.head(), Term::small_int(2));
        assert_eq!(second.tail(), Term::NIL);
        assert_eq!(arena.allocation_count(), 2);
    }

    #[test]
    fn empty_list_allocates_no_blocks() {
        let mut arena = MatchArena::new();
        let term = arena
            .alloc_list(&[])
            .expect("empty list allocation succeeds");

        assert_eq!(term, Term::NIL);
        assert_eq!(arena.allocation_count(), 0);
    }
}
