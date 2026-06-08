//! ETS `ordered_set` table implementation.

use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    sync::{Arc, Mutex, RwLock},
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
    rows: OrderedSetRows,
}

enum OrderedSetRows {
    Mutex(Mutex<BTreeMap<TermKey, Term>>),
    RwLock(RwLock<BTreeMap<TermKey, Term>>),
}

impl EtsOrderedSet {
    #[must_use]
    pub fn new(metadata: EtsTableMetadata) -> Self {
        Self::with_atom_table(metadata, Arc::new(AtomTable::with_common_atoms()))
    }

    #[must_use]
    pub fn with_atom_table(metadata: EtsTableMetadata, atom_table: Arc<AtomTable>) -> Self {
        if metadata.write_concurrency {
            eprintln!(
                "ets: ordered_set write_concurrency is ignored to preserve global key ordering"
            );
        }
        let rows = if metadata.read_concurrency {
            OrderedSetRows::RwLock(RwLock::new(BTreeMap::new()))
        } else {
            OrderedSetRows::Mutex(Mutex::new(BTreeMap::new()))
        };
        Self {
            metadata,
            atom_table,
            rows,
        }
    }

    /// Returns the smallest key in the table.
    #[must_use]
    pub fn first(&self) -> Option<Term> {
        self.with_rows(|rows| rows.keys().next().map(|key| key.term()))
    }

    /// Returns the largest key in the table.
    #[must_use]
    pub fn last(&self) -> Option<Term> {
        self.with_rows(|rows| rows.keys().next_back().map(|key| key.term()))
    }

    /// Returns the key immediately after `key`, even when `key` is absent.
    #[must_use]
    pub fn next(&self, key: Term) -> Option<Term> {
        let key = self.key(key);
        self.with_rows(|rows| {
            rows.range((Excluded(key), Unbounded))
                .next()
                .map(|(key, _tuple)| key.term())
        })
    }

    /// Returns the key immediately before `key`, even when `key` is absent.
    #[must_use]
    pub fn prev(&self, key: Term) -> Option<Term> {
        let key = self.key(key);
        self.with_rows(|rows| {
            rows.range((Unbounded, Excluded(key)))
                .next_back()
                .map(|(key, _tuple)| key.term())
        })
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

    fn with_rows<R>(&self, read: impl FnOnce(&BTreeMap<TermKey, Term>) -> R) -> R {
        match &self.rows {
            OrderedSetRows::Mutex(rows) => match rows.lock() {
                Ok(rows) => read(&rows),
                Err(poisoned) => read(&poisoned.into_inner()),
            },
            OrderedSetRows::RwLock(rows) => match rows.read() {
                Ok(rows) => read(&rows),
                Err(poisoned) => read(&poisoned.into_inner()),
            },
        }
    }

    fn with_rows_mut<R>(&self, write: impl FnOnce(&mut BTreeMap<TermKey, Term>) -> R) -> R {
        match &self.rows {
            OrderedSetRows::Mutex(rows) => match rows.lock() {
                Ok(mut rows) => write(&mut rows),
                Err(poisoned) => write(&mut poisoned.into_inner()),
            },
            OrderedSetRows::RwLock(rows) => match rows.write() {
                Ok(mut rows) => write(&mut rows),
                Err(poisoned) => write(&mut poisoned.into_inner()),
            },
        }
    }
}

impl EtsTable for EtsOrderedSet {
    fn metadata(&self) -> &EtsTableMetadata {
        &self.metadata
    }

    fn insert(&self, tuple: Term) -> Result<(), EtsError> {
        let key = self.tuple_key(tuple)?;
        self.with_rows_mut(|rows| rows.insert(key, tuple));
        Ok(())
    }

    fn lookup(&self, key: Term) -> Vec<Term> {
        let key = self.key(key);
        self.with_rows(|rows| rows.get(&key).copied().into_iter().collect())
    }

    fn delete_key(&self, key: Term) -> bool {
        let key = self.key(key);
        self.with_rows_mut(|rows| rows.remove(&key).is_some())
    }

    fn tab2list(&self) -> Vec<Term> {
        self.with_rows(|rows| rows.values().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use crate::{
        atom::AtomTable,
        ets::{EtsTable, EtsTableMetadata, EtsTableType, Protection},
        term::{Term, boxed::write_tuple},
    };

    use super::EtsOrderedSet;

    fn table(atom_table: Arc<AtomTable>) -> EtsOrderedSet {
        EtsOrderedSet::with_atom_table(
            EtsTableMetadata::new(None, 1, EtsTableType::OrderedSet, Protection::Protected, 42),
            atom_table,
        )
    }

    fn table_with_metadata(
        metadata: EtsTableMetadata,
        atom_table: Arc<AtomTable>,
    ) -> EtsOrderedSet {
        EtsOrderedSet::with_atom_table(metadata, atom_table)
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
        let mut metadata =
            EtsTableMetadata::new(None, 1, EtsTableType::OrderedSet, Protection::Protected, 42);
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

    #[test]
    fn read_concurrency_uses_shared_read_lock_for_concurrent_lookups() {
        let atom_table = Arc::new(AtomTable::new());
        let mut metadata =
            EtsTableMetadata::new(None, 1, EtsTableType::OrderedSet, Protection::Protected, 42);
        metadata.read_concurrency = true;
        let table = Arc::new(EtsOrderedSet::with_atom_table(
            metadata,
            Arc::clone(&atom_table),
        ));
        let mut heap = Box::new([0; 3]);
        let row = tuple(
            &mut heap,
            Term::small_int(1),
            Term::atom(atom_table.intern("value")),
        );
        assert_eq!(table.insert(row), Ok(()));

        let handles = (0..8)
            .map(|_| {
                let table = Arc::clone(&table);
                thread::spawn(move || {
                    for _ in 0..128 {
                        assert_eq!(table.lookup(Term::small_int(1)), vec![row]);
                    }
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().expect("reader thread completes");
        }
    }

    #[test]
    fn read_and_write_concurrency_combination_preserves_ordered_set_semantics() {
        let atom_table = Arc::new(AtomTable::new());
        let mut metadata =
            EtsTableMetadata::new(None, 1, EtsTableType::OrderedSet, Protection::Protected, 42);
        metadata.read_concurrency = true;
        metadata.write_concurrency = true;
        let table = table_with_metadata(metadata, Arc::clone(&atom_table));
        let mut heap_three = Box::new([0; 3]);
        let mut heap_one = Box::new([0; 3]);
        let mut heap_two = Box::new([0; 3]);
        let one = Term::small_int(1);
        let two = Term::small_int(2);
        let three = Term::small_int(3);
        let row_three = tuple(&mut heap_three, three, Term::atom(atom_table.intern("c")));
        let row_one = tuple(&mut heap_one, one, Term::atom(atom_table.intern("a")));
        let row_two = tuple(&mut heap_two, two, Term::atom(atom_table.intern("b")));

        assert_eq!(table.insert(row_three), Ok(()));
        assert_eq!(table.insert(row_one), Ok(()));
        assert_eq!(table.insert(row_two), Ok(()));

        assert_eq!(table.first(), Some(one));
        assert_eq!(table.next(one), Some(two));
        assert_eq!(table.next(two), Some(three));
        assert_eq!(table.next(three), None);
        assert_eq!(table.last(), Some(three));
        assert_eq!(table.prev(three), Some(two));
        assert_eq!(table.prev(two), Some(one));
        assert_eq!(table.prev(one), None);
        assert_eq!(table.tab2list(), vec![row_one, row_two, row_three]);
    }

    #[test]
    fn write_concurrency_without_read_concurrency_keeps_single_ordered_map() {
        let atom_table = Arc::new(AtomTable::new());
        let mut metadata =
            EtsTableMetadata::new(None, 1, EtsTableType::OrderedSet, Protection::Protected, 42);
        metadata.write_concurrency = true;
        let table = table_with_metadata(metadata, Arc::clone(&atom_table));
        let mut heap_one = Box::new([0; 3]);
        let mut heap_two = Box::new([0; 3]);

        assert_eq!(
            table.insert(tuple(
                &mut heap_two,
                Term::small_int(2),
                Term::small_int(20)
            )),
            Ok(())
        );
        assert_eq!(
            table.insert(tuple(
                &mut heap_one,
                Term::small_int(1),
                Term::small_int(10)
            )),
            Ok(())
        );

        assert_eq!(table.first(), Some(Term::small_int(1)));
        assert_eq!(table.next(Term::small_int(1)), Some(Term::small_int(2)));
        assert_eq!(table.last(), Some(Term::small_int(2)));
    }
}
