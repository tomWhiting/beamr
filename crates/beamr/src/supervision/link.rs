//! Bidirectional link management.
//!
//! A link bonds two processes: if either dies, the other receives
//! an exit signal. By default the signal is fatal — the linked
//! process dies too. If the linked process traps exits, the signal
//! arrives as a message instead. Links are symmetric: A linking to
//! B is the same as B linking to A.

use std::collections::{HashMap, VecDeque};

use crate::{
    atom::Atom,
    process::{ExitReason, Process, ProcessStatus, RemotePid},
    term::{Term, boxed},
};

/// Link two live process values bidirectionally.
///
/// Linking a process to itself is a no-op. Duplicate links are naturally
/// suppressed by each process's link set.
pub fn link(left: &mut Process, right: &mut Process) {
    if left.pid() == right.pid() {
        return;
    }

    left.add_link(right.pid());
    right.add_link(left.pid());
}

/// Remove the bidirectional link between two live process values.
pub fn unlink(left: &mut Process, right: &mut Process) {
    if left.pid() == right.pid() {
        return;
    }

    left.remove_link(right.pid());
    right.remove_link(left.pid());
}

/// In-memory supervision graph used by the scheduler and focused unit tests.
#[derive(Debug, Default)]
pub struct LinkSet {
    dead: HashMap<u64, ExitReason>,
    #[cfg(test)]
    delivered_signals: Vec<(u64, u64, ExitReason)>,
}

impl LinkSet {
    /// Create an empty link context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Exit reason previously recorded for `pid`, when known.
    #[must_use]
    pub fn dead_reason(&self, pid: u64) -> Option<ExitReason> {
        self.dead.get(&pid).copied()
    }

    /// Link two processes, or immediately signal the live side when the other
    /// PID is already known dead.
    pub fn link_processes(&mut self, left: &mut Process, right: &mut Process) {
        link(left, right);
    }

    /// Link `caller` to a PID that may already be dead.
    pub fn link_pid(&mut self, caller: &mut Process, target_pid: u64) {
        if caller.pid() == target_pid {
            return;
        }

        if let Some(reason) = self.dead_reason(target_pid) {
            self.deliver_exit_signal(target_pid, caller, reason);
            if should_die_from_signal(caller, reason) {
                let terminal_reason = terminal_reason(reason);
                caller.terminate(terminal_reason);
                self.dead.insert(caller.pid(), terminal_reason);
            }
        } else {
            caller.add_link(target_pid);
        }
    }

    /// Mark `process` exited, propagate exit signals to all links, and remember
    /// its tombstone reason for future already-dead link attempts.
    pub fn process_exited(
        &mut self,
        process: &mut Process,
        processes: &mut [&mut Process],
        reason: ExitReason,
    ) {
        let mut cascade = VecDeque::from(self.mark_exited(process, reason, None));
        while let Some((source_pid, linked_pid, signal_reason)) = cascade.pop_front() {
            if let Some(index) = process_index_by_pid(processes, linked_pid) {
                let linked = &mut processes[index];
                linked.remove_link(source_pid);
                let target_dies = should_die_from_signal(linked, signal_reason);
                let propagated_reason = terminal_reason(signal_reason);
                self.deliver_exit_signal(source_pid, linked, signal_reason);
                if target_dies {
                    if propagated_reason == ExitReason::Killed {
                        linked.set_trap_exit(false);
                    }
                    cascade.extend(self.mark_exited(linked, propagated_reason, Some(source_pid)));
                }
            }
        }
    }

    /// Record a process as dead without propagating signals.
    ///
    /// Used by the scheduler's supervision integration which handles propagation
    /// itself through the process_bodies map. This only records the tombstone
    /// so future `link_pid()` calls immediately signal.
    pub fn process_exited_tombstone(&mut self, pid: u64, reason: ExitReason) {
        self.dead.insert(pid, reason);
    }

    fn mark_exited(
        &mut self,
        process: &mut Process,
        reason: ExitReason,
        source: Option<u64>,
    ) -> Vec<(u64, u64, ExitReason)> {
        let terminal_reason = terminal_reason(reason);
        let pid = process.pid();
        let links = process.take_links();
        process.terminate(terminal_reason);
        self.dead.insert(pid, terminal_reason);

        links
            .into_iter()
            .filter(|linked_pid| Some(*linked_pid) != source)
            .map(|linked_pid| (pid, linked_pid, terminal_reason))
            .collect()
    }

    fn deliver_exit_signal(&mut self, source_pid: u64, target: &mut Process, reason: ExitReason) {
        #[cfg(test)]
        self.delivered_signals
            .push((source_pid, target.pid(), reason));
        if should_die_from_signal(target, reason) {
            let _ = target.transition_to(ProcessStatus::Exited(terminal_reason(reason)));
        } else if target.trap_exit() && enqueue_exit_message(target, source_pid, reason).is_err() {
            target.terminate(ExitReason::Error);
            self.dead.insert(target.pid(), ExitReason::Error);
        }
    }

    #[cfg(test)]
    fn delivered_signals(&self) -> &[(u64, u64, ExitReason)] {
        &self.delivered_signals
    }
}

/// Convert an incoming exit signal into the terminal reason reported by a target.
#[must_use]
pub const fn terminal_reason(signal: ExitReason) -> ExitReason {
    match signal {
        ExitReason::Kill => ExitReason::Killed,
        reason => reason,
    }
}

/// Whether an incoming link exit signal terminates `target`: an untrappable
/// `Kill` always does; any other abnormal reason does unless `target` is
/// trapping exits; a `Normal` exit never does. Shared with the cooperative wasm
/// scheduler's native link propagation so both paths agree by construction.
pub(crate) fn should_die_from_signal(target: &Process, reason: ExitReason) -> bool {
    reason == ExitReason::Kill || (reason != ExitReason::Normal && !target.trap_exit())
}

/// Deliver an {EXIT, SourcePid, Reason} message to a trapping process.
///
/// Used by the scheduler's supervision integration to deliver exit signals
/// to processes that have `trap_exit` enabled. Falls back to terminating
/// the process with `Error` if heap allocation fails.
pub fn enqueue_exit_message_pub(target: &mut Process, source_pid: u64, reason: ExitReason) {
    let source = Ok(Term::pid(source_pid));
    if enqueue_exit_message_with_source(target, source, reason).is_err() {
        target.terminate(ExitReason::Error);
    }
}

/// Deliver an {EXIT, RemoteSourcePid, Reason} message to a trapping process.
pub fn enqueue_remote_exit_message_pub(
    target: &mut Process,
    source_pid: RemotePid,
    reason: ExitReason,
) {
    let source = source_pid_term(target, source_pid);
    if enqueue_exit_message_with_source(target, source, reason).is_err() {
        target.terminate(ExitReason::Error);
    }
}

fn source_pid_term(target: &mut Process, source_pid: RemotePid) -> Result<Term, ()> {
    const EXTERNAL_PID_WORDS: usize = 4;
    crate::gc::ensure_space(target, EXTERNAL_PID_WORDS, 256).map_err(|_| ())?;
    let words = target
        .heap_mut()
        .alloc_slice(EXTERNAL_PID_WORDS)
        .map_err(|_| ())?;
    boxed::write_external_pid(
        words,
        source_pid.node,
        source_pid.pid_number,
        source_pid.serial,
    )
    .ok_or(())
}

fn enqueue_exit_message(
    target: &mut Process,
    source_pid: u64,
    reason: ExitReason,
) -> Result<(), ()> {
    let source = Ok(Term::pid(source_pid));
    enqueue_exit_message_with_source(target, source, reason)
}

fn enqueue_exit_message_with_source(
    target: &mut Process,
    source_pid: Result<Term, ()>,
    reason: ExitReason,
) -> Result<(), ()> {
    const EXIT_TUPLE_WORDS: usize = 4;

    // Route heap growth through the GC: `ensure_space` runs minor/major
    // collection (moving live young-generation data into old space and
    // rewriting roots) BEFORE growing the nursery. Calling
    // `grow_to_next_capacity` directly here would swap in a fresh empty region
    // without copying live data, leaving registers, prior mailbox messages, and
    // stack slots dangling. See `gc::ensure_space` and `heap.rs` region invariant.
    crate::gc::ensure_space(target, EXIT_TUPLE_WORDS, 256).map_err(|_| ())?;
    let source_pid = source_pid?;

    let elements = [
        Term::atom(Atom::EXIT),
        source_pid,
        terminal_reason(reason).as_term(),
    ];
    let words = target
        .heap_mut()
        .alloc(1 + elements.len())
        .map_err(|_| ())?;
    // SAFETY: `alloc` returned a live region with exactly the requested number
    // of words in the target heap, used only to initialize this tuple.
    let words = unsafe { std::slice::from_raw_parts_mut(words, 1 + elements.len()) };
    let message = boxed::write_tuple(words, &elements).ok_or(())?;
    target.mailbox_mut().push_owned(message);
    Ok(())
}

fn process_index_by_pid(processes: &[&mut Process], pid: u64) -> Option<usize> {
    processes.iter().position(|process| process.pid() == pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::boxed::Tuple;

    fn running(pid: u64) -> Process {
        let mut process = Process::new(pid, 64);
        process
            .transition_to(ProcessStatus::Running)
            .unwrap_or_else(|error| panic!("process starts: {error}"));
        process
    }

    fn mailbox_tuple(process: &mut Process) -> Tuple {
        process.mailbox_mut().drain_arrival();
        Tuple::new(
            process
                .mailbox()
                .front_for_test()
                .unwrap_or_else(|| panic!("message exists")),
        )
        .unwrap_or_else(|| panic!("message is tuple"))
    }

    #[test]
    fn link_is_bidirectional_without_duplicates_or_self_links() {
        let mut a = running(1);
        let mut b = running(2);

        link(&mut a, &mut b);
        link(&mut b, &mut a);
        link(&mut a, &mut b);

        assert_eq!(a.links(), &[2]);
        assert_eq!(b.links(), &[1]);

        let mut c = running(3);
        let mut d = running(4);
        links_in_order(&mut a, &mut [&mut c, &mut d]);

        assert_eq!(a.links(), &[2, 3, 4]);
        assert_eq!(c.links(), &[1]);
        assert_eq!(d.links(), &[1]);

        unlink(&mut a, &mut c);
        assert_eq!(a.links(), &[2, 4]);
        assert_eq!(c.links(), &[] as &[u64]);

        let mut self_link = running(5);
        let self_link_pid = self_link.pid();
        self_link.add_link(self_link_pid);
        assert!(self_link.links().is_empty());
    }

    #[test]
    fn unlink_removes_both_sides_and_suppresses_exit_signal() {
        let mut links = LinkSet::new();
        let mut a = running(1);
        let mut b = running(2);
        links.link_processes(&mut a, &mut b);
        unlink(&mut a, &mut b);

        links.process_exited(&mut a, &mut [&mut b], ExitReason::Error);

        assert_eq!(a.status(), ProcessStatus::Exited(ExitReason::Error));
        assert_eq!(b.status(), ProcessStatus::Running);
        assert!(a.links().is_empty());
        assert!(b.links().is_empty());
    }

    #[test]
    fn non_normal_exit_kills_all_linked_processes_and_cascades() {
        let mut links = LinkSet::new();
        let mut a = running(1);
        let mut b = running(2);
        let mut c = running(3);
        links.link_processes(&mut a, &mut b);
        links.link_processes(&mut b, &mut c);

        links.process_exited(&mut a, &mut [&mut b, &mut c], ExitReason::Error);

        assert_eq!(a.status(), ProcessStatus::Exited(ExitReason::Error));
        assert_eq!(b.status(), ProcessStatus::Exited(ExitReason::Error));
        assert_eq!(c.status(), ProcessStatus::Exited(ExitReason::Error));
    }

    #[test]
    fn normal_exit_signals_but_does_not_kill_linked_processes() {
        let mut links = LinkSet::new();
        let mut a = running(1);
        let mut b = running(2);
        links.link_processes(&mut a, &mut b);

        links.process_exited(&mut a, &mut [&mut b], ExitReason::Normal);

        assert_eq!(a.status(), ProcessStatus::Exited(ExitReason::Normal));
        assert_eq!(b.status(), ProcessStatus::Running);
        assert!(!b.links().contains(&1));
    }

    #[test]
    fn trap_exit_converts_exit_signal_to_mailbox_message() {
        let mut links = LinkSet::new();
        let mut a = running(1);
        let mut b = running(2);
        b.set_trap_exit(true);
        links.link_processes(&mut a, &mut b);

        links.process_exited(&mut a, &mut [&mut b], ExitReason::Error);

        assert_eq!(b.status(), ProcessStatus::Running);
        let tuple = mailbox_tuple(&mut b);
        assert_eq!(tuple.arity(), 3);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1).and_then(Term::as_pid), Some(1));
        assert_eq!(tuple.get(2), Some(Term::atom(Atom::ERROR)));
    }

    #[test]
    fn trapped_exit_signals_follow_link_insertion_order() {
        let mut links = LinkSet::new();
        let mut source = running(1);
        let mut first = running(2);
        let mut second = running(3);
        let mut third = running(4);
        first.set_trap_exit(true);
        second.set_trap_exit(true);
        third.set_trap_exit(true);
        links_in_order(&mut source, &mut [&mut first, &mut second, &mut third]);

        assert_eq!(source.links(), &[2, 3, 4]);
        links.process_exited(
            &mut source,
            &mut [&mut first, &mut second, &mut third],
            ExitReason::Error,
        );

        assert_eq!(
            links.delivered_signals(),
            &[
                (1, 2, ExitReason::Error),
                (1, 3, ExitReason::Error),
                (1, 4, ExitReason::Error),
            ]
        );
        assert_exit_from(&mut first, 1);
        assert_exit_from(&mut second, 1);
        assert_exit_from(&mut third, 1);
    }

    #[test]
    fn trapped_exit_signals_preserve_order_after_unlink() {
        let mut links = LinkSet::new();
        let mut source = running(1);
        let mut first = running(2);
        let mut removed = running(3);
        let mut third = running(4);
        first.set_trap_exit(true);
        removed.set_trap_exit(true);
        third.set_trap_exit(true);
        links_in_order(&mut source, &mut [&mut first, &mut removed, &mut third]);
        unlink(&mut source, &mut removed);

        assert_eq!(source.links(), &[2, 4]);
        links.process_exited(
            &mut source,
            &mut [&mut first, &mut removed, &mut third],
            ExitReason::Error,
        );

        assert_eq!(
            links.delivered_signals(),
            &[(1, 2, ExitReason::Error), (1, 4, ExitReason::Error)]
        );
        assert_exit_from(&mut first, 1);
        assert!(removed.mailbox().is_empty());
        assert_exit_from(&mut third, 1);
    }

    fn links_in_order(source: &mut Process, targets: &mut [&mut Process]) {
        for target in targets {
            link(source, target);
        }
    }

    fn assert_exit_from(process: &mut Process, source_pid: u64) {
        let tuple = mailbox_tuple(process);
        assert_eq!(tuple.arity(), 3);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1).and_then(Term::as_pid), Some(source_pid));
        assert_eq!(tuple.get(2), Some(Term::atom(Atom::ERROR)));
    }

    #[test]
    fn kill_bypasses_trap_exit_and_propagates_killed() {
        let mut links = LinkSet::new();
        let mut a = running(1);
        let mut b = running(2);
        let mut c = running(3);
        links.link_processes(&mut a, &mut b);
        links.link_processes(&mut b, &mut c);

        links.process_exited(&mut a, &mut [&mut b, &mut c], ExitReason::Kill);

        assert_eq!(b.status(), ProcessStatus::Exited(ExitReason::Killed));
        assert_eq!(links.dead_reason(b.pid()), Some(ExitReason::Killed));
        assert_eq!(c.status(), ProcessStatus::Exited(ExitReason::Killed));
        assert_eq!(links.dead_reason(c.pid()), Some(ExitReason::Killed));

        let mut d = running(4);
        d.set_trap_exit(true);
        links.link_pid(&mut d, b.pid());
        assert_eq!(d.status(), ProcessStatus::Running);
        let tuple = mailbox_tuple(&mut d);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1).and_then(Term::as_pid), Some(2));
        assert_eq!(tuple.get(2), Some(Term::atom(Atom::KILLED)));
    }

    #[test]
    fn linking_to_already_dead_process_immediately_signals_caller() {
        let mut links = LinkSet::new();
        let mut dead = running(1);
        let mut caller = running(2);
        links.process_exited(&mut dead, &mut [], ExitReason::Error);

        links.link_pid(&mut caller, dead.pid());

        assert_eq!(caller.status(), ProcessStatus::Exited(ExitReason::Error));
    }

    /// Allocate a 3-element tuple directly on the process nursery (no GC), so it
    /// is a live young-generation object the next heap growth must preserve.
    fn alloc_young_tuple(process: &mut Process, elements: &[Term]) -> Term {
        let ptr = process
            .heap_mut()
            .alloc(1 + elements.len())
            .unwrap_or_else(|_| panic!("young tuple fits"));
        // SAFETY: `alloc` returned `1 + elements.len()` writable young-heap words.
        let words = unsafe { std::slice::from_raw_parts_mut(ptr, 1 + elements.len()) };
        boxed::write_tuple(words, elements).unwrap_or_else(|| panic!("tuple writes"))
    }

    /// PR-6: delivering an exit signal to a trapping process whose nursery is
    /// near-full must not abandon live young-generation terms. Before the fix the
    /// signal path called `grow_to_next_capacity()` directly, swapping in a fresh
    /// empty region without copying live data or rewriting roots — every live
    /// young pointer (here, the tuple in X0) was left dangling into freed memory.
    /// Routing through `gc::ensure_space` collects and rewrites roots first, so
    /// the term survives intact.
    #[test]
    fn exit_signal_on_near_full_heap_preserves_live_young_terms() {
        let mut links = LinkSet::new();
        // Small nursery so we can drive it near-full cheaply.
        let mut watcher = Process::new(1, 16);
        watcher
            .transition_to(ProcessStatus::Running)
            .unwrap_or_else(|error| panic!("process starts: {error}"));
        watcher.set_trap_exit(true);

        // Live term kept as a root via X0.
        let live = alloc_young_tuple(&mut watcher, &[Term::small_int(111), Term::small_int(222)]);
        watcher.set_x_reg(0, live);

        // Fill the remaining nursery so the 4-word exit tuple cannot fit without
        // growth — forcing the heap-growth path on the signal.
        while watcher.heap().available() >= 4 {
            let _ = watcher.heap_mut().alloc(1);
        }
        assert!(watcher.heap().available() < 4);

        links.deliver_exit_signal(2, &mut watcher, ExitReason::Error);

        // The previously-live tuple must still be valid and correct.
        let recovered =
            Tuple::new(watcher.x_reg(0)).unwrap_or_else(|| panic!("X0 is still a tuple"));
        assert_eq!(recovered.arity(), 2);
        assert_eq!(recovered.get(0), Some(Term::small_int(111)));
        assert_eq!(recovered.get(1), Some(Term::small_int(222)));

        // And the exit message was still delivered.
        assert_eq!(watcher.status(), ProcessStatus::Running);
        let tuple = mailbox_tuple(&mut watcher);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1).and_then(Term::as_pid), Some(2));
    }

    #[test]
    fn trapped_exit_delivery_grows_mailbox_heap_instead_of_dropping_signal() {
        let mut links = LinkSet::new();
        let mut watcher = Process::new(1, 1);
        watcher
            .transition_to(ProcessStatus::Running)
            .unwrap_or_else(|error| panic!("process starts: {error}"));
        watcher.set_trap_exit(true);

        links.deliver_exit_signal(2, &mut watcher, ExitReason::Error);

        assert_eq!(watcher.status(), ProcessStatus::Running);
        assert!(watcher.heap().capacity() >= 4);
        let tuple = mailbox_tuple(&mut watcher);
        assert_eq!(tuple.get(0), Some(Term::atom(Atom::EXIT)));
        assert_eq!(tuple.get(1).and_then(Term::as_pid), Some(2));
        assert_eq!(tuple.get(2), Some(Term::atom(Atom::ERROR)));
        assert_eq!(links.dead_reason(watcher.pid()), None);
    }
}
