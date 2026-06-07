//! Hash-based ETS `set` table implementation.

use dashmap::DashMap;

use crate::ets::{EtsError, EtsTable, EtsTableMetadata};
use crate::term::{Term, boxed::Tuple, hash::EtsKey};

/// ETS `set` table backed by a concurrent hash map.
pub struct EtsSet {
    metadata: EtsTableMetadata,
    entries: DashMap<EtsKey, Term>,
}

impl EtsSet {
    #[must_use]
    pub fn new(metadata: EtsTableMetadata) -> Self {
        Self {
            metadata,
            entries: DashMap::new(),
        }
    }

    fn tuple_key(&self, tuple_term: Term) -> Result<Term, EtsError> {
        let tuple = Tuple::new(tuple_term).ok_or(EtsError::Badarg)?;
        let key_index = self
            .metadata
            .keypos
            .checked_sub(1)
            .ok_or(EtsError::Badarg)?;
        tuple.get(key_index).ok_or(EtsError::Badarg)
    }
}

impl EtsTable for EtsSet {
    fn metadata(&self) -> &EtsTableMetadata {
        &self.metadata
    }

    fn insert(&self, tuple: Term) -> Result<(), EtsError> {
        let key = self.tuple_key(tuple)?;
        self.entries.insert(EtsKey::new(key), tuple);
        Ok(())
    }

    fn lookup(&self, key: Term) -> Vec<Term> {
        self.entries
            .get(&EtsKey::new(key))
            .map_or_else(Vec::new, |entry| vec![*entry.value()])
    }

    fn delete_key(&self, key: Term) -> bool {
        self.entries.remove(&EtsKey::new(key)).is_some()
    }

    fn tab2list(&self) -> Vec<Term> {
        self.entries.iter().map(|entry| *entry.value()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::ets::{EtsTableId, EtsTableType, Protection};
    use crate::term::boxed;

    fn metadata(keypos: usize) -> EtsTableMetadata {
        EtsTableMetadata {
            name: None,
            id: EtsTableId::from(1_u64),
            table_type: EtsTableType::Set,
            protection: Protection::Protected,
            owner: 1,
            keypos,
        }
    }

    #[test]
    fn insert_lookup_and_overwrite_by_unique_key() {
        let table = EtsSet::new(metadata(1));
        let mut first_heap = [0_u64; 3];
        let mut second_heap = [0_u64; 3];
        let first =
            boxed::write_tuple(&mut first_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
                .expect("first tuple fits");
        let second = boxed::write_tuple(
            &mut second_heap,
            &[Term::atom(Atom::OK), Term::small_int(2)],
        )
        .expect("second tuple fits");

        table.insert(first).expect("first insert succeeds");
        assert_eq!(table.lookup(Term::atom(Atom::OK)), vec![first]);

        table.insert(second).expect("second insert succeeds");
        assert_eq!(table.lookup(Term::atom(Atom::OK)), vec![second]);
    }

    #[test]
    fn non_tuple_and_out_of_range_keypos_are_badarg() {
        let table = EtsSet::new(metadata(1));
        assert_eq!(table.insert(Term::small_int(1)), Err(EtsError::Badarg));

        let out_of_range = EtsSet::new(metadata(3));
        let mut heap = [0_u64; 3];
        let tuple = boxed::write_tuple(&mut heap, &[Term::atom(Atom::OK), Term::small_int(1)])
            .expect("tuple fits");
        assert_eq!(out_of_range.insert(tuple), Err(EtsError::Badarg));
    }

    #[test]
    fn delete_key_reports_existence_and_tab2list_returns_all_tuples() {
        let table = EtsSet::new(metadata(1));
        let mut first_heap = [0_u64; 3];
        let mut second_heap = [0_u64; 3];
        let first =
            boxed::write_tuple(&mut first_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
                .expect("first tuple fits");
        let second = boxed::write_tuple(
            &mut second_heap,
            &[Term::atom(Atom::ERROR), Term::small_int(2)],
        )
        .expect("second tuple fits");
        table.insert(first).expect("first insert succeeds");
        table.insert(second).expect("second insert succeeds");

        let mut listed = table.tab2list();
        listed.sort();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(listed, expected);

        assert!(table.delete_key(Term::atom(Atom::OK)));
        assert!(!table.delete_key(Term::atom(Atom::OK)));
        assert_eq!(table.lookup(Term::atom(Atom::OK)), Vec::<Term>::new());
        assert_eq!(table.lookup(Term::atom(Atom::ERROR)), vec![second]);
    }

    #[test]
    fn keypos_is_one_based() {
        let table = EtsSet::new(metadata(2));
        let mut heap = [0_u64; 3];
        let tuple = boxed::write_tuple(&mut heap, &[Term::atom(Atom::OK), Term::small_int(99)])
            .expect("tuple fits");
        table.insert(tuple).expect("insert succeeds");

        assert_eq!(table.lookup(Term::small_int(99)), vec![tuple]);
        assert_eq!(table.lookup(Term::atom(Atom::OK)), Vec::<Term>::new());
    }
}
