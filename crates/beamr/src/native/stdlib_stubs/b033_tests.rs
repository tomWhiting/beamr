use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::{Cons, Tuple, write_cons};
use std::sync::Arc;

use super::{
    bif_binary_part, bif_init_stop, encoding_bifs, gleam_stdlib_ffi, io_bifs,
    register_stdlib_stubs, string_bifs,
};

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn binary(bytes: &[u8]) -> Term {
    let data_words = binary::packed_word_count(bytes.len());
    let heap: &mut [u64] = Box::leak(vec![0u64; 2 + data_words].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary heap sized correctly")
}

fn assert_binary(term: Term, expected: &[u8]) {
    let binary = Binary::new(term).expect("binary term");
    assert_eq!(binary.as_bytes(), expected);
}

fn atom_context() -> ProcessContext {
    let table = Arc::new(AtomTable::with_common_atoms());
    for name in ["both", "all", "leading", "trailing", "standard", "nomatch"] {
        table.intern(name);
    }
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(table));
    ctx
}

fn atom(ctx: &ProcessContext, name: &str) -> Term {
    Term::atom(ctx.atom_table().unwrap().lookup(name).unwrap())
}

fn list(elements: &[Term]) -> Term {
    let mut tail = Term::NIL;
    for element in elements.iter().rev() {
        tail = write_cons(Box::leak(Box::new([0u64; 2])), *element, tail).expect("cons");
    }
    tail
}

#[test]
fn gleam_stdlib_each_stub_has_success_and_badarg_coverage() {
    let mut ctx = atom_context();

    assert_binary(
        gleam_stdlib_ffi::bif_string_replace(
            &[binary(b"banana"), binary(b"na"), binary(b"NA")],
            &mut ctx,
        )
        .expect("replace"),
        b"baNANA",
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_string_replace(
            &[binary(b"abc"), binary(b""), binary(b"x")],
            &mut ctx
        ),
        Err(badarg())
    );

    assert_eq!(
        gleam_stdlib_ffi::bif_less_than(&[Term::small_int(1), Term::small_int(2)], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_less_than(&[Term::small_int(2), Term::small_int(1)], &mut ctx),
        Ok(Term::atom(Atom::FALSE))
    );

    assert_binary(
        gleam_stdlib_ffi::bif_slice(
            &[binary(b"hello"), Term::small_int(1), Term::small_int(3)],
            &mut ctx,
        )
        .expect("slice"),
        b"ell",
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_slice(
            &[binary(b"hi"), Term::small_int(1), Term::small_int(5)],
            &mut ctx
        ),
        Err(badarg())
    );

    assert_binary(
        gleam_stdlib_ffi::bif_crop_string(&[binary(b"hello"), Term::small_int(99)], &mut ctx)
            .expect("crop"),
        b"hello",
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_crop_string(&[binary(b"hello"), Term::small_int(-1)], &mut ctx),
        Err(badarg())
    );

    assert_eq!(
        gleam_stdlib_ffi::bif_contains_string(&[binary(b"hello"), binary(b"ell")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_contains_string(&[Term::small_int(1), binary(b"ell")], &mut ctx),
        Err(badarg())
    );

    assert_eq!(
        gleam_stdlib_ffi::bif_string_starts_with(&[binary(b"hello"), binary(b"he")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_string_starts_with(&[binary(b"hello"), Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_eq!(
        gleam_stdlib_ffi::bif_string_ends_with(&[binary(b"hello"), binary(b"lo")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_string_ends_with(&[Term::small_int(1), binary(b"lo")], &mut ctx),
        Err(badarg())
    );

    let pop = gleam_stdlib_ffi::bif_string_pop_grapheme(&[binary("éx".as_bytes())], &mut ctx)
        .expect("pop grapheme");
    let tuple = Tuple::new(pop).expect("pop tuple");
    assert_binary(tuple.get(1).unwrap(), "é".as_bytes());
    assert_binary(tuple.get(2).unwrap(), b"x");
    assert_eq!(
        gleam_stdlib_ffi::bif_string_pop_grapheme(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        gleam_stdlib_ffi::bif_utf_codepoint_list_to_string(
            &[list(&[Term::small_int(65), Term::small_int(0xE9)])],
            &mut ctx,
        )
        .expect("codepoints"),
        "Aé".as_bytes(),
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_utf_codepoint_list_to_string(
            &[list(&[Term::small_int(-1)])],
            &mut ctx
        ),
        Err(badarg())
    );

    assert_binary(
        gleam_stdlib_ffi::bif_inspect(&[Term::small_int(42)], &mut ctx).expect("inspect"),
        b"42",
    );
    assert_eq!(gleam_stdlib_ffi::bif_inspect(&[], &mut ctx), Err(badarg()));

    let prefix =
        gleam_stdlib_ffi::bif_string_remove_prefix(&[binary(b"foobar"), binary(b"foo")], &mut ctx)
            .expect("remove prefix");
    let tuple = Tuple::new(prefix).expect("prefix tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_binary(tuple.get(1).unwrap(), b"bar");
    assert_eq!(
        gleam_stdlib_ffi::bif_string_remove_prefix(
            &[binary(b"foobar"), Term::small_int(1)],
            &mut ctx
        ),
        Err(badarg())
    );

    let suffix =
        gleam_stdlib_ffi::bif_string_remove_suffix(&[binary(b"foobar"), binary(b"bar")], &mut ctx)
            .expect("remove suffix");
    let tuple = Tuple::new(suffix).expect("suffix tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_binary(tuple.get(1).unwrap(), b"foo");
    assert_eq!(
        gleam_stdlib_ffi::bif_string_remove_suffix(&[Term::small_int(1), binary(b"bar")], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        gleam_stdlib_ffi::bif_iodata_append(&[binary(b"hello "), binary(b"world")], &mut ctx)
            .expect("append"),
        b"hello world",
    );
    assert_eq!(
        gleam_stdlib_ffi::bif_iodata_append(&[binary(b"hello"), Term::small_int(256)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn string_module_each_stub_has_success_and_badarg_coverage() {
    let mut ctx = atom_context();

    assert_eq!(
        string_bifs::bif_length(&[binary(b"hello")], &mut ctx),
        Ok(Term::small_int(5))
    );
    assert_eq!(
        string_bifs::bif_length(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        string_bifs::bif_reverse(&[binary(b"abc")], &mut ctx).expect("reverse"),
        b"cba",
    );
    assert_eq!(
        string_bifs::bif_reverse(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        string_bifs::bif_lowercase(&[binary(b"HELLO")], &mut ctx).expect("lowercase"),
        b"hello",
    );
    assert_eq!(
        string_bifs::bif_lowercase(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        string_bifs::bif_uppercase(&[binary(b"hello")], &mut ctx).expect("uppercase"),
        b"HELLO",
    );
    assert_eq!(
        string_bifs::bif_uppercase(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        string_bifs::bif_trim(&[binary(b" hi "), atom(&ctx, "both")], &mut ctx).expect("trim both"),
        b"hi",
    );
    assert_eq!(
        string_bifs::bif_trim(&[binary(b" hi "), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );

    let split = string_bifs::bif_split(
        &[binary(b"a-b-c"), binary(b"-"), atom(&ctx, "all")],
        &mut ctx,
    )
    .expect("split all");
    let first = Cons::new(split).expect("first");
    assert_binary(first.head(), b"a");
    let second = Cons::new(first.tail()).expect("second");
    assert_binary(second.head(), b"b");
    let third = Cons::new(second.tail()).expect("third");
    assert_binary(third.head(), b"c");
    assert_eq!(third.tail(), Term::NIL);
    assert_eq!(
        string_bifs::bif_split(&[binary(b"abc"), binary(b""), atom(&ctx, "all")], &mut ctx),
        Err(badarg())
    );

    assert_eq!(
        string_bifs::bif_equal(&[binary(b"a"), binary(b"a")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        string_bifs::bif_equal(&[binary(b"a"), Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_eq!(
        string_bifs::bif_is_empty(&[binary(b"")], &mut ctx),
        Ok(Term::atom(Atom::TRUE))
    );
    assert_eq!(
        string_bifs::bif_is_empty(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );

    assert_binary(
        string_bifs::bif_find(&[binary(b"hello world"), binary(b"world")], &mut ctx)
            .expect("find suffix"),
        b"world",
    );
    assert_eq!(
        string_bifs::bif_find(&[binary(b"hello"), binary(b"xyz")], &mut ctx),
        Ok(atom(&ctx, "nomatch"))
    );

    assert_binary(
        string_bifs::bif_pad(
            &[
                binary(b"hi"),
                Term::small_int(5),
                atom(&ctx, "trailing"),
                binary(b" "),
            ],
            &mut ctx,
        )
        .expect("pad trailing"),
        b"hi   ",
    );

    assert_binary(
        string_bifs::bif_replace(
            &[
                binary(b"aaa"),
                binary(b"a"),
                binary(b"b"),
                atom(&ctx, "all"),
            ],
            &mut ctx,
        )
        .expect("replace all"),
        b"bbb",
    );

    assert_binary(
        string_bifs::bif_slice(
            &[binary(b"hello"), Term::small_int(1), Term::small_int(3)],
            &mut ctx,
        )
        .expect("slice"),
        b"ell",
    );

    let grapheme = string_bifs::bif_next_grapheme(&[binary("éx".as_bytes())], &mut ctx)
        .expect("next grapheme");
    let tuple = Tuple::new(grapheme).expect("grapheme tuple");
    assert_eq!(tuple.get(0), Some(Term::atom(Atom::OK)));
    assert_binary(tuple.get(1).unwrap(), "é".as_bytes());
    assert_binary(tuple.get(2).unwrap(), b"x");
}

#[test]
fn binary_part_covers_normal_empty_and_invalid_edges() {
    let mut ctx = ProcessContext::new();
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
        .expect("empty"),
        b"",
    );
    assert_eq!(
        bif_binary_part(
            &[binary(b"hello"), Term::small_int(-1), Term::small_int(1)],
            &mut ctx
        ),
        Err(badarg())
    );
    assert_eq!(
        bif_binary_part(
            &[binary(b"hello"), Term::small_int(4), Term::small_int(2)],
            &mut ctx
        ),
        Err(badarg())
    );
}

#[test]
fn encoding_bifs_cover_exact_acceptance_and_round_trips() {
    let mut ctx = atom_context();

    assert_binary(
        encoding_bifs::bif_binary_encode_hex(&[binary(b"hello")], &mut ctx).expect("encode hex"),
        b"68656C6C6F",
    );
    assert_binary(
        encoding_bifs::bif_binary_decode_hex(&[binary(b"48454C4C4F")], &mut ctx)
            .expect("decode hex"),
        b"HELLO",
    );
    let hex = encoding_bifs::bif_binary_encode_hex(&[binary(b"roundtrip")], &mut ctx)
        .expect("hex encode");
    assert_binary(
        encoding_bifs::bif_binary_decode_hex(&[hex], &mut ctx).expect("hex decode"),
        b"roundtrip",
    );

    assert_binary(
        encoding_bifs::bif_base64_encode(&[binary(b"hello"), atom(&ctx, "standard")], &mut ctx)
            .expect("base64 encode"),
        b"aGVsbG8=",
    );
    assert_binary(
        encoding_bifs::bif_base64_decode(&[binary(b"aGVsbG8=")], &mut ctx).expect("base64 decode"),
        b"hello",
    );
    let encoded = encoding_bifs::bif_base64_encode(
        &[binary(b"many bytes"), atom(&ctx, "standard")],
        &mut ctx,
    )
    .expect("base64 roundtrip encode");
    assert_binary(
        encoding_bifs::bif_base64_decode(&[encoded], &mut ctx).expect("base64 roundtrip decode"),
        b"many bytes",
    );
}

#[test]
fn b039_io_and_init_bifs_cover_sink_formatter_and_stop() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<u8>>);

    impl crate::io::IoSink for RecordingSink {
        fn write(&self, bytes: &[u8]) {
            self.0.lock().expect("sink lock").extend_from_slice(bytes);
        }
    }

    let sink = Arc::new(RecordingSink::default());
    let mut ctx = atom_context();
    ctx.set_io_sink(sink.clone());

    assert_eq!(
        io_bifs::bif_io_put_chars_1(&[binary(b"hello")], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert_eq!(&*sink.0.lock().expect("sink lock"), b"hello");

    assert_binary(
        io_bifs::bif_io_lib_format_2(
            &[
                binary(b"~s ~s"),
                list(&[binary(b"hello"), binary(b"world")]),
            ],
            &mut ctx,
        )
        .expect("io_lib format"),
        b"hello world",
    );
    assert_binary(
        io_bifs::bif_io_lib_format_2(
            &[
                list(&[
                    Term::small_int(i64::from(b'~')),
                    Term::small_int(i64::from(b's')),
                ]),
                list(&[binary(b"iodata-format")]),
            ],
            &mut ctx,
        )
        .expect("io_lib format accepts Erlang string format"),
        b"iodata-format",
    );

    let mut null_ctx = atom_context();
    assert_eq!(
        io_bifs::bif_io_put_chars_1(&[binary(b"discarded")], &mut null_ctx),
        Ok(Term::atom(Atom::OK))
    );

    assert_eq!(
        bif_init_stop(&[Term::small_int(0)], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
    assert!(ctx.take_shutdown_request());
}

#[test]
fn b033_registry_entries_are_wired_to_function_pointers() {
    let atom_table = AtomTable::new();
    let registry = BifRegistryImpl::new();
    register_stdlib_stubs(&registry, &atom_table).expect("registration");

    let expected = [
        ("gleam_stdlib", "string_replace", 3),
        ("gleam_stdlib", "less_than", 2),
        ("gleam_stdlib", "slice", 3),
        ("gleam_stdlib", "crop_string", 2),
        ("gleam_stdlib", "contains_string", 2),
        ("gleam_stdlib", "string_starts_with", 2),
        ("gleam_stdlib", "string_ends_with", 2),
        ("gleam_stdlib", "string_pop_grapheme", 1),
        ("gleam_stdlib", "utf_codepoint_list_to_string", 1),
        ("gleam_stdlib", "inspect", 1),
        ("gleam_stdlib", "string_remove_prefix", 2),
        ("gleam_stdlib", "string_remove_suffix", 2),
        ("gleam_stdlib", "iodata_append", 2),
        ("string", "length", 1),
        ("string", "reverse", 1),
        ("string", "lowercase", 1),
        ("string", "uppercase", 1),
        ("string", "trim", 2),
        ("string", "split", 3),
        ("string", "find", 2),
        ("string", "next_grapheme", 1),
        ("string", "pad", 4),
        ("string", "replace", 4),
        ("string", "slice", 3),
        ("string", "equal", 2),
        ("string", "is_empty", 1),
        ("binary", "part", 3),
        ("binary", "encode_hex", 1),
        ("binary", "decode_hex", 1),
        ("base64", "encode", 2),
        ("base64", "decode", 1),
        ("io", "put_chars", 1),
        ("io", "put_chars", 2),
        ("io", "format", 3),
        ("io", "setopts", 2),
        ("io_lib", "format", 2),
        ("init", "stop", 1),
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
