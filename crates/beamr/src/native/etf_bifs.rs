//! ETF-related Erlang BIFs.

use crate::atom::{Atom, AtomTable};
use crate::etf::encode::encode_term;
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

const ETF_BIFS: &[(&str, u8, Capability, NativeFn)] =
    &[("term_to_binary", 1, Capability::Pure, bif_term_to_binary)];

pub fn register_etf_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    for &(function_name, arity, capability, native_function) in ETF_BIFS {
        let function = atom_table.intern(function_name);
        registry.register(erlang, function, arity, native_function, capability)?;
    }
    Ok(())
}

pub fn bif_term_to_binary(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let bytes = encode_term(*term, atom_table).map_err(|_| badarg())?;
    context.alloc_binary(&bytes)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::atom::{Atom, AtomTable};
    use crate::etf::tags;
    use crate::native::{BifRegistryImpl, Capability, ProcessContext};
    use crate::process::Process;
    use crate::term::Term;
    use crate::term::binary::Binary;
    use crate::term::boxed::write_tuple;

    use super::{bif_term_to_binary, register_etf_bifs};

    fn badarg() -> Term {
        Term::atom(Atom::BADARG)
    }

    fn ctx_with_atoms<'process>(
        process: &'process mut Process,
        atom_table: Arc<AtomTable>,
    ) -> ProcessContext<'process> {
        let mut context = ProcessContext::new();
        context.set_atom_table(Some(atom_table));
        context.attach_process(process, 0);
        context
    }

    #[test]
    fn register_etf_bifs_registers_term_to_binary() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();

        register_etf_bifs(&registry, &atom_table).expect("ETF BIF registration");

        let erlang = atom_table.intern("erlang");
        let function = atom_table.intern("term_to_binary");
        let entry = registry
            .lookup(erlang, function, 1)
            .expect("term_to_binary should be registered");
        assert_eq!(entry.capability, Capability::Pure);
    }

    #[test]
    fn term_to_binary_returns_etf_binary_for_tuple() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 128);
        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("tuple");
        let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));

        let result = bif_term_to_binary(&[tuple], &mut context).expect("term_to_binary result");
        let binary = Binary::new(result).expect("result should be binary");
        assert_eq!(
            binary.as_bytes(),
            &[
                tags::VERSION,
                tags::SMALL_TUPLE_EXT,
                2,
                tags::SMALL_ATOM_UTF8_EXT,
                2,
                b'o',
                b'k',
                tags::SMALL_INTEGER_EXT,
                42,
            ]
        );
    }

    #[test]
    fn term_to_binary_returns_badarg_without_atom_table() {
        let mut process = Process::new(1, 64);
        let mut context = ProcessContext::new();
        context.attach_process(&mut process, 0);

        assert_eq!(
            bif_term_to_binary(&[Term::atom(Atom::OK)], &mut context),
            Err(badarg())
        );
    }

    #[test]
    fn term_to_binary_returns_badarg_for_wrong_arity() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 64);
        let mut context = ctx_with_atoms(&mut process, atom_table);

        assert_eq!(bif_term_to_binary(&[], &mut context), Err(badarg()));
    }
}
