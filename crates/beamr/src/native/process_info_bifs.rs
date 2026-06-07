//! Process metadata BIFs.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

type ProcessInfoBif = (&'static str, u8, NativeFn);

const PROCESS_INFO_BIFS: &[ProcessInfoBif] = &[
    ("group_leader", 0, bif_group_leader_0),
    ("group_leader", 2, bif_group_leader_2),
];

/// Registers process metadata BIFs.
pub fn register_process_info_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in PROCESS_INFO_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(
            erlang,
            function,
            arity,
            native_function,
            Capability::ProcessLocal,
        )?;
    }

    Ok(())
}

/// erlang:group_leader/0 — returns the calling process's group leader PID.
pub fn bif_group_leader_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    context.group_leader()
}

/// erlang:group_leader/2 — sets `Pid`'s group leader to `NewLeader`.
pub fn bif_group_leader_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [new_leader, pid] = args else {
        return Err(badarg());
    };
    if !new_leader.is_pid() {
        return Err(badarg());
    }
    let Some(target_pid) = pid.as_pid() else {
        return Err(badarg());
    };

    let facility = context.group_leader_facility().ok_or_else(badarg)?;
    facility
        .set_group_leader(target_pid, *new_leader)
        .map_err(|_| badarg())?;

    Ok(Term::atom(Atom::TRUE))
}

const fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{bif_group_leader_0, bif_group_leader_2};
    use crate::atom::Atom;
    use crate::native::ProcessContext;
    use crate::native::group_leader::{GroupLeaderError, GroupLeaderFacility};
    use crate::process::Process;
    use crate::term::Term;

    #[derive(Debug)]
    struct MockGroupLeaderFacility {
        pid: u64,
        group_leader: Mutex<Term>,
    }

    impl MockGroupLeaderFacility {
        fn new(pid: u64, group_leader: Term) -> Self {
            Self {
                pid,
                group_leader: Mutex::new(group_leader),
            }
        }

        fn current_group_leader(&self) -> Term {
            *self
                .group_leader
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        }
    }

    impl GroupLeaderFacility for MockGroupLeaderFacility {
        fn set_group_leader(&self, pid: u64, leader: Term) -> Result<(), GroupLeaderError> {
            if pid != self.pid {
                return Err(GroupLeaderError::NoProc);
            }
            *self
                .group_leader
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = leader;
            Ok(())
        }

        fn group_leader(&self, pid: u64) -> Result<Term, GroupLeaderError> {
            if pid != self.pid {
                return Err(GroupLeaderError::NoProc);
            }
            Ok(self.current_group_leader())
        }
    }

    #[test]
    fn group_leader_0_returns_attached_process_group_leader() {
        let mut process = Process::new(7, 64);
        process.set_group_leader(Term::pid(3));
        let mut context = ProcessContext::new();
        context.attach_process(&mut process, 0);

        assert_eq!(bif_group_leader_0(&[], &mut context), Ok(Term::pid(3)));
    }

    #[test]
    fn group_leader_2_sets_target_group_leader_and_returns_true() {
        let facility = Arc::new(MockGroupLeaderFacility::new(9, Term::pid(1)));
        let facility_for_context: Arc<dyn GroupLeaderFacility> = facility.clone();
        let mut context = ProcessContext::new();
        context.set_group_leader_facility(Some(facility_for_context));

        assert_eq!(
            bif_group_leader_2(&[Term::pid(4), Term::pid(9)], &mut context),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(facility.current_group_leader(), Term::pid(4));
    }

    #[test]
    fn group_leader_2_rejects_non_pid_args_and_missing_process() {
        let facility = Arc::new(MockGroupLeaderFacility::new(9, Term::pid(1)));
        let facility_for_context: Arc<dyn GroupLeaderFacility> = facility;
        let mut context = ProcessContext::new();
        context.set_group_leader_facility(Some(facility_for_context));

        assert_eq!(
            bif_group_leader_2(&[Term::atom(Atom::OK), Term::pid(9)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
        assert_eq!(
            bif_group_leader_2(&[Term::pid(4), Term::atom(Atom::OK)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
        assert_eq!(
            bif_group_leader_2(&[Term::pid(4), Term::pid(10)], &mut context),
            Err(Term::atom(Atom::BADARG))
        );
    }
}
