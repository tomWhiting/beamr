use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, write_cons};
use std::sync::Arc;

use super::string_bifs;
use super::{
    bif_binary_part, bif_characters_to_binary, bif_characters_to_list, bif_debug_options,
    bif_logger_warning, register_stdlib_stubs,
};

fn context(process: &mut Process) -> ProcessContext<'_> {
    let mut context = ProcessContext::new();
    context.attach_process(process, 0);
    context
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn binary(bytes: &[u8]) -> Term {
    let data_words = binary::packed_word_count(bytes.len());
    let heap: &mut [u64] = Box::leak(vec![0u64; 2 + data_words].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary heap sized correctly")
}

fn assert_binary(term: Term, expected: &[u8]) {
    let binary = BinaryRef::new(term).expect("binary term");
    assert_eq!(binary.as_bytes(), expected);
}

fn atom_context(process: &mut Process) -> ProcessContext<'_> {
    let table = Arc::new(AtomTable::with_common_atoms());
    table.intern("both");
    table.intern("all");
    table.intern("leading");
    table.intern("trailing");
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(table));
    ctx.attach_process(process, 0);
    ctx
}

#[test]
fn debug_options_returns_empty_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_debug_options(&[Term::NIL], &mut ctx), Ok(Term::NIL));
}

#[test]
fn debug_options_accepts_non_empty_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::small_int(1), Term::NIL).unwrap();
    assert_eq!(bif_debug_options(&[list], &mut ctx), Ok(Term::NIL));
}

#[test]
fn debug_options_accepts_any_term() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    // Stub is permissive — any single argument returns [].
    assert_eq!(
        bif_debug_options(&[Term::small_int(42)], &mut ctx),
        Ok(Term::NIL)
    );
}

#[test]
fn debug_options_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_debug_options(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_debug_options(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- logger:warning/2 ----

#[test]
fn logger_warning_returns_ok_for_binary_format() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 4];
    let format = binary::write_binary(&mut heap, b"test warning").unwrap();
    let args = Term::NIL;
    assert_eq!(
        bif_logger_warning(&[format, args], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
}

#[test]
fn logger_warning_returns_ok_for_atom_format() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let format = Term::atom(Atom::ERROR);
    let args = Term::NIL;
    assert_eq!(
        bif_logger_warning(&[format, args], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
}

#[test]
fn logger_warning_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_logger_warning(&[], &mut ctx), Err(badarg()));
    assert_eq!(bif_logger_warning(&[Term::NIL], &mut ctx), Err(badarg()));
}

// ---- unicode:characters_to_binary/1 ----

#[test]
fn characters_to_binary_passes_through_binary() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").unwrap();
    let result = bif_characters_to_binary(&[bin], &mut ctx).unwrap();
    // Should return the same binary term.
    assert_eq!(result, bin);
}

#[test]
fn characters_to_binary_converts_empty_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let result = bif_characters_to_binary(&[Term::NIL], &mut ctx).unwrap();
    let bin = Binary::new(result).expect("should be a binary");
    assert!(bin.is_empty());
}

#[test]
fn characters_to_binary_converts_integer_code_point_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    // Build list [104, 105] = "hi"
    let mut c2 = [0u64; 2];
    let tail = write_cons(&mut c2, Term::small_int(105), Term::NIL).unwrap();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(104), tail).unwrap();

    let result = bif_characters_to_binary(&[list], &mut ctx).unwrap();
    let bin = Binary::new(result).expect("should be a binary");
    assert_eq!(bin.as_bytes(), b"hi");
}

#[test]
fn characters_to_binary_rejects_non_binary_non_list() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_characters_to_binary(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn characters_to_binary_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_characters_to_binary(&[], &mut ctx), Err(badarg()));
}

#[test]
fn characters_to_binary_rejects_list_with_invalid_code_point() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    // List containing an atom (not a valid code point).
    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::atom(Atom::OK), Term::NIL).unwrap();
    assert_eq!(bif_characters_to_binary(&[list], &mut ctx), Err(badarg()));
}

// ---- unicode:characters_to_list/1 ----

#[test]
fn characters_to_list_converts_ascii_binary() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"AB").unwrap();

    let result = bif_characters_to_list(&[bin], &mut ctx).unwrap();

    // Should be a list [65, 66].
    let cons1 = crate::term::boxed::Cons::new(result).expect("first cons");
    assert_eq!(cons1.head(), Term::small_int(65));
    let cons2 = crate::term::boxed::Cons::new(cons1.tail()).expect("second cons");
    assert_eq!(cons2.head(), Term::small_int(66));
    assert_eq!(cons2.tail(), Term::NIL);
}

#[test]
fn characters_to_list_returns_nil_for_empty_binary() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let mut heap = [0u64; 2];
    let bin = binary::write_binary(&mut heap, b"").unwrap();
    assert_eq!(bif_characters_to_list(&[bin], &mut ctx), Ok(Term::NIL));
}

#[test]
fn characters_to_list_handles_multibyte_utf8() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    // UTF-8 for 'e' with accent: U+00E9 = 0xC3 0xA9
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, "é".as_bytes()).unwrap();

    let result = bif_characters_to_list(&[bin], &mut ctx).unwrap();
    let cons = crate::term::boxed::Cons::new(result).expect("first cons");
    assert_eq!(cons.head(), Term::small_int(0xE9)); // U+00E9
    assert_eq!(cons.tail(), Term::NIL);
}

#[test]
fn characters_to_list_rejects_non_chardata() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_characters_to_list(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn characters_to_list_accepts_chardata_list() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);
    // ["A", 66 | <<"C">>] mixes binaries, codepoints, and an improper tail.
    let chunk = binary(b"A");
    let tail = binary(b"C");
    let heap = Box::leak(Box::new([0u64; 2]));
    let inner = write_cons(heap, Term::small_int(66), tail).expect("cons");
    let heap = Box::leak(Box::new([0u64; 2]));
    let chardata = write_cons(heap, chunk, inner).expect("cons");
    let result = bif_characters_to_list(&[chardata], &mut ctx).expect("list");
    let first = Cons::new(result).expect("first");
    assert_eq!(first.head(), Term::small_int(65));
    let second = Cons::new(first.tail()).expect("second");
    assert_eq!(second.head(), Term::small_int(66));
    let third = Cons::new(second.tail()).expect("third");
    assert_eq!(third.head(), Term::small_int(67));
    assert_eq!(third.tail(), Term::NIL);
}

#[test]
fn characters_to_binary_handles_nested_and_improper_chardata() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);
    // [[101, 769] | <<"xy">>] — string:next_grapheme cluster head shape.
    let tail = binary(b"xy");
    let heap = Box::leak(Box::new([0u64; 2]));
    let cluster_tail = write_cons(heap, Term::small_int(769), Term::NIL).expect("cons");
    let heap = Box::leak(Box::new([0u64; 2]));
    let cluster = write_cons(heap, Term::small_int(101), cluster_tail).expect("cons");
    let heap = Box::leak(Box::new([0u64; 2]));
    let chardata = write_cons(heap, cluster, tail).expect("cons");
    let result = bif_characters_to_binary(&[chardata], &mut ctx).expect("binary");
    assert_binary(result, "e\u{0301}xy".as_bytes());
}

#[test]
fn characters_to_list_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_characters_to_list(&[], &mut ctx), Err(badarg()));
}

// ---- B-033 string module stubs ----

#[test]
fn string_bifs_handle_binary_cases() {
    let mut process = Process::new(1, 256);
    let mut ctx = atom_context(&mut process);
    assert_eq!(
        string_bifs::bif_length(&[binary(b"hello")], &mut ctx),
        Ok(Term::small_int(5))
    );
    assert_binary(
        string_bifs::bif_reverse(&[binary(b"abc")], &mut ctx).expect("reverse"),
        b"cba",
    );
    assert_binary(
        string_bifs::bif_lowercase(&[binary(b"HELLO")], &mut ctx).expect("lowercase"),
        b"hello",
    );
    assert_binary(
        string_bifs::bif_uppercase(&[binary(b"hello")], &mut ctx).expect("uppercase"),
        b"HELLO",
    );
    assert_binary(
        string_bifs::bif_trim(
            &[
                binary(b" hi "),
                Term::atom(ctx.atom_table().unwrap().lookup("both").unwrap()),
            ],
            &mut ctx,
        )
        .expect("trim"),
        b"hi",
    );
    assert_eq!(
        string_bifs::bif_equal(&[binary(b"a"), binary(b"a")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        string_bifs::bif_is_empty(&[binary(b"")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
}

#[test]
fn string_split_returns_list_of_binaries_and_rejects_invalid_input() {
    let mut process = Process::new(1, 256);
    let mut ctx = atom_context(&mut process);
    let all = Term::atom(ctx.atom_table().unwrap().lookup("all").unwrap());
    let result =
        string_bifs::bif_split(&[binary(b"a-b-c"), binary(b"-"), all], &mut ctx).expect("split");
    let first = Cons::new(result).expect("first");
    assert_binary(first.head(), b"a");
    let second = Cons::new(first.tail()).expect("second");
    assert_binary(second.head(), b"b");
    let third = Cons::new(second.tail()).expect("third");
    assert_binary(third.head(), b"c");
    assert_eq!(third.tail(), Term::NIL);

    assert_eq!(
        string_bifs::bif_split(&[binary(b"abc"), binary(b""), all], &mut ctx),
        Err(badarg())
    );
}

// ---- B-033 binary module stubs ----

#[test]
fn binary_part_extracts_slices_and_rejects_out_of_bounds() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_binary(
        bif_binary_part(
            &[binary(b"hello"), Term::small_int(1), Term::small_int(3)],
            &mut ctx,
        )
        .expect("part"),
        b"ell",
    );
    assert_binary(
        bif_binary_part(
            &[binary(b"hello"), Term::small_int(5), Term::small_int(0)],
            &mut ctx,
        )
        .expect("empty at end"),
        b"",
    );
    assert_eq!(
        bif_binary_part(
            &[binary(b"hello"), Term::small_int(4), Term::small_int(2)],
            &mut ctx
        ),
        Err(badarg())
    );
}

// ---- Registration ----

#[test]
fn register_stdlib_stubs_registers_all_expected_mfas() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();

    register_stdlib_stubs(&registry, &atom_table).expect("registration should succeed");

    let expected = [
        ("erlang", "binary_to_float", 1),
        ("erlang", "binary_to_integer", 1),
        ("erlang", "binary_to_integer", 2),
        ("erlang", "float", 1),
        ("erlang", "integer_to_binary", 1),
        ("erlang", "integer_to_binary", 2),
        ("erlang", "integer_to_list", 1),
        ("erlang", "integer_to_list", 2),
        ("erlang", "iolist_to_binary", 1),
        ("erlang", "list_to_bitstring", 1),
        ("erlang", "list_to_tuple", 1),
        ("erlang", "tuple_to_list", 1),
        ("erlang", "band", 2),
        ("erlang", "bnot", 1),
        ("erlang", "bor", 2),
        ("erlang", "bsl", 2),
        ("erlang", "bsr", 2),
        ("erlang", "bxor", 2),
        ("math", "ceil", 1),
        ("math", "floor", 1),
        ("math", "exp", 1),
        ("math", "log", 1),
        ("math", "pow", 2),
        ("rand", "uniform", 0),
        ("logger", "warning", 2),
        ("unicode", "characters_to_list", 1),
        ("unicode", "characters_to_binary", 1),
        ("sys", "debug_options", 1),
        ("maps", "from_list", 1),
        ("maps", "merge", 2),
        ("maps", "remove", 2),
        ("maps", "map", 2),
        ("maps", "put", 3),
        ("maps", "find", 2),
        ("maps", "get", 2),
        ("maps", "get", 3),
        ("maps", "keys", 1),
        ("maps", "values", 1),
        ("maps", "to_list", 1),
        ("maps", "fold", 3),
        ("maps", "filter", 2),
        ("maps", "merge_with", 3),
        ("maps", "update_with", 4),
        ("maps", "with", 2),
        ("maps", "without", 2),
        ("lists", "reverse", 1),
        ("lists", "append", 1),
        ("lists", "append", 2),
        ("lists", "join", 2),
        ("lists", "nth", 2),
        ("lists", "member", 2),
        ("lists", "keyfind", 3),
        ("lists", "last", 1),
        ("lists", "sort", 1),
        ("lists", "flatten", 1),
        ("lists", "zip", 2),
        ("lists", "unzip", 1),
        ("lists", "filter", 2),
        ("lists", "filtermap", 2),
        ("lists", "map", 2),
        ("lists", "reverse", 2),
        ("lists", "seq", 2),
        ("lists", "keystore", 4),
        ("lists", "keysort", 2),
        ("lists", "keydelete", 3),
        ("lists", "foreach", 2),
        ("timer", "sleep", 1),
    ];

    for (module_name, function_name, arity) in expected {
        let module = atom_table.intern(module_name);
        let function = atom_table.intern(function_name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing {module_name}:{function_name}/{arity}"
        );
    }
}

#[test]
fn register_stdlib_stubs_fails_on_duplicate() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();

    register_stdlib_stubs(&registry, &atom_table).expect("first registration");
    assert!(register_stdlib_stubs(&registry, &atom_table).is_err());
}
