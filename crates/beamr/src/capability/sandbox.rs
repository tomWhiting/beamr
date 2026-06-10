//! Sandboxed execution environment presets.
//!
//! Sandboxes are fixed [`CapabilitySet`] presets used when creating a process.
//! They intentionally wrap the canonical native capability model rather than
//! introducing a separate authority taxonomy.

use crate::native::{Capability, CapabilitySet};

/// Named capability presets for sandboxed process execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Sandbox {
    /// No native capabilities. The process can only perform computation that
    /// does not cross the native capability boundary.
    Pure,
    /// File, network, environment, and other external I/O; no process spawn.
    Worker,
    /// Process-supervision authority; no direct external I/O.
    Supervisor,
    /// Every known capability.
    #[default]
    Unrestricted,
}

impl Sandbox {
    /// Returns the fixed capability set granted by this sandbox profile.
    #[must_use]
    pub fn capabilities(self) -> CapabilitySet {
        match self {
            Self::Pure => CapabilitySet::from_slice(&[]),
            Self::Worker => CapabilitySet::from_slice(&[Capability::ExternalIo]),
            Self::Supervisor => {
                CapabilitySet::from_slice(&[Capability::ProcessLocal, Capability::Spawn])
            }
            Self::Unrestricted => CapabilitySet::all(),
        }
    }
}

impl From<Sandbox> for CapabilitySet {
    fn from(sandbox: Sandbox) -> Self {
        sandbox.capabilities()
    }
}

#[cfg(test)]
mod tests {
    use super::Sandbox;
    use crate::native::{Capability, CapabilitySet};

    const ALL: [Capability; 6] = [
        Capability::Pure,
        Capability::ProcessLocal,
        Capability::Clock,
        Capability::Entropy,
        Capability::ExternalIo,
        Capability::Spawn,
    ];

    fn assert_exact_capabilities(sandbox: Sandbox, expected: &[Capability]) {
        let capabilities = sandbox.capabilities();
        for capability in ALL {
            assert_eq!(
                capabilities.contains(capability),
                expected.contains(&capability),
                "{sandbox:?} capability membership for {capability:?}"
            );
        }
        assert_eq!(capabilities.iter().count(), expected.len());
    }

    #[test]
    fn pure_sandbox_has_no_capabilities() {
        assert_exact_capabilities(Sandbox::Pure, &[]);
    }

    #[test]
    fn worker_sandbox_grants_external_io_without_spawn() {
        assert_exact_capabilities(Sandbox::Worker, &[Capability::ExternalIo]);
    }

    #[test]
    fn supervisor_sandbox_grants_process_supervision_without_io() {
        assert_exact_capabilities(
            Sandbox::Supervisor,
            &[Capability::ProcessLocal, Capability::Spawn],
        );
    }

    #[test]
    fn unrestricted_sandbox_grants_all_capabilities() {
        assert_eq!(Sandbox::Unrestricted.capabilities(), CapabilitySet::all());
        assert_exact_capabilities(Sandbox::Unrestricted, &ALL);
    }

    #[test]
    fn default_sandbox_is_unrestricted() {
        assert_eq!(Sandbox::default(), Sandbox::Unrestricted);
    }
}
