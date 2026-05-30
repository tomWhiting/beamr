//! Global atom table — the shared vocabulary of the running system.
//!
//! Every name (module names, function names, atoms like `ok` and `error`)
//! is interned here exactly once. Atoms are compared by index, not by
//! string content, making pattern matching cheap.
pub mod table;

/// Interned atom handle.
///
/// Atoms are compared by their table index. The raw index is intentionally
/// private to the crate so public APIs can pass atoms around without exposing
/// the table representation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Atom(u32);

impl Atom {
    /// Common atom `ok`.
    pub const OK: Self = Self(0);

    /// Common atom `error`.
    pub const ERROR: Self = Self(1);

    /// Common atom `true`.
    pub const TRUE: Self = Self(2);

    /// Common atom `false`.
    pub const FALSE: Self = Self(3);

    /// Common atom `nil`.
    pub const NIL: Self = Self(4);

    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> u32 {
        self.0
    }
}
