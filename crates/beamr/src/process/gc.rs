//! Process-owned GC root set construction.
//!
//! The collector lives in [`crate::gc`]. This module is the process facade for
//! building and replacing the ordered root snapshot that collector code rewrites.

use crate::{process::Process, term::Term};

/// Ordered snapshot of live process roots for GC pointer rewriting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RootSet {
    live_x: usize,
    roots: Vec<Term>,
}

impl RootSet {
    /// Build a root snapshot containing the first `live_x` X registers, all
    /// Y-registers, queued mailbox messages, and the current exception payload.
    pub(crate) fn snapshot(process: &mut Process, live_x: usize) -> Self {
        Self {
            live_x,
            roots: process.roots_with_live_x(live_x),
        }
    }

    /// Mutable access to the ordered root terms during collection.
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Term> {
        self.roots.iter_mut()
    }

    /// Replace the process roots from this snapshot's rewritten terms.
    pub(crate) fn replace_process_roots(self, process: &mut Process) {
        process.replace_roots_with_live_x(self.live_x, &self.roots);
    }

    /// Immutable access to roots for tests and diagnostics.
    #[cfg(test)]
    pub(crate) fn as_slice(&self) -> &[Term] {
        &self.roots
    }
}

/// Build the ordered GC root set for `process`.
pub(crate) fn root_set(process: &mut Process, live_x: usize) -> RootSet {
    RootSet::snapshot(process, live_x)
}

#[cfg(test)]
mod tests {
    use crate::{
        atom::Atom,
        gc::tests::{Snapshot, alloc_tuple, module_pin, snapshot},
        process::{Exception, Process},
        term::Term,
    };

    use super::root_set;

    #[test]
    fn root_set_includes_only_live_x_prefix() {
        let mut process = Process::new(1, 32);
        process.set_x_reg(0, Term::small_int(1));
        process.set_x_reg(1, Term::small_int(2));
        process.set_x_reg(2, Term::small_int(3));

        let roots = root_set(&mut process, 2);

        assert_eq!(roots.as_slice(), &[Term::small_int(1), Term::small_int(2)]);
    }

    #[test]
    fn root_set_includes_y_registers_mailbox_and_exception_payload() {
        let mut process = Process::new(1, 64);
        let y0 = alloc_tuple(&mut process, &[Term::small_int(10)]);
        let y1 = alloc_tuple(&mut process, &[Term::small_int(11)]);
        let message = alloc_tuple(&mut process, &[Term::small_int(12)]);
        let reason = alloc_tuple(&mut process, &[Term::small_int(13)]);
        let stacktrace = alloc_tuple(&mut process, &[Term::small_int(14)]);
        let dict_key = alloc_tuple(&mut process, &[Term::small_int(15)]);
        let dict_value = alloc_tuple(&mut process, &[Term::small_int(16)]);
        process
            .stack_mut()
            .push_frame(Atom::OK, 0, module_pin(Atom::OK), 2)
            .expect("frame fits");
        process.stack_mut().set_y_reg(0, y0).expect("Y0 exists");
        process.stack_mut().set_y_reg(1, y1).expect("Y1 exists");
        process.mailbox_mut().push_owned_for_test(message);
        process.set_current_exception(Some(Exception {
            class: Term::atom(Atom::ERROR),
            reason,
            stacktrace,
        }));
        process.dict_put(dict_key, dict_value);

        let roots = root_set(&mut process, 0);
        let snapshots: Vec<_> = roots.as_slice().iter().copied().map(snapshot).collect();

        assert_eq!(
            snapshots,
            vec![
                Snapshot::Tuple(vec![Snapshot::Int(10)]),
                Snapshot::Tuple(vec![Snapshot::Int(11)]),
                Snapshot::Tuple(vec![Snapshot::Int(12)]),
                Snapshot::Tuple(vec![Snapshot::Int(13)]),
                Snapshot::Tuple(vec![Snapshot::Int(14)]),
                Snapshot::Tuple(vec![Snapshot::Int(15)]),
                Snapshot::Tuple(vec![Snapshot::Int(16)]),
            ]
        );
    }
}
