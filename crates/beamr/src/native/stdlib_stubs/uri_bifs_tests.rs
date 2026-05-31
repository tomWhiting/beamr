use std::sync::Arc;

use crate::atom::AtomTable;
use crate::native::{BifRegistryImpl, ProcessContext, stdlib_stubs::register_stdlib_stubs};
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::Map;

use super::uri_bifs::*;

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

fn assert_binary(term: Term, expected: &[u8]) {
    let binary = Binary::new(term).expect("binary term");
    assert_eq!(binary.as_bytes(), expected);
}

#[test]
fn percent_encode_and_decode_basic_bytes() {
    let mut context = context();

    let encoded = bif_percent_encode(&[binary(b"hello world")], &mut context).expect("encoded");
    assert_binary(encoded, b"hello%20world");

    let decoded = bif_percent_decode(&[encoded], &mut context).expect("decoded");
    assert_binary(decoded, b"hello world");
}

#[test]
fn uri_parse_extracts_basic_fields() {
    let mut context = context();
    let parsed = bif_uri_string_parse(&[binary(b"https://example.com/path?q=1")], &mut context)
        .expect("uri map");
    let map = Map::new(parsed).expect("map");

    assert_binary(map.get(atom(&context, "scheme")).expect("scheme"), b"https");
    assert_binary(
        map.get(atom(&context, "host")).expect("host"),
        b"example.com",
    );
    assert_binary(map.get(atom(&context, "path")).expect("path"), b"/path");
    assert_binary(map.get(atom(&context, "query")).expect("query"), b"q=1");
}

#[test]
fn parse_query_returns_binary_key_value_map() {
    let mut context = context();
    let parsed = bif_parse_query(&[binary(b"a=1&b=hello")], &mut context).expect("query map");
    let map = Map::new(parsed).expect("map");

    assert_binary(map.get(binary(b"a")).expect("a"), b"1");
    assert_binary(map.get(binary(b"b")).expect("b"), b"hello");
}

#[test]
fn register_stdlib_stubs_includes_uri_bifs() {
    let atom_table = AtomTable::with_common_atoms();
    let mut registry = BifRegistryImpl::new();
    register_stdlib_stubs(&mut registry, &atom_table).expect("registration");

    let gleam_stdlib = atom_table.intern("gleam_stdlib");
    for name in [
        "parse_query",
        "percent_decode",
        "percent_encode",
        "uri_parse",
    ] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(gleam_stdlib, function, 1).is_some(),
            "missing gleam_stdlib:{name}/1"
        );
    }

    let uri_string = atom_table.intern("uri_string");
    for name in ["parse", "dissect_query"] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(uri_string, function, 1).is_some(),
            "missing uri_string:{name}/1"
        );
    }
}
