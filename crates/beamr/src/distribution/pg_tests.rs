use super::pg::*;
use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::boxed::{Cons, ExternalPid};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingPropagation {
    updates: Mutex<Vec<PgUpdate>>,
}

impl PgPropagation for RecordingPropagation {
    fn broadcast(&self, update: PgUpdate) {
        self.updates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(update);
    }
}

fn atom_table() -> AtomTable {
    AtomTable::with_common_atoms()
}

fn context<'a>(process: &'a mut Process, registry: Arc<PgRegistry>) -> ProcessContext<'a> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    let facility: Arc<dyn PgFacility> = registry;
    context.set_pg_facility(Some(facility));
    context
}

fn list_terms(mut term: Term) -> Vec<Term> {
    let mut terms = Vec::new();
    while term != Term::NIL {
        let cons = Cons::new(term).expect("proper list cons");
        terms.push(cons.head());
        term = cons.tail();
    }
    terms
}

#[test]
fn join_get_members_leave_round_trips() {
    let atoms = atom_table();
    let registry = Arc::new(PgRegistry::new(&atoms));
    let group = atoms.intern("workers");
    let pid = Term::pid(42);
    let mut process = Process::new(1, 128);
    let mut context = context(&mut process, Arc::clone(&registry));

    let join_args = [Term::atom(group), pid];
    let join_result = super::pg::bif_join_2(&join_args, &mut context);
    assert_eq!(join_result, Ok(Term::atom(Atom::OK)));

    let members_args = [Term::atom(group)];
    let members = super::pg::bif_get_members_1(&members_args, &mut context).expect("members");
    assert_eq!(list_terms(members), vec![pid]);

    let leave_args = [Term::atom(group), pid];
    let leave_result = super::pg::bif_leave_2(&leave_args, &mut context);
    assert_eq!(leave_result, Ok(Term::atom(Atom::OK)));

    let members = super::pg::bif_get_members_1(&members_args, &mut context).expect("members");
    assert!(list_terms(members).is_empty());
}

#[test]
fn duplicate_join_does_not_duplicate_members() {
    let atoms = atom_table();
    let registry = PgRegistry::new(&atoms);
    let group = atoms.intern("workers");
    registry.join(registry.default_scope(), group, 7);
    registry.join(registry.default_scope(), group, 7);

    assert_eq!(
        registry.local_members(registry.default_scope(), group),
        vec![7]
    );
}

#[test]
fn process_exit_removes_pid_from_all_groups_and_broadcasts_leaves() {
    let atoms = atom_table();
    let propagation = Arc::new(RecordingPropagation::default());
    let registry = PgRegistry::with_propagation(&atoms, propagation.clone());
    let group_a = atoms.intern("a");
    let group_b = atoms.intern("b");
    registry.join(registry.default_scope(), group_a, 7);
    registry.join(registry.default_scope(), group_b, 7);

    registry.remove_pid_from_all_scopes(7);

    assert!(
        registry
            .local_members(registry.default_scope(), group_a)
            .is_empty()
    );
    assert!(
        registry
            .local_members(registry.default_scope(), group_b)
            .is_empty()
    );
    let updates = propagation
        .updates
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(updates.contains(&PgUpdate::Leave {
        scope: registry.default_scope(),
        group: group_a,
        pid: 7
    }));
    assert!(updates.contains(&PgUpdate::Leave {
        scope: registry.default_scope(),
        group: group_b,
        pid: 7
    }));
}

#[test]
fn remote_apply_join_visible_only_in_all_members_and_purged_on_node_down() {
    let atoms = atom_table();
    let registry = Arc::new(PgRegistry::new(&atoms));
    let group = atoms.intern("workers");
    let node = atoms.intern("remote@host");
    registry.apply_remote_join(registry.default_scope(), group, node, 99, 1);

    let mut process = Process::new(1, 128);
    let mut context = context(&mut process, Arc::clone(&registry));

    let local_args = [Term::atom(group)];
    let local =
        super::pg::bif_get_local_members_1(&local_args, &mut context).expect("local");
    assert!(list_terms(local).is_empty());

    let all_args = [Term::atom(group)];
    let all = super::pg::bif_get_members_1(&all_args, &mut context).expect("all");
    let all = list_terms(all);
    assert_eq!(all.len(), 1);
    let external = ExternalPid::new(all[0]).expect("external pid");
    assert_eq!(external.node(), Some(node));
    assert_eq!(external.pid_number(), 99);
    assert_eq!(external.serial(), 1);

    registry.purge_remote_node(node);
    let all = super::pg::bif_get_members_1(&all_args, &mut context).expect("all");
    assert!(list_terms(all).is_empty());
}

#[test]
fn scoped_groups_are_independent() {
    let atoms = atom_table();
    let registry = PgRegistry::new(&atoms);
    let scope_a = atoms.intern("scope_a");
    let scope_b = atoms.intern("scope_b");
    let group = atoms.intern("workers");
    registry.start_scope(scope_a);
    registry.start_scope(scope_b);
    registry.join(scope_a, group, 1);
    registry.join(scope_b, group, 2);

    assert_eq!(registry.local_members(scope_a, group), vec![1]);
    assert_eq!(registry.local_members(scope_b, group), vec![2]);
    assert!(
        registry
            .local_members(registry.default_scope(), group)
            .is_empty()
    );
}
