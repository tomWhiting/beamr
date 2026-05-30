//! Process creation BIFs — self, spawn, spawn_link.
//!
//! These BIFs provide the ability to query the calling process's PID and to
//! create new processes from BEAM code. They are registered as Gate 2 BIFs
//! alongside the Gate 1 arithmetic, comparison, and utility functions.

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, NativeFn, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::Cons;

type Gate2Bif = (&'static str, u8, NativeFn);

const GATE2_BIFS: &[Gate2Bif] = &[
    ("self", 0, bif_self),
    ("spawn", 3, bif_spawn),
    ("spawn_link", 3, bif_spawn_link),
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

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::{bif_self, bif_spawn, bif_spawn_link, list_to_vec, register_gate2_bifs};
    use crate::atom::{Atom, AtomTable};
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
        for (name, arity) in [("self", 0), ("spawn", 3), ("spawn_link", 3)] {
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
}
