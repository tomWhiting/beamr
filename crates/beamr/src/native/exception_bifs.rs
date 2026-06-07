//! Exception BIFs — erlang:raise/3.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

type ExceptionBif = (&'static str, u8, Capability, NativeFn);

const EXCEPTION_BIFS: &[ExceptionBif] = &[("raise", 3, Capability::Pure, bif_raise_3)];

/// Registers exception BIFs into the VM-owned BIF registry.
pub fn register_exception_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, capability, native_function) in EXCEPTION_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }

    Ok(())
}

/// erlang:raise/3 — raises `Reason` with the supplied class and stacktrace.
pub fn bif_raise_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [class, reason, stacktrace] = args else {
        return Err(badarg());
    };

    if !is_exception_class(*class) {
        return Err(badarg());
    }

    context.set_exception_class(*class);
    context.set_exception_stacktrace(*stacktrace);
    Err(*reason)
}

fn is_exception_class(class: Term) -> bool {
    class == Term::atom(Atom::ERROR)
        || class == Term::atom(Atom::THROW)
        || class == Term::atom(Atom::EXIT_CLASS)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::{bif_raise_3, register_exception_bifs};
    use crate::atom::{Atom, AtomTable};
    use crate::native::{BifRegistryImpl, ProcessContext};
    use crate::term::Term;

    #[test]
    fn bif_raise_3_sets_exception_metadata_and_returns_reason_error() {
        let mut context = ProcessContext::new();
        let class = Term::atom(Atom::THROW);
        let reason = Term::atom(Atom::BADMATCH);
        let stacktrace = Term::small_int(123);

        assert_eq!(
            bif_raise_3(&[class, reason, stacktrace], &mut context),
            Err(reason)
        );
        assert_eq!(context.take_exception_class(), class);
        assert_eq!(context.take_exception_stacktrace(), stacktrace);
    }

    #[test]
    fn bif_raise_3_rejects_invalid_class() {
        let mut context = ProcessContext::new();

        assert_eq!(
            bif_raise_3(
                &[
                    Term::atom(Atom::OK),
                    Term::atom(Atom::BADARG),
                    Term::small_int(456)
                ],
                &mut context
            ),
            Err(Term::atom(Atom::BADARG))
        );
        assert_eq!(context.take_exception_class(), Term::atom(Atom::ERROR));
        assert_eq!(context.take_exception_stacktrace(), Term::NIL);
    }

    #[test]
    fn register_exception_bifs_registers_raise_3() {
        let registry = BifRegistryImpl::new();
        let atom_table = AtomTable::new();

        register_exception_bifs(&registry, &atom_table).expect("exception BIF registration");

        let erlang = atom_table.intern("erlang");
        let raise = atom_table.intern("raise");
        assert!(registry.lookup(erlang, raise, 3).is_some());
    }
}
