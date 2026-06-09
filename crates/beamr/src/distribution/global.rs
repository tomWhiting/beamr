//! Simplified distributed global name registry.
//!
//! This module models the cluster-wide `global` name table used by the native
//! `global:*_name` BIFs. It deliberately does not implement OTP's full global
//! lock protocol; instead, connected nodes can exchange registry snapshots and
//! deterministically merge conflicts. When the same name appears on two nodes,
//! the entry owned by the lexicographically lower node name wins.

use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::atom::{Atom, AtomTable};
use crate::distribution::Node;
use crate::term::Term;

/// PID identity stored by the global name registry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GlobalPid {
    /// Node that owns the PID.
    pub node: Atom,
    /// Numeric process id component.
    pub pid_number: u64,
    /// PID serial component. Local immediate PIDs use zero.
    pub serial: u64,
}

impl GlobalPid {
    /// Create a global PID identity.
    #[must_use]
    pub const fn new(node: Atom, pid_number: u64, serial: u64) -> Self {
        Self {
            node,
            pid_number,
            serial,
        }
    }
}

/// A registered global name entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GlobalNameEntry {
    /// Registered atom name.
    pub name: Atom,
    /// PID identity for the process registered under `name`.
    pub pid: GlobalPid,
    /// Optional BEAM resolver function from `global:register_name/3`.
    pub resolver: Option<Term>,
}

impl GlobalNameEntry {
    /// Create a registry entry.
    #[must_use]
    pub const fn new(name: Atom, pid: GlobalPid, resolver: Option<Term>) -> Self {
        Self {
            name,
            pid,
            resolver,
        }
    }
}

/// Notification emitted when conflict resolution discards a registration.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GlobalNameNotification {
    /// Name that conflicted.
    pub name: Atom,
    /// PID that lost the conflict.
    pub loser: GlobalPid,
    /// PID that survived the conflict.
    pub winner: GlobalPid,
    /// Resolver function associated with one of the conflicting entries, if any.
    pub resolver: Option<Term>,
}

/// Outcome of inserting or merging a global name entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GlobalRegistrationOutcome {
    /// Name was absent and has been inserted.
    Inserted,
    /// Name existed and the existing entry won.
    ExistingWon(GlobalNameNotification),
    /// Name existed and the new entry won.
    NewWon(GlobalNameNotification),
    /// Existing entry was identical; no state changed.
    Unchanged,
}

/// Error returned by global registry operations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GlobalNameError {
    /// Registering a name failed because another entry won conflict resolution.
    Conflict(GlobalNameNotification),
    /// Unregistering a name failed because it was absent or not owned by caller.
    NotRegistered,
}

/// In-memory global name registry with deterministic merge semantics.
pub struct GlobalNameRegistry {
    local_node: Node,
    atom_table: Arc<AtomTable>,
    entries: DashMap<Atom, GlobalNameEntry>,
    notifications: DashMap<(Atom, u64, u64), GlobalNameNotification>,
}

impl GlobalNameRegistry {
    /// Create a registry for `local_node`.
    #[must_use]
    pub fn new(local_node: Node, atom_table: Arc<AtomTable>) -> Self {
        Self {
            local_node,
            atom_table,
            entries: DashMap::new(),
            notifications: DashMap::new(),
        }
    }

    /// Return this registry's local node identity.
    #[must_use]
    pub const fn local_node(&self) -> Node {
        self.local_node
    }

    /// Register `name` for a local or remote PID identity.
    pub fn register(
        &self,
        name: Atom,
        pid: GlobalPid,
        resolver: Option<Term>,
    ) -> Result<GlobalRegistrationOutcome, GlobalNameError> {
        let entry = GlobalNameEntry::new(name, pid, resolver);
        let outcome = self.merge_entry(entry);
        match outcome {
            GlobalRegistrationOutcome::ExistingWon(notification) if notification.loser == pid => {
                Err(GlobalNameError::Conflict(notification))
            }
            _ => Ok(outcome),
        }
    }

    /// Return the entry registered under `name`, if any.
    #[must_use]
    pub fn whereis(&self, name: Atom) -> Option<GlobalNameEntry> {
        self.entries.get(&name).map(|entry| *entry)
    }

    /// Unregister `name` only if it is owned by `pid`.
    pub fn unregister(&self, name: Atom, pid: GlobalPid) -> Result<(), GlobalNameError> {
        match self.entries.remove_if(&name, |_, entry| entry.pid == pid) {
            Some(_) => Ok(()),
            None => Err(GlobalNameError::NotRegistered),
        }
    }

    /// Remove every registration owned by `node`, used when a node disconnects.
    pub fn remove_node(&self, node: Atom) -> Vec<GlobalNameEntry> {
        let names: Vec<Atom> = self
            .entries
            .iter()
            .filter_map(|entry| (entry.pid.node == node).then_some(*entry.key()))
            .collect();
        self.remove_names(names)
    }

    /// Remove every registration owned by `pid`, used during process exit.
    pub fn remove_pid(&self, pid: GlobalPid) -> Vec<GlobalNameEntry> {
        let names: Vec<Atom> = self
            .entries
            .iter()
            .filter_map(|entry| (entry.pid == pid).then_some(*entry.key()))
            .collect();
        self.remove_names(names)
    }

    /// Merge all entries from another registry snapshot into this registry.
    pub fn merge_snapshot<I>(&self, entries: I) -> Vec<GlobalRegistrationOutcome>
    where
        I: IntoIterator<Item = GlobalNameEntry>,
    {
        entries
            .into_iter()
            .map(|entry| self.merge_entry(entry))
            .collect()
    }

    /// Snapshot current entries for test propagation or distribution framing.
    #[must_use]
    pub fn snapshot(&self) -> Vec<GlobalNameEntry> {
        self.entries.iter().map(|entry| *entry).collect()
    }

    /// Return and clear pending loser notifications.
    #[must_use]
    pub fn take_notifications(&self) -> Vec<GlobalNameNotification> {
        let notifications: Vec<GlobalNameNotification> = self
            .notifications
            .iter()
            .map(|entry| *entry.value())
            .collect();
        self.notifications.clear();
        notifications
    }

    /// Merge a single entry using deterministic conflict resolution.
    pub fn merge_entry(&self, incoming: GlobalNameEntry) -> GlobalRegistrationOutcome {
        match self.entries.entry(incoming.name) {
            Entry::Vacant(entry) => {
                entry.insert(incoming);
                GlobalRegistrationOutcome::Inserted
            }
            Entry::Occupied(mut entry) => {
                let existing = *entry.get();
                if existing == incoming {
                    return GlobalRegistrationOutcome::Unchanged;
                }

                if !self.entry_wins(incoming, existing) {
                    let notification = loser_notification(existing, incoming);
                    self.record_notification(notification);
                    return GlobalRegistrationOutcome::ExistingWon(notification);
                }

                entry.insert(incoming);
                let notification = loser_notification(incoming, existing);
                self.record_notification(notification);
                GlobalRegistrationOutcome::NewWon(notification)
            }
        }
    }

    fn remove_names(&self, names: Vec<Atom>) -> Vec<GlobalNameEntry> {
        names
            .into_iter()
            .filter_map(|name| self.entries.remove(&name).map(|(_, entry)| entry))
            .collect()
    }

    fn entry_wins(&self, candidate: GlobalNameEntry, incumbent: GlobalNameEntry) -> bool {
        match self.compare_node_names(candidate.pid.node, incumbent.pid.node) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                (candidate.pid.pid_number, candidate.pid.serial)
                    < (incumbent.pid.pid_number, incumbent.pid.serial)
            }
        }
    }

    fn compare_node_names(&self, left: Atom, right: Atom) -> std::cmp::Ordering {
        let left_name = self.atom_table.resolve(left);
        let right_name = self.atom_table.resolve(right);
        match (left_name, right_name) {
            (Some(left), Some(right)) => left.cmp(right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.index().cmp(&right.index()),
        }
    }

    fn record_notification(&self, notification: GlobalNameNotification) {
        self.notifications.insert(
            (
                notification.name,
                notification.loser.pid_number,
                notification.loser.serial,
            ),
            notification,
        );
    }
}

fn loser_notification(winner: GlobalNameEntry, loser: GlobalNameEntry) -> GlobalNameNotification {
    GlobalNameNotification {
        name: winner.name,
        loser: loser.pid,
        winner: winner.pid,
        resolver: winner.resolver.or(loser.resolver),
    }
}

impl crate::native::GlobalNameFacility for GlobalNameRegistry {
    fn register_name(
        &self,
        name: Atom,
        pid: GlobalPid,
        resolver: Option<Term>,
    ) -> Result<(), GlobalNameError> {
        self.register(name, pid, resolver).map(|_| ())
    }

    fn whereis_name(&self, name: Atom) -> Option<GlobalNameEntry> {
        self.whereis(name)
    }

    fn unregister_name(&self, name: Atom, pid: GlobalPid) -> Result<(), GlobalNameError> {
        self.unregister(name, pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(name: &str) -> (Arc<AtomTable>, GlobalNameRegistry) {
        let atoms = Arc::new(AtomTable::with_common_atoms());
        let node = Node::new(atoms.intern(name), 0);
        let registry = GlobalNameRegistry::new(node, Arc::clone(&atoms));
        (atoms, registry)
    }

    #[test]
    fn snapshot_merge_propagates_registration_between_nodes() {
        let (atoms, node_a) = registry("a@host");
        let node_b = GlobalNameRegistry::new(Node::new(atoms.intern("b@host"), 0), atoms.clone());
        let name = atoms.intern("shared_name");
        let pid = GlobalPid::new(node_a.local_node().name, 41, 0);

        let result = node_a.register(name, pid, None);
        assert!(matches!(result, Ok(GlobalRegistrationOutcome::Inserted)));
        let outcomes = node_b.merge_snapshot(node_a.snapshot());

        assert_eq!(outcomes.len(), 1);
        assert_eq!(node_b.whereis(name).map(|entry| entry.pid), Some(pid));
    }

    #[test]
    fn lower_node_name_wins_conflict_and_loser_is_notified() {
        let (atoms, node_a) = registry("a@host");
        let node_z = GlobalNameRegistry::new(Node::new(atoms.intern("z@host"), 0), atoms.clone());
        let name = atoms.intern("conflict_name");
        let low_pid = GlobalPid::new(node_a.local_node().name, 1, 0);
        let high_pid = GlobalPid::new(node_z.local_node().name, 2, 0);

        assert!(node_z.register(name, high_pid, None).is_ok());
        let outcomes = node_z.merge_snapshot([GlobalNameEntry::new(name, low_pid, None)]);

        assert_eq!(node_z.whereis(name).map(|entry| entry.pid), Some(low_pid));
        assert!(matches!(
            outcomes.as_slice(),
            [GlobalRegistrationOutcome::NewWon(GlobalNameNotification { loser, winner, .. })]
                if *loser == high_pid && *winner == low_pid
        ));
        assert_eq!(node_z.take_notifications().len(), 1);
    }

    #[test]
    fn lower_node_name_wins_when_atom_indexes_differ() {
        let local_atoms = Arc::new(AtomTable::with_common_atoms());
        let z_node = local_atoms.intern("z@host");
        let a_node = local_atoms.intern("a@host");
        let registry = GlobalNameRegistry::new(Node::new(z_node, 0), local_atoms.clone());
        let name = local_atoms.intern("index_independent_conflict");
        let low_pid = GlobalPid::new(a_node, 1, 0);
        let high_pid = GlobalPid::new(z_node, 2, 0);

        assert!(registry.register(name, high_pid, None).is_ok());
        let outcomes = registry.merge_snapshot([GlobalNameEntry::new(name, low_pid, None)]);

        assert_eq!(registry.whereis(name).map(|entry| entry.pid), Some(low_pid));
        assert!(matches!(
            outcomes.as_slice(),
            [GlobalRegistrationOutcome::NewWon(GlobalNameNotification { loser, winner, .. })]
                if *loser == high_pid && *winner == low_pid
        ));
    }

    #[test]
    fn register_returns_conflict_when_caller_loses_existing_registration() {
        let (atoms, registry) = registry("b@host");
        let name = atoms.intern("register_conflict");
        let winner = GlobalPid::new(atoms.intern("a@host"), 1, 0);
        let loser = GlobalPid::new(atoms.intern("z@host"), 2, 0);

        assert!(registry.register(name, winner, None).is_ok());
        assert!(matches!(
            registry.register(name, loser, None),
            Err(GlobalNameError::Conflict(_))
        ));
        assert_eq!(registry.whereis(name).map(|entry| entry.pid), Some(winner));
    }

    #[test]
    fn remove_node_cleans_disconnected_registrations() {
        let (atoms, registry) = registry("local@host");
        let remote = atoms.intern("remote@host");
        let name = atoms.intern("remote_name");
        let pid = GlobalPid::new(remote, 7, 0);

        assert!(registry.register(name, pid, None).is_ok());
        let removed = registry.remove_node(remote);

        assert_eq!(removed.len(), 1);
        assert!(registry.whereis(name).is_none());
    }
}
