use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext, stdlib_stubs::register_stdlib_stubs};
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::{Cons, Float, Map, Tuple, write_float, write_tuple};

use super::gleam_stdlib_ffi2::*;

fn context() -> ProcessContext {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(table));
    context
}

fn atom(context: &ProcessContext, name: &str) -> Term {
    let atom = context
        .atom_table()
        .map(|table| table.intern(name))
        .expect("atom table should be configured");
    Term::atom(atom)
}

fn binary(bytes: &[u8]) -> Term {
    let heap = Box::leak(vec![0u64; 2 + binary::packed_word_count(bytes.len())].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary")
}

fn float(value: f64) -> Term {
    let heap = Box::leak(Box::new([0u64; 2]));
    write_float(heap, value).expect("float")
}

fn tuple(values: &[Term]) -> Term {
    let heap = Box::leak(vec![0u64; 1 + values.len()].into_boxed_slice());
    write_tuple(heap, values).expect("tuple")
}

fn list(values: &[Term]) -> Term {
    let mut tail = Term::NIL;
    for value in values.iter().rev() {
        let heap = Box::leak(Box::new([0u64; 2]));
        tail = crate::term::boxed::write_cons(heap, *value, tail).expect("cons");
    }
    tail
}

fn assert_binary(term: Term, expected: &[u8]) {
    let binary = Binary::new(term).expect("binary term");
    assert_eq!(binary.as_bytes(), expected);
}

fn assert_ok_tuple(term: Term, expected: Term) {
    let tuple = Tuple::new(term).expect("tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(tuple.get(1), Some(expected));
}

fn assert_error_nil_tuple(term: Term) {
    let tuple = Tuple::new(term).expect("tuple");
    assert_eq!(tuple.arity(), 2);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
    assert_eq!(tuple.get(1), Some(Term::NIL));
}

#[test]
fn classify_dynamic_returns_type_atoms() {
    let mut context = context();

    assert_eq!(
        bif_classify_dynamic(&[Term::small_int(42)], &mut context),
        Ok(atom(&context, "int"))
    );
    assert_eq!(
        bif_classify_dynamic(&[binary(b"hello")], &mut context),
        Ok(atom(&context, "binary"))
    );
}

#[test]
fn dict_converts_pair_list_to_map_with_last_value_winning() {
    let mut context = context();
    let a = atom(&context, "a");
    let b = atom(&context, "b");
    let pairs = list(&[
        tuple(&[a, Term::small_int(1)]),
        tuple(&[b, Term::small_int(2)]),
        tuple(&[a, Term::small_int(3)]),
    ]);

    let result = bif_dict(&[pairs], &mut context).expect("map");
    let map = Map::new(result).expect("map");
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(a), Some(Term::small_int(3)));
    assert_eq!(map.get(b), Some(Term::small_int(2)));
}

#[test]
fn float_converts_int_and_float_to_string_formats_float() {
    let mut context = context();
    let converted = bif_float(&[Term::small_int(7)], &mut context).expect("float");
    assert_eq!(Float::new(converted).expect("float").value(), 7.0);

    let result = bif_float_to_string(&[float(314_f64 / 100.0)], &mut context).expect("binary");
    assert_binary(result, b"3.14");
}

#[test]
fn index_reads_tuple_and_list_with_zero_based_index() {
    let mut context = context();
    let tuple = tuple(&[Term::small_int(10), Term::small_int(20)]);
    let list = list(&[Term::small_int(30), Term::small_int(40)]);

    assert_eq!(
        bif_index(&[tuple, Term::small_int(1)], &mut context),
        Ok(Term::small_int(20))
    );
    assert_eq!(
        bif_index(&[list, Term::small_int(0)], &mut context),
        Ok(Term::small_int(30))
    );
}

#[test]
fn int_accepts_small_int_only() {
    let mut context = context();
    assert_eq!(
        bif_int(&[Term::small_int(42)], &mut context),
        Ok(Term::small_int(42))
    );
    assert_eq!(
        bif_int(&[binary(b"42")], &mut context),
        Err(Term::atom(Atom::BADARG))
    );
}

#[test]
fn parse_int_and_int_from_base_string_return_result_tuples() {
    let mut context = context();

    let parsed = bif_parse_int(&[binary(b"42")], &mut context).expect("tuple");
    assert_ok_tuple(parsed, Term::small_int(42));
    let failed = bif_parse_int(&[binary(b"abc")], &mut context).expect("tuple");
    assert_error_nil_tuple(failed);
    let parsed_base = bif_int_from_base_string(&[binary(b"FF"), Term::small_int(16)], &mut context)
        .expect("tuple");
    assert_ok_tuple(parsed_base, Term::small_int(255));
}

#[test]
fn parse_float_returns_result_tuple() {
    let mut context = context();
    let parsed = bif_parse_float(&[binary(b"2.5")], &mut context).expect("tuple");
    let tuple = Tuple::new(parsed).expect("tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_eq!(
        Float::new(tuple.get(1).expect("value"))
            .expect("float")
            .value(),
        2.5
    );
    let failed = bif_parse_float(&[binary(b"nan?")], &mut context).expect("tuple");
    assert_error_nil_tuple(failed);
}

#[test]
fn wrap_list_preserves_lists_and_wraps_non_lists() {
    let mut context = context();
    let existing = list(&[Term::small_int(1)]);
    assert_eq!(bif_wrap_list(&[existing], &mut context), Ok(existing));

    let wrapped = bif_wrap_list(&[Term::small_int(2)], &mut context).expect("list");
    let cons = Cons::new(wrapped).expect("cons");
    assert_eq!(cons.head(), Term::small_int(2));
    assert_eq!(cons.tail(), Term::NIL);
}

#[test]
fn base16_and_base64_wrappers_encode_and_decode() {
    let mut context = context();
    let standard = atom(&context, "standard");

    let hex = bif_base16_encode(&[binary(b"hi")], &mut context).expect("hex");
    assert_binary(hex, b"6869");
    let decoded_hex = bif_base16_decode(&[hex], &mut context).expect("decoded");
    assert_binary(decoded_hex, b"hi");

    let encoded = bif_base64_encode(&[binary(b"hi"), standard], &mut context).expect("base64");
    assert_binary(encoded, b"aGk=");
    let decoded = bif_base64_decode(&[encoded], &mut context).expect("decoded");
    assert_binary(decoded, b"hi");
}

#[test]
fn bit_array_wrappers_operate_on_byte_aligned_binaries() {
    let mut context = context();

    let concatenated =
        bif_bit_array_concat(&[list(&[binary(b"a"), binary(b"b")])], &mut context).expect("binary");
    assert_binary(concatenated, b"ab");

    let padded = bif_bit_array_pad_to_bytes(&[binary(b"x")], &mut context).expect("binary");
    assert_binary(padded, b"x");

    let sliced = bif_bit_array_slice(
        &[binary(b"hello"), Term::small_int(1), Term::small_int(3)],
        &mut context,
    )
    .expect("slice");
    assert_binary(sliced, b"ell");

    let int_and_size =
        bif_bit_array_to_int_and_size(&[binary(&[0x01, 0x02])], &mut context).expect("tuple");
    let tuple = Tuple::new(int_and_size).expect("tuple");
    assert_eq!(tuple.get(0), Some(Term::small_int(258)));
    assert_eq!(tuple.get(1), Some(Term::small_int(16)));
}

#[test]
fn register_stdlib_stubs_includes_gleam_stdlib_ffi2_bifs() {
    let atom_table = AtomTable::with_common_atoms();
    let mut registry = BifRegistryImpl::new();
    register_stdlib_stubs(&mut registry, &atom_table).expect("registration");

    let expected = [
        ("classify_dynamic", 1),
        ("dict", 1),
        ("float", 1),
        ("float_to_string", 1),
        ("index", 2),
        ("int", 1),
        ("int_from_base_string", 2),
        ("parse_float", 1),
        ("parse_int", 1),
        ("wrap_list", 1),
        ("base16_decode", 1),
        ("base16_encode", 1),
        ("base64_decode", 1),
        ("base64_encode", 2),
        ("bit_array_concat", 1),
        ("bit_array_pad_to_bytes", 1),
        ("bit_array_slice", 3),
        ("bit_array_to_int_and_size", 1),
    ];
    let module = atom_table.intern("gleam_stdlib");
    for (name, arity) in expected {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing gleam_stdlib:{name}/{arity}"
        );
    }
}
