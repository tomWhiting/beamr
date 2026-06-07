//! Erlang Term Storage table registry and table implementations.

pub mod set;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::atom::Atom;
use crate::term::Term;

pub use set::EtsSet;

/// Runtime identifier for an ETS table.
pub type EtsTableId = u64;

/// ETS table kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EtsTableType {
    Set,
}

/// ETS access protection mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Protection {
    Public,
    Protected,
    Private,
}

/// Metadata common to all ETS table implementations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EtsTableMetadata {
    pub name: Option<Atom>,
    pub id: EtsTableId,
    pub table_type: EtsTableType,
    pub protection: Protection,
    pub owner: u64,
    /// One-based tuple element position used as the table key.
    pub keypos: usize,
}

/// Errors returned by ETS table operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EtsError {
    Badarg,
}

/// Common behavior for concrete ETS table storage implementations.
pub trait EtsTable: Send + Sync {
    fn metadata(&self) -> &EtsTableMetadata;
    fn insert(&self, tuple: Term) -> Result<(), EtsError>;
    fn lookup(&self, key: Term) -> Vec<Term>;
    fn delete_key(&self, key: Term) -> bool;
    fn tab2list(&self) -> Vec<Term>;
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
            metadata.id = self.next_table_id.fetch_add(1, Ordering::Relaxed);
        }
        let id = metadata.id;
        let name = metadata.name;
        let table: Arc<dyn EtsTable> = match metadata.table_type {
            EtsTableType::Set => Arc::new(EtsSet::new(metadata)),
        };
        self.tables.insert(id, table);
        if let Some(name) = name {
            self.names.insert(name, id);
        }
        id
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
        let Some((_, table)) = self.tables.remove(&id) else {
            return false;
        };
        if let Some(name) = table.metadata().name {
            self.names.remove(&name);
        }
        true
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
}
