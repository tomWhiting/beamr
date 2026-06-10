use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, Float, Map, Tuple};

use super::json_bifs::{
    bif_json_decode, bif_json_encode, bif_json_encode_binary, bif_json_encode_float,
    bif_json_encode_integer,
};

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
    context.attach_process(process, 0);
    context
}

fn binary_string(term: Term) -> String {
    let binary = BinaryRef::new(term).expect("binary result");
    String::from_utf8(binary.as_bytes().to_vec()).expect("utf8")
}

#[test]
fn encode_integer_formats_small_and_big() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);

    let small = bif_json_encode_integer(&[Term::small_int(-42)], &mut ctx).expect("small");
    assert_eq!(binary_string(small), "-42");

    let big = ctx.alloc_bigint(false, &[u64::MAX]).expect("bignum");
    let encoded = bif_json_encode_integer(&[big], &mut ctx).expect("big");
    assert_eq!(binary_string(encoded), "18446744073709551615");
}

#[test]
fn encode_float_keeps_a_decimal_point() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);

    let one = ctx.alloc_float(1.0).expect("float");
    let encoded = bif_json_encode_float(&[one], &mut ctx).expect("encode");
    assert_eq!(binary_string(encoded), "1.0");

    let fractional = ctx.alloc_float(-2.5).expect("float");
    let encoded = bif_json_encode_float(&[fractional], &mut ctx).expect("encode");
    assert_eq!(binary_string(encoded), "-2.5");

    let infinity = ctx.alloc_float(f64::INFINITY).expect("float");
    assert!(bif_json_encode_float(&[infinity], &mut ctx).is_err());
}

#[test]
fn encode_binary_escapes_json_string_characters() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);

    let mut raw = b"say \"hi\" back".to_vec();
    raw.push(b'\\');
    raw.push(b'\n');
    raw.push(0x01);
    let input = ctx.alloc_binary(&raw).expect("binary");
    let encoded = bif_json_encode_binary(&[input], &mut ctx).expect("encode");
    let mut expected = String::from("\"say \\\"hi\\\" back");
    expected.push_str("\\\\");
    expected.push_str("\\n");
    expected.push_str("\\u0001");
    expected.push('"');
    assert_eq!(binary_string(encoded), expected);
}

#[test]
fn encode_term_handles_nested_structures() {
    let mut process = Process::new(1, 512);
    let mut ctx = context(&mut process);

    let label = ctx.alloc_binary(b"total").expect("key");
    let list = {
        let one = ctx.alloc_cons(Term::small_int(2), Term::NIL).expect("cons");
        ctx.alloc_cons(Term::small_int(1), one).expect("cons")
    };
    let map = ctx.alloc_map(&[label], &[list]).expect("map");
    let encoded = bif_json_encode(&[map], &mut ctx).expect("encode");
    assert_eq!(binary_string(encoded), r#"{"total":[1,2]}"#);
}

#[test]
fn decode_parses_objects_arrays_and_scalars() {
    let mut process = Process::new(1, 4096);
    let mut ctx = context(&mut process);

    let input = ctx
        .alloc_binary(br#" {"a": [1, -2.5, "xA", true, false, null], "b": 9} "#)
        .expect("binary");
    let decoded = bif_json_decode(&[input], &mut ctx).expect("decode");
    let map = Map::new(decoded).expect("object decodes to a map");
    assert_eq!(map.len(), 2);

    let key_a = ctx.alloc_binary(b"a").expect("key");
    let array = map.get(key_a).expect("a present");
    let cons = Cons::new(array).expect("array decodes to a list");
    assert_eq!(cons.head().as_small_int(), Some(1));
    let cons = Cons::new(cons.tail()).expect("second");
    let float = Float::new(cons.head()).expect("float element");
    assert!((float.value() - -2.5).abs() < f64::EPSILON);
    let cons = Cons::new(cons.tail()).expect("third");
    let text = BinaryRef::new(cons.head()).expect("string element");
    assert_eq!(text.as_bytes(), b"xA");
    let cons = Cons::new(cons.tail()).expect("fourth");
    assert_eq!(cons.head(), Term::atom(Atom::TRUE));
    let cons = Cons::new(cons.tail()).expect("fifth");
    assert_eq!(cons.head(), Term::atom(Atom::FALSE));
    let cons = Cons::new(cons.tail()).expect("sixth");
    let table = ctx.atom_table_arc().expect("atoms");
    assert_eq!(cons.head(), Term::atom(table.intern("null")));
    assert!(cons.tail().is_nil());

    let key_b = ctx.alloc_binary(b"b").expect("key");
    assert_eq!(map.get(key_b).and_then(Term::as_small_int), Some(9));
}

#[test]
fn decode_reports_otp_error_reasons() {
    let mut process = Process::new(1, 512);
    let mut ctx = context(&mut process);
    let table = ctx.atom_table_arc().expect("atoms");

    let truncated = ctx.alloc_binary(b"{\"a\": 1").expect("binary");
    let error = bif_json_decode(&[truncated], &mut ctx).expect_err("truncated input");
    assert_eq!(error, Term::atom(table.intern("unexpected_end")));

    let invalid = ctx.alloc_binary(b"{\"a\" 1}").expect("binary");
    let error = bif_json_decode(&[invalid], &mut ctx).expect_err("invalid byte");
    let tuple = Tuple::new(error).expect("invalid_byte tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(table.intern("invalid_byte"))));
    assert_eq!(
        tuple.get(1).and_then(Term::as_small_int),
        Some(i64::from(b'1'))
    );

    let trailing = ctx.alloc_binary(b"1 x").expect("binary");
    let error = bif_json_decode(&[trailing], &mut ctx).expect_err("trailing garbage");
    let tuple = Tuple::new(error).expect("invalid_byte tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(table.intern("invalid_byte"))));
}

#[test]
fn decode_handles_bignums_and_surrogate_pairs() {
    let mut process = Process::new(1, 1024);
    let mut ctx = context(&mut process);

    let big = ctx
        .alloc_binary(b"123456789012345678901234567890")
        .expect("binary");
    let decoded = bif_json_decode(&[big], &mut ctx).expect("bignum decodes");
    let encoded = bif_json_encode_integer(&[decoded], &mut ctx).expect("round trip");
    assert_eq!(binary_string(encoded), "123456789012345678901234567890");

    let emoji = ctx.alloc_binary(b"\"\\uD83D\\uDE00\"").expect("binary");
    let decoded = bif_json_decode(&[emoji], &mut ctx).expect("surrogate pair decodes");
    let text = BinaryRef::new(decoded).expect("string");
    assert_eq!(text.as_bytes(), "😀".as_bytes());
}
