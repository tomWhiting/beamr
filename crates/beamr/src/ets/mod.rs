//! Erlang Term Storage registry, metadata, and lifecycle support.

pub mod copy;
pub mod table;

pub use copy::{OwnedTerm, copy_term_to_ets, copy_term_to_heap};
pub use table::{
    AccessOp, EtsError, EtsTable, EtsTableId, EtsTableMetadata, EtsTableType, Protection,
};
