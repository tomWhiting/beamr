//! Erlang Term Storage registry, metadata, and lifecycle support.

pub mod bag;
pub mod copy;
pub mod ordered_set;
pub mod set;
pub mod table;
pub mod term_key;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::atom::Atom;
use crate::term::Term;
use crate::term::boxed::Tuple;

pub use bag::{EtsBag, EtsDuplicateBag};
pub use copy::{OwnedTerm, copy_term_to_ets, copy_term_to_heap};
pub use ordered_set::EtsOrderedSet;
pub use set::EtsSet;
pub use table::{
    AccessOp, EtsError, EtsTable, EtsTableId, EtsTableMetadata, EtsTableType, Protection,
};
pub use term_key::TermKey;

pub(crate) fn tuple_key(tuple_term: Term, keypos: usize) -> Result<Term, EtsError> {
    let tuple = Tuple::new(tuple_term).ok_or(EtsError::Badarg)?;
    let key_index = keypos.checked_sub(1).ok_or(EtsError::Badarg)?;
    tuple.get(key_index).ok_or(EtsError::Badarg)
}

/// Concurrent ETS table registry shared by schedulers.
pub struct EtsRegistry {
    next_table_id: AtomicU64,
    tables: DashMap<EtsTableId, Arc<dyn EtsTable>>,
    names: DashMap<Atom, EtsTableId>,
}

impl EtsRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_table_id: AtomicU64::new(1),
            tables: DashMap::new(),
            names: DashMap::new(),
        }
    }

    pub fn create_table(&self, mut metadata: EtsTableMetadata) -> EtsTableId {
        if metadata.id == 0 {
            metadata.id = self.allocate_table_id();
        } else {
            self.reserve_table_id(metadata.id);
        }
        let id = metadata.id;
        let name = metadata.name;
        let table: Arc<dyn EtsTable> = match metadata.table_type {
            EtsTableType::Set => Arc::new(EtsSet::new(metadata)),
            EtsTableType::OrderedSet => Arc::new(EtsOrderedSet::new(metadata)),
            EtsTableType::Bag => Arc::new(EtsBag::new(metadata)),
            EtsTableType::DuplicateBag => Arc::new(EtsDuplicateBag::new(metadata)),
        };
        if let Some(previous_table) = self.tables.insert(id, table)
            && let Some(previous_name) = previous_table.metadata().name
        {
            self.names
                .remove_if(&previous_name, |_, table_id| *table_id == id);
        }
        if let Some(name) = name {
            self.names.insert(name, id);
        }
        id
    }

    fn allocate_table_id(&self) -> EtsTableId {
        self.next_table_id.fetch_add(1, Ordering::Relaxed)
    }

    fn reserve_table_id(&self, id: EtsTableId) {
        let mut current = self.next_table_id.load(Ordering::Relaxed);
        while current <= id {
            match self.next_table_id.compare_exchange_weak(
                current,
                id.saturating_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(next) => current = next,
            }
        }
    }

    #[must_use]
    pub fn lookup_table(&self, id: EtsTableId) -> Option<Arc<dyn EtsTable>> {
        self.tables.get(&id).map(|entry| Arc::clone(entry.value()))
    }

    #[must_use]
    pub fn lookup_named_table(&self, name: Atom) -> Option<Arc<dyn EtsTable>> {
        let id = *self.names.get(&name)?;
        self.lookup_table(id)
    }

    pub fn delete_table(&self, id: EtsTableId) -> bool {
        let Some(table) = self.tables.remove(&id).map(|(_, v)| v) else {
            return false;
        };
        if let Some(name) = table.metadata().name {
            self.names.remove_if(&name, |_, table_id| *table_id == id);
        }
        true
    }

    pub fn delete_tables_owned_by(&self, owner_pid: u64) {
        let owned_ids: Vec<EtsTableId> = self
            .tables
            .iter()
            .filter(|entry| entry.value().metadata().owner == owner_pid)
            .map(|entry| *entry.key())
            .collect();
        for id in owned_ids {
            self.delete_table(id);
        }
    }

    #[must_use]
    pub fn lookup_table_by_name(&self, name: Atom) -> Option<EtsTableId> {
        self.names.get(&name).map(|entry| *entry.value())
    }

    #[must_use]
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }
}

impl Default for EtsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::Atom;
    use crate::term::{Term, boxed};

    fn metadata(table_type: EtsTableType) -> EtsTableMetadata {
        EtsTableMetadata {
            name: Some(Atom::OK),
            id: 0,
            table_type,
            protection: Protection::Protected,
            owner: 7,
            keypos: 1,
        }
    }

    #[test]
    fn registry_creates_set_table_and_round_trips_through_trait_object() {
        let registry = EtsRegistry::new();
        let table_id = registry.create_table(metadata(EtsTableType::Set));
        let table = registry.lookup_table(table_id).expect("set table exists");

        let mut tuple_heap = [0_u64; 3];
        let tuple =
            boxed::write_tuple(&mut tuple_heap, &[Term::atom(Atom::OK), Term::small_int(1)])
                .expect("tuple fits");

        table.insert(tuple).expect("tuple inserts");
        assert_eq!(table.lookup(Term::atom(Atom::OK)), vec![tuple]);
    }

    #[test]
    fn registry_does_not_reuse_explicit_table_ids_for_implicit_tables() {
        let registry = EtsRegistry::new();
        let mut explicit = metadata(EtsTableType::Set);
        explicit.id = 7;

        assert_eq!(registry.create_table(explicit), 7);

        let implicit_id = registry.create_table(EtsTableMetadata {
            name: None,
            ..metadata(EtsTableType::Set)
        });

        assert_ne!(implicit_id, 7);
        assert!(implicit_id > 7);
        assert!(registry.lookup_table(7).is_some());
        assert!(registry.lookup_table(implicit_id).is_some());
    }

    #[test]
    fn registry_keeps_reused_names_bound_to_latest_table_when_old_table_deleted() {
        let registry = EtsRegistry::new();
        let first_id = registry.create_table(metadata(EtsTableType::Set));
        let second_id = registry.create_table(metadata(EtsTableType::Set));

        assert_ne!(first_id, second_id);
        assert_eq!(
            registry
                .lookup_named_table(Atom::OK)
                .expect("latest name binding exists")
                .metadata()
                .id,
            second_id
        );

        assert!(registry.delete_table(first_id));
        assert_eq!(
            registry
                .lookup_named_table(Atom::OK)
                .expect("newer name binding survives old table deletion")
                .metadata()
                .id,
            second_id
        );
    }
}
