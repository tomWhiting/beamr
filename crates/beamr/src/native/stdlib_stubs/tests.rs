use crate::atom::{Atom, AtomTable};
use crate::native::{BifRegistryImpl, ProcessContext};
use crate::term::Term;
use crate::term::binary::{self, Binary};
use crate::term::boxed::write_cons;

use super::{
    bif_characters_to_binary, bif_characters_to_list, bif_debug_options, bif_identity,
    bif_logger_warning, register_stdlib_stubs,
};

fn context() -> ProcessContext {
    ProcessContext::new()
}

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

// ---- gleam_stdlib:identity/1 ----

#[test]
fn identity_returns_atom_unchanged() {
    let mut ctx = context();
    let term = Term::atom(Atom::OK);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_returns_integer_unchanged() {
    let mut ctx = context();
    let term = Term::small_int(42);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_returns_nil_unchanged() {
    let mut ctx = context();
    assert_eq!(bif_identity(&[Term::NIL], &mut ctx), Ok(Term::NIL));
}

#[test]
fn identity_returns_pid_unchanged() {
    let mut ctx = context();
    let term = Term::pid(7);
    assert_eq!(bif_identity(&[term], &mut ctx), Ok(term));
}

#[test]
fn identity_rejects_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_identity(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_identity(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- sys:debug_options/1 ----

#[test]
fn debug_options_returns_empty_list() {
    let mut ctx = context();
    assert_eq!(bif_debug_options(&[Term::NIL], &mut ctx), Ok(Term::NIL));
}

#[test]
fn debug_options_accepts_non_empty_list() {
    let mut ctx = context();
    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::small_int(1), Term::NIL).unwrap();
    assert_eq!(bif_debug_options(&[list], &mut ctx), Ok(Term::NIL));
}

#[test]
fn debug_options_accepts_any_term() {
    let mut ctx = context();
    // Stub is permissive — any single argument returns [].
    assert_eq!(
        bif_debug_options(&[Term::small_int(42)], &mut ctx),
        Ok(Term::NIL)
    );
}

#[test]
fn debug_options_rejects_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_debug_options(&[], &mut ctx), Err(badarg()));
    assert_eq!(
        bif_debug_options(&[Term::NIL, Term::NIL], &mut ctx),
        Err(badarg())
    );
}

// ---- logger:warning/2 ----

#[test]
fn logger_warning_returns_ok_for_binary_format() {
    let mut ctx = context();
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
    let mut ctx = context();
    let format = Term::atom(Atom::ERROR);
    let args = Term::NIL;
    assert_eq!(
        bif_logger_warning(&[format, args], &mut ctx),
        Ok(Term::atom(Atom::OK))
    );
}

#[test]
fn logger_warning_rejects_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_logger_warning(&[], &mut ctx), Err(badarg()));
    assert_eq!(bif_logger_warning(&[Term::NIL], &mut ctx), Err(badarg()));
}

// ---- unicode:characters_to_binary/1 ----

#[test]
fn characters_to_binary_passes_through_binary() {
    let mut ctx = context();
    let mut heap = [0u64; 3];
    let bin = binary::write_binary(&mut heap, b"hello").unwrap();
    let result = bif_characters_to_binary(&[bin], &mut ctx).unwrap();
    // Should return the same binary term.
    assert_eq!(result, bin);
}

#[test]
fn characters_to_binary_converts_empty_list() {
    let mut ctx = context();
    let result = bif_characters_to_binary(&[Term::NIL], &mut ctx).unwrap();
    let bin = Binary::new(result).expect("should be a binary");
    assert!(bin.is_empty());
}

#[test]
fn characters_to_binary_converts_integer_code_point_list() {
    let mut ctx = context();
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
    let mut ctx = context();
    assert_eq!(
        bif_characters_to_binary(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn characters_to_binary_rejects_wrong_arity() {
    let mut ctx = context();
    assert_eq!(bif_characters_to_binary(&[], &mut ctx), Err(badarg()));
}

#[test]
fn characters_to_binary_rejects_list_with_invalid_code_point() {
    let mut ctx = context();
    // List containing an atom (not a valid code point).
    let mut cell = [0u64; 2];
    let list = write_cons(&mut cell, Term::atom(Atom::OK), Term::NIL).unwrap();
    assert_eq!(bif_characters_to_binary(&[list], &mut ctx), Err(badarg()));
}

// ---- unicode:characters_to_list/1 ----

#[test]
fn characters_to_list_converts_ascii_binary() {
    let mut ctx = context();
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
    let mut ctx = context();
    let mut heap = [0u64; 2];
    let bin = binary::write_binary(&mut heap, b"").unwrap();
    assert_eq!(bif_characters_to_list(&[bin], &mut ctx), Ok(Term::NIL));
}

#[test]
fn characters_to_list_handles_multibyte_utf8() {
    let mut ctx = context();
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
    let mut ctx = context();
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
    let mut ctx = context();
    assert_eq!(bif_characters_to_list(&[], &mut ctx), Err(badarg()));
}

// ---- Registration ----

#[test]
fn register_stdlib_stubs_registers_all_expected_mfas() {
    let atom_table = AtomTable::new();
    let mut registry = BifRegistryImpl::new();

    register_stdlib_stubs(&mut registry, &atom_table).expect("registration should succeed");

    let expected = [
        ("logger", "warning", 2),
        ("unicode", "characters_to_list", 1),
        ("unicode", "characters_to_binary", 1),
        ("sys", "debug_options", 1),
        ("gleam_stdlib", "identity", 1),
        ("maps", "from_list", 1),
        ("maps", "merge", 2),
        ("maps", "remove", 2),
        ("maps", "map", 2),
        ("lists", "reverse", 1),
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
    let mut registry = BifRegistryImpl::new();

    register_stdlib_stubs(&mut registry, &atom_table).expect("first registration");
    assert!(register_stdlib_stubs(&mut registry, &atom_table).is_err());
}
