//! Capability audit events and violation reporting hooks.
//!
//! Runtime native capability checks can publish audit events to an optional
//! sink. Denied checks can additionally notify a violation handler. Neither
//! facility persists events; embedders choose the in-memory collector or callback
//! behavior they need by wiring these traits into `NativeServices`.

use crate::atom::Atom;
use crate::native::{Capability, CapabilitySet};

/// Native operation whose capability was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilityOperation {
    /// Native module atom.
    pub module: Atom,
    /// Native function atom.
    pub function: Atom,
    /// Native arity.
    pub arity: u8,
}

/// Observable record for a runtime native capability check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAuditEvent {
    /// Calling process identifier.
    pub pid: u64,
    /// Required capability for the native operation.
    pub capability: Capability,
    /// Native operation being checked.
    pub operation: CapabilityOperation,
    /// Whether the process capability set granted the operation.
    pub granted: bool,
    /// Capabilities held by the process at check time.
    pub process_capabilities: CapabilitySet,
}

/// Sink for capability audit events.
pub trait CapabilityAuditSink: Send + Sync {
    /// Observe one capability check event.
    fn record(&self, event: CapabilityAuditEvent);
}

/// Callback invoked for denied capability checks.
pub trait ViolationHandler: Send + Sync {
    /// Handle one capability violation.
    fn on_violation(&self, event: CapabilityAuditEvent);
}

/// Violation handler that writes denied capability context to stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct StderrViolationHandler;

impl ViolationHandler for StderrViolationHandler {
    fn on_violation(&self, event: CapabilityAuditEvent) {
        eprintln!(
            "capability violation: pid={} operation={:?}:{:?}/{} required={:?} process_capabilities={:?}",
            event.pid,
            event.operation.module,
            event.operation.function,
            event.operation.arity,
            event.capability,
            event.process_capabilities,
        );
    }
}
