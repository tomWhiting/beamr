//! Global atom table — the shared vocabulary of the running system.
//!
//! Every name (module names, function names, atoms like `ok` and `error`)
//! is interned here exactly once. Atoms are compared by index, not by
//! string content, making pattern matching cheap.
pub mod table;
