//! Process management BIFs — self, spawn, spawn_link, link, unlink, process_flag.
//!
//! These BIFs provide the ability to query the calling process's PID, create
//! new processes, manage bidirectional links, and set process flags from BEAM
//! code. They are registered as Gate 2 BIFs alongside the Gate 1 arithmetic,
//! comparison, and utility functions.

use crate::atom::{Atom, AtomTable};
use crate::native::links::LinkError;
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::Cons;

type Gate2Bif = (&'static str, u8, NativeFn);

const GATE2_BIFS: &[Gate2Bif] = &[
    ("self", 0, bif_self),
    ("spawn", 3, bif_spawn),
    ("spawn_link", 3, bif_spawn_link),
    ("link", 1, bif_link),
    ("unlink", 1, bif_unlink),
    ("process_flag", 2, bif_process_flag),
];

/// Registers all Gate 2 (process creation) BIFs into the VM-owned BIF registry.
pub fn register_gate2_bifs(
    registry: &mut BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in GATE2_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function)?;
    }

    Ok(())
}

/// erlang:self/0 — returns the calling process's PID.
pub fn bif_self(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    let pid = context.pid().ok_or_else(badarg)?;
    Term::try_pid(pid).ok_or_else(badarg)
}

/// erlang:spawn/3 — creates a new process executing Module:Function(Args).
pub fn bif_spawn(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_impl(args, context, false)
}

/// erlang:spawn_link/3 — creates a new linked process executing Module:Function(Args).
pub fn bif_spawn_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    spawn_impl(args, context, true)
}

/// Shared implementation for spawn/3 and spawn_link/3.
fn spawn_impl(args: &[Term], context: &mut ProcessContext, link: bool) -> Result<Term, Term> {
    let [module_term, function_term, args_term] = args else {
        return Err(badarg());
    };

    let module = module_term.as_atom().ok_or_else(badarg)?;
    let function = function_term.as_atom().ok_or_else(badarg)?;
    let spawn_args = list_to_vec(*args_term)?;

    let link_to = if link {
        Some(context.pid().ok_or_else(badarg)?)
    } else {
        None
    };

    let facility = context.spawn_facility().ok_or_else(badarg)?;
    let new_pid = facility
        .spawn(module, function, spawn_args, link_to)
        .map_err(|_| badarg())?;

    Term::try_pid(new_pid).ok_or_else(badarg)
}

/// Converts a BEAM list term to a Vec<Term>, returning badarg for improper lists.
fn list_to_vec(term: Term) -> Result<Vec<Term>, Term> {
    let mut elements = Vec::new();
    let mut current = term;

    loop {
        if current.is_nil() {
            return Ok(elements);
        }
        let cons = Cons::new(current).ok_or_else(badarg)?;
        elements.push(cons.head());
        current = cons.tail();
    }
}

/// erlang:link/1 — establishes a bidirectional link to the target process.
pub fn bif_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [target_term] = args else {
        return Err(badarg());
    };

    let target_pid = target_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;

    // Self-link is a no-op returning true.
    if caller_pid == target_pid {
        return Ok(Term::atom(Atom::TRUE));
    }

    let facility = context.link_facility().ok_or_else(badarg)?;
    match facility.link(caller_pid, target_pid) {
        Ok(()) => Ok(Term::atom(Atom::TRUE)),
        Err(LinkError::NoProc) => Err(Term::atom(Atom::NOPROC)),
        Err(LinkError::NoCaller) => Err(badarg()),
    }
}

/// erlang:unlink/1 — removes the bidirectional link to the target process.
pub fn bif_unlink(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [target_term] = args else {
        return Err(badarg());
    };

    let target_pid = target_term.as_pid().ok_or_else(badarg)?;
    let caller_pid = context.pid().ok_or_else(badarg)?;

    // Self-unlink is a no-op returning true.
    if caller_pid == target_pid {
        return Ok(Term::atom(Atom::TRUE));
    }

    let facility = context.link_facility().ok_or_else(badarg)?;
    facility.unlink(caller_pid, target_pid).map_err(|_| badarg())?;

    Ok(Term::atom(Atom::TRUE))
}

/// erlang:process_flag/2 — sets a process flag, returns the previous value.
pub fn bif_process_flag(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [flag_term, value_term] = args else {
        return Err(badarg());
    };

    let flag = flag_term.as_atom().ok_or_else(badarg)?;

    if flag == Atom::TRAP_EXIT {
        let new_value = atom_to_bool(*value_term).ok_or_else(badarg)?;
        let caller_pid = context.pid().ok_or_else(badarg)?;
        let facility = context.link_facility().ok_or_else(badarg)?;
        let old_value = facility
            .set_trap_exit(caller_pid, new_value)
            .map_err(|_| badarg())?;
        Ok(bool_to_atom(old_value))
    } else {
        Err(badarg())
    }
}

/// Convert an atom term (`true`/`false`) to a Rust bool.
fn atom_to_bool(term: Term) -> Option<bool> {
    let atom = term.as_atom()?;
    if atom == Atom::TRUE {
        Some(true)
    } else if atom == Atom::FALSE {
        Some(false)
    } else {
        None
    }
}

/// Convert a Rust bool to the corresponding atom term.
const fn bool_to_atom(value: bool) -> Term {
    if value {
        Term::atom(Atom::TRUE)
    } else {
        Term::atom(Atom::FALSE)
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::{
        bif_link, bif_process_flag, bif_self, bif_spawn, bif_spawn_link, bif_unlink, list_to_vec,
        register_gate2_bifs,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::native::links::{LinkError, LinkFacility, LinkRecord};
    use crate::native::spawn::{SpawnError, SpawnFacility, SpawnRecord};
    use crate::native::{BifRegistryImpl, ProcessContext};
    use crate::term::Term;
    use crate::term::boxed::write_cons;
    use std::sync::{Arc, Mutex};

    fn badarg() -> Term {
        Term::atom(Atom::BADARG)
    }

    // --- erlang:self/0 tests ---

    #[test]
    fn self_returns_pid_when_context_has_pid() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(42));
        let result = bif_self(&[], &mut context);
        assert_eq!(result, Ok(Term::pid(42)));
    }

    #[test]
    fn self_returns_badarg_when_context_has_no_pid() {
        let mut context = ProcessContext::new();
        let result = bif_self(&[], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn self_returns_badarg_with_wrong_arity() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        let result = bif_self(&[Term::small_int(1)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    // --- erlang:spawn/3 tests ---

    #[test]
    fn spawn_returns_badarg_without_spawn_facility() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        let module = Term::atom(Atom::OK);
        let function = Term::atom(Atom::ERROR);
        let result = bif_spawn(&[module, function, Term::NIL], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn spawn_returns_new_pid_via_facility() {
        let facility = Arc::new(MockSpawnFacility::new(7));
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        context.set_spawn_facility(Some(facility.clone()));

        let module = Term::atom(Atom::OK);
        let function = Term::atom(Atom::ERROR);
        let result = bif_spawn(&[module, function, Term::NIL], &mut context);
        assert_eq!(result, Ok(Term::pid(7)));

        let records = facility.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].module, Atom::OK);
        assert_eq!(records[0].function, Atom::ERROR);
        assert!(records[0].args.is_empty());
        assert_eq!(records[0].link_to, None);
    }

    #[test]
    fn spawn_passes_list_args_to_facility() {
        let facility = Arc::new(MockSpawnFacility::new(10));
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        context.set_spawn_facility(Some(facility.clone()));

        // Build a list [1, 2]
        let mut cell2 = [0_u64; 2];
        let tail = write_cons(&mut cell2, Term::small_int(2), Term::NIL).unwrap();
        let mut cell1 = [0_u64; 2];
        let list = write_cons(&mut cell1, Term::small_int(1), tail).unwrap();

        let module = Term::atom(Atom::OK);
        let function = Term::atom(Atom::ERROR);
        let result = bif_spawn(&[module, function, list], &mut context);
        assert_eq!(result, Ok(Term::pid(10)));

        let records = facility.records();
        assert_eq!(records[0].args, vec![Term::small_int(1), Term::small_int(2)]);
    }

    #[test]
    fn spawn_returns_badarg_for_non_atom_module() {
        let facility = Arc::new(MockSpawnFacility::new(1));
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        context.set_spawn_facility(Some(facility));

        let result = bif_spawn(
            &[Term::small_int(42), Term::atom(Atom::OK), Term::NIL],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn spawn_returns_badarg_for_non_atom_function() {
        let facility = Arc::new(MockSpawnFacility::new(1));
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        context.set_spawn_facility(Some(facility));

        let result = bif_spawn(
            &[Term::atom(Atom::OK), Term::small_int(42), Term::NIL],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn spawn_returns_badarg_for_wrong_arity() {
        let mut context = ProcessContext::new();
        let result = bif_spawn(&[Term::atom(Atom::OK)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn spawn_returns_badarg_when_facility_fails() {
        let facility = Arc::new(FailingSpawnFacility);
        let mut context = ProcessContext::new();
        context.set_pid(Some(0));
        context.set_spawn_facility(Some(facility));

        let result = bif_spawn(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    // --- erlang:spawn_link/3 tests ---

    #[test]
    fn spawn_link_returns_new_pid_with_link_to_parent() {
        let facility = Arc::new(MockSpawnFacility::new(8));
        let mut context = ProcessContext::new();
        context.set_pid(Some(3));
        context.set_spawn_facility(Some(facility.clone()));

        let module = Term::atom(Atom::OK);
        let function = Term::atom(Atom::ERROR);
        let result = bif_spawn_link(&[module, function, Term::NIL], &mut context);
        assert_eq!(result, Ok(Term::pid(8)));

        let records = facility.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].link_to, Some(3));
    }

    #[test]
    fn spawn_link_returns_badarg_without_pid() {
        let facility = Arc::new(MockSpawnFacility::new(8));
        let mut context = ProcessContext::new();
        context.set_spawn_facility(Some(facility));

        let result = bif_spawn_link(
            &[Term::atom(Atom::OK), Term::atom(Atom::ERROR), Term::NIL],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    // --- list_to_vec tests ---

    #[test]
    fn list_to_vec_converts_empty_list() {
        let result = list_to_vec(Term::NIL).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn list_to_vec_converts_proper_list() {
        let mut cell2 = [0_u64; 2];
        let tail = write_cons(&mut cell2, Term::small_int(2), Term::NIL).unwrap();
        let mut cell1 = [0_u64; 2];
        let list = write_cons(&mut cell1, Term::small_int(1), tail).unwrap();

        let result = list_to_vec(list).unwrap();
        assert_eq!(result, vec![Term::small_int(1), Term::small_int(2)]);
    }

    #[test]
    fn list_to_vec_rejects_non_list_term() {
        let result = list_to_vec(Term::small_int(42));
        assert_eq!(result, Err(badarg()));
    }

    // --- Registration tests ---

    #[test]
    fn register_gate2_bifs_registers_all_process_mfas() {
        let atom_table = AtomTable::new();
        let mut registry = BifRegistryImpl::new();

        register_gate2_bifs(&mut registry, &atom_table).expect("gate 2 BIF registration");

        let erlang = atom_table.intern("erlang");
        for (name, arity) in [
            ("self", 0),
            ("spawn", 3),
            ("spawn_link", 3),
            ("link", 1),
            ("unlink", 1),
            ("process_flag", 2),
        ] {
            let function = atom_table.intern(name);
            assert!(
                registry.lookup(erlang, function, arity).is_some(),
                "missing erlang:{name}/{arity}"
            );
        }
    }

    #[test]
    fn register_gate2_bifs_fails_when_called_twice() {
        let atom_table = AtomTable::new();
        let mut registry = BifRegistryImpl::new();

        register_gate2_bifs(&mut registry, &atom_table).expect("first registration");

        assert!(register_gate2_bifs(&mut registry, &atom_table).is_err());
    }

    #[test]
    fn gate1_and_gate2_bifs_coexist_without_conflict() {
        let atom_table = AtomTable::new();
        let mut registry = BifRegistryImpl::new();

        crate::native::bifs::register_gate1_bifs(&mut registry, &atom_table)
            .expect("gate 1 registration");
        register_gate2_bifs(&mut registry, &atom_table).expect("gate 2 registration");

        let erlang = atom_table.intern("erlang");
        // Gate 1 BIF still present
        let plus = atom_table.intern("+");
        assert!(registry.lookup(erlang, plus, 2).is_some());
        // Gate 2 BIF present
        let self_atom = atom_table.intern("self");
        assert!(registry.lookup(erlang, self_atom, 0).is_some());
    }

    // --- Mock spawn facility ---

    struct MockSpawnFacility {
        next_pid: u64,
        records: Mutex<Vec<SpawnRecord>>,
    }

    impl MockSpawnFacility {
        fn new(next_pid: u64) -> Self {
            Self {
                next_pid,
                records: Mutex::new(Vec::new()),
            }
        }

        fn records(&self) -> Vec<SpawnRecord> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    impl SpawnFacility for MockSpawnFacility {
        fn spawn(
            &self,
            module: Atom,
            function: Atom,
            args: Vec<Term>,
            link_to: Option<u64>,
        ) -> Result<u64, SpawnError> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(SpawnRecord {
                    module,
                    function,
                    args,
                    link_to,
                });
            Ok(self.next_pid)
        }
    }

    struct FailingSpawnFacility;

    impl SpawnFacility for FailingSpawnFacility {
        fn spawn(
            &self,
            _module: Atom,
            _function: Atom,
            _args: Vec<Term>,
            _link_to: Option<u64>,
        ) -> Result<u64, SpawnError> {
            Err(SpawnError::UnresolvedMfa)
        }
    }

    // --- Mock link facility ---

    struct MockLinkFacility {
        records: Mutex<Vec<LinkRecord>>,
        trap_exit: Mutex<bool>,
    }

    impl MockLinkFacility {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
                trap_exit: Mutex::new(false),
            }
        }

        fn records(&self) -> Vec<LinkRecord> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    impl LinkFacility for MockLinkFacility {
        fn link(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(LinkRecord::Link {
                    caller_pid,
                    target_pid,
                });
            Ok(())
        }

        fn unlink(&self, caller_pid: u64, target_pid: u64) -> Result<(), LinkError> {
            self.records
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(LinkRecord::Unlink {
                    caller_pid,
                    target_pid,
                });
            Ok(())
        }

        fn set_trap_exit(&self, _caller_pid: u64, value: bool) -> Result<bool, LinkError> {
            let mut trap = self.trap_exit.lock().unwrap_or_else(|e| e.into_inner());
            let old = *trap;
            *trap = value;
            Ok(old)
        }
    }

    struct NoprocLinkFacility;

    impl LinkFacility for NoprocLinkFacility {
        fn link(&self, _caller_pid: u64, _target_pid: u64) -> Result<(), LinkError> {
            Err(LinkError::NoProc)
        }

        fn unlink(&self, _caller_pid: u64, _target_pid: u64) -> Result<(), LinkError> {
            Err(LinkError::NoProc)
        }

        fn set_trap_exit(&self, _caller_pid: u64, _value: bool) -> Result<bool, LinkError> {
            Err(LinkError::NoCaller)
        }
    }

    // --- erlang:link/1 tests ---

    #[test]
    fn link_establishes_bidirectional_link_via_facility() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility.clone()));

        let result = bif_link(&[Term::pid(2)], &mut context);
        assert_eq!(result, Ok(Term::atom(Atom::TRUE)));

        let records = facility.records();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0],
            LinkRecord::Link {
                caller_pid: 1,
                target_pid: 2,
            }
        );
    }

    #[test]
    fn link_self_is_noop_returning_true() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(5));
        context.set_link_facility(Some(facility.clone()));

        let result = bif_link(&[Term::pid(5)], &mut context);
        assert_eq!(result, Ok(Term::atom(Atom::TRUE)));
        assert!(facility.records().is_empty());
    }

    #[test]
    fn link_returns_noproc_when_target_does_not_exist() {
        let facility = Arc::new(NoprocLinkFacility);
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility));

        let result = bif_link(&[Term::pid(99)], &mut context);
        assert_eq!(result, Err(Term::atom(Atom::NOPROC)));
    }

    #[test]
    fn link_returns_badarg_for_non_pid_argument() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));

        let result = bif_link(&[Term::small_int(42)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn link_returns_badarg_without_pid() {
        let mut context = ProcessContext::new();
        let result = bif_link(&[Term::pid(2)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn link_returns_badarg_without_link_facility() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        let result = bif_link(&[Term::pid(2)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn link_returns_badarg_with_wrong_arity() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        let result = bif_link(&[], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    // --- erlang:unlink/1 tests ---

    #[test]
    fn unlink_removes_link_via_facility() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility.clone()));

        let result = bif_unlink(&[Term::pid(2)], &mut context);
        assert_eq!(result, Ok(Term::atom(Atom::TRUE)));

        let records = facility.records();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0],
            LinkRecord::Unlink {
                caller_pid: 1,
                target_pid: 2,
            }
        );
    }

    #[test]
    fn unlink_self_is_noop_returning_true() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(5));
        context.set_link_facility(Some(facility.clone()));

        let result = bif_unlink(&[Term::pid(5)], &mut context);
        assert_eq!(result, Ok(Term::atom(Atom::TRUE)));
        assert!(facility.records().is_empty());
    }

    #[test]
    fn unlink_returns_badarg_for_non_pid_argument() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));

        let result = bif_unlink(&[Term::atom(Atom::OK)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn unlink_returns_badarg_without_pid() {
        let mut context = ProcessContext::new();
        let result = bif_unlink(&[Term::pid(2)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn unlink_returns_badarg_without_link_facility() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        let result = bif_unlink(&[Term::pid(2)], &mut context);
        assert_eq!(result, Err(badarg()));
    }

    // --- erlang:process_flag/2 tests ---

    #[test]
    fn process_flag_trap_exit_sets_and_returns_previous_value() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility));

        let result = bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut context,
        );
        assert_eq!(result, Ok(Term::atom(Atom::FALSE)));
    }

    #[test]
    fn process_flag_trap_exit_returns_old_true_when_already_set() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility));

        bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut context,
        )
        .unwrap();

        let result = bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::FALSE)],
            &mut context,
        );
        assert_eq!(result, Ok(Term::atom(Atom::TRUE)));
    }

    #[test]
    fn process_flag_returns_badarg_for_unknown_flag() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility));

        let result = bif_process_flag(
            &[Term::atom(Atom::OK), Term::atom(Atom::TRUE)],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn process_flag_returns_badarg_for_non_atom_flag() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));

        let result = bif_process_flag(
            &[Term::small_int(42), Term::atom(Atom::TRUE)],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn process_flag_returns_badarg_for_non_bool_value() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        context.set_link_facility(Some(facility));

        let result = bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::small_int(1)],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn process_flag_returns_badarg_without_pid() {
        let facility = Arc::new(MockLinkFacility::new());
        let mut context = ProcessContext::new();
        context.set_link_facility(Some(facility));

        let result = bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn process_flag_returns_badarg_without_link_facility() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));

        let result = bif_process_flag(
            &[Term::atom(Atom::TRAP_EXIT), Term::atom(Atom::TRUE)],
            &mut context,
        );
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn process_flag_returns_badarg_with_wrong_arity() {
        let mut context = ProcessContext::new();
        context.set_pid(Some(1));
        let result = bif_process_flag(&[Term::atom(Atom::TRAP_EXIT)], &mut context);
        assert_eq!(result, Err(badarg()));
    }
}
