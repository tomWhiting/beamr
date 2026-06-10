use std::sync::Arc;
use std::sync::Mutex;

use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext, stdlib_stubs::register_stdlib_stubs};
use crate::process::Process;
use crate::term::Term;
use crate::term::binary;

use super::gleam_stdlib_ffi2::*;

fn context(process: &mut Process) -> ProcessContext<'_> {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut context = ProcessContext::new();
    context.set_atom_table(Some(table));
    context.attach_process(process, 0);
    context
}

fn binary(bytes: &[u8]) -> Term {
    let heap = Box::leak(vec![0u64; 2 + binary::packed_word_count(bytes.len())].into_boxed_slice());
    binary::write_binary(heap, bytes).expect("binary")
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<u8>>);

impl crate::io::IoSink for RecordingSink {
    fn write(&self, bytes: &[u8]) {
        self.0.lock().expect("sink lock").extend_from_slice(bytes);
    }
}

#[test]
fn print_wrappers_write_to_configured_sink_and_return_nil() {
    let sink = Arc::new(RecordingSink::default());
    let mut process = Process::new(1, 256);
    let mut context = context(&mut process);
    context.set_io_sink(sink.clone());

    // gleam_stdlib.erl's print family returns the `nil` atom, not `ok`.
    let nil = Ok(Term::atom(Atom::NIL));
    assert_eq!(bif_print(&[binary(b"a")], &mut context), nil);
    assert_eq!(bif_print_error(&[binary(b"b")], &mut context), nil);
    assert_eq!(bif_println(&[binary(b"c")], &mut context), nil);
    assert_eq!(bif_println_error(&[binary(b"d")], &mut context), nil);
    assert_eq!(&*sink.0.lock().expect("sink lock"), b"abc\nd\n");
}

#[test]
fn gleam_stdlib_natives_are_print_family_only() {
    let atom_table = AtomTable::with_common_atoms();
    let registry = BifRegistryImpl::new();
    register_stdlib_stubs(&registry, &atom_table).expect("registration");
    let module = atom_table.intern("gleam_stdlib");

    // beamr owns the IO sink, so the print family is served natively.
    let expected = [
        ("print", 1),
        ("print_error", 1),
        ("println", 1),
        ("println_error", 1),
    ];
    for (name, arity) in expected {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(module, function, arity).is_some(),
            "missing gleam_stdlib:{name}/{arity}"
        );
    }

    // Every other gleam_stdlib function ships as compiled bytecode in
    // gleam_stdlib.beam and must NOT be shadowed: a native entry overrides
    // the loaded module and any contract drift silently breaks the stdlib.
    let removed = [
        ("classify_dynamic", 1),
        ("dict", 1),
        ("is_null", 1),
        ("list", 5),
        ("float", 1),
        ("index", 2),
        ("int", 1),
        ("bit_array", 1),
        ("identity", 1),
        ("float_to_string", 1),
        ("int_from_base_string", 2),
        ("parse_float", 1),
        ("parse_int", 1),
        ("map_get", 2),
        ("wrap_list", 1),
        ("parse_query", 1),
        ("percent_decode", 1),
        ("percent_encode", 1),
        ("uri_parse", 1),
        ("base16_decode", 1),
        ("base16_encode", 1),
        ("base64_decode", 1),
        ("base64_encode", 2),
        ("bit_array_concat", 1),
        ("bit_array_pad_to_bytes", 1),
        ("bit_array_slice", 3),
        ("bit_array_to_int_and_size", 1),
        ("string_replace", 3),
        ("less_than", 2),
        ("slice", 3),
        ("crop_string", 2),
        ("contains_string", 2),
        ("string_starts_with", 2),
        ("string_ends_with", 2),
        ("string_pop_grapheme", 1),
        ("utf_codepoint_list_to_string", 1),
        ("inspect", 1),
        ("string_remove_prefix", 2),
        ("string_remove_suffix", 2),
        ("iodata_append", 2),
    ];
    for (name, arity) in removed {
        let function = atom_table.intern(name);
        assert!(
            registry.lookup(module, function, arity).is_none(),
            "gleam_stdlib:{name}/{arity} must come from loaded bytecode, not a stub"
        );
    }
}
