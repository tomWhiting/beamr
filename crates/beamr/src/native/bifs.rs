//! Built-in function implementations.
//!
//! The set of BIFs is demand-driven: only functions that appear in
//! the loader's unresolved-import report are implemented. Common
//! BIFs include arithmetic (+, -, *, div, rem), comparison (==, /=,
//! <, >), type checks (is_atom, is_integer, is_list, is_tuple),
//! and process operations (spawn, self, send).

pub(crate) fn _scaffold() {}
