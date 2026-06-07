//! ETF-related Erlang BIFs.

use crate::atom::{Atom, AtomTable};
use crate::etf::decode::decode_term;
use crate::etf::encode::encode_term;
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::binary::Binary;

const ETF_BIFS: &[(&str, u8, Capability, NativeFn)] = &[
    ("term_to_binary", 1, Capability::Pure, bif_term_to_binary),
    ("binary_to_term", 1, Capability::Pure, bif_binary_to_term),
];

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

pub fn bif_binary_to_term(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [arg] = args else {
        return Err(badarg());
    };
    let binary = Binary::new(*arg).ok_or_else(badarg)?;
    let bytes = binary.as_bytes();
    let atom_table = context.atom_table_arc().ok_or_else(badarg)?;
    let process = context.process_mut().ok_or_else(badarg)?;
    decode_term(bytes, process, &atom_table).map_err(|_| badarg())
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
    use crate::term::binary::{Binary, write_binary};
    use crate::term::boxed::{Cons, Map, Tuple, write_cons, write_map, write_tuple};

    use super::{bif_binary_to_term, bif_term_to_binary, register_etf_bifs};

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
    fn register_etf_bifs_registers_term_to_binary_and_binary_to_term() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();

        register_etf_bifs(&registry, &atom_table).expect("ETF BIF registration");

        let erlang = atom_table.intern("erlang");
        for function_name in ["term_to_binary", "binary_to_term"] {
            let function = atom_table.intern(function_name);
            let capability = registry
                .lookup(erlang, function, 1)
                .map(|entry| entry.capability);
            assert_eq!(capability, Some(Capability::Pure), "{function_name}");
        }
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

    #[test]
    fn binary_to_term_decodes_small_integer_binary() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 128);
        let mut binary_heap = [0_u64; 3];
        let binary = write_binary(
            &mut binary_heap,
            &[tags::VERSION, tags::SMALL_INTEGER_EXT, 42],
        )
        .expect("ETF binary");
        let mut context = ctx_with_atoms(&mut process, atom_table);

        assert_eq!(
            bif_binary_to_term(&[binary], &mut context),
            Ok(Term::small_int(42))
        );
    }

    #[test]
    fn binary_to_term_returns_badarg_for_wrong_arity_non_binary_and_invalid_etf() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 128);
        let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
        assert_eq!(bif_binary_to_term(&[], &mut context), Err(badarg()));
        assert_eq!(
            bif_binary_to_term(&[Term::small_int(42)], &mut context),
            Err(badarg())
        );

        let mut invalid_heap = [0_u64; 2];
        let invalid = write_binary(&mut invalid_heap, &[0]).expect("invalid ETF binary");
        assert_eq!(bif_binary_to_term(&[invalid], &mut context), Err(badarg()));
    }

    #[test]
    fn binary_to_term_round_trips_atoms_integers_tuples_lists_maps_and_binaries() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());

        round_trip(Term::atom(Atom::OK), Arc::clone(&atom_table));
        round_trip(Term::small_int(42), Arc::clone(&atom_table));

        let mut tuple_heap = [0_u64; 3];
        let tuple = write_tuple(
            &mut tuple_heap,
            &[Term::atom(Atom::OK), Term::small_int(42)],
        )
        .expect("tuple");
        round_trip(tuple, Arc::clone(&atom_table));

        let mut cells = [[0_u64; 2]; 3];
        let mut list = Term::NIL;
        for (index, value) in [1, 2, 3].iter().copied().enumerate().rev() {
            list = write_cons(&mut cells[index], Term::small_int(value), list).expect("cons");
        }
        round_trip(list, Arc::clone(&atom_table));

        let mut map_heap = [0_u64; 4];
        let map = write_map(
            &mut map_heap,
            &[Term::atom(Atom::OK)],
            &[Term::small_int(1)],
        )
        .expect("map");
        round_trip(map, Arc::clone(&atom_table));

        let mut binary_heap = [0_u64; 3];
        let binary = write_binary(&mut binary_heap, &[1, 2, 3]).expect("binary");
        round_trip(binary, atom_table);
    }

    fn round_trip(term: Term, atom_table: Arc<AtomTable>) {
        let mut encode_process = Process::new(1, 256);
        let mut encode_context = ctx_with_atoms(&mut encode_process, Arc::clone(&atom_table));
        let encoded = bif_term_to_binary(&[term], &mut encode_context).expect("encode BIF");

        let mut decode_process = Process::new(2, 256);
        let mut decode_context = ctx_with_atoms(&mut decode_process, atom_table);
        let decoded = bif_binary_to_term(&[encoded], &mut decode_context).expect("decode BIF");
        assert_eq!(decoded, term);

        if let Some(list) = Cons::new(decoded) {
            assert_eq!(list.head(), Term::small_int(1));
        }
        if let Some(map) = Map::new(decoded) {
            assert_eq!(map.len(), 1);
        }
        if let Some(tuple) = Tuple::new(decoded) {
            assert!(tuple.arity() > 0);
        }
    }
}
