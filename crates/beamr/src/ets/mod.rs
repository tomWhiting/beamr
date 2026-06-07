//! Erlang Term Storage table primitives.

pub mod ordered_set;
pub mod term_key;

pub use ordered_set::EtsOrderedSet;
pub use term_key::TermKey;

use crate::{atom::Atom, term::Term};

/// Stable identifier for an ETS table.
pub type EtsTableId = u64;

/// Supported ETS table kinds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EtsTableType {
    Set,
    OrderedSet,
}

/// ETS access protection mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Protection {
    Public,
    Protected,
    Private,
}

/// Common metadata carried by ETS table implementations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EtsTableMetadata {
    pub name: Option<Atom>,
    pub id: EtsTableId,
    pub table_type: EtsTableType,
    pub protection: Protection,
    pub owner: u64,
    /// One-based tuple position used as the table key.
    pub keypos: usize,
}

impl EtsTableMetadata {
    /// Creates metadata for an `ordered_set` ETS table.
    #[must_use]
    pub const fn ordered_set(id: EtsTableId, owner: u64) -> Self {
        Self {
            name: None,
            id,
            table_type: EtsTableType::OrderedSet,
            protection: Protection::Protected,
            owner,
            keypos: 1,
        }
    }
}

/// Errors produced by ETS table operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EtsError {
    Badarg,
}

/// Shared interface implemented by concrete ETS table types.
pub trait EtsTable {
    fn metadata(&self) -> &EtsTableMetadata;
    fn insert(&self, tuple: Term) -> Result<(), EtsError>;
    fn lookup(&self, key: Term) -> Vec<Term>;
    fn delete_key(&self, key: Term) -> bool;
    fn tab2list(&self) -> Vec<Term>;
}
