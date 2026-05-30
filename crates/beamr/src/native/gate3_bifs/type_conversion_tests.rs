use std::sync::Arc;

use crate::atom::{Atom, AtomTable};
use crate::native::ProcessContext;
use crate::term::Term;
use crate::term::binary::write_binary;
use crate::term::boxed::{write_cons, write_map};

use super::*;

fn badarg() -> Term {
    Term::atom(Atom::BADARG)
}

fn ctx_with_atoms() -> ProcessContext {
    let table = Arc::new(AtomTable::with_common_atoms());
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(table));
    ctx
}

// ---- erlang:atom_to_binary/2 ----

#[test]
fn atom_to_binary_converts_ok_to_binary() {
    let mut ctx = ctx_with_atoms();
    let result =
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::UTF8)], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"ok");
}

#[test]
fn atom_to_binary_latin1_encoding_works() {
    let mut ctx = ctx_with_atoms();
    let result =
        bif_atom_to_binary(&[Term::atom(Atom::ERROR), Term::atom(Atom::LATIN1)], &mut ctx)
            .unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("should be binary");
    assert_eq!(binary.as_bytes(), b"error");
}

#[test]
fn atom_to_binary_badarg_invalid_encoding() {
    let mut ctx = ctx_with_atoms();
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_non_atom() {
    let mut ctx = ctx_with_atoms();
    assert_eq!(
        bif_atom_to_binary(&[Term::small_int(42), Term::atom(Atom::UTF8)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_no_atom_table() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK), Term::atom(Atom::UTF8)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn atom_to_binary_badarg_wrong_arity() {
    let mut ctx = ctx_with_atoms();
    assert_eq!(
        bif_atom_to_binary(&[Term::atom(Atom::OK)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:binary_to_existing_atom/1 ----

#[test]
fn binary_to_existing_atom_finds_interned_atom() {
    let mut ctx = ctx_with_atoms();
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");
    let result = bif_binary_to_existing_atom(&[bin], &mut ctx).unwrap();
    assert_eq!(result.as_atom(), Some(Atom::OK));
}

#[test]
fn binary_to_existing_atom_badarg_unknown() {
    let mut ctx = ctx_with_atoms();
    let mut heap = [0u64; 5];
    let bin = write_binary(&mut heap, b"nonexistent_atom_xyz").expect("binary");
    assert_eq!(
        bif_binary_to_existing_atom(&[bin], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn binary_to_existing_atom_badarg_non_binary() {
    let mut ctx = ctx_with_atoms();
    assert_eq!(
        bif_binary_to_existing_atom(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn binary_to_existing_atom_badarg_no_atom_table() {
    let mut ctx = ProcessContext::new();
    let mut heap = [0u64; 3];
    let bin = write_binary(&mut heap, b"ok").expect("binary");
    assert_eq!(
        bif_binary_to_existing_atom(&[bin], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:binary_to_list/1 ----

#[test]
fn binary_to_list_converts_bytes() {
    let mut ctx = ProcessContext::new();
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
    let mut ctx = ProcessContext::new();
    let mut heap = [0u64; 2];
    let bin = write_binary(&mut heap, b"").expect("binary");
    assert_eq!(bif_binary_to_list(&[bin], &mut ctx), Ok(Term::NIL));
}

#[test]
fn binary_to_list_badarg_non_binary() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_binary_to_list(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:list_to_binary/1 ----

#[test]
fn list_to_binary_converts_byte_list() {
    let mut ctx = ProcessContext::new();
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
    let mut ctx = ProcessContext::new();
    let result = bif_list_to_binary(&[Term::NIL], &mut ctx).unwrap();
    let binary = crate::term::binary::Binary::new(result).expect("binary");
    assert!(binary.is_empty());
}

#[test]
fn list_to_binary_badarg_value_out_of_range() {
    let mut ctx = ProcessContext::new();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(256), Term::NIL).unwrap();
    assert_eq!(bif_list_to_binary(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn list_to_binary_badarg_negative_value() {
    let mut ctx = ProcessContext::new();
    let mut c1 = [0u64; 2];
    let list = write_cons(&mut c1, Term::small_int(-1), Term::NIL).unwrap();
    assert_eq!(bif_list_to_binary(&[list], &mut ctx), Err(badarg()));
}

#[test]
fn list_to_binary_badarg_non_list() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_list_to_binary(&[Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

// ---- erlang:map_get/2 ----

#[test]
fn map_get_returns_value() {
    let mut ctx = ProcessContext::new();
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
    let mut ctx = ProcessContext::new();
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
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_map_get(&[Term::small_int(1), Term::small_int(42)], &mut ctx),
        Err(badarg())
    );
}

#[test]
fn map_get_badarg_wrong_arity() {
    let mut ctx = ProcessContext::new();
    assert_eq!(
        bif_map_get(&[Term::small_int(1)], &mut ctx),
        Err(badarg())
    );
}
