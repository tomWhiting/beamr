//! Capability metadata and policies for native imports.
//!
//! Native functions are tagged with the authority they need. Import resolution
//! checks those tags once, then either binds the real native entry or an
//! explicit `ResolvedImportTarget::Denied` placeholder that raises a rich
//! `undef` when called.

/// Authority required by a native function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// No side effects, no external I/O, and no nondeterminism.
    ///
    /// Arithmetic, comparisons, type checks, and pure collection operations are
    /// in this class.
    Pure,
    /// Mutates or reads only the calling process's private runtime state.
    ProcessLocal,
    /// Reads wall-clock time or otherwise waits on timers.
    Clock,
    /// Consumes randomness or cryptographic entropy.
    Entropy,
    /// Talks to the outside world: shell commands, filesystem, network, or the
    /// process environment.
    ExternalIo,
    /// Creates child processes.
    Spawn,
}

impl Capability {
    const ALL: [Self; 6] = [
        Self::Pure,
        Self::ProcessLocal,
        Self::Clock,
        Self::Entropy,
        Self::ExternalIo,
        Self::Spawn,
    ];
}

/// Policy consulted by import resolution before binding a native function.
pub trait CapabilityPolicy: Send + Sync {
    /// Returns true when `capability` is granted by this policy.
    fn is_granted(&self, capability: Capability) -> bool;
}

/// Deny-by-default policy: only pure natives are granted.
#[derive(Debug, Clone, Copy, Default)]
pub struct LeastAuthorityPolicy;

impl CapabilityPolicy for LeastAuthorityPolicy {
    fn is_granted(&self, capability: Capability) -> bool {
        matches!(capability, Capability::Pure | Capability::ProcessLocal)
    }
}

/// Backwards-compatible policy for trusted embedders: all natives are granted.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllCapabilitiesPolicy;

impl CapabilityPolicy for AllCapabilitiesPolicy {
    fn is_granted(&self, _capability: Capability) -> bool {
        true
    }
}

/// Small custom capability policy built from an explicit allow-list.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CapabilitySet {
    grants: Vec<Capability>,
}

impl CapabilitySet {
    /// Builds a backwards-compatible set containing every known capability.
    #[must_use]
    pub fn all() -> Self {
        Self::from_slice(&Capability::ALL)
    }

    /// Builds a custom policy from a slice of granted capabilities.
    #[must_use]
    pub fn from_slice(capabilities: &[Capability]) -> Self {
        let mut grants = Vec::new();
        for capability in capabilities {
            if !grants.contains(capability) {
                grants.push(*capability);
            }
        }
        Self { grants }
    }

    /// Returns true when `capability` is in this set.
    #[must_use]
    pub fn grants(&self, capability: Capability) -> bool {
        self.grants.contains(&capability)
    }

    /// Returns true when `capability` is in this set.
    #[must_use]
    pub fn contains(&self, capability: Capability) -> bool {
        self.grants(capability)
    }

    /// Returns true when every capability in `self` is present in `other`.
    #[must_use]
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.grants
            .iter()
            .all(|capability| other.contains(*capability))
    }

    /// Iterates over granted capabilities.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.grants.iter().copied()
    }
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self::all()
    }
}

impl CapabilityPolicy for CapabilitySet {
    fn is_granted(&self, capability: Capability) -> bool {
        self.grants(capability)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AllCapabilitiesPolicy, Capability, CapabilityPolicy, CapabilitySet, LeastAuthorityPolicy,
    };

    #[test]
    fn least_authority_grants_only_pure() {
        let policy = LeastAuthorityPolicy;
        assert!(policy.is_granted(Capability::Pure));
        assert!(policy.is_granted(Capability::ProcessLocal));
        assert!(!policy.is_granted(Capability::Clock));
        assert!(!policy.is_granted(Capability::Entropy));
        assert!(!policy.is_granted(Capability::ExternalIo));
        assert!(!policy.is_granted(Capability::Spawn));
    }

    #[test]
    fn all_capabilities_grants_everything() {
        let policy = AllCapabilitiesPolicy;
        assert!(policy.is_granted(Capability::Pure));
        assert!(policy.is_granted(Capability::ProcessLocal));
        assert!(policy.is_granted(Capability::Clock));
        assert!(policy.is_granted(Capability::Entropy));
        assert!(policy.is_granted(Capability::ExternalIo));
        assert!(policy.is_granted(Capability::Spawn));
    }

    #[test]
    fn capability_set_grants_exact_members() {
        let policy = CapabilitySet::from_slice(&[Capability::Pure, Capability::ProcessLocal]);
        assert!(policy.grants(Capability::Pure));
        assert!(policy.grants(Capability::ProcessLocal));
        assert!(!policy.grants(Capability::Clock));
        assert!(!policy.grants(Capability::Entropy));
        assert!(!policy.grants(Capability::ExternalIo));
        assert!(!policy.grants(Capability::Spawn));
    }
}
