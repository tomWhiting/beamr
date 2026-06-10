use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext, stdlib_stubs::register_stdlib_stubs};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::{Cons, Map, Tuple};

use super::uri_bifs::*;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(table));
    context.attach_process(process, 0);
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
fn uri_parse_extracts_all_components_with_integer_port() {
    let mut process = Process::new(1, 512);
    let mut context = context(&mut process);
    let parsed = bif_uri_string_parse(
        &[binary(
            b"https://user@example.com:8042/over/there?name=ferret#nose",
        )],
        &mut context,
    )
    .expect("uri map");
    let map = Map::new(parsed).expect("map");

    assert_binary(map.get(atom(&context, "scheme")).expect("scheme"), b"https");
    assert_binary(
        map.get(atom(&context, "userinfo")).expect("userinfo"),
        b"user",
    );
    assert_binary(
        map.get(atom(&context, "host")).expect("host"),
        b"example.com",
    );
    assert_eq!(
        map.get(atom(&context, "port")).expect("port"),
        Term::small_int(8042)
    );
    assert_binary(
        map.get(atom(&context, "path")).expect("path"),
        b"/over/there",
    );
    assert_binary(
        map.get(atom(&context, "query")).expect("query"),
        b"name=ferret",
    );
    assert_binary(
        map.get(atom(&context, "fragment")).expect("fragment"),
        b"nose",
    );
}

#[test]
fn uri_parse_omits_absent_components() {
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);
    let parsed = bif_uri_string_parse(&[binary(b"/relative/path")], &mut context).expect("uri map");
    let map = Map::new(parsed).expect("map");

    assert_binary(
        map.get(atom(&context, "path")).expect("path"),
        b"/relative/path",
    );
    assert_eq!(map.get(atom(&context, "scheme")), None);
    assert_eq!(map.get(atom(&context, "host")), None);
    assert_eq!(map.get(atom(&context, "port")), None);
    assert_eq!(map.get(atom(&context, "query")), None);
    assert_eq!(map.get(atom(&context, "fragment")), None);
}

#[test]
fn uri_parse_strips_ipv6_brackets() {
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);
    let parsed = bif_uri_string_parse(&[binary(b"ldap://[2001:db8::7]/c=GB")], &mut context)
        .expect("uri map");
    let map = Map::new(parsed).expect("map");
    assert_binary(
        map.get(atom(&context, "host")).expect("host"),
        b"2001:db8::7",
    );
    assert_binary(map.get(atom(&context, "path")).expect("path"), b"/c=GB");
}

#[test]
fn uri_parse_rejects_invalid_port_with_error_tuple() {
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);
    let result =
        bif_uri_string_parse(&[binary(b"http://host:bad/")], &mut context).expect("error tuple");
    let tuple = Tuple::new(result).expect("tuple");
    assert_eq!(tuple.arity(), 3);
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::ERROR)));
}

#[test]
fn dissect_query_returns_pairs_with_form_decoding() {
    let mut process = Process::new(1, 512);
    let mut context = context(&mut process);
    let result =
        bif_uri_string_dissect_query(&[binary(b"a=1&b=two+words&flag&x=%2Fenc")], &mut context)
            .expect("pair list");

    let first = Cons::new(result).expect("first");
    let pair = Tuple::new(first.head()).expect("pair");
    assert_binary(pair.get(0).expect("key"), b"a");
    assert_binary(pair.get(1).expect("value"), b"1");

    let second = Cons::new(first.tail()).expect("second");
    let pair = Tuple::new(second.head()).expect("pair");
    assert_binary(pair.get(0).expect("key"), b"b");
    assert_binary(pair.get(1).expect("value"), b"two words");

    let third = Cons::new(second.tail()).expect("third");
    let pair = Tuple::new(third.head()).expect("pair");
    assert_binary(pair.get(0).expect("key"), b"flag");
    assert_eq!(pair.get(1), Some(Term::atom(Atom::TRUE)));

    let fourth = Cons::new(third.tail()).expect("fourth");
    let pair = Tuple::new(fourth.head()).expect("pair");
    assert_binary(pair.get(0).expect("key"), b"x");
    assert_binary(pair.get(1).expect("value"), b"/enc");
    assert_eq!(fourth.tail(), Term::NIL);
}

#[test]
fn maps_get_returns_value_or_raises_badkey() {
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);
    let key = atom(&context, "k");
    let missing = atom(&context, "missing");
    let map = context
        .alloc_map(&[key], &[Term::small_int(7)])
        .expect("map");

    assert_eq!(
        bif_maps_get_2(&[key, map], &mut context),
        Ok(Term::small_int(7))
    );
    let raised = bif_maps_get_2(&[missing, map], &mut context).expect_err("badkey");
    let tuple = Tuple::new(raised).expect("badkey tuple");
    assert_eq!(tuple.get(0), Some(atom(&context, "badkey")));
    assert_eq!(tuple.get(1), Some(missing));

    assert_eq!(
        bif_maps_get_3(&[missing, map, Term::small_int(9)], &mut context),
        Ok(Term::small_int(9))
    );
}

#[test]
fn register_stdlib_stubs_includes_uri_bifs() {
    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_stdlib_stubs(&registry, &atom_table).expect("registration");

    let uri_string = atom_table.intern("uri_string");
    for name in ["parse", "dissect_query"] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(uri_string, function, 1).is_some(),
            "missing uri_string:{name}/1"
        );
    }

    // The gleam_stdlib URI helpers ship as bytecode and must not be shadowed.
    let gleam_stdlib = atom_table.intern("gleam_stdlib");
    for name in [
        "parse_query",
        "percent_decode",
        "percent_encode",
        "uri_parse",
    ] {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(gleam_stdlib, function, 1).is_none(),
            "gleam_stdlib:{name}/1 must come from loaded bytecode, not a stub"
        );
    }
}
