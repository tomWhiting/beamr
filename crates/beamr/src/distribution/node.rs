use crate::atom::Atom;

/// Default node name used by BEAM in non-distributed mode.
pub const DEFAULT_NODE_NAME: &str = "nonode@nohost";

/// Named BEAM node identity for the lifetime of a VM instance.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Node {
    pub name: Atom,
    pub creation: u32,
}

impl Node {
    /// Creates a node identity from an interned node name and restart creation.
    #[must_use]
    pub const fn new(name: Atom, creation: u32) -> Self {
        Self { name, creation }
    }

    /// Returns true when `other` is the same node name and creation.
    #[must_use]
    pub const fn is_local(&self, other: &Node) -> bool {
        self.name.index() == other.name.index() && self.creation == other.creation
    }
}
