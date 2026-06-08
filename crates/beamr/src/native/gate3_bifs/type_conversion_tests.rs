use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::write_binary;
use crate::term::boxed::{Float, write_cons, write_float, write_map, write_tuple};

use super::*;

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn ctx_with_atoms(process: &mut Process) -> ProcessContext<'_> {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(table));
    ctx.attach_process(process, 0);
    ctx
}

fn char_list(ctx: &mut ProcessContext<'_>, bytes: &[u8]) -> Term {
    let elements: Vec<_> = bytes
        .iter()
        .copied()
        .map(|byte| Term::small_int(i64::from(byte)))
        .collect();
    ctx.alloc_list(&elements).expect("character list")
}

fn list_bytes(term: Term) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut current = term;
    loop {
        if current.is_nil() {
            return bytes;
        }
        let cons = crate::term::boxed::Cons::new(current).expect("proper list");
        let byte = cons
            .head()
            .as_small_int()
            .and_then(|value| u8::try_from(value).ok())
            .expect("byte integer");
        bytes.push(byte);
        current = cons.tail();
    }
}

// ---- erlang:atom_to_binary/2 ----

#[test]
fn atom_to_binary_1_converts_ok_to_binary() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let result = bif_atom_to_binary_1(&[Term::atom(Atom::OK)], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"ok");
}

#[test]
fn atom_to_binary_converts_ok_to_binary() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let result =
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::UTF8)], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"ok");
}

#[test]
fn atom_to_binary_latin1_encoding_works() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let result = bif_atom_to_binary(
        &[Term::atom(Atom::ERROR), Term::atom(Atom::LATIN1)],
        &mut ctx,
    )
    .unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"error");
}

#[test]
fn atom_to_binary_resolves_encoding_by_name() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    let table = Arc::new(AtomTable::new());
    let ok = table.intern("ok");
    let utf8 = table.intern("utf8");
    ctx.set_atom_table(Some(table));
    ctx.attach_process(&mut process, 0);

    let result = bif_atom_to_binary(&[Term::atom(ok), Term::atom(utf8)], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"ok");
}

#[test]
fn atom_to_binary_badarg_invalid_encoding() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_non_atom() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    assert_eq!(
        bif_atom_to_binary(&[Term::small_int(42), Term::atom(Atom::UTF8)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_no_atom_table() {
    let mut no_atom_process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut no_atom_process, 0);
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::UTF8)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_wrong_arity() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn binary_to_atom_interns_utf8_binary() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"hello").expect("binary");
    let result = bif_binary_to_atom(&[bin], &mut ctx).unwrap();
    let atom = result.as_atom().expect("atom");
    let table = ctx.atom_table().expect("atom table");
    assert_eq!(table.resolve(atom), Some("hello"));
}

#[test]
fn binary_to_atom_badarg_invalid_utf8() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, &[0xff]).expect("binary");
    assert_eq!(bif_binary_to_atom(&[bin], &mut ctx), Err(badarg()));
}

// ---- erlang:binary_to_existing_atom/1 ----

#[test]
fn binary_to_existing_atom_finds_interned_atom() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");
    let result = bif_binary_to_existing_atom(&[bin], &mut ctx).unwrap();
    assert_eq!(result.as_atom(), Some(Atom::OK));
}

#[test]
fn binary_to_existing_atom_badarg_unknown() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 5];
    let bin = write_binary(&mut heap, b"nonexistent_atom_xyz").expect("binary");
    assert_eq!(bif_binary_to_existing_atom(&[bin], &mut ctx), Err(badarg()));
}

#[test]
fn binary_to_existing_atom_badarg_non_binary() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    assert_eq!(
        bif_binary_to_existing_atom(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn binary_to_existing_atom_badarg_no_atom_table() {
    let mut no_table_process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut no_table_process, 0);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");
    assert_eq!(bif_binary_to_existing_atom(&[bin], &mut ctx), Err(badarg()));
}

#[test]
fn binary_to_existing_atom_2_finds_interned_atom() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");
    let result = bif_binary_to_existing_atom_2(&[bin, Term::atom(Atom::UTF8)], &mut ctx).unwrap();
    assert_eq!(result.as_atom(), Some(Atom::OK));
}

#[test]
fn binary_to_existing_atom_2_resolves_encoding_by_name() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    let table = Arc::new(AtomTable::new());
    let ok = table.intern("ok");
    let utf8 = table.intern("utf8");
    ctx.set_atom_table(Some(table));
    ctx.attach_process(&mut process, 0);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");

    let result = bif_binary_to_existing_atom_2(&[bin, Term::atom(utf8)], &mut ctx).unwrap();
    assert_eq!(result.as_atom(), Some(ok));
}

#[test]
fn binary_to_existing_atom_2_badarg_unknown() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut heap = [0u64; 5];
    let bin = write_binary(&mut heap, b"nonexistent_atom_xyz").expect("binary");
    assert_eq!(
        bif_binary_to_existing_atom_2(&[bin, Term::atom(Atom::UTF8)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:binary_to_list/1 ----

#[test]
fn binary_to_list_converts_bytes() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"hi").expect("binary");
    let result = bif_binary_to_list(&[bin], &mut ctx).unwrap();

    // Walk the list: should be [104, 105]
    let cons1 = crate::term::boxed::Cons::new(result).expect("cons");
    assert_eq!(cons1.head(), Term::small_int(b'h' as i64));
    let cons2 = crate::term::boxed::Cons::new(cons1.tail()).expect("cons");
    assert_eq!(cons2.head(), Term::small_int(b'i' as i64));
    assert!(cons2.tail().is_nil());
}

#[test]
fn binary_to_list_empty_returns_nil() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut heap = [0u64; 2];
    let bin = write_binary(&mut heap, b"").expect("binary");
    assert_eq!(bif_binary_to_list(&[bin], &mut ctx), Ok(Term::NIL));
}

#[test]
fn binary_to_list_badarg_non_binary() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    assert_eq!(
        bif_binary_to_list(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:list_to_atom/1, atom_to_list/1, list_to_existing_atom/1 ----

#[test]
fn list_to_atom_converts_character_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let list = char_list(&mut ctx, b"hi");
    let result = bif_list_to_atom(&[list], &mut ctx).unwrap();
    let atom = result.as_atom().expect("atom");
    let table = ctx.atom_table().expect("atom table");
    assert_eq!(table.resolve(atom), Some("hi"));
}

#[test]
fn atom_to_list_converts_atom_name_to_character_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let result = bif_atom_to_list(&[Term::atom(Atom::OK)], &mut ctx).unwrap();
    assert_eq!(list_bytes(result), b"ok");
}

#[test]
fn list_to_existing_atom_finds_existing_atom() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let list = char_list(&mut ctx, b"ok");
    let result = bif_list_to_existing_atom(&[list], &mut ctx).unwrap();
    assert_eq!(result.as_atom(), Some(Atom::OK));
}

#[test]
fn list_to_existing_atom_badarg_unknown_atom() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let list = char_list(&mut ctx, b"unknown_atom_xyz");
    assert_eq!(bif_list_to_existing_atom(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn atom_list_round_trip_preserves_character_list() {
    let mut process = Process::new(1, 256);
    let mut ctx = ctx_with_atoms(&mut process);
    let list = char_list(&mut ctx, b"round_trip");
    let atom = bif_list_to_atom(&[list], &mut ctx).unwrap();
    let result = bif_atom_to_list(&[atom], &mut ctx).unwrap();
    assert_eq!(list_bytes(result), b"round_trip");
}

#[test]
fn list_to_atom_badarg_improper_or_non_byte_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut c1 = [0u64; 2];
    let improper = write_cons(&mut c1, Term::small_int(104), Term::small_int(105)).unwrap();
    assert_eq!(bif_list_to_atom(&[improper], &mut ctx), Err(badarg()));

    let mut c2 = [0u64; 2];
    let out_of_range = write_cons(&mut c2, Term::small_int(256), Term::NIL).unwrap();
    assert_eq!(bif_list_to_atom(&[out_of_range], &mut ctx), Err(badarg()));
}

// ---- erlang:list_to_binary/1 ----

#[test]
fn list_to_binary_converts_byte_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(105), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(104), tail).unwrap();
    let result = bif_list_to_binary(&[list], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("binary");
    assert_eq!(binary.as_bytes(), b"hi");
}

#[test]
fn list_to_binary_empty_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let result = bif_list_to_binary(&[Term::NIL], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("binary");
    assert!(binary.is_empty());
}

#[test]
fn list_to_binary_badarg_value_out_of_range() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(256), Term::NIL).unwrap();
    assert_eq!(bif_list_to_binary(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn list_to_binary_badarg_negative_value() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(-1), Term::NIL).unwrap();
    assert_eq!(bif_list_to_binary(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn list_to_binary_badarg_non_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    assert_eq!(
        bif_list_to_binary(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:list_to_integer/1, list_to_float/1, float_to_list/1, float_to_binary/2 ----

#[test]
fn list_to_integer_parses_decimal_character_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let list = char_list(&mut ctx, b"42");
    assert_eq!(
        bif_list_to_integer(&[list], &mut ctx),
        Ok(Term::small_int(42))
    );
}

#[test]
fn list_to_integer_badarg_invalid_input() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let list = char_list(&mut ctx, b"4.2");
    assert_eq!(bif_list_to_integer(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn list_to_float_parses_decimal_character_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let list = char_list(&mut ctx, b"3.14");
    let result = bif_list_to_float(&[list], &mut ctx).unwrap();
    let value = Float::new(result).expect("float").value();
    assert!((value - 3.14).abs() < f64::EPSILON);
}

#[test]
fn list_to_float_badarg_invalid_input() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let list = char_list(&mut ctx, b"not-a-float");
    assert_eq!(bif_list_to_float(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn float_to_list_formats_character_list() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut heap = [0u64; 2];
    let float = write_float(&mut heap, 3.14).expect("float");
    let result = bif_float_to_list(&[float], &mut ctx).unwrap();
    assert_eq!(list_bytes(result), b"3.14");
}

#[test]
fn float_to_list_formats_integral_float_as_float_text() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let mut heap = [0u64; 2];
    let float = write_float(&mut heap, 3.0).expect("float");
    let result = bif_float_to_list(&[float], &mut ctx).unwrap();
    assert_eq!(list_bytes(result), b"3.0");
}

#[test]
fn float_to_binary_formats_with_decimals_option() {
    let mut process = Process::new(1, 256);
    let mut ctx = ctx_with_atoms(&mut process);
    let decimals = ctx.atom_table_arc().expect("atom table").intern("decimals");
    let mut tuple_heap = [0u64; 3];
    let option =
        write_tuple(&mut tuple_heap, &[Term::atom(decimals), Term::small_int(2)]).expect("tuple");
    let options = ctx.alloc_list(&[option]).expect("options list");
    let mut float_heap = [0u64; 2];
    let float = write_float(&mut float_heap, 3.14).expect("float");
    let result = bif_float_to_binary_2(&[float, options], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("binary");
    assert_eq!(binary.as_bytes(), b"3.14");
}

#[test]
fn float_to_binary_badarg_malformed_options() {
    let mut process = Process::new(1, 128);
    let mut ctx = ctx_with_atoms(&mut process);
    let mut float_heap = [0u64; 2];
    let float = write_float(&mut float_heap, 3.14).expect("float");
    let options = ctx
        .alloc_list(&[Term::atom(Atom::OK)])
        .expect("options list");
    assert_eq!(
        bif_float_to_binary_2(&[float, options], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn float_to_binary_badarg_decimals_out_of_range() {
    let mut process = Process::new(1, 256);
    let mut ctx = ctx_with_atoms(&mut process);
    let decimals = ctx.atom_table_arc().expect("atom table").intern("decimals");
    let mut tuple_heap = [0u64; 3];
    let option = write_tuple(
        &mut tuple_heap,
        &[Term::atom(decimals), Term::small_int(254)],
    )
    .expect("tuple");
    let options = ctx.alloc_list(&[option]).expect("options list");
    let mut float_heap = [0u64; 2];
    let float = write_float(&mut float_heap, 3.14).expect("float");

    assert_eq!(
        bif_float_to_binary_2(&[float, options], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:map_get/2 ----

#[test]
fn map_get_returns_value() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let keys = [Term::small_int(1), Term::small_int(2)];
    let values = [Term::atom(Atom::OK), Term::atom(Atom::ERROR)];
    let mut heap = [0u64; 6];
    let map = write_map(&mut heap, &keys, &values).expect("map");
    assert_eq!(
        bif_map_get(&[Term::small_int(1), map], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(
        bif_map_get(&[Term::small_int(2), map], &mut ctx),
        Ok(Term::atom(Atom::ERROR))
    );
}

#[test]
fn map_get_badkey_for_missing_key() {
    let mut process = Process::new(1, 128);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    let keys = [Term::small_int(1)];
    let values = [Term::atom(Atom::OK)];
    let mut heap = [0u64; 4];
    let map = write_map(&mut heap, &keys, &values).expect("map");
    let result = bif_map_get(&[Term::small_int(99), map], &mut ctx);
    assert!(result.is_err());
    // The error should be a {badkey, Key} tuple.
    let err = result.unwrap_err();
    let tuple = crate::term::boxed::Tuple::new(err).expect("tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::BADKEY)));
    assert_eq!(tuple.get(1), Some(Term::small_int(99)));
}

#[test]
fn map_get_badarg_non_map() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    assert_eq!(
        bif_map_get(&[Term::small_int(1), Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn map_get_badarg_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = ProcessContext::new();
    ctx.attach_process(&mut process, 0);
    assert_eq!(bif_map_get(&[Term::small_int(1)], &mut ctx), Err(badarg()));
}
