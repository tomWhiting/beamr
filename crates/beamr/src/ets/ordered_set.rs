//! ETS `ordered_set` table implementation.

use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    atom::AtomTable,
    ets::{EtsError, EtsTable, EtsTableMetadata},
    term::{Term, boxed::Tuple},
};

use super::TermKey;

/// B-tree backed ETS `ordered_set` table.
pub struct EtsOrderedSet {
    metadata: EtsTableMetadata,
    atom_table: Arc<AtomTable>,
    rows: Mutex<BTreeMap<TermKey, Term>>,
}

impl EtsOrderedSet {
    #[must_use]
    pub fn new(metadata: EtsTableMetadata) -> Self {
        Self::with_atom_table(metadata, Arc::new(AtomTable::with_common_atoms()))
    }

    #[must_use]
    pub fn with_atom_table(metadata: EtsTableMetadata, atom_table: Arc<AtomTable>) -> Self {
        Self {
            metadata,
            atom_table,
            rows: Mutex::new(BTreeMap::new()),
        }
    }

    /// Returns the smallest key in the table.
    #[must_use]
    pub fn first(&self) -> Option<Term> {
        let rows = self.rows();
        rows.keys().next().map(|key| key.term())
    }

    /// Returns the largest key in the table.
    #[must_use]
    pub fn last(&self) -> Option<Term> {
        let rows = self.rows();
        rows.keys().next_back().map(|key| key.term())
    }

    /// Returns the key immediately after `key`, even when `key` is absent.
    #[must_use]
    pub fn next(&self, key: Term) -> Option<Term> {
        let rows = self.rows();
        rows.range((Excluded(self.key(key)), Unbounded))
            .next()
            .map(|(key, _tuple)| key.term())
    }

    /// Returns the key immediately before `key`, even when `key` is absent.
    #[must_use]
    pub fn prev(&self, key: Term) -> Option<Term> {
        let rows = self.rows();
        rows.range((Unbounded, Excluded(self.key(key))))
            .next_back()
            .map(|(key, _tuple)| key.term())
    }

    fn key(&self, term: Term) -> TermKey {
        TermKey::with_atom_table(term, Arc::clone(&self.atom_table))
    }

    fn tuple_key(&self, tuple: Term) -> Result<TermKey, EtsError> {
        let tuple = Tuple::new(tuple).ok_or(EtsError::Badarg)?;
        let index = self
            .metadata
            .keypos
            .checked_sub(1)
            .ok_or(EtsError::Badarg)?;
        let key = tuple.get(index).ok_or(EtsError::Badarg)?;
        Ok(self.key(key))
    }

    fn rows(&self) -> MutexGuard<'_, BTreeMap<TermKey, Term>> {
        match self.rows.lock() {
            Ok(rows) => rows,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl EtsTable for EtsOrderedSet {
    fn metadata(&self) -> &EtsTableMetadata {
        &self.metadata
    }

    fn insert(&self, tuple: Term) -> Result<(), EtsError> {
        let key = self.tuple_key(tuple)?;
        let mut rows = self.rows();
        rows.insert(key, tuple);
        Ok(())
    }

    fn lookup(&self, key: Term) -> Vec<Term> {
        let rows = self.rows();
        rows.get(&self.key(key)).copied().into_iter().collect()
    }

    fn delete_key(&self, key: Term) -> bool {
        let mut rows = self.rows();
        rows.remove(&self.key(key)).is_some()
    }

    fn tab2list(&self) -> Vec<Term> {
        let rows = self.rows();
        rows.values().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        atom::AtomTable,
        ets::{EtsTable, EtsTableMetadata},
        term::{Term, boxed::write_tuple},
    };

    use super::EtsOrderedSet;

    fn table(atom_table: Arc<AtomTable>) -> EtsOrderedSet {
        EtsOrderedSet::with_atom_table(EtsTableMetadata::ordered_set(1, 42), atom_table)
    }

    fn tuple(heap: &mut [u64; 3], key: Term, value: Term) -> Term {
        match write_tuple(&mut heap[..], &[key, value]) {
            Some(term) => term,
            None => unreachable!("test heap has room for a 2-tuple"),
        }
    }

    #[test]
    fn tab2list_returns_tuples_in_key_order() {
        let atom_table = Arc::new(AtomTable::new());
        let table = table(Arc::clone(&atom_table));
        let mut heap_c = Box::new([0; 3]);
        let mut heap_a = Box::new([0; 3]);
        let mut heap_b = Box::new([0; 3]);
        let tuple_c = tuple(
            &mut heap_c,
            Term::small_int(3),
            Term::atom(atom_table.intern("c")),
        );
        let tuple_a = tuple(
            &mut heap_a,
            Term::small_int(1),
            Term::atom(atom_table.intern("a")),
        );
        let tuple_b = tuple(
            &mut heap_b,
            Term::small_int(2),
            Term::atom(atom_table.intern("b")),
        );

        assert_eq!(table.insert(tuple_c), Ok(()));
        assert_eq!(table.insert(tuple_a), Ok(()));
        assert_eq!(table.insert(tuple_b), Ok(()));

        assert_eq!(table.tab2list(), vec![tuple_a, tuple_b, tuple_c]);
    }

    #[test]
    fn lookup_insert_overwrites_and_delete_key_uses_ordered_key() {
        let atom_table = Arc::new(AtomTable::new());
        let table = table(Arc::clone(&atom_table));
        let mut first_heap = Box::new([0; 3]);
        let mut replacement_heap = Box::new([0; 3]);
        let first = tuple(
            &mut first_heap,
            Term::small_int(1),
            Term::atom(atom_table.intern("first")),
        );
        let replacement = tuple(
            &mut replacement_heap,
            Term::small_int(1),
            Term::atom(atom_table.intern("replacement")),
        );

        assert_eq!(table.insert(first), Ok(()));
        assert_eq!(table.insert(replacement), Ok(()));
        assert_eq!(table.lookup(Term::small_int(1)), vec![replacement]);
        assert!(table.delete_key(Term::small_int(1)));
        assert!(table.lookup(Term::small_int(1)).is_empty());
        assert!(!table.delete_key(Term::small_int(1)));
    }

    #[test]
    fn insert_rejects_non_tuple_and_missing_keypos() {
        let atom_table = Arc::new(AtomTable::new());
        let mut metadata = EtsTableMetadata::ordered_set(1, 42);
        metadata.keypos = 3;
        let table = EtsOrderedSet::with_atom_table(metadata, Arc::clone(&atom_table));
        let mut heap = Box::new([0; 3]);
        let tuple = tuple(
            &mut heap,
            Term::small_int(1),
            Term::atom(atom_table.intern("value")),
        );

        assert_eq!(
            table.insert(Term::small_int(1)),
            Err(crate::ets::EtsError::Badarg)
        );
        assert_eq!(table.insert(tuple), Err(crate::ets::EtsError::Badarg));
    }

    #[test]
    fn ordered_traversal_returns_neighboring_keys_and_boundaries() {
        let atom_table = Arc::new(AtomTable::new());
        let table = table(Arc::clone(&atom_table));
        let mut heap_one = Box::new([0; 3]);
        let mut heap_two = Box::new([0; 3]);
        let mut heap_three = Box::new([0; 3]);
        let one = Term::small_int(1);
        let two = Term::small_int(2);
        let three = Term::small_int(3);

        assert_eq!(
            table.insert(tuple(
                &mut heap_three,
                three,
                Term::atom(atom_table.intern("c")),
            )),
            Ok(())
        );
        assert_eq!(
            table.insert(tuple(
                &mut heap_one,
                one,
                Term::atom(atom_table.intern("a")),
            )),
            Ok(())
        );
        assert_eq!(
            table.insert(tuple(
                &mut heap_two,
                two,
                Term::atom(atom_table.intern("b")),
            )),
            Ok(())
        );

        assert_eq!(table.first(), Some(one));
        assert_eq!(table.next(one), Some(two));
        assert_eq!(table.next(Term::small_int(0)), Some(one));
        assert_eq!(table.next(three), None);
        assert_eq!(table.last(), Some(three));
        assert_eq!(table.prev(three), Some(two));
        assert_eq!(table.prev(Term::small_int(4)), Some(three));
        assert_eq!(table.prev(one), None);
    }
}
