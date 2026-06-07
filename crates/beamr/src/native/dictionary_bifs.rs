//! Process dictionary BIFs.
//!
//! The dictionary is private to the calling process and is exposed through the
//! standard `erlang` process-dictionary BIFs.

use crate::atom::{Atom, AtomTable};
use crate::native::{
    BifRegistryImpl, Capability, NativeFn, NativeRegistrationError, ProcessContext,
};
use crate::term::Term;

type DictionaryBif = (&'static str, u8, NativeFn);

const DICTIONARY_BIFS: &[DictionaryBif] = &[
    ("put", 2, bif_put),
    ("get", 1, bif_get_1),
    ("get", 0, bif_get_0),
    ("erase", 1, bif_erase_1),
    ("erase", 0, bif_erase_0),
    ("get_keys", 1, bif_get_keys_1),
];

/// Registers all process dictionary BIFs.
pub fn register_dictionary_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");

    for &(function_name, arity, native_function) in DICTIONARY_BIFS {
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

/// erlang:put/2 — stores `Value` under `Key`, returning the old value or `undefined`.
pub fn bif_put(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key, value] = args else {
        return Err(badarg());
    };

    context.dict_put(*key, *value)
}

/// erlang:get/1 — returns the value for `Key`, or `undefined`.
pub fn bif_get_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key] = args else {
        return Err(badarg());
    };

    context.dict_get(*key)
}

/// erlang:get/0 — returns the full process dictionary as a list of `{Key, Value}` tuples.
pub fn bif_get_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    let entries = context.dict_get_all()?;
    entries_to_list(&entries, context)
}

/// erlang:erase/1 — removes `Key`, returning the old value or `undefined`.
pub fn bif_erase_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [key] = args else {
        return Err(badarg());
    };

    context.dict_erase(*key)
}

/// erlang:erase/0 — clears the dictionary and returns previous entries as tuples.
pub fn bif_erase_0(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    if !args.is_empty() {
        return Err(badarg());
    }

    let entries = context.dict_erase_all()?;
    entries_to_list(&entries, context)
}

/// erlang:get_keys/1 — returns all keys whose value exactly matches the argument.
pub fn bif_get_keys_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let [value] = args else {
        return Err(badarg());
    };

    let keys = context.dict_get_keys(*value)?;
    context.alloc_list(&keys)
}

fn entries_to_list(entries: &[(Term, Term)], context: &mut ProcessContext) -> Result<Term, Term> {
    let mut tuples = Vec::with_capacity(entries.len());
    for &(key, value) in entries {
        tuples.push(context.alloc_tuple(&[key, value])?);
    }
    context.alloc_list(&tuples)
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

#[cfg(test)]
mod tests {
    use super::{
        bif_erase_0, bif_erase_1, bif_get_0, bif_get_1, bif_get_keys_1, bif_put,
        register_dictionary_bifs,
    };
    use crate::atom::{Atom, AtomTable};
    use crate::native::{BifRegistryImpl, Capability, ProcessContext};
    use crate::process::Process;
    use crate::term::Term;
    use crate::term::boxed::{Cons, Tuple};

    fn context(process: &mut Process) -> ProcessContext<'_> {
        let mut context = ProcessContext::new();
        context.attach_process(process, 0);
        context
    }

    fn list_terms(list: Term) -> Vec<Term> {
        let mut values = Vec::new();
        let mut tail = list;
        while !tail.is_nil() {
            let cons = Cons::new(tail).expect("proper list cons");
            values.push(cons.head());
            tail = cons.tail();
        }
        values
    }

    fn tuple_pair(term: Term) -> (Term, Term) {
        let tuple = Tuple::new(term).expect("dictionary entry tuple");
        assert_eq!(tuple.arity(), 2);
        (
            tuple.get(0).expect("key element"),
            tuple.get(1).expect("value element"),
        )
    }

    #[test]
    fn dictionary_bifs_put_get_and_erase_round_trip() {
        let mut process = Process::new(1, 64);
        let mut context = context(&mut process);
        let key = Term::atom(Atom::OK);
        let value = Term::small_int(42);

        assert_eq!(
            bif_put(&[key, value], &mut context),
            Ok(Term::atom(Atom::UNDEFINED))
        );
        assert_eq!(bif_get_1(&[key], &mut context), Ok(value));
        assert_eq!(bif_erase_1(&[key], &mut context), Ok(value));
        assert_eq!(
            bif_get_1(&[key], &mut context),
            Ok(Term::atom(Atom::UNDEFINED))
        );
    }

    #[test]
    fn get_0_returns_complete_dictionary_as_tuple_list() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);
        bif_put(&[Term::atom(Atom::OK), Term::small_int(1)], &mut context).expect("put ok");
        bif_put(&[Term::atom(Atom::ERROR), Term::small_int(2)], &mut context).expect("put error");

        let list = bif_get_0(&[], &mut context).expect("get/0");
        let pairs: Vec<_> = list_terms(list).into_iter().map(tuple_pair).collect();

        assert_eq!(
            pairs,
            vec![
                (Term::atom(Atom::OK), Term::small_int(1)),
                (Term::atom(Atom::ERROR), Term::small_int(2)),
            ]
        );
    }

    #[test]
    fn erase_0_returns_previous_entries_and_clears_dictionary() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);
        bif_put(&[Term::atom(Atom::OK), Term::small_int(1)], &mut context).expect("put ok");

        let list = bif_erase_0(&[], &mut context).expect("erase/0");
        let pairs: Vec<_> = list_terms(list).into_iter().map(tuple_pair).collect();

        assert_eq!(pairs, vec![(Term::atom(Atom::OK), Term::small_int(1))]);
        assert_eq!(bif_get_0(&[], &mut context), Ok(Term::NIL));
    }

    #[test]
    fn get_keys_1_matches_values_with_exact_equality() {
        let mut process = Process::new(1, 128);
        let mut context = context(&mut process);
        bif_put(&[Term::atom(Atom::OK), Term::small_int(7)], &mut context).expect("put ok");
        bif_put(&[Term::atom(Atom::ERROR), Term::small_int(7)], &mut context).expect("put error");
        bif_put(
            &[Term::atom(Atom::UNDEFINED), Term::small_int(8)],
            &mut context,
        )
        .expect("put undefined");

        let keys = bif_get_keys_1(&[Term::small_int(7)], &mut context).expect("get_keys/1");

        assert_eq!(
            list_terms(keys),
            vec![Term::atom(Atom::OK), Term::atom(Atom::ERROR)]
        );
    }

    #[test]
    fn register_dictionary_bifs_registers_all_process_local() {
        let atom_table = AtomTable::new();
        let registry = BifRegistryImpl::new();
        register_dictionary_bifs(&registry, &atom_table).expect("dictionary registration");
        let erlang = atom_table.intern("erlang");

        for (name, arity) in [
            ("put", 2),
            ("get", 1),
            ("get", 0),
            ("erase", 1),
            ("erase", 0),
            ("get_keys", 1),
        ] {
            let entry = registry
                .lookup(erlang, atom_table.intern(name), arity)
                .expect("dictionary BIF registered");
            assert_eq!(entry.capability, Capability::ProcessLocal);
        }
    }
}
