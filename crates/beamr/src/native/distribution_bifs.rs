//! Distribution-related BIFs.

use crate::atom::{Atom, AtomTable};
use crate::distribution::global::{GlobalNameEntry, GlobalNameError, GlobalPid};
use crate::native::{BifRegistryImpl, Capability, NativeRegistrationError, ProcessContext};
use crate::term::Term;
use crate::term::boxed::Closure;
use crate::term::pid_ref::PidRef;

/// Facility backing `global:*_name` BIFs.
pub trait GlobalNameFacility: Send + Sync {
    /// Register a name for `pid` with an optional custom resolver callback.
    fn register_name(
        &self,
        name: Atom,
        pid: GlobalPid,
        resolver: Option<Term>,
    ) -> Result<(), GlobalNameError>;

    /// Look up a registered global name.
    fn whereis_name(&self, name: Atom) -> Option<GlobalNameEntry>;

    /// Remove a registration owned by `pid`.
    fn unregister_name(&self, name: Atom, pid: GlobalPid) -> Result<(), GlobalNameError>;
}

/// Register distribution BIFs under their Erlang module names.
pub fn register_distribution_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let global = atom_table.intern("global");
    let register_name = atom_table.intern("register_name");
    let whereis_name = atom_table.intern("whereis_name");
    let unregister_name = atom_table.intern("unregister_name");

    registry.register(
        global,
        register_name,
        2,
        bif_global_register_name_2,
        Capability::Pure,
    )?;
    registry.register(
        global,
        register_name,
        3,
        bif_global_register_name_3,
        Capability::Pure,
    )?;
    registry.register(
        global,
        whereis_name,
        1,
        bif_global_whereis_name,
        Capability::Pure,
    )?;
    registry.register(
        global,
        unregister_name,
        1,
        bif_global_unregister_name,
        Capability::Pure,
    )?;
    Ok(())
}

/// global:register_name(Name, Pid).
pub fn bif_global_register_name_2(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [name_term, pid_term] = args else {
        return Err(badarg());
    };
    register_name(*name_term, *pid_term, None, context)
}

/// global:register_name(Name, Pid, ResolveFun).
pub fn bif_global_register_name_3(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [name_term, pid_term, resolver] = args else {
        return Err(badarg());
    };
    let Some(closure) = Closure::new(*resolver) else {
        return Err(badarg());
    };
    if closure.arity() != 3 {
        return Err(badarg());
    }
    register_name(*name_term, *pid_term, Some(*resolver), context)
}

/// global:whereis_name(Name).
pub fn bif_global_whereis_name(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let facility = context.global_name_facility().ok_or_else(badarg)?;
    let local_node = context.local_node().ok_or_else(badarg)?;

    match facility.whereis_name(name) {
        Some(entry) if entry.pid.node == local_node.name => {
            Term::try_pid(entry.pid.pid_number).ok_or_else(badarg)
        }
        Some(entry) => {
            context.alloc_external_pid(entry.pid.node, entry.pid.pid_number, entry.pid.serial)
        }
        None => Ok(Term::atom(Atom::UNDEFINED)),
    }
}

/// global:unregister_name(Name).
pub fn bif_global_unregister_name(
    args: &[Term],
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let pid = context.pid().ok_or_else(badarg)?;
    let local_node = context.local_node().ok_or_else(badarg)?;
    let facility = context.global_name_facility().ok_or_else(badarg)?;
    facility
        .unregister_name(name, GlobalPid::new(local_node.name, pid, 0))
        .map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

fn register_name(
    name_term: Term,
    pid_term: Term,
    resolver: Option<Term>,
    context: &mut ProcessContext,
) -> Result<Term, Term> {
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let local_node = context.local_node().ok_or_else(badarg)?;
    let pid_ref = PidRef::new(pid_term).ok_or_else(badarg)?;
    let node = pid_ref.node().unwrap_or(local_node.name);
    let pid = GlobalPid::new(node, pid_ref.pid_number(), pid_ref.serial());
    let facility = context.global_name_facility().ok_or_else(badarg)?;
    facility
        .register_name(name, pid, resolver)
        .map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::distribution::Node;
    use crate::distribution::global::GlobalNameRegistry;
    use crate::native::ProcessContext;
    use crate::term::boxed::{write_closure, write_external_pid};

    use super::*;

    #[test]
    fn register_whereis_unregister_local_global_name() {
        let atoms = Arc::new(AtomTable::with_common_atoms());
        let node = Node::new(atoms.intern("a@host"), 0);
        let registry = Arc::new(GlobalNameRegistry::new(node, atoms.clone()));
        let name = atoms.intern("service");
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(10));
        ctx.set_local_node(Some(node));
        ctx.set_atom_table(Some(atoms));
        ctx.set_global_name_facility(Some(registry));

        assert_eq!(
            bif_global_register_name_2(&[Term::atom(name), Term::pid(10)], &mut ctx),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_global_whereis_name(&[Term::atom(name)], &mut ctx),
            Ok(Term::pid(10))
        );
        assert_eq!(
            bif_global_unregister_name(&[Term::atom(name)], &mut ctx),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            bif_global_whereis_name(&[Term::atom(name)], &mut ctx),
            Ok(Term::atom(Atom::UNDEFINED))
        );
    }

    #[test]
    fn whereis_returns_remote_pid_boxed_on_callers_heap() {
        let atoms = Arc::new(AtomTable::with_common_atoms());
        let local = Node::new(atoms.intern("a@host"), 0);
        let remote = atoms.intern("b@host");
        let registry = Arc::new(GlobalNameRegistry::new(local, atoms.clone()));
        let name = atoms.intern("remote_service");
        let mut remote_heap = [0_u64; 4];
        let remote_pid = write_external_pid(&mut remote_heap, remote, 55, 1)
            .expect("remote pid fits in test heap");

        let mut process = crate::process::Process::new(100, 128);
        let mut ctx = ProcessContext::new();
        ctx.set_local_node(Some(local));
        ctx.set_atom_table(Some(atoms));
        ctx.set_global_name_facility(Some(registry));
        ctx.attach_process(&mut process, 0);

        assert_eq!(
            bif_global_register_name_2(&[Term::atom(name), remote_pid], &mut ctx),
            Ok(Term::atom(Atom::TRUE))
        );
        let result = bif_global_whereis_name(&[Term::atom(name)], &mut ctx)
            .expect("whereis returns remote pid");
        let pid = PidRef::new(result).expect("remote pid ref");
        assert_eq!(pid.node(), Some(remote));
        assert_eq!(pid.pid_number(), 55);
        assert_eq!(pid.serial(), 1);
    }

    #[test]
    fn register_name_3_accepts_and_stores_ternary_resolver() {
        let atoms = Arc::new(AtomTable::with_common_atoms());
        let node = Node::new(atoms.intern("a@host"), 0);
        let registry = Arc::new(GlobalNameRegistry::new(node, atoms.clone()));
        let name = atoms.intern("resolver_service");
        let mut closure_heap = [0_u64; 7];
        let resolver = write_closure(&mut closure_heap, Atom::OK, 0, 3, 1, 0, &[])
            .expect("ternary resolver closure fits");

        let mut ctx = ProcessContext::new();
        ctx.set_local_node(Some(node));
        ctx.set_atom_table(Some(atoms));
        ctx.set_global_name_facility(Some(registry.clone()));

        assert_eq!(
            bif_global_register_name_3(&[Term::atom(name), Term::pid(10), resolver], &mut ctx),
            Ok(Term::atom(Atom::TRUE))
        );
        assert_eq!(
            registry.whereis(name).and_then(|entry| entry.resolver),
            Some(resolver)
        );
    }

    #[test]
    fn register_name_3_rejects_non_ternary_resolver() {
        let atoms = Arc::new(AtomTable::with_common_atoms());
        let node = Node::new(atoms.intern("a@host"), 0);
        let registry = Arc::new(GlobalNameRegistry::new(node, atoms.clone()));
        let name = atoms.intern("bad_resolver_service");
        let mut closure_heap = [0_u64; 7];
        let resolver = write_closure(&mut closure_heap, Atom::OK, 0, 2, 1, 0, &[])
            .expect("binary closure fits");

        let mut ctx = ProcessContext::new();
        ctx.set_local_node(Some(node));
        ctx.set_atom_table(Some(atoms));
        ctx.set_global_name_facility(Some(registry));

        assert_eq!(
            bif_global_register_name_3(&[Term::atom(name), Term::pid(10), resolver], &mut ctx),
            Err(Term::atom(Atom::BADARG))
        );
    }
}
