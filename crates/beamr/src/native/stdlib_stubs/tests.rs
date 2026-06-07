use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{Cons, ProcBin, Tuple, write_cons};
use std::sync::Arc;

use super::{
    bif_binary_part, bif_characters_to_binary, bif_characters_to_list, bif_debug_options,
    bif_identity, bif_logger_warning, register_stdlib_stubs,
};
use super::{gleam_stdlib_ffi, string_bifs};

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

// ---- gleam_stdlib:identity/1 ----

#[test]
fn identity_returns_atom_unchanged() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let term = Term::atom(Atom::OK);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_returns_integer_unchanged() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let term = Term::small_int(42);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_returns_nil_unchanged() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_identity(&[Term::NIL], &mut ctx), Ok(Term::NIL));
}

#[test]
fn identity_returns_pid_unchanged() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    let term = Term::pid(7);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_identity(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_identity(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- sys:debug_options/1 ----

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
fn characters_to_list_rejects_non_binary() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(
        bif_characters_to_list(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
    assert_eq!(
        bif_characters_to_list(&[Term::NIL], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn characters_to_list_rejects_wrong_arity() {
    let mut process = Process::new(1, 64);
    let mut ctx = context(&mut process);
    assert_eq!(bif_characters_to_list(&[], &mut ctx), Err(badarg()));
}

// ---- B-033 Gleam stdlib stubs ----

#[test]
fn gleam_stdlib_string_functions_handle_binary_cases() {
    let mut process = Process::new(1, 256);
    let mut ctx = context(&mut process);
    assert_binary(
        gleam_stdlib_ffi::bif_string_replace(
            &[binary(b"hello"), binary(b"l"), binary(b"L")],
            &mut ctx,
        )
        .expect("replace"),
        b"heLLo",
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_contains_string(&[binary(b"hello"), binary(b"ell")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_string_starts_with(&[binary(b"hello"), binary(b"he")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_string_ends_with(&[binary(b"hello"), binary(b"lo")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_binary(
        gleam_stdlib_ffi::bif_slice(
            &[binary(b"hello"), Term::small_int(1), Term::small_int(3)],
            &mut ctx,
        )
        .expect("slice"),
        b"ell",
    );
    assert_binary(
        gleam_stdlib_ffi::bif_crop_string(&[binary(b"hello"), Term::small_int(2)], &mut ctx)
            .expect("crop"),
        b"he",
    );
}

#[test]
fn gleam_stdlib_other_functions_handle_binary_cases() {
    let mut process = Process::new(1, 256);
    let mut ctx = atom_context(&mut process);
    assert_eq!(
        gleam_stdlib_ffi::bif_less_than(&[Term::small_int(1), Term::small_int(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_binary(
        gleam_stdlib_ffi::bif_iodata_append(&[binary(b"hello "), binary(b"world")], &mut ctx)
            .expect("append"),
        b"hello world",
    );

    let large_left = vec![b'a'; 40];
    let large_right = vec![b'b'; 25];
    let large_result =
        gleam_stdlib_ffi::bif_iodata_append(&[binary(&large_left), binary(&large_right)], &mut ctx)
            .expect("large append");
    let mut expected_large = large_left;
    expected_large.extend_from_slice(&large_right);
    assert_eq!(
        BinaryRef::new(large_result)
            .expect("large binary")
            .as_bytes(),
        expected_large.as_slice()
    );
    assert!(ProcBin::new(large_result).is_some());
    assert_binary(
        gleam_stdlib_ffi::bif_utf_codepoint_list_to_string(
            &[write_cons(
                Box::leak(Box::new([0u64; 2])),
                Term::small_int(65),
                Term::NIL,
            )
            .expect("list")],
            &mut ctx,
        )
        .expect("codepoints"),
        b"A",
    );
    assert_binary(
        gleam_stdlib_ffi::bif_inspect(&[binary(b"hello")], &mut ctx).expect("inspect"),
        b"hello",
    );

    let prefix =
        gleam_stdlib_ffi::bif_string_remove_prefix(&[binary(b"foobar"), binary(b"foo")], &mut ctx)
            .expect("prefix");
    let tuple = Tuple::new(prefix).expect("ok tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_binary(tuple.get(1).expect("rest"), b"bar");

    let pop = gleam_stdlib_ffi::bif_string_pop_grapheme(&[binary(b"hi")], &mut ctx).expect("pop");
    let tuple = Tuple::new(pop).expect("pop tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_binary(tuple.get(1).expect("head"), b"h");
    assert_binary(tuple.get(2).expect("rest"), b"i");

    assert_eq!(
        gleam_stdlib_ffi::bif_string_remove_suffix(&[binary(b"foobar"), binary(b"baz")], &mut ctx),
        Ok(Term::atom(Atom::ERROR))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_contains_string(&[Term::small_int(1), binary(b"x")], &mut ctx),
        Err(badarg())
    );
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
        ("erlang", "atom_to_binary", 1),
        ("erlang", "binary_to_float", 1),
        ("erlang", "binary_to_integer", 1),
        ("erlang", "binary_to_integer", 2),
        ("erlang", "float", 1),
        ("erlang", "integer_to_binary", 1),
        ("erlang", "integer_to_binary", 2),
        ("erlang", "integer_to_list", 1),
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
        ("gleam_stdlib", "identity", 1),
        ("maps", "from_list", 1),
        ("maps", "merge", 2),
        ("maps", "remove", 2),
        ("maps", "map", 2),
        ("maps", "put", 3),
        ("maps", "find", 2),
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
        ("lists", "map", 2),
        ("lists", "reverse", 2),
        ("lists", "seq", 2),
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
