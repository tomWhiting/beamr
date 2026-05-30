//! Process registry BIFs — register/2, unregister/1, whereis/1.

use crate::atom::Atom;
use crate::native::ProcessContext;
use crate::term::Term;

/// erlang:register/2 — associates an atom name with a PID.
///
/// Fails if the name is already registered or the PID is already registered
/// under another name.
pub fn bif_register(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term, pid_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let pid = pid_term.as_pid().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    facility.register(name, pid).map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

/// erlang:unregister/1 — removes the registration for an atom name.
///
/// Fails if the name is not currently registered.
pub fn bif_unregister(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    facility.unregister(name).map_err(|_| badarg())?;
    Ok(Term::atom(Atom::TRUE))
}

/// erlang:whereis/1 — returns the PID registered under `name`, or
/// the atom `undefined`.
pub fn bif_whereis(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [name_term] = args else {
        return Err(badarg());
    };
    let name = name_term.as_atom().ok_or_else(badarg)?;
    let facility = context.registry_facility().ok_or_else(badarg)?;
    match facility.whereis(name) {
        Some(pid) => Term::try_pid(pid).ok_or_else(badarg),
        None => Ok(Term::atom(Atom::UNDEFINED)),
    }
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
#[path = "registry_bif_tests.rs"]
mod tests;
