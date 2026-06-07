//! ETF-related Erlang BIFs.

use crate::atom::{Atom, AtomTable};
use crate::etf::decode::{DecodeOptions, decode_term, decode_term_with_options};
use crate::etf::encode::{EncodeOptions, encode_term, encode_term_with_options};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;
use crate::term::binary::Binary;
use crate::term::boxed::{Cons, Tuple};

const ETF_BIFS: &[(&str, u8, Capability, NativeFn)] = &[
    ("term_to_binary", 1, Capability::Pure, bif_term_to_binary),
    ("term_to_binary", 2, Capability::Pure, bif_term_to_binary_2),
    ("binary_to_term", 1, Capability::Pure, bif_binary_to_term),
    ("binary_to_term", 2, Capability::Pure, bif_binary_to_term_2),
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

pub fn bif_term_to_binary_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [term, options] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table().ok_or_else(badarg)?;
    let options = parse_encode_options(*options, atom_table)?;
    let bytes = encode_term_with_options(*term, atom_table, options).map_err(|_| badarg())?;
    context.alloc_binary(&bytes)
}

pub fn bif_binary_to_term(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table_arc().ok_or_else(badarg)?;
    let bytes = Binary::new(*binary).ok_or_else(badarg)?.as_bytes();
    decode_term(bytes, context, atom_table.as_ref()).map_err(|_| badarg())
}

pub fn bif_binary_to_term_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [binary, options_term] = args else {
        return Err(badarg());
    };
    let atom_table = context.atom_table_arc().ok_or_else(badarg)?;
    let options = parse_decode_options(*options_term, atom_table.as_ref())?;
    let bytes = Binary::new(*binary).ok_or_else(badarg)?.as_bytes();
    let decoded = decode_term_with_options(bytes, context, atom_table.as_ref(), options)
        .map_err(|_| badarg())?;
    if options.return_used {
        let used = i64::try_from(decoded.used).map_err(|_| badarg())?;
        let used_term = Term::try_small_int(used).ok_or_else(badarg)?;
        context.alloc_tuple(&[decoded.term, used_term])
    } else {
        Ok(decoded.term)
    }
}

fn parse_encode_options(options: Term, atom_table: &AtomTable) -> Result<EncodeOptions, Term> {
    let mut parsed = EncodeOptions::default();
    let mut current = options;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let option = cons.head();
        if let Some(atom) = option.as_atom() {
            match atom_table.resolve(atom) {
                Some("compressed") => parsed.compression_level = Some(6),
                _ => return Err(badarg()),
            }
        } else if let Some(tuple) = Tuple::new(option) {
            if tuple.arity() != 2 {
                return Err(badarg());
            }
            let key = tuple.get(0).ok_or_else(badarg)?;
            let value = tuple.get(1).ok_or_else(badarg)?;
            let key_atom = key.as_atom().ok_or_else(badarg)?;
            match atom_table.resolve(key_atom) {
                Some("compressed") => {
                    let level = value.as_small_int().ok_or_else(badarg)?;
                    if !(0..=9).contains(&level) {
                        return Err(badarg());
                    }
                    parsed.compression_level = Some(u32::try_from(level).map_err(|_| badarg())?);
                }
                Some("minor_version") => {
                    if value.as_small_int() != Some(2) {
                        return Err(badarg());
                    }
                }
                _ => return Err(badarg()),
            }
        } else {
            return Err(badarg());
        }
        current = cons.tail();
    }
    Ok(parsed)
}

fn parse_decode_options(options: Term, atom_table: &AtomTable) -> Result<DecodeOptions, Term> {
    let mut parsed = DecodeOptions::default();
    let mut current = options;
    while !current.is_nil() {
        let cons = Cons::new(current).ok_or_else(badarg)?;
        let option_atom = cons.head().as_atom().ok_or_else(badarg)?;
        match atom_table.resolve(option_atom) {
            Some("safe") => parsed.safe = true,
            Some("used") => parsed.return_used = true,
            _ => return Err(badarg()),
        }
        current = cons.tail();
    }
    Ok(parsed)
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
    use crate::term::boxed::{Tuple, write_cons, write_tuple};

    use super::{
        bif_binary_to_term, bif_binary_to_term_2, bif_term_to_binary, bif_term_to_binary_2,
        parse_decode_options, parse_encode_options, register_etf_bifs,
    };

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
    fn register_etf_bifs_registers_term_to_binary_and_binary_to_term_arities() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();

        register_etf_bifs(&registry, &atom_table).expect("ETF BIF registration");

        let erlang = atom_table.intern("erlang");
        for (name, arity) in [
            ("term_to_binary", 1),
            ("term_to_binary", 2),
            ("binary_to_term", 1),
            ("binary_to_term", 2),
        ] {
            let function = atom_table.intern(name);
            let entry = registry
                .lookup(erlang, function, arity)
                .expect("ETF BIF should be registered");
            assert_eq!(entry.capability, Capability::Pure);
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
    fn term_to_binary_2_compresses_large_terms_and_round_trips() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let compressed_atom = atom_table.intern("compressed");
        let mut process = Process::new(1, 2048);
        let mut cells = vec![[0_u64; 2]; 512];
        let mut list = Term::NIL;
        for cell in cells.iter_mut().rev() {
            list = write_cons(cell, Term::small_int(7), list).expect("cons");
        }
        let option = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_list(&[Term::atom(compressed_atom)])
                .expect("option list")
        };
        let uncompressed = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary(&[list], &mut context).expect("uncompressed")
        };
        let compressed = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary_2(&[list, option], &mut context).expect("compressed")
        };
        let uncompressed_bytes = Binary::new(uncompressed)
            .expect("uncompressed binary")
            .as_bytes();
        let compressed_bytes = Binary::new(compressed)
            .expect("compressed binary")
            .as_bytes();
        assert!(compressed_bytes.len() < uncompressed_bytes.len());
        assert_eq!(compressed_bytes[0], tags::VERSION);
        assert_eq!(compressed_bytes[1], tags::COMPRESSED_EXT);

        let decoded = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_binary_to_term(&[compressed], &mut context).expect("decode")
        };
        assert_eq!(decoded, list);
    }

    #[test]
    fn term_to_binary_2_compressed_zero_is_uncompressed() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let compressed_atom = atom_table.intern("compressed");
        let mut process = Process::new(1, 128);
        let option = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            let tuple = context
                .alloc_tuple(&[Term::atom(compressed_atom), Term::small_int(0)])
                .expect("tuple");
            context.alloc_list(&[tuple]).expect("options")
        };
        let uncompressed = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary(&[Term::small_int(42)], &mut context).expect("uncompressed")
        };
        let encoded = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary_2(&[Term::small_int(42), option], &mut context).expect("encoded")
        };
        assert_eq!(
            Binary::new(encoded).expect("encoded").as_bytes(),
            Binary::new(uncompressed).expect("uncompressed").as_bytes()
        );
    }

    #[test]
    fn term_to_binary_2_accepts_minor_version_two_noop() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let minor_version_atom = atom_table.intern("minor_version");
        let mut process = Process::new(1, 128);
        let option = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            let tuple = context
                .alloc_tuple(&[Term::atom(minor_version_atom), Term::small_int(2)])
                .expect("tuple");
            context.alloc_list(&[tuple]).expect("options")
        };
        let uncompressed = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary(&[Term::small_int(42)], &mut context).expect("uncompressed")
        };
        let encoded = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary_2(&[Term::small_int(42), option], &mut context).expect("encoded")
        };
        assert_eq!(
            Binary::new(encoded).expect("encoded").as_bytes(),
            Binary::new(uncompressed).expect("uncompressed").as_bytes()
        );
    }

    #[test]
    fn term_to_binary_2_rejects_malformed_encode_options() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let compressed_atom = atom_table.intern("compressed");
        let mut process = Process::new(1, 128);
        let out_of_range_option = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            let tuple = context
                .alloc_tuple(&[Term::atom(compressed_atom), Term::small_int(10)])
                .expect("tuple");
            context.alloc_list(&[tuple]).expect("options")
        };
        let improper_options = {
            let mut cell = [0_u64; 2];
            write_cons(&mut cell, Term::atom(compressed_atom), Term::small_int(0))
                .expect("improper options")
        };

        for options in [Term::small_int(0), out_of_range_option, improper_options] {
            let result = {
                let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
                bif_term_to_binary_2(&[Term::small_int(42), options], &mut context)
            };
            assert_eq!(result, Err(badarg()));
        }
    }

    #[test]
    fn parse_decode_options_rejects_non_atoms_and_improper_lists() {
        let atom_table = AtomTable::with_common_atoms();
        let safe_atom = atom_table.intern("safe");
        let mut cell = [0_u64; 2];
        let improper_options = write_cons(&mut cell, Term::atom(safe_atom), Term::small_int(0))
            .expect("improper options");

        assert_eq!(
            parse_decode_options(Term::small_int(0), &atom_table),
            Err(badarg())
        );
        assert_eq!(
            parse_decode_options(improper_options, &atom_table),
            Err(badarg())
        );
        assert_eq!(
            parse_decode_options(Term::NIL, &atom_table).expect("empty options"),
            Default::default()
        );
        assert_eq!(
            parse_encode_options(Term::NIL, &atom_table).expect("empty options"),
            Default::default()
        );
    }

    #[test]
    fn binary_to_term_2_safe_known_atom_succeeds_and_novel_atom_fails() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let safe_atom = atom_table.intern("safe");
        let hello = atom_table.intern("hello");
        let mut process = Process::new(1, 128);
        let options = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_list(&[Term::atom(safe_atom)])
                .expect("options")
        };
        let hello_binary = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_term_to_binary(&[Term::atom(hello)], &mut context).expect("hello binary")
        };
        let decoded = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_binary_to_term_2(&[hello_binary, options], &mut context).expect("decode safe")
        };
        assert_eq!(decoded, Term::atom(hello));

        let novel_binary = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_binary(&[
                    tags::VERSION,
                    tags::SMALL_ATOM_UTF8_EXT,
                    5,
                    b'n',
                    b'o',
                    b'v',
                    b'e',
                    b'l',
                ])
                .expect("novel binary")
        };
        let result = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_binary_to_term_2(&[novel_binary, options], &mut context)
        };
        assert_eq!(result, Err(badarg()));
    }

    #[test]
    fn binary_to_term_2_used_returns_term_and_bytes_used() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let used_atom = atom_table.intern("used");
        let mut process = Process::new(1, 128);
        let options = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_list(&[Term::atom(used_atom)])
                .expect("options")
        };
        let binary = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_binary(&[tags::VERSION, tags::SMALL_INTEGER_EXT, 42, 99])
                .expect("binary")
        };
        let result = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_binary_to_term_2(&[binary, options], &mut context).expect("used result")
        };
        let tuple = Tuple::new(result).expect("used tuple");
        assert_eq!(tuple.get(0), Some(Term::small_int(42)));
        assert_eq!(tuple.get(1), Some(Term::small_int(3)));
    }

    #[test]
    fn binary_to_term_1_rejects_trailing_bytes() {
        let atom_table = Arc::new(AtomTable::with_common_atoms());
        let mut process = Process::new(1, 128);
        let binary = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            context
                .alloc_binary(&[tags::VERSION, tags::SMALL_INTEGER_EXT, 42, 99])
                .expect("binary")
        };
        let result = {
            let mut context = ctx_with_atoms(&mut process, Arc::clone(&atom_table));
            bif_binary_to_term(&[binary], &mut context)
        };
        assert_eq!(result, Err(badarg()));
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
