//! Lock-free concurrent intern map for atom storage.
//!
//! Supports concurrent reads from all scheduler threads and occasional
//! writes (during module loading). The first insert of a string assigns
//! a unique index; subsequent inserts of the same string return the
//! existing index. Common atoms (ok, error, true, false, nil, etc.)
//! are pre-registered at table creation.

pub(crate) fn _scaffold() {}
